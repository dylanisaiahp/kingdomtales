import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_functions,
    view_transformations::position_world_to_clip,
};

struct CloudUniformData {
    // rgb = day/night-tinted cloud color, a = unused
    tint: vec4<f32>,
    // xyz = sun direction (world), w = sun strength (0..1, also proxies day brightness)
    sun_dir: vec4<f32>,
    // x = unused, y = bottom brightness, z = top brightness, w = unused
    params: vec4<f32>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> cloud: CloudUniformData;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
};

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    let world_pos = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(in.position, 1.0),
    );
    out.world_position = world_pos;
    out.position = position_world_to_clip(world_pos.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    @if(VERTEX_UVS_A) { out.uv = in.uv; }
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let sun_strength = clamp(cloud.sun_dir.w, 0.0, 1.0);

    // Stylized cloud lighting: keep undersides bright, preserve top/side/bottom depth.
    // Ambient scales down at night so day/night tint is clearly visible.
    let up = clamp(n.y * 0.5 + 0.5, 0.0, 1.0);
    let bottom_brightness = cloud.params.y;
    let top_brightness = cloud.params.z;
    let vertical = mix(bottom_brightness, top_brightness, up);

    let sun_dir = normalize(cloud.sun_dir.xyz);
    let sun_dot = max(dot(n, sun_dir), 0.0);
    let ambient = mix(0.45, 0.88, sun_strength);
    let sun = sun_dot * 0.14 * sun_strength;
    let shade = clamp(vertical * ambient + sun, 0.0, 1.1);

    var color = cloud.tint.rgb * shade;
    color = clamp(color, vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 1.0));
    return vec4<f32>(color, 1.0);
}
