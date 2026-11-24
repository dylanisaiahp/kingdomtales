//! Direct building rules: placement, hammer shapes and wrench rotation.
//!
//! Placement spends no abstract currency. The caller decides whether the
//! inventory can pay; these rules only decide whether a single cell can change.
//! The hammer changes geometry while preserving material and orientation. The
//! wrench advances orientation while preserving material and shape.

use bevy::prelude::*;

use crate::player::voxel_intersects_player;
use crate::world::ChunkManager;
use crate::world::block::{Block, Facing, Orientation, Shape, Voxel};
use crate::world::chunk::detail_y_in_range;

/// The current shape choice used by the hammer menu and placement preview.
#[derive(Resource, Debug, Clone)]
pub struct ConstructionState {
    pub shape: Shape,
    recent_shapes: Vec<Shape>,
}

impl Default for ConstructionState {
    fn default() -> Self {
        Self {
            shape: Shape::Cube,
            recent_shapes: vec![Shape::Cube],
        }
    }
}

impl ConstructionState {
    /// Choose a buildable shape for the hammer.
    pub fn select_shape(&mut self, shape: Shape) {
        if !shape.is_buildable() {
            return;
        }
        self.shape = shape;
        remember(&mut self.recent_shapes, shape);
    }

    pub fn recent_shapes(&self) -> &[Shape] {
        &self.recent_shapes
    }
}

fn remember<T: Copy + PartialEq>(recent: &mut Vec<T>, value: T) {
    recent.retain(|entry| *entry != value);
    recent.insert(0, value);
    recent.truncate(4);
}

/// Upright orientation whose front points back toward the player.
pub fn orientation_facing_player(look: Vec3) -> Orientation {
    let horizontal = Vec3::new(-look.x, 0.0, -look.z);
    if horizontal.length_squared() < 1e-6 {
        Orientation::default()
    } else {
        Orientation::from_facing(Facing::from_direction(horizontal))
    }
}

/// Orientations a half block or panel can rest in.
const RESTING_ORIENTATIONS: [u8; 6] = [0, 4, 8, 12, 16, 20];

/// Keep an orientation meaningful when a shape changes. Slopes and stairs use
/// all 24 orientations; half blocks and panels only rest on a cardinal face.
fn orientation_for_shape(current: Orientation, shape: Shape) -> Orientation {
    match shape {
        Shape::HalfBlock | Shape::Panel if !RESTING_ORIENTATIONS.contains(&current.index()) => {
            let up = current.up();
            Orientation::ALL
                .into_iter()
                .filter(|candidate| RESTING_ORIENTATIONS.contains(&candidate.index()))
                .max_by(|a, b| {
                    a.up()
                        .dot(up)
                        .partial_cmp(&b.up().dot(up))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(current)
        }
        _ => current,
    }
}

fn cell_is_buildable(pos: IVec3) -> bool {
    detail_y_in_range(pos.y)
}

/// Apply a shape to an existing block, returning the chunks needing a remesh.
/// Material is preserved and orientation is snapped when the new shape has
/// fewer resting faces.
pub fn apply_shape_checked(
    manager: &mut ChunkManager,
    pos: IVec3,
    shape: Shape,
) -> Option<Vec<IVec3>> {
    if !shape.is_buildable() || !cell_is_buildable(pos) {
        return None;
    }
    let existing = manager.get_voxel_if_loaded(pos)?;
    if existing.is_air() || !Block::BUILDABLE.contains(&existing.block()) {
        return None;
    }

    let orientation = orientation_for_shape(existing.orientation(), shape);
    let updated = Voxel::from_oriented_parts(existing.block(), shape, orientation);
    Some(manager.set_voxel(pos, updated))
}

/// Advance an existing block's orientation and return the chunks needing a
/// remesh. Material and shape never change.
pub fn rotate_block_checked(manager: &mut ChunkManager, pos: IVec3) -> Option<Vec<IVec3>> {
    if !cell_is_buildable(pos) {
        return None;
    }
    let existing = manager.get_voxel_if_loaded(pos)?;
    if existing.is_air() {
        return None;
    }
    let next = existing.orientation().next_for_shape(existing.shape());
    if next == existing.orientation() {
        return None;
    }
    let updated = Voxel::from_oriented_parts(existing.block(), existing.shape(), next);
    Some(manager.set_voxel(pos, updated))
}

/// Place one full cube and return the chunks needing a remesh. Cost is the
/// caller's job. `player` is `(feet position, height)`; occupied or overlapping
/// cells are rejected.
#[allow(clippy::too_many_arguments)]
pub fn place_block_checked(
    manager: &mut ChunkManager,
    pos: IVec3,
    material: Block,
    orientation: Orientation,
    player: Option<(Vec3, f32)>,
) -> Option<Vec<IVec3>> {
    if !Block::BUILDABLE.contains(&material) || !cell_is_buildable(pos) {
        return None;
    }
    let existing = manager.get_voxel_if_loaded(pos)?;
    if !existing.is_air() {
        return None;
    }

    let voxel = Voxel::from_oriented_parts(material, Shape::Cube, orientation);
    if let Some((position, height)) = player
        && voxel_intersects_player(position, height, pos, voxel)
    {
        return None;
    }
    Some(manager.set_voxel(pos, voxel))
}

pub struct ConstructionPlugin;

impl Plugin for ConstructionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConstructionState>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::PLAYER_HEIGHT;
    use crate::world::block::Shape;
    use crate::world::chunk::{Chunk, ChunkData};
    use bevy::prelude::Entity;

    fn manager_with_chunks(coords: &[IVec3]) -> ChunkManager {
        let mut manager = ChunkManager::default();
        for coord in coords {
            manager.chunks.insert(
                *coord,
                Chunk {
                    data: ChunkData::new(*coord),
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
        manager
    }

    #[test]
    fn shape_change_keeps_material() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        manager.set_voxel(IVec3::new(4, 8, 4), Voxel::new(Block::Granite));

        assert!(apply_shape_checked(&mut manager, IVec3::new(4, 8, 4), Shape::HalfBlock).is_some());
        let voxel = manager.get_voxel_if_loaded(IVec3::new(4, 8, 4)).unwrap();
        assert_eq!(voxel.block(), Block::Granite);
        assert_eq!(voxel.shape(), Shape::HalfBlock);
    }

    #[test]
    fn shape_change_snaps_to_a_resting_orientation() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        let sideways = Orientation::ALL
            .into_iter()
            .find(|orientation| orientation.up().y.abs() < 0.5)
            .unwrap();
        let voxel = Voxel::from_oriented_parts(Block::Granite, Shape::Stairs, sideways);
        manager.set_voxel(IVec3::new(4, 8, 4), voxel);

        assert!(apply_shape_checked(&mut manager, IVec3::new(4, 8, 4), Shape::Panel).is_some());
        let updated = manager.get_voxel_if_loaded(IVec3::new(4, 8, 4)).unwrap();
        assert_eq!(updated.block(), Block::Granite);
        assert_eq!(updated.shape(), Shape::Panel);
        assert!(RESTING_ORIENTATIONS.contains(&updated.orientation().index()));
    }

    #[test]
    fn rotation_keeps_material_and_shape() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        let original =
            Voxel::from_oriented_parts(Block::Slate, Shape::Stairs, Orientation::default());
        manager.set_voxel(IVec3::new(6, 8, 6), original);

        assert!(rotate_block_checked(&mut manager, IVec3::new(6, 8, 6)).is_some());
        let rotated = manager.get_voxel_if_loaded(IVec3::new(6, 8, 6)).unwrap();
        assert_eq!(rotated.block(), original.block());
        assert_eq!(rotated.shape(), original.shape());
        assert_ne!(rotated.orientation(), original.orientation());
    }

    #[test]
    fn cubes_do_not_rotate() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        manager.set_voxel(IVec3::new(6, 8, 6), Voxel::new(Block::Granite));
        assert!(rotate_block_checked(&mut manager, IVec3::new(6, 8, 6)).is_none());
    }

    #[test]
    fn placement_writes_one_cell_and_reports_dirty_chunks() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        let pos = IVec3::new(4, 8, 4);
        let dirty = place_block_checked(
            &mut manager,
            pos,
            Block::Granite,
            Orientation::default(),
            None,
        )
        .expect("placement should succeed");
        assert!(!dirty.is_empty());
        let voxel = manager.get_voxel_if_loaded(pos).unwrap();
        assert_eq!(voxel.block(), Block::Granite);
        assert_eq!(voxel.shape(), Shape::Cube);
    }

    #[test]
    fn placement_rejects_occupied_cells() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        let pos = IVec3::new(4, 8, 4);
        manager.set_voxel(pos, Voxel::new(Block::Topsoil));
        assert!(
            place_block_checked(
                &mut manager,
                pos,
                Block::Granite,
                Orientation::default(),
                None
            )
            .is_none()
        );
        assert_eq!(
            manager.get_voxel_if_loaded(pos).unwrap().block(),
            Block::Topsoil
        );
    }

    #[test]
    fn placement_rejects_player_cells() {
        let mut manager = manager_with_chunks(&[IVec3::ZERO]);
        let pos = IVec3::new(4, 8, 4);
        let player_position = pos.as_vec3() + Vec3::splat(0.5);
        assert!(
            place_block_checked(
                &mut manager,
                pos,
                Block::Granite,
                Orientation::default(),
                Some((player_position - Vec3::Y * 0.5, PLAYER_HEIGHT)),
            )
            .is_none()
        );
        assert!(manager.get_voxel_if_loaded(pos).unwrap().is_air());
    }

    #[test]
    fn selection_keeps_recent_shapes() {
        let mut state = ConstructionState::default();
        state.select_shape(Shape::Slope);
        state.select_shape(Shape::Panel);
        assert_eq!(state.shape, Shape::Panel);
        assert_eq!(state.recent_shapes()[0], Shape::Panel);
        assert_eq!(state.recent_shapes()[1], Shape::Slope);
    }
}
