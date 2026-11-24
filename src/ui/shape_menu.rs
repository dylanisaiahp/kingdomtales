//! Hold-RMB hammer shape menu.
//!
//! The hammer only changes the geometry of an existing block, so the menu offers
//! the buildable shapes and remembers recent choices in [`ConstructionState`].
//! Holding right mouse with the hammer opens the menu; releasing it applies the
//! hovered shape. Clicking an option applies it immediately.
//!
//! This is a vertical presentation menu, not a radial tool wheel.

use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::Pressed;
use bevy::ui_widgets::{Activate, Button};

use super::theme::{UiTheme, spawn_panel, spawn_text};
use crate::construction::ConstructionState;
use crate::items::selected_tool;
use crate::player::CursorCapture;
use crate::pockets::Pockets;
use crate::tools::ToolKind;
use crate::ui_manager::{Overlay, UiManager};
use crate::world::block::Shape;

/// Ordered alongside the other UI chains so shared `Node`/`Text` access is
/// deterministic.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ShapeMenuSystems;

#[derive(Component)]
struct ShapeMenuRoot;

#[derive(Component)]
struct ShapeText;

#[derive(Component, Clone, Copy)]
struct ShapeChoice(Shape);

#[derive(Component)]
struct RecentText;

pub struct ShapeMenuPlugin;

impl Plugin for ShapeMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_shape_menu)
            .add_systems(
                Update,
                (
                    hold_shape_menu,
                    sync_shape_menu_visibility,
                    style_shape_options,
                    update_recent_text,
                )
                    .chain()
                    .in_set(ShapeMenuSystems),
            )
            .add_observer(on_activate);
    }
}

fn setup_shape_menu(mut commands: Commands, theme: Res<UiTheme>) {
    let root = commands
        .spawn((
            ShapeMenuRoot,
            Name::new("ShapeMenu"),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::None,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(theme.colors.overlay_bg),
            GlobalZIndex(theme.layers.shape_menu),
        ))
        .id();

    let panel = spawn_panel(
        &mut commands,
        &theme,
        root,
        Node {
            width: Val::Px(300.0),
            padding: UiRect::all(Val::Px(18.0)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
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
        "HAMMER SHAPE",
        theme.fonts.heading,
        theme.colors.text,
        ShapeText,
    );
    spawn_text(
        &mut commands,
        &theme,
        panel,
        "Hold RMB and hover | release to apply",
        theme.fonts.small,
        theme.colors.muted_text,
        ShapeText,
    );

    for shape in Shape::BUILDABLE {
        spawn_option(
            &mut commands,
            &theme,
            panel,
            ShapeChoice(shape),
            210.0,
            42.0,
            shape.display_name(),
            shape_hint(shape),
        );
    }

    spawn_text(
        &mut commands,
        &theme,
        panel,
        "Recent: none",
        theme.fonts.small,
        theme.colors.muted_text,
        RecentText,
    );
    spawn_text(
        &mut commands,
        &theme,
        panel,
        "LMB applies the shape | material is preserved",
        theme.fonts.small,
        theme.colors.muted_text,
        ShapeText,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_option(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    choice: ShapeChoice,
    width: f32,
    height: f32,
    name: &str,
    hint: &str,
) {
    let button = spawn_panel(
        commands,
        theme,
        parent,
        Node {
            width: Val::Px(width),
            height: Val::Px(height),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: theme.shape.emphasis_border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.button_bg,
        theme.colors.border_idle,
    );
    commands
        .entity(button)
        .insert((Button, choice, Hovered::default()));

    spawn_text(
        commands,
        theme,
        button,
        name,
        theme.fonts.compass,
        theme.colors.text,
        ShapeText,
    );
    spawn_text(
        commands,
        theme,
        button,
        hint,
        theme.fonts.micro,
        theme.colors.muted_text,
        ShapeText,
    );
}

fn shape_hint(shape: Shape) -> &'static str {
    match shape {
        Shape::Cube => "Full block",
        Shape::HalfBlock => "Half height",
        Shape::Slope => "Ramp",
        Shape::Stairs => "Two steps",
        Shape::Panel => "Thin layer",
        Shape::Stick
        | Shape::Pebble
        | Shape::Foliage
        | Shape::SmallFoliage
        | Shape::Trunk
        | Shape::Stem
        | Shape::DebarkedTrunk
        | Shape::DebarkedStem
        | Shape::StoneDeposit => "Not buildable",
    }
}

fn holding_hammer(pockets: &Pockets) -> bool {
    selected_tool(pockets).is_some_and(ToolKind::is_hammer)
}

fn hold_shape_menu(
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    mut ui: ResMut<UiManager>,
    pockets: Res<Pockets>,
    mut construction: ResMut<ConstructionState>,
    options: Query<(&ShapeChoice, &Hovered)>,
) {
    if !holding_hammer(&pockets) {
        if ui.is_open(Overlay::ShapeMenu) {
            ui.close(Overlay::ShapeMenu);
        }
        return;
    }

    if !ui.is_open(Overlay::ShapeMenu) {
        if mouse.just_pressed(MouseButton::Right) && capture.grabbed && !ui.any_open() {
            ui.open(Overlay::ShapeMenu);
        }
        return;
    }

    if mouse.just_released(MouseButton::Right) || !mouse.pressed(MouseButton::Right) {
        for (choice, hovered) in &options {
            if hovered.0 {
                apply_choice(&mut construction, *choice);
            }
        }
        ui.close(Overlay::ShapeMenu);
    }
}

fn apply_choice(construction: &mut ConstructionState, choice: ShapeChoice) {
    construction.select_shape(choice.0);
}

fn sync_shape_menu_visibility(
    ui: Res<UiManager>,
    mut roots: Query<&mut Node, With<ShapeMenuRoot>>,
) {
    let desired = if ui.is_open(Overlay::ShapeMenu) {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut roots {
        if node.display != desired {
            node.display = desired;
        }
    }
}

type ShapeOptionQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static ShapeChoice,
        &'static Hovered,
        Has<Pressed>,
        &'static mut BackgroundColor,
        &'static mut BorderColor,
    ),
    With<Button>,
>;

fn style_shape_options(
    construction: Res<ConstructionState>,
    theme: Res<UiTheme>,
    mut options: ShapeOptionQuery,
) {
    for (choice, hovered, pressed, mut background, mut border) in &mut options {
        let active = construction.shape == choice.0;
        let desired = if pressed {
            theme.colors.button_pressed
        } else if hovered.0 {
            theme.colors.button_hover
        } else if active {
            theme.colors.selected_bg
        } else {
            theme.colors.button_bg
        };
        if background.0 != desired {
            background.0 = desired;
        }

        let desired_border = if hovered.0 {
            theme.colors.border_hover
        } else if active {
            theme.colors.border_active
        } else {
            theme.colors.border_idle
        };
        if border.top != desired_border {
            *border = BorderColor::all(desired_border);
        }
    }
}

fn update_recent_text(
    construction: Res<ConstructionState>,
    mut texts: Query<&mut Text, With<RecentText>>,
) {
    if !construction.is_changed() {
        return;
    }
    let shape = construction
        .recent_shapes()
        .first()
        .copied()
        .unwrap_or(construction.shape);
    let desired = format!("Recent: {}", shape.display_name());
    for mut text in &mut texts {
        if text.0 != desired {
            text.0 = desired.clone();
        }
    }
}

fn on_activate(
    activate: On<Activate>,
    options: Query<&ShapeChoice>,
    mut construction: ResMut<ConstructionState>,
    mut ui: ResMut<UiManager>,
) {
    if !ui.is_open(Overlay::ShapeMenu) {
        return;
    }
    let Ok(choice) = options.get(activate.entity) else {
        return;
    };

    apply_choice(&mut construction, *choice);
    ui.close(Overlay::ShapeMenu);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_only_shows_buildable_shapes() {
        assert_eq!(Shape::BUILDABLE.len(), 5);
        for shape in Shape::BUILDABLE {
            assert!(shape.is_buildable());
        }
    }

    #[test]
    fn choosing_a_shape_remembers_it() {
        let mut state = ConstructionState::default();
        apply_choice(&mut state, ShapeChoice(Shape::Stairs));
        assert_eq!(state.shape, Shape::Stairs);
        assert_eq!(state.recent_shapes().first().copied(), Some(Shape::Stairs));
    }

    #[test]
    fn natural_shapes_are_ignored() {
        let mut state = ConstructionState::default();
        apply_choice(&mut state, ShapeChoice(Shape::Stick));
        assert_eq!(state.shape, Shape::Cube);
    }
}
