import bevy_pbr::{
    render::forward_io::VertexOutput,
    render::mesh_functions,
    render::pbr_functions::pbr_input_from_standard_material,
    render::pbr_functions::alpha_discard,
    render::view_transformations::position_world_to_clip,
};

struct FoliageWindMaterial {
    params: vec4<f32>, // x = wind strength, y = elapsed time
};

@group(3) @binding(100) var<uniform> wind: FoliageWindMaterial;

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

    // Continuous world-space wind field. Displacement depends only on undeformed
    // world position + vertex wind weight, so vertices sharing the same world
    // position (including chunk/block boundaries) move together and do not seam.
    let wind_strength = wind.params.x;
    let wind_speed = 1.15;
    let t = wind.params.y * wind_speed;
    let wp = world_position.xz;

    // Layered continuous sin waves give coherent sway with natural variation.
    let sway_a = sin(t + wp.x * 0.55 + wp.y * 0.35);
    let sway_b = sin(t * 1.37 + wp.x * 0.21 - wp.y * 0.46 + 1.7);
    let sway_c = sin(t * 0.73 - wp.x * 0.18 + wp.y * 0.63 + 0.4);
    let gust = sin(t * 0.23 + wp.x * 0.07 + wp.y * 0.05);
    let sway = sway_a * 0.55 + sway_b * 0.30 + sway_c * 0.15;
    let amount = (sway * 0.78 + gust * 0.22) * wind_strength * vertex.wind_weight;

    var displaced = world_position;
    displaced.x += amount;
    displaced.z += amount * 0.62 + sway_b * 0.012 * vertex.wind_weight;

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
