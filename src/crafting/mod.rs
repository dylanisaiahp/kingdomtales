//! Data-driven crafting: contexts, recipes, validation, and transactions.
//!
//! Crafting is recipe-driven (no 2x2/3x3 grid). Recipes declare ingredients,
//! required tools/stations/contexts, ordered processing steps, and outputs.
//! Validation reports concrete missing reasons, and execution is atomic: inputs
//! are only consumed when the full craft can succeed.

use bevy::prelude::*;

use crate::backpack::Backpack;
use crate::items::{ItemId, ItemStack, inventory_count, selected_tool, store_item, tool_for_item};
use crate::pockets::Pockets;
use crate::tools::ToolKind;

/// Where a craft is being performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CraftingContext {
    /// Always-available baseline crafting using only carried items and hands.
    Hands,
    /// A natural, in-world surface used for shaping/knapping/assembly.
    #[allow(dead_code)]
    NaturalSurface(NaturalSurfaceKind),
    /// A placeable constructed station (future tiers).
    #[allow(dead_code)]
    Station(StationKind),
}

impl CraftingContext {
    #[allow(dead_code)]
    pub fn display_name(self) -> &'static str {
        match self {
            CraftingContext::Hands => "Hands",
            CraftingContext::NaturalSurface(kind) => kind.display_name(),
            CraftingContext::Station(kind) => kind.display_name(),
        }
    }

    /// Whether `self` satisfies a recipe requirement.
    pub fn satisfies(self, required: CraftingContextRequirement) -> bool {
        match required {
            CraftingContextRequirement::Hands => matches!(self, CraftingContext::Hands),
            CraftingContextRequirement::NaturalSurface(kind) => {
                matches!(self, CraftingContext::NaturalSurface(actual) if actual == kind)
            }
            CraftingContextRequirement::AnyNaturalSurface => {
                matches!(self, CraftingContext::NaturalSurface(_))
            }
            CraftingContextRequirement::Station(kind) => {
                matches!(self, CraftingContext::Station(actual) if actual == kind)
            }
        }
    }
}

/// Recognised natural crafting surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NaturalSurfaceKind {
    #[allow(dead_code)]
    FlatRock,
    #[allow(dead_code)]
    Stump,
    #[allow(dead_code)]
    FallenLog,
}

impl NaturalSurfaceKind {
    pub fn display_name(self) -> &'static str {
        match self {
            NaturalSurfaceKind::FlatRock => "Flat Rock",
            NaturalSurfaceKind::Stump => "Stump",
            NaturalSurfaceKind::FallenLog => "Fallen Log",
        }
    }
}

/// Constructed crafting stations/work surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StationKind {
    #[allow(dead_code)]
    PrimitiveWorkSurface,
    #[allow(dead_code)]
    Workbench,
}

impl StationKind {
    pub fn display_name(self) -> &'static str {
        match self {
            StationKind::PrimitiveWorkSurface => "Primitive Work Surface",
            StationKind::Workbench => "Workbench",
        }
    }
}

/// Workbench UI categories. Recipes assign these via metadata; the UI shows
/// only categories that have at least one available recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecipeCategory {
    Components,
    Equipment,
    Construction,
    Stations,
}

impl RecipeCategory {
    pub const ALL: [RecipeCategory; 4] = [
        RecipeCategory::Components,
        RecipeCategory::Equipment,
        RecipeCategory::Construction,
        RecipeCategory::Stations,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            RecipeCategory::Components => "Components",
            RecipeCategory::Equipment => "Equipment",
            RecipeCategory::Construction => "Construction",
            RecipeCategory::Stations => "Stations",
        }
    }
}

/// Context requirement declared by a recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CraftingContextRequirement {
    Hands,
    #[allow(dead_code)]
    NaturalSurface(NaturalSurfaceKind),
    #[allow(dead_code)]
    AnyNaturalSurface,
    #[allow(dead_code)]
    Station(StationKind),
}

impl CraftingContextRequirement {
    pub fn display_name(self) -> &'static str {
        match self {
            CraftingContextRequirement::Hands => "Hands",
            CraftingContextRequirement::NaturalSurface(kind) => kind.display_name(),
            CraftingContextRequirement::AnyNaturalSurface => "Natural Surface",
            CraftingContextRequirement::Station(kind) => kind.display_name(),
        }
    }
}

/// One ingredient required by a recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ingredient {
    pub item: ItemId,
    pub count: u32,
}

/// One output produced by a recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CraftOutput {
    pub item: ItemId,
    pub count: u32,
}

/// Tool capability required while crafting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRequirement {
    /// Any of the listed tool kinds satisfies the requirement (carried or selected).
    #[allow(dead_code)]
    AnyOf(&'static [ToolKind]),
    /// A specific tool kind must be available.
    #[allow(dead_code)]
    Specific(ToolKind),
}

impl ToolRequirement {
    pub fn display_name(self) -> String {
        match self {
            ToolRequirement::AnyOf(kinds) => kinds
                .iter()
                .map(|kind| kind.display_name())
                .collect::<Vec<_>>()
                .join(" or "),
            ToolRequirement::Specific(kind) => kind.display_name().to_string(),
        }
    }

    fn satisfied(self, selected: Option<ToolKind>, pockets: &Pockets, backpack: &Backpack) -> bool {
        let matches = |kind: ToolKind| {
            selected == Some(kind) || tool_for_item_in_inventory(kind, pockets, backpack)
        };
        match self {
            ToolRequirement::AnyOf(kinds) => kinds.iter().copied().any(matches),
            ToolRequirement::Specific(kind) => matches(kind),
        }
    }
}

fn tool_for_item_in_inventory(kind: ToolKind, pockets: &Pockets, backpack: &Backpack) -> bool {
    ItemId::ALL.into_iter().any(|item| {
        tool_for_item(item) == Some(kind) && inventory_count(item, pockets, backpack) > 0
    })
}

/// A descriptive processing/assembly step shown in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CraftStep {
    pub text: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecipeId {
    SharpenGranitePebble,
    SharpenSlatePebble,
    SharpenSandstonePebble,
    SharpenLimestonePebble,
    StoneTippedStick,
    TwistCordage,
    StoneHandAxe,
    BarkScraper,
    SmallStoneHammer,
    Workbench,
}

pub struct Recipe {
    #[allow(dead_code)]
    pub id: RecipeId,
    pub name: &'static str,
    #[allow(dead_code)]
    pub description: &'static str,
    /// Workbench UI category; None for hand/non-workbench recipes.
    pub category: Option<RecipeCategory>,
    pub context: CraftingContextRequirement,
    pub ingredients: &'static [Ingredient],
    pub tools: &'static [ToolRequirement],
    pub steps: &'static [CraftStep],
    pub outputs: &'static [CraftOutput],
}

/// Why a recipe cannot be crafted right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CraftingIssue {
    WrongContext {
        required: &'static str,
    },
    MissingIngredients(Vec<(ItemId, u32, u32)>), // item, have, need
    MissingTool(String),
    #[allow(dead_code)]
    NoSpace,
}

impl CraftingIssue {
    pub fn label(&self) -> String {
        match self {
            CraftingIssue::WrongContext { required } => format!("Requires {required}"),
            CraftingIssue::MissingIngredients(missing) => {
                let parts: Vec<String> = missing
                    .iter()
                    .map(|(item, have, need)| format!("{} {}/{}", item.display_name(), have, need))
                    .collect();
                format!("Need {}", parts.join(", "))
            }
            CraftingIssue::MissingTool(tool) => format!("Need tool: {tool}"),
            CraftingIssue::NoSpace => "No space for output".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CraftValidation {
    pub issues: Vec<CraftingIssue>,
}

impl CraftValidation {
    pub fn is_craftable(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.is_craftable() {
            "Ready to craft".to_string()
        } else {
            self.issues
                .iter()
                .map(CraftingIssue::label)
                .collect::<Vec<_>>()
                .join(" | ")
        }
    }
}

const SHARPEN_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Strike and chip the pebble by hand to form a crude sharp edge.",
}];
const SHARPEN_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::SharpenedStone,
    count: 1,
}];
const TIPPED_STICK_INGREDIENTS: [Ingredient; 2] = [
    Ingredient {
        item: ItemId::Stick,
        count: 1,
    },
    Ingredient {
        item: ItemId::SharpenedStone,
        count: 1,
    },
];
const TIPPED_STICK_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Fit and bind a sharpened stone to a stick by hand.",
}];
const TIPPED_STICK_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::StoneTippedStick,
    count: 1,
}];

const GRANITE_ING: [Ingredient; 1] = [Ingredient {
    item: ItemId::GranitePebble,
    count: 1,
}];
const SLATE_ING: [Ingredient; 1] = [Ingredient {
    item: ItemId::SlatePebble,
    count: 1,
}];
const SANDSTONE_ING: [Ingredient; 1] = [Ingredient {
    item: ItemId::SandstonePebble,
    count: 1,
}];
const LIMESTONE_ING: [Ingredient; 1] = [Ingredient {
    item: ItemId::LimestonePebble,
    count: 1,
}];
const CORDAGE_INGREDIENTS: [Ingredient; 1] = [Ingredient {
    item: ItemId::Fiber,
    count: 2,
}];
const CORDAGE_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Twist and bind loose plant fibers into a simple cord.",
}];
const CORDAGE_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::Cordage,
    count: 1,
}];

const STONE_HAND_AXE_INGREDIENTS: [Ingredient; 3] = [
    Ingredient {
        item: ItemId::Stick,
        count: 1,
    },
    Ingredient {
        item: ItemId::SharpenedStone,
        count: 1,
    },
    Ingredient {
        item: ItemId::Cordage,
        count: 1,
    },
];
const STONE_HAND_AXE_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Lash a sharpened stone to a stick to form a crude hand axe.",
}];
const STONE_HAND_AXE_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::StoneHandAxe,
    count: 1,
}];

const BARK_SCRAPER_INGREDIENTS: [Ingredient; 2] = [
    Ingredient {
        item: ItemId::Stick,
        count: 1,
    },
    Ingredient {
        item: ItemId::SharpenedStone,
        count: 1,
    },
];
const BARK_SCRAPER_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Bind a sharpened stone at an angle to make a bark scraper.",
}];
const BARK_SCRAPER_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::BarkScraper,
    count: 1,
}];

const SMALL_STONE_HAMMER_INGREDIENTS: [Ingredient; 3] = [
    Ingredient {
        item: ItemId::Stick,
        count: 1,
    },
    Ingredient {
        item: ItemId::GranitePebble,
        count: 1,
    },
    Ingredient {
        item: ItemId::Cordage,
        count: 1,
    },
];
const SMALL_STONE_HAMMER_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Lash a sturdy stone pebble to a stick to form a small hammer.",
}];
const SMALL_STONE_HAMMER_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::SmallStoneHammer,
    count: 1,
}];

const WORKBENCH_INGREDIENTS: [Ingredient; 2] = [
    Ingredient {
        item: ItemId::Wood,
        count: 4,
    },
    Ingredient {
        item: ItemId::Cordage,
        count: 2,
    },
];
const WORKBENCH_STEPS: [CraftStep; 1] = [CraftStep {
    text: "Assemble logs and cordage into a sturdy primitive workbench.",
}];
const WORKBENCH_OUTPUTS: [CraftOutput; 1] = [CraftOutput {
    item: ItemId::Workbench,
    count: 1,
}];

pub static RECIPES: [Recipe; 10] = [
    Recipe {
        id: RecipeId::SharpenGranitePebble,
        name: "Sharpen Granite Pebble",
        description: "Chip a granite pebble into a crude sharpened stone.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &GRANITE_ING,
        tools: &[],
        steps: &SHARPEN_STEPS,
        outputs: &SHARPEN_OUTPUTS,
    },
    Recipe {
        id: RecipeId::SharpenSlatePebble,
        name: "Sharpen Slate Pebble",
        description: "Chip a slate pebble into a crude sharpened stone.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &SLATE_ING,
        tools: &[],
        steps: &SHARPEN_STEPS,
        outputs: &SHARPEN_OUTPUTS,
    },
    Recipe {
        id: RecipeId::SharpenSandstonePebble,
        name: "Sharpen Sandstone Pebble",
        description: "Chip a sandstone pebble into a crude sharpened stone.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &SANDSTONE_ING,
        tools: &[],
        steps: &SHARPEN_STEPS,
        outputs: &SHARPEN_OUTPUTS,
    },
    Recipe {
        id: RecipeId::SharpenLimestonePebble,
        name: "Sharpen Limestone Pebble",
        description: "Chip a limestone pebble into a crude sharpened stone.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &LIMESTONE_ING,
        tools: &[],
        steps: &SHARPEN_STEPS,
        outputs: &SHARPEN_OUTPUTS,
    },
    Recipe {
        id: RecipeId::StoneTippedStick,
        name: "Stone Tipped Stick",
        description: "A simple pointed stick reinforced with a sharpened stone.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &TIPPED_STICK_INGREDIENTS,
        tools: &[],
        steps: &TIPPED_STICK_STEPS,
        outputs: &TIPPED_STICK_OUTPUTS,
    },
    Recipe {
        id: RecipeId::TwistCordage,
        name: "Twist Cordage",
        description: "Twist loose plant fibers into simple cordage for binding.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &CORDAGE_INGREDIENTS,
        tools: &[],
        steps: &CORDAGE_STEPS,
        outputs: &CORDAGE_OUTPUTS,
    },
    Recipe {
        id: RecipeId::StoneHandAxe,
        name: "Stone Hand Axe",
        description: "A primitive hand axe for early wood gathering and light cutting.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &STONE_HAND_AXE_INGREDIENTS,
        tools: &[],
        steps: &STONE_HAND_AXE_STEPS,
        outputs: &STONE_HAND_AXE_OUTPUTS,
    },
    Recipe {
        id: RecipeId::BarkScraper,
        name: "Bark Scraper",
        description: "A specialized scraper for removing bark from tree trunks.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &BARK_SCRAPER_INGREDIENTS,
        tools: &[],
        steps: &BARK_SCRAPER_STEPS,
        outputs: &BARK_SCRAPER_OUTPUTS,
    },
    Recipe {
        id: RecipeId::SmallStoneHammer,
        name: "Small Stone Hammer",
        description: "A primitive stone hammer for breaking natural stone deposits.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &SMALL_STONE_HAMMER_INGREDIENTS,
        tools: &[],
        steps: &SMALL_STONE_HAMMER_STEPS,
        outputs: &SMALL_STONE_HAMMER_OUTPUTS,
    },
    Recipe {
        id: RecipeId::Workbench,
        name: "Workbench",
        description: "A sturdy primitive wooden workbench for crafting at a station.",
        category: None,
        context: CraftingContextRequirement::Hands,
        ingredients: &WORKBENCH_INGREDIENTS,
        tools: &[],
        steps: &WORKBENCH_STEPS,
        outputs: &WORKBENCH_OUTPUTS,
    },
];

#[allow(dead_code)]
pub fn recipes_for_context(context: CraftingContext) -> impl Iterator<Item = &'static Recipe> {
    RECIPES
        .iter()
        .filter(move |recipe| context.satisfies(recipe.context))
}

#[allow(dead_code)]
pub fn format_ingredients(recipe: &Recipe, pockets: &Pockets, backpack: &Backpack) -> String {
    recipe
        .ingredients
        .iter()
        .map(|ing| {
            let have = inventory_count(ing.item, pockets, backpack);
            format!("{} {}/{}", ing.item.display_name(), have, ing.count)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[allow(dead_code)]
pub fn format_tools(recipe: &Recipe) -> String {
    if recipe.tools.is_empty() {
        "Hands only".to_string()
    } else {
        recipe
            .tools
            .iter()
            .map(|t| t.display_name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[allow(dead_code)]
pub fn format_steps(recipe: &Recipe) -> String {
    if recipe.steps.is_empty() {
        "No special steps".to_string()
    } else {
        recipe
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| format!("{}. {}", i + 1, step.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[allow(dead_code)]
pub fn format_outputs(recipe: &Recipe) -> String {
    recipe
        .outputs
        .iter()
        .map(|o| format!("{} x{}", o.item.display_name(), o.count))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn validate_recipe(
    recipe: &Recipe,
    context: CraftingContext,
    pockets: &Pockets,
    backpack: &Backpack,
) -> CraftValidation {
    let mut issues = Vec::new();

    if !context.satisfies(recipe.context) {
        issues.push(CraftingIssue::WrongContext {
            required: recipe.context.display_name(),
        });
    }

    let mut missing = Vec::new();
    for ingredient in recipe.ingredients {
        let have = inventory_count(ingredient.item, pockets, backpack);
        if have < ingredient.count {
            missing.push((ingredient.item, have, ingredient.count));
        }
    }
    if !missing.is_empty() {
        issues.push(CraftingIssue::MissingIngredients(missing));
    }

    let selected = selected_tool(pockets);
    for requirement in recipe.tools {
        if !requirement.satisfied(selected, pockets, backpack) {
            issues.push(CraftingIssue::MissingTool(requirement.display_name()));
        }
    }

    CraftValidation { issues }
}

/// Recipes shown in the crafting UI for the active context.
///
/// Hands recipes are always available. Recipes for other contexts/surfaces/stations
/// remain inspectable so missing requirements are visible.
pub fn recipes_for_ui(_context: CraftingContext) -> impl Iterator<Item = &'static Recipe> {
    RECIPES.iter()
}

/// Consume ingredients deterministically: pockets first (all slots), then backpack.
/// Drains across stacks/sections as long as the total is available.
#[allow(dead_code)]
fn consume_everywhere(
    item: ItemId,
    mut count: u32,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
) -> bool {
    if inventory_count(item, pockets, backpack) < count {
        return false;
    }
    for index in 0..crate::pockets::POCKET_COUNT {
        if count == 0 {
            break;
        }
        if let Some(mut stack) = pockets.take(index) {
            if stack.item == item {
                let taken = stack.count.min(count);
                stack.count -= taken;
                count -= taken;
                if stack.count > 0 {
                    let _ = pockets.replace(index, Some(stack));
                }
            } else {
                let _ = pockets.replace(index, Some(stack));
            }
        }
    }
    if count > 0 {
        let consumed = backpack.consume(item, count);
        debug_assert!(consumed, "ingredient validated but backpack consume failed");
    }
    true
}

/// Attempt to craft `recipe`. Returns produced stacks on success.
///
/// Crafting is atomic: ingredients are only consumed when every output can be
/// stored. If storage is full, no items are lost and `NoSpace` is reported.
#[allow(dead_code)]
fn restore_inventory_snapshot(
    pockets: &mut Pockets,
    backpack: &mut Backpack,
    pocket_snapshot: &(usize, Vec<Option<ItemStack>>),
    backpack_snapshot: &crate::backpack::BackpackSnapshot,
) {
    pockets
        .restore(pocket_snapshot.0, &pocket_snapshot.1)
        .expect("crafting pocket snapshot should be valid");
    backpack
        .restore(backpack_snapshot)
        .expect("crafting backpack snapshot should be valid");
}

#[allow(dead_code)]
pub fn craft_recipe(
    recipe: &Recipe,
    context: CraftingContext,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
) -> Result<Vec<ItemStack>, CraftValidation> {
    let validation = validate_recipe(recipe, context, pockets, backpack);
    if !validation.is_craftable() {
        return Err(validation);
    }

    let pocket_snapshot = pockets.snapshot();
    let backpack_snapshot = backpack.snapshot();

    for ingredient in recipe.ingredients {
        let ok = consume_everywhere(ingredient.item, ingredient.count, pockets, backpack);
        if !ok {
            restore_inventory_snapshot(pockets, backpack, &pocket_snapshot, &backpack_snapshot);
            return Err(CraftValidation {
                issues: vec![CraftingIssue::MissingIngredients(vec![(
                    ingredient.item,
                    0,
                    ingredient.count,
                )])],
            });
        }
    }

    let mut produced = Vec::new();
    for output in recipe.outputs {
        let leftover = store_item(pockets, backpack, output.item, output.count);
        if leftover > 0 {
            restore_inventory_snapshot(pockets, backpack, &pocket_snapshot, &backpack_snapshot);
            return Err(CraftValidation {
                issues: vec![CraftingIssue::NoSpace],
            });
        }
        produced.push(ItemStack {
            item: output.item,
            count: output.count,
        });
    }

    Ok(produced)
}

#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct CraftingUiState {
    pub selected: usize,
    pub station: Option<StationKind>,
    /// Selected workbench category tab.
    pub workbench_category: Option<RecipeCategory>,
}

impl CraftingUiState {
    #[allow(dead_code)]
    pub fn context(&self) -> CraftingContext {
        match self.station {
            Some(kind) => CraftingContext::Station(kind),
            None => CraftingContext::Hands,
        }
    }
}

pub fn workbench_recipes() -> impl Iterator<Item = &'static Recipe> {
    RECIPES.iter().filter(|r| {
        matches!(
            r.context,
            CraftingContextRequirement::Station(StationKind::Workbench)
        )
    })
}

pub fn available_workbench_categories() -> Vec<RecipeCategory> {
    RecipeCategory::ALL
        .into_iter()
        .filter(|cat| workbench_recipes().any(|r| r.category == Some(*cat)))
        .collect()
}

/// Recipes for a workbench category. Only categories with recipes are shown by UI.
pub fn workbench_recipes_for_category(category: RecipeCategory) -> Vec<&'static Recipe> {
    workbench_recipes()
        .filter(|r| r.category == Some(category))
        .collect()
}

pub fn can_craft_quantity(
    recipe: &Recipe,
    context: CraftingContext,
    pockets: &Pockets,
    backpack: &Backpack,
    quantity: u32,
) -> bool {
    if quantity == 0 {
        return false;
    }
    let validation = validate_recipe(recipe, context, pockets, backpack);
    // validate_recipe checks for 1x; for quantity we check ingredients and capacity.
    if validation.issues.iter().any(|i| {
        matches!(
            i,
            CraftingIssue::WrongContext { .. } | CraftingIssue::MissingTool(_)
        )
    }) {
        return false;
    }
    for ing in recipe.ingredients {
        let need = match ing.count.checked_mul(quantity) {
            Some(n) => n,
            None => return false,
        };
        if inventory_count(ing.item, pockets, backpack) < need {
            return false;
        }
    }
    // Capacity check via dry-run on snapshots.
    let pocket_snapshot = pockets.snapshot();
    let backpack_snapshot = backpack.snapshot();
    let mut pockets_sim = Pockets::default();
    let mut backpack_sim = Backpack::default();
    if pockets_sim
        .restore(pocket_snapshot.0, &pocket_snapshot.1)
        .is_err()
    {
        return false;
    }
    if backpack_sim.restore(&backpack_snapshot).is_err() {
        return false;
    }
    for _ in 0..quantity {
        match craft_recipe(recipe, context, &mut pockets_sim, &mut backpack_sim) {
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    true
}

pub struct CraftingPlugin;

impl Plugin for CraftingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CraftingUiState>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backpack::Backpack;

    #[test]
    fn hands_recipes_are_available() {
        let hands: Vec<_> = recipes_for_context(CraftingContext::Hands).collect();
        assert!(!hands.is_empty());
        assert!(hands.iter().any(|r| r.id == RecipeId::StoneTippedStick));
        assert!(hands.iter().any(|r| r.id == RecipeId::StoneHandAxe));
        assert!(hands.iter().any(|r| r.id == RecipeId::BarkScraper));
        assert!(hands.iter().any(|r| r.id == RecipeId::SmallStoneHammer));
    }

    #[test]
    fn primitive_tool_recipes_use_hammerless_hand_gatherable_inputs() {
        for id in [
            RecipeId::StoneHandAxe,
            RecipeId::BarkScraper,
            RecipeId::SmallStoneHammer,
        ] {
            let recipe = RECIPES.iter().find(|r| r.id == id).unwrap();
            assert_eq!(recipe.context, CraftingContextRequirement::Hands);
            assert!(recipe.tools.is_empty(), "{id:?} should not require tools");
            for ing in recipe.ingredients {
                assert!(
                    matches!(
                        ing.item,
                        ItemId::Stick
                            | ItemId::Fiber
                            | ItemId::Cordage
                            | ItemId::SharpenedStone
                            | ItemId::GranitePebble
                            | ItemId::SlatePebble
                            | ItemId::SandstonePebble
                            | ItemId::LimestonePebble
                    ),
                    "{id:?} uses unexpected/hard-gated ingredient {:?}",
                    ing.item
                );
            }
        }
    }

    #[test]
    fn validation_summary_reports_missing_tool() {
        let recipe = Recipe {
            id: RecipeId::StoneTippedStick,
            name: "Test",
            description: "Test",
            category: None,
            context: CraftingContextRequirement::Hands,
            ingredients: &[],
            tools: &[ToolRequirement::Specific(ToolKind::Hammer)],
            steps: &[],
            outputs: &[],
        };
        let pockets = Pockets::default();
        let backpack = Backpack::default();
        let validation = validate_recipe(&recipe, CraftingContext::Hands, &pockets, &backpack);
        assert!(!validation.is_craftable());
        assert!(validation.summary().contains("Hammer"));
    }

    #[test]
    fn validation_reports_wrong_context() {
        let recipe = Recipe {
            id: RecipeId::StoneTippedStick,
            name: "Test",
            description: "Test",
            category: None,
            context: CraftingContextRequirement::NaturalSurface(NaturalSurfaceKind::FlatRock),
            ingredients: &[],
            tools: &[],
            steps: &[],
            outputs: &[],
        };
        let pockets = Pockets::default();
        let backpack = Backpack::default();
        let validation = validate_recipe(&recipe, CraftingContext::Hands, &pockets, &backpack);
        assert!(!validation.is_craftable());
        assert!(validation.summary().contains("Flat Rock"));
    }

    #[test]
    fn validate_reports_missing_ingredients() {
        let pockets = Pockets::default();
        let backpack = Backpack::default();
        let recipe = RECIPES
            .iter()
            .find(|r| r.id == RecipeId::StoneTippedStick)
            .unwrap();
        let validation = validate_recipe(recipe, CraftingContext::Hands, &pockets, &backpack);
        assert!(!validation.is_craftable());
        assert!(matches!(
            validation.issues.first(),
            Some(CraftingIssue::MissingIngredients(_))
        ));
        assert!(validation.summary().contains("Stick"));
    }

    #[test]
    fn craft_consumes_inputs_and_produces_output() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        pockets.add(ItemId::GranitePebble, 1);
        let sharpen = RECIPES
            .iter()
            .find(|r| r.id == RecipeId::SharpenGranitePebble)
            .unwrap();
        let produced = craft_recipe(sharpen, CraftingContext::Hands, &mut pockets, &mut backpack)
            .expect("craftable");
        assert_eq!(produced[0].item, ItemId::SharpenedStone);
        assert_eq!(
            inventory_count(ItemId::GranitePebble, &pockets, &backpack),
            0
        );
        assert_eq!(
            inventory_count(ItemId::SharpenedStone, &pockets, &backpack),
            1
        );

        pockets.add(ItemId::Stick, 1);
        let tipped = RECIPES
            .iter()
            .find(|r| r.id == RecipeId::StoneTippedStick)
            .unwrap();
        let produced = craft_recipe(tipped, CraftingContext::Hands, &mut pockets, &mut backpack)
            .expect("craftable");
        assert_eq!(produced[0].item, ItemId::StoneTippedStick);
        assert_eq!(inventory_count(ItemId::Stick, &pockets, &backpack), 0);
        assert_eq!(
            inventory_count(ItemId::SharpenedStone, &pockets, &backpack),
            0
        );
    }

    #[test]
    fn can_craft_cordage_from_fiber_by_hand() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        pockets.add(ItemId::Fiber, 2);
        let recipe = RECIPES
            .iter()
            .find(|r| r.id == RecipeId::TwistCordage)
            .unwrap();
        let validation = validate_recipe(recipe, CraftingContext::Hands, &pockets, &backpack);
        assert!(validation.is_craftable(), "{}", validation.summary());
        let produced =
            craft_recipe(recipe, CraftingContext::Hands, &mut pockets, &mut backpack).unwrap();
        assert_eq!(produced[0].item, ItemId::Cordage);
        assert_eq!(inventory_count(ItemId::Fiber, &pockets, &backpack), 0);
        assert_eq!(inventory_count(ItemId::Cordage, &pockets, &backpack), 1);
    }

    #[test]
    fn craft_consumes_ingredients_split_across_backpack_stacks() {
        let recipe = Recipe {
            id: RecipeId::StoneTippedStick,
            name: "Split Stack Test",
            description: "Test",
            category: None,
            context: CraftingContextRequirement::Hands,
            ingredients: &[Ingredient {
                item: ItemId::GranitePebble,
                count: 2,
            }],
            tools: &[],
            steps: &[],
            outputs: &[CraftOutput {
                item: ItemId::SharpenedStone,
                count: 1,
            }],
        };
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        let mut snapshot = backpack.snapshot();
        snapshot.slots[0] = Some(ItemStack {
            item: ItemId::GranitePebble,
            count: 1,
        });
        snapshot.slots[1] = Some(ItemStack {
            item: ItemId::GranitePebble,
            count: 1,
        });
        backpack.restore(&snapshot).unwrap();

        let result = craft_recipe(&recipe, CraftingContext::Hands, &mut pockets, &mut backpack);
        assert!(result.is_ok(), "split stacks should satisfy and consume");
        assert_eq!(backpack.count(ItemId::GranitePebble), 0);
        assert!(inventory_count(ItemId::SharpenedStone, &pockets, &backpack) >= 1);
    }

    #[test]
    fn craft_fails_with_no_space_without_consuming_inputs() {
        let recipe = Recipe {
            id: RecipeId::StoneTippedStick,
            name: "No Space Test",
            description: "Test",
            category: None,
            context: CraftingContextRequirement::Hands,
            ingredients: &[],
            tools: &[],
            steps: &[],
            outputs: &[CraftOutput {
                item: ItemId::GranitePebble,
                count: 1,
            }],
        };
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        for index in 0..crate::pockets::POCKET_COUNT {
            pockets.replace(
                index,
                Some(ItemStack {
                    item: ItemId::SlatePebble,
                    count: ItemId::SlatePebble.def().max_stack,
                }),
            );
        }
        let mut snapshot = backpack.snapshot();
        for slot in snapshot.slots.iter_mut() {
            *slot = Some(ItemStack {
                item: ItemId::SandstonePebble,
                count: ItemId::SandstonePebble.def().max_stack,
            });
        }
        backpack.restore(&snapshot).unwrap();

        let pocket_before = pockets.snapshot();
        let backpack_before = backpack.snapshot();
        let result = craft_recipe(&recipe, CraftingContext::Hands, &mut pockets, &mut backpack);

        assert_eq!(
            result,
            Err(CraftValidation {
                issues: vec![CraftingIssue::NoSpace],
            })
        );
        assert_eq!(pockets.snapshot(), pocket_before);
        assert_eq!(backpack.snapshot(), backpack_before);
    }
}
