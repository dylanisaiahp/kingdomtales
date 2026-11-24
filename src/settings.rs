//! Runtime-adjustable game settings and the pause-menu state.
//!
//! Values live in a resource so the settings menu, player controller, camera,
//! and chunk streamer all read from the same source.

use bevy::input::keyboard::KeyCode;
use bevy::light::{
    CascadeShadowConfig, CascadeShadowConfigBuilder, DirectionalLightShadowMap,
    ShadowFilteringMethod,
};
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};

use crate::player::{Player, PlayerCamera};
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

/// Smoothed underground state used by fog and lighting.
#[derive(Resource, Debug, Clone, Copy)]
pub struct UndergroundLighting {
    underground: bool,
    blend: f32,
}

impl Default for UndergroundLighting {
    fn default() -> Self {
        Self {
            underground: false,
            blend: 0.0,
        }
    }
}

impl UndergroundLighting {
    pub fn blend(self) -> f32 {
        self.blend
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
    a + (b - a) * t
}

#[inline]
fn mix_color(a: [f32; 3], b: [f32; 3], t: f32) -> Color {
    Color::srgb(mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t))
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
/// Mouse buttons are intentionally not rebound here yet; the important part is
/// that every gameplay key lives in one resource instead of being scattered
/// through systems. A future controls screen can edit this resource without
/// touching the systems that read it.
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
    /// Reserved for the map screen; no map system is implemented yet.
    pub map: KeyCode,
    /// Toggle HUD visibility.
    pub toggle_hud: KeyCode,
    /// Reserved for future remappable interact key; RMB is currently used.
    #[allow(dead_code)]
    pub interact: KeyCode,
    /// Place the currently selected placeable item/structure.
    pub place: KeyCode,
    /// Rotate placeable structure preview clockwise.
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
            place: KeyCode::KeyF,
            rotate_place: KeyCode::KeyR,
            save: KeyCode::F9,
            load: KeyCode::F10,
            screenshot: KeyCode::F12,
        }
    }
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameSettings>()
            .init_resource::<InputBindings>()
            .init_resource::<MenuState>()
            .init_resource::<DirectionalLightShadowMap>()
            .init_resource::<UndergroundLighting>()
            .add_systems(
                Update,
                (update_underground_lighting, apply_settings).chain(),
            );
    }
}

/// Track whether the camera is enclosed by terrain and fade lighting between the
/// surface and cave presets. The height field supplies the depth test, while
/// terrain raycasts keep entrances that still see the sky on the daylight preset.
#[allow(clippy::too_many_arguments)]
fn update_underground_lighting(
    time: Res<Time>,
    generator: Res<WorldGenResource>,
    manager: Res<ChunkManager>,
    player: Query<&Transform, With<Player>>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut state: ResMut<UndergroundLighting>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut sunlight: Query<&mut DirectionalLight, With<SunLight>>,
) {
    let Ok(transform) = player.single() else {
        return;
    };
    let world = detail_to_world(transform.translation.floor().as_ivec3());
    let surface_y = generator.0.height_at(world.x, world.z) * DETAILS_PER_WORLD_AXIS;
    let sky_visible = camera
        .single()
        .is_ok_and(|camera| sky_is_visible(&manager, camera.translation()));
    let target = underground_target(
        transform.translation.y,
        surface_y as f32,
        state.underground,
        sky_visible,
    );
    state.underground = target;

    let desired = if target { 1.0 } else { 0.0 };
    state.blend = move_toward(
        state.blend,
        desired,
        time.delta_secs() * CAVE_LIGHT_FADE_RATE,
    );

    let blend = state.blend;
    ambient.color = mix_color(SURFACE_AMBIENT_COLOR, CAVE_AMBIENT_COLOR, blend);
    ambient.brightness = mix(SURFACE_AMBIENT_BRIGHTNESS, CAVE_AMBIENT_BRIGHTNESS, blend);
    let illuminance = mix(SURFACE_SUN_ILLUMINANCE, CAVE_SUN_ILLUMINANCE, blend);
    for mut light in &mut sunlight {
        light.illuminance = illuminance;
    }
}

/// Push settings into the camera, streamer, and window whenever they differ.
/// Writing only on change avoids resetting window state every frame.
#[allow(clippy::too_many_arguments)]
fn apply_settings(
    settings: Res<GameSettings>,
    mut camera: Query<(&mut Projection, &mut ShadowFilteringMethod), With<PlayerCamera>>,
    mut fog: Query<&mut DistanceFog, With<PlayerCamera>>,
    mut manager: ResMut<ChunkManager>,
    underground: Res<UndergroundLighting>,
    mut shadow_map: ResMut<DirectionalLightShadowMap>,
    mut sunlight: Query<(&mut DirectionalLight, &mut CascadeShadowConfig), With<SunLight>>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
) {
    if let Ok((mut projection, mut filtering)) = camera.single_mut() {
        if let Projection::Perspective(perspective) = &mut *projection {
            let fov = settings.fov_degrees.to_radians();
            if (perspective.fov - fov).abs() > f32::EPSILON {
                perspective.fov = fov;
            }
        }

        if settings.is_changed() {
            let desired = settings.shadow_quality.shadow_filtering();
            if *filtering != desired {
                *filtering = desired;
            }
        }
    }

    if manager.view_distance != settings.render_distance {
        manager.view_distance = settings.render_distance;
    }

    // Surface fog stays anchored to the normal streamed band, so changing pitch
    // cannot move it. Underground fog darkens and closes in to hide unloaded
    // cave space without affecting the skybox seen through a mouth.
    let blend = underground.blend();
    let fog_end = mix(manager.fog_distance(), CAVE_FOG_END, blend);
    let fog_start = fog_end * FOG_START_FRACTION;
    if let Ok(mut fog) = fog.single_mut() {
        let color = mix_color(SURFACE_FOG_COLOR, CAVE_FOG_COLOR, blend);
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

    if settings.is_changed() {
        let map_size = settings.shadow_quality.shadow_map_size();
        if shadow_map.size != map_size {
            shadow_map.size = map_size;
        }

        for (mut light, mut config) in &mut sunlight {
            apply_shadow_quality(&mut light, &mut config, settings.shadow_quality);
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
    }

    #[test]
    fn default_bindings_reserve_the_map_key() {
        assert_eq!(InputBindings::default().map, KeyCode::KeyM);
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
