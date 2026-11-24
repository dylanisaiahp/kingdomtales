//! Contextual target information for the HUD.
//!
//! Insight is the player-facing name for the WAILA-style panel. This module
//! gathers the facts from world/player state; the UI module only decides how to
//! draw them. The data shape is deliberately generic so resources, objects and
//! future NPCs can all use the same panel.

use bevy::prelude::*;

use crate::interaction::PickupPrompt;
use crate::items::{ItemId, selected_tool};
use crate::player::{CrosshairTarget, CursorCapture};
use crate::pockets::Pockets;
use crate::tools::{ToolKind, required_tools};
use crate::world::ChunkManager;
use crate::world::block::{Block, Shape, Voxel};

/// The simplified Insight panel only answers two questions: what am I looking
/// at, and what do I need to do to it.
#[derive(Resource, Default, Clone, Debug)]
pub struct InsightInfo {
    pub visible: bool,
    pub title: String,
    pub action: String,
}

/// Broad material family shown beside object names. Kept here rather than on
/// `Block` because it is presentation metadata, not voxel behaviour.
pub fn material_group(block: Block) -> Option<&'static str> {
    match block {
        Block::Air => None,
        Block::Turf | Block::Topsoil | Block::Humus => Some("Soil"),
        Block::Granite
        | Block::Slate
        | Block::Shale
        | Block::Sandstone
        | Block::Limestone
        | Block::RedSandstone => Some("Mineral"),
        Block::Sand => Some("Sand"),
        Block::Snow => Some("Snow"),
        Block::Water => Some("Liquid"),
        Block::Wood => Some("Wood"),
        Block::Leaves => Some("Foliage"),
        Block::Bedrock => Some("Bedrock"),
    }
}

fn loose_shape(voxel: Voxel) -> Option<String> {
    match voxel.shape() {
        Shape::Stick => Some("Stick".to_string()),
        Shape::Pebble => Some(
            ItemId::from_pebble(voxel.block())
                .map(|item| item.display_name().to_string())
                .unwrap_or_else(|| format!("{} Pebble", voxel.block().display_name())),
        ),
        _ => None,
    }
}

fn action_verb(block: Block) -> &'static str {
    match block {
        Block::Wood | Block::Leaves => "chop",
        Block::Topsoil | Block::Turf | Block::Humus | Block::Sand | Block::Snow => "dig",
        _ => "mine",
    }
}

fn block_action(block: Block, selected: Option<ToolKind>) -> String {
    let required = required_tools(block);
    if required.is_empty() {
        return String::new();
    }
    let verb = action_verb(block);
    if selected.is_some_and(|tool| tool.can_mine(block)) {
        return format!("LMB {verb}");
    }
    let names = required
        .iter()
        .map(|tool| tool.display_name())
        .collect::<Vec<_>>()
        .join(" or ");
    format!("Need {names}")
}

pub struct InsightPlugin;

impl Plugin for InsightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InsightInfo>()
            .add_systems(Update, update_insight);
    }
}

fn update_insight(
    capture: Res<CursorCapture>,
    crosshair: Res<CrosshairTarget>,
    manager: Res<ChunkManager>,
    pockets: Res<Pockets>,
    prompt: Res<PickupPrompt>,
    mut insight: ResMut<InsightInfo>,
) {
    insight.visible = false;
    insight.title.clear();
    insight.action.clear();

    if !capture.grabbed || capture.just_grabbed {
        return;
    }

    if prompt.visible {
        let Some(item) = prompt.item else {
            return;
        };
        insight.visible = true;
        insight.title = item.display_name().to_string();
        insight.action = "RMB pickup".to_string();
        return;
    }

    let Some(hit) = crosshair.hit else {
        return;
    };
    let Some(voxel) = manager.get_voxel_if_loaded(hit.block) else {
        return;
    };
    let block = voxel.block();
    if material_group(block).is_none() {
        return;
    }

    insight.visible = true;
    if let Some(loose) = loose_shape(voxel) {
        insight.title = loose.to_string();
        insight.action = "RMB gather".to_string();
    } else {
        insight.title = match voxel.shape() {
            Shape::StoneDeposit => "Stone Deposit".to_string(),
            Shape::Stem | Shape::DebarkedStem if block == Block::Wood => "Wood Limb".to_string(),
            Shape::Trunk | Shape::DebarkedTrunk if block == Block::Wood => "Tree Trunk".to_string(),
            Shape::Stem | Shape::DebarkedStem => block.display_name().to_string(),
            _ => block.display_name().to_string(),
        };
        let selected = selected_tool(&pockets);
        insight.action = if voxel.shape() == Shape::StoneDeposit {
            if selected == Some(ToolKind::SmallStoneHammer) {
                "LMB mine".to_string()
            } else {
                "Need Small Stone Hammer".to_string()
            }
        } else {
            block_action(block, selected)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_groups_cover_every_block() {
        for block in [
            Block::Air,
            Block::Turf,
            Block::Topsoil,
            Block::Granite,
            Block::Sand,
            Block::Snow,
            Block::Water,
            Block::Wood,
            Block::Leaves,
            Block::Bedrock,
            Block::Slate,
            Block::Shale,
            Block::Sandstone,
            Block::Limestone,
        ] {
            assert_eq!(material_group(block).is_none(), block == Block::Air);
        }
        assert_eq!(material_group(Block::Wood), Some("Wood"));
        assert_eq!(material_group(Block::Granite), Some("Mineral"));
    }

    #[test]
    fn block_actions_name_the_required_tool() {
        assert_eq!(
            block_action(Block::Wood, None),
            "Need Stone Hand Axe or Axe"
        );
        assert_eq!(block_action(Block::Granite, None), "Need Pickaxe");
        assert_eq!(block_action(Block::Topsoil, None), "Need Shovel or Hoe");
        assert_eq!(block_action(Block::Wood, Some(ToolKind::Axe)), "LMB chop");
        assert_eq!(
            block_action(Block::Wood, Some(ToolKind::StoneHandAxe)),
            "LMB chop"
        );
        assert_eq!(
            block_action(Block::Leaves, Some(ToolKind::Scythe)),
            "LMB chop"
        );
        assert_eq!(block_action(Block::Bedrock, None), "");
    }

    #[test]
    fn natural_shapes_get_their_own_labels() {
        assert_eq!(
            loose_shape(Voxel::new(Block::Wood).with_shape(Shape::Stick)).as_deref(),
            Some("Stick")
        );
        assert_eq!(
            loose_shape(Voxel::new(Block::Slate).with_shape(Shape::Pebble)).as_deref(),
            Some("Slate Pebble")
        );
        assert_eq!(
            loose_shape(Voxel::new(Block::Granite).with_shape(Shape::Pebble)).as_deref(),
            Some("Granite Pebble")
        );
        assert_eq!(
            loose_shape(Voxel::new(Block::Wood).with_shape(Shape::Stem)),
            None
        );
        assert_eq!(loose_shape(Voxel::new(Block::Granite)), None);
    }
}
