import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_functions,
    mesh_view_bindings::globals,
    view_transformations::position_world_to_clip,
};

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    @if(VERTEX_UVS_B)
    @location(3) uv_b: vec2<f32>,
    @if(VERTEX_TANGENTS)
    @location(4) tangent: vec4<f32>,
    @if(VERTEX_COLORS)
    @location(5) color: vec4<f32>,
    @location(14) wind_weight: f32,
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    let world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );

    // Coherent sway: snap phase to a coarse grid so nearby foliage moves together,
    // preventing gaps between adjacent leaf cells. Bottom vertices have weight 0.
    let wind_strength = 0.03;
    let wind_speed = 1.1;
    let wind_scale = 0.35;

    let t = globals.time * wind_speed;
    let cell = floor(world_position.xz / 3.0);
    let phase = dot(cell, vec2<f32>(0.37, 0.61)) * wind_scale;
    let sway = sin(t + phase) + 0.35 * sin(t * 1.6 + phase * 1.2);
    let gust = sin(t * 0.22 + cell.x * 0.11 + cell.y * 0.09);
    let amount = (sway * 0.75 + gust * 0.25) * wind_strength * vertex.wind_weight;

    var displaced = world_position;
    displaced.x += amount;
    displaced.z += amount * 0.55;

    out.world_position = displaced;
    out.position = position_world_to_clip(displaced.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(vertex.normal, vertex.instance_index);

    @if(VERTEX_UVS_A)
    out.uv = vertex.uv;
    @if(VERTEX_UVS_B)
    out.uv_b = vertex.uv_b;
    @if(VERTEX_TANGENTS)
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        vertex.tangent,
        vertex.instance_index,
    );
    @if(VERTEX_COLORS)
    out.color = vertex.color;
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = vertex.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index,
        world_from_local[3],
    );

    return out;
}
