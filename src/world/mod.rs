//! Voxel world: storage, procedural generation, meshing and chunk streaming.
//!
//! Rendering is intentionally chunked: each chunk is one entity with its own
//! mesh, which lets Bevy frustum-cull it automatically. On top of that we do
//! face culling, greedy meshing, distance culling, and asynchronous
//! generation/meshing so the main thread stays smooth.

pub mod block;
pub mod chunk;
pub mod generation;
pub mod generation_data;
pub mod mesher;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::light::NotShadowCaster;
use bevy::material::AlphaMode;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
    MaterialPlugin,
};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Face, SpecializedMeshPipelineError};
use bevy::tasks::{
    AsyncComputeTaskPool, Task, futures_lite::future::block_on, futures_lite::future::poll_once,
};

use crate::player::Player;

use block::{
    Block, CollisionBoxes, PANEL_HEIGHT, Shape, Voxel, centered_detail_bounds,
    stem_bounds_for_shape, uses_centered_detail_bounds,
};
use chunk::{
    BLOCK_SIZE, CHUNK_X, CHUNK_Y, CHUNK_Z, Chunk, ChunkData, DETAIL_Y_MAX, DETAIL_Y_MIN,
    DETAILS_PER_WORLD_AXIS, PAD, SurfaceLayer, WORLD_CHUNK_X, WORLD_CHUNK_Y, WORLD_CHUNK_Z,
    WORLD_PAD, WORLD_Y_MIN, chunk_origin, chunk_world_origin, detail_to_chunk, detail_to_local,
    detail_to_world, detail_y_in_range, position_to_chunk, world_to_chunk, world_to_detail_origin,
    world_to_local, world_voxel_cells, world_y_in_range,
};
pub use generation::MapColumn;
use generation::WorldGen;
use generation_data::BiomeId;
use mesher::{ChunkMeshData, mesh_chunk};

/// Chunks generated / meshed per frame while streaming.
const STREAM_BUDGET: usize = 6;
/// Maximum number of background jobs in flight.
const MAX_PENDING: usize = 12;
/// Initial startup loading area: 8x3x8 chunks centered around the spawn chunk.
/// Even horizontal size uses offsets -4..=3 (4 chunks each side of the spawn
/// chunk boundary), keeping the region balanced around spawn.
const INITIAL_LOAD_RADIUS_XZ: i32 = 4; // produces 8: -4..=3
const INITIAL_LOAD_RADIUS_Y: i32 = 1; // produces 3: -1..=1
/// Synchronous teleport safety patch remains small to avoid long freezes.
const TELEPORT_PRELOAD_BUDGET_MS: u128 = 60;
const TELEPORT_PRELOAD_RADIUS: i32 = 2;
const CHUNK_FADE_IN_SECS: f32 = 0.35;

#[derive(Component)]
struct ChunkFadeIn {
    remaining: f32,
    total: f32,
}
/// Chunks are kept a little beyond the render radius to avoid popping.
const UNLOAD_MARGIN: i32 = 2;
/// Vertical streaming radius is capped so a large horizontal view distance
/// does not explode the number of loaded chunks into a tall column.
pub const VERTICAL_VIEW_CHUNKS: i32 = 3;
/// Extra depth loaded below the normal band when the player is underground,
/// falling, or looking down. Caves reach about 320 m down, so this tail is 20
/// detail chunks (320 m) below the player's chunk.
pub const DEEP_TAIL_DEPTH: i32 = 20;
/// Horizontal radius of the deep tail. Three chunks keep large chambers in
/// view without multiplying the full horizontal cylinder.
pub const DEEP_TAIL_RADIUS: i32 = 3;

/// Reserve this many pending slots for velocity-predicted urgent chunks.
const URGENT_RESERVED: usize = 4;
/// Keep a preempted chunk out of the queue briefly so changing priorities do
/// not cancel and restart the same generation work every frame.
const URGENT_SUPPRESS_FRAMES: u64 = 90;
/// How far ahead velocity prediction looks, in seconds.
const PREDICT_SECONDS: f32 = 0.75;
/// Number of samples along the predicted velocity path.
const PREDICT_SAMPLES: usize = 6;
/// Number of chunks sampled along the camera direction for tier 2.
const CAMERA_SAMPLE_CHUNKS: i32 = 4;
/// Detail-cell vertical speed that counts as actively falling.
const DEEP_TAIL_FALL_SPEED: f32 = -4.0;
/// Hysteresis thresholds for enabling the deep tail while looking down.
const DEEP_TAIL_LOOK_DOWN_ENTER: f32 = -0.30;
const DEEP_TAIL_LOOK_DOWN_EXIT: f32 = -0.15;

/// Scheduling class for a background generation task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Urgency {
    Normal,
    Urgent,
}

/// One pending generation task plus the metadata needed to prioritize and,
/// when necessary, preempt it.
pub(crate) struct PendingChunk<T = Task<GeneratedChunk>> {
    task: T,
    urgency: Urgency,
    /// Squared chunk distance from the player when the task was queued. Used to
    /// evict the least useful normal task when urgent work needs a slot.
    distance: f32,
}

/// The streamed volume of chunks around the player.
///
/// The normal band is a full horizontal cylinder. When underground, falling, or
/// looking down, a narrow tail extends that volume far below the player so deep
/// caves have real geometry instead of a hole to the sky.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamVolume {
    horizontal: i32,
    vertical: i32,
    deep_tail: bool,
    tail_radius: i32,
    tail_bottom: i32,
}

impl StreamVolume {
    fn new(horizontal: i32, vertical: i32, deep_tail: bool) -> Self {
        let horizontal = horizontal.max(0);
        Self {
            horizontal,
            vertical: vertical.max(0),
            deep_tail,
            tail_radius: DEEP_TAIL_RADIUS.min(horizontal),
            tail_bottom: -DEEP_TAIL_DEPTH,
        }
    }

    fn expanded(self, margin: i32) -> Self {
        let margin = margin.max(0);
        Self {
            horizontal: self.horizontal + margin,
            vertical: self.vertical + margin,
            deep_tail: self.deep_tail,
            tail_radius: self.tail_radius + margin,
            tail_bottom: self.tail_bottom - margin,
        }
    }

    fn min_y(self) -> i32 {
        if self.deep_tail {
            self.tail_bottom.min(-self.vertical)
        } else {
            -self.vertical
        }
    }

    fn max_y(self) -> i32 {
        self.vertical
    }

    fn radius_for_y(self, dy: i32) -> Option<i32> {
        if (-self.vertical..=self.vertical).contains(&dy) {
            Some(self.horizontal)
        } else if self.deep_tail && (self.tail_bottom..-self.vertical).contains(&dy) {
            Some(self.tail_radius)
        } else {
            None
        }
    }

    fn contains(self, delta: IVec3) -> bool {
        self.radius_for_y(delta.y)
            .is_some_and(|radius| delta.x * delta.x + delta.z * delta.z <= radius * radius)
    }

    fn chunk_count(self) -> usize {
        let mut count = 0;
        for dy in self.min_y()..=self.max_y() {
            if let Some(radius) = self.radius_for_y(dy) {
                for dz in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx * dx + dz * dz <= radius * radius {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }
}

/// Number of chunks in the stream volume used for the settings memory hint.
pub fn estimated_stream_chunk_count(radius: i32, deep_tail: bool) -> usize {
    StreamVolume::new(radius, radius.min(VERTICAL_VIEW_CHUNKS), deep_tail).chunk_count()
}

/// Cache key for the stream candidate list. Rebuilding that list is only
/// necessary when the player changes chunk, the view distance changes, or the
/// deep tail turns on or off. Bulk streaming is distance-centered around the
/// player for predictable loading.
#[derive(Clone, Copy, PartialEq, Eq)]
struct StreamCacheKey {
    center: IVec3,
    horizontal: i32,
    vertical: i32,
    deep_tail: bool,
}

struct StreamCache {
    key: StreamCacheKey,
    candidates: Vec<IVec3>,
    cursor: usize,
}

/// Rolling per-frame timings for the world systems, shown in the F3 overlay.
/// These are wall-clock measurements on the thread running each system, not GPU
/// timings or a replacement for a full profiler.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct WorldTimings {
    pub poll_ms: f32,
    pub stream_ms: f32,
    pub cull_ms: f32,
}

fn record_timing(previous: &mut f32, elapsed: std::time::Duration) {
    let sample = elapsed.as_secs_f32() * 1000.0;
    *previous = if *previous <= 0.0 {
        sample
    } else {
        *previous * 0.9 + sample * 0.1
    };
}

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        let seed = std::env::var("KT_SEED")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0x4B54_414C);

        let generator = Arc::new(WorldGen::new(seed));
        let spawn = generator.find_spawn();

        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, FoliageWindMaterial>,
        >::default())
            .insert_resource(WorldGenResource(generator))
            .insert_resource(SpawnPoint(spawn))
            .init_resource::<ChunkManager>()
            .init_resource::<WorldTimings>()
            .init_resource::<PendingTeleport>()
            .init_resource::<BiomeInfo>()
            .init_resource::<InitialWorldLoad>()
            .add_systems(Startup, setup_world)
            .add_systems(First, apply_pending_teleport)
            .add_systems(
                Update,
                (
                    update_biome_info,
                    poll_chunk_tasks,
                    stream_chunks,
                    animate_chunk_fade_in,
                    distance_culling,
                )
                    .chain(),
            );
    }
}

#[derive(Resource)]
pub struct WorldGenResource(pub Arc<WorldGen>);

/// Current biome under the player, sampled from the generator for the HUD.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct BiomeInfo {
    pub biome: BiomeId,
}

/// Resolve the biome at an entity position, which uses the 0.5 m detail grid.
fn biome_at_position(generator: &WorldGen, position: Vec3) -> BiomeId {
    let metres = position * BLOCK_SIZE;
    generator.biome_at(metres.x.floor() as i32, metres.z.floor() as i32)
}

fn update_biome_info(
    generator: Res<WorldGenResource>,
    player: Query<&Transform, With<Player>>,
    mut info: ResMut<BiomeInfo>,
) {
    let Ok(transform) = player.single() else {
        return;
    };
    let biome = biome_at_position(&generator.0, transform.translation);
    if info.biome != biome {
        info.biome = biome;
    }
}

/// Safe, dry starting position chosen by the generator.
#[derive(Resource, Clone, Copy)]
pub struct SpawnPoint(pub Vec3);

/// Tracks the mandatory initial world preload. Gameplay input is blocked until
/// `ready` is true. The required area is 8x3x8 chunks centered around spawn:
/// x/z offsets -4..=3 (balanced even region), y offsets -1..=1.
#[derive(Resource, Debug, Clone)]
pub struct InitialWorldLoad {
    pub required: HashSet<IVec3>,
    pub loaded: usize,
    pub total: usize,
    pub ready: bool,
}

impl InitialWorldLoad {
    pub fn new(center: IVec3) -> Self {
        let mut required = HashSet::new();
        for dy in -INITIAL_LOAD_RADIUS_Y..=INITIAL_LOAD_RADIUS_Y {
            for dz in -INITIAL_LOAD_RADIUS_XZ..INITIAL_LOAD_RADIUS_XZ {
                for dx in -INITIAL_LOAD_RADIUS_XZ..INITIAL_LOAD_RADIUS_XZ {
                    required.insert(center + IVec3::new(dx, dy, dz));
                }
            }
        }
        let total = required.len();
        Self {
            required,
            loaded: 0,
            total,
            ready: total == 0,
        }
    }

    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            1.0
        } else {
            self.loaded as f32 / self.total as f32
        }
    }

    pub fn mark_loaded(&mut self, coord: IVec3) {
        if self.ready {
            return;
        }
        if self.required.remove(&coord) {
            self.loaded += 1;
            if self.required.is_empty() {
                self.ready = true;
            }
        }
    }
}

impl Default for InitialWorldLoad {
    fn default() -> Self {
        Self {
            required: HashSet::new(),
            loaded: 0,
            total: 0,
            ready: true,
        }
    }
}

/// Destination queued by `/tp` or `/spawn`.
///
/// Teleports are applied before movement in the next frame, after the chunks
/// around the destination have been generated synchronously. Without this,
/// collision treats the not-yet-streamed destination as solid and pushes the
/// player upward through the unloaded column.
#[derive(Resource, Default, Debug)]
pub struct PendingTeleport(pub Option<Vec3>);

/// Marks a chunk root entity so systems can find it.
#[derive(Component)]
pub struct ChunkEntity;

/// Marker material for foliage wind sway. The actual motion is computed in the
/// foliage vertex shader using global time and per-vertex wind weight.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct FoliageWindMaterial {}

impl MaterialExtension for FoliageWindMaterial {
    fn vertex_shader() -> bevy::shader::ShaderRef {
        "shaders/foliage_wind.wesl".into()
    }

    fn fragment_shader() -> bevy::shader::ShaderRef {
        bevy::shader::ShaderRef::Default
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let Some(index) = layout
            .0
            .attribute_ids()
            .iter()
            .position(|id| *id == mesher::ATTRIBUTE_WIND_WEIGHT.id)
        else {
            return Ok(());
        };
        let Some(vertex_layout) = descriptor.vertex.buffers.first_mut() else {
            return Ok(());
        };
        if vertex_layout
            .attributes
            .iter()
            .any(|attribute| attribute.shader_location == mesher::WIND_WEIGHT_LOCATION)
        {
            return Ok(());
        }

        let layout_attribute = &layout.0.layout().attributes[index];
        vertex_layout
            .attributes
            .push(bevy::render::render_resource::VertexAttribute {
                format: layout_attribute.format,
                offset: layout_attribute.offset,
                shader_location: mesher::WIND_WEIGHT_LOCATION,
            });
        Ok(())
    }
}

pub struct GeneratedChunk {
    pub coord: IVec3,
    pub data: ChunkData,
    pub opaque: Option<Mesh>,
    pub foliage: Option<Mesh>,
    pub water: Option<Mesh>,
    pub triangles: usize,
    pub foliage_triangles: usize,
    pub water_triangles: usize,
}

/// The result of a voxel raycast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// The solid voxel the ray hit.
    pub block: IVec3,
    /// Axis-aligned normal of the face the ray entered. Used as the placement
    /// direction against shaped voxels.
    pub normal: IVec3,
    pub distance: f32,
}

#[derive(Resource)]
pub struct ChunkManager {
    pub chunks: HashMap<IVec3, Chunk>,
    pub pending: HashMap<IVec3, PendingChunk>,
    pub last_center: Option<IVec3>,
    stream_cache: Option<StreamCache>,
    /// Whether the narrow downward tail is currently part of the stream volume.
    deep_tail_active: bool,
    /// Frame counter used to expire preempted-task suppression.
    frame: u64,
    /// Chunks recently preempted for urgent work. They are skipped briefly so a
    /// slowly changing priority cannot cancel and restart the same task.
    suppressed: HashMap<IVec3, u64>,
    pub view_distance: i32,
    pub opaque_material: Handle<StandardMaterial>,
    /// Swaying foliage material using a custom vertex shader.
    pub foliage_material: Option<Handle<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
    pub water_material: Handle<StandardMaterial>,
    /// Per-chunk fade materials for newly loaded chunks (true alpha fade-in).
    pub opaque_fade_materials: HashMap<IVec3, Handle<StandardMaterial>>,
    pub foliage_fade_materials:
        HashMap<IVec3, Handle<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
    pub water_fade_materials: HashMap<IVec3, Handle<StandardMaterial>>,
    /// Player edits to 0.5 m detail cells, kept for saving.
    pub detail_edits: HashMap<IVec3, Voxel>,
    /// Player edits to the authoritative 1 m world layer, kept for saving.
    pub world_edits: HashMap<IVec3, Block>,
    /// Player changes to the natural-turf flag on world voxels, kept for saving.
    pub turf_edits: HashMap<IVec3, bool>,
    /// Player changes to the natural-snow flag on world voxels, kept for saving.
    pub snow_edits: HashMap<IVec3, bool>,
}

impl Default for ChunkManager {
    fn default() -> Self {
        Self {
            chunks: HashMap::new(),
            pending: HashMap::new(),
            last_center: None,
            stream_cache: None,
            deep_tail_active: false,
            frame: 0,
            suppressed: HashMap::new(),
            view_distance: 8,
            opaque_material: Handle::default(),
            foliage_material: None,
            water_material: Handle::default(),
            opaque_fade_materials: HashMap::new(),
            foliage_fade_materials: HashMap::new(),
            water_fade_materials: HashMap::new(),
            detail_edits: HashMap::new(),
            world_edits: HashMap::new(),
            turf_edits: HashMap::new(),
            snow_edits: HashMap::new(),
        }
    }
}

impl ChunkManager {
    pub fn loaded_chunks(&self) -> usize {
        self.chunks.len()
    }

    pub fn pending_chunks(&self) -> usize {
        self.pending.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.chunks
            .values()
            .map(|chunk| chunk.triangles + chunk.foliage_triangles + chunk.water_triangles)
            .sum()
    }

    /// Horizontal streaming radius in chunks.
    pub fn horizontal_view_distance(&self) -> i32 {
        self.view_distance
    }

    /// Vertical streaming radius in chunks. It is capped independently of the
    /// horizontal radius so a large render distance does not load a huge
    /// vertical column.
    pub fn vertical_view_distance(&self) -> i32 {
        self.view_distance.min(VERTICAL_VIEW_CHUNKS)
    }

    /// World-space distance from the camera at which chunks are hidden. The
    /// normal band and, when active, the deep downward tail both contribute so
    /// visible cave geometry is never culled just because it is far below.
    pub fn cull_distance(&self) -> f32 {
        let horizontal = self.horizontal_view_distance() as f32 * CHUNK_X as f32;
        let downward = if self.deep_tail_active {
            DEEP_TAIL_DEPTH
        } else {
            self.vertical_view_distance()
        };
        let vertical = downward.max(self.vertical_view_distance()) as f32 * CHUNK_Y as f32;
        (horizontal * horizontal + vertical * vertical).sqrt()
    }

    /// Fog end distance. This deliberately ignores the deep tail: fog should
    /// stay anchored to the horizontal view distance even while looking down a
    /// cave, otherwise the surface would visibly clear and darken with pitch.
    pub fn fog_distance(&self) -> f32 {
        let horizontal = self.horizontal_view_distance() as f32 * CHUNK_X as f32;
        let vertical = self.vertical_view_distance() as f32 * CHUNK_Y as f32;
        (horizontal * horizontal + vertical * vertical).sqrt()
    }

    /// Get a block, treating unloaded chunks as solid so the player cannot walk
    /// out of the streamed area. Used by collision.
    pub fn get_block(&self, pos: IVec3) -> Block {
        self.get_block_if_loaded(pos).unwrap_or(Block::Granite)
    }

    /// Get a block only if its chunk is loaded. Used by raycasting and meshing
    /// so we never interact with invisible terrain.
    pub fn get_block_if_loaded(&self, pos: IVec3) -> Option<Block> {
        self.get_voxel_if_loaded(pos).map(Voxel::block)
    }

    /// Get the full voxel (material + shape + orientation) if its chunk is loaded.
    ///
    /// `pos` is a global detail-cell coordinate. Outside the global detail Y
    /// range the result is decided by the world bounds: below the bottom is
    /// bedrock, above the top is air.
    pub fn get_voxel_if_loaded(&self, pos: IVec3) -> Option<Voxel> {
        if !detail_y_in_range(pos.y) {
            return if pos.y < DETAIL_Y_MIN {
                Some(Voxel::new(Block::Bedrock))
            } else {
                Some(Voxel::AIR)
            };
        }
        let coord = detail_to_chunk(pos);
        let chunk = self.chunks.get(&coord)?;
        let (lx, ly, lz) = detail_to_local(pos);
        Some(chunk.data.get_local(lx, ly, lz))
    }

    pub fn is_solid(&self, pos: IVec3) -> bool {
        self.get_block(pos).is_solid()
    }

    /// Get the authoritative 1 m world voxel covering a world position.
    pub fn get_world_block(&self, pos: IVec3) -> Option<Block> {
        if !world_y_in_range(pos.y) {
            return Some(if pos.y < WORLD_Y_MIN {
                Block::Bedrock
            } else {
                Block::Air
            });
        }
        let coord = world_to_chunk(pos);
        let chunk = self.chunks.get(&coord)?;
        let (lx, ly, lz) = world_to_local(pos);
        Some(chunk.data.get_world_local(lx, ly, lz))
    }

    /// Whether a 1 m world voxel carries a raised natural surface layer.
    pub fn get_world_surface(&self, layer: SurfaceLayer, pos: IVec3) -> Option<bool> {
        if !world_y_in_range(pos.y) {
            return Some(false);
        }
        let coord = world_to_chunk(pos);
        let chunk = self.chunks.get(&coord)?;
        let (lx, ly, lz) = world_to_local(pos);
        Some(chunk.data.get_world_surface_local(layer, lx, ly, lz))
    }

    /// Whether a 1 m world voxel carries a raised natural turf surface.
    #[cfg(test)]
    #[inline]
    pub fn get_world_turf(&self, pos: IVec3) -> Option<bool> {
        self.get_world_surface(SurfaceLayer::Turf, pos)
    }

    /// The 1 m terrain voxel a detail cell belongs to, if any. Only untouched
    /// full-cube terrain cells map to their world voxel. Natural surfaces are
    /// part of that world voxel rather than detail overlays.
    pub fn world_terrain_target(&self, detail: IVec3) -> Option<IVec3> {
        let voxel = self.get_voxel_if_loaded(detail)?;
        let world_pos = detail_to_world(detail);
        let world = self.get_world_block(world_pos)?;
        if world.is_solid() && !world.is_water() && voxel.is_full_cube() && voxel.block() == world {
            return Some(world_pos);
        }
        None
    }

    /// The world voxel whose top natural surface a detail cell belongs to.
    ///
    /// The layer is stored as a flag on the 1 m voxel and rendered above it.
    /// Only the topmost detail cell of that voxel counts, and only while that
    /// cell is still the untouched terrain cube, so a constructed piece cannot
    /// expose turf or snow through itself.
    pub fn natural_surface_world(&self, layer: SurfaceLayer, detail: IVec3) -> Option<IVec3> {
        let world = detail_to_world(detail);
        let top_cell = (world.y + 1) * DETAILS_PER_WORLD_AXIS - 1;
        if detail.y != top_cell {
            return None;
        }
        if !self.get_world_surface(layer, world)? {
            return None;
        }
        let block = self.get_world_block(world)?;
        if !block.is_solid() || block.is_water() {
            return None;
        }
        let voxel = self.get_voxel_if_loaded(detail)?;
        if !voxel.is_full_cube() || voxel.block() != block {
            return None;
        }
        Some(world)
    }

    /// The world voxel whose top turf surface a detail cell belongs to.
    #[cfg(test)]
    #[inline]
    pub fn natural_turf_world(&self, detail: IVec3) -> Option<IVec3> {
        self.natural_surface_world(SurfaceLayer::Turf, detail)
    }

    /// True when a detail cell contains a raised natural surface layer of the
    /// world voxel directly below it.
    ///
    /// Turf and snow are rendered as 0.1 m slabs above their world voxel, so the
    /// first detail row above that voxel is only partly occupied. The cell itself
    /// must still be open, otherwise a solid world voxel above has buried it.
    pub fn is_surface_cell(&self, layer: SurfaceLayer, detail: IVec3) -> bool {
        if !detail_y_in_range(detail.y) || detail.y.rem_euclid(DETAILS_PER_WORLD_AXIS) != 0 {
            return false;
        }
        let world = detail_to_world(detail - IVec3::Y);
        if !self.get_world_surface(layer, world).unwrap_or(false) {
            return false;
        }
        let Some(soil) = self.get_world_block(world) else {
            return false;
        };
        if !soil.is_solid() || soil.is_water() {
            return false;
        }
        !self
            .get_world_block(detail_to_world(detail))
            .is_some_and(|block| block.is_opaque())
    }

    /// True when a detail cell contains the raised natural-turf layer.
    #[cfg(test)]
    #[inline]
    pub fn is_turf_surface_cell(&self, detail: IVec3) -> bool {
        self.is_surface_cell(SurfaceLayer::Turf, detail)
    }

    /// Vertical offset applied to natural detail geometry that rests on any
    /// raised natural surface. Structural stems (roots/branches/cores) stay at
    /// ground level so they connect to trunks and terrain.
    pub fn natural_detail_offset(&self, detail: IVec3) -> f32 {
        if let Some(voxel) = self.get_voxel_if_loaded(detail)
            && (voxel.shape().is_wood_stem() || voxel.shape().is_wood_trunk())
        {
            return 0.0;
        }
        if SurfaceLayer::ALL
            .iter()
            .any(|layer| self.is_surface_cell(*layer, detail))
        {
            PANEL_HEIGHT
        } else {
            0.0
        }
    }

    /// Update the world layer and refresh padding in adjacent chunks. The edit
    /// is recorded for the save file.
    pub fn set_world_block(&mut self, pos: IVec3, block: Block) -> Vec<IVec3> {
        let dirty = self.set_world_block_untracked(pos, block);
        if !dirty.is_empty() {
            self.world_edits.insert(pos, block);
        }
        dirty
    }

    /// Update the world layer without touching the save edit log. Used when a
    /// save file is being restored.
    pub fn set_world_block_untracked(&mut self, pos: IVec3, block: Block) -> Vec<IVec3> {
        let mut dirty = Vec::new();
        if !world_y_in_range(pos.y) {
            return dirty;
        }

        let center = world_to_chunk(pos);
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let coord = center + IVec3::new(dx, dy, dz);
                    let Some(chunk) = self.chunks.get_mut(&coord) else {
                        continue;
                    };
                    let local = pos - chunk_world_origin(coord);
                    if local.x < -(WORLD_PAD as i32)
                        || local.x > WORLD_CHUNK_X as i32
                        || local.y < -(WORLD_PAD as i32)
                        || local.y > WORLD_CHUNK_Y as i32
                        || local.z < -(WORLD_PAD as i32)
                        || local.z > WORLD_CHUNK_Z as i32
                    {
                        continue;
                    }
                    chunk.data.set_world_padded(
                        (local.x + WORLD_PAD as i32) as usize,
                        (local.y + WORLD_PAD as i32) as usize,
                        (local.z + WORLD_PAD as i32) as usize,
                        block,
                    );
                    dirty.push(coord);
                }
            }
        }
        dirty
    }

    /// Update a natural surface flag on a world voxel and refresh padding in
    /// adjacent chunks. The edit is recorded for the save file.
    pub fn set_world_surface(
        &mut self,
        layer: SurfaceLayer,
        pos: IVec3,
        present: bool,
    ) -> Vec<IVec3> {
        let dirty = self.set_world_surface_untracked(layer, pos, present);
        if !dirty.is_empty() {
            match layer {
                SurfaceLayer::Turf => {
                    self.turf_edits.insert(pos, present);
                }
                SurfaceLayer::Snow => {
                    self.snow_edits.insert(pos, present);
                }
            }
        }
        dirty
    }

    /// Update the natural-turf flag on a world voxel.
    #[cfg(test)]
    #[inline]
    pub fn set_world_turf(&mut self, pos: IVec3, turf: bool) -> Vec<IVec3> {
        self.set_world_surface(SurfaceLayer::Turf, pos, turf)
    }

    /// Update a natural surface flag without touching the save edit log. Used
    /// when a save file is being restored.
    pub fn set_world_surface_untracked(
        &mut self,
        layer: SurfaceLayer,
        pos: IVec3,
        present: bool,
    ) -> Vec<IVec3> {
        let mut dirty = Vec::new();
        if !world_y_in_range(pos.y) {
            return dirty;
        }

        let center = world_to_chunk(pos);
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let coord = center + IVec3::new(dx, dy, dz);
                    let Some(chunk) = self.chunks.get_mut(&coord) else {
                        continue;
                    };
                    let local = pos - chunk_world_origin(coord);
                    if local.x < -(WORLD_PAD as i32)
                        || local.x > WORLD_CHUNK_X as i32
                        || local.y < -(WORLD_PAD as i32)
                        || local.y > WORLD_CHUNK_Y as i32
                        || local.z < -(WORLD_PAD as i32)
                        || local.z > WORLD_CHUNK_Z as i32
                    {
                        continue;
                    }
                    chunk.data.set_world_surface_padded(
                        layer,
                        (local.x + WORLD_PAD as i32) as usize,
                        (local.y + WORLD_PAD as i32) as usize,
                        (local.z + WORLD_PAD as i32) as usize,
                        present,
                    );
                    dirty.push(coord);
                }
            }
        }
        dirty
    }

    /// Detail cells that should be removed when mining the terrain voxel that
    /// contains `detail`. Only cells still holding the untouched terrain block
    /// are cleared, so a constructed piece inside the same 1 m volume is safe.
    /// Natural turf is a flag on the world voxel, so harvesting it is handled
    /// separately by the interaction layer.
    pub fn terrain_cells_for_mining(&self, detail: IVec3) -> Vec<IVec3> {
        let Some(world) = self.world_terrain_target(detail) else {
            return vec![detail];
        };
        let block = self.get_world_block(world);
        world_voxel_cells(world)
            .into_iter()
            .filter(|cell| {
                self.get_voxel_if_loaded(*cell).is_some_and(|voxel| {
                    voxel.is_full_cube() && voxel.block() == block.unwrap_or(Block::Air)
                })
            })
            .collect()
    }

    /// Physical collision volumes for the voxel at `pos`, in cell-local space.
    ///
    /// Shapes share the same definitions used by targeting: cubes are full
    /// cells, half blocks are half cells, panels are thin mats, stairs are two
    /// boxes and slopes are a four-slice staircase approximation. Unloaded
    /// chunks stay solid so the player cannot walk out of the streamed area.
    pub fn collision_boxes(&self, pos: IVec3) -> Option<CollisionBoxes> {
        if !detail_y_in_range(pos.y) {
            return if pos.y < DETAIL_Y_MIN {
                Some(Shape::Cube.collision_boxes(block::Orientation::default()))
            } else {
                None
            };
        }

        // Gather collision from this cell and any centred-detail shapes whose
        // pre-oriented bounds extend into it (stems/roots/trunks/clutter). Own
        // solid shape is always included; neighbor centred-detail overlaps are
        // merged in so collision matches rendered geometry.
        let mut merged = CollisionBoxes::empty();
        let push_boxes = |source: IVec3, voxel: Voxel, merged: &mut CollisionBoxes| {
            let boxes = voxel.shape().collision_boxes(voxel.orientation());
            if boxes.is_empty() {
                return;
            }
            let delta = (source - pos).as_vec3();
            for (min, max) in boxes.iter() {
                let smin = min + delta;
                let smax = max + delta;
                if smax.x <= 0.0
                    || smin.x >= 1.0
                    || smax.y <= 0.0
                    || smin.y >= 1.0
                    || smax.z <= 0.0
                    || smin.z >= 1.0
                {
                    continue;
                }
                let cmin = smin.max(Vec3::ZERO);
                let cmax = smax.min(Vec3::ONE);
                if cmax.x > cmin.x && cmax.y > cmin.y && cmax.z > cmin.z {
                    merged.push(cmin, cmax);
                }
            }
        };

        let Some(voxel) = self.get_voxel_if_loaded(pos) else {
            let owner = world_to_detail_origin(detail_to_world(pos));
            if let Some(owner_voxel) = self.get_voxel_if_loaded(owner)
                && owner_voxel.is_solid()
                && uses_centered_detail_bounds(owner_voxel.shape())
            {
                push_boxes(owner, owner_voxel, &mut merged);
            }
            if !merged.is_empty() {
                return Some(merged);
            }
            return Some(Shape::Cube.collision_boxes(block::Orientation::default()));
        };

        let mut has_solid_shape = false;
        if voxel.is_solid() && !voxel.is_water() {
            match voxel.shape() {
                Shape::Stick | Shape::Pebble | Shape::Foliage => {}
                _ => {
                    has_solid_shape = true;
                    push_boxes(pos, voxel, &mut merged);
                }
            }
        }

        let owner = world_to_detail_origin(detail_to_world(pos));
        let mut checked = [IVec3::new(i32::MIN, i32::MIN, i32::MIN); 10];
        let mut checked_len = 0usize;
        let check_pos = |p: IVec3, checked: &mut [IVec3; 10], checked_len: &mut usize| -> bool {
            if checked[..*checked_len].contains(&p) || *checked_len >= checked.len() {
                return false;
            }
            checked[*checked_len] = p;
            *checked_len += 1;
            true
        };
        check_pos(pos, &mut checked, &mut checked_len);

        if check_pos(owner, &mut checked, &mut checked_len)
            && detail_y_in_range(owner.y)
            && let Some(v) = self.get_voxel_if_loaded(owner)
            && v.is_solid()
            && uses_centered_detail_bounds(v.shape())
        {
            push_boxes(owner, v, &mut merged);
        }
        for offset in [
            IVec3::X,
            IVec3::NEG_X,
            IVec3::Z,
            IVec3::NEG_Z,
            IVec3::new(1, 0, 1),
            IVec3::new(1, 0, -1),
            IVec3::new(-1, 0, 1),
            IVec3::new(-1, 0, -1),
        ] {
            let p = pos + offset;
            if !detail_y_in_range(p.y) || !check_pos(p, &mut checked, &mut checked_len) {
                continue;
            }
            if let Some(v) = self.get_voxel_if_loaded(p)
                && v.is_solid()
                && uses_centered_detail_bounds(v.shape())
            {
                push_boxes(p, v, &mut merged);
            }
        }

        if !merged.is_empty() {
            return Some(merged);
        }
        if has_solid_shape {
            // Defensive fallback: never make a solid shape passable due to clipping.
            let boxes = voxel.shape().collision_boxes(voxel.orientation());
            if !boxes.is_empty() {
                return Some(boxes);
            }
        }
        None
    }

    /// Write a complete voxel (material + shape) and refresh the padding copy in
    /// every adjacent chunk.
    ///
    /// Returns all chunk coordinates that need remeshing (up to 27: the owner
    /// and any of its 26 neighbours whose padding covers the edited block).
    pub fn set_voxel(&mut self, pos: IVec3, voxel: Voxel) -> Vec<IVec3> {
        let dirty = self.set_voxel_untracked(pos, voxel);
        if !dirty.is_empty() {
            self.detail_edits.insert(pos, voxel);
        }
        dirty
    }

    /// Write a voxel without recording it in the save edit log. Used when a
    /// save file is being restored.
    pub fn set_voxel_untracked(&mut self, pos: IVec3, voxel: Voxel) -> Vec<IVec3> {
        let mut dirty = Vec::new();
        if !detail_y_in_range(pos.y) {
            return dirty;
        }

        let center = detail_to_chunk(pos);
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let coord = center + IVec3::new(dx, dy, dz);
                    let Some(chunk) = self.chunks.get_mut(&coord) else {
                        continue;
                    };

                    let origin = IVec3::new(
                        coord.x * CHUNK_X as i32,
                        coord.y * CHUNK_Y as i32,
                        coord.z * CHUNK_Z as i32,
                    );
                    let local = pos - origin;
                    if local.x < -(PAD as i32)
                        || local.x > CHUNK_X as i32
                        || local.y < -(PAD as i32)
                        || local.y > CHUNK_Y as i32
                        || local.z < -(PAD as i32)
                        || local.z > CHUNK_Z as i32
                    {
                        continue;
                    }

                    chunk.data.set_padded(
                        (local.x + PAD as i32) as usize,
                        (local.y + PAD as i32) as usize,
                        (local.z + PAD as i32) as usize,
                        voxel,
                    );
                    dirty.push(coord);
                }
            }
        }

        dirty
    }

    /// Voxel DDA raycast (Amanatides & Woo). Returns the first solid block,
    /// testing each voxel's actual shape so a ray can pass above a thin panel
    /// and hit the block behind it.
    pub fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RayHit> {
        let dir = direction.normalize_or_zero();
        if dir == Vec3::ZERO {
            return None;
        }

        let origin_arr = origin.to_array();
        let dir_arr = dir.to_array();
        let mut cell = [
            origin_arr[0].floor() as i32,
            origin_arr[1].floor() as i32,
            origin_arr[2].floor() as i32,
        ];
        let mut step = [0i32; 3];
        let mut t_max = [f32::INFINITY; 3];
        let mut t_delta = [f32::INFINITY; 3];

        for axis in 0..3 {
            if dir_arr[axis] > 0.0 {
                step[axis] = 1;
                t_max[axis] = (cell[axis] as f32 + 1.0 - origin_arr[axis]) / dir_arr[axis];
                t_delta[axis] = 1.0 / dir_arr[axis];
            } else if dir_arr[axis] < 0.0 {
                step[axis] = -1;
                t_max[axis] = (origin_arr[axis] - cell[axis] as f32) / -dir_arr[axis];
                t_delta[axis] = -1.0 / dir_arr[axis];
            }
        }

        loop {
            // Stop before testing an out-of-bounds cell when the ray is moving
            // further out. Starting outside and moving back into the world is
            // still allowed; the first in-bounds cell is tested normally.
            if cell[1] < DETAIL_Y_MIN && step[1] <= 0 {
                break;
            }
            if cell[1] >= DETAIL_Y_MAX && step[1] >= 0 {
                break;
            }

            let block = IVec3::from_array(cell);
            // Centered shapes (trunk/stems/loose detail) are stored in one detail
            // cell but their geometry can extend into neighbouring detail cells.
            // Test the current cell, its 1m-voxel owner, and adjacent detail cells
            // with centered-detail shapes so rays don't pass through overlapping
            // geometry.
            let owner = world_to_detail_origin(detail_to_world(block));
            let mut candidates: [(IVec3, Option<Voxel>, f32); 10] = [(block, None, 0.0); 10];
            let mut candidate_count = 0usize;
            let mut seen = [IVec3::new(i32::MIN, i32::MIN, i32::MIN); 10];

            let push_candidate = |pos: IVec3,
                                  voxel: Voxel,
                                  lift: f32,
                                  candidates: &mut [(IVec3, Option<Voxel>, f32); 10],
                                  candidate_count: &mut usize,
                                  seen: &mut [IVec3; 10]| {
                if seen[..*candidate_count].contains(&pos) || *candidate_count >= candidates.len() {
                    return;
                }
                seen[*candidate_count] = pos;
                candidates[*candidate_count] = (pos, Some(voxel), lift);
                *candidate_count += 1;
            };

            if let Some(voxel) = self.get_voxel_if_loaded(block) {
                if voxel.is_solid() {
                    let lift = if voxel.shape().is_natural() {
                        self.natural_detail_offset(block)
                    } else {
                        0.0
                    };
                    push_candidate(
                        block,
                        voxel,
                        lift,
                        &mut candidates,
                        &mut candidate_count,
                        &mut seen,
                    );
                }
            } else {
                // Do not let interaction reach through unloaded terrain.
                break;
            }

            if let Some(voxel) = self.get_voxel_if_loaded(owner)
                && voxel.is_solid()
                && uses_centered_detail_bounds(voxel.shape())
            {
                push_candidate(
                    owner,
                    voxel,
                    self.natural_detail_offset(owner),
                    &mut candidates,
                    &mut candidate_count,
                    &mut seen,
                );
            }

            for offset in [
                IVec3::X,
                IVec3::NEG_X,
                IVec3::Z,
                IVec3::NEG_Z,
                IVec3::new(1, 0, 1),
                IVec3::new(1, 0, -1),
                IVec3::new(-1, 0, 1),
                IVec3::new(-1, 0, -1),
            ] {
                let pos = block + offset;
                if !detail_y_in_range(pos.y) {
                    continue;
                }
                let Some(voxel) = self.get_voxel_if_loaded(pos) else {
                    continue;
                };
                if voxel.is_solid() && uses_centered_detail_bounds(voxel.shape()) {
                    push_candidate(
                        pos,
                        voxel,
                        self.natural_detail_offset(pos),
                        &mut candidates,
                        &mut candidate_count,
                        &mut seen,
                    );
                }
            }

            let mut best: Option<(f32, IVec3, IVec3)> = None;
            for (cell, voxel, lift) in candidates.into_iter().take(candidate_count) {
                let Some(voxel) = voxel else {
                    continue;
                };
                if let Some(hit) = voxel_ray_hit(origin, dir, cell, voxel, lift)
                    && hit.distance <= max_distance
                    && best.is_none_or(|(distance, _, _)| hit.distance < distance)
                {
                    best = Some((hit.distance, cell, hit.normal));
                }
            }
            if let Some((distance, cell, normal)) = best {
                return Some(RayHit {
                    block: cell,
                    normal,
                    distance,
                });
            }

            let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
                0
            } else if t_max[1] < t_max[2] {
                1
            } else {
                2
            };

            let distance = t_max[axis];
            if !distance.is_finite() || distance > max_distance {
                break;
            }

            cell[axis] += step[axis];
            t_max[axis] += t_delta[axis];
        }

        None
    }
}

struct VoxelHit {
    distance: f32,
    normal: IVec3,
}

/// Intersect a ray with a voxel's oriented shape. Bounds use the mesher's
/// canonical cell space (0..1 on each axis, rotated about the cell centre).
/// `lift` raises the shape inside the cell, matching the mesher's offset for
/// natural detail resting on raised turf.
fn voxel_ray_hit(
    origin: Vec3,
    direction: Vec3,
    block: IVec3,
    voxel: Voxel,
    lift: f32,
) -> Option<VoxelHit> {
    let orientation = voxel.orientation();
    let centered = uses_centered_detail_bounds(voxel.shape());
    let rotation = if centered {
        Quat::IDENTITY
    } else {
        orientation.quat()
    };
    let inverse = rotation.inverse();
    let cell_center = block.as_vec3() + Vec3::splat(0.5);
    let local_origin = inverse * (origin - cell_center) + Vec3::splat(0.5) - Vec3::Y * lift;
    let local_direction = inverse * direction;

    let (distance, local_normal) = match voxel.shape() {
        Shape::Cube => ray_box(local_origin, local_direction, Vec3::ZERO, Vec3::ONE)?,
        Shape::HalfBlock => ray_box(
            local_origin,
            local_direction,
            Vec3::ZERO,
            Vec3::new(1.0, 0.5, 1.0),
        )?,
        Shape::Panel => ray_box(
            local_origin,
            local_direction,
            Vec3::ZERO,
            Vec3::new(1.0, PANEL_HEIGHT, 1.0),
        )?,
        Shape::Slope => ray_slope(local_origin, local_direction)?,
        Shape::Stairs => {
            let lower = ray_box(
                local_origin,
                local_direction,
                Vec3::ZERO,
                Vec3::new(1.0, 0.5, 1.0),
            );
            let upper = ray_box(
                local_origin,
                local_direction,
                Vec3::new(0.0, 0.5, 0.5),
                Vec3::ONE,
            );
            match (lower, upper) {
                (Some(lower), Some(upper)) => {
                    if lower.0 <= upper.0 {
                        lower
                    } else {
                        upper
                    }
                }
                (Some(hit), None) | (None, Some(hit)) => hit,
                (None, None) => return None,
            }
        }
        // Tree geometry and ground detail use the mesher's exact cell-local
        // boxes so raycasts, mining previews and highlights agree.
        Shape::Trunk | Shape::DebarkedTrunk => {
            let (min, max) = centered_detail_bounds(voxel.shape());
            ray_box(local_origin, local_direction, min, max)?
        }
        Shape::Stem | Shape::DebarkedStem => {
            let (min, max) = stem_bounds_for_shape(voxel.shape(), orientation);
            ray_box(local_origin, local_direction, min, max)?
        }
        Shape::Pebble => {
            let mut nearest: Option<(f32, Vec3)> = None;
            for (min, max) in crate::world::block::pebble_parts(orientation) {
                if let Some(hit) = ray_box(local_origin, local_direction, min, max)
                    && nearest.is_none_or(|current| hit.0 < current.0)
                {
                    nearest = Some(hit);
                }
            }
            nearest?
        }
        Shape::StoneDeposit => {
            let mut nearest: Option<(f32, Vec3)> = None;
            for (min, max) in crate::world::block::stone_deposit_parts(orientation) {
                if let Some(hit) = ray_box(local_origin, local_direction, min, max)
                    && nearest.is_none_or(|current| hit.0 < current.0)
                {
                    nearest = Some(hit);
                }
            }
            nearest?
        }
        Shape::Stick => {
            let mut nearest: Option<(f32, Vec3)> = None;
            for (min, max) in crate::world::block::stick_parts(orientation) {
                if let Some(hit) = ray_box(local_origin, local_direction, min, max)
                    && nearest.is_none_or(|current| hit.0 < current.0)
                {
                    nearest = Some(hit);
                }
            }
            nearest?
        }
        Shape::Foliage => ray_box(local_origin, local_direction, Vec3::ZERO, Vec3::ONE)?,
        Shape::SmallFoliage => ray_box(
            local_origin,
            local_direction,
            Vec3::new(0.1, 0.0, 0.1),
            Vec3::new(0.9, 0.75, 0.9),
        )?,
    };

    Some(VoxelHit {
        distance,
        normal: snap_normal(rotation * local_normal),
    })
}

fn ray_box(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<(f32, Vec3)> {
    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    let mut enter_normal = Vec3::ZERO;

    for axis in 0..3 {
        let offset = origin[axis];
        let denom = direction[axis];
        if denom.abs() < 1e-8 {
            if offset < min[axis] || offset > max[axis] {
                return None;
            }
            continue;
        }

        let inv = 1.0 / denom;
        let mut near_t = (min[axis] - offset) * inv;
        let mut far_t = (max[axis] - offset) * inv;
        let mut near_normal = -axis_vector(axis);
        let mut far_normal = axis_vector(axis);
        if near_t > far_t {
            std::mem::swap(&mut near_t, &mut far_t);
            std::mem::swap(&mut near_normal, &mut far_normal);
        }

        if near_t > t_enter {
            t_enter = near_t;
            enter_normal = near_normal;
        }
        t_exit = t_exit.min(far_t);
        if t_enter > t_exit + 1e-6 {
            return None;
        }
    }

    if t_exit < 0.0 {
        return None;
    }

    if t_enter < 0.0 {
        // The ray starts inside the box; use a stable normal for placement.
        enter_normal = -axis_vector(dominant_axis(direction));
    }

    Some((t_enter.max(0.0), enter_normal))
}

fn ray_slope(origin: Vec3, direction: Vec3) -> Option<(f32, Vec3)> {
    // Canonical wedge: low edge at -Z, high edge at +Z, so y <= z.
    let planes = [
        (Vec3::NEG_X, 0.0),
        (Vec3::X, 1.0),
        (Vec3::NEG_Y, 0.0),
        (Vec3::NEG_Z, 0.0),
        (Vec3::Z, 1.0),
        (Vec3::new(0.0, 1.0, -1.0), 0.0),
    ];

    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    let mut enter_normal = Vec3::ZERO;

    for (plane_normal, offset) in planes {
        let denom = plane_normal.dot(direction);
        let gap = offset - plane_normal.dot(origin);
        if denom.abs() < 1e-8 {
            if gap < 0.0 {
                return None;
            }
            continue;
        }

        let t = gap / denom;
        if denom > 0.0 {
            t_exit = t_exit.min(t);
        } else if t > t_enter {
            t_enter = t;
            enter_normal = plane_normal.normalize();
        }
        if t_enter > t_exit + 1e-6 {
            return None;
        }
    }

    if t_exit < 0.0 {
        return None;
    }

    if t_enter < 0.0 {
        enter_normal = -axis_vector(dominant_axis(direction));
    }

    Some((t_enter.max(0.0), enter_normal))
}

fn axis_vector(axis: usize) -> Vec3 {
    match axis {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

fn dominant_axis(direction: Vec3) -> usize {
    let abs = direction.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        0
    } else if abs.y >= abs.z {
        1
    } else {
        2
    }
}

fn snap_normal(normal: Vec3) -> IVec3 {
    let abs = normal.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        IVec3::new(normal.x.signum() as i32, 0, 0)
    } else if abs.y >= abs.z {
        IVec3::new(0, normal.y.signum() as i32, 0)
    } else {
        IVec3::new(0, 0, normal.z.signum() as i32)
    }
}

/// True when a chunk offset lies inside the stream volume.
fn in_stream_volume(delta: IVec3, volume: StreamVolume) -> bool {
    volume.contains(delta)
}

/// True when a chunk offset lies outside the margin-expanded eviction volume.
fn outside_stream_volume(delta: IVec3, volume: StreamVolume) -> bool {
    !volume.expanded(UNLOAD_MARGIN).contains(delta)
}

/// Build the cached bulk candidate list. Chunks are ranked by squared
/// distance from the player chunk center, so loading is predictable and
/// centered around player position rather than camera facing. The list is
/// independent of which chunks are currently loaded, so it can be cached until
/// the player chunk or view volume changes.
fn stream_candidates(center: IVec3, volume: StreamVolume) -> Vec<IVec3> {
    let capacity = volume.chunk_count();
    let mut candidates = Vec::with_capacity(capacity);

    for dy in volume.min_y()..=volume.max_y() {
        let Some(radius) = volume.radius_for_y(dy) else {
            continue;
        };
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let delta = IVec3::new(dx, dy, dz);
                if !in_stream_volume(delta, volume) {
                    continue;
                }
                let distance = dx * dx + dy * dy + dz * dz;
                candidates.push((center + delta, distance));
            }
        }
    }
    candidates.sort_unstable_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.0.x.cmp(&b.0.x))
            .then_with(|| a.0.y.cmp(&b.0.y))
            .then_with(|| a.0.z.cmp(&b.0.z))
    });
    candidates.into_iter().map(|(coord, _)| coord).collect()
}

/// Small, recomputed list of chunks that matter right now.
///
/// Tiers are immediate surroundings, predicted velocity path, and camera
/// direction. Within a tier, score is time-to-arrival, so a distant chunk is not
/// promoted merely because it lies along the movement direction.
fn ranked_urgent_candidates(
    position: Vec3,
    velocity: Vec3,
    camera_forward: Vec3,
    volume: StreamVolume,
) -> Vec<(IVec3, u8, f32)> {
    let center = position_to_chunk(position);
    let chunk_size = Vec3::new(CHUNK_X as f32, CHUNK_Y as f32, CHUNK_Z as f32);
    let position_chunks = position / chunk_size;
    let velocity_chunks = velocity / chunk_size;
    let speed = velocity_chunks.length().max(0.5);
    let mut seen = HashSet::new();
    let mut ranked: Vec<(IVec3, u8, f32)> = Vec::new();

    let mut add = |coord: IVec3, tier: u8, ranked: &mut Vec<(IVec3, u8, f32)>| {
        if !seen.insert(coord) || !volume.contains(coord - center) {
            return;
        }
        let delta = Vec3::new(
            coord.x as f32 + 0.5 - position_chunks.x,
            coord.y as f32 + 0.5 - position_chunks.y,
            coord.z as f32 + 0.5 - position_chunks.z,
        );
        let eta = delta.length() / speed;
        ranked.push((coord, tier, eta));
    };

    let mut add_neighborhood = |chunk: IVec3, tier: u8, ranked: &mut Vec<(IVec3, u8, f32)>| {
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    add(chunk + IVec3::new(dx, dy, dz), tier, ranked);
                }
            }
        }
    };

    // Tier 0: the chunk under the player and its immediate neighbours.
    add_neighborhood(center, 0, &mut ranked);

    // Tier 1: a short path along the current velocity. Faster movement reaches
    // more chunks within the same prediction window.
    for sample in 1..=PREDICT_SAMPLES {
        let t = PREDICT_SECONDS * sample as f32 / PREDICT_SAMPLES as f32;
        let predicted = position_to_chunk(position + velocity * t);
        add_neighborhood(predicted, 1, &mut ranked);
    }

    // Tier 2: a short look-ahead along the camera direction.
    let forward = camera_forward.normalize_or_zero();
    if forward.length_squared() > 1e-6 {
        for distance in 1..=CAMERA_SAMPLE_CHUNKS {
            let sample = position + forward * (distance as f32 * chunk_size.x);
            add_neighborhood(position_to_chunk(sample), 2, &mut ranked);
        }
    }

    ranked.sort_unstable_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.2.total_cmp(&b.2))
            .then_with(|| a.0.x.cmp(&b.0.x))
            .then_with(|| a.0.y.cmp(&b.0.y))
            .then_with(|| a.0.z.cmp(&b.0.z))
    });
    ranked
}

fn urgent_candidates(
    position: Vec3,
    velocity: Vec3,
    camera_forward: Vec3,
    volume: StreamVolume,
) -> Vec<IVec3> {
    ranked_urgent_candidates(position, velocity, camera_forward, volume)
        .into_iter()
        .map(|(coord, _, _)| coord)
        .collect()
}

fn chunk_priority(coord: IVec3, position: Vec3) -> f32 {
    let chunk_size = Vec3::new(CHUNK_X as f32, CHUNK_Y as f32, CHUNK_Z as f32);
    let delta = Vec3::new(coord.x as f32, coord.y as f32, coord.z as f32) - position / chunk_size;
    delta.length_squared()
}

/// Polling order is explicit: urgent tasks first, then normal tasks nearest
/// first. `HashMap` iteration order is deliberately not relied upon.
fn pending_poll_order<T>(pending: &HashMap<IVec3, PendingChunk<T>>) -> Vec<IVec3> {
    let mut order: Vec<(Urgency, f32, IVec3)> = pending
        .iter()
        .map(|(coord, entry)| (entry.urgency, entry.distance, *coord))
        .collect();
    order.sort_unstable_by(|a, b| {
        let urgent = (a.0 == Urgency::Urgent)
            .cmp(&(b.0 == Urgency::Urgent))
            .reverse();
        urgent
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.x.cmp(&b.2.x))
            .then_with(|| a.2.y.cmp(&b.2.y))
            .then_with(|| a.2.z.cmp(&b.2.z))
    });
    order.into_iter().map(|(_, _, coord)| coord).collect()
}

/// Preempt the furthest normal tasks until at least `reserve` urgent slots are
/// free. Preempted coordinates are suppressed briefly so they are not restarted
/// immediately, and removal from `pending` guarantees their results are dropped.
fn reserve_urgent_capacity<T>(
    pending: &mut HashMap<IVec3, PendingChunk<T>>,
    suppressed: &mut HashMap<IVec3, u64>,
    frame: u64,
    urgent_count: usize,
) -> Vec<IVec3> {
    let target_normals = MAX_PENDING.saturating_sub(urgent_count.min(URGENT_RESERVED));
    let mut preempted = Vec::new();
    loop {
        let normal_count = pending
            .values()
            .filter(|entry| entry.urgency == Urgency::Normal)
            .count();
        if normal_count <= target_normals {
            break;
        }
        let farthest = pending
            .iter()
            .filter(|(_, entry)| entry.urgency == Urgency::Normal)
            .max_by(|a, b| {
                a.1.distance
                    .total_cmp(&b.1.distance)
                    .then_with(|| (a.0.x, a.0.y, a.0.z).cmp(&(b.0.x, b.0.y, b.0.z)))
            })
            .map(|(coord, _)| *coord);
        let Some(coord) = farthest else {
            break;
        };
        pending.remove(&coord);
        suppressed.insert(coord, frame + URGENT_SUPPRESS_FRAMES);
        preempted.push(coord);
    }
    preempted
}

fn is_suppressed(manager: &ChunkManager, coord: IVec3) -> bool {
    manager
        .suppressed
        .get(&coord)
        .is_some_and(|&expiry| expiry > manager.frame)
}

/// Queue one generation task with its scheduling class.
fn schedule_chunk(
    manager: &mut ChunkManager,
    generator: &Arc<WorldGen>,
    coord: IVec3,
    urgency: Urgency,
    position: Vec3,
) -> bool {
    if manager.chunks.contains_key(&coord)
        || manager.pending.contains_key(&coord)
        || is_suppressed(manager, coord)
        || manager.pending.len() >= MAX_PENDING
    {
        return false;
    }
    let generator = generator.clone();
    let task =
        AsyncComputeTaskPool::get().spawn(async move { generate_and_mesh(&generator, coord) });
    manager.pending.insert(
        coord,
        PendingChunk {
            task,
            urgency,
            distance: chunk_priority(coord, position),
        },
    );
    true
}

/// The downward tail is only useful below ground, during a fall, or while the
/// camera points down. The look-down thresholds form a small hysteresis band so
/// aiming near the horizon does not toggle the stream volume every frame.
fn should_enable_deep_tail(
    position: Vec3,
    generator: &WorldGen,
    velocity: Vec3,
    look_down: f32,
    active: bool,
) -> bool {
    let world = detail_to_world(position.floor().as_ivec3());
    let surface_detail_y = generator.height_at(world.x, world.z) * DETAILS_PER_WORLD_AXIS;
    let underground = position.y < (surface_detail_y - DETAILS_PER_WORLD_AXIS) as f32;
    let falling = velocity.y <= DEEP_TAIL_FALL_SPEED;
    let threshold = if active {
        DEEP_TAIL_LOOK_DOWN_EXIT
    } else {
        DEEP_TAIL_LOOK_DOWN_ENTER
    };
    underground || falling || look_down < threshold
}

fn generate_and_mesh(generator: &WorldGen, coord: IVec3) -> GeneratedChunk {
    let data = generator.generate_chunk(coord);
    let ChunkMeshData {
        opaque,
        foliage,
        water,
    } = mesh_chunk(&data);
    let triangles = opaque.triangle_count();
    let foliage_triangles = foliage.triangle_count();
    let water_triangles = water.triangle_count();

    GeneratedChunk {
        coord,
        data,
        opaque: opaque.into_mesh(),
        foliage: foliage.into_foliage_mesh(),
        water: water.into_mesh(),
        triangles,
        foliage_triangles,
        water_triangles,
    }
}

#[allow(clippy::too_many_arguments)]
fn setup_world(
    mut commands: Commands,
    generator: Res<WorldGenResource>,
    spawn: Res<SpawnPoint>,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut foliage_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
    mut initial_load: ResMut<InitialWorldLoad>,
) {
    // One material for all opaque chunk meshes. Vertex colours carry the block
    // palette, face shading and ambient occlusion.
    manager.opaque_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.93,
        metallic: 0.0,
        reflectance: 0.0,
        cull_mode: Some(Face::Back),
        ..default()
    });

    // Foliage uses the same opaque look but sways in the wind via a custom
    // vertex shader. The extension piggybacks on the standard PBR pipeline.
    manager.foliage_material = Some(foliage_materials.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.93,
            metallic: 0.0,
            reflectance: 0.0,
            cull_mode: Some(Face::Back),
            ..default()
        },
        extension: FoliageWindMaterial::default(),
    }));

    manager.water_material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.72),
        perceptual_roughness: 0.12,
        metallic: 0.0,
        reflectance: 0.45,
        cull_mode: None,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });

    let center = position_to_chunk(spawn.0);
    manager.last_center = Some(center);
    *initial_load = InitialWorldLoad::new(center);

    // Synchronously preload only the 3x3x3 safe patch so the player never
    // spawns in void. The remaining required 8x3x8 area loads asynchronously via
    // normal chunk streaming behind the loading screen.
    preload_safe_patch(
        &mut commands,
        &generator.0,
        &mut manager,
        &mut meshes,
        &mut materials,
        &mut foliage_materials,
        center,
        &mut initial_load,
    );
}

/// Load the destination's local chunk patch and apply a queued teleport before
/// the player controller runs. The 3x3x3 patch matches startup so the player
/// never collides against unloaded chunks, which are treated as solid.
#[allow(clippy::too_many_arguments)]
fn apply_pending_teleport(
    mut commands: Commands,
    generator: Res<WorldGenResource>,
    mut pending: ResMut<PendingTeleport>,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut foliage_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
    mut player: Query<(&mut Transform, &mut Player), With<Player>>,
) {
    let Some(destination) = pending.0.take() else {
        return;
    };

    let center = position_to_chunk(destination);
    manager.last_center = Some(center);
    manager.stream_cache = None;

    preload_teleport_chunks(
        &mut commands,
        &generator.0,
        &mut manager,
        &mut meshes,
        &mut materials,
        &mut foliage_materials,
        center,
    );

    if let Ok((mut transform, mut player)) = player.single_mut() {
        transform.translation = destination;
        player.reset_motion();
    }
}

#[allow(clippy::too_many_arguments)]
fn preload_safe_patch(
    commands: &mut Commands,
    generator: &WorldGen,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    foliage_materials: &mut Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>,
    center: IVec3,
    initial_load: &mut InitialWorldLoad,
) {
    for dy in -1..=1 {
        for dz in -1..=1 {
            for dx in -1..=1 {
                let coord = center + IVec3::new(dx, dy, dz);
                if manager.chunks.contains_key(&coord) {
                    initial_load.mark_loaded(coord);
                    continue;
                }
                manager.pending.remove(&coord);
                let generated = generate_and_mesh(generator, coord);
                spawn_chunk(
                    commands,
                    manager,
                    meshes,
                    materials,
                    foliage_materials,
                    generated,
                );
                initial_load.mark_loaded(coord);
            }
        }
    }
}

/// Teleport preload: keep a small safe patch synchronous, then spend a short
/// bounded budget on closest-first nearby chunks. This avoids a full loading
/// screen while preserving collision safety.
fn preload_teleport_chunks(
    commands: &mut Commands,
    generator: &WorldGen,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    foliage_materials: &mut Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>,
    center: IVec3,
) {
    let started = std::time::Instant::now();
    for dy in -1..=1 {
        for dz in -1..=1 {
            for dx in -1..=1 {
                let coord = center + IVec3::new(dx, dy, dz);
                if manager.chunks.contains_key(&coord) {
                    continue;
                }
                manager.pending.remove(&coord);
                let generated = generate_and_mesh(generator, coord);
                spawn_chunk(
                    commands,
                    manager,
                    meshes,
                    materials,
                    foliage_materials,
                    generated,
                );
            }
        }
    }
    let radius = TELEPORT_PRELOAD_RADIUS;
    let mut candidates: Vec<(IVec3, i32)> = Vec::new();
    for dy in -1..=1 {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let delta = IVec3::new(dx, dy, dz);
                if delta.x.abs() <= 1 && delta.y.abs() <= 1 && delta.z.abs() <= 1 {
                    continue;
                }
                let dist = dx * dx + dy * dy + dz * dz;
                candidates.push((center + delta, dist));
            }
        }
    }
    candidates.sort_unstable_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.0.x.cmp(&b.0.x))
            .then_with(|| a.0.y.cmp(&b.0.y))
            .then_with(|| a.0.z.cmp(&b.0.z))
    });
    for (coord, _) in candidates {
        if started.elapsed().as_millis() >= TELEPORT_PRELOAD_BUDGET_MS {
            break;
        }
        if manager.chunks.contains_key(&coord) || manager.pending.contains_key(&coord) {
            continue;
        }
        manager.pending.remove(&coord);
        let generated = generate_and_mesh(generator, coord);
        spawn_chunk(
            commands,
            manager,
            meshes,
            materials,
            foliage_materials,
            generated,
        );
    }
}

/// Accept a finished task only if it is still the current pending entry for
/// that coordinate. Preempted tasks have already been removed, so their late
/// results cannot spawn a duplicate chunk.
fn accept_generated_chunk<T>(pending: &mut HashMap<IVec3, PendingChunk<T>>, coord: IVec3) -> bool {
    pending.remove(&coord).is_some()
}

fn poll_chunk_tasks(
    mut commands: Commands,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut foliage_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
    mut timings: ResMut<WorldTimings>,
    mut initial_load: ResMut<InitialWorldLoad>,
) {
    let started = std::time::Instant::now();
    let mut ready = Vec::new();
    let mut budget = STREAM_BUDGET;

    for coord in pending_poll_order(&manager.pending) {
        if budget == 0 {
            break;
        }
        let generated = {
            let Some(entry) = manager.pending.get_mut(&coord) else {
                continue;
            };
            // `poll_once` is a future in current futures-lite; block_on resolves
            // it immediately because it always returns `Ready` after one poll.
            block_on(poll_once(&mut entry.task))
        };
        if let Some(generated) = generated
            && accept_generated_chunk(&mut manager.pending, coord)
        {
            ready.push(generated);
            budget -= 1;
        }
    }

    for generated in ready {
        let coord = generated.coord;
        spawn_chunk(
            &mut commands,
            &mut manager,
            &mut meshes,
            &mut materials,
            &mut foliage_materials,
            generated,
        );
        initial_load.mark_loaded(coord);
    }
    record_timing(&mut timings.poll_ms, started.elapsed());
}

#[allow(clippy::too_many_arguments)]
fn stream_chunks(
    mut commands: Commands,
    mut manager: ResMut<ChunkManager>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut foliage_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
    generator: Res<WorldGenResource>,
    camera: Single<&GlobalTransform, With<crate::player::PlayerCamera>>,
    player: Single<(&Transform, &Player), With<Player>>,
    mut timings: ResMut<WorldTimings>,
    initial_load: Res<InitialWorldLoad>,
) {
    let started = std::time::Instant::now();
    let manager = &mut *manager;
    manager.frame = manager.frame.wrapping_add(1);
    let frame = manager.frame;
    manager.suppressed.retain(|_, expiry| *expiry > frame);

    let view_forward = camera.rotation() * Vec3::NEG_Z;
    let view_forward = if view_forward.length_squared() > 1e-6 {
        view_forward.normalize()
    } else {
        Vec3::NEG_Z
    };

    // Use the authoritative player translation, not the camera's
    // `GlobalTransform`: transform propagation runs after Update, so after a
    // teleport the camera still reports the old position for one frame. That
    // made streaming unload the destination patch we had just generated.
    let (player_transform, player_state) = *player;
    let position = player_transform.translation;
    let center = position_to_chunk(position);
    let radius = manager.horizontal_view_distance();
    let vertical_radius = manager.vertical_view_distance();
    manager.deep_tail_active = should_enable_deep_tail(
        position,
        &generator.0,
        player_state.velocity,
        view_forward.y,
        manager.deep_tail_active,
    );
    let volume = StreamVolume::new(radius, vertical_radius, manager.deep_tail_active);

    let stale: Vec<IVec3> = manager
        .chunks
        .keys()
        .copied()
        .filter(|coord| outside_stream_volume(*coord - center, volume))
        .collect();
    for coord in stale {
        if let Some(chunk) = manager.chunks.remove(&coord) {
            commands.entity(chunk.entity).despawn();
            if let Some(h) = manager.opaque_fade_materials.remove(&coord) {
                materials.remove(&h);
            }
            if let Some(h) = manager.water_fade_materials.remove(&coord) {
                materials.remove(&h);
            }
            if let Some(h) = manager.foliage_fade_materials.remove(&coord) {
                foliage_materials.remove(&h);
            }
        }
    }

    let stale_pending: Vec<IVec3> = manager
        .pending
        .keys()
        .copied()
        .filter(|coord| outside_stream_volume(*coord - center, volume))
        .collect();
    for coord in stale_pending {
        manager.pending.remove(&coord);
    }

    let key = StreamCacheKey {
        center,
        horizontal: radius,
        vertical: vertical_radius,
        deep_tail: manager.deep_tail_active,
    };
    let cache_is_stale = manager
        .stream_cache
        .as_ref()
        .is_none_or(|cache| cache.key != key);
    if cache_is_stale {
        manager.stream_cache = Some(StreamCache {
            key,
            candidates: stream_candidates(center, volume),
            cursor: 0,
        });
    }
    manager.last_center = Some(center);

    // The urgent list is small and deliberately rebuilt from live motion. The
    // large distance-ranked list stays cached; only its cursor advances.

    // During initial startup load, prioritize the required 8x3x8 area before
    // normal gameplay-oriented urgent candidates.
    if !initial_load.ready {
        let mut required: Vec<IVec3> = initial_load.required.iter().copied().collect();
        required.sort_unstable_by_key(|c| {
            let d = *c - center;
            (d.x * d.x + d.y * d.y + d.z * d.z, c.x, c.y, c.z)
        });
        for coord in required {
            if manager.pending.len() >= MAX_PENDING {
                break;
            }
            let _ = schedule_chunk(manager, &generator.0, coord, Urgency::Urgent, position);
        }
    }

    let pending_urgent = manager
        .pending
        .values()
        .filter(|entry| entry.urgency == Urgency::Urgent)
        .count();
    let urgent_room = URGENT_RESERVED.saturating_sub(pending_urgent);
    let mut urgent_to_spawn = Vec::new();
    for coord in urgent_candidates(position, player_state.velocity, view_forward, volume) {
        if urgent_to_spawn.len() >= urgent_room {
            break;
        }
        if manager.chunks.contains_key(&coord)
            || manager.pending.contains_key(&coord)
            || is_suppressed(manager, coord)
        {
            continue;
        }
        urgent_to_spawn.push(coord);
    }
    reserve_urgent_capacity(
        &mut manager.pending,
        &mut manager.suppressed,
        frame,
        pending_urgent + urgent_to_spawn.len(),
    );

    let mut spawned = 0;
    for coord in urgent_to_spawn {
        if schedule_chunk(manager, &generator.0, coord, Urgency::Urgent, position) {
            spawned += 1;
        }
    }

    // Walk the cached closest-first list instead of rebuilding and sorting the
    // whole stream volume every frame. Already loaded or queued entries are
    // skipped permanently until the cache is invalidated by movement or a
    // view-volume change.
    let mut cache = manager
        .stream_cache
        .take()
        .expect("stream cache is initialized above");
    while spawned < STREAM_BUDGET && cache.cursor < cache.candidates.len() {
        let coord = cache.candidates[cache.cursor];
        cache.cursor += 1;
        if schedule_chunk(manager, &generator.0, coord, Urgency::Normal, position) {
            spawned += 1;
        }
    }
    manager.stream_cache = Some(cache);
    record_timing(&mut timings.stream_ms, started.elapsed());
}

fn animate_chunk_fade_in(
    time: Res<Time>,
    mut commands: Commands,
    mut manager: ResMut<ChunkManager>,
    mut fades: Query<(Entity, &mut ChunkFadeIn), With<ChunkEntity>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut foliage_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>>,
) {
    let dt = time.delta_secs();
    let mut finished = Vec::new();
    for (entity, mut fade) in &mut fades {
        fade.remaining = (fade.remaining - dt).max(0.0);
        let t = 1.0 - (fade.remaining / fade.total.max(0.001)).clamp(0.0, 1.0);
        let alpha = t.clamp(0.0, 1.0);

        let Some(coord) = manager
            .chunks
            .iter()
            .find(|(_, c)| c.entity == entity)
            .map(|(coord, _)| *coord)
        else {
            if fade.remaining <= 0.0 {
                finished.push((entity, None));
            }
            continue;
        };

        if let Some(handle) = manager.opaque_fade_materials.get(&coord)
            && let Some(mut mat) = materials.get_mut(handle)
        {
            let c = mat.base_color.to_srgba();
            mat.base_color = Color::srgba(c.red, c.green, c.blue, alpha);
            mat.alpha_mode = if alpha < 0.999 {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            };
        }
        if let Some(handle) = manager.water_fade_materials.get(&coord)
            && let Some(mut mat) = materials.get_mut(handle)
        {
            let c = mat.base_color.to_srgba();
            mat.base_color = Color::srgba(c.red, c.green, c.blue, alpha * 0.72);
            mat.alpha_mode = AlphaMode::Blend;
        }
        if let Some(handle) = manager.foliage_fade_materials.get(&coord)
            && let Some(mut mat) = foliage_materials.get_mut(handle)
        {
            let c = mat.base.base_color.to_srgba();
            mat.base.base_color = Color::srgba(c.red, c.green, c.blue, alpha);
            mat.base.alpha_mode = if alpha < 0.999 {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            };
        }

        if fade.remaining <= 0.0 {
            finished.push((entity, Some(coord)));
        }
    }

    for (entity, coord) in finished {
        commands.entity(entity).remove::<ChunkFadeIn>();
        if let Some(coord) = coord {
            // Restore shared materials so we don't keep per-chunk materials around.
            if let Some(chunk) = manager.chunks.get(&coord) {
                commands
                    .entity(chunk.entity)
                    .insert(MeshMaterial3d(manager.opaque_material.clone()));
                if let Some(foliage) = chunk.foliage_entity
                    && let Some(mat) = manager.foliage_material.clone()
                {
                    commands.entity(foliage).insert(MeshMaterial3d(mat));
                }
                if let Some(water) = chunk.water_entity {
                    commands
                        .entity(water)
                        .insert(MeshMaterial3d(manager.water_material.clone()));
                }
            }
            if let Some(h) = manager.opaque_fade_materials.remove(&coord) {
                materials.remove(&h);
            }
            if let Some(h) = manager.water_fade_materials.remove(&coord) {
                materials.remove(&h);
            }
            if let Some(h) = manager.foliage_fade_materials.remove(&coord) {
                foliage_materials.remove(&h);
            }
        }
    }
}

fn distance_culling(
    camera: Single<&GlobalTransform, With<crate::player::PlayerCamera>>,
    manager: Res<ChunkManager>,
    mut chunks: Query<(&Transform, &mut Visibility), With<ChunkEntity>>,
    mut timings: ResMut<WorldTimings>,
) {
    let started = std::time::Instant::now();
    let camera = camera.translation();
    let cull_distance = manager.cull_distance();
    let cull_sq = cull_distance * cull_distance;
    let chunk_extent = Vec3::new(CHUNK_X as f32, CHUNK_Y as f32, CHUNK_Z as f32);

    for (transform, mut visibility) in &mut chunks {
        // Measure to the nearest point of the chunk, not its center, so the
        // fog can be fully opaque exactly when the chunk is culled.
        let min = transform.translation;
        let nearest = camera.clamp(min, min + chunk_extent);
        let desired = if nearest.distance_squared(camera) <= cull_sq {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };

        if *visibility != desired {
            *visibility = desired;
        }
    }
    record_timing(&mut timings.cull_ms, started.elapsed());
}

/// Re-apply edits recorded during this session to a freshly generated chunk.
///
/// Chunks unload and regenerate as the player travels. Without this, a mined
/// world voxel or a harvested turf/snow flag would reappear when its chunk
/// streams back in. Returns `true` when the chunk data changed and needs remeshing.
fn apply_recorded_edits(manager: &ChunkManager, coord: IVec3, data: &mut ChunkData) -> bool {
    let mut changed = false;

    // An edit is applied to this chunk when it lands in its interior or in the
    // one-cell padding shell. The full IVec3 origin is used, so edits in other
    // Y layers are correctly ignored. Padding is refreshed as well as the
    // interior so regenerated chunks stay seamless with their neighbours.
    let world_origin = chunk_world_origin(coord);
    for (pos, block) in &manager.world_edits {
        if !world_y_in_range(pos.y) {
            continue;
        }
        let local = *pos - world_origin;
        if local.x < -(WORLD_PAD as i32)
            || local.x > WORLD_CHUNK_X as i32
            || local.y < -(WORLD_PAD as i32)
            || local.y > WORLD_CHUNK_Y as i32
            || local.z < -(WORLD_PAD as i32)
            || local.z > WORLD_CHUNK_Z as i32
        {
            continue;
        }
        data.set_world_padded(
            (local.x + WORLD_PAD as i32) as usize,
            (local.y + WORLD_PAD as i32) as usize,
            (local.z + WORLD_PAD as i32) as usize,
            *block,
        );
        changed = true;
    }

    for (layer, edits) in [
        (SurfaceLayer::Turf, &manager.turf_edits),
        (SurfaceLayer::Snow, &manager.snow_edits),
    ] {
        for (pos, present) in edits {
            if !world_y_in_range(pos.y) {
                continue;
            }
            let local = *pos - world_origin;
            if local.x < -(WORLD_PAD as i32)
                || local.x > WORLD_CHUNK_X as i32
                || local.y < -(WORLD_PAD as i32)
                || local.y > WORLD_CHUNK_Y as i32
                || local.z < -(WORLD_PAD as i32)
                || local.z > WORLD_CHUNK_Z as i32
            {
                continue;
            }
            data.set_world_surface_padded(
                layer,
                (local.x + WORLD_PAD as i32) as usize,
                (local.y + WORLD_PAD as i32) as usize,
                (local.z + WORLD_PAD as i32) as usize,
                *present,
            );
            changed = true;
        }
    }

    let detail_origin = IVec3::new(
        coord.x * CHUNK_X as i32,
        coord.y * CHUNK_Y as i32,
        coord.z * CHUNK_Z as i32,
    );
    for (pos, voxel) in &manager.detail_edits {
        if !detail_y_in_range(pos.y) {
            continue;
        }
        let local = *pos - detail_origin;
        if local.x < -(PAD as i32)
            || local.x > CHUNK_X as i32
            || local.y < -(PAD as i32)
            || local.y > CHUNK_Y as i32
            || local.z < -(PAD as i32)
            || local.z > CHUNK_Z as i32
        {
            continue;
        }
        data.set_padded(
            (local.x + PAD as i32) as usize,
            (local.y + PAD as i32) as usize,
            (local.z + PAD as i32) as usize,
            *voxel,
        );
        changed = true;
    }

    changed
}

fn spawn_chunk(
    commands: &mut Commands,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    foliage_materials: &mut Assets<ExtendedMaterial<StandardMaterial, FoliageWindMaterial>>,
    mut generated: GeneratedChunk,
) {
    // Re-apply this session's edits before the chunk is meshed, so streaming
    // never brings back a mined voxel or harvested turf. Generated meshes are
    // rebuilt too: patching only `data` would leave a stale mesh on screen.
    if apply_recorded_edits(manager, generated.coord, &mut generated.data) {
        let ChunkMeshData {
            opaque,
            foliage,
            water,
        } = mesh_chunk(&generated.data);
        generated.triangles = opaque.triangle_count();
        generated.foliage_triangles = foliage.triangle_count();
        generated.water_triangles = water.triangle_count();
        generated.opaque = opaque.into_mesh();
        generated.foliage = foliage.into_foliage_mesh();
        generated.water = water.into_mesh();
    }

    let coord = generated.coord;
    let entity = commands
        .spawn((
            ChunkEntity,
            Name::new(format!("Chunk ({}, {}, {})", coord.x, coord.y, coord.z)),
            Transform::from_translation(chunk_origin(coord)),
            Visibility::default(),
            ChunkFadeIn {
                remaining: CHUNK_FADE_IN_SECS,
                total: CHUNK_FADE_IN_SECS,
            },
        ))
        .id();

    // Per-chunk fade materials for true alpha fade-in.
    let opaque_fade = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
        perceptual_roughness: 0.93,
        metallic: 0.0,
        reflectance: 0.0,
        cull_mode: Some(Face::Back),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    let water_fade = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
        perceptual_roughness: 0.12,
        metallic: 0.0,
        reflectance: 0.45,
        cull_mode: None,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    let foliage_fade = manager.foliage_material.as_ref().map(|_| {
        foliage_materials.add(ExtendedMaterial {
            base: StandardMaterial {
                base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
                perceptual_roughness: 0.93,
                metallic: 0.0,
                reflectance: 0.0,
                cull_mode: Some(Face::Back),
                alpha_mode: AlphaMode::Blend,
                ..default()
            },
            extension: FoliageWindMaterial::default(),
        })
    });
    manager
        .opaque_fade_materials
        .insert(coord, opaque_fade.clone());
    manager
        .water_fade_materials
        .insert(coord, water_fade.clone());
    if let Some(f) = foliage_fade.clone() {
        manager.foliage_fade_materials.insert(coord, f);
    }

    if let Some(mesh) = generated.opaque {
        let mesh_handle = meshes.add(mesh);
        commands
            .entity(entity)
            .insert((Mesh3d(mesh_handle), MeshMaterial3d(opaque_fade.clone())));
    }

    let mut foliage_entity = None;
    if let Some(mesh) = generated.foliage {
        let child = if let Some(material) = foliage_fade.clone() {
            commands
                .spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material),
                    NotShadowCaster,
                    Transform::default(),
                    Visibility::default(),
                    ChildOf(entity),
                ))
                .id()
        } else {
            commands
                .spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(opaque_fade.clone()),
                    NotShadowCaster,
                    Transform::default(),
                    Visibility::default(),
                    ChildOf(entity),
                ))
                .id()
        };
        foliage_entity = Some(child);
    }

    let mut water_entity = None;
    if let Some(mesh) = generated.water {
        let child = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(water_fade.clone()),
                NotShadowCaster,
                Transform::default(),
                Visibility::default(),
                ChildOf(entity),
            ))
            .id();
        water_entity = Some(child);
    }

    manager.chunks.insert(
        coord,
        Chunk {
            data: generated.data,
            entity,
            foliage_entity,
            water_entity,
            triangles: generated.triangles,
            foliage_triangles: generated.foliage_triangles,
            water_triangles: generated.water_triangles,
            fade_remaining: CHUNK_FADE_IN_SECS,
        },
    );
}

/// Remesh one already-loaded chunk. Called after block edits.
///
/// Editing is a synchronous, immediate operation: it affects at most 27
/// chunks and keeps block placement responsive. Streaming itself remains async.
pub fn remesh_chunk(
    commands: &mut Commands,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    coord: IVec3,
) {
    // Mesh directly from the stored chunk instead of cloning the whole 34^3/18^3
    // padded grid for every edit. The immutable borrow ends once the owned mesh
    // buffers are built, before the chunk metadata is updated below.
    let (
        entity,
        foliage_entity,
        water_entity,
        opaque,
        foliage,
        water,
        triangles,
        foliage_triangles,
        water_triangles,
    ) = {
        let Some(chunk) = manager.chunks.get(&coord) else {
            return;
        };
        let mesh_data = mesh_chunk(&chunk.data);
        let triangles = mesh_data.opaque.triangle_count();
        let foliage_triangles = mesh_data.foliage.triangle_count();
        let water_triangles = mesh_data.water.triangle_count();
        (
            chunk.entity,
            chunk.foliage_entity,
            chunk.water_entity,
            mesh_data.opaque.into_mesh(),
            mesh_data.foliage.into_foliage_mesh(),
            mesh_data.water.into_mesh(),
            triangles,
            foliage_triangles,
            water_triangles,
        )
    };

    if let Some(mesh) = opaque {
        let mesh_handle = meshes.add(mesh);
        commands.entity(entity).insert((
            Mesh3d(mesh_handle),
            MeshMaterial3d(manager.opaque_material.clone()),
        ));
    } else {
        commands.entity(entity).remove::<Mesh3d>();
    }

    let mut new_foliage_entity = foliage_entity;
    match (foliage, foliage_entity) {
        (Some(mesh), Some(child)) => {
            commands.entity(child).insert(Mesh3d(meshes.add(mesh)));
        }
        (Some(mesh), None) => {
            let child = if let Some(material) = manager.foliage_material.clone() {
                commands
                    .spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(material),
                        NotShadowCaster,
                        Transform::default(),
                        Visibility::default(),
                        ChildOf(entity),
                    ))
                    .id()
            } else {
                commands
                    .spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(manager.opaque_material.clone()),
                        NotShadowCaster,
                        Transform::default(),
                        Visibility::default(),
                        ChildOf(entity),
                    ))
                    .id()
            };
            new_foliage_entity = Some(child);
        }
        (None, Some(child)) => {
            commands.entity(child).despawn();
            new_foliage_entity = None;
        }
        (None, None) => {}
    }

    let mut new_water_entity = water_entity;
    match (water, water_entity) {
        (Some(mesh), Some(child)) => {
            let mesh_handle = meshes.add(mesh);
            commands.entity(child).insert((
                Mesh3d(mesh_handle),
                MeshMaterial3d(manager.water_material.clone()),
            ));
        }
        (Some(mesh), None) => {
            let child = commands
                .spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(manager.water_material.clone()),
                    NotShadowCaster,
                    Transform::default(),
                    Visibility::default(),
                    ChildOf(entity),
                ))
                .id();
            new_water_entity = Some(child);
        }
        (None, Some(child)) => {
            commands.entity(child).despawn();
            new_water_entity = None;
        }
        (None, None) => {}
    }

    if let Some(chunk) = manager.chunks.get_mut(&coord) {
        chunk.triangles = triangles;
        chunk.foliage_triangles = foliage_triangles;
        chunk.water_triangles = water_triangles;
        chunk.foliage_entity = new_foliage_entity;
        chunk.water_entity = new_water_entity;
    }
}

/// Apply several voxel edits and remesh every affected chunk exactly once.
///
/// Mining a 1 m world voxel clears eight detail cells, which can touch several
/// chunks. Batching keeps that operation to a single remesh per chunk instead of
/// rebuilding the same mesh eight times.
pub fn apply_voxel_edits(
    commands: &mut Commands,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    world_edits: &[(IVec3, Block)],
    edits: &[(IVec3, Voxel)],
    surface_edits: &[(SurfaceLayer, IVec3, bool)],
) {
    let mut dirty = Vec::new();
    for (pos, block) in world_edits {
        dirty.extend(manager.set_world_block(*pos, *block));
    }
    for (pos, voxel) in edits {
        dirty.extend(manager.set_voxel(*pos, *voxel));
    }
    for (layer, pos, present) in surface_edits {
        dirty.extend(manager.set_world_surface(*layer, *pos, *present));
    }
    dirty.sort_unstable_by_key(|coord| (coord.x, coord.y, coord.z));
    dirty.dedup();
    for coord in dirty {
        remesh_chunk(commands, manager, meshes, coord);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::chunk::{
        BLOCKS_PER_METRE, SEA_LEVEL_DETAIL, SEA_LEVEL_WORLD, WORLD_Y_MAX, detail_to_world,
        world_to_detail_origin,
    };

    #[test]
    fn biome_info_uses_the_player_position_in_metres() {
        let generator = WorldGen::new(0x4B54_414C);
        assert_eq!(
            biome_at_position(&generator, Vec3::ZERO),
            BiomeId::Plains,
            "the spawn cradle stays plains"
        );
        assert_eq!(
            biome_at_position(&generator, Vec3::splat(2000.0)),
            BiomeId::Desert,
            "detail units are converted to 1000 m before sampling"
        );
    }

    fn insert_chunk(manager: &mut ChunkManager, coord: IVec3) {
        manager.chunks.insert(
            coord,
            Chunk {
                data: ChunkData::new(coord),
                entity: Entity::PLACEHOLDER,
                water_entity: None,
                foliage_entity: None,
                triangles: 0,
                water_triangles: 0,
                foliage_triangles: 0,
                fade_remaining: 0.0,
            },
        );
    }

    fn set_detail(manager: &mut ChunkManager, pos: IVec3, voxel: impl Into<Voxel>) {
        let coord = detail_to_chunk(pos);
        if let Some(chunk) = manager.chunks.get_mut(&coord) {
            let (lx, ly, lz) = detail_to_local(pos);
            chunk.data.set_local(lx, ly, lz, voxel);
        }
    }

    fn set_world(manager: &mut ChunkManager, pos: IVec3, block: Block) {
        let coord = world_to_chunk(pos);
        if let Some(chunk) = manager.chunks.get_mut(&coord) {
            let (lx, ly, lz) = world_to_local(pos);
            chunk.data.set_world_local(lx, ly, lz, block);
        }
    }

    fn set_turf(manager: &mut ChunkManager, pos: IVec3, turf: bool) {
        let coord = world_to_chunk(pos);
        if let Some(chunk) = manager.chunks.get_mut(&coord) {
            let (lx, ly, lz) = world_to_local(pos);
            chunk.data.set_world_turf_local(lx, ly, lz, turf);
        }
    }

    fn set_snow(manager: &mut ChunkManager, pos: IVec3, snow: bool) {
        let coord = world_to_chunk(pos);
        if let Some(chunk) = manager.chunks.get_mut(&coord) {
            let (lx, ly, lz) = world_to_local(pos);
            chunk.data.set_world_snow_local(lx, ly, lz, snow);
        }
    }

    #[test]
    fn raycast_hits_a_block() {
        let mut manager = ChunkManager::default();
        let block = IVec3::new(8, 64, 8);
        insert_chunk(&mut manager, detail_to_chunk(block));
        set_detail(&mut manager, block, Block::Granite);

        let hit = manager
            .raycast(Vec3::new(8.5, 64.5, 12.5), Vec3::new(0.0, 0.0, -1.0), 10.0)
            .expect("ray should hit the block");
        assert_eq!(hit.block, block);
        assert_eq!(hit.normal, IVec3::Z);
    }

    #[test]
    fn raycast_hits_a_block_below_sea_level() {
        let mut manager = ChunkManager::default();
        let block = IVec3::new(8, -8, 8);
        assert_eq!(detail_to_chunk(block), IVec3::new(0, -1, 0));
        insert_chunk(&mut manager, detail_to_chunk(block));
        set_detail(&mut manager, block, Block::Granite);

        let hit = manager
            .raycast(Vec3::new(8.5, -7.5, 12.5), Vec3::NEG_Z, 10.0)
            .expect("negative detail Y should be raycastable");
        assert_eq!(hit.block, block);
        assert_eq!(hit.normal, IVec3::Z);
    }

    #[test]
    fn raycast_stops_only_at_global_detail_bounds() {
        let manager = ChunkManager::default();
        assert_eq!(
            manager.get_voxel_if_loaded(IVec3::new(0, DETAIL_Y_MIN - 1, 0)),
            Some(Voxel::new(Block::Bedrock))
        );
        assert_eq!(
            manager.get_voxel_if_loaded(IVec3::new(0, DETAIL_Y_MAX, 0)),
            Some(Voxel::AIR)
        );
        // Travelling further down from below the bottom stops immediately.
        assert!(
            manager
                .raycast(
                    Vec3::new(0.5, (DETAIL_Y_MIN - 1) as f32, 0.5),
                    Vec3::NEG_Y,
                    4.0
                )
                .is_none()
        );
        // Travelling further up from above the top stops immediately.
        assert!(
            manager
                .raycast(Vec3::new(0.5, DETAIL_Y_MAX as f32, 0.5), Vec3::Y, 4.0)
                .is_none()
        );
    }

    #[test]
    fn raycast_passes_over_a_thin_panel_to_hit_the_block_behind() {
        let mut manager = ChunkManager::default();
        let panel = IVec3::new(8, 64, 8);
        let stone = IVec3::new(8, 64, 7);
        insert_chunk(&mut manager, detail_to_chunk(panel));
        set_detail(
            &mut manager,
            panel,
            Voxel::new(Block::Turf).with_shape(Shape::Panel),
        );
        set_detail(&mut manager, stone, Block::Granite);

        let hit = manager
            .raycast(Vec3::new(8.5, 64.5, 10.5), Vec3::NEG_Z, 10.0)
            .expect("ray should pass above the panel and hit the stone");
        assert_eq!(hit.block, stone);
        assert_eq!(hit.normal, IVec3::Z);
    }

    #[test]
    fn raycast_hits_the_top_of_a_thin_panel() {
        let mut manager = ChunkManager::default();
        let panel = IVec3::new(8, 64, 8);
        insert_chunk(&mut manager, detail_to_chunk(panel));
        set_detail(
            &mut manager,
            panel,
            Voxel::new(Block::Turf).with_shape(Shape::Panel),
        );

        let hit = manager
            .raycast(Vec3::new(8.5, 66.5, 8.5), Vec3::NEG_Y, 10.0)
            .expect("ray should hit the top of the panel");
        assert_eq!(hit.block, panel);
        assert_eq!(hit.normal, IVec3::Y);
        assert!((hit.distance - (2.5 - PANEL_HEIGHT)).abs() < 1e-4);
    }

    #[test]
    fn mining_a_world_voxel_clears_its_eight_terrain_cells() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Granite);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Granite);
        }

        let cells = manager.terrain_cells_for_mining(world_to_detail_origin(world));
        assert_eq!(cells.len(), 8);
        for cell in world_voxel_cells(world) {
            assert!(cells.contains(&cell));
        }
    }

    #[test]
    fn mining_a_world_voxel_preserves_constructed_cells_inside_it() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Granite);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Granite);
        }
        // A player-built stair inside the same 1 m volume must survive mining.
        let built = IVec3::new(9, 41, 9);
        set_detail(
            &mut manager,
            built,
            Voxel::new(Block::Granite).with_shape(Shape::Stairs),
        );

        let cells = manager.terrain_cells_for_mining(world_to_detail_origin(world));
        assert_eq!(cells.len(), 7);
        assert!(!cells.contains(&built));
    }

    #[test]
    fn mining_a_detail_object_only_clears_that_cell() {
        let mut manager = ChunkManager::default();
        let panel = IVec3::new(10, 42, 10);
        insert_chunk(&mut manager, detail_to_chunk(panel));
        set_detail(
            &mut manager,
            panel,
            Voxel::new(Block::Turf).with_shape(Shape::Panel),
        );

        assert_eq!(manager.terrain_cells_for_mining(panel), vec![panel]);
    }

    #[test]
    fn natural_turf_is_a_flag_on_the_top_world_voxel() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Topsoil);
        set_turf(&mut manager, world, true);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Topsoil);
        }

        let top_cell = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS - 1,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        assert_eq!(manager.natural_turf_world(top_cell), Some(world));
        assert_eq!(manager.get_world_turf(world), Some(true));

        // Only the topmost detail cell of the world voxel owns the surface.
        assert_eq!(manager.natural_turf_world(top_cell - IVec3::Y), None);

        // Mining the world voxel from the side still clears all eight terrain
        // cells; the turf flag is removed separately by the interaction layer.
        let side_cells = manager.terrain_cells_for_mining(world_to_detail_origin(world));
        assert_eq!(side_cells.len(), 8);
        for cell in world_voxel_cells(world) {
            assert!(side_cells.contains(&cell));
        }

        // Removing the flag leaves the topsoil voxel and its detail cells.
        let dirty = manager.set_world_turf(world, false);
        assert!(!dirty.is_empty());
        assert_eq!(manager.get_world_turf(world), Some(false));
        assert_eq!(manager.get_world_block(world), Some(Block::Topsoil));
        assert_eq!(
            manager.turf_edits.get(&world),
            Some(&false),
            "turf removal should be tracked for saving"
        );
    }

    #[test]
    fn natural_turf_panel_has_no_extra_collision() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Topsoil);
        set_turf(&mut manager, world, true);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Topsoil);
        }

        let surface = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        assert!(manager.is_turf_surface_cell(surface));
        assert_eq!(manager.natural_detail_offset(surface), PANEL_HEIGHT);

        assert!(
            manager.collision_boxes(surface).is_none(),
            "decorative turf panel must not add collision"
        );

        // The topsoil cell below is still a full cube.
        let soil = surface - IVec3::Y;
        let boxes = manager
            .collision_boxes(soil)
            .expect("topsoil should stay solid");
        let (_, max) = boxes.iter().next().expect("topsoil cube box");
        assert_eq!(max, Vec3::ONE);
    }

    #[test]
    fn wood_stems_collide_but_loose_clutter_does_not() {
        let mut manager = ChunkManager::default();
        insert_chunk(&mut manager, IVec3::ZERO);

        let stem = IVec3::new(8, 8, 8);
        set_detail(
            &mut manager,
            stem,
            Voxel::from_oriented_parts(Block::Wood, Shape::Stem, block::Orientation::default()),
        );
        let boxes = manager.collision_boxes(stem).expect("stem should be solid");
        assert!(boxes.iter().next().is_some(), "stem needs a collision box");

        let pebble = IVec3::new(10, 8, 8);
        set_detail(
            &mut manager,
            pebble,
            Voxel::new(Block::Granite).with_shape(Shape::Pebble),
        );
        assert!(
            manager.collision_boxes(pebble).is_none(),
            "loose pebbles must remain passable"
        );

        let stick = IVec3::new(12, 8, 8);
        set_detail(
            &mut manager,
            stick,
            Voxel::new(Block::Wood).with_shape(Shape::Stick),
        );
        assert!(
            manager.collision_boxes(stick).is_none(),
            "loose sticks must remain passable"
        );
    }

    #[test]
    fn natural_snow_is_a_flag_on_intact_rock() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Granite);
        set_snow(&mut manager, world, true);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Granite);
        }

        let top_cell = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS - 1,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        assert_eq!(
            manager.natural_surface_world(SurfaceLayer::Snow, top_cell),
            Some(world)
        );
        assert_eq!(
            manager.get_world_surface(SurfaceLayer::Snow, world),
            Some(true)
        );
        assert_eq!(manager.get_world_block(world), Some(Block::Granite));

        let surface = top_cell + IVec3::Y;
        assert!(manager.is_surface_cell(SurfaceLayer::Snow, surface));
        assert_eq!(manager.natural_detail_offset(surface), PANEL_HEIGHT);
        assert!(
            manager.collision_boxes(surface).is_none(),
            "decorative snow panel must not add collision"
        );

        let dirty = manager.set_world_surface(SurfaceLayer::Snow, world, false);
        assert!(!dirty.is_empty());
        assert_eq!(
            manager.get_world_surface(SurfaceLayer::Snow, world),
            Some(false)
        );
        assert_eq!(
            manager.snow_edits.get(&world),
            Some(&false),
            "snow removal should be tracked for saving"
        );
        assert_eq!(manager.get_world_block(world), Some(Block::Granite));
    }

    #[test]
    fn natural_detail_on_turf_raycasts_at_the_raised_height() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Topsoil);
        set_turf(&mut manager, world, true);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Topsoil);
        }
        let surface = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        set_detail(
            &mut manager,
            surface,
            Voxel::from_oriented_parts(
                Block::Wood,
                Shape::Stick,
                block::Orientation::from_index(2),
            ),
        );

        let hit = manager
            .raycast(Vec3::new(9.0, 43.5, 9.0), Vec3::NEG_Y, 10.0)
            .expect("ray should hit the raised stick");
        assert_eq!(hit.block, surface);
        assert_eq!(hit.normal, IVec3::Y);
        let expected = 43.5 - (surface.y as f32 + PANEL_HEIGHT + 0.06);
        assert!(
            (hit.distance - expected).abs() < 1e-4,
            "stick should be lifted onto the turf: {} vs {}",
            hit.distance,
            expected
        );
    }

    #[test]
    fn natural_turf_cannot_be_harvested_through_a_constructed_piece() {
        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 20, 4);
        insert_chunk(&mut manager, world_to_chunk(world));
        set_world(&mut manager, world, Block::Topsoil);
        set_turf(&mut manager, world, true);
        for cell in world_voxel_cells(world) {
            set_detail(&mut manager, cell, Block::Topsoil);
        }
        let top_cell = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS - 1,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        set_detail(
            &mut manager,
            top_cell,
            Voxel::new(Block::Granite).with_shape(Shape::Stairs),
        );

        assert_eq!(manager.natural_turf_world(top_cell), None);
    }

    #[test]
    fn turf_flag_edits_refresh_adjacent_padding() {
        let mut manager = ChunkManager::default();
        for coord in [IVec3::new(0, 1, 0), IVec3::new(1, 1, 0)] {
            insert_chunk(&mut manager, coord);
        }

        let world = IVec3::new(WORLD_CHUNK_X as i32 - 1, 20, 4);
        let dirty = manager.set_world_turf(world, true);
        assert!(dirty.contains(&IVec3::new(0, 1, 0)));
        assert!(dirty.contains(&IVec3::new(1, 1, 0)));
        assert_eq!(manager.get_world_turf(world), Some(true));
        assert!(
            manager.chunks[&IVec3::new(1, 1, 0)]
                .data
                .sample_world_turf_local(-1, 4, 4)
        );
    }

    #[test]
    fn editing_a_border_updates_padding() {
        let mut manager = ChunkManager::default();
        for coord in [IVec3::new(0, 2, 0), IVec3::new(1, 2, 0)] {
            insert_chunk(&mut manager, coord);
        }

        let detail = IVec3::new(CHUNK_X as i32 - 1, 64, 8);
        let dirty = manager.set_voxel(detail, Voxel::new(Block::Granite));
        assert!(dirty.contains(&IVec3::new(0, 2, 0)));
        assert!(dirty.contains(&IVec3::new(1, 2, 0)));
        assert_eq!(manager.get_block_if_loaded(detail), Some(Block::Granite));
        assert_eq!(
            manager.chunks[&IVec3::new(1, 2, 0)]
                .data
                .sample_local(-1, 0, 8),
            Block::Granite
        );
    }

    #[test]
    fn edits_propagate_to_y_neighbours() {
        let mut manager = ChunkManager::default();
        for coord in [IVec3::new(0, 0, 0), IVec3::new(0, 1, 0)] {
            insert_chunk(&mut manager, coord);
        }

        // Top border of chunk (0, 0, 0).
        let top = IVec3::new(8, CHUNK_Y as i32 - 1, 8);
        let dirty = manager.set_voxel(top, Voxel::new(Block::Granite));
        assert!(dirty.contains(&IVec3::new(0, 0, 0)));
        assert!(dirty.contains(&IVec3::new(0, 1, 0)));
        assert_eq!(
            manager.chunks[&IVec3::new(0, 1, 0)]
                .data
                .sample_local(8, -1, 8),
            Block::Granite
        );

        // Bottom border of chunk (0, 1, 0).
        let bottom = IVec3::new(9, CHUNK_Y as i32, 8);
        let dirty = manager.set_voxel(bottom, Voxel::new(Block::Slate));
        assert!(dirty.contains(&IVec3::new(0, 0, 0)));
        assert!(dirty.contains(&IVec3::new(0, 1, 0)));
        assert_eq!(
            manager.chunks[&IVec3::new(0, 0, 0)]
                .data
                .sample_local(9, CHUNK_Y as i32, 8),
            Block::Slate
        );

        // The 1 m world layer and turf flag propagate in Y too.
        let world_top = IVec3::new(4, WORLD_CHUNK_Y as i32 - 1, 5);
        let dirty = manager.set_world_block(world_top, Block::Granite);
        assert!(dirty.contains(&IVec3::new(0, 0, 0)));
        assert!(dirty.contains(&IVec3::new(0, 1, 0)));
        assert_eq!(
            manager.chunks[&IVec3::new(0, 1, 0)]
                .data
                .sample_world_local(4, -1, 5),
            Block::Granite
        );

        let dirty = manager.set_world_turf(IVec3::new(4, WORLD_CHUNK_Y as i32 - 1, 6), true);
        assert!(dirty.contains(&IVec3::new(0, 0, 0)));
        assert!(dirty.contains(&IVec3::new(0, 1, 0)));
    }

    #[test]
    fn negative_y_chunk_uses_signed_coordinates_and_collision() {
        let mut manager = ChunkManager::default();
        let coord = IVec3::new(0, -1, 0);
        insert_chunk(&mut manager, coord);
        assert_eq!(chunk_origin(coord), Vec3::new(0.0, -(CHUNK_Y as f32), 0.0));

        let detail = IVec3::new(5, -5, 5);
        set_detail(&mut manager, detail, Block::Granite);
        assert_eq!(
            manager.get_voxel_if_loaded(detail),
            Some(Voxel::new(Block::Granite))
        );
        assert_eq!(manager.get_block(detail), Block::Granite);

        let boxes = manager.collision_boxes(detail).expect("solid block");
        let (min, max) = boxes.iter().next().expect("cube box");
        assert_eq!(min, Vec3::ZERO);
        assert_eq!(max, Vec3::ONE);

        // Global detail bounds, not y < 0, decide the outside-world result.
        assert!(
            manager
                .collision_boxes(IVec3::new(0, DETAIL_Y_MIN - 1, 0))
                .is_some()
        );
        assert!(
            manager
                .collision_boxes(IVec3::new(0, DETAIL_Y_MAX, 0))
                .is_none()
        );
    }

    #[test]
    fn world_and_detail_targets_below_sea() {
        let mut manager = ChunkManager::default();
        insert_chunk(&mut manager, IVec3::new(0, -1, 0));

        assert_eq!(SEA_LEVEL_WORLD, 0);
        assert_eq!(SEA_LEVEL_DETAIL, 0);

        let below_sea_detail = IVec3::new(8, SEA_LEVEL_DETAIL - 1, 8);
        assert_eq!(detail_to_world(below_sea_detail), IVec3::new(4, -1, 4));
        assert_eq!(
            manager.get_world_block(IVec3::new(4, WORLD_Y_MIN - 1, 4)),
            Some(Block::Bedrock)
        );
        assert_eq!(
            manager.get_voxel_if_loaded(IVec3::new(8, DETAIL_Y_MIN - 1, 8)),
            Some(Voxel::new(Block::Bedrock))
        );

        // Real terrain below sea level is stored in the cy = -1 chunk.
        let world = IVec3::new(4, -5, 4);
        set_world(&mut manager, world, Block::Granite);
        set_turf(&mut manager, world, true);
        assert_eq!(manager.get_world_block(world), Some(Block::Granite));
        assert_eq!(manager.get_world_turf(world), Some(true));

        let detail = IVec3::new(8, -10, 8);
        set_detail(&mut manager, detail, Block::Granite);
        assert_eq!(manager.world_terrain_target(detail), Some(world));
    }

    #[test]
    fn cull_distance_accounts_for_horizontal_and_asymmetric_vertical_radius() {
        let mut manager = ChunkManager::default();
        assert_eq!(manager.vertical_view_distance(), VERTICAL_VIEW_CHUNKS);
        let horizontal = manager.view_distance as f32 * CHUNK_X as f32;
        let vertical = manager.vertical_view_distance() as f32 * CHUNK_Y as f32;
        let base = (horizontal * horizontal + vertical * vertical).sqrt();
        assert!((manager.cull_distance() - base).abs() < 1e-3);

        manager.deep_tail_active = true;
        let deep_vertical = DEEP_TAIL_DEPTH as f32 * CHUNK_Y as f32;
        let deep = (horizontal * horizontal + deep_vertical * deep_vertical).sqrt();
        assert!(manager.cull_distance() > base);
        assert!((manager.cull_distance() - deep).abs() < 1e-3);
        // Fog stays tied to the normal band so looking down cannot move it.
        assert!((manager.fog_distance() - base).abs() < 1e-3);

        // A larger horizontal radius must not grow the capped vertical radius.
        manager.deep_tail_active = false;
        manager.view_distance = 16;
        assert_eq!(manager.vertical_view_distance(), VERTICAL_VIEW_CHUNKS);
        let horizontal = 16.0 * CHUNK_X as f32;
        let expected = (horizontal * horizontal + vertical * vertical).sqrt();
        assert!((manager.cull_distance() - expected).abs() < 1e-3);

        // Small view distances shrink both radii.
        manager.view_distance = 2;
        assert_eq!(manager.vertical_view_distance(), 2);
        let horizontal = 2.0 * CHUNK_X as f32;
        let vertical = 2.0 * CHUNK_Y as f32;
        let expected = (horizontal * horizontal + vertical * vertical).sqrt();
        assert!((manager.cull_distance() - expected).abs() < 1e-3);
    }

    #[test]
    fn stream_volume_uses_horizontal_circle_and_capped_vertical_band() {
        let manager = ChunkManager::default();
        let radius = manager.horizontal_view_distance();
        let vertical = manager.vertical_view_distance();
        let volume = StreamVolume::new(radius, vertical, false);
        assert_eq!(radius, manager.view_distance);
        assert_eq!(vertical, VERTICAL_VIEW_CHUNKS);

        assert!(in_stream_volume(IVec3::new(radius, 0, 0), volume));
        assert!(in_stream_volume(IVec3::new(0, vertical, 0), volume));
        assert!(in_stream_volume(IVec3::new(5, 0, 5), volume));
        assert!(!in_stream_volume(IVec3::new(6, 0, 6), volume));
        assert!(!in_stream_volume(IVec3::new(0, vertical + 1, 0), volume));
        assert!(!in_stream_volume(
            IVec3::new(0, -DEEP_TAIL_DEPTH, 0),
            volume
        ));

        // Eviction expands the volume by the unload margin.
        assert!(!outside_stream_volume(IVec3::new(radius + 1, 0, 0), volume));
        assert!(outside_stream_volume(
            IVec3::new(radius + UNLOAD_MARGIN + 1, 0, 0),
            volume
        ));
        assert!(outside_stream_volume(
            IVec3::new(0, vertical + UNLOAD_MARGIN + 1, 0),
            volume
        ));
    }

    #[test]
    fn deep_tail_volume_is_narrow_and_bounded() {
        let volume = StreamVolume::new(8, VERTICAL_VIEW_CHUNKS, true);
        assert!(in_stream_volume(IVec3::new(8, 0, 0), volume));
        assert!(in_stream_volume(IVec3::new(3, -10, 0), volume));
        assert!(!in_stream_volume(IVec3::new(4, -10, 0), volume));
        assert!(in_stream_volume(IVec3::new(0, -DEEP_TAIL_DEPTH, 0), volume));
        assert!(!in_stream_volume(
            IVec3::new(0, -DEEP_TAIL_DEPTH - 1, 0),
            volume
        ));
        assert!(!in_stream_volume(
            IVec3::new(0, VERTICAL_VIEW_CHUNKS + 1, 0),
            volume
        ));

        assert!(!outside_stream_volume(
            IVec3::new(DEEP_TAIL_RADIUS, -10, 0),
            volume
        ));
        assert!(!outside_stream_volume(
            IVec3::new(DEEP_TAIL_RADIUS + 1, -10, 0),
            volume
        ));
        assert!(outside_stream_volume(
            IVec3::new(DEEP_TAIL_RADIUS + UNLOAD_MARGIN + 1, -10, 0),
            volume
        ));
    }

    #[test]
    fn stream_candidates_include_the_tail_without_a_full_depth_cylinder() {
        let center = IVec3::new(3, -2, 5);
        let normal_volume = StreamVolume::new(8, VERTICAL_VIEW_CHUNKS, false);
        let tail_volume = StreamVolume::new(8, VERTICAL_VIEW_CHUNKS, true);
        let normal = stream_candidates(center, normal_volume);
        let tail = stream_candidates(center, tail_volume);

        assert_eq!(normal.len(), normal_volume.chunk_count());
        assert_eq!(tail.len(), tail_volume.chunk_count());
        assert!(tail.len() > normal.len());
        assert!(
            tail.iter()
                .all(|coord| in_stream_volume(*coord - center, tail_volume))
        );
        assert!(
            tail.iter()
                .any(|coord| coord.y - center.y < -VERTICAL_VIEW_CHUNKS)
        );
        assert!(tail.iter().all(|coord| {
            coord.y - center.y >= -VERTICAL_VIEW_CHUNKS
                || (coord.x - center.x).abs() <= DEEP_TAIL_RADIUS
                    && (coord.z - center.z).abs() <= DEEP_TAIL_RADIUS
        }));
    }

    #[test]
    fn stream_candidates_are_closest_first_and_follow_center() {
        let center = IVec3::new(3, -2, 5);
        let volume = StreamVolume::new(2, 1, false);
        let candidates = stream_candidates(center, volume);

        assert_eq!(candidates.len(), volume.chunk_count());
        assert!(
            candidates
                .iter()
                .all(|coord| in_stream_volume(*coord - center, volume))
        );

        let distance = |coord: IVec3| {
            let delta = coord - center;
            delta.x * delta.x + delta.y * delta.y + delta.z * delta.z
        };
        assert!(
            candidates
                .windows(2)
                .all(|pair| distance(pair[0]) <= distance(pair[1]))
        );
        assert_eq!(candidates[0], center);

        let moved = stream_candidates(center + IVec3::X, volume);
        assert_eq!(moved[0], center + IVec3::X);
        assert_ne!(candidates, moved);
        assert!(stream_candidates(center, StreamVolume::new(3, 1, false)).len() > candidates.len());
    }

    #[test]
    fn stream_candidates_are_distance_centered_not_camera_biased() {
        let center = IVec3::ZERO;
        let volume = StreamVolume::new(4, 0, false);
        let candidates = stream_candidates(center, volume);
        let distance = |coord: IVec3| {
            let d = coord - center;
            d.x * d.x + d.y * d.y + d.z * d.z
        };
        assert!(
            candidates
                .windows(2)
                .all(|pair| distance(pair[0]) <= distance(pair[1])),
            "bulk stream should be ordered by distance from player"
        );
    }

    #[test]
    fn urgent_candidates_put_falling_ahead_of_camera_direction() {
        let volume = StreamVolume::new(8, VERTICAL_VIEW_CHUNKS, true);
        let position = Vec3::new(16.0, 16.0, 16.0);
        let velocity = Vec3::NEG_Y * (BLOCKS_PER_METRE * 50.0);
        let ranked = ranked_urgent_candidates(position, velocity, Vec3::X, volume);
        let below = IVec3::new(0, -3, 0);
        let ahead = IVec3::new(3, 0, 0);

        let below_index = ranked
            .iter()
            .position(|(coord, _, _)| *coord == below)
            .unwrap();
        let ahead_index = ranked
            .iter()
            .position(|(coord, _, _)| *coord == ahead)
            .unwrap();
        assert_eq!(ranked[below_index].1, 1, "falling path is tier 1");
        assert_eq!(ranked[ahead_index].1, 2, "camera look-ahead is tier 2");
        assert!(below_index < ahead_index);
    }

    #[test]
    fn urgent_candidates_put_walking_ahead_of_camera_direction() {
        let volume = StreamVolume::new(8, VERTICAL_VIEW_CHUNKS, true);
        let position = Vec3::new(16.0, 16.0, 16.0);
        let velocity = Vec3::X * (BLOCKS_PER_METRE * 50.0);
        let ranked = ranked_urgent_candidates(position, velocity, Vec3::NEG_X, volume);
        let moving_to = IVec3::new(3, 0, 0);
        let looking_at = IVec3::new(-3, 0, 0);

        let moving_index = ranked
            .iter()
            .position(|(coord, _, _)| *coord == moving_to)
            .unwrap();
        let looking_index = ranked
            .iter()
            .position(|(coord, _, _)| *coord == looking_at)
            .unwrap();
        assert_eq!(ranked[moving_index].1, 1);
        assert_eq!(ranked[looking_index].1, 2);
        assert!(moving_index < looking_index);
    }

    #[test]
    fn urgent_prediction_extends_with_movement_speed() {
        let volume = StreamVolume::new(8, VERTICAL_VIEW_CHUNKS, true);
        let position = Vec3::new(16.0, 16.0, 16.0);
        let furthest_ahead = |speed: f32| {
            let velocity = Vec3::X * (BLOCKS_PER_METRE * speed);
            ranked_urgent_candidates(position, velocity, Vec3::X, volume)
                .into_iter()
                .filter(|(_, tier, _)| *tier == 1)
                .map(|(coord, _, _)| coord.x)
                .max()
                .unwrap_or(0)
        };
        assert!(furthest_ahead(50.0) > furthest_ahead(7.8));
    }

    #[test]
    fn pending_poll_order_prioritizes_urgent_then_nearest() {
        let mut pending = HashMap::new();
        let mut insert = |coord: IVec3, urgency: Urgency, distance: f32| {
            pending.insert(
                coord,
                PendingChunk {
                    task: (),
                    urgency,
                    distance,
                },
            );
        };
        insert(IVec3::new(1, 0, 0), Urgency::Normal, 10.0);
        insert(IVec3::new(2, 0, 0), Urgency::Urgent, 100.0);
        insert(IVec3::new(3, 0, 0), Urgency::Normal, 1.0);
        insert(IVec3::new(4, 0, 0), Urgency::Urgent, 1.0);

        assert_eq!(
            pending_poll_order(&pending),
            vec![
                IVec3::new(4, 0, 0),
                IVec3::new(2, 0, 0),
                IVec3::new(3, 0, 0),
                IVec3::new(1, 0, 0),
            ]
        );
    }

    #[test]
    fn urgent_reservation_preempts_farthest_normals_and_suppresses_restart() {
        let mut pending = HashMap::new();
        for index in 0..MAX_PENDING {
            pending.insert(
                IVec3::new(index as i32, 0, 0),
                PendingChunk {
                    task: (),
                    urgency: Urgency::Normal,
                    distance: index as f32 * 10.0,
                },
            );
        }
        let mut suppressed = HashMap::new();
        let preempted = reserve_urgent_capacity(&mut pending, &mut suppressed, 7, 4);

        assert_eq!(preempted.len(), URGENT_RESERVED);
        assert_eq!(pending.len(), MAX_PENDING - URGENT_RESERVED);
        assert!(
            pending
                .values()
                .all(|entry| entry.urgency == Urgency::Normal)
        );
        assert!(preempted.iter().all(|coord| coord.x >= 8));
        assert_eq!(
            suppressed.get(&preempted[0]),
            Some(&(7 + URGENT_SUPPRESS_FRAMES))
        );
        assert!(preempted.iter().all(|coord| !pending.contains_key(coord)));
        assert!(
            preempted
                .iter()
                .all(|coord| !accept_generated_chunk(&mut pending, *coord))
        );

        let mut manager = ChunkManager {
            frame: 7,
            suppressed,
            ..Default::default()
        };
        assert!(
            preempted
                .iter()
                .all(|coord| is_suppressed(&manager, *coord))
        );
        manager.frame += URGENT_SUPPRESS_FRAMES;
        assert!(
            preempted
                .iter()
                .all(|coord| !is_suppressed(&manager, *coord))
        );
    }

    #[test]
    fn deep_tail_activation_uses_position_velocity_and_look_hysteresis() {
        let generator = WorldGen::new(0x4B54_414C);
        let surface = generator.height_at(0, 0) * DETAILS_PER_WORLD_AXIS;
        let above = Vec3::new(16.0, (surface + 16) as f32, 16.0);
        let below = Vec3::new(16.0, (surface - 16) as f32, 16.0);

        assert!(!should_enable_deep_tail(
            above,
            &generator,
            Vec3::ZERO,
            0.0,
            false
        ));
        assert!(should_enable_deep_tail(
            above,
            &generator,
            Vec3::ZERO,
            -0.35,
            false
        ));
        assert!(!should_enable_deep_tail(
            above,
            &generator,
            Vec3::ZERO,
            -0.2,
            false
        ));
        assert!(should_enable_deep_tail(
            above,
            &generator,
            Vec3::ZERO,
            -0.2,
            true
        ));
        assert!(!should_enable_deep_tail(
            above,
            &generator,
            Vec3::ZERO,
            -0.12,
            true
        ));
        assert!(should_enable_deep_tail(
            below,
            &generator,
            Vec3::ZERO,
            1.0,
            false
        ));
        let falling = Vec3::NEG_Y * (BLOCKS_PER_METRE * 4.0);
        assert!(should_enable_deep_tail(
            above, &generator, falling, 1.0, false
        ));
    }

    #[test]
    fn recorded_edits_are_reapplied_when_a_chunk_regenerates() {
        let mut manager = ChunkManager::default();
        let coord = IVec3::new(0, 1, 0);
        let world = IVec3::new(3, 20, 5);
        let turf = IVec3::new(4, 20, 6);
        let snow = IVec3::new(5, 20, 6);
        let detail = IVec3::new(7, 41, 11);
        manager.world_edits.insert(world, Block::Air);
        manager.turf_edits.insert(turf, false);
        manager.snow_edits.insert(snow, false);
        manager
            .detail_edits
            .insert(detail, Voxel::new(Block::Slate));

        // Pristine generated state, as if the chunk had just been regenerated.
        let mut data = ChunkData::new(coord);
        data.set_world_local(3, 4, 5, Block::Topsoil);
        data.set_world_turf_local(4, 4, 6, true);
        data.set_world_snow_local(5, 4, 6, true);
        data.set_local(7, 9, 11, Block::Topsoil);

        assert!(apply_recorded_edits(&manager, coord, &mut data));
        assert_eq!(data.get_world_local(3, 4, 5), Block::Air);
        assert!(!data.get_world_turf_local(4, 4, 6));
        assert!(!data.get_world_snow_local(5, 4, 6));
        assert_eq!(data.get_local(7, 9, 11), Voxel::new(Block::Slate));

        // Edits in other X and Y chunks are left for their own chunk.
        let mut elsewhere = ChunkManager::default();
        elsewhere.detail_edits.insert(
            IVec3::new(CHUNK_X as i32 + 2, 41, 11),
            Voxel::new(Block::Sand),
        );
        elsewhere
            .detail_edits
            .insert(IVec3::new(7, 16, 11), Voxel::new(Block::Sand));
        let mut other = ChunkData::new(coord);
        assert!(!apply_recorded_edits(&elsewhere, coord, &mut other));
    }

    #[test]
    fn recorded_edits_below_sea_are_reapplied() {
        let mut manager = ChunkManager::default();
        let coord = IVec3::new(0, -1, 0);
        let world = IVec3::new(3, -5, 5);
        let detail = IVec3::new(7, -10, 11);
        manager.world_edits.insert(world, Block::Air);
        manager
            .detail_edits
            .insert(detail, Voxel::new(Block::Slate));

        let mut data = ChunkData::new(coord);
        data.set_world_local(3, 11, 5, Block::Granite);
        data.set_local(7, 22, 11, Block::Granite);

        assert!(apply_recorded_edits(&manager, coord, &mut data));
        assert_eq!(data.get_world_local(3, 11, 5), Block::Air);
        assert_eq!(data.get_local(7, 22, 11), Voxel::new(Block::Slate));
    }

    #[test]
    fn unloaded_chunks_are_solid_for_collision() {
        let manager = ChunkManager::default();
        assert_eq!(manager.get_block(IVec3::new(0, 50, 0)), Block::Granite);
        assert_eq!(manager.get_block_if_loaded(IVec3::new(0, 50, 0)), None);
        assert_eq!(
            manager.get_block(IVec3::new(0, DETAIL_Y_MIN - 1, 0)),
            Block::Bedrock
        );
        assert_eq!(
            manager.get_block(IVec3::new(0, DETAIL_Y_MAX, 0)),
            Block::Air
        );
        assert_eq!(
            manager.get_world_block(IVec3::new(0, WORLD_Y_MIN - 1, 0)),
            Some(Block::Bedrock)
        );
        assert_eq!(
            manager.get_world_block(IVec3::new(0, WORLD_Y_MAX, 0)),
            Some(Block::Air)
        );
    }
}
