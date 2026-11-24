use bevy::prelude::*;
use std::collections::HashMap;

use crate::world::ChunkManager;
use crate::world::block::Facing;
use crate::world::chunk::detail_y_in_range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StructureId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StructureKind {
    Workbench,
    Furnace,
}

impl StructureKind {
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Workbench),
            1 => Some(Self::Furnace),
            _ => None,
        }
    }
    pub const fn index(self) -> u8 {
        match self {
            Self::Workbench => 0,
            Self::Furnace => 1,
        }
    }
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Workbench => "Workbench",
            Self::Furnace => "Furnace",
        }
    }
    pub const fn footprint(self) -> (i32, i32) {
        match self {
            // 2m wide x 1m deep in detail cells (0.5m each).
            Self::Workbench => (4, 2),
            // 2m wide x 1m deep in detail cells, matching workbench footprint.
            Self::Furnace => (4, 2),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlacedStructure {
    pub id: StructureId,
    pub kind: StructureKind,
    pub anchor: IVec3,
    pub facing: Facing,
}

#[derive(Resource, Debug, Default)]
pub struct Structures {
    pub next_id: u32,
    pub placed: HashMap<StructureId, PlacedStructure>,
    pub by_cell: HashMap<IVec3, StructureId>,
}

impl Structures {
    #[allow(dead_code)]
    pub fn get_at(&self, cell: IVec3) -> Option<&PlacedStructure> {
        let id = self.by_cell.get(&cell).copied()?;
        self.placed.get(&id)
    }
    pub fn occupied_cells(kind: StructureKind, anchor: IVec3, facing: Facing) -> Vec<IVec3> {
        let (w, d) = kind.footprint();
        let mut cells = Vec::with_capacity((w * d) as usize);
        for dz in 0..d {
            for dx in 0..w {
                cells.push(anchor + rotate_local(IVec3::new(dx, 0, dz), facing, w, d));
            }
        }
        cells
    }
    pub fn can_place(
        &self,
        manager: &ChunkManager,
        kind: StructureKind,
        anchor: IVec3,
        facing: Facing,
        player: Option<(Vec3, f32)>,
    ) -> bool {
        for cell in Self::occupied_cells(kind, anchor, facing) {
            if !detail_y_in_range(cell.y) {
                return false;
            }
            if self.by_cell.contains_key(&cell) {
                return false;
            }
            if manager
                .get_voxel_if_loaded(cell)
                .is_some_and(|v| v.is_solid())
            {
                return false;
            }
            let below = cell + IVec3::NEG_Y;
            let supported = manager
                .get_voxel_if_loaded(below)
                .is_some_and(|v| v.is_solid())
                || {
                    let world = crate::world::chunk::detail_to_world(below);
                    manager.get_world_block(world).is_some_and(|b| b.is_solid())
                };
            if !supported {
                return false;
            }
            if let Some((pp, ph)) = player {
                let min = cell.as_vec3();
                let max = min + Vec3::ONE;
                if aabb_intersects_player(min, max, pp, ph) {
                    return false;
                }
            }
        }
        if let Some((pp, ph)) = player {
            for (min, max) in collision_boxes(kind, anchor, facing) {
                if aabb_intersects_player(min, max, pp, ph) {
                    return false;
                }
            }
        }
        true
    }
    pub fn place(
        &mut self,
        kind: StructureKind,
        anchor: IVec3,
        facing: Facing,
    ) -> Option<StructureId> {
        let id = StructureId(self.next_id);
        self.next_id = self.next_id.checked_add(1)?;
        let placed = PlacedStructure {
            id,
            kind,
            anchor,
            facing,
        };
        for cell in placed.occupied_cells() {
            if self.by_cell.contains_key(&cell) {
                return None;
            }
        }
        for cell in placed.occupied_cells() {
            self.by_cell.insert(cell, id);
        }
        self.placed.insert(id, placed);
        Some(id)
    }
    pub fn remove(&mut self, id: StructureId) -> Option<PlacedStructure> {
        let placed = self.placed.remove(&id)?;
        for cell in placed.occupied_cells() {
            if self.by_cell.get(&cell).copied() == Some(id) {
                self.by_cell.remove(&cell);
            }
        }
        Some(placed)
    }
    #[allow(dead_code)]
    pub fn remove_at(&mut self, cell: IVec3) -> Option<PlacedStructure> {
        let id = *self.by_cell.get(&cell)?;
        self.remove(id)
    }
    pub fn iter(&self) -> impl Iterator<Item = &PlacedStructure> {
        self.placed.values()
    }
}

impl PlacedStructure {
    pub fn occupied_cells(&self) -> Vec<IVec3> {
        Structures::occupied_cells(self.kind, self.anchor, self.facing)
    }
    pub fn collision_boxes(&self) -> Vec<(Vec3, Vec3)> {
        collision_boxes(self.kind, self.anchor, self.facing)
    }
}

/// Rotate a local detail-cell offset around the structure's center.
/// `local` is relative to the unrotated footprint min corner.
/// Returns offset relative to the rotated footprint min corner (anchor).
pub fn rotate_local(local: IVec3, facing: Facing, fw: i32, fh: i32) -> IVec3 {
    let cx = local.x as f32 + 0.5 - fw as f32 / 2.0;
    let cz = local.z as f32 + 0.5 - fh as f32 / 2.0;
    let (rx, rz) = match facing {
        Facing::North => (cx, cz),
        Facing::East => (cz, -cx),
        Facing::South => (-cx, -cz),
        Facing::West => (-cz, cx),
    };
    let (rfw, rfh) = rotated_footprint(fw, fh, facing);
    IVec3::new(
        (rx + rfw as f32 / 2.0 - 0.5).floor() as i32,
        local.y,
        (rz + rfh as f32 / 2.0 - 0.5).floor() as i32,
    )
}

fn rotated_footprint(fw: i32, fh: i32, facing: Facing) -> (i32, i32) {
    match facing {
        Facing::North | Facing::South => (fw, fh),
        Facing::East | Facing::West => (fh, fw),
    }
}

pub fn collision_boxes(kind: StructureKind, anchor: IVec3, facing: Facing) -> Vec<(Vec3, Vec3)> {
    let (fw, fh) = kind.footprint();
    let (rfw, rfh) = rotated_footprint(fw, fh, facing);
    let center = anchor.as_vec3() + Vec3::new(rfw as f32 * 0.5, 0.0, rfh as f32 * 0.5);
    match kind {
        StructureKind::Workbench => workbench_boxes(center, facing),
        StructureKind::Furnace => furnace_boxes(center, facing),
    }
}

fn local_part(min: Vec3, max: Vec3, center: Vec3, facing: Facing) -> (Vec3, Vec3) {
    let (rmin, rmax) = rotate_part_y(min, max, facing);
    (center + rmin, center + rmax)
}

fn furnace_boxes(base_center: Vec3, facing: Facing) -> Vec<(Vec3, Vec3)> {
    // 2m wide x 1m deep x ~3m tall primitive stone furnace.
    // Local origin is structure center at ground. North (-Z) is the front/opening side.
    let m = crate::world::chunk::BLOCKS_PER_METRE;
    let w = 2.0 * m;
    let d = 1.0 * m;
    let c = Vec3::new(w * 0.5, 0.0, d * 0.5);
    let wall = 0.18 * m;
    let base_h = 0.18 * m;
    let body_h = 1.45 * m; // main body top before dome (~2.5m total)
    let opening_w = 0.76 * m;
    let opening_h = 0.68 * m;
    let opening_x0 = (w - opening_w) * 0.5;
    let opening_x1 = opening_x0 + opening_w;
    let opening_y1 = base_h + opening_h;
    let front_z1 = wall;
    let dome1_h = 0.32 * m;
    let dome2_h = 0.26 * m;
    let dome3_h = 0.18 * m;
    let cap_h = 0.11 * m;

    let parts: Vec<(Vec3, Vec3)> = vec![
        // Base slab / plinth
        (Vec3::new(0.0, 0.0, 0.0) - c, Vec3::new(w, base_h, d) - c),
        // Back wall
        (
            Vec3::new(0.0, base_h, d - wall) - c,
            Vec3::new(w, body_h, d) - c,
        ),
        // Side walls
        (
            Vec3::new(0.0, base_h, front_z1) - c,
            Vec3::new(wall, body_h, d - wall) - c,
        ),
        (
            Vec3::new(w - wall, base_h, front_z1) - c,
            Vec3::new(w, body_h, d - wall) - c,
        ),
        // Front wall around opening
        (
            Vec3::new(0.0, base_h, 0.0) - c,
            Vec3::new(opening_x0, opening_y1, front_z1) - c,
        ),
        (
            Vec3::new(opening_x1, base_h, 0.0) - c,
            Vec3::new(w, opening_y1, front_z1) - c,
        ),
        (
            Vec3::new(0.0, opening_y1, 0.0) - c,
            Vec3::new(w, body_h, front_z1) - c,
        ),
        // Rounded/domed top tiers
        (
            Vec3::new(0.08 * m, body_h, 0.06 * m) - c,
            Vec3::new(w - 0.08 * m, body_h + dome1_h, d - 0.06 * m) - c,
        ),
        (
            Vec3::new(0.22 * m, body_h + dome1_h, 0.12 * m) - c,
            Vec3::new(w - 0.22 * m, body_h + dome1_h + dome2_h, d - 0.12 * m) - c,
        ),
        (
            Vec3::new(0.42 * m, body_h + dome1_h + dome2_h, 0.20 * m) - c,
            Vec3::new(
                w - 0.42 * m,
                body_h + dome1_h + dome2_h + dome3_h,
                d - 0.20 * m,
            ) - c,
        ),
        (
            Vec3::new(0.68 * m, body_h + dome1_h + dome2_h + dome3_h, 0.30 * m) - c,
            Vec3::new(
                w - 0.68 * m,
                body_h + dome1_h + dome2_h + dome3_h + cap_h,
                d - 0.30 * m,
            ) - c,
        ),
    ];
    parts
        .into_iter()
        .map(|(min, max)| local_part(min, max, base_center, facing))
        .collect()
}

fn rotate_part_y(min: Vec3, max: Vec3, facing: Facing) -> (Vec3, Vec3) {
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
    ];
    let mut out_min = Vec3::splat(f32::INFINITY);
    let mut out_max = Vec3::splat(f32::NEG_INFINITY);
    for c in corners {
        let r = match facing {
            Facing::North => c,
            Facing::East => Vec3::new(c.z, c.y, -c.x),
            Facing::South => Vec3::new(-c.x, c.y, -c.z),
            Facing::West => Vec3::new(-c.z, c.y, c.x),
        };
        out_min = out_min.min(r);
        out_max = out_max.max(r);
    }
    (out_min, out_max)
}

fn workbench_boxes(base_center: Vec3, facing: Facing) -> Vec<(Vec3, Vec3)> {
    // Dimensions in metres, converted to detail/world units (1m = 2 units).
    // base_center is the structure's world-space center at ground level.
    // Parts are built around local origin (0,0,0) = structure center.
    let m = crate::world::chunk::BLOCKS_PER_METRE;
    let top_y = 0.82 * m;
    let top_thick = 0.10 * m;
    let brace_y = 0.24 * m;
    let brace_h = 0.08 * m;
    let leg = 0.10 * m;
    let w = 2.0 * m;
    let d = 1.0 * m;
    let c = Vec3::new(w * 0.5, 0.0, d * 0.5);
    let parts_local: [(Vec3, Vec3); 9] = [
        (Vec3::new(0.0, 0.0, 0.0) - c, Vec3::new(leg, top_y, leg) - c),
        (
            Vec3::new(w - leg, 0.0, 0.0) - c,
            Vec3::new(w, top_y, leg) - c,
        ),
        (
            Vec3::new(0.0, 0.0, d - leg) - c,
            Vec3::new(leg, top_y, d) - c,
        ),
        (
            Vec3::new(w - leg, 0.0, d - leg) - c,
            Vec3::new(w, top_y, d) - c,
        ),
        (
            Vec3::new(0.0, top_y, 0.0) - c,
            Vec3::new(w, top_y + top_thick, d) - c,
        ),
        (
            Vec3::new(leg, brace_y, 0.03 * m) - c,
            Vec3::new(w - leg, brace_y + brace_h, 0.09 * m) - c,
        ),
        (
            Vec3::new(leg, brace_y, d - 0.09 * m) - c,
            Vec3::new(w - leg, brace_y + brace_h, d - 0.03 * m) - c,
        ),
        (
            Vec3::new(0.03 * m, brace_y, leg) - c,
            Vec3::new(0.09 * m, brace_y + brace_h, d - leg) - c,
        ),
        (
            Vec3::new(w - 0.09 * m, brace_y, leg) - c,
            Vec3::new(w - 0.03 * m, brace_y + brace_h, d - leg) - c,
        ),
    ];
    parts_local
        .into_iter()
        .map(|(min, max)| {
            let (rmin, rmax) = rotate_part_y(min, max, facing);
            (base_center + rmin, base_center + rmax)
        })
        .collect()
}

pub fn raycast(
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
    structures: &Structures,
) -> Option<(f32, Vec3, StructureId)> {
    let mut best: Option<(f32, Vec3, StructureId)> = None;
    for placed in structures.iter() {
        for (min, max) in placed.collision_boxes() {
            if let Some((t, n)) = ray_aabb(origin, dir, min, max)
                && t >= 0.0
                && t <= max_distance
                && best.as_ref().is_none_or(|(bt, _, _)| t < *bt)
            {
                best = Some((t, n, placed.id));
            }
        }
    }
    best
}

fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<(f32, Vec3)> {
    let mut tmin = 0.0f32;
    let mut tmax = f32::INFINITY;
    let mut normal = Vec3::ZERO;
    for axis in 0..3 {
        let o = origin[axis];
        let d = dir[axis];
        let min_a = min[axis];
        let max_a = max[axis];
        if d.abs() < 1e-8 {
            if o < min_a || o > max_a {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (min_a - o) * inv;
            let mut t2 = (max_a - o) * inv;
            let mut n = Vec3::ZERO;
            n[axis] = -1.0;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
                n[axis] = 1.0;
            }
            if t1 > tmin {
                tmin = t1;
                normal = n;
            }
            tmax = tmax.min(t2);
            if tmin > tmax {
                return None;
            }
        }
    }
    Some((tmin, normal))
}

fn aabb_intersects_player(min: Vec3, max: Vec3, player_pos: Vec3, player_height: f32) -> bool {
    let half = 0.30;
    let pmin = player_pos - Vec3::new(half, 0.0, half);
    let pmax = player_pos + Vec3::new(half, player_height, half);
    min.x < pmax.x
        && max.x > pmin.x
        && min.y < pmax.y
        && max.y > pmin.y
        && min.z < pmax.z
        && max.z > pmin.z
}

#[derive(Component, Debug, Clone, Copy)]
pub struct StructureEntity(#[allow(dead_code)] pub StructureId);

#[derive(Resource, Default)]
pub struct StructureEntities(pub HashMap<StructureId, Entity>);

pub struct StructuresPlugin;

impl Plugin for StructuresPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Structures>()
            .init_resource::<StructureEntities>()
            .init_resource::<StructureCrosshair>()
            .add_systems(bevy::prelude::PreUpdate, update_structure_crosshair)
            .add_systems(Update, sync_structure_visuals);
    }
}

fn sync_structure_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    structures: Res<Structures>,
    mut entities: ResMut<StructureEntities>,
) {
    if !structures.is_changed() {
        return;
    }
    let ids: Vec<_> = structures.placed.keys().copied().collect();
    let stale: Vec<_> = entities
        .0
        .keys()
        .copied()
        .filter(|id| !structures.placed.contains_key(id))
        .collect();
    for id in stale {
        despawn_structure_visual(&mut commands, &mut entities, id);
    }
    for id in ids {
        if let Some(placed) = structures.placed.get(&id).cloned() {
            spawn_structure_visual(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut entities,
                &placed,
            );
        }
    }
}

pub fn spawn_structure_visual(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    entities: &mut StructureEntities,
    placed: &PlacedStructure,
) {
    if entities.0.contains_key(&placed.id) {
        return;
    }
    let mat = structure_material(materials, placed.kind);
    let mut parent = commands.spawn((
        Name::new(placed.kind.display_name()),
        StructureEntity(placed.id),
        Transform::default(),
        GlobalTransform::default(),
        Visibility::Visible,
        InheritedVisibility::VISIBLE,
        ViewVisibility::default(),
    ));
    parent.with_children(|c| {
        for (min, max) in placed.collision_boxes() {
            let size = max - min;
            let center = (min + max) * 0.5;
            c.spawn((
                Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
                MeshMaterial3d(mat.clone()),
                Transform::from_translation(center),
                GlobalTransform::default(),
                Visibility::Visible,
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
            ));
        }
    });
    let e = parent.id();
    entities.0.insert(placed.id, e);
}

fn structure_material(
    materials: &mut Assets<StandardMaterial>,
    kind: StructureKind,
) -> Handle<StandardMaterial> {
    let (color, roughness) = match kind {
        StructureKind::Workbench => (Color::srgb(0.45, 0.30, 0.18), 0.9),
        StructureKind::Furnace => (Color::srgb(0.46, 0.45, 0.43), 0.95),
    };
    materials.add(StandardMaterial {
        base_color: color,
        perceptual_roughness: roughness,
        ..default()
    })
}
pub fn despawn_structure_visual(
    commands: &mut Commands,
    entities: &mut StructureEntities,
    id: StructureId,
) {
    if let Some(e) = entities.0.remove(&id) {
        commands.entity(e).despawn();
    }
}

#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct StructureCrosshair {
    pub hit: Option<(f32, Vec3, StructureId)>,
}

fn update_structure_crosshair(
    camera: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    structures: Res<Structures>,
    mut target: ResMut<StructureCrosshair>,
) {
    let Ok(camera) = camera.single() else {
        target.hit = None;
        return;
    };
    let origin = camera.translation();
    let dir = camera.rotation() * Vec3::NEG_Z;
    target.hit = raycast(origin, dir, crate::player::REACH, &structures);
}
