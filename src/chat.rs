//! In-game chat and commands.
//!
//! Press Enter to open the chat, type a message and press Enter to send it.
//! Sending keeps the chat open so you can type several lines; Escape closes it.
//! Lines starting with `/` run a command instead. The chat releases the mouse
//! cursor while it is open, just like the pause menu and the hammer shape menu.
//!
//! The log is taller than the old HUD chat, has a scrollbar, and keeps a
//! runtime-only command/message history that can be browsed with Up/Down.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::input::{ButtonState, keyboard::KeyboardInput};
use bevy::prelude::*;

use crate::backpack::Backpack;
use crate::items::{ItemCategory, ItemId, store_item};
use crate::player::{CursorCapture, Player};
use crate::pockets::Pockets;
use crate::settings::InputBindings;
use crate::ui::theme::{UiTheme, display_if, key_label, spawn_panel, spawn_text};
use crate::ui_manager::{Overlay, UiManager};
use crate::world::chunk::{BLOCK_SIZE, BLOCKS_PER_METRE};
use crate::world::{PendingTeleport, SpawnPoint, WorldGenResource};

const MAX_MESSAGES: usize = 160;
const MAX_HISTORY: usize = 50;
const VISIBLE_LINES: usize = 14;
const VISIBLE_SECONDS: f32 = 9.0;
const LOG_HEIGHT: f32 = 260.0;

#[derive(Clone)]
pub struct ChatMessage {
    pub text: String,
    pub color: Color,
}

#[derive(Resource)]
pub struct ChatState {
    pub input: String,
    messages: Vec<ChatMessage>,
    /// Runtime-only submitted-line history. Never written to disk.
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    /// Number of lines scrolled up from the bottom.
    pub scroll: usize,
    visible_timer: f32,
    /// Set on the frame the chat opens so the opening `Enter` is not typed.
    just_opened: bool,
    revision: u64,
    rendered_revision: u64,
    rendered_scroll: usize,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            input: String::new(),
            messages: Vec::new(),
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            scroll: 0,
            visible_timer: 0.0,
            just_opened: false,
            revision: 0,
            rendered_revision: 0,
            rendered_scroll: 0,
        }
    }
}

impl ChatState {
    pub fn max_scroll(&self) -> usize {
        self.messages.len().saturating_sub(VISIBLE_LINES)
    }

    fn remember(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.history_index = None;
        self.draft.clear();
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = self.input.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.input = self.history[index].clone();
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.input = self.draft.clone();
        }
    }
}

#[derive(Component)]
struct ChatRoot;

/// Marker for the compact header title and key-hint labels.
#[derive(Component)]
struct ChatLog;

#[derive(Component)]
struct ChatInputRow;

#[derive(Component)]
struct ChatInputText;

#[derive(Component)]
struct ChatScrollbarTrack;

#[derive(Component)]
struct ChatScrollbarThumb;

pub struct ChatPlugin;

/// Ordered before the other UI chains so shared `Node`/`Text` access is
/// deterministic.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ChatUiSystems;

impl Plugin for ChatPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatState>()
            .add_systems(Startup, setup_chat)
            .add_systems(
                Update,
                (
                    chat_input,
                    scroll_chat,
                    sync_chat_visibility,
                    update_chat_input_text,
                    render_chat_log,
                    update_scrollbar,
                )
                    .chain()
                    .in_set(ChatUiSystems),
            );
    }
}

fn setup_chat(mut commands: Commands, bindings: Res<InputBindings>, theme: Res<UiTheme>) {
    let root = commands
        .spawn((
            ChatRoot,
            Name::new("Chat"),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(14.0),
                bottom: Val::Px(76.0),
                width: Val::Px(424.0),
                padding: UiRect::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(5.0),
                border: theme.shape.border(),
                border_radius: theme.shape.rounded(),
                display: Display::None,
                ..default()
            },
            BackgroundColor(theme.colors.panel_bg_strong),
            BorderColor::all(theme.colors.panel_border),
            GlobalZIndex(theme.layers.chat),
        ))
        .id();

    // Compact header: title on the left, dynamic key hints on the right.
    let header = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    commands.spawn((
        Text::new("Chat"),
        TextFont::from_font_size(theme.fonts.label),
        TextColor(theme.colors.accent),
        theme.shadow,
        ChildOf(header),
    ));
    commands.spawn((
        Text::new(format!(
            "[{}] send   [{}] close",
            key_label(bindings.chat),
            key_label(bindings.pause)
        )),
        TextFont::from_font_size(theme.fonts.slot_count),
        TextColor(theme.colors.muted_text),
        theme.shadow,
        ChildOf(header),
    ));

    let body = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(LOG_HEIGHT),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(5.0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    commands.spawn((
        ChatLog,
        Node {
            flex_grow: 1.0,
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(1.0),
            overflow: Overflow::clip(),
            ..default()
        },
        ChildOf(body),
    ));

    let track = commands
        .spawn((
            ChatScrollbarTrack,
            Node {
                width: Val::Px(7.0),
                height: Val::Percent(100.0),
                border_radius: theme.shape.rounded(),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme.colors.faint_border),
            ChildOf(body),
        ))
        .id();

    commands.spawn((
        ChatScrollbarThumb,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        BackgroundColor(theme.colors.slider_thumb),
        ChildOf(track),
    ));

    let input_row = spawn_panel(
        &mut commands,
        &theme,
        root,
        Node {
            display: Display::None,
            padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)),
            border: theme.shape.border(),
            border_radius: theme.shape.rounded(),
            ..default()
        },
        theme.colors.slider_track,
        theme.colors.faint_border,
    );
    commands.entity(input_row).insert(ChatInputRow);

    spawn_text(
        &mut commands,
        &theme,
        input_row,
        "> ",
        theme.fonts.label,
        theme.colors.text,
        ChatInputText,
    );
}

#[allow(clippy::too_many_arguments)]
fn chat_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut keyboard: MessageReader<KeyboardInput>,
    mut chat: ResMut<ChatState>,
    mut ui: ResMut<UiManager>,
    capture: Res<CursorCapture>,
    bindings: Res<InputBindings>,
    theme: Res<UiTheme>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    generator: Res<WorldGenResource>,
    spawn: Res<SpawnPoint>,
    mut pending_teleport: ResMut<PendingTeleport>,
    mut movement_mode: ResMut<crate::player::MovementMode>,
    mut player: Query<(&mut Transform, &mut Player)>,
) {
    let dt = time.delta_secs().min(0.05);
    let chat_open = ui.is_open(Overlay::Chat);

    if !chat_open {
        chat.visible_timer = (chat.visible_timer - dt).max(0.0);
    }

    if !chat_open && !ui.any_open() && capture.grabbed && keys.just_pressed(bindings.chat) {
        ui.open(Overlay::Chat);
        chat.just_opened = true;
        chat.input.clear();
        chat.visible_timer = 0.0;
        chat.scroll = 0;
        if chat.messages.is_empty() {
            push_message(
                &mut chat,
                format!(
                    "[{}] sends. [{}] closes. Up/Down history. Type /help for commands.",
                    key_label(bindings.chat),
                    key_label(bindings.pause),
                ),
                theme.colors.muted_text,
            );
        }
    }

    // Up/Down browse the runtime-only line history while typing.
    if ui.is_open(Overlay::Chat) {
        if keys.just_pressed(KeyCode::ArrowUp) {
            chat.history_previous();
        }
        if keys.just_pressed(KeyCode::ArrowDown) {
            chat.history_next();
        }
    }

    let open = ui.is_open(Overlay::Chat);
    let skip_this_frame = chat.just_opened;
    let mut submitted = false;

    for input in keyboard.read() {
        if !open || skip_this_frame || input.state != ButtonState::Pressed {
            continue;
        }

        match input.key_code {
            KeyCode::Backspace => {
                chat.input.pop();
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                submitted = true;
                break;
            }
            _ => {
                if let Some(text) = &input.text {
                    for character in text.chars() {
                        if !character.is_control() {
                            chat.input.push(character);
                        }
                    }
                }
            }
        }
    }

    chat.just_opened = false;

    if submitted {
        let line = chat.input.clone();
        chat.input.clear();
        chat.history_index = None;
        chat.draft.clear();
        chat.remember(&line);

        let player_data = player.single_mut().ok();
        let movement = Some(&mut *movement_mode);
        send_chat(
            &theme,
            &mut chat,
            &mut ui,
            &mut pockets,
            &mut backpack,
            &generator,
            &spawn,
            &mut pending_teleport,
            player_data,
            movement,
            &line,
        );
    }
}

type PlayerData<'a> = (Mut<'a, Transform>, Mut<'a, Player>);

#[allow(clippy::too_many_arguments)]
fn send_chat(
    theme: &UiTheme,
    chat: &mut ChatState,
    ui: &mut UiManager,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
    generator: &WorldGenResource,
    spawn: &SpawnPoint,
    pending_teleport: &mut PendingTeleport,
    player: Option<PlayerData>,
    movement: Option<&mut crate::player::MovementMode>,
    line: &str,
) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    if let Some(command) = line.strip_prefix('/') {
        execute_command(
            theme,
            chat,
            ui,
            pockets,
            backpack,
            generator,
            spawn,
            pending_teleport,
            player,
            movement,
            command,
        );
    } else {
        push_message(chat, format!("You: {line}"), theme.colors.text);
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_command(
    theme: &UiTheme,
    chat: &mut ChatState,
    ui: &mut UiManager,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
    generator: &WorldGenResource,
    spawn: &SpawnPoint,
    pending_teleport: &mut PendingTeleport,
    mut player: Option<PlayerData>,
    movement: Option<&mut crate::player::MovementMode>,
    command: &str,
) {
    let mut parts = command.split_whitespace();
    let name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();

    match name.as_str() {
        "help" => {
            push_message(chat, "/help - show this list", theme.colors.accent);
            push_message(
                chat,
                "Open the backpack (Tab) to craft by hand",
                theme.colors.muted_text,
            );
            push_message(
                chat,
                "/give <item> [count] - add items",
                theme.colors.accent,
            );
            push_message(
                chat,
                "/clear - empty pockets and backpack",
                theme.colors.accent,
            );
            push_message(
                chat,
                "/upgrade capacity - add backpack slots",
                theme.colors.accent,
            );
            push_message(
                chat,
                "/unlock <category> - unlock backpack section",
                theme.colors.accent,
            );
            push_message(
                chat,
                "/fly - toggle flying (Space up, Ctrl down)",
                theme.colors.accent,
            );
            push_message(chat, "/noclip - toggle collision", theme.colors.accent);
            push_message(
                chat,
                "/tp <x> <y> <z> - teleport (metres)",
                theme.colors.accent,
            );
            push_message(
                chat,
                "/spawn - return to the spawn point",
                theme.colors.accent,
            );
            push_message(chat, "/seed - show the world seed", theme.colors.accent);
            push_message(
                chat,
                "/pos - show your position (metres)",
                theme.colors.accent,
            );
        }
        "give" => {
            if rest.is_empty() {
                ui.open(Overlay::GiveMenu);
                chat.input.clear();
                chat.visible_timer = VISIBLE_SECONDS;
                return;
            }

            let (item_parts, count) = match parse_give_count(&rest) {
                Ok(parsed) => parsed,
                Err(message) => {
                    push_message(chat, message, theme.colors.danger);
                    return;
                }
            };
            let item_name = item_parts.join(" ");

            let Some(item) = ItemId::from_name(&item_name) else {
                push_message(
                    chat,
                    format!("Unknown item: {item_name}"),
                    theme.colors.danger,
                );
                return;
            };

            if let Some(message) = give_count_error(item, count) {
                push_message(chat, message, theme.colors.danger);
                return;
            }

            let leftover = store_item(pockets, backpack, item, count);
            let given = count - leftover;
            if given > 0 {
                push_message(
                    chat,
                    format!("Gave {} x{}", item.display_name(), given),
                    theme.colors.text,
                );
            }
            if leftover > 0 {
                push_message(chat, "Pockets and backpack are full.", theme.colors.danger);
            }
        }
        "clear" => {
            pockets.clear();
            backpack.clear();
            push_message(chat, "Pockets and backpack cleared.", theme.colors.text);
        }
        "upgrade" => match rest.first().map(|value| value.to_ascii_lowercase()) {
            Some(target) if target == "capacity" => {
                if backpack.upgrade_capacity() {
                    push_message(
                        chat,
                        format!("Backpack capacity is now {}.", backpack.capacity()),
                        theme.colors.text,
                    );
                } else {
                    push_message(
                        chat,
                        "Backpack is already at maximum capacity.",
                        theme.colors.danger,
                    );
                }
            }
            _ => push_message(chat, "Usage: /upgrade capacity", theme.colors.danger),
        },
        "unlock" => {
            let Some(category) = rest.first().and_then(|name| ItemCategory::from_name(name)) else {
                push_message(
                    chat,
                    "Usage: /unlock food|materials|ores|misc|tools",
                    theme.colors.danger,
                );
                return;
            };
            if backpack.unlock_category(category) {
                push_message(
                    chat,
                    format!("Unlocked {} storage.", category.display_name()),
                    theme.colors.text,
                );
            } else {
                push_message(
                    chat,
                    "That backpack section is already unlocked.",
                    theme.colors.danger,
                );
            }
        }
        "fly" => {
            let Some(movement) = movement else {
                push_message(chat, "Flight is unavailable.", theme.colors.danger);
                return;
            };
            movement.flying = !movement.flying;
            if movement.flying {
                movement.noclip = true;
                if let Some((_, player)) = player.as_mut() {
                    player.reset_motion();
                }
                push_message(
                    chat,
                    "Flight on (Space up, Ctrl down). Collision off.",
                    theme.colors.text,
                );
            } else {
                movement.noclip = false;
                push_message(chat, "Flight off. Collision on.", theme.colors.text);
            }
        }
        "noclip" => {
            let Some(movement) = movement else {
                push_message(
                    chat,
                    "Collision control is unavailable.",
                    theme.colors.danger,
                );
                return;
            };
            movement.noclip = !movement.noclip;
            if let Some((_, player)) = player.as_mut() {
                player.reset_motion();
            }
            push_message(
                chat,
                if movement.noclip {
                    "Collision off."
                } else {
                    "Collision on."
                },
                theme.colors.text,
            );
        }
        "tp" => {
            let coordinates = match parse_tp_args(&rest) {
                Ok(coords) => coords,
                Err(message) => {
                    push_message(chat, message, theme.colors.danger);
                    return;
                }
            };

            if player.is_some() {
                pending_teleport.0 = Some(
                    Vec3::new(coordinates[0], coordinates[1], coordinates[2]) * BLOCKS_PER_METRE,
                );
                push_message(
                    chat,
                    format!(
                        "Teleported to {:.1}, {:.1}, {:.1} m",
                        coordinates[0], coordinates[1], coordinates[2]
                    ),
                    theme.colors.text,
                );
            }
        }
        "spawn" => {
            if player.is_some() {
                pending_teleport.0 = Some(spawn.0);
                let position = spawn.0 * BLOCK_SIZE;
                push_message(
                    chat,
                    format!(
                        "Returned to spawn ({:.1}, {:.1}, {:.1} m)",
                        position.x, position.y, position.z
                    ),
                    theme.colors.text,
                );
            }
        }
        "seed" => {
            push_message(
                chat,
                format!("World seed: {}", generator.0.seed),
                theme.colors.text,
            );
        }
        "pos" => {
            if let Some((transform, _)) = player.as_ref() {
                let position = transform.translation * BLOCK_SIZE;
                push_message(
                    chat,
                    format!(
                        "Position: {:.1}, {:.1}, {:.1} m",
                        position.x, position.y, position.z
                    ),
                    theme.colors.text,
                );
            }
        }
        "" => {}
        other => {
            push_message(
                chat,
                format!("Unknown command: /{other}"),
                theme.colors.danger,
            );
        }
    }
}

fn give_count_error(item: ItemId, count: u32) -> Option<String> {
    let max_stack = item.def().max_stack.max(1);
    if count == 0 {
        Some("Count must be at least 1.".to_string())
    } else if count > max_stack {
        Some(format!(
            "Count must be 1-{max_stack} for {} (max stack size).",
            item.display_name()
        ))
    } else {
        None
    }
}

pub(crate) fn parse_give_count<'a>(
    args: &'a [&'a str],
) -> Result<(&'a [&'a str], u32), &'static str> {
    if args.is_empty() {
        return Err("Usage: /give <item> [count]");
    }

    let Some(last) = args.last() else {
        return Err("Usage: /give <item> [count]");
    };

    if args.len() >= 2 && last.parse::<u32>().is_ok() {
        let count = last
            .parse::<u32>()
            .map_err(|_| "Usage: /give <item> [count]")?;
        if count == 0 {
            return Err("Count must be at least 1.");
        }
        Ok((&args[..args.len() - 1], count))
    } else if args.len() >= 2
        && (last.starts_with('-') || last.parse::<i64>().is_ok() || last.parse::<f64>().is_ok())
    {
        // A numeric-looking trailing arg is treated as an invalid count, not part of item name.
        Err("Count must be a whole number >= 1.")
    } else {
        Ok((args, 1))
    }
}

pub(crate) fn parse_tp_args(args: &[&str]) -> Result<[f32; 3], &'static str> {
    if args.len() != 3 {
        return Err("Usage: /tp <x> <y> <z> (metres)");
    }
    let mut coordinates = [0.0_f32; 3];
    for (index, value) in args.iter().enumerate() {
        let parsed = value
            .parse::<f32>()
            .map_err(|_| "Usage: /tp <x> <y> <z> (metres)")?;
        if !parsed.is_finite() {
            return Err("Coordinates must be finite numbers.");
        }
        coordinates[index] = parsed;
    }
    Ok(coordinates)
}

pub(crate) fn push_message(chat: &mut ChatState, text: impl Into<String>, color: Color) {
    chat.messages.push(ChatMessage {
        text: text.into(),
        color,
    });
    if chat.messages.len() > MAX_MESSAGES {
        chat.messages.remove(0);
    }
    chat.revision = chat.revision.wrapping_add(1);
    // Keep the log visible for a while after the chat is closed, even when the
    // message was sent while it was open.
    chat.visible_timer = VISIBLE_SECONDS;
    // A new line should be visible immediately.
    chat.scroll = 0;
}

fn scroll_chat(
    scroll: Option<Res<AccumulatedMouseScroll>>,
    keys: Res<ButtonInput<KeyCode>>,
    ui: Res<UiManager>,
    mut chat: ResMut<ChatState>,
) {
    if !ui.is_open(Overlay::Chat) {
        return;
    }

    let mut delta = 0i32;
    if let Some(scroll) = scroll {
        if scroll.delta.y > 0.0 {
            delta += 1;
        } else if scroll.delta.y < 0.0 {
            delta -= 1;
        }
    }
    if keys.just_pressed(KeyCode::PageUp) {
        delta += VISIBLE_LINES as i32;
    }
    if keys.just_pressed(KeyCode::PageDown) {
        delta -= VISIBLE_LINES as i32;
    }

    if delta != 0 {
        let max_scroll = chat.max_scroll() as i32;
        chat.scroll = (chat.scroll as i32 + delta).clamp(0, max_scroll) as usize;
    }
}

fn sync_chat_visibility(
    chat: Res<ChatState>,
    ui: Res<UiManager>,
    mut roots: Query<&mut Node, (With<ChatRoot>, Without<ChatInputRow>)>,
    mut inputs: Query<&mut Node, (With<ChatInputRow>, Without<ChatRoot>)>,
) {
    let open = ui.is_open(Overlay::Chat);
    let visible = open || chat.visible_timer > 0.0;
    let root_display = display_if(visible);
    let input_display = display_if(open);

    for mut node in &mut roots {
        if node.display != root_display {
            node.display = root_display;
        }
    }
    for mut node in &mut inputs {
        if node.display != input_display {
            node.display = input_display;
        }
    }
}

fn update_chat_input_text(chat: Res<ChatState>, mut input: Query<&mut Text, With<ChatInputText>>) {
    let Ok(mut text) = input.single_mut() else {
        return;
    };
    let desired = format!("> {}", chat.input);
    if text.0 != desired {
        text.0 = desired;
    }
}

fn render_chat_log(
    mut commands: Commands,
    mut chat: ResMut<ChatState>,
    theme: Res<UiTheme>,
    log: Query<(Entity, Option<&Children>), With<ChatLog>>,
) {
    if chat.revision == chat.rendered_revision && chat.scroll == chat.rendered_scroll {
        return;
    }
    chat.rendered_revision = chat.revision;
    chat.rendered_scroll = chat.scroll;

    let Ok((log_entity, children)) = log.single() else {
        return;
    };
    if let Some(children) = children {
        for child in children.iter() {
            commands.entity(child).despawn();
        }
    }

    let scroll = chat.scroll.min(chat.max_scroll());
    let end = chat.messages.len().saturating_sub(scroll);
    let start = end.saturating_sub(VISIBLE_LINES);
    for message in &chat.messages[start..end] {
        commands.spawn((
            Text::new(message.text.clone()),
            TextFont::from_font_size(theme.fonts.chat),
            TextColor(message.color),
            // Keep default wrapping for long lines, but use the shared shadow.
            theme.shadow,
            ChildOf(log_entity),
        ));
    }
}

fn update_scrollbar(
    chat: Res<ChatState>,
    ui: Res<UiManager>,
    mut thumbs: Query<&mut Node, With<ChatScrollbarThumb>>,
    mut tracks: Query<&mut Node, (With<ChatScrollbarTrack>, Without<ChatScrollbarThumb>)>,
) {
    let total = chat.messages.len().max(1);
    let visible = VISIBLE_LINES.min(total);
    let thumb_percent = (visible as f32 / total as f32 * 100.0).clamp(6.0, 100.0);
    let max_scroll = chat.max_scroll();
    let travel = (100.0 - thumb_percent).max(0.0);
    let top = if max_scroll == 0 {
        0.0
    } else {
        chat.scroll.min(max_scroll) as f32 / max_scroll as f32 * travel
    };

    for mut node in &mut thumbs {
        let height = Val::Percent(thumb_percent);
        let top_value = Val::Percent(top);
        if node.height != height {
            node.height = height;
        }
        if node.top != top_value {
            node.top = top_value;
        }
    }

    // The track is only meaningful while the chat is open.
    let chat_open = ui.is_open(Overlay::Chat);
    for mut node in &mut tracks {
        let desired = display_if(chat_open);
        if node.display != desired {
            node.display = desired;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_starts_closed_and_empty() {
        let chat = ChatState::default();
        assert!(chat.input.is_empty());
        assert!(chat.messages.is_empty());
        assert!(chat.history.is_empty());
        assert_eq!(chat.scroll, 0);
    }

    #[test]
    fn push_message_marks_the_log_for_redraw() {
        let mut chat = ChatState::default();
        push_message(&mut chat, "hello", UiTheme::default().colors.text);
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.revision, 1);
        assert_eq!(chat.rendered_revision, 0);
    }

    #[test]
    fn history_browses_backwards_and_forwards() {
        let mut chat = ChatState::default();
        chat.remember("first");
        chat.remember("second");
        chat.input = "draft".into();

        chat.history_previous();
        assert_eq!(chat.input, "second");
        chat.history_previous();
        assert_eq!(chat.input, "first");
        chat.history_previous();
        assert_eq!(chat.input, "first");

        chat.history_next();
        assert_eq!(chat.input, "second");
        chat.history_next();
        assert_eq!(chat.input, "draft");
    }

    #[test]
    fn attachment_item_names_are_recognised_by_commands() {
        assert_eq!(ItemId::from_name("lantern"), Some(ItemId::Lantern));
        assert_eq!(ItemId::from_name("magnet"), Some(ItemId::Magnet));
    }

    #[test]
    fn max_scroll_accounts_for_visible_lines() {
        let mut chat = ChatState::default();
        for index in 0..(VISIBLE_LINES + 3) {
            push_message(
                &mut chat,
                format!("line {index}"),
                UiTheme::default().colors.text,
            );
        }
        assert_eq!(chat.max_scroll(), 3);
    }

    #[test]
    fn parse_tp_args_rejects_invalid_and_non_finite_coordinates() {
        assert!(parse_tp_args(&[]).is_err());
        assert!(parse_tp_args(&["1", "2"]).is_err());
        assert!(parse_tp_args(&["1", "2", "3", "4"]).is_err());
        assert!(
            parse_tp_args(&["1", "abc", "3"]).is_err(),
            "invalid token must be rejected, not dropped"
        );
        assert!(parse_tp_args(&["NaN", "2", "3"]).is_err());
        assert!(parse_tp_args(&["inf", "2", "3"]).is_err());
        assert!(parse_tp_args(&["-inf", "2", "3"]).is_err());
        assert_eq!(
            parse_tp_args(&["1.5", "-2", "3.25"]).unwrap(),
            [1.5, -2.0, 3.25]
        );
    }

    #[test]
    fn parse_give_count_validates_numeric_trailing_arg() {
        let (parts, count) = parse_give_count(&["iron_ore"]).unwrap();
        assert_eq!(parts, ["iron_ore"]);
        assert_eq!(count, 1);

        let (parts, count) = parse_give_count(&["iron_ore", "4"]).unwrap();
        assert_eq!(parts, ["iron_ore"]);
        assert_eq!(count, 4);

        assert!(parse_give_count(&["iron_ore", "0"]).is_err());
        assert!(parse_give_count(&["iron_ore", "-1"]).is_err());
        assert!(parse_give_count(&["iron_ore", "1.5"]).is_err());
        assert!(parse_give_count(&["iron_ore", "999999999999"]).is_err());
    }

    #[test]
    fn give_count_rejects_over_max_stack() {
        let max = ItemId::Turf.def().max_stack;
        assert!(give_count_error(ItemId::Turf, 1).is_none());
        assert!(give_count_error(ItemId::Turf, max).is_none());
        let err =
            give_count_error(ItemId::Turf, max.saturating_add(1)).expect("over-max stack rejected");
        assert!(err.contains(&format!("1-{max}")));
        assert!(err.contains("Turf"));
    }
}
