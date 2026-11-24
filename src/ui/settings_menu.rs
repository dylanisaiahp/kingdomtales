//! Pause menu and tabbed settings screen.
//!
//! Esc opens a small vertical pause menu, matching the hammer shape menu's
//! presentation. "Settings" opens this module's tabbed panel; the pause menu
//! itself only offers Resume, Settings and Quit. All buttons use Bevy 0.20's
//! `Button` widget with an `Activate` observer, while numeric settings use the
//! `Slider` widget. Audio and Map tabs are deliberately reserved UI placeholders.

use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::ComputedNode;
use bevy::ui::Pressed;
use bevy::ui_widgets::{
    Activate, Button, Slider, SliderOrientation, SliderPrecision, SliderRange, SliderStep,
    SliderThumb, SliderValue, TrackClick, ValueChange, slider_self_update,
};

use crate::settings::{
    DEFAULT_MOUSE_SENSITIVITY, GameSettings, InputBindings, MAX_FOV_DEGREES, MAX_RENDER_DISTANCE,
    MAX_SENSITIVITY_MULTIPLIER, MIN_FOV_DEGREES, MIN_RENDER_DISTANCE, MIN_SENSITIVITY_MULTIPLIER,
    MenuState, estimated_chunk_memory_mb,
};
use crate::ui::theme::{UiTheme, display_if, key_label, spawn_keycap, spawn_panel, spawn_text};
use crate::ui_manager::{Overlay, UiManager};

const SLIDER_WIDTH: f32 = 190.0;
const SLIDER_HEIGHT: f32 = 20.0;
const THUMB_SIZE: f32 = 14.0;
const VALUE_WIDTH: f32 = 78.0;
const TAB_WIDTH: f32 = 116.0;
const SETTINGS_PADDING_X: f32 = 36.0;
const SETTINGS_PADDING_Y: f32 = 24.0;
const SETTINGS_CONTENT_WIDTH: f32 = 760.0;

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
    Video,
    Audio,
    Mouse,
    Controls,
    Map,
}

impl SettingsTab {
    const ALL: [Self; 5] = [
        Self::Video,
        Self::Audio,
        Self::Mouse,
        Self::Controls,
        Self::Map,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Mouse => "Mouse",
            Self::Controls => "Controls",
            Self::Map => "Map",
        }
    }
}

#[derive(Component, Clone, Copy, Debug)]
enum SettingsAction {
    ToggleVsync,
    ToggleFullscreen,
    TogglePerfLog,
    CycleShadows,
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
}

impl SettingsSlider {
    fn range(self) -> (f32, f32) {
        match self {
            SettingsSlider::Sensitivity => (MIN_SENSITIVITY_MULTIPLIER, MAX_SENSITIVITY_MULTIPLIER),
            SettingsSlider::Fov => (MIN_FOV_DEGREES, MAX_FOV_DEGREES),
            SettingsSlider::RenderDistance => {
                (MIN_RENDER_DISTANCE as f32, MAX_RENDER_DISTANCE as f32)
            }
        }
    }

    fn step(self) -> f32 {
        match self {
            SettingsSlider::Sensitivity => 0.1,
            SettingsSlider::Fov | SettingsSlider::RenderDistance => 1.0,
        }
    }

    fn precision(self) -> i32 {
        match self {
            SettingsSlider::Sensitivity => 1,
            SettingsSlider::Fov | SettingsSlider::RenderDistance => 0,
        }
    }

    fn value(self, settings: &GameSettings) -> f32 {
        match self {
            SettingsSlider::Sensitivity => settings.mouse_sensitivity / DEFAULT_MOUSE_SENSITIVITY,
            SettingsSlider::Fov => settings.fov_degrees,
            SettingsSlider::RenderDistance => settings.render_distance as f32,
        }
    }
}

/// Marks the thumb of a settings slider so it can follow the value.
#[derive(Component)]
struct SettingsThumb;

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
                    sync_slider_values,
                    sync_slider_thumbs,
                    update_values,
                    update_ram_hint,
                    style_buttons,
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
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme.colors.settings_scrim),
            GlobalZIndex(theme.layers.settings),
        ))
        .id();

    let panel = spawn_panel(
        commands,
        theme,
        root,
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            padding: UiRect::axes(Val::Px(SETTINGS_PADDING_X), Val::Px(SETTINGS_PADDING_Y)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(12.0),
            overflow: Overflow::clip(),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.settings_panel_bg,
        theme.colors.panel_border,
    );

    spawn_text(
        commands,
        theme,
        panel,
        "SETTINGS",
        theme.fonts.title,
        theme.colors.text,
        StaticLabel,
    );
    spawn_text(
        commands,
        theme,
        panel,
        "Esc returns to the pause menu",
        theme.fonts.small,
        theme.colors.muted_text,
        StaticLabel,
    );

    let tabs = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(8.0),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();

    for kind in SettingsTab::ALL {
        spawn_menu_button(
            commands,
            tabs,
            kind.label(),
            TAB_WIDTH,
            36.0,
            SettingsAction::SelectTab(kind),
            theme,
        );
    }

    let content = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                max_width: Val::Px(SETTINGS_CONTENT_WIDTH),
                min_height: Val::Px(0.0),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                align_self: AlignSelf::Center,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(7.0),
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();

    let video = spawn_tab_panel(commands, content, SettingsTab::Video, active_tab);
    setup_video_tab(commands, video, settings, theme);

    let audio = spawn_tab_panel(commands, content, SettingsTab::Audio, active_tab);
    setup_audio_tab(commands, audio, theme);

    let mouse = spawn_tab_panel(commands, content, SettingsTab::Mouse, active_tab);
    setup_mouse_tab(commands, mouse, settings, theme);

    let controls = spawn_tab_panel(commands, content, SettingsTab::Controls, active_tab);
    setup_controls_tab(commands, controls, bindings, theme);

    let map = spawn_tab_panel(commands, content, SettingsTab::Map, active_tab);
    setup_map_tab(commands, map, bindings, theme);

    let actions = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(8.0),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(panel),
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

fn setup_video_tab(commands: &mut Commands, tab: Entity, settings: &GameSettings, theme: &UiTheme) {
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
    commands
        .entity(hint)
        .insert(TextLayout::justify(Justify::Left));

    spawn_toggle_row(
        commands,
        tab,
        "Shadows",
        settings.shadow_quality.label(),
        120.0,
        SettingsAction::CycleShadows,
        SettingsValue::Shadows,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "VSync",
        "Off",
        80.0,
        SettingsAction::ToggleVsync,
        SettingsValue::Vsync,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "Fullscreen",
        "On",
        80.0,
        SettingsAction::ToggleFullscreen,
        SettingsValue::Fullscreen,
        theme,
    );
    spawn_toggle_row(
        commands,
        tab,
        "Perf log (1 Hz)",
        "Off",
        80.0,
        SettingsAction::TogglePerfLog,
        SettingsValue::PerfLog,
        theme,
    );

    spawn_divider(commands, tab, theme);
    spawn_note(
        commands,
        tab,
        "Esc returns to the pause menu. Changes apply immediately.",
        theme.fonts.slot_count,
        theme.colors.muted_text,
        theme,
    );
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        spawn_note(
            commands,
            tab,
            "Note: Wayland syncs every frame to the monitor, so VSync Off is ignored.",
            theme.fonts.small,
            theme.colors.muted_text,
            theme,
        );
    }
}

fn setup_audio_tab(commands: &mut Commands, tab: Entity, theme: &UiTheme) {
    spawn_text(
        commands,
        theme,
        tab,
        "AUDIO",
        theme.fonts.compass,
        theme.colors.accent,
        StaticLabel,
    );
    spawn_note(
        commands,
        tab,
        "Audio controls are reserved for a future milestone.",
        theme.fonts.body,
        theme.colors.muted_text,
        theme,
    );
    spawn_note(
        commands,
        tab,
        "No volume settings are active in this build.",
        theme.fonts.slot_count,
        theme.colors.muted_text,
        theme,
    );
}

fn setup_mouse_tab(commands: &mut Commands, tab: Entity, settings: &GameSettings, theme: &UiTheme) {
    spawn_text(
        commands,
        theme,
        tab,
        "MOUSE",
        theme.fonts.compass,
        theme.colors.accent,
        StaticLabel,
    );
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
        theme.fonts.slot_count,
        theme.colors.muted_text,
        theme,
    );
}

fn setup_controls_tab(
    commands: &mut Commands,
    tab: Entity,
    bindings: &InputBindings,
    theme: &UiTheme,
) {
    spawn_text(
        commands,
        theme,
        tab,
        "CONTROLS",
        theme.fonts.compass,
        theme.colors.accent,
        StaticLabel,
    );
    for (key, action) in [
        (bindings.move_forward, "Move forward"),
        (bindings.move_back, "Move back"),
        (bindings.move_left, "Move left"),
        (bindings.move_right, "Move right"),
        (bindings.jump, "Jump"),
        (bindings.sprint, "Sprint"),
        (bindings.crouch, "Crouch"),
        (bindings.chat, "Chat"),
        (bindings.pause, "Pause"),
        (bindings.help, "Help"),
        (bindings.backpack, "Backpack and character"),
        (bindings.debug_overlay, "Debug overlay"),
        (bindings.save, "Save"),
        (bindings.load, "Load"),
        (bindings.screenshot, "Screenshot"),
    ] {
        spawn_binding_row(commands, tab, key_label(key), action, theme);
    }
    spawn_note(
        commands,
        tab,
        "LMB mines or places. RMB gathers, opens the hammer menu and picks up.",
        theme.fonts.small,
        theme.colors.muted_text,
        theme,
    );
    spawn_note(
        commands,
        tab,
        "1-8 select hotbar slots; the wheel cycles them.",
        theme.fonts.small,
        theme.colors.muted_text,
        theme,
    );
}

fn setup_map_tab(commands: &mut Commands, tab: Entity, bindings: &InputBindings, theme: &UiTheme) {
    spawn_text(
        commands,
        theme,
        tab,
        "MAP / DEBUG",
        theme.fonts.compass,
        theme.colors.accent,
        StaticLabel,
    );
    spawn_note(
        commands,
        tab,
        format!(
            "{} opens the overhead map. Drag to pan, scroll to zoom, C/Home to recenter.",
            key_label(bindings.map)
        ),
        theme.fonts.slot_count,
        theme.colors.muted_text,
        theme,
    );
    spawn_note(
        commands,
        tab,
        format!(
            "{} shows the debug overlay.",
            key_label(bindings.debug_overlay)
        ),
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
) {
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
}

fn spawn_divider(commands: &mut Commands, parent: Entity, theme: &UiTheme) {
    commands.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(1.0),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(theme.colors.faint_border),
        ChildOf(parent),
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

fn spawn_binding_row(
    commands: &mut Commands,
    parent: Entity,
    key: String,
    action: &str,
    theme: &UiTheme,
) {
    let row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                min_height: Val::Px(22.0),
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                ..default()
            },
            ChildOf(parent),
        ))
        .id();
    let key = spawn_keycap(commands, theme, row, key, 64.0);
    commands.entity(key).insert(StaticLabel);
    spawn_text(
        commands,
        theme,
        row,
        action,
        theme.fonts.body,
        theme.colors.text,
        StaticLabel,
    );
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
    }
}

fn on_activate(
    activate: On<Activate>,
    actions: Query<&SettingsAction>,
    mut settings: ResMut<GameSettings>,
    mut tab: ResMut<SettingsTab>,
    mut menu: ResMut<MenuState>,
    mut ui: ResMut<UiManager>,
    mut commands: Commands,
) {
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
        SettingsAction::CycleShadows => {
            settings.shadow_quality = settings.shadow_quality.next();
        }
        SettingsAction::ResetDefaults => {
            *settings = GameSettings::default();
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
        assert_eq!(SettingsTab::Video.label(), "Video");
        assert_eq!(SettingsTab::Controls.label(), "Controls");
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
            assert!(SETTINGS_CONTENT_WIDTH > 0.0);
            assert!(SETTINGS_CONTENT_WIDTH < 1280.0);
        }
    }
}
