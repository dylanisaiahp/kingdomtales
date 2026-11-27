//! First-person held-item viewmodel.
//!
//! Data-driven item presentation: 32x pixel-art textures, textured 3D models
//! built from simple parts, and tool-specific mining animations. The Stone Hand
//! Axe is the visual standard for primitive tools.

use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot};

use crate::interaction::MiningState;
use crate::items::{ItemId, selected_item, tool_for_item};
use crate::player::{CursorCapture, Player, PlayerCamera};
use crate::pockets::Pockets;

const HAND_BASE_POS: Vec3 = Vec3::new(0.42, -0.30, -0.62);
const HAND_BASE_ROT: Vec3 = Vec3::new(-0.18, -0.32, 0.08);

#[derive(Resource, Default)]
struct HeldItemState {
    displayed: Option<ItemId>,
    bob_phase: f32,
    swing_phase: f32,
    swing_amount: f32,
    /// 0..1 normalized mining animation time, loops while mining.
    mine_phase: f32,
    mine_active: bool,
}

#[derive(Component)]
struct HeldItemRoot;

#[derive(Component)]
struct HeldItemModel;

/// Marks mesh entities whose material has already been switched to a private
/// viewmodel-safe clone, so GLB scene children are not cloned every frame.
#[derive(Component)]
struct HeldItemViewMaterial;

#[derive(Component, Clone, Copy)]
struct HeldPart {
    base_translation: Vec3,
    base_rotation: Quat,
    base_scale: Vec3,
}

pub struct HeldItemPlugin;

impl Plugin for HeldItemPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HeldItemState>().add_systems(
            PostUpdate,
            (
                ensure_held_item_root,
                update_held_item_viewmodel,
                propagate_held_item_shadow_flags,
                prepare_held_item_viewmodel_materials,
            )
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
        Transform::from_translation(HAND_BASE_POS).with_rotation(base_hand_rot()),
        Visibility::Hidden,
        ChildOf(camera_entity),
        Name::new("HeldItemRoot"),
    ));
}

fn base_hand_rot() -> Quat {
    Quat::from_euler(
        EulerRot::XYZ,
        HAND_BASE_ROT.x,
        HAND_BASE_ROT.y,
        HAND_BASE_ROT.z,
    )
}

#[allow(clippy::too_many_arguments)]
fn update_held_item_viewmodel(
    mut commands: Commands,
    time: Res<Time>,
    asset_server: Res<AssetServer>,
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
    mut parts: Query<(&HeldPart, &mut Transform), Without<HeldItemRoot>>,
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
                &asset_server,
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
    let is_mining = mining.target.is_some();
    let swinging = is_mining || (active && mouse.pressed(MouseButton::Left) && item.is_some());
    if swinging {
        state.swing_phase = (state.swing_phase + dt * 6.5) % std::f32::consts::TAU;
        state.swing_amount = (state.swing_amount + dt * 10.0).min(1.0);
    } else {
        state.swing_phase = 0.0;
        state.swing_amount = (state.swing_amount - dt * 8.0).max(0.0);
    }

    if is_mining {
        state.mine_active = true;
        let speed = mining_anim_speed(item);
        state.mine_phase = (state.mine_phase + dt * speed) % 1.0;
    } else if state.mine_active {
        // quick recovery
        state.mine_phase += dt * 2.0;
        if state.mine_phase >= 1.0 {
            state.mine_phase = 0.0;
            state.mine_active = false;
        }
    }

    let bob_amp = if moving { 0.018 } else { 0.006 };
    let bob_x = (state.bob_phase * 0.5).sin() * bob_amp * 0.6;
    let bob_y = state.bob_phase.sin().abs() * bob_amp;

    let (anim_pos, anim_rot) = if state.mine_active || is_mining {
        mining_animation(item, state.mine_phase)
    } else {
        let swing_t = if swinging {
            ((state.swing_phase.sin() * 0.5 + 0.5) * state.swing_amount).clamp(0.0, 1.0)
        } else {
            (state.swing_amount * std::f32::consts::PI).sin()
        };
        (
            Vec3::new(-swing_t * 0.04, -swing_t * 0.03, swing_t * 0.08),
            Quat::from_euler(
                EulerRot::XYZ,
                -swing_t * 0.55,
                swing_t * 0.12,
                -swing_t * 0.18,
            ),
        )
    };

    // For the Stone Hand Axe the model origin is at the bottom of the handle
    // (grip pivot). Place that pivot at the hand/grip location and rotate the
    // root around it so the whole axe swings as one rigid object.
    let grip_offset = if item == Some(ItemId::StoneHandAxe) {
        STONE_HAND_AXE_GRIP_OFFSET
    } else {
        Vec3::ZERO
    };
    let base_rot = base_hand_rot();
    root_transform.translation =
        HAND_BASE_POS + Vec3::new(bob_x, bob_y, 0.0) + grip_offset + anim_pos;
    root_transform.rotation = base_rot * anim_rot;

    // Apply subtle per-part follow for feel (currently identity; data-driven hooks ready).
    for (part, mut transform) in &mut parts {
        transform.translation = part.base_translation;
        transform.rotation = part.base_rotation;
        transform.scale = part.base_scale;
    }
}

fn mining_anim_speed(item: Option<ItemId>) -> f32 {
    match item.and_then(tool_for_item) {
        Some(crate::tools::ToolKind::Axe | crate::tools::ToolKind::StoneHandAxe) => 1.35,
        Some(crate::tools::ToolKind::Pickaxe) => 1.55,
        Some(crate::tools::ToolKind::Shovel) => 1.25,
        Some(crate::tools::ToolKind::Hammer | crate::tools::ToolKind::SmallStoneHammer) => 1.2,
        _ => 1.5,
    }
}

/// Returns (translation offset, rotation offset) for the held-item root.
fn mining_animation(item: Option<ItemId>, t: f32) -> (Vec3, Quat) {
    let t = t.fract();
    match item.and_then(tool_for_item) {
        Some(crate::tools::ToolKind::Axe | crate::tools::ToolKind::StoneHandAxe) => {
            axe_chop_anim(t)
        }
        Some(crate::tools::ToolKind::Pickaxe) => pickaxe_strike_anim(t),
        Some(crate::tools::ToolKind::Shovel) => shovel_dig_anim(t),
        Some(crate::tools::ToolKind::Hammer | crate::tools::ToolKind::SmallStoneHammer) => {
            hammer_strike_anim(t)
        }
        _ => generic_swing_anim(t),
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let x = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn axe_chop_anim(t: f32) -> (Vec3, Quat) {
    // Simple sideways chop in camera-local held-item root space (+X right, +Y up, -Z forward).
    // Primary axis: Z rotation around the handle-bottom grip pivot.
    // Phases: draw back/outward to the right, then fast chop left toward the target, brief recoil, recover.
    let rot_z = if t < 0.30 {
        // Draw back: cock axe out to the right (negative Z) and slightly up.
        let a = smoothstep(0.0, 0.30, t);
        -0.60 * a
    } else if t < 0.48 {
        // Chop: fast swing left/toward target (positive Z).
        let a = smoothstep(0.30, 0.48, t);
        -0.60 + 1.50 * a
    } else if t < 0.58 {
        // Brief impact/recoil.
        let a = smoothstep(0.48, 0.58, t);
        0.90 - 0.10 * a
    } else {
        // Recover to rest.
        let a = smoothstep(0.58, 1.0, t);
        0.80 * (1.0 - a)
    };
    (Vec3::ZERO, Quat::from_rotation_z(rot_z))
}

fn pickaxe_strike_anim(t: f32) -> (Vec3, Quat) {
    let (pos, rot) = if t < 0.40 {
        let a = smoothstep(0.0, 0.40, t);
        (
            Vec3::new(0.0, 0.08 * a, -0.08 * a),
            Vec3::new(-0.55 * a, 0.0, 0.08 * a),
        )
    } else if t < 0.58 {
        let a = smoothstep(0.40, 0.58, t);
        (
            Vec3::new(0.0, 0.08 - 0.14 * a, -0.08 + 0.20 * a),
            Vec3::new(-0.55 + 1.25 * a, 0.0, 0.08 - 0.12 * a),
        )
    } else {
        let a = smoothstep(0.58, 1.0, t);
        (
            Vec3::new(0.0, -0.06 * (1.0 - a), 0.12 * (1.0 - a)),
            Vec3::new(0.70 * (1.0 - a), 0.0, -0.04 * (1.0 - a)),
        )
    };
    (pos, Quat::from_euler(EulerRot::XYZ, rot.x, rot.y, rot.z))
}

fn shovel_dig_anim(t: f32) -> (Vec3, Quat) {
    let (pos, rot) = if t < 0.35 {
        let a = smoothstep(0.0, 0.35, t);
        (
            Vec3::new(0.0, 0.03 * a, -0.10 * a),
            Vec3::new(-0.25 * a, 0.0, 0.0),
        )
    } else if t < 0.60 {
        let a = smoothstep(0.35, 0.60, t);
        (
            Vec3::new(0.0, 0.03 - 0.10 * a, -0.10 + 0.22 * a),
            Vec3::new(-0.25 + 0.85 * a, 0.0, 0.0),
        )
    } else {
        let a = smoothstep(0.60, 1.0, t);
        (
            Vec3::new(0.0, -0.07 * (1.0 - a), 0.12 * (1.0 - a)),
            Vec3::new(0.60 * (1.0 - a), 0.0, 0.0),
        )
    };
    (pos, Quat::from_euler(EulerRot::XYZ, rot.x, rot.y, rot.z))
}

fn hammer_strike_anim(t: f32) -> (Vec3, Quat) {
    let (pos, rot) = if t < 0.40 {
        let a = smoothstep(0.0, 0.40, t);
        (
            Vec3::new(0.0, 0.10 * a, -0.04 * a),
            Vec3::new(-0.65 * a, 0.0, 0.05 * a),
        )
    } else if t < 0.55 {
        let a = smoothstep(0.40, 0.55, t);
        (
            Vec3::new(0.0, 0.10 - 0.16 * a, -0.04 + 0.14 * a),
            Vec3::new(-0.65 + 1.25 * a, 0.0, 0.05 - 0.05 * a),
        )
    } else {
        let a = smoothstep(0.55, 1.0, t);
        (
            Vec3::new(0.0, -0.06 * (1.0 - a), 0.10 * (1.0 - a)),
            Vec3::new(0.60 * (1.0 - a), 0.0, 0.0),
        )
    };
    (pos, Quat::from_euler(EulerRot::XYZ, rot.x, rot.y, rot.z))
}

fn generic_swing_anim(t: f32) -> (Vec3, Quat) {
    let s = (t * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    (
        Vec3::new(-0.03 * s, -0.02 * s, 0.06 * s),
        Quat::from_euler(EulerRot::XYZ, -0.45 * s, 0.08 * s, -0.10 * s),
    )
}

fn spawn_item_model(
    commands: &mut Commands,
    asset_server: &AssetServer,
    root: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    item: ItemId,
) {
    if let Some(block) = item.def().place_block {
        let [r, g, b] = block.srgb();
        let mat = materials.add(StandardMaterial {
            base_color: Color::srgb(r, g, b),
            perceptual_roughness: 0.8,
            ..default()
        });
        let mesh = meshes.add(Cuboid::new(0.22, 0.22, 0.22));
        spawn_part(commands, root, mesh, mat, Transform::default());
        return;
    }

    if let Some((model, translation, rotation)) = glb_item_model(item) {
        spawn_glb_item(commands, asset_server, root, model, translation, rotation);
        return;
    }

    match item {
        ItemId::StoneHandAxe => spawn_stone_hand_axe(commands, asset_server, root),
        _ if tool_for_item(item).is_some() => spawn_generic_tool(
            commands,
            root,
            meshes,
            materials,
            tool_for_item(item).unwrap(),
        ),
        _ => spawn_simple_item(commands, root, meshes, materials, item),
    }
}

pub(crate) fn drop_model(item: ItemId) -> Option<&'static str> {
    Some(match item {
        ItemId::StoneHandAxe => "models/items/stone_hand_axe.glb#Scene0",
        ItemId::Stick => "models/items/stick.glb#Scene0",
        ItemId::StoneTippedStick => "models/items/stone_tipped_stick.glb#Scene0",
        ItemId::Fiber => "models/items/fiber.glb#Scene0",
        ItemId::Cordage => "models/items/cordage.glb#Scene0",
        ItemId::Clay => "models/items/clay.glb#Scene0",
        ItemId::Berry => "models/items/berry.glb#Scene0",
        ItemId::GranitePebble => "models/items/granite_pebble.glb#Scene0",
        ItemId::SlatePebble => "models/items/slate_pebble.glb#Scene0",
        ItemId::SandstonePebble => "models/items/sandstone_pebble.glb#Scene0",
        ItemId::LimestonePebble => "models/items/limestone_pebble.glb#Scene0",
        ItemId::Hammer => "models/items/hammer.glb#Scene0",
        ItemId::Wrench => "models/items/wrench.glb#Scene0",
        ItemId::Pickaxe => "models/items/pickaxe.glb#Scene0",
        ItemId::Axe => "models/items/axe.glb#Scene0",
        ItemId::Shovel => "models/items/shovel.glb#Scene0",
        ItemId::Hoe => "models/items/hoe.glb#Scene0",
        ItemId::Scythe => "models/items/scythe.glb#Scene0",
        ItemId::Lantern => "models/items/lantern.glb#Scene0",
        ItemId::Magnet => "models/items/magnet.glb#Scene0",
        ItemId::SharpenedStone => "models/items/sharpened_stone.glb#Scene0",
        ItemId::BarkScraper => "models/items/bark_scraper.glb#Scene0",
        ItemId::SmallStoneHammer => "models/items/small_stone_hammer.glb#Scene0",
        ItemId::Bark => "models/items/bark.glb#Scene0",
        ItemId::Workbench => "models/items/workbench.glb#Scene0",
        ItemId::Furnace => "models/items/furnace.glb#Scene0",
        _ => return None,
    })
}

fn glb_item_model(item: ItemId) -> Option<(&'static str, Vec3, Quat)> {
    let rot_tool = Quat::from_euler(EulerRot::XYZ, 0.18, -1.0, 0.12);
    let rot_small = Quat::from_euler(EulerRot::XYZ, 0.20, -0.55, 0.10);
    let rot_blocky = Quat::from_euler(EulerRot::XYZ, 0.12, -0.55, 0.0);
    Some(match item {
        ItemId::Stick => (drop_model(item)?, Vec3::new(0.02, -0.16, 0.02), rot_tool),
        ItemId::StoneTippedStick => (drop_model(item)?, Vec3::new(0.02, -0.16, 0.02), rot_tool),
        ItemId::Fiber => (drop_model(item)?, Vec3::new(0.0, -0.10, 0.0), rot_small),
        ItemId::Cordage => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_small),
        ItemId::Clay => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_blocky),
        ItemId::Berry => (drop_model(item)?, Vec3::new(0.0, -0.10, 0.0), rot_small),
        ItemId::GranitePebble => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_small),
        ItemId::SlatePebble => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_small),
        ItemId::SandstonePebble => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_small),
        ItemId::LimestonePebble => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_small),
        ItemId::Hammer => (drop_model(item)?, Vec3::new(0.02, -0.22, 0.02), rot_tool),
        ItemId::Wrench => (drop_model(item)?, Vec3::new(0.02, -0.20, 0.02), rot_tool),
        ItemId::Pickaxe => (drop_model(item)?, Vec3::new(0.02, -0.22, 0.02), rot_tool),
        ItemId::Axe => (drop_model(item)?, Vec3::new(0.02, -0.22, 0.02), rot_tool),
        ItemId::Shovel => (drop_model(item)?, Vec3::new(0.02, -0.24, 0.02), rot_tool),
        ItemId::Hoe => (drop_model(item)?, Vec3::new(0.02, -0.22, 0.02), rot_tool),
        ItemId::Scythe => (drop_model(item)?, Vec3::new(0.02, -0.26, 0.02), rot_tool),
        ItemId::Lantern => (drop_model(item)?, Vec3::new(0.0, -0.12, 0.0), rot_small),
        ItemId::Magnet => (drop_model(item)?, Vec3::new(0.0, -0.10, 0.0), rot_small),
        ItemId::SharpenedStone => (drop_model(item)?, Vec3::new(0.0, -0.10, 0.0), rot_small),
        ItemId::BarkScraper => (drop_model(item)?, Vec3::new(0.02, -0.22, 0.02), rot_tool),
        ItemId::SmallStoneHammer => (drop_model(item)?, Vec3::new(0.02, -0.22, 0.02), rot_tool),
        ItemId::Bark => (drop_model(item)?, Vec3::new(0.0, -0.08, 0.0), rot_small),
        ItemId::Workbench => (drop_model(item)?, Vec3::new(0.0, -0.12, 0.0), rot_blocky),
        ItemId::Furnace => (drop_model(item)?, Vec3::new(0.0, -0.14, 0.0), rot_blocky),
        _ => return None,
    })
}

fn spawn_glb_item(
    commands: &mut Commands,
    asset_server: &AssetServer,
    root: Entity,
    model: &'static str,
    base_translation: Vec3,
    base_rotation: Quat,
) {
    let scene: Handle<WorldAsset> = asset_server.load(model);
    commands.spawn((
        HeldItemModel,
        HeldPart {
            base_translation,
            base_rotation,
            base_scale: Vec3::splat(1.0),
        },
        Transform::from_translation(base_translation).with_rotation(base_rotation),
        Visibility::Inherited,
        WorldAssetRoot(scene),
        NotShadowCaster,
        NotShadowReceiver,
        ChildOf(root),
    ));
}

fn spawn_part(
    commands: &mut Commands,
    root: Entity,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    transform: Transform,
) {
    commands.spawn((
        HeldItemModel,
        HeldPart {
            base_translation: transform.translation,
            base_rotation: transform.rotation,
            base_scale: transform.scale,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        transform,
        NotShadowCaster,
        NotShadowReceiver,
        ChildOf(root),
    ));
}

fn color_mat(color: Color, roughness: f32, metallic: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        perceptual_roughness: roughness,
        metallic,
        ..default()
    }
}

/// Visual offset from the held-item root to the Stone Hand Axe grip pivot
/// (bottom of handle, model origin). Root rotation is applied around this pivot.
const STONE_HAND_AXE_GRIP_OFFSET: Vec3 = Vec3::new(0.04, -0.26, 0.03);

fn spawn_stone_hand_axe(commands: &mut Commands, asset_server: &AssetServer, root: Entity) {
    // Authored GLB model: origin at bottom of handle (grip pivot), handle along +Y,
    // stone head near top, cutting edge toward -X in model space.
    // The model is placed so its origin sits at the root pivot; root rotation
    // therefore swings the whole axe around the handle bottom.
    let scene: Handle<WorldAsset> = asset_server.load("models/items/stone_hand_axe.glb#Scene0");
    let base_translation = Vec3::ZERO;
    let base_rotation = Quat::from_euler(EulerRot::XYZ, 0.20, -1.05, 0.14);
    commands.spawn((
        HeldItemModel,
        HeldPart {
            base_translation,
            base_rotation,
            base_scale: Vec3::splat(1.0),
        },
        Transform::from_translation(base_translation).with_rotation(base_rotation),
        Visibility::Inherited,
        WorldAssetRoot(scene),
        NotShadowCaster,
        NotShadowReceiver,
        ChildOf(root),
    ));
}

/// Propagate shadow flags to GLB scene children spawned under held item models.
fn propagate_held_item_shadow_flags(
    mut commands: Commands,
    roots: Query<Entity, With<HeldItemRoot>>,
    children: Query<&Children>,
    meshes: Query<Entity, (With<Mesh3d>, Without<NotShadowCaster>)>,
) {
    let mut stack: Vec<Entity> = roots.iter().collect();
    while let Some(e) = stack.pop() {
        if meshes.get(e).is_ok() {
            commands
                .entity(e)
                .insert((NotShadowCaster, NotShadowReceiver));
        }
        if let Ok(kids) = children.get(e) {
            for &k in kids {
                stack.push(k);
            }
        }
    }
}
/// Replace held-item materials with private mostly-unlit clones.
///
/// The source materials may be shared with dropped or world models, so mutating
/// them in place would change the world too. A small diffuse albedo remains for
/// shape readability, while most visible color comes from emissive lighting that
/// does not swing with the sun as the camera turns.
fn prepare_held_item_viewmodel_materials(
    mut commands: Commands,
    roots: Query<Entity, With<HeldItemRoot>>,
    children: Query<&Children>,
    mesh_mats: Query<(Entity, &MeshMaterial3d<StandardMaterial>), Without<HeldItemViewMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut stack: Vec<Entity> = roots.iter().collect();
    while let Some(entity) = stack.pop() {
        if let Ok((entity, handle)) = mesh_mats.get(entity)
            && let Some(source) = materials.get(handle.0.id())
        {
            let mut material = source.clone();
            apply_viewmodel_material(&mut material);
            let view_handle = materials.add(material);
            commands
                .entity(entity)
                .insert((MeshMaterial3d(view_handle), HeldItemViewMaterial));
        }
        if let Ok(kids) = children.get(entity) {
            for &kid in kids {
                stack.push(kid);
            }
        }
    }
}

/// Make a first-person viewmodel material almost independent of scene lights.
///
/// Shared by held items and the offhand lantern so both stay stable while the
/// camera moves relative to the sun or a nearby point light.
pub(crate) fn apply_viewmodel_material(material: &mut StandardMaterial) {
    /// Fraction of the original diffuse response kept for sunlight/ambient.
    const SUN_ALBEDO_FRACTION: f32 = 0.06;
    /// Fraction of the original base color supplied as stable self-lighting.
    const EMISSIVE_FRACTION: f32 = 1.0;

    let base = material.base_color.to_linear();
    let existing_glow = material.emissive;

    material.base_color = Color::linear_rgba(
        base.red * SUN_ALBEDO_FRACTION,
        base.green * SUN_ALBEDO_FRACTION,
        base.blue * SUN_ALBEDO_FRACTION,
        base.alpha,
    );
    material.emissive = LinearRgba::rgb(
        base.red * EMISSIVE_FRACTION + existing_glow.red,
        base.green * EMISSIVE_FRACTION + existing_glow.green,
        base.blue * EMISSIVE_FRACTION + existing_glow.blue,
    );
    // Keep existing emissive maps (for example a lantern glow). Otherwise reuse
    // the base-color texture so textured models keep their detail while unlit.
    if material.emissive_texture.is_none()
        && let Some(texture) = material.base_color_texture.clone()
    {
        material.emissive_texture = Some(texture);
        material.emissive_channel = material.base_color_channel.clone();
    }
    material.emissive_exposure_weight = 1.0;

    // Kill viewmodel specular flashes without flattening the base color.
    material.reflectance = 0.0;
    material.perceptual_roughness = 1.0;
    material.metallic = 0.0;
}

fn spawn_generic_tool(
    commands: &mut Commands,
    root: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    tool: crate::tools::ToolKind,
) {
    let handle_mat = materials.add(color_mat(Color::srgb(0.46, 0.32, 0.18), 0.85, 0.0));
    let head_mat = materials.add(color_mat(Color::srgb(0.62, 0.64, 0.68), 0.45, 0.15));
    let handle = meshes.add(Cuboid::new(0.045, 0.34, 0.045));
    spawn_part(
        commands,
        root,
        handle,
        handle_mat,
        Transform::from_xyz(0.0, 0.02, 0.0).with_rotation(Quat::from_rotation_z(0.18)),
    );
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
        _ => (
            Vec3::new(0.09, 0.08, 0.04),
            Vec3::new(0.04, 0.14, 0.0),
            Quat::from_rotation_z(0.18),
        ),
    };
    let head = meshes.add(Cuboid::new(head_size.x, head_size.y, head_size.z));
    spawn_part(
        commands,
        root,
        head,
        head_mat,
        Transform::from_translation(head_pos).with_rotation(head_rot),
    );
}

fn spawn_simple_item(
    commands: &mut Commands,
    root: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    item: ItemId,
) {
    let color = item_color(item);
    let mat = materials.add(color_mat(color, 0.8, 0.0));
    let mesh = meshes.add(Cuboid::new(0.12, 0.12, 0.12));
    spawn_part(commands, root, mesh, mat, Transform::default());
}

fn item_color(item: ItemId) -> Color {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewmodel_materials_keep_color_without_sun_swing() {
        let mut material = StandardMaterial {
            base_color: Color::srgb(0.5, 0.25, 0.1),
            perceptual_roughness: 0.15,
            metallic: 0.8,
            reflectance: 0.6,
            ..default()
        };

        apply_viewmodel_material(&mut material);

        let base = material.base_color.to_linear();
        assert!(base.red < 0.02);
        assert!(base.green < 0.01);
        assert!(base.blue < 0.001);
        assert!(material.emissive.red > 0.2);
        assert!(material.emissive.green > 0.04);
        assert!(material.emissive.blue > 0.005);
        assert_eq!(material.reflectance, 0.0);
        assert_eq!(material.metallic, 0.0);
        assert_eq!(material.perceptual_roughness, 1.0);
        assert_eq!(material.emissive_exposure_weight, 1.0);
    }
}
