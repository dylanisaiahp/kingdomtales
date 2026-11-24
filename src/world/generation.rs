//! Deterministic procedural world generation.
//!
//! Terrain is generated on the coarse 1 m world grid first, then expanded into
//! the 0.5 m detail grid used for rendering, collision and construction. Grass
//! is a natural-turf flag on its 1 m topsoil voxel and is rendered as a raised
//! 0.1 m layer; trees, bushes, sticks and pebbles are detail objects layered on
//! top of that terrain. The generator is pure: the same seed and chunk
//! coordinate always produce the same blocks, so chunk generation can run on
//! worker threads.

use bevy::math::{IVec3, Vec3};
use noise::{Fbm, MultiFractal, NoiseFn, OpenSimplex};
use std::collections::HashMap;

use super::block::{Block, Facing, Orientation, PANEL_HEIGHT, Shape, Voxel};
use super::chunk::{
    CHUNK_X, CHUNK_Y, CHUNK_Z, ChunkData, DETAILS_PER_WORLD_AXIS, LAND_HALF_EXTENT,
    OCEAN_FLOOR_WORLD, PAD, PADDED_X, PADDED_Y, PADDED_Z, SEA_LEVEL_DETAIL, SEA_LEVEL_WORLD,
    WORLD_CHUNK_X, WORLD_CHUNK_Y, WORLD_CHUNK_Z, WORLD_PAD, WORLD_PADDED_X, WORLD_PADDED_Y,
    WORLD_PADDED_Z, WORLD_Y_MIN, world_y_in_range,
};
use super::generation_data::{
    BIOME_CELL_BIOME_OFFSET, BIOME_CELL_JITTER, BIOME_CELL_JITTER_OFFSET, BIOME_CELL_SIZE,
    BIOME_CELL_SMOOTH_RADIUS, BiomeId, BiomeSample, CAVE_CONNECTOR_CELL_CHANCE,
    CAVE_CONNECTOR_CELL_SIZE, CAVE_CONNECTOR_MIN_RADIUS, CAVE_CONNECTOR_RADIUS_RANGE,
    CAVE_CONNECTOR_WORM_MARGIN, CAVE_DEPTH_FADE, CAVE_DETAIL_NOISE, CAVE_DETAIL_SCALE,
    CAVE_DETAIL_WEIGHT, CAVE_FIRST_LAYER_DEPTH, CAVE_LAYER_COUNT, CAVE_LAYER_DRIFT,
    CAVE_LAYER_NOISE, CAVE_LAYER_SCALE, CAVE_LAYER_SPACING, CAVE_MAX_DEPTH, CAVE_MOUTH_CELL_CHANCE,
    CAVE_MOUTH_CELL_SIZE, CAVE_MOUTH_NOISE, CAVE_MOUTH_NOISE_MIN, CAVE_MOUTH_NOISE_SCALE,
    CAVE_MOUTH_TUNNEL_DEPTH, CAVE_MOUTH_TUNNEL_RADIUS, CAVE_MOUTH_WORM_RADIUS,
    CAVE_NOISE_FIELD_COUNT, CAVE_NOISE_SEED_OFFSETS, CAVE_ROOM_CELL_CHANCE, CAVE_ROOM_CELL_SIZE,
    CAVE_ROOM_MAX_RADIUS, CAVE_ROOM_MIN_RADIUS, CAVE_ROOM_RADIUS_RANGE, CAVE_ROOM_WORM_MARGIN,
    CAVE_SPAWN_MOUTH_BONUS, CAVE_SPAWN_MOUTH_RADIUS, CAVE_SPAWN_WORM_BONUS, CAVE_SPAWN_WORM_RADIUS,
    CAVE_WARP_METRES, CAVE_WARP_NOISE, CAVE_WARP_SCALE, CAVE_WORM_A_NOISE, CAVE_WORM_A_SCALE,
    CAVE_WORM_B_NOISE, CAVE_WORM_B_SCALE, CAVE_WORM_RADIUS, CAVE_WORM_WIDTH, FOREST_GRAPH,
    GEOLOGY_STRATA_SCALE, GEOLOGY_STRATA_SEED_OFFSET, GEOLOGY_STRATA_WARP, GEOLOGY_SURFACE_CAP,
    GraphContext, HEIGHT_GRAPH, NOISE_FIELD_COUNT, NOISE_FIELDS, PLAINS_CRADLE_RADIUS,
    PLAINS_TRANSITION_RADIUS, biome_for_cell_roll, blend_biome_samples, eval_graph, geology_strata,
    sample_biome, smoothstep,
};

/// Width of the beach/underwater slope on each side of the nominal boundary.
const SHORE_BLEND_WIDTH: i32 = 8;

/// A small, fast integer hash used for deterministic per-column randomness.
#[inline]
pub fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

#[inline]
pub fn hash2(seed: u32, x: i32, z: i32) -> u32 {
    hash_u32(
        (x as u32)
            .wrapping_mul(0x9e37_79b1)
            .wrapping_add((z as u32).wrapping_mul(0x85eb_ca77))
            ^ seed,
    )
}

#[inline]
pub fn hash3(seed: u32, x: i32, y: i32, z: i32) -> u32 {
    hash2(seed ^ (y as u32).wrapping_mul(0x27d4_eb2d), x, z)
}

/// True when a world column falls inside the nominal land footprint. The
/// terrain now fades into ocean across this boundary instead of ending as a
/// cliff, so the footprint mainly limits where turf and vegetation appear.
#[inline]
fn in_land_footprint(x: i32, z: i32) -> bool {
    x.abs() < LAND_HALF_EXTENT && z.abs() < LAND_HALF_EXTENT
}

/// Signed distance to the nearest edge of the nominal land footprint. Positive
/// values are inside it; negative values are out at sea.
#[inline]
fn footprint_edge_distance(x: i32, z: i32) -> i32 {
    LAND_HALF_EXTENT - x.abs().max(z.abs())
}

/// 1 at full inland elevation, 0 at the open ocean floor. The transition spans
/// [`SHORE_BLEND_WIDTH`] metres on both sides of the nominal edge.
#[inline]
fn shore_blend(x: i32, z: i32) -> f32 {
    let distance = footprint_edge_distance(x, z) as f32;
    let width = SHORE_BLEND_WIDTH as f32;
    smoothstep(-width, width, distance)
}

/// Jittered centre of one Voronoi cell. The cell index covers one square of
/// [`BIOME_CELL_SIZE`] metres; jitter keeps borders irregular without ever
/// pushing the centre outside the neighbouring grid.
fn biome_cell_center(seed: u32, gx: i32, gz: i32) -> (f32, f32) {
    let jitter = hash2(seed ^ BIOME_CELL_JITTER_OFFSET, gx, gz);
    let jitter_x = ((jitter & 0xffff) as f32 / 65535.0 - 0.5) * 2.0 * BIOME_CELL_JITTER;
    let jitter_z = (((jitter >> 16) & 0xffff) as f32 / 65535.0 - 0.5) * 2.0 * BIOME_CELL_JITTER;
    let size = BIOME_CELL_SIZE as f32;
    (
        (gx as f32 + 0.5 + jitter_x) * size,
        (gz as f32 + 0.5 + jitter_z) * size,
    )
}

/// Resolve the biome patch containing one world column. The nearest jittered
/// cell centre owns all discrete biome rules, while nearby centres soften only
/// the numeric terrain values so a patch edge does not become a cliff.
fn biome_cell_at(seed: u32, x: i32, z: i32, distance: f32) -> (BiomeId, BiomeSample) {
    if distance <= PLAINS_CRADLE_RADIUS {
        return (BiomeId::Plains, sample_biome(BiomeId::Plains));
    }

    let cell_x = x.div_euclid(BIOME_CELL_SIZE);
    let cell_z = z.div_euclid(BIOME_CELL_SIZE);
    let position_x = x as f32;
    let position_z = z as f32;
    let smooth_radius_sq = BIOME_CELL_SMOOTH_RADIUS * BIOME_CELL_SMOOTH_RADIUS;
    let mut nearest = f32::INFINITY;
    let mut dominant = BiomeId::Plains;
    let mut samples = [(sample_biome(BiomeId::Plains), 0.0); 9];
    let mut total_weight = 0.0;
    let mut index = 0;

    for gz in (cell_z - 1)..=(cell_z + 1) {
        for gx in (cell_x - 1)..=(cell_x + 1) {
            let (centre_x, centre_z) = biome_cell_center(seed, gx, gz);
            let dx = position_x - centre_x;
            let dz = position_z - centre_z;
            let distance_sq = dx * dx + dz * dz;
            let biome = biome_for_cell_roll(hash2(seed ^ BIOME_CELL_BIOME_OFFSET, gx, gz));
            if distance_sq < nearest {
                nearest = distance_sq;
                dominant = biome;
            }
            let weight = 1.0 / (distance_sq + smooth_radius_sq);
            samples[index] = (sample_biome(biome), weight);
            total_weight += weight;
            index += 1;
        }
    }

    let mut sample = blend_biome_samples(&samples[..index], total_weight, dominant);
    if distance < PLAINS_TRANSITION_RADIUS {
        let t = smoothstep(PLAINS_CRADLE_RADIUS, PLAINS_TRANSITION_RADIUS, distance);
        let plains = sample_biome(BiomeId::Plains);
        sample = blend_biome_samples(&[(plains, 1.0 - t), (sample, t)], 1.0, dominant);
    }
    (dominant, sample)
}

/// Minimal sink for deterministic vegetation placement. `ChunkData` implements this for
/// real world generation; the map uses a lightweight sink to capture canopy footprints from
/// the same placement logic.
pub trait VegetationSink {
    #[allow(dead_code)]
    fn veg_get(&self, x: i32, y: i32, z: i32) -> Voxel;
    fn veg_set_if_air(&mut self, x: i32, y: i32, z: i32, voxel: Voxel);
}

impl VegetationSink for ChunkData {
    #[inline]
    fn veg_get(&self, x: i32, y: i32, z: i32) -> Voxel {
        if !(0..CHUNK_X as i32).contains(&x)
            || !(0..CHUNK_Z as i32).contains(&z)
            || !(0..CHUNK_Y as i32).contains(&y)
        {
            return Voxel::AIR;
        }
        self.get_local(x as usize, y as usize, z as usize)
    }

    #[inline]
    fn veg_set_if_air(&mut self, x: i32, y: i32, z: i32, voxel: Voxel) {
        if !(0..CHUNK_X as i32).contains(&x)
            || !(0..CHUNK_Z as i32).contains(&z)
            || !(0..CHUNK_Y as i32).contains(&y)
        {
            return;
        }
        if self.get_local(x as usize, y as usize, z as usize).is_air() {
            self.set_local(x as usize, y as usize, z as usize, voxel);
        }
    }
}

/// Place a voxel at a cell-relative offset if the target cell is inside the
/// chunk and still air. Trees and bushes use this so branches and foliage never
/// replace a trunk or each other.
fn place_if_air<S: VegetationSink + ?Sized>(
    data: &mut S,
    origin: IVec3,
    offset: IVec3,
    voxel: Voxel,
) {
    let x = origin.x + offset.x;
    let y = origin.y + offset.y;
    let z = origin.z + offset.z;
    data.veg_set_if_air(x, y, z, voxel);
}

fn place_leaf_cluster<S: VegetationSink + ?Sized>(
    data: &mut S,
    origin: IVec3,
    center: IVec3,
    radius: i32,
    voxel: Voxel,
    seed: u32,
) {
    for dy in -radius..=radius {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let dist2 = dx * dx + dy * dy + dz * dz;
                if dist2 > radius * radius + 1 {
                    continue;
                }
                if dist2 >= radius * radius
                    && (hash3(seed, center.x + dx, center.y + dy, center.z + dz) & 3) == 0
                {
                    continue;
                }
                place_if_air(data, origin, center + IVec3::new(dx, dy, dz), voxel);
            }
        }
    }
}

/// Whether leaves or a small woody branch occupy the neighbourhood. Used to
/// keep loose ground clutter from spawning on or inside a bush or tree.
fn vegetation_near(data: &ChunkData, lx: i32, base: i32, lz: i32, radius: i32) -> bool {
    for dy in 0..=2 {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let x = lx + dx;
                let y = base + dy;
                let z = lz + dz;
                if !(0..CHUNK_X as i32).contains(&x)
                    || !(0..CHUNK_Z as i32).contains(&z)
                    || !(0..CHUNK_Y as i32).contains(&y)
                {
                    continue;
                }
                let voxel = data.get_local(x as usize, y as usize, z as usize);
                if matches!(voxel.shape(), Shape::Foliage | Shape::SmallFoliage)
                    || (voxel.block() == Block::Wood
                        && matches!(voxel.shape(), Shape::Stick | Shape::Stem | Shape::Trunk))
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Ground clutter must rest on a full terrain cube, never on foliage or another
/// small object.
fn rests_on_ground(data: &ChunkData, lx: i32, base: i32, lz: i32) -> bool {
    if base <= 0 {
        return false;
    }
    let below = data.get_local(lx as usize, (base - 1) as usize, lz as usize);
    below.is_full_cube()
        && matches!(
            below.block(),
            Block::Topsoil
                | Block::Humus
                | Block::RedSandstone
                | Block::Sand
                | Block::Snow
                | Block::Granite
                | Block::Slate
                | Block::Shale
                | Block::Sandstone
                | Block::Limestone
        )
}

/// Write one loose object in the world voxel's lower-left detail cell. Its
/// geometry is offset to the block centre, so it stays a single voxel and a
/// single pickup. `orientation` selects a deterministic visual variant for
/// natural shapes such as sticks and pebble piles.
fn set_centered_clutter(
    data: &mut ChunkData,
    origin: IVec3,
    shape: Shape,
    material: Block,
    orientation: Orientation,
) {
    if !(0..CHUNK_X as i32).contains(&origin.x)
        || !(0..CHUNK_Z as i32).contains(&origin.z)
        || !(0..CHUNK_Y as i32).contains(&origin.y)
    {
        return;
    }
    if !data
        .get_local(origin.x as usize, origin.y as usize, origin.z as usize)
        .is_air()
    {
        return;
    }
    data.set_local(
        origin.x as usize,
        origin.y as usize,
        origin.z as usize,
        Voxel::from_oriented_parts(material, shape, orientation),
    );
}

/// A horizontal limb leaving the trunk. Each `Stem` cell is a 0.25 m square that
/// is 0.5 m long along its orientation, so a run of cells reads as a thin branch
/// extending out from the centred trunk cube. The limb starts at a trunk-edge
/// detail cell so it connects to the centred trunk instead of floating away.
fn place_branch<S: VegetationSink + ?Sized>(
    data: &mut S,
    trunk_storage: IVec3,
    start_dy: i32,
    facing: Facing,
    length: i32,
    rise: i32,
) {
    let dir = facing.direction();
    // Limbs lie flat. `rise` is kept in the silhouette table for future use.
    let _ = rise;
    let orientation = Orientation::from_up_forward(dir, Vec3::Y);
    let stem = Voxel::from_oriented_parts(Block::Wood, Shape::Stem, orientation);
    let foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);

    // Start on the side of the trunk's centred footprint so the limb touches it.
    let origin = branch_origin(trunk_storage, start_dy, facing);

    // A perpendicular horizontal direction used to tuck tufts against the limb.
    let side = IVec3::new(-dir.z as i32, 0, dir.x as i32);

    for distance in 0..=length {
        let dx = dir.x as i32 * distance;
        let dz = dir.z as i32 * distance;
        let dy = 0;
        place_if_air(data, origin, IVec3::new(dx, dy, dz), stem);

        // Keep a small tuft beside the first segment and outer half so limbs read
        // as leafy and stay detectable by attachment-sensitive tests.
        if distance == 0 || (distance > 0 && distance * 2 >= length) {
            place_if_air(
                data,
                origin,
                IVec3::new(dx + side.x, dy, dz + side.z),
                foliage,
            );
        }
        // A tuft at the tip, just beyond the end of the limb.
        if distance == length {
            place_if_air(
                data,
                origin,
                IVec3::new(dx + dir.x as i32, dy, dz + dir.z as i32),
                foliage,
            );
        }
    }
}

/// Detail-cell origin on the side of a centred 0.5 m trunk for a horizontal
/// facing.
///
/// Trunk occupies 0.75..1.25 in its storage cell. Horizontal stem bounds are
/// pre-oriented and use centred-detail space (may extend outside 0..1).
/// +axis: cell trunk+1, along 0.25..0.75 touches trunk edge 1.25.
/// -axis: cell trunk-1, along 1.25..1.75 touches trunk edge 0.75.
/// Perp is 0.75..1.25, aligned with trunk cross-section, stored in trunk cell.
fn branch_origin(trunk_storage: IVec3, dy: i32, facing: Facing) -> IVec3 {
    let dir = facing.direction();
    let (ox, oz) = if dir.x.abs() > 0.5 {
        if dir.x > 0.0 {
            (trunk_storage.x + 1, trunk_storage.z)
        } else {
            (trunk_storage.x - 1, trunk_storage.z)
        }
    } else if dir.z > 0.0 {
        (trunk_storage.x, trunk_storage.z + 1)
    } else {
        (trunk_storage.x, trunk_storage.z - 1)
    };
    IVec3::new(ox, trunk_storage.y + dy, oz)
}

/// Place one flat root leaving the centred trunk. A single stem segment keeps
/// the root connected to the trunk without protruding too far.
fn place_root<S: VegetationSink + ?Sized>(data: &mut S, trunk_storage: IVec3, facing: Facing) {
    let dir = facing.direction();
    let orientation = Orientation::from_up_forward(dir, Vec3::Y);
    let stem = Voxel::from_oriented_parts(Block::Wood, Shape::Stem, orientation);
    let origin = branch_origin(trunk_storage, 0, facing);
    place_if_air(data, origin, IVec3::ZERO, stem);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeKind {
    /// The original approved broadleaf reference tree.
    BroadleafOak,
    Birch,
    Pine,
    Spruce,
    HardyShrubTree,
}

#[derive(Clone, Copy, Debug)]
pub struct TreeSpecies {
    pub kind: TreeKind,
    pub min_trunk: i32,
    pub max_trunk: i32,
    pub crown_top: i32,
}

impl TreeSpecies {
    const fn new(kind: TreeKind, min_trunk: i32, max_trunk: i32, crown_top: i32) -> Self {
        Self {
            kind,
            min_trunk,
            max_trunk,
            crown_top,
        }
    }
}

const PLAINS_TREES: &[TreeSpecies] = &[TreeSpecies::new(TreeKind::BroadleafOak, 10, 16, 4)];
const FOREST_TREES: &[TreeSpecies] = &[
    TreeSpecies::new(TreeKind::BroadleafOak, 10, 16, 4),
    TreeSpecies::new(TreeKind::Birch, 12, 18, 4),
];
const ALPINE_TREES: &[TreeSpecies] = &[
    TreeSpecies::new(TreeKind::Pine, 12, 18, 5),
    TreeSpecies::new(TreeKind::Spruce, 10, 16, 5),
];
const DESERT_TREES: &[TreeSpecies] = &[TreeSpecies::new(TreeKind::HardyShrubTree, 6, 9, 3)];
const MESA_TREES: &[TreeSpecies] = &[TreeSpecies::new(TreeKind::HardyShrubTree, 6, 10, 3)];

fn tree_species_for_column(column: &ColumnInfo, roll: u32) -> Option<TreeSpecies> {
    let options = match column.sample.dominant {
        BiomeId::Plains => PLAINS_TREES,
        BiomeId::Forest => FOREST_TREES,
        BiomeId::Alpine => ALPINE_TREES,
        BiomeId::Desert => DESERT_TREES,
        BiomeId::Mesa => MESA_TREES,
    };
    if options.is_empty() {
        return None;
    }
    let mut idx = ((roll >> 20) as usize) % options.len();
    // In forest, bias slightly toward birch for variety while keeping oaks common.
    if column.sample.dominant == BiomeId::Forest && ((roll >> 28) & 1) == 0 {
        idx = 0;
    }
    Some(options[idx])
}

pub(crate) fn tree_plan_from_column(
    seed: u32,
    x: i32,
    z: i32,
    column: &ColumnInfo,
) -> Option<(i32, TreeSpecies)> {
    if !column.supports_flora {
        return None;
    }
    let density =
        (column.sample.tree_base + column.forest * column.sample.tree_gain).clamp(0.0, 0.14);
    let r = hash2(seed ^ 0x51ed_270b, x, z);
    let roll = (r & 0xffff) as f32 / 65535.0;
    if roll > density {
        return None;
    }
    let species = tree_species_for_column(column, r)?;
    let span = (species.max_trunk - species.min_trunk).max(0) as u32;
    let trunk_h = species.min_trunk + ((r >> 16) % (span + 1)) as i32;
    Some((trunk_h, species))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapFloraKind {
    None,
    Tree,
    Bush,
}

/// Compact column summary for map/UI sampling. Cheaper to pass than full ColumnInfo.
#[derive(Clone, Copy, Debug)]
pub struct MapColumn {
    pub height: i32,
    pub surface: Block,
    pub has_turf: bool,
    pub has_snow: bool,
    pub biome: BiomeId,
    pub forest: f32,
    pub supports_flora: bool,
    #[allow(dead_code)]
    pub flora: MapFloraKind,
}

/// Public map/helper: deterministic bush kind for a column and roll, matching `place_bushes`.
#[allow(dead_code)]
pub fn bush_kind_for_column(column: &MapColumn, roll: u32) -> i32 {
    match column.biome {
        BiomeId::Plains => 0,
        BiomeId::Forest => {
            if ((roll >> 18) & 1) == 0 {
                1
            } else {
                2
            }
        }
        BiomeId::Alpine => 3,
        BiomeId::Desert => 4,
        BiomeId::Mesa => 5,
    }
}

/// Public map/helper: should a bush be placed at this column? Uses same deterministic rule.
#[allow(dead_code)]
pub fn bush_plan_for_column(seed: u32, x: i32, z: i32, column: &MapColumn) -> Option<(u32, i32)> {
    if !column.supports_flora || column.height <= SEA_LEVEL_WORLD {
        return None;
    }
    let roll = hash2(seed ^ 0x1b87_3593, x, z);
    // bush_threshold is encoded in sample; MapColumn doesn't carry it. Caller should check with map_flora_at logic? Better expose threshold via column_at sample.
    // For now, use map_flora_at/bush decision through column: caller has MapColumn.flora.
    if column.flora != MapFloraKind::Bush {
        return None;
    }
    Some((roll, bush_kind_for_column(column, roll)))
}

/// Public map/helper: deterministic bush plan for a map column. Uses same MapColumn flora decision.
#[allow(dead_code)]
pub fn bush_plan_for_map_column(
    seed: u32,
    x: i32,
    z: i32,
    column: &MapColumn,
) -> Option<(u32, i32)> {
    if column.flora != MapFloraKind::Bush
        || !column.supports_flora
        || column.height <= SEA_LEVEL_WORLD
    {
        return None;
    }
    let roll = hash2(seed ^ 0x1b87_3593, x, z);
    Some((roll, bush_kind_for_column(column, roll)))
}
impl WorldGen {
    #[allow(dead_code)]
    pub fn seed(&self) -> u32 {
        self.seed
    }

    /// Deterministic flora marker for map rendering. Uses same rules as worldgen placement checks
    /// (density/hash/biome), without chunk-local occupancy.
    #[allow(dead_code)]
    pub fn map_flora_at(&self, x: i32, z: i32) -> MapFloraKind {
        let column = self.column_at(x, z);
        if !column.supports_flora || column.cave_mouth || column.height <= SEA_LEVEL_WORLD {
            return MapFloraKind::None;
        }
        if tree_plan_from_column(self.seed, x, z, &column).is_some() {
            return MapFloraKind::Tree;
        }
        let roll = hash2(self.seed ^ 0x1b87_3593, x, z);
        if (roll & 0xffff) <= column.sample.bush_threshold {
            return MapFloraKind::Bush;
        }
        MapFloraKind::None
    }
}

/// One fully sampled world column. Chunk generation samples these once and
/// reuses them for terrain, padding, trees, bushes and loose detail, so adding
/// graph nodes does not multiply noise cost per detail cell.
#[derive(Clone, Copy)]
pub(crate) struct ColumnInfo {
    height: i32,
    surface: Block,
    has_turf: bool,
    has_snow: bool,
    supports_flora: bool,
    forest: f32,
    sample: BiomeSample,
    /// True when the cave field breaks through this column's surface.
    cave_mouth: bool,
}

/// A compact spherical chamber attached to one worm layer. Rooms are resolved
/// once at generation time, so per-voxel work is just a bounded local lookup.
#[derive(Clone, Copy)]
struct CaveRoom {
    layer: i32,
    x: i32,
    z: i32,
    /// Depth below the local surface at the room centre.
    depth: f64,
    radius: f64,
}

/// A narrow junction between two neighbouring worm layers. Its capsule spans
/// the gap between both tunnel centres and is kept only where both worms exist.
#[derive(Clone, Copy, Debug)]
struct CaveConnector {
    x: i32,
    z: i32,
    shallow_depth: f64,
    deep_depth: f64,
    radius: f64,
}

/// A short slanted tunnel from the surface to a first-layer worm. The lateral
/// offset is what keeps an entrance from reading as a vertical pit.
#[derive(Clone, Copy, Debug)]
struct CaveMouth {
    surface_x: f64,
    surface_y: f64,
    surface_z: f64,
    target_x: f64,
    target_y: f64,
    target_z: f64,
    radius: f64,
}

/// Procedural terrain generator. Cheap to clone, but normally shared through an
/// `Arc` so worker tasks can borrow it.
pub struct WorldGen {
    pub seed: u32,
    noise: [Fbm<OpenSimplex>; NOISE_FIELD_COUNT],
    cave_noise: [OpenSimplex; CAVE_NOISE_FIELD_COUNT],
    strata_noise: OpenSimplex,
    /// Compact rooms indexed by their coarse horizontal cell.
    rooms: HashMap<(i32, i32), Vec<CaveRoom>>,
    /// Rare vertical worm junctions, indexed by their coarse horizontal cell.
    connectors: HashMap<(i32, i32), Vec<CaveConnector>>,
    /// Accepted surface entrance tunnels, indexed by their coarse grid cell.
    mouths: HashMap<(i32, i32), CaveMouth>,
}

impl WorldGen {
    pub fn new(seed: u32) -> Self {
        let mut generator = Self {
            seed,
            noise: std::array::from_fn(|index| {
                let definition = &NOISE_FIELDS[index];
                Fbm::<OpenSimplex>::new(seed.wrapping_add(definition.seed_offset))
                    .set_octaves(definition.octaves)
                    .set_frequency(definition.frequency)
                    .set_persistence(definition.persistence)
            }),
            cave_noise: std::array::from_fn(|index| {
                OpenSimplex::new(seed ^ CAVE_NOISE_SEED_OFFSETS[index])
            }),
            strata_noise: OpenSimplex::new(seed ^ GEOLOGY_STRATA_SEED_OFFSET),
            rooms: HashMap::new(),
            connectors: HashMap::new(),
            mouths: HashMap::new(),
        };

        // Rooms, junctions and entrances are sparse, so resolving them once keeps
        // per-voxel generation cheap. Every room and junction attaches to real
        // worms; every entrance stops at the first worm layer.
        let room_limit = LAND_HALF_EXTENT / CAVE_ROOM_CELL_SIZE + 2;
        for cell_x in -room_limit..=room_limit {
            for cell_z in -room_limit..=room_limit {
                let rooms = generator.cave_rooms_for_cell(cell_x, cell_z);
                if !rooms.is_empty() {
                    generator.rooms.insert((cell_x, cell_z), rooms);
                }
            }
        }

        let connector_limit = LAND_HALF_EXTENT / CAVE_CONNECTOR_CELL_SIZE + 2;
        for cell_x in -connector_limit..=connector_limit {
            for cell_z in -connector_limit..=connector_limit {
                let connectors = generator.cave_connectors_for_cell(cell_x, cell_z);
                if !connectors.is_empty() {
                    generator.connectors.insert((cell_x, cell_z), connectors);
                }
            }
        }

        let mouth_limit = LAND_HALF_EXTENT / CAVE_MOUTH_CELL_SIZE + 2;
        for cell_x in -mouth_limit..=mouth_limit {
            for cell_z in -mouth_limit..=mouth_limit {
                if let Some(tunnel) = generator.cave_mouth_tunnel(cell_x, cell_z) {
                    generator.mouths.insert((cell_x, cell_z), tunnel);
                }
            }
        }
        generator
    }

    /// Evaluate the declarative data graph for one 1 m column.
    fn column_at(&self, x: i32, z: i32) -> ColumnInfo {
        let origin_distance = ((x as f32) * (x as f32) + (z as f32) * (z as f32)).sqrt();
        let (_, sample) = biome_cell_at(self.seed, x, z, origin_distance);
        let mut context = GraphContext {
            noise: [0.0; NOISE_FIELD_COUNT],
            shore_blend: shore_blend(x, z),
            biome: sample.params(),
        };
        for (field, noise) in self.noise.iter().enumerate() {
            context.noise[field] = noise.get([x as f64, z as f64]);
        }

        let height = eval_graph(&HEIGHT_GRAPH, &context)
            .round()
            .clamp(OCEAN_FLOOR_WORLD as f32, sample.max_height as f32) as i32;
        let near_shore = footprint_edge_distance(x, z) <= SHORE_BLEND_WIDTH;
        let biome = sample.definition();
        let inland = in_land_footprint(x, z) && !near_shore;
        let surface = if !inland { Block::Sand } else { biome.surface };
        let cave_mouth = self.cave_at(x, height, z, height);
        let dry_inland = inland && height >= SEA_LEVEL_WORLD;

        // Vegetation and the visible turf layer are separate rules: forest soil
        // is dark humus with only patchy grass, but it still carries trees.
        // Natural cave openings remove the surface voxel, so nothing may grow
        // over them.
        // Flora can grow on soil and, sparsely, on hardy dry surfaces (sand/red
        // sandstone) when the biome allows it; turf/snow remain soil/rock rules.
        let flora_surface = surface.is_soil()
            || matches!(surface, Block::Sand | Block::RedSandstone | Block::Granite);
        let supports_flora = dry_inland && !cave_mouth && biome.flora && flora_surface;
        let turf_roll = (hash2(self.seed ^ 0x8d3a_91ef, x, z) & 0xffff) as f32 / 65535.0;
        let has_turf = supports_flora && surface.is_soil() && turf_roll < sample.turf_coverage;

        // Snow follows the same raised-surface model as turf: the rock below is
        // left intact and a thin white layer sits on its exposed top face. It is
        // restricted to dry, exposed alpine rock, never buried inside it.
        let snow_roll = (hash2(self.seed ^ 0x2f1b_4c7d, x, z) & 0xffff) as f32 / 65535.0;
        let has_snow = dry_inland
            && !cave_mouth
            && surface == Block::Granite
            && snow_roll < sample.snow_coverage;
        let forest = eval_graph(&FOREST_GRAPH, &context).clamp(0.0, 1.2);

        ColumnInfo {
            height,
            surface,
            has_turf,
            has_snow,
            supports_flora,
            forest,
            sample,
            cave_mouth,
        }
    }

    /// Sample every world column needed by one chunk, including its padding.
    fn column_grid(&self, coord: IVec3) -> Vec<ColumnInfo> {
        let origin_world = IVec3::new(
            coord.x * WORLD_CHUNK_X as i32,
            coord.y * WORLD_CHUNK_Y as i32,
            coord.z * WORLD_CHUNK_Z as i32,
        );
        let mut columns = Vec::with_capacity(WORLD_PADDED_X * WORLD_PADDED_Z);
        for pz in 0..WORLD_PADDED_Z {
            for px in 0..WORLD_PADDED_X {
                let wx = origin_world.x + px as i32 - WORLD_PAD as i32;
                let wz = origin_world.z + pz as i32 - WORLD_PAD as i32;
                columns.push(self.column_at(wx, wz));
            }
        }
        columns
    }

    #[inline]
    fn padded_column(columns: &[ColumnInfo], origin_world: IVec3, wx: i32, wz: i32) -> &ColumnInfo {
        let px = (wx - origin_world.x + WORLD_PAD as i32) as usize;
        let pz = (wz - origin_world.z + WORLD_PAD as i32) as usize;
        &columns[pz * WORLD_PADDED_X + px]
    }

    /// Terrain surface height at a global world column, in 1 m world voxels.
    pub fn height_at(&self, x: i32, z: i32) -> i32 {
        self.column_at(x, z).height
    }

    /// Dominant biome at a global world column, for the HUD readout.
    pub fn biome_at(&self, x: i32, z: i32) -> BiomeId {
        self.column_at(x, z).sample.dominant
    }

    /// Compact summary for map rendering. Uses the same generator rules as terrain.
    pub fn map_column_at(&self, x: i32, z: i32) -> MapColumn {
        let column = self.column_at(x, z);
        let flora =
            if !column.supports_flora || column.cave_mouth || column.height <= SEA_LEVEL_WORLD {
                MapFloraKind::None
            } else if tree_plan_from_column(self.seed, x, z, &column).is_some() {
                MapFloraKind::Tree
            } else {
                let roll = hash2(self.seed ^ 0x1b87_3593, x, z);
                if (roll & 0xffff) <= column.sample.bush_threshold {
                    MapFloraKind::Bush
                } else {
                    MapFloraKind::None
                }
            };
        MapColumn {
            height: column.height,
            surface: column.surface,
            has_turf: column.has_turf,
            has_snow: column.has_snow,
            biome: column.sample.dominant,
            forest: column.forest,
            supports_flora: column.supports_flora,
            flora,
        }
    }

    /// The solid material at the surface of a column. Sand is limited to the
    /// short coastal strip and the underwater seabed; inland surfaces come from
    /// the selected biome table.
    #[cfg(test)]
    fn surface_block(&self, x: i32, z: i32, _height: i32) -> Block {
        self.column_at(x, z).surface
    }

    /// Whether this column has a grass surface. Only dry topsoil from a turf
    /// biome gets a raised grass layer; coasts and submerged basins stay bare.
    #[cfg(test)]
    fn turf_at(&self, x: i32, z: i32, _height: i32) -> bool {
        self.column_at(x, z).has_turf
    }

    /// Ordered rock layer at a global world voxel. Boundaries undulate gently
    /// in X/Z, but the sequence itself is a biome table, never a random roll.
    fn deep_block(&self, x: i32, y: i32, z: i32, biome: BiomeId, height: i32) -> Block {
        let strata = geology_strata(biome);
        let warp = self.strata_noise.get([
            x as f64 / GEOLOGY_STRATA_SCALE,
            z as f64 / GEOLOGY_STRATA_SCALE,
        ]) as f32
            * GEOLOGY_STRATA_WARP;
        let mut depth = (height - y - GEOLOGY_SURFACE_CAP + warp.round() as i32).max(0) as usize;
        for stratum in strata {
            if depth < stratum.thickness as usize {
                return stratum.block;
            }
            depth -= stratum.thickness as usize;
        }
        strata.last().map_or(Block::Granite, |last| last.block)
    }

    /// Nearest tunnel layer for a depth below the local surface. Only one layer
    /// is evaluated per voxel, so the crust is not filled by overlapping slabs.
    fn cave_layer(depth: i32) -> i32 {
        let position = (depth as f64 - CAVE_FIRST_LAYER_DEPTH) / CAVE_LAYER_SPACING;
        (position.round() as i32).clamp(0, CAVE_LAYER_COUNT - 1)
    }

    /// Vertical centre of one worm layer at a horizontal position. Slow drift
    /// makes tunnels rise and fall instead of forming flat, shelf-like holes.
    /// Adjacent layers sometimes touch where their drift brings them together.
    fn cave_layer_center(&self, layer: i32, x: f64, z: f64) -> f64 {
        let offset = layer as f64 * 19.37;
        let drift = self.cave_noise[CAVE_LAYER_NOISE].get([
            x / CAVE_LAYER_SCALE + offset,
            z / CAVE_LAYER_SCALE - offset * 0.68,
        ]);
        CAVE_FIRST_LAYER_DEPTH + layer as f64 * CAVE_LAYER_SPACING + drift * CAVE_LAYER_DRIFT
    }

    /// Horizontal distance from a worm path, in noise units. Positive means the
    /// column is inside the tube's footprint. This is depth-independent so the
    /// same method can attach rooms and entrances to real tunnels.
    fn cave_worm_margin(&self, x: i32, z: i32, layer: i32) -> f64 {
        let wx = x as f64;
        let wz = z as f64;
        let layer_offset = layer as f64 * 13.71;
        let warp = self.cave_noise[CAVE_WARP_NOISE].get([
            wx / CAVE_WARP_SCALE + layer_offset,
            wz / CAVE_WARP_SCALE - layer_offset * 0.61,
        ]) * CAVE_WARP_METRES;
        let path_x = wx + warp;
        let path_z = wz - warp * 0.75;

        let a = self.cave_noise[CAVE_WORM_A_NOISE].get([
            path_x / CAVE_WORM_A_SCALE + layer_offset * 1.31,
            path_z / CAVE_WORM_A_SCALE - layer_offset * 0.27,
        ]);
        let b = self.cave_noise[CAVE_WORM_B_NOISE].get([
            path_x / CAVE_WORM_B_SCALE - layer_offset * 0.83,
            path_z / CAVE_WORM_B_SCALE + layer_offset * 0.47,
        ]);
        let path_distance = a.abs().min(b.abs());

        let distance = (wx * wx + wz * wz).sqrt() as f32;
        let spawn_widening = (1.0
            - smoothstep(0.0, CAVE_SPAWN_WORM_RADIUS as f32, distance) as f64)
            * CAVE_SPAWN_WORM_BONUS;
        CAVE_WORM_WIDTH + spawn_widening - path_distance
    }

    /// Resolve the discrete spherical rooms in one coarse horizontal cell. A
    /// room is only kept when it sits on a worm, so every chamber opens into the
    /// tunnel network instead of floating as a sealed bubble.
    fn cave_rooms_for_cell(&self, cell_x: i32, cell_z: i32) -> Vec<CaveRoom> {
        let mut rooms = Vec::new();
        let span = CAVE_ROOM_CELL_SIZE - 8;
        for layer in 0..CAVE_LAYER_COUNT {
            let roll =
                (hash3(self.seed ^ 0x51d8_2fb7, cell_x, layer, cell_z) & 0xffff) as f32 / 65535.0;
            if roll >= CAVE_ROOM_CELL_CHANCE {
                continue;
            }

            let jitter = hash3(self.seed ^ 0x2e64_91c3, cell_x, layer, cell_z);
            let x = cell_x * CAVE_ROOM_CELL_SIZE + 4 + (jitter & 0xff) as i32 % span;
            let z = cell_z * CAVE_ROOM_CELL_SIZE + 4 + ((jitter >> 8) & 0xff) as i32 % span;
            let coast = SHORE_BLEND_WIDTH + CAVE_ROOM_MAX_RADIUS as i32;
            if !in_land_footprint(x, z)
                || footprint_edge_distance(x, z) <= coast
                || self.height_at(x, z) < SEA_LEVEL_WORLD
            {
                continue;
            }
            if self.cave_worm_margin(x, z, layer) < CAVE_ROOM_WORM_MARGIN {
                continue;
            }

            let depth = self.cave_layer_center(layer, x as f64, z as f64);
            if depth + CAVE_ROOM_MAX_RADIUS > CAVE_MAX_DEPTH as f64 {
                continue;
            }
            let radius = CAVE_ROOM_MIN_RADIUS
                + ((jitter >> 16) & 0xff) as f64 / 255.0 * CAVE_ROOM_RADIUS_RANGE;
            rooms.push(CaveRoom {
                layer,
                x,
                z,
                depth,
                radius,
            });
        }
        rooms
    }

    /// Positive margin means this voxel is inside a compact room on its layer.
    /// Only nearby coarse cells can contribute because room centres are jittered
    /// inside their own cell and every room is smaller than half a cell.
    fn cave_room_margin(&self, x: i32, z: i32, depth: i32, layer: i32) -> f64 {
        let cell_x = x.div_euclid(CAVE_ROOM_CELL_SIZE);
        let cell_z = z.div_euclid(CAVE_ROOM_CELL_SIZE);
        let wx = x as f64;
        let wz = z as f64;
        let wd = depth as f64;
        let mut best = f64::NEG_INFINITY;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let Some(rooms) = self.rooms.get(&(cell_x + dx, cell_z + dz)) else {
                    continue;
                };
                // A room is a compact sphere on its own layer, but its upper or
                // lower half may cross into the neighbouring layer's depth band.
                for room in rooms.iter().filter(|room| (room.layer - layer).abs() <= 1) {
                    let dx = wx - room.x as f64;
                    let dz = wz - room.z as f64;
                    let dy = wd - room.depth;
                    let distance = (dx * dx + dz * dz + dy * dy).sqrt();
                    best = best.max(room.radius - distance);
                }
            }
        }
        best
    }

    /// Resolve the rare short junctions in one coarse cell. Both neighbouring
    /// worms must pass near the jittered centre, so a junction never creates a
    /// disconnected shaft.
    fn cave_connectors_for_cell(&self, cell_x: i32, cell_z: i32) -> Vec<CaveConnector> {
        let mut connectors = Vec::new();
        let span = CAVE_CONNECTOR_CELL_SIZE - 16;
        for lower_layer in 0..CAVE_LAYER_COUNT - 1 {
            let roll = (hash3(self.seed ^ 0x7c31_4ad9, cell_x, lower_layer, cell_z) & 0xffff)
                as f32
                / 65535.0;
            if roll >= CAVE_CONNECTOR_CELL_CHANCE {
                continue;
            }

            let jitter = hash3(self.seed ^ 0x16b8_5e27, cell_x, lower_layer, cell_z);
            let x = cell_x * CAVE_CONNECTOR_CELL_SIZE + 8 + (jitter & 0xff) as i32 % span;
            let z = cell_z * CAVE_CONNECTOR_CELL_SIZE + 8 + ((jitter >> 8) & 0xff) as i32 % span;
            let coast = SHORE_BLEND_WIDTH + CAVE_CONNECTOR_MIN_RADIUS as i32 + 2;
            if !in_land_footprint(x, z)
                || footprint_edge_distance(x, z) <= coast
                || self.height_at(x, z) < SEA_LEVEL_WORLD
            {
                continue;
            }
            if self.cave_worm_margin(x, z, lower_layer) < CAVE_CONNECTOR_WORM_MARGIN
                || self.cave_worm_margin(x, z, lower_layer + 1) < CAVE_CONNECTOR_WORM_MARGIN
            {
                continue;
            }

            let shallow_depth = self.cave_layer_center(lower_layer, x as f64, z as f64);
            let deep_depth = self.cave_layer_center(lower_layer + 1, x as f64, z as f64);
            if deep_depth + CAVE_CONNECTOR_MIN_RADIUS > CAVE_MAX_DEPTH as f64 {
                continue;
            }
            let radius = CAVE_CONNECTOR_MIN_RADIUS
                + ((jitter >> 16) & 0xff) as f64 / 255.0 * CAVE_CONNECTOR_RADIUS_RANGE;
            connectors.push(CaveConnector {
                x,
                z,
                shallow_depth,
                deep_depth,
                radius,
            });
        }
        connectors
    }

    /// Positive margin means the voxel is inside a short junction between two
    /// worms. Centres stay at least eight metres inside their cell and the
    /// capsule is under four metres wide, so only the voxel's own cell matters.
    fn cave_connector_margin(&self, x: i32, z: i32, depth: i32) -> f64 {
        let cell_x = x.div_euclid(CAVE_CONNECTOR_CELL_SIZE);
        let cell_z = z.div_euclid(CAVE_CONNECTOR_CELL_SIZE);
        let Some(connectors) = self.connectors.get(&(cell_x, cell_z)) else {
            return f64::NEG_INFINITY;
        };
        let wx = x as f64;
        let wz = z as f64;
        let wd = depth as f64;
        let mut best = f64::NEG_INFINITY;
        for connector in connectors {
            let dx = wx - connector.x as f64;
            let dz = wz - connector.z as f64;
            let clamped = wd.clamp(connector.shallow_depth, connector.deep_depth);
            let dy = wd - clamped;
            let distance = (dx * dx + dz * dz + dy * dy).sqrt();
            best = best.max(connector.radius - distance);
        }
        best
    }

    /// Positive margin means the voxel is cave air. Narrow worm tubes, compact
    /// rooms and rare short junctions are the only underground voids; there is
    /// no field that can thicken into a ravine or a deep shaft.
    fn cave_score(&self, x: i32, y: i32, z: i32, depth: i32, layer: i32) -> f64 {
        let wx = x as f64;
        let wy = y as f64;
        let wz = z as f64;
        let bottom_t = 1.0
            - smoothstep(
                (CAVE_MAX_DEPTH - CAVE_DEPTH_FADE) as f32,
                CAVE_MAX_DEPTH as f32,
                depth as f32,
            ) as f64;
        let fade = (1.0 - bottom_t) * 24.0;

        // Small 3D roughness keeps walls from looking extruded without changing
        // the worm network itself.
        let detail = self.cave_noise[CAVE_DETAIL_NOISE].get([
            wx / CAVE_DETAIL_SCALE,
            wy / CAVE_DETAIL_SCALE,
            wz / CAVE_DETAIL_SCALE,
        ]) * CAVE_DETAIL_WEIGHT;

        let horizontal = self.cave_worm_margin(x, z, layer);
        let center = self.cave_layer_center(layer, wx, wz);
        let vertical = CAVE_WORM_RADIUS - (depth as f64 - center).abs() - fade + detail;
        let room = self.cave_room_margin(x, z, depth, layer) - fade + detail;
        let connector = self.cave_connector_margin(x, z, depth) - fade + detail;
        horizontal.min(vertical).max(room).max(connector)
    }

    /// Resolve one possible entrance cell into a short tunnel. The cell is
    /// accepted only when a first-layer worm lies within the search radius. The
    /// target is chosen as far sideways as possible so the entrance slopes into
    /// the terrain rather than dropping straight down.
    fn cave_mouth_tunnel(&self, cell_x: i32, cell_z: i32) -> Option<CaveMouth> {
        let roll = (hash2(self.seed ^ 0x7a91_3f27, cell_x, cell_z) & 0xffff) as f32 / 65535.0;
        if roll >= CAVE_MOUTH_CELL_CHANCE {
            return None;
        }

        let jitter = hash2(self.seed ^ 0x6d1f_2a93, cell_x, cell_z);
        let span = CAVE_MOUTH_CELL_SIZE - 4;
        let center_x = cell_x * CAVE_MOUTH_CELL_SIZE + 2 + (jitter & 0xff) as i32 % span;
        let center_z = cell_z * CAVE_MOUTH_CELL_SIZE + 2 + ((jitter >> 8) & 0xff) as i32 % span;

        let entrance = self.cave_noise[CAVE_MOUTH_NOISE].get([
            center_x as f64 / CAVE_MOUTH_NOISE_SCALE,
            0.0,
            center_z as f64 / CAVE_MOUTH_NOISE_SCALE,
        ]);
        let distance = ((center_x * center_x + center_z * center_z) as f32).sqrt();
        let spawn_bias = (1.0 - smoothstep(0.0, CAVE_SPAWN_MOUTH_RADIUS as f32, distance) as f64)
            * CAVE_SPAWN_MOUTH_BONUS;
        if entrance < CAVE_MOUTH_NOISE_MIN - spawn_bias {
            return None;
        }

        let surface_height = self.height_at(center_x, center_z);
        let worm_depth = CAVE_FIRST_LAYER_DEPTH as i32;
        let worm_y = surface_height - worm_depth;
        let mut target = None;
        for dz in -CAVE_MOUTH_WORM_RADIUS..=CAVE_MOUTH_WORM_RADIUS {
            for dx in -CAVE_MOUTH_WORM_RADIUS..=CAVE_MOUTH_WORM_RADIUS {
                let tx = center_x + dx;
                let tz = center_z + dz;
                if footprint_edge_distance(tx, tz) <= SHORE_BLEND_WIDTH
                    || self.cave_score(tx, worm_y, tz, worm_depth, 0) < 0.0
                {
                    continue;
                }
                let offset = dx * dx + dz * dz;
                if target.is_none_or(|(_, _, best)| offset > best) {
                    target = Some((tx, tz, offset));
                }
            }
        }
        let (target_x, target_z, _) = target?;
        Some(CaveMouth {
            surface_x: center_x as f64,
            surface_y: surface_height as f64,
            surface_z: center_z as f64,
            target_x: target_x as f64,
            target_y: self.height_at(target_x, target_z) as f64 - CAVE_FIRST_LAYER_DEPTH,
            target_z: target_z as f64,
            radius: CAVE_MOUTH_TUNNEL_RADIUS,
        })
    }

    /// Positive margin means a voxel is inside one of the short entrance
    /// tunnels. Only the three by three nearby cells can contribute because
    /// centres are jittered inside their own cell.
    fn cave_mouth_tunnel_margin(&self, x: i32, y: i32, z: i32) -> f64 {
        let cell_x = x.div_euclid(CAVE_MOUTH_CELL_SIZE);
        let cell_z = z.div_euclid(CAVE_MOUTH_CELL_SIZE);
        let px = x as f64;
        let py = y as f64;
        let pz = z as f64;
        let mut best = f64::NEG_INFINITY;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let Some(&mouth) = self.mouths.get(&(cell_x + dx, cell_z + dz)) else {
                    continue;
                };
                let ax = mouth.target_x - mouth.surface_x;
                let ay = mouth.target_y - mouth.surface_y;
                let az = mouth.target_z - mouth.surface_z;
                let length_squared = ax * ax + ay * ay + az * az;
                let t = (((px - mouth.surface_x) * ax
                    + (py - mouth.surface_y) * ay
                    + (pz - mouth.surface_z) * az)
                    / length_squared)
                    .clamp(0.0, 1.0);
                let cx = mouth.surface_x + ax * t;
                let cy = mouth.surface_y + ay * t;
                let cz = mouth.surface_z + az * t;
                let distance = ((px - cx).powi(2) + (py - cy).powi(2) + (pz - cz).powi(2)).sqrt();
                best = best.max(mouth.radius - distance);
            }
        }
        best
    }

    /// True when a global world voxel should be cave air. Worms, rooms and rare
    /// junctions live below the shallow cap; only short entrance tunnels can
    /// reach daylight, and each one ends at the first worm layer.
    fn cave_at(&self, x: i32, y: i32, z: i32, height: i32) -> bool {
        if !world_y_in_range(y)
            || y > height
            || height < SEA_LEVEL_WORLD
            || footprint_edge_distance(x, z) <= SHORE_BLEND_WIDTH
        {
            return false;
        }
        let depth = height - y;
        if depth > CAVE_MAX_DEPTH {
            return false;
        }

        if depth <= CAVE_MOUTH_TUNNEL_DEPTH && self.cave_mouth_tunnel_margin(x, y, z) >= 0.0 {
            return true;
        }
        let shallow_floor = (CAVE_FIRST_LAYER_DEPTH - CAVE_ROOM_MAX_RADIUS - 1.0) as i32;
        if depth < shallow_floor {
            return false;
        }

        let layer = Self::cave_layer(depth);
        self.cave_score(x, y, z, depth, layer) >= 0.0
    }

    /// The world voxel at a global 1 m coordinate, using an already sampled
    /// column. Generation walks whole columns, so this avoids recomputing the
    /// data graph for every Y voxel.
    fn world_block_from_column(&self, x: i32, y: i32, z: i32, column: &ColumnInfo) -> Block {
        if !world_y_in_range(y) {
            return Block::Air;
        }
        if y == WORLD_Y_MIN {
            return Block::Bedrock;
        }
        if self.cave_at(x, y, z, column.height) {
            return Block::Air;
        }
        if y > column.height {
            return if y <= SEA_LEVEL_WORLD {
                Block::Water
            } else {
                Block::Air
            };
        }
        if column.height - y >= GEOLOGY_SURFACE_CAP {
            self.deep_block(x, y, z, column.sample.dominant, column.height)
        } else {
            column.surface
        }
    }

    /// The world voxel at a global 1 m coordinate. Terrain, water and bedrock
    /// live here; the natural-turf flag on a topsoil voxel marks a raised 0.1 m
    /// grass layer, while trees, bushes and construction are detail objects in
    /// the 0.5 m grid.
    ///
    /// Bedrock exists only at [`WORLD_Y_MIN`]. Every other negative Y below a
    /// column's surface is ordinary geology; submerged columns hold water from
    /// just above their seabed up to sea level, and air sits above that.
    #[cfg(test)]
    pub fn world_block_at(&self, x: i32, y: i32, z: i32) -> Block {
        if !world_y_in_range(y) {
            return Block::Air;
        }
        if y == WORLD_Y_MIN {
            return Block::Bedrock;
        }

        let column = self.column_at(x, z);
        self.world_block_from_column(x, y, z, &column)
    }

    /// Find a safe spawn on dry turf above sea level inside the land footprint.
    pub fn find_spawn(&self) -> Vec3 {
        let scale = DETAILS_PER_WORLD_AXIS;
        let mut best: Option<(i32, i32, i32, i32)> = None; // dist2, x, z, height
        let radius = 512;
        for z in -radius..=radius {
            for x in -radius..=radius {
                if !in_land_footprint(x, z) {
                    continue;
                }
                let column = self.column_at(x, z);
                if !column.supports_flora || column.cave_mouth || column.height <= SEA_LEVEL_WORLD {
                    continue;
                }
                let dist2 = x * x + z * z;
                let replace = match best {
                    None => true,
                    Some((bd, _, _, _)) => dist2 < bd,
                };
                if replace {
                    best = Some((dist2, x, z, column.height));
                }
            }
        }
        if let Some((_, x, z, height)) = best {
            return Vec3::new(
                (x * scale) as f32 + 1.0,
                (height + 1) as f32 * scale as f32 + PANEL_HEIGHT + 0.02,
                (z * scale) as f32 + 1.0,
            );
        }
        Vec3::new(1.0, (SEA_LEVEL_DETAIL + 8) as f32, 1.0)
    }

    /// If a tree should grow at this global world column, returns its trunk
    /// height in detail cells. Whether the whole tree fits inside a particular
    /// Y chunk is checked by the placement code.
    fn tree_height_at(&self, x: i32, z: i32) -> Option<i32> {
        let column = self.column_at(x, z);
        Self::tree_height_from_column(self.seed, x, z, &column)
    }

    fn tree_height_from_column(seed: u32, x: i32, z: i32, column: &ColumnInfo) -> Option<i32> {
        tree_plan_from_column(seed, x, z, column).map(|(h, _)| h)
    }

    /// Public: deterministic tree plan at world column, using same rules as generation.
    pub fn tree_plan_at(&self, x: i32, z: i32) -> Option<(i32, TreeSpecies)> {
        let column = self.column_at(x, z);
        tree_plan_from_column(self.seed, x, z, &column)
    }

    /// Public: deterministic bush plan at world column, using same rules as generation.
    pub fn bush_plan_at(&self, x: i32, z: i32) -> Option<(u32, i32)> {
        let column = self.column_at(x, z);
        if !column.supports_flora || column.cave_mouth || column.height <= SEA_LEVEL_WORLD {
            return None;
        }
        let roll = hash2(self.seed ^ 0x1b87_3593, x, z);
        if (roll & 0xffff) > column.sample.bush_threshold {
            return None;
        }
        let kind = match column.sample.dominant {
            BiomeId::Plains => 0,
            BiomeId::Forest => {
                if ((roll >> 18) & 1) == 0 {
                    1
                } else {
                    2
                }
            }
            BiomeId::Alpine => 3,
            BiomeId::Desert => 4,
            BiomeId::Mesa => 5,
        };
        Some((roll, kind))
    }

    /// Keep a deterministic two-metre gap between tree centres. When two eligible
    /// columns are close, the taller tree wins and equal heights are broken by a
    /// stable hash, so the result is independent of chunk generation order.
    pub fn tree_has_spacing(&self, x: i32, z: i32, trunk: i32) -> bool {
        let own_rank = hash2(self.seed ^ 0x6a1d_2f39, x, z);
        for dz in -2..=2 {
            for dx in -2..=2 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let Some(other) = self.tree_height_at(x + dx, z + dz) else {
                    continue;
                };
                if other > trunk
                    || (other == trunk && hash2(self.seed ^ 0x6a1d_2f39, x + dx, z + dz) < own_rank)
                {
                    return false;
                }
            }
        }
        true
    }

    /// Generate a complete cubic chunk: world terrain, expanded detail terrain,
    /// trees and bushes. Grass is stored as a natural-turf flag on the world
    /// layer. All three arrays include one padding cell on every axis, so the
    /// padded coordinate ranges are -1..=32 for details and -1..=16 for world
    /// voxels.
    pub fn generate_chunk(&self, coord: IVec3) -> ChunkData {
        let mut data = ChunkData::new(coord);
        let origin_world = IVec3::new(
            coord.x * WORLD_CHUNK_X as i32,
            coord.y * WORLD_CHUNK_Y as i32,
            coord.z * WORLD_CHUNK_Z as i32,
        );
        let origin_detail = IVec3::new(
            coord.x * CHUNK_X as i32,
            coord.y * CHUNK_Y as i32,
            coord.z * CHUNK_Z as i32,
        );
        let columns = self.column_grid(coord);

        // 1 m authoritative terrain, including one world voxel of padding on
        // every axis. Grass and snow columns carry a natural surface flag on
        // their top voxel; the mesher renders that flag as a raised 0.1 m layer.
        for pz in 0..WORLD_PADDED_Z {
            for px in 0..WORLD_PADDED_X {
                let wx = origin_world.x + px as i32 - WORLD_PAD as i32;
                let wz = origin_world.z + pz as i32 - WORLD_PAD as i32;
                let column = &columns[pz * WORLD_PADDED_X + px];
                for py in 0..WORLD_PADDED_Y {
                    let wy = origin_world.y + py as i32 - WORLD_PAD as i32;
                    let block = self.world_block_from_column(wx, wy, wz, column);
                    data.set_world_padded(px, py, pz, block);
                    if wy == column.height && block != Block::Air {
                        if column.has_turf {
                            data.set_world_turf_padded(px, py, pz, true);
                        }
                        if column.has_snow {
                            data.set_world_snow_padded(px, py, pz, true);
                        }
                    }
                }
            }
        }

        // Expand the world layer into 0.5 m detail cells for rendering,
        // collision and construction. Both 16 m cubes share chunk borders, and
        // the detail padding samples the same world voxels as the world padding.
        for pz in 0..PADDED_Z {
            for py in 0..PADDED_Y {
                for px in 0..PADDED_X {
                    let detail_x = origin_detail.x + px as i32 - PAD as i32;
                    let detail_y = origin_detail.y + py as i32 - PAD as i32;
                    let detail_z = origin_detail.z + pz as i32 - PAD as i32;
                    let world_x = detail_x.div_euclid(DETAILS_PER_WORLD_AXIS);
                    let world_y = detail_y.div_euclid(DETAILS_PER_WORLD_AXIS);
                    let world_z = detail_z.div_euclid(DETAILS_PER_WORLD_AXIS);
                    let world_px = world_x - origin_world.x + WORLD_PAD as i32;
                    let world_py = world_y - origin_world.y + WORLD_PAD as i32;
                    let world_pz = world_z - origin_world.z + WORLD_PAD as i32;

                    let block = if (0..WORLD_PADDED_X as i32).contains(&world_px)
                        && (0..WORLD_PADDED_Y as i32).contains(&world_py)
                        && (0..WORLD_PADDED_Z as i32).contains(&world_pz)
                    {
                        data.get_world_padded(
                            world_px as usize,
                            world_py as usize,
                            world_pz as usize,
                        )
                        .unwrap_or(Block::Air)
                    } else {
                        Block::Air
                    };
                    data.set_padded(px, py, pz, block);
                }
            }
        }

        self.place_trees(&mut data, &columns, origin_world);
        self.place_bushes(&mut data, &columns, origin_world);
        self.place_natural_details(&mut data, &columns, origin_world);
        data
    }

    /// Scatter small sticks and pebbles on dry ground. These are ordinary
    /// natural detail voxels: non-solid, raycastable, mineable and saved like any
    /// other detail cell. Density is deliberately low so the world still reads
    /// as terrain rather than clutter.
    fn place_natural_details(
        &self,
        data: &mut ChunkData,
        columns: &[ColumnInfo],
        origin_world: IVec3,
    ) {
        let origin_x = data.coord.x * CHUNK_X as i32;
        let origin_z = data.coord.z * CHUNK_Z as i32;
        let scale = DETAILS_PER_WORLD_AXIS;

        // Sticks and pebbles occupy a single detail cell, so only the cell itself
        // has to fit inside this 16 m tall chunk.
        for lz in 1..(CHUNK_Z as i32 - 1) {
            for lx in 1..(CHUNK_X as i32 - 1) {
                let wx = origin_x + lx;
                let wz = origin_z + lz;
                let world_x = wx.div_euclid(scale);
                let world_z = wz.div_euclid(scale);
                if !in_land_footprint(world_x, world_z) {
                    continue;
                }
                let column = Self::padded_column(columns, origin_world, world_x, world_z);
                if column.cave_mouth {
                    continue;
                }
                let height = column.height;
                let base = (height + 1) * scale - data.coord.y * CHUNK_Y as i32;
                if !(0..CHUNK_Y as i32).contains(&base) {
                    continue;
                }
                if data.get_local(lx as usize, base as usize, lz as usize) != Block::Air {
                    continue;
                }

                // Clutter is generated once per 1 m world voxel. One voxel owns
                // the object in the lower-left detail cell, and its geometry is
                // drawn at the block centre instead of in a corner.
                if lx.rem_euclid(scale) != 0 || lz.rem_euclid(scale) != 0 {
                    continue;
                }
                let roll = hash2(self.seed ^ 0x3c6e_f372, world_x, world_z) & 0xffff;
                if !rests_on_ground(data, lx, base, lz) || vegetation_near(data, lx, base, lz, 1) {
                    continue;
                }

                let surface = column.surface;
                let origin = IVec3::new(lx, base, lz);
                if roll < column.sample.stick_threshold && surface.is_soil() {
                    // A thin stick centred across the world voxel with deterministic
                    // silhouette/yaw variation (kept within the voxel).
                    let variant = (hash2(self.seed ^ 0x5ad1_c33b, world_x, world_z) % 12) as u8;
                    set_centered_clutter(
                        data,
                        origin,
                        Shape::Stick,
                        Block::Wood,
                        Orientation::from_index(variant),
                    );
                } else if (column.sample.pebble_start..column.sample.pebble_end).contains(&roll) {
                    // Loose stone matches the biome's characteristic rock. Single
                    // pebbles can appear anywhere pebbles spawn; pairs and trios
                    // only form on rocky stone terrain and remain one voxel/pickup.
                    let rocky_terrain = matches!(
                        surface,
                        Block::Granite
                            | Block::Slate
                            | Block::Shale
                            | Block::Sandstone
                            | Block::Limestone
                            | Block::RedSandstone
                    );
                    let pick = (hash2(self.seed ^ 0xa4d2_19ef, world_x, world_z) % 12) as u8;
                    let variant = if rocky_terrain {
                        pick
                    } else {
                        pick % 4 // singles only off rocky terrain
                    };
                    set_centered_clutter(
                        data,
                        origin,
                        Shape::Pebble,
                        column.sample.definition().pebble,
                        Orientation::from_index(variant),
                    );
                } else if roll >= column.sample.pebble_end
                    && roll < column.sample.pebble_end.saturating_add(90)
                    && surface.is_solid()
                    && !surface.is_water()
                {
                    // Rare natural stone deposit: a single-cell irregular outcrop
                    // cluster, visually larger than loose pebbles and mineable with
                    // the Small Stone Hammer. Deterministic and chunk-boundary safe
                    // through centered clutter placement.
                    let variant = (hash2(self.seed ^ 0x51de_b051, world_x, world_z) % 6) as u8;
                    set_centered_clutter(
                        data,
                        origin,
                        Shape::StoneDeposit,
                        column.sample.definition().pebble,
                        Orientation::from_index(variant),
                    );
                }
            }
        }
    }

    pub fn place_broadleaf_oak<S: VegetationSink + ?Sized>(
        data: &mut S,
        seed: u32,
        trunk_storage: IVec3,
        trunk_h: i32,
        roll: u32,
    ) {
        let foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);
        let trunk_voxel = Voxel::new(Block::Wood).with_shape(Shape::Trunk);
        let lx = trunk_storage.x;
        let base = trunk_storage.y;
        let lz = trunk_storage.z;
        for y in base..=base + trunk_h {
            place_if_air(data, IVec3::new(lx, y, lz), IVec3::ZERO, trunk_voxel);
        }

        let skip_root = Facing::ALL[((roll >> 12) & 3) as usize];
        for facing in Facing::ALL {
            if facing == skip_root && ((roll >> 18) & 1) == 0 {
                continue;
            }
            place_root(data, trunk_storage, facing);
        }

        let branch_plan: [(i32, usize, i32, i32); 4] = [
            (
                trunk_h * 2 / 5,
                ((roll >> 6) & 3) as usize,
                2 + ((roll >> 8) & 1) as i32,
                0,
            ),
            (
                trunk_h / 2 + ((roll >> 9) & 1) as i32,
                ((roll >> 10) & 3) as usize,
                3,
                1,
            ),
            (
                trunk_h * 2 / 3,
                ((roll >> 13) & 3) as usize,
                2 + ((roll >> 15) & 1) as i32,
                0,
            ),
            (trunk_h * 4 / 5, ((roll >> 16) & 3) as usize, 2, 1),
        ];
        let crown_origin = IVec3::new(lx + 1, base, lz + 1);
        for (idx, (by, facing_idx, len, rise)) in branch_plan.into_iter().enumerate() {
            let facing = Facing::ALL[(facing_idx + idx) % 4];
            let by = by.clamp(3, trunk_h - 2);
            let len = len.max(2);
            place_branch(data, trunk_storage, by, facing, len, rise);
            let dir = facing.direction();
            let tip = IVec3::new(
                lx + if dir.x > 0.0 { 1 } else { 0 } + dir.x as i32 * len,
                base + by + rise,
                lz + if dir.z > 0.0 { 1 } else { 0 } + dir.z as i32 * len,
            );
            place_leaf_cluster(
                data,
                crown_origin,
                tip - crown_origin,
                2,
                foliage,
                seed ^ (0xA11C_u32.wrapping_add(idx as u32 * 977)),
            );
        }

        place_leaf_cluster(
            data,
            crown_origin,
            IVec3::new(0, trunk_h, 0),
            3,
            foliage,
            seed ^ 0xC0DE_1234 ^ roll,
        );
        place_leaf_cluster(
            data,
            crown_origin,
            IVec3::new(0, trunk_h + 1, 0),
            2,
            foliage,
            seed ^ 0xC0DE_5678 ^ roll,
        );
        for dy in 0..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dz == 0 && dy == 0 {
                        continue;
                    }
                    if dx * dx + dz * dz <= 1 {
                        place_if_air(
                            data,
                            crown_origin,
                            IVec3::new(dx, trunk_h - 1 + dy, dz),
                            foliage,
                        );
                    }
                }
            }
        }
    }

    pub fn place_birch<S: VegetationSink + ?Sized>(
        data: &mut S,
        seed: u32,
        trunk_storage: IVec3,
        trunk_h: i32,
        roll: u32,
    ) {
        let foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);
        let trunk_voxel = Voxel::new(Block::Wood).with_shape(Shape::Trunk);
        let base = trunk_storage.y;
        for y in base..=base + trunk_h {
            place_if_air(data, trunk_storage.with_y(y), IVec3::ZERO, trunk_voxel);
        }
        // Minimal roots - birch has subtle root flare.
        for facing in [Facing::North, Facing::South] {
            if ((roll >> (8 + facing as u8 as u32)) & 1) == 0 {
                place_root(data, trunk_storage, facing);
            }
        }
        // Tall, narrow, airy crown: small clusters along upper trunk.
        let crown_origin = IVec3::new(trunk_storage.x + 1, base, trunk_storage.z + 1);
        let crown_start = (trunk_h * 2 / 5).max(4);
        for dy in (crown_start..=trunk_h + 2).step_by(2) {
            let r = ((roll >> ((dy % 16) as u32)) & 3) as i32;
            let radius = if dy >= trunk_h { 1 } else { 2 };
            let ox = ((roll >> ((dy + 3) % 16) as u32) & 1) as i32 - if r == 0 { 0 } else { 1 };
            let oz = ((roll >> ((dy + 5) % 16) as u32) & 1) as i32 - if r == 2 { 0 } else { 1 };
            place_leaf_cluster(
                data,
                crown_origin,
                IVec3::new(ox, dy, oz),
                radius,
                foliage,
                seed ^ roll ^ (dy as u32 * 0x9E37),
            );
        }
        // Small upward branches near top.
        for (i, facing) in [Facing::East, Facing::West].into_iter().enumerate() {
            let by = trunk_h - 3 - i as i32;
            if by > 4 {
                place_branch(data, trunk_storage, by, facing, 2, 1);
                let dir = facing.direction();
                let tip = IVec3::new(
                    trunk_storage.x + if dir.x > 0.0 { 1 } else { 0 } + dir.x as i32 * 2,
                    base + by + 1,
                    trunk_storage.z + if dir.z > 0.0 { 1 } else { 0 } + dir.z as i32 * 2,
                );
                place_leaf_cluster(
                    data,
                    crown_origin,
                    tip - crown_origin,
                    1,
                    foliage,
                    seed ^ 0xB17C ^ i as u32,
                );
            }
        }
    }

    pub fn place_pine<S: VegetationSink + ?Sized>(
        data: &mut S,
        seed: u32,
        trunk_storage: IVec3,
        trunk_h: i32,
        roll: u32,
    ) {
        let foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);
        let trunk_voxel = Voxel::new(Block::Wood).with_shape(Shape::Trunk);
        let base = trunk_storage.y;
        for y in base..=base + trunk_h {
            place_if_air(data, trunk_storage.with_y(y), IVec3::ZERO, trunk_voxel);
        }
        for facing in Facing::ALL {
            if ((roll >> (10 + facing as u8 as u32)) & 1) == 0 {
                place_root(data, trunk_storage, facing);
            }
        }
        let crown_origin = IVec3::new(trunk_storage.x + 1, base, trunk_storage.z + 1);
        // Conical layered canopy: wider at bottom, narrow at top.
        let bottom = (trunk_h / 3).max(4);
        let mut layer = 0i32;
        let mut dy = bottom;
        while dy <= trunk_h + 1 {
            let from_top = (trunk_h + 1 - dy).max(0);
            let radius = (1 + from_top / 3).clamp(1, 3);
            place_leaf_cluster(
                data,
                crown_origin,
                IVec3::new(0, dy, 0),
                radius,
                foliage,
                seed ^ 0x917E ^ (layer as u32 * 31),
            );
            // occasional branch nubs
            if radius >= 2 && ((roll >> ((layer as u32 + 4) % 16)) & 1) == 0 {
                let facing = Facing::ALL[(layer as usize + (roll as usize >> 6)) % 4];
                place_branch(data, trunk_storage, dy - 1, facing, radius - 1, 0);
            }
            dy += if radius >= 3 { 3 } else { 2 };
            layer += 1;
        }
        place_leaf_cluster(
            data,
            crown_origin,
            IVec3::new(0, trunk_h + 2, 0),
            1,
            foliage,
            seed ^ 0x71E7,
        );
    }

    pub fn place_spruce<S: VegetationSink + ?Sized>(
        data: &mut S,
        seed: u32,
        trunk_storage: IVec3,
        trunk_h: i32,
        roll: u32,
    ) {
        let foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);
        let trunk_voxel = Voxel::new(Block::Wood).with_shape(Shape::Trunk);
        let base = trunk_storage.y;
        for y in base..=base + trunk_h {
            place_if_air(data, trunk_storage.with_y(y), IVec3::ZERO, trunk_voxel);
        }
        for facing in Facing::ALL {
            place_root(data, trunk_storage, facing);
        }
        let crown_origin = IVec3::new(trunk_storage.x + 1, base, trunk_storage.z + 1);
        // Dense conical spruce: tighter layers than pine.
        let bottom = (trunk_h / 4).max(3);
        let mut dy = bottom;
        let mut i = 0u32;
        while dy <= trunk_h + 2 {
            let from_top = (trunk_h + 2 - dy).max(0);
            let radius = (1 + from_top / 2).clamp(1, 3);
            // Dense cross-shaped/diamond layer.
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if dx.abs() + dz.abs() > radius + 1 {
                        continue;
                    }
                    if dx == 0 && dz == 0 && dy < trunk_h {
                        continue;
                    }
                    let gap = hash3(seed ^ 0x5B2C, dx, dy, dz) & 7;
                    if gap == 0 && dx.abs() + dz.abs() == radius + 1 {
                        continue;
                    }
                    place_if_air(data, crown_origin, IVec3::new(dx, dy, dz), foliage);
                }
            }
            dy += 2;
            i += 1;
            let _ = (roll, i);
        }
        place_if_air(data, crown_origin, IVec3::new(0, trunk_h + 3, 0), foliage);
    }

    pub fn place_hardy_shrub_tree<S: VegetationSink + ?Sized>(
        data: &mut S,
        seed: u32,
        trunk_storage: IVec3,
        trunk_h: i32,
        roll: u32,
    ) {
        let foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);
        let trunk_voxel = Voxel::new(Block::Wood).with_shape(Shape::Trunk);
        let base = trunk_storage.y;
        for y in base..=base + trunk_h {
            place_if_air(data, trunk_storage.with_y(y), IVec3::ZERO, trunk_voxel);
        }
        let skip = Facing::ALL[(roll as usize >> 10) % 4];
        for facing in Facing::ALL {
            if facing != skip {
                place_root(data, trunk_storage, facing);
            }
        }
        let crown_origin = IVec3::new(trunk_storage.x + 1, base, trunk_storage.z + 1);
        // Gnarled, sparse, irregular crown: low and wind-shaped.
        let lean = Facing::ALL[((roll >> 4) & 3) as usize].direction();
        let lean_off = IVec3::new(lean.x as i32, 0, lean.z as i32);
        place_leaf_cluster(
            data,
            crown_origin,
            IVec3::new(lean_off.x, trunk_h - 1, lean_off.z),
            2,
            foliage,
            seed ^ 0xAA11 ^ roll,
        );
        place_leaf_cluster(
            data,
            crown_origin,
            IVec3::new(0, trunk_h, 0),
            2,
            foliage,
            seed ^ 0xAA22 ^ roll,
        );
        if ((roll >> 7) & 1) == 0 {
            place_leaf_cluster(
                data,
                crown_origin,
                IVec3::new(-lean_off.x, trunk_h - 2, -lean_off.z),
                1,
                foliage,
                seed ^ 0xAA33 ^ roll,
            );
        }
        // A few twisted branches.
        for (i, facing) in Facing::ALL.into_iter().enumerate() {
            let by = trunk_h / 2 + (i as i32 % 3);
            if by < trunk_h - 1 && ((roll >> (12 + i)) & 1) == 0 {
                let len = 1 + ((roll >> (16 + i)) & 1) as i32;
                place_branch(data, trunk_storage, by, facing, len, 0);
            }
        }
    }

    fn place_bushes(&self, data: &mut ChunkData, columns: &[ColumnInfo], origin_world: IVec3) {
        let origin_x = data.coord.x * CHUNK_X as i32;
        let origin_z = data.coord.z * CHUNK_Z as i32;
        let scale = DETAILS_PER_WORLD_AXIS;

        for lz in 2..(CHUNK_Z as i32 - 2) {
            for lx in 2..(CHUNK_X as i32 - 2) {
                if lx.rem_euclid(scale) != 0 || lz.rem_euclid(scale) != 0 {
                    continue;
                }
                let wx = origin_x + lx;
                let wz = origin_z + lz;
                let world_x = wx.div_euclid(scale);
                let world_z = wz.div_euclid(scale);
                let column = Self::padded_column(columns, origin_world, world_x, world_z);
                if !column.supports_flora {
                    continue;
                }
                let roll = hash2(self.seed ^ 0x1b87_3593, world_x, world_z);
                if (roll & 0xffff) > column.sample.bush_threshold {
                    continue;
                }
                let height = column.height;
                let base = (height + 1) * scale - data.coord.y * CHUNK_Y as i32;
                if base < 0 || base + 4 >= CHUNK_Y as i32 {
                    continue;
                }
                if data.get_local(lx as usize, base as usize, lz as usize) != Block::Air
                    || !rests_on_ground(data, lx, base, lz)
                    || vegetation_near(data, lx, base, lz, 2)
                {
                    continue;
                }

                let origin = IVec3::new(lx, base, lz);
                let kind = match column.sample.dominant {
                    BiomeId::Plains => 0, // temperate broadleaf bush
                    BiomeId::Forest => {
                        if ((roll >> 18) & 1) == 0 {
                            1
                        } else {
                            2
                        }
                    } // ferny / berry
                    BiomeId::Alpine => 3, // conifer shrub
                    BiomeId::Desert => 4, // dry scrub
                    BiomeId::Mesa => 5,   // hardy dry shrub
                };
                Self::place_bush_variant(data, self.seed, origin, roll, kind);
            }
        }
    }

    pub fn place_bush_variant<S: VegetationSink + ?Sized>(
        data: &mut S,
        seed: u32,
        origin: IVec3,
        roll: u32,
        kind: i32,
    ) {
        let leaf = Voxel::new(Block::Leaves).with_shape(Shape::SmallFoliage);
        let stem = Voxel::from_oriented_parts(Block::Wood, Shape::Stem, Orientation::default());
        let place_leaf = |data: &mut S, off: IVec3| {
            place_if_air(data, origin, off, leaf);
        };
        match kind {
            1 => {
                // Forest ferny bush: low, wide, leafy around a short stem.
                place_if_air(data, origin, IVec3::ZERO, stem);
                for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    place_leaf(data, IVec3::new(dx, 1, dz));
                }
                for (dx, dz) in [(0, 0), (1, 0), (0, 1)] {
                    place_leaf(data, IVec3::new(dx, 2, dz));
                }
                if ((roll >> 22) & 1) == 0 {
                    place_leaf(data, IVec3::new(1, 1, -1));
                }
            }
            2 => {
                // Forest dense bush: compact rounded, leaves close to stem.
                place_if_air(data, origin, IVec3::ZERO, stem);
                place_if_air(data, origin, IVec3::Y, stem);
                for (dx, dy, dz) in [
                    (1, 1, 0),
                    (-1, 1, 0),
                    (0, 1, 1),
                    (0, 1, -1),
                    (1, 1, 1),
                    (0, 2, 0),
                    (1, 2, 0),
                    (0, 2, 1),
                ] {
                    place_leaf(data, IVec3::new(dx, dy, dz));
                }
            }
            3 => {
                // Alpine conifer shrub: narrow, pointed, tight foliage.
                place_if_air(data, origin, IVec3::ZERO, stem);
                place_if_air(data, origin, IVec3::Y, stem);
                for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    place_leaf(data, IVec3::new(dx, 1, dz));
                }
                for (dx, dz) in [(0, 0), (1, 0), (0, 1)] {
                    place_leaf(data, IVec3::new(dx, 2, dz));
                }
                place_leaf(data, IVec3::new(0, 3, 0));
            }
            4 => {
                // Desert dry scrub: sparse but not bare, close to woody core.
                place_if_air(data, origin, IVec3::ZERO, stem);
                place_leaf(data, IVec3::new(1, 0, 0));
                place_leaf(data, IVec3::new(0, 1, 0));
                place_leaf(data, IVec3::new(1, 1, 0));
                place_leaf(data, IVec3::new(0, 1, 1));
                if ((roll >> 23) & 1) == 0 {
                    place_leaf(data, IVec3::new(-1, 1, 0));
                }
            }
            5 => {
                // Mesa hardy shrub: low, woody, fuller around stems.
                place_if_air(data, origin, IVec3::ZERO, stem);
                if ((roll >> 20) & 1) == 0 {
                    place_if_air(data, origin, IVec3::new(1, 0, 0), stem);
                    place_leaf(data, IVec3::new(1, 1, 1));
                }
                for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1), (-1, 0), (0, -1)] {
                    place_leaf(data, IVec3::new(dx, 1, dz));
                }
                place_leaf(data, IVec3::new(0, 2, 0));
                place_leaf(data, IVec3::new(1, 2, 0));
            }
            _ => {
                // Plains temperate bush: rounded, leaves hugging the stem.
                place_if_air(data, origin, IVec3::ZERO, stem);
                place_if_air(data, origin, IVec3::Y, stem);
                for (dx, dy, dz) in [
                    (1, 0, 0),
                    (0, 0, 1),
                    (1, 1, 0),
                    (-1, 1, 0),
                    (0, 1, 1),
                    (0, 1, -1),
                    (1, 1, 1),
                    (0, 2, 0),
                    (1, 2, 0),
                    (0, 2, 1),
                ] {
                    place_leaf(data, IVec3::new(dx, dy, dz));
                }
            }
        }
        let _ = (seed, roll);
    }

    fn place_trees(&self, data: &mut ChunkData, columns: &[ColumnInfo], origin_world: IVec3) {
        let origin_x = data.coord.x * CHUNK_X as i32;
        let origin_z = data.coord.z * CHUNK_Z as i32;
        let scale = DETAILS_PER_WORLD_AXIS;
        let _foliage = Voxel::new(Block::Leaves).with_shape(Shape::Foliage);
        let margin = 3 * scale;
        for lz in margin..(CHUNK_Z as i32 - margin) {
            for lx in margin..(CHUNK_X as i32 - margin) {
                if lx.rem_euclid(scale) != 0 || lz.rem_euclid(scale) != 0 {
                    continue;
                }
                let wx = origin_x + lx;
                let wz = origin_z + lz;
                let world_x = wx.div_euclid(scale);
                let world_z = wz.div_euclid(scale);
                let column = Self::padded_column(columns, origin_world, world_x, world_z);
                let Some((trunk_h, species)) =
                    tree_plan_from_column(self.seed, world_x, world_z, column)
                else {
                    continue;
                };
                if !self.tree_has_spacing(world_x, world_z, trunk_h) {
                    continue;
                }

                let height = column.height;
                let base = (height + 1) * scale - data.coord.y * CHUNK_Y as i32;
                let roll = hash2(self.seed ^ 0x51ed_270b, world_x, world_z);
                let crown_top = species.crown_top;
                if base < 1 || base + trunk_h + crown_top >= CHUNK_Y as i32 {
                    continue;
                }
                if data.get_local(lx as usize, base as usize, lz as usize) != Block::Air
                    || data.get_local((lx + 1) as usize, base as usize, (lz + 1) as usize)
                        != Block::Air
                {
                    continue;
                }

                let trunk_storage = IVec3::new(lx, base, lz);
                match species.kind {
                    TreeKind::BroadleafOak => {
                        Self::place_broadleaf_oak(data, self.seed, trunk_storage, trunk_h, roll);
                    }
                    TreeKind::Birch => {
                        Self::place_birch(data, self.seed, trunk_storage, trunk_h, roll);
                    }
                    TreeKind::Pine => {
                        Self::place_pine(data, self.seed, trunk_storage, trunk_h, roll);
                    }
                    TreeKind::Spruce => {
                        Self::place_spruce(data, self.seed, trunk_storage, trunk_h, roll);
                    }
                    TreeKind::HardyShrubTree => {
                        Self::place_hardy_shrub_tree(data, self.seed, trunk_storage, trunk_h, roll);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct CaveComponent {
    size: usize,
    min: IVec3,
    max: IVec3,
}

#[cfg(test)]
impl CaveComponent {
    fn span(self) -> IVec3 {
        self.max - self.min
    }
}

#[cfg(test)]
impl WorldGen {
    fn cave_component(
        generator: &WorldGen,
        x_range: std::ops::Range<i32>,
        y_range: std::ops::Range<i32>,
        z_range: std::ops::Range<i32>,
        seed: Option<IVec3>,
    ) -> Option<CaveComponent> {
        let dims = IVec3::new(
            x_range.end - x_range.start,
            y_range.end - y_range.start,
            z_range.end - z_range.start,
        );
        let (nx, ny, nz) = (dims.x as usize, dims.y as usize, dims.z as usize);
        let origin = IVec3::new(x_range.start, y_range.start, z_range.start);
        let index = |position: IVec3| -> usize {
            let dx = (position.x - origin.x) as usize;
            let dy = (position.y - origin.y) as usize;
            let dz = (position.z - origin.z) as usize;
            (dz * ny + dy) * nx + dx
        };

        let mut open = vec![false; nx * ny * nz];
        for z in z_range.clone() {
            for y in y_range.clone() {
                for x in x_range.clone() {
                    let column = generator.column_at(x, z);
                    open[index(IVec3::new(x, y, z))] = generator.cave_at(x, y, z, column.height);
                }
            }
        }

        fn flood(
            open: &[bool],
            visited: &mut [bool],
            start: usize,
            dims: IVec3,
            origin: IVec3,
        ) -> CaveComponent {
            use std::collections::VecDeque;

            let (nx, ny) = (dims.x as usize, dims.y as usize);
            let decode = |index: usize| -> IVec3 {
                let dz = index / (nx * ny);
                let remainder = index % (nx * ny);
                let dy = remainder / nx;
                let dx = remainder % nx;
                IVec3::new(dx as i32, dy as i32, dz as i32)
            };
            let start_position = origin + decode(start);
            let mut min = start_position;
            let mut max = start_position;
            let mut queue = VecDeque::from([start]);
            visited[start] = true;
            let mut size = 0;

            while let Some(current) = queue.pop_front() {
                size += 1;
                let local = decode(current);
                for (dx, dy, dz) in [
                    (1, 0, 0),
                    (-1, 0, 0),
                    (0, 1, 0),
                    (0, -1, 0),
                    (0, 0, 1),
                    (0, 0, -1),
                ] {
                    let neighbour = local + IVec3::new(dx, dy, dz);
                    if neighbour.x < 0
                        || neighbour.y < 0
                        || neighbour.z < 0
                        || neighbour.x >= dims.x
                        || neighbour.y >= dims.y
                        || neighbour.z >= dims.z
                    {
                        continue;
                    }
                    let next = (neighbour.z as usize * ny + neighbour.y as usize) * nx
                        + neighbour.x as usize;
                    if !open[next] || visited[next] {
                        continue;
                    }
                    visited[next] = true;
                    queue.push_back(next);
                    let position = origin + neighbour;
                    min = IVec3::new(
                        min.x.min(position.x),
                        min.y.min(position.y),
                        min.z.min(position.z),
                    );
                    max = IVec3::new(
                        max.x.max(position.x),
                        max.y.max(position.y),
                        max.z.max(position.z),
                    );
                }
            }

            CaveComponent { size, min, max }
        }

        if let Some(start) = seed {
            if !(x_range.contains(&start.x)
                && y_range.contains(&start.y)
                && z_range.contains(&start.z))
                || !open[index(start)]
            {
                return None;
            }
            let mut visited = vec![false; open.len()];
            return Some(flood(&open, &mut visited, index(start), dims, origin));
        }

        let mut visited = vec![false; open.len()];
        let mut largest: Option<CaveComponent> = None;
        for start in 0..open.len() {
            if !open[start] || visited[start] {
                continue;
            }
            let component = flood(&open, &mut visited, start, dims, origin);
            if largest.is_none_or(|best| component.size > best.size) {
                largest = Some(component);
            }
        }
        largest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::chunk::{DETAILS_PER_WORLD_AXIS, PADDED_X, PADDED_Z};
    use bevy::math::IVec3;

    #[test]
    fn caves_form_large_connected_galleries() {
        let generator = WorldGen::new(0x4B54_414C);
        let component = WorldGen::cave_component(&generator, 96..288, -256..-16, 96..288, None)
            .expect("the sampled underground region should contain cave air");
        let span = component.span();

        assert!(
            component.size > 15_000,
            "the largest cave should contain more than 15,000 connected voxels, got {}",
            component.size
        );
        assert!(
            span.x > 48 && span.z > 48 && span.y > 32,
            "caves should span tens of metres on every axis, got {span:?}"
        );
    }

    #[test]
    fn caves_form_winding_horizontal_tubes() {
        let generator = WorldGen::new(0x4B54_414C);
        let x_range = 128..224;
        let y_range = -176..-16;
        let z_range = 128..224;
        let nx = x_range.end - x_range.start;
        let ny = y_range.end - y_range.start;
        let nz = z_range.end - z_range.start;
        let index = |x: i32, y: i32, z: i32| -> usize {
            (((z - z_range.start) * ny + (y - y_range.start)) * nx + (x - x_range.start)) as usize
        };

        let mut open = vec![false; (nx * ny * nz) as usize];
        for z in z_range.clone() {
            for y in y_range.clone() {
                for x in x_range.clone() {
                    let column = generator.column_at(x, z);
                    open[index(x, y, z)] = generator.cave_at(x, y, z, column.height);
                }
            }
        }
        let air = open.iter().filter(|open| **open).count();
        assert!(
            air > open.len() / 20 && air < open.len() / 5,
            "underground should be cavernous but not porous, got {}/{}",
            air,
            open.len()
        );

        let mut horizontal = 0;
        let mut vertical = 0;
        for z in z_range.clone().take(nz as usize - 1) {
            for y in y_range.clone().take(ny as usize - 1) {
                for x in x_range.clone().take(nx as usize - 1) {
                    let here = index(x, y, z);
                    if !open[here] {
                        continue;
                    }
                    if open[index(x + 1, y, z)] {
                        horizontal += 1;
                    }
                    if open[index(x, y, z + 1)] {
                        horizontal += 1;
                    }
                    if open[index(x, y + 1, z)] {
                        vertical += 1;
                    }
                }
            }
        }
        assert!(
            horizontal > vertical * 3 / 2,
            "caves should run more horizontally than vertically, got {horizontal}/{vertical}"
        );
        const {
            assert!(
                CAVE_MOUTH_TUNNEL_DEPTH < CAVE_FIRST_LAYER_DEPTH as i32 + CAVE_LAYER_SPACING as i32
            );
        }
    }

    #[test]
    fn cave_mouths_are_narrow_and_slanted() {
        let generator = WorldGen::new(0x4B54_414C);
        let mut mouths = 0;
        let mut wide_mouths = 0;
        let mut vertical_mouths = 0;

        for z in (-128..384).step_by(2) {
            for x in (-128..384).step_by(2) {
                let column = generator.column_at(x, z);
                if !column.cave_mouth {
                    continue;
                }
                mouths += 1;
                let height = column.height;

                let mut surface_area = 0;
                for dz in -6..=6 {
                    for dx in -6..=6 {
                        if generator.cave_at(x + dx, height, z + dz, height) {
                            surface_area += 1;
                        }
                    }
                }
                if surface_area > 60 {
                    wide_mouths += 1;
                }

                if generator.cave_at(x, height - 24, z, height)
                    && generator.cave_at(x, height - 32, z, height)
                {
                    vertical_mouths += 1;
                }
            }
        }

        assert!(mouths > 50, "expected enough sampled mouths, got {mouths}");
        assert!(
            wide_mouths * 4 < mouths,
            "mouth openings should be narrow, got {wide_mouths}/{mouths}"
        );
        assert!(
            vertical_mouths * 4 < mouths,
            "entrances should slope into a worm, got {vertical_mouths}/{mouths}"
        );
    }

    #[test]
    fn cave_field_opens_at_the_surface_everywhere() {
        const SPAWN_SAMPLE_RADIUS: i32 = 96;
        let generator = WorldGen::new(0x4B54_414C);
        let spawn = generator.find_spawn();
        let spawn_x = spawn.x.floor() as i32 / DETAILS_PER_WORLD_AXIS;
        let spawn_z = spawn.z.floor() as i32 / DETAILS_PER_WORLD_AXIS;
        let mut nearest_spawn_mouth = i32::MAX;
        let mut eligible = 0;
        let mut mouths = 0;
        let mut spawn_mouths = 0;
        let mut mouth_coords = Vec::new();
        for z in -128..384 {
            for x in -128..384 {
                let column = generator.column_at(x, z);
                if column.height < SEA_LEVEL_WORLD
                    || !in_land_footprint(x, z)
                    || footprint_edge_distance(x, z) <= SHORE_BLEND_WIDTH
                {
                    continue;
                }
                eligible += 1;
                if column.cave_mouth {
                    mouths += 1;
                    if x.abs() <= SPAWN_SAMPLE_RADIUS && z.abs() <= SPAWN_SAMPLE_RADIUS {
                        spawn_mouths += 1;
                    }
                    nearest_spawn_mouth =
                        nearest_spawn_mouth.min((x - spawn_x).abs() + (z - spawn_z).abs());
                    mouth_coords.push((x, z));
                }
            }
        }
        assert!(!mouth_coords.is_empty());
        assert!(
            mouths > 500,
            "caves should open often, got {mouths}/{eligible}"
        );
        assert!(
            mouths < eligible / 20,
            "the surface should not be porous, got {mouths}/{eligible}"
        );
        assert!(
            spawn_mouths > 50,
            "caves must also open near spawn, got {spawn_mouths}"
        );
        assert!(
            nearest_spawn_mouth <= 32,
            "a cave mouth should be a short walk from spawn, got {nearest_spawn_mouth} m"
        );
        let stride = mouth_coords.len().div_ceil(16).max(1);
        let mut deep_mouth = None;
        for (index, &(x, z)) in mouth_coords.iter().enumerate() {
            if index % stride != 0 {
                continue;
            }
            let column = generator.column_at(x, z);
            assert_eq!(
                generator.world_block_at(x, column.height, z),
                Block::Air,
                "a natural cave mouth should open through the surface"
            );
            assert!(!column.has_turf && !column.has_snow);
            assert!(!column.supports_flora);

            let height = column.height;
            let component = WorldGen::cave_component(
                &generator,
                x - 64..x + 65,
                height - 160..height + 1,
                z - 64..z + 65,
                Some(IVec3::new(x, height, z)),
            )
            .expect("the surface mouth should be a generated cave voxel");
            if component.min.y <= height - 64 && component.size > 4_000 {
                deep_mouth = Some(component);
                break;
            }
        }
        assert!(
            deep_mouth.is_some(),
            "at least one sampled mouth should reach deep galleries"
        );
    }

    #[test]
    fn caves_avoid_coasts_and_only_open_through_mouths() {
        let generator = WorldGen::new(7);
        for (x, z) in [
            (LAND_HALF_EXTENT + 1, 0),
            (LAND_HALF_EXTENT - SHORE_BLEND_WIDTH, 0),
        ] {
            let column = generator.column_at(x, z);
            for depth in (8..=120).step_by(16) {
                assert!(
                    !generator.cave_at(x, column.height - depth, z, column.height),
                    "caves should not open into the coast or seabed"
                );
            }
        }

        for z in (128..320).step_by(16) {
            for x in (128..320).step_by(16) {
                let column = generator.column_at(x, z);
                if column.cave_mouth || column.height < SEA_LEVEL_WORLD {
                    continue;
                }
                assert_eq!(
                    generator.world_block_at(x, column.height, z),
                    column.surface,
                    "only natural cave mouths should remove the surface"
                );
            }
        }
    }

    #[test]
    fn caves_match_across_chunk_padding() {
        let generator = WorldGen::new(0x4B54_414C);
        let mut sample = None;
        'search: for cz in 0..20 {
            for cx in 0..20 {
                let world_x = (cx + 1) * WORLD_CHUNK_X as i32 - 1;
                let world_z = (cz + 1) * WORLD_CHUNK_Z as i32 - 1;
                for cy in -6..0 {
                    let world_y = cy * WORLD_CHUNK_Y as i32 + WORLD_CHUNK_Y as i32 / 2;
                    let column = generator.column_at(world_x, world_z);
                    if generator.cave_at(world_x, world_y, world_z, column.height) {
                        sample = Some(IVec3::new(cx, cy, cz));
                        break 'search;
                    }
                }
            }
        }
        let coord = sample.expect("the sampled area should contain a cave chunk");
        let chunk = generator.generate_chunk(coord);
        let east = generator.generate_chunk(coord + IVec3::X);
        let south = generator.generate_chunk(coord + IVec3::Z);
        let above = generator.generate_chunk(coord + IVec3::Y);
        let mut cave_air = 0;

        for py in 0..WORLD_PADDED_Y {
            for pz in 0..WORLD_PADDED_Z {
                let left = chunk.get_world_padded(WORLD_PADDED_X - 1, py, pz);
                let right = east.get_world_padded(WORLD_PAD, py, pz);
                assert_eq!(left, right, "X padding should match across cave chunks");
                if left == Some(Block::Air) {
                    cave_air += 1;
                }
            }
        }
        for py in 0..WORLD_PADDED_Y {
            for px in 0..WORLD_PADDED_X {
                assert_eq!(
                    chunk.get_world_padded(px, py, WORLD_PADDED_Z - 1),
                    south.get_world_padded(px, py, WORLD_PAD),
                    "Z padding should match across cave chunks"
                );
            }
        }
        for pz in 0..WORLD_PADDED_Z {
            for px in 0..WORLD_PADDED_X {
                assert_eq!(
                    chunk.get_world_padded(px, WORLD_PADDED_Y - 1, pz),
                    above.get_world_padded(px, WORLD_PAD, pz),
                    "Y padding should match across cave chunks"
                );
            }
        }
        assert!(cave_air > 0, "the seam sample should cross real cave air");
    }

    #[test]
    fn underground_rock_forms_ordered_biome_layers() {
        let generator = WorldGen::new(0x4B54_414C);
        let mut checked_biomes = std::collections::BTreeSet::new();

        'scan: for z in (-1200..=1200).step_by(48) {
            for x in (-1200..=1200).step_by(48) {
                let biome = generator.biome_at(x, z);
                if checked_biomes.contains(&biome) {
                    continue;
                }
                let column = generator.column_at(x, z);
                if column.height < SEA_LEVEL_WORLD
                    || !in_land_footprint(x, z)
                    || footprint_edge_distance(x, z) <= SHORE_BLEND_WIDTH
                {
                    continue;
                }

                let warp = (generator.strata_noise.get([
                    x as f64 / GEOLOGY_STRATA_SCALE,
                    z as f64 / GEOLOGY_STRATA_SCALE,
                ]) as f32
                    * GEOLOGY_STRATA_WARP)
                    .round() as i32;
                let expected: Vec<Block> = geology_strata(biome)
                    .iter()
                    .flat_map(|stratum| {
                        std::iter::repeat_n(stratum.block, stratum.thickness as usize)
                    })
                    .collect();
                let mut verified = 0;
                for depth in 0..120 {
                    let y = column.height - GEOLOGY_SURFACE_CAP - depth;
                    if generator.cave_at(x, y, z, column.height) {
                        continue;
                    }
                    let layer = (depth + warp).max(0) as usize;
                    assert_eq!(
                        generator.world_block_at(x, y, z),
                        expected[layer],
                        "{biome:?} layer order broke at ({x}, {y}, {z})"
                    );
                    verified += 1;
                }
                if verified > 80 {
                    checked_biomes.insert(biome);
                }
                if checked_biomes.len() == crate::world::generation_data::BIOMES.len() {
                    break 'scan;
                }
            }
        }
        assert_eq!(
            checked_biomes.len(),
            crate::world::generation_data::BIOMES.len(),
            "each biome should have ordered underground layers: {checked_biomes:?}"
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let a = WorldGen::new(1234);
        let b = WorldGen::new(1234);
        let ca = a.generate_chunk(IVec3::new(-2, -1, 3));
        let cb = b.generate_chunk(IVec3::new(-2, -1, 3));
        assert_eq!(ca.blocks, cb.blocks);
        assert_eq!(ca.world_blocks, cb.world_blocks);
        assert_eq!(ca.world_turf, cb.world_turf);
        assert_eq!(ca.world_snow, cb.world_snow);
    }

    #[test]
    fn spawn_cradle_stays_plains_while_distant_columns_vary() {
        let generator = WorldGen::new(0x4B54_414C);
        let radius = PLAINS_CRADLE_RADIUS as i32;
        for z in (-radius..=radius).step_by(8) {
            for x in (-radius..=radius).step_by(8) {
                if x * x + z * z > radius * radius {
                    continue;
                }
                assert_eq!(
                    generator.column_at(x, z).sample.dominant,
                    BiomeId::Plains,
                    "column ({x}, {z}) should stay inside the plains cradle"
                );
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for z in (-1200..=1200).step_by(48) {
            for x in (-1200..=1200).step_by(48) {
                seen.insert(generator.column_at(x, z).sample.dominant);
            }
        }
        assert_eq!(
            seen.len(),
            crate::world::generation_data::BIOMES.len(),
            "every biome table should appear in a wide scan: {seen:?}"
        );
    }

    #[test]
    fn biome_patches_have_local_extent_and_neighbours_can_match() {
        let generator = WorldGen::new(0x4B54_414C);
        let centre = generator.biome_at(1000, 1000);
        for dz in (-8..=8).step_by(8) {
            for dx in (-8..=8).step_by(8) {
                assert_eq!(
                    generator.biome_at(1000 + dx, 1000 + dz),
                    centre,
                    "a 16 m neighbourhood should stay inside one patch"
                );
            }
        }

        let mut matching_neighbours = 0;
        for gz in -12..12 {
            for gx in -12..12 {
                let left =
                    biome_for_cell_roll(hash2(generator.seed ^ BIOME_CELL_BIOME_OFFSET, gx, gz));
                let right = biome_for_cell_roll(hash2(
                    generator.seed ^ BIOME_CELL_BIOME_OFFSET,
                    gx + 1,
                    gz,
                ));
                if left == right {
                    matching_neighbours += 1;
                }
            }
        }
        assert!(
            matching_neighbours > 0,
            "different Voronoi cells may choose the same biome"
        );
    }

    #[test]
    fn biome_borders_blend_heights_without_cliffs() {
        let generator = WorldGen::new(0x4B54_414C);
        let z = 512;
        let mut borders = 0;

        for x in -1200..1200 {
            let left = generator.biome_at(x, z);
            let right = generator.biome_at(x + 1, z);
            if left == right {
                continue;
            }
            borders += 1;
            let left_height = generator.height_at(x, z);
            let right_height = generator.height_at(x + 1, z);
            assert!(
                (left_height - right_height).abs() <= 2,
                "border at x={x}: height jumped {left_height} -> {right_height}"
            );
        }
        assert!(borders > 0, "the scan should cross several biome borders");
    }

    #[test]
    fn teleport_destination_has_a_real_desert_surface() {
        let generator = WorldGen::new(0x4B54_414C);
        let column = generator.column_at(1000, 1000);
        assert_eq!(column.sample.dominant, BiomeId::Desert);
        assert_eq!(column.surface, Block::Sand);
        assert!(!column.has_turf);
        assert!(!column.has_snow);
    }

    #[test]
    fn forest_turf_is_patchy_but_forest_soil_still_grows_trees() {
        let generator = WorldGen::new(0x4B54_414C);
        let mut turf_patches = 0;
        let mut bare_patches = 0;
        let mut trees = 0;

        for z in (-800..=800).step_by(16) {
            for x in (-800..=800).step_by(16) {
                let column = generator.column_at(x, z);
                if column.sample.dominant != BiomeId::Forest || !column.supports_flora {
                    continue;
                }
                assert_eq!(column.surface, Block::Humus);
                if column.has_turf {
                    turf_patches += 1;
                } else {
                    bare_patches += 1;
                }
                if generator.tree_height_at(x, z).is_some() {
                    trees += 1;
                }
            }
        }

        assert!(turf_patches > 0, "forest should have some turf patches");
        assert!(bare_patches > 0, "forest should have bare humus patches");
        assert!(trees > 0, "forest soil should still support wild trees");
    }

    #[test]
    fn alpine_snow_is_a_surface_flag_not_a_buried_cube() {
        let generator = WorldGen::new(0x4B54_414C);
        let mut snowy = None;
        'scan: for z in (-1200..=1200).step_by(16) {
            for x in (-1200..=1200).step_by(16) {
                let column = generator.column_at(x, z);
                if column.has_snow {
                    snowy = Some((x, z, column));
                    break 'scan;
                }
            }
        }
        let (x, z, column) = snowy.expect("the default seed should generate alpine snow");
        assert_eq!(column.sample.dominant, BiomeId::Alpine);
        assert_eq!(column.surface, Block::Granite);
        assert!(column.height >= SEA_LEVEL_WORLD);
        assert!(!column.has_turf);

        let coord = IVec3::new(
            x.div_euclid(WORLD_CHUNK_X as i32),
            column.height.div_euclid(WORLD_CHUNK_Y as i32),
            z.div_euclid(WORLD_CHUNK_Z as i32),
        );
        let data = generator.generate_chunk(coord);
        assert!(
            data.world_blocks.iter().all(|block| *block != Block::Snow),
            "natural snow must never replace the rock surface"
        );
        let (lx, ly, lz) = crate::world::chunk::world_to_local(IVec3::new(x, column.height, z));
        assert!(data.get_world_snow_local(lx, ly, lz));
        assert_eq!(
            data.get_world_local(lx, ly, lz),
            Block::Granite,
            "the snow layer sits above an intact rock voxel"
        );
    }

    #[test]
    fn column_helper_matches_world_block_at() {
        let generator = WorldGen::new(77);
        for (x, z) in [(-9, -3), (0, 0), (17, -24), (2001, 0)] {
            let column = generator.column_at(x, z);
            let height = column.height;
            for y in [
                WORLD_Y_MIN - 1,
                WORLD_Y_MIN,
                -5,
                -1,
                0,
                1,
                height - 3,
                height - 2,
                height,
                height + 1,
                height + 2,
                crate::world::chunk::WORLD_Y_MAX,
            ] {
                assert_eq!(
                    generator.world_block_at(x, y, z),
                    generator.world_block_from_column(x, y, z, &column),
                    "column helper diverged at ({x}, {y}, {z})"
                );
            }
        }
    }

    #[test]
    fn land_is_solid_at_its_surface_inside_footprint() {
        let generator = WorldGen::new(42);
        let height = generator.height_at(4, 4);
        let cy = height.div_euclid(WORLD_CHUNK_Y as i32);
        let local_y = (height - cy * WORLD_CHUNK_Y as i32) as usize;
        let data = generator.generate_chunk(IVec3::new(0, cy, 0));
        assert!(in_land_footprint(4, 4));
        let block = data.get_world_local(4, local_y, 4);
        assert_eq!(block, generator.world_block_at(4, height, 4));
        assert!(block.is_solid(), "land at its surface should be solid");
        assert_ne!(block, Block::Bedrock);
    }

    #[test]
    fn cell_terrain_stays_within_blended_limits_and_holds_inland_water() {
        for seed in [42, 7, 12_345, 0x4B54_414C] {
            let generator = WorldGen::new(seed);
            let mut saw_inland_water = false;
            let mut saw_low_inland_soil = false;
            let mut saw_dry_low_turf = false;
            let mut saw_submerged_bare_soil = false;

            for z in -128..=128 {
                for x in -128..=128 {
                    if !in_land_footprint(x, z) {
                        continue;
                    }
                    let column = generator.column_at(x, z);
                    if column.sample.dominant != BiomeId::Plains {
                        continue;
                    }
                    let height = column.height;
                    assert!(
                        height <= column.sample.max_height,
                        "seed {seed}: column ({x}, {z}) rose to {height}, above its blended limit {}",
                        column.sample.max_height
                    );
                    let surface = column.surface;
                    if generator.world_block_at(x, height, z) == Block::Air {
                        // A cave mouth removes the surface voxel by design.
                        continue;
                    }
                    assert_eq!(generator.world_block_at(x, height, z), surface);
                    if column.has_turf {
                        assert!(
                            height >= SEA_LEVEL_WORLD && surface.is_soil(),
                            "only dry inland soil should carry turf"
                        );
                    }

                    let radius = PLAINS_CRADLE_RADIUS as i32;
                    if height <= SEA_LEVEL_WORLD + 1
                        && x * x + z * z <= radius * radius
                        && footprint_edge_distance(x, z) > SHORE_BLEND_WIDTH
                    {
                        assert_eq!(
                            surface,
                            Block::Topsoil,
                            "low inland basins should not be sandy beaches"
                        );
                        assert_eq!(
                            column.has_turf,
                            height >= SEA_LEVEL_WORLD,
                            "dry sea-level soil should have turf; submerged soil should not"
                        );
                        if height >= SEA_LEVEL_WORLD {
                            saw_dry_low_turf = true;
                        }
                        saw_low_inland_soil = true;
                    }

                    if height < SEA_LEVEL_WORLD {
                        assert_eq!(generator.world_block_at(x, height + 1, z), Block::Water);
                        saw_inland_water = true;
                        if surface.is_soil() {
                            saw_submerged_bare_soil = true;
                        }
                    } else {
                        assert_eq!(generator.world_block_at(x, height + 1, z), Block::Air);
                    }
                }
            }

            assert!(saw_inland_water, "seed {seed}: expected some inland water");
            assert!(
                saw_low_inland_soil,
                "seed {seed}: expected some low inland topsoil"
            );
            assert!(
                saw_dry_low_turf,
                "seed {seed}: expected dry low topsoil with turf"
            );
            assert!(
                saw_submerged_bare_soil,
                "seed {seed}: expected submerged bare topsoil"
            );
        }
    }

    #[test]
    fn shoreline_fades_into_ocean_without_a_cliff() {
        let generator = WorldGen::new(0x4B54_414C);
        let edge = LAND_HALF_EXTENT;

        assert!(
            generator.height_at(edge, 0) < SEA_LEVEL_WORLD,
            "the nominal footprint edge should already be submerged"
        );
        assert_eq!(
            generator.world_block_at(edge, SEA_LEVEL_WORLD, 0),
            Block::Water
        );
        let seabed = generator.height_at(edge + SHORE_BLEND_WIDTH, 0);
        assert!(
            seabed < SEA_LEVEL_WORLD && seabed > OCEAN_FLOOR_WORLD,
            "open ocean should have a shallow, varied seabed rather than the -64 clamp"
        );
        assert_eq!(
            generator.surface_block(edge + SHORE_BLEND_WIDTH, 0, seabed),
            Block::Sand
        );

        let mut previous = generator.height_at(edge - SHORE_BLEND_WIDTH, 0);
        for x in (edge - SHORE_BLEND_WIDTH + 1)..=(edge + SHORE_BLEND_WIDTH) {
            let height = generator.height_at(x, 0);
            assert!(
                (height - previous).abs() <= 4,
                "shore jumped from {previous} to {height} at x={x}"
            );
            previous = height;
        }
    }

    #[test]
    fn ocean_seabed_has_underwater_relief() {
        let generator = WorldGen::new(0x4B54_414C);
        let start = LAND_HALF_EXTENT + SHORE_BLEND_WIDTH + 1;
        let mut heights = Vec::new();

        for offset in (0..320).step_by(8) {
            let height = generator.height_at(start + offset, 0);
            assert!(
                height < SEA_LEVEL_WORLD && height > OCEAN_FLOOR_WORLD,
                "ocean column at x={} should be submerged above the absolute floor: {height}",
                start + offset
            );
            heights.push(height);
        }

        let lowest = *heights.iter().min().unwrap();
        let highest = *heights.iter().max().unwrap();
        assert!(
            highest - lowest >= 4,
            "ocean floor should vary, got {lowest}..{highest}"
        );
    }

    #[test]
    fn ocean_is_water_at_sea_level_outside_footprint() {
        let generator = WorldGen::new(42);
        let ocean_chunk = LAND_HALF_EXTENT / WORLD_CHUNK_X as i32 + 1;
        let data = generator.generate_chunk(IVec3::new(ocean_chunk, 0, 0));
        let world_x = ocean_chunk * WORLD_CHUNK_X as i32;

        assert!(!in_land_footprint(world_x, 0));
        assert_eq!(data.get_world_local(0, 0, 0), Block::Water);
        assert_eq!(data.get_world_local(0, 1, 0), Block::Air);
        // The detail padding samples the same ocean column.
        assert_eq!(data.get_local(0, 0, 0), Block::Water);

        // Below the ocean floor is solid rock, found in a negative-Y chunk.
        let floor_y = OCEAN_FLOOR_WORLD - 1;
        let floor_cy = floor_y.div_euclid(WORLD_CHUNK_Y as i32);
        let floor = generator.generate_chunk(IVec3::new(ocean_chunk, floor_cy, 0));
        let floor_local_y = (floor_y - floor_cy * WORLD_CHUNK_Y as i32) as usize;
        assert!(floor.get_world_local(0, floor_local_y, 0).is_solid());
    }

    #[test]
    fn negative_terrain_is_geology_not_blanket_bedrock() {
        let generator = WorldGen::new(42);
        let coord = IVec3::new(0, -2, 0);
        let below_sea = generator.generate_chunk(coord);
        // This chunk spans world Y -32..=-17. Caves can reach spawn now, so find
        // a solid voxel instead of assuming one fixed coordinate is untouched.
        let origin_y = coord.y * WORLD_CHUNK_Y as i32;
        let mut deep = None;
        'search: for ly in 0..WORLD_CHUNK_Y {
            for lz in 0..WORLD_CHUNK_Z {
                for lx in 0..WORLD_CHUNK_X {
                    let block = below_sea.get_world_local(lx, ly, lz);
                    if block.is_solid() && block != Block::Bedrock {
                        deep = Some((lx, ly, lz, block));
                        break 'search;
                    }
                }
            }
        }
        let (lx, ly, lz, block) = deep.expect("the chunk should contain ordinary deep rock");
        assert_eq!(
            block,
            generator.world_block_at(lx as i32, origin_y + ly as i32, lz as i32)
        );

        // Detail cells below sea are generated geology too, not blanket bedrock.
        let mut detail = None;
        'detail: for ly in 0..CHUNK_Y {
            for lz in 0..CHUNK_Z {
                for lx in 0..CHUNK_X {
                    let voxel = below_sea.get_local(lx, ly, lz);
                    if voxel.is_full_cube() && voxel.block() != Block::Bedrock {
                        detail = Some(voxel);
                        break 'detail;
                    }
                }
            }
        }
        let detail = detail.expect("the detail layer should contain ordinary rock");
        assert_ne!(detail.block(), Block::Bedrock);

        // Bedrock appears only at the global bottom of the world.
        let bottom = generator.generate_chunk(IVec3::new(0, WORLD_Y_MIN / WORLD_CHUNK_Y as i32, 0));
        assert_eq!(bottom.get_world_local(4, 0, 4), Block::Bedrock);
        assert_ne!(bottom.get_world_local(4, 1, 4), Block::Bedrock);
    }

    #[test]
    fn padding_is_generated_on_every_axis_including_y() {
        let generator = WorldGen::new(42);
        let data = generator.generate_chunk(IVec3::new(0, 0, 0));

        // World padding: local Y -1 and 16 in this chunk.
        for (py, wy) in [(0usize, -1i32), (WORLD_PADDED_Y - 1, WORLD_CHUNK_Y as i32)] {
            for pz in [0usize, WORLD_PADDED_Z - 1] {
                for px in [0usize, WORLD_PADDED_X - 1] {
                    let wx = px as i32 - WORLD_PAD as i32;
                    let wz = pz as i32 - WORLD_PAD as i32;
                    assert_eq!(
                        data.get_world_padded(px, py, pz),
                        Some(generator.world_block_at(wx, wy, wz)),
                        "world padding at ({px}, {py}, {pz}) should be generated"
                    );
                }
            }
        }

        // Detail padding: local Y -1 and 32 in this chunk.
        for (py, detail_y) in [(0usize, -1i32), (PADDED_Y - 1, CHUNK_Y as i32)] {
            let world_y = detail_y.div_euclid(DETAILS_PER_WORLD_AXIS);
            for pz in [0usize, PADDED_Z - 1] {
                for px in [0usize, PADDED_X - 1] {
                    let detail_x = px as i32 - PAD as i32;
                    let detail_z = pz as i32 - PAD as i32;
                    let wx = detail_x.div_euclid(DETAILS_PER_WORLD_AXIS);
                    let wz = detail_z.div_euclid(DETAILS_PER_WORLD_AXIS);
                    assert_eq!(
                        data.get_padded(px, py, pz),
                        Voxel::new(generator.world_block_at(wx, world_y, wz)),
                        "detail padding at ({px}, {py}, {pz}) should be generated"
                    );
                }
            }
        }
    }

    #[test]
    fn detail_terrain_expands_one_world_voxel_into_eight_cells() {
        let generator = WorldGen::new(99);
        let (wx, wz) = (0..WORLD_CHUNK_X as i32)
            .flat_map(|x| (0..WORLD_CHUNK_Z as i32).map(move |z| (x, z)))
            .find(|&(x, z)| {
                let height = generator.height_at(x, z);
                generator.world_block_at(x, height, z).is_solid()
            })
            .expect("the first chunk should contain a solid surface column");
        let height = generator.height_at(wx, wz);
        let cy = height.div_euclid(WORLD_CHUNK_Y as i32);
        let local_y = (height - cy * WORLD_CHUNK_Y as i32) as usize;
        let data = generator.generate_chunk(IVec3::new(0, cy, 0));
        let block = data.get_world_local(wx as usize, local_y, wz as usize);
        assert!(block.is_solid());
        for dy in 0..2 {
            for dz in 0..2 {
                for dx in 0..2 {
                    assert_eq!(
                        data.get_local(
                            wx as usize * 2 + dx,
                            local_y * 2 + dy,
                            wz as usize * 2 + dz
                        ),
                        block,
                        "detail cell should match its world voxel"
                    );
                }
            }
        }
    }

    #[test]
    fn spawn_is_above_sea_inside_footprint() {
        let generator = WorldGen::new(7);
        let spawn = generator.find_spawn();
        assert!(spawn.y > SEA_LEVEL_DETAIL as f32);

        // The spawn search must find real grass rather than falling back to the
        // emergency position. Recover the world column from the detail-space
        // spawn point and check it is a turf surface inside the footprint.
        let world_x = ((spawn.x - 1.0) / DETAILS_PER_WORLD_AXIS as f32).round() as i32;
        let world_z = ((spawn.z - 1.0) / DETAILS_PER_WORLD_AXIS as f32).round() as i32;
        assert!(in_land_footprint(world_x, world_z));
        let height = generator.height_at(world_x, world_z);
        assert!(height > SEA_LEVEL_WORLD);
        assert!(generator.turf_at(world_x, world_z, height));
        let surface = (height + 1) as f32 * DETAILS_PER_WORLD_AXIS as f32 + PANEL_HEIGHT;
        assert!(
            (spawn.y - surface).abs() < 0.1,
            "spawn y should sit just above the raised turf surface"
        );
    }

    #[test]
    fn grass_is_a_world_turf_surface_not_a_detail_panel() {
        let generator = WorldGen::new(42);
        let mut found = false;

        'columns: for wz in 0..WORLD_CHUNK_X as i32 {
            for wx in 0..WORLD_CHUNK_X as i32 {
                let height = generator.height_at(wx, wz);
                if !generator.turf_at(wx, wz, height) {
                    continue;
                }
                let cy = height.div_euclid(WORLD_CHUNK_Y as i32);
                let data = generator.generate_chunk(IVec3::new(0, cy, 0));
                let local_y = height - cy * WORLD_CHUNK_Y as i32;

                // The 1 m terrain voxel is a soil and carries the turf flag on
                // its top surface.
                let surface = data.get_world_local(wx as usize, local_y as usize, wz as usize);
                assert!(surface.is_soil(), "unexpected turf surface: {surface:?}");
                assert!(data.get_world_turf_local(wx as usize, local_y as usize, wz as usize));

                // Turf is part of the world voxel, never a detail panel in the
                // layer above it.
                let above_y = (height + 1) * DETAILS_PER_WORLD_AXIS - cy * CHUNK_Y as i32;
                if (0..CHUNK_Y as i32).contains(&above_y) {
                    let above = data.get_local(
                        wx as usize * DETAILS_PER_WORLD_AXIS as usize,
                        above_y as usize,
                        wz as usize * DETAILS_PER_WORLD_AXIS as usize,
                    );
                    assert_ne!(
                        above.block(),
                        Block::Turf,
                        "turf must not be stored as a detail voxel"
                    );
                }
                found = true;
                break 'columns;
            }
        }
        assert!(found, "expected at least one grass column");
    }

    #[test]
    fn natural_details_rest_in_the_cell_above_the_turf() {
        let generator = WorldGen::new(42);
        let mut found = false;

        'chunks: for cy in 0..8 {
            for cz in -1..=1 {
                for cx in -1..=1 {
                    let coord = IVec3::new(cx, cy, cz);
                    let data = generator.generate_chunk(coord);
                    let origin_x = coord.x * CHUNK_X as i32;
                    let origin_z = coord.z * CHUNK_Z as i32;
                    let world_origin_x = coord.x * WORLD_CHUNK_X as i32;
                    let world_origin_z = coord.z * WORLD_CHUNK_Z as i32;

                    for lz in 1..(CHUNK_Z as i32 - 1) {
                        for lx in 1..(CHUNK_X as i32 - 1) {
                            let world_x = (origin_x + lx).div_euclid(DETAILS_PER_WORLD_AXIS);
                            let world_z = (origin_z + lz).div_euclid(DETAILS_PER_WORLD_AXIS);
                            let height = generator.height_at(world_x, world_z);
                            if !generator.turf_at(world_x, world_z, height) {
                                continue;
                            }
                            let base =
                                (height + 1) * DETAILS_PER_WORLD_AXIS - coord.y * CHUNK_Y as i32;
                            if !(0..CHUNK_Y as i32).contains(&base) {
                                continue;
                            }
                            let voxel = data.get_local(lx as usize, base as usize, lz as usize);
                            if !matches!(voxel.shape(), Shape::Stick | Shape::Pebble) {
                                continue;
                            }

                            // The detail occupies the layer directly above the world
                            // voxel and rests on its raised 0.1 m turf layer.
                            assert!(
                                data.sample_world_turf_local(
                                    world_x - world_origin_x,
                                    height - coord.y * WORLD_CHUNK_Y as i32,
                                    world_z - world_origin_z,
                                ),
                                "natural detail should sit on a turf surface"
                            );
                            if base == 0 {
                                continue;
                            }
                            let below =
                                data.get_local(lx as usize, (base - 1) as usize, lz as usize);
                            assert!(
                                below.is_full_cube() && below.block().is_soil(),
                                "natural detail should rest on the topsoil voxel"
                            );
                            found = true;
                            break 'chunks;
                        }
                    }
                }
            }
        }

        assert!(found, "expected at least one natural stick or pebble");
    }

    #[test]
    fn trees_keep_a_two_metre_gap() {
        let generator = WorldGen::new(42);
        let mut centres = std::collections::HashSet::new();

        for cy in 0..8 {
            for cz in -1..=1 {
                for cx in -1..=1 {
                    let coord = IVec3::new(cx, cy, cz);
                    let data = generator.generate_chunk(coord);
                    for lz in (0..CHUNK_Z - 1).step_by(DETAILS_PER_WORLD_AXIS as usize) {
                        for lx in (0..CHUNK_X - 1).step_by(DETAILS_PER_WORLD_AXIS as usize) {
                            let mut found = false;
                            for ly in 0..CHUNK_Y {
                                let mut height = 0;
                                while ly + height < CHUNK_Y
                                    && data.get_local(lx, ly + height, lz).shape() == Shape::Trunk
                                {
                                    height += 1;
                                }
                                if height >= 4 {
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                continue;
                            }
                            // The trunk is stored in the world voxel's lower-left
                            // detail cell but centred in that 1 m block.
                            centres.insert((
                                (data.coord.x * CHUNK_X as i32 + lx as i32)
                                    .div_euclid(DETAILS_PER_WORLD_AXIS),
                                (data.coord.z * CHUNK_Z as i32 + lz as i32)
                                    .div_euclid(DETAILS_PER_WORLD_AXIS),
                            ));
                        }
                    }
                }
            }
        }

        assert!(centres.len() > 1, "expected several generated trees");
        let centres = centres.into_iter().collect::<Vec<_>>();
        for (index, &(ax, az)) in centres.iter().enumerate() {
            for &(bx, bz) in &centres[index + 1..] {
                assert!(
                    ax.abs_diff(bx) > 2 || az.abs_diff(bz) > 2,
                    "trees at ({ax}, {az}) and ({bx}, {bz}) are too close"
                );
            }
        }
    }

    #[test]
    fn bushes_have_leaves_around_their_stems() {
        let generator = WorldGen::new(42);
        let mut found = false;

        'chunks: for cy in 0..8 {
            for cz in -1..=1 {
                for cx in -1..=1 {
                    let data = generator.generate_chunk(IVec3::new(cx, cy, cz));
                    for y in 1..(CHUNK_Y - 2) {
                        for z in 1..(CHUNK_Z - 1) {
                            for x in 1..(CHUNK_X - 1) {
                                if data.get_local(x, y, z).shape() != Shape::Stem {
                                    continue;
                                }
                                let leaves = (y - 1..=y + 1)
                                    .flat_map(|ny| {
                                        (z - 1..=z + 1).flat_map(move |nz| {
                                            (x - 1..=x + 1).map(move |nx| (nx, ny, nz))
                                        })
                                    })
                                    .filter(|(nx, ny, nz)| {
                                        data.get_local(*nx, *ny, *nz).block() == Block::Leaves
                                    })
                                    .count();
                                if leaves >= 6 {
                                    found = true;
                                    break 'chunks;
                                }
                            }
                        }
                    }
                }
            }
        }

        assert!(found, "expected a bush with leaves around its stem");
    }

    #[test]
    fn bush_tops_are_rounded() {
        let generator = WorldGen::new(42);

        for cy in 0..8 {
            for cz in -1..=1 {
                for cx in -1..=1 {
                    let data = generator.generate_chunk(IVec3::new(cx, cy, cz));
                    for y in 2..(CHUNK_Y - 1) {
                        for z in 2..(CHUNK_Z - 2) {
                            for x in 2..(CHUNK_X - 2) {
                                let stem_below = data.get_local(x, y - 2, z);
                                if data.get_local(x, y, z).block() != Block::Leaves
                                    || data.get_local(x, y + 1, z).block() == Block::Leaves
                                    || stem_below.shape() != Shape::Stem
                                    || stem_below.orientation().up().y < 0.5
                                {
                                    continue;
                                }
                                let layer = (-1..=1)
                                    .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
                                    .filter(|(dx, dz)| {
                                        data.get_local(
                                            (x as i32 + dx) as usize,
                                            y,
                                            (z as i32 + dz) as usize,
                                        )
                                        .block()
                                            == Block::Leaves
                                    })
                                    .count();
                                assert!(
                                    layer <= 5,
                                    "bush top at ({x}, {y}, {z}) has {layer} leaves; expected a rounded top"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn trees_have_smaller_limbs_with_leaves() {
        let generator = WorldGen::new(42);
        let mut found_limb = false;

        'chunks: for cy in 0..8 {
            for cz in -1..=1 {
                for cx in -1..=1 {
                    let data = generator.generate_chunk(IVec3::new(cx, cy, cz));
                    for lz in 1..(CHUNK_Z - 1) {
                        for lx in 1..(CHUNK_X - 1) {
                            for y in 1..(CHUNK_Y - 2) {
                                let voxel = data.get_local(lx, y, lz);
                                if voxel.block() != Block::Wood || voxel.shape() != Shape::Trunk {
                                    continue;
                                }

                                // Only consider the base of a real trunk so
                                // chunk/canopy scanning does not re-test upper
                                // segments as shorter trunks.
                                if y > 0 && data.get_local(lx, y - 1, lz).shape() == Shape::Trunk {
                                    continue;
                                }
                                let mut trunk_height = 0;
                                while y + trunk_height < CHUNK_Y
                                    && data.get_local(lx, y + trunk_height, lz).shape()
                                        == Shape::Trunk
                                {
                                    trunk_height += 1;
                                }
                                assert!(
                                    trunk_height >= 6 * DETAILS_PER_WORLD_AXIS as usize,
                                    "generated trunk is only {trunk_height} detail cells tall"
                                );

                                // Limbs are thin stem-shaped wood cells leaving
                                // the centred trunk. Each limb carries foliage on
                                // itself/at its tip (attached at limb level).
                                for dy in 1..trunk_height {
                                    for (dx, dz) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                                        let sx = lx as i32 + if dx > 0 { 1 } else { 0 };
                                        let sz = lz as i32 + if dz > 0 { 1 } else { 0 };
                                        if !(0..CHUNK_X as i32).contains(&sx)
                                            || !(0..CHUNK_Z as i32).contains(&sz)
                                        {
                                            continue;
                                        }
                                        let limb = data.get_local(sx as usize, y + dy, sz as usize);
                                        if limb.block() != Block::Wood
                                            || limb.shape() != Shape::Stem
                                        {
                                            continue;
                                        }
                                        // Branches must lie flat, not point up
                                        // like a bush stem.
                                        assert!(
                                            limb.orientation().up().y.abs() < 0.5,
                                            "tree limb at ({sx}, {}, {sz}) is not horizontal",
                                            y + dy
                                        );
                                        let tip_x = sx + dx;
                                        let tip_z = sz + dz;
                                        let has_tip_leaf = (0..CHUNK_X as i32).contains(&tip_x)
                                            && (0..CHUNK_Z as i32).contains(&tip_z)
                                            && {
                                                let tip = data.get_local(
                                                    tip_x as usize,
                                                    y + dy,
                                                    tip_z as usize,
                                                );
                                                tip.block() == Block::Leaves
                                                    && tip.shape() == Shape::Foliage
                                            };
                                        let side_x = sx - dz;
                                        let side_z = sz + dx;
                                        let has_side_leaf = (0..CHUNK_X as i32).contains(&side_x)
                                            && (0..CHUNK_Z as i32).contains(&side_z)
                                            && {
                                                let side = data.get_local(
                                                    side_x as usize,
                                                    y + dy,
                                                    side_z as usize,
                                                );
                                                side.block() == Block::Leaves
                                                    && side.shape() == Shape::Foliage
                                            };
                                        if has_tip_leaf || has_side_leaf {
                                            found_limb = true;
                                            break 'chunks;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        assert!(
            found_limb,
            "expected a tree limb smaller than the trunk with leaves at its tip"
        );
    }

    #[cfg(test)]
    fn has_nearby_trunk(data: &ChunkData, lx: usize, base: usize, lz: usize) -> bool {
        for dz in -3..=3 {
            for dx in -3..=3 {
                for dy in -2..=8 {
                    let x = lx as i32 + dx;
                    let y = base as i32 + dy;
                    let z = lz as i32 + dz;
                    if !(0..CHUNK_X as i32).contains(&x)
                        || !(0..CHUNK_Y as i32).contains(&y)
                        || !(0..CHUNK_Z as i32).contains(&z)
                    {
                        continue;
                    }
                    let voxel = data.get_local(x as usize, y as usize, z as usize);
                    if voxel.block() == Block::Wood
                        && matches!(voxel.shape(), Shape::Cube | Shape::Trunk)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn natural_details_rest_on_the_surface() {
        let mut total_sticks = 0;
        let mut total_pebbles = 0;
        for seed in [42, 7, 12_345, 0x4B54_414C] {
            let generator = WorldGen::new(seed);
            let mut sticks = 0;
            let mut pebbles = 0;
            let mut per_column: std::collections::BTreeMap<(i32, i32), usize> =
                std::collections::BTreeMap::new();

            for cy in 0..8 {
                for cz in 0..1 {
                    for cx in 0..1 {
                        let coord = IVec3::new(cx, cy, cz);
                        let data = generator.generate_chunk(coord);
                        let origin_x = coord.x * CHUNK_X as i32;
                        let origin_z = coord.z * CHUNK_Z as i32;

                        for lz in 0..CHUNK_Z {
                            for lx in 0..CHUNK_X {
                                let world_x =
                                    (origin_x + lx as i32).div_euclid(DETAILS_PER_WORLD_AXIS);
                                let world_z =
                                    (origin_z + lz as i32).div_euclid(DETAILS_PER_WORLD_AXIS);
                                let height = generator.height_at(world_x, world_z);
                                let base = ((height + 1) * DETAILS_PER_WORLD_AXIS
                                    - coord.y * CHUNK_Y as i32)
                                    as usize;
                                if base == 0 || base >= CHUNK_Y {
                                    continue;
                                }
                                let voxel = data.get_local(lx, base, lz);
                                if !matches!(voxel.shape(), Shape::Stick | Shape::Pebble) {
                                    continue;
                                }
                                if voxel.shape() == Shape::Stick
                                    && has_nearby_trunk(&data, lx, base, lz)
                                {
                                    continue;
                                }

                                // Loose clutter rests on terrain, never on leaves or
                                // another small object, and there is at most one piece
                                // per square metre.
                                let below = data.get_local(lx, base - 1, lz);
                                assert!(
                                    below.is_full_cube(),
                                    "clutter at ({lx}, {base}, {lz}) is not on a full cube"
                                );
                                assert_ne!(below.block(), Block::Leaves);
                                *per_column.entry((world_x, world_z)).or_insert(0) += 1;

                                if voxel.shape() == Shape::Stick {
                                    sticks += 1;
                                    assert_eq!(voxel.block(), Block::Wood);
                                }
                                if voxel.shape() == Shape::Pebble {
                                    pebbles += 1;
                                    assert_eq!(
                                        voxel.block(),
                                        generator
                                            .column_at(world_x, world_z)
                                            .sample
                                            .definition()
                                            .pebble,
                                        "loose stone should match its biome at ({world_x}, {world_z})"
                                    );
                                    // Piles are only allowed on rocky stone terrain.
                                    let rocky = matches!(
                                        generator.column_at(world_x, world_z).surface,
                                        Block::Granite
                                            | Block::Slate
                                            | Block::Shale
                                            | Block::Sandstone
                                            | Block::Limestone
                                            | Block::RedSandstone
                                    );
                                    if !rocky {
                                        assert_eq!(
                                            crate::world::block::pebble_variant_count(
                                                voxel.orientation()
                                            ),
                                            1,
                                            "pebble piles should only spawn on rocky terrain at ({world_x}, {world_z})"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            total_sticks += sticks;
            total_pebbles += pebbles;
            assert!(
                per_column.values().all(|&count| count == 1),
                "seed {seed}: one loose object should be one voxel"
            );
        }

        assert!(total_sticks > 0, "expected scattered sticks");
        assert!(total_pebbles > 0, "expected scattered pebbles");
    }

    #[test]
    #[ignore]
    fn bench_generate_chunk() {
        use std::{hint::black_box, time::Instant};

        let generator = WorldGen::new(0);
        let runs = 10;
        let start = Instant::now();
        let mut world_blocks = 0;
        for _ in 0..runs {
            let data = black_box(generator.generate_chunk(IVec3::ZERO));
            world_blocks = data
                .world_blocks
                .iter()
                .filter(|block| !block.is_air())
                .count();
        }
        let elapsed = start.elapsed() / runs;
        println!(
            "generated chunk: {world_blocks} non-air world voxels, {elapsed:?} per generation"
        );
    }
}
