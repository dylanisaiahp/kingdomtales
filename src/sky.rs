//! Procedural gradient skybox.
//!
//! The sky is generated in memory as a small cubemap, so the game needs no
//! external texture assets. It renders behind the voxel world and is not fogged,
//! which keeps a cave mouth visibly open to daylight while underground fog stays
//! dark inside the cave.

use bevy::asset::RenderAssetUsages;
use bevy::camera::Exposure;
use bevy::light::Skybox;
use bevy::math::Vec3;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

/// One face of the generated cubemap. Six 256x256 faces are still only 1.5 MB
/// and give the sun a smooth circular edge instead of visible square texels.
const FACE_SIZE: u32 = 256;

/// World direction the sun appears to sit, opposite the directional light.
const SUN_DIRECTION: Vec3 = Vec3::new(-0.383, 0.751, 0.537);
/// Angular radius of the bright solar disc, with a small antialiasing band.
const SUN_DISK_RADIUS: f32 = 0.030;
const SUN_DISK_EDGE: f32 = 0.006;
/// Broader soft halo around the disc.
const SUN_HALO_ANGLE: f32 = 0.26;
const SUN_HALO_STRENGTH: f32 = 0.35;

const HORIZON_COLOR: [f32; 3] = [0.62, 0.78, 0.92];
const ZENITH_COLOR: [f32; 3] = [0.24, 0.45, 0.78];
const GROUND_COLOR: [f32; 3] = [0.32, 0.39, 0.43];
const SUN_COLOR: [f32; 3] = [1.0, 0.94, 0.76];

#[inline]
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn mix_color(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

/// Convert one cubemap texel to a direction. `face` follows the fixed-face
/// order +X, -X, +Y, -Y, +Z, -Z; `u` and `v` run from -1 to 1 with `v` top-down.
fn cubemap_direction(face: usize, u: f32, v: f32) -> Vec3 {
    match face {
        0 => Vec3::new(1.0, -v, -u),
        1 => Vec3::new(-1.0, -v, u),
        2 => Vec3::new(u, 1.0, v),
        3 => Vec3::new(u, -1.0, -v),
        4 => Vec3::new(u, -v, 1.0),
        _ => Vec3::new(-u, -v, -1.0),
    }
    .normalize()
}

/// Bevy's skybox shader samples cubemap coordinates with Z negated relative to
/// the world. Flip it here so the painted sun matches the directional light.
#[inline]
fn world_direction(cube_direction: Vec3) -> Vec3 {
    Vec3::new(cube_direction.x, cube_direction.y, -cube_direction.z)
}

/// Sample the procedural sky in linear scalar colour channels. The caller
/// writes these directly as sRGB bytes, which matches the image format.
fn sky_color(cube_direction: Vec3) -> [f32; 3] {
    let direction = world_direction(cube_direction);
    let y = direction.y;
    let above = smoothstep(-0.12, 0.08, y);
    let sky = mix_color(HORIZON_COLOR, ZENITH_COLOR, smoothstep(0.0, 0.75, y));
    let mut color = mix_color(GROUND_COLOR, sky, above);

    // Compare angles instead of thresholding the dot product. That keeps the
    // disc round and gives it a soft edge instead of a blocky square.
    let angle = direction
        .dot(SUN_DIRECTION.normalize())
        .clamp(-1.0, 1.0)
        .acos();
    let disk = 1.0
        - smoothstep(
            SUN_DISK_RADIUS - SUN_DISK_EDGE,
            SUN_DISK_RADIUS + SUN_DISK_EDGE,
            angle,
        );
    let halo = (1.0 - smoothstep(SUN_DISK_RADIUS, SUN_HALO_ANGLE, angle)) * SUN_HALO_STRENGTH;
    let glow = disk.max(halo);
    if glow > 0.0 {
        color = mix_color(color, SUN_COLOR, glow);
    }
    color
}

/// Build the cubemap image handed to Bevy's `Skybox` component.
pub fn create_sky_image() -> Image {
    let pixels_per_face = (FACE_SIZE * FACE_SIZE) as usize;
    let mut data = Vec::with_capacity(pixels_per_face * 6 * 4);
    for face in 0..6 {
        for row in 0..FACE_SIZE {
            let v = (row as f32 + 0.5) / FACE_SIZE as f32 * 2.0 - 1.0;
            for column in 0..FACE_SIZE {
                let u = (column as f32 + 0.5) / FACE_SIZE as f32 * 2.0 - 1.0;
                let color = sky_color(cubemap_direction(face, u, v));
                for channel in color {
                    data.push((channel.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                data.push(255);
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: FACE_SIZE,
            height: FACE_SIZE,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image
}

/// Attach the generated sky to the first-person camera.
///
/// Bevy multiplies skybox samples by the camera exposure. The default exposure
/// is calibrated for HDR skies, so an LDR cubemap needs the inverse gain or it
/// renders nearly black.
pub fn create_skybox(image: Handle<Image>) -> Skybox {
    Skybox {
        image: Some(image),
        brightness: 1.0 / Exposure::default().exposure(),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubemap_faces_point_outward() {
        let faces = [
            (Vec3::X, 0),
            (Vec3::NEG_X, 1),
            (Vec3::Y, 2),
            (Vec3::NEG_Y, 3),
            (Vec3::Z, 4),
            (Vec3::NEG_Z, 5),
        ];
        for (expected, face) in faces {
            let center = cubemap_direction(face, 0.0, 0.0);
            assert!(
                center.dot(expected) > 0.99,
                "face {face} should point {expected:?}, got {center:?}"
            );
        }
    }

    #[test]
    fn sky_darkens_below_the_horizon_and_shows_a_small_sun() {
        let zenith = sky_color(Vec3::Y);
        let horizon = sky_color(Vec3::new(0.0, 0.0, 1.0));
        let ground = sky_color(Vec3::NEG_Y);
        assert!(zenith[2] > horizon[2]);
        assert!(ground[0] < horizon[0]);

        let sun_cube = Vec3::new(SUN_DIRECTION.x, SUN_DIRECTION.y, -SUN_DIRECTION.z).normalize();
        let sun = sky_color(sun_cube);
        let near_sun = sky_color(Vec3::new(0.0, 1.0, 0.0));
        assert!(sun[0] > near_sun[0]);
        assert!(sun[1] > near_sun[1]);
    }

    #[test]
    fn solar_disc_is_round_and_soft_edged() {
        let sun = SUN_DIRECTION.normalize();
        let tangent = sun.cross(Vec3::Y).normalize();
        let bitangent = sun.cross(tangent).normalize();
        let to_cube = |world: Vec3| Vec3::new(world.x, world.y, -world.z).normalize();

        let sample = |axis: Vec3, angle: f32| {
            let direction = sun * angle.cos() + axis * angle.sin();
            sky_color(to_cube(direction))
        };
        let inside_a = sample(tangent, SUN_DISK_RADIUS * 0.5);
        let inside_b = sample(bitangent, SUN_DISK_RADIUS * 0.5);
        assert!((inside_a[0] - inside_b[0]).abs() < 0.02);
        assert!(inside_a[0] > 0.9);

        let outside = sample(tangent, SUN_DISK_RADIUS + SUN_DISK_EDGE * 2.0);
        let edge = sample(tangent, SUN_DISK_RADIUS);
        assert!(edge[0] < inside_a[0]);
        assert!(outside[0] < edge[0]);
    }

    #[test]
    fn sky_brightness_compensates_for_default_exposure() {
        let skybox = create_skybox(Handle::default());
        assert!(skybox.brightness > 100.0);
        assert!((skybox.brightness * Exposure::default().exposure() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn sky_cubemap_has_six_square_faces() {
        let image = create_sky_image();
        assert_eq!(image.texture_descriptor.size.width, FACE_SIZE);
        assert_eq!(image.texture_descriptor.size.height, FACE_SIZE);
        assert_eq!(image.texture_descriptor.size.depth_or_array_layers, 6);
        assert_eq!(
            image.data.as_ref().unwrap().len(),
            (FACE_SIZE * FACE_SIZE * 6 * 4) as usize
        );
        assert_eq!(
            image
                .texture_view_descriptor
                .as_ref()
                .unwrap()
                .dimension
                .unwrap(),
            TextureViewDimension::Cube
        );
    }
}
