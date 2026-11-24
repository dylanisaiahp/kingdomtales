//! 3D overhead world map (Phase 1).
//!
//! Press M to open a north-up overhead view rendered by a dedicated camera into
//! an offscreen texture displayed in a UI overlay. The map reuses the existing
//! loaded world/chunk rendering; it does not create world data.
//!
//! Controls while open: drag to pan, scroll to zoom, click/press Recenter to
//! return to the player. Esc closes the map. Fog of war/persistence is Phase 2.

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Camera, Camera3d, ClearColorConfig, RenderTarget};
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::math::Rot2;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::Msaa;
use bevy::ui::{Pressed, UiTransform};
use bevy::ui_widgets::{Activate, Button};
use bevy::window::PrimaryWindow;

use crate::player::Player;
use crate::ui::theme::{UiTheme, spawn_keycap, spawn_panel, spawn_text};
use crate::ui_manager::{Overlay, UiManager};
use crate::world::MapColumn;
use crate::world::block::{Block, Shape};
use crate::world::chunk::{
    BLOCKS_PER_METRE, CHUNK_X, CHUNK_Y, CHUNK_Z, SEA_LEVEL_WORLD, detail_to_chunk,
};
use crate::world::generation::MapFloraKind;
use crate::world::generation_data::BiomeId;
use crate::world::{ChunkManager, WorldGenResource};

const MAP_IMAGE_WIDTH: u32 = 1280;
const MAP_IMAGE_HEIGHT: u32 = 720;
const MAP_VIEW_PADDING: f32 = 24.0;
const MIN_ZOOM: f32 = 40.0 * BLOCKS_PER_METRE;
const MAX_ZOOM: f32 = 420.0 * BLOCKS_PER_METRE;
const DEFAULT_ZOOM: f32 = 180.0 * BLOCKS_PER_METRE;
const PAN_SPEED: f32 = 1.0;
const TRANSITION_SPEED: f32 = 8.0;
/// Exploration grid cell size in world metres. Smaller = more detail, more data.
const EXPLORE_CELL_METRES: f32 = 4.0;
/// Radius around player marked currently visible (metres).
const EXPLORE_VISIBLE_RADIUS_M: f32 = 48.0;
/// Radius around player marked explored (metres).
const EXPLORE_DISCOVER_RADIUS_M: f32 = 64.0;
const FOG_IMAGE_WIDTH: u32 = 1280;
const FOG_IMAGE_HEIGHT: u32 = 720;
const FOG_UPDATE_INTERVAL: f32 = 0.12; // ~8 fps updates
const BUFFER_MARGIN: f32 = 0.25; // balance: fewer full regens, nearest sampling limits blur

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapSystems;

#[derive(Component)]
struct MapRoot;

#[derive(Component)]
struct MapViewportImage;

#[derive(Component)]
struct MapExploreImage;

#[derive(Component)]
struct MapModeTab {
    mode: MapMode,
}

#[derive(Component)]
struct MapModeTabsRoot;

#[derive(Component)]
struct MapPlayerMarker;

// Direction is encoded in the marker shape/rotation.

#[derive(Component)]
struct MapStatusText;

#[derive(Component)]
struct MapViewport;

// Recenter is keyboard-only (C/Home) to keep map UI minimal.

#[derive(Component)]
struct MapCamera;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMode {
    Overhead3D,
    TopDown2D,
}

#[derive(Resource)]
pub struct MapState {
    pub image: Handle<Image>,
    pub explore_image: Handle<Image>,
    pub marker_image: Handle<Image>,
    pub mode: MapMode,
    pub initialized: bool,
    pub target_open: bool,
    pub open_amount: f32,
    pub center_xz: Vec2,
    pub zoom: f32,
    pub dragging: bool,
    pub last_cursor: Option<Vec2>,
    // perf/debug
    pub perf_accum_terrain_ms: f32,
    pub perf_accum_fog_ms: f32,
    pub perf_accum_upload_ms: f32,
    pub perf_frames: u32,
    pub perf_timer: f32,
    pub last_fog_center: Vec2,
    pub last_fog_zoom: f32,
    pub last_fog_mode: Option<MapMode>,
    pub last_explored_len: usize,
    pub last_visible_signature: (IVec2, IVec2, usize),
    pub last_fog_update: f32,
    // Explore render buffer: larger than viewport for smooth panning
    pub buffer_center_xz: Vec2,
    pub buffer_zoom: f32,
    pub buffer_valid: bool,
    pub raw_explore: Vec<u8>, // composited RGBA sent to GPU
    pub raw_terrain: Vec<u8>, // CPU-only: RGB terrain, A=255 explored, A=0 unexplored
    /// Cached worldgen samples by block (x,z) for explored rendering.
    pub sample_cache: HashMap<IVec2, MapColumn>,
    pub flora_cache: HashMap<IVec2, MapFloraKind>,
}

/// Persistent explored-map data (Phase 2). Independent of loaded chunks.
#[derive(Resource, Default)]
pub struct MapExploration {
    /// Explored cells in exploration-grid coordinates.
    pub explored: HashSet<IVec2>,
    /// Currently visible cells, recomputed periodically from player position.
    pub visible: HashSet<IVec2>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            image: Handle::default(),
            explore_image: Handle::default(),
            marker_image: Handle::default(),
            mode: MapMode::Overhead3D,
            initialized: false,
            target_open: false,
            open_amount: 0.0,
            center_xz: Vec2::ZERO,
            zoom: DEFAULT_ZOOM,
            dragging: false,
            last_cursor: None,
            perf_accum_terrain_ms: 0.0,
            perf_accum_fog_ms: 0.0,
            perf_accum_upload_ms: 0.0,
            perf_frames: 0,
            perf_timer: 0.0,
            last_fog_center: Vec2::splat(f32::NAN),
            last_fog_zoom: f32::NAN,
            last_fog_mode: None,
            last_explored_len: 0,
            last_visible_signature: (IVec2::splat(i32::MIN), IVec2::splat(i32::MIN), 0),
            last_fog_update: -1000.0,
            buffer_center_xz: Vec2::splat(f32::NAN),
            buffer_zoom: f32::NAN,
            buffer_valid: false,
            raw_explore: Vec::new(),
            raw_terrain: Vec::new(),
            sample_cache: HashMap::new(),
            flora_cache: HashMap::new(),
        }
    }
}

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapState>()
            .init_resource::<MapExploration>()
            .add_systems(
                Update,
                (
                    sync_map_target,
                    animate_map_transition,
                    handle_map_drag,
                    handle_map_zoom,
                    update_map_mode,
                    update_map_exploration,
                    update_fog_image,
                    update_map_camera,
                    update_map_ui,
                    style_map_mode_tabs,
                )
                    .chain()
                    .in_set(MapSystems),
            );
        app.add_observer(on_map_mode_tab_activate);
        app.add_systems(Startup, (setup_map_render_target, setup_map_ui).chain());
    }
}

fn setup_map_render_target(mut images: ResMut<Assets<Image>>, mut state: ResMut<MapState>) {
    let size = Extent3d {
        width: MAP_IMAGE_WIDTH,
        height: MAP_IMAGE_HEIGHT,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    state.image = images.add(image);

    // Up-pointing player marker arrow, rotated in UI by player yaw.
    let mw: u32 = 32;
    let mh: u32 = 32;
    let mut data = vec![0u8; (mw * mh * 4) as usize];
    let tip = Vec2::new(0.0, -0.72);
    let br = Vec2::new(0.56, 0.62);
    let bl = Vec2::new(-0.56, 0.62);
    let round = 0.26;
    let sd_segment = |p: Vec2, a: Vec2, b: Vec2| -> f32 {
        let pa = p - a;
        let ba = b - a;
        let h = (pa.dot(ba) / ba.dot(ba)).clamp(0.0, 1.0);
        (pa - ba * h).length()
    };
    let sd_tri = |p: Vec2| -> f32 {
        let d0 = sd_segment(p, tip, br);
        let d1 = sd_segment(p, br, bl);
        let d2 = sd_segment(p, bl, tip);
        let d = d0.min(d1).min(d2);
        // Sign: negative inside triangle.
        let e0 = br - tip;
        let e1 = bl - br;
        let e2 = tip - bl;
        let s0 = e0.x * (p.y - tip.y) - e0.y * (p.x - tip.x);
        let s1 = e1.x * (p.y - br.y) - e1.y * (p.x - br.x);
        let s2 = e2.x * (p.y - bl.y) - e2.y * (p.x - bl.x);
        let inside = (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0) || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0);
        if inside { -d } else { d }
    };
    for y in 0..mh {
        for x in 0..mw {
            let fx = (x as f32 + 0.5) / mw as f32 * 2.0 - 1.0;
            let fy = (y as f32 + 0.5) / mh as f32 * 2.0 - 1.0;
            let p = Vec2::new(fx, fy);
            let d = sd_tri(p) - round;
            let border = 1.0 - ((d - 0.02) / 0.08).clamp(0.0, 1.0);
            let fill = 1.0 - (d / 0.08).clamp(0.0, 1.0);
            let idx = ((y * mw + x) * 4) as usize;
            if border > 0.0 {
                let fill_c = Vec3::new(0.91, 0.77, 0.33);
                let border_c = Vec3::new(1.0, 1.0, 1.0);
                let c = border_c.lerp(fill_c, fill);
                let a = border;
                data[idx] = (c.x * 255.0) as u8;
                data[idx + 1] = (c.y * 255.0) as u8;
                data[idx + 2] = (c.z * 255.0) as u8;
                data[idx + 3] = (a * 245.0) as u8;
            }
        }
    }
    let mut marker = Image::new(
        Extent3d {
            width: mw,
            height: mh,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    marker.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    state.marker_image = images.add(marker);

    let explore_size = Extent3d {
        width: FOG_IMAGE_WIDTH,
        height: FOG_IMAGE_HEIGHT,
        depth_or_array_layers: 1,
    };
    let mut explore = Image::new_fill(
        explore_size,
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    explore.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    explore.sampler = bevy::image::ImageSampler::nearest();
    state.explore_image = images.add(explore);
}

fn setup_map_ui(mut commands: Commands, theme: Res<UiTheme>, state: Res<MapState>) {
    // Dedicated map camera renders world into offscreen texture only.
    commands.spawn((
        Name::new("MapCamera"),
        MapCamera,
        Camera3d::default(),
        Camera {
            order: -10,
            is_active: false,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(state.image.clone().into()),
        Projection::Perspective(PerspectiveProjection {
            fov: 35f32.to_radians(),
            aspect_ratio: MAP_IMAGE_WIDTH as f32 / MAP_IMAGE_HEIGHT as f32,
            near: 0.5 * BLOCKS_PER_METRE,
            far: 900.0 * BLOCKS_PER_METRE,
            ..default()
        }),
        Transform::from_xyz(0.0, DEFAULT_ZOOM, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z),
        Msaa::Off,
    ));

    let root = commands
        .spawn((
            Name::new("MapOverlay"),
            MapRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::None,
                ..default()
            },
            // Full-screen translucent scrim (Bevy UI has no backdrop blur here).
            BackgroundColor(Color::srgba(39.0 / 255.0, 57.0 / 255.0, 77.0 / 255.0, 0.85)),
            GlobalZIndex(theme.layers.dock + 1),
        ))
        .id();

    let viewport_frame = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(MAP_VIEW_PADDING),
                top: Val::Px(MAP_VIEW_PADDING),
                right: Val::Px(MAP_VIEW_PADDING),
                bottom: Val::Px(MAP_VIEW_PADDING + 56.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();

    let viewport = commands
        .spawn((
            MapViewport,
            Node {
                width: Val::Percent(100.0),
                height: Val::Auto,
                max_height: Val::Percent(100.0),
                aspect_ratio: Some(MAP_IMAGE_WIDTH as f32 / MAP_IMAGE_HEIGHT as f32),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::NONE),
            Hovered::default(),
            ChildOf(viewport_frame),
        ))
        .id();

    // Top-center mode tabs.
    let tabs_root = commands
        .spawn((
            MapModeTabsRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(MAP_VIEW_PADDING + 8.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    let tabs_panel = spawn_panel(
        &mut commands,
        &theme,
        tabs_root,
        Node {
            padding: UiRect::axes(Val::Px(4.0), Val::Px(4.0)),
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(4.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );
    for (mode, label) in [
        (MapMode::Overhead3D, "Live"),
        (MapMode::TopDown2D, "Explore"),
    ] {
        let tab = spawn_panel(
            &mut commands,
            &theme,
            tabs_panel,
            Node {
                width: Val::Px(88.0),
                height: Val::Px(28.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: theme.shape.border(),
                border_radius: theme.shape.rounded_small(),
                ..default()
            },
            theme.colors.button_bg,
            theme.colors.border_idle,
        );
        commands
            .entity(tab)
            .insert((Button, MapModeTab { mode }, Hovered::default()));
        spawn_text(
            &mut commands,
            &theme,
            tab,
            label,
            theme.fonts.small,
            theme.colors.text,
            MapStatusText,
        );
    }

    // Bottom-center controls, styled like HUD control panels.
    let controls = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(MAP_VIEW_PADDING + 10.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();
    let controls_panel = spawn_panel(
        &mut commands,
        &theme,
        controls,
        Node {
            padding: UiRect::axes(Val::Px(theme.spacing.sm), Val::Px(theme.spacing.xs + 2.0)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(14.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );

    let hints = [
        ("Drag", "Pan"),
        ("Scroll", "Zoom"),
        ("C", "Recenter"),
        ("M", "Close"),
    ];
    for (key, action) in hints {
        let row = commands
            .spawn((
                Node {
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(controls_panel),
            ))
            .id();
        let key_width = if key.len() > 6 { 72.0 } else { 46.0 };
        spawn_keycap(&mut commands, &theme, row, key, key_width);
        spawn_text(
            &mut commands,
            &theme,
            row,
            action,
            theme.fonts.small,
            theme.colors.muted_text,
            MapStatusText,
        );
    }

    commands.spawn((
        MapViewportImage,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        ImageNode::new(state.image.clone()),
        Pickable::IGNORE,
        ChildOf(viewport),
    ));

    commands.spawn((
        MapExploreImage,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        ImageNode::new(state.explore_image.clone()),
        Pickable::IGNORE,
        ChildOf(viewport),
    ));

    // Player marker: rounded triangle/arrowhead, rotated to facing direction.
    // 0 rotation points "forward" via the sharp bottom-right corner after -45deg offset.
    commands.spawn((
        MapPlayerMarker,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Px(20.0),
            height: Val::Px(20.0),
            margin: UiRect {
                left: Val::Px(-10.0),
                top: Val::Px(-10.0),
                ..default()
            },
            border: UiRect::all(Val::Px(2.0)),
            border_radius: BorderRadius {
                top_left: Val::Px(10.0).into(),
                top_right: Val::Px(10.0).into(),
                bottom_left: Val::Px(10.0).into(),
                bottom_right: Val::Px(3.0).into(),
            },
            ..default()
        },
        BackgroundColor(theme.colors.accent),
        BorderColor::all(theme.colors.text.with_alpha(0.95)),
        UiTransform::default(),
        Pickable::IGNORE,
        ChildOf(viewport),
    ));
}

fn sync_map_target(
    ui: Res<UiManager>,
    mut state: ResMut<MapState>,
    player: Query<&Transform, With<Player>>,
) {
    let open = ui.is_open(Overlay::Map);
    if state.target_open != open {
        state.target_open = open;
        if open {
            if let Ok(p) = player.single() {
                state.center_xz = Vec2::new(p.translation.x, p.translation.z);
                state.initialized = true;
            }
        } else {
            state.dragging = false;
            state.last_cursor = None;
        }
    }
}

fn animate_map_transition(time: Res<Time>, mut state: ResMut<MapState>) {
    let target = if state.target_open { 1.0 } else { 0.0 };
    let delta = (target - state.open_amount).clamp(-1.0, 1.0);
    let step = TRANSITION_SPEED * time.delta_secs();
    if delta.abs() <= step {
        state.open_amount = target;
    } else {
        state.open_amount += delta.signum() * step;
    }
}

fn handle_map_drag(
    ui: Res<UiManager>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    viewport: Query<(&Hovered, &ComputedNode), With<MapViewport>>,
    mut state: ResMut<MapState>,
) {
    if !ui.is_open(Overlay::Map) {
        state.dragging = false;
        state.last_cursor = None;
        return;
    }
    let Ok(window) = window.single() else { return };
    let Some(cursor) = window.cursor_position() else {
        state.dragging = false;
        state.last_cursor = None;
        return;
    };

    let (viewport_hovered, viewport_size) = viewport
        .iter()
        .next()
        .map(|(h, n)| (h.0, n.size()))
        .unwrap_or((false, Vec2::new(1024.0, 1024.0)));
    if mouse.just_pressed(MouseButton::Left) && viewport_hovered {
        state.dragging = true;
        state.last_cursor = Some(cursor);
    }
    if !mouse.pressed(MouseButton::Left) {
        state.dragging = false;
        state.last_cursor = None;
        return;
    }
    if !state.dragging {
        return;
    }
    let Some(last) = state.last_cursor else {
        state.last_cursor = Some(cursor);
        return;
    };
    let delta = cursor - last;
    state.last_cursor = Some(cursor);
    if delta.length_squared() < f32::EPSILON {
        return;
    }

    // North-up: screen +x => world +x, screen +y => world +z.
    let world_size = visible_world_size(&state);
    let px_size = viewport_size.x.max(1.0);
    let py_size = viewport_size.y.max(1.0);
    state.center_xz.x -= delta.x * (world_size.x / px_size) * PAN_SPEED;
    state.center_xz.y -= delta.y * (world_size.y / py_size) * PAN_SPEED;
}

fn visible_world_size(state: &MapState) -> Vec2 {
    let fov = 35f32.to_radians();
    let height = 2.0 * state.zoom * (fov * 0.5).tan();
    let width = height * (MAP_IMAGE_WIDTH as f32 / MAP_IMAGE_HEIGHT as f32);
    Vec2::new(width, height)
}

fn world_to_explore_cell(pos_xz: Vec2) -> IVec2 {
    let s = EXPLORE_CELL_METRES * BLOCKS_PER_METRE;
    IVec2::new((pos_xz.x / s).floor() as i32, (pos_xz.y / s).floor() as i32)
}

fn explore_cell_center(cell: IVec2) -> Vec2 {
    let s = EXPLORE_CELL_METRES * BLOCKS_PER_METRE;
    Vec2::new((cell.x as f32 + 0.5) * s, (cell.y as f32 + 0.5) * s)
}

fn update_map_mode(
    keys: Res<ButtonInput<KeyCode>>,
    ui: Res<UiManager>,
    mut state: ResMut<MapState>,
) {
    if !ui.is_open(Overlay::Map) {
        return;
    }
    if keys.just_pressed(KeyCode::Tab) {
        state.mode = match state.mode {
            MapMode::Overhead3D => MapMode::TopDown2D,
            MapMode::TopDown2D => MapMode::Overhead3D,
        };
    }
}

fn sample_column(
    cache: &mut HashMap<IVec2, MapColumn>,
    generator: &WorldGenResource,
    x: i32,
    z: i32,
) -> MapColumn {
    let key = IVec2::new(x, z);
    if let Some(c) = cache.get(&key) {
        return *c;
    }
    let c = generator.0.map_column_at(x, z);
    cache.insert(key, c);
    c
}

#[inline]
fn hash01(x: i32, z: i32, salt: u32) -> f32 {
    let mut n = (x as u32).wrapping_mul(374761393)
        ^ (z as u32).wrapping_mul(668265263)
        ^ salt.wrapping_mul(2246822519);
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    n ^= n >> 16;
    (n & 0xFFFF) as f32 / 65535.0
}

fn biome_base_color(col: MapColumn) -> [f32; 3] {
    if col.height < SEA_LEVEL_WORLD || col.surface.is_water() {
        let depth = (SEA_LEVEL_WORLD - col.height).max(0) as f32;
        let t = (depth / 18.0).clamp(0.0, 1.0);
        let shallow = [76.0, 148.0, 176.0];
        let deep = [24.0, 58.0, 110.0];
        return [
            shallow[0] + (deep[0] - shallow[0]) * t,
            shallow[1] + (deep[1] - shallow[1]) * t,
            shallow[2] + (deep[2] - shallow[2]) * t,
        ];
    }
    let shore = (col.height - SEA_LEVEL_WORLD) as f32;
    if shore <= 1.5 && matches!(col.surface, Block::Sand | Block::RedSandstone) {
        return [194.0, 178.0, 132.0];
    }
    match col.biome {
        BiomeId::Desert => [198.0, 168.0, 112.0],
        BiomeId::Mesa => [176.0, 118.0, 86.0],
        BiomeId::Alpine => {
            if col.has_snow {
                [214.0, 220.0, 228.0]
            } else {
                [142.0, 136.0, 128.0]
            }
        }
        BiomeId::Plains => {
            if col.has_turf {
                [108.0, 150.0, 78.0]
            } else {
                [140.0, 128.0, 96.0]
            }
        }
        BiomeId::Forest => {
            if col.has_turf {
                [72.0, 114.0, 62.0]
            } else {
                [98.0, 90.0, 72.0]
            }
        }
    }
}

#[allow(dead_code)]
fn flora_from_loaded_world(
    manager: &ChunkManager,
    wx: i32,
    wz: i32,
    surface_height_m: i32,
) -> Option<MapFloraKind> {
    let base_x = wx * BLOCKS_PER_METRE as i32;
    let base_z = wz * BLOCKS_PER_METRE as i32;
    let probe = IVec3::new(base_x, surface_height_m * BLOCKS_PER_METRE as i32, base_z);
    let coord = detail_to_chunk(probe);
    if !manager.chunks.contains_key(&coord) {
        return None;
    }
    // Scan only the plausible flora band above surface. Early exit on first hit.
    let y_min = (surface_height_m + 1) * BLOCKS_PER_METRE as i32;
    let y_max = (surface_height_m + 16) * BLOCKS_PER_METRE as i32;
    for y in (y_min..=y_max).rev() {
        for lx in 0..BLOCKS_PER_METRE as i32 {
            for lz in 0..BLOCKS_PER_METRE as i32 {
                let Some(voxel) =
                    manager.get_voxel_if_loaded(IVec3::new(base_x + lx, y, base_z + lz))
                else {
                    continue;
                };
                if voxel.is_air() {
                    continue;
                }
                let shape = voxel.shape();
                if voxel.block() == Block::Leaves || matches!(shape, Shape::Foliage) {
                    return Some(MapFloraKind::Tree);
                }
                if matches!(shape, Shape::SmallFoliage) {
                    return Some(MapFloraKind::Bush);
                }
                if matches!(shape, Shape::Trunk | Shape::DebarkedTrunk) {
                    return Some(MapFloraKind::Tree);
                }
            }
        }
    }
    Some(MapFloraKind::None)
}
fn terrain_color_at(
    cache: &mut HashMap<IVec2, MapColumn>,
    generator: &WorldGenResource,
    wx: i32,
    wz: i32,
) -> [u8; 4] {
    let col = sample_column(cache, generator, wx, wz);
    let mut rgb = biome_base_color(col);

    let elev = (col.height - SEA_LEVEL_WORLD) as f32;
    let elev_t = (elev / 80.0).clamp(-0.5, 1.0);
    let elev_factor = 1.0 + elev_t * 0.18;
    rgb[0] *= elev_factor;
    rgb[1] *= elev_factor;
    rgb[2] *= elev_factor;

    let h_l = sample_column(cache, generator, wx - 1, wz).height as f32;
    let h_r = sample_column(cache, generator, wx + 1, wz).height as f32;
    let h_u = sample_column(cache, generator, wx, wz - 1).height as f32;
    let h_d = sample_column(cache, generator, wx, wz + 1).height as f32;
    let shade = ((h_r - h_l) * -0.08 + (h_d - h_u) * -0.08).clamp(-0.35, 0.35);
    let hill = 1.0 + shade;
    rgb[0] *= hill;
    rgb[1] *= hill;
    rgb[2] *= hill;

    if col.height >= SEA_LEVEL_WORLD && col.supports_flora {
        let n = hash01(wx, wz, 0x0A11_CE55);
        if col.biome == BiomeId::Forest {
            let canopy = (col.forest * 0.7 + n * 0.3).clamp(0.0, 1.0);
            let dark = 0.75 + (1.0 - canopy) * 0.25;
            rgb[0] *= dark * 0.92;
            rgb[1] *= dark;
            rgb[2] *= dark * 0.90;
            if n > 0.82 {
                rgb[0] *= 0.88;
                rgb[1] *= 0.92;
                rgb[2] *= 0.86;
            }
        } else if col.biome == BiomeId::Plains && col.has_turf {
            let g = 0.95 + n * 0.1;
            rgb[0] *= 0.97;
            rgb[1] *= g;
            rgb[2] *= 0.95;
        } else if col.biome == BiomeId::Desert {
            let d = 0.95 + n * 0.08;
            rgb[0] *= d;
            rgb[1] *= 0.96 + n * 0.05;
            rgb[2] *= 0.93;
        }
    }

    if col.height >= SEA_LEVEL_WORLD
        && col.height <= SEA_LEVEL_WORLD + 1
        && matches!(col.surface, Block::Sand | Block::RedSandstone)
    {
        rgb[0] = rgb[0] * 0.85 + 210.0 * 0.15;
        rgb[1] = rgb[1] * 0.85 + 196.0 * 0.15;
        rgb[2] = rgb[2] * 0.85 + 150.0 * 0.15;
    }

    [
        rgb[0].clamp(0.0, 255.0) as u8,
        rgb[1].clamp(0.0, 255.0) as u8,
        rgb[2].clamp(0.0, 255.0) as u8,
        255,
    ]
}

fn update_map_exploration(
    player: Query<&Transform, With<Player>>,
    mut exploration: ResMut<MapExploration>,
) {
    let Ok(tf) = player.single() else { return };
    let pos = Vec2::new(tf.translation.x, tf.translation.z);
    let discover_r = EXPLORE_DISCOVER_RADIUS_M * BLOCKS_PER_METRE;
    let visible_r = EXPLORE_VISIBLE_RADIUS_M * BLOCKS_PER_METRE;
    let max_r = discover_r.max(visible_r);
    let min_cell = world_to_explore_cell(pos - Vec2::splat(max_r));
    let max_cell = world_to_explore_cell(pos + Vec2::splat(max_r));
    exploration.visible.clear();
    for z in min_cell.y..=max_cell.y {
        for x in min_cell.x..=max_cell.x {
            let cell = IVec2::new(x, z);
            let center = explore_cell_center(cell);
            let dist = pos.distance(center);
            if dist <= discover_r {
                exploration.explored.insert(cell);
                if dist <= visible_r {
                    exploration.visible.insert(cell);
                }
            }
        }
    }
}

fn visible_signature(exploration: &MapExploration) -> (IVec2, IVec2, usize) {
    let mut min = IVec2::splat(i32::MAX);
    let mut max = IVec2::splat(i32::MIN);
    for c in &exploration.visible {
        min = min.min(*c);
        max = max.max(*c);
    }
    if exploration.visible.is_empty() {
        min = IVec2::splat(i32::MIN);
        max = IVec2::splat(i32::MIN);
    }
    (min, max, exploration.visible.len())
}

fn update_fog_image(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    exploration: Res<MapExploration>,
    generator: Res<WorldGenResource>,
    manager: Res<ChunkManager>,
    mut images: ResMut<Assets<Image>>,
) {
    state.perf_timer += time.delta_secs();
    if state.mode != MapMode::TopDown2D {
        state.last_fog_mode = Some(state.mode);
        return;
    }

    let vis = visible_world_size(&state);
    let buffer_world_w = vis.x * (1.0 + BUFFER_MARGIN * 2.0);
    let buffer_world_h = vis.y * (1.0 + BUFFER_MARGIN * 2.0);

    let zoom_changed = (state.zoom - state.last_fog_zoom).abs() > 0.01;
    let vis_sig = visible_signature(&exploration);
    let explored_changed = state.last_explored_len != exploration.explored.len();
    let visible_changed = state.last_visible_signature != vis_sig;
    let mode_changed = state.last_fog_mode != Some(MapMode::TopDown2D);

    let buffer_covers = state.buffer_valid
        && !zoom_changed
        && (state.center_xz.x - state.buffer_center_xz.x).abs() <= vis.x * BUFFER_MARGIN * 0.5
        && (state.center_xz.y - state.buffer_center_xz.y).abs() <= vis.y * BUFFER_MARGIN * 0.5;

    let need_terrain_full = mode_changed || zoom_changed || !buffer_covers;
    let need_terrain_patch = !need_terrain_full && explored_changed;
    let need_fog = need_terrain_full || need_terrain_patch || visible_changed;
    if !need_fog {
        if state.perf_timer >= 1.0 {
            info!(
                "map perf: cached (0.00ms gen/upload) over {:.2}s",
                state.perf_timer
            );
            state.perf_accum_terrain_ms = 0.0;
            state.perf_accum_fog_ms = 0.0;
            state.perf_accum_upload_ms = 0.0;
            state.perf_frames = 0;
            state.perf_timer = 0.0;
        }
        return;
    }

    let now = time.elapsed_secs();
    if !need_terrain_full && need_fog && now - state.last_fog_update < FOG_UPDATE_INTERVAL {
        return;
    }

    if need_terrain_full {
        state.buffer_center_xz = state.center_xz;
        state.buffer_zoom = state.zoom;
        state.buffer_valid = true;
        state.flora_cache.clear();
    }

    let start_total = std::time::Instant::now();
    let w = FOG_IMAGE_WIDTH as i32;
    let h = FOG_IMAGE_HEIGHT as i32;
    let expected = (w as usize) * (h as usize) * 4;
    if state.raw_terrain.len() != expected {
        state.raw_terrain = [0u8, 0, 0, 0].repeat(expected / 4);
    }
    if state.raw_explore.len() != expected {
        state.raw_explore = [0u8, 0, 0, 0].repeat(expected / 4);
    }

    let mut terrain = std::mem::take(&mut state.raw_terrain);
    let mut out = std::mem::take(&mut state.raw_explore);
    if terrain.len() != expected {
        terrain = [0u8, 0, 0, 0].repeat(expected / 4);
    }
    if out.len() != expected {
        out = [0u8, 0, 0, 0].repeat(expected / 4);
    }

    let t0 = std::time::Instant::now();
    let regen_content = need_terrain_full || need_terrain_patch || need_fog;

    if regen_content {
        let cell_size = EXPLORE_CELL_METRES * BLOCKS_PER_METRE;
        let min_cell = world_to_explore_cell(Vec2::new(
            state.buffer_center_xz.x - buffer_world_w * 0.5,
            state.buffer_center_xz.y - buffer_world_h * 0.5,
        ));
        let max_cell = world_to_explore_cell(Vec2::new(
            state.buffer_center_xz.x + buffer_world_w * 0.5,
            state.buffer_center_xz.y + buffer_world_h * 0.5,
        ));

        for cz in min_cell.y..=max_cell.y {
            for cx in min_cell.x..=max_cell.x {
                let cell = IVec2::new(cx, cz);
                let explored = exploration.explored.contains(&cell);
                let wx0 = cx as f32 * cell_size;
                let wz0 = cz as f32 * cell_size;
                let wx1 = wx0 + cell_size;
                let wz1 = wz0 + cell_size;
                let px0 = (((wx0 - (state.buffer_center_xz.x - buffer_world_w * 0.5))
                    / buffer_world_w)
                    * w as f32)
                    .floor() as i32;
                let px1 = (((wx1 - (state.buffer_center_xz.x - buffer_world_w * 0.5))
                    / buffer_world_w)
                    * w as f32)
                    .ceil() as i32;
                let py0 = (((wz0 - (state.buffer_center_xz.y - buffer_world_h * 0.5))
                    / buffer_world_h)
                    * h as f32)
                    .floor() as i32;
                let py1 = (((wz1 - (state.buffer_center_xz.y - buffer_world_h * 0.5))
                    / buffer_world_h)
                    * h as f32)
                    .ceil() as i32;
                let px0 = px0.clamp(0, w);
                let px1 = px1.clamp(0, w);
                let py0 = py0.clamp(0, h);
                let py1 = py1.clamp(0, h);
                if px0 >= px1 || py0 >= py1 {
                    continue;
                }
                if !(need_terrain_full || (need_terrain_patch && explored)) {
                    continue;
                }

                let mut last_wx_i = i32::MIN;
                let mut last_wz_i = i32::MIN;
                let mut last_c = [12u8, 14u8, 18u8, 255u8];
                for py in py0..py1 {
                    for px in px0..px1 {
                        let idx = ((py * w + px) * 4) as usize;
                        if explored {
                            let u = (px as f32 + 0.5) / w as f32;
                            let v = (py as f32 + 0.5) / h as f32;
                            let wxp = state.buffer_center_xz.x + (u - 0.5) * buffer_world_w;
                            let wzp = state.buffer_center_xz.y + (v - 0.5) * buffer_world_h;
                            let wx_i = (wxp / BLOCKS_PER_METRE).floor() as i32;
                            let wz_i = (wzp / BLOCKS_PER_METRE).floor() as i32;
                            let c = if wx_i == last_wx_i && wz_i == last_wz_i {
                                last_c
                            } else {
                                let c = terrain_color_at(
                                    &mut state.sample_cache,
                                    &generator,
                                    wx_i,
                                    wz_i,
                                );
                                last_wx_i = wx_i;
                                last_wz_i = wz_i;
                                last_c = c;
                                c
                            };
                            if idx + 3 < terrain.len() {
                                terrain[idx] = c[0];
                                terrain[idx + 1] = c[1];
                                terrain[idx + 2] = c[2];
                                terrain[idx + 3] = 255;
                            }
                        } else if idx + 3 < terrain.len() {
                            terrain[idx] = 0;
                            terrain[idx + 1] = 0;
                            terrain[idx + 2] = 0;
                            terrain[idx + 3] = 0;
                        }
                    }
                }
            }
        }

        // Flora overlay from actual loaded chunk data. Scanned on terrain regen only.
        if need_terrain_full || need_terrain_patch {
            let px_per_m = (w as f32 / buffer_world_w).max(0.001);
            let min_wx = ((state.buffer_center_xz.x - buffer_world_w * 0.5) / BLOCKS_PER_METRE)
                .floor() as i32
                - 1;
            let max_wx = ((state.buffer_center_xz.x + buffer_world_w * 0.5) / BLOCKS_PER_METRE)
                .ceil() as i32
                + 1;
            let min_wz = ((state.buffer_center_xz.y - buffer_world_h * 0.5) / BLOCKS_PER_METRE)
                .floor() as i32
                - 1;
            let max_wz = ((state.buffer_center_xz.y + buffer_world_h * 0.5) / BLOCKS_PER_METRE)
                .ceil() as i32
                + 1;
            let min_detail_x = min_wx * BLOCKS_PER_METRE as i32;
            let max_detail_x = (max_wx + 1) * BLOCKS_PER_METRE as i32;
            let min_detail_z = min_wz * BLOCKS_PER_METRE as i32;
            let max_detail_z = (max_wz + 1) * BLOCKS_PER_METRE as i32;

            for (coord, chunk) in manager.chunks.iter() {
                let chunk_min_x = coord.x * CHUNK_X as i32;
                let chunk_max_x = chunk_min_x + CHUNK_X as i32;
                let chunk_min_z = coord.z * CHUNK_Z as i32;
                let chunk_max_z = chunk_min_z + CHUNK_Z as i32;
                if chunk_max_x < min_detail_x
                    || chunk_min_x > max_detail_x
                    || chunk_max_z < min_detail_z
                    || chunk_min_z > max_detail_z
                {
                    continue;
                }
                for lz in 0..CHUNK_Z {
                    for lx in 0..CHUNK_X {
                        let world_x = chunk_min_x + lx as i32;
                        let world_z = chunk_min_z + lz as i32;
                        let wx_i = (world_x as f32 / BLOCKS_PER_METRE).floor() as i32;
                        let wz_i = (world_z as f32 / BLOCKS_PER_METRE).floor() as i32;
                        let wxp_center = (wx_i as f32 + 0.5) * BLOCKS_PER_METRE;
                        let wzp_center = (wz_i as f32 + 0.5) * BLOCKS_PER_METRE;
                        let cell = world_to_explore_cell(Vec2::new(wxp_center, wzp_center));
                        if !exploration.explored.contains(&cell) {
                            continue;
                        }

                        let mut flora = MapFloraKind::None;
                        // Scan down column in this chunk; find topmost flora shape.
                        for ly in (0..CHUNK_Y).rev() {
                            let voxel = chunk.data.get_local(lx, ly, lz);
                            if voxel.is_air() {
                                continue;
                            }
                            let shape = voxel.shape();
                            let block = voxel.block();
                            if block == Block::Leaves || matches!(shape, Shape::Foliage) {
                                flora = MapFloraKind::Tree;
                                break;
                            }
                            if matches!(shape, Shape::SmallFoliage) {
                                flora = MapFloraKind::Bush;
                                break;
                            }
                            if matches!(shape, Shape::Trunk | Shape::DebarkedTrunk) {
                                flora = MapFloraKind::Tree;
                                break;
                            }
                            if block.is_solid() {
                                break;
                            } // hit ground, no flora above in this local stack
                        }
                        if flora == MapFloraKind::None {
                            continue;
                        }

                        let col = sample_column(&mut state.sample_cache, &generator, wx_i, wz_i);
                        let (radius_m, canopy, strength) = match flora {
                            MapFloraKind::Tree => {
                                let c = match col.biome {
                                    crate::world::generation_data::BiomeId::Desert => {
                                        [92.0, 118.0, 62.0]
                                    }
                                    crate::world::generation_data::BiomeId::Alpine => {
                                        [58.0, 92.0, 58.0]
                                    }
                                    crate::world::generation_data::BiomeId::Plains => {
                                        [54.0, 102.0, 48.0]
                                    }
                                    _ => [40.0, 88.0, 40.0],
                                };
                                (2.6f32, c, 0.72f32)
                            }
                            MapFloraKind::Bush => {
                                let c = match col.biome {
                                    crate::world::generation_data::BiomeId::Desert => {
                                        [126.0, 128.0, 74.0]
                                    }
                                    crate::world::generation_data::BiomeId::Alpine => {
                                        [88.0, 112.0, 76.0]
                                    }
                                    _ => [74.0, 120.0, 58.0],
                                };
                                (1.4f32, c, 0.45f32)
                            }
                            MapFloraKind::None => continue,
                        };
                        let r_px = (radius_m * px_per_m).ceil() as i32;
                        let cxp = (((wxp_center
                            - (state.buffer_center_xz.x - buffer_world_w * 0.5))
                            / buffer_world_w)
                            * w as f32) as i32;
                        let cyp = (((wzp_center
                            - (state.buffer_center_xz.y - buffer_world_h * 0.5))
                            / buffer_world_h)
                            * h as f32) as i32;
                        let n = hash01(wx_i, wz_i, 0x51ed_270b);
                        let jitter = 0.92 + n * 0.16;
                        for dy in -r_px..=r_px {
                            for dx in -r_px..=r_px {
                                let px = cxp + dx;
                                let py = cyp + dy;
                                if px < 0 || px >= w || py < 0 || py >= h {
                                    continue;
                                }
                                let d2 = (dx * dx + dy * dy) as f32;
                                let rr = (r_px as f32 + 0.5) * (r_px as f32 + 0.5);
                                if d2 > rr {
                                    continue;
                                }
                                let idx = ((py * w + px) * 4) as usize;
                                if idx + 3 >= terrain.len() || terrain[idx + 3] == 0 {
                                    continue;
                                }
                                let edge = (d2 / rr).clamp(0.0, 1.0);
                                let t =
                                    ((1.0 - edge).powf(0.8) * strength * jitter).clamp(0.0, 0.85);
                                let br = terrain[idx] as f32;
                                let bg = terrain[idx + 1] as f32;
                                let bb = terrain[idx + 2] as f32;
                                terrain[idx] = (br + (canopy[0] - br) * t).clamp(0.0, 255.0) as u8;
                                terrain[idx + 1] =
                                    (bg + (canopy[1] - bg) * t).clamp(0.0, 255.0) as u8;
                                terrain[idx + 2] =
                                    (bb + (canopy[2] - bb) * t).clamp(0.0, 255.0) as u8;
                            }
                        }
                    }
                }
            }
        }
    }

    // Composite cached terrain. Edge fade is handled by a fixed UI overlay (both modes).
    let n = out.len().min(terrain.len());
    out[..n].copy_from_slice(&terrain[..n]);

    let gen_ms = t0.elapsed().as_secs_f32() * 1000.0;

    if let Some(mut img) = images.get_mut(&state.explore_image) {
        img.data = Some(out.clone());
    }
    state.raw_terrain = terrain;
    state.raw_explore = out;

    state.perf_accum_terrain_ms += gen_ms * 0.5;
    state.perf_accum_fog_ms += gen_ms * 0.5;
    state.perf_accum_upload_ms += 0.0;
    state.perf_frames += 1;
    state.last_fog_center = state.center_xz;
    state.last_fog_zoom = state.zoom;
    state.last_fog_mode = Some(MapMode::TopDown2D);
    state.last_explored_len = exploration.explored.len();
    state.last_visible_signature = vis_sig;
    if regen_content {
        state.last_fog_update = now;
    }

    if state.perf_timer >= 1.0 {
        info!(
            "map perf: gen {:.2}ms upload {:.2}ms total {:.2}ms (avg over {} frames, dirty)",
            gen_ms,
            0.0,
            start_total.elapsed().as_secs_f32() * 1000.0,
            state.perf_frames
        );
        state.perf_accum_terrain_ms = 0.0;
        state.perf_accum_fog_ms = 0.0;
        state.perf_accum_upload_ms = 0.0;
        state.perf_frames = 0;
        state.perf_timer = 0.0;
    }
}

fn handle_map_zoom(
    ui: Res<UiManager>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<MapState>,
    player: Query<&Transform, With<Player>>,
) {
    if !ui.is_open(Overlay::Map) {
        return;
    }
    if (keys.just_pressed(KeyCode::KeyC) || keys.just_pressed(KeyCode::Home))
        && let Ok(p) = player.single()
    {
        state.center_xz = Vec2::new(p.translation.x, p.translation.z);
    }
    let Some(scroll) = scroll else { return };
    if scroll.delta.y.abs() < f32::EPSILON {
        return;
    }
    let factor = (1.0 - scroll.delta.y * 0.08).clamp(0.5, 1.5);
    state.zoom = (state.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
}

fn update_map_camera(
    state: Res<MapState>,
    ui: Res<UiManager>,
    mut camera: Query<(&mut Camera, &mut Transform, &mut Projection), With<MapCamera>>,
) {
    let Ok((mut cam, mut transform, mut projection)) = camera.single_mut() else {
        return;
    };
    let map_visible = ui.is_open(Overlay::Map) || state.open_amount > 0.001;
    let active_3d = map_visible && state.mode == MapMode::Overhead3D;
    if cam.is_active != active_3d {
        cam.is_active = active_3d;
    }
    if !active_3d {
        return;
    }

    let center = Vec3::new(state.center_xz.x, 0.0, state.center_xz.y);
    let pos = center + Vec3::Y * state.zoom;
    *transform = Transform::from_translation(pos).looking_at(center, Vec3::NEG_Z);

    if let Projection::Perspective(p) = &mut *projection {
        p.far = (state.zoom + 300.0 * BLOCKS_PER_METRE).max(500.0 * BLOCKS_PER_METRE);
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_map_ui(
    state: Res<MapState>,
    ui: Res<UiManager>,
    player: Query<(&Transform, &crate::player::Player), With<Player>>,
    mut roots: Query<
        &mut Node,
        (
            With<MapRoot>,
            Without<MapPlayerMarker>,
            Without<MapExploreImage>,
        ),
    >,
    mut images: Query<&mut ImageNode, (With<MapViewportImage>, Without<MapExploreImage>)>,
    mut explore_images: Query<
        (&mut ImageNode, &mut Node),
        (
            With<MapExploreImage>,
            Without<MapViewportImage>,
            Without<MapPlayerMarker>,
            Without<MapRoot>,
        ),
    >,
    mut marker: Query<(&mut Node, &mut Visibility, &mut UiTransform), With<MapPlayerMarker>>,
) {
    let show = ui.is_open(Overlay::Map) || state.open_amount > 0.001;
    for mut node in &mut roots {
        let d = if show { Display::Flex } else { Display::None };
        if node.display != d {
            node.display = d;
        }
    }
    let alpha = state.open_amount.clamp(0.0, 1.0);
    for mut img in &mut images {
        let a = if state.mode == MapMode::Overhead3D {
            alpha
        } else {
            0.0
        };
        let c = Color::srgba(1.0, 1.0, 1.0, a);
        if img.color != c {
            img.color = c;
        }
    }

    let (buf_left, buf_top, buf_w, buf_h) =
        if state.buffer_valid && state.mode == MapMode::TopDown2D {
            let vis = visible_world_size(&state);
            let buf_world_w = vis.x * (1.0 + BUFFER_MARGIN * 2.0);
            let buf_world_h = vis.y * (1.0 + BUFFER_MARGIN * 2.0);
            let dx = (state.buffer_center_xz.x - state.center_xz.x) / vis.x;
            let dy = (state.buffer_center_xz.y - state.center_xz.y) / vis.y;
            let w_pct = (buf_world_w / vis.x) * 100.0;
            let h_pct = (buf_world_h / vis.y) * 100.0;
            let left_pct = (0.5 + dx - (buf_world_w / vis.x) * 0.5) * 100.0;
            let top_pct = (0.5 + dy - (buf_world_h / vis.y) * 0.5) * 100.0;
            (
                Val::Percent(left_pct),
                Val::Percent(top_pct),
                Val::Percent(w_pct),
                Val::Percent(h_pct),
            )
        } else {
            (
                Val::Percent(0.0),
                Val::Percent(0.0),
                Val::Percent(100.0),
                Val::Percent(100.0),
            )
        };

    for (mut img, mut node) in &mut explore_images {
        let a = if state.mode == MapMode::TopDown2D {
            alpha
        } else {
            0.0
        };
        let c = Color::srgba(1.0, 1.0, 1.0, a);
        if img.color != c {
            img.color = c;
        }
        node.left = buf_left;
        node.top = buf_top;
        node.width = buf_w;
        node.height = buf_h;
    }

    let Ok((mut marker_node, mut marker_vis, mut marker_tf)) = marker.single_mut() else {
        return;
    };
    if !show {
        *marker_vis = Visibility::Hidden;
        return;
    }

    let Ok((player_tf, player)) = player.single() else {
        *marker_vis = Visibility::Hidden;
        return;
    };

    let world = visible_world_size(&state);
    let px = (player_tf.translation.x - state.center_xz.x) / world.x + 0.5;
    let py = (player_tf.translation.z - state.center_xz.y) / world.y + 0.5;
    if !(0.0..=1.0).contains(&px) || !(0.0..=1.0).contains(&py) {
        *marker_vis = Visibility::Hidden;
        return;
    }
    *marker_vis = Visibility::Inherited;
    marker_node.left = Val::Percent(px * 100.0);
    marker_node.top = Val::Percent(py * 100.0);
    // Teardrop marker: sharp bottom-right corner is "forward" at 0 rotation.
    // Offset -135deg makes yaw 0 point north/up. yaw=-pi/2 points east/right.
    marker_tf.rotation = Rot2::radians(-player.yaw - 3.0 * std::f32::consts::FRAC_PI_4);
}

fn on_map_mode_tab_activate(
    activate: On<Activate>,
    tabs: Query<&MapModeTab>,
    ui: Res<UiManager>,
    mut state: ResMut<MapState>,
) {
    if !ui.is_open(Overlay::Map) {
        return;
    }
    if let Ok(tab) = tabs.get(activate.entity) {
        state.mode = tab.mode;
    }
}

fn style_map_mode_tabs(
    state: Res<MapState>,
    theme: Res<UiTheme>,
    mut tabs: Query<(
        &MapModeTab,
        &Hovered,
        Has<Pressed>,
        &mut BackgroundColor,
        &mut BorderColor,
    )>,
) {
    for (tab, hovered, pressed, mut bg, mut border) in &mut tabs {
        let active = tab.mode == state.mode;
        let (bg_c, border_c) = if active || pressed {
            (theme.colors.accent.with_alpha(0.25), theme.colors.accent)
        } else if hovered.0 {
            (theme.colors.button_hover, theme.colors.border_hover)
        } else {
            (theme.colors.button_bg, theme.colors.border_idle)
        };
        if bg.0 != bg_c {
            bg.0 = bg_c;
        }
        if border.top != border_c {
            *border = BorderColor::all(border_c);
        }
    }
}
