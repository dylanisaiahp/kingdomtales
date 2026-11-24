//! Tool definitions and mining roles.
//!
//! Tools are inventory items; the selected hotbar stack decides which one is
//! active. This module only defines what each tool is and how it behaves.

use crate::world::block::Block;

/// Every tool type the game knows about.
///
/// Adding a tool means adding a variant, a [`ToolDef`] entry and its role in
/// [`ToolKind::mining_time`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ToolKind {
    Pickaxe,
    Axe,
    Hammer,
    Shovel,
    Hoe,
    Scythe,
    Wrench,
    StoneHandAxe,
    BarkScraper,
    SmallStoneHammer,
}

impl ToolKind {
    #[inline]
    pub fn def(self) -> &'static ToolDef {
        let index = self as usize;
        let def = &TOOL_DEFS[index];
        debug_assert_eq!(def.kind, self, "tool definition table is out of sync");
        def
    }

    #[inline]
    pub fn display_name(self) -> &'static str {
        self.def().name
    }

    /// The hammer changes block geometry only.
    #[inline]
    pub fn is_hammer(self) -> bool {
        matches!(self, ToolKind::Hammer)
    }

    /// The wrench changes block orientation only.
    #[inline]
    pub fn is_wrench(self) -> bool {
        matches!(self, ToolKind::Wrench)
    }

    /// Axes, including primitive stone axes, can chop wood and dismantle
    /// wooden structures.
    #[inline]
    pub fn is_axe(self) -> bool {
        matches!(self, ToolKind::Axe | ToolKind::StoneHandAxe)
    }

    /// Seconds needed to break `block` with this tool, or `None` when the tool
    /// is not suitable. This is where tool roles are defined.
    pub fn mining_time(self, block: Block) -> Option<f32> {
        let base = block.break_time()?;
        match self {
            ToolKind::Pickaxe
                if matches!(
                    block,
                    Block::Granite
                        | Block::Bedrock
                        | Block::Slate
                        | Block::Shale
                        | Block::Sandstone
                        | Block::Limestone
                        | Block::RedSandstone
                ) =>
            {
                Some(base)
            }
            ToolKind::Axe if matches!(block, Block::Wood | Block::Leaves) => Some(base * 0.85),
            // Primitive stone axe: slower than the proper axe, but can cut wood
            // and thin vegetation in the early game.
            ToolKind::StoneHandAxe if matches!(block, Block::Wood | Block::Leaves) => {
                Some(base * 2.0)
            }
            ToolKind::Shovel
                if matches!(
                    block,
                    Block::Topsoil | Block::Turf | Block::Sand | Block::Snow | Block::Humus
                ) =>
            {
                Some(base * 0.9)
            }
            ToolKind::Hoe if matches!(block, Block::Turf | Block::Topsoil | Block::Humus) => {
                Some(base * 1.2)
            }
            ToolKind::Scythe if matches!(block, Block::Leaves) => Some(base * 0.7),
            // Bark scraper is specialized: it removes bark from tree trunks via
            // interaction handling, not general mining.
            ToolKind::BarkScraper => None,
            // Primitive stone hammer mines natural stone deposits only; it does
            // not mine normal stone terrain/voxels.
            ToolKind::SmallStoneHammer => None,
            _ => None,
        }
    }

    #[inline]
    pub fn can_mine(self, block: Block) -> bool {
        self.mining_time(block).is_some()
    }
}

/// Tools that can break `block`, in the order shown in Insight hints. This is
/// the inverse of [`ToolKind::mining_time`] and keeps gameplay rules and HUD
/// hints from drifting apart.
pub fn required_tools(block: Block) -> &'static [ToolKind] {
    match block {
        Block::Turf | Block::Topsoil | Block::Humus => &[ToolKind::Shovel, ToolKind::Hoe],
        Block::Sand | Block::Snow => &[ToolKind::Shovel],
        Block::Wood => &[ToolKind::StoneHandAxe, ToolKind::Axe],
        Block::Leaves => &[ToolKind::StoneHandAxe, ToolKind::Axe, ToolKind::Scythe],
        Block::Granite
        | Block::Slate
        | Block::Shale
        | Block::Sandstone
        | Block::Limestone
        | Block::RedSandstone => &[ToolKind::Pickaxe],
        _ => &[],
    }
}

pub struct ToolDef {
    pub kind: ToolKind,
    pub name: &'static str,
}

/// Single source of truth for tool names.
pub static TOOL_DEFS: [ToolDef; 10] = [
    ToolDef {
        kind: ToolKind::Pickaxe,
        name: "Pickaxe",
    },
    ToolDef {
        kind: ToolKind::Axe,
        name: "Axe",
    },
    ToolDef {
        kind: ToolKind::Hammer,
        name: "Hammer",
    },
    ToolDef {
        kind: ToolKind::Shovel,
        name: "Shovel",
    },
    ToolDef {
        kind: ToolKind::Hoe,
        name: "Hoe",
    },
    ToolDef {
        kind: ToolKind::Scythe,
        name: "Scythe",
    },
    ToolDef {
        kind: ToolKind::Wrench,
        name: "Wrench",
    },
    ToolDef {
        kind: ToolKind::StoneHandAxe,
        name: "Stone Hand Axe",
    },
    ToolDef {
        kind: ToolKind::BarkScraper,
        name: "Bark Scraper",
    },
    ToolDef {
        kind: ToolKind::SmallStoneHammer,
        name: "Small Stone Hammer",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_table_matches_enum_order() {
        assert_eq!(TOOL_DEFS.len(), 10);
        for (index, def) in TOOL_DEFS.iter().enumerate() {
            assert_eq!(def.kind as usize, index);
        }
    }

    #[test]
    fn tools_only_mine_their_own_materials() {
        assert!(ToolKind::Pickaxe.can_mine(Block::Granite));
        assert!(!ToolKind::Pickaxe.can_mine(Block::Wood));
        assert!(ToolKind::Axe.can_mine(Block::Wood));
        assert!(ToolKind::StoneHandAxe.can_mine(Block::Wood));
        assert!(ToolKind::StoneHandAxe.can_mine(Block::Leaves));
        assert!(!ToolKind::StoneHandAxe.can_mine(Block::Granite));
        assert!(!ToolKind::BarkScraper.can_mine(Block::Wood));
        assert!(!ToolKind::SmallStoneHammer.can_mine(Block::Granite));
        assert!(ToolKind::Shovel.can_mine(Block::Topsoil));
        assert!(!ToolKind::Hammer.can_mine(Block::Granite));
        assert!(!ToolKind::Wrench.can_mine(Block::Granite));
        assert!(!ToolKind::Pickaxe.can_mine(Block::Bedrock));
    }

    #[test]
    fn required_tools_match_mining_roles() {
        let blocks = [
            Block::Turf,
            Block::Topsoil,
            Block::Sand,
            Block::Snow,
            Block::Wood,
            Block::Leaves,
            Block::Granite,
            Block::Slate,
            Block::Shale,
            Block::Sandstone,
            Block::Limestone,
            Block::Humus,
            Block::RedSandstone,
            Block::Air,
            Block::Water,
            Block::Bedrock,
        ];
        for block in blocks {
            let allowed = required_tools(block);
            let any_can_mine = [
                ToolKind::Pickaxe,
                ToolKind::Axe,
                ToolKind::Hammer,
                ToolKind::Shovel,
                ToolKind::Hoe,
                ToolKind::Scythe,
                ToolKind::Wrench,
            ]
            .into_iter()
            .any(|tool| tool.can_mine(block));
            assert_eq!(!allowed.is_empty(), any_can_mine, "{block:?}");
            for tool in allowed {
                assert!(tool.can_mine(block), "{tool:?} on {block:?}");
            }
        }
    }

    #[test]
    fn the_pickaxe_mines_every_geological_material() {
        for block in [
            Block::Granite,
            Block::Granite,
            Block::Slate,
            Block::Shale,
            Block::Sandstone,
            Block::Limestone,
            Block::RedSandstone,
        ] {
            assert!(ToolKind::Pickaxe.can_mine(block), "{block:?}");
        }
    }

    #[test]
    fn geometry_and_orientation_tools_have_distinct_roles() {
        assert!(ToolKind::Hammer.is_hammer());
        assert!(!ToolKind::Hammer.is_wrench());
        assert!(ToolKind::Wrench.is_wrench());
        assert!(!ToolKind::Wrench.is_hammer());
    }
}
