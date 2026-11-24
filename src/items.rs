//! Item definitions and the player's inventory.
//!
//! Items are defined in one table so adding a new material, resource or tool
//! does not require touching the player, UI, or interaction code. For now the
//! table is compiled in; the same shape can later be loaded from a data file.

use bevy::prelude::*;

use crate::backpack::Backpack;
use crate::pockets::Pockets;
use crate::tools::ToolKind;
use crate::world::block::Block;

/// Every item the player can carry.
///
/// The discriminant doubles as the index into [`ITEM_DEFS`], so keep the two in
/// the same order. A test verifies they stay aligned.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u16)]
pub enum ItemId {
    Turf = 0,
    Topsoil,
    Sand,
    Snow,
    Wood,
    Leaves,
    Stick,
    Fiber,
    Clay,
    Berry,
    IronOre,
    Granite,
    Slate,
    Shale,
    Sandstone,
    Limestone,
    GranitePebble,
    SlatePebble,
    SandstonePebble,
    LimestonePebble,
    Hammer,
    Wrench,
    Pickaxe,
    Axe,
    Shovel,
    Hoe,
    Scythe,
    Lantern,
    Magnet,
    Humus,
    RedSandstone,
    SharpenedStone,
    StoneTippedStick,
    Cordage,
    StoneHandAxe,
    BarkScraper,
    SmallStoneHammer,
    Bark,
    Workbench,
    Furnace,
}

impl ItemId {
    pub const ALL: [ItemId; 40] = [
        ItemId::Turf,
        ItemId::Topsoil,
        ItemId::Sand,
        ItemId::Snow,
        ItemId::Wood,
        ItemId::Leaves,
        ItemId::Stick,
        ItemId::Fiber,
        ItemId::Clay,
        ItemId::Berry,
        ItemId::IronOre,
        ItemId::Granite,
        ItemId::Slate,
        ItemId::Shale,
        ItemId::Sandstone,
        ItemId::Limestone,
        ItemId::GranitePebble,
        ItemId::SlatePebble,
        ItemId::SandstonePebble,
        ItemId::LimestonePebble,
        ItemId::Hammer,
        ItemId::Wrench,
        ItemId::Pickaxe,
        ItemId::Axe,
        ItemId::Shovel,
        ItemId::Hoe,
        ItemId::Scythe,
        ItemId::Lantern,
        ItemId::Magnet,
        ItemId::Humus,
        ItemId::RedSandstone,
        ItemId::SharpenedStone,
        ItemId::StoneTippedStick,
        ItemId::Cordage,
        ItemId::StoneHandAxe,
        ItemId::BarkScraper,
        ItemId::SmallStoneHammer,
        ItemId::Bark,
        ItemId::Workbench,
        ItemId::Furnace,
    ];

    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub fn def(self) -> &'static ItemDef {
        let def = &ITEM_DEFS[self.index()];
        debug_assert_eq!(def.id, self, "item definition table is out of sync");
        def
    }

    #[inline]
    pub fn display_name(self) -> &'static str {
        self.def().name
    }

    /// Parse an item from a chat/command-friendly name. Matching ignores case,
    /// spaces, underscores and hyphens, so `/give iron_ore` and `/give Iron Ore`
    /// both work. The old `rubble` names remain aliases for the renamed pebbles.
    pub fn from_name(name: &str) -> Option<ItemId> {
        let normalized = name
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '-'], " ")
            .replace("rubble", "pebble");
        let compact = normalized.replace(' ', "");
        if compact.is_empty() {
            return None;
        }

        Self::ALL.into_iter().find(|id| {
            let display = id.display_name().to_ascii_lowercase();
            display == normalized
                || display.replace(' ', "") == compact
                || format!("{id:?}").to_ascii_lowercase() == compact
        })
    }

    /// Colour used for the item's ground-drop cube.
    pub fn color(self) -> Color {
        if let Some(block) = self.def().place_block {
            let [r, g, b] = block.srgb();
            Color::srgb(r, g, b)
        } else {
            match self {
                ItemId::Stick | ItemId::StoneTippedStick => Color::srgb(0.55, 0.38, 0.20),
                ItemId::Fiber | ItemId::Cordage => Color::srgb(0.72, 0.74, 0.50),
                ItemId::Leaves => Color::srgb(0.28, 0.52, 0.22),
                ItemId::Berry => Color::srgb(0.74, 0.18, 0.24),
                ItemId::IronOre => Color::srgb(0.72, 0.60, 0.50),
                ItemId::GranitePebble => Color::srgb(0.56, 0.48, 0.45),
                ItemId::SlatePebble => Color::srgb(0.32, 0.36, 0.40),
                ItemId::SandstonePebble => Color::srgb(0.78, 0.66, 0.45),
                ItemId::LimestonePebble => Color::srgb(0.75, 0.72, 0.62),
                ItemId::Lantern => Color::srgb(0.92, 0.74, 0.36),
                ItemId::Magnet => Color::srgb(0.78, 0.30, 0.30),
                _ => self.def().category.color(),
            }
        }
    }

    /// The loose stone pebble item produced by a natural pebble of `block`. Pebbles
    /// are separate from the parent stone so picking one up does not look like
    /// harvesting a full building block.
    pub fn from_pebble(block: Block) -> Option<ItemId> {
        match block {
            Block::Granite => Some(ItemId::GranitePebble),
            Block::Slate => Some(ItemId::SlatePebble),
            Block::Sandstone => Some(ItemId::SandstonePebble),
            Block::Limestone => Some(ItemId::LimestonePebble),
            _ => None,
        }
    }

    /// The item produced when this block is mined.
    pub fn from_block(block: Block) -> Option<ItemId> {
        match block {
            Block::Turf => Some(ItemId::Turf),
            Block::Topsoil => Some(ItemId::Topsoil),
            Block::Sand => Some(ItemId::Sand),
            Block::Snow => Some(ItemId::Snow),
            Block::Wood => Some(ItemId::Wood),
            Block::Leaves => Some(ItemId::Leaves),
            Block::Granite => Some(ItemId::Granite),
            Block::Slate => Some(ItemId::Slate),
            Block::Shale => Some(ItemId::Shale),
            Block::Sandstone => Some(ItemId::Sandstone),
            Block::Limestone => Some(ItemId::Limestone),
            Block::Humus => Some(ItemId::Humus),
            Block::RedSandstone => Some(ItemId::RedSandstone),
            Block::Air | Block::Water | Block::Bedrock => None,
        }
    }
}

/// Broad storage categories from the backpack design.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ItemCategory {
    Food,
    Materials,
    Ores,
    Miscellaneous,
    Tools,
}

impl ItemCategory {
    pub const ALL: [ItemCategory; 5] = [
        ItemCategory::Food,
        ItemCategory::Materials,
        ItemCategory::Ores,
        ItemCategory::Miscellaneous,
        ItemCategory::Tools,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            ItemCategory::Food => "Food",
            ItemCategory::Materials => "Materials",
            ItemCategory::Ores => "Ores",
            ItemCategory::Miscellaneous => "Misc",
            ItemCategory::Tools => "Tools",
        }
    }

    pub fn from_name(name: &str) -> Option<ItemCategory> {
        let normalized = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|category| {
            category.display_name().eq_ignore_ascii_case(&normalized)
                || format!("{category:?}").eq_ignore_ascii_case(&normalized)
        })
    }

    /// Colour used to tint category storage slots later.
    pub fn color(self) -> Color {
        match self {
            ItemCategory::Food => Color::srgb(0.72, 0.48, 0.86),
            ItemCategory::Materials => Color::srgb(0.82, 0.42, 0.40),
            ItemCategory::Ores => Color::srgb(0.45, 0.78, 0.48),
            ItemCategory::Miscellaneous => Color::srgb(0.65, 0.68, 0.70),
            ItemCategory::Tools => Color::srgb(0.55, 0.68, 0.82),
        }
    }
}

/// Fallback stack limit. Every entry in [`ITEM_DEFS`] sets its own `max_stack`,
/// so heavy or rare materials can hold fewer items than light bulk goods.
pub const DEFAULT_MAX_STACK: u32 = 999;

pub struct ItemDef {
    pub id: ItemId,
    pub name: &'static str,
    /// Short description shown in the backpack details panel.
    pub description: &'static str,
    pub category: ItemCategory,
    pub max_stack: u32,
    /// Weight of one item. Shown in the backpack details panel and used by
    /// future crafting or carrying rules.
    pub weight: f32,
    /// The block this item places, if any. Non-block items leave this `None`.
    pub place_block: Option<Block>,
}

/// The single source of truth for item metadata.
pub static ITEM_DEFS: [ItemDef; 40] = [
    ItemDef {
        id: ItemId::Turf,
        name: "Turf",
        description: "Grassy surface block used for building.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.5,
        place_block: Some(Block::Turf),
    },
    ItemDef {
        id: ItemId::Topsoil,
        name: "Topsoil",
        description: "Rich soil block.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.0,
        place_block: Some(Block::Topsoil),
    },
    ItemDef {
        id: ItemId::Sand,
        name: "Sand",
        description: "Loose sand block.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.2,
        place_block: Some(Block::Sand),
    },
    ItemDef {
        id: ItemId::Snow,
        name: "Snow",
        description: "Packed snow block.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.3,
        place_block: Some(Block::Snow),
    },
    ItemDef {
        id: ItemId::Wood,
        name: "Wood",
        description: "Trunk block from trees.",
        category: ItemCategory::Materials,
        max_stack: 500,
        weight: 1.0,
        place_block: Some(Block::Wood),
    },
    ItemDef {
        id: ItemId::Leaves,
        name: "Leaves",
        description: "Foliage block from trees.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.1,
        place_block: Some(Block::Leaves),
    },
    ItemDef {
        id: ItemId::Stick,
        name: "Stick",
        description: "Light wooden stick used in crafting.",
        category: ItemCategory::Miscellaneous,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.1,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Fiber,
        name: "Fiber",
        description: "Plant fiber used in crafting.",
        category: ItemCategory::Miscellaneous,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.05,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Clay,
        name: "Clay",
        description: "Malleable clay.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.5,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Berry,
        name: "Berry",
        description: "Small edible berry.",
        category: ItemCategory::Food,
        max_stack: 99,
        weight: 0.1,
        place_block: None,
    },
    ItemDef {
        id: ItemId::IronOre,
        name: "Iron Ore",
        description: "Raw iron ore.",
        category: ItemCategory::Ores,
        max_stack: 250,
        weight: 3.0,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Granite,
        name: "Granite",
        description: "Hard igneous building stone.",
        category: ItemCategory::Materials,
        max_stack: 500,
        weight: 2.2,
        place_block: Some(Block::Granite),
    },
    ItemDef {
        id: ItemId::Slate,
        name: "Slate",
        description: "Fine-grained building stone.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 2.0,
        place_block: Some(Block::Slate),
    },
    ItemDef {
        id: ItemId::Shale,
        name: "Shale",
        description: "Layered sedimentary stone.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.8,
        place_block: Some(Block::Shale),
    },
    ItemDef {
        id: ItemId::Sandstone,
        name: "Sandstone",
        description: "Cemented sand building stone.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.6,
        place_block: Some(Block::Sandstone),
    },
    ItemDef {
        id: ItemId::Limestone,
        name: "Limestone",
        description: "Pale sedimentary building stone.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.9,
        place_block: Some(Block::Limestone),
    },
    ItemDef {
        id: ItemId::GranitePebble,
        name: "Granite Pebble",
        description: "Small granite pebble.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.4,
        place_block: None,
    },
    ItemDef {
        id: ItemId::SlatePebble,
        name: "Slate Pebble",
        description: "Small slate pebble.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.4,
        place_block: None,
    },
    ItemDef {
        id: ItemId::SandstonePebble,
        name: "Sandstone Pebble",
        description: "Small sandstone pebble.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.4,
        place_block: None,
    },
    ItemDef {
        id: ItemId::LimestonePebble,
        name: "Limestone Pebble",
        description: "Small limestone pebble.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.4,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Hammer,
        name: "Hammer",
        description: "Reshapes existing blocks without changing their material.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 2.5,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Wrench,
        name: "Wrench",
        description: "Rotates existing blocks without changing their shape.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 1.5,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Pickaxe,
        name: "Pickaxe",
        description: "Mines stone and ore.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 3.0,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Axe,
        name: "Axe",
        description: "Chops wood and leaves.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 2.8,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Shovel,
        name: "Shovel",
        description: "Digs soil and sand.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 2.0,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Hoe,
        name: "Hoe",
        description: "Prepares soil.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 2.0,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Scythe,
        name: "Scythe",
        description: "Harvests crops.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 2.2,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Lantern,
        name: "Lantern",
        description: "Warm light for caves when attached to the backpack.",
        category: ItemCategory::Miscellaneous,
        max_stack: 1,
        weight: 0.8,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Magnet,
        name: "Magnet",
        description: "Backpack attachment that draws nearby ground drops toward the player.",
        category: ItemCategory::Miscellaneous,
        max_stack: 1,
        weight: 1.0,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Humus,
        name: "Humus",
        description: "Dark organic forest soil rich in decomposed leaf litter.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.9,
        place_block: Some(Block::Humus),
    },
    ItemDef {
        id: ItemId::RedSandstone,
        name: "Red Sandstone",
        description: "Iron-oxidised sandstone that forms layered mesa caps.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 1.4,
        place_block: Some(Block::RedSandstone),
    },
    ItemDef {
        id: ItemId::SharpenedStone,
        name: "Sharpened Stone",
        description: "A stone pebble crudely chipped into a sharp edge.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.35,
        place_block: None,
    },
    ItemDef {
        id: ItemId::StoneTippedStick,
        name: "Stone Tipped Stick",
        description: "A simple hand-crafted pointed stick reinforced with stone.",
        category: ItemCategory::Miscellaneous,
        max_stack: 99,
        weight: 0.45,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Cordage,
        name: "Cordage",
        description: "Twisted plant fibers used for simple binding and lashing.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.1,
        place_block: None,
    },
    ItemDef {
        id: ItemId::StoneHandAxe,
        name: "Stone Hand Axe",
        description: "A primitive hand axe for cutting wood and small vegetation.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 0.9,
        place_block: None,
    },
    ItemDef {
        id: ItemId::BarkScraper,
        name: "Bark Scraper",
        description: "A sharp-edged scraper for removing bark from tree trunks.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 0.6,
        place_block: None,
    },
    ItemDef {
        id: ItemId::SmallStoneHammer,
        name: "Small Stone Hammer",
        description: "A primitive stone hammer for breaking natural stone deposits.",
        category: ItemCategory::Tools,
        max_stack: 1,
        weight: 1.1,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Bark,
        name: "Bark",
        description: "Tough bark stripped from a tree trunk.",
        category: ItemCategory::Materials,
        max_stack: DEFAULT_MAX_STACK,
        weight: 0.2,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Workbench,
        name: "Workbench",
        description: "A sturdy primitive wooden workbench for crafting.",
        category: ItemCategory::Miscellaneous,
        max_stack: 1,
        weight: 8.0,
        place_block: None,
    },
    ItemDef {
        id: ItemId::Furnace,
        name: "Furnace",
        description: "A primitive stone furnace. Smelting is not available yet.",
        category: ItemCategory::Miscellaneous,
        max_stack: 1,
        weight: 20.0,
        place_block: None,
    },
];

/// The tool represented by a tool item, if any. Materials and resources return
/// `None`, so hotbar handlers can branch on a single value.
pub fn tool_for_item(item: ItemId) -> Option<ToolKind> {
    match item {
        ItemId::Hammer => Some(ToolKind::Hammer),
        ItemId::Wrench => Some(ToolKind::Wrench),
        ItemId::Pickaxe => Some(ToolKind::Pickaxe),
        ItemId::Axe => Some(ToolKind::Axe),
        ItemId::Shovel => Some(ToolKind::Shovel),
        ItemId::Hoe => Some(ToolKind::Hoe),
        ItemId::Scythe => Some(ToolKind::Scythe),
        ItemId::StoneHandAxe => Some(ToolKind::StoneHandAxe),
        ItemId::BarkScraper => Some(ToolKind::BarkScraper),
        ItemId::SmallStoneHammer => Some(ToolKind::SmallStoneHammer),
        _ => None,
    }
}

/// Resolve the item in the player's selected hotbar slot.
pub fn selected_item(pockets: &Pockets) -> Option<ItemId> {
    pockets.selected_stack().map(|stack| stack.item)
}

/// Resolve the tool represented by the currently selected hotbar slot.
pub fn selected_tool(pockets: &Pockets) -> Option<ToolKind> {
    selected_item(pockets).and_then(tool_for_item)
}

/// Total count of an item across pockets and backpack storage.
/// Backpack attachments are separate physical slots and not included.
pub fn inventory_count(item: ItemId, pockets: &Pockets, backpack: &Backpack) -> u32 {
    pockets.count(item) + backpack.count(item)
}

/// Route items into backpack storage first, then the hotbar when storage is
/// full. Returns the amount that did not fit anywhere.
pub fn store_item(pockets: &mut Pockets, backpack: &mut Backpack, item: ItemId, count: u32) -> u32 {
    let leftover = backpack.add(item, count);
    pockets.add(item, leftover)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemStack {
    pub item: ItemId,
    pub count: u32,
}

pub struct ItemsPlugin;

impl Plugin for ItemsPlugin {
    fn build(&self, app: &mut App) {
        debug_assert_eq!(ITEM_DEFS.len(), ItemId::ALL.len());
        let _ = app;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_table_matches_enum_order() {
        assert_eq!(ITEM_DEFS.len(), ItemId::ALL.len());
        for (index, id) in ItemId::ALL.into_iter().enumerate() {
            assert_eq!(id.index(), index);
            assert_eq!(ITEM_DEFS[index].id, id);
        }
    }

    #[test]
    fn placeable_items_map_to_blocks() {
        assert_eq!(ItemId::Granite.def().place_block, Some(Block::Granite));
        assert_eq!(ItemId::Stick.def().place_block, None);
        assert_eq!(ItemId::from_block(Block::Turf), Some(ItemId::Turf));
        assert_eq!(ItemId::from_block(Block::Topsoil), Some(ItemId::Topsoil));
        assert_eq!(ItemId::from_block(Block::Bedrock), None);
        assert_eq!(ItemId::GranitePebble.def().place_block, None);
    }

    #[test]
    fn pebbles_map_to_pebble_items() {
        assert_eq!(ItemId::from_pebble(Block::Slate), Some(ItemId::SlatePebble));
        assert_eq!(
            ItemId::from_pebble(Block::Limestone),
            Some(ItemId::LimestonePebble)
        );
        assert_eq!(ItemId::from_pebble(Block::Wood), None);
        assert_eq!(ItemId::from_name("Slate Pebble"), Some(ItemId::SlatePebble));
        // Legacy `rubble` commands remain aliases.
        assert_eq!(ItemId::from_name("Slate Rubble"), Some(ItemId::SlatePebble));
        assert_eq!(ItemId::from_name("slate_rubble"), Some(ItemId::SlatePebble));
    }

    #[test]
    fn item_names_are_parsed_flexibly() {
        assert_eq!(ItemId::from_name("granite"), Some(ItemId::Granite));
        assert_eq!(ItemId::from_name("Iron Ore"), Some(ItemId::IronOre));
        assert_eq!(ItemId::from_name("iron_ore"), Some(ItemId::IronOre));
        assert_eq!(ItemId::from_name("hammer"), Some(ItemId::Hammer));
        assert_eq!(ItemId::from_name("wrench"), Some(ItemId::Wrench));
        assert_eq!(ItemId::from_name("lantern"), Some(ItemId::Lantern));
        assert_eq!(ItemId::from_name("Lantern"), Some(ItemId::Lantern));
        assert_eq!(ItemId::from_name("magnet"), Some(ItemId::Magnet));
        assert_eq!(ItemId::from_name("Magnet"), Some(ItemId::Magnet));
        assert_eq!(ItemId::from_name("nonsense"), None);
    }

    #[test]
    fn categories_have_stable_metadata() {
        assert_eq!(ItemId::Granite.def().category, ItemCategory::Materials);
        assert_eq!(ItemId::IronOre.def().category, ItemCategory::Ores);
        assert_eq!(
            ItemCategory::from_name("materials"),
            Some(ItemCategory::Materials)
        );
        assert_eq!(ItemCategory::from_name("ores"), Some(ItemCategory::Ores));
        assert_eq!(ItemCategory::from_name("tools"), Some(ItemCategory::Tools));
        assert_eq!(ItemCategory::from_name("unknown"), None);
    }

    #[test]
    fn every_item_has_a_non_empty_description() {
        for item in ItemId::ALL {
            assert!(
                !item.def().description.trim().is_empty(),
                "{item:?} has no description"
            );
        }
    }

    #[test]
    fn storing_pebble_pickups_stacks_correct_counts() {
        use crate::backpack::Backpack;
        use crate::pockets::Pockets;

        for count in 1..=3u32 {
            let mut pockets = Pockets::default();
            let mut backpack = Backpack::default();
            let leftover = store_item(&mut pockets, &mut backpack, ItemId::GranitePebble, count);
            assert_eq!(leftover, 0);
            assert_eq!(
                inventory_count(ItemId::GranitePebble, &pockets, &backpack),
                count
            );
            let pocket_stacks = pockets
                .pockets()
                .iter()
                .filter(|pocket| !pocket.is_empty())
                .count();
            let backpack_stacks = backpack
                .slots()
                .iter()
                .filter(|slot| slot.is_some())
                .count();
            assert_eq!(
                pocket_stacks + backpack_stacks,
                1,
                "count {count} should stack into one slot"
            );
        }
    }

    #[test]
    fn every_item_has_a_positive_weight() {
        for item in ItemId::ALL {
            assert!(item.def().weight > 0.0, "{:?} has no weight", item);
        }
    }

    #[test]
    fn stack_limits_are_configurable_per_item() {
        assert_eq!(DEFAULT_MAX_STACK, 999);
        for item in ItemId::ALL {
            assert!(item.def().max_stack > 0, "{item:?} has no stack limit");
        }

        // Heavy and rare goods intentionally hold less than light bulk goods.
        assert_eq!(ItemId::Granite.def().max_stack, 500);
        assert_eq!(ItemId::Wood.def().max_stack, 500);
        assert_eq!(ItemId::IronOre.def().max_stack, 250);
        assert_eq!(ItemId::Berry.def().max_stack, 99);
        assert_eq!(ItemId::Topsoil.def().max_stack, DEFAULT_MAX_STACK);

        let limits: std::collections::BTreeSet<u32> = ItemId::ALL
            .iter()
            .map(|item| item.def().max_stack)
            .collect();
        assert!(limits.len() > 1, "items should not all share one limit");
    }

    #[test]
    fn tools_are_single_stack_items_that_do_not_place_blocks() {
        for item in [
            ItemId::Hammer,
            ItemId::Wrench,
            ItemId::Pickaxe,
            ItemId::Axe,
            ItemId::Shovel,
            ItemId::Hoe,
            ItemId::Scythe,
            ItemId::StoneHandAxe,
            ItemId::BarkScraper,
            ItemId::SmallStoneHammer,
        ] {
            let def = item.def();
            assert_eq!(def.category, ItemCategory::Tools, "{item:?}");
            assert_eq!(def.max_stack, 1, "{item:?}");
            assert_eq!(def.place_block, None, "{item:?}");
        }
    }

    #[test]
    fn tool_items_map_to_tool_kinds() {
        assert_eq!(tool_for_item(ItemId::Hammer), Some(ToolKind::Hammer));
        assert_eq!(tool_for_item(ItemId::Wrench), Some(ToolKind::Wrench));
        assert_eq!(tool_for_item(ItemId::Pickaxe), Some(ToolKind::Pickaxe));
        assert_eq!(tool_for_item(ItemId::Axe), Some(ToolKind::Axe));
        assert_eq!(tool_for_item(ItemId::Shovel), Some(ToolKind::Shovel));
        assert_eq!(tool_for_item(ItemId::Hoe), Some(ToolKind::Hoe));
        assert_eq!(tool_for_item(ItemId::Scythe), Some(ToolKind::Scythe));
        assert_eq!(
            tool_for_item(ItemId::StoneHandAxe),
            Some(ToolKind::StoneHandAxe)
        );
        assert_eq!(
            tool_for_item(ItemId::BarkScraper),
            Some(ToolKind::BarkScraper)
        );
        assert_eq!(
            tool_for_item(ItemId::SmallStoneHammer),
            Some(ToolKind::SmallStoneHammer)
        );
        assert_eq!(tool_for_item(ItemId::Granite), None);
        assert_eq!(tool_for_item(ItemId::Stick), None);
        assert_eq!(tool_for_item(ItemId::Lantern), None);
        assert_eq!(tool_for_item(ItemId::Magnet), None);
        assert_eq!(tool_for_item(ItemId::Bark), None);
    }

    #[test]
    fn attachment_items_are_carryable_non_tools() {
        for item in [ItemId::Lantern, ItemId::Magnet] {
            let def = item.def();
            assert_eq!(def.category, ItemCategory::Miscellaneous, "{item:?}");
            assert_eq!(def.max_stack, 1, "{item:?}");
            assert_eq!(def.place_block, None, "{item:?}");
            assert_eq!(tool_for_item(item), None, "{item:?}");
        }
        assert_eq!(ItemId::Lantern.display_name(), "Lantern");
        assert_eq!(ItemId::Magnet.display_name(), "Magnet");
    }

    #[test]
    fn selected_item_comes_from_the_selected_hotbar_slot() {
        let mut pockets = Pockets::default();
        assert_eq!(selected_item(&pockets), None);

        pockets.add(ItemId::Hammer, 1);
        assert_eq!(selected_item(&pockets), Some(ItemId::Hammer));

        pockets.select(1);
        pockets.add(ItemId::Pickaxe, 1);
        assert_eq!(selected_item(&pockets), Some(ItemId::Pickaxe));

        pockets.select(2);
        assert_eq!(selected_item(&pockets), None);
    }

    #[test]
    fn selected_tool_returns_tool_for_selected_slot() {
        use crate::tools::ToolKind;

        let mut pockets = Pockets::default();
        assert_eq!(selected_tool(&pockets), None);

        pockets.replace(
            0,
            Some(ItemStack {
                item: ItemId::Pickaxe,
                count: 1,
            }),
        );
        pockets.select(0);
        assert_eq!(selected_tool(&pockets), Some(ToolKind::Pickaxe));

        pockets.replace(
            0,
            Some(ItemStack {
                item: ItemId::Granite,
                count: 1,
            }),
        );
        assert_eq!(selected_tool(&pockets), None);

        pockets.replace(0, None);
        assert_eq!(selected_tool(&pockets), None);
    }

    #[test]
    fn inventory_count_sums_pockets_and_backpack_storage() {
        use crate::backpack::Backpack;

        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        pockets.replace(
            0,
            Some(ItemStack {
                item: ItemId::GranitePebble,
                count: 2,
            }),
        );
        let leftover = backpack.add(ItemId::GranitePebble, 3);
        assert_eq!(leftover, 0);

        assert_eq!(
            inventory_count(ItemId::GranitePebble, &pockets, &backpack),
            5
        );
        assert_eq!(inventory_count(ItemId::Stick, &pockets, &backpack), 0);
    }
}
