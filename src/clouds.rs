//! Classic voxel-style cloud formations.
//!
//! Each formation is a merged mesh of connected 1m voxel cells. Occupancy is
//! generated on a coarse 3D grid as a broad, thin, rounded footprint with
//! edge-tapered thickness. Only faces exposed to empty space are emitted, and
//! adjacent coplanar exposed faces are greedy-meshed into larger rectangles.
//! Formations sit in a camera-centered world-space grid so the same cloud
//! field surrounds the player, including overhead and behind. No raymarching,
//! no cloud shadows, no per-voxel entities.

use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, Mesh3d, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MeshMaterial3d};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use std::collections::VecDeque;

use crate::day_night::DayNightCycle;
use crate::player::PlayerCamera;
use crate::settings::GameSettings;

const FORMATION_GRID: i32 = 3; // 3x3 cells around camera
const CELL_SIZE: f32 = 190.0;
const CLOUD_BASE_Y: f32 = 145.0;
const WIND_SPEED: Vec3 = Vec3::new(2.0, 0.0, 0.6);

// 1m voxel cells for recognizable Minecraft-style voxel clouds.
const VOXEL: f32 = 1.0;

#[derive(Component)]
struct CloudFormation {
    cell: IVec2,
    drift: Vec3,
}

#[derive(Clone, Copy, Debug, ShaderType)]
struct CloudUniformData {
    /// rgb = day/night-tinted cloud color.
    tint: Vec4,
    /// xyz = sun direction (world), w = sun strength (0..1).
    sun_dir: Vec4,
    /// x = unused, y = bottom brightness, z = top brightness, w = unused.
    params: Vec4,
}

impl Default for CloudUniformData {
    fn default() -> Self {
        Self {
            tint: Vec4::new(0.93, 0.95, 0.98, 1.0),
            sun_dir: Vec4::new(0.0, 1.0, 0.0, 1.0),
            params: Vec4::new(0.0, CLOUD_BOTTOM_BRIGHTNESS, CLOUD_TOP_BRIGHTNESS, 0.0),
        }
    }
}

/// Keep undersides naturally shaded, not dark; night ambient is reduced separately.
const CLOUD_BOTTOM_BRIGHTNESS: f32 = 0.78;
const CLOUD_TOP_BRIGHTNESS: f32 = 1.0;

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone, Default)]
struct CloudMaterial {
    #[uniform(0)]
    data: CloudUniformData,
}

impl Material for CloudMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/cloud.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/cloud.wesl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }

    // Clouds are forward-only alpha-to-coverage with custom depth behavior and
    // no prepass shader variants; exclude them from depth/normal prepasses.
    fn enable_prepass() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        _descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        Ok(())
    }
}

#[derive(Resource)]
struct CloudAssets {
    material: Handle<CloudMaterial>,
}

pub struct CloudsPlugin;

impl Plugin for CloudsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<CloudMaterial>::default())
            .add_systems(Startup, spawn_clouds)
            .add_systems(Update, (update_clouds, update_cloud_material));
    }
}

fn spawn_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    let material = materials.add(CloudMaterial::default());
    commands.insert_resource(CloudAssets {
        material: material.clone(),
    });

    let half = FORMATION_GRID / 2;
    for gz in -half..=half {
        for gx in -half..=half {
            let cell = IVec2::new(gx, gz);
            let seed = cell_seed(cell);
            // Natural gaps.
            if hash_u32(seed ^ 0xA11CE) % 100 < 20 {
                continue;
            }
            let mesh = meshes.add(build_formation_mesh(seed));
            let drift = initial_drift(seed);
            commands.spawn((
                Name::new(format!("CloudFormation_{}_{}", gx, gz)),
                CloudFormation { cell, drift },
                NotShadowCaster,
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                Transform::from_xyz(0.0, -1000.0, 0.0),
                Visibility::Hidden,
            ));
        }
    }
}

fn update_clouds(
    time: Res<Time>,
    settings: Res<GameSettings>,
    camera_q: Query<&Transform, (With<PlayerCamera>, Without<CloudFormation>)>,
    mut q: Query<(&mut CloudFormation, &mut Transform, &mut Visibility)>,
) {
    let Ok(cam) = camera_q.single() else {
        return;
    };
    let dt = time.delta_secs();
    let cam_cell = IVec2::new(
        (cam.translation.x / CELL_SIZE).floor() as i32,
        (cam.translation.z / CELL_SIZE).floor() as i32,
    );
    let half = FORMATION_GRID / 2;
    let extent = FORMATION_GRID as f32 * CELL_SIZE;

    for (mut cloud, mut transform, mut vis) in &mut q {
        if !settings.clouds_enabled {
            *vis = Visibility::Hidden;
            continue;
        }
        *vis = Visibility::Inherited;

        let mut rel = cloud.cell - cam_cell;
        rel.x = wrap_i32(rel.x, -half, half);
        rel.y = wrap_i32(rel.y, -half, half);
        cloud.cell = cam_cell + rel;

        cloud.drift += WIND_SPEED * dt;
        cloud.drift.x = wrap_f32(cloud.drift.x, -extent * 0.5, extent * 0.5);
        cloud.drift.z = wrap_f32(cloud.drift.z, -extent * 0.5, extent * 0.5);

        let seed = cell_seed(cloud.cell);
        let jitter = cell_jitter(seed);
        let base = Vec3::new(
            cloud.cell.x as f32 * CELL_SIZE + jitter.x,
            CLOUD_BASE_Y + jitter.y,
            cloud.cell.y as f32 * CELL_SIZE + jitter.z,
        );
        transform.translation = base + cloud.drift;
    }
}

fn update_cloud_material(
    settings: Res<GameSettings>,
    cycle: Res<DayNightCycle>,
    assets: Option<Res<CloudAssets>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    let Some(assets) = assets else { return };
    let Some(mut mat) = materials.get_mut(&assets.material) else {
        return;
    };

    if !settings.clouds_enabled {
        mat.data.tint = Vec4::new(1.0, 1.0, 1.0, 0.0);
        return;
    }

    let day = cycle.daylight_factor().clamp(0.0, 1.0);
    let night = cycle.night_factor().clamp(0.0, 1.0);
    let twilight = cycle.twilight_factor().clamp(0.0, 1.0);
    let base = Vec3::lerp(
        Vec3::new(0.22, 0.25, 0.35),
        Vec3::new(0.93, 0.95, 0.98),
        day,
    );
    let warm = Vec3::new(1.0, 0.78, 0.58) * (twilight * 0.18);
    let moon = Vec3::new(0.55, 0.66, 0.88) * (night * 0.12);
    let mut c = base + warm + moon;
    c.x = c.x.clamp(0.0, 1.0);
    c.y = c.y.clamp(0.0, 1.0);
    c.z = c.z.clamp(0.0, 1.0);
    mat.data.tint = Vec4::new(c.x, c.y, c.z, 1.0);

    let sun_dir = cycle.sun_direction();
    // Soft sun influence: brighter by day, very weak at night, keeps clouds readable.
    let sun_strength = (day * 1.0 + twilight * 0.35).clamp(0.0, 1.0);
    mat.data.sun_dir = Vec4::new(sun_dir.x, sun_dir.y, sun_dir.z, sun_strength);

    mat.data.params = Vec4::new(0.0, CLOUD_BOTTOM_BRIGHTNESS, CLOUD_TOP_BRIGHTNESS, 0.0);
}

fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

fn rand01(h: u32) -> f32 {
    (h as f32) / (u32::MAX as f32)
}

fn cell_seed(cell: IVec2) -> u32 {
    let x = cell.x as u32;
    let z = cell.y as u32;
    hash_u32(x ^ z.rotate_left(16) ^ 0xC10D_C10D)
}

fn cell_jitter(seed: u32) -> Vec3 {
    let jx = (rand01(hash_u32(seed ^ 1)) - 0.5) * CELL_SIZE * 0.35;
    let jy = (rand01(hash_u32(seed ^ 2)) - 0.5) * 18.0;
    let jz = (rand01(hash_u32(seed ^ 3)) - 0.5) * CELL_SIZE * 0.35;
    Vec3::new(jx, jy, jz)
}

fn initial_drift(seed: u32) -> Vec3 {
    let dx = (rand01(hash_u32(seed ^ 4)) - 0.5) * CELL_SIZE;
    let dz = (rand01(hash_u32(seed ^ 5)) - 0.5) * CELL_SIZE;
    Vec3::new(dx, 0.0, dz)
}

fn wrap_i32(v: i32, min: i32, max: i32) -> i32 {
    let range = max - min;
    if range <= 0 {
        return min;
    }
    let mut r = (v - min) % range;
    if r < 0 {
        r += range;
    }
    r + min
}

fn wrap_f32(v: f32, min: f32, max: f32) -> f32 {
    let range = max - min;
    if range <= 0.0 {
        return min;
    }
    let mut r = (v - min) % range;
    if r < 0.0 {
        r += range;
    }
    r + min
}
// ---------------------------------------------------------------------------
// Voxel occupancy + greedy-meshed outer surface
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct VoxelGrid {
    sx: i32,
    sy: i32,
    sz: i32,
    /// Occupancy in x + z*sx + y*sx*sz order.
    cells: Vec<bool>,
}

impl VoxelGrid {
    fn new(sx: i32, sy: i32, sz: i32) -> Self {
        Self {
            sx,
            sy,
            sz,
            cells: vec![false; (sx * sy * sz).max(0) as usize],
        }
    }

    fn idx(&self, x: i32, y: i32, z: i32) -> usize {
        (x + z * self.sx + y * self.sx * self.sz) as usize
    }

    fn in_bounds(&self, x: i32, y: i32, z: i32) -> bool {
        x >= 0 && x < self.sx && y >= 0 && y < self.sy && z >= 0 && z < self.sz
    }

    fn get(&self, x: i32, y: i32, z: i32) -> bool {
        self.in_bounds(x, y, z) && self.cells[self.idx(x, y, z)]
    }

    fn set(&mut self, x: i32, y: i32, z: i32, v: bool) {
        if self.in_bounds(x, y, z) {
            let i = self.idx(x, y, z);
            self.cells[i] = v;
        }
    }
}

fn build_formation_mesh(seed: u32) -> Mesh {
    let grid = generate_formation_grid(seed);
    let mesh_data = build_greedy_mesh(&grid);

    Mesh::new(PrimitiveTopology::TriangleList, default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, mesh_data.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, mesh_data.normals)
        .with_inserted_indices(Indices::U32(mesh_data.indices))
}

fn generate_formation_grid(seed: u32) -> VoxelGrid {
    // Classic Minecraft-style voxel clouds: broad, flat, connected rectangular
    // masses. Simple, efficient, and recognizably cloud-like.
    let sx = 64 + (hash_u32(seed ^ 0x11) % 33) as i32; // 64-96
    let sz = 32 + (hash_u32(seed ^ 0x22) % 25) as i32; // 32-56
    let thickness = 3 + (hash_u32(seed ^ 0x33) % 2) as i32; // 3-4 voxels

    let mut footprint = vec![false; (sx * sz) as usize];

    // A few large overlapping axis-aligned rectangles form the main cloud body.
    let rect_count = 3 + (hash_u32(seed ^ 0x44) % 3) as i32; // 3-5
    for ri in 0..rect_count {
        let ru = ri as u32;
        let rw =
            (sx as f32 * (0.28 + rand01(hash_u32(seed ^ ru.wrapping_mul(73) ^ 1)) * 0.34)) as i32;
        let rh =
            (sz as f32 * (0.30 + rand01(hash_u32(seed ^ ru.wrapping_mul(73) ^ 2)) * 0.38)) as i32;
        let rw = rw.clamp(10, sx - 4);
        let rh = rh.clamp(8, sz - 4);
        let rx0 = ((sx - rw) as f32 * rand01(hash_u32(seed ^ ru.wrapping_mul(73) ^ 3))) as i32;
        let rz0 = ((sz - rh) as f32 * rand01(hash_u32(seed ^ ru.wrapping_mul(73) ^ 4))) as i32;
        for z in rz0..(rz0 + rh).min(sz) {
            for x in rx0..(rx0 + rw).min(sx) {
                footprint[(x + z * sx) as usize] = true;
            }
        }
    }

    // Add a few smaller rectangular protrusions for classic cloud silhouette.
    let ext_count = 2 + (hash_u32(seed ^ 0x55) % 3) as i32; // 2-4
    for ei in 0..ext_count {
        let eu = ei as u32 + 100;
        let ew = (8 + (hash_u32(seed ^ eu.wrapping_mul(41) ^ 1) % 14) as i32).min(sx / 2);
        let eh = (6 + (hash_u32(seed ^ eu.wrapping_mul(41) ^ 2) % 10) as i32).min(sz / 2);
        let ex0 =
            (rand01(hash_u32(seed ^ eu.wrapping_mul(41) ^ 3)) * (sx - ew).max(1) as f32) as i32;
        let ez0 =
            (rand01(hash_u32(seed ^ eu.wrapping_mul(41) ^ 4)) * (sz - eh).max(1) as f32) as i32;
        // Only keep protrusion if it touches existing footprint, so it merges.
        let mut touches = false;
        'touch: for z in ez0.max(0)..(ez0 + eh).min(sz) {
            for x in ex0.max(0)..(ex0 + ew).min(sx) {
                for (nx, nz) in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                    if nx >= 0
                        && nx < sx
                        && nz >= 0
                        && nz < sz
                        && footprint[(nx + nz * sx) as usize]
                    {
                        touches = true;
                        break 'touch;
                    }
                }
            }
        }
        if touches || ei == 0 {
            for z in ez0.max(0)..(ez0 + eh).min(sz) {
                for x in ex0.max(0)..(ex0 + ew).min(sx) {
                    footprint[(x + z * sx) as usize] = true;
                }
            }
        }
    }

    keep_largest_component_2d(&mut footprint, sx, sz);

    let mut grid = VoxelGrid::new(sx, thickness, sz);
    for z in 0..sz {
        for x in 0..sx {
            if footprint[(x + z * sx) as usize] {
                for y in 0..thickness {
                    grid.set(x, y, z, true);
                }
            }
        }
    }

    // Guarantee at least a small cloud if generation got too sparse.
    if !grid.cells.iter().any(|v| *v) {
        let gx = sx.min(24);
        let gz = sz.min(12);
        let mut g = VoxelGrid::new(sx, 3, sz);
        for z in 0..gz {
            for x in 0..gx {
                for y in 0..3 {
                    g.set(x + (sx - gx) / 2, y, z + (sz - gz) / 2, true);
                }
            }
        }
        return g;
    }

    grid
}

fn keep_largest_component_2d(footprint: &mut [bool], sx: i32, sz: i32) {
    let mut labels = vec![-1i32; footprint.len()];
    let mut best_label = -1i32;
    let mut best_size = 0usize;
    let mut next_label = 0i32;
    let mut q = VecDeque::new();
    for z in 0..sz {
        for x in 0..sx {
            let i = (x + z * sx) as usize;
            if !footprint[i] || labels[i] != -1 {
                continue;
            }
            let label = next_label;
            next_label += 1;
            let mut size = 0usize;
            labels[i] = label;
            q.push_back((x, z));
            while let Some((cx, cz)) = q.pop_front() {
                size += 1;
                for (nx, nz) in [(cx - 1, cz), (cx + 1, cz), (cx, cz - 1), (cx, cz + 1)] {
                    if nx < 0 || nx >= sx || nz < 0 || nz >= sz {
                        continue;
                    }
                    let ni = (nx + nz * sx) as usize;
                    if footprint[ni] && labels[ni] == -1 {
                        labels[ni] = label;
                        q.push_back((nx, nz));
                    }
                }
            }
            if size > best_size {
                best_size = size;
                best_label = label;
            }
        }
    }
    if best_label == -1 {
        return;
    }
    for (i, v) in footprint.iter_mut().enumerate() {
        if *v && labels[i] != best_label {
            *v = false;
        }
    }
}

struct GreedyMeshOutput {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Quad {
    d: usize,
    side: i32,
    slice: i32,
    iu: i32,
    iv: i32,
    w: i32,
    h: i32,
}

fn build_greedy_quads(grid: &VoxelGrid) -> Vec<Quad> {
    let mut quads = Vec::new();
    for d in 0..3 {
        for &side in &[1i32, -1i32] {
            greedy_mesh_axis(grid, d, side, &mut quads);
        }
    }
    quads
}

/// Greedy mesh exposed voxel faces. Only boundary faces (neighbor empty or out
/// of bounds) are emitted. Adjacent coplanar faces with same normal are merged.
fn build_greedy_mesh(grid: &VoxelGrid) -> GreedyMeshOutput {
    let quads = build_greedy_quads(grid);
    build_greedy_quad_mesh(grid, &quads)
}

#[allow(clippy::too_many_arguments)]
fn greedy_mesh_axis(grid: &VoxelGrid, d: usize, side: i32, quads: &mut Vec<Quad>) {
    let dims = [grid.sx, grid.sy, grid.sz];
    let u = (d + 1) % 3;
    let v = (d + 2) % 3;
    let du = dims[u];
    let dv = dims[v];
    let dd = dims[d];

    let mut mask = vec![false; (du * dv) as usize];

    for slice in 0..dd {
        for iv in 0..dv {
            for iu in 0..du {
                let mut p = [0i32; 3];
                p[d] = slice;
                p[u] = iu;
                p[v] = iv;
                let occupied = grid.get(p[0], p[1], p[2]);
                let mut npos = p;
                npos[d] += side;
                let neighbor_occupied = grid.get(npos[0], npos[1], npos[2]);
                mask[(iu + iv * du) as usize] = occupied && !neighbor_occupied;
            }
        }

        let mut iv = 0;
        while iv < dv {
            let mut iu = 0;
            while iu < du {
                if !mask[(iu + iv * du) as usize] {
                    iu += 1;
                    continue;
                }
                let mut w = 1;
                while iu + w < du && mask[((iu + w) + iv * du) as usize] {
                    w += 1;
                }
                let mut h = 1;
                'outer: while iv + h < dv {
                    for k in 0..w {
                        if !mask[((iu + k) + (iv + h) * du) as usize] {
                            break 'outer;
                        }
                    }
                    h += 1;
                }
                for y2 in 0..h {
                    for x2 in 0..w {
                        mask[((iu + x2) + (iv + y2) * du) as usize] = false;
                    }
                }

                quads.push(make_quad(d, side, slice, iu, iv, w, h));
                iu += w;
            }
            iv += 1;
        }
    }
}

fn make_quad(d: usize, side: i32, slice: i32, iu: i32, iv: i32, w: i32, h: i32) -> Quad {
    Quad {
        d,
        side,
        slice,
        iu,
        iv,
        w,
        h,
    }
}

fn quad_world_corners(grid: &VoxelGrid, q: &Quad) -> [[f32; 3]; 4] {
    let origin = Vec3::new(
        -(grid.sx as f32) * VOXEL * 0.5,
        0.0,
        -(grid.sz as f32) * VOXEL * 0.5,
    );
    let u = (q.d + 1) % 3;
    let v = (q.d + 2) % 3;
    let plane = if q.side > 0 { q.slice + 1 } else { q.slice };
    let mut p00 = [0.0f32; 3];
    let mut p10 = [0.0f32; 3];
    let mut p11 = [0.0f32; 3];
    let mut p01 = [0.0f32; 3];
    p00[q.d] = plane as f32;
    p10[q.d] = plane as f32;
    p11[q.d] = plane as f32;
    p01[q.d] = plane as f32;
    p00[u] = q.iu as f32;
    p10[u] = (q.iu + q.w) as f32;
    p11[u] = (q.iu + q.w) as f32;
    p01[u] = q.iu as f32;
    p00[v] = q.iv as f32;
    p10[v] = q.iv as f32;
    p11[v] = (q.iv + q.h) as f32;
    p01[v] = (q.iv + q.h) as f32;
    let to_world = |p: [f32; 3]| (origin + Vec3::from_array(p) * VOXEL).to_array();
    [to_world(p00), to_world(p10), to_world(p11), to_world(p01)]
}

fn build_greedy_quad_mesh(grid: &VoxelGrid, quads: &[Quad]) -> GreedyMeshOutput {
    let mut positions = Vec::with_capacity(quads.len() * 4);
    let mut normals = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);

    for q in quads {
        let corners = quad_world_corners(grid, q);
        let mut normal = [0.0f32; 3];
        normal[q.d] = q.side as f32;
        let base = positions.len() as u32;
        let mut verts = if q.side > 0 {
            [0usize, 3, 2, 1]
        } else {
            [0, 1, 2, 3]
        };
        let p0 = Vec3::from_array(corners[verts[0]]);
        let p1 = Vec3::from_array(corners[verts[1]]);
        let p2 = Vec3::from_array(corners[verts[2]]);
        let geo = (p1 - p0).cross(p2 - p0);
        let n = Vec3::from_array(normal);
        if geo.dot(n) < 0.0 {
            verts = [verts[0], verts[3], verts[2], verts[1]];
        }
        for &vi in &verts {
            positions.push(corners[vi]);
            normals.push(normal);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    GreedyMeshOutput {
        positions,
        normals,
        indices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn mesh_data(mesh: &Mesh) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
        let p = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .unwrap()
            .as_float3()
            .unwrap()
            .to_vec();
        let n = mesh
            .attribute(Mesh::ATTRIBUTE_NORMAL)
            .unwrap()
            .as_float3()
            .unwrap()
            .to_vec();
        let Some(Indices::U32(i)) = mesh.indices() else {
            panic!("expected u32 indices");
        };
        (p, n, i.clone())
    }

    #[test]
    fn greedy_mesh_has_valid_indices_and_normals() {
        let mesh = build_formation_mesh(12345);
        let (positions, normals, indices) = mesh_data(&mesh);
        assert_eq!(positions.len(), normals.len());
        assert_eq!(indices.len() % 3, 0);
        for &idx in &indices {
            assert!((idx as usize) < positions.len(), "index out of range");
        }
        for tri in indices.as_chunks::<3>().0 {
            let a = Vec3::from_array(positions[tri[0] as usize]);
            let b = Vec3::from_array(positions[tri[1] as usize]);
            let c = Vec3::from_array(positions[tri[2] as usize]);
            let geo = (b - a).cross(c - a);
            let area2 = geo.length();
            assert!(area2 > 1e-5, "degenerate triangle");
            let geo_n = geo / area2;
            for &idx in tri {
                let n = Vec3::from_array(normals[idx as usize]);
                assert!(n.is_finite(), "normal not finite");
                assert!((n.length() - 1.0).abs() < 1e-2, "bad normal length");
                assert!(n.dot(geo_n) > -1e-3, "normal flipped against winding");
            }
        }
    }

    #[test]
    fn greedy_mesh_has_no_internal_faces_between_adjacent_occupied_cells() {
        let grid = generate_formation_grid(12345);
        let quads = build_greedy_quads(&grid);
        for q in &quads {
            let u = (q.d + 1) % 3;
            let v = (q.d + 2) % 3;
            for iv in 0..q.h {
                for iu in 0..q.w {
                    let mut p = [0i32; 3];
                    p[q.d] = q.slice;
                    p[u] = q.iu + iu;
                    p[v] = q.iv + iv;
                    assert!(grid.get(p[0], p[1], p[2]), "quad covers unoccupied cell");
                    let mut npos = p;
                    npos[q.d] += q.side;
                    assert!(
                        !grid.get(npos[0], npos[1], npos[2]),
                        "quad covers internal face at {p:?} side {}",
                        q.side
                    );
                }
            }
        }
        let mesh = build_formation_mesh(12345);
        let (positions, normals, _indices) = mesh_data(&mesh);
        assert!(positions.iter().all(|p| Vec3::from_array(*p).is_finite()));
        assert!(normals.iter().all(|n| Vec3::from_array(*n).is_finite()));
    }

    #[test]
    fn greedy_mesh_represents_occupancy_outer_boundary() {
        let grid = generate_formation_grid(4242);
        let quads = build_greedy_quads(&grid);

        let mut exposed = HashSet::<(i32, i32, i32, usize, i32)>::new();
        for y in 0..grid.sy {
            for z in 0..grid.sz {
                for x in 0..grid.sx {
                    if !grid.get(x, y, z) {
                        continue;
                    }
                    let checks = [
                        (x + 1, y, z, 0usize, 1i32),
                        (x - 1, y, z, 0usize, -1i32),
                        (x, y + 1, z, 1usize, 1i32),
                        (x, y - 1, z, 1usize, -1i32),
                        (x, y, z + 1, 2usize, 1i32),
                        (x, y, z - 1, 2usize, -1i32),
                    ];
                    for (nx, ny, nz, axis, side) in checks {
                        if !grid.get(nx, ny, nz) {
                            exposed.insert((x, y, z, axis, side));
                        }
                    }
                }
            }
        }

        let mut covered = HashSet::<(i32, i32, i32, usize, i32)>::new();
        for q in &quads {
            let u = (q.d + 1) % 3;
            let v = (q.d + 2) % 3;
            for iv in 0..q.h {
                for iu in 0..q.w {
                    let mut cell = [0i32; 3];
                    cell[q.d] = q.slice;
                    cell[u] = q.iu + iu;
                    cell[v] = q.iv + iv;
                    covered.insert((cell[0], cell[1], cell[2], q.d, q.side));
                }
            }
        }

        for e in &exposed {
            assert!(covered.contains(e), "missing exposed face {e:?}");
        }
        for c in &covered {
            assert!(
                exposed.contains(c),
                "covered face not in exposed boundary {c:?}"
            );
        }
    }

    #[test]
    fn greedy_mesh_quads_are_within_grid_bounds() {
        let grid = generate_formation_grid(4242);
        let quads = build_greedy_quads(&grid);
        for q in &quads {
            assert!(q.w > 0 && q.h > 0);
            let u = (q.d + 1) % 3;
            let v = (q.d + 2) % 3;
            let dims = [grid.sx, grid.sy, grid.sz];
            assert!(q.slice >= 0 && q.slice < dims[q.d]);
            assert!(q.iu >= 0 && q.iu + q.w <= dims[u]);
            assert!(q.iv >= 0 && q.iv + q.h <= dims[v]);
            assert!(q.side == 1 || q.side == -1);
        }
    }

    #[test]
    fn formation_is_broad_thin_and_connected() {
        for seed in [1u32, 12345, 4242, 0xC10D5EED, 99991] {
            let grid = generate_formation_grid(seed);
            assert!(grid.sx >= 60, "formation should be broad in X");
            assert!(grid.sz >= 35, "formation should be broad in Z");
            assert!(
                grid.sy <= 8,
                "formation should be relatively thin vertically"
            );

            // 3D connectivity: all occupied voxels in one component.
            let mut total = 0usize;
            let mut first = None;
            for y in 0..grid.sy {
                for z in 0..grid.sz {
                    for x in 0..grid.sx {
                        if grid.get(x, y, z) {
                            total += 1;
                            if first.is_none() {
                                first = Some((x, y, z));
                            }
                        }
                    }
                }
            }
            assert!(total > 0);
            let Some(start) = first else {
                continue;
            };
            let mut seen = vec![false; grid.cells.len()];
            let mut q = VecDeque::new();
            seen[grid.idx(start.0, start.1, start.2)] = true;
            q.push_back(start);
            let mut reached = 0usize;
            while let Some((cx, cy, cz)) = q.pop_front() {
                reached += 1;
                for (nx, ny, nz) in [
                    (cx - 1, cy, cz),
                    (cx + 1, cy, cz),
                    (cx, cy - 1, cz),
                    (cx, cy + 1, cz),
                    (cx, cy, cz - 1),
                    (cx, cy, cz + 1),
                ] {
                    if !grid.in_bounds(nx, ny, nz) || !grid.get(nx, ny, nz) {
                        continue;
                    }
                    let ni = grid.idx(nx, ny, nz);
                    if !seen[ni] {
                        seen[ni] = true;
                        q.push_back((nx, ny, nz));
                    }
                }
            }
            assert_eq!(
                reached, total,
                "formation has disconnected fragments, seed={seed}"
            );

            // Classic Minecraft clouds are flat slabs with uniform thickness.
            let mut min_bottom = i32::MAX;
            let mut max_bottom = i32::MIN;
            let mut min_top = i32::MAX;
            let mut max_top = i32::MIN;
            for z in 0..grid.sz {
                for x in 0..grid.sx {
                    let mut col_bottom = None;
                    let mut col_top = None;
                    for y in 0..grid.sy {
                        if grid.get(x, y, z) {
                            col_bottom = Some(col_bottom.unwrap_or(y));
                            col_top = Some(y + 1);
                        }
                    }
                    if let (Some(b), Some(t)) = (col_bottom, col_top) {
                        min_bottom = min_bottom.min(b);
                        max_bottom = max_bottom.max(b);
                        min_top = min_top.min(t);
                        max_top = max_top.max(t);
                    }
                }
            }
            assert_eq!(
                min_bottom, max_bottom,
                "Minecraft-style cloud bottom should be flat, seed={seed}"
            );
            assert_eq!(
                min_top, max_top,
                "Minecraft-style cloud top should be flat, seed={seed}"
            );
        }
    }
}
