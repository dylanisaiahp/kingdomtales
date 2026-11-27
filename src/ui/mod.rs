//! Heads-up display and overlay presentation.
//!
//! The HUD is deliberately read-only: progression, survival, notifications and
//! Insight are authoritative resources owned by their own plugins. This module
//! only builds widgets and copies current values into them. Shared palette and
//! widget helpers live in [`theme`].
//!
//! Layout:
//! - top left: character HUD (portrait placeholder, level, XP progress bar,
//!   and current XP / XP required)
//! - top centre: sliding cardinal compass strip (unchanged position/design)
//! - top right: coins, with F3 debug underneath
//! - centre: crosshair dot
//! - bottom centre: Insight above stamina; stamina bar visually attached on top
//!   of the hotbar; hunger left and thirst right of the hotbar (separate)
//! - bottom left / bottom right: split controls HUD
//! - notifications: transient status feed above the right controls

use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::LineBreak;
use bevy::ui::Pressed;
use bevy::ui_widgets::{Activate, Button};
use bevy::window::PrimaryWindow;

use crate::crafting::{CraftingUiState, recipes_for_ui, validate_recipe};

use self::icons::UiIcons;
use self::theme::{
    UiTheme, display_if, format_number, key_label, spawn_bar_overlay, spawn_hud_icon,
    spawn_item_icon, spawn_keycap, spawn_panel, spawn_text,
};
use crate::backpack::{ATTACHMENT_SLOTS, Backpack, CATEGORY_SLOTS_PER_UNLOCK, MAX_CAPACITY};
use crate::insight::InsightInfo;
use crate::interaction::PickupPrompt;
use crate::items::{ItemCategory, ItemId, ItemStack, selected_tool};
use crate::notifications::{NotificationFeed, NotificationTone};
use crate::player::{CrosshairTarget, CursorCapture, Player, Stamina};
use crate::pockets::{POCKET_COUNT, Pockets};
use crate::progression::{Attribute, Progression};
use crate::settings::{GameSettings, InputBindings, LightingSystems, UndergroundLighting};
use crate::ui_manager::{Overlay, UiManager};
use crate::world::chunk::BLOCK_SIZE;
use crate::world::{BiomeInfo, ChunkManager, InitialWorldLoad, WorldTimings};

mod give_menu;
mod icons;
mod settings_menu;
mod shape_menu;
pub(crate) mod theme;

/// Append-only perf snapshot log. Written only when the settings toggle is on.
const PERF_LOG_PATH: &str = "/tmp/opencode/kt-perf.log";

const HOTBAR_SLOT_SIZE: f32 = 58.0;
const CHARACTER_SLOT_SIZE: f32 = 50.0;
const CHARACTER_SLOT_GAP: f32 = 8.0;
const HOTBAR_PADDING_X: f32 = 10.0;
const HOTBAR_PADDING_Y: f32 = 7.0;
const HOTBAR_GAP: f32 = 8.0;
const POCKET_GAP: f32 = 4.0;
const HOTBAR_WIDTH: f32 = HOTBAR_PADDING_X * 2.0
    + HOTBAR_SLOT_SIZE
    + HOTBAR_GAP
    + POCKET_COUNT as f32 * HOTBAR_SLOT_SIZE
    + (POCKET_COUNT as f32 - 1.0) * POCKET_GAP;
const HOTBAR_HEIGHT: f32 = HOTBAR_SLOT_SIZE + HOTBAR_PADDING_Y * 2.0;
const COMPASS_WIDTH: f32 = 232.0;
const COMPASS_LABEL_WIDTH: f32 = 26.0;
const MAX_NOTIFICATIONS: usize = 5;

// The bottom dock keeps survival readouts visible above the Tab backdrop.
const CHARACTER_PANEL_PADDING_X: f32 = 8.0;
const SIDE_PANEL_WIDTH: f32 =
    CHARACTER_PANEL_PADDING_X * 2.0 + CHARACTER_SLOT_SIZE * 4.0 + CHARACTER_SLOT_GAP * 3.0 + 2.0;
const DOCK_BOTTOM: f32 = 24.0;
const BOTTOM_PANELS_HEIGHT: f32 = 400.0;

// All HUD bars share the same compact level-bar style (value centred inside
// a thin, lightweight track).
const STAT_BAR_HEIGHT: f32 = 22.0;
// Stamina sits directly on top of the hotbar as a visually connected component.
const STAMINA_BAR_HEIGHT: f32 = 16.0;
// Stamina stretches almost the entire hotbar width with 8 px side insets.
const STAMINA_SIDE_INSET: f32 = 8.0;
const STAMINA_BAR_WIDTH: f32 = HOTBAR_WIDTH - STAMINA_SIDE_INSET * 2.0;
// Padding reserved at the bottom of the backpack storage panel above hotbar.
const SURVIVAL_INSET: f32 = 8.0;
// Stamina sits flush on top of the hotbar (1px overlap removes seam); Insight floats above it.
const STAMINA_RISE: f32 = HOTBAR_HEIGHT - 1.0;
const INSIGHT_RISE: f32 = 6.0;
// Hunger/thirst sit to the left/right of the hotbar, separate from it.
const VITAL_GAP_FROM_HOTBAR: f32 = 10.0;
const NOTIFICATION_GAP: f32 = 8.0;
const VITAL_INDICATOR_SIZE: f32 = 44.0;

/// When the backpack storage is open, flatten the hotbar's top edge so it
/// connects to the storage panel above it; the stamina bar overlaps the same
/// edge by 1px to stay visually attached. Otherwise keep all corners rounded.
#[inline]
fn hotbar_radius(joined: bool, theme: &UiTheme) -> BorderRadius {
    let rounded = theme.shape.rounded();
    let zero = BorderRadius::all(Val::Px(0.0));
    if joined {
        BorderRadius {
            top_left: zero.top_left,
            top_right: zero.top_right,
            ..rounded
        }
    } else {
        rounded
    }
}

/// The backpack's bottom edge joins the hotbar when Tab is open.
#[inline]
fn storage_radius(joined: bool, theme: &UiTheme) -> BorderRadius {
    let rounded = theme.shape.rounded();
    let zero = BorderRadius::all(Val::Px(0.0));
    if joined {
        BorderRadius {
            bottom_left: zero.bottom_left,
            bottom_right: zero.bottom_right,
            ..rounded
        }
    } else {
        rounded
    }
}

/// Stamina sits on top of the hotbar: flat-bottom when closed to join the
/// hotbar, fully rounded when the backpack opens and join geometry changes.
#[inline]
fn stamina_bar_radius(open: bool, theme: &UiTheme) -> BorderRadius {
    let rounded = theme.shape.rounded_small();
    let zero = BorderRadius::all(Val::Px(0.0));
    if open {
        rounded
    } else {
        BorderRadius {
            top_left: rounded.top_left,
            top_right: rounded.top_right,
            bottom_left: zero.bottom_left,
            bottom_right: zero.bottom_right,
        }
    }
}

/// Horizontal position of a cardinal band at `delta` degrees from centre.
#[inline]
fn compass_label_left(delta: f32) -> f32 {
    let travel = (COMPASS_WIDTH - COMPASS_LABEL_WIDTH) * 0.5;
    COMPASS_WIDTH * 0.5 - COMPASS_LABEL_WIDTH * 0.5 + (delta / 90.0) * travel
}

pub struct UiPlugin;

/// Ordered after the menu chains so shared `Node`/`Text` access is deterministic.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HudUpdateSystems;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(UiTheme::default())
            .init_resource::<UiIcons>()
            .init_resource::<FpsCounter>()
            .init_resource::<PerfLogTimer>()
            .init_resource::<HudState>()
            .init_resource::<DraggedSlot>()
            .add_plugins((
                settings_menu::SettingsMenuPlugin,
                shape_menu::ShapeMenuPlugin,
                give_menu::GiveMenuPlugin,
            ))
            .configure_sets(
                Update,
                (
                    crate::chat::ChatUiSystems,
                    settings_menu::SettingsMenuSystems,
                    shape_menu::ShapeMenuSystems,
                    give_menu::GiveMenuSystems,
                    HudUpdateSystems,
                )
                    .chain(),
            )
            .add_systems(Startup, (setup_hud, setup_initial_loading_screen))
            .add_systems(
                Update,
                (
                    (
                        track_fps,
                        log_perf_snapshot,
                        toggle_hud,
                        update_debug_text,
                        update_level_panel,
                        update_vital_indicators,
                        update_compass,
                        update_coins,
                        update_stamina_bar,
                        update_insight_panel,
                        update_notifications,
                        update_initial_loading_screen,
                    )
                        .chain(),
                    (
                        update_pockets,
                        update_inventory_slot_styles,
                        update_slot_visuals,
                        update_pickup_prompt,
                        sync_visibility,
                        sync_backpack_visibility,
                        update_backpack_texts,
                        update_backpack_xp_bar,
                        update_backpack_cells,
                        rebuild_backpack_crafting_list,
                        refresh_workbench_ui,
                        handle_crafting_scroll,
                        update_crafting_scrollbar,
                        update_backpack_tooltip,
                        update_drag_ghost,
                        style_backpack_buttons,
                    )
                        .chain(),
                )
                    .chain()
                    .in_set(HudUpdateSystems),
            )
            .add_systems(
                Update,
                update_underwater_tint.after(LightingSystems::UpdateUnderground),
            )
            .add_observer(on_backpack_activate)
            .add_observer(on_slot_drag_start)
            .add_observer(on_slot_drag_drop)
            .add_observer(on_slot_drag_end)
            .add_observer(on_slot_click_quick_move);
    }
}

/// Smoothed frame statistics used by the debug panel and optional perf log.
#[derive(Resource)]
pub struct FpsCounter {
    pub fps: f32,
    pub frame_ms: f32,
    frames: u32,
    elapsed: f32,
    recent: Vec<f32>,
}

impl FpsCounter {
    pub fn frame_stats(&self) -> (f32, f32, f32) {
        if self.recent.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut sorted = self.recent.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let max = *sorted.last().unwrap_or(&0.0);
        let p95 = sorted[((sorted.len() - 1) as f32 * 0.95) as usize];
        let p99 = sorted[((sorted.len() - 1) as f32 * 0.99) as usize];
        (max, p95, p99)
    }
}

impl Default for FpsCounter {
    fn default() -> Self {
        Self {
            fps: 0.0,
            frame_ms: 0.0,
            frames: 0,
            elapsed: 0.0,
            recent: Vec::with_capacity(240),
        }
    }
}

#[derive(Resource, Default)]
struct PerfLogTimer {
    elapsed: f32,
}

#[derive(Resource, Default)]
pub struct HudState {
    pub debug_visible: bool,
    pub hint_visible: bool,
    pub hud_hidden: bool,
}

/// The inventory slot being dragged, while the backpack panel is open.
#[derive(Resource, Default)]
struct DraggedSlot(Option<SlotSource>);

/// The always-visible bottom layer. Its Z index keeps survival readouts above
/// the Tab backdrop while the inventory panels swap in and out.
#[derive(Component)]
struct BottomDock;

/// Root HUD node; hidden with F1 or when full-screen overlays take over.
#[derive(Component)]
struct HudRoot;

#[derive(Component)]
struct PanelsRow;

/// Always-present flex slot that keeps the centre stack centred when the
/// character sheet is hidden.
#[derive(Component)]
struct CharacterAnchor;

#[derive(Component)]
struct CharacterPanel;

/// In-backpack crafting panel occupying the former details area.
#[derive(Component)]
struct CraftingPanel;

#[derive(Component)]
struct CraftingPanelTitle;

#[derive(Component)]
struct DebugPanel;

#[derive(Component)]
struct DebugText;

#[derive(Component)]
struct HintPanel;

#[derive(Component)]
struct InitialLoadingScreen;

#[derive(Component)]
struct InitialLoadingText;

#[derive(Component)]
struct InitialLoadingFill;

#[derive(Component)]
struct UnderwaterTint;

#[derive(Component)]
struct CrosshairRoot;

/// Always-present flex slot that keeps the hunger and thirst bars in place
/// while Insight is hidden.
#[derive(Component)]
struct InsightRoot;

#[derive(Component)]
struct LevelText;

#[derive(Component)]
struct XpFill;

#[derive(Component)]
struct XpText;

#[derive(Component)]
struct CoinsText;

/// One cardinal band in the compass strip. The index maps to N, E, S, W.
#[derive(Component, Clone, Copy)]
struct CompassCardinal(usize);

/// The glyph inside a cardinal band, kept separate so flex centering measures
/// the letter itself rather than relying on text-layout justification.
#[derive(Component, Clone, Copy)]
struct CompassLabel(usize);

#[derive(Component)]
struct StaminaBar;

#[derive(Component)]
struct StaminaFill;

#[derive(Component)]
struct StaminaText;

#[derive(Component)]
struct HungerValueText;

#[derive(Component)]
struct ThirstValueText;

#[derive(Component)]
struct HungerFill;

#[derive(Component)]
struct ThirstFill;

#[derive(Component)]
struct WorldInfoBiomeText;

#[derive(Component)]
struct WorldInfoDayText;

#[derive(Component)]
struct WorldInfoClockText;

/// Day/night indicator beside the day counter in the character HUD.
#[derive(Component)]
struct WeatherIcon;

#[derive(Component)]
struct InsightIcon;

#[derive(Component)]
struct InsightTitle;

#[derive(Component)]
struct InsightDescription;

#[derive(Component)]
struct PickupPromptText;

#[derive(Component, Clone, Copy)]
struct NotificationRow(usize);

#[derive(Component, Clone, Copy)]
struct NotificationIcon(usize);

#[derive(Component, Clone, Copy)]
struct NotificationTitle(usize);

#[derive(Component, Clone, Copy)]
struct NotificationDetail(usize);

/// Marker for static HUD text that is never rewritten.
#[derive(Component)]
struct HudText;

/// An inventory slot that can be dragged to or from another slot.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum SlotSource {
    Pocket(usize),
    Storage(usize),
    Category(ItemCategory, usize),
    Attachment(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotVisualKind {
    Icon,
    Count,
}

/// Visual child of an inventory slot. Empty slots keep these hidden.
#[derive(Component, Clone, Copy)]
struct SlotVisual {
    source: SlotSource,
    kind: SlotVisualKind,
}

#[derive(Component)]
struct PocketSlot(usize);

/// The hotbar itself; its upper corners join the backpack while Tab is open.
#[derive(Component)]
struct HotbarPanel;

#[derive(Component)]
struct BackpackRoot;

/// The bottom edge joins the backpack while Tab is open.
#[derive(Component)]
struct BackpackStoragePanel;

#[derive(Component)]
struct BackpackXpFill;

#[derive(Component)]
struct BackpackXpText;

#[derive(Component)]
struct BackpackButton;

/// Buttons in the character column that spend a perk point.
#[derive(Component, Clone, Copy)]
enum BackpackAction {
    SpendPerk(Attribute),
}

/// Every dynamic label in the backpack panel.
#[derive(Component, Clone, Copy)]
enum BackpackText {
    Capacity,
    Level,
    Coins,
    PerkPointsTitle,
    Max(Attribute),
    CategoryHeader(ItemCategory),
}

/// Cells whose visibility depends on capacity/category unlocks. Slot cells are
/// also click targets for the details panel.
#[derive(Component, Clone, Copy)]
enum BackpackCell {
    Slot(usize),
    CategorySection(ItemCategory),
    CategorySlot,
}

#[derive(Component)]
struct TooltipRoot;

#[derive(Component)]
struct TooltipTitle;

#[derive(Component)]
struct TooltipBody;

#[derive(Component)]
struct DragGhostRoot;

#[derive(Component)]
struct DragGhostIcon;

#[derive(Component)]
struct DragGhostCount;

#[derive(Component)]
struct CraftingList;

#[derive(Component, Clone, Copy)]
struct CraftingSelectButton(#[allow(dead_code)] usize);

#[derive(Component, Clone, Copy)]
struct CraftingCraftButton(#[allow(dead_code)] usize);

/* Crafting details panel removed: recipes show status inline and each row has a Craft button. */

#[derive(Component)]
struct BackpackSortButton;

type PanelQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Node,
        Option<&'static DebugPanel>,
        Option<&'static HintPanel>,
        Option<&'static CrosshairRoot>,
        Option<&'static HudRoot>,
    ),
    bevy::ecs::query::Or<(
        With<DebugPanel>,
        With<HintPanel>,
        With<CrosshairRoot>,
        With<HudRoot>,
    )>,
>;

fn setup_initial_loading_screen(mut commands: Commands, theme: Res<UiTheme>) {
    commands
        .spawn((
            InitialLoadingScreen,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(40.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.09, 0.12, 0.16)),
            ZIndex(500),
        ))
        .with_children(|root| {
            let panel_id = root
                .spawn((Node {
                    width: Val::Percent(100.0),
                    max_width: Val::Px(720.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(16.0),
                    ..default()
                },))
                .id();
            // omitted
            root.commands().entity(panel_id).with_children(|panel| {
                panel.spawn((
                    Text::new("Loading World"),
                    TextFont::from_font_size(34.0),
                    TextColor(theme.colors.text),
                ));
                panel.spawn((
                    InitialLoadingText,
                    Text::new("Preparing chunks..."),
                    TextFont::from_font_size(16.0),
                    TextColor(theme.colors.muted_text),
                ));
                let track_id = panel
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: Val::Px(16.0),
                            ..default()
                        },
                        BackgroundColor(theme.colors.bar_track),
                    ))
                    .id();
                // omitted
                panel.commands().entity(track_id).with_children(|track| {
                    let _fill = track
                        .spawn((
                            InitialLoadingFill,
                            Node {
                                width: Val::Percent(0.0),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                            BackgroundColor(theme.colors.accent),
                        ))
                        .id();
                    // omitted
                });
            });
        });
}

fn update_initial_loading_screen(
    initial_load: Res<InitialWorldLoad>,
    mut screen: Query<&mut Visibility, With<InitialLoadingScreen>>,
    mut text: Query<&mut Text, With<InitialLoadingText>>,
    mut fill: Query<&mut Node, With<InitialLoadingFill>>,
) {
    let visible = !initial_load.ready;
    for mut vis in &mut screen {
        *vis = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if let Ok(mut t) = text.single_mut() {
        let pct = (initial_load.progress() * 100.0).clamp(0.0, 100.0);
        **t = format!(
            "Loading surrounding area... {}/{} chunks ({:.0}%)",
            initial_load.loaded, initial_load.total, pct
        );
    }
    if let Ok(mut node) = fill.single_mut() {
        node.width = Val::Percent((initial_load.progress() * 100.0).clamp(0.0, 100.0));
    }
}

fn update_underwater_tint(
    lighting: Res<UndergroundLighting>,
    mut tint: Query<&mut BackgroundColor, With<UnderwaterTint>>,
) {
    let color = Color::srgba(0.07, 0.24, 0.40, lighting.water_blend() * 0.42);
    for mut background in &mut tint {
        if background.0 != color {
            background.0 = color;
        }
    }
}

fn setup_hud(
    mut commands: Commands,
    bindings: Res<InputBindings>,
    theme: Res<UiTheme>,
    icons: Res<UiIcons>,
) {
    let root = commands
        .spawn((
            Name::new("HUD"),
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
        ))
        .id();

    commands.spawn((
        UnderwaterTint,
        Name::new("UnderwaterTint"),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.07, 0.24, 0.40, 0.0)),
        GlobalZIndex(-1),
        Pickable::IGNORE,
    ));

    let dock = commands
        .spawn((
            BottomDock,
            Name::new("BottomDock"),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(DOCK_BOTTOM),
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                ..default()
            },
            GlobalZIndex(theme.layers.dock),
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .id();

    setup_character_hud(&mut commands, root, &theme, &icons);
    setup_compass(&mut commands, root, &theme);
    setup_coins_panel(&mut commands, root, &theme);
    setup_debug_panel(&mut commands, root, &theme);
    setup_aim(&mut commands, root, &theme);
    setup_pickup_prompt(&mut commands, root, &theme);
    setup_controls_hud(&mut commands, root, &bindings, &theme);
    setup_help(&mut commands, root, &bindings, &theme);
    setup_notifications(&mut commands, root, &theme, &icons);
    setup_bottom_panels(&mut commands, dock, &bindings, &theme, &icons);
    setup_backdrop(&mut commands, root, &theme);
    setup_tooltip(&mut commands, root, &theme);
    setup_drag_ghost(&mut commands, root, &theme, &icons);
}

/// Centres the inventory stack with the character and crafting panels beside it.
fn setup_bottom_panels(
    commands: &mut Commands,
    dock: Entity,
    bindings: &InputBindings,
    theme: &UiTheme,
    icons: &UiIcons,
) {
    let row = commands
        .spawn((
            PanelsRow,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(BOTTOM_PANELS_HEIGHT),
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(8.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(dock),
        ))
        .id();

    let left = commands
        .spawn((
            CharacterAnchor,
            Node {
                width: Val::Px(SIDE_PANEL_WIDTH),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::FlexEnd,
                padding: UiRect::bottom(Val::Px(
                    VITAL_INDICATOR_SIZE + (HOTBAR_HEIGHT - VITAL_INDICATOR_SIZE) * 0.5 + 8.0,
                )),
                row_gap: Val::Px(8.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(row),
        ))
        .id();
    setup_character_panel(commands, left, theme, icons);

    let centre = commands
        .spawn((
            Node {
                width: Val::Px(HOTBAR_WIDTH),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::FlexEnd,
                align_items: AlignItems::Center,
                row_gap: Val::Px(0.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(row),
        ))
        .id();
    setup_workbench_crafting_panel(commands, centre, theme);
    setup_backpack_storage(commands, centre, bindings, theme, icons);
    setup_stamina_and_insight(commands, centre, theme, icons);
    setup_hotbar(commands, centre, theme, icons);

    let right = commands
        .spawn((
            Node {
                width: Val::Px(SIDE_PANEL_WIDTH),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::FlexEnd,
                padding: UiRect::bottom(Val::Px(
                    VITAL_INDICATOR_SIZE + (HOTBAR_HEIGHT - VITAL_INDICATOR_SIZE) * 0.5 + 8.0,
                )),
                row_gap: Val::Px(8.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(row),
        ))
        .id();
    setup_crafting_panel(commands, right, theme);
}

/// Always-visible character HUD: portrait placeholder, level, XP progress bar,
/// and current XP / XP required. Compass remains separate and unchanged.
fn setup_character_hud(commands: &mut Commands, hud: Entity, theme: &UiTheme, icons: &UiIcons) {
    let panel = spawn_panel(
        commands,
        theme,
        hud,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(14.0),
            left: Val::Px(14.0),
            width: Val::Px(292.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(10.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.panel_border,
    );

    let header = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::FlexStart,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();

    let identity = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(header),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        identity,
        "ADVENTURER",
        theme.fonts.micro,
        theme.colors.muted_text,
        HudText,
    );
    spawn_text(
        commands,
        theme,
        identity,
        "Lv 1",
        theme.fonts.heading,
        theme.colors.accent,
        LevelText,
    );

    let time_block = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(1.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(header),
        ))
        .id();
    let time_row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::FlexEnd,
                column_gap: Val::Px(5.0),
                flex_shrink: 0.0,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(time_block),
        ))
        .id();
    spawn_hud_icon(
        commands,
        icons,
        time_row,
        crate::ui::icons::HudIcon::WeatherDay,
        20.0,
        WeatherIcon,
    );
    spawn_text(
        commands,
        theme,
        time_row,
        "Day 1",
        theme.fonts.small,
        theme.colors.text,
        WorldInfoDayText,
    );
    spawn_text(
        commands,
        theme,
        time_block,
        "07:00 AM",
        theme.fonts.micro,
        theme.colors.muted_text,
        WorldInfoClockText,
    );

    let xp_block = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();
    let xp_label_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(xp_block),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        xp_label_row,
        "EXPERIENCE",
        theme.fonts.micro,
        theme.colors.muted_text,
        HudText,
    );
    spawn_text(
        commands,
        theme,
        xp_label_row,
        "0 / 100 XP",
        theme.fonts.small,
        theme.colors.muted_text,
        XpText,
    );
    spawn_bar_overlay(
        commands,
        theme,
        xp_block,
        Val::Percent(100.0),
        10.0,
        0.0,
        theme.colors.xp,
        "",
        "",
        XpFill,
        HudText,
    );

    let footer = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::top(Val::Px(2.0)),
                border: UiRect::top(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(theme.colors.faint_border),
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();
    let biome_text = spawn_text(
        commands,
        theme,
        footer,
        "Plains",
        theme.fonts.small,
        theme.colors.muted_text,
        WorldInfoBiomeText,
    );
    commands.entity(biome_text).insert(TextLayout::no_wrap());
}

fn setup_compass(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    let anchor = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(14.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            ChildOf(hud),
        ))
        .id();

    let panel = spawn_panel(
        commands,
        theme,
        anchor,
        Node {
            width: Val::Px(COMPASS_WIDTH),
            height: Val::Px(30.0),
            overflow: Overflow::clip(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );

    // The centre mark stays fixed while the cardinal labels slide underneath it.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(COMPASS_WIDTH * 0.5 - 1.0),
            bottom: Val::Px(2.0),
            width: Val::Px(2.0),
            height: Val::Px(6.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        BackgroundColor(theme.colors.accent),
        ChildOf(panel),
    ));

    for (index, label) in ["N", "E", "S", "W"].into_iter().enumerate() {
        let band = commands
            .spawn((
                CompassCardinal(index),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(5.0),
                    left: Val::Px(compass_label_left(0.0)),
                    width: Val::Px(COMPASS_LABEL_WIDTH),
                    height: Val::Px(18.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(panel),
            ))
            .id();
        commands.spawn((
            CompassLabel(index),
            Text::new(label),
            TextFont::from_font_size(13.0),
            TextColor(theme.colors.muted_text),
            TextLayout::no_wrap(),
            theme.shadow,
            ChildOf(band),
        ));
    }
}

fn setup_coins_panel(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    let panel = spawn_panel(
        commands,
        theme,
        hud,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(14.0),
            right: Val::Px(14.0),
            width: Val::Px(168.0),
            padding: UiRect::axes(Val::Px(11.0), Val::Px(7.0)),
            align_items: AlignItems::Center,
            column_gap: Val::Px(9.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );

    let coin = commands
        .spawn((
            Node {
                width: Val::Px(18.0),
                height: Val::Px(18.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: theme.shape.rounded(),
                ..default()
            },
            BackgroundColor(theme.colors.coin),
            ChildOf(panel),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        coin,
        "C",
        theme.fonts.slot_count,
        theme.colors.coin_glyph,
        HudText,
    );

    let column = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        column,
        "COINS",
        theme.fonts.micro,
        theme.colors.muted_text,
        HudText,
    );
    spawn_text(
        commands,
        theme,
        column,
        "0",
        theme.fonts.value,
        theme.colors.coin,
        CoinsText,
    );
}

fn setup_debug_panel(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    let panel = spawn_panel(
        commands,
        theme,
        hud,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(84.0),
            right: Val::Px(14.0),
            display: Display::None,
            width: Val::Px(340.0),
            padding: UiRect::all(Val::Px(11.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands.entity(panel).insert(DebugPanel);

    spawn_text(
        commands,
        theme,
        panel,
        "Debug (F3)",
        theme.fonts.compass,
        theme.colors.accent,
        HudText,
    );
    spawn_text(
        commands,
        theme,
        panel,
        "",
        theme.fonts.body,
        theme.colors.text,
        DebugText,
    );
}

fn setup_aim(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    let anchor = commands
        .spawn((
            CrosshairRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(0.0),
                height: Val::Px(0.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            ChildOf(hud),
        ))
        .id();

    // A single centered dot marks the exact aim point.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(-2.5),
            top: Val::Px(-2.5),
            width: Val::Px(5.0),
            height: Val::Px(5.0),
            border_radius: theme.shape.circle(),
            ..default()
        },
        BackgroundColor(theme.colors.crosshair),
        Pickable::IGNORE,
        ChildOf(anchor),
    ));

    // Stamina is shown in the grouped survival bar row above the hotbar;
    // keep the crosshair unobstructed.
}

/// Stamina bar visually attached on top of the hotbar and Insight floating above it.
/// The stamina bar's bottom corners are flat so it connects to the hotbar as one component.
fn setup_stamina_and_insight(
    commands: &mut Commands,
    centre: Entity,
    theme: &UiTheme,
    icons: &UiIcons,
) {
    let anchor = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(STAMINA_RISE),
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(INSIGHT_RISE),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(centre),
        ))
        .id();

    // Insight HUD floats above the stamina bar with icon on the left and title
    // and description beside it.
    let insight = commands
        .spawn((
            InsightRoot,
            Node {
                display: Display::None,
                width: Val::Auto,
                max_width: Val::Px(STAMINA_BAR_WIDTH),
                padding: UiRect::axes(Val::Px(theme.spacing.sm), Val::Px(theme.spacing.xs)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(theme.spacing.sm),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            BackgroundColor(theme.colors.panel_bg_strong),
            BorderColor::all(theme.colors.faint_border),
            Pickable::IGNORE,
            ChildOf(anchor),
        ))
        .id();
    commands.spawn((
        InsightIcon,
        Node {
            width: Val::Px(18.0),
            height: Val::Px(18.0),
            flex_shrink: 0.0,
            ..default()
        },
        ImageNode::new(icons.hud(crate::ui::icons::HudIcon::Insight)),
        Pickable::IGNORE,
        ChildOf(insight),
    ));
    let insight_text = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(1.0),
                flex_grow: 1.0,
                overflow: Overflow::clip_x(),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(insight),
        ))
        .id();
    let title = spawn_text(
        commands,
        theme,
        insight_text,
        "",
        theme.fonts.body,
        theme.colors.text,
        InsightTitle,
    );
    commands.entity(title).insert(TextLayout::no_wrap());
    let desc = spawn_text(
        commands,
        theme,
        insight_text,
        "",
        theme.fonts.small,
        theme.colors.muted_text,
        InsightDescription,
    );
    commands.entity(desc).insert(TextLayout::no_wrap());

    // Custom bar: flat bottom corners to visually connect to the hotbar beneath it.
    let rounded = theme.shape.rounded_small();
    let zero = BorderRadius::all(Val::Px(0.0));
    let connected_radius = BorderRadius {
        top_left: rounded.top_left,
        top_right: rounded.top_right,
        bottom_left: zero.bottom_left,
        bottom_right: zero.bottom_right,
    };
    let stamina_bar = commands
        .spawn((
            StaminaBar,
            Node {
                width: Val::Px(STAMINA_BAR_WIDTH),
                height: Val::Px(STAMINA_BAR_HEIGHT),
                flex_grow: 0.0,
                overflow: Overflow::clip(),
                border: theme.shape.border(),
                border_radius: connected_radius,
                ..default()
            },
            BackgroundColor(theme.colors.bar_track),
            BorderColor::all(theme.colors.faint_border),
            Pickable::IGNORE,
            ChildOf(anchor),
        ))
        .id();
    commands.spawn((
        StaminaFill,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            border_radius: connected_radius,
            ..default()
        },
        BackgroundColor(theme.colors.stamina),
        Pickable::IGNORE,
        ChildOf(stamina_bar),
    ));
    let stamina_labels = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(stamina_bar),
        ))
        .id();
    let stamina_font = (STAMINA_BAR_HEIGHT * 0.58).clamp(theme.fonts.small, theme.fonts.body);
    commands.spawn((
        StaminaText,
        Text::new("100 / 100"),
        TextFont::from_font_size(stamina_font),
        TextColor(theme.colors.text),
        TextLayout::no_wrap().with_justify(Justify::Center),
        theme.shadow,
        Pickable::IGNORE,
        ChildOf(stamina_labels),
    ));
}

fn setup_pickup_prompt(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    let anchor = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(0.0),
                height: Val::Px(0.0),
                ..default()
            },
            ChildOf(hud),
        ))
        .id();

    commands.spawn((
        PickupPromptText,
        Text::new(""),
        TextFont::from_font_size(12.0),
        TextColor(theme.colors.text),
        theme.shadow,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(-190.0),
            top: Val::Px(68.0),
            width: Val::Px(380.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        ChildOf(anchor),
    ));
}

fn setup_hotbar(commands: &mut Commands, centre: Entity, theme: &UiTheme, icons: &UiIcons) {
    let bottom = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(centre),
        ))
        .id();

    // Wrapper sized to the hotbar so hunger/thirst can sit at its sides,
    // separate from (not connected to) the hotbar.
    let hotbar_anchor = commands
        .spawn((
            Node {
                width: Val::Px(HOTBAR_WIDTH),
                height: Val::Px(HOTBAR_HEIGHT),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(bottom),
        ))
        .id();

    let hotbar_row = spawn_panel(
        commands,
        theme,
        hotbar_anchor,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(HOTBAR_GAP),
            padding: UiRect::axes(Val::Px(HOTBAR_PADDING_X), Val::Px(HOTBAR_PADDING_Y)),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.faint_border,
    );
    commands
        .entity(hotbar_row)
        .insert((HotbarPanel, Pickable::IGNORE));

    // Off-hand/attachment slot on the left of the hotbar.
    let offhand_source = SlotSource::Attachment(0);
    let offhand = spawn_inventory_slot(commands, hotbar_row, BackpackCell::CategorySlot, theme);
    commands.entity(offhand).insert(offhand_source);
    spawn_slot_contents(commands, offhand, offhand_source, theme, icons);

    let pockets_row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(POCKET_GAP),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(hotbar_row),
        ))
        .id();
    for index in 0..POCKET_COUNT {
        let source = SlotSource::Pocket(index);
        let slot = spawn_inventory_slot(commands, pockets_row, PocketSlot(index), theme);
        commands.entity(slot).insert(source);
        spawn_slot_contents(commands, slot, source, theme, icons);
    }

    // Hunger on the left side of the hotbar, thirst on the right side.
    // Separate square indicators with icons, progress fill, and actual values.
    spawn_side_vital(
        commands,
        theme,
        icons,
        hotbar_anchor,
        crate::ui::icons::HudIcon::Hunger,
        "100",
        theme.colors.hunger,
        HungerFill,
        HungerValueText,
        true,
    );
    spawn_side_vital(
        commands,
        theme,
        icons,
        hotbar_anchor,
        crate::ui::icons::HudIcon::Thirst,
        "100",
        theme.colors.hydration,
        ThirstFill,
        ThirstValueText,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_side_vital<Fill: Component, Value: Component>(
    commands: &mut Commands,
    theme: &UiTheme,
    icons: &UiIcons,
    hotbar_anchor: Entity,
    icon: crate::ui::icons::HudIcon,
    initial: &str,
    accent: Color,
    fill_marker: Fill,
    value_marker: Value,
    left_side: bool,
) {
    let mut node = Node {
        position_type: PositionType::Absolute,
        bottom: Val::Px((HOTBAR_HEIGHT - VITAL_INDICATOR_SIZE) * 0.5),
        width: Val::Px(VITAL_INDICATOR_SIZE),
        height: Val::Px(VITAL_INDICATOR_SIZE),
        overflow: Overflow::clip(),
        flex_direction: FlexDirection::Column,
        align_items: AlignItems::Center,
        justify_content: JustifyContent::Center,
        row_gap: Val::Px(2.0),
        // Progress-bar style track/border.
        border: theme.shape.border(),
        border_radius: theme.shape.rounded_small(),
        flex_shrink: 0.0,
        ..default()
    };
    let side_offset = -(VITAL_INDICATOR_SIZE + VITAL_GAP_FROM_HOTBAR);
    if left_side {
        node.left = Val::Px(side_offset);
    } else {
        node.right = Val::Px(side_offset);
    }

    let indicator = commands
        .spawn((
            node,
            BackgroundColor(theme.colors.bar_track),
            BorderColor::all(theme.colors.panel_border),
            Pickable::IGNORE,
            ChildOf(hotbar_anchor),
        ))
        .id();

    // Progress fill behind icon/value.
    commands.spawn((
        fill_marker,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            bottom: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(theme::with_alpha(accent, 0.28)),
        Pickable::IGNORE,
        ChildOf(indicator),
    ));

    commands.spawn((
        Node {
            width: Val::Px(18.0),
            height: Val::Px(18.0),
            ..default()
        },
        ImageNode::new(icons.hud(icon)),
        Pickable::IGNORE,
        ChildOf(indicator),
    ));
    let value = spawn_text(
        commands,
        theme,
        indicator,
        initial,
        theme.fonts.body,
        theme.colors.text,
        value_marker,
    );
    commands
        .entity(value)
        .insert(TextLayout::no_wrap().with_justify(Justify::Center));
}

/// Shared slot look for the hotbar and backpack storage.
fn spawn_inventory_slot<M: Component>(
    commands: &mut Commands,
    parent: Entity,
    marker: M,
    theme: &UiTheme,
) -> Entity {
    spawn_inventory_slot_sized(commands, parent, marker, theme, HOTBAR_SLOT_SIZE)
}

fn spawn_inventory_slot_sized<M: Component>(
    commands: &mut Commands,
    parent: Entity,
    marker: M,
    theme: &UiTheme,
    size: f32,
) -> Entity {
    let slot = spawn_panel(
        commands,
        theme,
        parent,
        Node {
            width: Val::Px(size),
            height: Val::Px(size),
            padding: UiRect::all(Val::Px(3.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: theme.shape.emphasis_border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );
    commands.entity(slot).insert((
        marker,
        bevy::picking::Pickable::default(),
        Hovered::default(),
    ));
    slot
}

/// Spawn hidden item visuals for one slot. They are revealed only when the
/// source slot actually holds a stack.
fn spawn_slot_contents(
    commands: &mut Commands,
    slot: Entity,
    source: SlotSource,
    theme: &UiTheme,
    icons: &UiIcons,
) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(6.0),
            top: Val::Px(6.0),
            right: Val::Px(6.0),
            bottom: Val::Px(6.0),
            display: Display::None,
            padding: UiRect::all(Val::Px(3.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        ImageNode::new(icons.item(ItemId::Turf)).with_color(Color::WHITE),
        SlotVisual {
            source,
            kind: SlotVisualKind::Icon,
        },
        Pickable::IGNORE,
        ChildOf(slot),
    ));

    let count = spawn_text(
        commands,
        theme,
        slot,
        "",
        theme.fonts.slot_count,
        theme.colors.text,
        SlotVisual {
            source,
            kind: SlotVisualKind::Count,
        },
    );
    commands.entity(count).insert((
        TextLayout::no_wrap(),
        Node {
            position_type: PositionType::Absolute,
            display: Display::None,
            right: Val::Px(3.0),
            bottom: Val::Px(1.0),
            ..default()
        },
        Pickable::IGNORE,
    ));
}

/// Controls HUD split between left and right sides of the screen.
fn setup_controls_hud(
    commands: &mut Commands,
    hud: Entity,
    bindings: &InputBindings,
    theme: &UiTheme,
) {
    let panel_node = |left: Option<f32>, right: Option<f32>| Node {
        position_type: PositionType::Absolute,
        left: left.map(Val::Px).unwrap_or(Val::Auto),
        right: right.map(Val::Px).unwrap_or(Val::Auto),
        bottom: Val::Px(14.0),
        padding: UiRect::axes(Val::Px(theme.spacing.sm), Val::Px(theme.spacing.xs + 2.0)),
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(14.0),
        border_radius: theme.shape.rounded(),
        ..default()
    };

    let left = spawn_panel(
        commands,
        theme,
        hud,
        panel_node(Some(14.0), None),
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );
    let left_entries = [
        (key_label(bindings.help), "Help"),
        (key_label(bindings.chat), "Chat"),
    ];
    for (key, action) in left_entries {
        spawn_hint_row(commands, left, &key, action, theme);
    }

    let right = spawn_panel(
        commands,
        theme,
        hud,
        panel_node(None, Some(14.0)),
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );
    let right_entries = [
        (key_label(bindings.backpack), "Backpack"),
        (key_label(bindings.map), "Map"),
    ];
    for (key, action) in right_entries {
        spawn_hint_row(commands, right, &key, action, theme);
    }
}

fn spawn_hint_row(
    commands: &mut Commands,
    parent: Entity,
    key: &str,
    action: &str,
    theme: &UiTheme,
) {
    let row = commands
        .spawn((
            Node {
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(parent),
        ))
        .id();
    spawn_keycap(commands, theme, row, key, 46.0);
    spawn_text(
        commands,
        theme,
        row,
        action,
        theme.fonts.small,
        theme.colors.muted_text,
        HudText,
    );
}

fn setup_help(commands: &mut Commands, hud: Entity, bindings: &InputBindings, theme: &UiTheme) {
    let overlay = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            ChildOf(hud),
        ))
        .id();

    let panel = spawn_panel(
        commands,
        theme,
        overlay,
        Node {
            display: Display::None,
            padding: UiRect::axes(Val::Px(20.0), Val::Px(16.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands.entity(panel).insert(HintPanel);

    spawn_text(
        commands,
        theme,
        panel,
        "Controls",
        theme.fonts.title,
        theme.colors.accent,
        HudText,
    );
    spawn_text(
        commands,
        theme,
        panel,
        format!("Press {} to close", key_label(bindings.help)),
        theme.fonts.small,
        theme.colors.muted_text,
        HudText,
    );

    let hints = [
        (
            "WASD".to_string(),
            format!(
                "Move ({}/{}/{}/{})",
                key_label(bindings.move_forward),
                key_label(bindings.move_left),
                key_label(bindings.move_back),
                key_label(bindings.move_right)
            ),
        ),
        (key_label(bindings.jump), "Jump (hold)".to_string()),
        (
            key_label(bindings.sprint),
            "Sprint (uses stamina)".to_string(),
        ),
        (key_label(bindings.crouch), "Crouch".to_string()),
        (
            "LMB".to_string(),
            "Use tool: mine / dismantle / shape / rotate".to_string(),
        ),
        (
            "RMB".to_string(),
            "Place selected block/structure; interact; pick up drops".to_string(),
        ),
        (
            key_label(bindings.rotate_place),
            "Rotate placeable structure preview".to_string(),
        ),
        (
            key_label(bindings.backpack),
            "Backpack, crafting and perks".to_string(),
        ),
        (key_label(bindings.chat), "Chat and commands".to_string()),
        ("1-8".to_string(), "Select hotbar slot".to_string()),
        ("Wheel".to_string(), "Cycle hotbar slots".to_string()),
        (
            key_label(bindings.map),
            "Open overhead map (Live/Explore)".to_string(),
        ),
        (key_label(bindings.pause), "Pause and settings".to_string()),
        (
            key_label(bindings.debug_overlay),
            "Debug overlay".to_string(),
        ),
        (key_label(bindings.save), "Save".to_string()),
        (key_label(bindings.load), "Load".to_string()),
        (key_label(bindings.screenshot), "Screenshot".to_string()),
    ];
    for (key, action) in hints {
        spawn_hint_row(commands, panel, &key, &action, theme);
    }
}

fn setup_notifications(commands: &mut Commands, hud: Entity, theme: &UiTheme, icons: &UiIcons) {
    for index in 0..MAX_NOTIFICATIONS {
        let row = spawn_panel(
            commands,
            theme,
            hud,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(14.0),
                bottom: Val::Px(68.0 + index as f32 * (36.0 + NOTIFICATION_GAP)),
                width: Val::Px(244.0),
                height: Val::Px(36.0),
                padding: UiRect::axes(Val::Px(7.0), Val::Px(5.0)),
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            theme.colors.panel_bg_strong,
            theme.colors.faint_border,
        );
        commands.entity(row).insert(NotificationRow(index));

        spawn_item_icon(
            commands,
            icons,
            row,
            ItemId::Turf,
            23.0,
            NotificationIcon(index),
        );

        let column = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    overflow: Overflow::clip(),
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        spawn_text(
            commands,
            theme,
            column,
            "",
            theme.fonts.slot_count,
            theme.colors.text,
            NotificationTitle(index),
        );
        commands.spawn((
            NotificationDetail(index),
            Text::new(""),
            TextFont::from_font_size(theme.fonts.small),
            TextColor(theme.colors.muted_text),
            TextLayout::no_wrap(),
            Node {
                display: Display::None,
                ..default()
            },
            ChildOf(column),
        ));
    }
}

/// The Tab backdrop sits behind the bottom dock so survival readouts stay
/// visible while the inventory panels are open.
fn setup_backdrop(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    commands.spawn((
        BackpackRoot,
        Name::new("Backdrop"),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            display: Display::None,
            ..default()
        },
        BackgroundColor(theme.colors.backdrop_bg),
        GlobalZIndex(theme.layers.backdrop),
        Pickable::IGNORE,
        ChildOf(hud),
    ));
}

/// The left-hand character sheet uses the same footprint and typography as the
/// right-hand details panel.
fn setup_character_panel(
    commands: &mut Commands,
    anchor: Entity,
    theme: &UiTheme,
    icons: &UiIcons,
) {
    let character = spawn_panel(
        commands,
        theme,
        anchor,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            padding: UiRect::new(
                Val::Px(CHARACTER_PANEL_PADDING_X),
                Val::Px(CHARACTER_PANEL_PADDING_X),
                Val::Px(10.0),
                Val::Px(CHARACTER_PANEL_PADDING_X),
            ),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::FlexStart,
            row_gap: Val::Px(6.0),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands
        .entity(character)
        .insert((CharacterPanel, Pickable::IGNORE));
    let header = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(8.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(character),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        header,
        "CHARACTER",
        theme.fonts.heading,
        theme.colors.accent,
        HudText,
    );
    let level_title = spawn_text(
        commands,
        theme,
        header,
        "Level 1",
        theme.fonts.body,
        theme.colors.text,
        BackpackText::Level,
    );
    commands
        .entity(level_title)
        .insert(TextLayout::no_wrap().with_justify(Justify::Right));

    spawn_bar_overlay(
        commands,
        theme,
        character,
        Val::Percent(100.0),
        STAT_BAR_HEIGHT,
        0.0,
        theme.colors.xp,
        "0 / 100 XP",
        "",
        BackpackXpFill,
        BackpackXpText,
    );

    spawn_backpack_stat_row(
        commands,
        character,
        "Coins",
        BackpackText::Coins,
        theme.colors.coin,
        theme,
    );

    commands.spawn((
        Node {
            height: Val::Px(1.0),
            width: Val::Percent(100.0),
            margin: UiRect::vertical(Val::Px(2.0)),
            ..default()
        },
        BackgroundColor(theme.colors.faint_border),
        Pickable::IGNORE,
        ChildOf(character),
    ));

    let stats_header = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(character),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        stats_header,
        "STATS",
        theme.fonts.body,
        theme.colors.accent,
        HudText,
    );
    let perk_title = spawn_text(
        commands,
        theme,
        stats_header,
        "0 PERK POINTS",
        theme.fonts.small,
        theme.colors.muted_text,
        BackpackText::PerkPointsTitle,
    );
    commands
        .entity(perk_title)
        .insert(TextLayout::no_wrap().with_justify(Justify::Right));
    for attribute in Attribute::ALL {
        spawn_backpack_perk_row(commands, character, attribute, theme);
    }

    commands.spawn((
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(character),
    ));

    spawn_text(
        commands,
        theme,
        character,
        "BACKPACK ATTACHMENTS",
        theme.fonts.body,
        theme.colors.accent,
        HudText,
    );
    let attachments = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::FlexStart,
                column_gap: Val::Px(CHARACTER_SLOT_GAP),
                margin: UiRect::top(Val::Px(2.0)),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(character),
        ))
        .id();
    for index in 1..ATTACHMENT_SLOTS {
        let source = SlotSource::Attachment(index);
        let slot = spawn_inventory_slot_sized(
            commands,
            attachments,
            BackpackCell::CategorySlot,
            theme,
            CHARACTER_SLOT_SIZE,
        );
        commands.entity(slot).insert(source);
        spawn_slot_contents(commands, slot, source, theme, icons);
    }
    // 4th visual slot placeholder to make attachment row feel complete (reserved)
    let reserved = spawn_panel(
        commands,
        theme,
        attachments,
        Node {
            width: Val::Px(CHARACTER_SLOT_SIZE),
            height: Val::Px(CHARACTER_SLOT_SIZE),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: theme.shape.emphasis_border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.bar_track,
        theme.colors.faint_border,
    );
    commands.entity(reserved).insert(Pickable::IGNORE);
    spawn_text(
        commands,
        theme,
        reserved,
        "",
        theme.fonts.small,
        theme.colors.muted_text,
        HudText,
    );
}

/// Storage slots sit directly above the hotbar in the centre column.
fn setup_backpack_storage(
    commands: &mut Commands,
    centre: Entity,
    _bindings: &InputBindings,
    theme: &UiTheme,
    icons: &UiIcons,
) {
    let storage = spawn_panel(
        commands,
        theme,
        centre,
        Node {
            display: Display::None,
            width: Val::Px(HOTBAR_WIDTH),
            margin: UiRect {
                top: Val::Px(8.0),
                bottom: Val::Px(0.0),
                ..default()
            },
            padding: UiRect {
                left: Val::Px(8.0),
                right: Val::Px(8.0),
                top: Val::Px(8.0),
                // Keep slot content clear of the stamina bar that overlaps the
                // hotbar/top edge while preserving the visual connection below.
                bottom: Val::Px(SURVIVAL_INSET + STAMINA_BAR_HEIGHT - 1.0),
            },
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            border_radius: storage_radius(false, theme),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands
        .entity(storage)
        .insert((BackpackStoragePanel, Pickable::IGNORE));

    let header = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(8.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(storage),
        ))
        .id();

    spawn_text(
        commands,
        theme,
        header,
        "Backpack",
        theme.fonts.body,
        theme.colors.accent,
        BackpackText::Capacity,
    );

    let sort_button = spawn_panel(
        commands,
        theme,
        header,
        Node {
            width: Val::Px(24.0),
            height: Val::Px(20.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.button_bg,
        theme.colors.border_idle,
    );
    commands.entity(sort_button).insert((
        Button,
        BackpackSortButton,
        bevy::picking::Pickable::default(),
        Hovered::default(),
    ));
    commands.spawn((
        Node {
            width: Val::Px(12.0),
            height: Val::Px(12.0),
            ..default()
        },
        ImageNode {
            image: icons.hud(crate::ui::icons::HudIcon::Sort),
            color: theme.colors.text,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(sort_button),
    ));

    let grid = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_wrap: FlexWrap::Wrap,
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(4.0),
                row_gap: Val::Px(4.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(storage),
        ))
        .id();

    for index in 0..MAX_CAPACITY {
        let source = SlotSource::Storage(index);
        let cell = spawn_inventory_slot(commands, grid, BackpackCell::Slot(index), theme);
        commands.entity(cell).insert(source);
        spawn_slot_contents(commands, cell, source, theme, icons);
    }

    let categories = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                flex_wrap: FlexWrap::Wrap,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(storage),
        ))
        .id();

    for category in ItemCategory::ALL {
        let section = commands
            .spawn((
                BackpackCell::CategorySection(category),
                Node {
                    width: Val::Px(136.0),
                    display: Display::None,
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(3.0),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(categories),
            ))
            .id();

        spawn_text(
            commands,
            theme,
            section,
            category.display_name(),
            theme.fonts.micro,
            category.color(),
            BackpackText::CategoryHeader(category),
        );

        let row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: Val::Px(4.0),
                    row_gap: Val::Px(4.0),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(section),
            ))
            .id();

        for index in 0..CATEGORY_SLOTS_PER_UNLOCK {
            let source = SlotSource::Category(category, index);
            let cell = spawn_inventory_slot(commands, row, BackpackCell::CategorySlot, theme);
            commands.entity(cell).insert(source);
            spawn_slot_contents(commands, cell, source, theme, icons);
        }
    }
}

/// Crafting panel on the right of the inventory stack.
#[derive(Component)]
struct CraftingScrollArea;

#[derive(Component)]
struct CraftingScrollbarTrack;

#[derive(Component)]
struct CraftingScrollbarThumb;

#[derive(Component)]
struct WorkbenchCraftingPanel;

#[derive(Component)]
struct WorkbenchTabsRow;

#[derive(Component)]
struct WorkbenchCategoryTab(crate::crafting::RecipeCategory);

#[derive(Component)]
struct WorkbenchRecipeList;

#[derive(Component)]
struct WorkbenchRecipeSelect(usize);

#[derive(Component)]
struct WorkbenchCraftButton(u32);

type WorkbenchTabItem<'w> = (
    &'w WorkbenchCategoryTab,
    &'w mut Node,
    &'w mut BackgroundColor,
    &'w mut BorderColor,
);
type WorkbenchTabQuery<'w, 's> = Query<
    'w,
    's,
    WorkbenchTabItem<'static>,
    (
        Without<WorkbenchCraftButton>,
        Without<WorkbenchBody>,
        Without<WorkbenchEmptyState>,
    ),
>;
type WorkbenchBodyNodeQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Node,
    (
        With<WorkbenchBody>,
        Without<WorkbenchEmptyState>,
        Without<WorkbenchCategoryTab>,
    ),
>;
type WorkbenchEmptyNodeQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Node,
    (
        With<WorkbenchEmptyState>,
        Without<WorkbenchBody>,
        Without<WorkbenchCategoryTab>,
    ),
>;
type WorkbenchDetailNameQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Text,
    (
        With<WorkbenchDetailName>,
        Without<WorkbenchDetailDesc>,
        Without<WorkbenchDetailIngredients>,
    ),
>;
type WorkbenchDetailDescQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Text,
    (
        With<WorkbenchDetailDesc>,
        Without<WorkbenchDetailName>,
        Without<WorkbenchDetailIngredients>,
    ),
>;
type WorkbenchDetailIngredientsQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Text,
    (
        With<WorkbenchDetailIngredients>,
        Without<WorkbenchDetailName>,
        Without<WorkbenchDetailDesc>,
    ),
>;

#[derive(Component)]
struct WorkbenchDetailName;

#[derive(Component)]
struct WorkbenchDetailDesc;

#[derive(Component)]
struct WorkbenchDetailIngredients;

#[derive(Component)]
struct WorkbenchDetailPreview;

#[derive(Component)]
struct WorkbenchEmptyState;

#[derive(Component)]
struct WorkbenchBody;

fn spawn_crafting_list_ui(
    commands: &mut Commands,
    theme: &UiTheme,
    panel: Entity,
    list_marker: impl Bundle,
    scroll_marker: impl Bundle,
    track_marker: impl Bundle,
    thumb_marker: impl Bundle,
) {
    let scroll_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                min_height: Val::Px(0.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(4.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();

    commands.spawn((
        list_marker,
        scroll_marker,
        ScrollPosition::default(),
        Node {
            flex_grow: 1.0,
            min_height: Val::Px(0.0),
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(3.0),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        bevy::picking::Pickable::default(),
        Hovered::default(),
        ChildOf(scroll_row),
    ));

    let track = spawn_panel(
        commands,
        theme,
        scroll_row,
        Node {
            width: Val::Px(6.0),
            height: Val::Percent(100.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg,
        theme.colors.faint_border,
    );
    commands
        .entity(track)
        .insert((track_marker, Pickable::IGNORE));
    commands.spawn((
        thumb_marker,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            min_height: Val::Px(16.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        BackgroundColor(theme.colors.border_hover),
        Pickable::IGNORE,
        ChildOf(track),
    ));
}

fn setup_crafting_panel(commands: &mut Commands, anchor: Entity, theme: &UiTheme) {
    let panel = spawn_panel(
        commands,
        theme,
        anchor,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            display: Display::None,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
            padding: UiRect::new(
                Val::Px(CHARACTER_PANEL_PADDING_X),
                Val::Px(CHARACTER_PANEL_PADDING_X),
                Val::Px(10.0),
                Val::Px(CHARACTER_PANEL_PADDING_X),
            ),
            overflow: Overflow::clip(),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands
        .entity(panel)
        .insert((CraftingPanel, Pickable::IGNORE));

    commands.spawn((
        CraftingPanelTitle,
        HudText,
        Text::new("HAND CRAFTING"),
        TextFont::from_font_size(theme.fonts.heading),
        TextColor(theme.colors.accent),
        Node::default(),
        Pickable::IGNORE,
        ChildOf(panel),
    ));
    spawn_crafting_list_ui(
        commands,
        theme,
        panel,
        CraftingList,
        CraftingScrollArea,
        CraftingScrollbarTrack,
        CraftingScrollbarThumb,
    );
}

fn setup_workbench_crafting_panel(commands: &mut Commands, parent: Entity, theme: &UiTheme) {
    let panel = spawn_panel(
        commands,
        theme,
        parent,
        Node {
            width: Val::Px(HOTBAR_WIDTH),
            height: Val::Px(292.0),
            display: Display::None,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
            padding: UiRect::all(Val::Px(8.0)),
            margin: UiRect::bottom(Val::Px(4.0)),
            overflow: Overflow::clip(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands
        .entity(panel)
        .insert((WorkbenchCraftingPanel, Pickable::IGNORE));

    commands.spawn((
        HudText,
        Text::new("WORKBENCH"),
        TextFont::from_font_size(theme.fonts.body),
        TextColor(theme.colors.accent),
        Node::default(),
        Pickable::IGNORE,
        ChildOf(panel),
    ));

    // Tabs row - categories with no recipes are hidden by refresh system.
    let tabs = commands
        .spawn((
            WorkbenchTabsRow,
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(4.0),
                flex_wrap: FlexWrap::Wrap,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();
    for cat in crate::crafting::RecipeCategory::ALL {
        let tab = spawn_panel(
            commands,
            theme,
            tabs,
            Node {
                display: Display::None,
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            theme.colors.button_bg,
            theme.colors.border_idle,
        );
        commands.entity(tab).insert((
            Button,
            WorkbenchCategoryTab(cat),
            bevy::picking::Pickable::default(),
            Hovered::default(),
        ));
        spawn_text(
            commands,
            theme,
            tab,
            cat.display_name(),
            theme.fonts.micro,
            theme.colors.text,
            HudText,
        );
    }

    // Empty state
    let empty = commands
        .spawn((
            WorkbenchEmptyState,
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(4.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        empty,
        "No recipes available at this station.",
        theme.fonts.small,
        theme.colors.muted_text,
        HudText,
    );
    spawn_text(
        commands,
        theme,
        empty,
        "Craft new stations or components as they become available.",
        theme.fonts.micro,
        theme.colors.muted_text,
        HudText,
    );

    // Body: left recipe list, right details
    let body = commands
        .spawn((
            WorkbenchBody,
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                min_height: Val::Px(0.0),
                display: Display::None,
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(panel),
        ))
        .id();

    // Left: scrollable recipe list
    let list = commands
        .spawn((
            WorkbenchRecipeList,
            ScrollPosition::default(),
            Node {
                width: Val::Px(180.0),
                min_height: Val::Px(0.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            bevy::picking::Pickable::default(),
            Hovered::default(),
            ChildOf(body),
        ))
        .id();
    let _ = list;

    // Right: details
    let details = commands
        .spawn((
            Node {
                flex_grow: 1.0,
                min_width: Val::Px(0.0),
                min_height: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                overflow: Overflow::clip(),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(body),
        ))
        .id();

    commands.spawn((
        WorkbenchDetailPreview,
        Node {
            width: Val::Px(56.0),
            height: Val::Px(56.0),
            ..default()
        },
        ImageNode {
            image: Handle::default(),
            color: Color::WHITE,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(details),
    ));
    commands.spawn((
        WorkbenchDetailName,
        HudText,
        Text::new(""),
        TextFont::from_font_size(theme.fonts.body),
        TextColor(theme.colors.accent),
        Node::default(),
        Pickable::IGNORE,
        ChildOf(details),
    ));
    commands.spawn((
        WorkbenchDetailDesc,
        HudText,
        Text::new(""),
        TextFont::from_font_size(theme.fonts.micro),
        TextColor(theme.colors.muted_text),
        Node {
            max_width: Val::Percent(100.0),
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(details),
    ));
    commands.spawn((
        WorkbenchDetailIngredients,
        HudText,
        Text::new(""),
        TextFont::from_font_size(theme.fonts.micro),
        TextColor(theme.colors.text),
        Node {
            max_width: Val::Percent(100.0),
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(details),
    ));

    let buttons = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(4.0),
                margin: UiRect::top(Val::Auto),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(details),
        ))
        .id();
    for (label, qty) in [("CRAFT", 1u32), ("CRAFT 5", 5), ("CRAFT 10", 10)] {
        let btn = spawn_panel(
            commands,
            theme,
            buttons,
            Node {
                flex_grow: 1.0,
                padding: UiRect::axes(Val::Px(4.0), Val::Px(5.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            theme.colors.button_bg,
            theme.colors.border_idle,
        );
        commands.entity(btn).insert((
            Button,
            WorkbenchCraftButton(qty),
            bevy::picking::Pickable::default(),
            Hovered::default(),
        ));
        spawn_text(
            commands,
            theme,
            btn,
            label,
            theme.fonts.micro,
            theme.colors.text,
            HudText,
        );
    }
}

fn setup_tooltip(commands: &mut Commands, hud: Entity, theme: &UiTheme) {
    let root = spawn_panel(
        commands,
        theme,
        hud,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            display: Display::None,
            max_width: Val::Px(220.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(6.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );
    commands.entity(root).insert((
        TooltipRoot,
        GlobalZIndex(theme.layers.pause + 10),
        Pickable::IGNORE,
    ));

    let title = spawn_text(
        commands,
        theme,
        root,
        "",
        theme.fonts.small,
        theme.colors.accent,
        TooltipTitle,
    );
    commands.entity(title).insert(TextLayout {
        linebreak: LineBreak::WordBoundary,
        ..default()
    });

    let body = spawn_text(
        commands,
        theme,
        root,
        "",
        theme.fonts.micro,
        theme.colors.text,
        TooltipBody,
    );
    commands.entity(body).insert(TextLayout {
        linebreak: LineBreak::WordBoundary,
        ..default()
    });
}

fn setup_drag_ghost(commands: &mut Commands, hud: Entity, theme: &UiTheme, icons: &UiIcons) {
    let root = commands
        .spawn((
            DragGhostRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Px(HOTBAR_SLOT_SIZE - 8.0),
                height: Val::Px(HOTBAR_SLOT_SIZE - 8.0),
                display: Display::None,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            GlobalZIndex(theme.layers.pause + 20),
            Pickable::IGNORE,
            ChildOf(hud),
        ))
        .id();
    commands.spawn((
        DragGhostIcon,
        Node {
            width: Val::Px(HOTBAR_SLOT_SIZE - 16.0),
            height: Val::Px(HOTBAR_SLOT_SIZE - 16.0),
            ..default()
        },
        ImageNode {
            image: icons.item(ItemId::Turf),
            color: Color::srgba(1.0, 1.0, 1.0, 0.9),
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(root),
    ));
    commands.spawn((
        DragGhostCount,
        Text::new(""),
        TextFont::from_font_size(theme.fonts.slot_count),
        TextColor(theme.colors.text),
        TextLayout::no_wrap(),
        theme.shadow,
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(2.0),
            bottom: Val::Px(0.0),
            display: Display::None,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(root),
    ));
}

fn spawn_backpack_stat_row(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    value: BackpackText,
    color: Color,
    theme: &UiTheme,
) {
    let row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(16.0),
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                ..default()
            },
            ChildOf(parent),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        row,
        label,
        theme.fonts.small,
        theme.colors.muted_text,
        HudText,
    );
    commands.spawn((
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(row),
    ));
    let text = spawn_text(commands, theme, row, "", theme.fonts.body, color, value);
    commands.entity(text).insert((
        TextLayout::no_wrap().with_justify(Justify::Right),
        Node {
            width: Val::Auto,
            max_width: Val::Px(96.0),
            ..default()
        },
    ));
}

fn spawn_backpack_perk_row(
    commands: &mut Commands,
    parent: Entity,
    attribute: Attribute,
    theme: &UiTheme,
) {
    let row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(30.0),
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                padding: UiRect::axes(Val::Px(2.0), Val::Px(2.0)),
                ..default()
            },
            ChildOf(parent),
        ))
        .id();
    let color = match attribute {
        Attribute::Stamina => theme.colors.accent,
        Attribute::Hunger => theme.colors.hunger,
        Attribute::Hydration => theme.colors.hydration,
    };
    spawn_text(
        commands,
        theme,
        row,
        attribute.label(),
        theme.fonts.body,
        color,
        HudText,
    );
    commands.spawn((
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(row),
    ));
    let value = spawn_text(
        commands,
        theme,
        row,
        "",
        theme.fonts.body,
        color,
        BackpackText::Max(attribute),
    );
    commands.entity(value).insert((
        TextLayout::justify(Justify::Right),
        Node {
            width: Val::Px(40.0),
            ..default()
        },
    ));

    let button = spawn_panel(
        commands,
        theme,
        row,
        Node {
            width: Val::Px(28.0),
            height: Val::Px(28.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: theme.shape.emphasis_border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.button_bg,
        theme.colors.border_idle,
    );
    commands.entity(button).insert((
        Button,
        BackpackButton,
        BackpackAction::SpendPerk(attribute),
        bevy::picking::Pickable::default(),
        Hovered::default(),
    ));
    spawn_text(
        commands,
        theme,
        button,
        "+",
        theme.fonts.body,
        theme.colors.text,
        HudText,
    );
}

fn track_fps(time: Res<Time>, mut counter: ResMut<FpsCounter>) {
    let dt = time.delta_secs();
    let ms = dt * 1000.0;
    counter.frames += 1;
    counter.elapsed += dt;
    counter.recent.push(ms);
    if counter.recent.len() > 240 {
        let drain = counter.recent.len() - 240;
        counter.recent.drain(0..drain);
    }

    if counter.elapsed >= 0.25 {
        counter.fps = counter.frames as f32 / counter.elapsed;
        counter.frame_ms = 1000.0 / counter.fps.max(0.001);
        counter.frames = 0;
        counter.elapsed = 0.0;
    }
}

fn log_perf_snapshot(
    settings: Res<GameSettings>,
    time: Res<Time>,
    counter: Res<FpsCounter>,
    manager: Res<ChunkManager>,
    timings: Res<WorldTimings>,
    mut timer: ResMut<PerfLogTimer>,
) {
    if !settings.perf_log {
        return;
    }
    timer.elapsed += time.delta_secs();
    if timer.elapsed < 1.0 {
        return;
    }
    timer.elapsed = 0.0;
    if counter.fps <= 0.0 {
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (max_ms, p95_ms, p99_ms) = counter.frame_stats();
    let line = format!(
        "t={now} fps={:.1} frame_ms={:.2} max_ms={:.2} p95_ms={:.2} p99_ms={:.2} view_distance={} loaded={} pending={} \
         triangles={} poll_ms={:.3} stream_ms={:.3} cull_ms={:.3}",
        counter.fps,
        counter.frame_ms,
        max_ms,
        p95_ms,
        p99_ms,
        manager.view_distance,
        manager.loaded_chunks(),
        manager.pending_chunks(),
        manager.triangle_count(),
        timings.poll_ms,
        timings.stream_ms,
        timings.cull_ms,
    );

    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(PERF_LOG_PATH)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn toggle_hud(
    keys: Res<ButtonInput<KeyCode>>,
    bindings: Res<InputBindings>,
    capture: Res<CursorCapture>,
    ui: Res<UiManager>,
    mut state: ResMut<HudState>,
) {
    if keys.just_pressed(bindings.toggle_hud) && !ui.any_open() && capture.grabbed {
        state.hud_hidden = !state.hud_hidden;
    }
    if !ui.any_open() && keys.just_pressed(bindings.debug_overlay) {
        state.debug_visible = !state.debug_visible;
    }
    // Any modal overlay takes over the screen, including the pause menu.
    if ui.any_open() {
        state.hint_visible = false;
    }
    // Do not toggle the help overlay while the player is typing in chat.
    if capture.grabbed && keys.just_pressed(bindings.help) && !ui.any_open() {
        state.hint_visible = !state.hint_visible;
    }
}

#[allow(clippy::too_many_arguments)]
fn update_debug_text(
    state: Res<HudState>,
    counter: Res<FpsCounter>,
    manager: Res<ChunkManager>,
    timings: Res<WorldTimings>,
    stamina: Res<Stamina>,
    progression: Res<Progression>,
    pockets: Res<Pockets>,
    backpack: Res<Backpack>,
    player: Query<(&Player, &Transform)>,
    crosshair: Res<CrosshairTarget>,
    mut query: Query<&mut Text, With<DebugText>>,
) {
    if !state.debug_visible {
        return;
    }

    let Ok(mut text) = query.single_mut() else {
        return;
    };
    let Ok((player, transform)) = player.single() else {
        return;
    };

    let target = crosshair
        .hit
        .map(|hit| {
            let voxel = manager.get_voxel_if_loaded(hit.block).unwrap_or_default();
            format!(
                "{} {} @ {:.1} m",
                voxel.block().display_name(),
                voxel.shape().display_name(),
                hit.distance * BLOCK_SIZE
            )
        })
        .unwrap_or_else(|| "none".to_string());

    let held = pockets
        .selected_stack()
        .map(|stack| {
            format!(
                "{} x{} ({} in pockets) [{}]",
                stack.item.display_name(),
                stack.count,
                pockets.count(stack.item),
                stack.item.def().category.display_name()
            )
        })
        .unwrap_or_else(|| "empty".to_string());

    let tool = selected_tool(&pockets)
        .map_or("none", |tool| tool.display_name())
        .to_string();
    let attachments: Vec<_> = backpack
        .attachments()
        .iter()
        .flatten()
        .map(|stack| stack.item.display_name())
        .collect();
    let attachments_text = if attachments.is_empty() {
        "none".to_string()
    } else {
        attachments.join(", ")
    };

    let position = transform.translation * BLOCK_SIZE;
    text.0 = format!(
        "FPS      {:.0}  ({:.2} ms)\n\
         Position {:.1}, {:.1}, {:.1} m\n\
         Level    {} ({} / {} XP)\n\
         Coins    {}\n\
         Perks    {} available (+{} each)\n\
         Bonus    {} +{}  {} +{}  {} +{}\n\
         Hunger   {:.0}/{:.0}\n\
         Hydration {:.0}/{:.0}\n\
         Chunks   {} loaded, {} pending\n\
         Triangles {}\n\
         View distance {}\n\
         World poll  {:.2} ms\n\
         World stream {:.2} ms\n\
         World cull  {:.2} ms\n\
         Target   {}\n\
         Tool     {}\n\
         Stamina  {:.0}/{:.0}\n\
         Pockets  {} items\n\
         Backpack {} items\n\
         Attachments {}\n\
         Held     {}\n\
         Crouching {}\n\
         Grounded {}\n\
         In water {}",
        counter.fps,
        counter.frame_ms,
        position.x,
        position.y,
        position.z,
        progression.level(),
        progression.xp(),
        progression.xp_required(),
        format_number(progression.coins()),
        progression.perk_points(),
        progression.perk_gain(),
        Attribute::Stamina.short_label(),
        progression.bonus(Attribute::Stamina),
        Attribute::Hunger.short_label(),
        progression.bonus(Attribute::Hunger),
        Attribute::Hydration.short_label(),
        progression.bonus(Attribute::Hydration),
        progression.hunger(),
        progression.max(Attribute::Hunger),
        progression.hydration(),
        progression.max(Attribute::Hydration),
        manager.loaded_chunks(),
        manager.pending_chunks(),
        manager.triangle_count(),
        manager.view_distance,
        timings.poll_ms,
        timings.stream_ms,
        timings.cull_ms,
        target,
        tool,
        stamina.current,
        stamina.max,
        pockets.total_items(),
        backpack.total_items(),
        attachments_text,
        held,
        yes_no(player.crouching),
        yes_no(player.grounded),
        yes_no(player.in_water),
    );
}

#[allow(clippy::type_complexity)]
fn update_level_panel(
    progression: Res<Progression>,
    biome: Res<BiomeInfo>,
    cycle: Res<crate::day_night::DayNightCycle>,
    icons: Res<UiIcons>,
    mut widgets: ParamSet<(
        Query<&mut Text, With<LevelText>>,
        Query<&mut Node, With<XpFill>>,
        Query<&mut Text, With<XpText>>,
        Query<&mut Text, With<WorldInfoBiomeText>>,
        Query<&mut Text, With<WorldInfoDayText>>,
        Query<&mut Text, With<WorldInfoClockText>>,
        Query<&mut ImageNode, With<WeatherIcon>>,
    )>,
) {
    for mut text in widgets.p0().iter_mut() {
        let desired = format!("Lv {}", progression.level());
        if text.0 != desired {
            text.0 = desired;
        }
    }

    let fraction = progression.xp_fraction() * 100.0;
    for mut node in widgets.p1().iter_mut() {
        let width = Val::Percent(fraction);
        if node.width != width {
            node.width = width;
        }
    }

    let xp_label = format!("{} / {} XP", progression.xp(), progression.xp_required());
    for mut text in widgets.p2().iter_mut() {
        if text.0 != xp_label {
            text.0 = xp_label.clone();
        }
    }

    let biome_label = biome.biome.label();
    for mut text in widgets.p3().iter_mut() {
        if text.0.as_str() != biome_label {
            text.0.clear();
            text.0.push_str(biome_label);
        }
    }

    let day_label = cycle.day_label();
    for mut text in widgets.p4().iter_mut() {
        if text.0 != day_label {
            text.0 = day_label.clone();
        }
    }
    let clock = cycle.clock_string();
    for mut text in widgets.p5().iter_mut() {
        if text.0 != clock {
            text.0 = clock.clone();
        }
    }

    let weather = if cycle.is_daytime() {
        icons.hud(crate::ui::icons::HudIcon::WeatherDay)
    } else {
        icons.hud(crate::ui::icons::HudIcon::WeatherNight)
    };
    for mut image in widgets.p6().iter_mut() {
        if image.image != weather {
            image.image = weather.clone();
        }
    }
}

#[allow(clippy::type_complexity)]
fn update_vital_indicators(
    progression: Res<Progression>,
    mut widgets: ParamSet<(
        Query<&mut Text, With<HungerValueText>>,
        Query<&mut Text, With<ThirstValueText>>,
        Query<&mut Node, With<HungerFill>>,
        Query<&mut Node, With<ThirstFill>>,
    )>,
) {
    let hunger = progression.hunger().round().max(0.0) as i32;
    let hunger_text = hunger.to_string();
    for mut text in widgets.p0().iter_mut() {
        if text.0 != hunger_text {
            text.0 = hunger_text.clone();
        }
    }

    let thirst = progression.hydration().round().max(0.0) as i32;
    let thirst_text = thirst.to_string();
    for mut text in widgets.p1().iter_mut() {
        if text.0 != thirst_text {
            text.0 = thirst_text.clone();
        }
    }

    let hunger_h = Val::Percent(progression.hunger_fraction() * 100.0);
    for mut node in widgets.p2().iter_mut() {
        if node.height != hunger_h {
            node.height = hunger_h;
        }
    }
    let thirst_h = Val::Percent(progression.hydration_fraction() * 100.0);
    for mut node in widgets.p3().iter_mut() {
        if node.height != thirst_h {
            node.height = thirst_h;
        }
    }
}

fn update_compass(
    players: Query<&Player>,
    mut bands: Query<(&CompassCardinal, &mut Node)>,
    mut glyphs: Query<(&CompassLabel, &mut TextColor)>,
    theme: Res<UiTheme>,
) {
    // Read the authoritative yaw instead of a possibly stale GlobalTransform,
    // and ignore pitch so looking up or down never shifts a cardinal.
    let heading = players
        .single()
        .map(|player| compass_heading(player.yaw))
        .unwrap_or(0.0);
    let center = cardinal_index(heading);
    let mut desired_colors = [theme.colors.text; 4];

    for (cardinal, mut node) in &mut bands {
        let bearing = cardinal.0 as f32 * 90.0;
        // Wrap the bearing into [-180, 180) so labels slide across the strip in
        // the direction the player turns.
        let delta = (bearing - heading + 540.0).rem_euclid(360.0) - 180.0;
        let left = compass_label_left(delta);
        if node.left != Val::Px(left) {
            node.left = Val::Px(left);
        }

        // Only the nearest cardinal band is shown; the opposite direction is
        // behind the player and would otherwise overlap an edge label.
        let desired_display = display_if(delta.abs() <= 90.0);
        if node.display != desired_display {
            node.display = desired_display;
        }

        let alpha = 1.0 - (delta.abs() / 90.0) * 0.45;
        let base = if cardinal.0 == center {
            theme.colors.accent
        } else {
            theme.colors.text
        };
        desired_colors[cardinal.0] = alpha_color(base, alpha);
    }

    for (label, mut color) in &mut glyphs {
        let desired = desired_colors[label.0];
        if color.0 != desired {
            color.0 = desired;
        }
    }
}

#[inline]
fn cardinal_index(heading: f32) -> usize {
    (((heading + 45.0) / 90.0).floor() as i32).rem_euclid(4) as usize
}

fn update_coins(progression: Res<Progression>, mut texts: Query<&mut Text, With<CoinsText>>) {
    if !progression.is_changed() {
        return;
    }
    let desired = format_number(progression.coins());
    for mut text in &mut texts {
        if text.0 != desired {
            text.0 = desired.clone();
        }
    }
}

fn survival_color(fraction: f32, base: Color, theme: &UiTheme) -> Color {
    if fraction <= 0.15 {
        theme.colors.danger
    } else if fraction < 0.35 {
        theme.colors.warn
    } else {
        base
    }
}

#[allow(clippy::type_complexity)]
fn update_stamina_bar(
    stamina: Res<Stamina>,
    mut widgets: ParamSet<(
        Query<(&mut Node, &mut BackgroundColor), With<StaminaFill>>,
        Query<(&mut Text, &mut TextColor), With<StaminaText>>,
    )>,
    theme: Res<UiTheme>,
) {
    let fraction = stamina.fraction().clamp(0.0, 1.0);
    let desired_width = Val::Percent(fraction * 100.0);
    let fill_color = survival_color(fraction, theme.colors.stamina, &theme);

    for (mut node, mut color) in widgets.p0().iter_mut() {
        if node.width != desired_width {
            node.width = desired_width;
        }
        if color.0 != fill_color {
            color.0 = fill_color;
        }
    }
    let desired = format!(
        "{} / {}",
        stamina.current.round().max(0.0) as i32,
        stamina.max.round() as i32
    );
    for (mut text, mut color) in widgets.p1().iter_mut() {
        if text.0 != desired {
            text.0 = desired.clone();
        }
        let desired_color = survival_color(fraction, theme.colors.text, &theme);
        if color.0 != desired_color {
            color.0 = desired_color;
        }
    }
}

#[allow(clippy::type_complexity)]
fn update_insight_panel(
    state: Res<HudState>,
    capture: Res<CursorCapture>,
    ui: Res<UiManager>,
    insight: Res<InsightInfo>,
    mut widgets: ParamSet<(
        Query<&mut Node, With<InsightRoot>>,
        Query<&mut Text, With<InsightTitle>>,
        Query<&mut Text, With<InsightDescription>>,
    )>,
) {
    let visible = insight.visible && capture.grabbed && !state.hint_visible && !ui.any_open();
    for mut node in widgets.p0().iter_mut() {
        let desired = display_if(visible);
        if node.display != desired {
            node.display = desired;
        }
    }

    for mut text in widgets.p1().iter_mut() {
        if text.0 != insight.title {
            text.0.clear();
            text.0.push_str(&insight.title);
        }
    }
    for mut text in widgets.p2().iter_mut() {
        if text.0 != insight.action {
            text.0.clear();
            text.0.push_str(&insight.action);
        }
    }
}

#[allow(clippy::type_complexity)]
fn update_notifications(
    state: Res<HudState>,
    feed: Res<NotificationFeed>,
    icons: Res<UiIcons>,
    mut widgets: ParamSet<(
        Query<(
            &NotificationRow,
            &mut Node,
            &mut BackgroundColor,
            &mut BorderColor,
        )>,
        Query<(&NotificationIcon, &mut ImageNode)>,
        Query<(&NotificationTitle, &mut Text)>,
        Query<(&NotificationDetail, &mut Text, &mut Node)>,
    )>,
    theme: Res<UiTheme>,
) {
    let hidden = state.hint_visible;

    for (marker, mut node, mut background, mut border) in widgets.p0().iter_mut() {
        let Some(notification) = feed.notifications().get(marker.0) else {
            if node.display != Display::None {
                node.display = Display::None;
            }
            continue;
        };
        let desired = if hidden { Display::None } else { Display::Flex };
        if node.display != desired {
            node.display = desired;
        }

        let alpha = notification.alpha();
        let base = tone_color(notification.tone, &theme);
        let desired_right = Val::Px(14.0 + notification.offset_x());
        if node.right != desired_right {
            node.right = desired_right;
        }
        let desired_bg = alpha_color(theme.colors.panel_bg_strong, alpha);
        let desired_border = alpha_color(base, (alpha * 0.65).clamp(0.0, 1.0));
        if background.0 != desired_bg {
            background.0 = desired_bg;
        }
        if border.top != desired_border {
            *border = BorderColor::all(desired_border);
        }
    }

    for (marker, mut image) in widgets.p1().iter_mut() {
        let Some(notification) = feed.notifications().get(marker.0) else {
            continue;
        };
        let alpha = notification.alpha();
        let desired_image = match notification.item {
            Some(item) => icons.item(item),
            None => icons.tone(notification.tone),
        };
        if image.image != desired_image {
            image.image = desired_image;
        }
        let desired_tint = Color::srgba(1.0, 1.0, 1.0, alpha);
        if image.color != desired_tint {
            image.color = desired_tint;
        }
    }

    for (marker, mut text) in widgets.p2().iter_mut() {
        let desired = feed
            .notifications()
            .get(marker.0)
            .map_or("", |notification| notification.title.as_str());
        if text.0.as_str() != desired {
            text.0.clear();
            text.0.push_str(desired);
        }
    }

    for (marker, mut text, mut node) in widgets.p3().iter_mut() {
        let detail = feed
            .notifications()
            .get(marker.0)
            .map_or("", |notification| notification.detail.as_str());
        if text.0.as_str() != detail {
            text.0.clear();
            text.0.push_str(detail);
        }
        let desired = if detail.is_empty() {
            Display::None
        } else {
            Display::Flex
        };
        if node.display != desired {
            node.display = desired;
        }
    }
}

fn tone_color(tone: NotificationTone, theme: &UiTheme) -> Color {
    // Keep this as the single authoritative mapping from tone to HUD/theme color.
    match tone {
        NotificationTone::Normal => theme.colors.accent,
        NotificationTone::Rare => theme.colors.coin,
        NotificationTone::Info => theme.colors.xp,
        NotificationTone::Success => theme.colors.success,
        NotificationTone::Error => theme.colors.danger,
    }
}

fn alpha_color(color: Color, alpha: f32) -> Color {
    let srgba = color.to_srgba();
    Color::srgba(
        srgba.red,
        srgba.green,
        srgba.blue,
        (srgba.alpha * alpha).clamp(0.0, 1.0),
    )
}

fn heading_degrees(forward: Vec3) -> f32 {
    // North is -Z (the default camera facing), east is +X.
    forward.x.atan2(-forward.z).to_degrees().rem_euclid(360.0)
}

/// Convert the player's yaw into the compass heading used by the strip.
#[inline]
fn compass_heading(yaw: f32) -> f32 {
    heading_degrees(Quat::from_rotation_y(yaw) * Vec3::NEG_Z)
}

fn slot_stack(source: SlotSource, pockets: &Pockets, backpack: &Backpack) -> Option<ItemStack> {
    match source {
        SlotSource::Pocket(index) => {
            let pocket = pockets.pocket(index)?;
            if pocket.is_empty() {
                None
            } else {
                pocket.entries().first().copied().flatten()
            }
        }
        SlotSource::Storage(index) => backpack.slots().get(index).copied().flatten(),
        SlotSource::Category(category, index) => backpack
            .category_slots
            .iter()
            .find(|storage| storage.category == category)
            .and_then(|storage| storage.entries.get(index).copied().flatten()),
        SlotSource::Attachment(index) => backpack.attachments().get(index).copied().flatten(),
    }
}

fn slot_accepts(source: SlotSource, pockets: &Pockets, backpack: &Backpack) -> bool {
    match source {
        SlotSource::Pocket(index) => index < pockets.pockets().len(),
        SlotSource::Storage(index) => index < backpack.capacity(),
        SlotSource::Category(category, index) => {
            index < CATEGORY_SLOTS_PER_UNLOCK && backpack.has_category(category)
        }
        SlotSource::Attachment(index) => index < ATTACHMENT_SLOTS,
    }
}

fn take_slot(
    source: SlotSource,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
) -> Option<ItemStack> {
    match source {
        SlotSource::Pocket(index) => pockets.take(index),
        SlotSource::Storage(index) => backpack.take_slot(index),
        SlotSource::Category(category, index) => backpack.take_category_slot(category, index),
        SlotSource::Attachment(index) => backpack.take_attachment(index),
    }
}

fn put_slot(
    source: SlotSource,
    stack: Option<ItemStack>,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
) -> Option<ItemStack> {
    match source {
        SlotSource::Pocket(index) => pockets.replace(index, stack),
        SlotSource::Storage(index) => backpack.replace_slot(index, stack),
        SlotSource::Category(category, index) => {
            backpack.replace_category_slot(category, index, stack)
        }
        SlotSource::Attachment(index) => {
            if let Some(stack) = stack.filter(|stack| stack.count > 0) {
                // Backpack attachments are physical utility items (e.g. Lantern, Magnet).
                if !matches!(stack.item, ItemId::Lantern | ItemId::Magnet) || stack.count != 1 {
                    return Some(stack);
                }
                backpack.replace_attachment(index, Some(stack))
            } else {
                backpack.replace_attachment(index, None)
            }
        }
    }
}

fn on_slot_drag_start(
    drag: On<PointerDragStart>,
    keys: Res<ButtonInput<KeyCode>>,
    slots: Query<&SlotSource>,
    ui: Res<UiManager>,
    mut dragged: ResMut<DraggedSlot>,
) {
    if drag.button != PointerButton::Primary || !ui.is_open(Overlay::Backpack) {
        return;
    }
    // Shift+click is quick-move; don't start a drag for it.
    if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
        dragged.0 = None;
        return;
    }
    if let Ok(source) = slots.get(drag.entity) {
        dragged.0 = Some(*source);
    }
}

#[allow(clippy::too_many_arguments)]
fn on_slot_drag_drop(
    drop: On<PointerDragDrop>,
    slots: Query<&SlotSource>,
    ui: Res<UiManager>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    mut dragged: ResMut<DraggedSlot>,
) {
    if drop.button != PointerButton::Primary || !ui.is_open(Overlay::Backpack) {
        return;
    }
    let Some(source) = dragged.0 else {
        return;
    };
    let Ok(target) = slots.get(drop.entity) else {
        return;
    };
    let target = *target;
    if target == source
        || !slot_accepts(source, &pockets, &backpack)
        || !slot_accepts(target, &pockets, &backpack)
    {
        return;
    }

    let Some(carried) = take_slot(source, &mut pockets, &mut backpack) else {
        return;
    };
    if let Some(displaced) = put_slot(target, Some(carried), &mut pockets, &mut backpack) {
        let _ = put_slot(source, Some(displaced), &mut pockets, &mut backpack);
    }
    dragged.0 = None;
}

fn quick_move_slot(source: SlotSource, pockets: &mut Pockets, backpack: &mut Backpack) -> bool {
    let Some(stack) = slot_stack(source, pockets, backpack) else {
        return false;
    };
    let count = stack.count;
    let item = stack.item;

    // Take from source first to avoid duplicate counts when merging into same pool.
    let Some(taken) = take_slot(source, pockets, backpack) else {
        return false;
    };

    let leftover = match source {
        SlotSource::Pocket(_) => backpack.add(taken.item, taken.count),
        SlotSource::Storage(_) | SlotSource::Category(_, _) => {
            let left = pockets.add(taken.item, taken.count);
            if left > 0 {
                backpack.add(taken.item, left)
            } else {
                0
            }
        }
        SlotSource::Attachment(_) => {
            // Attachments are physical single items; move to backpack storage/categories.
            backpack.add(taken.item, taken.count)
        }
    };

    if leftover == 0 {
        return true;
    }
    if leftover == count && item == taken.item {
        // Nothing moved; restore original stack.
        let _ = put_slot(source, Some(taken), pockets, backpack);
        return false;
    }

    // Partial move: return leftovers to source, or force-add to backpack as fallback.
    let remainder = ItemStack {
        item: taken.item,
        count: leftover,
    };
    if put_slot(source, Some(remainder), pockets, backpack).is_some() {
        let _ = backpack.add(taken.item, leftover);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn on_slot_click_quick_move(
    click: On<PointerClick>,
    keys: Res<ButtonInput<KeyCode>>,
    ui: Res<UiManager>,
    slots: Query<&SlotSource>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    mut dragged: ResMut<DraggedSlot>,
) {
    if click.button != PointerButton::Primary || !ui.is_open(Overlay::Backpack) {
        return;
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !shift {
        return;
    }
    // Any real click means no drag ghost should remain.
    dragged.0 = None;
    let Ok(source) = slots.get(click.entity) else {
        return;
    };
    if quick_move_slot(*source, &mut pockets, &mut backpack) {
        dragged.0 = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn on_slot_drag_end(
    drag: On<PointerDragEnd>,
    mut commands: Commands,
    ui: Res<UiManager>,
    mut dragged: ResMut<DraggedSlot>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    slots: Query<(&SlotSource, Option<&Hovered>)>,
    camera: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    visuals: Option<Res<crate::interaction::InteractionVisuals>>,
    asset_server: Res<AssetServer>,
) {
    if drag.button != PointerButton::Primary {
        return;
    }
    let Some(source) = dragged.0.take() else {
        return;
    };
    if !ui.is_open(Overlay::Backpack) {
        return;
    }
    // If the pointer ended over any inventory slot, the drop was handled (or
    // intentionally cancelled) by slot logic; don't throw the item.
    let over_slot = slots
        .iter()
        .any(|(_, hovered)| hovered.is_some_and(|hovered| hovered.0));
    if over_slot {
        return;
    }

    let Some(stack) = take_slot(source, &mut pockets, &mut backpack) else {
        return;
    };
    let Some(visuals) = visuals.as_deref() else {
        let _ = put_slot(source, Some(stack), &mut pockets, &mut backpack);
        return;
    };
    let Ok(camera) = camera.single() else {
        let _ = put_slot(source, Some(stack), &mut pockets, &mut backpack);
        return;
    };

    let forward = camera.rotation() * Vec3::NEG_Z;
    let position = camera.translation() + forward * 1.5 + Vec3::Y * -0.2;
    crate::interaction::spawn_player_drop(
        &mut commands,
        visuals,
        &asset_server,
        stack.item,
        stack.count,
        position,
    );
}

fn update_pockets(
    pockets: Res<Pockets>,
    dragged: Res<DraggedSlot>,
    mut slots: Query<(
        &SlotSource,
        &PocketSlot,
        Option<&Hovered>,
        &mut BorderColor,
        &mut BackgroundColor,
    )>,
    theme: Res<UiTheme>,
) {
    for (source, marker, hovered, mut border, mut background) in &mut slots {
        let selected = marker.0 == pockets.selected();
        let dragging = dragged.0 == Some(*source);
        let hovering = hovered.is_some_and(|hovered| hovered.0);

        let desired_border = if dragging {
            theme.colors.border_active
        } else if selected {
            theme.colors.accent
        } else if hovering {
            theme.colors.border_hover
        } else {
            theme.colors.faint_border
        };
        if border.top != desired_border {
            *border = BorderColor::all(desired_border);
        }

        let desired_bg = if dragging || hovering || selected {
            theme.colors.selected_bg
        } else {
            theme.colors.panel_bg
        };
        if background.0 != desired_bg {
            background.0 = desired_bg;
        }
    }
}

type InventorySlotStyleQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static SlotSource,
        Option<&'static Hovered>,
        &'static mut BorderColor,
        &'static mut BackgroundColor,
    ),
    (Without<PocketSlot>, With<BackpackCell>),
>;

fn update_inventory_slot_styles(
    dragged: Res<DraggedSlot>,
    mut slots: InventorySlotStyleQuery,
    theme: Res<UiTheme>,
) {
    for (source, hovered, mut border, mut background) in &mut slots {
        // Skip pocket slots? They're handled by update_pockets, but we excluded PocketSlot.
        let dragging = dragged.0 == Some(*source);
        let hovering = hovered.is_some_and(|h| h.0);
        let desired_border = if dragging {
            theme.colors.border_active
        } else if hovering {
            theme.colors.border_hover
        } else {
            theme.colors.faint_border
        };
        if border.top != desired_border {
            *border = BorderColor::all(desired_border);
        }
        let desired_bg = if dragging || hovering {
            theme.colors.selected_bg
        } else {
            theme.colors.panel_bg
        };
        if background.0 != desired_bg {
            background.0 = desired_bg;
        }
    }
}

/// Reveal slot contents only for occupied slots; empty slots stay visually blank.
fn update_slot_visuals(
    pockets: Res<Pockets>,
    backpack: Res<Backpack>,
    mut visuals: Query<(
        &SlotVisual,
        &mut Node,
        Option<&mut Text>,
        Option<&mut ImageNode>,
    )>,
    icons: Res<UiIcons>,
) {
    // Slot contents only depend on inventory state and icon assets; skip rescans
    // when neither changed. Bevy marks resources changed on first availability.
    if !pockets.is_changed() && !backpack.is_changed() && !icons.is_changed() {
        return;
    }
    for (visual, mut node, text, image) in &mut visuals {
        let stack = slot_stack(visual.source, &pockets, &backpack);
        let visible = match visual.kind {
            SlotVisualKind::Count => stack.is_some_and(|stack| stack.count > 1),
            SlotVisualKind::Icon => stack.is_some(),
        };
        let desired = display_if(visible);
        if node.display != desired {
            node.display = desired;
        }

        let Some(stack) = stack else {
            continue;
        };
        match visual.kind {
            SlotVisualKind::Icon => {
                if let Some(mut image) = image {
                    let desired = icons.item(stack.item);
                    if image.image != desired {
                        image.image = desired;
                    }
                }
            }
            SlotVisualKind::Count => {
                if let Some(mut text) = text {
                    let desired = stack.count.to_string();
                    if text.0 != desired {
                        text.0 = desired;
                    }
                }
            }
        }
    }
}

fn update_pickup_prompt(
    state: Res<HudState>,
    prompt: Res<PickupPrompt>,
    mut query: Query<(&mut Text, &mut Node), With<PickupPromptText>>,
) {
    let Ok((mut text, mut node)) = query.single_mut() else {
        return;
    };
    if text.0 != prompt.text {
        text.0 = prompt.text.clone();
    }
    let desired = display_if(prompt.visible && !state.hint_visible);
    if node.display != desired {
        node.display = desired;
    }
}

fn sync_visibility(
    state: Res<HudState>,
    capture: Res<CursorCapture>,
    ui: Res<UiManager>,
    mut panels: PanelQuery,
) {
    let hud_hidden = state.hud_hidden || ui.is_open(Overlay::Map);
    for (mut node, debug, hint, crosshair, hud_root) in &mut panels {
        let desired = if hud_root.is_some() {
            display_if(!hud_hidden)
        } else if debug.is_some() {
            display_if(state.debug_visible && !hud_hidden)
        } else if hint.is_some() {
            display_if(state.hint_visible && !hud_hidden)
        } else if crosshair.is_some() {
            display_if(capture.grabbed && !state.hint_visible && !hud_hidden)
        } else {
            continue;
        };

        if node.display != desired {
            node.display = desired;
        }
    }
}

#[inline]
fn panels_row_gap(_open: bool) -> Val {
    // 8px gap from the center backpack/hotbar column.
    Val::Px(8.0)
}

#[allow(clippy::type_complexity)]
fn sync_backpack_visibility(
    ui: Res<UiManager>,
    mut panels: Query<
        (
            &mut Node,
            Option<&BackpackRoot>,
            Option<&HotbarPanel>,
            Option<&BackpackStoragePanel>,
            Option<&CharacterPanel>,
            Option<&CraftingPanel>,
            Option<&WorkbenchCraftingPanel>,
            Option<&StaminaBar>,
            Option<&StaminaFill>,
            Option<&PanelsRow>,
        ),
        bevy::ecs::query::Or<(
            With<BackpackRoot>,
            With<HotbarPanel>,
            With<BackpackStoragePanel>,
            With<CharacterPanel>,
            With<CraftingPanel>,
            With<WorkbenchCraftingPanel>,
            With<StaminaBar>,
            With<StaminaFill>,
            With<PanelsRow>,
        )>,
    >,
    theme: Res<UiTheme>,
    crafting: Res<crate::crafting::CraftingUiState>,
) {
    let open = ui.is_open(Overlay::Backpack);
    let desired_display = display_if(open);
    let desired_hotbar = hotbar_radius(open, &theme);
    let desired_storage = storage_radius(open, &theme);
    let desired_stamina = stamina_bar_radius(open, &theme);
    let desired_row_gap = panels_row_gap(open);
    let workbench_open = open && crafting.station == Some(crate::crafting::StationKind::Workbench);

    for (
        mut node,
        root,
        hotbar,
        storage,
        character,
        crafting_panel,
        workbench_panel,
        stamina_bar,
        stamina_fill,
        panels_row,
    ) in &mut panels
    {
        let desired = if panels_row.is_some()
            || hotbar.is_some()
            || stamina_bar.is_some()
            || stamina_fill.is_some()
        {
            Display::Flex
        } else if workbench_panel.is_some() {
            display_if(workbench_open)
        } else if root.is_some()
            || storage.is_some()
            || character.is_some()
            || crafting_panel.is_some()
        {
            desired_display
        } else {
            continue;
        };
        if node.display != desired {
            node.display = desired;
        }
        if panels_row.is_some() && node.column_gap != desired_row_gap {
            node.column_gap = desired_row_gap;
        }
        if hotbar.is_some() && node.border_radius != desired_hotbar {
            node.border_radius = desired_hotbar;
        }
        if storage.is_some() && node.border_radius != desired_storage {
            node.border_radius = desired_storage;
        }
        if (stamina_bar.is_some() || stamina_fill.is_some())
            && node.border_radius != desired_stamina
        {
            node.border_radius = desired_stamina;
        }
    }
}

fn update_backpack_texts(
    backpack: Res<Backpack>,
    progression: Res<Progression>,
    theme: Res<UiTheme>,
    mut texts: Query<(&BackpackText, &mut Text, &mut TextColor)>,
) {
    if !backpack.is_changed() && !progression.is_changed() && !theme.is_changed() {
        return;
    }

    for (marker, mut text, mut color) in &mut texts {
        let desired = match *marker {
            BackpackText::Capacity => "Backpack".to_string(),
            BackpackText::Level => format!("Level {}", progression.level()),
            BackpackText::Coins => format_number(progression.coins()),
            BackpackText::PerkPointsTitle => {
                let pts = progression.perk_points();
                let desired_color = if pts > 0 {
                    theme.colors.accent
                } else {
                    theme.colors.muted_text
                };
                if color.0 != desired_color {
                    color.0 = desired_color;
                }
                if pts == 1 {
                    "1 PERK POINT".to_string()
                } else {
                    format!("{pts} PERK POINTS")
                }
            }
            BackpackText::Max(attribute) => format!("{:.0}", progression.max(attribute)),
            BackpackText::CategoryHeader(category) => {
                match backpack
                    .category_slots
                    .iter()
                    .find(|storage| storage.category == category)
                {
                    Some(storage) => {
                        let used = storage
                            .entries
                            .iter()
                            .filter(|entry| entry.is_some())
                            .count();
                        format!(
                            "{} {used}/{}",
                            category.display_name(),
                            storage.entries.len()
                        )
                    }
                    None => format!("{} locked", category.display_name()),
                }
            }
        };
        if text.0 != desired {
            text.0 = desired;
        }
    }
}

fn update_backpack_xp_bar(
    progression: Res<Progression>,
    mut fills: Query<&mut Node, With<BackpackXpFill>>,
    mut labels: Query<&mut Text, With<BackpackXpText>>,
) {
    if !progression.is_changed() {
        return;
    }
    let fraction = progression.xp_fraction();
    let width = Val::Percent(fraction * 100.0);
    let label = format!("{} XP", progression.xp());

    for mut fill in &mut fills {
        if fill.width != width {
            fill.width = width;
        }
    }
    for mut text in &mut labels {
        if text.0 != label {
            text.0 = label.clone();
        }
    }
}

#[allow(clippy::type_complexity)]
fn update_backpack_cells(
    backpack: Res<Backpack>,
    dragged: Res<DraggedSlot>,
    mut cells: Query<(
        &BackpackCell,
        &mut Node,
        Option<&SlotSource>,
        Option<&Hovered>,
        &mut BorderColor,
        &mut BackgroundColor,
    )>,
    theme: Res<UiTheme>,
) {
    for (cell, mut node, source, hovered, mut border, mut background) in &mut cells {
        let visible = match *cell {
            BackpackCell::Slot(index) => index < backpack.capacity(),
            BackpackCell::CategorySection(category) => backpack.has_category(category),
            BackpackCell::CategorySlot => true,
        };
        let desired = display_if(visible);
        if node.display != desired {
            node.display = desired;
        }

        let Some(source) = source else {
            continue;
        };
        let dragging = dragged.0 == Some(*source);
        let hovering = hovered.is_some_and(|hovered| hovered.0);
        let desired_border = if dragging {
            theme.colors.border_active
        } else if hovering {
            theme.colors.border_hover
        } else {
            theme.colors.faint_border
        };
        if border.top != desired_border {
            *border = BorderColor::all(desired_border);
        }
        let desired_bg = if dragging || hovering {
            theme.colors.selected_bg
        } else {
            theme.colors.panel_bg
        };
        if background.0 != desired_bg {
            background.0 = desired_bg;
        }
    }
}

type BackpackButtonStyleQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static Hovered,
        Has<Pressed>,
        &'static mut BackgroundColor,
        &'static mut BorderColor,
        Option<&'static BackpackAction>,
    ),
    Or<(With<BackpackButton>, With<BackpackSortButton>)>,
>;

fn style_backpack_buttons(
    progression: Res<Progression>,
    mut buttons: BackpackButtonStyleQuery,
    theme: Res<UiTheme>,
) {
    let can_spend = progression.perk_points() > 0;
    for (hovered, pressed, mut background, mut border, action) in &mut buttons {
        let perk_button = matches!(action, Some(BackpackAction::SpendPerk(_)));
        let disabled = perk_button && !can_spend;
        let desired_background = if disabled {
            theme::with_alpha(theme.colors.button_bg, 0.45)
        } else if pressed {
            theme.colors.button_pressed
        } else if hovered.0 {
            theme.colors.button_hover
        } else {
            theme.colors.button_bg
        };
        if background.0 != desired_background {
            background.0 = desired_background;
        }

        let desired_border = if disabled {
            theme.colors.faint_border
        } else if pressed {
            theme.colors.border_active
        } else if hovered.0 {
            theme.colors.border_hover
        } else {
            theme.colors.border_idle
        };
        if border.top != desired_border {
            *border = BorderColor::all(desired_border);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn on_backpack_activate(
    activate: On<Activate>,
    actions: Query<&BackpackAction>,
    sort_buttons: Query<(), With<BackpackSortButton>>,
    wb_tabs: Query<&WorkbenchCategoryTab>,
    wb_select: Query<&WorkbenchRecipeSelect>,
    wb_craft: Query<&WorkbenchCraftButton>,
    craft_select: Query<&CraftingSelectButton>,
    craft_buttons: Query<&CraftingCraftButton>,
    mut progression: ResMut<Progression>,
    mut backpack: ResMut<Backpack>,
    mut pockets: ResMut<Pockets>,
    mut crafting: ResMut<crate::crafting::CraftingUiState>,
    mut feed: ResMut<NotificationFeed>,
) {
    if sort_buttons.get(activate.entity).is_ok() {
        backpack.sort();
        feed.push_status("Backpack sorted", NotificationTone::Info);
        return;
    }

    if let Ok(tab) = wb_tabs.get(activate.entity) {
        crafting.workbench_category = Some(tab.0);
        crafting.selected = 0;
        return;
    }
    if let Ok(sel) = wb_select.get(activate.entity) {
        crafting.selected = sel.0;
        return;
    }
    if let Ok(btn) = wb_craft.get(activate.entity) {
        let qty = btn.0.max(1);
        if let Some(cat) = crafting.workbench_category {
            let recipes = crate::crafting::workbench_recipes_for_category(cat);
            if let Some(recipe) = recipes.get(crafting.selected).copied() {
                let context = crate::crafting::CraftingContext::Station(
                    crate::crafting::StationKind::Workbench,
                );
                let mut crafted = 0u32;
                for _ in 0..qty {
                    match crate::crafting::craft_recipe(
                        recipe,
                        context,
                        &mut pockets,
                        &mut backpack,
                    ) {
                        Ok(_) => crafted += 1,
                        Err(v) => {
                            if crafted == 0 {
                                feed.push_status(v.summary(), NotificationTone::Error);
                            }
                            break;
                        }
                    }
                }
                if crafted > 0 {
                    feed.push_status(
                        format!("Crafted {} x{}", recipe.name, crafted),
                        NotificationTone::Success,
                    );
                }
            }
        }
        return;
    }

    if let Ok(sel) = craft_select.get(activate.entity) {
        crafting.selected = sel.0;
        return;
    }
    if let Ok(btn) = craft_buttons.get(activate.entity) {
        let context = crate::crafting::CraftingContext::Hands;
        let recipes: Vec<&crate::crafting::Recipe> =
            crate::crafting::recipes_for_ui(context).collect();
        let selected = crafting.selected.min(recipes.len().saturating_sub(1));
        if let Some(recipe) = recipes.get(selected).copied() {
            let qty = btn.0.max(1);
            let mut crafted = 0u32;
            for _ in 0..qty {
                match crate::crafting::craft_recipe(recipe, context, &mut pockets, &mut backpack) {
                    Ok(_) => crafted += 1,
                    Err(v) => {
                        if crafted == 0 {
                            feed.push_status(v.summary(), NotificationTone::Error);
                        }
                        break;
                    }
                }
            }
            if crafted > 0 {
                feed.push_status(
                    format!("Crafted {} x{}", recipe.name, crafted),
                    NotificationTone::Success,
                );
            }
        }
        return;
    }

    let Ok(action) = actions.get(activate.entity) else {
        return;
    };

    let BackpackAction::SpendPerk(attribute) = *action;
    let gain = progression.perk_gain();
    if progression.spend_perk(attribute) {
        feed.push_status(
            format!("{} +{}", attribute.label(), gain),
            NotificationTone::Success,
        );
    } else {
        feed.push_status("No perk points", NotificationTone::Error);
    }
}

#[inline]
fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_crafting_rows(
    commands: &mut Commands,
    theme: &UiTheme,
    list_entity: Entity,
    children: Option<&Children>,
    recipes: &[&crate::crafting::Recipe],
    context: crate::crafting::CraftingContext,
    selected_index: usize,
    pockets: &Pockets,
    backpack: &Backpack,
) {
    if let Some(children) = children {
        for child in children.iter() {
            commands.entity(child).despawn();
        }
    }

    for (index, recipe) in recipes.iter().enumerate() {
        let validation = validate_recipe(recipe, context, pockets, backpack);
        let craftable = validation.is_craftable();
        let selected = index == selected_index.min(recipes.len().saturating_sub(1));
        let (bg, border) = if selected {
            (theme.colors.selected_bg, theme.colors.border_active)
        } else {
            (theme.colors.panel_bg, theme.colors.faint_border)
        };

        let row = spawn_panel(
            commands,
            theme,
            list_entity,
            Node {
                width: Val::Percent(100.0),
                flex_shrink: 0.0,
                padding: UiRect::axes(Val::Px(6.0), Val::Px(4.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            bg,
            border,
        );
        commands.entity(row).insert((
            Button,
            CraftingSelectButton(index),
            bevy::picking::Pickable::default(),
            Hovered::default(),
        ));

        let info = commands
            .spawn((
                Node {
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(1.0),
                    min_width: Val::Px(0.0),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(row),
            ))
            .id();

        let name = spawn_text(
            commands,
            theme,
            info,
            recipe.name,
            theme.fonts.small,
            if craftable {
                theme.colors.text
            } else {
                theme.colors.muted_text
            },
            HudText,
        );
        commands
            .entity(name)
            .insert(TextLayout::default().with_linebreak(LineBreak::WordBoundary));

        let status = if craftable {
            "Ready".to_string()
        } else if !context.satisfies(recipe.context) {
            format!("Requires {}", recipe.context.display_name())
        } else {
            validation.summary()
        };
        let status_text = spawn_text(
            commands,
            theme,
            info,
            status,
            theme.fonts.micro,
            if craftable {
                theme.colors.success
            } else {
                theme.colors.warn
            },
            HudText,
        );
        commands
            .entity(status_text)
            .insert(TextLayout::default().with_linebreak(LineBreak::WordBoundary));

        let (btn_bg, btn_border, btn_text) = if craftable {
            (
                theme.colors.button_bg,
                theme.colors.border_idle,
                theme.colors.text,
            )
        } else {
            (
                theme.colors.panel_bg,
                theme.colors.faint_border,
                theme.colors.muted_text,
            )
        };
        let craft_btn = spawn_panel(
            commands,
            theme,
            row,
            Node {
                flex_shrink: 0.0,
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            btn_bg,
            btn_border,
        );
        commands.entity(craft_btn).insert((
            Button,
            CraftingCraftButton(index),
            bevy::picking::Pickable::default(),
            Hovered::default(),
        ));
        spawn_text(
            commands,
            theme,
            craft_btn,
            "CRAFT",
            theme.fonts.micro,
            btn_text,
            HudText,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_backpack_crafting_list(
    mut commands: Commands,
    ui: Res<UiManager>,
    state: Res<CraftingUiState>,
    theme: Res<UiTheme>,
    titles: Query<&mut Text, With<CraftingPanelTitle>>,
    list: Query<(Entity, Option<&Children>), With<CraftingList>>,
    pockets: Res<Pockets>,
    backpack: Res<Backpack>,
) {
    if !ui.is_open(Overlay::Backpack) {
        return;
    }
    if !ui.is_changed() && !state.is_changed() && !pockets.is_changed() && !backpack.is_changed() {
        return;
    }
    for mut title in titles {
        title.0 = "HAND CRAFTING".to_string();
    }
    let context = crate::crafting::CraftingContext::Hands;
    let recipes: Vec<&crate::crafting::Recipe> = recipes_for_ui(context).collect();
    let Ok((list_entity, children)) = list.single() else {
        return;
    };
    rebuild_crafting_rows(
        &mut commands,
        &theme,
        list_entity,
        children,
        &recipes,
        context,
        state.selected,
        &pockets,
        &backpack,
    );
}

#[allow(clippy::too_many_arguments)]
fn refresh_workbench_ui(
    mut commands: Commands,
    ui: Res<UiManager>,
    mut state: ResMut<crate::crafting::CraftingUiState>,
    theme: Res<UiTheme>,
    icons: Res<UiIcons>,
    pockets: Res<Pockets>,
    backpack: Res<Backpack>,
    mut tab_q: WorkbenchTabQuery<'_, '_>,
    mut panel_q: ParamSet<(
        WorkbenchBodyNodeQuery<'_, '_>,
        WorkbenchEmptyNodeQuery<'_, '_>,
    )>,
    list_q: Query<(Entity, Option<&Children>), With<WorkbenchRecipeList>>,
    mut detail_q: ParamSet<(
        WorkbenchDetailNameQuery<'_, '_>,
        WorkbenchDetailDescQuery<'_, '_>,
        WorkbenchDetailIngredientsQuery<'_, '_>,
    )>,
    mut detail_preview: Query<&mut ImageNode, With<WorkbenchDetailPreview>>,
    mut craft_buttons: Query<
        (
            &WorkbenchCraftButton,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        Without<WorkbenchCategoryTab>,
    >,
) {
    let open = ui.is_open(Overlay::Backpack)
        && state.station == Some(crate::crafting::StationKind::Workbench);
    if !open {
        return;
    }
    if !ui.is_changed()
        && !state.is_changed()
        && !pockets.is_changed()
        && !backpack.is_changed()
        && !icons.is_changed()
    {
        return;
    }

    let available = crate::crafting::available_workbench_categories();
    if state
        .workbench_category
        .is_none_or(|c| !available.contains(&c))
    {
        state.workbench_category = available.first().copied();
    }
    let selected_cat = state.workbench_category;

    for (tab, mut node, mut bg, mut border) in &mut tab_q {
        let visible = available.contains(&tab.0);
        node.display = if visible {
            Display::Flex
        } else {
            Display::None
        };
        let selected = selected_cat == Some(tab.0);
        *bg = BackgroundColor(if selected {
            theme.colors.selected_bg
        } else {
            theme.colors.button_bg
        });
        *border = BorderColor::all(if selected {
            theme.colors.border_active
        } else {
            theme.colors.border_idle
        });
    }

    let has_any = !available.is_empty();
    if let Ok(mut n) = panel_q.p0().single_mut() {
        n.display = if has_any {
            Display::Flex
        } else {
            Display::None
        };
    }
    if let Ok(mut n) = panel_q.p1().single_mut() {
        n.display = if has_any {
            Display::None
        } else {
            Display::Flex
        };
    }

    if !has_any {
        if let Ok(mut t) = detail_q.p0().single_mut() {
            t.0.clear();
        }
        if let Ok(mut t) = detail_q.p1().single_mut() {
            t.0 = "No workbench recipes are available yet.".to_string();
        }
        if let Ok(mut t) = detail_q.p2().single_mut() {
            t.0.clear();
        }
        if let Ok(mut img) = detail_preview.single_mut() {
            img.color = Color::NONE;
        }
        for (_, mut bg, mut border) in &mut craft_buttons {
            *bg = BackgroundColor(theme.colors.panel_bg);
            *border = BorderColor::all(theme.colors.faint_border);
        }
        return;
    }

    let Some(cat) = selected_cat else {
        return;
    };
    let recipes = crate::crafting::workbench_recipes_for_category(cat);
    if recipes.is_empty() {
        return;
    }
    if state.selected >= recipes.len() {
        state.selected = 0;
    }
    let context =
        crate::crafting::CraftingContext::Station(crate::crafting::StationKind::Workbench);

    if let Ok((list_entity, children)) = list_q.single() {
        if let Some(children) = children {
            for child in children.iter() {
                commands.entity(child).despawn();
            }
        }
        for (index, recipe) in recipes.iter().enumerate() {
            let validation = crate::crafting::validate_recipe(recipe, context, &pockets, &backpack);
            let craftable = validation.is_craftable();
            let selected = index == state.selected;
            let (bg, border) = if selected {
                (theme.colors.selected_bg, theme.colors.border_active)
            } else {
                (theme.colors.panel_bg, theme.colors.faint_border)
            };
            let row = spawn_panel(
                &mut commands,
                &theme,
                list_entity,
                Node {
                    width: Val::Percent(100.0),
                    flex_shrink: 0.0,
                    padding: UiRect::axes(Val::Px(6.0), Val::Px(4.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(1.0),
                    border: theme.shape.border(),
                    border_radius: theme.shape.rounded(),
                    ..default()
                },
                bg,
                border,
            );
            commands.entity(row).insert((
                Button,
                WorkbenchRecipeSelect(index),
                bevy::picking::Pickable::default(),
                Hovered::default(),
            ));
            spawn_text(
                &mut commands,
                &theme,
                row,
                recipe.name,
                theme.fonts.small,
                if craftable {
                    theme.colors.text
                } else {
                    theme.colors.muted_text
                },
                HudText,
            );
            let status = if craftable {
                "Ready".to_string()
            } else {
                validation.summary()
            };
            spawn_text(
                &mut commands,
                &theme,
                row,
                status,
                theme.fonts.micro,
                if craftable {
                    theme.colors.success
                } else {
                    theme.colors.warn
                },
                HudText,
            );
        }
    }

    let recipe = recipes[state.selected];
    if let Ok(mut t) = detail_q.p0().single_mut() {
        t.0 = recipe.name.to_string();
    }
    if let Ok(mut t) = detail_q.p1().single_mut() {
        t.0 = recipe.description.to_string();
    }
    let mut ing_lines = Vec::new();
    for ing in recipe.ingredients {
        let have = crate::items::inventory_count(ing.item, &pockets, &backpack);
        ing_lines.push(format!(
            "{}: {}/{}",
            ing.item.display_name(),
            have,
            ing.count
        ));
    }
    if ing_lines.is_empty() {
        ing_lines.push("No ingredients".to_string());
    }
    if let Ok(mut t) = detail_q.p2().single_mut() {
        t.0 = ing_lines.join("\n");
    }
    if let Ok(mut img) = detail_preview.single_mut() {
        if let Some(out) = recipe.outputs.first() {
            img.image = icons.item(out.item);
            img.color = Color::WHITE;
        } else {
            img.color = Color::NONE;
        }
    }

    for (btn, mut bg, mut border) in &mut craft_buttons {
        let can = crate::crafting::can_craft_quantity(recipe, context, &pockets, &backpack, btn.0);
        *bg = BackgroundColor(if can {
            theme.colors.button_bg
        } else {
            theme.colors.panel_bg
        });
        *border = BorderColor::all(if can {
            theme.colors.border_idle
        } else {
            theme.colors.faint_border
        });
    }
}

#[allow(clippy::type_complexity)]
fn handle_crafting_scroll(
    ui: Res<UiManager>,
    mut mouse_wheel: bevy::ecs::message::MessageReader<bevy::input::mouse::MouseWheel>,
    mut areas: Query<(&mut ScrollPosition, &Hovered, &ComputedNode), With<CraftingScrollArea>>,
    row_hovered: Query<&Hovered, Or<(With<CraftingSelectButton>, With<CraftingCraftButton>)>>,
) {
    if !ui.is_open(Overlay::Backpack) {
        return;
    }
    let mut delta = 0.0_f32;
    for ev in mouse_wheel.read() {
        let y = match ev.unit {
            bevy::input::mouse::MouseScrollUnit::Line => ev.y * 24.0,
            bevy::input::mouse::MouseScrollUnit::Pixel => ev.y,
        };
        delta += y;
    }
    if delta.abs() < f32::EPSILON {
        return;
    }
    let Ok((mut scroll, hovered, area_node)) = areas.single_mut() else {
        return;
    };
    let over_list = hovered.get() || row_hovered.iter().any(|h| h.get());
    if !over_list {
        return;
    }
    let content_h = area_node.content_size().y;
    let viewport_h = area_node.size().y.max(1.0);
    let max_scroll = (content_h - viewport_h).max(0.0);
    scroll.y = (scroll.y - delta).clamp(0.0, max_scroll);
}

fn update_crafting_scrollbar(
    ui: Res<UiManager>,
    areas: Query<(&ScrollPosition, &ComputedNode), With<CraftingScrollArea>>,
    mut thumbs: Query<(&mut Node, &mut BackgroundColor), With<CraftingScrollbarThumb>>,
    theme: Res<UiTheme>,
) {
    if !ui.is_open(Overlay::Backpack) {
        return;
    }
    let Ok((scroll, area_node)) = areas.single() else {
        return;
    };
    let Ok((mut thumb_node, mut thumb_bg)) = thumbs.single_mut() else {
        return;
    };

    let content_h = area_node.content_size().y;
    let viewport_h = area_node.size().y.max(1.0);
    let max_scroll = (content_h - viewport_h).max(0.0);

    if max_scroll <= 1.0 {
        thumb_node.height = Val::Percent(100.0);
        thumb_node.top = Val::Px(0.0);
        if thumb_bg.0 != theme.colors.faint_border {
            thumb_bg.0 = theme.colors.faint_border;
        }
        return;
    }

    let ratio = (viewport_h / content_h).clamp(0.1, 1.0);
    let offset = (scroll.y / max_scroll).clamp(0.0, 1.0);
    let thumb_pct = ratio * 100.0;
    let top_pct = offset * (100.0 - thumb_pct);
    thumb_node.height = Val::Percent(thumb_pct);
    thumb_node.top = Val::Percent(top_pct);
    if thumb_bg.0 != theme.colors.border_hover {
        thumb_bg.0 = theme.colors.border_hover;
    }
}

#[allow(clippy::too_many_arguments)]
fn update_backpack_tooltip(
    ui: Res<UiManager>,
    dragged: Res<DraggedSlot>,
    window: Query<&Window, With<PrimaryWindow>>,
    slots: Query<(&SlotSource, &Hovered)>,
    pockets: Res<Pockets>,
    backpack: Res<Backpack>,
    mut tooltip: Query<&mut Node, With<TooltipRoot>>,
    mut title: Query<&mut Text, (With<TooltipTitle>, Without<TooltipBody>)>,
    mut body: Query<&mut Text, (With<TooltipBody>, Without<TooltipTitle>)>,
) {
    let Ok(mut node) = tooltip.single_mut() else {
        return;
    };
    let Ok(mut title_text) = title.single_mut() else {
        return;
    };
    let Ok(mut body_text) = body.single_mut() else {
        return;
    };

    let mut hide = || {
        if node.display != Display::None {
            node.display = Display::None;
        }
    };

    if !ui.is_open(Overlay::Backpack) || dragged.0.is_some() {
        hide();
        return;
    }
    let Ok(window) = window.single() else {
        hide();
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        hide();
        return;
    };

    let mut hovered_stack = None;
    for (source, hovered) in &slots {
        if hovered.0 {
            hovered_stack = slot_stack(*source, &pockets, &backpack);
            if hovered_stack.is_some() {
                break;
            }
        }
    }
    let Some(stack) = hovered_stack else {
        hide();
        return;
    };

    let item = stack.item;
    let mut desc = item.def().description.to_string();
    if matches!(item, ItemId::Lantern | ItemId::Magnet) {
        desc.push_str("\nBackpack attachment: provides its effect while attached.");
    }
    desc.push('\n');
    desc.push_str(&format!(
        "Category: {}\nCount: {}\nWeight: {:.1} | Stack: {}",
        item.def().category.display_name(),
        stack.count,
        item.def().weight,
        item.def().max_stack
    ));
    let desired_title = item.display_name().to_string();
    if title_text.0 != desired_title {
        title_text.0 = desired_title;
    }
    if body_text.0 != desc {
        body_text.0 = desc;
    }
    if node.display != Display::Flex {
        node.display = Display::Flex;
    }
    // Clamp tooltip to viewport so it stays readable near edges.
    let tooltip_w = 220.0f32;
    let tooltip_h = 96.0f32;
    let max_left = (window.width() - tooltip_w - 8.0).max(8.0);
    let max_top = (window.height() - tooltip_h - 8.0).max(8.0);
    let desired_left = Val::Px((cursor.x + 16.0).clamp(8.0, max_left));
    let desired_top = Val::Px((cursor.y + 16.0).clamp(8.0, max_top));
    if node.left != desired_left {
        node.left = desired_left;
    }
    if node.top != desired_top {
        node.top = desired_top;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_drag_ghost(
    ui: Res<UiManager>,
    dragged: Res<DraggedSlot>,
    window: Query<&Window, With<PrimaryWindow>>,
    pockets: Res<Pockets>,
    backpack: Res<Backpack>,
    icons: Res<UiIcons>,
    mut root: Query<&mut Node, With<DragGhostRoot>>,
    mut icon: Query<
        (&mut Node, &mut ImageNode),
        (
            With<DragGhostIcon>,
            Without<DragGhostRoot>,
            Without<DragGhostCount>,
        ),
    >,
    mut count: Query<
        (&mut Node, &mut Text),
        (
            With<DragGhostCount>,
            Without<DragGhostRoot>,
            Without<DragGhostIcon>,
        ),
    >,
) {
    let Ok(mut root_node) = root.single_mut() else {
        return;
    };
    let Some(source) = dragged.0.filter(|_| ui.is_open(Overlay::Backpack)) else {
        root_node.display = Display::None;
        return;
    };
    let Ok(window) = window.single() else {
        root_node.display = Display::None;
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        root_node.display = Display::None;
        return;
    };
    let Some(stack) = slot_stack(source, &pockets, &backpack) else {
        root_node.display = Display::None;
        return;
    };

    let size = HOTBAR_SLOT_SIZE - 8.0;
    root_node.display = Display::Flex;
    root_node.left = Val::Px(cursor.x - size * 0.5);
    root_node.top = Val::Px(cursor.y - size * 0.5);

    if let Ok((_, mut image)) = icon.single_mut() {
        let desired = icons.item(stack.item);
        if image.image != desired {
            image.image = desired;
        }
    }
    if let Ok((mut count_node, mut text)) = count.single_mut() {
        if stack.count > 1 {
            count_node.display = Display::Flex;
            let desired = stack.count.to_string();
            if text.0 != desired {
                text.0 = desired;
            }
        } else {
            count_node.display = Display::None;
            if !text.0.is_empty() {
                text.0.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_starts_at_north_and_turns_clockwise() {
        assert_eq!(heading_degrees(Vec3::NEG_Z), 0.0);
        assert_eq!(heading_degrees(Vec3::X), 90.0);
        assert_eq!(heading_degrees(Vec3::Z), 180.0);
        assert_eq!(heading_degrees(Vec3::NEG_X), 270.0);
    }

    #[test]
    fn compass_label_is_centred_under_the_fixed_tick() {
        let tick_center = COMPASS_WIDTH * 0.5;
        let band_center = compass_label_left(0.0) + COMPASS_LABEL_WIDTH * 0.5;
        assert_eq!(tick_center, band_center);
    }

    #[test]
    fn compass_heading_follows_player_yaw() {
        assert_eq!(compass_heading(0.0), 0.0);
        let east = compass_heading(-90.0_f32.to_radians());
        assert!((east - 90.0).abs() < 1e-4);
    }

    #[test]
    fn compass_uses_four_cardinal_bands() {
        assert_eq!(cardinal_index(0.0), 0);
        assert_eq!(cardinal_index(44.0), 0);
        assert_eq!(cardinal_index(46.0), 1);
        assert_eq!(cardinal_index(135.0), 2);
        assert_eq!(cardinal_index(225.0), 3);
        assert_eq!(cardinal_index(359.0), 0);
    }

    #[test]
    fn alpha_color_preserves_channels() {
        let colored = alpha_color(Color::srgb(0.2, 0.4, 0.6), 0.5);
        let srgba = colored.to_srgba();
        assert!((srgba.red - 0.2).abs() < 1e-5);
        assert!((srgba.green - 0.4).abs() < 1e-5);
        assert!((srgba.blue - 0.6).abs() < 1e-5);
        assert!((srgba.alpha - 0.5).abs() < 1e-5);
    }

    #[test]
    fn docked_panels_join_cleanly() {
        let theme = UiTheme::default();
        let zero = BorderRadius::all(Val::Px(0.0));
        let rounded = theme.shape.rounded();
        // When closed the hotbar is fully rounded (stamina overlaps its top edge).
        // When the backpack is open, the hotbar's top edge flattens to join the
        // storage panel above it while the storage bottom flattens to meet it.
        let hotbar = hotbar_radius(false, &theme);
        assert_eq!(hotbar.top_left, rounded.top_left);
        assert_eq!(hotbar.top_right, rounded.top_right);
        assert_eq!(hotbar.bottom_left, rounded.bottom_left);
        assert_eq!(hotbar.bottom_right, rounded.bottom_right);
        let hotbar_joined = hotbar_radius(true, &theme);
        assert_eq!(hotbar_joined.top_left, zero.top_left);
        assert_eq!(hotbar_joined.top_right, zero.top_right);
        assert_eq!(hotbar_joined.bottom_left, rounded.bottom_left);
        assert_eq!(hotbar_joined.bottom_right, rounded.bottom_right);
        let storage = storage_radius(true, &theme);
        assert_eq!(storage.bottom_left, zero.bottom_left);
        assert_eq!(storage.bottom_right, zero.bottom_right);
    }

    #[test]
    fn hud_bars_use_compact_readable_style() {
        const {
            assert!(STAT_BAR_HEIGHT == 22.0);
            assert!(STAMINA_BAR_HEIGHT == 16.0);
        }
        let theme = UiTheme::default();
        assert!(theme.fonts.body >= theme.fonts.small);
    }

    #[test]
    fn stamina_attaches_to_hotbar_with_side_insets() {
        const {
            assert!(STAMINA_SIDE_INSET == 8.0);
            assert!(STAMINA_BAR_WIDTH == HOTBAR_WIDTH - STAMINA_SIDE_INSET * 2.0);
            // Sits directly on top of the hotbar with a 1px overlap to avoid a seam.
            assert!(STAMINA_RISE == HOTBAR_HEIGHT - 1.0);
            assert!(STAMINA_BAR_HEIGHT == 16.0);
            assert!(INSIGHT_RISE > 0.0);
            assert!(VITAL_GAP_FROM_HOTBAR > 0.0);
        }
    }

    #[test]
    fn character_hud_indicators_are_square_and_readable() {
        const {
            assert!(VITAL_INDICATOR_SIZE >= 40.0);
        }
        let theme = UiTheme::default();
        let title = theme.colors.text.to_srgba();
        let desc = theme.colors.muted_text.to_srgba();
        assert!(desc.red < title.red && desc.green < title.green && desc.blue < title.blue);
        assert!(theme.fonts.small < theme.fonts.body);
    }

    #[test]
    fn drag_swap_moves_stacks_between_pocket_and_storage() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        let granite = ItemStack {
            item: ItemId::Granite,
            count: 3,
        };
        let sticks = ItemStack {
            item: ItemId::Stick,
            count: 5,
        };
        pockets.replace(0, Some(granite));
        backpack.replace_slot(1, Some(sticks));

        let source = SlotSource::Pocket(0);
        let target = SlotSource::Storage(1);
        assert!(slot_accepts(source, &pockets, &backpack));
        assert!(slot_accepts(target, &pockets, &backpack));

        let carried = take_slot(source, &mut pockets, &mut backpack).unwrap();
        let displaced = put_slot(target, Some(carried), &mut pockets, &mut backpack).unwrap();
        let _ = put_slot(source, Some(displaced), &mut pockets, &mut backpack);

        assert_eq!(slot_stack(source, &pockets, &backpack), Some(sticks));
        assert_eq!(slot_stack(target, &pockets, &backpack), Some(granite));
    }

    #[test]
    fn drag_lantern_into_attachment_slot_and_back() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        let lantern = ItemStack {
            item: ItemId::Lantern,
            count: 1,
        };
        pockets.replace(0, Some(lantern));

        let pocket = SlotSource::Pocket(0);
        let attachment = SlotSource::Attachment(0);
        let carried = take_slot(pocket, &mut pockets, &mut backpack).unwrap();
        let displaced = put_slot(attachment, Some(carried), &mut pockets, &mut backpack);
        assert!(displaced.is_none());
        assert!(backpack.has_attachment_item(ItemId::Lantern));
        assert_eq!(slot_stack(attachment, &pockets, &backpack), Some(lantern));
        assert!(slot_stack(pocket, &pockets, &backpack).is_none());

        let carried = take_slot(attachment, &mut pockets, &mut backpack).unwrap();
        let displaced = put_slot(pocket, Some(carried), &mut pockets, &mut backpack);
        assert!(displaced.is_none());
        assert!(!backpack.has_attachment_item(ItemId::Lantern));
        assert_eq!(slot_stack(pocket, &pockets, &backpack), Some(lantern));
    }

    #[test]
    fn drag_magnet_into_attachment_slot() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        let magnet = ItemStack {
            item: ItemId::Magnet,
            count: 1,
        };
        pockets.replace(0, Some(magnet));

        let pocket = SlotSource::Pocket(0);
        let attachment = SlotSource::Attachment(1);
        let carried = take_slot(pocket, &mut pockets, &mut backpack).unwrap();
        let displaced = put_slot(attachment, Some(carried), &mut pockets, &mut backpack);
        assert!(displaced.is_none());
        assert!(backpack.has_attachment_item(ItemId::Magnet));
        assert_eq!(slot_stack(attachment, &pockets, &backpack), Some(magnet));
    }
}
