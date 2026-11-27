import bevy_pbr::render::{
    mesh_functions,
    view_transformations::position_world_to_clip,
};

struct WaterUniformData {
    time: f32,
    alpha: f32,
    wave_scale: f32,
    _padding: f32,
    camera: vec4<f32>, // xyz=camera world pos
    deep_color: vec4<f32>,
    shallow_color: vec4<f32>,
    sun_dir: vec4<f32>,    // xyz=direction toward sun, w=daylight factor
    sun_color: vec4<f32>,  // rgb=tint, w=spec intensity
    moon_dir: vec4<f32>,   // xyz=direction toward moon, w=night factor
    moon_color: vec4<f32>, // rgb=tint, w=spec intensity
};

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> water_u: WaterUniformData;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var detail_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var detail_sampler: sampler;

// Mirrors bevy_pbr::forward_io::VertexOutput and adds normalized water depth.
struct WaterVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @if(VERTEX_UVS_A) @location(2) uv: vec2<f32>,
    @if(VERTEX_UVS_B) @location(3) uv_b: vec2<f32>,
    @if(VERTEX_TANGENTS) @location(4) world_tangent: vec4<f32>,
    @if(VERTEX_COLORS) @location(5) color: vec4<f32>,
    @if(VERTEX_OUTPUT_INSTANCE_INDEX) @location(6) @interpolate(flat) instance_index: u32,
    @if(VISIBILITY_RANGE_DITHER) @location(7) @interpolate(flat) visibility_range_dither: i32,
    @location(8) water_depth: f32,
};

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @if(VERTEX_UVS_A) @location(2) uv: vec2<f32>,
    @if(VERTEX_TANGENTS) @location(4) tangent: vec4<f32>,
    @if(VERTEX_COLORS) @location(5) color: vec4<f32>,
    @location(15) wave_weight: f32,
    @location(16) water_depth: f32,
};

fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453);
}

fn noise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(i);
    let b = hash2(i + vec2<f32>(1.0, 0.0));
    let c = hash2(i + vec2<f32>(0.0, 1.0));
    let d = hash2(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Shared by the top surface and the exposed upper edge of side faces. Sampling
// world XZ from the undisplaced vertex makes matching lattice corners identical.
// x=height, y=d(height)/dx, z=d(height)/dz. Slopes are world-space because the
// wave phases are evaluated from world XZ.
fn water_wave_field(world_xz: vec2<f32>, t: f32) -> vec3<f32> {
    let p1 = world_xz.x * 0.28 + t * 1.35;
    let p2 = world_xz.y * 0.36 + t * 1.05;
    let p3 = (world_xz.x * 0.7 + world_xz.y * 0.9) * 0.22 + t * 0.85;
    let p4 = (world_xz.x - world_xz.y) * 0.18 - t * 0.65;
    let scale = water_u.wave_scale;
    let height = (sin(p1) * 0.045 + cos(p2) * 0.035 + sin(p3) * 0.03 + cos(p4) * 0.02) * scale;
    let slope_x = (cos(p1) * 0.045 * 0.28 + cos(p3) * 0.03 * 0.22 * 0.7 - sin(p4) * 0.02 * 0.18) * scale;
    let slope_z = (-sin(p2) * 0.035 * 0.36 + cos(p3) * 0.03 * 0.22 * 0.9 + sin(p4) * 0.02 * 0.18) * scale;
    return vec3<f32>(height, slope_x, slope_z);
}

@vertex
fn vertex(in: Vertex) -> WaterVertexOutput {
    var out: WaterVertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    var local_pos = in.position;
    var world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    if (in.wave_weight > 0.0) {
        let world_xz = (world_from_local * vec4<f32>(local_pos, 1.0)).xz;
        let wave = water_wave_field(world_xz, water_u.time);
        local_pos.y += wave.x * in.wave_weight;
        // Top faces get the analytic slope of the exact wave used above. Side
        // faces keep their geometric normal because only their upper edge waves.
        if (in.normal.y > 0.5) {
            world_normal = normalize(vec3<f32>(-wave.y, 1.0, -wave.z));
        }
    }
    let world_pos = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(local_pos, 1.0));
    out.world_position = world_pos;
    out.position = position_world_to_clip(world_pos.xyz);
    out.world_normal = world_normal;
@if(VERTEX_UVS_A) { out.uv = in.uv; }
@if(VERTEX_TANGENTS) {
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(world_from_local, in.tangent, in.instance_index);
}
@if(VERTEX_COLORS) { out.color = in.color; }
@if(VERTEX_OUTPUT_INSTANCE_INDEX) { out.instance_index = in.instance_index; }
    out.water_depth = in.water_depth;
    return out;
}

@fragment
fn fragment(in: WaterVertexOutput) -> @location(0) vec4<f32> {
    let t = water_u.time;
    let world_pos = in.world_position.xyz;
    let view_dir = normalize(water_u.camera.xyz - world_pos);
    var n = normalize(in.world_normal);
    let top_face = smoothstep(0.7, 0.95, n.y);

    // Two world-space tile samples: different scales, rotations and scroll
    // directions keep the 32x32 tile from reading as a repeating grid.
    let uv_a = world_pos.xz * 0.16 + vec2<f32>(t * 0.010, -t * 0.014);
    let rotated = vec2<f32>(
        dot(world_pos.xz, vec2<f32>(0.866, -0.5)),
        dot(world_pos.xz, vec2<f32>(0.5, 0.866)),
    );
    let uv_b = rotated * 0.085 + vec2<f32>(-t * 0.007, t * 0.011);
    let detail_a = textureSample(detail_texture, detail_sampler, uv_a);
    let detail_b = textureSample(detail_texture, detail_sampler, uv_b);

    // One cheap procedural layer keeps the two periodic tiles from lining up.
    let variation = noise2(world_pos.xz * 0.07 + vec2<f32>(t * 0.02, t * 0.01));

    // Ripple normal perturbation (stronger on top surface)
    let ripple = (detail_a.r - 0.5) * 0.26 + (detail_b.g - 0.5) * 0.14 + (variation - 0.5) * 0.05;
    n = normalize(vec3<f32>(n.x + ripple * top_face, n.y, n.z + ripple * 0.7 * top_face));

    let fresnel = pow(1.0 - clamp(dot(n, view_dir), 0.0, 1.0), 4.5);
    let daylight = clamp(water_u.sun_dir.w, 0.0, 1.0);
    let night = clamp(water_u.moon_dir.w, 0.0, 1.0);

    // Evaluate both lights so sunrise/sunset crossfade smoothly instead of
    // switching direction and color at the horizon. Spec strengths are scaled
    // by the shared daylight/night factors, keeping moon glints subtle.
    let sun_half = normalize(water_u.sun_dir.xyz + view_dir + vec3<f32>(0.0, 1e-6, 0.0));
    let sun_ndoth = max(dot(n, sun_half), 0.0);
    let sun_spec = pow(sun_ndoth, 180.0) * 0.9 * water_u.sun_color.w * daylight;
    let sun_broad_spec = pow(sun_ndoth, 28.0) * 0.12 * water_u.sun_color.w * daylight;

    let moon_half = normalize(water_u.moon_dir.xyz + view_dir + vec3<f32>(0.0, 1e-6, 0.0));
    let moon_ndoth = max(dot(n, moon_half), 0.0);
    let moon_spec = pow(moon_ndoth, 180.0) * 0.9 * water_u.moon_color.w * night;

    // Base color. `depth_tint` is surface variation only; actual water depth is
    // the dedicated per-vertex seabed value from the mesher.
    let depth_tint = clamp(detail_a.r * 0.6 + detail_b.g * 0.4 + (variation - 0.5) * 0.1, 0.0, 1.0);
    let water_depth = clamp(in.water_depth, 0.0, 1.0);
    let shallow = water_u.shallow_color.rgb;
    let deep = water_u.deep_color.rgb;
    let surface_variation = clamp(depth_tint * 0.55 + fresnel * 0.15, 0.0, 1.0);
    // Shore water stays close to the deep palette. Depth progressively removes
    // even that small shallow tint and darkens the whole column.
    let shallow_weight = surface_variation * 0.35 * (1.0 - 0.85 * water_depth);
    var base = mix(deep, shallow, shallow_weight) * (1.0 - 0.42 * water_depth);
    // Direct, subtle albedo mottling so the tile reads as surface detail rather
    // than only changing highlights at grazing angles.
    base *= mix(0.88, 1.12, detail_a.g);
    // Subtle glints are daylight-only; reflections must not illuminate the water.
    let sparkle = smoothstep(0.70, 0.92, detail_a.r) * 0.07 * top_face * daylight * water_u.sun_color.w;
    base += water_u.sun_color.rgb * sparkle;

    let reflected = water_u.sun_color.rgb * (sun_spec + sun_broad_spec)
        + water_u.moon_color.rgb * moon_spec;
    let color = base + reflected;
    // Even shallow water is mostly opaque; deeper water becomes nearly solid so
    // neither the seabed nor the open-ocean floor shows through.
    let base_alpha = mix(
        max(water_u.shallow_color.a, 0.86),
        max(water_u.deep_color.a, 0.92),
        fresnel,
    );
    let depth_alpha = mix(base_alpha, 0.99, smoothstep(0.0, 0.60, water_depth));
    let alpha = clamp(depth_alpha, 0.34, 0.99) * water_u.alpha;
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), alpha);
}
