//! Cubic chunk dimensions, storage and coordinate helpers.
//!
//! Chunks are 16 m cubes. They store a one-block padding border on every axis so
//! the mesher can cull faces and compute ambient occlusion across chunk borders
//! without looking at neighbouring chunks. Edits are written into the padding of
//! adjacent chunks as well, so borders stay seamless.
//!
//! The world is finite in depth: world voxel Y runs from [`WORLD_Y_MIN`] to
//! [`WORLD_Y_MAX`]. Sea level is Y = 0. The nominal land footprint is
//! [`LAND_HALF_EXTENT`] metres from the origin on X and Z; generation blends its
//! edge down into ocean rather than ending abruptly.

use bevy::prelude::*;

use super::block::{Block, Voxel};

/// Detail cells per chunk axis. 32 detail cells at 0.5 m each is a 16 m cube.
pub const CHUNK_X: usize = 32;
pub const CHUNK_Y: usize = 32;
pub const CHUNK_Z: usize = 32;

/// Physical size of one detail block in metres. Detail voxels are 0.5 m and are
/// the engine's spatial unit, so physical tuning values are converted with
/// [`BLOCKS_PER_METRE`].
pub const DETAIL_VOXEL_SIZE_M: f32 = 0.5;
/// Backwards-compatible name for the detail voxel size.
pub const BLOCK_SIZE: f32 = DETAIL_VOXEL_SIZE_M;
/// Blocks per metre.
pub const BLOCKS_PER_METRE: f32 = 1.0 / BLOCK_SIZE;

/// World/terrain voxels are 1 m. A world voxel is always a 2x2x2 block of
/// detail voxels, so the two grids stay aligned on every chunk border.
pub const WORLD_VOXEL_SIZE_M: f32 = 1.0;
pub const DETAILS_PER_WORLD_AXIS: i32 = (WORLD_VOXEL_SIZE_M / DETAIL_VOXEL_SIZE_M) as i32;
pub const DETAILS_PER_WORLD_VOXEL: u32 =
    (DETAILS_PER_WORLD_AXIS * DETAILS_PER_WORLD_AXIS * DETAILS_PER_WORLD_AXIS) as u32;

/// World-layer dimensions for one detail chunk (half the detail dimensions).
pub const WORLD_CHUNK_X: usize = CHUNK_X / DETAILS_PER_WORLD_AXIS as usize;
pub const WORLD_CHUNK_Y: usize = CHUNK_Y / DETAILS_PER_WORLD_AXIS as usize;
pub const WORLD_CHUNK_Z: usize = CHUNK_Z / DETAILS_PER_WORLD_AXIS as usize;

/// One block of padding on every axis.
pub const PAD: usize = 1;
pub const WORLD_PAD: usize = 1;
pub const PADDED_X: usize = CHUNK_X + PAD * 2;
pub const PADDED_Y: usize = CHUNK_Y + PAD * 2;
pub const PADDED_Z: usize = CHUNK_Z + PAD * 2;
pub const PADDED_VOLUME: usize = PADDED_X * PADDED_Y * PADDED_Z;
pub const WORLD_PADDED_X: usize = WORLD_CHUNK_X + WORLD_PAD * 2;
pub const WORLD_PADDED_Y: usize = WORLD_CHUNK_Y + WORLD_PAD * 2;
pub const WORLD_PADDED_Z: usize = WORLD_CHUNK_Z + WORLD_PAD * 2;
pub const WORLD_PADDED_VOLUME: usize = WORLD_PADDED_X * WORLD_PADDED_Y * WORLD_PADDED_Z;

/// A thin natural layer on top of a 1 m world voxel. Turf caps soil; snow caps
/// exposed alpine rock. Both use the same raised-slab storage and rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceLayer {
    Turf,
    Snow,
}

impl SurfaceLayer {
    pub const ALL: [Self; 2] = [Self::Turf, Self::Snow];

    #[inline]
    pub fn block(self) -> Block {
        match self {
            Self::Turf => Block::Turf,
            Self::Snow => Block::Snow,
        }
    }

    #[inline]
    pub fn record_name(self) -> &'static str {
        match self {
            Self::Turf => "turf",
            Self::Snow => "snow",
        }
    }
}

/// Sea level at Y = 0, measured in 1 m world voxels.
pub const SEA_LEVEL_WORLD: i32 = 0;
/// Sea level in detail cells.
pub const SEA_LEVEL_DETAIL: i32 = SEA_LEVEL_WORLD * DETAILS_PER_WORLD_AXIS;

/// Lowest generated world voxel. Four kilometres below sea level leaves room
/// for caves and deep geology. The bottom layer is bedrock.
pub const WORLD_Y_MIN: i32 = -4000;
/// Highest generated world voxel, exclusive. Four kilometres above sea level
/// leaves room for mountains.
pub const WORLD_Y_MAX: i32 = 4000;
/// Lowest generated detail cell.
pub const DETAIL_Y_MIN: i32 = WORLD_Y_MIN * DETAILS_PER_WORLD_AXIS;
/// Highest generated detail cell, exclusive.
pub const DETAIL_Y_MAX: i32 = WORLD_Y_MAX * DETAILS_PER_WORLD_AXIS;

/// Half-width of the nominal land footprint in world metres. Terrain fades into
/// ocean around this square, so dry land ends before the boundary is reached.
pub const LAND_HALF_EXTENT: i32 = 2000;
/// Ocean floor below sea level. Everything deeper is bedrock geology.
pub const OCEAN_FLOOR_WORLD: i32 = -64;

/// True when a world voxel Y is inside the generated world.
#[inline]
pub const fn world_y_in_range(y: i32) -> bool {
    y >= WORLD_Y_MIN && y < WORLD_Y_MAX
}

/// True when a detail cell Y is inside the generated world.
#[inline]
pub const fn detail_y_in_range(y: i32) -> bool {
    y >= DETAIL_Y_MIN && y < DETAIL_Y_MAX
}

/// Map a detail cell to the world voxel that owns it.
#[inline]
pub fn detail_to_world(pos: IVec3) -> IVec3 {
    pos.div_euclid(IVec3::splat(DETAILS_PER_WORLD_AXIS))
}

/// Map the minimum detail cell of a world voxel.
#[inline]
pub fn world_to_detail_origin(pos: IVec3) -> IVec3 {
    pos * DETAILS_PER_WORLD_AXIS
}

/// Engine-space centre of the 1 m world voxel that owns a detail cell.
///
/// Engine units are detail cells, so this is the world coordinate scaled by
/// [`DETAILS_PER_WORLD_AXIS`], not the raw world coordinate.
#[inline]
pub fn world_voxel_center(detail: IVec3) -> Vec3 {
    let scale = DETAILS_PER_WORLD_AXIS as f32;
    detail_to_world(detail).as_vec3() * scale + Vec3::splat(scale * 0.5)
}

/// The eight detail cells covered by one world voxel.
#[inline]
pub fn world_voxel_cells(pos: IVec3) -> [IVec3; 8] {
    let origin = world_to_detail_origin(pos);
    let mut cells = [origin; 8];
    let mut index = 0;
    for dy in 0..DETAILS_PER_WORLD_AXIS {
        for dz in 0..DETAILS_PER_WORLD_AXIS {
            for dx in 0..DETAILS_PER_WORLD_AXIS {
                cells[index] = origin + IVec3::new(dx, dy, dz);
                index += 1;
            }
        }
    }
    cells
}

/// The chunk a world-voxel position belongs to.
#[inline]
pub fn world_to_chunk(pos: IVec3) -> IVec3 {
    IVec3::new(
        pos.x.div_euclid(WORLD_CHUNK_X as i32),
        pos.y.div_euclid(WORLD_CHUNK_Y as i32),
        pos.z.div_euclid(WORLD_CHUNK_Z as i32),
    )
}

/// The chunk a detail-cell position belongs to.
#[inline]
pub fn detail_to_chunk(pos: IVec3) -> IVec3 {
    IVec3::new(
        pos.x.div_euclid(CHUNK_X as i32),
        pos.y.div_euclid(CHUNK_Y as i32),
        pos.z.div_euclid(CHUNK_Z as i32),
    )
}

/// The chunk a detail-space (float) position belongs to.
#[inline]
pub fn position_to_chunk(pos: Vec3) -> IVec3 {
    detail_to_chunk(pos.floor().as_ivec3())
}

/// Local world-voxel coordinates within a chunk.
#[inline]
pub fn world_to_local(pos: IVec3) -> (usize, usize, usize) {
    (
        pos.x.rem_euclid(WORLD_CHUNK_X as i32) as usize,
        pos.y.rem_euclid(WORLD_CHUNK_Y as i32) as usize,
        pos.z.rem_euclid(WORLD_CHUNK_Z as i32) as usize,
    )
}

/// Local detail-cell coordinates within a chunk.
#[inline]
pub fn detail_to_local(pos: IVec3) -> (usize, usize, usize) {
    (
        pos.x.rem_euclid(CHUNK_X as i32) as usize,
        pos.y.rem_euclid(CHUNK_Y as i32) as usize,
        pos.z.rem_euclid(CHUNK_Z as i32) as usize,
    )
}

/// Detail-space origin (minimum corner) of a chunk.
#[inline]
pub fn chunk_origin(coord: IVec3) -> Vec3 {
    Vec3::new(
        (coord.x * CHUNK_X as i32) as f32,
        (coord.y * CHUNK_Y as i32) as f32,
        (coord.z * CHUNK_Z as i32) as f32,
    )
}

/// World-voxel origin (minimum corner) of a chunk.
#[inline]
pub fn chunk_world_origin(coord: IVec3) -> IVec3 {
    IVec3::new(
        coord.x * WORLD_CHUNK_X as i32,
        coord.y * WORLD_CHUNK_Y as i32,
        coord.z * WORLD_CHUNK_Z as i32,
    )
}

/// Index into the padded detail array. All coordinates include padding.
#[inline]
pub fn padded_index(px: usize, py: usize, pz: usize) -> usize {
    debug_assert!(px < PADDED_X && py < PADDED_Y && pz < PADDED_Z);
    (py * PADDED_Z + pz) * PADDED_X + px
}

/// Index into the padded world-voxel array. All coordinates include padding.
#[inline]
pub fn world_padded_index(px: usize, py: usize, pz: usize) -> usize {
    debug_assert!(px < WORLD_PADDED_X && py < WORLD_PADDED_Y && pz < WORLD_PADDED_Z);
    (py * WORLD_PADDED_Z + pz) * WORLD_PADDED_X + px
}

/// Block storage for one cubic chunk, including padding on every axis.
///
/// `blocks` is the 0.5 m detail/render/collision grid. `world_blocks` is the
/// authoritative 1 m terrain grid. The latter is expanded into detail cells on
/// generation, while player construction only edits `blocks`.
///
/// `world_turf` marks world voxels that carry a natural turf surface. Turf is a
/// property of the 1 m topsoil voxel rather than a separate detail object, but
/// it is rendered as a raised 0.1 m layer above the voxel's top plane, so
/// natural sticks and pebbles rest in the detail cell directly above it.
/// `world_snow` uses the same raised-surface model for thin alpine snow on rock.
#[derive(Clone)]
pub struct ChunkData {
    pub coord: IVec3,
    pub blocks: Vec<Voxel>,
    pub world_blocks: Vec<Block>,
    pub world_turf: Vec<bool>,
    pub world_snow: Vec<bool>,
}

impl ChunkData {
    pub fn new(coord: IVec3) -> Self {
        Self {
            coord,
            blocks: vec![Voxel::AIR; PADDED_VOLUME],
            world_blocks: vec![Block::Air; WORLD_PADDED_VOLUME],
            world_turf: vec![false; WORLD_PADDED_VOLUME],
            world_snow: vec![false; WORLD_PADDED_VOLUME],
        }
    }

    #[inline]
    pub fn get_padded(&self, px: usize, py: usize, pz: usize) -> Voxel {
        if px >= PADDED_X || py >= PADDED_Y || pz >= PADDED_Z {
            return Voxel::AIR;
        }
        self.blocks[padded_index(px, py, pz)]
    }

    #[inline]
    pub fn set_padded(&mut self, px: usize, py: usize, pz: usize, voxel: impl Into<Voxel>) {
        if px < PADDED_X && py < PADDED_Y && pz < PADDED_Z {
            let i = padded_index(px, py, pz);
            self.blocks[i] = voxel.into();
        }
    }

    #[inline]
    pub fn get_world_padded(&self, px: usize, py: usize, pz: usize) -> Option<Block> {
        if px >= WORLD_PADDED_X || py >= WORLD_PADDED_Y || pz >= WORLD_PADDED_Z {
            return None;
        }
        Some(self.world_blocks[world_padded_index(px, py, pz)])
    }

    #[inline]
    pub fn set_world_padded(&mut self, px: usize, py: usize, pz: usize, block: Block) {
        if px < WORLD_PADDED_X && py < WORLD_PADDED_Y && pz < WORLD_PADDED_Z {
            let i = world_padded_index(px, py, pz);
            self.world_blocks[i] = block;
        }
    }

    #[inline]
    pub fn get_world_turf_padded(&self, px: usize, py: usize, pz: usize) -> bool {
        if px >= WORLD_PADDED_X || py >= WORLD_PADDED_Y || pz >= WORLD_PADDED_Z {
            return false;
        }
        self.world_turf[world_padded_index(px, py, pz)]
    }

    #[inline]
    pub fn set_world_turf_padded(&mut self, px: usize, py: usize, pz: usize, turf: bool) {
        if px < WORLD_PADDED_X && py < WORLD_PADDED_Y && pz < WORLD_PADDED_Z {
            let i = world_padded_index(px, py, pz);
            self.world_turf[i] = turf;
        }
    }

    #[inline]
    pub fn get_world_snow_padded(&self, px: usize, py: usize, pz: usize) -> bool {
        if px >= WORLD_PADDED_X || py >= WORLD_PADDED_Y || pz >= WORLD_PADDED_Z {
            return false;
        }
        self.world_snow[world_padded_index(px, py, pz)]
    }

    #[inline]
    pub fn set_world_snow_padded(&mut self, px: usize, py: usize, pz: usize, snow: bool) {
        if px < WORLD_PADDED_X && py < WORLD_PADDED_Y && pz < WORLD_PADDED_Z {
            let i = world_padded_index(px, py, pz);
            self.world_snow[i] = snow;
        }
    }

    /// World-layer lookup using interior chunk-local world coordinates.
    #[inline]
    pub fn get_world_local(&self, x: usize, y: usize, z: usize) -> Block {
        self.get_world_padded(x + WORLD_PAD, y + WORLD_PAD, z + WORLD_PAD)
            .unwrap_or(Block::Air)
    }

    /// Turf lookup using interior chunk-local world coordinates.
    #[inline]
    pub fn get_world_turf_local(&self, x: usize, y: usize, z: usize) -> bool {
        self.get_world_turf_padded(x + WORLD_PAD, y + WORLD_PAD, z + WORLD_PAD)
    }

    /// Snow lookup using interior chunk-local world coordinates.
    #[inline]
    pub fn get_world_snow_local(&self, x: usize, y: usize, z: usize) -> bool {
        self.get_world_snow_padded(x + WORLD_PAD, y + WORLD_PAD, z + WORLD_PAD)
    }

    #[cfg(test)]
    #[inline]
    pub fn set_world_local(&mut self, x: usize, y: usize, z: usize, block: Block) {
        self.set_world_padded(x + WORLD_PAD, y + WORLD_PAD, z + WORLD_PAD, block);
    }

    #[cfg(test)]
    #[inline]
    pub fn set_world_turf_local(&mut self, x: usize, y: usize, z: usize, turf: bool) {
        self.set_world_turf_padded(x + WORLD_PAD, y + WORLD_PAD, z + WORLD_PAD, turf);
    }

    #[cfg(test)]
    #[inline]
    pub fn set_world_snow_local(&mut self, x: usize, y: usize, z: usize, snow: bool) {
        self.set_world_snow_padded(x + WORLD_PAD, y + WORLD_PAD, z + WORLD_PAD, snow);
    }

    /// Sample the authoritative 1 m world layer with chunk-local coordinates
    /// that may be -1 or `WORLD_CHUNK_*` (the padding border). Global Y bounds
    /// decide the outside-world result: below the bottom is bedrock, above the
    /// top is air.
    #[inline]
    pub fn sample_world_local(&self, x: i32, y: i32, z: i32) -> Block {
        let global_y = self.coord.y * WORLD_CHUNK_Y as i32 + y;
        if global_y < WORLD_Y_MIN {
            return Block::Bedrock;
        }
        if global_y >= WORLD_Y_MAX {
            return Block::Air;
        }
        if x < -(WORLD_PAD as i32)
            || x > WORLD_CHUNK_X as i32
            || z < -(WORLD_PAD as i32)
            || z > WORLD_CHUNK_Z as i32
        {
            return Block::Air;
        }
        self.get_world_padded(
            (x + WORLD_PAD as i32) as usize,
            (y + WORLD_PAD as i32) as usize,
            (z + WORLD_PAD as i32) as usize,
        )
        .unwrap_or(Block::Air)
    }

    /// Sample the natural-turf flag with the same padded coordinates as
    /// [`Self::sample_world_local`].
    #[inline]
    pub fn sample_world_turf_local(&self, x: i32, y: i32, z: i32) -> bool {
        let global_y = self.coord.y * WORLD_CHUNK_Y as i32 + y;
        if !world_y_in_range(global_y) {
            return false;
        }
        if x < -(WORLD_PAD as i32)
            || x > WORLD_CHUNK_X as i32
            || z < -(WORLD_PAD as i32)
            || z > WORLD_CHUNK_Z as i32
        {
            return false;
        }
        self.get_world_turf_padded(
            (x + WORLD_PAD as i32) as usize,
            (y + WORLD_PAD as i32) as usize,
            (z + WORLD_PAD as i32) as usize,
        )
    }

    /// Sample the natural-snow flag with the same padded coordinates as
    /// [`Self::sample_world_local`].
    #[inline]
    pub fn sample_world_snow_local(&self, x: i32, y: i32, z: i32) -> bool {
        let global_y = self.coord.y * WORLD_CHUNK_Y as i32 + y;
        if !world_y_in_range(global_y) {
            return false;
        }
        if x < -(WORLD_PAD as i32)
            || x > WORLD_CHUNK_X as i32
            || z < -(WORLD_PAD as i32)
            || z > WORLD_CHUNK_Z as i32
        {
            return false;
        }
        self.get_world_snow_padded(
            (x + WORLD_PAD as i32) as usize,
            (y + WORLD_PAD as i32) as usize,
            (z + WORLD_PAD as i32) as usize,
        )
    }

    /// True when a detail cell is still the untouched expansion of its 1 m
    /// world voxel. Those cells are rendered from the world layer, so the
    /// detail mesher skips them instead of drawing 0.5 m terrain cubes.
    #[inline]
    pub fn is_untouched_world_cell_local(&self, x: i32, y: i32, z: i32) -> bool {
        let voxel = self.sample_local(x, y, z);
        if voxel.is_air() || !voxel.is_full_cube() {
            return false;
        }
        let world = self.sample_world_local(
            x.div_euclid(DETAILS_PER_WORLD_AXIS),
            y.div_euclid(DETAILS_PER_WORLD_AXIS),
            z.div_euclid(DETAILS_PER_WORLD_AXIS),
        );
        voxel.block() == world
    }

    /// True when a detail cell contains the raised natural-turf layer of the
    /// world voxel directly below it.
    #[inline]
    pub fn is_turf_surface_local(&self, x: i32, y: i32, z: i32) -> bool {
        if y.rem_euclid(DETAILS_PER_WORLD_AXIS) != 0 {
            return false;
        }

        let scale = DETAILS_PER_WORLD_AXIS;
        let wx = x.div_euclid(scale);
        let wz = z.div_euclid(scale);
        let wy = y.div_euclid(scale) - 1;
        if !self.sample_world_turf_local(wx, wy, wz) {
            return false;
        }

        let soil = self.sample_world_local(wx, wy, wz);
        if !soil.is_solid() || soil.is_water() {
            return false;
        }

        !self
            .sample_world_local(
                x.div_euclid(scale),
                y.div_euclid(scale),
                z.div_euclid(scale),
            )
            .is_opaque()
    }

    /// True when a detail cell contains the raised natural-snow layer of the
    /// world voxel directly below it.
    #[inline]
    pub fn is_snow_surface_local(&self, x: i32, y: i32, z: i32) -> bool {
        if y.rem_euclid(DETAILS_PER_WORLD_AXIS) != 0 {
            return false;
        }

        let scale = DETAILS_PER_WORLD_AXIS;
        let wx = x.div_euclid(scale);
        let wz = z.div_euclid(scale);
        let wy = y.div_euclid(scale) - 1;
        if !self.sample_world_snow_local(wx, wy, wz) {
            return false;
        }

        let rock = self.sample_world_local(wx, wy, wz);
        if !rock.is_solid() || rock.is_water() {
            return false;
        }

        !self
            .sample_world_local(
                x.div_euclid(scale),
                y.div_euclid(scale),
                z.div_euclid(scale),
            )
            .is_opaque()
    }

    #[inline]
    pub fn set_world_surface_padded(
        &mut self,
        layer: SurfaceLayer,
        px: usize,
        py: usize,
        pz: usize,
        present: bool,
    ) {
        match layer {
            SurfaceLayer::Turf => self.set_world_turf_padded(px, py, pz, present),
            SurfaceLayer::Snow => self.set_world_snow_padded(px, py, pz, present),
        }
    }

    #[inline]
    pub fn get_world_surface_local(
        &self,
        layer: SurfaceLayer,
        x: usize,
        y: usize,
        z: usize,
    ) -> bool {
        match layer {
            SurfaceLayer::Turf => self.get_world_turf_local(x, y, z),
            SurfaceLayer::Snow => self.get_world_snow_local(x, y, z),
        }
    }

    #[inline]
    pub fn sample_world_surface_local(&self, layer: SurfaceLayer, x: i32, y: i32, z: i32) -> bool {
        match layer {
            SurfaceLayer::Turf => self.sample_world_turf_local(x, y, z),
            SurfaceLayer::Snow => self.sample_world_snow_local(x, y, z),
        }
    }

    #[inline]
    pub fn is_surface_layer_cell(&self, layer: SurfaceLayer, x: i32, y: i32, z: i32) -> bool {
        match layer {
            SurfaceLayer::Turf => self.is_turf_surface_local(x, y, z),
            SurfaceLayer::Snow => self.is_snow_surface_local(x, y, z),
        }
    }

    #[inline]
    pub fn get_local(&self, lx: usize, ly: usize, lz: usize) -> Voxel {
        self.get_padded(lx + PAD, ly + PAD, lz + PAD)
    }

    #[inline]
    pub fn set_local(&mut self, lx: usize, ly: usize, lz: usize, voxel: impl Into<Voxel>) {
        self.set_padded(lx + PAD, ly + PAD, lz + PAD, voxel);
    }

    /// Sample a block using chunk-local coordinates that may be -1 or the chunk
    /// size on any axis (the padding). Global Y bounds decide the outside-world
    /// result instead of treating every negative Y as bedrock.
    #[inline]
    pub fn sample_local(&self, x: i32, y: i32, z: i32) -> Voxel {
        let global_y = self.coord.y * CHUNK_Y as i32 + y;
        if global_y < DETAIL_Y_MIN {
            return Voxel::new(Block::Bedrock);
        }
        if global_y >= DETAIL_Y_MAX {
            return Voxel::AIR;
        }
        if x < -(PAD as i32) || x > CHUNK_X as i32 || z < -(PAD as i32) || z > CHUNK_Z as i32 {
            return Voxel::AIR;
        }
        self.get_padded(
            (x + PAD as i32) as usize,
            (y + PAD as i32) as usize,
            (z + PAD as i32) as usize,
        )
    }
}

/// A loaded chunk entity plus its block data and render statistics.
pub struct Chunk {
    pub data: ChunkData,
    pub entity: Entity,
    pub foliage_entity: Option<Entity>,
    pub water_entity: Option<Entity>,
    pub triangles: usize,
    pub foliage_triangles: usize,
    pub water_triangles: usize,
    /// Time remaining for visual fade-in. 0 means fully visible.
    #[allow(dead_code)]
    pub fade_remaining: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_coordinates_divide_euclidically() {
        assert_eq!(world_to_chunk(IVec3::new(0, 0, 0)), IVec3::ZERO);
        assert_eq!(world_to_chunk(IVec3::new(15, 15, 15)), IVec3::ZERO);
        assert_eq!(world_to_chunk(IVec3::new(16, 16, 16)), IVec3::ONE);
        assert_eq!(
            world_to_chunk(IVec3::new(-1, -1, -1)),
            IVec3::new(-1, -1, -1)
        );
        assert_eq!(
            world_to_chunk(IVec3::new(-17, -17, -17)),
            IVec3::new(-2, -2, -2)
        );
        assert_eq!(
            detail_to_chunk(IVec3::new(-1, -1, -1)),
            IVec3::new(-1, -1, -1)
        );
    }

    #[test]
    fn local_coordinates_wrap_correctly() {
        assert_eq!(world_to_local(IVec3::new(-1, -1, -1)), (15, 15, 15));
        assert_eq!(world_to_local(IVec3::new(16, 16, 16)), (0, 0, 0));
        assert_eq!(detail_to_local(IVec3::new(-1, -1, -1)), (31, 31, 31));
        assert_eq!(detail_to_local(IVec3::new(32, 32, 32)), (0, 0, 0));
    }

    #[test]
    fn padded_roundtrip_covers_all_axes() {
        let coord = IVec3::new(2, -3, -4);
        let mut data = ChunkData::new(coord);
        data.set_local(0, 0, 0, Block::Turf);
        data.set_local(31, 31, 31, Block::Granite);
        assert_eq!(data.get_local(0, 0, 0), Block::Turf);
        assert_eq!(data.get_local(31, 31, 31), Block::Granite);
        assert_eq!(data.sample_local(-1, 0, 0), Block::Air);
        assert_eq!(data.sample_local(0, -1, 0), Block::Air);
        assert_eq!(data.sample_local(0, CHUNK_Y as i32, 0), Block::Air);
        assert_eq!(chunk_origin(coord), Vec3::new(64.0, -96.0, -128.0));
        assert_eq!(chunk_world_origin(coord), IVec3::new(32, -48, -64));
    }

    #[test]
    fn world_voxel_center_uses_detail_engine_units() {
        assert_eq!(
            world_voxel_center(IVec3::new(8, 0, 8)),
            Vec3::new(9.0, 1.0, 9.0)
        );
        assert_eq!(
            world_voxel_center(IVec3::new(-1, -1, -1)),
            Vec3::new(-1.0, -1.0, -1.0)
        );
    }

    #[test]
    fn world_uses_eight_kilometres_of_y_with_sea_at_zero() {
        assert_eq!(SEA_LEVEL_WORLD, 0);
        assert_eq!(WORLD_Y_MIN, -4000);
        assert_eq!(WORLD_Y_MAX, 4000);
        assert_eq!(DETAIL_Y_MIN, -8000);
        assert_eq!(DETAIL_Y_MAX, 8000);
        assert!(world_y_in_range(-1));
        assert!(world_y_in_range(0));
        assert!(!world_y_in_range(-4001));
        assert!(!world_y_in_range(4000));
    }

    #[test]
    fn chunk_is_a_sixteen_metre_cube() {
        assert_eq!(CHUNK_X as f32 * BLOCK_SIZE, 16.0);
        assert_eq!(CHUNK_Y as f32 * BLOCK_SIZE, 16.0);
        assert_eq!(CHUNK_Z as f32 * BLOCK_SIZE, 16.0);
        assert_eq!(SEA_LEVEL_DETAIL, 0);
    }

    #[test]
    fn below_world_bottom_is_bedrock_not_air() {
        let mut data = ChunkData::new(IVec3::new(0, -250, 0));
        data.set_world_padded(1, 1, 1, Block::Bedrock);
        data.set_padded(1, 1, 1, Voxel::new(Block::Bedrock));
        assert_eq!(data.sample_world_local(0, 0, 0), Block::Bedrock);
        assert_eq!(data.sample_local(0, 0, 0), Voxel::new(Block::Bedrock));

        // One step outside the generated bounds uses the global fallback even
        // though this empty chunk has no stored block there.
        assert_eq!(data.sample_world_local(0, -1, 0), Block::Bedrock);
        assert_eq!(data.sample_local(0, -1, 0), Voxel::new(Block::Bedrock));

        data.set_world_padded(1, 1, 1, Block::Granite);
        assert_eq!(data.sample_world_local(0, 0, 0), Block::Granite);
    }
}
