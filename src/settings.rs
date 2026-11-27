//! Runtime-adjustable game settings and the pause-menu state.
//!
//! Values live in a resource so the settings menu, player controller, camera,
//! and chunk streamer all read from the same source.

use bevy::input::keyboard::KeyCode;
use bevy::light::{
    CascadeShadowConfig, CascadeShadowConfigBuilder, DirectionalLightShadowMap,
    ShadowFilteringMethod,
};
use bevy::pbr::ScreenSpaceAmbientOcclusion;
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::view::{
    ColorGrading, ColorGradingGlobal, ColorGradingSection, Msaa, Tonemapping,
};
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};

use crate::day_night::{DayNightCycle, lerp, lerp_color, smoothstep};
use crate::player::{Player, PlayerCamera};
use crate::world::block::PANEL_HEIGHT;
use crate::world::chunk::{BLOCKS_PER_METRE, DETAILS_PER_WORLD_AXIS, detail_to_world};
use crate::world::estimated_stream_chunk_count;
use crate::world::{ChunkManager, WorldGenResource};

pub const DEFAULT_MOUSE_SENSITIVITY: f32 = 0.0003;
pub const DEFAULT_FOV_DEGREES: f32 = 90.0;
pub const DEFAULT_RENDER_DISTANCE: i32 = 8;
/// Slider ranges exposed by the pause menu.
pub const MIN_FOV_DEGREES: f32 = 90.0;
pub const MAX_FOV_DEGREES: f32 = 120.0;
pub const MIN_SENSITIVITY_MULTIPLIER: f32 = 0.1;
pub const MAX_SENSITIVITY_MULTIPLIER: f32 = 10.0;
pub const MIN_RENDER_DISTANCE: i32 = 4;
pub const MAX_RENDER_DISTANCE: i32 = 128;
/// Size restored when leaving fullscreen.
pub const WINDOWED_WIDTH: u32 = 1280;
pub const WINDOWED_HEIGHT: u32 = 720;

/// Where linear fog begins, as a fraction of the normal-band fog distance. Fog
/// ends there, before the potentially larger cull distance of the cave tail, so
/// terrain is fully hidden before it can pop into view.
const FOG_START_FRACTION: f32 = 0.7;

/// Surface lighting values shared by startup and the underground transition.
pub const SURFACE_FOG_COLOR: [f32; 3] = [0.55, 0.72, 0.88];
pub const SURFACE_AMBIENT_COLOR: [f32; 3] = [0.66, 0.78, 0.98];
pub const SURFACE_AMBIENT_BRIGHTNESS: f32 = 320.0;
pub const SURFACE_SUN_ILLUMINANCE: f32 = 12_000.0;

/// Underground lighting stays dim enough that caves do not glow blue, but not
/// so dark that the player cannot see the next chamber.
pub const CAVE_FOG_COLOR: [f32; 3] = [0.02, 0.03, 0.045];
pub const CAVE_AMBIENT_COLOR: [f32; 3] = [0.38, 0.44, 0.52];
pub const CAVE_AMBIENT_BRIGHTNESS: f32 = 42.0;
pub const CAVE_SUN_ILLUMINANCE: f32 = 1_200.0;
pub const CAVE_FOG_END: f32 = 60.0 * BLOCKS_PER_METRE;

/// Enter after the eyes are clearly below the local surface, and exit sooner,
/// so the transition does not flicker while crossing a cave mouth. Open sky
/// overrides the depth test so a lit entrance keeps the daylight presets.
const CAVE_ENTER_DEPTH_METRES: f32 = 1.5;
const CAVE_EXIT_DEPTH_METRES: f32 = 0.5;
const CAVE_LIGHT_FADE_RATE: f32 = 1.4;
const WATER_TINT_FADE_RATE: f32 = 4.0;
/// How far upward probes search for an opening to the sky.
const DAYLIGHT_RAY_LENGTH: f32 = 96.0 * BLOCKS_PER_METRE;

/// Sun shadow cost/quality preset. Lower presets render fewer shadow cascades
/// and cover a shorter distance, which matters at large view distances where a
/// shadow pass can touch thousands of chunks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShadowQuality {
    Off,
    Low,
    #[default]
    Medium,
    High,
}

impl ShadowQuality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Off,
        }
    }

    fn cascades(self) -> usize {
        match self {
            Self::Off | Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        }
    }

    fn max_distance_m(self) -> f32 {
        match self {
            Self::Off | Self::Low => 70.0,
            Self::Medium => 110.0,
            Self::High => 140.0,
        }
    }

    fn first_cascade_m(self) -> f32 {
        match self {
            Self::Off | Self::Low => 70.0,
            Self::Medium | Self::High => 30.0,
        }
    }

    fn shadow_map_size(self) -> usize {
        match self {
            Self::High => 2048,
            Self::Off | Self::Low | Self::Medium => 1024,
        }
    }

    fn shadow_filtering(self) -> ShadowFilteringMethod {
        match self {
            Self::Off | Self::Low => ShadowFilteringMethod::Hardware2x2,
            Self::Medium | Self::High => ShadowFilteringMethod::Gaussian,
        }
    }
}

/// Marker used by the settings system to find the directional sun light.
#[derive(Component)]
pub struct SunLight;

/// Marker for the nighttime moon directional light.
#[derive(Component)]
pub struct MoonLight;

/// Ordering for systems that derive outdoor-only effects from underground state.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightingSystems {
    UpdateUnderground,
}

/// Smoothed underground state used by fog, lighting, and outdoor-only effects.
#[derive(Resource, Debug, Clone, Copy)]
pub struct UndergroundLighting {
    underground: bool,
    blend: f32,
    sky_visible: bool,
    water_blend: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceLightingPreset {
    pub fog_color: [f32; 3],
    pub ambient_color: [f32; 3],
    pub ambient_brightness: f32,
    pub sun_color: [f32; 3],
    pub sun_illuminance: f32,
    pub moon_color: [f32; 3],
    pub moon_illuminance: f32,
    pub clear_color: [f32; 3],
}

pub fn surface_preset_for_cycle(cycle: &DayNightCycle) -> SurfaceLightingPreset {
    let day = cycle.daylight_factor();
    let night = cycle.night_factor();
    let twilight = cycle.twilight_factor();
    let sunrise = sunrise_side(cycle.time_of_day);

    let day_fog = SURFACE_FOG_COLOR;
    let night_fog = [0.03, 0.05, 0.10];
    let dawn_fog = [0.72, 0.52, 0.44];
    let dusk_fog = [0.58, 0.36, 0.34];
    let warm_fog = if sunrise > 0.5 { dawn_fog } else { dusk_fog };

    let mut fog = lerp3(night_fog, day_fog, day);
    fog = lerp3(fog, warm_fog, twilight * 0.65);

    let day_ambient = SURFACE_AMBIENT_COLOR;
    let night_ambient = [0.16, 0.22, 0.38];
    let mut ambient = lerp3(night_ambient, day_ambient, day);
    ambient = lerp3(ambient, [0.58, 0.42, 0.44], twilight * 0.35);

    let ambient_brightness = lerp(55.0, SURFACE_AMBIENT_BRIGHTNESS, day) + twilight * 18.0;

    let sun_day = [1.0, 0.97, 0.90];
    let sun_warm = [1.0, 0.62, 0.38];
    let sun_color = lerp3(sun_day, sun_warm, twilight);
    let sun_illuminance = lerp(
        0.0,
        SURFACE_SUN_ILLUMINANCE,
        smoothstep(-0.08, 0.18, cycle.sun_height()),
    );

    let moon_color = [0.64, 0.74, 1.0];
    let moon_illuminance = lerp(0.0, 450.0, smoothstep(-0.05, 0.15, cycle.moon_height())) * night;

    // Keep clear color close to horizon/fog so sky transitions feel coherent.
    let clear = lerp3(fog, ambient, 0.15);

    SurfaceLightingPreset {
        fog_color: fog,
        ambient_color: ambient,
        ambient_brightness,
        sun_color,
        sun_illuminance,
        moon_color,
        moon_illuminance,
        clear_color: clear,
    }
}

fn sunrise_side(time_of_day: f32) -> f32 {
    let t = time_of_day.rem_euclid(1.0);
    let dawn = 1.0 - ((t - 0.25).abs().min(1.0 - (t - 0.25).abs()) / 0.2).clamp(0.0, 1.0);
    let dusk = 1.0 - ((t - 0.75).abs().min(1.0 - (t - 0.75).abs()) / 0.2).clamp(0.0, 1.0);
    if dawn + dusk <= 0.0001 {
        0.5
    } else {
        dawn / (dawn + dusk)
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    lerp_color(a, b, t)
}

impl Default for UndergroundLighting {
    fn default() -> Self {
        Self {
            underground: false,
            blend: 0.0,
            sky_visible: true,
            water_blend: 0.0,
        }
    }
}

impl UndergroundLighting {
    pub fn blend(self) -> f32 {
        self.blend
    }

    /// True when an upward terrain probe can still reach open sky.
    /// Test-only accessor; runtime lighting uses the smoothed cave blend.
    #[allow(dead_code)]
    pub fn sky_visible(self) -> bool {
        self.sky_visible
    }

    /// Smoothed 0..1 strength for the underwater screen tint.
    pub fn water_blend(self) -> f32 {
        self.water_blend
    }

    /// Surface-light fraction remaining after the cave blend, from 1 outdoors
    /// to 0 in a fully enclosed underground space.
    pub fn surface_visibility(self) -> f32 {
        1.0 - self.blend
    }
}

/// Upward probes used to detect a cave opening. Cardinal and diagonal tilts
/// cover mouths facing any horizontal direction while staying cheap per frame.
const DAYLIGHT_RAY_DIRECTIONS: [Vec3; 9] = [
    Vec3::Y,
    Vec3::new(0.6, 0.8, 0.0),
    Vec3::new(-0.6, 0.8, 0.0),
    Vec3::new(0.0, 0.8, 0.6),
    Vec3::new(0.0, 0.8, -0.6),
    Vec3::new(0.45, 0.8, 0.45),
    Vec3::new(0.45, 0.8, -0.45),
    Vec3::new(-0.45, 0.8, 0.45),
    Vec3::new(-0.45, 0.8, -0.45),
];

/// True when any probe can reach open sky. Reusing the terrain raycast means
/// this follows the real voxel opening instead of a simple depth threshold.
fn sky_is_visible(manager: &ChunkManager, eye: Vec3) -> bool {
    DAYLIGHT_RAY_DIRECTIONS.iter().any(|&direction| {
        manager
            .raycast(eye, direction, DAYLIGHT_RAY_LENGTH)
            .is_none()
    })
}

/// Hysteresis test for the underground lighting state. `eye_y` and `surface_y`
/// are both in detail cells. A visible opening keeps the surface preset even
/// when the camera is already several metres below the height field.
fn underground_target(eye_y: f32, surface_y: f32, active: bool, sky_visible: bool) -> bool {
    if sky_visible {
        return false;
    }
    let enter = surface_y - CAVE_ENTER_DEPTH_METRES * BLOCKS_PER_METRE;
    let exit = surface_y - CAVE_EXIT_DEPTH_METRES * BLOCKS_PER_METRE;
    if active { eye_y < exit } else { eye_y < enter }
}

#[inline]
fn mix(a: f32, b: f32, t: f32) -> f32 {
    lerp(a, b, t)
}

#[inline]
fn mix_color(a: [f32; 3], b: [f32; 3], t: f32) -> Color {
    let c = lerp_color(a, b, t);
    Color::srgb(c[0], c[1], c[2])
}

#[inline]
fn move_toward(current: f32, target: f32, step: f32) -> f32 {
    if current < target {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

/// Apply a shadow preset to the sun light and its cascade config.
pub fn apply_shadow_quality(
    light: &mut DirectionalLight,
    config: &mut CascadeShadowConfig,
    quality: ShadowQuality,
) {
    let enabled = quality != ShadowQuality::Off;
    if light.shadow_maps_enabled != enabled {
        light.shadow_maps_enabled = enabled;
    }

    let desired = CascadeShadowConfigBuilder {
        num_cascades: quality.cascades(),
        maximum_distance: quality.max_distance_m() * BLOCKS_PER_METRE,
        first_cascade_far_bound: quality.first_cascade_m() * BLOCKS_PER_METRE,
        ..default()
    }
    .build();

    if config.bounds != desired.bounds
        || config.overlap_proportion != desired.overlap_proportion
        || config.minimum_distance != desired.minimum_distance
    {
        *config = desired;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
pub enum BloomMode {
    Off,
    #[default]
    Low,
    Medium,
    High,
}

impl BloomMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Off,
        }
    }

    pub fn to_bevy(self) -> Option<Bloom> {
        match self {
            Self::Off => None,
            // Conservative, natural bloom. Prefilter avoids glowing ordinary surfaces.
            Self::Low => Some(Bloom {
                intensity: 0.08,
                low_frequency_boost: 0.35,
                low_frequency_boost_curvature: 0.7,
                high_pass_frequency: 1.0,
                prefilter: BloomPrefilter {
                    threshold: 1.0,
                    threshold_softness: 0.4,
                },
                composite_mode: BloomCompositeMode::EnergyConserving,
                ..Bloom::NATURAL
            }),
            Self::Medium => Some(Bloom {
                intensity: 0.12,
                low_frequency_boost: 0.45,
                low_frequency_boost_curvature: 0.8,
                high_pass_frequency: 1.0,
                prefilter: BloomPrefilter {
                    threshold: 0.9,
                    threshold_softness: 0.35,
                },
                composite_mode: BloomCompositeMode::EnergyConserving,
                ..Bloom::NATURAL
            }),
            Self::High => Some(Bloom {
                intensity: 0.18,
                low_frequency_boost: 0.55,
                low_frequency_boost_curvature: 0.85,
                high_pass_frequency: 1.0,
                prefilter: BloomPrefilter {
                    threshold: 0.8,
                    threshold_softness: 0.3,
                },
                composite_mode: BloomCompositeMode::EnergyConserving,
                ..Bloom::NATURAL
            }),
        }
    }
}

/// Screen-space ambient occlusion quality. SSAO requires MSAA Off on the
/// PlayerCamera; the authoritative render-requirements resolver handles that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
pub enum SsaoMode {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl SsaoMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Off,
        }
    }

    fn to_bevy_quality(self) -> bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel {
        match self {
            Self::Low => bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel::Low,
            Self::Medium => bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel::Medium,
            Self::High | Self::Off => bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel::High,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
pub enum TonemappingMode {
    #[default]
    TonyMcMapface,
    AgX,
    AcesFitted,
    BlenderFilmic,
    SomewhatBoring,
    Reinhard,
    ReinhardLuminance,
    None,
}

impl TonemappingMode {
    pub const ALL: [Self; 8] = [
        Self::TonyMcMapface,
        Self::AgX,
        Self::AcesFitted,
        Self::BlenderFilmic,
        Self::SomewhatBoring,
        Self::Reinhard,
        Self::ReinhardLuminance,
        Self::None,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::TonyMcMapface => "TonyMcMapface",
            Self::AgX => "AgX",
            Self::AcesFitted => "ACES Fitted",
            Self::BlenderFilmic => "Blender Filmic",
            Self::SomewhatBoring => "SomewhatBoring",
            Self::Reinhard => "Reinhard",
            Self::ReinhardLuminance => "Reinhard Lum",
            Self::None => "None",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn to_bevy(self) -> Tonemapping {
        match self {
            Self::TonyMcMapface => Tonemapping::TonyMcMapface,
            Self::AgX => Tonemapping::AgX,
            Self::AcesFitted => Tonemapping::AcesFitted,
            Self::BlenderFilmic => Tonemapping::BlenderFilmic,
            Self::SomewhatBoring => Tonemapping::SomewhatBoringDisplayTransform,
            Self::Reinhard => Tonemapping::Reinhard,
            Self::ReinhardLuminance => Tonemapping::ReinhardLuminance,
            Self::None => Tonemapping::None,
        }
    }
}

pub fn apply_color_grading(settings: &GameSettings) -> ColorGrading {
    let section = ColorGradingSection {
        saturation: settings.saturation.clamp(0.0, 2.0),
        contrast: settings.contrast.clamp(0.5, 1.5),
        ..default()
    };
    let global = ColorGradingGlobal {
        exposure: settings.exposure_ev.clamp(-4.0, 4.0),
        temperature: settings.temperature.clamp(-1.0, 1.0),
        post_saturation: settings.saturation.clamp(0.0, 2.0),
        ..default()
    };
    ColorGrading::with_identical_sections(global, section)
}
#[derive(Resource, Clone, Copy, Debug)]
pub struct GameSettings {
    pub mouse_sensitivity: f32,
    pub fov_degrees: f32,
    pub render_distance: i32,
    pub shadow_quality: ShadowQuality,
    pub vsync: bool,
    pub fullscreen: bool,
    /// Append the 1 Hz F3 performance snapshot to /tmp/opencode/kt-perf.log.
    /// Off by default so a normal game does not write a dev artifact.
    pub perf_log: bool,
    /// Lightweight stylized clouds; disable for performance comparison.
    pub clouds_enabled: bool,
    /// Screen-space light shafts mode.
    pub light_shafts_mode: crate::light_shafts::LightShaftsMode,
    /// Tonemapping operator for the 3D camera.
    pub tonemapping: TonemappingMode,
    /// Exposure in EV stops (ColorGrading global exposure).
    pub exposure_ev: f32,
    /// Post-tonemapping saturation multiplier.
    pub saturation: f32,
    /// Contrast applied to shadows/midtones/highlights.
    pub contrast: f32,
    /// White-balance temperature (negative cooler, positive warmer).
    pub temperature: f32,
    /// Foliage wind displacement strength.
    pub wind_strength: f32,
    /// Screen-space ambient occlusion quality. SSAO requires MSAA Off.
    pub ssao: SsaoMode,
    /// User's preferred MSAA; restored when SSAO is disabled.
    pub preferred_msaa_samples: u32,
    /// Bloom/glow intensity preset.
    pub bloom: BloomMode,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            mouse_sensitivity: DEFAULT_MOUSE_SENSITIVITY,
            fov_degrees: DEFAULT_FOV_DEGREES,
            render_distance: DEFAULT_RENDER_DISTANCE,
            shadow_quality: ShadowQuality::Medium,
            vsync: false,
            fullscreen: true,
            perf_log: false,
            clouds_enabled: true,
            light_shafts_mode: crate::light_shafts::LightShaftsMode::Simple,
            tonemapping: TonemappingMode::TonyMcMapface,
            exposure_ev: 0.0,
            saturation: 1.0,
            contrast: 1.0,
            temperature: 0.0,
            wind_strength: 0.075,
            ssao: SsaoMode::Off,
            preferred_msaa_samples: 2,
            bloom: BloomMode::Low,
        }
    }
}

/// Whether the pause/settings menu is open is tracked by
/// [`crate::ui_manager::UiManager`]. This resource only carries the one-shot
/// request made by the menu's Resume button.
#[derive(Resource, Default)]
pub struct MenuState {
    /// Set by the menu's Resume button; the cursor system consumes it on the
    /// next frame so the click cannot also break a block.
    pub resume_requested: bool,
}

/// Keyboard bindings for gameplay actions.
///
/// Mouse buttons are intentionally not rebound here yet; the Keybinds tab edits
/// this resource, and every gameplay key lives in one place instead of being
/// scattered through systems.
#[derive(Resource, Clone, Debug)]
pub struct InputBindings {
    pub move_forward: KeyCode,
    pub move_back: KeyCode,
    pub move_left: KeyCode,
    pub move_right: KeyCode,
    pub jump: KeyCode,
    pub sprint: KeyCode,
    pub crouch: KeyCode,
    pub chat: KeyCode,
    pub pause: KeyCode,
    pub help: KeyCode,
    pub debug_overlay: KeyCode,
    pub backpack: KeyCode,
    /// Opens the overhead map (Live/Explore).
    pub map: KeyCode,
    /// Toggle HUD visibility.
    pub toggle_hud: KeyCode,
    /// Reserved for future remappable interact key; RMB is currently used.
    #[allow(dead_code)]
    pub interact: KeyCode,
    /// Rotate placeable structure preview clockwise. Placement itself uses RMB.
    pub rotate_place: KeyCode,
    /// Write the current world and inventory to the save file.
    pub save: KeyCode,
    /// Restore the save file.
    pub load: KeyCode,
    /// Write a timestamped PNG of the current frame.
    pub screenshot: KeyCode,
}

impl Default for InputBindings {
    fn default() -> Self {
        Self {
            move_forward: KeyCode::KeyW,
            move_back: KeyCode::KeyS,
            move_left: KeyCode::KeyA,
            move_right: KeyCode::KeyD,
            jump: KeyCode::Space,
            sprint: KeyCode::ShiftLeft,
            crouch: KeyCode::ControlLeft,
            chat: KeyCode::Enter,
            pause: KeyCode::Escape,
            help: KeyCode::KeyH,
            debug_overlay: KeyCode::F3,
            backpack: KeyCode::Tab,
            map: KeyCode::KeyM,
            toggle_hud: KeyCode::F1,
            interact: KeyCode::KeyE,
            rotate_place: KeyCode::KeyR,
            save: KeyCode::F9,
            load: KeyCode::F10,
            screenshot: KeyCode::F12,
        }
    }
}

/// A gameplay key that can be changed from the Keybinds tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingAction {
    MoveForward,
    MoveBack,
    MoveLeft,
    MoveRight,
    Jump,
    Sprint,
    Crouch,
    Chat,
    Pause,
    Help,
    DebugOverlay,
    Backpack,
    Map,
    ToggleHud,
    RotatePlace,
    Save,
    Load,
    Screenshot,
}

impl BindingAction {
    pub const ALL: [Self; 18] = [
        Self::MoveForward,
        Self::MoveBack,
        Self::MoveLeft,
        Self::MoveRight,
        Self::Jump,
        Self::Sprint,
        Self::Crouch,
        Self::Chat,
        Self::Pause,
        Self::Help,
        Self::DebugOverlay,
        Self::Backpack,
        Self::Map,
        Self::ToggleHud,
        Self::RotatePlace,
        Self::Save,
        Self::Load,
        Self::Screenshot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::MoveForward => "Move forward",
            Self::MoveBack => "Move back",
            Self::MoveLeft => "Move left",
            Self::MoveRight => "Move right",
            Self::Jump => "Jump",
            Self::Sprint => "Sprint",
            Self::Crouch => "Crouch",
            Self::Chat => "Chat",
            Self::Pause => "Pause / settings",
            Self::Help => "Help",
            Self::DebugOverlay => "Debug overlay",
            Self::Backpack => "Backpack",
            Self::Map => "Overhead map",
            Self::ToggleHud => "Toggle HUD",
            Self::RotatePlace => "Rotate placement",
            Self::Save => "Save",
            Self::Load => "Load",
            Self::Screenshot => "Screenshot",
        }
    }

    pub fn get(self, bindings: &InputBindings) -> KeyCode {
        match self {
            Self::MoveForward => bindings.move_forward,
            Self::MoveBack => bindings.move_back,
            Self::MoveLeft => bindings.move_left,
            Self::MoveRight => bindings.move_right,
            Self::Jump => bindings.jump,
            Self::Sprint => bindings.sprint,
            Self::Crouch => bindings.crouch,
            Self::Chat => bindings.chat,
            Self::Pause => bindings.pause,
            Self::Help => bindings.help,
            Self::DebugOverlay => bindings.debug_overlay,
            Self::Backpack => bindings.backpack,
            Self::Map => bindings.map,
            Self::ToggleHud => bindings.toggle_hud,
            Self::RotatePlace => bindings.rotate_place,
            Self::Save => bindings.save,
            Self::Load => bindings.load,
            Self::Screenshot => bindings.screenshot,
        }
    }

    /// Assign `key` to this action. If another action already owns it, the two
    /// keys are swapped so every action keeps a unique binding.
    pub fn set(self, bindings: &mut InputBindings, key: KeyCode) {
        if let Some(other) = Self::ALL
            .into_iter()
            .find(|candidate| *candidate != self && candidate.get(bindings) == key)
        {
            let previous = self.get(bindings);
            other.set_raw(bindings, previous);
        }
        self.set_raw(bindings, key);
    }

    fn set_raw(self, bindings: &mut InputBindings, key: KeyCode) {
        match self {
            Self::MoveForward => bindings.move_forward = key,
            Self::MoveBack => bindings.move_back = key,
            Self::MoveLeft => bindings.move_left = key,
            Self::MoveRight => bindings.move_right = key,
            Self::Jump => bindings.jump = key,
            Self::Sprint => bindings.sprint = key,
            Self::Crouch => bindings.crouch = key,
            Self::Chat => bindings.chat = key,
            Self::Pause => bindings.pause = key,
            Self::Help => bindings.help = key,
            Self::DebugOverlay => bindings.debug_overlay = key,
            Self::Backpack => bindings.backpack = key,
            Self::Map => bindings.map = key,
            Self::ToggleHud => bindings.toggle_hud = key,
            Self::RotatePlace => bindings.rotate_place = key,
            Self::Save => bindings.save = key,
            Self::Load => bindings.load = key,
            Self::Screenshot => bindings.screenshot = key,
        }
    }
}

/// The Keybinds tab is waiting for the next key press for this action.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct KeybindCapture(pub Option<BindingAction>);

pub struct SettingsPlugin;

fn apply_visual_settings(
    settings: Res<GameSettings>,
    mut camera: Query<(&mut Tonemapping, &mut ColorGrading), With<PlayerCamera>>,
) {
    if !settings.is_changed() {
        return;
    }
    let tonemapping = settings.tonemapping.to_bevy();
    for (mut tm, mut cg) in &mut camera {
        if *tm != tonemapping {
            *tm = tonemapping;
        }
        let grading = apply_color_grading(&settings);
        if *cg != grading {
            *cg = grading;
        }
    }
}

/// Authoritative PlayerCamera rendering requirements.
///
/// This is the only system that should add/remove SSAO/prepass/MSAA components
/// on `PlayerCamera`. Requirements are derived from settings so future feature
/// activation does not require another ownership rewrite.
#[allow(clippy::type_complexity)]
fn sync_player_camera_render_requirements(
    mut commands: Commands,
    settings: Res<GameSettings>,
    mut cameras: Query<
        (
            Entity,
            &mut Msaa,
            Has<ScreenSpaceAmbientOcclusion>,
            Has<bevy::core_pipeline::prepass::DepthPrepass>,
            Has<bevy::core_pipeline::prepass::NormalPrepass>,
        ),
        With<PlayerCamera>,
    >,
) {
    let ssao_active = settings.ssao != SsaoMode::Off;
    // Detailed shafts need the same depth prepass as SSAO; the light-shaft pass
    // falls back to Simple if that depth is unexpectedly unavailable.
    let detailed_light_shafts_active = settings.light_shafts_mode.is_detailed();

    let need_depth_prepass = ssao_active || detailed_light_shafts_active;
    let need_normal_prepass = ssao_active;
    // SSAO already requires single-sample rendering. Detailed does too, avoiding
    // the unvalidated multisampled-depth shader permutation.
    // `preferred_msaa_samples` is not changed, so it is restored automatically
    // when neither SSAO nor Detailed shafts is active.
    let single_sample_required = ssao_active || detailed_light_shafts_active;
    let desired_msaa = if single_sample_required {
        Msaa::Off
    } else {
        match settings.preferred_msaa_samples {
            1 => Msaa::Off,
            4 => Msaa::Sample4,
            8 => Msaa::Sample8,
            _ => Msaa::Sample2,
        }
    };

    for (entity, mut msaa, has_ssao, has_depth, has_normal) in &mut cameras {
        if *msaa != desired_msaa {
            *msaa = desired_msaa;
        }

        // Only mutate components when the authoritative state differs. This
        // avoids churn and limits ownership to PlayerCamera entities.
        if has_ssao != ssao_active
            || has_depth != need_depth_prepass
            || has_normal != need_normal_prepass
        {
            let mut entity_cmds = commands.entity(entity);

            if has_ssao && !ssao_active {
                entity_cmds.remove::<ScreenSpaceAmbientOcclusion>();
            }
            if has_depth && !need_depth_prepass {
                entity_cmds.remove::<bevy::core_pipeline::prepass::DepthPrepass>();
            }
            if has_normal && !need_normal_prepass {
                entity_cmds.remove::<bevy::core_pipeline::prepass::NormalPrepass>();
            }
            if ssao_active {
                entity_cmds.insert(ScreenSpaceAmbientOcclusion {
                    quality_level: settings.ssao.to_bevy_quality(),
                    ..default()
                });
            }
            if !has_depth && need_depth_prepass {
                entity_cmds.insert(bevy::core_pipeline::prepass::DepthPrepass);
            }
            if !has_normal && need_normal_prepass {
                entity_cmds.insert(bevy::core_pipeline::prepass::NormalPrepass);
            }
        } else if ssao_active {
            // Keep SSAO quality in sync with settings.
            commands.entity(entity).insert(ScreenSpaceAmbientOcclusion {
                quality_level: settings.ssao.to_bevy_quality(),
                ..default()
            });
        }
    }
}

fn apply_bloom_settings(
    mut commands: Commands,
    settings: Res<GameSettings>,
    camera: Query<(Entity, Option<&Bloom>), With<PlayerCamera>>,
) {
    if !settings.is_changed() {
        return;
    }
    let desired = settings.bloom.to_bevy();
    for (entity, current) in &camera {
        match (&desired, current) {
            (Some(b), Some(c))
                if c.intensity != b.intensity
                    || c.prefilter.threshold != b.prefilter.threshold
                    || c.prefilter.threshold_softness != b.prefilter.threshold_softness
                    || c.low_frequency_boost != b.low_frequency_boost
                    || c.low_frequency_boost_curvature != b.low_frequency_boost_curvature
                    || c.composite_mode != b.composite_mode =>
            {
                commands.entity(entity).insert(b.clone());
            }
            (Some(b), None) => {
                commands.entity(entity).insert(b.clone());
            }
            (None, Some(_)) => {
                commands.entity(entity).remove::<Bloom>();
            }
            _ => {}
        }
    }
}

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameSettings>()
            .init_resource::<InputBindings>()
            .init_resource::<KeybindCapture>()
            .init_resource::<MenuState>()
            .init_resource::<DirectionalLightShadowMap>()
            .init_resource::<UndergroundLighting>()
            .add_systems(
                Update,
                (
                    update_underground_lighting.in_set(LightingSystems::UpdateUnderground),
                    apply_settings,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (apply_visual_settings, apply_bloom_settings)
                    .run_if(resource_changed::<GameSettings>),
            )
            // Authoritative camera requirements run every frame but only mutate
            // components when derived requirements differ. This keeps PlayerCamera
            // prepass/MSAA state correct even if components are missing/changed.
            .add_systems(Update, sync_player_camera_render_requirements);
    }
}

/// Track whether the camera is enclosed by terrain and fade lighting between the
/// surface and cave presets. The height field supplies the depth test, while
/// terrain raycasts keep entrances that still see the sky on the daylight preset.
/// The resulting sky/visibility signal also gates outdoor-only effects such as
/// light shafts.
#[allow(clippy::too_many_arguments)]
fn update_underground_lighting(
    time: Res<Time>,
    generator: Res<WorldGenResource>,
    manager: Res<ChunkManager>,
    player: Query<&Transform, With<Player>>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut state: ResMut<UndergroundLighting>,
) {
    let Ok(transform) = player.single() else {
        return;
    };
    let world = detail_to_world(transform.translation.floor().as_ivec3());
    let surface_y = generator.0.height_at(world.x, world.z) * DETAILS_PER_WORLD_AXIS;
    let eye = camera.single().ok().map(|camera| camera.translation());
    let sky_visible = eye.is_some_and(|eye| sky_is_visible(&manager, eye));
    // Match the recessed rendered surface rather than the voxel's top plane.
    let underwater = eye.is_some_and(|eye| {
        manager
            .get_block_if_loaded((eye - Vec3::Y * PANEL_HEIGHT).floor().as_ivec3())
            .is_some_and(|block| block.is_water())
    });
    let target = underground_target(
        transform.translation.y,
        surface_y as f32,
        state.underground,
        sky_visible,
    );
    state.underground = target;
    state.sky_visible = sky_visible;

    let desired = if target { 1.0 } else { 0.0 };
    state.blend = move_toward(
        state.blend,
        desired,
        time.delta_secs() * CAVE_LIGHT_FADE_RATE,
    );

    let water_target = if underwater { 1.0 } else { 0.0 };
    state.water_blend = move_toward(
        state.water_blend,
        water_target,
        time.delta_secs() * WATER_TINT_FADE_RATE,
    );
}

/// Push settings into the camera, streamer, and window whenever they differ.
/// Writing only on change avoids resetting window state every frame.
type SunLightQuery<'a> = (
    &'a mut DirectionalLight,
    &'a mut CascadeShadowConfig,
    &'a mut Transform,
);
type MoonLightQuery<'a> = (&'a mut DirectionalLight, &'a mut Transform);

#[allow(clippy::too_many_arguments)]
fn apply_settings(
    settings: Res<GameSettings>,
    cycle: Res<DayNightCycle>,
    mut camera: Query<(&mut Projection, &mut ShadowFilteringMethod), With<PlayerCamera>>,
    mut fog: Query<&mut DistanceFog, With<PlayerCamera>>,
    mut manager: ResMut<ChunkManager>,
    underground: Res<UndergroundLighting>,
    mut shadow_map: ResMut<DirectionalLightShadowMap>,
    mut sunlight: Query<SunLightQuery<'_>, (With<SunLight>, Without<MoonLight>)>,
    mut moonlight: Query<MoonLightQuery<'_>, (With<MoonLight>, Without<SunLight>)>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut clear_color: ResMut<ClearColor>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
) {
    if let Ok((mut projection, mut filtering)) = camera.single_mut() {
        if let Projection::Perspective(perspective) = &mut *projection {
            let fov = settings.fov_degrees.to_radians();
            if (perspective.fov - fov).abs() > f32::EPSILON {
                perspective.fov = fov;
            }
        }

        let desired = settings.shadow_quality.shadow_filtering();
        if *filtering != desired {
            *filtering = desired;
        }
    }

    if manager.view_distance != settings.render_distance {
        manager.view_distance = settings.render_distance;
    }

    let surface = surface_preset_for_cycle(&cycle);
    let blend = underground.blend();

    // Ambient: day/night surface preset blended with cave preset.
    let ambient_color = mix_color(surface.ambient_color, CAVE_AMBIENT_COLOR, blend);
    let ambient_brightness = mix(surface.ambient_brightness, CAVE_AMBIENT_BRIGHTNESS, blend);
    if ambient.color != ambient_color {
        ambient.color = ambient_color;
    }
    if (ambient.brightness - ambient_brightness).abs() > f32::EPSILON {
        ambient.brightness = ambient_brightness;
    }

    let clear = mix_color(surface.clear_color, CAVE_FOG_COLOR, blend);
    if clear_color.0 != clear {
        clear_color.0 = clear;
    }

    // Surface fog stays anchored to the normal streamed band, so changing pitch
    // cannot move it. Underground fog darkens and closes in to hide unloaded
    // cave space without affecting the skybox seen through a mouth.
    let fog_end = mix(manager.fog_distance(), CAVE_FOG_END, blend);
    let fog_start = fog_end * FOG_START_FRACTION;
    if let Ok(mut fog) = fog.single_mut() {
        let color = mix_color(surface.fog_color, CAVE_FOG_COLOR, blend);
        if fog.color != color {
            fog.color = color;
        }
        if let FogFalloff::Linear { start, end } = &mut fog.falloff {
            if (*start - fog_start).abs() > f32::EPSILON {
                *start = fog_start;
            }
            if (*end - fog_end).abs() > f32::EPSILON {
                *end = fog_end;
            }
        }
    }

    let map_size = settings.shadow_quality.shadow_map_size();
    if shadow_map.size != map_size {
        shadow_map.size = map_size;
    }

    let sun_dir = cycle.sun_direction();
    let sun_color = mix_color(surface.sun_color, [0.45, 0.52, 0.60], blend);
    let sun_illuminance = mix(surface.sun_illuminance, CAVE_SUN_ILLUMINANCE, blend);
    for (mut light, mut config, mut transform) in &mut sunlight {
        apply_shadow_quality(&mut light, &mut config, settings.shadow_quality);
        if light.color != sun_color {
            light.color = sun_color;
        }
        if (light.illuminance - sun_illuminance).abs() > 0.1 {
            light.illuminance = sun_illuminance;
        }
        let desired = Transform::from_translation(Vec3::ZERO).looking_to(sun_dir, Vec3::Y);
        if transform.rotation != desired.rotation {
            *transform = desired;
        }
    }

    let moon_dir = cycle.moon_direction();
    let moon_color = mix_color(surface.moon_color, [0.30, 0.36, 0.48], blend);
    let moon_illuminance = mix(surface.moon_illuminance, 0.0, blend);
    for (mut light, mut transform) in &mut moonlight {
        light.shadow_maps_enabled = false;
        if light.color != moon_color {
            light.color = moon_color;
        }
        if (light.illuminance - moon_illuminance).abs() > 0.1 {
            light.illuminance = moon_illuminance;
        }
        let desired = Transform::from_translation(Vec3::ZERO).looking_to(moon_dir, Vec3::Y);
        if transform.rotation != desired.rotation {
            *transform = desired;
        }
    }

    if let Ok(mut window) = window.single_mut() {
        let window_mode = if settings.fullscreen {
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
        } else {
            WindowMode::Windowed
        };
        if window.mode != window_mode {
            window.mode = window_mode;
            if !settings.fullscreen {
                // Entering fullscreen rewrites the resolution to the monitor
                // size, so restore a sensible windowed size on the way back.
                window
                    .resolution
                    .set(WINDOWED_WIDTH as f32, WINDOWED_HEIGHT as f32);
            }
        }

        // Reassert the present mode after any mode change: some backends reset
        // it while recreating the fullscreen surface, which silently turns
        // VSync Off back into a compositor-paced 75 FPS cap.
        let present_mode = if settings.vsync {
            PresentMode::AutoVsync
        } else {
            PresentMode::AutoNoVsync
        };
        if window.present_mode != present_mode {
            window.present_mode = present_mode;
        }
    }
}

/// Approximate RAM used by the voxel data of the worst-case stream volume for a
/// horizontal `radius`. This includes the capped normal band plus the narrow
/// downward cave tail, because that tail can activate while exploring. Each
/// cubic chunk stores a padded 34x34x34 detail grid, a padded 18x18x18 world
/// grid and a matching turf-flag grid. This is a rough figure for the slider
/// hint, not a memory budget.
pub fn estimated_chunk_memory_mb(radius: i32) -> f32 {
    use crate::world::chunk::{PADDED_VOLUME, WORLD_PADDED_VOLUME};

    let chunks = estimated_stream_chunk_count(radius.max(0), true);
    let bytes_per_chunk = (PADDED_VOLUME * 2 + WORLD_PADDED_VOLUME * 2) as f32;
    chunks as f32 * bytes_per_chunk / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_tuned_values() {
        let settings = GameSettings::default();
        assert_eq!(settings.mouse_sensitivity, DEFAULT_MOUSE_SENSITIVITY);
        assert_eq!(settings.fov_degrees, 90.0);
        assert_eq!(settings.render_distance, 8);
        assert_eq!(settings.shadow_quality, ShadowQuality::Medium);
        assert!(!settings.vsync);
        assert!(settings.fullscreen);
        assert!(!settings.perf_log);
        assert!(settings.clouds_enabled);
        assert_eq!(
            settings.light_shafts_mode,
            crate::light_shafts::LightShaftsMode::Simple
        );
        assert_eq!(settings.preferred_msaa_samples, 2);
    }

    #[test]
    fn default_bindings_use_the_map_key() {
        let bindings = InputBindings::default();
        assert_eq!(bindings.map, KeyCode::KeyM);
        assert!(
            BindingAction::ALL
                .into_iter()
                .all(|action| action.get(&bindings) != KeyCode::KeyF),
            "KeyM is the active map binding; F is unused"
        );
    }

    #[test]
    fn rebinding_swaps_conflicts_and_keeps_bindings_unique() {
        let mut bindings = InputBindings::default();
        BindingAction::Jump.set(&mut bindings, KeyCode::KeyW);

        assert_eq!(BindingAction::Jump.get(&bindings), KeyCode::KeyW);
        assert_eq!(BindingAction::MoveForward.get(&bindings), KeyCode::Space);

        let keys: Vec<KeyCode> = BindingAction::ALL
            .into_iter()
            .map(|action| action.get(&bindings))
            .collect();
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn shadow_quality_cycles_and_scales_cost() {
        assert_eq!(ShadowQuality::Off.next(), ShadowQuality::Low);
        assert_eq!(ShadowQuality::Low.next(), ShadowQuality::Medium);
        assert_eq!(ShadowQuality::Medium.next(), ShadowQuality::High);
        assert_eq!(ShadowQuality::High.next(), ShadowQuality::Off);

        assert!(ShadowQuality::Off.max_distance_m() < ShadowQuality::Medium.max_distance_m());
        assert!(ShadowQuality::Medium.max_distance_m() < ShadowQuality::High.max_distance_m());
        assert!(ShadowQuality::Medium.cascades() < ShadowQuality::High.cascades());
        assert!(ShadowQuality::Medium.shadow_map_size() < ShadowQuality::High.shadow_map_size());
        assert_eq!(
            ShadowQuality::Medium.shadow_filtering(),
            ShadowQuality::High.shadow_filtering()
        );
        assert_ne!(
            ShadowQuality::Low.shadow_filtering(),
            ShadowQuality::Medium.shadow_filtering()
        );
    }

    #[test]
    fn memory_hint_grows_with_render_distance() {
        assert!(estimated_chunk_memory_mb(16) > estimated_chunk_memory_mb(8));
    }

    #[test]
    fn memory_hint_includes_the_asymmetric_cave_tail() {
        use crate::world::chunk::{PADDED_VOLUME, WORLD_PADDED_VOLUME};

        let radius = 8;
        let normal = estimated_stream_chunk_count(radius, false);
        let with_tail = estimated_stream_chunk_count(radius, true);
        assert!(with_tail > normal);

        let bytes_per_chunk = (PADDED_VOLUME * 2 + WORLD_PADDED_VOLUME * 2) as f32;
        let expected = with_tail as f32 * bytes_per_chunk / (1024.0 * 1024.0);
        assert!((estimated_chunk_memory_mb(radius) - expected).abs() < 1e-4);
    }

    #[test]
    fn underground_state_uses_hysteresis() {
        let surface = 40.0 * BLOCKS_PER_METRE;
        assert!(!underground_target(surface, surface, false, false));
        assert!(underground_target(
            surface - 2.0 * BLOCKS_PER_METRE,
            surface,
            false,
            false
        ));
        assert!(!underground_target(
            surface - BLOCKS_PER_METRE,
            surface,
            false,
            false
        ));
        assert!(underground_target(
            surface - BLOCKS_PER_METRE,
            surface,
            true,
            false
        ));
        assert!(!underground_target(
            surface - 0.25 * BLOCKS_PER_METRE,
            surface,
            true,
            false
        ));
    }

    #[test]
    fn daylit_entrances_keep_the_surface_preset() {
        let surface = 40.0 * BLOCKS_PER_METRE;
        let inside_mouth = surface - 10.0 * BLOCKS_PER_METRE;
        assert!(!underground_target(inside_mouth, surface, false, true));
        assert!(!underground_target(inside_mouth, surface, true, true));
        assert!(underground_target(inside_mouth, surface, false, false));
    }

    #[test]
    fn underground_lighting_exposes_the_surface_signal() {
        let outdoor = UndergroundLighting::default();
        assert!(outdoor.sky_visible());
        assert_eq!(outdoor.surface_visibility(), 1.0);
        assert_eq!(outdoor.water_blend(), 0.0);

        let enclosed = UndergroundLighting {
            underground: true,
            blend: 1.0,
            sky_visible: false,
            water_blend: 0.0,
        };
        assert!(!enclosed.sky_visible());
        assert_eq!(enclosed.surface_visibility(), 0.0);

        let transitioning = UndergroundLighting {
            underground: true,
            blend: 0.25,
            sky_visible: true,
            water_blend: 0.5,
        };
        assert!(transitioning.sky_visible());
        assert_eq!(transitioning.surface_visibility(), 0.75);
        assert_eq!(transitioning.water_blend(), 0.5);
    }

    #[test]
    fn cave_lighting_interpolates_between_presets() {
        assert_eq!(mix(10.0, 20.0, 0.0), 10.0);
        assert_eq!(mix(10.0, 20.0, 1.0), 20.0);
        assert_eq!(mix(10.0, 20.0, 0.5), 15.0);
        assert_eq!(move_toward(0.0, 1.0, 0.25), 0.25);
        assert_eq!(move_toward(0.9, 1.0, 0.25), 1.0);
        assert_eq!(move_toward(1.0, 0.0, 0.25), 0.75);
        assert_eq!(move_toward(0.1, 0.0, 0.25), 0.0);

        assert_eq!(
            mix_color(SURFACE_FOG_COLOR, CAVE_FOG_COLOR, 0.0).to_srgba(),
            Color::srgb(
                SURFACE_FOG_COLOR[0],
                SURFACE_FOG_COLOR[1],
                SURFACE_FOG_COLOR[2]
            )
            .to_srgba()
        );
        const {
            assert!(CAVE_AMBIENT_BRIGHTNESS < SURFACE_AMBIENT_BRIGHTNESS);
            assert!(CAVE_SUN_ILLUMINANCE < SURFACE_SUN_ILLUMINANCE);
            assert!(CAVE_FOG_END < 210.0 * BLOCKS_PER_METRE);
            assert!(CAVE_LIGHT_FADE_RATE < 2.0);
            assert!(DAYLIGHT_RAY_LENGTH > 24.0 * BLOCKS_PER_METRE);
        }
    }
}
