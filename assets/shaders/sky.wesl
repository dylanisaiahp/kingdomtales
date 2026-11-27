import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_functions,
    mesh_view_bindings::view,
    utils::coords_to_viewport_uv,
    view_transformations::position_world_to_clip,
};

struct SkyUniformData {
    sun_dir: vec4<f32>,
    moon_dir: vec4<f32>,
    horizon: vec4<f32>,
    zenith: vec4<f32>,
    ground: vec4<f32>,
    sun_color: vec4<f32>,
    sun_glow: vec4<f32>,
    moon_color: vec4<f32>,
    params: vec4<f32>, // x=star_strength, y=twilight, z=day, w=night
};

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> sky_u: SkyUniformData;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

fn bxor(a: u32, b: u32) -> u32 { return (a | b) & ~(a & b); }
fn hash3(p: vec3<i32>) -> f32 {
    var n = bxor(bxor(u32(p.x) * 73856093u, u32(p.y) * 19349663u), u32(p.z) * 83492791u);
    n = bxor(n, n >> 13u) * 1274126177u;
    n = bxor(n, n >> 16u);
    return f32(n) / 4294967295.0;
}

fn celestial_basis(dir: vec3<f32>) -> mat3x3<f32> {
    let forward = normalize(dir);
    var world_up = vec3<f32>(0.0, 1.0, 0.0);
    if (abs(forward.y) > 0.95) {
        world_up = vec3<f32>(0.0, 0.0, 1.0);
    }
    var right = normalize(cross(world_up, forward));
    if (dot(right, right) < 0.0001) {
        right = vec3<f32>(1.0, 0.0, 0.0);
    }
    let up = normalize(cross(forward, right));
    return mat3x3<f32>(right, up, forward);
}

// Reconstruct the camera ray from the fragment's screen position. This mirrors
// Bevy's skybox: view space has the camera at the origin, so the unprojected
// near-plane position is the ray direction. The world translation is ignored by
// multiplying with w = 0, which keeps the sky stable at large world positions.
fn sky_ray_direction(frag_coord: vec2<f32>) -> vec3<f32> {
    let uv = coords_to_viewport_uv(frag_coord, view.main_pass_viewport);
    let ndc = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let view_position_homogeneous = view.view_from_clip * vec4<f32>(ndc, 1.0, 1.0);
    let view_ray_direction = view_position_homogeneous.xyz / view_position_homogeneous.w;
    let world_ray_direction = (view.world_from_view * vec4<f32>(view_ray_direction, 0.0)).xyz;
    return normalize(world_ray_direction);
}

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    let world_pos = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.world_position = world_pos;
    let clip = position_world_to_clip(world_pos.xyz);
    // Reverse-Z far plane. Pair with GreaterEqual/no depth write so the sky
    // never overwrites opaque geometry regardless of Opaque3d draw order.
    out.position = vec4<f32>(clip.xy, 0.0, clip.w);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
@if(VERTEX_UVS_A) { out.uv = in.uv; }
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Direction from the camera in world space.
    let dir = sky_ray_direction(in.position.xy);
    let y = dir.y;
    let above = smoothstep(-0.12, 0.08, y);
    let sky = mix(sky_u.horizon.rgb, sky_u.zenith.rgb, smoothstep(0.0, 0.75, max(y, 0.0)));
    var color = mix(sky_u.ground.rgb, sky, above);

    let sun_dir = normalize(sky_u.sun_dir.xyz);
    let sun_cos = clamp(dot(dir, sun_dir), -1.0, 1.0);
    let sun_halo_angle = 0.28;
    let sun_half = 0.038;
    if (sun_cos > cos(sun_halo_angle)) {
        let sun_angle = acos(sun_cos);
        let sun_halo = (1.0 - smoothstep(sun_half * 1.4, sun_halo_angle, sun_angle))
            * 0.28
            * smoothstep(-0.15, 0.05, sun_dir.y);
        if (sun_halo > 0.0) {
            let glow_color = mix(sky_u.sun_color.rgb, sky_u.sun_glow.rgb, clamp(sky_u.params.y, 0.0, 1.0));
            color = mix(color, glow_color, clamp(sun_halo, 0.0, 1.0));
        }
        if (sun_cos > cos(sun_half * 1.8)) {
            let b = celestial_basis(sun_dir);
            let lz = dot(dir, b[2]);
            if (lz > 0.0001) {
                let px = dot(dir, b[0]) / lz;
                let py = dot(dir, b[1]) / lz;
                if (abs(px) <= sun_half && abs(py) <= sun_half) {
                    let edge = max(0.0, sun_half - max(abs(px), abs(py)));
                    let aa = smoothstep(0.0, sun_half * 0.08, edge);
                    let edge_shade = 0.92 + 0.08 * aa;
                    var sun_disc = vec3<f32>(1.0, 0.84, 0.36) * edge_shade;
                    sun_disc = mix(sun_disc, vec3<f32>(1.0, 0.68, 0.40), clamp(sky_u.params.y, 0.0, 1.0) * 0.20);
                    color = sun_disc;
                }
            }
        }
    }

    let moon_dir = normalize(sky_u.moon_dir.xyz);
    let moon_cos = clamp(dot(dir, moon_dir), -1.0, 1.0);
    let moon_halo_angle = 0.18;
    let moon_half = 0.030;
    if (moon_cos > cos(moon_halo_angle)) {
        let moon_angle = acos(moon_cos);
        let moon_halo = (1.0 - smoothstep(moon_half * 1.4, moon_halo_angle, moon_angle))
            * 0.14
            * smoothstep(-0.1, 0.05, moon_dir.y);
        if (moon_halo > 0.0) {
            color = mix(color, sky_u.moon_color.rgb, clamp(moon_halo, 0.0, 1.0));
        }
        if (moon_cos > cos(moon_half * 1.8)) {
            let b = celestial_basis(moon_dir);
            let lz = dot(dir, b[2]);
            if (lz > 0.0001) {
                let px = dot(dir, b[0]) / lz;
                let py = dot(dir, b[1]) / lz;
                if (abs(px) <= moon_half && abs(py) <= moon_half) {
                    let edge = max(0.0, moon_half - max(abs(px), abs(py)));
                    let aa = smoothstep(0.0, moon_half * 0.08, edge);
                    let edge_shade = 0.92 + 0.08 * aa;
                    var moon_disc = vec3<f32>(0.74, 0.82, 0.96) * edge_shade;
                    let u = clamp(px / moon_half * 0.5 + 0.5, 0.0, 1.0);
                    let v = clamp(py / moon_half * 0.5 + 0.5, 0.0, 1.0);
                    let qx = i32(u * 24.0);
                    let qy = i32(v * 24.0);
                    let n = hash3(vec3<i32>(9, qx, qy));
                    let crater = smoothstep(0.72, 0.92, n);
                    let shade = 1.0 - crater * 0.10;
                    moon_disc = vec3<f32>(moon_disc.r * shade, moon_disc.g * shade, moon_disc.b * (1.0 - crater * 0.06));
                    color = moon_disc;
                }
            }
        }
    }

    // Stars: stable by quantized direction.
    let star_strength = clamp(sky_u.params.x, 0.0, 1.0);
    if (star_strength > 0.001 && dir.y > 0.02) {
        let cell = vec3<i32>(i32(floor(dir.x * 220.0)), i32(floor(dir.y * 220.0)), i32(floor(dir.z * 220.0)));
        let r = hash3(cell);
        if (r > 0.9975) {
            let brightness = clamp((r - 0.9975) / 0.0025, 0.0, 1.0);
            let twinkle = 0.65 + 0.35 * hash3(cell.yzx);
            let fade = smoothstep(0.02, 0.2, dir.y);
            let s = brightness * twinkle * fade * star_strength;
            color += vec3<f32>(s, s, s * 1.08);
        }
    }

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
