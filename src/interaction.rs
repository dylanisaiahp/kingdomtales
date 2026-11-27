//! World interaction: hold-to-mine, direct placement, ground drops and pickup.
//!
//! Mining removes the real voxels immediately and shows shaking preview cubes.
//! Releasing the button restores the original material, shape and orientation;
//! finishing the break spawns physical item drops. Felling drops wood at once
//! and leaves one falling piece per leaf cell; each piece drops straight down,
//! settles, and decays on its own schedule into a possible stick. A 1 m terrain
//! voxel is highlighted as one 1 m cube, mined as its eight detail cells, and
//! rewards one resource item. Detail objects keep 0.5 m shape-aware previews.
//!
//! The selected hotbar stack drives right click. Block materials and placeable
//! structures use RMB against the hit face and consume one item, matching
//! Minecraft. RMB also interacts with structures, gathers loose clutter and
//! picks up drops. The hammer applies the shape chosen in the hold-RMB menu to
//! the targeted block. The wrench rotates the targeted block with left click.
//! Mining tools keep the original hold-to-mine behaviour.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot};

use crate::backpack::Backpack;
use crate::construction::{
    ConstructionState, apply_shape_checked, orientation_facing_player, place_block_checked,
    rotate_block_checked,
};
use crate::crafting::{CraftingUiState, StationKind as CraftingStationKind};
use crate::felling::{
    FellPlan, FellingConfig, leaf_cells, leaf_decay_lifetime, leaf_fall_delay, plan_felling,
    wood_drop_counts,
};
use crate::items::{ItemId, selected_item, selected_tool, store_item, tool_for_item};
use crate::notifications::{NotificationFeed, NotificationTone};
use crate::player::{
    CROUCH_HEIGHT, CrosshairTarget, CursorCapture, PLAYER_HEIGHT, Player, PlayerCamera, REACH,
};
use crate::pockets::Pockets;
use crate::progression::Progression;
use crate::rewards::{
    MiningReward, detail_voxel_reward, natural_surface_harvest_reward, world_surface_reward,
};
use crate::structures::{
    StructureCrosshair, StructureEntities, StructureKind, Structures, despawn_structure_visual,
    spawn_structure_visual,
};
use crate::tools::ToolKind;
use crate::ui_manager::{Overlay, UiManager};
use crate::world::block::{
    Block, Orientation, PANEL_HEIGHT, Shape, Voxel, centered_detail_bounds, stem_bounds_for_shape,
    uses_centered_detail_bounds,
};
use crate::world::chunk::{
    BLOCKS_PER_METRE, DETAIL_Y_MIN, DETAILS_PER_WORLD_AXIS, DETAILS_PER_WORLD_VOXEL, SurfaceLayer,
    detail_y_in_range, world_to_detail_origin, world_voxel_center,
};
use crate::world::{ChunkManager, apply_voxel_edits, remesh_chunk};

const DROP_SIZE: f32 = 0.32;
const DROP_GRAVITY: f32 = 18.0 * BLOCKS_PER_METRE;
const DROP_TERMINAL_SPEED: f32 = 12.0;
const DROP_VELOCITY_EPSILON: f32 = 0.02;
const DROP_SUPPORT_MARGIN: f32 = 0.05;
const DROP_SUPPORT_INTERVAL: f32 = 0.12;
/// Debris below the generated world is discarded. The floor is the bottom of
/// the detail grid, not a fixed distance below the old sea level.
const DROP_FLOOR_Y: f32 = DETAIL_Y_MIN as f32;
/// A dropped stick uses the same slender geometry as a natural ground stick.
const STICK_DROP_SIZE: Vec3 = Vec3::new(0.88, 0.08, 0.16);
/// Leaf items also read as the 0.5 m voxel they came from.
const LEAF_DROP_SIZE: Vec3 = Vec3::ONE;
/// Each leaf from a felled tree is its own full 0.5 m detail block.
const LEAF_PIECE_SIZE: Vec3 = Vec3::ONE;
const LEAF_TERMINAL_SPEED: f32 = 8.0;
const LEAF_SUPPORT_MARGIN: f32 = 0.05;
const DROP_PICKUP_RADIUS: f32 = 0.50;
/// Fallback pickup range around the camera. This is what makes held right click
/// reliable even when the crosshair is slightly off the drop.
const PICKUP_NEAR_RADIUS: f32 = 2.8 * BLOCKS_PER_METRE;
const PICKUP_CLOSE_RADIUS: f32 = 1.2 * BLOCKS_PER_METRE;
const PICKUP_INTERVAL: f32 = 0.12;
/// Radius around the player where an equipped Magnet pulls ground drops.
const MAGNET_RADIUS: f32 = 6.0 * BLOCKS_PER_METRE;
/// Horizontal pull speed applied to drops inside the Magnet radius.
const MAGNET_SPEED: f32 = 4.5 * BLOCKS_PER_METRE;

#[derive(Resource)]
pub(crate) struct InteractionVisuals {
    preview_mesh: Handle<Mesh>,
    drop_meshes: HashMap<ItemId, Handle<Mesh>>,
    /// One shared 0.5 m block mesh for every leaf piece from a felled tree.
    leaf_mesh: Handle<Mesh>,
    drop_materials: HashMap<ItemId, Handle<StandardMaterial>>,
    fallback_drop_material: Handle<StandardMaterial>,
    block_materials: HashMap<Block, Handle<StandardMaterial>>,
    /// Outlines are cached per shape and stem orientation: flat limbs and roots
    /// use a bottom-aligned box, while upright bush stems are centred.
    highlight_meshes: HashMap<(Shape, u8), Handle<Mesh>>,
    /// Selection outline for a whole 1 m terrain voxel.
    world_highlight_mesh: Handle<Mesh>,
    /// Selection outline shared by turf and snow surfaces: 1 m x 0.1 m x 1 m.
    surface_highlight_mesh: Handle<Mesh>,
    /// Cached structure selection outlines; rebuilt when structures change.
    structure_highlight_meshes: HashMap<crate::structures::StructureId, Handle<Mesh>>,
}

#[derive(Resource, Default)]
pub(crate) struct MiningState {
    pub(crate) target: Option<IVec3>,
    originals: Vec<(IVec3, Voxel)>,
    /// The 1 m world voxel removed alongside the detail cells, kept so a
    /// cancelled swing can restore it.
    world_original: Option<(IVec3, Block)>,
    /// Natural surface flags removed by this swing, kept for rollback.
    surface_originals: Vec<(SurfaceLayer, IVec3, bool)>,
    elapsed: f32,
    duration: f32,
    /// Held item that started this swing. Changing items cancels the swing so
    /// blocks cannot be harvested with an item that was not checked at the start.
    held_item: Option<ItemId>,
    previews: Vec<Entity>,
    phase: f32,
    is_world: bool,
    /// True when this swing only harvests a thin natural surface layer.
    is_surface: bool,
    /// Which layer a surface-only swing is harvesting.
    surface_layer: Option<SurfaceLayer>,
    /// Tree collected when the swing started. The rest of the tree is removed
    /// only when the trunk actually breaks, so cancelling leaves it intact.
    tree_plan: Option<FellPlan>,
    /// Structure being dismantled. Structures use the same timed mining animation
    /// as blocks but are removed as a whole entity on completion.
    structure_target: Option<crate::structures::StructureId>,
    structure_original: Option<crate::structures::PlacedStructure>,
}

#[derive(Resource, Default)]
struct TargetedDrop(Option<Entity>);

#[derive(Resource, Default)]
struct PickupTimer(f32);

/// Text shown near the crosshair when the player is looking at a ground drop.
#[derive(Resource, Default)]
pub struct PickupPrompt {
    pub visible: bool,
    pub text: String,
    /// Structured target data used by the Insight panel and notifications.
    pub item: Option<ItemId>,
    pub count: u32,
}

#[derive(Component)]
struct MiningPreview {
    base: Vec3,
    scale: Vec3,
    rotation: Quat,
    /// Support point to shrink toward (usually top of block below).
    anchor: Vec3,
}

/// The white outline that marks the block under the crosshair.
#[derive(Component)]
struct BlockHighlight;

/// A physical item on the ground. Drops fall until they find support, then stay
/// perfectly still until the block beneath them is removed.
#[derive(Component)]
pub struct BlockDrop {
    pub item: ItemId,
    pub count: u32,
    velocity: Vec3,
    /// Vertical half-extent of this item's mesh, used for its support probe.
    half_height: f32,
    settled: bool,
    support_timer: f32,
    /// Seconds before the player can pick this drop up again. Used for
    /// intentionally dropped items to prevent immediate re-pickup.
    pub pickup_cooldown: f32,
}

/// Plain, testable state for one drop step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropState {
    pub position: Vec3,
    pub velocity: Vec3,
    pub settled: bool,
    pub support_timer: f32,
}

/// The result of one drop step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropStep {
    pub state: DropState,
    pub despawn: bool,
}

/// Plain state for one falling leaf from a felled tree. X and Z never change:
/// leaves fall straight down and settle where they land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeafPieceState {
    pub position: Vec3,
    pub velocity_y: f32,
    pub fall_delay: f32,
    pub half_height: f32,
    pub lifetime_after_settle: f32,
    pub settled_for: Option<f32>,
    /// Optional item dropped when the leaf decays.
    pub drop_item: Option<ItemId>,
}

/// The result of one leaf step. `fade` shrinks a settled leaf before it decays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeafPieceStep {
    pub state: LeafPieceState,
    pub fade: f32,
    pub drop_item: Option<ItemId>,
    pub despawn: bool,
}

/// Lightweight component for one leaf cell from a felled tree.
#[derive(Component, Debug)]
pub struct TreeDebris(pub LeafPieceState);

/// Cell directly beneath a drop, using a small margin so a resting drop does
/// not settle into the cell below through floating-point noise.
pub fn drop_support_cell(position: Vec3, half_size: f32) -> IVec3 {
    (position - Vec3::Y * (half_size + DROP_SUPPORT_MARGIN))
        .floor()
        .as_ivec3()
}

/// Top surface under a support probe, including raised natural surface slabs.
///
/// Turf and snow are 0.1 m layers above the world voxel rather than detail
/// voxels, so `is_solid` alone would make drops settle 0.2 engine units below the
/// visible surface and disappear inside it. A drop can also tunnel past the thin
/// slab in one frame, so the cell above a solid support is checked too.
fn support_surface_top_y(manager: &ChunkManager, probe: IVec3) -> Option<f32> {
    let has_surface = |cell: IVec3| {
        SurfaceLayer::ALL
            .iter()
            .any(|layer| manager.is_surface_cell(*layer, cell))
    };
    if manager.is_solid(probe) {
        let above = probe + IVec3::Y;
        if !manager.is_solid(above) && has_surface(above) {
            return Some(probe.y as f32 + 1.0 + PANEL_HEIGHT);
        }
        return Some(probe.y as f32 + 1.0);
    }
    if has_surface(probe) {
        return Some(probe.y as f32 + PANEL_HEIGHT);
    }
    None
}

/// Integrate one drop frame. Settled drops never bob or spin; they only wait
/// for their support check and start falling again if the voxel below is gone.
pub fn step_drop(
    mut state: DropState,
    dt: f32,
    gravity: f32,
    half_size: f32,
    surface_top_y: Option<f32>,
    floor_y: f32,
    support_interval: f32,
) -> DropStep {
    let dt = dt.clamp(0.0, 0.05);

    if state.settled {
        state.support_timer -= dt;
        if state.support_timer > 0.0 {
            return DropStep {
                state,
                despawn: false,
            };
        }
        state.support_timer = support_interval;
        if surface_top_y.is_some() {
            return DropStep {
                state,
                despawn: false,
            };
        }
        state.settled = false;
    }

    state.velocity.y = (state.velocity.y - gravity * dt).max(-DROP_TERMINAL_SPEED);
    clamp_small_velocity(&mut state.velocity);
    state.position += state.velocity * dt;

    if state.velocity.y <= 0.0
        && let Some(top) = surface_top_y
        && state.position.y <= top + half_size
    {
        state.position.y = top + half_size;
        state.velocity = Vec3::ZERO;
        state.settled = true;
        state.support_timer = support_interval;
    }

    DropStep {
        state,
        despawn: state.position.y < floor_y,
    }
}

/// Horizontal pull toward `target` for an equipped Magnet, or `None` when the
/// drop is out of range. Vertical distance is intentionally ignored: drops
/// slide along the ground toward the player instead of flying through terrain.
pub fn magnet_pull(drop_position: Vec3, target: Vec3, radius: f32, speed: f32) -> Option<Vec3> {
    let offset = Vec3::new(target.x - drop_position.x, 0.0, target.z - drop_position.z);
    let distance = offset.length();
    if distance <= f32::EPSILON || distance > radius {
        return None;
    }
    Some(offset * (speed / distance))
}

/// Integrate one falling leaf frame. The leaf never moves sideways or spins;
/// after settling it fades, then decays exactly once into its own stick roll.
pub fn step_leaf_piece(
    mut state: LeafPieceState,
    dt: f32,
    gravity: f32,
    surface_top_y: Option<f32>,
    floor_y: f32,
) -> LeafPieceStep {
    let dt = dt.clamp(0.0, 0.05);

    if state.position.y < floor_y {
        return LeafPieceStep {
            state,
            fade: 1.0,
            drop_item: None,
            despawn: true,
        };
    }

    if let Some(age) = state.settled_for {
        let age = age + dt;
        if age >= state.lifetime_after_settle {
            return LeafPieceStep {
                state,
                fade: 0.0,
                drop_item: state.drop_item,
                despawn: true,
            };
        }
        state.settled_for = Some(age);
        let fade = 1.0 - age / state.lifetime_after_settle.max(0.01);
        return LeafPieceStep {
            state,
            fade: fade.clamp(0.02, 1.0),
            drop_item: None,
            despawn: false,
        };
    }

    if state.fall_delay > 0.0 {
        state.fall_delay = (state.fall_delay - dt).max(0.0);
        return LeafPieceStep {
            state,
            fade: 1.0,
            drop_item: None,
            despawn: false,
        };
    }

    state.velocity_y = (state.velocity_y - gravity * dt).max(-LEAF_TERMINAL_SPEED);
    state.position.y += state.velocity_y * dt;

    if state.velocity_y <= 0.0
        && let Some(top) = surface_top_y
        && state.position.y <= top + state.half_height
    {
        state.position.y = top + state.half_height;
        state.velocity_y = 0.0;
        state.settled_for = Some(0.0);
    }

    LeafPieceStep {
        state,
        fade: 1.0,
        drop_item: None,
        despawn: false,
    }
}

fn clamp_small_velocity(velocity: &mut Vec3) {
    if velocity.x.abs() < DROP_VELOCITY_EPSILON {
        velocity.x = 0.0;
    }
    if velocity.y.abs() < DROP_VELOCITY_EPSILON {
        velocity.y = 0.0;
    }
    if velocity.z.abs() < DROP_VELOCITY_EPSILON {
        velocity.z = 0.0;
    }
}

/// Extra quarter-turns applied by the player while placing structures (R key).
#[derive(Resource, Default, Debug, Clone, Copy)]
struct StructurePlaceRotation(u8);

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MiningState>()
            .init_resource::<TargetedDrop>()
            .init_resource::<PickupTimer>()
            .init_resource::<PickupPrompt>()
            .init_resource::<FellingConfig>()
            .init_resource::<StructurePlaceRotation>()
            .add_systems(Startup, setup_visuals)
            .add_systems(
                Update,
                (
                    handle_mining,
                    animate_mining_preview,
                    update_drops,
                    update_tree_debris,
                    update_pickup_prompt,
                )
                    .chain(),
            )
            .add_systems(Update, handle_pickup_actions)
            .add_systems(Update, handle_placement_actions)
            .add_systems(Update, handle_hammer_actions)
            .add_systems(Update, handle_wrench_actions)
            .add_systems(Update, handle_structure_interact)
            .add_systems(
                Update,
                (
                    refresh_structure_highlight_meshes,
                    update_structure_place_rotation,
                    update_placement_preview,
                    update_block_highlight,
                )
                    .chain(),
            );
    }
}

fn update_structure_place_rotation(
    keys: Res<ButtonInput<KeyCode>>,
    bindings: Res<crate::settings::InputBindings>,
    capture: Res<CursorCapture>,
    pockets: Res<Pockets>,
    mut rot: ResMut<StructurePlaceRotation>,
) {
    let placing_structure = capture.grabbed
        && pockets
            .selected_stack()
            .is_some_and(|s| matches!(s.item, ItemId::Workbench | ItemId::Furnace));
    if !placing_structure {
        rot.0 = 0;
        return;
    }
    if keys.just_pressed(bindings.rotate_place) {
        rot.0 = (rot.0 + 1) % 4;
    }
}

fn setup_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    theme: Res<crate::ui::theme::UiTheme>,
) {
    let preview_mesh = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let mut drop_meshes = HashMap::new();
    for item in ItemId::ALL {
        let size = drop_mesh_size(item);
        drop_meshes.insert(item, meshes.add(Cuboid::new(size.x, size.y, size.z)));
    }
    let leaf_mesh = meshes.add(Cuboid::new(
        LEAF_PIECE_SIZE.x,
        LEAF_PIECE_SIZE.y,
        LEAF_PIECE_SIZE.z,
    ));

    let mut drop_materials = HashMap::new();
    for item in ItemId::ALL {
        let material = materials.add(StandardMaterial {
            base_color: item.color(),
            perceptual_roughness: 0.78,
            metallic: 0.0,
            ..default()
        });
        drop_materials.insert(item, material);
    }
    let fallback_drop_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.8, 0.8, 0.8),
        perceptual_roughness: 0.78,
        metallic: 0.0,
        ..default()
    });

    let mut highlight_meshes = HashMap::new();
    let stem_orientations = [
        Orientation::default(),
        Orientation::from_up_forward(Vec3::X, Vec3::Y),
        Orientation::from_up_forward(Vec3::NEG_X, Vec3::Y),
        Orientation::from_up_forward(Vec3::Z, Vec3::Y),
        Orientation::from_up_forward(Vec3::NEG_Z, Vec3::Y),
    ];
    for shape in Shape::ALL {
        let variant_count = match shape {
            Shape::Stem => 5usize,
            Shape::Stick | Shape::Pebble | Shape::StoneDeposit => 24usize,
            _ => 1usize,
        };
        #[allow(clippy::needless_range_loop)]
        for variant in 0..variant_count {
            let sample = if shape == Shape::Stem {
                stem_orientations[variant]
            } else {
                Orientation::from_index(variant as u8)
            };
            let (size, center) = shape_bounds(shape, sample);
            highlight_meshes.insert(
                (shape, variant as u8),
                meshes.add(outline_mesh(size, center)),
            );
        }
    }
    let highlight_mesh = highlight_meshes[&(Shape::Cube, 0)].clone();
    let world_highlight_mesh = meshes.add(outline_mesh(
        Vec3::splat(DETAILS_PER_WORLD_AXIS as f32),
        Vec3::ZERO,
    ));
    // Natural turf and snow are raised 0.1 m layers on top of a 1 m world voxel.
    // The outline covers that whole 1 m x 0.1 m x 1 m surface rather than a
    // single cell.
    let surface_highlight_mesh = meshes.add(outline_mesh(
        Vec3::new(
            DETAILS_PER_WORLD_AXIS as f32,
            PANEL_HEIGHT,
            DETAILS_PER_WORLD_AXIS as f32,
        ),
        Vec3::ZERO,
    ));

    commands.insert_resource(InteractionVisuals {
        preview_mesh,
        drop_meshes,
        leaf_mesh,
        drop_materials,
        fallback_drop_material,
        block_materials: HashMap::new(),
        highlight_meshes,
        world_highlight_mesh,
        surface_highlight_mesh,
        structure_highlight_meshes: HashMap::new(),
    });

    // The outline is built from thin bars whose outer faces sit just proud of
    // the target's bounds, so it stays readable against the coplanar block
    // faces. A positive depth bias keeps it in front of them.
    let highlight_material = materials.add(StandardMaterial {
        base_color: theme.colors.accent,
        unlit: true,
        cull_mode: None,
        depth_bias: 100.0,
        ..default()
    });
    commands.spawn((
        BlockHighlight,
        Mesh3d(highlight_mesh),
        MeshMaterial3d(highlight_material),
        Transform::default(),
        Visibility::Hidden,
        NotShadowCaster,
        Name::new("BlockHighlight"),
    ));
}

/// Visual size of one dropped item. Tools and resources get recognizable silhouettes.
fn drop_mesh_size(item: ItemId) -> Vec3 {
    if crate::items::tool_for_item(item).is_some() {
        return match item {
            ItemId::Hammer => Vec3::new(0.18, 0.22, 0.10),
            ItemId::Wrench => Vec3::new(0.14, 0.22, 0.08),
            ItemId::Pickaxe => Vec3::new(0.26, 0.22, 0.08),
            ItemId::Axe => Vec3::new(0.22, 0.24, 0.08),
            ItemId::Shovel => Vec3::new(0.14, 0.28, 0.06),
            ItemId::Hoe => Vec3::new(0.22, 0.22, 0.06),
            ItemId::Scythe => Vec3::new(0.24, 0.26, 0.06),
            _ => Vec3::new(0.18, 0.24, 0.08),
        };
    }
    match item {
        ItemId::Stick | ItemId::StoneTippedStick => STICK_DROP_SIZE,
        ItemId::Leaves => LEAF_DROP_SIZE,
        ItemId::Fiber | ItemId::Cordage => Vec3::new(0.20, 0.05, 0.20),
        ItemId::Bark => Vec3::new(0.20, 0.04, 0.16),
        ItemId::SharpenedStone
        | ItemId::GranitePebble
        | ItemId::SlatePebble
        | ItemId::SandstonePebble
        | ItemId::LimestonePebble => Vec3::new(0.14, 0.08, 0.14),
        ItemId::Lantern | ItemId::Magnet => Vec3::new(0.14, 0.16, 0.14),
        _ => Vec3::splat(DROP_SIZE),
    }
}

/// Local bounds of a shape's visible geometry inside its cell. Used by both the
/// selection outline and the shaking mining preview so they match the mesher
/// and raycast boxes. For sticks/pebbles, the outline matches the actual
/// variant/pile parts rather than a generic full-voxel bound.
fn shape_bounds(shape: Shape, orientation: Orientation) -> (Vec3, Vec3) {
    let (min, max) = match shape {
        Shape::Cube | Shape::Slope | Shape::Stairs | Shape::Foliage => (Vec3::ZERO, Vec3::ONE),
        Shape::SmallFoliage => (Vec3::new(0.1, 0.0, 0.1), Vec3::new(0.9, 0.75, 0.9)),
        Shape::HalfBlock => (Vec3::ZERO, Vec3::new(1.0, 0.5, 1.0)),
        Shape::Panel => (Vec3::ZERO, Vec3::new(1.0, PANEL_HEIGHT, 1.0)),
        Shape::Trunk | Shape::DebarkedTrunk => centered_detail_bounds(shape),
        Shape::Stem | Shape::DebarkedStem => stem_bounds_for_shape(shape, orientation),
        Shape::Stick => {
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for (pmin, pmax) in crate::world::block::stick_parts(orientation) {
                min = min.min(pmin);
                max = max.max(pmax);
            }
            (min, max)
        }
        Shape::Pebble => {
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for (pmin, pmax) in crate::world::block::pebble_parts(orientation) {
                min = min.min(pmin);
                max = max.max(pmax);
            }
            (min, max)
        }
        Shape::StoneDeposit => {
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for (pmin, pmax) in crate::world::block::stone_deposit_parts(orientation) {
                min = min.min(pmin);
                max = max.max(pmax);
            }
            (min, max)
        }
    };
    (max - min, (min + max) * 0.5 - Vec3::splat(0.5))
}

/// Centre of the 1 m x 0.1 m x 1 m natural surface on top of a world voxel.
/// Turf and snow are raised layers above that voxel, so the outline sits in the
/// slab between the voxel's top plane and the visible surface.
fn surface_layer_center(world: IVec3) -> Vec3 {
    let scale = DETAILS_PER_WORLD_AXIS as f32;
    let top = (world.y as f32 + 1.0) * scale;
    Vec3::new(
        world.x as f32 * scale + scale * 0.5,
        top + PANEL_HEIGHT * 0.5,
        world.z as f32 * scale + scale * 0.5,
    )
}

/// Build the twelve edge bars of a block outline. The mesh is centred on the
/// cell origin; `size` and `center` describe the target's local bounds so half
/// blocks and panels hug the correct part of the cell.
fn outline_mesh(size: Vec3, center: Vec3) -> Mesh {
    // Fixed world-space thickness for consistent, thin selection outlines.
    let thickness = 0.012_f32.min(size.min_element().max(0.02) * 0.8);
    let margin = thickness * 0.5;
    let half = size * 0.5 + Vec3::splat(margin);
    let min = center - half;
    let max = center + half;
    let inset = thickness * 0.5;

    let xs = [min.x + inset, max.x - inset];
    let ys = [min.y + inset, max.y - inset];
    let zs = [min.z + inset, max.z - inset];

    let mut builder = OutlineMeshBuilder::default();

    // Bars running along X.
    for y in ys {
        for z in zs {
            builder.push_box(
                Vec3::new(min.x, y - inset, z - inset),
                Vec3::new(max.x, y + inset, z + inset),
            );
        }
    }
    // Bars running along Y.
    for x in xs {
        for z in zs {
            builder.push_box(
                Vec3::new(x - inset, min.y, z - inset),
                Vec3::new(x + inset, max.y, z + inset),
            );
        }
    }
    // Bars running along Z.
    for x in xs {
        for y in ys {
            builder.push_box(
                Vec3::new(x - inset, y - inset, min.z),
                Vec3::new(x + inset, y + inset, max.z),
            );
        }
    }

    builder.into_mesh()
}

#[derive(Default)]
struct OutlineMeshBuilder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl OutlineMeshBuilder {
    fn push_box(&mut self, min: Vec3, max: Vec3) {
        let faces: [([Vec3; 4], Vec3); 6] = [
            (
                [
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(max.x, min.y, max.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(max.x, max.y, min.z),
                ],
                Vec3::X,
            ),
            (
                [
                    Vec3::new(min.x, min.y, max.z),
                    Vec3::new(min.x, min.y, min.z),
                    Vec3::new(min.x, max.y, min.z),
                    Vec3::new(min.x, max.y, max.z),
                ],
                Vec3::NEG_X,
            ),
            (
                [
                    Vec3::new(min.x, max.y, max.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(max.x, max.y, min.z),
                    Vec3::new(min.x, max.y, min.z),
                ],
                Vec3::Y,
            ),
            (
                [
                    Vec3::new(min.x, min.y, min.z),
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(max.x, min.y, max.z),
                    Vec3::new(min.x, min.y, max.z),
                ],
                Vec3::NEG_Y,
            ),
            (
                [
                    Vec3::new(min.x, min.y, max.z),
                    Vec3::new(max.x, min.y, max.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(min.x, max.y, max.z),
                ],
                Vec3::Z,
            ),
            (
                [
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(min.x, min.y, min.z),
                    Vec3::new(min.x, max.y, min.z),
                    Vec3::new(max.x, max.y, min.z),
                ],
                Vec3::NEG_Z,
            ),
        ];

        for (points, normal) in faces {
            let base = self.positions.len() as u32;
            for point in points {
                self.positions.push(point.to_array());
                self.normals.push(normal.to_array());
            }
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    fn into_mesh(self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_mining(
    mut commands: Commands,
    time: Res<Time>,
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    pockets: Res<Pockets>,
    crosshair: Res<CrosshairTarget>,
    structure_hit: Res<StructureCrosshair>,
    mut structures: ResMut<Structures>,
    mut structure_entities: ResMut<StructureEntities>,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut visuals: ResMut<InteractionVisuals>,
    asset_server: Res<AssetServer>,
    mut mining: ResMut<MiningState>,
    felling: Res<FellingConfig>,
) {
    let dt = time.delta_secs().min(0.05);
    let item = selected_item(&pockets);
    let tool = item.and_then(tool_for_item);
    let active = capture.grabbed && !capture.just_grabbed;

    if mining.target.is_some()
        && (!active || !mouse.pressed(MouseButton::Left) || mining.held_item != item)
    {
        cancel_mining(
            &mut commands,
            &mut manager,
            &mut meshes,
            &mut materials,
            &mut structure_entities,
            &mut mining,
        );
        return;
    }

    // Structure dismantle: requires primitive axe, uses timed mining animation.
    if mining.target.is_none() && active && mouse.pressed(MouseButton::Left) {
        let voxel_dist = crosshair.hit.map(|h| h.distance);
        if let Some((sdist, _, sid)) = structure_hit.hit
            && voxel_dist.is_none_or(|vd| sdist <= vd)
        {
            let can_dismantle = structures.placed.get(&sid).is_some_and(|p| match p.kind {
                StructureKind::Workbench => {
                    selected_tool(&pockets).is_some_and(crate::tools::ToolKind::is_axe)
                }
                StructureKind::Furnace => selected_tool(&pockets).is_some_and(|t| {
                    matches!(
                        t,
                        crate::tools::ToolKind::Pickaxe | crate::tools::ToolKind::SmallStoneHammer
                    )
                }),
            });
            if can_dismantle && let Some(placed) = structures.placed.get(&sid).cloned() {
                let mut previews = Vec::new();
                let material = preview_material(&mut materials, &mut visuals, Block::Wood);
                let boxes = placed.collision_boxes();
                let (omin, omax) = boxes.iter().fold(
                    (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                    |(a, b), (mn, mx)| (a.min(*mn), b.max(*mx)),
                );
                let size = omax - omin;
                let base = (omin + omax) * 0.5;
                let anchor = Vec3::new(base.x, omin.y, base.z);
                previews.push(
                    commands
                        .spawn((
                            MiningPreview {
                                base,
                                scale: size,
                                rotation: Quat::IDENTITY,
                                anchor,
                            },
                            Mesh3d(visuals.preview_mesh.clone()),
                            MeshMaterial3d(material.clone()),
                            Transform::from_translation(base).with_scale(size),
                            NotShadowCaster,
                        ))
                        .id(),
                );
                // Hide the real structure while mining preview plays.
                despawn_structure_visual(&mut commands, &mut structure_entities, sid);
                mining.target = Some(placed.anchor);
                mining.originals.clear();
                mining.world_original = None;
                mining.surface_originals.clear();
                mining.elapsed = 0.0;
                mining.duration = 0.6;
                mining.held_item = item;
                mining.previews = previews;
                mining.phase =
                    (placed.anchor.x as f32 * 1.7 + placed.anchor.z as f32 * 2.3).fract();
                mining.is_world = false;
                mining.is_surface = false;
                mining.surface_layer = None;
                mining.tree_plan = None;
                mining.structure_target = Some(sid);
                mining.structure_original = Some(placed);
            }
            return;
        }
    }

    // A selected material belongs to the placement system, not mining. Only a
    // suitable tool or a primitive hand item with a mining use can break
    // blocks; empty hands gather loose clutter instead.
    if item.is_some() && tool.is_none() && item != Some(ItemId::StoneTippedStick) {
        return;
    }

    if !active {
        return;
    }

    if mining.target.is_none() {
        // Use `pressed` rather than `just_pressed` so holding the button keeps
        // mining the next block after one breaks, instead of requiring a new
        // click for every block.
        if !mouse.pressed(MouseButton::Left) {
            return;
        }

        let Some(hit) = crosshair.hit else {
            return;
        };
        if !detail_y_in_range(hit.block.y) {
            return;
        }
        let Some(voxel) = manager.get_voxel_if_loaded(hit.block) else {
            return;
        };
        let Some(duration) = mining_duration_for_item(item, voxel.block(), voxel.shape()) else {
            return;
        };

        // Bark scraping is a specialized trunk/stem action: it yields bark and
        // keeps the tree intact (no felling, no voxel removal).
        let bark_scrape = tool == Some(ToolKind::BarkScraper)
            && voxel.block() == Block::Wood
            && matches!(voxel.shape(), Shape::Trunk | Shape::Stem);

        // Turf and snow are raised 0.1 m layers on top of a world voxel, so
        // clicking their top face harvests only that flag and leaves the world
        // voxel and its detail cells intact. Every other hit mines real voxels.
        // Snow wins if a save edit has put both flags on one column.
        let natural_surface = if !bark_scrape && hit.normal == IVec3::Y {
            SurfaceLayer::ALL.iter().rev().find_map(|layer| {
                manager
                    .natural_surface_world(*layer, hit.block)
                    .map(|world| (*layer, world))
            })
        } else {
            None
        };

        if bark_scrape {
            let material = preview_material(&mut materials, &mut visuals, voxel.block());
            let base = hit.block.as_vec3() + Vec3::splat(0.5);
            let scale = Vec3::splat(0.95);
            let anchor = base - Vec3::Y * 0.45;
            let preview = commands
                .spawn((
                    MiningPreview {
                        base,
                        scale,
                        rotation: Quat::IDENTITY,
                        anchor,
                    },
                    Mesh3d(visuals.preview_mesh.clone()),
                    MeshMaterial3d(material),
                    Transform::from_translation(base).with_scale(scale),
                    NotShadowCaster,
                ))
                .id();
            mining.target = Some(hit.block);
            mining.originals = vec![(hit.block, voxel)];
            mining.world_original = None;
            mining.surface_originals.clear();
            mining.elapsed = 0.0;
            mining.duration = duration;
            mining.held_item = item;
            mining.previews = vec![preview];
            mining.phase = (hit.block.x as f32 * 1.7 + hit.block.z as f32 * 2.3).fract();
            mining.is_world = false;
            mining.is_surface = false;
            mining.surface_layer = None;
            mining.tree_plan = None;
            return;
        }

        if let Some((layer, world)) = natural_surface {
            let material = preview_material(&mut materials, &mut visuals, layer.block());
            let base = surface_layer_center(world);
            let scale = Vec3::new(
                DETAILS_PER_WORLD_AXIS as f32,
                PANEL_HEIGHT,
                DETAILS_PER_WORLD_AXIS as f32,
            );
            let anchor = base - Vec3::Y * (scale.y * 0.5);
            let preview = commands
                .spawn((
                    MiningPreview {
                        base,
                        scale,
                        rotation: Quat::IDENTITY,
                        anchor,
                    },
                    Mesh3d(visuals.preview_mesh.clone()),
                    MeshMaterial3d(material),
                    Transform::from_translation(base).with_scale(scale),
                    NotShadowCaster,
                ))
                .id();

            apply_voxel_edits(
                &mut commands,
                &mut manager,
                &mut meshes,
                &[],
                &[],
                &[(layer, world, false)],
            );

            mining.target = Some(hit.block);
            mining.originals.clear();
            mining.world_original = None;
            mining.surface_originals = vec![(layer, world, true)];
            mining.elapsed = 0.0;
            mining.duration = duration;
            mining.held_item = item;
            mining.previews = vec![preview];
            mining.phase = (hit.block.x as f32 * 1.7 + hit.block.z as f32 * 2.3).fract();
            mining.is_world = false;
            mining.is_surface = true;
            mining.surface_layer = Some(layer);
            mining.tree_plan = None;
            return;
        }

        let world_target = manager.world_terrain_target(hit.block);
        let cells = manager.terrain_cells_for_mining(hit.block);
        let is_world = world_target.is_some();
        let mut originals = Vec::new();
        let mut edits = Vec::new();
        for cell in cells {
            let Some(original) = manager.get_voxel_if_loaded(cell) else {
                continue;
            };
            if original.is_air() {
                continue;
            }
            originals.push((cell, original));
            edits.push((cell, Voxel::AIR));
        }
        if originals.is_empty() {
            return;
        }

        // Terrain is rendered from the 1 m layer, so remove the world voxel
        // too. Any natural surface flag on it is also cleared and restored on
        // cancel.
        let mut world_edits = Vec::new();
        let mut surface_edits = Vec::new();
        let mut surface_originals = Vec::new();
        let world_original = if let Some(world_pos) = world_target {
            manager.get_world_block(world_pos).map(|block| {
                world_edits.push((world_pos, Block::Air));
                for layer in SurfaceLayer::ALL {
                    if manager.get_world_surface(layer, world_pos).unwrap_or(false) {
                        surface_edits.push((layer, world_pos, false));
                        surface_originals.push((layer, world_pos, true));
                    }
                }
                (world_pos, block)
            })
        } else {
            None
        };

        // Remove the real voxels now and replace them with previews that can
        // shake and shrink. Collect the tree first: the clicked trunk cell is
        // about to disappear, and the plan needs the intact tree.
        let tree_plan = plan_felling(&manager, hit.block, &felling);
        apply_voxel_edits(
            &mut commands,
            &mut manager,
            &mut meshes,
            &world_edits,
            &edits,
            &surface_edits,
        );

        // An intact world voxel previews as one 1 m cube. Partial voxels and
        // detail objects keep 0.5 m cell previews so preserved construction
        // stays visible.
        let intact_world = is_world
            && originals
                .iter()
                .filter(|(_, voxel)| voxel.is_full_cube())
                .count()
                == DETAILS_PER_WORLD_VOXEL as usize;
        let mut previews = Vec::new();
        if intact_world {
            let (cell, original) = &originals[0];
            let material = preview_material(&mut materials, &mut visuals, original.block());
            let base = world_voxel_center(*cell);
            let scale = Vec3::splat(DETAILS_PER_WORLD_AXIS as f32);
            let anchor = base - Vec3::Y * (scale.y * 0.5);
            previews.push(
                commands
                    .spawn((
                        MiningPreview {
                            base,
                            scale,
                            rotation: Quat::IDENTITY,
                            anchor,
                        },
                        Mesh3d(visuals.preview_mesh.clone()),
                        MeshMaterial3d(material),
                        Transform::from_translation(base).with_scale(scale),
                        NotShadowCaster,
                    ))
                    .id(),
            );
        } else {
            for (cell, original) in &originals {
                let material = preview_material(&mut materials, &mut visuals, original.block());
                // Preview the real shape bounds, so a shaped piece shakes as a
                // thin or partial form instead of a full cube.
                let orientation = original.orientation();
                let centered = uses_centered_detail_bounds(original.shape());
                let (size, center) = shape_bounds(original.shape(), orientation);
                let rotation = if centered {
                    Quat::IDENTITY
                } else {
                    orientation.quat()
                };
                let base = cell.as_vec3()
                    + Vec3::splat(0.5)
                    + rotation * center
                    + Vec3::Y * manager.natural_detail_offset(*cell);
                let anchor = base - Vec3::Y * (size.y * 0.5);
                previews.push(
                    commands
                        .spawn((
                            MiningPreview {
                                base,
                                scale: size,
                                rotation,
                                anchor,
                            },
                            Mesh3d(visuals.preview_mesh.clone()),
                            MeshMaterial3d(material),
                            Transform::from_translation(base)
                                .with_rotation(rotation)
                                .with_scale(size),
                            NotShadowCaster,
                        ))
                        .id(),
                );
            }
        }

        mining.target = Some(hit.block);
        mining.originals = originals;
        mining.world_original = world_original;
        mining.surface_originals = surface_originals;
        mining.elapsed = 0.0;
        mining.duration = duration;
        mining.held_item = item;
        mining.previews = previews;
        mining.phase = (hit.block.x as f32 * 1.7 + hit.block.z as f32 * 2.3).fract();
        mining.is_world = is_world;
        mining.is_surface = false;
        mining.surface_layer = None;
        mining.tree_plan = tree_plan;
    } else {
        mining.elapsed += dt;
        if mining.elapsed >= mining.duration {
            let tree_plan = mining.tree_plan.take();
            complete_mining(
                &mut commands,
                &visuals,
                &asset_server,
                &mut manager,
                &mut structures,
                &mut structure_entities,
                &mut mining,
            );
            if let Some(plan) = tree_plan {
                fell_tree(
                    &mut commands,
                    &mut manager,
                    &mut meshes,
                    &visuals,
                    &asset_server,
                    plan,
                    &felling,
                );
            }
        }
    }
}

/// Adventure-style rule: a block only breaks with a suitable selected tool.
/// Loose sticks and pebbles stay hand-gatherable because they are pickups rather
/// than block harvesting.
#[cfg(test)]
fn mining_duration(tool: Option<ToolKind>, block: Block) -> Option<f32> {
    tool.and_then(|tool| tool.mining_time(block))
}

/// Mining duration for the currently held item, including primitive hand items
/// and specialized tools whose behavior depends on the target shape.
fn mining_duration_for_item(item: Option<ItemId>, block: Block, shape: Shape) -> Option<f32> {
    let item = item?;
    if let Some(tool) = tool_for_item(item) {
        if let Some(duration) = tool.mining_time(block) {
            return Some(duration);
        }
        return match tool {
            // Bark scraper removes bark from tree trunks/stems without acting as
            // a general wood-cutting tool.
            crate::tools::ToolKind::BarkScraper
                if block == Block::Wood && matches!(shape, Shape::Trunk | Shape::Stem) =>
            {
                block.break_time().map(|base| base * 2.0)
            }
            // Small stone hammer breaks natural stone deposits, not normal terrain.
            crate::tools::ToolKind::SmallStoneHammer
                if shape == Shape::StoneDeposit && block.is_stone() =>
            {
                block.break_time().map(|base| base * 1.9)
            }
            _ => None,
        };
    }
    match item {
        // A crude pointed stick can cut and pry loose foliage by hand.
        ItemId::StoneTippedStick if block == Block::Leaves => {
            block.break_time().map(|base| base * 2.0)
        }
        _ => None,
    }
}

fn cancel_mining(
    commands: &mut Commands,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    structure_entities: &mut StructureEntities,
    mining: &mut MiningState,
) {
    if mining.target.take().is_some() {
        if let Some(placed) = mining.structure_original.take() {
            // Restore hidden structure visual; structure was not removed from data.
            spawn_structure_visual(commands, meshes, materials, structure_entities, &placed);
            mining.structure_target = None;
        } else {
            let edits: Vec<_> = mining
                .originals
                .iter()
                .map(|(cell, voxel)| (*cell, *voxel))
                .collect();
            let world_edits: Vec<_> = mining.world_original.iter().copied().collect();
            let surface_edits = mining.surface_originals.clone();
            if !edits.is_empty() || !world_edits.is_empty() || !surface_edits.is_empty() {
                apply_voxel_edits(
                    commands,
                    manager,
                    meshes,
                    &world_edits,
                    &edits,
                    &surface_edits,
                );
            }
        }
    }
    for preview in mining.previews.drain(..) {
        commands.entity(preview).despawn();
    }
    mining.originals.clear();
    mining.world_original = None;
    mining.surface_originals.clear();
    mining.elapsed = 0.0;
    mining.duration = 0.0;
    mining.held_item = None;
    mining.is_world = false;
    mining.is_surface = false;
    mining.surface_layer = None;
    mining.tree_plan = None;
    mining.structure_target = None;
    mining.structure_original = None;
}

fn complete_mining(
    commands: &mut Commands,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    manager: &mut ChunkManager,
    structures: &mut Structures,
    structure_entities: &mut StructureEntities,
    mining: &mut MiningState,
) {
    let Some(target) = mining.target.take() else {
        return;
    };
    for preview in mining.previews.drain(..) {
        commands.entity(preview).despawn();
    }

    if let Some(sid) = mining.structure_target.take() {
        if let Some(placed) = structures.remove(sid) {
            let refund = match placed.kind {
                StructureKind::Workbench => ItemId::Workbench,
                StructureKind::Furnace => ItemId::Furnace,
            };
            let (min, max) = placed.collision_boxes().into_iter().fold(
                (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                |a, b| (a.0.min(b.0), a.1.max(b.1)),
            );
            let origin = (min + max) * 0.5;
            spawn_drop(commands, visuals, asset_server, refund, 1, origin);
        }
        // visual already hidden at start; ensure gone
        despawn_structure_visual(commands, structure_entities, sid);
        mining.structure_original = None;
        mining.elapsed = 0.0;
        mining.duration = 0.0;
        mining.held_item = None;
        mining.is_world = false;
        mining.is_surface = false;
        mining.surface_layer = None;
        mining.tree_plan = None;
        mining.originals.clear();
        mining.world_original = None;
        mining.surface_originals.clear();
        return;
    }

    let originals = std::mem::take(&mut mining.originals);
    if mining.is_surface {
        // A surface-only swing removed just the world-layer flag, so both turf
        // and snow reward one item at the surface centre.
        let centre = mining
            .surface_originals
            .first()
            .map(|(_, world, _)| surface_layer_center(*world))
            .unwrap_or(Vec3::ZERO);
        let Some(layer) = mining
            .surface_layer
            .or_else(|| mining.surface_originals.first().map(|(layer, _, _)| *layer))
        else {
            mining.surface_originals.clear();
            mining.is_surface = false;
            mining.surface_layer = None;
            return;
        };
        spawn_reward(
            commands,
            visuals,
            asset_server,
            natural_surface_harvest_reward(layer),
            centre,
        );
        mining.surface_originals.clear();
        mining.elapsed = 0.0;
        mining.duration = 0.0;
        mining.held_item = None;
        mining.is_world = false;
        mining.is_surface = false;
        mining.surface_layer = None;
        return;
    }

    // Bark scraping is a special non-destructive action: remove bark from the
    // trunk/stem, shrink it slightly to a debarked shape, and do not fell the tree.
    if mining.held_item == Some(ItemId::BarkScraper)
        && let Some((cell, voxel)) = originals.first().copied()
        && voxel.block() == Block::Wood
        && matches!(voxel.shape(), Shape::Trunk | Shape::Stem)
    {
        let debarked_shape = match voxel.shape() {
            Shape::Trunk => Shape::DebarkedTrunk,
            Shape::Stem => Shape::DebarkedStem,
            _ => voxel.shape(),
        };
        manager.set_voxel(cell, voxel.with_shape(debarked_shape));
        let centre = target.as_vec3() + Vec3::splat(0.5);
        spawn_drop(commands, visuals, asset_server, ItemId::Bark, 1, centre);
        mining.surface_originals.clear();
        mining.world_original = None;
        mining.elapsed = 0.0;
        mining.duration = 0.0;
        mining.held_item = None;
        mining.is_world = false;
        mining.is_surface = false;
        mining.surface_layer = None;
        mining.tree_plan = None;
        return;
    }

    if originals.is_empty() {
        return;
    }

    let reward = if mining.is_world {
        let world_block = mining
            .world_original
            .map(|(_, block)| block)
            .unwrap_or(Block::Air);
        // Side-mining a surfaced voxel also removes its flag, so the reward
        // names the visible layer instead of the rock or soil underneath.
        let surface = mining.surface_originals.first().map(|(layer, _, _)| *layer);
        world_surface_reward(world_block, surface)
    } else {
        detail_voxel_reward(originals[0].1)
    };
    let centre = average_centre(&originals);

    spawn_reward(commands, visuals, asset_server, reward, centre);
    if !mining.is_world
        && let Some((cell, voxel)) = originals.first()
        && voxel.block() == Block::Leaves
        && let Some(item) = crate::felling::leaf_drop_item(*cell)
    {
        spawn_drop(commands, visuals, asset_server, item, 1, centre);
    }

    // The 1 m layer was already cleared when the swing started, so finishing
    // only has to forget the rollback copy.
    mining.world_original = None;
    mining.surface_originals.clear();
    mining.elapsed = 0.0;
    mining.duration = 0.0;
    mining.held_item = None;
    mining.is_world = false;
    mining.is_surface = false;
    mining.surface_layer = None;
}

fn average_centre(originals: &[(IVec3, Voxel)]) -> Vec3 {
    let sum: Vec3 = originals
        .iter()
        .map(|(cell, _)| cell.as_vec3() + Vec3::splat(0.5))
        .sum();
    sum / originals.len() as f32
}

fn spawn_reward(
    commands: &mut Commands,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    reward: MiningReward,
    centre: Vec3,
) {
    if let Some(item) = reward.item
        && reward.item_count > 0
    {
        spawn_drop(
            commands,
            visuals,
            asset_server,
            item,
            reward.item_count,
            centre,
        );
    }
}

fn animate_mining_preview(
    time: Res<Time>,
    mining: Res<MiningState>,
    mut previews: Query<(&MiningPreview, &mut Transform)>,
) {
    if mining.target.is_none() {
        return;
    }

    let progress = (mining.elapsed / mining.duration.max(0.001)).clamp(0.0, 1.0);
    // More shake, slower/less shrink, and shrink toward the support below.
    let shake = 0.05 * (1.0 - progress * 0.35) + 0.008;
    let shrink = 1.0 - 0.45 * progress;
    let t = time.elapsed_secs();

    for preview in &mining.previews {
        let Ok((marker, mut transform)) = previews.get_mut(*preview) else {
            continue;
        };
        let shake_offset = Vec3::new(
            (t * 47.0 + mining.phase * 10.0).sin() * shake,
            (t * 61.0 + mining.phase * 17.0).sin() * shake * 0.35,
            (t * 53.0 + mining.phase * 23.0).sin() * shake,
        );
        // Lerp toward anchor as it shrinks, so it collapses onto the block below.
        let shrunk_base = marker.anchor + (marker.base - marker.anchor) * shrink;
        transform.translation = shrunk_base + shake_offset;
        transform.rotation = marker.rotation;
        transform.scale = marker.scale * shrink.max(0.35);
    }
}

fn preview_material(
    materials: &mut Assets<StandardMaterial>,
    visuals: &mut InteractionVisuals,
    block: Block,
) -> Handle<StandardMaterial> {
    if let Some(handle) = visuals.block_materials.get(&block) {
        return handle.clone();
    }

    let [r, g, b] = block.srgb();
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(r, g, b),
        perceptual_roughness: 0.90,
        metallic: 0.0,
        ..default()
    });
    visuals.block_materials.insert(block, handle.clone());
    handle
}

fn spawn_drop(
    commands: &mut Commands,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    item: ItemId,
    count: u32,
    position: Vec3,
) {
    spawn_drop_with_cooldown(commands, visuals, asset_server, item, count, position, 0.0);
}

fn spawn_drop_with_cooldown(
    commands: &mut Commands,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    item: ItemId,
    count: u32,
    position: Vec3,
    pickup_cooldown: f32,
) {
    let half_height = drop_mesh_size(item).y * 0.5;
    let mut entity = commands.spawn((
        BlockDrop {
            item,
            count,
            velocity: Vec3::new(
                (position.x * 12.3).sin() * 0.7,
                1.8,
                (position.z * 7.7).cos() * 0.7,
            ),
            half_height,
            settled: false,
            support_timer: DROP_SUPPORT_INTERVAL,
            pickup_cooldown: pickup_cooldown.max(0.0),
        },
        Transform::from_translation(position),
        Visibility::Inherited,
        Name::new(format!("Drop ({})", item.display_name())),
    ));

    if let Some(model) = crate::held_item::drop_model(item) {
        let scene: Handle<WorldAsset> = asset_server.load(model);
        entity.insert(WorldAssetRoot(scene));
    } else {
        let material = visuals
            .drop_materials
            .get(&item)
            .cloned()
            .unwrap_or_else(|| visuals.fallback_drop_material.clone());
        let mesh = visuals.drop_meshes.get(&item).cloned().unwrap_or_default();
        entity.insert((Mesh3d(mesh), MeshMaterial3d(material)));
    }
}

#[allow(clippy::too_many_arguments)]
fn update_drops(
    mut commands: Commands,
    time: Res<Time>,
    manager: Res<ChunkManager>,
    mut backpack: ResMut<Backpack>,
    mut pockets: ResMut<Pockets>,
    mut progression: ResMut<crate::progression::Progression>,
    mut feed: ResMut<crate::notifications::NotificationFeed>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut drops: Query<(Entity, &mut BlockDrop, &mut Transform)>,
) {
    let dt = time.delta_secs().min(0.05);
    let magnet_target = if backpack.has_attachment_item(crate::items::ItemId::Magnet) {
        camera
            .single()
            .ok()
            .map(|transform| transform.translation())
    } else {
        None
    };

    for (entity, mut drop, mut transform) in &mut drops {
        drop.pickup_cooldown = (drop.pickup_cooldown - dt).max(0.0);
        if let Some(target) = magnet_target {
            let dist = Vec2::new(
                transform.translation.x - target.x,
                transform.translation.z - target.z,
            )
            .length();
            if drop.pickup_cooldown <= 0.0 && dist <= 0.9 * BLOCKS_PER_METRE {
                let (item, count) = (drop.item, drop.count);
                let leftover = crate::items::store_item(&mut pockets, &mut backpack, item, count);
                let collected = count - leftover;
                if leftover == 0 {
                    commands.entity(entity).despawn();
                } else {
                    drop.count = leftover;
                }
                if collected > 0 {
                    progression.award_pickup(item, collected);
                    feed.push_item(item, collected);
                }
                continue;
            }
            if drop.pickup_cooldown <= 0.0
                && let Some(pull) =
                    magnet_pull(transform.translation, target, MAGNET_RADIUS, MAGNET_SPEED)
            {
                drop.velocity.x = pull.x;
                drop.velocity.z = pull.z;
                drop.settled = false;
            }
        }

        let probe = drop_support_cell(transform.translation, drop.half_height);
        let surface_top_y = support_surface_top_y(&manager, probe);
        let state = DropState {
            position: transform.translation,
            velocity: drop.velocity,
            settled: drop.settled,
            support_timer: drop.support_timer,
        };

        let step = step_drop(
            state,
            dt,
            DROP_GRAVITY,
            drop.half_height,
            surface_top_y,
            DROP_FLOOR_Y,
            DROP_SUPPORT_INTERVAL,
        );
        if step.despawn {
            commands.entity(entity).despawn();
            continue;
        }

        transform.translation = step.state.position;
        drop.velocity = step.state.velocity;
        drop.settled = step.state.settled;
        drop.support_timer = step.state.support_timer;
    }
}

/// Remove the planned tree, immediately drop its wood, and leave one falling
/// piece per leaf cell. The clicked cell is already gone; the rest is removed
/// here so a cancelled swing never destroys the tree.
fn fell_tree(
    commands: &mut Commands,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    plan: FellPlan,
    config: &FellingConfig,
) {
    let removed: Vec<_> = plan
        .cells
        .iter()
        .copied()
        .filter(|(cell, voxel)| manager.get_voxel_if_loaded(*cell) == Some(*voxel))
        .collect();
    if removed.is_empty() {
        return;
    }

    let edits: Vec<_> = removed
        .iter()
        .map(|(cell, _)| (*cell, Voxel::AIR))
        .collect();
    apply_voxel_edits(commands, manager, meshes, &[], &edits, &[]);

    let centre = average_centre(&removed);
    for (index, (item, count)) in wood_drop_counts(&removed).into_iter().enumerate() {
        let offset = Vec3::X * (index as f32 * 0.35);
        spawn_drop(
            commands,
            visuals,
            asset_server,
            item,
            count,
            centre + offset,
        );
    }

    let leaf_plan = FellPlan { cells: removed };
    for cell in leaf_cells(&leaf_plan) {
        spawn_leaf_piece(commands, visuals, cell, config);
    }
}

/// Spawn one full 0.5 m leaf block. Every cell owns its own fall delay, lifetime and
/// stick roll, so a canopy settles and decays piece by piece.
fn spawn_leaf_piece(
    commands: &mut Commands,
    visuals: &InteractionVisuals,
    cell: IVec3,
    config: &FellingConfig,
) {
    let material = visuals
        .drop_materials
        .get(&ItemId::Leaves)
        .cloned()
        .unwrap_or_default();
    let state = LeafPieceState {
        position: cell.as_vec3() + Vec3::splat(0.5),
        velocity_y: 0.0,
        fall_delay: leaf_fall_delay(cell),
        half_height: LEAF_PIECE_SIZE.y * 0.5,
        lifetime_after_settle: leaf_decay_lifetime(cell, config.leaf_lifetime),
        settled_for: None,
        drop_item: crate::felling::leaf_drop_item(cell),
    };

    commands.spawn((
        TreeDebris(state),
        Mesh3d(visuals.leaf_mesh.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(state.position),
        NotShadowCaster,
        Name::new("Falling leaf"),
    ));
}

fn update_tree_debris(
    mut commands: Commands,
    time: Res<Time>,
    manager: Res<ChunkManager>,
    visuals: Res<InteractionVisuals>,
    asset_server: Res<AssetServer>,
    config: Res<FellingConfig>,
    mut debris: Query<(Entity, &mut TreeDebris, &mut Transform)>,
) {
    let dt = time.delta_secs().min(0.05);

    for (entity, mut piece, mut transform) in &mut debris {
        let bottom = piece.0.position.y - piece.0.half_height;
        let probe = IVec3::new(
            piece.0.position.x.floor() as i32,
            (bottom - LEAF_SUPPORT_MARGIN).floor() as i32,
            piece.0.position.z.floor() as i32,
        );
        let surface_top_y = support_surface_top_y(&manager, probe);
        let step = step_leaf_piece(piece.0, dt, config.gravity, surface_top_y, DROP_FLOOR_Y);

        if step.despawn {
            if let Some(item) = step.drop_item {
                spawn_drop(
                    &mut commands,
                    &visuals,
                    &asset_server,
                    item,
                    1,
                    step.state.position,
                );
            }
            commands.entity(entity).despawn();
            continue;
        }

        piece.0 = step.state;
        transform.translation = step.state.position;
        transform.scale = Vec3::splat(step.fade);
    }
}

fn update_pickup_prompt(
    capture: Res<CursorCapture>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    drops: Query<(Entity, &Transform, &BlockDrop)>,
    mut targeted: ResMut<TargetedDrop>,
    mut prompt: ResMut<PickupPrompt>,
) {
    targeted.0 = None;
    prompt.visible = false;
    prompt.item = None;
    prompt.count = 0;

    if !capture.grabbed || capture.just_grabbed {
        return;
    }

    let Ok(camera) = camera.single() else {
        return;
    };
    let Some(entity) = find_targeted_drop(camera, &drops, REACH) else {
        return;
    };

    targeted.0 = Some(entity);
    if let Ok((_, _, drop)) = drops.get(entity) {
        prompt.text = format!("Hold RMB to pick up {}", drop.item.display_name());
        prompt.item = Some(drop.item);
        prompt.count = drop.count;
        prompt.visible = true;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DropCandidate {
    entity: Entity,
    position: Vec3,
}

/// Camera origin and forward direction used by drop targeting/nearby pickup.
fn camera_pose(camera: &GlobalTransform) -> (Vec3, Vec3) {
    (camera.translation(), camera.rotation() * Vec3::NEG_Z)
}

/// Distance along the view ray to a drop whose centre lies within the pickup
/// cylinder. Aimed pickup targets the closest eligible drop in front of the
/// camera (smallest `along`).
fn targeted_drop_distance(
    origin: Vec3,
    direction: Vec3,
    drop_pos: Vec3,
    max_distance: f32,
    pickup_radius: f32,
) -> Option<f32> {
    let offset = drop_pos - origin;
    let along = offset.dot(direction);
    if !(0.0..=max_distance).contains(&along) {
        return None;
    }

    let closest = origin + direction * along;
    if (drop_pos - closest).length_squared() > pickup_radius * pickup_radius {
        return None;
    }
    Some(along)
}

/// Score for nearby pickup fallback. Lower is better: prefer closer drops and,
/// when in front, reduce the score by facing so the crosshair still wins ties.
/// Drops behind the camera are only eligible when very close.
fn nearby_drop_priority(
    origin: Vec3,
    forward: Vec3,
    drop_pos: Vec3,
    near_radius: f32,
    close_radius: f32,
) -> Option<f32> {
    let offset = drop_pos - origin;
    let distance = offset.length();
    if distance > near_radius {
        return None;
    }

    let facing = offset.normalize_or_zero().dot(forward);
    if facing < 0.0 && distance > close_radius {
        return None;
    }

    Some(distance - facing.max(0.0) * 0.6)
}

fn best_drop_by_key<I, F>(candidates: I, mut key: F) -> Option<Entity>
where
    I: IntoIterator<Item = DropCandidate>,
    F: FnMut(Vec3) -> Option<f32>,
{
    let mut best: Option<(Entity, f32)> = None;
    for candidate in candidates {
        let Some(score) = key(candidate.position) else {
            continue;
        };
        if best.is_none_or(|(_, best_score)| score < best_score) {
            best = Some((candidate.entity, score));
        }
    }
    best.map(|(entity, _)| entity)
}

/// Spawns a player-dropped item with a short pickup cooldown so the same item
/// is not immediately collected by hold-to-pickup.
pub fn spawn_player_drop(
    commands: &mut Commands,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    item: ItemId,
    count: u32,
    position: Vec3,
) {
    spawn_drop_with_cooldown(commands, visuals, asset_server, item, count, position, 1.25);
}

fn find_targeted_drop(
    camera: &GlobalTransform,
    drops: &Query<(Entity, &Transform, &BlockDrop)>,
    max_distance: f32,
) -> Option<Entity> {
    let (origin, direction) = camera_pose(camera);
    best_drop_by_key(
        drops.iter().filter_map(|(entity, transform, drop)| {
            (drop.pickup_cooldown <= 0.0).then_some(DropCandidate {
                entity,
                position: transform.translation,
            })
        }),
        |position| {
            targeted_drop_distance(
                origin,
                direction,
                position,
                max_distance,
                DROP_PICKUP_RADIUS,
            )
        },
    )
}

/// Right click with a placeable item places it against the hit face and costs
/// one item from the pockets, then the backpack. Nothing is refunded after a
/// successful placement, and an invalid placement costs nothing.
#[allow(clippy::too_many_arguments)]
#[derive(bevy::ecs::system::SystemParam)]
struct StructureSpawn<'w, 's> {
    commands: Commands<'w, 's>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    structure_entities: ResMut<'w, StructureEntities>,
}

#[allow(clippy::too_many_arguments)]
fn handle_placement_actions(
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    crosshair: Res<CrosshairTarget>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    player_query: Query<(&Transform, &Player), Without<PlayerCamera>>,
    mut manager: ResMut<ChunkManager>,
    mut structures: ResMut<Structures>,
    mut spawn: StructureSpawn,
    place_rotation: Res<StructurePlaceRotation>,
) {
    if !capture.grabbed || capture.just_grabbed || !mouse.just_pressed(MouseButton::Right) {
        return;
    }
    let Some(stack) = pockets.selected_stack() else {
        return;
    };
    if tool_for_item(stack.item).is_some() {
        return;
    }
    let Ok((player_transform, player)) = player_query.single() else {
        return;
    };
    let Ok(camera) = camera.single() else {
        return;
    };
    let height = if player.crouching {
        CROUCH_HEIGHT
    } else {
        PLAYER_HEIGHT
    };

    let place_structure_kind = match stack.item {
        ItemId::Workbench => Some(StructureKind::Workbench),
        ItemId::Furnace => Some(StructureKind::Furnace),
        _ => None,
    };
    if let Some(kind) = place_structure_kind {
        let Some(hit) = crosshair.hit else {
            return;
        };
        let look = camera.rotation() * Vec3::NEG_Z;
        let facing = crate::world::block::Facing::facing_toward(Vec3::new(look.x, 0.0, look.z))
            .rotated_y(place_rotation.0);
        let (fw, fh) = kind.footprint();
        let (rfw, rfh) = match facing {
            crate::world::block::Facing::North | crate::world::block::Facing::South => (fw, fh),
            crate::world::block::Facing::East | crate::world::block::Facing::West => (fh, fw),
        };
        let hit_pos = camera.translation() + look * hit.distance;
        let mut center_x = (hit_pos.x / 2.0).round() as i32 * 2;
        let mut center_z = (hit_pos.z / 2.0).round() as i32 * 2;
        if (rfw / 2) % 2 == 1 {
            center_x += 1;
        }
        if (rfh / 2) % 2 == 1 {
            center_z += 1;
        }
        let anchor = IVec3::new(center_x - rfw / 2, hit.block.y + 1, center_z - rfh / 2);
        if !structures.can_place(
            &manager,
            kind,
            anchor,
            facing,
            Some((player_transform.translation, height)),
        ) {
            return;
        }
        if !pockets.consume_selected(1) && !backpack.consume(stack.item, 1) {
            return;
        }
        if let Some(id) = structures.place(kind, anchor, facing)
            && let Some(placed) = structures.placed.get(&id).cloned()
        {
            spawn_structure_visual(
                &mut spawn.commands,
                &mut spawn.meshes,
                &mut spawn.materials,
                &mut spawn.structure_entities,
                &placed,
            );
        }
        return;
    }

    let Some(material) = stack.item.def().place_block else {
        return;
    };
    let Some(hit) = crosshair.hit else {
        return;
    };
    let cell = hit.block + hit.normal;
    let look = camera.rotation() * Vec3::NEG_Z;
    let orientation = orientation_facing_player(look);
    let Some(dirty) = place_block_checked(
        &mut manager,
        cell,
        material,
        orientation,
        Some((player_transform.translation, height)),
    ) else {
        return;
    };

    if !pockets.consume_selected(1) {
        backpack.consume(stack.item, 1);
    }
    for coord in dirty {
        remesh_chunk(&mut spawn.commands, &mut manager, &mut spawn.meshes, coord);
    }
}

/// Right-click pickup. The hammer reserves right click for its shape menu, so
/// it skips both natural clutter and held drops.
#[derive(SystemParam)]
struct PickupActionsParams<'w, 's> {
    commands: Commands<'w, 's>,
    time: Res<'w, Time>,
    mouse: Res<'w, ButtonInput<MouseButton>>,
    capture: Res<'w, CursorCapture>,
    crosshair: Res<'w, CrosshairTarget>,
    structure_hit: Res<'w, StructureCrosshair>,
    camera: Query<'w, 's, &'static GlobalTransform, With<PlayerCamera>>,
    targeted: Res<'w, TargetedDrop>,
    manager: ResMut<'w, ChunkManager>,
    meshes: ResMut<'w, Assets<Mesh>>,
    visuals: Res<'w, InteractionVisuals>,
    asset_server: Res<'w, AssetServer>,
    pickup_timer: ResMut<'w, PickupTimer>,
    drops: Query<'w, 's, (Entity, &'static Transform, &'static mut BlockDrop)>,
    pockets: ResMut<'w, Pockets>,
    backpack: ResMut<'w, Backpack>,
    progression: ResMut<'w, Progression>,
    feed: ResMut<'w, NotificationFeed>,
}

fn handle_pickup_actions(mut p: PickupActionsParams) {
    if !p.capture.grabbed || p.capture.just_grabbed {
        p.pickup_timer.0 = 0.0;
        return;
    }

    let hammer = selected_tool(&p.pockets).is_some_and(ToolKind::is_hammer);
    if hammer {
        p.pickup_timer.0 = 0.0;
        return;
    }

    // RMB is interact; when targeting an interactable structure, let
    // structure interaction handle it instead of natural pickup.
    let voxel_dist = p.crosshair.hit.map(|h| h.distance);
    let structure_blocks_pickup = p
        .structure_hit
        .hit
        .is_some_and(|(sd, _, _)| voxel_dist.is_none_or(|vd| sd <= vd));
    if p.mouse.just_pressed(MouseButton::Right)
        && !structure_blocks_pickup
        && try_natural_pickup(
            &mut p.commands,
            p.crosshair.hit,
            &mut p.manager,
            &mut p.meshes,
            &p.visuals,
            &p.asset_server,
            &mut p.pockets,
            &mut p.backpack,
            &mut p.progression,
            &mut p.feed,
        )
    {
        return;
    }

    handle_pickup(
        &mut p.commands,
        &p.time,
        &p.mouse,
        &p.camera,
        &p.targeted,
        &mut p.pickup_timer.0,
        &mut p.drops,
        &mut p.pockets,
        &mut p.backpack,
        &mut p.progression,
        &mut p.feed,
    );
}

/// Right-click on a structure interacts with it (workbench opens crafting).
#[allow(clippy::too_many_arguments)]
fn handle_structure_interact(
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    pockets: Res<Pockets>,
    crosshair: Res<CrosshairTarget>,
    structure_hit: Res<StructureCrosshair>,
    structures: Res<Structures>,
    mut ui: ResMut<UiManager>,
    mut crafting: ResMut<CraftingUiState>,
    mut feed: ResMut<NotificationFeed>,
) {
    if !capture.grabbed || capture.just_grabbed || !mouse.just_pressed(MouseButton::Right) {
        return;
    }
    if selected_tool(&pockets).is_some_and(ToolKind::is_hammer) {
        return;
    }
    // If holding a placeable item, RMB places (handled by placement system).
    let holding_placeable = pockets.selected_stack().is_some_and(|s| {
        tool_for_item(s.item).is_none()
            && (s.item.def().place_block.is_some()
                || matches!(s.item, ItemId::Workbench | ItemId::Furnace))
    });
    if holding_placeable {
        return;
    }
    let voxel_dist = crosshair.hit.map(|h| h.distance);
    let Some((sdist, _, sid)) = structure_hit.hit else {
        return;
    };
    if voxel_dist.is_some_and(|vd| vd < sdist) {
        return;
    }
    let Some(placed) = structures.placed.get(&sid) else {
        return;
    };
    match placed.kind {
        StructureKind::Workbench => {
            crafting.station = Some(CraftingStationKind::Workbench);
            crafting.selected = 0;
            ui.open(Overlay::Backpack);
        }
        StructureKind::Furnace => {
            feed.push_status("Smelting is not available yet.", NotificationTone::Info);
        }
    }
}

/// Hammer: left click applies the selected shape to the targeted block.
#[allow(clippy::too_many_arguments)]
fn handle_hammer_actions(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    pockets: Res<Pockets>,
    construction: Res<ConstructionState>,
    crosshair: Res<CrosshairTarget>,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !capture.grabbed
        || capture.just_grabbed
        || !mouse.just_pressed(MouseButton::Left)
        || !selected_tool(&pockets).is_some_and(ToolKind::is_hammer)
    {
        return;
    }
    let Some(hit) = crosshair.hit else {
        return;
    };
    let Some(dirty) = apply_shape_checked(&mut manager, hit.block, construction.shape) else {
        return;
    };
    for coord in dirty {
        remesh_chunk(&mut commands, &mut manager, &mut meshes, coord);
    }
}

/// Wrench: left click advances the targeted block's orientation cycle.
fn handle_wrench_actions(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    capture: Res<CursorCapture>,
    pockets: Res<Pockets>,
    crosshair: Res<CrosshairTarget>,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !capture.grabbed
        || capture.just_grabbed
        || !mouse.just_pressed(MouseButton::Left)
        || !selected_tool(&pockets).is_some_and(ToolKind::is_wrench)
    {
        return;
    }
    let Some(hit) = crosshair.hit else {
        return;
    };
    let Some(dirty) = rotate_block_checked(&mut manager, hit.block) else {
        return;
    };
    for coord in dirty {
        remesh_chunk(&mut commands, &mut manager, &mut meshes, coord);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_pickup(
    commands: &mut Commands,
    time: &Time,
    mouse: &ButtonInput<MouseButton>,
    camera: &Query<&GlobalTransform, With<PlayerCamera>>,
    targeted: &TargetedDrop,
    pickup_timer: &mut f32,
    drops: &mut Query<(Entity, &Transform, &mut BlockDrop)>,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
    progression: &mut Progression,
    feed: &mut NotificationFeed,
) {
    if !mouse.pressed(MouseButton::Right) {
        *pickup_timer = 0.0;
        return;
    }

    *pickup_timer += time.delta_secs().min(0.05);
    if mouse.just_pressed(MouseButton::Right) {
        *pickup_timer = PICKUP_INTERVAL;
    }
    if *pickup_timer < PICKUP_INTERVAL {
        return;
    }
    *pickup_timer = 0.0;
    if let Ok(camera) = camera.single() {
        try_pickup(
            commands,
            camera,
            targeted,
            drops,
            pockets,
            backpack,
            progression,
            feed,
        );
    }
}

#[derive(Component)]
struct PlacementPreview;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_placement_preview(
    mut commands: Commands,
    capture: Res<CursorCapture>,
    pockets: Res<Pockets>,
    crosshair: Res<CrosshairTarget>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    #[allow(clippy::type_complexity)] player_query: Query<
        (&Transform, &Player),
        (Without<PlayerCamera>, Without<PlacementPreview>),
    >,
    manager: Res<ChunkManager>,
    structures: Res<Structures>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    preview: Query<Entity, (With<PlacementPreview>, Without<bevy::prelude::ChildOf>)>,
    place_rotation: Res<StructurePlaceRotation>,
) {
    let Ok((player_transform, player)) = player_query.single() else {
        return;
    };
    let Ok(camera) = camera.single() else {
        return;
    };
    let height = if player.crouching {
        CROUCH_HEIGHT
    } else {
        PLAYER_HEIGHT
    };

    let preview_kind = capture
        .grabbed
        .then(|| pockets.selected_stack())
        .flatten()
        .and_then(|s| match s.item {
            ItemId::Workbench => Some(StructureKind::Workbench),
            ItemId::Furnace => Some(StructureKind::Furnace),
            _ => None,
        });
    let Some(kind) = preview_kind else {
        for e in &preview {
            commands.entity(e).try_despawn();
        }
        return;
    };
    if capture.just_grabbed {
        for e in &preview {
            commands.entity(e).try_despawn();
        }
        return;
    }
    let Some(hit) = crosshair.hit else {
        for e in &preview {
            commands.entity(e).try_despawn();
        }
        return;
    };

    let look = camera.rotation() * Vec3::NEG_Z;
    let facing = crate::world::block::Facing::facing_toward(Vec3::new(look.x, 0.0, look.z))
        .rotated_y(place_rotation.0);
    let (fw, fh) = kind.footprint();
    let (rfw, rfh) = match facing {
        crate::world::block::Facing::North | crate::world::block::Facing::South => (fw, fh),
        crate::world::block::Facing::East | crate::world::block::Facing::West => (fh, fw),
    };
    // Snap center to 1m grid with parity so the footprint covers whole 1m blocks.
    // A 2m axis (4 detail) snaps between blocks; a 1m axis (2 detail) snaps to block center.
    let hit_pos = camera.translation() + look * hit.distance;
    let mut center_x = (hit_pos.x / 2.0).round() as i32 * 2;
    let mut center_z = (hit_pos.z / 2.0).round() as i32 * 2;
    if (rfw / 2) % 2 == 1 {
        center_x += 1;
    }
    if (rfh / 2) % 2 == 1 {
        center_z += 1;
    }
    let anchor = IVec3::new(center_x - rfw / 2, hit.block.y + 1, center_z - rfh / 2);
    let valid = structures.can_place(
        &manager,
        kind,
        anchor,
        facing,
        Some((player_transform.translation, height)),
    );
    let color = if valid {
        Color::srgba(0.45, 0.95, 0.45, 0.35)
    } else {
        Color::srgba(0.95, 0.35, 0.35, 0.35)
    };

    for e in &preview {
        commands.entity(e).try_despawn();
    }

    let mat = materials.add(StandardMaterial {
        base_color: color,
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        cull_mode: None,
        ..default()
    });
    let mut parent = commands.spawn((
        Name::new("PlacementPreview"),
        PlacementPreview,
        Transform::default(),
        GlobalTransform::default(),
        Visibility::Visible,
        InheritedVisibility::VISIBLE,
        ViewVisibility::default(),
    ));
    parent.with_children(|c| {
        for (min, max) in crate::structures::collision_boxes(kind, anchor, facing) {
            let size = max - min;
            let center = (min + max) * 0.5;
            c.spawn((
                Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
                MeshMaterial3d(mat.clone()),
                Transform::from_translation(center),
                GlobalTransform::default(),
                Visibility::Visible,
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
            ));
        }
    });
}

fn refresh_structure_highlight_meshes(
    structures: Res<Structures>,
    mut visuals: ResMut<InteractionVisuals>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !structures.is_changed() {
        return;
    }
    visuals
        .structure_highlight_meshes
        .retain(|id, _| structures.placed.contains_key(id));
    for placed in structures.iter() {
        if visuals.structure_highlight_meshes.contains_key(&placed.id) {
            continue;
        }
        let (min, max) = placed.collision_boxes().into_iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(amin, amax), (bmin, bmax)| (amin.min(bmin), amax.max(bmax)),
        );
        let size = max - min;
        visuals
            .structure_highlight_meshes
            .insert(placed.id, meshes.add(outline_mesh(size, Vec3::ZERO)));
    }
}
/// Keep the white selection outline on the block under the crosshair.
#[allow(clippy::type_complexity)]
fn update_block_highlight(
    capture: Res<CursorCapture>,
    crosshair: Res<CrosshairTarget>,
    structure_hit: Res<StructureCrosshair>,
    structures: Res<Structures>,
    manager: Res<ChunkManager>,
    visuals: Res<InteractionVisuals>,
    mut highlight: Query<
        (&mut Transform, &mut Mesh3d, &mut Visibility),
        (With<BlockHighlight>, Without<PlacementPreview>),
    >,
) {
    let Ok((mut transform, mut mesh, mut visibility)) = highlight.single_mut() else {
        return;
    };

    let visible = capture.grabbed && !capture.just_grabbed;
    if !visible {
        if *visibility != Visibility::Hidden {
            *visibility = Visibility::Hidden;
        }
        return;
    }

    let voxel_dist = crosshair.hit.map(|h| h.distance);
    if let Some((sdist, _, sid)) = structure_hit.hit
        && voxel_dist.is_none_or(|vd| sdist <= vd)
        && let Some(placed) = structures.placed.get(&sid)
    {
        let (min, max) = placed.collision_boxes().into_iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(amin, amax), (bmin, bmax)| (amin.min(bmin), amax.max(bmax)),
        );
        let center = (min + max) * 0.5;
        let _size = max - min;
        let handle = visuals
            .structure_highlight_meshes
            .get(&sid)
            .cloned()
            .unwrap_or_else(|| visuals.world_highlight_mesh.clone());
        if mesh.0 != handle {
            mesh.0 = handle;
        }
        transform.translation = center;
        transform.rotation = Quat::IDENTITY;
        transform.scale = Vec3::ONE;
        if *visibility != Visibility::Visible {
            *visibility = Visibility::Visible;
        }
        return;
    }

    let Some(hit) = crosshair.hit else {
        *visibility = Visibility::Hidden;
        return;
    };

    let voxel = manager.get_voxel_if_loaded(hit.block).unwrap_or_default();
    // Turf and snow are raised 0.1 m layers above a world voxel, so only a
    // click on their top face outlines the thin surface; the sides still
    // outline the whole voxel.
    let natural_surface = if hit.normal == IVec3::Y {
        SurfaceLayer::ALL.iter().rev().find_map(|layer| {
            manager
                .natural_surface_world(*layer, hit.block)
                .map(|world| (*layer, world))
        })
    } else {
        None
    };
    let (handle, translation, rotation) = if let Some((_, world)) = natural_surface {
        // Any natural surface spans the whole 1 m top of its world voxel.
        (
            visuals.surface_highlight_mesh.clone(),
            surface_layer_center(world),
            Quat::IDENTITY,
        )
    } else if let Some(world) = manager.world_terrain_target(hit.block) {
        // A whole 1 m terrain voxel gets a 1 m outline, so the selection matches
        // the block that mining will actually remove. `world_terrain_target`
        // returns world coordinates, while `world_voxel_center` expects a
        // detail cell, so convert back to the voxel's detail origin first.
        (
            visuals.world_highlight_mesh.clone(),
            world_voxel_center(world_to_detail_origin(world)),
            Quat::IDENTITY,
        )
    } else {
        // Detail objects keep their own 0.5 m footprint and shape. The
        // outline follows the voxel's full orientation, so rotated slopes,
        // stairs, panels and natural sticks line up with their geometry.
        let shape = voxel.shape();
        let orientation = voxel.orientation();
        let centered = uses_centered_detail_bounds(shape);
        let variant = if shape == Shape::Stem {
            let up = orientation.up();
            if up.x > 0.5 {
                1
            } else if up.x < -0.5 {
                2
            } else if up.z > 0.5 {
                3
            } else if up.z < -0.5 {
                4
            } else {
                0
            }
        } else if matches!(shape, Shape::Stick | Shape::Pebble | Shape::StoneDeposit) {
            orientation.index()
        } else {
            0
        };
        let handle = visuals
            .highlight_meshes
            .get(&(shape, variant))
            .cloned()
            .unwrap_or_default();
        (
            handle,
            hit.block.as_vec3()
                + Vec3::splat(0.5)
                + Vec3::Y * manager.natural_detail_offset(hit.block),
            if centered {
                Quat::IDENTITY
            } else {
                orientation.quat()
            },
        )
    };

    if mesh.0 != handle {
        mesh.0 = handle;
    }
    transform.translation = translation;
    transform.rotation = rotation;
    transform.scale = Vec3::ONE;
    if *visibility != Visibility::Visible {
        *visibility = Visibility::Visible;
    }
}

/// Instant right-click gather for loose ground clutter. Returns `true` when a
/// natural pebble or ground stick was removed.
#[allow(clippy::too_many_arguments)]
fn try_natural_pickup(
    commands: &mut Commands,
    hit: Option<crate::world::RayHit>,
    manager: &mut ChunkManager,
    meshes: &mut Assets<Mesh>,
    visuals: &InteractionVisuals,
    asset_server: &AssetServer,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
    progression: &mut Progression,
    feed: &mut NotificationFeed,
) -> bool {
    let Some(hit) = hit else {
        return false;
    };
    let Some(voxel) = manager.get_voxel_if_loaded(hit.block) else {
        return false;
    };

    let loose = match voxel.shape() {
        Shape::Pebble => true,
        // Only ground sticks are loose. A stick-shaped tree limb has wood or
        // air below it, not a terrain cube.
        Shape::Stick => manager
            .get_voxel_if_loaded(hit.block - IVec3::Y)
            .is_some_and(|below| {
                below.is_full_cube()
                    && matches!(
                        below.block(),
                        Block::Topsoil
                            | Block::Sand
                            | Block::Snow
                            | Block::Granite
                            | Block::Slate
                            | Block::Shale
                            | Block::Sandstone
                            | Block::Limestone
                    )
            }),
        _ => false,
    };
    if !loose {
        return false;
    }

    let reward = detail_voxel_reward(voxel);
    let Some(item) = reward.item else {
        return false;
    };
    let local_centre = if uses_centered_detail_bounds(voxel.shape()) {
        let (min, max) = centered_detail_bounds(voxel.shape());
        (min + max) * 0.5
    } else {
        Vec3::splat(0.5)
    };
    let centre =
        hit.block.as_vec3() + local_centre + Vec3::Y * manager.natural_detail_offset(hit.block);
    apply_voxel_edits(
        commands,
        manager,
        meshes,
        &[],
        &[(hit.block, Voxel::AIR)],
        &[],
    );

    let leftover = store_item(pockets, backpack, item, reward.item_count);
    let collected = reward.item_count - leftover;
    if leftover > 0 {
        spawn_drop(commands, visuals, asset_server, item, leftover, centre);
    }
    progression.award_pickup(item, collected);
    feed.push_item(item, collected);
    true
}

#[allow(clippy::too_many_arguments)]
fn try_pickup(
    commands: &mut Commands,
    camera: &GlobalTransform,
    targeted: &TargetedDrop,
    drops: &mut Query<(Entity, &Transform, &mut BlockDrop)>,
    pockets: &mut Pockets,
    backpack: &mut Backpack,
    progression: &mut Progression,
    feed: &mut NotificationFeed,
) {
    let entity = targeted
        .0
        .filter(|entity| drops.get(*entity).is_ok())
        .or_else(|| nearest_drop(camera, drops));

    let Some(entity) = entity else {
        return;
    };
    let Ok((_, _, drop)) = drops.get(entity) else {
        return;
    };
    let (item, count) = (drop.item, drop.count);

    let leftover = store_item(pockets, backpack, item, count);
    let collected = count - leftover;
    if leftover == 0 {
        commands.entity(entity).despawn();
    } else if let Ok((_, _, mut drop)) = drops.get_mut(entity) {
        drop.count = leftover;
    }
    progression.award_pickup(item, collected);
    feed.push_item(item, collected);
}

/// Find the closest drop around the camera, preferring what the player is
/// looking at but still allowing very close drops behind the crosshair.
fn nearest_drop(
    camera: &GlobalTransform,
    drops: &Query<(Entity, &Transform, &mut BlockDrop)>,
) -> Option<Entity> {
    let (origin, forward) = camera_pose(camera);
    best_drop_by_key(
        drops.iter().map(|(entity, transform, _)| DropCandidate {
            entity,
            position: transform.translation,
        }),
        |position| {
            nearby_drop_priority(
                origin,
                forward,
                position,
                PICKUP_NEAR_RADIUS,
                PICKUP_CLOSE_RADIUS,
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::chunk::{Chunk, ChunkData, world_voxel_cells};
    use bevy::prelude::Entity;

    fn manager_with_surface(layer: SurfaceLayer, block: Block) -> (ChunkManager, IVec3) {
        use crate::world::chunk::{
            detail_to_chunk, detail_to_local, world_to_chunk, world_to_local,
        };

        let mut manager = ChunkManager::default();
        let world = IVec3::new(4, 4, 4);
        let coord = world_to_chunk(world);
        let (lx, ly, lz) = world_to_local(world);
        let mut data = ChunkData::new(coord);
        data.set_world_local(lx, ly, lz, block);
        match layer {
            SurfaceLayer::Turf => data.set_world_turf_local(lx, ly, lz, true),
            SurfaceLayer::Snow => data.set_world_snow_local(lx, ly, lz, true),
        }
        manager.chunks.insert(
            coord,
            Chunk {
                data,
                entity: Entity::PLACEHOLDER,
                water_entity: None,
                foliage_entity: None,
                triangles: 0,
                water_triangles: 0,
                foliage_triangles: 0,
            },
        );
        for cell in world_voxel_cells(world) {
            let cell_coord = detail_to_chunk(cell);
            let (cx, cy, cz) = detail_to_local(cell);
            manager
                .chunks
                .entry(cell_coord)
                .or_insert_with(|| Chunk {
                    data: ChunkData::new(cell_coord),
                    entity: Entity::PLACEHOLDER,
                    water_entity: None,
                    foliage_entity: None,
                    triangles: 0,
                    water_triangles: 0,
                    foliage_triangles: 0,
                })
                .data
                .set_local(cx, cy, cz, block);
        }
        (manager, world)
    }

    fn manager_with_turf() -> (ChunkManager, IVec3) {
        manager_with_surface(SurfaceLayer::Turf, Block::Topsoil)
    }

    #[test]
    fn blocks_require_the_correct_tool() {
        for block in [
            Block::Turf,
            Block::Topsoil,
            Block::Sand,
            Block::Snow,
            Block::Wood,
            Block::Leaves,
            Block::Granite,
        ] {
            assert_eq!(mining_duration(None, block), None, "{block:?}");
        }
        assert_eq!(
            mining_duration(Some(ToolKind::Axe), Block::Wood),
            Some(0.5525)
        );
        let shovel_time = mining_duration(Some(ToolKind::Shovel), Block::Topsoil).unwrap();
        assert!((shovel_time - 0.36).abs() < 1e-6);
        assert_eq!(
            mining_duration(Some(ToolKind::Pickaxe), Block::Granite),
            Some(0.95)
        );
    }

    #[test]
    fn hammer_does_not_mine() {
        assert_eq!(
            mining_duration(Some(ToolKind::Hammer), Block::Granite),
            None
        );
    }

    #[test]
    fn pickaxe_mines_new_geology() {
        assert_eq!(
            mining_duration(Some(ToolKind::Pickaxe), Block::Granite),
            Some(0.95)
        );
    }

    #[test]
    fn world_highlight_is_centred_on_the_world_voxel() {
        // `world_terrain_target` hands back world coordinates; converting to the
        // detail origin must place the 2-unit outline around detail 8..10.
        let world = IVec3::new(4, 64, 4);
        assert_eq!(
            world_voxel_center(world_to_detail_origin(world)),
            Vec3::new(9.0, 129.0, 9.0)
        );
    }

    #[test]
    fn surface_highlight_covers_the_whole_thin_surface() {
        // Natural surfaces are raised 0.1 m slabs above world voxel (4, 64, 4),
        // whose top plane is detail y 130. The 1 m x 0.1 m x 1 m band is centred
        // at (9, 130 + PANEL_HEIGHT / 2, 9).
        let center = surface_layer_center(IVec3::new(4, 64, 4));
        assert_eq!(center, Vec3::new(9.0, 130.0 + PANEL_HEIGHT * 0.5, 9.0));
    }

    #[test]
    fn highlight_outline_hugs_its_shape_without_sinking_into_it() {
        let flat_stem = Orientation::from_up_forward(Vec3::X, Vec3::Y);
        for shape in Shape::ALL {
            for sample in [Orientation::default(), flat_stem] {
                let (size, center) = shape_bounds(shape, sample);
                let min = center - size * 0.5 - Vec3::splat(0.08);
                let max = center + size * 0.5 + Vec3::splat(0.08);
                let mesh = outline_mesh(size, center);
                let positions = mesh
                    .attribute(Mesh::ATTRIBUTE_POSITION)
                    .and_then(|values| values.as_float3())
                    .expect("outline mesh has positions");

                let mut escaped = false;
                for position in positions {
                    let point = Vec3::from_array(*position);
                    if !(point.cmpge(min).all() && point.cmple(max).all()) {
                        escaped = true;
                    }
                }
                assert!(
                    !escaped,
                    "{shape:?} outline escaped bounds {min:?}..{max:?}"
                );
            }
        }
    }

    fn falling_state() -> DropState {
        DropState {
            position: Vec3::new(0.0, 10.0, 0.0),
            velocity: Vec3::ZERO,
            settled: false,
            support_timer: DROP_SUPPORT_INTERVAL,
        }
    }

    #[test]
    fn aimed_drop_targeting_uses_ray_distance_and_pickup_radius() {
        let origin = Vec3::ZERO;
        let direction = Vec3::NEG_Z;

        assert_eq!(
            targeted_drop_distance(origin, direction, Vec3::new(0.0, 0.0, -2.0), 5.0, 0.5),
            Some(2.0)
        );
        // Within the pickup cylinder but off the ray centre.
        assert_eq!(
            targeted_drop_distance(origin, direction, Vec3::new(0.4, 0.0, -2.0), 5.0, 0.5),
            Some(2.0)
        );
        // Outside the pickup radius around the ray.
        assert_eq!(
            targeted_drop_distance(origin, direction, Vec3::new(0.6, 0.0, -2.0), 5.0, 0.5),
            None
        );
        // Behind the camera or beyond reach.
        assert_eq!(
            targeted_drop_distance(origin, direction, Vec3::new(0.0, 0.0, 1.0), 5.0, 0.5),
            None
        );
        assert_eq!(
            targeted_drop_distance(origin, direction, Vec3::new(0.0, 0.0, -6.0), 5.0, 0.5),
            None
        );
    }

    #[test]
    fn aimed_drop_selection_picks_closest_along_the_ray() {
        let origin = Vec3::ZERO;
        let direction = Vec3::NEG_Z;
        let near = DropCandidate {
            entity: Entity::from_raw_u32(1).expect("valid entity"),
            position: Vec3::new(0.1, 0.0, -1.5),
        };
        let far = DropCandidate {
            entity: Entity::from_raw_u32(2).expect("valid entity"),
            position: Vec3::new(0.0, 0.0, -3.0),
        };
        let picked = best_drop_by_key([far, near], |position| {
            targeted_drop_distance(origin, direction, position, 5.0, DROP_PICKUP_RADIUS)
        });
        assert_eq!(picked, Some(near.entity));
    }

    #[test]
    fn nearby_drop_pickup_prefers_facing_and_allows_only_very_close_behind() {
        let origin = Vec3::ZERO;
        let forward = Vec3::NEG_Z;

        // In front and within near radius: eligible, facing lowers the score.
        assert!(
            nearby_drop_priority(origin, forward, Vec3::new(0.0, 0.0, -2.0), 3.0, 1.0).is_some()
        );
        // Behind but very close: eligible.
        assert!(
            nearby_drop_priority(origin, forward, Vec3::new(0.0, 0.0, 0.8), 3.0, 1.0).is_some()
        );
        // Behind and beyond the close fallback radius: not eligible.
        assert_eq!(
            nearby_drop_priority(origin, forward, Vec3::new(0.0, 0.0, 1.5), 3.0, 1.0),
            None
        );
        // Too far away in any direction.
        assert_eq!(
            nearby_drop_priority(origin, forward, Vec3::new(0.0, 0.0, -4.0), 3.0, 1.0),
            None
        );

        let front = DropCandidate {
            entity: Entity::from_raw_u32(10).expect("valid entity"),
            position: Vec3::new(0.0, 0.0, -1.2),
        };
        let side_close = DropCandidate {
            entity: Entity::from_raw_u32(11).expect("valid entity"),
            position: Vec3::new(1.1, 0.0, -0.2),
        };
        let picked = best_drop_by_key([side_close, front], |position| {
            nearby_drop_priority(origin, forward, position, 3.0, 1.0)
        });
        assert_eq!(
            picked,
            Some(front.entity),
            "facing drop should win over a similarly close side drop"
        );
    }

    #[test]
    fn magnet_pull_draws_nearby_drops_toward_the_player() {
        let drop = Vec3::new(2.0, 8.0, 0.0);
        let player = Vec3::new(4.0, 12.0, 0.0);
        assert_eq!(
            magnet_pull(drop, player, 10.0, 2.0),
            Some(Vec3::new(2.0, 0.0, 0.0))
        );

        let diagonal = magnet_pull(Vec3::new(3.0, 0.0, 4.0), Vec3::ZERO, 10.0, 2.0)
            .expect("drop inside radius is attracted");
        assert!((diagonal.length() - 2.0).abs() < 0.00001);
        assert!(diagonal.x < 0.0 && diagonal.z < 0.0);
        assert_eq!(diagonal.y, 0.0);
    }

    #[test]
    fn magnet_pull_ignores_distant_drops_and_pure_height_differences() {
        assert_eq!(
            magnet_pull(Vec3::new(20.0, 0.0, 0.0), Vec3::ZERO, 10.0, 2.0),
            None
        );
        assert_eq!(
            magnet_pull(Vec3::new(0.0, 50.0, 0.0), Vec3::ZERO, 10.0, 2.0),
            None
        );
    }

    #[test]
    fn step_drop_falls_without_support() {
        let step = step_drop(
            falling_state(),
            0.05,
            DROP_GRAVITY,
            DROP_SIZE * 0.5,
            None,
            DROP_FLOOR_Y,
            DROP_SUPPORT_INTERVAL,
        );
        assert!(!step.state.settled);
        assert!(step.state.position.y < 10.0);
        assert!(step.state.velocity.y < 0.0);
        assert!(!step.despawn);
    }

    #[test]
    fn step_drop_snaps_and_settles_on_support() {
        let state = DropState {
            position: Vec3::new(0.0, 4.20, 0.0),
            velocity: Vec3::new(0.0, -1.0, 0.0),
            ..falling_state()
        };
        let step = step_drop(
            state,
            0.05,
            DROP_GRAVITY,
            DROP_SIZE * 0.5,
            Some(4.0),
            DROP_FLOOR_Y,
            DROP_SUPPORT_INTERVAL,
        );
        assert!(step.state.settled);
        assert!((step.state.position.y - 4.16).abs() < 0.00001);
        assert_eq!(step.state.velocity, Vec3::ZERO);
        assert_eq!(step.state.support_timer, DROP_SUPPORT_INTERVAL);
    }

    #[test]
    fn settled_drop_never_bobs_and_rechecks_support() {
        let mut state = DropState {
            position: Vec3::new(0.0, 4.16, 0.0),
            velocity: Vec3::ZERO,
            settled: true,
            support_timer: DROP_SUPPORT_INTERVAL,
        };

        for _ in 0..4 {
            let step = step_drop(
                state,
                0.02,
                DROP_GRAVITY,
                DROP_SIZE * 0.5,
                Some(4.0),
                DROP_FLOOR_Y,
                DROP_SUPPORT_INTERVAL,
            );
            assert_eq!(step.state.position, state.position);
            assert_eq!(step.state.velocity, Vec3::ZERO);
            assert!(step.state.settled);
            state = step.state;
        }

        state.support_timer = 0.01;
        let step = step_drop(
            state,
            0.05,
            DROP_GRAVITY,
            DROP_SIZE * 0.5,
            None,
            DROP_FLOOR_Y,
            DROP_SUPPORT_INTERVAL,
        );
        assert!(!step.state.settled);
        assert!(step.state.position.y < state.position.y);
        assert_eq!(step.state.support_timer, DROP_SUPPORT_INTERVAL);
    }

    #[test]
    fn step_drop_clamps_tiny_velocities() {
        let state = DropState {
            position: Vec3::new(1.0, 5.0, 2.0),
            velocity: Vec3::new(0.01, -0.01, 0.01),
            ..falling_state()
        };
        let step = step_drop(
            state,
            0.05,
            0.0,
            DROP_SIZE * 0.5,
            None,
            DROP_FLOOR_Y,
            DROP_SUPPORT_INTERVAL,
        );
        assert_eq!(step.state.velocity, Vec3::ZERO);
        assert_eq!(step.state.position, state.position);
    }

    #[test]
    fn step_drop_despawns_below_the_world_floor() {
        let state = DropState {
            position: Vec3::new(0.0, DROP_FLOOR_Y - 1.0, 0.0),
            ..falling_state()
        };
        let step = step_drop(
            state,
            0.02,
            DROP_GRAVITY,
            DROP_SIZE * 0.5,
            None,
            DROP_FLOOR_Y,
            DROP_SUPPORT_INTERVAL,
        );
        assert!(step.despawn);
    }

    #[test]
    fn support_probe_sits_below_the_drop() {
        assert_eq!(
            drop_support_cell(Vec3::new(3.2, 8.0, 5.7), DROP_SIZE * 0.5),
            IVec3::new(3, 7, 5)
        );
    }

    #[test]
    fn dropped_stick_has_a_stick_shaped_mesh() {
        let size = drop_mesh_size(ItemId::Stick);
        assert!(size.x > size.y * 4.0);
        assert!(size.x > size.z * 3.0);
        assert_ne!(size, Vec3::splat(DROP_SIZE));
        assert_eq!(drop_mesh_size(ItemId::Leaves), Vec3::ONE);
        assert_eq!(LEAF_PIECE_SIZE, Vec3::ONE);
        assert_eq!(drop_mesh_size(ItemId::Wood), Vec3::splat(DROP_SIZE));
    }

    #[test]
    fn support_uses_the_raised_turf_surface() {
        let (manager, world) = manager_with_turf();
        let soil = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS - 1,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        let surface = soil + IVec3::Y;
        let expected = soil.y as f32 + 1.0 + PANEL_HEIGHT;

        // A fast drop can end the frame inside the soil cell; the turf above it
        // is still the surface it must rest on.
        assert_eq!(support_surface_top_y(&manager, soil), Some(expected));
        assert_eq!(
            support_surface_top_y(&manager, surface),
            Some(surface.y as f32 + PANEL_HEIGHT)
        );
        assert_eq!(expected, surface.y as f32 + PANEL_HEIGHT);

        let mut bare = manager;
        bare.set_world_turf(world, false);
        assert_eq!(
            support_surface_top_y(&bare, soil),
            Some(soil.y as f32 + 1.0)
        );
        assert_eq!(support_surface_top_y(&bare, surface), None);
    }

    #[test]
    fn support_uses_the_raised_snow_surface() {
        let (manager, world) = manager_with_surface(SurfaceLayer::Snow, Block::Granite);
        let rock = IVec3::new(
            world.x * DETAILS_PER_WORLD_AXIS,
            (world.y + 1) * DETAILS_PER_WORLD_AXIS - 1,
            world.z * DETAILS_PER_WORLD_AXIS,
        );
        let surface = rock + IVec3::Y;
        let expected = rock.y as f32 + 1.0 + PANEL_HEIGHT;

        assert_eq!(support_surface_top_y(&manager, rock), Some(expected));
        assert_eq!(
            support_surface_top_y(&manager, surface),
            Some(surface.y as f32 + PANEL_HEIGHT)
        );

        let mut bare = manager;
        bare.set_world_surface(SurfaceLayer::Snow, world, false);
        assert_eq!(
            support_surface_top_y(&bare, rock),
            Some(rock.y as f32 + 1.0)
        );
        assert_eq!(support_surface_top_y(&bare, surface), None);
    }

    fn falling_leaf() -> LeafPieceState {
        LeafPieceState {
            position: Vec3::new(3.2, 5.0, -1.0),
            velocity_y: 0.0,
            fall_delay: 0.0,
            half_height: LEAF_PIECE_SIZE.y * 0.5,
            lifetime_after_settle: 1.0,
            settled_for: None,
            drop_item: None,
        }
    }

    #[test]
    fn leaf_piece_falls_straight_down() {
        let mut state = falling_leaf();
        for _ in 0..10 {
            state = step_leaf_piece(state, 0.05, 36.0, None, DROP_FLOOR_Y).state;
            assert_eq!(state.position.x, 3.2);
            assert_eq!(state.position.z, -1.0);
        }
        assert!(state.position.y < 5.0);
        assert!(state.velocity_y < 0.0);
    }

    #[test]
    fn leaf_piece_waits_settles_and_fades_independently() {
        let mut state = falling_leaf();
        state.fall_delay = 0.1;
        let step = step_leaf_piece(state, 0.05, 36.0, Some(3.0), DROP_FLOOR_Y);
        assert_eq!(step.state.position, state.position);
        assert_eq!(step.state.fall_delay, 0.05);

        let mut state = falling_leaf();
        state.position.y = 4.1;
        let step = step_leaf_piece(state, 0.05, 36.0, Some(4.0), DROP_FLOOR_Y);
        assert_eq!(step.state.position.y, 4.0 + state.half_height);
        assert_eq!(step.state.velocity_y, 0.0);
        assert_eq!(step.state.settled_for, Some(0.0));

        let settled = step.state;
        let step = step_leaf_piece(settled, 0.2, 36.0, Some(4.0), DROP_FLOOR_Y);
        assert_eq!(step.state.position, settled.position);
        assert!(step.fade < 1.0);
        assert!(!step.despawn);
    }

    #[test]
    fn leaf_piece_decays_once_with_optional_drop() {
        let mut state = falling_leaf();
        state.position.y = 4.03;
        state.settled_for = Some(0.0);
        state.lifetime_after_settle = 0.1;
        state.drop_item = Some(ItemId::Stick);

        let step = step_leaf_piece(state, 0.05, 36.0, Some(4.0), DROP_FLOOR_Y);
        assert!(!step.despawn);
        assert_eq!(step.drop_item, None);

        let step = step_leaf_piece(step.state, 0.05, 36.0, Some(4.0), DROP_FLOOR_Y);
        assert!(step.despawn);
        assert_eq!(step.drop_item, Some(ItemId::Stick));

        state.drop_item = None;
        state.settled_for = Some(0.06);
        let step = step_leaf_piece(state, 0.2, 36.0, Some(4.0), DROP_FLOOR_Y);
        assert!(step.despawn);
        assert_eq!(step.drop_item, None);
    }

    #[test]
    fn stone_tipped_stick_can_break_foliage() {
        assert!(
            mining_duration_for_item(
                Some(ItemId::StoneTippedStick),
                Block::Leaves,
                Shape::Foliage
            )
            .is_some(),
            "stone tipped stick should cut leaves/foliage"
        );
        assert_eq!(
            mining_duration_for_item(Some(ItemId::StoneTippedStick), Block::Granite, Shape::Cube),
            None
        );
        assert_eq!(
            mining_duration_for_item(Some(ItemId::Stick), Block::Leaves, Shape::Foliage),
            None
        );
    }
}
