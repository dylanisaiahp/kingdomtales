//! Creative give menu opened by `/give` with no arguments.
//!
//! Small centered panel: search bar + scrollable item list. Clicking an item
//! gives one to pockets/backpack.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::input::{ButtonState, keyboard::KeyboardInput};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::Pressed;
use bevy::ui_widgets::{Activate, Button};

use super::theme::{UiTheme, display_if, spawn_panel, spawn_text};
use crate::backpack::Backpack;
use crate::items::{ItemId, store_item};
use crate::notifications::NotificationFeed;
use crate::pockets::Pockets;
use crate::ui::icons::UiIcons;
use crate::ui_manager::{Overlay, UiManager};

const MENU_W: f32 = 356.0;
const MENU_H: f32 = 512.0;
const LIST_H: f32 = 400.0;
const ROW_H: f32 = 30.0;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GiveMenuSystems;

#[derive(Resource, Default, Debug)]
struct GiveMenuState {
    search: String,
    scroll: f32,
}

#[derive(Component)]
struct GiveMenuRoot;

#[derive(Component)]
struct GiveMenuSearchText;

#[derive(Component)]
struct GiveMenuList;

#[derive(Component)]
struct GiveMenuRow(ItemId);

#[derive(Component)]
struct GiveMenuCountText;

pub struct GiveMenuPlugin;

impl Plugin for GiveMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GiveMenuState>()
            .add_systems(Startup, setup_give_menu)
            .add_systems(
                Update,
                (
                    sync_give_menu_visibility,
                    handle_give_menu_input,
                    scroll_give_menu,
                    rebuild_give_menu_list,
                    style_give_rows,
                )
                    .chain()
                    .in_set(GiveMenuSystems),
            )
            .add_observer(on_give_row_activate);
    }
}

fn setup_give_menu(mut commands: Commands, theme: Res<UiTheme>) {
    let root = commands
        .spawn((
            GiveMenuRoot,
            Name::new("GiveMenu"),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::None,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(theme.colors.overlay_bg),
            GlobalZIndex(theme.layers.shape_menu + 5),
        ))
        .id();

    let panel = spawn_panel(
        &mut commands,
        &theme,
        root,
        Node {
            width: Val::Px(MENU_W),
            height: Val::Px(MENU_H),
            padding: UiRect::all(Val::Px(10.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.panel_bg_strong,
        theme.colors.panel_border,
    );

    spawn_text(
        &mut commands,
        &theme,
        panel,
        "GIVE ITEM",
        theme.fonts.heading,
        theme.colors.text,
        GiveMenuCountText,
    );
    spawn_text(
        &mut commands,
        &theme,
        panel,
        "Click item to give 1. Esc to close.",
        theme.fonts.small,
        theme.colors.muted_text,
        GiveMenuCountText,
    );

    let search_row = spawn_panel(
        &mut commands,
        &theme,
        panel,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded_small(),
            ..default()
        },
        theme.colors.slider_track,
        theme.colors.faint_border,
    );
    spawn_text(
        &mut commands,
        &theme,
        search_row,
        "Search: ",
        theme.fonts.body,
        theme.colors.muted_text,
        GiveMenuCountText,
    );
    spawn_text(
        &mut commands,
        &theme,
        search_row,
        "",
        theme.fonts.body,
        theme.colors.text,
        GiveMenuSearchText,
    );

    commands.spawn((
        GiveMenuList,
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(LIST_H),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            overflow: Overflow::clip_y(),
            ..default()
        },
        ChildOf(panel),
    ));
}

fn sync_give_menu_visibility(
    ui: Res<UiManager>,
    mut roots: Query<&mut Node, With<GiveMenuRoot>>,
    mut state: ResMut<GiveMenuState>,
) {
    let open = ui.is_open(Overlay::GiveMenu);
    let desired = display_if(open);
    for mut node in &mut roots {
        if node.display != desired {
            node.display = desired;
        }
    }
    if ui.is_changed() && !open {
        state.search.clear();
        state.scroll = 0.0;
    }
}

fn handle_give_menu_input(
    ui: Res<UiManager>,
    mut keyboard: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<GiveMenuState>,
) {
    if !ui.is_open(Overlay::GiveMenu) {
        return;
    }
    for input in keyboard.read() {
        if input.state != ButtonState::Pressed {
            continue;
        }
        match input.key_code {
            KeyCode::Backspace => {
                state.search.pop();
            }
            KeyCode::Escape => {} // handled by player cursor system
            _ => {
                if let Some(text) = &input.text {
                    for c in text.chars() {
                        if !c.is_control() {
                            state.search.push(c);
                        }
                    }
                }
            }
        }
    }
    if keys.just_pressed(KeyCode::PageDown) {
        state.scroll += LIST_H;
    }
    if keys.just_pressed(KeyCode::PageUp) {
        state.scroll = (state.scroll - LIST_H).max(0.0);
    }
}

fn filtered_items(search: &str) -> Vec<ItemId> {
    let q = search.trim().to_ascii_lowercase();
    ItemId::ALL
        .into_iter()
        .filter(|id| {
            if q.is_empty() {
                return true;
            }
            let name = id.display_name().to_ascii_lowercase();
            let key = format!("{:?}", id).to_ascii_lowercase();
            name.contains(&q) || key.contains(&q)
        })
        .collect()
}

fn scroll_give_menu(
    ui: Res<UiManager>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    mut state: ResMut<GiveMenuState>,
) {
    if !ui.is_open(Overlay::GiveMenu) {
        return;
    }
    let Some(scroll) = scroll else { return };
    if scroll.delta.y.abs() < f32::EPSILON {
        return;
    }
    let max = max_scroll(&state.search);
    state.scroll = (state.scroll - scroll.delta.y * ROW_H * 2.0).clamp(0.0, max);
}

fn max_scroll(search: &str) -> f32 {
    let count = filtered_items(search).len() as f32;
    (count * (ROW_H + 2.0) - LIST_H).max(0.0)
}

#[allow(clippy::too_many_arguments)]
fn rebuild_give_menu_list(
    mut commands: Commands,
    ui: Res<UiManager>,
    state: Res<GiveMenuState>,
    theme: Res<UiTheme>,
    icons: Option<Res<UiIcons>>,
    lists: Query<Entity, With<GiveMenuList>>,
    children_q: Query<&Children>,
    mut search_text: Query<&mut Text, With<GiveMenuSearchText>>,
) {
    if !ui.is_open(Overlay::GiveMenu) {
        return;
    }
    if let Ok(mut text) = search_text.single_mut() {
        let desired = if state.search.is_empty() {
            "type to search...".to_string()
        } else {
            state.search.clone()
        };
        if text.0 != desired {
            text.0 = desired;
        }
    }

    if !state.is_changed() && !ui.is_changed() {
        // icons may load later, but cheap enough to rebuild on open/change only
        if icons.as_ref().is_none_or(|i| !i.is_changed()) {
            return;
        }
    }

    let Ok(list) = lists.single() else { return };
    if let Ok(children) = children_q.get(list) {
        for child in children.iter() {
            commands.entity(child).despawn();
        }
    }

    let items = filtered_items(&state.search);
    let max = max_scroll(&state.search);
    let scroll = state.scroll.clamp(0.0, max);
    let start = (scroll / (ROW_H + 2.0)).floor() as usize;
    let visible =
        ((LIST_H / (ROW_H + 2.0)).ceil() as usize + 1).min(items.len().saturating_sub(start));
    let Some(icons) = icons.as_deref() else {
        return;
    };

    for item in items.iter().skip(start).take(visible).copied() {
        let row = spawn_panel(
            &mut commands,
            &theme,
            list,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(ROW_H),
                padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded_small(),
                ..default()
            },
            theme.colors.button_bg,
            theme.colors.faint_border,
        );
        commands
            .entity(row)
            .insert((Button, GiveMenuRow(item), Hovered::default()));

        commands.spawn((
            Node {
                width: Val::Px(22.0),
                height: Val::Px(22.0),
                ..default()
            },
            ImageNode::new(icons.item(item)).with_color(Color::WHITE),
            Pickable::IGNORE,
            ChildOf(row),
        ));

        spawn_text(
            &mut commands,
            &theme,
            row,
            item.display_name(),
            theme.fonts.body,
            theme.colors.text,
            GiveMenuCountText,
        );
    }
}

fn style_give_rows(
    mut rows: Query<
        (
            &Hovered,
            Has<Pressed>,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        With<GiveMenuRow>,
    >,
    theme: Res<UiTheme>,
) {
    for (hovered, pressed, mut bg, mut border) in &mut rows {
        let (bg_color, border_color) = if pressed {
            (theme.colors.button_pressed, theme.colors.accent)
        } else if hovered.0 {
            (theme.colors.button_hover, theme.colors.border_hover)
        } else {
            (theme.colors.button_bg, theme.colors.faint_border)
        };
        if bg.0 != bg_color {
            bg.0 = bg_color;
        }
        if border.top != border_color {
            *border = BorderColor::all(border_color);
        }
    }
}

fn on_give_row_activate(
    activate: On<Activate>,
    rows: Query<&GiveMenuRow>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    mut chat: ResMut<crate::chat::ChatState>,
    mut notifications: ResMut<NotificationFeed>,
    theme: Res<UiTheme>,
) {
    let Ok(row) = rows.get(activate.entity) else {
        return;
    };
    let item = row.0;
    let count = item.def().max_stack.max(1);
    let leftover = store_item(&mut pockets, &mut backpack, item, count);
    let given = count - leftover;
    if given > 0 {
        notifications.push_item(item, given);
        crate::chat::push_message(
            &mut chat,
            format!("Gave {} x{}", item.display_name(), given),
            theme.colors.text,
        );
    }
    if leftover > 0 {
        crate::chat::push_message(
            &mut chat,
            "Pockets and backpack are full.",
            theme.colors.danger,
        );
    }
}
