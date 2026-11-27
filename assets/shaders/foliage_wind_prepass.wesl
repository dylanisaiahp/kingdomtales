// Prepass vertex shader for wind-deformed foliage.
//
// Keep the wind math in `apply_wind` identical to `foliage_wind.wesl` so the
// forward pass and depth/normal prepass agree on geometry. The prepass uses
// Bevy's prepass vertex interface (not forward_io).

import bevy_pbr::{
    prepass::io::VertexOutput,
    render::{
        mesh_bindings::mesh,
        mesh_functions,
        morph,
        morph::{morph_normal, morph_position, morph_tangent},
        skinning,
        view_transformations::position_world_to_clip,
    },
};

struct FoliageWindMaterial {
    params: vec4<f32>, // x = wind strength, y = elapsed time
};

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> wind: FoliageWindMaterial;

// --- Prepass vertex interface, with the extra WindWeight attribute at 14. ---

struct UncompressedVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,

    @if(VERTEX_UVS_A)
    @location(1) uv: vec2<f32>,

    @if(VERTEX_UVS_B)
    @location(2) uv_b: vec2<f32>,

    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_NORMALS)
    @location(3) normal: vec3<f32>,
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS)
    @location(4) tangent: vec4<f32>,

    @if(SKINNED)
    @location(5) joint_indices: vec4<u32>,
    @if(SKINNED)
    @location(6) joint_weights: vec4<f32>,

    @if(VERTEX_COLORS)
    @location(7) color: vec4<f32>,

    @location(14) wind_weight: f32,

    @if(MORPH_TARGETS)
    @builtin(vertex_index) index: u32,
};

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @if(VERTEX_POSITIONS_COMPRESSED)
    @location(0) compressed_position: vec4<f32>,
    @else
    @location(0) position: vec3<f32>,
    @if(VERTEX_UVS_A && VERTEX_UVS_A_COMPRESSED)
    @location(1) compressed_uv: vec2<f32>,
    @if(VERTEX_UVS_A && !VERTEX_UVS_A_COMPRESSED)
    @location(1) uv: vec2<f32>,
    @if(VERTEX_UVS_B && VERTEX_UVS_B_COMPRESSED)
    @location(2) compressed_uv_b: vec2<f32>,
    @if(VERTEX_UVS_B && !VERTEX_UVS_B_COMPRESSED)
    @location(2) uv_b: vec2<f32>,

    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_NORMALS && VERTEX_NORMALS_COMPRESSED)
    @location(3) compressed_normal: vec2<f32>,
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_NORMALS && !VERTEX_NORMALS_COMPRESSED)
    @location(3) normal: vec3<f32>,
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS && VERTEX_TANGENTS_COMPRESSED)
    @location(4) compressed_tangent: vec2<f32>,
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS && !VERTEX_TANGENTS_COMPRESSED)
    @location(4) tangent: vec4<f32>,

    @if(SKINNED)
    @location(5) joint_indices: vec4<u32>,
    @if(SKINNED)
    @location(6) joint_weights: vec4<f32>,

    @if(VERTEX_COLORS)
    @location(7) color: vec4<f32>,

    @location(14) wind_weight: f32,

    @if(MORPH_TARGETS)
    @builtin(vertex_index) index: u32,
}

fn decompress_vertex(vertex_in: Vertex, instance_index: u32) -> UncompressedVertex {
    let mesh_metadata = mesh_functions::get_metadata(instance_index);
    var uncompressed_vertex: UncompressedVertex;
    uncompressed_vertex.instance_index = instance_index;
    uncompressed_vertex.wind_weight = vertex_in.wind_weight;
@if(VERTEX_POSITIONS_COMPRESSED)
    uncompressed_vertex.position = bevy_render::utils::decompress_vertex_position(vertex_in.compressed_position, mesh_metadata.aabb_center, mesh_metadata.aabb_half_extents);
@else
    uncompressed_vertex.position = vertex_in.position;
@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_NORMALS && VERTEX_NORMALS_COMPRESSED)
    uncompressed_vertex.normal = bevy_render::utils::decompress_vertex_normal(vertex_in.compressed_normal);
@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_NORMALS && !VERTEX_NORMALS_COMPRESSED)
    uncompressed_vertex.normal = vertex_in.normal;
@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS && VERTEX_TANGENTS_COMPRESSED)
    uncompressed_vertex.tangent = bevy_render::utils::decompress_vertex_tangent(vertex_in.compressed_tangent);
@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS && !VERTEX_TANGENTS_COMPRESSED)
    uncompressed_vertex.tangent = vertex_in.tangent;
@if(VERTEX_UVS_A && VERTEX_UVS_A_COMPRESSED) {
    let uv_min_and_extents_a = mesh_metadata.uv_channels_min_and_extents[0];
    uncompressed_vertex.uv = bevy_render::utils::decompress_vertex_uv(vertex_in.compressed_uv, uv_min_and_extents_a);
}
@if(VERTEX_UVS_A && !VERTEX_UVS_A_COMPRESSED)
    uncompressed_vertex.uv = vertex_in.uv;
@if(VERTEX_UVS_B && VERTEX_UVS_B_COMPRESSED) {
    let uv_min_and_extents_b = mesh_metadata.uv_channels_min_and_extents[1];
    uncompressed_vertex.uv_b = bevy_render::utils::decompress_vertex_uv(vertex_in.compressed_uv_b, uv_min_and_extents_b);
}
@if(VERTEX_UVS_B && !VERTEX_UVS_B_COMPRESSED)
    uncompressed_vertex.uv_b = vertex_in.uv_b;
@if(VERTEX_COLORS)
    uncompressed_vertex.color = vertex_in.color;
@if(SKINNED) {
    uncompressed_vertex.joint_indices = vertex_in.joint_indices;
    uncompressed_vertex.joint_weights = vertex_in.joint_weights;
}
@if(MORPH_TARGETS)
    uncompressed_vertex.index = vertex_in.index;
    return uncompressed_vertex;
}

@if(MORPH_TARGETS)
fn morph_vertex(vertex_in: UncompressedVertex, instance_index: u32) -> UncompressedVertex {
    var vertex = vertex_in;
    let first_vertex = mesh[instance_index].first_vertex_index;
    let vertex_index = vertex.index - first_vertex;
    let morph_descriptor_index = mesh[instance_index].morph_descriptor_index;

    let weight_count = morph::layer_count(morph_descriptor_index);
    for (var i: u32 = 0u; i < weight_count; i++) {
        let weight = morph::weight_at(i, morph_descriptor_index);
        if weight == 0.0 {
            continue;
        }
        vertex.position += weight * morph_position(vertex_index, i, morph_descriptor_index);
@if(VERTEX_NORMALS)
        vertex.normal += weight * morph_normal(vertex_index, i, morph_descriptor_index);
@if(VERTEX_TANGENTS)
        vertex.tangent += vec4<f32>(weight * morph_tangent(vertex_index, i, morph_descriptor_index), 0.0);
    }
    return vertex;
}

@if(MORPH_TARGETS)
fn morph_prev_vertex(vertex_in: UncompressedVertex, instance_index: u32) -> UncompressedVertex {
    var vertex = vertex_in;
    let first_vertex = mesh[instance_index].first_vertex_index;
    let vertex_index = vertex.index - first_vertex;
    let morph_descriptor_index = mesh[instance_index].morph_descriptor_index;
    let weight_count = morph::layer_count(morph_descriptor_index);
    for (var i: u32 = 0u; i < weight_count; i++) {
        let weight = morph::prev_weight_at(i, morph_descriptor_index);
        if weight == 0.0 {
            continue;
        }
        vertex.position += weight * morph_position(vertex_index, i, morph_descriptor_index);
    }
    return vertex;
}

// Identical to the displacement in `foliage_wind.wesl`. The sway is a small
// translation-like displacement; normals are deliberately left unchanged in
// both shaders, so forward lighting and prepass normals remain consistent.
fn apply_wind(world_position: vec3<f32>, wind_weight: f32) -> vec3<f32> {
    let wind_strength = wind.params.x;
    let wind_speed = 1.15;
    let t = wind.params.y * wind_speed;
    let wp = world_position.xz;

    let sway_a = sin(t + wp.x * 0.55 + wp.y * 0.35);
    let sway_b = sin(t * 1.37 + wp.x * 0.21 - wp.y * 0.46 + 1.7);
    let sway_c = sin(t * 0.73 - wp.x * 0.18 + wp.y * 0.63 + 0.4);
    let gust = sin(t * 0.23 + wp.x * 0.07 + wp.y * 0.05);
    let sway = sway_a * 0.55 + sway_b * 0.30 + sway_c * 0.15;
    let amount = (sway * 0.78 + gust * 0.22) * wind_strength * wind_weight;

    var displaced = world_position;
    displaced.x += amount;
    displaced.z += amount * 0.62 + sway_b * 0.012 * wind_weight;
    return displaced;
}

@vertex
fn vertex(vertex_no_morph: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let instance_index = vertex_no_morph.instance_index;
    let uncompressed_vertex_no_morph = decompress_vertex(vertex_no_morph, instance_index);
@if(MORPH_TARGETS)
    var vertex = morph_vertex(uncompressed_vertex_no_morph, instance_index);
@else
    var vertex = uncompressed_vertex_no_morph;

    let mesh_world_from_local = mesh_functions::get_world_from_local(instance_index);

@if(SKINNED)
    var world_from_local = skinning::skin_model(
        vertex.joint_indices,
        vertex.joint_weights,
        instance_index
    );
@else
    var world_from_local = mesh_world_from_local;

    let undisplaced_world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0)
    );
    let displaced_world_position = apply_wind(
        undisplaced_world_position.xyz,
        vertex.wind_weight
    );
    out.world_position = vec4<f32>(displaced_world_position, 1.0);
    out.position = position_world_to_clip(displaced_world_position);
@if(UNCLIPPED_DEPTH_ORTHO_EMULATION) {
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0);
}

@if(VERTEX_UVS_A)
    out.uv = vertex.uv;

@if(VERTEX_UVS_B)
    out.uv_b = vertex.uv_b;

@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS) {
@if(VERTEX_NORMALS && SKINNED)
    out.world_normal = skinning::skin_normals(world_from_local, vertex.normal);
@elif(VERTEX_NORMALS)
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        instance_index
    );

@if(VERTEX_TANGENTS)
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        vertex.tangent,
        instance_index
    );
}

@if(VERTEX_COLORS)
    out.color = vertex.color;

@if(MOTION_VECTOR_PREPASS) {
@if(MORPH_TARGETS && HAS_PREVIOUS_MORPH)
    let prev_vertex = morph_prev_vertex(uncompressed_vertex_no_morph, instance_index);
@elif(MORPH_TARGETS)
    let prev_vertex = uncompressed_vertex_no_morph;
@else
    let prev_vertex = uncompressed_vertex_no_morph;

@if(SKINNED && HAS_PREVIOUS_SKIN)
    let prev_model = skinning::skin_prev_model(
        prev_vertex.joint_indices,
        prev_vertex.joint_weights,
        instance_index
    );
@elif(SKINNED)
    let prev_model = mesh_functions::get_previous_world_from_local(instance_index);
@else
    let prev_model = mesh_functions::get_previous_world_from_local(instance_index);

    out.previous_world_position = mesh_functions::mesh_position_local_to_world(
        prev_model,
        vec4<f32>(prev_vertex.position, 1.0)
    );
}

@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = instance_index;

@if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        instance_index,
        mesh_world_from_local[3]
    );

    return out;
}
