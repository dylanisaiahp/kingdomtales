//! Simple first-person held-item viewmodel.
//!
//! Shows the currently selected pocket item in the right hand and plays a light
//! bob/swing animation. This is a first-pass 3D representation; backpack
//! attachment visuals (lantern/magnet) remain owned by `staff`.

use bevy::prelude::*;

use crate::interaction::MiningState;
use crate::items::{ItemId, selected_item, tool_for_item};
use crate::player::{CursorCapture, Player, PlayerCamera};
use crate::pockets::Pockets;

const HAND_BASE_POS: Vec3 = Vec3::new(0.42, -0.30, -0.62);
const HAND_BASE_ROT: Vec3 = Vec3::new(-0.18, -0.32, 0.08);
const ITEM_SCALE: f32 = 0.22;

#[derive(Resource, Default)]
struct HeldItemState {
    displayed: Option<ItemId>,
    swing: f32,
    swing_phase: f32,
    bob_phase: f32,
}

#[derive(Component)]
struct HeldItemRoot;

#[derive(Component)]
struct HeldItemModel;

pub struct HeldItemPlugin;

impl Plugin for HeldItemPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HeldItemState>().add_systems(
            PostUpdate,
            (ensure_held_item_root, update_held_item_viewmodel)
                .chain()
                .after(bevy::transform::TransformSystems::Propagate),
        );
    }
}

fn ensure_held_item_root(
    mut commands: Commands,
    camera: Query<Entity, With<PlayerCamera>>,
    roots: Query<Entity, With<HeldItemRoot>>,
) {
    let Ok(camera_entity) = camera.single() else {
        return;
    };
    if !roots.is_empty() {
        return;
    }
    commands.spawn((
        HeldItemRoot,
        Transform::from_translation(HAND_BASE_POS).with_rotation(Quat::from_euler(
            EulerRot::XYZ,
            HAND_BASE_ROT.x,
            HAND_BASE_ROT.y,
            HAND_BASE_ROT.z,
        )),
        Visibility::Hidden,
        ChildOf(camera_entity),
        Name::new("HeldItemRoot"),
    ));
}

#[allow(clippy::too_many_arguments)]
fn update_held_item_viewmodel(
    mut commands: Commands,
    time: Res<Time>,
    pockets: Res<Pockets>,
    mining: Res<MiningState>,
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    player: Query<&Player>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut state: ResMut<HeldItemState>,
    mut roots: Query<(Entity, &mut Transform, &mut Visibility), With<HeldItemRoot>>,
    models: Query<Entity, With<HeldItemModel>>,
) {
    let Ok((root_entity, mut root_transform, mut root_visibility)) = roots.single_mut() else {
        return;
    };

    let item = selected_item(&pockets);
    if item != state.displayed {
        for entity in &models {
            commands.entity(entity).despawn();
        }
        if let Some(item) = item {
            spawn_item_model(
                &mut commands,
                root_entity,
                &mut meshes,
                &mut materials,
                item,
            );
        }
        state.displayed = item;
    }

    *root_visibility = if item.is_some() {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };

    let dt = time.delta_secs();
    let moving = player
        .single()
        .is_ok_and(|p| p.velocity.xz().length_squared() > 0.05 && p.grounded);
    let bob_speed = if moving { 10.0 } else { 2.0 };
    state.bob_phase += dt * bob_speed;

    let active = capture.grabbed && !capture.just_grabbed;
    let swinging =
        mining.target.is_some() || (active && mouse.pressed(MouseButton::Left) && item.is_some());
    if swinging {
        // Loop the swing while held/mining so long actions animate continuously.
        state.swing_phase = (state.swing_phase + dt * 6.5) % std::f32::consts::TAU;
        state.swing = (state.swing + dt * 10.0).min(1.0);
    } else {
        state.swing_phase = 0.0;
        state.swing = (state.swing - dt * 8.0).max(0.0);
    }

    let bob_amp = if moving { 0.018 } else { 0.006 };
    let bob_x = (state.bob_phase * 0.5).sin() * bob_amp * 0.6;
    let bob_y = (state.bob_phase).sin().abs() * bob_amp;
    let swing_t = if swinging {
        ((state.swing_phase.sin() * 0.5 + 0.5) * state.swing).clamp(0.0, 1.0)
    } else {
        (state.swing * std::f32::consts::PI).sin()
    };
    let swing_rot = Quat::from_euler(
        EulerRot::XYZ,
        -swing_t * 0.55,
        swing_t * 0.12,
        -swing_t * 0.18,
    );
    let swing_pos = Vec3::new(-swing_t * 0.04, -swing_t * 0.03, swing_t * 0.08);

    let base_rot = Quat::from_euler(
        EulerRot::XYZ,
        HAND_BASE_ROT.x,
        HAND_BASE_ROT.y,
        HAND_BASE_ROT.z,
    );
    root_transform.translation = HAND_BASE_POS + Vec3::new(bob_x, bob_y, 0.0) + swing_pos;
    root_transform.rotation = base_rot * swing_rot;
}

fn spawn_item_model(
    commands: &mut Commands,
    root: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    item: ItemId,
) {
    let color = item_color(item);
    let material = materials.add(StandardMaterial {
        base_color: color,
        perceptual_roughness: 0.8,
        metallic: 0.0,
        ..default()
    });

    if let Some(block) = item.def().place_block {
        let mesh = meshes.add(Cuboid::new(ITEM_SCALE, ITEM_SCALE, ITEM_SCALE));
        commands.spawn((
            HeldItemModel,
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_xyz(0.0, 0.0, 0.0),
            ChildOf(root),
        ));
        let _ = block;
        return;
    }

    if let Some(tool) = tool_for_item(item) {
        spawn_tool_model(commands, root, meshes, materials, tool);
        return;
    }

    spawn_simple_item_model(commands, root, meshes, materials, item, material);
}

fn spawn_tool_model(
    commands: &mut Commands,
    root: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    tool: crate::tools::ToolKind,
) {
    let handle_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.46, 0.32, 0.18),
        perceptual_roughness: 0.85,
        ..default()
    });
    let head_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.64, 0.68),
        perceptual_roughness: 0.45,
        metallic: 0.15,
        ..default()
    });
    let handle_mesh = meshes.add(Cuboid::new(0.045, 0.34, 0.045));
    commands.spawn((
        HeldItemModel,
        Mesh3d(handle_mesh),
        MeshMaterial3d(handle_mat),
        Transform::from_xyz(0.0, 0.02, 0.0).with_rotation(Quat::from_rotation_z(0.18)),
        ChildOf(root),
    ));

    let (head_size, head_pos, head_rot) = match tool {
        crate::tools::ToolKind::Pickaxe => (
            Vec3::new(0.22, 0.045, 0.045),
            Vec3::new(0.0, 0.17, 0.0),
            Quat::from_rotation_z(0.18),
        ),
        crate::tools::ToolKind::Axe => (
            Vec3::new(0.11, 0.10, 0.04),
            Vec3::new(0.05, 0.14, 0.0),
            Quat::from_rotation_z(0.18),
        ),
        crate::tools::ToolKind::Hammer => (
            Vec3::new(0.12, 0.07, 0.07),
            Vec3::new(0.0, 0.17, 0.0),
            Quat::IDENTITY,
        ),
        crate::tools::ToolKind::Shovel => (
            Vec3::new(0.09, 0.11, 0.03),
            Vec3::new(0.0, 0.19, 0.0),
            Quat::IDENTITY,
        ),
        crate::tools::ToolKind::Hoe => (
            Vec3::new(0.14, 0.035, 0.035),
            Vec3::new(0.05, 0.16, 0.0),
            Quat::from_rotation_z(0.18),
        ),
        crate::tools::ToolKind::Scythe => (
            Vec3::new(0.16, 0.03, 0.03),
            Vec3::new(0.06, 0.12, 0.0),
            Quat::from_rotation_z(0.7),
        ),
        crate::tools::ToolKind::Wrench => (
            Vec3::new(0.08, 0.08, 0.04),
            Vec3::new(0.0, 0.17, 0.0),
            Quat::IDENTITY,
        ),
        crate::tools::ToolKind::StoneHandAxe => (
            Vec3::new(0.09, 0.08, 0.04),
            Vec3::new(0.04, 0.14, 0.0),
            Quat::from_rotation_z(0.18),
        ),
        crate::tools::ToolKind::BarkScraper => (
            Vec3::new(0.05, 0.10, 0.03),
            Vec3::new(0.01, 0.17, 0.0),
            Quat::from_rotation_z(0.35),
        ),
        crate::tools::ToolKind::SmallStoneHammer => (
            Vec3::new(0.10, 0.08, 0.08),
            Vec3::new(0.0, 0.17, 0.0),
            Quat::IDENTITY,
        ),
    };
    let head_mesh = meshes.add(Cuboid::new(head_size.x, head_size.y, head_size.z));
    commands.spawn((
        HeldItemModel,
        Mesh3d(head_mesh),
        MeshMaterial3d(head_mat),
        Transform::from_translation(head_pos).with_rotation(head_rot),
        ChildOf(root),
    ));
}

fn spawn_simple_item_model(
    commands: &mut Commands,
    root: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    item: ItemId,
    material: Handle<StandardMaterial>,
) {
    match item {
        ItemId::StoneTippedStick => {
            let stick_mat = materials.add(StandardMaterial {
                base_color: Color::srgb(0.55, 0.38, 0.20),
                perceptual_roughness: 0.85,
                ..default()
            });
            let stone_mat = materials.add(StandardMaterial {
                base_color: Color::srgb(0.58, 0.58, 0.56),
                perceptual_roughness: 0.7,
                ..default()
            });
            let stick = meshes.add(Cuboid::new(0.04, 0.30, 0.04));
            let tip = meshes.add(Cuboid::new(0.05, 0.07, 0.05));
            commands.spawn((
                HeldItemModel,
                Mesh3d(stick),
                MeshMaterial3d(stick_mat),
                Transform::from_xyz(0.0, -0.01, 0.0).with_rotation(Quat::from_rotation_z(0.35)),
                ChildOf(root),
            ));
            commands.spawn((
                HeldItemModel,
                Mesh3d(tip),
                MeshMaterial3d(stone_mat),
                Transform::from_xyz(-0.05, 0.14, 0.0).with_rotation(Quat::from_rotation_z(0.35)),
                ChildOf(root),
            ));
        }
        ItemId::Stick => {
            let mesh = meshes.add(Cuboid::new(0.04, 0.28, 0.04));
            commands.spawn((
                HeldItemModel,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_rotation(Quat::from_rotation_z(0.35)),
                ChildOf(root),
            ));
        }
        ItemId::Fiber | ItemId::Cordage | ItemId::Leaves | ItemId::Bark => {
            let mesh = meshes.add(Cuboid::new(0.14, 0.03, 0.14));
            commands.spawn((
                HeldItemModel,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_rotation(Quat::from_rotation_x(0.2)),
                ChildOf(root),
            ));
        }
        ItemId::SharpenedStone
        | ItemId::GranitePebble
        | ItemId::SlatePebble
        | ItemId::SandstonePebble
        | ItemId::LimestonePebble => {
            let mesh = meshes.add(Cuboid::new(0.10, 0.06, 0.12));
            commands.spawn((
                HeldItemModel,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
                ChildOf(root),
            ));
        }
        ItemId::Lantern | ItemId::Magnet => {
            let mesh = meshes.add(Cuboid::new(0.10, 0.10, 0.10));
            commands.spawn((
                HeldItemModel,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
                ChildOf(root),
            ));
        }
        _ => {
            let mesh = meshes.add(Cuboid::new(0.12, 0.12, 0.12));
            commands.spawn((
                HeldItemModel,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
                ChildOf(root),
            ));
        }
    }
}

fn item_color(item: ItemId) -> Color {
    if let Some(block) = item.def().place_block {
        let [r, g, b] = block.srgb();
        return Color::srgb(r, g, b);
    }
    match item {
        ItemId::Stick | ItemId::StoneTippedStick => Color::srgb(0.55, 0.38, 0.20),
        ItemId::Fiber | ItemId::Cordage => Color::srgb(0.62, 0.66, 0.38),
        ItemId::Bark => Color::srgb(0.42, 0.30, 0.20),
        ItemId::SharpenedStone
        | ItemId::GranitePebble
        | ItemId::SlatePebble
        | ItemId::SandstonePebble
        | ItemId::LimestonePebble => Color::srgb(0.55, 0.55, 0.52),
        ItemId::Lantern => Color::srgb(0.95, 0.82, 0.45),
        ItemId::Magnet => Color::srgb(0.55, 0.20, 0.22),
        _ => Color::srgb(0.7, 0.7, 0.7),
    }
}
