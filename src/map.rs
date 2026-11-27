//! Overhead world map (Live 3D / Explore 2D).
//!
//! Press M to open the map. Live renders a north-up 3D view with a dedicated
//! camera into an offscreen texture; Explore uses CPU-composited snapshots.
//! Neither mode mutates world chunks.
//!
//! Controls while open: drag to pan, scroll to zoom, C/Home recenters,
//! Tab switches Live/Explore, and M/Esc closes. Explore fog is persisted via
//! `map_explored` records.

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
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
use crate::world::block::Block;
use crate::world::block::{Shape, Voxel};
use crate::world::chunk::{BLOCKS_PER_METRE, CHUNK_X, CHUNK_Y, CHUNK_Z, SEA_LEVEL_WORLD};
use crate::world::generation::{MapFloraKind, TreeKind, VegetationSink, WorldGen, hash2};
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
    pub focus_y: f32,
    pub zoom: f32,
    pub dragging: bool,
    pub last_cursor: Option<Vec2>,
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
            focus_y: 0.0,
            zoom: DEFAULT_ZOOM,
            dragging: false,
            last_cursor: None,
            last_fog_center: Vec2::ZERO,
            last_fog_zoom: 0.0,
            last_fog_mode: None,
            last_explored_len: 0,
            last_visible_signature: (IVec2::splat(i32::MIN), IVec2::splat(i32::MIN), 0),
            last_fog_update: 0.0,
            buffer_center_xz: Vec2::ZERO,
            buffer_zoom: 0.0,
            buffer_valid: false,
            raw_explore: Vec::new(),
            raw_terrain: Vec::new(),
        }
    }
}

/// Persistent explored-map data, serialized via `map_explored` records.
/// Independent of loaded chunks.
#[derive(Resource, Default)]
pub struct MapExploration {
    /// Explored cells in exploration-grid coordinates.
    pub explored: HashSet<IVec2>,
    /// Currently visible cells, recomputed periodically from player position.
    pub visible: HashSet<IVec2>,
}

const MAP_CHUNK_COLS: usize =
    (CHUNK_X / BLOCKS_PER_METRE as usize) * (CHUNK_Z / BLOCKS_PER_METRE as usize);
/// Flora coverage raster resolution per 16m map chunk (64x64 = 0.25m/cell).
const MAP_FLORA_RASTER: usize = 64;
const MAP_FLORA_CELLS: usize = MAP_FLORA_RASTER * MAP_FLORA_RASTER;
const MAP_FLORA_MIP1: usize = 32;
const MAP_FLORA_MIP1_CELLS: usize = MAP_FLORA_MIP1 * MAP_FLORA_MIP1;
const MAP_FLORA_MIP2: usize = 16;
const MAP_FLORA_MIP2_CELLS: usize = MAP_FLORA_MIP2 * MAP_FLORA_MIP2;
const MAP_FLORA_MIP3: usize = 8;
const MAP_FLORA_MIP3_CELLS: usize = MAP_FLORA_MIP3 * MAP_FLORA_MIP3;
/// Reduced from 4096 to limit memory with 64x64 flora planes+mips.
const MAP_CACHE_MAX_ENTRIES: usize = 2048;
const MAP_CACHE_PRUNE_MARGIN_CHUNKS: i32 = 4;

#[derive(Clone, Copy)]
struct MapSnapshotColumn {
    col: MapColumn,
    rgba: [u8; 4],
}

/// Separate tree and bush coverage planes. Coverage is 0..=255.
/// Meta packs compact visual info: low 3 bits species/variant, high 2 bits height bucket.
#[derive(Clone)]
struct MapChunkSnapshot {
    columns: [MapSnapshotColumn; MAP_CHUNK_COLS],
    tree_cov: [u8; MAP_FLORA_CELLS],
    bush_cov: [u8; MAP_FLORA_CELLS],
    tree_meta: [u8; MAP_FLORA_CELLS],
    bush_meta: [u8; MAP_FLORA_CELLS],
    tree_mip1: [u8; MAP_FLORA_MIP1_CELLS],
    tree_mip2: [u8; MAP_FLORA_MIP2_CELLS],
    tree_mip3: [u8; MAP_FLORA_MIP3_CELLS],
    bush_mip1: [u8; MAP_FLORA_MIP1_CELLS],
    bush_mip2: [u8; MAP_FLORA_MIP2_CELLS],
    bush_mip3: [u8; MAP_FLORA_MIP3_CELLS],
    has_flora: bool,
    terrain_dirty: bool,
    flora_dirty: bool,
    last_used: u64,
}

impl MapChunkSnapshot {
    fn new() -> Self {
        Self {
            columns: [MapSnapshotColumn {
                col: MapColumn {
                    height: SEA_LEVEL_WORLD,
                    surface: Block::Air,
                    has_turf: false,
                    has_snow: false,
                    biome: BiomeId::Plains,
                    forest: 0.0,
                    supports_flora: false,
                    flora: MapFloraKind::None,
                },
                rgba: [0, 0, 0, 0],
            }; MAP_CHUNK_COLS],
            tree_cov: [0u8; MAP_FLORA_CELLS],
            bush_cov: [0u8; MAP_FLORA_CELLS],
            tree_meta: [0u8; MAP_FLORA_CELLS],
            bush_meta: [0u8; MAP_FLORA_CELLS],
            tree_mip1: [0u8; MAP_FLORA_MIP1_CELLS],
            tree_mip2: [0u8; MAP_FLORA_MIP2_CELLS],
            tree_mip3: [0u8; MAP_FLORA_MIP3_CELLS],
            bush_mip1: [0u8; MAP_FLORA_MIP1_CELLS],
            bush_mip2: [0u8; MAP_FLORA_MIP2_CELLS],
            bush_mip3: [0u8; MAP_FLORA_MIP3_CELLS],
            has_flora: false,
            terrain_dirty: true,
            flora_dirty: true,
            last_used: 0,
        }
    }
}

const MAP_SNAPSHOT_BUILD_BUDGET_MS: f32 = 2.0;

#[derive(Resource, Default)]
pub struct MapTerrainCache {
    chunks: HashMap<IVec2, MapChunkSnapshot>,
    frame: u64,
    hits: u64,
    misses: u64,
    builds: u64,
    build_ms: f32,
    flora_build_ms: f32,
    flora_mip_ms: f32,
    builds_this_frame: u32,
    build_ms_this_frame: f32,
    queue_len: usize,
}

impl MapTerrainCache {
    fn touch_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.builds_this_frame = 0;
        self.build_ms_this_frame = 0.0;
    }
    fn prune(&mut self, center: IVec2, keep_radius_chunks: i32) {
        self.chunks.retain(|k, _| {
            (k.x - center.x).abs() <= keep_radius_chunks
                && (k.y - center.y).abs() <= keep_radius_chunks
        });
        if self.chunks.len() <= MAP_CACHE_MAX_ENTRIES {
            return;
        }
        let mut keys: Vec<(u64, IVec2)> =
            self.chunks.iter().map(|(k, v)| (v.last_used, *k)).collect();
        keys.sort_by_key(|(u, _)| *u);
        let remove = self.chunks.len().saturating_sub(MAP_CACHE_MAX_ENTRIES);
        for (_, k) in keys.into_iter().take(remove) {
            self.chunks.remove(&k);
        }
    }
}

fn snapshot_index(lx: usize, lz: usize) -> usize {
    lz * (CHUNK_X / BLOCKS_PER_METRE as usize) + lx
}

#[inline]
fn flora_raster_index(lx: usize, lz: usize) -> usize {
    lz * MAP_FLORA_RASTER + lx
}

#[inline]
fn build_flora_mips(
    src: &[u8; MAP_FLORA_CELLS],
    m1: &mut [u8; MAP_FLORA_MIP1_CELLS],
    m2: &mut [u8; MAP_FLORA_MIP2_CELLS],
    m3: &mut [u8; MAP_FLORA_MIP3_CELLS],
) {
    for z in 0..MAP_FLORA_MIP1 {
        for x in 0..MAP_FLORA_MIP1 {
            let mut sum = 0u16;
            for dz in 0..2 {
                for dx in 0..2 {
                    sum += src[(z * 2 + dz) * MAP_FLORA_RASTER + (x * 2 + dx)] as u16;
                }
            }
            m1[z * MAP_FLORA_MIP1 + x] = (sum / 4) as u8;
        }
    }
    for z in 0..MAP_FLORA_MIP2 {
        for x in 0..MAP_FLORA_MIP2 {
            let mut sum = 0u16;
            for dz in 0..2 {
                for dx in 0..2 {
                    sum += m1[(z * 2 + dz) * MAP_FLORA_MIP1 + (x * 2 + dx)] as u16;
                }
            }
            m2[z * MAP_FLORA_MIP2 + x] = (sum / 4) as u8;
        }
    }
    for z in 0..MAP_FLORA_MIP3 {
        for x in 0..MAP_FLORA_MIP3 {
            let mut sum = 0u16;
            for dz in 0..2 {
                for dx in 0..2 {
                    sum += m2[(z * 2 + dz) * MAP_FLORA_MIP2 + (x * 2 + dx)] as u16;
                }
            }
            m3[z * MAP_FLORA_MIP3 + x] = (sum / 4) as u8;
        }
    }
}

#[derive(Clone)]
struct MapCanopySink {
    tree_cov: [u8; MAP_FLORA_CELLS],
    bush_cov: [u8; MAP_FLORA_CELLS],
    tree_meta: [u8; MAP_FLORA_CELLS],
    bush_meta: [u8; MAP_FLORA_CELLS],
    occupied: HashSet<IVec3>,
    y_min: i32,
    y_max: i32,
}

impl MapCanopySink {
    fn new(_coord_xz: IVec2, y_min: i32, y_max: i32) -> Self {
        Self {
            tree_cov: [0u8; MAP_FLORA_CELLS],
            bush_cov: [0u8; MAP_FLORA_CELLS],
            tree_meta: [0u8; MAP_FLORA_CELLS],
            bush_meta: [0u8; MAP_FLORA_CELLS],
            occupied: HashSet::new(),
            y_min,
            y_max,
        }
    }
    #[inline]
    fn local_to_raster(&self, x: i32, z: i32) -> Option<(usize, usize)> {
        // detail local in 16m chunk: 0..32 corresponds to world 0..16m. Map to MAP_FLORA_RASTER.
        let scale = MAP_FLORA_RASTER as f32 / CHUNK_X as f32;
        let fx = ((x as f32) * scale).floor() as isize;
        let fz = ((z as f32) * scale).floor() as isize;
        if fx < 0 || fz < 0 || fx >= MAP_FLORA_RASTER as isize || fz >= MAP_FLORA_RASTER as isize {
            return None;
        }
        Some((fx as usize, fz as usize))
    }
    fn record_voxel(&mut self, x: i32, _y: i32, z: i32, voxel: Voxel) {
        let Some((fx, fz)) = self.local_to_raster(x, z) else {
            return;
        };
        let shape = voxel.shape();
        let block = voxel.block();
        let i = flora_raster_index(fx, fz);
        if block == Block::Leaves || matches!(shape, Shape::Foliage) {
            self.tree_cov[i] = self.tree_cov[i].saturating_add(32);
            self.tree_meta[i] = 1;
        } else if matches!(shape, Shape::SmallFoliage) {
            self.bush_cov[i] = self.bush_cov[i].saturating_add(40);
            self.bush_meta[i] = 1;
        } else if matches!(shape, Shape::Trunk | Shape::DebarkedTrunk) {
            self.tree_cov[i] = self.tree_cov[i].saturating_add(12);
            if self.tree_meta[i] == 0 {
                self.tree_meta[i] = 1;
            }
        }
    }
}

impl VegetationSink for MapCanopySink {
    fn veg_get(&self, x: i32, y: i32, z: i32) -> Voxel {
        if x < 0
            || z < 0
            || x >= CHUNK_X as i32
            || z >= CHUNK_Z as i32
            || y < self.y_min
            || y > self.y_max
        {
            return Voxel::AIR;
        }
        if self.occupied.contains(&IVec3::new(x, y, z)) {
            // Return a non-air placeholder so placement_if_air respects occupancy.
            Voxel::new(Block::Leaves)
        } else {
            Voxel::AIR
        }
    }
    fn veg_set_if_air(&mut self, x: i32, y: i32, z: i32, voxel: Voxel) {
        if x < 0
            || z < 0
            || x >= CHUNK_X as i32
            || z >= CHUNK_Z as i32
            || y < self.y_min
            || y > self.y_max
        {
            return;
        }
        let p = IVec3::new(x, y, z);
        if self.occupied.insert(p) {
            self.record_voxel(x, y, z, voxel);
        }
    }
}

fn rasterize_tree_plan_into_sink(
    wg: &WorldGen,
    sink: &mut MapCanopySink,
    seed: u32,
    world_x: i32,
    world_z: i32,
    column: &MapColumn,
) {
    let Some((trunk_h, species)) = wg.tree_plan_at(world_x, world_z) else {
        return;
    };
    if !wg.tree_has_spacing(world_x, world_z, trunk_h) {
        return;
    }
    let scale = BLOCKS_PER_METRE as i32;
    let lx = world_x.rem_euclid(CHUNK_X as i32 / scale) * scale;
    let lz = world_z.rem_euclid(CHUNK_Z as i32 / scale) * scale;
    let base = (column.height + 1) * scale;
    let roll = hash2(seed ^ 0x51ed_270b, world_x, world_z);
    let trunk_storage = IVec3::new(lx, base, lz);
    match species.kind {
        TreeKind::BroadleafOak => {
            WorldGen::place_broadleaf_oak(sink, seed, trunk_storage, trunk_h, roll)
        }
        TreeKind::Birch => WorldGen::place_birch(sink, seed, trunk_storage, trunk_h, roll),
        TreeKind::Pine => WorldGen::place_pine(sink, seed, trunk_storage, trunk_h, roll),
        TreeKind::Spruce => WorldGen::place_spruce(sink, seed, trunk_storage, trunk_h, roll),
        TreeKind::HardyShrubTree => {
            WorldGen::place_hardy_shrub_tree(sink, seed, trunk_storage, trunk_h, roll)
        }
    }
    let _ = (wg, world_x, world_z);
}

fn rasterize_bush_plan_into_sink(
    wg: &WorldGen,
    sink: &mut MapCanopySink,
    seed: u32,
    world_x: i32,
    world_z: i32,
    column: &MapColumn,
) {
    let Some((roll, kind)) = wg.bush_plan_at(world_x, world_z) else {
        return;
    };
    let scale = BLOCKS_PER_METRE as i32;
    let lx = world_x.rem_euclid(CHUNK_X as i32 / scale) * scale;
    let lz = world_z.rem_euclid(CHUNK_Z as i32 / scale) * scale;
    let base = (column.height + 1) * scale;
    WorldGen::place_bush_variant(sink, seed, IVec3::new(lx, base, lz), roll, kind);
    let _ = column;
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

fn sample_snapshot_column(
    generator: &WorldGenResource,
    manager: &ChunkManager,
    wx: i32,
    wz: i32,
) -> MapColumn {
    let mut col = generator.0.map_column_at(wx, wz);
    let mut edited_height = col.height;
    let mut edited_surface = col.surface;
    let mut edited_turf = col.has_turf;
    let mut edited_snow = col.has_snow;
    let mut found_world_edit = false;
    for y in (SEA_LEVEL_WORLD - 32..=SEA_LEVEL_WORLD + 128).rev() {
        let p = IVec3::new(wx, y, wz);
        if let Some(&b) = manager.world_edits.get(&p) {
            edited_height = y;
            edited_surface = b;
            found_world_edit = true;
            edited_turf = manager.turf_edits.get(&p).copied().unwrap_or(false);
            edited_snow = manager.snow_edits.get(&p).copied().unwrap_or(false);
            break;
        }
    }
    if found_world_edit {
        col.height = edited_height;
        col.surface = edited_surface;
        col.has_turf = edited_turf;
        col.has_snow = edited_snow;
        col.supports_flora =
            edited_surface.is_solid() && !edited_surface.is_water() && !edited_snow;
    } else {
        let surf_p = IVec3::new(wx, col.height, wz);
        if let Some(&t) = manager.turf_edits.get(&surf_p) {
            col.has_turf = t;
        }
        if let Some(&s) = manager.snow_edits.get(&surf_p) {
            col.has_snow = s;
            col.supports_flora = col.surface.is_solid() && !col.surface.is_water() && !col.has_snow;
        }
    }
    col
}

fn terrain_rgba_from_column(
    col: MapColumn,
    h_l: i32,
    h_r: i32,
    h_u: i32,
    h_d: i32,
    wx: i32,
    wz: i32,
) -> [u8; 4] {
    let mut rgb = biome_base_color(col);
    let elev = (col.height - SEA_LEVEL_WORLD) as f32;
    let elev_t = (elev / 80.0).clamp(-0.5, 1.0);
    let elev_factor = 1.0 + elev_t * 0.18;
    rgb[0] *= elev_factor;
    rgb[1] *= elev_factor;
    rgb[2] *= elev_factor;
    let shade = ((h_r - h_l) as f32 * -0.08 + (h_d - h_u) as f32 * -0.08).clamp(-0.35, 0.35);
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

fn build_snapshot_terrain(
    cache: &mut MapTerrainCache,
    generator: &WorldGenResource,
    manager: &ChunkManager,
    coord_xz: IVec2,
) {
    let frame = cache.frame;
    let t0 = std::time::Instant::now();
    let mut snap = cache
        .chunks
        .remove(&coord_xz)
        .unwrap_or_else(MapChunkSnapshot::new);
    snap.last_used = frame;
    let cols_per_side = CHUNK_X / BLOCKS_PER_METRE as usize;
    let mut cols = [MapColumn {
        height: SEA_LEVEL_WORLD,
        surface: Block::Air,
        has_turf: false,
        has_snow: false,
        biome: BiomeId::Plains,
        forest: 0.0,
        supports_flora: false,
        flora: MapFloraKind::None,
    }; MAP_CHUNK_COLS];
    for lz in 0..cols_per_side {
        for lx in 0..cols_per_side {
            let wx = coord_xz.x * cols_per_side as i32 + lx as i32;
            let wz = coord_xz.y * cols_per_side as i32 + lz as i32;
            cols[snapshot_index(lx, lz)] = sample_snapshot_column(generator, manager, wx, wz);
        }
    }
    for lz in 0..cols_per_side {
        for lx in 0..cols_per_side {
            let wx = coord_xz.x * cols_per_side as i32 + lx as i32;
            let wz = coord_xz.y * cols_per_side as i32 + lz as i32;
            let col = cols[snapshot_index(lx, lz)];
            let h_l = if lx > 0 {
                cols[snapshot_index(lx - 1, lz)].height
            } else {
                sample_snapshot_column(generator, manager, wx - 1, wz).height
            };
            let h_r = if lx + 1 < cols_per_side {
                cols[snapshot_index(lx + 1, lz)].height
            } else {
                sample_snapshot_column(generator, manager, wx + 1, wz).height
            };
            let h_u = if lz > 0 {
                cols[snapshot_index(lx, lz - 1)].height
            } else {
                sample_snapshot_column(generator, manager, wx, wz - 1).height
            };
            let h_d = if lz + 1 < cols_per_side {
                cols[snapshot_index(lx, lz + 1)].height
            } else {
                sample_snapshot_column(generator, manager, wx, wz + 1).height
            };
            let rgba = terrain_rgba_from_column(col, h_l, h_r, h_u, h_d, wx, wz);
            snap.columns[snapshot_index(lx, lz)] = MapSnapshotColumn { col, rgba };
        }
    }
    snap.terrain_dirty = false;
    snap.flora_dirty = true;
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    cache.builds += 1;
    cache.build_ms += ms;
    cache.builds_this_frame += 1;
    cache.build_ms_this_frame += ms;
    cache.chunks.insert(coord_xz, snap);
}

fn snapshot_needs_terrain(cache: &MapTerrainCache, coord_xz: IVec2) -> bool {
    cache
        .chunks
        .get(&coord_xz)
        .map(|s| s.terrain_dirty)
        .unwrap_or(true)
}
fn snapshot_needs_flora(cache: &MapTerrainCache, coord_xz: IVec2) -> bool {
    cache
        .chunks
        .get(&coord_xz)
        .map(|s| s.terrain_dirty || s.flora_dirty)
        .unwrap_or(true)
}
fn build_snapshot_flora(
    cache: &mut MapTerrainCache,
    generator: &WorldGenResource,
    manager: &ChunkManager,
    coord_xz: IVec2,
) {
    if snapshot_needs_terrain(cache, coord_xz) {
        build_snapshot_terrain(cache, generator, manager, coord_xz);
    }
    let Some(mut snap) = cache.chunks.remove(&coord_xz) else {
        return;
    };
    snap.last_used = cache.frame;
    let t0 = std::time::Instant::now();
    snap.tree_cov = [0u8; MAP_FLORA_CELLS];
    snap.bush_cov = [0u8; MAP_FLORA_CELLS];
    snap.tree_meta = [0u8; MAP_FLORA_CELLS];
    snap.bush_meta = [0u8; MAP_FLORA_CELLS];

    let cols_per_side = CHUNK_X / BLOCKS_PER_METRE as usize;
    let chunk_world_min_x = coord_xz.x * CHUNK_X as i32;
    let chunk_world_min_z = coord_xz.y * CHUNK_Z as i32;
    let chunk_world_max_x = chunk_world_min_x + CHUNK_X as i32;
    let chunk_world_max_z = chunk_world_min_z + CHUNK_Z as i32;

    let mut any_loaded = false;
    for (chunk_coord, chunk) in manager.chunks.iter() {
        let cminx = chunk_coord.x * CHUNK_X as i32;
        let cmaxx = cminx + CHUNK_X as i32;
        let cminz = chunk_coord.z * CHUNK_Z as i32;
        let cmaxz = cminz + CHUNK_Z as i32;
        if cmaxx <= chunk_world_min_x
            || cminx >= chunk_world_max_x
            || cmaxz <= chunk_world_min_z
            || cminz >= chunk_world_max_z
        {
            continue;
        }
        any_loaded = true;
        // Actual loaded chunk voxel projection into the 0.25 m flora raster.
        // Captures player modifications.
        for lz in 0..CHUNK_Z {
            for lx in 0..CHUNK_X {
                let wx_detail = cminx + lx as i32;
                let wz_detail = cminz + lz as i32;
                let local_x = (wx_detail - chunk_world_min_x) as f32 + 0.5;
                let local_z = (wz_detail - chunk_world_min_z) as f32 + 0.5;
                let fx = (local_x * (MAP_FLORA_RASTER as f32 / CHUNK_X as f32)) as usize;
                let fz = (local_z * (MAP_FLORA_RASTER as f32 / CHUNK_Z as f32)) as usize;
                if fx >= MAP_FLORA_RASTER || fz >= MAP_FLORA_RASTER {
                    continue;
                }
                let i = flora_raster_index(fx, fz);
                for ly in (0..CHUNK_Y).rev() {
                    let voxel = chunk.data.get_local(lx, ly, lz);
                    if voxel.is_air() {
                        continue;
                    }
                    let shape = voxel.shape();
                    let block = voxel.block();
                    if block == Block::Leaves || matches!(shape, Shape::Foliage) {
                        snap.tree_cov[i] = snap.tree_cov[i].saturating_add(24);
                        if snap.tree_meta[i] == 0 {
                            snap.tree_meta[i] = 1;
                        }
                    } else if matches!(shape, Shape::SmallFoliage) {
                        snap.bush_cov[i] = snap.bush_cov[i].saturating_add(36);
                        if snap.bush_meta[i] == 0 {
                            snap.bush_meta[i] = 1;
                        }
                    } else if matches!(shape, Shape::Trunk | Shape::DebarkedTrunk) {
                        snap.tree_cov[i] = snap.tree_cov[i].saturating_add(10);
                        if snap.tree_meta[i] == 0 {
                            snap.tree_meta[i] = 1;
                        }
                        // don't break: foliage above may have been found in other cells; but top-down same cell means canopy likely above
                    } else if block.is_solid() {
                        break;
                    }
                }
            }
        }
    }

    if !any_loaded {
        // Deterministic base from actual worldgen placement rules via VegetationSink.
        let seed = generator.0.seed;
        let mut sink = MapCanopySink::new(coord_xz, 0, CHUNK_Y as i32 * 4);
        for lz in 0..cols_per_side {
            for lx in 0..cols_per_side {
                let wx = coord_xz.x * cols_per_side as i32 + lx as i32;
                let wz = coord_xz.y * cols_per_side as i32 + lz as i32;
                let col = generator.0.map_column_at(wx, wz);
                if col.flora == MapFloraKind::Tree {
                    rasterize_tree_plan_into_sink(&generator.0, &mut sink, seed, wx, wz, &col);
                } else if col.flora == MapFloraKind::Bush {
                    rasterize_bush_plan_into_sink(&generator.0, &mut sink, seed, wx, wz, &col);
                }
            }
        }
        snap.tree_cov = sink.tree_cov;
        snap.bush_cov = sink.bush_cov;
        snap.tree_meta = sink.tree_meta;
        snap.bush_meta = sink.bush_meta;

        // Apply vegetation-related detail edits for unloaded edited chunks.
        let detail_min_x = chunk_world_min_x * BLOCKS_PER_METRE as i32;
        let detail_min_z = chunk_world_min_z * BLOCKS_PER_METRE as i32;
        let detail_max_x = chunk_world_max_x * BLOCKS_PER_METRE as i32;
        let detail_max_z = chunk_world_max_z * BLOCKS_PER_METRE as i32;
        for (pos, voxel) in &manager.detail_edits {
            if pos.x < detail_min_x
                || pos.x >= detail_max_x
                || pos.z < detail_min_z
                || pos.z >= detail_max_z
            {
                continue;
            }
            let local_x = (pos.x - detail_min_x) as f32 / BLOCKS_PER_METRE;
            let local_z = (pos.z - detail_min_z) as f32 / BLOCKS_PER_METRE;
            let fx =
                ((local_x * 2.0).floor() as isize).clamp(0, MAP_FLORA_RASTER as isize - 1) as usize;
            let fz =
                ((local_z * 2.0).floor() as isize).clamp(0, MAP_FLORA_RASTER as isize - 1) as usize;
            let i = flora_raster_index(fx, fz);
            if voxel.is_air() {
                // Local removal only.
                snap.tree_cov[i] = snap.tree_cov[i].saturating_sub(48);
                snap.bush_cov[i] = snap.bush_cov[i].saturating_sub(64);
                if snap.tree_cov[i] == 0 {
                    snap.tree_meta[i] = 0;
                }
                if snap.bush_cov[i] == 0 {
                    snap.bush_meta[i] = 0;
                }
            } else {
                let shape = voxel.shape();
                let block = voxel.block();
                if block == Block::Leaves || matches!(shape, Shape::Foliage) {
                    snap.tree_cov[i] = snap.tree_cov[i].saturating_add(32);
                    snap.tree_meta[i] = 1;
                } else if matches!(shape, Shape::SmallFoliage) {
                    snap.bush_cov[i] = snap.bush_cov[i].saturating_add(40);
                    snap.bush_meta[i] = 1;
                } else if matches!(shape, Shape::Trunk | Shape::DebarkedTrunk) {
                    snap.tree_cov[i] = snap.tree_cov[i].saturating_add(12);
                    if snap.tree_meta[i] == 0 {
                        snap.tree_meta[i] = 1;
                    }
                }
            }
        }
    }

    let mip_t0 = std::time::Instant::now();
    build_flora_mips(
        &snap.tree_cov,
        &mut snap.tree_mip1,
        &mut snap.tree_mip2,
        &mut snap.tree_mip3,
    );
    build_flora_mips(
        &snap.bush_cov,
        &mut snap.bush_mip1,
        &mut snap.bush_mip2,
        &mut snap.bush_mip3,
    );
    let mip_ms = mip_t0.elapsed().as_secs_f32() * 1000.0;

    snap.has_flora = snap.tree_cov.iter().any(|&v| v != 0) || snap.bush_cov.iter().any(|&v| v != 0);
    snap.flora_dirty = false;
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    cache.builds += 1;
    cache.build_ms += ms;
    cache.builds_this_frame += 1;
    cache.build_ms_this_frame += ms;
    cache.flora_build_ms += ms - mip_ms;
    cache.flora_mip_ms += mip_ms;
    cache.chunks.insert(coord_xz, snap);
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
        // Keep the sky dome out of the Live map. The clear color is transparent,
        // so only terrain, flora, and clouds appear in the offscreen texture.
        RenderLayers::layer(0),
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
    // 0 rotation points "forward" via the sharp bottom-right corner after -135deg offset.
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

#[allow(clippy::type_complexity)]
fn sync_map_target(
    ui: Res<UiManager>,
    mut state: ResMut<MapState>,
    player: Query<&Transform, With<Player>>,
    mut camera: Query<
        (&mut Camera, &mut Transform, &mut Projection),
        (With<MapCamera>, Without<Player>),
    >,
) {
    let open = ui.is_open(Overlay::Map);
    if state.target_open != open {
        state.target_open = open;
        if open {
            if let Ok(p) = player.single() {
                state.center_xz = Vec2::new(p.translation.x, p.translation.z);
                state.focus_y = p.translation.y;
                state.initialized = true;
            }
            // Snap map camera immediately on open to avoid one-frame stale/cave view.
            if let Ok((mut cam, mut transform, mut projection)) = camera.single_mut() {
                let active_3d = state.mode == MapMode::Overhead3D;
                cam.is_active = active_3d;
                if active_3d {
                    let center = Vec3::new(state.center_xz.x, state.focus_y, state.center_xz.y);
                    *transform = Transform::from_translation(center + Vec3::Y * state.zoom)
                        .looking_at(center, Vec3::NEG_Z);
                    if let Projection::Perspective(p) = &mut *projection {
                        p.far =
                            (state.zoom + 300.0 * BLOCKS_PER_METRE).max(500.0 * BLOCKS_PER_METRE);
                    }
                }
            }
        } else {
            state.dragging = false;
            state.last_cursor = None;
            if let Ok((mut cam, _, _)) = camera.single_mut() {
                cam.is_active = false;
            }
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

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapState>()
            .init_resource::<MapExploration>()
            .init_resource::<MapTerrainCache>()
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

#[derive(Resource, Default)]
pub struct MapExplorationState {
    last_player_cell: Option<IVec2>,
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

fn hash01(x: i32, z: i32, salt: u32) -> f32 {
    let mut n = (x as u32).wrapping_mul(374761393)
        ^ (z as u32).wrapping_mul(668265263)
        ^ salt.wrapping_mul(2246822519);
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    n ^= n >> 16;
    (n & 0xFFFF) as f32 / 65535.0
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

fn update_map_exploration(
    player: Query<&Transform, With<Player>>,
    mut exploration: ResMut<MapExploration>,
    mut expl_state: Local<MapExplorationState>,
) {
    let Ok(tf) = player.single() else { return };
    let pos = Vec2::new(tf.translation.x, tf.translation.z);
    let player_cell = world_to_explore_cell(pos);
    if expl_state.last_player_cell == Some(player_cell) && !exploration.explored.is_empty() {
        return;
    }
    expl_state.last_player_cell = Some(player_cell);

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

fn update_fog_image(
    time: Res<Time>,
    mut state: ResMut<MapState>,
    mut cache: ResMut<MapTerrainCache>,
    exploration: Res<MapExploration>,
    generator: Res<WorldGenResource>,
    manager: Res<ChunkManager>,
    mut images: ResMut<Assets<Image>>,
) {
    // Only render/snapshot-build when Explore map is actually open. Exploration accumulation
    // runs separately and continues while map is closed.
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

    let need_terrain_full = mode_changed || !state.buffer_valid || !buffer_covers;
    let need_terrain_patch = !need_terrain_full && explored_changed;
    let need_fog = need_terrain_full || need_terrain_patch || visible_changed;
    let regen_content = need_terrain_full || need_terrain_patch;

    if !need_fog && cache.queue_len == 0 {
        return;
    }

    let now = time.elapsed_secs();
    if !need_terrain_full
        && need_fog
        && cache.queue_len == 0
        && now - state.last_fog_update < FOG_UPDATE_INTERVAL
    {
        return;
    }

    if need_terrain_full {
        state.buffer_center_xz = state.center_xz;
        state.buffer_zoom = state.zoom;
        state.buffer_valid = true;
    }

    let w = FOG_IMAGE_WIDTH as i32;
    let h_px = FOG_IMAGE_HEIGHT as i32;
    let expected = (w as usize) * (h_px as usize) * 4;
    if state.raw_terrain.len() != expected {
        state.raw_terrain = [0u8, 0, 0, 0].repeat(expected / 4);
    }
    if state.raw_explore.len() != expected {
        state.raw_explore = [0u8, 0, 0, 0].repeat(expected / 4);
    }

    let mut terrain = std::mem::take(&mut state.raw_terrain);
    let mut out = std::mem::take(&mut state.raw_explore);

    cache.touch_frame();
    let cols_per_side = CHUNK_X as i32 / BLOCKS_PER_METRE as i32;
    let min_wx =
        ((state.buffer_center_xz.x - buffer_world_w * 0.5) / BLOCKS_PER_METRE).floor() as i32 - 1;
    let max_wx =
        ((state.buffer_center_xz.x + buffer_world_w * 0.5) / BLOCKS_PER_METRE).ceil() as i32 + 1;
    let min_wz =
        ((state.buffer_center_xz.y - buffer_world_h * 0.5) / BLOCKS_PER_METRE).floor() as i32 - 1;
    let max_wz =
        ((state.buffer_center_xz.y + buffer_world_h * 0.5) / BLOCKS_PER_METRE).ceil() as i32 + 1;
    let min_chunk = IVec2::new(
        min_wx.div_euclid(cols_per_side),
        min_wz.div_euclid(cols_per_side),
    );
    let max_chunk = IVec2::new(
        max_wx.div_euclid(cols_per_side),
        max_wz.div_euclid(cols_per_side),
    );
    let center_chunk = IVec2::new(
        (state.buffer_center_xz.x / BLOCKS_PER_METRE).floor() as i32 / cols_per_side,
        (state.buffer_center_xz.y / BLOCKS_PER_METRE).floor() as i32 / cols_per_side,
    );
    let keep_radius = ((buffer_world_w.max(buffer_world_h) / (CHUNK_X as f32)) as i32)
        + MAP_CACHE_PRUNE_MARGIN_CHUNKS;
    cache.prune(center_chunk, keep_radius.max(8));

    let mut pending: Vec<(i32, IVec2)> = Vec::new();
    for cz in min_chunk.y..=max_chunk.y {
        for cx in min_chunk.x..=max_chunk.x {
            let coord = IVec2::new(cx, cz);
            let needs_terrain = snapshot_needs_terrain(&cache, coord);
            let needs_flora = regen_content && snapshot_needs_flora(&cache, coord);
            if needs_terrain || needs_flora {
                let dist = (cx - center_chunk.x).abs() + (cz - center_chunk.y).abs();
                pending.push((dist, coord));
            } else {
                cache.hits += 1;
                let frame = cache.frame;
                if let Some(s) = cache.chunks.get_mut(&coord) {
                    s.last_used = frame;
                }
            }
        }
    }
    pending.sort_by_key(|(d, c)| (*d, c.x, c.y));
    cache.queue_len = pending.len();

    for (_, coord) in pending.iter().copied() {
        if cache.build_ms_this_frame >= MAP_SNAPSHOT_BUILD_BUDGET_MS {
            break;
        }
        let needs_terrain = snapshot_needs_terrain(&cache, coord);
        let needs_flora = regen_content && snapshot_needs_flora(&cache, coord);
        if !needs_terrain && !needs_flora {
            continue;
        }
        cache.misses += 1;
        if needs_terrain {
            build_snapshot_terrain(&mut cache, &generator, &manager, coord);
        }
        if regen_content && snapshot_needs_flora(&cache, coord) {
            build_snapshot_flora(&mut cache, &generator, &manager, coord);
        }
    }
    // Refresh queue len after budgeted work.
    cache.queue_len = pending
        .iter()
        .filter(|(_, c)| {
            snapshot_needs_terrain(&cache, *c)
                || (regen_content && snapshot_needs_flora(&cache, *c))
        })
        .count();

    let world_per_px = (buffer_world_w / w as f32)
        .max(buffer_world_h / h_px as f32)
        .max(0.001);
    let base_cell_m = CHUNK_X as f32 / MAP_FLORA_RASTER as f32;
    // Keep flora squares readable when zoomed in; coarser mips stay gapless.
    let mip_level = ((world_per_px / base_cell_m).log2().round() as i32).clamp(1, 3);

    #[inline(always)]
    fn sample_flora_plane(
        snap: &MapChunkSnapshot,
        local_x: f32,
        local_z: f32,
        tree: bool,
        mip: i32,
    ) -> f32 {
        let (res, plane): (usize, &[u8]) = match (mip, tree) {
            (0, true) => (MAP_FLORA_RASTER, &snap.tree_cov),
            (0, false) => (MAP_FLORA_RASTER, &snap.bush_cov),
            (1, true) => (MAP_FLORA_MIP1, &snap.tree_mip1[..]),
            (1, false) => (MAP_FLORA_MIP1, &snap.bush_mip1[..]),
            (2, true) => (MAP_FLORA_MIP2, &snap.tree_mip2[..]),
            (2, false) => (MAP_FLORA_MIP2, &snap.bush_mip2[..]),
            (_, true) => (MAP_FLORA_MIP3, &snap.tree_mip3[..]),
            (_, false) => (MAP_FLORA_MIP3, &snap.bush_mip3[..]),
        };
        // Nearest raster cell for solid, gapless square flora tiles.
        let cell_m = CHUNK_X as f32 / res as f32;
        let ix = ((local_x / cell_m).floor() as i32).clamp(0, res as i32 - 1) as usize;
        let iz = ((local_z / cell_m).floor() as i32).clamp(0, res as i32 - 1) as usize;
        plane[iz * res + ix] as f32 * (1.0 / 255.0)
    }

    for py in 0..h_px {
        let v = (py as f32 + 0.5) / h_px as f32;
        let wzp = state.buffer_center_xz.y + (v - 0.5) * buffer_world_h;
        let wz_i = (wzp / BLOCKS_PER_METRE).floor() as i32;
        let chunk_z = wz_i.div_euclid(cols_per_side);
        let lz = wz_i.rem_euclid(cols_per_side) as usize;
        let row = py as usize * w as usize;

        let mut px = 0i32;
        while px < w {
            let u = (px as f32 + 0.5) / w as f32;
            let wxp = state.buffer_center_xz.x + (u - 0.5) * buffer_world_w;
            let wx_i = (wxp / BLOCKS_PER_METRE).floor() as i32;
            let chunk_x = wx_i.div_euclid(cols_per_side);
            let lx = wx_i.rem_euclid(cols_per_side) as usize;
            let coord = IVec2::new(chunk_x, chunk_z);

            let next_wx_boundary = (wx_i as f32 + 1.0) * BLOCKS_PER_METRE;
            let mut px_end = (((next_wx_boundary
                - (state.buffer_center_xz.x - buffer_world_w * 0.5))
                / buffer_world_w)
                * w as f32)
                .ceil() as i32;
            if px_end <= px {
                px_end = px + 1;
            }
            if px_end > w {
                px_end = w;
            }

            let cell = world_to_explore_cell(Vec2::new(
                (wx_i as f32 + 0.5) * BLOCKS_PER_METRE,
                (wz_i as f32 + 0.5) * BLOCKS_PER_METRE,
            ));
            let explored = exploration.explored.contains(&cell);
            let snap_ref = if explored {
                cache.chunks.get(&coord)
            } else {
                None
            };

            if let Some(snap) = snap_ref {
                let si_col = snapshot_index(lx, lz);
                let base = snap.columns[si_col].rgba;
                let col = snap.columns[si_col].col;
                let base_r = base[0] as f32;
                let base_g = base[1] as f32;
                let base_b = base[2] as f32;
                let has_flora = snap.has_flora;
                let local_fx_base = coord.x as f32 * CHUNK_X as f32;
                let local_fz =
                    (wzp - coord.y as f32 * CHUNK_Z as f32).clamp(0.0, CHUNK_Z as f32 - 0.001);
                let bc = match col.biome {
                    BiomeId::Desert => (132.0, 136.0, 84.0),
                    BiomeId::Alpine => (84.0, 112.0, 74.0),
                    BiomeId::Plains => (82.0, 126.0, 66.0),
                    BiomeId::Forest => (74.0, 118.0, 60.0),
                    _ => (78.0, 120.0, 62.0),
                };
                let tc = match col.biome {
                    BiomeId::Desert => (108.0, 126.0, 78.0),
                    BiomeId::Alpine => (52.0, 88.0, 56.0),
                    BiomeId::Plains => (62.0, 112.0, 56.0),
                    BiomeId::Forest => (44.0, 96.0, 46.0),
                    _ => (54.0, 102.0, 52.0),
                };
                for x in px..px_end {
                    let mut r = base_r;
                    let mut g = base_g;
                    let mut b = base_b;
                    if has_flora {
                        let wx_x = state.buffer_center_xz.x
                            + ((x as f32 + 0.5) / w as f32 - 0.5) * buffer_world_w;
                        let local_fx = (wx_x - local_fx_base).clamp(0.0, CHUNK_X as f32 - 0.001);
                        let tree_cov =
                            (sample_flora_plane(snap, local_fx, local_fz, true, mip_level) * 4.0)
                                .min(1.0);
                        let bush_cov =
                            (sample_flora_plane(snap, local_fx, local_fz, false, mip_level) * 4.0)
                                .min(1.0);
                        if bush_cov > 0.001 || tree_cov > 0.001 {
                            let lum = (r * 0.2126 + g * 0.7152 + b * 0.0722) * (1.0 / 255.0);
                            if bush_cov > 0.001 {
                                let shade = (0.72 + lum * 0.28).clamp(0.52, 1.0);
                                r = bc.0 * shade;
                                g = bc.1 * shade;
                                b = bc.2 * shade;
                            }
                            if tree_cov > 0.001 {
                                let shade = (0.58 + lum * 0.24).clamp(0.36, 1.0);
                                r = tc.0 * shade;
                                g = tc.1 * shade;
                                b = tc.2 * shade;
                            }
                        }
                    }
                    let idx = (row + x as usize) * 4;
                    terrain[idx] = r.clamp(0.0, 255.0) as u8;
                    terrain[idx + 1] = g.clamp(0.0, 255.0) as u8;
                    terrain[idx + 2] = b.clamp(0.0, 255.0) as u8;
                    terrain[idx + 3] = 255;
                }
            } else {
                for x in px..px_end {
                    let idx = (row + x as usize) * 4;
                    terrain[idx] = 0;
                    terrain[idx + 1] = 0;
                    terrain[idx + 2] = 0;
                    terrain[idx + 3] = 0;
                }
            }
            px = px_end;
        }
    }
    let ncopy = out.len().min(terrain.len());
    out[..ncopy].copy_from_slice(&terrain[..ncopy]);

    if let Some(mut img) = images.get_mut(&state.explore_image) {
        img.data = Some(out.clone());
    }
    state.raw_terrain = terrain;
    state.raw_explore = out;

    state.last_fog_center = state.center_xz;
    state.last_fog_zoom = state.zoom;
    state.last_fog_mode = Some(MapMode::TopDown2D);
    state.last_explored_len = exploration.explored.len();
    state.last_visible_signature = vis_sig;
    if regen_content || need_fog {
        state.last_fog_update = if cache.queue_len > 0 {
            now - FOG_UPDATE_INTERVAL
        } else {
            now
        };
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
        state.focus_y = p.translation.y;
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

    let center = Vec3::new(state.center_xz.x, state.focus_y, state.center_xz.y);
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

#[cfg(test)]
mod map_flora_parity_tests {
    use super::*;
    use crate::world::chunk::ChunkData;
    use crate::world::generation::{MapColumn, WorldGen};

    fn project_chunkdata_vegetation(
        data: &ChunkData,
    ) -> ([u8; MAP_FLORA_CELLS], [u8; MAP_FLORA_CELLS]) {
        let mut tree = [0u8; MAP_FLORA_CELLS];
        let mut bush = [0u8; MAP_FLORA_CELLS];
        let scale = MAP_FLORA_RASTER as f32 / CHUNK_X as f32;
        for lz in 0..CHUNK_Z {
            for lx in 0..CHUNK_X {
                let fx = ((lx as f32) * scale).floor() as usize;
                let fz = ((lz as f32) * scale).floor() as usize;
                if fx >= MAP_FLORA_RASTER || fz >= MAP_FLORA_RASTER {
                    continue;
                }
                let i = flora_raster_index(fx, fz);
                for ly in (0..CHUNK_Y).rev() {
                    let voxel = data.get_local(lx, ly, lz);
                    if voxel.is_air() {
                        continue;
                    }
                    let shape = voxel.shape();
                    let block = voxel.block();
                    if block == Block::Leaves || matches!(shape, Shape::Foliage) {
                        tree[i] = tree[i].saturating_add(32);
                    } else if matches!(shape, Shape::SmallFoliage) {
                        bush[i] = bush[i].saturating_add(40);
                    } else if matches!(shape, Shape::Trunk | Shape::DebarkedTrunk) {
                        tree[i] = tree[i].saturating_add(12);
                    } else if block.is_solid() {
                        break;
                    }
                }
            }
        }
        (tree, bush)
    }

    fn project_sink_tree(
        wg: &WorldGen,
        seed: u32,
        wx: i32,
        wz: i32,
        col: &MapColumn,
    ) -> ([u8; MAP_FLORA_CELLS], [u8; MAP_FLORA_CELLS]) {
        let mut sink = MapCanopySink::new(IVec2::ZERO, 0, CHUNK_Y as i32 * 4);
        rasterize_tree_plan_into_sink(wg, &mut sink, seed, wx, wz, col);
        (sink.tree_cov, sink.bush_cov)
    }

    fn project_sink_bush(
        wg: &WorldGen,
        seed: u32,
        wx: i32,
        wz: i32,
        col: &MapColumn,
    ) -> ([u8; MAP_FLORA_CELLS], [u8; MAP_FLORA_CELLS]) {
        let mut sink = MapCanopySink::new(IVec2::ZERO, 0, CHUNK_Y as i32 * 4);
        rasterize_bush_plan_into_sink(wg, &mut sink, seed, wx, wz, col);
        (sink.tree_cov, sink.bush_cov)
    }

    fn first_tree_column(
        wg: &WorldGen,
    ) -> Option<(
        i32,
        i32,
        MapColumn,
        i32,
        crate::world::generation::TreeSpecies,
    )> {
        for z in -256..256 {
            for x in -256..256 {
                let col = wg.map_column_at(x, z);
                if col.flora != MapFloraKind::Tree {
                    continue;
                }
                if let Some((trunk_h, species)) = wg.tree_plan_at(x, z)
                    && wg.tree_has_spacing(x, z, trunk_h)
                {
                    return Some((x, z, col, trunk_h, species));
                }
            }
        }
        None
    }

    fn first_bush_column(wg: &WorldGen) -> Option<(i32, i32, MapColumn, u32, i32)> {
        for z in -256..256 {
            for x in -256..256 {
                let col = wg.map_column_at(x, z);
                if col.flora != MapFloraKind::Bush {
                    continue;
                }
                if let Some((roll, kind)) = wg.bush_plan_at(x, z) {
                    return Some((x, z, col, roll, kind));
                }
            }
        }
        None
    }

    #[test]
    fn map_tree_projection_matches_chunkdata_footprint_for_representative_tree() {
        let wg = WorldGen::new(0x4B54_414C);
        let (wx, wz, col, _trunk_h, _species) = first_tree_column(&wg).expect("tree");
        let seed = wg.seed();
        let scale = BLOCKS_PER_METRE as i32;
        let lx = wx.rem_euclid(CHUNK_X as i32 / scale) * scale;
        let lz = wz.rem_euclid(CHUNK_Z as i32 / scale) * scale;
        let base = (col.height + 1) * scale;
        let mut chunk = ChunkData::new(IVec3::ZERO);
        let (sink_tree, sink_bush) = project_sink_tree(&wg, seed, wx, wz, &col);
        // place into chunk via same plan
        let (trunk_h, species) = wg.tree_plan_at(wx, wz).unwrap();
        let roll = hash2(seed ^ 0x51ed_270b, wx, wz);
        let trunk_storage = IVec3::new(lx, base, lz);
        match species.kind {
            crate::world::generation::TreeKind::BroadleafOak => {
                WorldGen::place_broadleaf_oak(&mut chunk, seed, trunk_storage, trunk_h, roll)
            }
            crate::world::generation::TreeKind::Birch => {
                WorldGen::place_birch(&mut chunk, seed, trunk_storage, trunk_h, roll)
            }
            crate::world::generation::TreeKind::Pine => {
                WorldGen::place_pine(&mut chunk, seed, trunk_storage, trunk_h, roll)
            }
            crate::world::generation::TreeKind::Spruce => {
                WorldGen::place_spruce(&mut chunk, seed, trunk_storage, trunk_h, roll)
            }
            crate::world::generation::TreeKind::HardyShrubTree => {
                WorldGen::place_hardy_shrub_tree(&mut chunk, seed, trunk_storage, trunk_h, roll)
            }
        }
        let (cd_tree, cd_bush) = project_chunkdata_vegetation(&chunk);
        assert_eq!(sink_tree, cd_tree, "tree coverage mismatch at {wx},{wz}");
        assert_eq!(sink_bush, cd_bush, "bush coverage mismatch at {wx},{wz}");
    }

    #[test]
    fn map_bush_projection_matches_chunkdata_footprint_for_representative_bush() {
        let wg = WorldGen::new(0x4B54_414C);
        let (wx, wz, col, _roll, kind) = first_bush_column(&wg).expect("bush");
        let seed = wg.seed();
        let scale = BLOCKS_PER_METRE as i32;
        let lx = wx.rem_euclid(CHUNK_X as i32 / scale) * scale;
        let lz = wz.rem_euclid(CHUNK_Z as i32 / scale) * scale;
        let base = (col.height + 1) * scale;
        let mut chunk = ChunkData::new(IVec3::ZERO);
        let (sink_tree, sink_bush) = project_sink_bush(&wg, seed, wx, wz, &col);
        let (roll, kind2) = wg.bush_plan_at(wx, wz).unwrap();
        assert_eq!(kind, kind2);
        WorldGen::place_bush_variant(&mut chunk, seed, IVec3::new(lx, base, lz), roll, kind);
        let (cd_tree, cd_bush) = project_chunkdata_vegetation(&chunk);
        assert_eq!(sink_tree, cd_tree, "tree coverage mismatch at {wx},{wz}");
        assert_eq!(sink_bush, cd_bush, "bush coverage mismatch at {wx},{wz}");
    }
}
