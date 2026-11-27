//! Backpack-attached utility items.
//!
//! Lantern: provides warm light when in any backpack attachment slot. If in the
//! offhand slot (attachment 0), a 3D lantern model is shown in the player's left
//! hand; if in backpack slots (1-3), no hand model is shown.
//! Magnet: pulls/picks up drops when attached (handled in interaction).

use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot};

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

/// Marks offhand GLB mesh children whose material is already a private clone.
#[derive(Component)]
struct OffhandLanternViewMaterial;

fn spawn_offhand_lantern(commands: &mut Commands, asset_server: &AssetServer) -> Entity {
    let scene: Handle<WorldAsset> = asset_server.load("models/items/lantern.glb#Scene0");
    commands
        .spawn((
            OffhandLanternModel,
            Transform::from_translation(OFFHAND_LANTERN_POS)
                .with_rotation(Quat::from_euler(EulerRot::XYZ, -0.08, 0.25, 0.05))
                .with_scale(Vec3::splat(1.15)),
            Visibility::Hidden,
            WorldAssetRoot(scene),
            NotShadowCaster,
            NotShadowReceiver,
            Name::new("OffhandLantern"),
        ))
        .id()
}

fn update_lantern(
    mut commands: Commands,
    backpack: Res<Backpack>,
    player: Query<&GlobalTransform, With<crate::player::Player>>,
    camera: Query<(Entity, &GlobalTransform), With<PlayerCamera>>,
    asset_server: Res<AssetServer>,
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
        let root = spawn_offhand_lantern(&mut commands, &asset_server);
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

/// Prepare offhand lantern children with the same mostly-unlit treatment used
/// by held items. The lantern's own point light is extremely close to its model,
/// so the shared GLB material would otherwise flash as the camera moves.
fn prepare_offhand_lantern_viewmodel(
    mut commands: Commands,
    roots: Query<Entity, With<OffhandLanternModel>>,
    children: Query<&Children>,
    mesh_entities: Query<Entity, With<Mesh3d>>,
    mesh_mats: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>),
        Without<OffhandLanternViewMaterial>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut stack: Vec<Entity> = roots.iter().collect();
    while let Some(entity) = stack.pop() {
        if mesh_entities.get(entity).is_ok() {
            if let Ok((entity, handle)) = mesh_mats.get(entity)
                && let Some(source) = materials.get(handle.0.id())
            {
                let mut material = source.clone();
                crate::held_item::apply_viewmodel_material(&mut material);
                let view_handle = materials.add(material);
                commands.entity(entity).insert((
                    MeshMaterial3d(view_handle),
                    NotShadowCaster,
                    NotShadowReceiver,
                    OffhandLanternViewMaterial,
                ));
            } else {
                commands
                    .entity(entity)
                    .insert((NotShadowCaster, NotShadowReceiver));
            }
        }
        if let Ok(kids) = children.get(entity) {
            for &kid in kids {
                stack.push(kid);
            }
        }
    }
}

pub struct AttachmentPlugin;

impl Plugin for AttachmentPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            (update_lantern, prepare_offhand_lantern_viewmodel)
                .chain()
                .after(bevy::transform::TransformSystems::Propagate),
        );
    }
}
