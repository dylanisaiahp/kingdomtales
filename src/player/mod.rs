//! First-person player controller with voxel AABB collision and block editing.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::light::ShadowFilteringMethod;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::chat::ChatState;
use crate::crafting::CraftingUiState;
use crate::pockets::Pockets;
use crate::settings::{GameSettings, InputBindings, MenuState, SURFACE_FOG_COLOR};
use crate::structures::Structures;
use crate::ui_manager::{Overlay, UiManager};
use crate::world::block::{Block, Voxel};
use crate::world::chunk::{BLOCKS_PER_METRE, DETAIL_Y_MIN};
use crate::world::{ChunkManager, SpawnPoint};

const PLAYER_HALF_WIDTH: f32 = 0.30 * BLOCKS_PER_METRE;
pub const PLAYER_HEIGHT: f32 = 1.80 * BLOCKS_PER_METRE;
pub const CROUCH_HEIGHT: f32 = 1.20 * BLOCKS_PER_METRE;
const EYE_HEIGHT: f32 = 1.62 * BLOCKS_PER_METRE;
const CROUCH_EYE_HEIGHT: f32 = 1.02 * BLOCKS_PER_METRE;

const WALK_SPEED: f32 = 4.8 * BLOCKS_PER_METRE;
const CROUCH_SPEED: f32 = 2.2 * BLOCKS_PER_METRE;
const SPRINT_SPEED: f32 = 7.8 * BLOCKS_PER_METRE;
const SWIM_SPEED: f32 = 3.2 * BLOCKS_PER_METRE;
const FLY_SPEED: f32 = 14.0 * BLOCKS_PER_METRE;
const FLY_SPRINT_SPEED: f32 = 28.0 * BLOCKS_PER_METRE;
const FLY_ACCELERATION: f32 = 70.0 * BLOCKS_PER_METRE;
const FLY_DECELERATION: f32 = 40.0 * BLOCKS_PER_METRE;

const GRAVITY: f32 = 24.0 * BLOCKS_PER_METRE;
const JUMP_SPEED: f32 = 8.4 * BLOCKS_PER_METRE;
const TERMINAL_VELOCITY: f32 = 50.0 * BLOCKS_PER_METRE;

/// Horizontal acceleration/deceleration. The lower deceleration gives the
/// controller a little momentum instead of stopping on the exact release frame.
const GROUND_ACCELERATION: f32 = 42.0 * BLOCKS_PER_METRE;
const GROUND_DECELERATION: f32 = 16.0 * BLOCKS_PER_METRE;
const AIR_ACCELERATION: f32 = 12.0 * BLOCKS_PER_METRE;
const AIR_DECELERATION: f32 = 6.0 * BLOCKS_PER_METRE;
const WATER_ACCELERATION: f32 = 10.0 * BLOCKS_PER_METRE;
const WATER_DECELERATION: f32 = 6.0 * BLOCKS_PER_METRE;

/// Starting stamina capacity. Progression raises the resource maximum; this is
/// just the level-1 base used by the default resource.
const STAMINA_MAX: f32 = crate::progression::BASE_STAMINA;
const SPRINT_STAMINA_DRAIN: f32 = 8.0;
const STAMINA_REGEN: f32 = 14.0;
const STAMINA_REGEN_DELAY: f32 = 0.6;

const PITCH_LIMIT: f32 = 1.553; // ~89 degrees

const EPSILON: f32 = 1e-4;
/// Block interaction reach (6 m).
pub const REACH: f32 = 6.0 * BLOCKS_PER_METRE;

/// How far below the player's feet we look for solid ground.
const GROUND_PROBE: f32 = 0.08 * BLOCKS_PER_METRE;
/// Grace period after walking off a ledge during which a jump still works.
const COYOTE_TIME: f32 = 0.12;
/// How long a jump press is remembered while airborne.
const JUMP_BUFFER_TIME: f32 = 0.15;
/// The player automatically climbs obstacles up to this height. Anything
/// taller (two blocks or more) still requires a jump.
const STEP_HEIGHT: f32 = 1.0 + EPSILON;
/// How quickly the camera catches up after a step-up, in reciprocal seconds.
/// The body moves immediately; only the eye height is eased, which removes the
/// harsh vertical snap without affecting collision.
const STEP_SMOOTHING: f32 = 11.0;

/// Toggleable movement modes used by `/fly` and `/noclip`. Both are runtime
/// only: they never change the world or the save file.
#[derive(Resource, Debug, Default)]
pub struct MovementMode {
    pub flying: bool,
    pub noclip: bool,
}

/// Cached centre-screen voxel target for the current frame.
///
/// Updated once per frame so interaction, insight and UI systems share the
/// same crosshair raycast instead of recomputing it.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct CrosshairTarget {
    pub hit: Option<crate::world::RayHit>,
}

/// Raycast from the player camera along the crosshair using the standard reach.
pub fn crosshair_hit(
    camera: &GlobalTransform,
    manager: &ChunkManager,
) -> Option<crate::world::RayHit> {
    manager.raycast(camera.translation(), camera.rotation() * Vec3::NEG_Z, REACH)
}

#[derive(Component)]
pub struct Player {
    pub yaw: f32,
    pub pitch: f32,
    pub velocity: Vec3,
    pub grounded: bool,
    pub in_water: bool,
    pub crouching: bool,
    coyote_timer: f32,
    jump_buffer: f32,
}

impl Player {
    /// Clear all motion state that must not survive a teleport or fall reset.
    /// Timers are private, so callers cannot accidentally leave a queued jump
    /// or coyote window armed at the destination.
    pub fn reset_motion(&mut self) {
        self.velocity = Vec3::ZERO;
        self.grounded = false;
        self.in_water = false;
        self.crouching = false;
        self.coyote_timer = 0.0;
        self.jump_buffer = 0.0;
    }
}

/// Sprint/jump stamina. The HUD draws this as a thin bar below the crosshair
/// that fades after a delay when full; the maximum is kept in step with character
/// progression.
#[derive(Resource, Debug)]
pub struct Stamina {
    pub current: f32,
    pub max: f32,
    pub sprinting: bool,
    regen_delay: f32,
}

impl Default for Stamina {
    fn default() -> Self {
        Self {
            current: STAMINA_MAX,
            max: STAMINA_MAX,
            sprinting: false,
            regen_delay: 0.0,
        }
    }
}

impl Stamina {
    #[inline]
    pub fn fraction(&self) -> f32 {
        if self.max <= 0.0 {
            0.0
        } else {
            (self.current / self.max).clamp(0.0, 1.0)
        }
    }

    fn spend(&mut self, amount: f32) {
        self.current = (self.current - amount).max(0.0);
        self.regen_delay = STAMINA_REGEN_DELAY;
    }

    fn update(&mut self, dt: f32, wants_sprint: bool, sprinting: bool) {
        self.sprinting = sprinting;
        if sprinting {
            self.spend(SPRINT_STAMINA_DRAIN * dt);
            return;
        }

        self.regen_delay = (self.regen_delay - dt).max(0.0);
        if wants_sprint {
            // Sprint was requested but stamina is empty: do not regenerate while
            // Shift is still held, so the bar cannot flicker between empty and a
            // tiny amount. Releasing Shift lets recovery begin.
            self.regen_delay = self.regen_delay.max(STAMINA_REGEN_DELAY);
        }
        if self.regen_delay <= 0.0 {
            self.current = (self.current + STAMINA_REGEN * dt).min(self.max);
        }
    }
}

/// Whether a shallow-water jump should use the normal coyote/buffer path. In
/// one-block-deep water the head stays above the surface, so ordinary swimming
/// cannot lift the player out; the jump must come from the seabed instead. Deep
/// water keeps its separate submerged-head swim thrust.
#[inline]
fn wants_water_jump(
    jump_down: bool,
    head_submerged: bool,
    grounded: bool,
    jump_buffered: bool,
) -> bool {
    jump_down && !head_submerged && grounded && jump_buffered
}

#[derive(Component)]
pub struct PlayerCamera;

type PlayerMovementQuery<'w, 's> = Query<
    'w,
    's,
    (&'static mut Player, &'static mut Transform),
    (With<Player>, Without<PlayerCamera>),
>;

/// Camera-only vertical smoothing. `eye_height` handles crouch transitions and
/// `offset` absorbs the instant lift from a step-up so the view glides up.
#[derive(Resource)]
struct CameraSmoothing {
    eye_height: f32,
    offset: f32,
}

impl Default for CameraSmoothing {
    fn default() -> Self {
        Self {
            eye_height: EYE_HEIGHT,
            offset: 0.0,
        }
    }
}

#[derive(Resource, Default)]
pub struct CursorCapture {
    pub grabbed: bool,
    pub just_grabbed: bool,
    initialized: bool,
    /// Tracks the previous frame so closing any overlay re-grabs exactly once.
    overlays_open: bool,
}

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CursorCapture>()
            .init_resource::<Stamina>()
            .init_resource::<CameraSmoothing>()
            .init_resource::<MovementMode>()
            .init_resource::<CrosshairTarget>()
            .add_systems(Startup, spawn_player)
            .add_systems(PreUpdate, update_crosshair_target)
            .add_systems(
                Update,
                (
                    cursor_grab,
                    select_hotbar,
                    mouse_look,
                    movement,
                    update_camera_height,
                )
                    .chain(),
            );
    }
}

fn spawn_player(mut commands: Commands, spawn: Res<SpawnPoint>, mut images: ResMut<Assets<Image>>) {
    let player = commands
        .spawn((
            Player {
                yaw: 0.0,
                pitch: -0.12,
                velocity: Vec3::ZERO,
                grounded: false,
                in_water: false,
                crouching: false,
                coyote_timer: 0.0,
                jump_buffer: 0.0,
            },
            Transform::from_translation(spawn.0),
            Visibility::default(),
            Name::new("Player"),
        ))
        .id();

    let sky_image = images.add(crate::sky::create_sky_image());
    commands.spawn((
        PlayerCamera,
        Camera3d::default(),
        crate::sky::create_skybox(sky_image),
        Projection::Perspective(PerspectiveProjection {
            fov: 90f32.to_radians(),
            near: 0.05 * BLOCKS_PER_METRE,
            far: 600.0 * BLOCKS_PER_METRE,
            ..default()
        }),
        Transform::from_xyz(0.0, EYE_HEIGHT, 0.0),
        DistanceFog {
            color: Color::srgb(
                SURFACE_FOG_COLOR[0],
                SURFACE_FOG_COLOR[1],
                SURFACE_FOG_COLOR[2],
            ),
            directional_light_color: Color::srgba(0.0, 0.0, 0.0, 0.0),
            directional_light_exponent: 30.0,
            falloff: FogFalloff::Linear {
                start: 90.0 * BLOCKS_PER_METRE,
                end: 210.0 * BLOCKS_PER_METRE,
            },
        },
        ShadowFilteringMethod::Gaussian,
        Msaa::Sample2,
        ChildOf(player),
    ));
}

fn grab_cursor(cursor: &mut CursorOptions) {
    cursor.grab_mode = CursorGrabMode::Locked;
    cursor.visible = false;
}

fn release_cursor(cursor: &mut CursorOptions) {
    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
}

#[allow(clippy::too_many_arguments)]
fn cursor_grab(
    mut cursor_query: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut capture: ResMut<CursorCapture>,
    mut menu: ResMut<MenuState>,
    mut chat: ResMut<ChatState>,
    mut crafting: ResMut<CraftingUiState>,
    mut ui: ResMut<UiManager>,
    bindings: Res<InputBindings>,
    initial_load: Res<crate::world::InitialWorldLoad>,
) {
    capture.just_grabbed = false;

    let Ok(mut cursor) = cursor_query.single_mut() else {
        capture.grabbed = false;
        return;
    };

    // During initial world load, keep cursor visible and block gameplay input.
    if !initial_load.ready {
        if capture.grabbed || !capture.initialized {
            release_cursor(&mut cursor);
        }
        capture.initialized = true;
        capture.grabbed = false;
        capture.overlays_open = false;
        return;
    }

    if !capture.initialized {
        grab_cursor(&mut cursor);
        capture.initialized = true;
        capture.grabbed = true;
        capture.just_grabbed = true;
        capture.overlays_open = false;
        return;
    }

    // The menu's Resume button asks us to re-capture on the next frame.
    if menu.resume_requested {
        menu.resume_requested = false;
        ui.close(Overlay::Menu);
        ui.close(Overlay::Settings);
        grab_cursor(&mut cursor);
        capture.grabbed = true;
        capture.just_grabbed = true;
        capture.overlays_open = false;
        return;
    }

    // Escape closes the most recently opened overlay before it opens the pause
    // menu. Modal panels are dismissed here so their own input systems do not
    // duplicate cursor handling.
    if keys.just_pressed(bindings.pause) {
        match ui.active() {
            Some(Overlay::Menu) => {
                ui.close(Overlay::Menu);
            }
            Some(Overlay::Settings) => {
                ui.open(Overlay::Menu);
            }
            Some(Overlay::Chat) => {
                ui.close(Overlay::Chat);
                chat.input.clear();
            }
            Some(Overlay::Backpack) => {
                ui.close(Overlay::Backpack);
                crafting.station = None;
            }
            Some(Overlay::ShapeMenu) => {
                ui.close(Overlay::ShapeMenu);
            }
            Some(Overlay::GiveMenu) => {
                ui.close(Overlay::GiveMenu);
            }
            Some(Overlay::Map) => {
                ui.close(Overlay::Map);
            }
            None => {
                ui.open(Overlay::Menu);
            }
        }
    }

    if keys.just_pressed(bindings.backpack) {
        if ui.is_open(Overlay::Backpack) {
            ui.close(Overlay::Backpack);
            crafting.station = None;
        } else if capture.grabbed && !ui.any_open() {
            crafting.station = None;
            crafting.selected = 0;
            ui.open(Overlay::Backpack);
        }
    }

    if capture.grabbed && keys.just_pressed(bindings.map) && !ui.any_open() {
        ui.open(Overlay::Map);
    }

    if ui.any_open() {
        if capture.grabbed {
            release_cursor(&mut cursor);
        }
        capture.grabbed = false;
        capture.overlays_open = true;
        return;
    }

    // Any overlay just closed: re-grab and suppress the click that dismissed it.
    if capture.overlays_open {
        grab_cursor(&mut cursor);
        capture.grabbed = true;
        capture.just_grabbed = true;
        capture.overlays_open = false;
        return;
    }

    if !capture.grabbed && mouse.just_pressed(MouseButton::Left) {
        grab_cursor(&mut cursor);
        capture.grabbed = true;
        capture.just_grabbed = true;
    }
}

fn select_hotbar(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    capture: Res<CursorCapture>,
    mut pockets: ResMut<Pockets>,
) {
    if !capture.grabbed {
        return;
    }

    const DIGITS: [KeyCode; 8] = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
    ];

    for (index, key) in DIGITS.into_iter().enumerate() {
        if keys.just_pressed(key) {
            pockets.select(index);
        }
    }

    if let Some(scroll) = scroll
        && scroll.delta.y != 0.0
    {
        // Wheel up selects the previous pocket.
        pockets.cycle_selection(if scroll.delta.y > 0.0 { -1 } else { 1 });
    }
}

fn mouse_look(
    motion: Res<AccumulatedMouseMotion>,
    capture: Res<CursorCapture>,
    settings: Res<GameSettings>,
    mut player_query: PlayerMovementQuery,
    mut camera_query: Query<&mut Transform, (With<PlayerCamera>, Without<Player>)>,
) {
    if !capture.grabbed {
        return;
    }

    let Ok((mut player, mut transform)) = player_query.single_mut() else {
        return;
    };
    let Ok(mut camera) = camera_query.single_mut() else {
        return;
    };

    let sensitivity = settings.mouse_sensitivity;
    let delta = motion.delta;
    player.yaw -= delta.x * sensitivity;
    player.pitch = (player.pitch - delta.y * sensitivity).clamp(-PITCH_LIMIT, PITCH_LIMIT);

    transform.rotation = Quat::from_rotation_y(player.yaw);
    camera.rotation = Quat::from_rotation_x(player.pitch);
}

#[inline]
fn move_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    if (target - current).abs() <= max_delta {
        target
    } else {
        current + (target - current).signum() * max_delta
    }
}

#[inline]
fn input_axis(positive: bool, negative: bool) -> f32 {
    match (positive, negative) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

/// Horizontal camera basis. The player body carries yaw and the camera adds
/// pitch, so movement must use the same convention as the camera transform.
#[inline]
fn heading_basis(yaw: f32) -> (Vec3, Vec3) {
    (
        Vec3::new(-yaw.sin(), 0.0, -yaw.cos()),
        Vec3::new(yaw.cos(), 0.0, -yaw.sin()),
    )
}

/// Camera-relative flight direction. Forward follows the view pitch, strafing
/// stays level with the horizon and vertical thrust stays world-aligned.
#[inline]
fn flight_wish(
    forward: Vec3,
    right: Vec3,
    pitch: f32,
    forward_axis: f32,
    strafe_axis: f32,
    vertical_axis: f32,
) -> Vec3 {
    let pitch_cos = pitch.cos();
    let view_forward = Vec3::new(forward.x * pitch_cos, pitch.sin(), forward.z * pitch_cos);
    (view_forward * forward_axis + right * strafe_axis + Vec3::Y * vertical_axis)
        .normalize_or_zero()
}

#[allow(clippy::too_many_arguments)]
fn movement(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    bindings: Res<InputBindings>,
    capture: Res<CursorCapture>,
    manager: Res<ChunkManager>,
    structures: Res<Structures>,
    spawn: Res<SpawnPoint>,
    mut stamina: ResMut<Stamina>,
    mut smoothing: ResMut<CameraSmoothing>,
    movement_mode: Res<MovementMode>,
    mut player_query: PlayerMovementQuery,
) {
    let Ok((mut player, mut transform)) = player_query.single_mut() else {
        return;
    };

    let dt = time.delta_secs().min(0.05);
    if dt <= 0.0 {
        return;
    }

    let ctrl = keys.any_pressed([bindings.crouch, KeyCode::ControlRight]);
    let shift = keys.any_pressed([bindings.sprint, KeyCode::ShiftRight]);
    let jump_down = capture.grabbed && keys.pressed(bindings.jump);
    let move_forward = capture.grabbed && keys.pressed(bindings.move_forward);
    let move_back = capture.grabbed && keys.pressed(bindings.move_back);
    let move_right = capture.grabbed && keys.pressed(bindings.move_right);
    let move_left = capture.grabbed && keys.pressed(bindings.move_left);
    let forward_axis = input_axis(move_forward, move_back);
    let strafe_axis = input_axis(move_right, move_left);
    let vertical_axis = input_axis(jump_down, capture.grabbed && ctrl);

    // Holding jump keeps the buffer topped up so the player keeps hopping as
    // soon as they touch the ground, like Minecraft/Hytale.
    if jump_down {
        player.jump_buffer = JUMP_BUFFER_TIME;
    } else {
        player.jump_buffer = (player.jump_buffer - dt).max(0.0);
    }

    if movement_mode.flying {
        player.crouching = false;
        player.in_water = false;
        player.grounded = false;
        player.jump_buffer = 0.0;
        player.coyote_timer = 0.0;
    }

    let feet = transform.translation.floor().as_ivec3();
    let current_height = if player.crouching {
        CROUCH_HEIGHT
    } else {
        PLAYER_HEIGHT
    };
    let head = (transform.translation + Vec3::Y * (current_height - 0.2 * BLOCKS_PER_METRE))
        .floor()
        .as_ivec3();
    // Upward swimming is only allowed while the head is actually underwater.
    // Otherwise the player can keep thrusting out of the surface and hover
    // there, effectively walking on the water.
    let head_submerged = manager.get_block(head) == Block::Water;
    player.in_water = manager.get_block(feet) == Block::Water || head_submerged;

    // Ctrl crouches on land and dives in water. Stay crouched if there is not
    // enough headroom to stand back up. While flying, Space and Ctrl are
    // vertical thrust instead.
    if movement_mode.flying || player.in_water {
        player.crouching = false;
    } else if ctrl {
        player.crouching = true;
    } else if player.crouching && has_headroom(&manager, &structures, transform.translation) {
        player.crouching = false;
    }
    let player_height = if player.crouching {
        CROUCH_HEIGHT
    } else {
        PLAYER_HEIGHT
    };

    // Probe the voxels just below the AABB. The collision pass only reports a
    // landing on the frame vertical motion stops, so this keeps `grounded`
    // stable while standing still.
    let on_ground = is_on_ground(&manager, &structures, transform.translation);
    if on_ground {
        player.coyote_timer = COYOTE_TIME;
        player.grounded = true;
    } else {
        player.coyote_timer = (player.coyote_timer - dt).max(0.0);
    }

    let (forward, right) = heading_basis(player.yaw);
    let wish = if movement_mode.flying {
        // Use the raw input axes here. Reusing the ground-space `wish` would
        // feed a world-space component into the camera-space basis, making W
        // and D drift toward fixed world directions.
        flight_wish(
            forward,
            right,
            player.pitch,
            forward_axis,
            strafe_axis,
            vertical_axis,
        )
    } else {
        (forward * forward_axis + right * strafe_axis).normalize_or_zero()
    };

    let wants_sprint =
        capture.grabbed && shift && !ctrl && !player.in_water && wish.length_squared() > 0.0;
    let sprinting =
        wants_sprint && (movement_mode.flying || movement_mode.noclip || stamina.current > 0.0);

    let speed = if movement_mode.flying {
        if sprinting {
            FLY_SPRINT_SPEED
        } else {
            FLY_SPEED
        }
    } else if player.in_water {
        SWIM_SPEED
    } else if player.crouching {
        CROUCH_SPEED
    } else if sprinting {
        SPRINT_SPEED
    } else {
        WALK_SPEED
    };

    let target = wish * speed;
    let (acceleration, deceleration) = if movement_mode.flying {
        (FLY_ACCELERATION, FLY_DECELERATION)
    } else if player.in_water {
        (WATER_ACCELERATION, WATER_DECELERATION)
    } else if player.grounded {
        (GROUND_ACCELERATION, GROUND_DECELERATION)
    } else {
        (AIR_ACCELERATION, AIR_DECELERATION)
    };
    let rate = if wish.length_squared() > 0.0 {
        acceleration
    } else {
        deceleration
    };
    player.velocity.x = move_towards(player.velocity.x, target.x, rate * dt);
    player.velocity.z = move_towards(player.velocity.z, target.z, rate * dt);

    if movement_mode.flying {
        player.velocity.y = move_towards(player.velocity.y, target.y, rate * dt);
    } else if player.in_water {
        if jump_down && head_submerged {
            // Deep water still uses the swimming thrust rather than a full jump.
            player.velocity.y = 4.2 * BLOCKS_PER_METRE;
        } else if wants_water_jump(
            jump_down,
            head_submerged,
            player.coyote_timer > 0.0,
            player.jump_buffer > 0.0,
        ) {
            player.velocity.y = JUMP_SPEED;
            // Shallow-water jumps consume the buffered ground jump so the
            // player cannot keep thrusting upward from the surface.
            player.grounded = false;
            player.coyote_timer = 0.0;
            player.jump_buffer = 0.0;
        } else if capture.grabbed && ctrl {
            // Ctrl dives while swimming.
            player.velocity.y =
                (player.velocity.y - 12.0 * BLOCKS_PER_METRE * dt).max(-6.0 * BLOCKS_PER_METRE);
        } else {
            player.velocity.y =
                (player.velocity.y - 6.0 * BLOCKS_PER_METRE * dt).max(-6.0 * BLOCKS_PER_METRE);
        }
        // Gentle drag makes swimming feel less like ice.
        let drag = (1.0 - 2.5 * dt).clamp(0.0, 1.0);
        player.velocity.x *= drag;
        player.velocity.z *= drag;
    } else {
        // Coyote time + jump buffering make jumping reliable even if the press
        // lands a frame or two off the ground.
        if player.coyote_timer > 0.0 && player.jump_buffer > 0.0 {
            player.velocity.y = JUMP_SPEED;
            player.grounded = false;
            player.coyote_timer = 0.0;
            player.jump_buffer = 0.0;
        }
        player.velocity.y = (player.velocity.y - GRAVITY * dt).max(-TERMINAL_VELOCITY);
    }

    let stamina_draining = sprinting && !movement_mode.flying && !movement_mode.noclip;
    stamina.update(dt, wants_sprint, stamina_draining);

    if movement_mode.noclip {
        transform.translation += player.velocity * dt;
        player.grounded = false;
        return;
    }

    // Substep so fast falls cannot tunnel through blocks.
    let distance = player.velocity.length() * dt;
    let steps = ((distance / (0.35 * BLOCKS_PER_METRE)).ceil() as usize).clamp(1, 8);
    let sub_dt = dt / steps as f32;
    let mut grounded = false;
    let mut step_lift = 0.0;
    let can_step = is_on_ground(&manager, &structures, transform.translation);

    for _ in 0..steps {
        let before_y = transform.translation.y;
        let landed = move_and_collide(
            &manager,
            &structures,
            &mut transform.translation,
            &mut player.velocity,
            sub_dt,
            player_height,
            can_step,
        );
        grounded |= landed;

        // A step-up raises the body while horizontal velocity is non-positive in
        // Y. Landing from a fall never raises the body, so this only catches the
        // automatic one-block climb.
        if landed && player.velocity.y <= 0.0 {
            let lift = transform.translation.y - before_y;
            if lift > 0.05 {
                step_lift += lift;
            }
        }
    }

    if step_lift > 0.0 {
        // Push the camera down by the amount the body just climbed, then let it
        // ease back up in `update_camera_height`. Clamp so fast stair climbs do
        // not leave the camera below the player's feet.
        smoothing.offset = (smoothing.offset - step_lift).max(-STEP_HEIGHT);
    }

    let landed_this_frame = grounded || is_on_ground(&manager, &structures, transform.translation);
    player.grounded = landed_this_frame;

    // Safety net in case something goes wrong near unloaded terrain. The old
    // -20 m floor is now ordinary world space; only falling out of the generated
    // detail grid should reset the player.
    if transform.translation.y < DETAIL_Y_MIN as f32 {
        transform.translation = spawn.0;
        player.reset_motion();
    }
}

/// Cache the voxel currently under the crosshair so gameplay and UI systems can
/// share one reach-limited raycast per frame.
///
/// Runs in `PreUpdate` so the target is ready for all `Update` consumers with a
/// consistent, start-of-frame view (after teleports/world setup in `First` and
/// using the previous frame's propagated camera transform, matching direct
/// `GlobalTransform` reads in `Update`).
fn update_crosshair_target(
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    manager: Res<ChunkManager>,
    mut target: ResMut<CrosshairTarget>,
) {
    target.hit = camera
        .single()
        .ok()
        .and_then(|camera| crosshair_hit(camera, &manager));
}

/// Smoothly move the camera between standing and crouching eye heights. The
/// camera is a child of the player, so this only changes its local offset.
fn update_camera_height(
    time: Res<Time>,
    player: Query<&Player>,
    mut smoothing: ResMut<CameraSmoothing>,
    mut camera: Query<&mut Transform, With<PlayerCamera>>,
) {
    let Ok(player) = player.single() else {
        return;
    };
    let Ok(mut camera) = camera.single_mut() else {
        return;
    };

    let dt = time.delta_secs().min(0.05);

    // Ease the step-up offset back to zero. This is applied directly to the
    // camera rather than through `move_towards`, so the instant lift is cancelled
    // on the same frame and only the recovery is visible.
    smoothing.offset *= (-STEP_SMOOTHING * dt).exp();
    if smoothing.offset.abs() < 0.001 {
        smoothing.offset = 0.0;
    }

    let target = if player.crouching {
        CROUCH_EYE_HEIGHT
    } else {
        EYE_HEIGHT
    };
    let max_delta = 8.0 * BLOCKS_PER_METRE * dt;
    smoothing.eye_height = move_towards(smoothing.eye_height, target, max_delta);
    camera.translation.y = smoothing.eye_height + smoothing.offset;
}

/// True when a solid voxel sits directly beneath any part of the player's
/// footprint. This is independent of vertical velocity, so it stays stable
/// while standing still and lets jump buffering work.
fn is_on_ground(manager: &ChunkManager, structures: &Structures, position: Vec3) -> bool {
    let footprint_min_x = position.x - PLAYER_HALF_WIDTH;
    let footprint_max_x = position.x + PLAYER_HALF_WIDTH;
    let footprint_min_z = position.z - PLAYER_HALF_WIDTH;
    let footprint_max_z = position.z + PLAYER_HALF_WIDTH;
    let probe_min_y = position.y - GROUND_PROBE;
    let probe_max_y = position.y + EPSILON;

    let min = Vec3::new(footprint_min_x, probe_min_y, footprint_min_z);
    let max = Vec3::new(footprint_max_x, probe_max_y, footprint_max_z);

    let min_cell = min.floor().as_ivec3();
    let max_cell = (max - Vec3::splat(EPSILON)).floor().as_ivec3();

    for y in min_cell.y..=max_cell.y {
        for z in min_cell.z..=max_cell.z {
            for x in min_cell.x..=max_cell.x {
                let Some(boxes) = manager.collision_boxes(IVec3::new(x, y, z)) else {
                    continue;
                };
                let cell = Vec3::new(x as f32, y as f32, z as f32);
                for (local_min, local_max) in boxes.iter() {
                    let box_min = cell + local_min;
                    let box_max = cell + local_max;
                    if box_min.x < footprint_max_x - EPSILON
                        && box_max.x > footprint_min_x + EPSILON
                        && box_min.z < footprint_max_z - EPSILON
                        && box_max.z > footprint_min_z + EPSILON
                        && box_min.y < probe_max_y
                        && box_max.y > probe_min_y
                    {
                        return true;
                    }
                }
            }
        }
    }

    for placed in structures.iter() {
        for (box_min, box_max) in placed.collision_boxes() {
            if box_min.x < footprint_max_x - EPSILON
                && box_max.x > footprint_min_x + EPSILON
                && box_min.z < footprint_max_z - EPSILON
                && box_max.z > footprint_min_z + EPSILON
                && box_min.y < probe_max_y
                && box_max.y > probe_min_y
            {
                return true;
            }
        }
    }

    false
}

/// Move the player's AABB one axis at a time, resolving penetration against the
/// voxel grid. Returns true if the player landed on something during the Y pass.
fn move_and_collide(
    manager: &ChunkManager,
    structures: &Structures,
    position: &mut Vec3,
    velocity: &mut Vec3,
    dt: f32,
    height: f32,
    can_step: bool,
) -> bool {
    let mut grounded = false;

    if velocity.y != 0.0 {
        let moving_up = velocity.y > 0.0;
        position.y += velocity.y * dt;
        if let Some(limit) = resolve_axis(manager, structures, *position, 1, moving_up, height) {
            position.y = limit;
            velocity.y = 0.0;
            grounded = !moving_up;
        }
    }

    let start = *position;
    let horizontal_velocity = Vec3::new(velocity.x, 0.0, velocity.z);
    if horizontal_velocity != Vec3::ZERO {
        let blocked = move_horizontal(manager, structures, position, velocity, dt, height);
        if blocked
            && can_step
            && velocity.y <= 0.0
            && try_step_up(
                manager,
                structures,
                &start,
                &horizontal_velocity,
                dt,
                height,
                position,
                velocity,
            )
        {
            grounded = true;
        }
    }

    grounded
}

/// Move along X and Z, resolving collisions one axis at a time. Returns true if
/// either axis was blocked.
fn move_horizontal(
    manager: &ChunkManager,
    structures: &Structures,
    position: &mut Vec3,
    velocity: &mut Vec3,
    dt: f32,
    height: f32,
) -> bool {
    let mut blocked = false;

    if velocity.x != 0.0 {
        let moving_positive = velocity.x > 0.0;
        position.x += velocity.x * dt;
        if let Some(limit) =
            resolve_axis(manager, structures, *position, 0, moving_positive, height)
        {
            position.x = limit;
            velocity.x = 0.0;
            blocked = true;
        }
    }

    if velocity.z != 0.0 {
        let moving_positive = velocity.z > 0.0;
        position.z += velocity.z * dt;
        if let Some(limit) =
            resolve_axis(manager, structures, *position, 2, moving_positive, height)
        {
            position.z = limit;
            velocity.z = 0.0;
            blocked = true;
        }
    }

    blocked
}

/// Try to climb a one-block obstacle. The horizontal move is repeated from a
/// raised position; if that clears the obstacle the player is lowered onto it.
/// Two-block obstacles still overlap the raised AABB and require a jump.
#[allow(clippy::too_many_arguments)]
fn try_step_up(
    manager: &ChunkManager,
    structures: &Structures,
    start: &Vec3,
    horizontal_velocity: &Vec3,
    dt: f32,
    height: f32,
    position: &mut Vec3,
    velocity: &mut Vec3,
) -> bool {
    let mut raised = *start;
    raised.y += STEP_HEIGHT;
    if aabb_overlaps_solid(manager, structures, raised, height) {
        return false;
    }

    let mut stepped = raised;
    let mut stepped_velocity = *horizontal_velocity;
    move_horizontal(
        manager,
        structures,
        &mut stepped,
        &mut stepped_velocity,
        dt,
        height,
    );

    // No horizontal progress means the raised position was blocked as well.
    if (stepped.x - raised.x).abs() < EPSILON && (stepped.z - raised.z).abs() < EPSILON {
        return false;
    }

    // Lower onto the first solid surface below the raised position.
    let mut lowered = stepped;
    lowered.y = start.y;
    if let Some(limit) = resolve_axis(manager, structures, lowered, 1, false, height) {
        lowered.y = limit;
    }
    if lowered.y > start.y + STEP_HEIGHT + EPSILON {
        return false;
    }
    if aabb_overlaps_solid(manager, structures, lowered, height) {
        return false;
    }

    *position = lowered;
    velocity.x = stepped_velocity.x;
    velocity.z = stepped_velocity.z;
    true
}

/// True when the player's AABB overlaps any solid voxel.
fn aabb_overlaps_solid(
    manager: &ChunkManager,
    structures: &Structures,
    position: Vec3,
    height: f32,
) -> bool {
    let min = Vec3::new(
        position.x - PLAYER_HALF_WIDTH,
        position.y,
        position.z - PLAYER_HALF_WIDTH,
    );
    let max = Vec3::new(
        position.x + PLAYER_HALF_WIDTH,
        position.y + height,
        position.z + PLAYER_HALF_WIDTH,
    );

    let min_cell = min.floor().as_ivec3();
    let max_cell = (max - Vec3::splat(EPSILON)).floor().as_ivec3();

    for y in min_cell.y..=max_cell.y {
        for z in min_cell.z..=max_cell.z {
            for x in min_cell.x..=max_cell.x {
                let Some(boxes) = manager.collision_boxes(IVec3::new(x, y, z)) else {
                    continue;
                };
                let cell = Vec3::new(x as f32, y as f32, z as f32);
                for (local_min, local_max) in boxes.iter() {
                    let box_min = cell + local_min;
                    let box_max = cell + local_max;
                    if box_min.x < max.x - EPSILON
                        && box_max.x > min.x + EPSILON
                        && box_min.y < max.y - EPSILON
                        && box_max.y > min.y + EPSILON
                        && box_min.z < max.z - EPSILON
                        && box_max.z > min.z + EPSILON
                    {
                        return true;
                    }
                }
            }
        }
    }

    for placed in structures.iter() {
        for (box_min, box_max) in placed.collision_boxes() {
            if box_min.x < max.x - EPSILON
                && box_max.x > min.x + EPSILON
                && box_min.y < max.y - EPSILON
                && box_max.y > min.y + EPSILON
                && box_min.z < max.z - EPSILON
                && box_max.z > min.z + EPSILON
            {
                return true;
            }
        }
    }
    false
}

/// Scan every voxel overlapping the player's AABB and return the closest legal
/// coordinate along one axis.
fn resolve_axis(
    manager: &ChunkManager,
    structures: &Structures,
    position: Vec3,
    axis: usize,
    moving_positive: bool,
    height: f32,
) -> Option<f32> {
    let min = Vec3::new(
        position.x - PLAYER_HALF_WIDTH,
        position.y,
        position.z - PLAYER_HALF_WIDTH,
    );
    let max = Vec3::new(
        position.x + PLAYER_HALF_WIDTH,
        position.y + height,
        position.z + PLAYER_HALF_WIDTH,
    );

    let min_cell = min.floor().as_ivec3();
    let max_cell = (max - Vec3::splat(EPSILON)).floor().as_ivec3();

    let mut result: Option<f32> = None;

    for y in min_cell.y..=max_cell.y {
        for z in min_cell.z..=max_cell.z {
            for x in min_cell.x..=max_cell.x {
                let Some(boxes) = manager.collision_boxes(IVec3::new(x, y, z)) else {
                    continue;
                };
                let cell = Vec3::new(x as f32, y as f32, z as f32);

                for (local_min, local_max) in boxes.iter() {
                    let box_min = cell + local_min;
                    let box_max = cell + local_max;

                    let overlaps = match axis {
                        0 => {
                            box_max.y > min.y + EPSILON
                                && box_min.y < max.y - EPSILON
                                && box_max.z > min.z + EPSILON
                                && box_min.z < max.z - EPSILON
                                && box_max.x > min.x + EPSILON
                                && box_min.x < max.x - EPSILON
                        }
                        1 => {
                            // Require vertical overlap too. Without it, a thin
                            // block under the player's feet (a panel) would
                            // count as an overhead obstacle while moving up,
                            // producing a downward limit and teleporting the
                            // player through the floor. When moving up, also
                            // ignore boxes whose bottom is below the feet: they
                            // are ground the player is leaving, not a ceiling.
                            box_max.x > min.x + EPSILON
                                && box_min.x < max.x - EPSILON
                                && box_max.z > min.z + EPSILON
                                && box_min.z < max.z - EPSILON
                                && box_max.y > min.y + EPSILON
                                && box_min.y < max.y - EPSILON
                                && (!moving_positive || box_min.y >= min.y - EPSILON)
                        }
                        _ => {
                            box_max.x > min.x + EPSILON
                                && box_min.x < max.x - EPSILON
                                && box_max.y > min.y + EPSILON
                                && box_min.y < max.y - EPSILON
                                && box_max.z > min.z + EPSILON
                                && box_min.z < max.z - EPSILON
                        }
                    };
                    if !overlaps {
                        continue;
                    }

                    // Resolve to the nearest face along this axis. This keeps normal
                    // movement contacts stable while also depenetrating correctly if
                    // the player is already inside a collider (directional resolution
                    // would otherwise eject to the opposite side).
                    let (neg_limit, pos_limit) = match axis {
                        0 => (
                            box_min.x - PLAYER_HALF_WIDTH - EPSILON,
                            box_max.x + PLAYER_HALF_WIDTH + EPSILON,
                        ),
                        1 => (box_min.y - height - EPSILON, box_max.y + EPSILON),
                        _ => (
                            box_min.z - PLAYER_HALF_WIDTH - EPSILON,
                            box_max.z + PLAYER_HALF_WIDTH + EPSILON,
                        ),
                    };
                    let coord = position[axis];
                    let limit = if (coord - neg_limit).abs() <= (pos_limit - coord).abs() {
                        neg_limit
                    } else {
                        pos_limit
                    };

                    result = Some(match result {
                        Some(current) => {
                            if moving_positive {
                                current.min(limit)
                            } else {
                                current.max(limit)
                            }
                        }
                        None => limit,
                    });
                }
            }
        }
    }

    for placed in structures.iter() {
        for (box_min, box_max) in placed.collision_boxes() {
            let overlaps = match axis {
                0 => {
                    box_max.y > min.y + EPSILON
                        && box_min.y < max.y - EPSILON
                        && box_max.z > min.z + EPSILON
                        && box_min.z < max.z - EPSILON
                        && box_max.x > min.x + EPSILON
                        && box_min.x < max.x - EPSILON
                }
                1 => {
                    box_max.x > min.x + EPSILON
                        && box_min.x < max.x - EPSILON
                        && box_max.z > min.z + EPSILON
                        && box_min.z < max.z - EPSILON
                        && box_max.y > min.y + EPSILON
                        && box_min.y < max.y - EPSILON
                        && (!moving_positive || box_min.y >= min.y - EPSILON)
                }
                _ => {
                    box_max.x > min.x + EPSILON
                        && box_min.x < max.x - EPSILON
                        && box_max.y > min.y + EPSILON
                        && box_min.y < max.y - EPSILON
                        && box_max.z > min.z + EPSILON
                        && box_min.z < max.z - EPSILON
                }
            };
            if !overlaps {
                continue;
            }
            let (neg_limit, pos_limit) = match axis {
                0 => (
                    box_min.x - PLAYER_HALF_WIDTH - EPSILON,
                    box_max.x + PLAYER_HALF_WIDTH + EPSILON,
                ),
                1 => (box_min.y - height - EPSILON, box_max.y + EPSILON),
                _ => (
                    box_min.z - PLAYER_HALF_WIDTH - EPSILON,
                    box_max.z + PLAYER_HALF_WIDTH + EPSILON,
                ),
            };
            let coord = position[axis];
            let limit = if (coord - neg_limit).abs() <= (pos_limit - coord).abs() {
                neg_limit
            } else {
                pos_limit
            };
            result = Some(match result {
                Some(current) => {
                    if moving_positive {
                        current.min(limit)
                    } else {
                        current.max(limit)
                    }
                }
                None => limit,
            });
        }
    }

    result
}

/// True when the player could stand up here without their head hitting a block.
/// The space below the crouched height is already occupied by the player, so
/// only the extra height needed to stand is tested.
fn has_headroom(manager: &ChunkManager, structures: &Structures, position: Vec3) -> bool {
    let min = Vec3::new(
        position.x - PLAYER_HALF_WIDTH,
        position.y + CROUCH_HEIGHT,
        position.z - PLAYER_HALF_WIDTH,
    );
    let max = Vec3::new(
        position.x + PLAYER_HALF_WIDTH,
        position.y + PLAYER_HEIGHT,
        position.z + PLAYER_HALF_WIDTH,
    );

    let min_cell = min.floor().as_ivec3();
    let max_cell = (max - Vec3::splat(EPSILON)).floor().as_ivec3();

    for y in min_cell.y..=max_cell.y {
        for z in min_cell.z..=max_cell.z {
            for x in min_cell.x..=max_cell.x {
                let Some(boxes) = manager.collision_boxes(IVec3::new(x, y, z)) else {
                    continue;
                };
                let cell = Vec3::new(x as f32, y as f32, z as f32);
                for (local_min, local_max) in boxes.iter() {
                    let box_min = cell + local_min;
                    let box_max = cell + local_max;
                    if box_min.x < max.x
                        && box_max.x > min.x
                        && box_min.y < max.y
                        && box_max.y > min.y
                        && box_min.z < max.z
                        && box_max.z > min.z
                    {
                        return false;
                    }
                }
            }
        }
    }

    for placed in structures.iter() {
        for (box_min, box_max) in placed.collision_boxes() {
            if box_min.x < max.x
                && box_max.x > min.x
                && box_min.y < max.y
                && box_max.y > min.y
                && box_min.z < max.z
                && box_max.z > min.z
            {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
pub fn block_intersects_player(player_position: Vec3, height: f32, block: IVec3) -> bool {
    voxel_intersects_player(
        player_position,
        height,
        block,
        Voxel::new(crate::world::block::Block::Granite),
    )
}

/// True when a shaped voxel overlaps the player's AABB. Construction uses this
/// so a half block or slope can sit in space the full cell would intersect.
pub fn voxel_intersects_player(
    player_position: Vec3,
    height: f32,
    block: IVec3,
    voxel: Voxel,
) -> bool {
    let min = Vec3::new(
        player_position.x - PLAYER_HALF_WIDTH,
        player_position.y,
        player_position.z - PLAYER_HALF_WIDTH,
    );
    let max = Vec3::new(
        player_position.x + PLAYER_HALF_WIDTH,
        player_position.y + height,
        player_position.z + PLAYER_HALF_WIDTH,
    );
    let cell = block.as_vec3();

    for (local_min, local_max) in voxel.shape().collision_boxes(voxel.orientation()).iter() {
        let block_min = cell + local_min;
        let block_max = cell + local_max;
        if min.x < block_max.x
            && max.x > block_min.x
            && min.y < block_max.y
            && max.y > block_min.y
            && min.z < block_max.z
            && max.z > block_min.z
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::ChunkManager;
    use crate::world::block::{PANEL_HEIGHT, Shape};
    use crate::world::chunk::Chunk;
    use crate::world::chunk::ChunkData;

    fn empty_chunk(coord: IVec3) -> Chunk {
        Chunk {
            data: ChunkData::new(coord),
            entity: Entity::PLACEHOLDER,
            water_entity: None,
            foliage_entity: None,
            triangles: 0,
            water_triangles: 0,
            foliage_triangles: 0,
            fade_remaining: 0.0,
        }
    }

    fn manager_with_voxel(cell: IVec3, voxel: Voxel) -> ChunkManager {
        use crate::world::chunk::{detail_to_chunk, detail_to_local};

        let mut manager = ChunkManager::default();
        let coord = detail_to_chunk(cell);
        let (lx, ly, lz) = detail_to_local(cell);
        let mut chunk = empty_chunk(coord);
        chunk.data.set_local(lx, ly, lz, voxel);
        manager.chunks.insert(coord, chunk);
        manager
    }

    fn manager_with_block(block_pos: IVec3) -> ChunkManager {
        manager_with_voxel(block_pos, Voxel::new(Block::Granite))
    }

    fn manager_with_turf(world: IVec3) -> ChunkManager {
        use crate::world::chunk::{
            detail_to_chunk, detail_to_local, world_to_chunk, world_to_local, world_voxel_cells,
        };

        let mut manager = ChunkManager::default();
        let coord = world_to_chunk(world);
        let (lx, ly, lz) = world_to_local(world);
        let mut chunk = empty_chunk(coord);
        chunk.data.set_world_local(lx, ly, lz, Block::Topsoil);
        chunk.data.set_world_turf_local(lx, ly, lz, true);
        manager.chunks.insert(coord, chunk);

        for cell in world_voxel_cells(world) {
            let cell_coord = detail_to_chunk(cell);
            let (cx, cy, cz) = detail_to_local(cell);
            manager
                .chunks
                .entry(cell_coord)
                .or_insert_with(|| empty_chunk(cell_coord))
                .data
                .set_local(cx, cy, cz, Block::Topsoil);
        }
        manager
    }

    #[test]
    fn flight_follows_the_camera_basis() {
        let (forward, right) = heading_basis(0.0);
        let level = flight_wish(forward, right, 0.0, 1.0, 0.0, 0.0);
        assert!((level - Vec3::NEG_Z).length() < 0.0001);

        let (forward, right) = heading_basis(std::f32::consts::FRAC_PI_2);
        let turned = flight_wish(forward, right, 0.0, 1.0, 0.0, 0.0);
        assert!((turned - Vec3::NEG_X).length() < 0.0001);

        let strafed = flight_wish(forward, right, 0.0, 0.0, 1.0, 0.0);
        assert!((strafed - Vec3::NEG_Z).length() < 0.0001);

        let pitched = flight_wish(forward, right, 0.5, 1.0, 0.0, 0.0);
        assert!(pitched.x < 0.0);
        assert!(pitched.y > 0.0);
        assert!((pitched.length() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn flight_vertical_thrust_stays_world_aligned() {
        let (forward, right) = heading_basis(1.1);
        let up = flight_wish(forward, right, -0.4, 0.0, 0.0, 1.0);
        assert!((up - Vec3::Y).length() < 0.0001);

        let down = flight_wish(forward, right, 0.7, 0.0, 0.0, -1.0);
        assert!((down - Vec3::NEG_Y).length() < 0.0001);
    }

    #[test]
    fn opposing_input_axes_cancel() {
        assert_eq!(input_axis(true, false), 1.0);
        assert_eq!(input_axis(false, true), -1.0);
        assert_eq!(input_axis(true, true), 0.0);
        assert_eq!(input_axis(false, false), 0.0);
    }

    #[test]
    fn player_lands_on_the_solid_block_below_the_turf_panel() {
        let world = IVec3::new(4, 5, 4);
        let manager = manager_with_turf(world);
        let surface = (world.y + 1) as f32 * 2.0;
        let mut position = Vec3::new(8.5, surface + 0.5, 8.5);
        let mut velocity = Vec3::new(0.0, -10.0, 0.0);
        let grounded = move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );
        assert!(grounded);
        assert!((position.y - surface).abs() < 1e-3);
        assert_eq!(velocity.y, 0.0);
    }

    #[test]
    fn player_lands_on_a_block() {
        // Keep the whole footprint inside one loaded chunk: unloaded chunks are
        // treated as solid, which would otherwise catch the player early.
        let manager = manager_with_block(IVec3::new(8, 10, 8));
        let mut position = Vec3::new(8.5, 11.5, 8.5);
        let mut velocity = Vec3::new(0.0, -10.0, 0.0);
        // The real controller substeps large moves; keep this step small enough
        // that the player's AABB actually overlaps the block.
        let grounded = move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );
        assert!(grounded);
        assert!((position.y - 11.0).abs() < 1e-3);
        assert_eq!(velocity.y, 0.0);
    }

    #[test]
    fn player_cannot_walk_through_a_wall() {
        let manager = manager_with_block(IVec3::new(10, 10, 8));
        let mut position = Vec3::new(8.5, 10.0, 8.5);
        let mut velocity = Vec3::new(10.0, 0.0, 0.0);
        move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );
        assert!(position.x < 10.0);
        assert_eq!(velocity.x, 0.0);
    }

    #[test]
    fn player_steps_up_one_block_but_not_two() {
        // One-block step: the player is lifted onto its top surface.
        let manager = manager_with_block(IVec3::new(9, 10, 8));
        let mut position = Vec3::new(8.5, 10.0, 8.5);
        let mut velocity = Vec3::new(6.0, 0.0, 0.0);
        move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            true,
        );
        assert!(position.x > 9.0);
        assert!((position.y - 11.0).abs() < 1e-3);

        // Two-block step: the raised AABB still overlaps the upper block, so the
        // player stays put and has to jump.
        let mut manager = manager_with_block(IVec3::new(9, 10, 8));
        let upper = IVec3::new(9, 11, 8);
        let coord = crate::world::chunk::detail_to_chunk(upper);
        let (lx, ly, lz) = crate::world::chunk::detail_to_local(upper);
        manager
            .chunks
            .get_mut(&coord)
            .unwrap()
            .data
            .set_local(lx, ly, lz, Block::Granite);

        let mut position = Vec3::new(8.5, 10.0, 8.5);
        let mut velocity = Vec3::new(6.0, 0.0, 0.0);
        move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            true,
        );
        assert!(position.x < 9.0);
        assert!((position.y - 10.0).abs() < 1e-3);
    }

    #[test]
    fn ground_probe_is_stable_while_standing_still() {
        let manager = manager_with_block(IVec3::new(8, 10, 8));
        let mut position = Vec3::new(8.5, 11.0 + EPSILON, 8.5);
        let mut velocity = Vec3::ZERO;

        // With no vertical velocity the collision pass reports no landing...
        assert!(!move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false
        ));
        // ...but the probe still sees the block underneath.
        assert!(is_on_ground(&manager, &Structures::default(), position));
    }

    #[test]
    fn player_lands_on_a_panel_at_its_thin_height() {
        let manager = manager_with_voxel(
            IVec3::new(8, 10, 8),
            Voxel::new(Block::Turf).with_shape(Shape::Panel),
        );
        let mut position = Vec3::new(8.5, 11.0, 8.5);
        let mut velocity = Vec3::new(0.0, -10.0, 0.0);
        let grounded = move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );

        assert!(grounded);
        let expected = 10.0 + PANEL_HEIGHT;
        assert!((position.y - expected).abs() < 1e-3);
    }

    #[test]
    fn standing_on_a_panel_does_not_block_horizontal_movement() {
        let manager = manager_with_voxel(
            IVec3::new(8, 10, 8),
            Voxel::new(Block::Turf).with_shape(Shape::Panel),
        );
        // The player's feet are just above the mat, so its cell is inside the
        // vertical AABB range even though the mat top is below the feet.
        let mut position = Vec3::new(8.5, 10.0 + PANEL_HEIGHT + EPSILON, 8.5);
        let mut velocity = Vec3::new(6.0, 0.0, 0.0);
        move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );

        assert!(position.x > 8.5);
        assert_eq!(velocity.x, 6.0);
    }

    #[test]
    fn jumping_from_a_panel_does_not_snap_to_the_ground() {
        let manager = manager_with_voxel(
            IVec3::new(8, 10, 8),
            Voxel::new(Block::Turf).with_shape(Shape::Panel),
        );
        // Start a jump while very slightly embedded in the panel. The panel
        // cell overlaps the player's horizontal footprint and part of its
        // vertical extent, but it is ground being left, not a ceiling, so it
        // must not produce a downward vertical limit.
        let start = 10.0 + PANEL_HEIGHT - 0.01;
        let mut position = Vec3::new(8.5, start, 8.5);
        let mut velocity = Vec3::new(0.0, 10.0, 0.0);
        let grounded = move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );

        assert!(!grounded);
        assert!((position.y - (start + 1.0)).abs() < 1e-3);
        assert_eq!(velocity.y, 10.0);
    }

    #[test]
    fn placement_inside_the_player_is_rejected() {
        assert!(block_intersects_player(
            Vec3::new(0.5, 10.0, 0.5),
            PLAYER_HEIGHT,
            IVec3::new(0, 10, 0)
        ));
        assert!(!block_intersects_player(
            Vec3::new(0.5, 10.0, 0.5),
            PLAYER_HEIGHT,
            IVec3::new(3, 10, 0)
        ));
    }

    #[test]
    fn crouching_needs_headroom_to_stand() {
        let position = Vec3::new(8.5, 11.0, 8.5);

        // The block at y=14 is above the crouched height (11.0 + 2.4 = 13.4)
        // but overlaps the standing height (11.0 + 3.6 = 14.6), so the player
        // cannot stand.
        let manager = manager_with_block(IVec3::new(8, 14, 8));
        assert!(!has_headroom(&manager, &Structures::default(), position));

        let open = manager_with_block(IVec3::new(8, 15, 8));
        assert!(has_headroom(&open, &Structures::default(), position));
    }

    #[test]
    fn shallow_water_allows_a_buffered_ground_jump() {
        assert!(wants_water_jump(true, false, true, true));
        assert!(!wants_water_jump(true, true, true, true));
        assert!(!wants_water_jump(true, false, true, false));
        assert!(!wants_water_jump(true, false, false, true));
        assert!(!wants_water_jump(false, false, true, true));
    }

    #[test]
    fn sprinting_drains_stamina_and_regenerates_after_a_delay() {
        let mut stamina = Stamina::default();
        stamina.update(1.0, true, true);
        assert!(stamina.sprinting);
        assert!(stamina.current < stamina.max);
        let after_sprint = stamina.current;

        // Recovery waits for the delay before ticking up.
        stamina.update(0.5, false, false);
        assert_eq!(stamina.current, after_sprint);
        stamina.update(0.5, false, false);
        assert!(stamina.current > after_sprint);
    }

    #[test]
    fn a_full_stamina_bar_lasts_a_reasonable_sprint() {
        let mut stamina = Stamina::default();
        for _ in 0..10 {
            stamina.update(1.0, true, true);
        }
        assert!(
            stamina.current > 0.0,
            "a full bar should last more than ten seconds of sprinting"
        );

        stamina.update(3.0, true, true);
        assert_eq!(stamina.current, 0.0);
    }

    #[test]
    fn holding_sprint_at_zero_does_not_flicker_back_to_sprinting() {
        let mut stamina = Stamina {
            current: 0.0,
            ..default()
        };
        stamina.update(1.0, true, false);
        assert_eq!(stamina.current, 0.0);
        assert_eq!(stamina.fraction(), 0.0);
    }

    #[test]
    fn resolve_axis_depenetrates_to_nearest_face_without_teleporting_across() {
        let manager = manager_with_block(IVec3::new(10, 10, 10));
        // Overlapping the block from the left side: nearest exit is back to the
        // left face, not ejection to the far/right side of the block.
        let position = Vec3::new(10.2, 10.0, 10.5);
        let limit = resolve_axis(
            &manager,
            &Structures::default(),
            position,
            0,
            true,
            PLAYER_HEIGHT,
        )
        .expect("overlapping block should resolve");
        let expected_left = 10.0 - PLAYER_HALF_WIDTH - EPSILON;
        let expected_right = 11.0 + PLAYER_HALF_WIDTH + EPSILON;
        assert!(
            (limit - expected_left).abs() < 1e-4,
            "should resolve to nearest (left) face: got {limit}, left {expected_left}, right {expected_right}"
        );

        // Overlapping from the right side while moving left: nearest exit is the
        // right face, so resolution must not fling the player across to the left.
        let position = Vec3::new(10.8, 10.0, 10.5);
        let limit = resolve_axis(
            &manager,
            &Structures::default(),
            position,
            0,
            false,
            PLAYER_HEIGHT,
        )
        .expect("overlapping block should resolve");
        assert!(
            (limit - expected_right).abs() < 1e-4,
            "should resolve to nearest (right) face: got {limit}, left {expected_left}, right {expected_right}"
        );
    }

    #[test]
    fn player_can_move_out_of_overlap_without_being_teleported_across() {
        let manager = manager_with_block(IVec3::new(10, 10, 10));
        // Start overlapping from the right side and try to move away to the right.
        let mut position = Vec3::new(10.8, 10.0, 10.5);
        let mut velocity = Vec3::new(2.0, 0.0, 0.0);
        let start_x = position.x;
        move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );
        assert!(
            position.x >= 11.0 + PLAYER_HALF_WIDTH - 1e-3,
            "player should exit to the near/right side, got {} (started {})",
            position.x,
            start_x
        );
        assert!(
            position.x < 12.0,
            "player must not be teleported far away, got {}",
            position.x
        );
    }

    #[test]
    fn wood_stems_block_player_movement() {
        let stem = Voxel::from_oriented_parts(
            Block::Wood,
            Shape::Stem,
            crate::world::block::Orientation::default(),
        );
        let manager = manager_with_voxel(IVec3::new(10, 10, 10), stem);
        assert!(
            !manager
                .collision_boxes(IVec3::new(10, 10, 10))
                .is_none_or(|boxes| boxes.is_empty()),
            "stem collision must be reported for movement"
        );

        let mut position = Vec3::new(9.5, 10.0, 10.5);
        let mut velocity = Vec3::new(6.0, 0.0, 0.0);
        move_and_collide(
            &manager,
            &Structures::default(),
            &mut position,
            &mut velocity,
            0.1,
            PLAYER_HEIGHT,
            false,
        );
        assert!(
            position.x < 10.25,
            "player should be stopped by the stem collider, got x={}",
            position.x
        );
        assert_eq!(velocity.x, 0.0);
    }

    #[test]
    fn ground_probe_requires_horizontal_support_overlap() {
        // Thin stem occupies the centre of the cell; probing beside it (outside
        // its XZ bounds but inside the same cell range) must not count as ground.
        let stem = Voxel::from_oriented_parts(
            Block::Wood,
            Shape::Stem,
            crate::world::block::Orientation::default(),
        );
        let manager = manager_with_voxel(IVec3::new(10, 10, 10), stem);
        // Stem is at x/z 0.25..0.75 within cell 10 => world 10.25..10.75.
        // Place player so feet probe cell 10 vertically but footprint is left of stem.
        let beside = Vec3::new(9.5, 11.0, 10.5);
        assert!(
            !is_on_ground(&manager, &Structures::default(), beside),
            "ground support must overlap the player's footprint"
        );
        let above = Vec3::new(10.5, 11.0, 10.5);
        assert!(is_on_ground(&manager, &Structures::default(), above));
    }
}
