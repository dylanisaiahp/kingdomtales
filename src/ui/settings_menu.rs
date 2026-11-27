//! Pause menu and tabbed settings screen.
//!
//! Esc opens a small vertical pause menu, matching the hammer shape menu's
//! presentation. "Settings" opens this module's tabbed panel; the pause menu
//! itself only offers Resume, Settings and Quit. All buttons use Bevy 0.20's
//! `Button` widget with an `Activate` observer, while numeric settings use the
//! `Slider` widget. The Keybinds tab captures the next key press and swaps
//! conflicting assignments so every action keeps a unique key.

use bevy::ecs::system::SystemParam;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::ComputedNode;
use bevy::ui::Pressed;
use bevy::ui_widgets::{
    Activate, Button, Slider, SliderOrientation, SliderPrecision, SliderRange, SliderStep,
    SliderThumb, SliderValue, TrackClick, ValueChange, slider_self_update,
};

use crate::settings::{
    BindingAction, DEFAULT_MOUSE_SENSITIVITY, GameSettings, InputBindings, KeybindCapture,
    MAX_FOV_DEGREES, MAX_RENDER_DISTANCE, MAX_SENSITIVITY_MULTIPLIER, MIN_FOV_DEGREES,
    MIN_RENDER_DISTANCE, MIN_SENSITIVITY_MULTIPLIER, MenuState, estimated_chunk_memory_mb,
};
use crate::ui::theme::{UiTheme, display_if, key_label, spawn_panel, spawn_text};
use crate::ui_manager::{Overlay, UiManager};

const SLIDER_WIDTH: f32 = 190.0;
const SLIDER_HEIGHT: f32 = 20.0;
const THUMB_SIZE: f32 = 14.0;
const VALUE_WIDTH: f32 = 78.0;
const SETTINGS_PADDING_X: f32 = 36.0;
const SETTINGS_PADDING_Y: f32 = 24.0;
const SETTINGS_SIDEBAR_WIDTH: f32 = 208.0;
const SETTINGS_TAB_WIDTH: f32 = 156.0;
const SETTINGS_ROW_WIDTH: f32 = 720.0;
const SETTINGS_CONTENT_WIDTH: f32 = 980.0;
const KEYBIND_CARD_WIDTH: f32 = 300.0;

/// Ordered alongside the other UI chains so shared `Node`/`Text` access is
/// deterministic.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SettingsMenuSystems;

#[derive(Component)]
struct PauseMenuRoot;

#[derive(Component)]
struct SettingsMenuRoot;

/// Static text that never needs a per-frame update.
#[derive(Component)]
struct StaticLabel;

/// Shared marker so pause, tab and settings buttons get the same hover state.
#[derive(Component)]
struct MenuButton;

/// Marks the visible content panel for one settings tab.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
struct TabPanel(SettingsTab);

#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SettingsTab {
    #[default]
    Graphics,
    Lighting,
    Color,
    Mouse,
    Keybinds,
    Audio,
}

impl SettingsTab {
    const ALL: [Self; 6] = [
        Self::Graphics,
        Self::Lighting,
        Self::Color,
        Self::Mouse,
        Self::Keybinds,
        Self::Audio,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Graphics => "Graphics",
            Self::Lighting => "Lighting",
            Self::Color => "Color",
            Self::Mouse => "Mouse",
            Self::Keybinds => "Keybinds",
            Self::Audio => "Audio",
        }
    }
}

#[derive(Component, Clone, Copy, Debug)]
enum SettingsAction {
    ToggleVsync,
    ToggleFullscreen,
    TogglePerfLog,
    ToggleClouds,
    CycleLightShafts,
    CycleTonemapping,
    CycleSsao,
    CycleBloom,
    CycleShadows,
    Rebind(BindingAction),
    ResetDefaults,
    Resume,
    OpenSettings,
    Back,
    Quit,
    SelectTab(SettingsTab),
}

/// Which numeric setting a slider edits.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSlider {
    Sensitivity,
    Fov,
    RenderDistance,
    Exposure,
    Saturation,
    Contrast,
    Temperature,
    WindStrength,
}

impl SettingsSlider {
    fn range(self) -> (f32, f32) {
        match self {
            SettingsSlider::Sensitivity => (MIN_SENSITIVITY_MULTIPLIER, MAX_SENSITIVITY_MULTIPLIER),
            SettingsSlider::Fov => (MIN_FOV_DEGREES, MAX_FOV_DEGREES),
            SettingsSlider::RenderDistance => {
                (MIN_RENDER_DISTANCE as f32, MAX_RENDER_DISTANCE as f32)
            }
            SettingsSlider::Exposure => (-2.0, 2.0),
            SettingsSlider::Saturation => (0.5, 1.5),
            SettingsSlider::Contrast => (0.8, 1.3),
            SettingsSlider::Temperature => (-0.5, 0.5),
            SettingsSlider::WindStrength => (0.0, 0.2),
        }
    }

    fn step(self) -> f32 {
        match self {
            SettingsSlider::Sensitivity
            | SettingsSlider::Saturation
            | SettingsSlider::Contrast
            | SettingsSlider::Temperature
            | SettingsSlider::WindStrength => 0.05,
            SettingsSlider::Exposure => 0.1,
            SettingsSlider::Fov | SettingsSlider::RenderDistance => 1.0,
        }
    }

    fn precision(self) -> i32 {
        match self {
            SettingsSlider::Sensitivity
            | SettingsSlider::Exposure
            | SettingsSlider::Saturation
            | SettingsSlider::Contrast
            | SettingsSlider::Temperature
            | SettingsSlider::WindStrength => 2,
            SettingsSlider::Fov | SettingsSlider::RenderDistance => 0,
        }
    }

    fn value(self, settings: &GameSettings) -> f32 {
        match self {
            SettingsSlider::Sensitivity => settings.mouse_sensitivity / DEFAULT_MOUSE_SENSITIVITY,
            SettingsSlider::Fov => settings.fov_degrees,
            SettingsSlider::RenderDistance => settings.render_distance as f32,
            SettingsSlider::Exposure => settings.exposure_ev,
            SettingsSlider::Saturation => settings.saturation,
            SettingsSlider::Contrast => settings.contrast,
            SettingsSlider::Temperature => settings.temperature,
            SettingsSlider::WindStrength => settings.wind_strength,
        }
    }
}

/// Marks the thumb of a settings slider so it can follow the value.
#[derive(Component)]
struct SettingsThumb;

/// Sidebar tab button, styled from the active tab resource.
#[derive(Component, Clone, Copy)]
struct SettingsTabButton(SettingsTab);

/// A keybind card button; highlighted while it is waiting for a key.
#[derive(Component, Clone, Copy)]
struct BindingButton(BindingAction);

/// Text showing the key assigned to a rebindable action.
#[derive(Component, Clone, Copy)]
struct BindingValue(BindingAction);

type TabButtonQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static SettingsTabButton,
        &'static Hovered,
        Has<Pressed>,
        &'static mut BackgroundColor,
        &'static mut BorderColor,
    ),
    With<MenuButton>,
>;

type BindingButtonQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static BindingButton,
        &'static Hovered,
        Has<Pressed>,
        &'static mut BackgroundColor,
        &'static mut BorderColor,
    ),
    With<MenuButton>,
>;

/// Text that displays a setting's current value.
#[derive(Component, Clone, Copy, Debug)]
enum SettingsValue {
    Sensitivity,
    Fov,
    RenderDistance,
    Shadows,
    Vsync,
    Fullscreen,
    PerfLog,
    Clouds,
    LightShaftsMode,
    Tonemapping,
    Exposure,
    Saturation,
    Contrast,
    Temperature,
    WindStrength,
    Ssao,
    Bloom,
}

/// The RAM hint under the render-distance slider.
#[derive(Component)]
struct SettingsHint;

pub struct SettingsMenuPlugin;

impl Plugin for SettingsMenuPlugin {
    fn build(&self, app: &mut App) {
        // `DefaultPlugins` already installs `UiWidgetsPlugins`, which includes
        // `ButtonPlugin` and `SliderPlugin`; adding them again panics at startup.
        app.init_resource::<SettingsTab>()
            .add_systems(Startup, setup_menus)
            .add_systems(
                Update,
                (
                    sync_menu_visibility,
                    sync_tab_panels,
                    capture_keybind,
                    sync_slider_values,
                    sync_slider_thumbs,
                    update_values,
                    update_binding_values,
                    update_ram_hint,
                    style_buttons,
                    style_tab_buttons,
                    style_binding_buttons,
                    style_slider_thumbs,
                )
                    .chain()
                    .in_set(SettingsMenuSystems),
            )
            .add_observer(on_activate)
            .add_observer(on_slider_change)
            // The slider is intentionally uncontrolled: this observer writes the
            // emitted value back into `SliderValue`.
            .add_observer(slider_self_update);
    }
}

fn setup_menus(
    mut commands: Commands,
    settings: Res<GameSettings>,
    bindings: Res<InputBindings>,
    tab: Res<SettingsTab>,
    theme: Res<UiTheme>,
) {
    setup_pause_menu(&mut commands, &theme);
    setup_settings_menu(&mut commands, &settings, &bindings, *tab, &theme);
}

fn setup_pause_menu(commands: &mut Commands, theme: &UiTheme) {
    let root = commands
        .spawn((
            PauseMenuRoot,
            Name::new("PauseMenu"),
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
            GlobalZIndex(theme.layers.pause),
        ))
        .id();

    let panel = spawn_panel(
        commands,
        theme,
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
        commands,
        theme,
        panel,
        "PAUSED",
        theme.fonts.heading,
        theme.colors.text,
        StaticLabel,
    );
    spawn_text(
        commands,
        theme,
        panel,
        "The world is paused while this menu is open",
        theme.fonts.small,
        theme.colors.muted_text,
        StaticLabel,
    );

    spawn_menu_button(
        commands,
        panel,
        "Resume",
        210.0,
        42.0,
        SettingsAction::Resume,
        theme,
    );
    spawn_menu_button(
        commands,
        panel,
        "Settings",
        210.0,
        42.0,
        SettingsAction::OpenSettings,
        theme,
    );
    spawn_menu_button(
        commands,
        panel,
        "Quit",
        210.0,
        42.0,
        SettingsAction::Quit,
        theme,
    );

    spawn_text(
        commands,
        theme,
        panel,
        "Esc resumes",
        theme.fonts.small,
        theme.colors.muted_text,
        StaticLabel,
    );
}

fn setup_settings_menu(
    commands: &mut Commands,
    settings: &GameSettings,
    bindings: &InputBindings,
    active_tab: SettingsTab,
    theme: &UiTheme,
) {
    let root = commands
        .spawn((
            SettingsMenuRoot,
            Name::new("SettingsMenu"),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::None,
                // Bevy defaults to a row. Without an explicit column, the
                // header/body/footer are laid out horizontally and the body is
                // squeezed off screen, leaving only the header text visible.
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
                border_radius: BorderRadius::all(Val::Px(0.0)),
                ..default()
            },
            BackgroundColor(theme.colors.settings_panel_bg),
            GlobalZIndex(theme.layers.settings),
        ))
        .id();

    let header = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(78.0),
                padding: UiRect::axes(
                    Val::Px(SETTINGS_PADDING_X),
                    Val::Px(SETTINGS_PADDING_Y * 0.65),
                ),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(3.0),
                flex_shrink: 0.0,
                ..default()
            },
            BackgroundColor(theme.colors.panel_bg_strong),
            ChildOf(root),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        header,
        "SETTINGS",
        theme.fonts.title,
        theme.colors.text,
        StaticLabel,
    );
    spawn_text(
        commands,
        theme,
        header,
        "Tune the game to your setup. Changes apply immediately.",
        theme.fonts.small,
        theme.colors.muted_text,
        StaticLabel,
    );

    let body = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(0.0),
                flex_grow: 1.0,
                flex_direction: FlexDirection::Row,
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    let sidebar = commands
        .spawn((
            Node {
                width: Val::Px(SETTINGS_SIDEBAR_WIDTH),
                min_width: Val::Px(SETTINGS_SIDEBAR_WIDTH),
                padding: UiRect {
                    left: Val::Px(SETTINGS_PADDING_X),
                    right: Val::Px(16.0),
                    top: Val::Px(18.0),
                    bottom: Val::Px(18.0),
                },
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                border: UiRect {
                    right: Val::Px(1.0),
                    ..default()
                },
                ..default()
            },
            BackgroundColor(theme.colors.panel_bg_soft),
            BorderColor::all(theme.colors.faint_border),
            ChildOf(body),
        ))
        .id();

    spawn_text(
        commands,
        theme,
        sidebar,
        "CATEGORIES",
        theme.fonts.micro,
        theme.colors.muted_text,
        StaticLabel,
    );
    for kind in SettingsTab::ALL {
        let button = spawn_menu_button(
            commands,
            sidebar,
            kind.label(),
            SETTINGS_TAB_WIDTH,
            38.0,
            SettingsAction::SelectTab(kind),
            theme,
        );
        commands.entity(button).insert(SettingsTabButton(kind));
    }

    let content = commands
        .spawn((
            Node {
                min_width: Val::Px(0.0),
                max_width: Val::Px(SETTINGS_CONTENT_WIDTH),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                padding: UiRect {
                    left: Val::Px(28.0),
                    right: Val::Px(SETTINGS_PADDING_X),
                    top: Val::Px(20.0),
                    bottom: Val::Px(20.0),
                },
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(body),
        ))
        .id();

    let graphics = spawn_tab_panel(commands, content, SettingsTab::Graphics, active_tab);
    setup_graphics_tab(commands, graphics, settings, theme);

    let lighting = spawn_tab_panel(commands, content, SettingsTab::Lighting, active_tab);
    setup_lighting_tab(commands, lighting, settings, theme);

    let color = spawn_tab_panel(commands, content, SettingsTab::Color, active_tab);
    setup_color_tab(commands, color, settings, theme);

    let mouse = spawn_tab_panel(commands, content, SettingsTab::Mouse, active_tab);
    setup_mouse_tab(commands, mouse, settings, theme);

    let keybinds = spawn_tab_panel(commands, content, SettingsTab::Keybinds, active_tab);
    setup_keybinds_tab(commands, keybinds, bindings, theme);

    let audio = spawn_tab_panel(commands, content, SettingsTab::Audio, active_tab);
    setup_audio_tab(commands, audio, theme);

    let footer = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(62.0),
                padding: UiRect::axes(
                    Val::Px(SETTINGS_PADDING_X),
                    Val::Px(SETTINGS_PADDING_Y * 0.6),
                ),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(10.0),
                flex_shrink: 0.0,
                border: UiRect {
                    top: Val::Px(1.0),
                    ..default()
                },
                ..default()
            },
            BackgroundColor(theme.colors.panel_bg_strong),
            BorderColor::all(theme.colors.faint_border),
            ChildOf(root),
        ))
        .id();

    spawn_text(
        commands,
        theme,
        footer,
        "Esc returns to the pause menu",
        theme.fonts.small,
        theme.colors.muted_text,
        StaticLabel,
    );

    let actions = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                ..default()
            },
            ChildOf(footer),
        ))
        .id();

    spawn_menu_button(
        commands,
        actions,
        "Back",
        120.0,
        32.0,
        SettingsAction::Back,
        theme,
    );
    spawn_menu_button(
        commands,
        actions,
        "Reset defaults",
        150.0,
        32.0,
        SettingsAction::ResetDefaults,
        theme,
    );
    spawn_menu_button(
        commands,
        actions,
        "Resume",
        130.0,
        32.0,
        SettingsAction::Resume,
        theme,
    );
}

fn spawn_tab_panel(
    commands: &mut Commands,
    parent: Entity,
    kind: SettingsTab,
    active_tab: SettingsTab,
) -> Entity {
    commands
        .spawn((
            TabPanel(kind),
            Node {
                width: Val::Percent(100.0),
                display: display_if(kind == active_tab),
                min_height: Val::Px(0.0),
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(7.0),
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(parent),
        ))
        .id()
}

fn setup_graphics_tab(
    commands: &mut Commands,
    tab: Entity,
    settings: &GameSettings,
    theme: &UiTheme,
) {
    spawn_section_heading(commands, tab, "VIEW", theme);
    spawn_slider_row(
        commands,
        tab,
        "Field of view",
        SettingsSlider::Fov,
        SettingsValue::Fov,
        settings,
        theme,
    );
    spawn_slider_row(
        commands,
        tab,
        "View distance",
        SettingsSlider::RenderDistance,
        SettingsValue::RenderDistance,
        settings,
        theme,
    );
    let hint = spawn_text(
        commands,
        theme,
        tab,
        "",
        theme.fonts.slot_count,
        theme.colors.muted_text,
        SettingsHint,
    );
    commands.entity(hint).insert((
        TextLayout::justify(Justify::Left),
        Node {
            width: Val::Percent(100.0),
            max_width: Val::Px(SETTINGS_ROW_WIDTH),
            flex_shrink: 0.0,
            ..default()
        },
    ));

    spawn_section_heading(commands, tab, "DISPLAY", theme);
    spawn_toggle_row(
        commands,
        tab,
        "VSync",
        "Off",
        90.0,
        SettingsAction::ToggleVsync,
        SettingsValue::Vsync,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "Fullscreen",
        "On",
        90.0,
        SettingsAction::ToggleFullscreen,
        SettingsValue::Fullscreen,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "Perf log (1 Hz)",
        "Off",
        90.0,
        SettingsAction::TogglePerfLog,
        SettingsValue::PerfLog,
        theme,
    );
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        spawn_note(
            commands,
            tab,
            "Wayland syncs every frame to the monitor, so VSync Off is ignored here.",
            theme.fonts.small,
            theme.colors.muted_text,
            theme,
        );
    }

    spawn_section_heading(commands, tab, "WORLD", theme);
    spawn_toggle_row(
        commands,
        tab,
        "Clouds",
        "On",
        90.0,
        SettingsAction::ToggleClouds,
        SettingsValue::Clouds,
        theme,
    );
    spawn_slider_row(
        commands,
        tab,
        "Wind strength",
        SettingsSlider::WindStrength,
        SettingsValue::WindStrength,
        settings,
        theme,
    );
}

fn setup_lighting_tab(
    commands: &mut Commands,
    tab: Entity,
    settings: &GameSettings,
    theme: &UiTheme,
) {
    spawn_section_heading(commands, tab, "SHADOWS & OCCLUSION", theme);
    spawn_toggle_row(
        commands,
        tab,
        "Shadow quality",
        settings.shadow_quality.label(),
        130.0,
        SettingsAction::CycleShadows,
        SettingsValue::Shadows,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "SSAO",
        "Unavailable",
        140.0,
        SettingsAction::CycleSsao,
        SettingsValue::Ssao,
        theme,
    );
    spawn_note(
        commands,
        tab,
        "SSAO is unavailable in this build due an unresolved rendering regression. The setting is preserved but cannot be activated.",
        theme.fonts.small,
        theme.colors.muted_text,
        theme,
    );

    spawn_section_heading(commands, tab, "SUN & ATMOSPHERE", theme);
    spawn_toggle_row(
        commands,
        tab,
        "Light shafts",
        "Simple",
        180.0,
        SettingsAction::CycleLightShafts,
        SettingsValue::LightShaftsMode,
        theme,
    );
    spawn_note(
        commands,
        tab,
        "Simple shafts work without a depth prepass. Detailed uses depth-prepass occlusion, forces MSAA Off, and may show artifacts around transparent effects.",
        theme.fonts.small,
        theme.colors.muted_text,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "Bloom",
        "Low",
        120.0,
        SettingsAction::CycleBloom,
        SettingsValue::Bloom,
        theme,
    );
}

fn setup_color_tab(commands: &mut Commands, tab: Entity, settings: &GameSettings, theme: &UiTheme) {
    spawn_section_heading(commands, tab, "TONE MAPPING", theme);
    spawn_toggle_row(
        commands,
        tab,
        "Tonemapping",
        "TonyMcMapface",
        170.0,
        SettingsAction::CycleTonemapping,
        SettingsValue::Tonemapping,
        theme,
    );

    spawn_section_heading(commands, tab, "COLOR GRADING", theme);
    spawn_slider_row(
        commands,
        tab,
        "Exposure",
        SettingsSlider::Exposure,
        SettingsValue::Exposure,
        settings,
        theme,
    );
    spawn_slider_row(
        commands,
        tab,
        "Saturation",
        SettingsSlider::Saturation,
        SettingsValue::Saturation,
        settings,
        theme,
    );
    spawn_slider_row(
        commands,
        tab,
        "Contrast",
        SettingsSlider::Contrast,
        SettingsValue::Contrast,
        settings,
        theme,
    );
    spawn_slider_row(
        commands,
        tab,
        "Temperature",
        SettingsSlider::Temperature,
        SettingsValue::Temperature,
        settings,
        theme,
    );
}

fn setup_audio_tab(commands: &mut Commands, tab: Entity, theme: &UiTheme) {
    spawn_section_heading(commands, tab, "AUDIO", theme);
    spawn_note(
        commands,
        tab,
        "Audio settings are not available in this build yet.",
        theme.fonts.body,
        theme.colors.muted_text,
        theme,
    );
}

fn setup_mouse_tab(commands: &mut Commands, tab: Entity, settings: &GameSettings, theme: &UiTheme) {
    spawn_section_heading(commands, tab, "LOOK", theme);
    spawn_slider_row(
        commands,
        tab,
        "Sensitivity",
        SettingsSlider::Sensitivity,
        SettingsValue::Sensitivity,
        settings,
        theme,
    );
    spawn_note(
        commands,
        tab,
        "Lower values turn slowly; higher values turn quickly.",
        theme.fonts.small,
        theme.colors.muted_text,
        theme,
    );
}

fn setup_keybinds_tab(
    commands: &mut Commands,
    tab: Entity,
    bindings: &InputBindings,
    theme: &UiTheme,
) {
    spawn_note(
        commands,
        tab,
        "Click a key to rebind it. Esc cancels without changing the binding. Taking an assigned key swaps the two actions. Rebindings last for this session.",
        theme.fonts.small,
        theme.colors.muted_text,
        theme,
    );

    let grid = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: Val::Px(10.0),
                row_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(tab),
        ))
        .id();

    spawn_section_heading(commands, grid, "MOVEMENT", theme);
    for action in [
        BindingAction::MoveForward,
        BindingAction::MoveBack,
        BindingAction::MoveLeft,
        BindingAction::MoveRight,
        BindingAction::Jump,
        BindingAction::Sprint,
        BindingAction::Crouch,
    ] {
        spawn_keybind_row(commands, grid, action, bindings, theme);
    }

    spawn_section_heading(commands, grid, "INTERFACE", theme);
    for action in [
        BindingAction::Chat,
        BindingAction::Pause,
        BindingAction::Help,
        BindingAction::DebugOverlay,
        BindingAction::Backpack,
        BindingAction::Map,
        BindingAction::ToggleHud,
    ] {
        spawn_keybind_row(commands, grid, action, bindings, theme);
    }

    spawn_section_heading(commands, grid, "WORLD", theme);
    for action in [
        BindingAction::RotatePlace,
        BindingAction::Save,
        BindingAction::Load,
        BindingAction::Screenshot,
    ] {
        spawn_keybind_row(commands, grid, action, bindings, theme);
    }

    spawn_note(
        commands,
        tab,
        "RMB places and interacts. Mouse buttons and hotbar slots are not rebindable yet.",
        theme.fonts.slot_count,
        theme.colors.muted_text,
        theme,
    );
}

fn spawn_menu_button(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    width: f32,
    height: f32,
    action: SettingsAction,
    theme: &UiTheme,
) -> Entity {
    let button = commands
        .spawn((
            Button,
            MenuButton,
            action,
            Node {
                width: Val::Px(width),
                height: Val::Px(height),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            BackgroundColor(theme.colors.button_bg),
            BorderColor::all(theme.colors.border_idle),
            Hovered::default(),
            ChildOf(parent),
        ))
        .id();

    spawn_text(
        commands,
        theme,
        button,
        label,
        theme.fonts.compass,
        theme.colors.text,
        StaticLabel,
    );
    button
}

fn spawn_section_heading(commands: &mut Commands, parent: Entity, label: &str, theme: &UiTheme) {
    let heading = spawn_text(
        commands,
        theme,
        parent,
        label,
        theme.fonts.compass,
        theme.colors.accent,
        StaticLabel,
    );
    commands.entity(heading).insert((
        TextLayout::justify(Justify::Left),
        Node {
            width: Val::Percent(100.0),
            max_width: Val::Px(SETTINGS_ROW_WIDTH),
            flex_shrink: 0.0,
            margin: UiRect::top(Val::Px(6.0)),
            ..default()
        },
    ));
}

fn spawn_slider_row(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    slider_kind: SettingsSlider,
    value_kind: SettingsValue,
    settings: &GameSettings,
    theme: &UiTheme,
) {
    let row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                max_width: Val::Px(SETTINGS_ROW_WIDTH),
                min_height: Val::Px(32.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(10.0),
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
        theme.fonts.label,
        theme.colors.text,
        StaticLabel,
    );

    let (start, end) = slider_kind.range();
    let value = slider_kind.value(settings).clamp(start, end);

    let slider = commands
        .spawn((
            Slider {
                track_click: TrackClick::Snap,
                orientation: SliderOrientation::Horizontal,
            },
            slider_kind,
            SliderValue(value),
            SliderRange::new(start, end),
            SliderStep(slider_kind.step()),
            SliderPrecision(slider_kind.precision()),
            Node {
                width: Val::Px(SLIDER_WIDTH),
                min_width: Val::Px(SLIDER_WIDTH),
                height: Val::Px(SLIDER_HEIGHT),
                align_items: AlignItems::Center,
                border_radius: theme.shape.rounded(),
                ..default()
            },
            BackgroundColor(theme.colors.slider_track),
            ChildOf(row),
        ))
        .id();

    commands.spawn((
        SliderThumb,
        SettingsThumb,
        Hovered::default(),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px((SLIDER_HEIGHT - THUMB_SIZE) * 0.5),
            width: Val::Px(THUMB_SIZE),
            height: Val::Px(THUMB_SIZE),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        BackgroundColor(theme.colors.slider_thumb),
        ChildOf(slider),
    ));

    let value_text = spawn_text(
        commands,
        theme,
        row,
        "",
        theme.fonts.compass,
        theme.colors.text,
        value_kind,
    );
    commands.entity(value_text).insert((
        TextLayout::justify(Justify::Right),
        Node {
            width: Val::Px(VALUE_WIDTH),
            ..default()
        },
    ));
}

#[allow(clippy::too_many_arguments)]
fn spawn_toggle_row(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    value: &str,
    width: f32,
    action: SettingsAction,
    value_kind: SettingsValue,
    theme: &UiTheme,
) {
    let row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                max_width: Val::Px(SETTINGS_ROW_WIDTH),
                min_height: Val::Px(32.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(10.0),
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
        theme.fonts.label,
        theme.colors.text,
        StaticLabel,
    );

    let button = commands
        .spawn((
            Button,
            MenuButton,
            action,
            Node {
                width: Val::Px(width),
                height: Val::Px(30.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                ..default()
            },
            BackgroundColor(theme.colors.button_bg),
            BorderColor::all(theme.colors.border_idle),
            Hovered::default(),
            ChildOf(row),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        button,
        value,
        theme.fonts.compass,
        theme.colors.text,
        value_kind,
    );
}

fn spawn_keybind_row(
    commands: &mut Commands,
    parent: Entity,
    action: BindingAction,
    bindings: &InputBindings,
    theme: &UiTheme,
) {
    let card = commands
        .spawn((
            Button,
            MenuButton,
            BindingButton(action),
            SettingsAction::Rebind(action),
            Node {
                width: Val::Px(KEYBIND_CARD_WIDTH),
                height: Val::Px(34.0),
                flex_shrink: 0.0,
                padding: UiRect::axes(Val::Px(10.0), Val::Px(0.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(8.0),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded_small(),
                ..default()
            },
            BackgroundColor(theme.colors.button_bg),
            BorderColor::all(theme.colors.border_idle),
            Hovered::default(),
            ChildOf(parent),
        ))
        .id();

    spawn_text(
        commands,
        theme,
        card,
        action.label(),
        theme.fonts.body,
        theme.colors.text,
        StaticLabel,
    );
    let key = spawn_text(
        commands,
        theme,
        card,
        key_label(action.get(bindings)),
        theme.fonts.keycap,
        theme.colors.accent_soft,
        BindingValue(action),
    );
    commands.entity(key).insert((
        TextLayout::justify(Justify::Right),
        Node {
            width: Val::Px(88.0),
            ..default()
        },
    ));
}

/// Static text that may need to wrap inside a narrow tab.
fn spawn_note(
    commands: &mut Commands,
    parent: Entity,
    content: impl Into<String>,
    size: f32,
    color: Color,
    theme: &UiTheme,
) {
    let entity = spawn_text(commands, theme, parent, content, size, color, StaticLabel);
    commands.entity(entity).insert((
        TextLayout::justify(Justify::Left),
        Node {
            width: Val::Percent(100.0),
            max_width: Val::Px(SETTINGS_ROW_WIDTH),
            flex_shrink: 0.0,
            ..default()
        },
    ));
}

fn sync_menu_visibility(
    ui: Res<UiManager>,
    mut pause: Query<&mut Node, (With<PauseMenuRoot>, Without<SettingsMenuRoot>)>,
    mut settings: Query<&mut Node, (With<SettingsMenuRoot>, Without<PauseMenuRoot>)>,
) {
    let pause_display = display_if(ui.is_open(Overlay::Menu));
    for mut node in &mut pause {
        if node.display != pause_display {
            node.display = pause_display;
        }
    }

    let settings_display = display_if(ui.is_open(Overlay::Settings));
    for mut node in &mut settings {
        if node.display != settings_display {
            node.display = settings_display;
        }
    }
}

fn sync_tab_panels(tab: Res<SettingsTab>, mut panels: Query<(&TabPanel, &mut Node)>) {
    for (panel, mut node) in &mut panels {
        let desired = display_if(panel.0 == *tab);
        if node.display != desired {
            node.display = desired;
        }
    }
}

fn capture_keybind(
    keys: Res<ButtonInput<KeyCode>>,
    ui: Res<UiManager>,
    mut capture: ResMut<KeybindCapture>,
    mut bindings: ResMut<InputBindings>,
) {
    if !ui.is_open(Overlay::Settings) {
        if capture.0.is_some() {
            capture.0 = None;
        }
        return;
    }

    let Some(action) = capture.0 else {
        return;
    };

    if keys.just_pressed(KeyCode::Escape) {
        capture.0 = None;
        return;
    }

    if let Some(key) = keys.get_just_pressed().next().copied() {
        action.set(&mut bindings, key);
        capture.0 = None;
    }
}

fn update_binding_values(
    bindings: Res<InputBindings>,
    capture: Res<KeybindCapture>,
    theme: Res<UiTheme>,
    mut labels: Query<(&BindingValue, &mut Text, &mut TextColor)>,
) {
    if !bindings.is_changed() && !capture.is_changed() {
        return;
    }

    for (binding, mut text, mut color) in &mut labels {
        let waiting = capture.0 == Some(binding.0);
        let desired = if waiting {
            "PRESS KEY".to_string()
        } else {
            key_label(binding.0.get(&bindings))
        };
        if text.0 != desired {
            text.0 = desired;
        }
        let desired_color = if waiting {
            theme.colors.accent_soft
        } else {
            theme.colors.text
        };
        if color.0 != desired_color {
            color.0 = desired_color;
        }
    }
}

fn style_tab_buttons(
    tab: Res<SettingsTab>,
    theme: Res<UiTheme>,
    mut buttons: TabButtonQuery<'_, '_>,
) {
    for (button, hovered, pressed, mut background, mut border) in &mut buttons {
        let active = button.0 == *tab;
        let desired_background = if active {
            theme.colors.selected_bg
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

        let desired_border = if active {
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

fn style_binding_buttons(
    capture: Res<KeybindCapture>,
    theme: Res<UiTheme>,
    mut buttons: BindingButtonQuery<'_, '_>,
) {
    for (button, hovered, pressed, mut background, mut border) in &mut buttons {
        if capture.0 != Some(button.0) {
            continue;
        }
        if background.0 != theme.colors.button_pressed {
            background.0 = theme.colors.button_pressed;
        }
        if border.top != theme.colors.border_active {
            *border = BorderColor::all(theme.colors.border_active);
        }
        let _ = (hovered, pressed);
    }
}

fn sync_slider_values(
    settings: Res<GameSettings>,
    sliders: Query<(Entity, &SettingsSlider, &SliderValue)>,
    mut commands: Commands,
) {
    if !settings.is_changed() {
        return;
    }

    for (entity, kind, value) in &sliders {
        let desired = kind.value(&settings);
        if (value.0 - desired).abs() > 1e-4 {
            commands.entity(entity).insert(SliderValue(desired));
        }
    }
}

fn sync_slider_thumbs(
    sliders: Query<(&SliderValue, &SliderRange, &ComputedNode, &Children)>,
    mut thumbs: Query<&mut Node, With<SettingsThumb>>,
) {
    for (value, range, slider_node, children) in &sliders {
        let travel = (slider_node.size().x - THUMB_SIZE).max(0.0);
        let left = Val::Px(range.thumb_position(value.0) * travel);

        for child in children.iter() {
            if let Ok(mut node) = thumbs.get_mut(child)
                && node.left != left
            {
                node.left = left;
            }
        }
    }
}

fn update_values(settings: Res<GameSettings>, mut labels: Query<(&SettingsValue, &mut Text)>) {
    if !settings.is_changed() {
        return;
    }
    for (value, mut text) in &mut labels {
        let desired = match *value {
            SettingsValue::Sensitivity => {
                let multiplier = settings.mouse_sensitivity / DEFAULT_MOUSE_SENSITIVITY;
                format!("{multiplier:.1}x")
            }
            SettingsValue::Fov => format!("{:.0} deg", settings.fov_degrees),
            SettingsValue::RenderDistance => format!("{} chunks", settings.render_distance),
            SettingsValue::Shadows => settings.shadow_quality.label().to_string(),
            SettingsValue::Vsync => on_off(settings.vsync),
            SettingsValue::Fullscreen => on_off(settings.fullscreen),
            SettingsValue::PerfLog => on_off(settings.perf_log),
            SettingsValue::Clouds => on_off(settings.clouds_enabled),
            SettingsValue::LightShaftsMode => match settings.light_shafts_mode {
                crate::light_shafts::LightShaftsMode::Off => "Off".to_string(),
                crate::light_shafts::LightShaftsMode::Simple => "Simple".to_string(),
                crate::light_shafts::LightShaftsMode::Detailed => "Detailed".to_string(),
            },
            SettingsValue::Tonemapping => settings.tonemapping.label().to_string(),
            SettingsValue::Exposure => format!("{:+.2} EV", settings.exposure_ev),
            SettingsValue::Saturation => format!("{:.2}", settings.saturation),
            SettingsValue::Contrast => format!("{:.2}", settings.contrast),
            SettingsValue::Temperature => format!("{:+.2}", settings.temperature),
            SettingsValue::WindStrength => format!("{:.3}", settings.wind_strength),
            SettingsValue::Ssao => settings.ssao.label().to_string(),
            SettingsValue::Bloom => settings.bloom.label().to_string(),
        };
        if text.0 != desired {
            text.0 = desired;
        }
    }
}

fn update_ram_hint(settings: Res<GameSettings>, mut hints: Query<&mut Text, With<SettingsHint>>) {
    if !settings.is_changed() {
        return;
    }

    let memory = estimated_chunk_memory_mb(settings.render_distance);
    let desired = if memory >= 1024.0 {
        format!(
            "World voxel data: about {:.1} GB. Large view distances need a lot of RAM.",
            memory / 1024.0
        )
    } else {
        format!(
            "World voxel data: about {:.0} MB. Higher view distances use more RAM.",
            memory
        )
    };

    for mut text in &mut hints {
        if text.0 != desired {
            text.0 = desired.clone();
        }
    }
}

fn style_buttons(
    theme: Res<UiTheme>,
    mut buttons: Query<
        (
            &Hovered,
            Has<Pressed>,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        With<MenuButton>,
    >,
) {
    for (hovered, pressed, mut background, mut border) in &mut buttons {
        let desired = if pressed {
            theme.colors.button_pressed
        } else if hovered.0 {
            theme.colors.button_hover
        } else {
            theme.colors.button_bg
        };
        if background.0 != desired {
            background.0 = desired;
        }

        let desired_border = if pressed {
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

fn style_slider_thumbs(
    theme: Res<UiTheme>,
    mut thumbs: Query<(&Hovered, &mut BackgroundColor), With<SettingsThumb>>,
) {
    for (hovered, mut background) in &mut thumbs {
        let desired = if hovered.0 {
            theme.colors.slider_thumb_hover
        } else {
            theme.colors.slider_thumb
        };
        if background.0 != desired {
            background.0 = desired;
        }
    }
}

#[inline]
fn on_off(value: bool) -> String {
    if value { "On" } else { "Off" }.to_string()
}

fn on_slider_change(
    change: On<ValueChange<f32>>,
    sliders: Query<&SettingsSlider>,
    mut settings: ResMut<GameSettings>,
) {
    let Ok(kind) = sliders.get(change.source) else {
        return;
    };

    match kind {
        SettingsSlider::Sensitivity => {
            settings.mouse_sensitivity = change.value * DEFAULT_MOUSE_SENSITIVITY;
        }
        SettingsSlider::Fov => {
            settings.fov_degrees = change.value.clamp(MIN_FOV_DEGREES, MAX_FOV_DEGREES);
        }
        SettingsSlider::RenderDistance => {
            settings.render_distance =
                (change.value.round() as i32).clamp(MIN_RENDER_DISTANCE, MAX_RENDER_DISTANCE);
        }
        SettingsSlider::Exposure => settings.exposure_ev = change.value.clamp(-2.0, 2.0),
        SettingsSlider::Saturation => settings.saturation = change.value.clamp(0.5, 1.5),
        SettingsSlider::Contrast => settings.contrast = change.value.clamp(0.8, 1.3),
        SettingsSlider::Temperature => settings.temperature = change.value.clamp(-0.5, 0.5),
        SettingsSlider::WindStrength => settings.wind_strength = change.value.clamp(0.0, 0.2),
    }
}

#[derive(SystemParam)]
struct SettingsActionState<'w, 's> {
    actions: Query<'w, 's, &'static SettingsAction>,
    settings: ResMut<'w, GameSettings>,
    bindings: ResMut<'w, InputBindings>,
    capture: ResMut<'w, KeybindCapture>,
    tab: ResMut<'w, SettingsTab>,
    menu: ResMut<'w, MenuState>,
    ui: ResMut<'w, UiManager>,
    commands: Commands<'w, 's>,
}

fn on_activate(activate: On<Activate>, state: SettingsActionState<'_, '_>) {
    let SettingsActionState {
        actions,
        mut settings,
        mut bindings,
        mut capture,
        mut tab,
        mut menu,
        mut ui,
        mut commands,
    } = state;
    let Ok(action) = actions.get(activate.entity) else {
        return;
    };

    match *action {
        SettingsAction::ToggleVsync => {
            settings.vsync = !settings.vsync;
        }
        SettingsAction::ToggleFullscreen => {
            settings.fullscreen = !settings.fullscreen;
        }
        SettingsAction::TogglePerfLog => {
            settings.perf_log = !settings.perf_log;
        }
        SettingsAction::ToggleClouds => {
            settings.clouds_enabled = !settings.clouds_enabled;
        }
        SettingsAction::CycleLightShafts => {
            settings.light_shafts_mode = settings.light_shafts_mode.next();
        }
        SettingsAction::CycleTonemapping => {
            settings.tonemapping = settings.tonemapping.next();
        }
        SettingsAction::CycleSsao => {
            settings.ssao = settings.ssao.next();
        }
        SettingsAction::CycleBloom => {
            settings.bloom = settings.bloom.next();
        }
        SettingsAction::CycleShadows => {
            settings.shadow_quality = settings.shadow_quality.next();
        }
        SettingsAction::Rebind(action) => {
            capture.0 = Some(action);
        }
        SettingsAction::ResetDefaults => {
            *settings = GameSettings::default();
            *bindings = InputBindings::default();
            capture.0 = None;
        }
        SettingsAction::Resume => {
            menu.resume_requested = true;
        }
        SettingsAction::OpenSettings => {
            ui.open(Overlay::Settings);
        }
        SettingsAction::Back => {
            ui.open(Overlay::Menu);
        }
        SettingsAction::Quit => {
            commands.write_message(AppExit::Success);
        }
        SettingsAction::SelectTab(selected) => {
            *tab = selected;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_labels_are_short() {
        assert_eq!(SettingsTab::Graphics.label(), "Graphics");
        assert_eq!(SettingsTab::Lighting.label(), "Lighting");
        assert_eq!(SettingsTab::Color.label(), "Color");
        assert_eq!(SettingsTab::Keybinds.label(), "Keybinds");
        assert_eq!(SettingsTab::Audio.label(), "Audio");
        assert_eq!(SettingsTab::ALL.len(), 6);
    }

    #[test]
    fn toggle_labels_are_compact() {
        assert_eq!(on_off(true), "On");
        assert_eq!(on_off(false), "Off");
    }

    #[test]
    fn settings_layout_stays_full_screen() {
        const {
            assert!(SETTINGS_PADDING_X > 0.0);
            assert!(SETTINGS_PADDING_Y > 0.0);
            assert!(SETTINGS_SIDEBAR_WIDTH > SETTINGS_TAB_WIDTH);
            assert!(SETTINGS_ROW_WIDTH > SLIDER_WIDTH);
            assert!(SETTINGS_CONTENT_WIDTH > SETTINGS_ROW_WIDTH);
            assert!(KEYBIND_CARD_WIDTH > 200.0);
        }
    }
}
