//! Shared UI theme and reusable Bevy UI building blocks.
//!
//! [`UiTheme`] is the single source of truth for palette, typography, radii,
//! borders, shadows, spacing, and stacking order. The visual direction is
//! clean, balanced, and peaceful: dark slate-blue surfaces do most of the work,
//! warm gold is a restrained accent, and text remains soft and readable.
//! Every HUD, menu, inventory, crafting, tooltip, and overlay surface reads
//! from this resource so the game presents one cohesive identity.

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;

use super::icons::UiIcons;
use crate::items::ItemId;

/// Semantic colors used by the entire game UI.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct UiColors {
    pub panel_bg: Color,
    pub panel_bg_strong: Color,
    pub panel_bg_soft: Color,
    pub overlay_bg: Color,
    pub settings_scrim: Color,
    pub settings_panel_bg: Color,
    pub backdrop_bg: Color,
    pub bar_track: Color,
    pub selected_bg: Color,
    pub button_bg: Color,
    pub button_hover: Color,
    pub button_pressed: Color,
    pub slider_track: Color,
    pub slider_thumb: Color,
    pub slider_thumb_hover: Color,

    pub text: Color,
    pub text_on_accent: Color,
    pub muted_text: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub xp: Color,
    pub coin: Color,
    pub coin_glyph: Color,
    pub hunger: Color,
    pub hydration: Color,
    pub stamina: Color,
    pub warn: Color,
    pub danger: Color,
    pub success: Color,

    pub faint_border: Color,
    pub panel_border: Color,
    pub border_idle: Color,
    pub border_hover: Color,
    pub border_active: Color,
    pub crosshair: Color,
}

/// Font size roles. Screens refer to these instead of bare pixel numbers.
#[derive(Debug, Clone, Copy)]
pub struct UiTypography {
    pub micro: f32,
    pub small: f32,
    pub body: f32,
    pub label: f32,
    pub heading: f32,
    pub title: f32,
    pub value: f32,
    pub compass: f32,
    pub chat: f32,
    pub keycap: f32,
    pub slot_count: f32,
    pub bar_min: f32,
    pub bar_max: f32,
}

/// Shared geometry tokens for corners and borders.
#[derive(Debug, Clone, Copy)]
pub struct UiShape {
    pub radius: f32,
    pub small_radius: f32,
    pub border_width: f32,
    pub emphasis_border_width: f32,
}

impl UiShape {
    #[inline]
    pub fn rounded(&self) -> BorderRadius {
        BorderRadius::all(Val::Px(self.radius))
    }

    #[inline]
    pub fn rounded_small(&self) -> BorderRadius {
        BorderRadius::all(Val::Px(self.small_radius))
    }

    #[inline]
    pub fn circle(&self) -> BorderRadius {
        BorderRadius::all(Val::Percent(50.0))
    }

    #[inline]
    pub fn border(&self) -> UiRect {
        UiRect::all(Val::Px(self.border_width))
    }

    #[inline]
    pub fn emphasis_border(&self) -> UiRect {
        UiRect::all(Val::Px(self.emphasis_border_width))
    }
}

/// Consistent spacing roles for calm, balanced layouts.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct UiSpacing {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
}

/// Stacking order for every overlay class.
#[derive(Debug, Clone, Copy)]
pub struct UiLayers {
    pub chat: i32,
    pub backdrop: i32,
    pub dock: i32,
    pub shape_menu: i32,
    pub pause: i32,
    pub settings: i32,
}

/// Resource injected by [`crate::ui::UiPlugin`] and used by every UI system.
#[derive(Resource, Debug, Clone)]
pub struct UiTheme {
    pub colors: UiColors,
    pub fonts: UiTypography,
    pub shape: UiShape,
    pub spacing: UiSpacing,
    pub layers: UiLayers,
    pub shadow: TextShadow,
}

impl Default for UiTheme {
    fn default() -> Self {
        // Starting palette (adjustable after in-game evaluation):
        // accent warm gold, primary dark slate blue, secondary lighter slate blue,
        // soft white text, near-black for dark contrast details.
        let accent = Color::srgb_u8(217, 182, 92);
        let accent_soft = Color::srgb_u8(228, 204, 140);
        let primary = Color::srgb_u8(39, 57, 77); // ~2-3 shades darker than #354B63
        let secondary = Color::srgb_u8(104, 127, 150);
        let text = Color::srgb_u8(242, 240, 233);
        let dark = Color::srgb_u8(23, 29, 37);

        Self {
            colors: UiColors {
                // Slate-blue surfaces do most visual work; translucency keeps the
                // HUD calm over the world without heavy panels everywhere.
                panel_bg: with_alpha(primary, 0.72),
                panel_bg_strong: with_alpha(mix(primary, dark, 0.35), 0.90),
                panel_bg_soft: with_alpha(mix(primary, secondary, 0.35), 0.58),
                overlay_bg: with_alpha(dark, 0.68),
                settings_scrim: with_alpha(dark, 0.56),
                settings_panel_bg: with_alpha(mix(primary, dark, 0.30), 0.94),
                backdrop_bg: with_alpha(dark, 0.40),
                bar_track: with_alpha(mix(dark, primary, 0.45), 0.82),
                selected_bg: with_alpha(mix(primary, accent, 0.22), 0.92),
                button_bg: with_alpha(mix(primary, dark, 0.18), 0.88),
                button_hover: with_alpha(mix(primary, secondary, 0.28), 0.94),
                button_pressed: with_alpha(mix(primary, accent, 0.24), 0.96),
                slider_track: with_alpha(mix(dark, primary, 0.55), 0.92),
                slider_thumb: accent,
                slider_thumb_hover: accent_soft,

                text,
                text_on_accent: dark,
                muted_text: mix(text, secondary, 0.34),
                accent,
                accent_soft,
                xp: accent,
                coin: Color::srgb_u8(230, 198, 112),
                coin_glyph: dark,
                hunger: Color::srgb_u8(214, 156, 92),
                hydration: Color::srgb_u8(126, 168, 196),
                stamina: Color::srgb_u8(176, 198, 150),
                warn: Color::srgb_u8(228, 190, 112),
                danger: Color::srgb_u8(214, 118, 108),
                success: Color::srgb_u8(150, 196, 150),

                // Thin, subtle borders; gold appears only for active/important states.
                faint_border: with_alpha(secondary, 0.28),
                panel_border: with_alpha(secondary, 0.42),
                border_idle: with_alpha(secondary, 0.30),
                border_hover: with_alpha(mix(secondary, accent, 0.35), 0.58),
                border_active: with_alpha(accent, 0.78),
                crosshair: with_alpha(text, 0.92),
            },
            fonts: UiTypography {
                micro: 10.0,
                small: 11.0,
                body: 12.5,
                label: 14.0,
                heading: 15.0,
                title: 18.0,
                value: 16.0,
                compass: 13.0,
                chat: 13.0,
                keycap: 10.0,
                slot_count: 11.0,
                bar_min: 10.0,
                bar_max: 14.0,
            },
            shape: UiShape {
                radius: 7.0,
                small_radius: 5.0,
                border_width: 1.0,
                emphasis_border_width: 1.0,
            },
            spacing: UiSpacing {
                xs: 4.0,
                sm: 8.0,
                md: 12.0,
                lg: 16.0,
            },
            layers: UiLayers {
                chat: 10,
                backdrop: 12,
                dock: 13,
                shape_menu: 15,
                pause: 20,
                settings: 21,
            },
            shadow: TextShadow {
                offset: Vec2::new(1.0, 1.0),
                color: with_alpha(dark, 0.78),
            },
        }
    }
}

#[inline]
pub fn display_if(visible: bool) -> Display {
    if visible {
        Display::Flex
    } else {
        Display::None
    }
}

#[inline]
#[allow(dead_code)]
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let a = a.to_srgba();
    let b = b.to_srgba();
    let t = t.clamp(0.0, 1.0);
    Color::srgba(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        a.alpha + (b.alpha - a.alpha) * t,
    )
}

#[inline]
#[allow(dead_code)]
pub fn with_alpha(color: Color, alpha: f32) -> Color {
    let c = color.to_srgba();
    Color::srgba(c.red, c.green, c.blue, alpha.clamp(0.0, 1.0))
}

/// Text with the shared outline shadow. Generic marker lets each screen query
/// only its own labels.
pub fn spawn_text<M: Component>(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    content: impl Into<String>,
    size: f32,
    color: Color,
    marker: M,
) -> Entity {
    commands
        .spawn((
            Text::new(content),
            TextFont::from_font_size(size),
            TextColor(color),
            TextLayout::no_wrap(),
            theme.shadow,
            marker,
            ChildOf(parent),
        ))
        .id()
}

/// Standard translucent panel. Clean and undecorated: slate surface + thin
/// subtle border, with no gradients or decorative bevel textures.
pub fn spawn_panel(
    commands: &mut Commands,
    _theme: &UiTheme,
    parent: Entity,
    node: Node,
    background: Color,
    border: Color,
) -> Entity {
    commands
        .spawn((
            node,
            BackgroundColor(background),
            BorderColor::all(border),
            ChildOf(parent),
        ))
        .id()
}

/// A compact fill bar with its current value on the left and a static caption
/// on the right. `Fill` and `Value` are markers used by the owning screen's
/// update system.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub fn spawn_bar<Fill: Component, Value: Component>(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    width: Val,
    height: f32,
    flex_grow: f32,
    fill_color: Color,
    initial: impl Into<String>,
    caption: impl Into<String>,
    fill: Fill,
    value: Value,
) -> Entity {
    spawn_bar_inner(
        commands,
        theme,
        parent,
        width,
        height,
        flex_grow,
        fill_color,
        initial,
        caption,
        fill,
        value,
        false,
        theme.colors.bar_track,
        theme.colors.faint_border,
    )
}

/// Like [`spawn_bar`], but shows the value as a centred overlay inside the
/// track for a clean, lightweight progress readout.
#[allow(clippy::too_many_arguments)]
pub fn spawn_bar_overlay<Fill: Component, Value: Component>(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    width: Val,
    height: f32,
    flex_grow: f32,
    fill_color: Color,
    initial: impl Into<String>,
    caption: impl Into<String>,
    fill: Fill,
    value: Value,
) -> Entity {
    spawn_bar_overlay_styled(
        commands,
        theme,
        parent,
        width,
        height,
        flex_grow,
        fill_color,
        initial,
        caption,
        fill,
        value,
        theme.colors.bar_track,
        theme.colors.faint_border,
    )
}

/// [`spawn_bar_overlay`] with an explicit track background and border.
#[allow(clippy::too_many_arguments)]
pub fn spawn_bar_overlay_styled<Fill: Component, Value: Component>(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    width: Val,
    height: f32,
    flex_grow: f32,
    fill_color: Color,
    initial: impl Into<String>,
    caption: impl Into<String>,
    fill: Fill,
    value: Value,
    track_bg: Color,
    track_border: Color,
) -> Entity {
    spawn_bar_inner(
        commands,
        theme,
        parent,
        width,
        height,
        flex_grow,
        fill_color,
        initial,
        caption,
        fill,
        value,
        true,
        track_bg,
        track_border,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_bar_inner<Fill: Component, Value: Component>(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    width: Val,
    height: f32,
    flex_grow: f32,
    fill_color: Color,
    initial: impl Into<String>,
    caption: impl Into<String>,
    fill: Fill,
    value: Value,
    overlay: bool,
    track_bg: Color,
    track_border: Color,
) -> Entity {
    let root = commands
        .spawn((
            Node {
                width,
                height: Val::Px(height),
                flex_grow,
                flex_basis: if flex_grow > 0.0 {
                    Val::Px(0.0)
                } else {
                    Val::Auto
                },
                overflow: Overflow::clip(),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded_small(),
                ..default()
            },
            BackgroundColor(track_bg),
            BorderColor::all(track_border),
            ChildOf(parent),
        ))
        .id();

    commands.spawn((
        fill,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            border_radius: theme.shape.rounded_small(),
            ..default()
        },
        BackgroundColor(fill_color),
        ChildOf(root),
    ));

    let labels = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                padding: if overlay {
                    UiRect::ZERO
                } else {
                    UiRect::axes(Val::Px(theme.spacing.sm), Val::Px(0.0))
                },
                align_items: AlignItems::Center,
                justify_content: if overlay {
                    JustifyContent::Center
                } else {
                    JustifyContent::SpaceBetween
                },
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    let font_size = if overlay {
        (height * 0.58).clamp(theme.fonts.small, theme.fonts.body)
    } else {
        ((height * 0.5) + 2.0).clamp(theme.fonts.bar_min, theme.fonts.bar_max)
    };
    commands.spawn((
        value,
        Text::new(initial),
        TextFont::from_font_size(font_size),
        TextColor(theme.colors.text),
        TextLayout::no_wrap().with_justify(if overlay {
            Justify::Center
        } else {
            Justify::Left
        }),
        theme.shadow,
        ChildOf(labels),
    ));
    if !overlay {
        commands.spawn((
            Text::new(caption),
            TextFont::from_font_size(font_size),
            TextColor(theme.colors.muted_text),
            TextLayout::no_wrap().with_justify(Justify::Right),
            theme.shadow,
            ChildOf(labels),
        ));
    }

    root
}

/// Small unframed code-generated item or status icon.
pub fn spawn_item_icon<M: Component>(
    commands: &mut Commands,
    icons: &UiIcons,
    parent: Entity,
    item: ItemId,
    size: f32,
    marker: M,
) -> Entity {
    commands
        .spawn((
            Node {
                width: Val::Px(size),
                height: Val::Px(size),
                flex_shrink: 0.0,
                ..default()
            },
            ImageNode::new(icons.item(item)),
            marker,
            ChildOf(parent),
        ))
        .id()
}

/// Small unframed HUD/status icon (non-item).
#[allow(dead_code)]
pub fn spawn_hud_icon<M: Component>(
    commands: &mut Commands,
    icons: &UiIcons,
    parent: Entity,
    icon: crate::ui::icons::HudIcon,
    size: f32,
    marker: M,
) -> Entity {
    commands
        .spawn((
            Node {
                width: Val::Px(size),
                height: Val::Px(size),
                flex_shrink: 0.0,
                ..default()
            },
            ImageNode::new(icons.hud(icon)),
            marker,
            ChildOf(parent),
        ))
        .id()
}

/// Compact keycap used by hints and help panels.
pub fn spawn_keycap(
    commands: &mut Commands,
    theme: &UiTheme,
    parent: Entity,
    label: impl Into<String>,
    width: f32,
) -> Entity {
    let key = commands
        .spawn((
            Node {
                width: Val::Px(width),
                height: Val::Px(18.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: theme.shape.border(),
                border_radius: theme.shape.rounded_small(),
                ..default()
            },
            BackgroundColor(theme.colors.button_bg),
            BorderColor::all(theme.colors.faint_border),
            ChildOf(parent),
        ))
        .id();
    spawn_text(
        commands,
        theme,
        key,
        label,
        theme.fonts.keycap,
        theme.colors.accent_soft,
        KeycapText,
    );
    key
}

#[derive(Component)]
pub struct KeycapText;

/// Convert a binding into the short label shown on HUD keycaps.
pub fn key_label(key: KeyCode) -> String {
    match key {
        KeyCode::KeyA => "A".into(),
        KeyCode::KeyB => "B".into(),
        KeyCode::KeyC => "C".into(),
        KeyCode::KeyD => "D".into(),
        KeyCode::KeyE => "E".into(),
        KeyCode::KeyF => "F".into(),
        KeyCode::KeyG => "G".into(),
        KeyCode::KeyH => "H".into(),
        KeyCode::KeyI => "I".into(),
        KeyCode::KeyJ => "J".into(),
        KeyCode::KeyK => "K".into(),
        KeyCode::KeyL => "L".into(),
        KeyCode::KeyM => "M".into(),
        KeyCode::KeyN => "N".into(),
        KeyCode::KeyO => "O".into(),
        KeyCode::KeyP => "P".into(),
        KeyCode::KeyQ => "Q".into(),
        KeyCode::KeyR => "R".into(),
        KeyCode::KeyS => "S".into(),
        KeyCode::KeyT => "T".into(),
        KeyCode::KeyU => "U".into(),
        KeyCode::KeyV => "V".into(),
        KeyCode::KeyW => "W".into(),
        KeyCode::KeyX => "X".into(),
        KeyCode::KeyY => "Y".into(),
        KeyCode::KeyZ => "Z".into(),
        KeyCode::Digit1 => "1".into(),
        KeyCode::Digit2 => "2".into(),
        KeyCode::Digit3 => "3".into(),
        KeyCode::Digit4 => "4".into(),
        KeyCode::Digit5 => "5".into(),
        KeyCode::Digit6 => "6".into(),
        KeyCode::Digit7 => "7".into(),
        KeyCode::Digit8 => "8".into(),
        KeyCode::Digit9 => "9".into(),
        KeyCode::Digit0 => "0".into(),
        KeyCode::Space => "SPACE".into(),
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "SHIFT".into(),
        KeyCode::ControlLeft | KeyCode::ControlRight => "CTRL".into(),
        KeyCode::AltLeft | KeyCode::AltRight => "ALT".into(),
        KeyCode::Enter => "ENTER".into(),
        KeyCode::Escape => "ESC".into(),
        KeyCode::Tab => "TAB".into(),
        KeyCode::Backspace => "BKSP".into(),
        KeyCode::F3 => "F3".into(),
        KeyCode::F9 => "F9".into(),
        KeyCode::F10 => "F10".into(),
        KeyCode::F12 => "F12".into(),
        other => format!("{other:?}"),
    }
}

/// Comma-separated numbers for coin counts and large XP values.
pub fn format_number(value: u32) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(byte as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(900), "900");
        assert_eq!(format_number(1_250), "1,250");
    }

    #[test]
    fn key_labels_are_short() {
        assert_eq!(key_label(KeyCode::KeyM), "M");
        assert_eq!(key_label(KeyCode::ShiftLeft), "SHIFT");
        assert_eq!(key_label(KeyCode::Tab), "TAB");
    }

    #[test]
    fn default_theme_defines_every_style_role() {
        let theme = UiTheme::default();
        assert_eq!(theme.colors.accent, theme.colors.slider_thumb);
        assert_ne!(theme.colors.accent, theme.colors.accent_soft);
        assert_eq!(theme.colors.xp, theme.colors.accent);
        assert!(theme.fonts.micro < theme.fonts.small);
        assert!(theme.fonts.small < theme.fonts.body);
        assert!(theme.fonts.body < theme.fonts.label);
        assert!(theme.shape.small_radius < theme.shape.radius);
        assert_eq!(theme.shape.border_width, 1.0);
        assert_eq!(theme.layers.chat, 10);
        assert_eq!(theme.layers.settings, 21);
        assert_eq!(theme.shadow.offset, Vec2::new(1.0, 1.0));
        assert!(theme.spacing.xs < theme.spacing.sm);
        assert!(theme.spacing.sm < theme.spacing.md);
    }

    #[test]
    fn default_palette_uses_slate_and_gold_direction() {
        let theme = UiTheme::default();
        let accent = theme.colors.accent.to_srgba();
        let text = theme.colors.text.to_srgba();
        // Warm gold accent has strong red/green, softer blue.
        assert!(accent.red > accent.blue && accent.green > accent.blue);
        // Soft white text stays bright and near-neutral.
        assert!(text.red > 0.85 && text.green > 0.85 && text.blue > 0.80);
        // XP uses gold accent, not a separate dominant color.
        assert_eq!(theme.colors.xp, theme.colors.accent);
    }
}
