//! KingdomTales - a peaceful voxel kingdom builder.
//!
//! Milestone 1: a walkable, editable procedural voxel world.
//!
//! Controls
//! --------
//! WASD move, Space jump (hold to keep hopping), Shift sprint (uses stamina),
//! Ctrl crouch, 1-8 select a hotbar slot, mouse wheel cycles slots. LMB mines
//! with a tool, places a block from inventory, applies the selected hammer shape,
//! or rotates with the wrench. Holding RMB with the hammer opens the shape menu;
//! otherwise RMB gathers loose clutter and picks up drops. Breaking a trunk
//! fells the tree into immediate wood drops and individual falling leaves.
//! Backpack attachment slots (in the character panel) hold physical utility
//! items such as Lantern or Magnet and keep their effects active while attached.
//! Tab opens the backpack (inventory, hands crafting, and attachments), M opens
//! the overhead map, Enter opens chat and commands, Esc opens the
//! pause menu (settings, character perks and reserved map/debug options), F3
//! toggles the debug overlay, F9 saves, F10 loads, F12 saves a screenshot and H
//! shows controls help. The HUD shows level/XP, coins, compass, stamina,
//! hunger/hydration, Insight and pickup notifications. A white outline marks the
//! block under the crosshair.

mod attachments;
mod backpack;
mod chat;
mod construction;
mod crafting;
mod felling;
mod held_item;
mod insight;
mod interaction;
mod items;
mod map;
mod notifications;
mod persistence;
mod player;
mod pockets;
mod progression;
mod rewards;
mod screenshots;
mod settings;
mod sky;
mod structures;
mod tools;
mod ui;
mod ui_manager;
mod world;

use bevy::light::CascadeShadowConfig;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, WindowMode, WindowResolution};

use settings::{GameSettings, SunLight};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "KingdomTales".into(),
                resolution: WindowResolution::new(
                    settings::WINDOWED_WIDTH,
                    settings::WINDOWED_HEIGHT,
                ),
                mode: WindowMode::BorderlessFullscreen(MonitorSelection::Primary),
                present_mode: PresentMode::AutoNoVsync,
                resizable: true,
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(
            settings::SURFACE_FOG_COLOR[0],
            settings::SURFACE_FOG_COLOR[1],
            settings::SURFACE_FOG_COLOR[2],
        )))
        .insert_resource(GlobalAmbientLight {
            color: Color::srgb(
                settings::SURFACE_AMBIENT_COLOR[0],
                settings::SURFACE_AMBIENT_COLOR[1],
                settings::SURFACE_AMBIENT_COLOR[2],
            ),
            brightness: settings::SURFACE_AMBIENT_BRIGHTNESS,
            ..default()
        })
        .add_plugins((
            ui_manager::UiManagerPlugin,
            settings::SettingsPlugin,
            items::ItemsPlugin,
            progression::ProgressionPlugin,
            notifications::NotificationPlugin,
            insight::InsightPlugin,
            pockets::PocketsPlugin,
            backpack::BackpackPlugin,
            construction::ConstructionPlugin,
            crafting::CraftingPlugin,
        ))
        .add_plugins(structures::StructuresPlugin)
        .add_plugins((
            persistence::PersistencePlugin,
            world::WorldPlugin,
            player::PlayerPlugin,
            attachments::AttachmentPlugin,
            held_item::HeldItemPlugin,
            screenshots::ScreenshotPlugin,
            interaction::InteractionPlugin,
            chat::ChatPlugin,
            map::MapPlugin,
            ui::UiPlugin,
        ))
        .add_systems(Startup, setup_lighting)
        .run();
}

fn setup_lighting(mut commands: Commands, settings: Res<GameSettings>) {
    let mut light = DirectionalLight {
        color: Color::srgb(1.0, 0.97, 0.90),
        illuminance: settings::SURFACE_SUN_ILLUMINANCE,
        shadow_maps_enabled: true,
        ..default()
    };
    let mut cascades = CascadeShadowConfig::default();
    settings::apply_shadow_quality(&mut light, &mut cascades, settings.shadow_quality);

    commands.spawn((
        Name::new("Sun"),
        SunLight,
        light,
        Transform::from_rotation(Quat::from_rotation_y(-0.62) * Quat::from_rotation_x(-0.85)),
        cascades,
    ));
}
