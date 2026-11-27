//! Tree felling data, drops and individual leaf debris.
//!
//! Mining a trunk collects the connected tree once. Every wood cell becomes a
//! normal `BlockDrop` immediately, batched into at most two item stacks. Each
//! leaf cell becomes its own lightweight falling piece that drops straight
//! down, settles, then fades and decays on its own schedule. A decayed leaf has
//! a small chance to yield either a stick or fiber.

use std::collections::{HashSet, VecDeque};

use bevy::prelude::*;

use crate::items::ItemId;
use crate::world::ChunkManager;
use crate::world::block::{Block, Shape, Voxel};
use crate::world::chunk::BLOCKS_PER_METRE;
use crate::world::generation::hash_u32;

const LEAF_DROP_SALT: u32 = 0x5eed_1eaf;
/// ~5% of leaf cells yield a stick or fiber (split evenly).
const LEAF_DROP_THRESHOLD: u32 = 13; // 13/256 ~ 5%
const LEAF_FALL_DELAY_SALT: u32 = 0x1ea4_5eed;
const LEAF_LIFETIME_SALT: u32 = 0x7eaf_1eaf;
const MAX_LEAF_FALL_DELAY: f32 = 0.35;
/// Leaves within this many detail cells of the tree's wood belong to that tree.
/// Felling follows wood only, so touching canopies cannot merge two trees.
const LEAF_ATTACH_RADIUS: i32 = 3;

/// Tunable felling behaviour. Kept as a resource so tree size, decay timing and
/// gravity can be adjusted without touching the mining or rendering code.
#[derive(Resource, Debug, Clone)]
pub struct FellingConfig {
    /// Maximum connected tree voxels collected from one trunk hit. This also
    /// bounds how many individual leaf pieces one tree can create.
    pub max_tree_voxels: usize,
    /// Base seconds a settled leaf lingers before decaying. Each cell gets a
    /// deterministic multiplier so a canopy does not vanish all at once.
    pub leaf_lifetime: f32,
    /// Downward acceleration in detail-block units per second squared.
    pub gravity: f32,
}

impl Default for FellingConfig {
    fn default() -> Self {
        Self {
            max_tree_voxels: 512,
            leaf_lifetime: 1.2,
            gravity: 18.0 * BLOCKS_PER_METRE,
        }
    }
}

/// The connected tree collected when a trunk is mined.
#[derive(Clone, Debug)]
pub struct FellPlan {
    pub cells: Vec<(IVec3, Voxel)>,
}

/// Deterministic noise in 0..255 for one leaf cell. Separate salts keep the
/// stick roll, fall delay and decay variation independent of one another.
fn leaf_noise(cell: IVec3, salt: u32) -> u32 {
    let mixed = (cell.x as u32).wrapping_mul(0x9e37_79b1)
        ^ (cell.y as u32).wrapping_mul(0x27d4_eb2d)
        ^ (cell.z as u32).wrapping_mul(0x85eb_ca77)
        ^ salt;
    hash_u32(mixed) & 0xff
}

/// Deterministic leaf drop roll: ~5% chance total, split between stick/fiber.
/// Returns Some(item) or None.
pub fn leaf_drop_item(cell: IVec3) -> Option<ItemId> {
    let roll = leaf_noise(cell, LEAF_DROP_SALT);
    if roll < LEAF_DROP_THRESHOLD / 2 {
        Some(ItemId::Stick)
    } else if roll < LEAF_DROP_THRESHOLD {
        Some(ItemId::Fiber)
    } else {
        None
    }
}

/// Seconds a leaf waits before starting its fall. The small stagger makes a
/// canopy come down piece by piece instead of as a single sheet.
pub fn leaf_fall_delay(cell: IVec3) -> f32 {
    leaf_noise(cell, LEAF_FALL_DELAY_SALT) as f32 / 255.0 * MAX_LEAF_FALL_DELAY
}

/// Lifetime of one settled leaf cell around the configured base value.
pub fn leaf_decay_lifetime(cell: IVec3, base: f32) -> f32 {
    let variation = leaf_noise(cell, LEAF_LIFETIME_SALT) as f32 / 255.0;
    (base * (0.6 + variation * 0.9)).max(0.05)
}

/// Every leaf cell in a felled tree, one entry per cell. Wood cells are not
/// debris: they are spawned as normal drops before this function is called.
pub fn leaf_cells(plan: &FellPlan) -> Vec<IVec3> {
    plan.cells
        .iter()
        .filter(|(_, voxel)| voxel.block() == Block::Leaves)
        .map(|(cell, _)| *cell)
        .collect()
}

/// Batch every remaining wood cell into at most two normal item drops.
pub fn wood_drop_counts(cells: &[(IVec3, Voxel)]) -> Vec<(ItemId, u32)> {
    let mut wood = 0u32;
    let mut sticks = 0u32;
    for (_, voxel) in cells {
        match (voxel.block(), voxel.shape()) {
            (Block::Wood, Shape::Stick | Shape::Stem | Shape::DebarkedStem) => sticks += 1,
            (Block::Wood, _) => wood += 1,
            _ => {}
        }
    }

    let mut batches = Vec::new();
    if wood > 0 {
        batches.push((ItemId::Wood, wood));
    }
    if sticks > 0 {
        batches.push((ItemId::Stick, sticks));
    }
    batches
}

/// Collect one tree around `hit`. Returns `None` for isolated wood (a placed
/// beam or a single branch) so construction is never felled.
///
/// The woody structure is followed through connected wood cells only. Leaves
/// are then gathered near that wood without being traversed, so two trees whose
/// canopies touch are still two separate trees instead of one forest-sized plan.
pub fn plan_felling(
    manager: &ChunkManager,
    hit: IVec3,
    config: &FellingConfig,
) -> Option<FellPlan> {
    let start = manager.get_voxel_if_loaded(hit)?;
    if !is_wood_voxel(start) {
        return None;
    }

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut wood_cells = Vec::new();

    visited.insert(hit);
    queue.push_back(hit);

    while let Some(cell) = queue.pop_front() {
        let Some(voxel) = manager.get_voxel_if_loaded(cell) else {
            continue;
        };
        if !is_wood_voxel(voxel) {
            continue;
        }
        if cell.x.abs_diff(hit.x) > 24 || cell.z.abs_diff(hit.z) > 24 || cell.y.abs_diff(hit.y) > 64
        {
            continue;
        }

        wood_cells.push((cell, voxel));
        if wood_cells.len() >= config.max_tree_voxels {
            break;
        }

        for offset in [
            IVec3::X,
            IVec3::NEG_X,
            IVec3::Y,
            IVec3::NEG_Y,
            IVec3::Z,
            IVec3::NEG_Z,
        ] {
            let next = cell + offset;
            if visited.insert(next) {
                queue.push_back(next);
            }
        }
    }

    let mut leaf_cells = Vec::new();
    for (wood, _) in &wood_cells {
        for dx in -LEAF_ATTACH_RADIUS..=LEAF_ATTACH_RADIUS {
            for dy in -LEAF_ATTACH_RADIUS..=LEAF_ATTACH_RADIUS {
                for dz in -LEAF_ATTACH_RADIUS..=LEAF_ATTACH_RADIUS {
                    let cell = *wood + IVec3::new(dx, dy, dz);
                    let Some(voxel) = manager.get_voxel_if_loaded(cell) else {
                        continue;
                    };
                    if is_leaf_voxel(voxel) {
                        leaf_cells.push((cell, voxel));
                    }
                }
            }
        }
    }
    leaf_cells.sort_by(|(a, _), (b, _)| a.x.cmp(&b.x).then(a.y.cmp(&b.y)).then(a.z.cmp(&b.z)));
    leaf_cells.dedup_by_key(|(cell, _)| *cell);

    // Trees have trunks and canopies. Bushes are stem-only with small foliage;
    // when their stem is cut, leaves fall like a small tree. Placed beams/sticks
    // with no leaves are not felled.
    let has_trunk = wood_cells
        .iter()
        .any(|(_, voxel)| voxel.shape().is_wood_trunk());
    let is_bush = !has_trunk
        && wood_cells.iter().all(|(_, voxel)| {
            matches!(
                voxel.shape(),
                Shape::Stem | Shape::DebarkedStem | Shape::Stick
            )
        })
        && !leaf_cells.is_empty();
    if has_trunk {
        if leaf_cells.len() < 4 || wood_cells.len() < 3 {
            return None;
        }
    } else if !is_bush {
        return None;
    }

    let mut cells = wood_cells;
    for leaf in leaf_cells {
        if cells.len() >= config.max_tree_voxels {
            break;
        }
        cells.push(leaf);
    }
    Some(FellPlan { cells })
}

fn is_wood_voxel(voxel: Voxel) -> bool {
    voxel.block() == Block::Wood
        && matches!(
            voxel.shape(),
            Shape::Cube
                | Shape::Stick
                | Shape::Trunk
                | Shape::Stem
                | Shape::DebarkedTrunk
                | Shape::DebarkedStem
        )
}

fn is_leaf_voxel(voxel: Voxel) -> bool {
    voxel.block() == Block::Leaves
        && matches!(
            voxel.shape(),
            Shape::Cube | Shape::Foliage | Shape::SmallFoliage
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::chunk::{Chunk, ChunkData};
    use bevy::prelude::Entity;

    fn manager_with(data: ChunkData) -> ChunkManager {
        let mut manager = ChunkManager::default();
        manager.chunks.insert(
            data.coord,
            Chunk {
                data,
                entity: Entity::PLACEHOLDER,
                water_entity: None,
                foliage_entity: None,
                triangles: 0,
                water_triangles: 0,
                foliage_triangles: 0,
            },
        );
        manager
    }

    fn sample_tree() -> (ChunkData, IVec3) {
        let mut data = ChunkData::new(IVec3::ZERO);
        let base = IVec3::new(8, 20, 8);
        for y in 0..8 {
            let cell = base + IVec3::Y * y;
            data.set_local(
                cell.x as usize,
                cell.y as usize,
                cell.z as usize,
                Voxel::new(Block::Wood).with_shape(Shape::Trunk),
            );
        }
        // One branch and a small canopy.
        let branch = base + IVec3::new(1, 5, 0);
        data.set_local(
            branch.x as usize,
            branch.y as usize,
            branch.z as usize,
            Voxel::new(Block::Wood).with_shape(Shape::Stem),
        );
        for dx in -2..=2 {
            for dz in -2..=2 {
                for dy in 0..3 {
                    if dx * dx + dz * dz > 5 {
                        continue;
                    }
                    let cell = base + IVec3::new(dx, 7 + dy, dz);
                    data.set_local(
                        cell.x as usize,
                        cell.y as usize,
                        cell.z as usize,
                        Voxel::new(Block::Leaves).with_shape(Shape::Foliage),
                    );
                }
            }
        }
        (data, base)
    }

    #[test]
    fn isolated_wood_is_not_felled() {
        let mut data = ChunkData::new(IVec3::ZERO);
        data.set_local(8, 20, 8, Voxel::new(Block::Wood));
        let manager = manager_with(data);
        assert!(plan_felling(&manager, IVec3::new(8, 20, 8), &FellingConfig::default()).is_none());
    }

    #[test]
    fn connected_tree_is_collected() {
        let (data, base) = sample_tree();
        let manager = manager_with(data);
        let plan = plan_felling(&manager, base, &FellingConfig::default())
            .expect("a connected trunk should fell");
        assert!(plan.cells.len() > 20);
        assert!(
            plan.cells
                .iter()
                .any(|(_, voxel)| voxel.block() == Block::Leaves)
        );
        assert!(
            plan.cells
                .iter()
                .any(|(_, voxel)| voxel.shape() == Shape::Stem)
        );
    }

    #[test]
    fn bush_stems_fell_like_small_trees() {
        let mut data = ChunkData::new(IVec3::ZERO);
        let base = IVec3::new(8, 20, 8);
        data.set_local(
            base.x as usize,
            base.y as usize,
            base.z as usize,
            Voxel::new(Block::Wood).with_shape(Shape::Stem),
        );
        for (dx, dy, dz) in [(1, 1, 0), (0, 1, 1), (1, 1, 1), (0, 2, 0)] {
            let cell = base + IVec3::new(dx, dy, dz);
            data.set_local(
                cell.x as usize,
                cell.y as usize,
                cell.z as usize,
                Voxel::new(Block::Leaves).with_shape(Shape::SmallFoliage),
            );
        }
        let manager = manager_with(data);
        let plan = plan_felling(&manager, base, &FellingConfig::default())
            .expect("bush stems should fell like small trees");
        assert!(
            plan.cells
                .iter()
                .any(|(_, voxel)| voxel.block() == Block::Leaves)
        );
    }

    #[test]
    fn touching_canopies_do_not_merge_two_trees() {
        let mut data = ChunkData::new(IVec3::ZERO);
        for center in [IVec3::new(8, 20, 8), IVec3::new(14, 20, 8)] {
            for y in center.y..center.y + 8 {
                data.set_local(
                    center.x as usize,
                    y as usize,
                    center.z as usize,
                    Voxel::new(Block::Wood).with_shape(Shape::Trunk),
                );
            }
            for dy in -2..=2 {
                for dx in -3..=3 {
                    for dz in -3..=3 {
                        if dx * dx + dz * dz > 9 || (dx == 0 && dz == 0 && dy <= 0) {
                            continue;
                        }
                        let cell = center + IVec3::new(dx, 7 + dy, dz);
                        data.set_local(
                            cell.x as usize,
                            cell.y as usize,
                            cell.z as usize,
                            Voxel::new(Block::Leaves).with_shape(Shape::Foliage),
                        );
                    }
                }
            }
        }

        let manager = manager_with(data);
        let plan = plan_felling(&manager, IVec3::new(8, 20, 8), &FellingConfig::default())
            .expect("a tree with a canopy should fell");
        assert!(
            plan.cells
                .iter()
                .any(|(cell, _)| *cell == IVec3::new(8, 20, 8))
        );
        assert!(
            plan.cells
                .iter()
                .all(|(cell, _)| *cell != IVec3::new(14, 20, 8))
        );
    }

    #[test]
    fn leaf_cells_are_one_piece_per_cell() {
        let (data, base) = sample_tree();
        let manager = manager_with(data);
        let plan = plan_felling(&manager, base, &FellingConfig::default()).unwrap();
        let expected = plan
            .cells
            .iter()
            .filter(|(_, voxel)| voxel.block() == Block::Leaves)
            .count();
        let cells = leaf_cells(&plan);

        assert_eq!(cells.len(), expected);
        assert!(expected > 4);
        assert_eq!(
            cells.iter().copied().collect::<HashSet<_>>().len(),
            expected
        );
        assert!(cells.iter().all(|cell| {
            manager
                .get_voxel_if_loaded(*cell)
                .is_some_and(|voxel| voxel.block() == Block::Leaves)
        }));
    }

    #[test]
    fn wood_drops_are_batched_counts() {
        let (data, base) = sample_tree();
        let manager = manager_with(data);
        let plan = plan_felling(&manager, base, &FellingConfig::default()).unwrap();
        let wood = plan
            .cells
            .iter()
            .filter(|(_, voxel)| voxel.block() == Block::Wood && !voxel.shape().is_wood_stem())
            .count() as u32;
        let sticks = plan
            .cells
            .iter()
            .filter(|(_, voxel)| voxel.block() == Block::Wood && voxel.shape().is_wood_stem())
            .count() as u32;

        assert_eq!(
            wood_drop_counts(&plan.cells),
            vec![(ItemId::Wood, wood), (ItemId::Stick, sticks)]
        );
        assert_eq!(wood_drop_counts(&[]), Vec::new());
    }

    #[test]
    fn leaf_drop_yield_is_deterministic() {
        let cell = IVec3::new(17, 42, -9);
        assert_eq!(leaf_drop_item(cell), leaf_drop_item(cell));
    }

    #[test]
    fn leaf_timing_is_individual_and_deterministic() {
        let (data, base) = sample_tree();
        let manager = manager_with(data);
        let plan = plan_felling(&manager, base, &FellingConfig::default()).unwrap();
        let cells = leaf_cells(&plan);
        let delays: Vec<_> = cells.iter().map(|cell| leaf_fall_delay(*cell)).collect();
        let lifetimes: Vec<_> = cells
            .iter()
            .map(|cell| leaf_decay_lifetime(*cell, 1.2))
            .collect();

        assert!(
            delays
                .iter()
                .all(|delay| (0.0..=MAX_LEAF_FALL_DELAY).contains(delay))
        );
        assert!(
            lifetimes
                .iter()
                .all(|lifetime| (0.71..=1.81).contains(lifetime))
        );
        assert!(delays.iter().any(|delay| *delay != delays[0]));
        assert!(lifetimes.iter().any(|lifetime| *lifetime != lifetimes[0]));
        assert_eq!(leaf_fall_delay(cells[0]), leaf_fall_delay(cells[0]));
        assert_eq!(
            leaf_decay_lifetime(cells[0], 1.2),
            leaf_decay_lifetime(cells[0], 1.2)
        );
    }

    #[test]
    fn generated_tree_does_not_absorb_the_whole_chunk() {
        use crate::world::chunk::{CHUNK_X, CHUNK_Y, CHUNK_Z, chunk_origin};
        use crate::world::generation::WorldGen;

        let config = FellingConfig::default();
        let generator = WorldGen::new(42);
        // Terrain surfaces now sit above sea level, so trees live in the Y
        // chunks that contain those surfaces rather than always in chunk 0.
        for cy in 0..=8 {
            let data = generator.generate_chunk(IVec3::new(0, cy, 0));
            let origin = chunk_origin(data.coord).as_ivec3();
            for y in 0..CHUNK_Y {
                for z in 0..CHUNK_Z {
                    for x in 0..CHUNK_X {
                        if data.get_local(x, y, z).shape() != Shape::Trunk
                            || data.get_local(x, y.saturating_sub(1), z).block() == Block::Wood
                        {
                            continue;
                        }
                        let cell = origin + IVec3::new(x as i32, y as i32, z as i32);
                        let manager = manager_with(data.clone());
                        let Some(plan) = plan_felling(&manager, cell, &config) else {
                            continue;
                        };
                        assert!(
                            plan.cells.len() < config.max_tree_voxels,
                            "one tree should not fill the whole felling cap"
                        );
                        let leaves = leaf_cells(&plan);
                        assert!(
                            leaves.len() < 400,
                            "one canopy should not absorb most of the chunk"
                        );
                        let drops = leaves
                            .iter()
                            .filter(|cell| leaf_drop_item(**cell).is_some())
                            .count();
                        assert!(
                            drops < 80,
                            "one tree should not yield too many leaf drops, got {drops}"
                        );
                        return;
                    }
                }
            }
        }
        panic!("expected a generated tree to fell");
    }
}
