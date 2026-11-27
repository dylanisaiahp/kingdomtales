//! Shader-driven procedural sky.
//!
//! The sky is rendered by a camera-attached fullscreen-background cube using a
//! custom material. Colors are computed per-fragment from `DayNightCycle`, so
//! sun/moon/stars move smoothly every frame with no CPU cubemap regeneration.

use bevy::camera::visibility::RenderLayers;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Mesh3d, MeshVertexBufferLayoutRef};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MeshMaterial3d};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::day_night::{DayNightCycle, lerp_color, smoothstep};
use crate::player::PlayerCamera;

/// The sky dome lives outside the shared world layer so the Live map camera can
/// render terrain, flora, and clouds over a transparent background.
pub(crate) const SKY_RENDER_LAYER: usize = 1;

/// Half-size of square celestial disc in sky-direction space (distance 1).
#[allow(dead_code)]
const SUN_HALF_SIZE: f32 = 0.038;
#[allow(dead_code)]
const MOON_HALF_SIZE: f32 = 0.030;
/// Soft halo around celestial bodies.
#[allow(dead_code)]
const SUN_HALO_ANGLE: f32 = 0.28;
#[allow(dead_code)]
const SUN_HALO_STRENGTH: f32 = 0.28;
#[allow(dead_code)]
const MOON_HALO_ANGLE: f32 = 0.18;
#[allow(dead_code)]
const MOON_HALO_STRENGTH: f32 = 0.14;

#[derive(Clone, Copy, Debug)]
pub struct SkyPalette {
    pub horizon: [f32; 3],
    pub zenith: [f32; 3],
    pub ground: [f32; 3],
    pub sun_color: [f32; 3],
    pub sun_glow: [f32; 3],
    pub moon_color: [f32; 3],
    pub star_strength: f32,
}

impl SkyPalette {
    pub fn from_cycle(cycle: &DayNightCycle) -> Self {
        let day = cycle.daylight_factor();
        let night = cycle.night_factor();
        let twilight = cycle.twilight_factor();
        let sunrise = sunrise_weight(cycle);

        let day_horizon = [0.62, 0.78, 0.92];
        let day_zenith = [0.24, 0.45, 0.78];
        let day_ground = [0.32, 0.39, 0.43];

        let night_horizon = [0.04, 0.06, 0.12];
        let night_zenith = [0.01, 0.02, 0.06];
        let night_ground = [0.01, 0.015, 0.03];

        let dusk_horizon = [0.92, 0.45, 0.25];
        let dusk_zenith = [0.18, 0.22, 0.45];
        let dawn_horizon = [0.95, 0.58, 0.38];

        let mut horizon = lerp_color(night_horizon, day_horizon, day);
        let mut zenith = lerp_color(night_zenith, day_zenith, day);
        let ground = lerp_color(night_ground, day_ground, day);

        let warm_horizon = if sunrise > 0.5 {
            dawn_horizon
        } else {
            dusk_horizon
        };
        horizon = lerp_color(horizon, warm_horizon, twilight * 0.85);
        zenith = lerp_color(zenith, dusk_zenith, twilight * 0.35);

        Self {
            horizon,
            zenith,
            ground,
            sun_color: [1.0, 0.94, 0.76],
            sun_glow: [1.0, 0.62, 0.32],
            moon_color: [0.74, 0.82, 1.0],
            star_strength: night * (1.0 - twilight * 0.8),
        }
    }
}

fn sunrise_weight(cycle: &DayNightCycle) -> f32 {
    let t = cycle.time_of_day.rem_euclid(1.0);
    let d = (t - 0.25).abs().min(1.0 - (t - 0.25).abs());
    let dawn = 1.0 - (d / 0.20).clamp(0.0, 1.0);
    let d2 = (t - 0.75).abs().min(1.0 - (t - 0.75).abs());
    let dusk = 1.0 - (d2 / 0.20).clamp(0.0, 1.0);
    if dawn + dusk <= 0.0001 {
        0.5
    } else {
        dawn / (dawn + dusk)
    }
}

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct SkyUniformData {
    pub sun_dir: Vec4,
    pub moon_dir: Vec4,
    pub horizon: Vec4,
    pub zenith: Vec4,
    pub ground: Vec4,
    pub sun_color: Vec4,
    pub sun_glow: Vec4,
    pub moon_color: Vec4,
    pub params: Vec4, // x=star_strength, y=twilight, z=day, w=night
}

impl Default for SkyUniformData {
    fn default() -> Self {
        let cycle = DayNightCycle::default();
        Self::from_cycle(&cycle)
    }
}

impl SkyUniformData {
    pub fn from_cycle(cycle: &DayNightCycle) -> Self {
        let palette = SkyPalette::from_cycle(cycle);
        Self {
            sun_dir: (-cycle.sun_direction()).normalize().extend(0.0),
            moon_dir: (-cycle.moon_direction()).normalize().extend(0.0),
            horizon: Vec4::from_array([
                palette.horizon[0],
                palette.horizon[1],
                palette.horizon[2],
                1.0,
            ]),
            zenith: Vec4::from_array([
                palette.zenith[0],
                palette.zenith[1],
                palette.zenith[2],
                1.0,
            ]),
            ground: Vec4::from_array([
                palette.ground[0],
                palette.ground[1],
                palette.ground[2],
                1.0,
            ]),
            sun_color: Vec4::from_array([
                palette.sun_color[0],
                palette.sun_color[1],
                palette.sun_color[2],
                1.0,
            ]),
            sun_glow: Vec4::from_array([
                palette.sun_glow[0],
                palette.sun_glow[1],
                palette.sun_glow[2],
                1.0,
            ]),
            moon_color: Vec4::from_array([
                palette.moon_color[0],
                palette.moon_color[1],
                palette.moon_color[2],
                1.0,
            ]),
            params: Vec4::new(
                palette.star_strength,
                cycle.twilight_factor(),
                cycle.daylight_factor(),
                cycle.night_factor(),
            ),
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone, Default)]
pub struct SkyMaterial {
    #[uniform(0)]
    pub data: SkyUniformData,
}

impl Material for SkyMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/sky.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/sky.wesl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }

    // Sky is a background full-scene dome rendered with custom depth handling;
    // it does not provide compatible prepass outputs for SSAO/occlusion.
    fn enable_prepass() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            // Bevy uses reverse-Z: 0.0 is the far plane. Keep the sky at far
            // depth and use GreaterEqual so it only draws where no opaque
            // geometry wrote depth, regardless of Opaque3d submission order.
            depth.depth_write_enabled = Some(false);
            depth.depth_compare = Some(CompareFunction::GreaterEqual);
        }
        Ok(())
    }
}

#[derive(Component)]
pub struct SkyDome;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<SkyMaterial>::default())
            .add_systems(Startup, spawn_sky_dome)
            .add_systems(Update, (follow_sky_dome, update_sky_material));
    }
}

fn spawn_sky_dome(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    let mesh = meshes.add(Cuboid::new(2.0, 2.0, 2.0));
    let material = materials.add(SkyMaterial::default());
    commands.spawn((
        Name::new("SkyDome"),
        SkyDome,
        NotShadowCaster,
        RenderLayers::layer(SKY_RENDER_LAYER),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_scale(Vec3::splat(1000.0)),
    ));
}

fn follow_sky_dome(
    camera_q: Query<&GlobalTransform, (With<PlayerCamera>, Without<SkyDome>)>,
    mut sky_q: Query<&mut Transform, With<SkyDome>>,
) {
    let Ok(cam) = camera_q.single() else {
        return;
    };
    let Ok(mut sky) = sky_q.single_mut() else {
        return;
    };
    sky.translation = cam.translation();
}

fn update_sky_material(
    cycle: Res<DayNightCycle>,
    sky_q: Query<&MeshMaterial3d<SkyMaterial>, With<SkyDome>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    let Ok(handle) = sky_q.single() else {
        return;
    };
    let Some(mut mat) = materials.get_mut(&handle.0) else {
        return;
    };
    mat.data = SkyUniformData::from_cycle(&cycle);
}

// ---------- CPU reference functions kept for tests and parity ----------

#[allow(dead_code)]
fn hash_star(face: u32, x: u32, y: u32) -> f32 {
    let mut n = face.wrapping_mul(73856093) ^ x.wrapping_mul(19349663) ^ y.wrapping_mul(83492791);
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    n ^= n >> 16;
    (n as f32 / u32::MAX as f32).clamp(0.0, 1.0)
}

#[allow(dead_code)]
fn celestial_basis(dir: Vec3) -> (Vec3, Vec3, Vec3) {
    let forward = dir.normalize();
    let world_up = if forward.y.abs() > 0.95 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let mut right = world_up.cross(forward).normalize_or_zero();
    if right.length_squared() < 0.0001 {
        right = Vec3::X;
    }
    let up = forward.cross(right).normalize();
    (right, up, forward)
}

#[allow(dead_code)]
fn celestial_square_color(
    view_dir: Vec3,
    body_dir: Vec3,
    half_size: f32,
    base_color: [f32; 3],
    is_moon: bool,
) -> Option<[f32; 3]> {
    let (right, up, forward) = celestial_basis(body_dir);
    let lz = view_dir.dot(forward);
    if lz <= 0.0001 {
        return None;
    }
    let px = view_dir.dot(right) / lz;
    let py = view_dir.dot(up) / lz;
    let ax = px.abs();
    let ay = py.abs();
    if ax > half_size || ay > half_size {
        return None;
    }

    let mut color = base_color;
    if is_moon {
        let u = (px / half_size * 0.5 + 0.5).clamp(0.0, 1.0);
        let v = (py / half_size * 0.5 + 0.5).clamp(0.0, 1.0);
        let qx = (u * 24.0) as u32;
        let qy = (v * 24.0) as u32;
        let n = hash_star(9, qx, qy);
        let crater = smoothstep(0.72, 0.92, n);
        let shade = 1.0 - crater * 0.10;
        color[0] *= shade;
        color[1] *= shade;
        color[2] *= 1.0 - crater * 0.06;
    }

    let edge = (half_size - ax.max(ay)).max(0.0);
    let aa = smoothstep(0.0, half_size * 0.08, edge);
    let edge_shade = 0.92 + 0.08 * aa;
    Some([
        (color[0] * edge_shade).clamp(0.0, 1.0),
        (color[1] * edge_shade).clamp(0.0, 1.0),
        (color[2] * edge_shade).clamp(0.0, 1.0),
    ])
}

/// Sample the procedural sky in linear scalar colour channels.
/// Kept as CPU reference for tests/parity with the shader.
#[allow(dead_code)]
pub fn sky_color_for_cycle(direction: Vec3, cycle: &DayNightCycle) -> [f32; 3] {
    let direction = direction.normalize();
    let palette = SkyPalette::from_cycle(cycle);
    let y = direction.y;
    let above = smoothstep(-0.12, 0.08, y);
    let sky = lerp_color(
        palette.horizon,
        palette.zenith,
        smoothstep(0.0, 0.75, y.max(0.0)),
    );
    let mut color = lerp_color(palette.ground, sky, above);

    let sun_dir = (-cycle.sun_direction()).normalize();
    let sun_cos = direction.dot(sun_dir).clamp(-1.0, 1.0);
    if sun_cos > (SUN_HALO_ANGLE).cos() {
        let sun_angle = sun_cos.acos();
        let sun_halo = (1.0 - smoothstep(SUN_HALF_SIZE * 1.4, SUN_HALO_ANGLE, sun_angle))
            * SUN_HALO_STRENGTH
            * smoothstep(-0.15, 0.05, sun_dir.y);
        if sun_halo > 0.0 {
            let glow_color =
                lerp_color(palette.sun_color, palette.sun_glow, cycle.twilight_factor());
            color = lerp_color(color, glow_color, sun_halo.clamp(0.0, 1.0));
        }
        if sun_cos > (SUN_HALF_SIZE * 1.8).cos()
            && let Some(sun_disc) =
                celestial_square_color(direction, sun_dir, SUN_HALF_SIZE, [1.0, 0.84, 0.36], false)
        {
            let sun_tint = lerp_color(sun_disc, [1.0, 0.68, 0.40], cycle.twilight_factor() * 0.20);
            color = sun_tint;
        }
    }

    let moon_dir = (-cycle.moon_direction()).normalize();
    let moon_cos = direction.dot(moon_dir).clamp(-1.0, 1.0);
    if moon_cos > (MOON_HALO_ANGLE).cos() {
        let moon_angle = moon_cos.acos();
        let moon_halo = (1.0 - smoothstep(MOON_HALF_SIZE * 1.4, MOON_HALO_ANGLE, moon_angle))
            * MOON_HALO_STRENGTH
            * smoothstep(-0.1, 0.05, moon_dir.y);
        if moon_halo > 0.0 {
            color = lerp_color(color, palette.moon_color, moon_halo.clamp(0.0, 1.0));
        }
        if moon_cos > (MOON_HALF_SIZE * 1.8).cos()
            && let Some(moon_disc) = celestial_square_color(
                direction,
                moon_dir,
                MOON_HALF_SIZE,
                [0.74, 0.82, 0.96],
                true,
            )
        {
            color = moon_disc;
        }
    }

    color
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_darkens_below_the_horizon_and_shows_a_small_sun() {
        let cycle = DayNightCycle {
            time_of_day: 0.5,
            ..Default::default()
        };
        let zenith = sky_color_for_cycle(Vec3::Y, &cycle);
        let horizon = sky_color_for_cycle(Vec3::new(0.0, 0.0, 1.0), &cycle);
        let ground = sky_color_for_cycle(Vec3::NEG_Y, &cycle);
        assert!(zenith[2] > horizon[2]);
        assert!(ground[0] < horizon[0]);

        let sun_dir = (-cycle.sun_direction()).normalize();
        let sun = sky_color_for_cycle(sun_dir, &cycle);
        let away = sun_dir.cross(Vec3::Z).normalize();
        let near_sun = sky_color_for_cycle((sun_dir + away * 0.4).normalize(), &cycle);
        assert!(sun[0] > near_sun[0]);
        assert!(sun[1] > near_sun[1]);
    }

    #[test]
    fn sky_palette_has_day_night_variation() {
        let day = SkyPalette::from_cycle(&DayNightCycle {
            time_of_day: 0.5,
            ..Default::default()
        });
        let night = SkyPalette::from_cycle(&DayNightCycle {
            time_of_day: 0.0,
            ..Default::default()
        });
        assert!(day.zenith[2] > night.zenith[2]);
        assert!(night.star_strength > day.star_strength);
    }
}
