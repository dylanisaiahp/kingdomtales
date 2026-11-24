//! Greedy voxel meshing with baked, per-vertex ambient occlusion.
//!
//! For each of the six face directions we build a 2D mask of visible faces, then
//! merge adjacent faces that share the same block type and a flat AO pattern. A
//! flat 16x16 patch therefore becomes a single quad instead of 256 quads, while
//! faces next to an occluder keep their one-block AO gradient intact.
//!
//! AO is sampled at world-space lattice corners, so every face that touches a
//! corner sees the same value. The padded chunk border supplies neighbouring
//! voxels at chunk seams on every axis (X, Y and Z), keeping AO continuous
//! across chunk boundaries.
//!
//! Two meshes are produced per chunk: an opaque mesh and a translucent water
//! mesh. Only exposed faces are emitted, and back-face culling is enabled on the
//! material, so hidden geometry never reaches the GPU.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexFormat};
use bevy::prelude::*;

use super::block::{
    Block, Orientation, PANEL_HEIGHT, Shape, Voxel, centered_detail_bounds, srgb_to_linear,
    stem_bounds_for_shape,
};
use super::chunk::{
    CHUNK_X, CHUNK_Y, CHUNK_Z, ChunkData, DETAILS_PER_WORLD_AXIS, SurfaceLayer, WORLD_CHUNK_X,
    WORLD_CHUNK_Y, WORLD_CHUNK_Z,
};

#[cfg(test)]
use super::chunk::{PADDED_X, PADDED_Y, PADDED_Z, WORLD_PADDED_X, WORLD_PADDED_Y, WORLD_PADDED_Z};

/// Detail grid meshed by this module: a 32x32x32 cubic chunk.
const DIMS: [usize; 3] = [CHUNK_X, CHUNK_Y, CHUNK_Z];
/// World grid meshed by this module: a 16x16x16 cubic chunk.
const WORLD_DIMS: [usize; 3] = [WORLD_CHUNK_X, WORLD_CHUNK_Y, WORLD_CHUNK_Z];
/// One world voxel spans this many detail cells, so world geometry is scaled
/// up by the same factor. World mesh coordinates are local 1 m cells scaled
/// into the engine's detail-unit space (0..16 world cells -> 0..32 units).
const WORLD_SCALE: f32 = DETAILS_PER_WORLD_AXIS as f32;

#[derive(Clone, Copy)]
struct FaceDir {
    axis: usize,
    sign: i32,
    u_axis: usize,
    v_axis: usize,
}

// For every direction, u x v == normal, which keeps the winding correct.
const FACE_DIRS: [FaceDir; 6] = [
    FaceDir {
        axis: 0,
        sign: 1,
        u_axis: 1,
        v_axis: 2,
    }, // +X
    FaceDir {
        axis: 0,
        sign: -1,
        u_axis: 2,
        v_axis: 1,
    }, // -X
    FaceDir {
        axis: 1,
        sign: 1,
        u_axis: 2,
        v_axis: 0,
    }, // +Y
    FaceDir {
        axis: 1,
        sign: -1,
        u_axis: 0,
        v_axis: 2,
    }, // -Y
    FaceDir {
        axis: 2,
        sign: 1,
        u_axis: 0,
        v_axis: 1,
    }, // +Z
    FaceDir {
        axis: 2,
        sign: -1,
        u_axis: 1,
        v_axis: 0,
    }, // -Z
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct FaceKey {
    block: Block,
    /// Solid-voxel count (0..=8) at each of the four face corners, in
    /// `(0,0), (1,0), (1,1), (0,1)` order.
    ao: [u8; 4],
}

impl FaceKey {
    /// AO can only be baked into a single merged quad when all four corners
    /// agree. Gradients must stay on individual faces or they stretch.
    #[inline]
    fn has_flat_ao(&self) -> bool {
        self.ao[0] == self.ao[1] && self.ao[1] == self.ao[2] && self.ao[2] == self.ao[3]
    }
}

/// How strongly enclosed corners darken. Kept gentle so caves stay readable.
const AO_STRENGTH: f32 = 0.38;

#[inline]
fn axis_unit(axis: usize) -> Vec3 {
    match axis {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

/// Map a corner's solid-neighbour count to a brightness factor.
///
/// Four solid neighbours is the baseline for an open, flat surface (four solid
/// voxels on the inside, four open on the exposed side). Convex corners have
/// fewer and are left at full brightness; concave corners have more and darken
/// along a gentle curve.
#[inline]
fn ao_factor(solid_neighbours: u8) -> f32 {
    let occlusion = (solid_neighbours.saturating_sub(4) as f32) / 4.0;
    1.0 - AO_STRENGTH * occlusion.powf(1.35)
}

/// World-layer equivalent of [`corner_occlusion`]. AO is sampled on the 1 m
/// grid so terrain shading lines up with whole world voxels. Samples at local
/// `-1` / `WORLD_CHUNK_*` read the 1 m padding border; global Y bounds decide
/// what lies beyond the generated world.
#[inline]
fn world_corner_occlusion(data: &ChunkData, corner: [i32; 3]) -> u8 {
    let mut solid = 0u8;
    for dy in -1..=0 {
        for dz in -1..=0 {
            for dx in -1..=0 {
                if data
                    .sample_world_local(corner[0] + dx, corner[1] + dy, corner[2] + dz)
                    .is_opaque()
                {
                    solid += 1;
                }
            }
        }
    }
    solid
}

fn compute_world_ao(data: &ChunkData, dir: FaceDir, block: [i32; 3]) -> [u8; 4] {
    let plane = block[dir.axis] + if dir.sign > 0 { 1 } else { 0 };
    let corners = [(0, 0), (1, 0), (1, 1), (0, 1)];

    let mut ao = [4u8; 4];
    for (index, (du, dv)) in corners.into_iter().enumerate() {
        let mut corner = [0i32; 3];
        corner[dir.axis] = plane;
        corner[dir.u_axis] = block[dir.u_axis] + du;
        corner[dir.v_axis] = block[dir.v_axis] + dv;
        ao[index] = world_corner_occlusion(data, corner).max(4);
    }
    ao
}

/// Emits a small centred foliage box for bushes/small plants. Uses the same
/// wind vertex attribute as full foliage.
#[allow(clippy::too_many_arguments)]
fn emit_small_foliage(
    buffers: &mut MeshBuffers,
    origin: Vec3,
    block: Block,
    orientation: Orientation,
    x: i32,
    y: i32,
    z: i32,
) {
    // Slightly larger leaf clump, closer to cell edges so it touches stems.
    let (x0, y0, z0) = (0.1, 0.0, 0.1);
    let (x1, y1, z1) = (0.9, 0.75, 0.9);
    let faces: [([Vec3; 4], Vec3); 6] = [
        (
            [
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y1, z0),
                Vec3::new(x1, y1, z1),
            ],
            Vec3::X,
        ),
        (
            [
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y1, z1),
                Vec3::new(x0, y1, z0),
            ],
            Vec3::NEG_X,
        ),
        (
            [
                Vec3::new(x0, y1, z0),
                Vec3::new(x0, y1, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y0, z0),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y0, z1),
            ],
            Vec3::NEG_Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x0, y1, z1),
            ],
            Vec3::Z,
        ),
        (
            [
                Vec3::new(x1, y0, z0),
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y1, z0),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::NEG_Z,
        ),
    ];
    for (points, normal) in faces {
        push_foliage_face(buffers, origin, points, normal, block, orientation);
    }
    let _ = (x, y, z);
}

/// Choose the quad diagonal that follows the brighter pair of corners.
///
/// Splitting along the bright diagonal keeps the darker corners on their own
/// triangle, which avoids the saddle-shaped shading artefact.
#[inline]
fn diagonal_split(ao: [f32; 4]) -> bool {
    ao[0] + ao[2] >= ao[1] + ao[3]
}

/// Directional shading baked into vertex colours. This keeps faces readable
/// even in ambient light and gives the world a crisp, stylised look.
#[inline]
fn face_tint(dir: FaceDir) -> f32 {
    match (dir.axis, dir.sign) {
        (1, 1) => 1.00,
        (1, -1) => 0.64,
        (0, 1) => 0.88,
        (0, -1) => 0.80,
        (2, 1) => 0.84,
        (2, -1) => 0.76,
        _ => 1.0,
    }
}

/// Shader location for per-vertex foliage wind weight.
pub const WIND_WEIGHT_LOCATION: u32 = 14;

/// Per-vertex wind sway weight for foliage meshes: 0 keeps a vertex anchored,
/// 1 lets it sway with the wind.
pub const ATTRIBUTE_WIND_WEIGHT: MeshVertexAttribute =
    MeshVertexAttribute::new("WindWeight", 988540917, VertexFormat::Float32);

/// Buffers for a single chunk mesh.
#[derive(Default)]
pub struct MeshBuffers {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    /// Per-vertex wind weight. Only populated for foliage meshes.
    pub wind_weights: Vec<f32>,
    pub indices: Vec<u32>,
}

impl MeshBuffers {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    #[inline]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn into_mesh(self) -> Option<Mesh> {
        self.into_mesh_with_wind(false)
    }

    pub fn into_foliage_mesh(self) -> Option<Mesh> {
        self.into_mesh_with_wind(true)
    }

    fn into_mesh_with_wind(self, wind: bool) -> Option<Mesh> {
        if self.is_empty() {
            return None;
        }

        // Chunk meshes are immutable once built, so there is no reason to keep
        // a CPU copy in the main world after they have been uploaded.
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        if wind {
            let weights = if self.wind_weights.is_empty() {
                vec![0.0; mesh.count_vertices()]
            } else {
                self.wind_weights
            };
            mesh.insert_attribute(ATTRIBUTE_WIND_WEIGHT, weights);
        }
        mesh.insert_indices(Indices::U32(self.indices));
        Some(mesh)
    }
}

#[derive(Default)]
pub struct ChunkMeshData {
    pub opaque: MeshBuffers,
    /// Small natural geometry such as leaves. Kept separate so it can skip the
    /// shadow pass without changing its appearance in the main pass.
    pub foliage: MeshBuffers,
    pub water: MeshBuffers,
}

/// Count the solid voxels around a world-space lattice corner.
///
/// A corner is shared by eight voxels. Because the result depends only on the
/// corner position, every face touching that corner gets the same value, so
/// perpendicular faces agree along shared edges and chunk borders stay
/// seamless. The padded chunk data supplies the neighbouring voxels on all
/// three axes: local coordinates `-1` and `CHUNK_*` index the padding layer,
/// and global Y bounds supply bedrock/air beyond the world.
#[inline]
fn corner_occlusion(data: &ChunkData, corner: [i32; 3]) -> u8 {
    let mut solid = 0u8;
    for dy in -1..=0 {
        for dz in -1..=0 {
            for dx in -1..=0 {
                if data
                    .sample_local(corner[0] + dx, corner[1] + dy, corner[2] + dz)
                    .is_opaque()
                {
                    solid += 1;
                }
            }
        }
    }
    solid
}

/// Compute the four corner occlusion counts for one face.
///
/// Corner order matches the quad vertex order used by `emit_quad`:
/// `(0,0), (1,0), (1,1), (0,1)` in the face's u/v plane.
fn compute_ao(data: &ChunkData, dir: FaceDir, block: [i32; 3]) -> [u8; 4] {
    let plane = block[dir.axis] + if dir.sign > 0 { 1 } else { 0 };
    let corners = [(0, 0), (1, 0), (1, 1), (0, 1)];

    let mut ao = [4u8; 4];
    for (index, (du, dv)) in corners.into_iter().enumerate() {
        let mut corner = [0i32; 3];
        corner[dir.axis] = plane;
        corner[dir.u_axis] = block[dir.u_axis] + du;
        corner[dir.v_axis] = block[dir.v_axis] + dv;
        // Counts of four or fewer all map to full brightness (convex corners
        // and open flat surfaces), so normalise them to the baseline. That
        // keeps flat surfaces on a single merge key instead of fragmenting at
        // every edge where the raw count happens to dip below four.
        ao[index] = corner_occlusion(data, corner).max(4);
    }
    ao
}

#[allow(clippy::too_many_arguments)]
fn emit_quad(
    buffers: &mut MeshBuffers,
    dir: FaceDir,
    slice: usize,
    u: usize,
    v: usize,
    width: usize,
    height: usize,
    key: FaceKey,
    scale: f32,
) {
    let mut plane = (slice as f32 + if dir.sign > 0 { 1.0 } else { 0.0 }) * scale;
    // Water sits slightly below the voxel's top edge. Only the upward face is
    // recessed, leaving the side faces full height so the surface reads as a
    // thin gap inside the shoreline rather than a perfect flush plane.
    if key.block.is_water() && dir.axis == 1 && dir.sign > 0 {
        plane -= PANEL_HEIGHT;
    }

    let mut base = [0.0f32; 3];
    base[dir.axis] = plane;
    base[dir.u_axis] = u as f32 * scale;
    base[dir.v_axis] = v as f32 * scale;

    let p0 = Vec3::from(base);
    let tu = axis_unit(dir.u_axis);
    let tv = axis_unit(dir.v_axis);
    let p1 = p0 + tu * width as f32 * scale;
    let p2 = p1 + tv * height as f32 * scale;
    let p3 = p0 + tv * height as f32 * scale;

    let normal = axis_unit(dir.axis) * dir.sign as f32;
    let tint = face_tint(dir);

    let srgb = key.block.srgb();
    let linear = [
        srgb_to_linear(srgb[0]),
        srgb_to_linear(srgb[1]),
        srgb_to_linear(srgb[2]),
    ];

    let base_index = buffers.positions.len() as u32;
    let ao = [
        ao_factor(key.ao[0]),
        ao_factor(key.ao[1]),
        ao_factor(key.ao[2]),
        ao_factor(key.ao[3]),
    ];
    let corners = [(p0, ao[0]), (p1, ao[1]), (p2, ao[2]), (p3, ao[3])];

    for (position, factor) in corners {
        // AO and the directional tint stay independent, then combine.
        let shade = tint * factor;
        buffers.positions.push(position.to_array());
        buffers.normals.push(normal.to_array());
        buffers
            .colors
            .push([linear[0] * shade, linear[1] * shade, linear[2] * shade, 1.0]);
    }

    // Split the quad along the diagonal that follows the brighter corners.
    // This avoids a visible saddle when opposite corners have different AO.
    if diagonal_split(ao) {
        buffers.indices.extend_from_slice(&[
            base_index,
            base_index + 1,
            base_index + 2,
            base_index,
            base_index + 2,
            base_index + 3,
        ]);
    } else {
        buffers.indices.extend_from_slice(&[
            base_index + 1,
            base_index + 2,
            base_index + 3,
            base_index + 1,
            base_index + 3,
            base_index,
        ]);
    }
}

/// Build both meshes for a chunk.
pub fn mesh_chunk(data: &ChunkData) -> ChunkMeshData {
    let mut output = ChunkMeshData::default();

    for dir in FACE_DIRS {
        let dim_u = DIMS[dir.u_axis];
        let dim_v = DIMS[dir.v_axis];
        let dim_slice = DIMS[dir.axis];
        let mut mask: Vec<Option<FaceKey>> = vec![None; dim_u * dim_v];

        for slice in 0..dim_slice {
            mask.fill(None);

            for v in 0..dim_v {
                for u in 0..dim_u {
                    let mut coords = [0i32; 3];
                    coords[dir.axis] = slice as i32;
                    coords[dir.u_axis] = u as i32;
                    coords[dir.v_axis] = v as i32;

                    let voxel = data.sample_local(coords[0], coords[1], coords[2]);
                    if voxel.is_air() || !voxel.is_full_cube() {
                        // Shaped voxels are emitted by `emit_shaped_voxels`
                        // after the greedy cube pass.
                        continue;
                    }
                    if data.is_untouched_world_cell_local(coords[0], coords[1], coords[2]) {
                        // Terrain is rendered from the 1 m world layer below,
                        // not as 0.5 m detail cells.
                        continue;
                    }
                    let block = voxel.block();

                    let mut neighbour = coords;
                    neighbour[dir.axis] += dir.sign;
                    let neighbour = data.sample_local(neighbour[0], neighbour[1], neighbour[2]);

                    let visible = if block.is_water() {
                        // Water only renders against air. Terrain next to water
                        // is rendered by the opaque pass, avoiding coplanar
                        // z-fighting between the two meshes.
                        neighbour.is_air()
                    } else {
                        !neighbour.is_opaque()
                    };
                    if !visible {
                        continue;
                    }

                    let ao = if block.is_water() {
                        // Water has no meaningful AO and should merge freely.
                        [4, 4, 4, 4]
                    } else {
                        compute_ao(data, dir, coords)
                    };

                    mask[u + v * dim_u] = Some(FaceKey { block, ao });
                }
            }

            // Greedy rectangle merging.
            let mut v = 0;
            while v < dim_v {
                let mut u = 0;
                while u < dim_u {
                    let Some(key) = mask[u + v * dim_u] else {
                        u += 1;
                        continue;
                    };

                    // Only faces with a flat AO pattern may merge: a merged
                    // quad carries one value per corner, so merging across an
                    // AO gradient would stretch that gradient over the whole
                    // rectangle. Flat regions still merge fully.
                    let flat = key.has_flat_ao();

                    let mut width = 1;
                    if flat {
                        while u + width < dim_u && mask[u + width + v * dim_u] == Some(key) {
                            width += 1;
                        }
                    }

                    let mut height = 1;
                    if flat {
                        'grow: while v + height < dim_v {
                            for k in 0..width {
                                if mask[u + k + (v + height) * dim_u] != Some(key) {
                                    break 'grow;
                                }
                            }
                            height += 1;
                        }
                    }

                    for dy in 0..height {
                        for dx in 0..width {
                            mask[u + dx + (v + dy) * dim_u] = None;
                        }
                    }

                    let buffers = if key.block.is_water() {
                        &mut output.water
                    } else {
                        &mut output.opaque
                    };
                    emit_quad(buffers, dir, slice, u, v, width, height, key, 1.0);

                    u += width;
                }
                v += 1;
            }
        }
    }

    emit_shaped_voxels(data, &mut output.opaque, &mut output.foliage);
    mesh_world(data, &mut output);
    emit_natural_surface(data, &mut output.opaque, SurfaceLayer::Turf);
    emit_natural_surface(data, &mut output.opaque, SurfaceLayer::Snow);
    output
}

/// Greedy-mesh the authoritative 1 m terrain layer. Terrain cells are skipped
/// by the detail pass, so every terrain face is emitted here at whole-voxel
/// scale with AO sampled on the world grid.
fn mesh_world(data: &ChunkData, output: &mut ChunkMeshData) {
    for dir in FACE_DIRS {
        let dim_u = WORLD_DIMS[dir.u_axis];
        let dim_v = WORLD_DIMS[dir.v_axis];
        let dim_slice = WORLD_DIMS[dir.axis];
        let mut mask: Vec<Option<FaceKey>> = vec![None; dim_u * dim_v];

        for slice in 0..dim_slice {
            mask.fill(None);

            for v in 0..dim_v {
                for u in 0..dim_u {
                    let mut coords = [0i32; 3];
                    coords[dir.axis] = slice as i32;
                    coords[dir.u_axis] = u as i32;
                    coords[dir.v_axis] = v as i32;

                    let block = data.sample_world_local(coords[0], coords[1], coords[2]);
                    if block.is_air() {
                        continue;
                    }

                    let mut neighbour = coords;
                    neighbour[dir.axis] += dir.sign;
                    let neighbour =
                        data.sample_world_local(neighbour[0], neighbour[1], neighbour[2]);

                    let visible = if block.is_water() {
                        neighbour.is_air()
                    } else {
                        !neighbour.is_opaque()
                    };
                    if !visible {
                        continue;
                    }

                    let ao = if block.is_water() {
                        [4, 4, 4, 4]
                    } else {
                        compute_world_ao(data, dir, coords)
                    };

                    // Natural turf and snow are raised 0.1 m layers above this
                    // voxel, so its own top face is hidden underneath.
                    // `emit_natural_surface` draws the visible plane and lips.
                    if dir.axis == 1
                        && dir.sign > 0
                        && SurfaceLayer::ALL.iter().any(|layer| {
                            data.sample_world_surface_local(*layer, coords[0], coords[1], coords[2])
                        })
                    {
                        continue;
                    }

                    mask[u + v * dim_u] = Some(FaceKey { block, ao });
                }
            }

            let mut v = 0;
            while v < dim_v {
                let mut u = 0;
                while u < dim_u {
                    let Some(key) = mask[u + v * dim_u] else {
                        u += 1;
                        continue;
                    };

                    let flat = key.has_flat_ao();

                    let mut width = 1;
                    if flat {
                        while u + width < dim_u && mask[u + width + v * dim_u] == Some(key) {
                            width += 1;
                        }
                    }

                    let mut height = 1;
                    if flat {
                        'grow: while v + height < dim_v {
                            for k in 0..width {
                                if mask[u + k + (v + height) * dim_u] != Some(key) {
                                    break 'grow;
                                }
                            }
                            height += 1;
                        }
                    }

                    for dy in 0..height {
                        for dx in 0..width {
                            mask[u + dx + (v + dy) * dim_u] = None;
                        }
                    }

                    let buffers = if key.block.is_water() {
                        &mut output.water
                    } else {
                        &mut output.opaque
                    };
                    emit_quad(buffers, dir, slice, u, v, width, height, key, WORLD_SCALE);

                    u += width;
                }
                v += 1;
            }
        }
    }
}

/// Emit the raised natural layer above each flagged world voxel.
///
/// The layer is one 1 m x 0.1 m x 1 m slab whose bottom sits exactly on the
/// world voxel's top plane. [`mesh_world`] skips the hidden top face; this pass
/// draws the visible top plane and the four thin side lips. A lip is culled when
/// the neighbouring column has the same layer (the shared face is internal) or
/// when a solid world voxel above the neighbour already hides it.
fn emit_natural_surface(data: &ChunkData, buffers: &mut MeshBuffers, layer: SurfaceLayer) {
    let scale = WORLD_SCALE;
    let surface_block = layer.block();
    for y in 0..WORLD_CHUNK_Y {
        for z in 0..WORLD_CHUNK_Z {
            for x in 0..WORLD_CHUNK_X {
                if !data.get_world_surface_local(layer, x, y, z) {
                    continue;
                }

                let wx = x as i32;
                let wy = y as i32;
                let wz = z as i32;

                // A solid world voxel directly above would bury the layer.
                if data.sample_world_local(wx, wy + 1, wz).is_opaque() {
                    continue;
                }

                let bottom = (y as f32 + 1.0) * scale;
                let top = bottom + PANEL_HEIGHT;
                let x0 = x as f32 * scale;
                let x1 = x0 + scale;
                let z0 = z as f32 * scale;
                let z1 = z0 + scale;

                // One top plane spanning the whole 1 m column. This is the
                // surface the player sees and stands on, not a 0.5 m panel.
                push_face(
                    buffers,
                    Vec3::ZERO,
                    [
                        Vec3::new(x0, top, z0),
                        Vec3::new(x0, top, z1),
                        Vec3::new(x1, top, z1),
                        Vec3::new(x1, top, z0),
                    ],
                    Vec3::Y,
                    surface_block,
                    Orientation::default(),
                );

                let hidden = |nx: i32, nz: i32| {
                    data.sample_world_surface_local(layer, nx, wy, nz)
                        || data.sample_world_local(nx, wy + 1, nz).is_opaque()
                };

                if !hidden(wx + 1, wz) {
                    let px = x1;
                    push_face(
                        buffers,
                        Vec3::ZERO,
                        [
                            Vec3::new(px, bottom, z1),
                            Vec3::new(px, bottom, z0),
                            Vec3::new(px, top, z0),
                            Vec3::new(px, top, z1),
                        ],
                        Vec3::X,
                        surface_block,
                        Orientation::default(),
                    );
                }
                if !hidden(wx - 1, wz) {
                    let px = x0;
                    push_face(
                        buffers,
                        Vec3::ZERO,
                        [
                            Vec3::new(px, bottom, z0),
                            Vec3::new(px, bottom, z1),
                            Vec3::new(px, top, z1),
                            Vec3::new(px, top, z0),
                        ],
                        Vec3::NEG_X,
                        surface_block,
                        Orientation::default(),
                    );
                }
                if !hidden(wx, wz + 1) {
                    let pz = z1;
                    push_face(
                        buffers,
                        Vec3::ZERO,
                        [
                            Vec3::new(x0, bottom, pz),
                            Vec3::new(x1, bottom, pz),
                            Vec3::new(x1, top, pz),
                            Vec3::new(x0, top, pz),
                        ],
                        Vec3::Z,
                        surface_block,
                        Orientation::default(),
                    );
                }
                if !hidden(wx, wz - 1) {
                    let pz = z0;
                    push_face(
                        buffers,
                        Vec3::ZERO,
                        [
                            Vec3::new(x1, bottom, pz),
                            Vec3::new(x0, bottom, pz),
                            Vec3::new(x0, top, pz),
                            Vec3::new(x1, top, pz),
                        ],
                        Vec3::NEG_Z,
                        surface_block,
                        Orientation::default(),
                    );
                }
            }
        }
    }
}

/// Emit half blocks, slopes and stairs. These pieces are not greedy-meshed:
/// their custom geometry is small, and keeping it separate leaves the cube
/// path simple and fast.
fn emit_shaped_voxels(data: &ChunkData, buffers: &mut MeshBuffers, foliage: &mut MeshBuffers) {
    for y in 0..CHUNK_Y {
        for z in 0..CHUNK_Z {
            for x in 0..CHUNK_X {
                let voxel = data.get_local(x, y, z);
                if !voxel.is_solid() || voxel.is_water() || voxel.is_full_cube() {
                    continue;
                }
                // Loose ground clutter rests on the raised 0.1 m natural surface
                // layer. Structural stems (bush cores, branches, roots) stay at
                // ground level so they connect to trunks and terrain.
                let lift = if voxel.shape().is_natural()
                    && !voxel.shape().is_wood_stem()
                    && !voxel.shape().is_wood_trunk()
                    && SurfaceLayer::ALL.iter().any(|layer| {
                        data.is_surface_layer_cell(*layer, x as i32, y as i32, z as i32)
                    }) {
                    PANEL_HEIGHT
                } else {
                    0.0
                };
                let origin = Vec3::new(x as f32, y as f32 + lift, z as f32);
                emit_shape(
                    buffers, foliage, origin, voxel, data, x as i32, y as i32, z as i32,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_shape(
    buffers: &mut MeshBuffers,
    foliage: &mut MeshBuffers,
    origin: Vec3,
    voxel: Voxel,
    data: &ChunkData,
    x: i32,
    y: i32,
    z: i32,
) {
    let block = voxel.block();
    let orientation = voxel.orientation();

    match voxel.shape() {
        Shape::Cube => {}
        Shape::HalfBlock => {
            emit_box(
                buffers,
                origin,
                Vec3::ZERO,
                Vec3::new(1.0, 0.5, 1.0),
                block,
                orientation,
            );
        }
        Shape::Slope => emit_slope(buffers, origin, block, orientation),
        Shape::Stairs => {
            // Lower full step plus an upper half-depth step at the back.
            emit_box(
                buffers,
                origin,
                Vec3::ZERO,
                Vec3::new(1.0, 0.5, 1.0),
                block,
                orientation,
            );
            emit_box(
                buffers,
                origin,
                Vec3::new(0.0, 0.5, 0.5),
                Vec3::new(1.0, 1.0, 1.0),
                block,
                orientation,
            );
        }
        Shape::Panel => emit_panel(buffers, origin, block, orientation, data, x, y, z),
        // One trunk level is one full 0.5 m detail cube centred in its 1 m
        // world voxel. Roots and branches are separate horizontal Stem cells
        // that touch this cube.
        Shape::Trunk | Shape::DebarkedTrunk => {
            let (min, max) = centered_detail_bounds(voxel.shape());
            emit_box(buffers, origin, min, max, block, Orientation::default());
        }
        // Stems use pre-oriented bounds aligned to the centred trunk cross-section.
        Shape::Stem | Shape::DebarkedStem => {
            let (min, max) = stem_bounds_for_shape(voxel.shape(), orientation);
            emit_box(buffers, origin, min, max, block, Orientation::default());
        }
        // One voxel holds the whole object; its geometry is offset to the centre
        // of the 1 m world voxel. Sticks and pebble piles use orientation to pick
        // a deterministic variant/yaw while staying within that voxel.
        Shape::Pebble => {
            for (min, max) in crate::world::block::pebble_parts(orientation) {
                emit_box(buffers, origin, min, max, block, Orientation::default());
            }
        }
        Shape::StoneDeposit => {
            for (min, max) in crate::world::block::stone_deposit_parts(orientation) {
                emit_box(buffers, origin, min, max, block, Orientation::default());
            }
        }
        Shape::Stick => {
            for (min, max) in crate::world::block::stick_parts(orientation) {
                emit_box(buffers, origin, min, max, block, Orientation::default());
            }
        }
        Shape::Foliage => emit_foliage(data, foliage, origin, block, orientation, x, y, z, 1.0),
        Shape::SmallFoliage => emit_small_foliage(foliage, origin, block, orientation, x, y, z),
    }
}

/// Leaves are one box per detail cell. Faces shared with an adjacent leaf voxel
/// are skipped so neighbouring foliage merges into one continuous canopy instead
/// of drawing hidden interior faces. Wind sway is driven per vertex by the
/// foliage shader; a wind weight is stored alongside each vertex (0 at the cell
/// base, 1 at the top).
#[allow(clippy::too_many_arguments)]
fn emit_foliage(
    data: &ChunkData,
    buffers: &mut MeshBuffers,
    origin: Vec3,
    block: Block,
    orientation: Orientation,
    x: i32,
    y: i32,
    z: i32,
    scale: f32,
) {
    let inset = (1.0 - scale) * 0.5;
    let (x0, y0, z0) = (inset, 0.0, inset);
    let (x1, y1, z1) = (1.0 - inset, scale, 1.0 - inset);

    let faces: [([Vec3; 4], Vec3, IVec3); 6] = [
        (
            [
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y1, z0),
                Vec3::new(x1, y1, z1),
            ],
            Vec3::X,
            IVec3::X,
        ),
        (
            [
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y1, z1),
                Vec3::new(x0, y1, z0),
            ],
            Vec3::NEG_X,
            IVec3::NEG_X,
        ),
        (
            [
                Vec3::new(x0, y1, z0),
                Vec3::new(x0, y1, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::Y,
            IVec3::Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y0, z0),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y0, z1),
            ],
            Vec3::NEG_Y,
            IVec3::NEG_Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x0, y1, z1),
            ],
            Vec3::Z,
            IVec3::Z,
        ),
        (
            [
                Vec3::new(x1, y0, z0),
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y1, z0),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::NEG_Z,
            IVec3::NEG_Z,
        ),
    ];

    for (points, normal, offset) in faces {
        let neighbour = data.sample_local(x + offset.x, y + offset.y, z + offset.z);
        if neighbour.is_solid() && matches!(neighbour.shape(), Shape::Foliage | Shape::SmallFoliage)
        {
            continue;
        }
        push_foliage_face(buffers, origin, points, normal, block, orientation);
    }
}

#[allow(clippy::too_many_arguments)]
fn push_foliage_face(
    buffers: &mut MeshBuffers,
    origin: Vec3,
    points: [Vec3; 4],
    normal: Vec3,
    block: Block,
    orientation: Orientation,
) {
    let transformed = points.map(|point| origin + transform_point(point, orientation));
    let normal = transform_normal(normal, orientation).normalize();
    push_foliage_vertices(buffers, &transformed, normal, block, origin.y);
}

fn push_foliage_vertices(
    buffers: &mut MeshBuffers,
    points: &[Vec3],
    normal: Vec3,
    block: Block,
    base_y: f32,
) {
    let shade = shade_for_normal(normal);
    let srgb = block.srgb();
    let linear = [
        srgb_to_linear(srgb[0]),
        srgb_to_linear(srgb[1]),
        srgb_to_linear(srgb[2]),
    ];

    let base_index = buffers.positions.len() as u32;
    for point in points {
        buffers.positions.push(point.to_array());
        buffers.normals.push(normal.to_array());
        buffers
            .colors
            .push([linear[0] * shade, linear[1] * shade, linear[2] * shade, 1.0]);
        // Sway weight: 0 at the cell base, 1 at the top. The shader uses a coarse
        // phase grid so adjacent cells move together and don't open gaps.
        let weight = ((point[1] - base_y) / 1.0).clamp(0.0, 1.0);
        buffers.wind_weights.push(weight);
    }

    if points.len() == 4 {
        buffers.indices.extend_from_slice(&[
            base_index,
            base_index + 1,
            base_index + 2,
            base_index,
            base_index + 2,
            base_index + 3,
        ]);
    } else {
        buffers
            .indices
            .extend_from_slice(&[base_index, base_index + 1, base_index + 2]);
    }
}

/// A thin mat at the bottom of the cell. The bottom face is omitted: turf sits
/// directly on the soil block below, and drawing both would z-fight. Side faces
/// are also skipped against solid neighbours so neighbouring panels do not
/// produce coplanar flicker.
#[allow(clippy::too_many_arguments)]
fn emit_panel(
    buffers: &mut MeshBuffers,
    origin: Vec3,
    block: Block,
    orientation: Orientation,
    data: &ChunkData,
    x: i32,
    y: i32,
    z: i32,
) {
    let (x0, y0, z0) = (0.0, 0.0, 0.0);
    let (x1, y1, z1) = (1.0, PANEL_HEIGHT, 1.0);

    let faces: [([Vec3; 4], Vec3); 6] = [
        (
            [
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y1, z0),
                Vec3::new(x1, y1, z1),
            ],
            Vec3::X,
        ),
        (
            [
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y1, z1),
                Vec3::new(x0, y1, z0),
            ],
            Vec3::NEG_X,
        ),
        (
            [
                Vec3::new(x0, y1, z0),
                Vec3::new(x0, y1, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y0, z0),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y0, z1),
            ],
            Vec3::NEG_Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x0, y1, z1),
            ],
            Vec3::Z,
        ),
        (
            [
                Vec3::new(x1, y0, z0),
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y1, z0),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::NEG_Z,
        ),
    ];

    for (points, normal) in faces {
        // Cull against the neighbour in the face's rotated direction. For the
        // default floor mat this hides the bottom against the soil; for a mat
        // rotated onto a wall or ceiling it hides the face touching the support.
        let rotated = transform_normal(normal, orientation);
        let offset = [
            rotated.x.round() as i32,
            rotated.y.round() as i32,
            rotated.z.round() as i32,
        ];
        let neighbour = data.sample_local(x + offset[0], y + offset[1], z + offset[2]);
        // Only a *solid* full cube (or another panel sharing this slab)
        // actually covers the whole face. Air voxels default to the cube shape,
        // so checking the shape alone would wrongly cull every exposed face.
        let covers_face = neighbour.is_solid()
            && (neighbour.shape().is_full_cube()
                || (neighbour.shape() == Shape::Panel && neighbour.orientation() == orientation));
        if covers_face {
            continue;
        }
        push_face(buffers, origin, points, normal, block, orientation);
    }
}

/// Axis-aligned box in cell-local coordinates. The box is rotated about the
/// cell centre by the voxel's full 3D orientation.
#[allow(clippy::too_many_arguments)]
fn emit_box(
    buffers: &mut MeshBuffers,
    origin: Vec3,
    min: Vec3,
    max: Vec3,
    block: Block,
    orientation: Orientation,
) {
    let (x0, y0, z0) = (min.x, min.y, min.z);
    let (x1, y1, z1) = (max.x, max.y, max.z);

    let faces: [([Vec3; 4], Vec3); 6] = [
        (
            [
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y1, z0),
                Vec3::new(x1, y1, z1),
            ],
            Vec3::X,
        ),
        (
            [
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y1, z1),
                Vec3::new(x0, y1, z0),
            ],
            Vec3::NEG_X,
        ),
        (
            [
                Vec3::new(x0, y1, z0),
                Vec3::new(x0, y1, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x0, y0, z0),
                Vec3::new(x1, y0, z0),
                Vec3::new(x1, y0, z1),
            ],
            Vec3::NEG_Y,
        ),
        (
            [
                Vec3::new(x0, y0, z1),
                Vec3::new(x1, y0, z1),
                Vec3::new(x1, y1, z1),
                Vec3::new(x0, y1, z1),
            ],
            Vec3::Z,
        ),
        (
            [
                Vec3::new(x1, y0, z0),
                Vec3::new(x0, y0, z0),
                Vec3::new(x0, y1, z0),
                Vec3::new(x1, y1, z0),
            ],
            Vec3::NEG_Z,
        ),
    ];

    for (points, normal) in faces {
        push_face(buffers, origin, points, normal, block, orientation);
    }
}

/// Canonical slope: low edge at -Z, high edge at +Z. The orientation rotates
/// the whole wedge around the cell centre.
fn emit_slope(buffers: &mut MeshBuffers, origin: Vec3, block: Block, orientation: Orientation) {
    let a = Vec3::new(0.0, 0.0, 0.0);
    let b = Vec3::new(1.0, 0.0, 0.0);
    let c = Vec3::new(1.0, 0.0, 1.0);
    let d = Vec3::new(0.0, 0.0, 1.0);
    let e = Vec3::new(0.0, 1.0, 1.0);
    let f = Vec3::new(1.0, 1.0, 1.0);

    push_face(
        buffers,
        origin,
        [d, a, b, c],
        Vec3::NEG_Y,
        block,
        orientation,
    );
    push_face(buffers, origin, [d, c, f, e], Vec3::Z, block, orientation);
    push_face(
        buffers,
        origin,
        [a, e, f, b],
        Vec3::new(0.0, 1.0, -1.0),
        block,
        orientation,
    );
    push_tri(buffers, origin, [a, d, e], Vec3::NEG_X, block, orientation);
    push_tri(buffers, origin, [b, f, c], Vec3::X, block, orientation);
}

#[allow(clippy::too_many_arguments)]
fn push_face(
    buffers: &mut MeshBuffers,
    origin: Vec3,
    points: [Vec3; 4],
    normal: Vec3,
    block: Block,
    orientation: Orientation,
) {
    let transformed = points.map(|point| origin + transform_point(point, orientation));
    let normal = transform_normal(normal, orientation).normalize();
    push_vertices(buffers, &transformed, normal, block);
}

#[allow(clippy::too_many_arguments)]
fn push_tri(
    buffers: &mut MeshBuffers,
    origin: Vec3,
    points: [Vec3; 3],
    normal: Vec3,
    block: Block,
    orientation: Orientation,
) {
    let transformed = points.map(|point| origin + transform_point(point, orientation));
    let normal = transform_normal(normal, orientation).normalize();
    push_vertices(buffers, &transformed, normal, block);
}

fn push_vertices(buffers: &mut MeshBuffers, points: &[Vec3], normal: Vec3, block: Block) {
    let shade = shade_for_normal(normal);
    let srgb = block.srgb();
    let linear = [
        srgb_to_linear(srgb[0]),
        srgb_to_linear(srgb[1]),
        srgb_to_linear(srgb[2]),
    ];

    let base_index = buffers.positions.len() as u32;
    for point in points {
        buffers.positions.push(point.to_array());
        buffers.normals.push(normal.to_array());
        buffers
            .colors
            .push([linear[0] * shade, linear[1] * shade, linear[2] * shade, 1.0]);
    }

    if points.len() == 4 {
        buffers.indices.extend_from_slice(&[
            base_index,
            base_index + 1,
            base_index + 2,
            base_index,
            base_index + 2,
            base_index + 3,
        ]);
    } else {
        buffers
            .indices
            .extend_from_slice(&[base_index, base_index + 1, base_index + 2]);
    }
}

fn transform_point(point: Vec3, orientation: Orientation) -> Vec3 {
    let centre = Vec3::splat(0.5);
    orientation.quat() * (point - centre) + centre
}

fn transform_normal(normal: Vec3, orientation: Orientation) -> Vec3 {
    orientation.quat() * normal
}

fn shade_for_normal(normal: Vec3) -> f32 {
    if normal.y > 0.7 {
        1.0
    } else if normal.y < -0.7 {
        0.64
    } else if normal.x > 0.7 {
        0.88
    } else if normal.x < -0.7 {
        0.80
    } else if normal.z > 0.7 {
        0.84
    } else if normal.z < -0.7 {
        0.76
    } else {
        0.90
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cubic chunk at the origin (detail Y 0..32, world Y 0..16). Global Y
    /// zero is comfortably inside the generated world, so padding is air.
    fn empty_chunk() -> ChunkData {
        ChunkData::new(IVec3::ZERO)
    }

    #[test]
    fn meshing_grids_are_cubic() {
        assert_eq!(DIMS, [32, 32, 32]);
        assert_eq!(WORLD_DIMS, [16, 16, 16]);
    }

    #[test]
    fn single_block_has_six_faces() {
        let mut data = empty_chunk();
        data.set_local(8, 8, 8, Block::Granite);
        let mesh = mesh_chunk(&data);
        assert_eq!(mesh.opaque.triangle_count(), 12);
        assert_eq!(mesh.opaque.positions.len(), 24);
        assert!(mesh.water.is_empty());
    }

    #[test]
    fn shared_faces_are_culled() {
        let mut data = empty_chunk();
        data.set_local(8, 8, 8, Block::Granite);
        data.set_local(9, 8, 8, Block::Granite);
        let mesh = mesh_chunk(&data);
        // The two touching faces are hidden. Greedy meshing then merges the
        // coplanar top, bottom, front and back faces into single quads, so the
        // pair becomes a 2x1x1 box: six quads (12 triangles).
        assert_eq!(mesh.opaque.triangle_count(), 12);
    }

    #[test]
    fn panel_keeps_its_side_faces_next_to_a_small_object() {
        let mut data = empty_chunk();
        data.set_local(8, 7, 8, Block::Topsoil);
        data.set_local(9, 7, 8, Block::Topsoil);
        data.set_local(8, 8, 8, Voxel::new(Block::Turf).with_shape(Shape::Panel));
        data.set_local(9, 8, 8, Voxel::new(Block::Wood).with_shape(Shape::Stick));

        let mesh = mesh_chunk(&data);
        // The panel's +X face sits at x = 9.0 with y from 8.0 to 8.2. A
        // stick occupies the neighbouring cell but only covers part of that
        // face, so the turf side must still be emitted.
        let has_panel_side = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            (position[0] - 9.0).abs() < 1e-4
                && (position[1] - 8.0).abs() < 1e-4
                && position[2] >= 8.0
                && position[2] <= 9.0
                && normal == [1.0, 0.0, 0.0]
        });
        assert!(has_panel_side, "turf panel side face was culled by a stick");
    }

    #[test]
    fn panel_emits_its_top_face() {
        let mut data = empty_chunk();
        // World voxel (4, 8, 4) expands into detail cells (8..10, 16..18,
        // 8..10); the panels rest on the cell directly above it.
        data.set_world_local(4, 8, 4, Block::Topsoil);
        for dy in 0..2 {
            for dz in 0..2 {
                for dx in 0..2 {
                    data.set_local(8 + dx, 16 + dy, 8 + dz, Block::Topsoil);
                }
            }
        }
        let base = 18;
        for dz in 0..2 {
            for dx in 0..2 {
                data.set_local(
                    8 + dx,
                    base,
                    8 + dz,
                    Voxel::new(Block::Turf).with_shape(Shape::Panel),
                );
            }
        }

        let mesh = mesh_chunk(&data);
        assert_eq!(
            data.get_local(8, base, 8).shape(),
            Shape::Panel,
            "test setup did not place a panel"
        );
        let mut top_positions = Vec::new();
        for index in 0..mesh.opaque.positions.len() {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            if (position[1] - (base as f32 + PANEL_HEIGHT)).abs() < 1e-4 {
                top_positions.push((position, normal));
            }
        }
        assert!(
            !top_positions.is_empty(),
            "panel top plane was not emitted; y positions: {:?}",
            mesh.opaque
                .positions
                .iter()
                .map(|p| p[1])
                .collect::<Vec<_>>()
        );
        assert!(
            top_positions
                .iter()
                .any(|(_, normal)| *normal == [0.0, 1.0, 0.0]),
            "panel top plane has no upward face; normals: {:?}",
            top_positions.iter().map(|(_, n)| *n).collect::<Vec<_>>()
        );
    }

    #[test]
    fn turf_world_voxel_renders_a_raised_top_and_side_band() {
        let mut data = empty_chunk();
        data.set_world_local(4, 8, 4, Block::Topsoil);
        data.set_world_turf_local(4, 8, 4, true);

        let mesh = mesh_chunk(&data);
        // World voxel 8 spans detail cells 16..18; its top plane is at 18.
        let topsoil_top = 18.0;
        let turf_top = topsoil_top + PANEL_HEIGHT;
        let turf_linear = srgb_to_linear(Block::Turf.srgb()[0]);

        let has_turf_top = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            let color = mesh.opaque.colors[index];
            normal == [0.0, 1.0, 0.0]
                && (position[1] - turf_top).abs() < 1e-4
                && position[0] >= 8.0
                && position[0] <= 10.0
                && position[2] >= 8.0
                && position[2] <= 10.0
                && (color[0] - turf_linear).abs() < 1e-4
        });
        assert!(
            has_turf_top,
            "raised turf top face should be turf-coloured above the topsoil"
        );

        // The hidden topsoil top plane at the voxel boundary must not also be
        // drawn underneath the raised slab.
        let has_flush_top = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            normal == [0.0, 1.0, 0.0]
                && (position[1] - topsoil_top).abs() < 1e-4
                && position[0] >= 8.0
                && position[0] <= 10.0
                && position[2] >= 8.0
                && position[2] <= 10.0
        });
        assert!(
            !has_flush_top,
            "the topsoil top face should be hidden beneath the raised turf"
        );

        // The exposed +X side gets a thin turf lip spanning the raised band.
        let has_band = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            normal == [1.0, 0.0, 0.0]
                && (position[0] - 10.0).abs() < 1e-4
                && position[1] >= topsoil_top - 1e-4
                && position[1] <= turf_top + 1e-4
                && position[2] >= 8.0
                && position[2] <= 10.0
        });
        assert!(has_band, "raised turf side band was not emitted");
    }

    #[test]
    fn natural_detail_on_turf_is_lifted_onto_the_raised_layer() {
        let mut data = empty_chunk();
        data.set_world_local(4, 8, 4, Block::Topsoil);
        data.set_world_turf_local(4, 8, 4, true);
        // One centered stick voxel sits above the world voxel at its lower-left
        // detail cell, exactly where generation places it.
        let base = 18;
        data.set_local(
            8,
            base,
            8,
            Voxel::from_oriented_parts(Block::Wood, Shape::Stick, Orientation::from_index(2)),
        );

        let mesh = mesh_chunk(&data);
        let surface = base as f32 + PANEL_HEIGHT;
        let has_lifted_stick = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            position[1] >= surface + 0.01
                && position[1] <= surface + 0.15
                && position[0] >= 8.6
                && position[0] <= 9.4
                && position[2] >= 8.7
                && position[2] <= 9.3
        });
        assert!(
            has_lifted_stick,
            "ground stick should rest on the raised turf surface"
        );
    }

    #[test]
    fn turf_band_is_culled_between_two_turf_voxels() {
        let mut data = empty_chunk();
        for x in [4, 5] {
            data.set_world_local(x, 8, 4, Block::Topsoil);
            data.set_world_turf_local(x, 8, 4, true);
        }

        let mesh = mesh_chunk(&data);
        let top = 18.0;
        let raised = top + PANEL_HEIGHT;
        let has_internal_band = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            // Only an X-facing band on the shared plane would be wrong; the
            // outer Z bands of both voxels legitimately meet at x = 10.
            normal[0].abs() > 0.5
                && position[0] > 9.99
                && position[0] < 10.01
                && position[1] > top - 1e-4
                && position[1] < raised + 1e-4
        });
        assert!(
            !has_internal_band,
            "adjacent turf voxels must not draw a band at their shared face"
        );
    }

    #[test]
    fn bare_topsoil_has_no_turf_band() {
        let mut data = empty_chunk();
        data.set_world_local(4, 8, 4, Block::Topsoil);

        let mesh = mesh_chunk(&data);
        let top = 18.0;
        let has_band = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            normal[1].abs() < 0.5 && position[1] > top + 1e-4 && position[1] < top + PANEL_HEIGHT
        });
        assert!(!has_band, "bare topsoil should not render a turf band");
    }

    #[test]
    fn snow_world_voxel_renders_a_raised_layer_over_intact_rock() {
        let mut data = empty_chunk();
        data.set_world_local(4, 8, 4, Block::Granite);
        data.set_world_snow_local(4, 8, 4, true);

        let mesh = mesh_chunk(&data);
        let rock_top = 18.0;
        let snow_top = rock_top + PANEL_HEIGHT;
        let snow_linear = srgb_to_linear(Block::Snow.srgb()[0]);

        let has_snow_top = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            let color = mesh.opaque.colors[index];
            normal == [0.0, 1.0, 0.0]
                && (position[1] - snow_top).abs() < 1e-4
                && position[0] >= 8.0
                && position[0] <= 10.0
                && position[2] >= 8.0
                && position[2] <= 10.0
                && (color[0] - snow_linear).abs() < 1e-4
        });
        assert!(has_snow_top, "raised snow plane was not emitted");

        let has_flush_top = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            normal == [0.0, 1.0, 0.0]
                && (position[1] - rock_top).abs() < 1e-4
                && position[0] >= 8.0
                && position[0] <= 10.0
                && position[2] >= 8.0
                && position[2] <= 10.0
        });
        assert!(
            !has_flush_top,
            "the rock top face should be hidden beneath the snow layer"
        );

        let has_band = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            normal == [1.0, 0.0, 0.0]
                && (position[0] - 10.0).abs() < 1e-4
                && position[1] >= rock_top - 1e-4
                && position[1] <= snow_top + 1e-4
                && position[2] >= 8.0
                && position[2] <= 10.0
        });
        assert!(has_band, "raised snow side band was not emitted");
    }

    #[test]
    fn buried_snow_layer_is_not_drawn() {
        let mut data = empty_chunk();
        data.set_world_local(4, 8, 4, Block::Granite);
        data.set_world_local(4, 9, 4, Block::Granite);
        data.set_world_snow_local(4, 8, 4, true);

        let mesh = mesh_chunk(&data);
        let rock_top = 18.0;
        let has_band = (0..mesh.opaque.positions.len()).any(|index| {
            let position = mesh.opaque.positions[index];
            let normal = mesh.opaque.normals[index];
            normal[1].abs() < 0.5
                && position[1] > rock_top + 1e-4
                && position[1] < rock_top + PANEL_HEIGHT
        });
        assert!(!has_band, "snow buried under rock must not be drawn");
    }

    #[test]
    fn world_terrain_is_meshed_as_one_metre_blocks() {
        let mut data = empty_chunk();
        // One authoritative world voxel (4, 8, 4) and the eight detail cells
        // (8..10, 16..18, 8..10) it expands into. Only the 1 m cube should
        // reach the mesh, scaled from 0..16 world cells into 0..32 detail units.
        data.set_world_local(4, 8, 4, Block::Granite);
        for dy in 0..2 {
            for dz in 0..2 {
                for dx in 0..2 {
                    data.set_local(8 + dx, 16 + dy, 8 + dz, Block::Granite);
                }
            }
        }

        let mesh = mesh_chunk(&data);
        assert_eq!(mesh.opaque.triangle_count(), 12);

        let min = mesh
            .opaque
            .positions
            .iter()
            .fold(Vec3::splat(f32::INFINITY), |acc, p| {
                acc.min(Vec3::from_array(*p))
            });
        let max = mesh
            .opaque
            .positions
            .iter()
            .fold(Vec3::splat(f32::NEG_INFINITY), |acc, p| {
                acc.max(Vec3::from_array(*p))
            });
        assert!((min.x - 8.0).abs() < 1e-4 && (max.x - 10.0).abs() < 1e-4);
        assert!((min.y - 16.0).abs() < 1e-4 && (max.y - 18.0).abs() < 1e-4);
        assert!((min.z - 8.0).abs() < 1e-4 && (max.z - 10.0).abs() < 1e-4);
    }

    #[test]
    fn detail_objects_above_terrain_are_still_meshed() {
        let mut data = empty_chunk();
        data.set_world_local(4, 8, 4, Block::Granite);
        for dy in 0..2 {
            for dz in 0..2 {
                for dx in 0..2 {
                    data.set_local(8 + dx, 16 + dy, 8 + dz, Block::Granite);
                }
            }
        }
        // A tree trunk sits in the detail grid above the world voxel.
        data.set_local(8, 18, 8, Block::Wood);

        let mesh = mesh_chunk(&data);
        assert!(
            mesh.opaque.triangle_count() > 12,
            "detail object should add geometry on top of the 1 m terrain cube"
        );
    }

    #[test]
    fn flat_terrain_is_greedy_meshed() {
        let mut data = empty_chunk();
        for z in 0..4 {
            for x in 0..4 {
                data.set_local(x, 8, z, Block::Turf);
            }
        }
        let mesh = mesh_chunk(&data);
        // One merged top, one merged bottom, four sides.
        assert_eq!(mesh.opaque.triangle_count(), 12);
    }

    #[test]
    fn full_cube_culls_interior_and_bottom_faces_at_world_bottom() {
        // Local Y = 0 in chunk (0, -250, 0) is global detail Y = -8000, the
        // lowest generated layer. The -1 neighbour is bedrock, not padding air.
        let mut data = ChunkData::new(IVec3::new(0, -250, 0));
        for y in 0..16 {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    data.set_local(x, y, z, Block::Granite);
                }
            }
        }
        let mesh = mesh_chunk(&data);

        let has_bottom = (0..mesh.opaque.positions.len()).any(|i| {
            mesh.opaque.normals[i] == [0.0, -1.0, 0.0] && mesh.opaque.positions[i][1].abs() < 1e-4
        });
        assert!(
            !has_bottom,
            "the bottom face of the world's lowest layer is buried in bedrock"
        );

        let top_vertices = (0..mesh.opaque.positions.len())
            .filter(|&i| {
                mesh.opaque.normals[i] == [0.0, 1.0, 0.0]
                    && (mesh.opaque.positions[i][1] - 16.0).abs() < 1e-4
            })
            .count();
        assert_eq!(top_vertices, 4, "the exposed top merges into one quad");

        let stray_top = (0..mesh.opaque.positions.len()).any(|i| {
            mesh.opaque.normals[i] == [0.0, 1.0, 0.0] && mesh.opaque.positions[i][1] < 16.0 - 1e-4
        });
        assert!(!stray_top, "interior horizontal faces must be culled");

        let stray_side = (0..mesh.opaque.positions.len()).any(|i| {
            let normal = mesh.opaque.normals[i];
            let position = mesh.opaque.positions[i];
            match normal {
                [x, _, _] if x > 0.5 => (position[0] - 32.0).abs() > 1e-4,
                [x, _, _] if x < -0.5 => position[0].abs() > 1e-4,
                [_, _, z] if z > 0.5 => (position[2] - 32.0).abs() > 1e-4,
                [_, _, z] if z < -0.5 => position[2].abs() > 1e-4,
                _ => false,
            }
        });
        assert!(!stray_side, "interior vertical faces must be culled");
    }

    #[test]
    fn chunk_away_from_world_bottom_keeps_its_bottom_face() {
        // Padding air above global detail Y = -8000 is exposed, so a chunk at
        // the origin must render the -Y face of a block at local Y = 0.
        let mut data = empty_chunk();
        data.set_local(8, 0, 8, Block::Granite);
        let mesh = mesh_chunk(&data);
        let has_bottom = (0..mesh.opaque.positions.len()).any(|i| {
            mesh.opaque.normals[i] == [0.0, -1.0, 0.0] && mesh.opaque.positions[i][1].abs() < 1e-4
        });
        assert!(
            has_bottom,
            "a chunk above the world bottom must show its underside"
        );
    }

    #[test]
    fn y_padding_ao_uses_global_world_bounds() {
        let mut at_bottom = ChunkData::new(IVec3::new(0, -250, 0));
        let mut above_bottom = empty_chunk();
        for data in [&mut at_bottom, &mut above_bottom] {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    data.set_local(x, 0, z, Block::Granite);
                }
            }
        }

        // +X face on the bottom row of the floor. Its corners sample local
        // Y = -1: bedrock at the world bottom, air padding at the origin.
        let dir = FACE_DIRS[0];
        let bottom_ao = compute_ao(&at_bottom, dir, [CHUNK_X as i32 - 1, 0, 8]);
        let open_ao = compute_ao(&above_bottom, dir, [CHUNK_X as i32 - 1, 0, 8]);
        assert!(
            bottom_ao.iter().any(|&value| value > 4),
            "world-bottom padding should darken the corner: {bottom_ao:?}"
        );
        assert_eq!(open_ao, [4, 4, 4, 4]);
    }

    #[test]
    fn padding_layers_are_indexed_on_every_axis() {
        let mut data = empty_chunk();

        data.set_padded(0, 9, 9, Voxel::new(Block::Granite));
        assert_eq!(data.sample_local(-1, 8, 8), Voxel::new(Block::Granite));
        data.set_padded(9, PADDED_Y - 1, 9, Voxel::new(Block::Granite));
        assert_eq!(
            data.sample_local(8, CHUNK_Y as i32, 8),
            Voxel::new(Block::Granite)
        );
        data.set_padded(9, 9, 0, Voxel::new(Block::Granite));
        assert_eq!(data.sample_local(8, 8, -1), Voxel::new(Block::Granite));
        data.set_padded(9, 9, PADDED_Z - 1, Voxel::new(Block::Granite));
        assert_eq!(
            data.sample_local(8, 8, CHUNK_Z as i32),
            Voxel::new(Block::Granite)
        );

        data.set_world_padded(0, 9, 0, Block::Granite);
        assert_eq!(data.sample_world_local(-1, 8, -1), Block::Granite);
        data.set_world_padded(WORLD_PADDED_X - 1, 9, 0, Block::Granite);
        assert_eq!(
            data.sample_world_local(WORLD_CHUNK_X as i32, 8, -1),
            Block::Granite
        );
        data.set_world_padded(9, WORLD_PADDED_Y - 1, 0, Block::Granite);
        assert_eq!(
            data.sample_world_local(8, WORLD_CHUNK_Y as i32, -1),
            Block::Granite
        );
        data.set_world_padded(9, 9, WORLD_PADDED_Z - 1, Block::Granite);
        assert_eq!(
            data.sample_world_local(8, 8, WORLD_CHUNK_Z as i32),
            Block::Granite
        );

        // Detail and world geometry must both stay inside the local cube
        // (the raised turf slab may extend slightly past the top plane).
        data.set_local(8, 8, 8, Block::Granite);
        data.set_world_local(4, 12, 4, Block::Topsoil);
        data.set_world_turf_local(4, 12, 4, true);
        let mesh = mesh_chunk(&data);
        assert!(mesh.opaque.positions.iter().all(|p| {
            p.iter()
                .all(|&v| v >= -1e-4 && v <= CHUNK_X as f32 + PANEL_HEIGHT + 1e-4)
        }));
    }

    #[test]
    fn flat_plane_has_uniform_ao() {
        let mut data = empty_chunk();
        for z in 2..6 {
            for x in 2..6 {
                data.set_local(x, 8, z, Block::Turf);
            }
        }

        // Every top face of an open flat surface sits on the same baseline.
        for z in 2..6 {
            for x in 2..6 {
                let ao = compute_ao(&data, FACE_DIRS[2], [x, 8, z]);
                assert_eq!(ao, [4, 4, 4, 4], "unexpected AO at ({x}, 8, {z})");
            }
        }
    }

    #[test]
    fn single_occluder_only_darkens_adjacent_corners() {
        let mut data = empty_chunk();
        for z in 0..8 {
            for x in 0..8 {
                data.set_local(x, 8, z, Block::Granite);
            }
        }
        data.set_local(4, 9, 4, Block::Granite);

        // A block diagonally next to the occluder sees one darker corner.
        let adjacent = compute_ao(&data, FACE_DIRS[2], [3, 8, 3]);
        assert!(
            adjacent.iter().any(|&value| value > 4),
            "adjacent face should be occluded: {adjacent:?}"
        );

        // Two blocks away the surface is back to full brightness.
        let far = compute_ao(&data, FACE_DIRS[2], [1, 8, 1]);
        assert_eq!(far, [4, 4, 4, 4]);
    }

    #[test]
    fn perpendicular_faces_agree_at_a_shared_corner() {
        let mut data = empty_chunk();
        // A floor, with a one-block step beside it. The floor's top face and
        // the step's exposed side face meet at a concave corner.
        for z in 0..12 {
            for x in 0..12 {
                data.set_local(x, 8, z, Block::Granite);
            }
        }
        for z in 0..12 {
            for x in 9..12 {
                data.set_local(x, 9, z, Block::Granite);
            }
        }

        let top = compute_ao(&data, FACE_DIRS[2], [8, 8, 8]); // +Y floor face
        let side = compute_ao(&data, FACE_DIRS[1], [9, 9, 8]); // -X step face

        // World corner (9, 9, 8) is index 3 on the top face and index 0 on -X.
        assert_eq!(top[3], side[0]);
        assert!(top[3] > 4, "shared corner should be occluded");
    }

    #[test]
    fn merged_quad_uses_the_outer_corner_samples() {
        let mut data = empty_chunk();
        for z in 0..4 {
            for x in 0..4 {
                data.set_local(x, 8, z, Block::Turf);
            }
        }
        let mesh = mesh_chunk(&data);

        let top: Vec<_> = (0..mesh.opaque.positions.len())
            .filter(|&i| mesh.opaque.normals[i] == [0.0, 1.0, 0.0])
            .map(|i| (mesh.opaque.positions[i], mesh.opaque.colors[i]))
            .filter(|(position, _)| position[1] == 9.0)
            .collect();

        assert_eq!(top.len(), 4, "top should be one merged quad");
        let expected = top[0].1;
        assert!(top.iter().all(|(_, colour)| *colour == expected));
        assert!(expected[0] > 0.0);

        let xs: Vec<f32> = top.iter().map(|(position, _)| position[0]).collect();
        let zs: Vec<f32> = top.iter().map(|(position, _)| position[2]).collect();
        assert_eq!(xs.iter().filter(|&&x| x == 0.0).count(), 2);
        assert_eq!(xs.iter().filter(|&&x| x == 4.0).count(), 2);
        assert_eq!(zs.iter().filter(|&&z| z == 0.0).count(), 2);
        assert_eq!(zs.iter().filter(|&&z| z == 4.0).count(), 2);
    }

    #[test]
    fn chunk_border_ao_uses_neighbour_padding() {
        let mut border = empty_chunk();
        for z in 0..CHUNK_Z {
            for x in 0..CHUNK_X {
                border.set_local(x, 8, z, Block::Granite);
            }
        }
        // A wall in the padding column just outside the chunk, on top of a
        // continuous floor that also extends into the padding. Padded indices
        // are local coordinates plus one on every axis.
        for z in 0..CHUNK_Z {
            border.set_padded(PADDED_X - 1, 9, z + 1, Block::Granite);
            border.set_padded(PADDED_X - 1, 10, z + 1, Block::Granite);
        }
        let border_ao = compute_ao(&border, FACE_DIRS[2], [CHUNK_X as i32 - 1, 8, 8]);

        // Same geometry with the wall one column inside the chunk.
        let mut inside = empty_chunk();
        for z in 0..CHUNK_Z {
            for x in 0..CHUNK_X {
                inside.set_local(x, 8, z, Block::Granite);
            }
        }
        for z in 0..CHUNK_Z {
            inside.set_local(CHUNK_X - 1, 9, z, Block::Granite);
        }
        let inside_ao = compute_ao(&inside, FACE_DIRS[2], [CHUNK_X as i32 - 2, 8, 8]);

        assert_eq!(border_ao, inside_ao);
        assert!(
            border_ao.iter().any(|&value| value > 4),
            "the padding wall should occlude the border corner"
        );
    }

    #[test]
    fn ao_gradients_do_not_merge() {
        fn top_quads(mesh: &ChunkMeshData) -> usize {
            mesh.opaque
                .normals
                .iter()
                .filter(|normal| **normal == [0.0, 1.0, 0.0])
                .count()
                / 4
        }

        let mut open = empty_chunk();
        for z in 0..8 {
            for x in 0..8 {
                open.set_local(x, 8, z, Block::Granite);
            }
        }
        let open_mesh = mesh_chunk(&open);
        assert_eq!(top_quads(&open_mesh), 1, "open floor should merge fully");

        let mut walled = empty_chunk();
        for z in 0..8 {
            for x in 0..8 {
                walled.set_local(x, 8, z, Block::Granite);
            }
        }
        for z in 0..8 {
            walled.set_local(0, 9, z, Block::Granite);
        }
        let walled_mesh = mesh_chunk(&walled);
        assert!(
            top_quads(&walled_mesh) > 1,
            "the occluded row must not merge with the open rows"
        );
    }

    #[test]
    fn diagonal_follows_the_brighter_corners() {
        assert!(diagonal_split([1.0, 0.5, 1.0, 0.5]));
        assert!(!diagonal_split([0.5, 1.0, 0.5, 1.0]));
    }

    #[test]
    fn water_only_renders_against_air() {
        let mut data = empty_chunk();
        data.set_local(8, 10, 8, Block::Water);
        data.set_local(8, 9, 8, Block::Granite);
        let mesh = mesh_chunk(&data);
        // Five water faces (the bottom is against stone), plus the stone block's
        // six faces because water is not opaque.
        assert_eq!(mesh.water.triangle_count(), 10);
        assert_eq!(mesh.opaque.triangle_count(), 12);

        // The upward water face is recessed by 0.1 m, so it no longer sits on
        // the exact voxel edge.
        let surface_y = 11.0 - PANEL_HEIGHT;
        let top_vertices: Vec<_> = mesh
            .water
            .positions
            .iter()
            .zip(mesh.water.normals.iter())
            .filter(|(_, normal)| normal[1] > 0.5)
            .map(|(position, _)| position)
            .collect();
        assert_eq!(top_vertices.len(), 4);
        assert!(
            top_vertices
                .iter()
                .all(|position| (position[1] - surface_y).abs() < 1e-4),
            "water top should sit at {surface_y}: {top_vertices:?}"
        );
    }

    fn scenario_chunk(kind: &str) -> ChunkData {
        let mut data = empty_chunk();
        match kind {
            "open" => {
                for z in 0..CHUNK_Z {
                    for x in 0..CHUNK_X {
                        data.set_local(x, 8, z, Block::Turf);
                    }
                }
            }
            "corner" => {
                for z in 0..CHUNK_Z {
                    for x in 0..CHUNK_X {
                        data.set_local(x, 8, z, Block::Granite);
                    }
                }
                for y in 9..16 {
                    for x in 0..CHUNK_X {
                        data.set_local(x, y, 0, Block::Granite);
                        data.set_local(0, y, x, Block::Granite);
                    }
                }
            }
            "overhang" => {
                for z in 0..CHUNK_Z {
                    for x in 0..CHUNK_X {
                        data.set_local(x, 8, z, Block::Granite);
                    }
                }
                for z in 0..CHUNK_Z {
                    for x in 0..CHUNK_X {
                        data.set_local(x, 14, z, Block::Granite);
                    }
                }
                for y in 9..14 {
                    data.set_local(0, y, 0, Block::Granite);
                }
            }
            "cave" => {
                for y in 4..24 {
                    for z in 0..CHUNK_Z {
                        for x in 0..CHUNK_X {
                            data.set_local(x, y, z, Block::Granite);
                        }
                    }
                }
                for y in 8..12 {
                    for z in 4..12 {
                        for x in 4..12 {
                            data.set_local(x, y, z, Block::Air);
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
        data
    }

    #[test]
    #[ignore]
    fn report_scenario_quad_counts() {
        for kind in ["open", "corner", "overhang", "cave"] {
            let data = scenario_chunk(kind);
            let mesh = mesh_chunk(&data);
            println!(
                "{kind:>8}: {} opaque triangles",
                mesh.opaque.triangle_count()
            );
        }
    }

    #[test]
    #[ignore]
    fn bench_generated_chunk() {
        use crate::world::generation::WorldGen;
        use std::time::Instant;

        let generator = WorldGen::new(0);
        let data = generator.generate_chunk(IVec3::ZERO);
        let runs = 20;
        let start = Instant::now();
        let mut triangles = 0;
        let mut foliage_triangles = 0;
        let mut water_triangles = 0;
        for _ in 0..runs {
            let mesh = mesh_chunk(&data);
            triangles = mesh.opaque.triangle_count();
            foliage_triangles = mesh.foliage.triangle_count();
            water_triangles = mesh.water.triangle_count();
        }
        let elapsed = start.elapsed() / runs;
        println!(
            "generated chunk: {triangles} opaque + {foliage_triangles} foliage + \
             {water_triangles} water triangles, {elapsed:?} per mesh"
        );
    }

    #[test]
    #[ignore]
    fn bench_generated_chunk_mesh_breakdown() {
        use crate::world::generation::WorldGen;
        use std::time::Instant;

        let generator = WorldGen::new(0);
        let data = generator.generate_chunk(IVec3::ZERO);
        let total = mesh_chunk(&data).opaque.triangle_count();

        let mut terrain_only = data.clone();
        let mut shaped_cells = 0;
        for py in 0..PADDED_Y {
            for pz in 0..PADDED_Z {
                for px in 0..PADDED_X {
                    if !terrain_only.get_padded(px, py, pz).is_full_cube() {
                        terrain_only.set_padded(px, py, pz, Voxel::AIR);
                        shaped_cells += 1;
                    }
                }
            }
        }
        let without_shapes = mesh_chunk(&terrain_only).opaque.triangle_count();

        let start = Instant::now();
        for _ in 0..20 {
            let _ = mesh_chunk(&terrain_only);
        }
        let elapsed = start.elapsed() / 20;

        println!(
            "generated chunk mesh: {total} total opaque triangles, {without_shapes} without \
             {shaped_cells} shaped detail cells; terrain-only mesh {elapsed:?}"
        );
    }
}
