//! KingdomTales - a peaceful voxel kingdom builder.
//!
//! Core loop: a walkable, editable procedural voxel world.
//!
//! Controls
//! --------
//! WASD move, Space jump (hold to keep hopping), Shift sprint (uses stamina),
//! Ctrl crouch, 1-8 select a hotbar slot, mouse wheel cycles slots. LMB mines
//! with a tool, applies the selected hammer shape, or rotates with the wrench.
//! RMB places the selected block or structure, interacts, gathers loose clutter,
//! picks up drops, or holds the hammer shape menu. Breaking a trunk fells the
//! tree into immediate wood drops and individual falling leaves. Backpack
//! attachment slots (in the character panel) hold physical utility items such as
//! Lantern or Magnet and keep their effects active while attached. Tab opens the
//! backpack (inventory, hand crafting, and attachments), M opens the overhead
//! map, Enter opens chat and commands, Esc opens the pause menu (Resume,
//! Settings, Quit), F3 toggles the debug overlay, F9 saves, F10 loads, F12 saves
//! a screenshot and H shows controls help. The HUD shows level/XP, coins,
//! compass, stamina, hunger/hydration, Insight and pickup notifications. A white
//! outline marks the block under the crosshair.

mod attachments;
mod backpack;
mod chat;
mod clouds;
mod construction;
mod crafting;
mod day_night;
mod felling;
mod held_item;
mod insight;
mod interaction;
mod items;
mod light_shafts;
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

use bevy::gltf::GltfPlugin;
use bevy::image::{ImagePlugin, ImageSamplerDescriptor};
use bevy::light::CascadeShadowConfig;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, WindowMode, WindowResolution};

use settings::{GameSettings, MoonLight, SunLight, surface_preset_for_cycle};

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
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
                })
                .set(ImagePlugin::default_nearest())
                .set(GltfPlugin {
                    default_sampler: ImageSamplerDescriptor::nearest(),
                    ..default()
                }),
        )
        .insert_resource(ClearColor(Color::srgb(0.55, 0.72, 0.88)))
        .insert_resource(GlobalAmbientLight::default())
        .add_plugins((
            ui_manager::UiManagerPlugin,
            day_night::DayNightPlugin,
            sky::SkyPlugin,
            light_shafts::LightShaftsPlugin,
            clouds::CloudsPlugin,
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

fn setup_lighting(
    mut commands: Commands,
    settings: Res<GameSettings>,
    cycle: Res<day_night::DayNightCycle>,
) {
    let preset = surface_preset_for_cycle(&cycle);
    let mut sun = DirectionalLight {
        color: Color::srgb(
            preset.sun_color[0],
            preset.sun_color[1],
            preset.sun_color[2],
        ),
        illuminance: preset.sun_illuminance,
        shadow_maps_enabled: settings.shadow_quality != settings::ShadowQuality::Off,
        ..default()
    };
    let mut cascades = CascadeShadowConfig::default();
    settings::apply_shadow_quality(&mut sun, &mut cascades, settings.shadow_quality);

    commands.spawn((
        Name::new("Sun"),
        SunLight,
        sun,
        Transform::from_translation(Vec3::ZERO).looking_to(cycle.sun_direction(), Vec3::Y),
        cascades,
    ));

    commands.spawn((
        Name::new("Moon"),
        MoonLight,
        DirectionalLight {
            color: Color::srgb(
                preset.moon_color[0],
                preset.moon_color[1],
                preset.moon_color[2],
            ),
            illuminance: preset.moon_illuminance,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_translation(Vec3::ZERO).looking_to(cycle.moon_direction(), Vec3::Y),
    ));
}
