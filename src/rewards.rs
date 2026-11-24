//! Data-driven mining rewards.
//!
//! A 1 m terrain voxel is mined as eight 0.5 m detail cells but drops one
//! physical resource item. Detail voxels (trees, bushes, player-placed panels
//! and player-built pieces) drop one item. Keeping this table separate from
//! interaction makes the economy easy to rebalance and test without a game.

use crate::items::ItemId;
use crate::world::block::{Block, Shape, Voxel, pebble_variant_count};
use crate::world::chunk::SurfaceLayer;

/// The complete yield of breaking one voxel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiningReward {
    pub item: Option<ItemId>,
    pub item_count: u32,
}

impl MiningReward {
    pub const NONE: Self = Self {
        item: None,
        item_count: 0,
    };

    #[inline]
    pub fn is_empty(self) -> bool {
        self.item_count == 0
    }
}

/// Yield for a 1 m terrain voxel. Every solid, non-water material drops one
/// physical resource item. Leaves/fiber harvesting is currently fiber-only
/// regardless of tool; leaf-block shears can be added later as a separate tool.
pub fn world_voxel_reward(block: Block) -> MiningReward {
    if block.is_air() || block.is_water() || matches!(block, Block::Bedrock) {
        return MiningReward::NONE;
    }
    if block == Block::Leaves {
        return MiningReward {
            item: Some(ItemId::Fiber),
            item_count: 1,
        };
    }

    let item = ItemId::from_block(block);
    MiningReward {
        item,
        item_count: u32::from(item.is_some()),
    }
}

/// Yield for a 1 m terrain voxel capped by a natural surface. The rock or soil
/// stays in the world layer, so harvesting the visible layer hands the player
/// the matching resource instead of the underlying material.
pub fn world_surface_reward(block: Block, surface: Option<SurfaceLayer>) -> MiningReward {
    let reward = world_voxel_reward(block);
    let Some(layer) = surface else {
        return reward;
    };
    if reward.is_empty() {
        return reward;
    }
    MiningReward {
        item: Some(surface_item(layer)),
        item_count: 1,
    }
}

#[inline]
fn surface_item(layer: SurfaceLayer) -> ItemId {
    match layer {
        SurfaceLayer::Turf => ItemId::Turf,
        SurfaceLayer::Snow => ItemId::Snow,
    }
}

/// Yield for harvesting only a natural surface from the top of a world voxel.
/// The world voxel stays in place.
#[inline]
pub fn natural_surface_harvest_reward(layer: SurfaceLayer) -> MiningReward {
    MiningReward {
        item: Some(surface_item(layer)),
        item_count: 1,
    }
}

/// Progress granted when one unit of an item is collected.
///
/// Keeping economy values beside the mining tables means balancing a material
/// never requires touching the progression or UI code. Tools are intentionally
/// worth nothing: they are starting equipment, not a harvestable resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickupReward {
    pub xp: u32,
    pub coins: u32,
}

impl PickupReward {
    pub const NONE: Self = Self { xp: 0, coins: 0 };

    #[inline]
    pub fn is_empty(self) -> bool {
        self.xp == 0 && self.coins == 0
    }
}

/// One row per [`ItemId`]. The discriminants double as the indices here; the
/// table length is covered by a test.
pub static PICKUP_REWARDS: [PickupReward; 40] = [
    PickupReward { xp: 1, coins: 0 }, // Turf
    PickupReward { xp: 1, coins: 0 }, // Topsoil
    PickupReward { xp: 1, coins: 0 }, // Sand
    PickupReward { xp: 1, coins: 0 }, // Snow
    PickupReward { xp: 2, coins: 1 }, // Wood
    PickupReward { xp: 1, coins: 0 }, // Leaves
    PickupReward { xp: 1, coins: 0 }, // Stick
    PickupReward { xp: 2, coins: 1 }, // Fiber
    PickupReward { xp: 2, coins: 1 }, // Clay
    PickupReward { xp: 3, coins: 2 }, // Berry
    PickupReward { xp: 5, coins: 3 }, // IronOre
    PickupReward { xp: 3, coins: 2 }, // Granite
    PickupReward { xp: 3, coins: 2 }, // Slate
    PickupReward { xp: 3, coins: 2 }, // Shale
    PickupReward { xp: 3, coins: 2 }, // Sandstone
    PickupReward { xp: 3, coins: 2 }, // Limestone
    PickupReward { xp: 1, coins: 0 }, // GranitePebble
    PickupReward { xp: 1, coins: 0 }, // SlatePebble
    PickupReward { xp: 1, coins: 0 }, // SandstonePebble
    PickupReward { xp: 1, coins: 0 }, // LimestonePebble
    PickupReward::NONE,               // Hammer
    PickupReward::NONE,               // Wrench
    PickupReward::NONE,               // Pickaxe
    PickupReward::NONE,               // Axe
    PickupReward::NONE,               // Shovel
    PickupReward::NONE,               // Hoe
    PickupReward::NONE,               // Scythe
    PickupReward::NONE,               // Lantern
    PickupReward::NONE,               // Magnet
    PickupReward { xp: 1, coins: 0 }, // Humus
    PickupReward { xp: 3, coins: 2 }, // Red Sandstone
    PickupReward { xp: 1, coins: 0 }, // Sharpened Stone
    PickupReward { xp: 2, coins: 1 }, // Stone Tipped Stick
    PickupReward { xp: 2, coins: 1 }, // Cordage
    PickupReward::NONE,               // Stone Hand Axe
    PickupReward::NONE,               // Bark Scraper
    PickupReward::NONE,               // Small Stone Hammer
    PickupReward { xp: 1, coins: 0 }, // Bark
    PickupReward::NONE,               // Workbench
    PickupReward::NONE,               // Furnace
];

/// Total progress for collecting `count` of `item`.
pub fn pickup_reward(item: ItemId, count: u32) -> PickupReward {
    let unit = PICKUP_REWARDS[item.index()];
    PickupReward {
        xp: unit.xp.saturating_mul(count),
        coins: unit.coins.saturating_mul(count),
    }
}

/// Yield for a 0.5 m detail voxel. Natural objects and construction pieces
/// return one item. Shape-aware because a natural stick is stored as wood but
/// should drop a stick. Pebble piles are one pickup but yield as many items as
/// there are stones in the pile model.
pub fn detail_voxel_reward(voxel: Voxel) -> MiningReward {
    let block = voxel.block();
    if block.is_air() || block.is_water() || matches!(block, Block::Bedrock) {
        return MiningReward::NONE;
    }

    let (item, item_count) = match voxel.shape() {
        Shape::Stick | Shape::Stem | Shape::DebarkedStem => (Some(ItemId::Stick), 1),
        Shape::Pebble => (
            ItemId::from_pebble(block).or_else(|| ItemId::from_block(block)),
            u32::from(pebble_variant_count(voxel.orientation())),
        ),
        Shape::StoneDeposit => (ItemId::from_block(block).or(Some(ItemId::Granite)), 1),
        _ if block == Block::Leaves => (Some(ItemId::Fiber), 1),
        _ => (ItemId::from_block(block), 1),
    };

    MiningReward {
        item,
        item_count: if item.is_some() { item_count } else { 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_world_voxel_yields_one_item() {
        let reward = world_voxel_reward(Block::Granite);
        assert_eq!(reward.item, Some(ItemId::Granite));
        assert_eq!(reward.item_count, 1);
    }

    #[test]
    fn every_buildable_world_material_has_a_reward() {
        for block in Block::BUILDABLE {
            let reward = world_voxel_reward(block);
            assert!(reward.item.is_some(), "{block:?} has no item reward");
            assert_eq!(reward.item_count, 1);
        }
    }

    #[test]
    fn empty_and_unbreakable_blocks_reward_nothing() {
        for block in [Block::Air, Block::Water, Block::Bedrock] {
            assert!(world_voxel_reward(block).is_empty());
            assert!(detail_voxel_reward(Voxel::new(block)).is_empty());
        }
    }

    #[test]
    fn detail_objects_drop_their_item() {
        let reward = detail_voxel_reward(Voxel::new(Block::Wood));
        assert_eq!(reward.item, Some(ItemId::Wood));
        assert_eq!(reward.item_count, 1);
    }

    #[test]
    fn leaves_drop_fiber_regardless_of_shape() {
        for shape in [Shape::Foliage, Shape::SmallFoliage, Shape::Cube] {
            let leaves = Voxel::new(Block::Leaves).with_shape(shape);
            let reward = detail_voxel_reward(leaves);
            assert_eq!(reward.item, Some(ItemId::Fiber), "{shape:?}");
            assert_eq!(reward.item_count, 1);
        }
    }

    #[test]
    fn pickup_rewards_stay_aligned_with_items() {
        assert_eq!(PICKUP_REWARDS.len(), ItemId::ALL.len());
        for item in ItemId::ALL {
            let reward = pickup_reward(item, 3);
            let unit = PICKUP_REWARDS[item.index()];
            assert_eq!(reward.xp, unit.xp * 3);
            assert_eq!(reward.coins, unit.coins * 3);
        }
        assert!(!pickup_reward(ItemId::Berry, 1).is_empty());
        assert!(pickup_reward(ItemId::Hammer, 1).is_empty());
        assert!(pickup_reward(ItemId::Lantern, 1).is_empty());
        assert!(pickup_reward(ItemId::Magnet, 1).is_empty());
    }

    #[test]
    fn turf_harvest_yields_one_turf() {
        let reward = natural_surface_harvest_reward(SurfaceLayer::Turf);
        assert_eq!(reward.item, Some(ItemId::Turf));
        assert_eq!(reward.item_count, 1);
        assert!(!reward.is_empty());

        let snow = natural_surface_harvest_reward(SurfaceLayer::Snow);
        assert_eq!(snow.item, Some(ItemId::Snow));
    }

    #[test]
    fn natural_sticks_drop_sticks_and_grass_drops_turf() {
        let stick = Voxel::new(Block::Wood).with_shape(Shape::Stick);
        assert_eq!(detail_voxel_reward(stick).item, Some(ItemId::Stick));

        let pebble = Voxel::new(Block::Slate).with_shape(Shape::Pebble);
        let reward = detail_voxel_reward(pebble);
        assert_eq!(reward.item, Some(ItemId::SlatePebble));
        assert_eq!(reward.item_count, 1);

        let grass = world_surface_reward(Block::Topsoil, Some(SurfaceLayer::Turf));
        assert_eq!(grass.item, Some(ItemId::Turf));
        assert_eq!(grass.item_count, 1);

        let snow = world_surface_reward(Block::Granite, Some(SurfaceLayer::Snow));
        assert_eq!(snow.item, Some(ItemId::Snow));

        let bare = world_surface_reward(Block::Topsoil, None);
        assert_eq!(bare.item, Some(ItemId::Topsoil));
    }

    #[test]
    fn pebble_pile_variants_yield_correct_pickup_quantities() {
        use crate::world::block::Orientation;

        for index in 0..12u8 {
            let orientation = Orientation::from_index(index);
            let voxel = Voxel::from_oriented_parts(Block::Granite, Shape::Pebble, orientation);
            let reward = detail_voxel_reward(voxel);
            assert_eq!(reward.item, Some(ItemId::GranitePebble));
            let expected = u32::from(pebble_variant_count(orientation));
            assert_eq!(reward.item_count, expected, "variant {index}");
            assert!((1..=3).contains(&expected));
        }
    }
}
