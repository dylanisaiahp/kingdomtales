//! Backpack-attached utility items.
//!
//! Lantern: provides warm light when in any backpack attachment slot. If in the
//! offhand slot (attachment 0), a 3D lantern model is shown in the player's left
//! hand; if in backpack slots (1-3), no hand model is shown.
//! Magnet: pulls/picks up drops when attached (handled in interaction).

use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use crate::backpack::Backpack;
use crate::items::ItemId;
use crate::player::PlayerCamera;
use crate::world::chunk::BLOCKS_PER_METRE;

const LANTERN_INTENSITY: f32 = 3_600_000.0;
const LANTERN_RANGE: f32 = 56.0 * BLOCKS_PER_METRE;
const LANTERN_RADIUS: f32 = 0.5 * BLOCKS_PER_METRE;
const LANTERN_COLOR: Color = Color::srgb(1.0, 0.86, 0.58);
const BACKPACK_LANTERN_OFFSET: Vec3 = Vec3::new(0.0, 0.9, 0.35);
/// Camera-local offhand lantern position.
const OFFHAND_LANTERN_POS: Vec3 = Vec3::new(-0.38, -0.28, -0.58);

#[derive(Component)]
struct BackpackLanternLight;

#[derive(Component)]
struct OffhandLanternModel;

fn spawn_offhand_lantern(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> Entity {
    let frame_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.35, 0.27, 0.18),
        unlit: true,
        ..default()
    });
    let glass_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.9, 0.55, 0.2),
        alpha_mode: AlphaMode::Blend,
        emissive: LinearRgba::new(1.2, 1.0, 0.35, 1.0),
        perceptual_roughness: 0.1,
        ..default()
    });
    let core_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.93, 0.15),
        emissive: LinearRgba::new(10.0, 8.0, 0.6, 1.0),
        emissive_exposure_weight: 0.0,
        unlit: true,
        ..default()
    });
    let glow_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.88, 0.2, 0.25),
        emissive: LinearRgba::new(3.0, 2.4, 0.2, 1.0),
        emissive_exposure_weight: 0.0,
        alpha_mode: AlphaMode::Add,
        unlit: true,
        cull_mode: None,
        ..default()
    });

    let cap = meshes.add(Cuboid::new(0.15, 0.025, 0.15));
    let post = meshes.add(Cuboid::new(0.018, 0.16, 0.018));
    let glass = meshes.add(Cuboid::new(0.105, 0.12, 0.105));
    let core = meshes.add(Cuboid::new(0.065, 0.085, 0.065));
    let glow = meshes.add(Cuboid::new(0.085, 0.105, 0.085));
    let handle = meshes.add(Cuboid::new(0.025, 0.07, 0.07));

    let root = commands
        .spawn((
            OffhandLanternModel,
            Transform::from_translation(OFFHAND_LANTERN_POS).with_rotation(Quat::from_euler(
                EulerRot::XYZ,
                -0.08,
                0.25,
                0.05,
            )),
            Visibility::Hidden,
            Name::new("OffhandLantern"),
        ))
        .id();

    for y in [0.085_f32, -0.105] {
        commands.spawn((
            Mesh3d(cap.clone()),
            MeshMaterial3d(frame_mat.clone()),
            Transform::from_xyz(0.0, y, 0.0),
            NotShadowCaster,
            ChildOf(root),
        ));
    }
    for (x, z) in [
        (-0.058, -0.058),
        (0.058, -0.058),
        (-0.058, 0.058),
        (0.058, 0.058),
    ] {
        commands.spawn((
            Mesh3d(post.clone()),
            MeshMaterial3d(frame_mat.clone()),
            Transform::from_xyz(x, -0.01, z),
            NotShadowCaster,
            ChildOf(root),
        ));
    }
    commands.spawn((
        Mesh3d(glass),
        MeshMaterial3d(glass_mat),
        Transform::from_xyz(0.0, -0.01, 0.0),
        NotShadowCaster,
        ChildOf(root),
    ));
    commands.spawn((
        Mesh3d(glow),
        MeshMaterial3d(glow_mat),
        Transform::from_xyz(0.0, -0.01, 0.0),
        NotShadowCaster,
        ChildOf(root),
    ));
    commands.spawn((
        Mesh3d(core),
        MeshMaterial3d(core_mat),
        Transform::from_xyz(0.0, -0.01, 0.0),
        NotShadowCaster,
        ChildOf(root),
    ));
    commands.spawn((
        Mesh3d(handle),
        MeshMaterial3d(frame_mat),
        Transform::from_xyz(0.0, 0.135, 0.0),
        NotShadowCaster,
        ChildOf(root),
    ));
    root
}

#[allow(clippy::too_many_arguments)]
fn update_lantern(
    mut commands: Commands,
    backpack: Res<Backpack>,
    player: Query<&GlobalTransform, With<crate::player::Player>>,
    camera: Query<(Entity, &GlobalTransform), With<PlayerCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut lights: Query<(Entity, &mut PointLight, &mut Transform), With<BackpackLanternLight>>,
    mut models: Query<&mut Visibility, With<OffhandLanternModel>>,
) {
    let any_lantern = backpack.has_attachment_item(ItemId::Lantern);
    let offhand_lantern = backpack
        .attachments()
        .first()
        .and_then(|s| *s)
        .is_some_and(|s| s.item == ItemId::Lantern);

    let Ok(player_tf) = player.single() else {
        return;
    };
    let backpack_pos = player_tf.translation() + BACKPACK_LANTERN_OFFSET;

    // Light: follows offhand model when in offhand, otherwise backpack.
    let light_pos = if offhand_lantern {
        camera
            .single()
            .ok()
            .map(|(_, tf)| tf.transform_point(OFFHAND_LANTERN_POS))
            .unwrap_or(backpack_pos)
    } else {
        backpack_pos
    };

    if lights.is_empty() {
        commands.spawn((
            BackpackLanternLight,
            PointLight {
                color: LANTERN_COLOR,
                intensity: if any_lantern { LANTERN_INTENSITY } else { 0.0 },
                range: LANTERN_RANGE,
                radius: LANTERN_RADIUS,
                ..default()
            },
            Transform::from_translation(light_pos),
        ));
    } else {
        for (_e, mut light, mut tf) in &mut lights {
            let desired = if any_lantern { LANTERN_INTENSITY } else { 0.0 };
            if light.intensity != desired {
                light.intensity = desired;
            }
            tf.translation = light_pos;
        }
    }

    // Offhand model: only when lantern is in attachment slot 0.
    if models.is_empty()
        && let Ok((cam_entity, _)) = camera.single()
    {
        let root = spawn_offhand_lantern(&mut commands, &mut meshes, &mut materials);
        commands.entity(cam_entity).add_child(root);
    }
    for mut vis in &mut models {
        let desired = if offhand_lantern {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != desired {
            *vis = desired;
        }
    }
}

pub struct AttachmentPlugin;

impl Plugin for AttachmentPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            update_lantern.after(bevy::transform::TransformSystems::Propagate),
        );
    }
}
