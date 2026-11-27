import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;

struct LightShaftsSettings {
    sun_screen_pos: vec4<f32>, // xy in UV, z = enabled/visibility, w = intensity
    params: vec4<f32>,         // x=density, y=decay, z=weight, w=num_samples
    params2: vec4<f32>,        // x=exposure, y=sun_height, z=daylight, w=aspect
    color: vec4<f32>,          // rgb=color, w=detail mode (0 simple, 1 detailed)
};

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> settings: LightShaftsSettings;
@if(DETAILED_SHAFTS && MULTISAMPLED_DEPTH) @group(0) @binding(3) var depth_texture: texture_depth_multisampled_2d;
@if(DETAILED_SHAFTS && !MULTISAMPLED_DEPTH) @group(0) @binding(3) var depth_texture: texture_depth_2d;

fn depth_is_sky(uv: vec2<f32>) -> bool {
@if(DETAILED_SHAFTS) {
    let dims = vec2<f32>(textureDimensions(depth_texture));
    let coord = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - vec2<f32>(1.0)));
    // Reverse-Z clears sky depth to 0.0; the farthest rendered geometry is well
    // above 1e-7, so this only matches cleared sky instead of distant terrain.
    @if(MULTISAMPLED_DEPTH) {
        let sc = textureNumSamples(depth_texture);
        for (var i = 0u; i < 8u; i = i + 1u) {
            if (i >= sc) { break; }
            let d = textureLoad(depth_texture, coord, i32(i));
            if (d > 1e-7) { return false; }
        }
        return true;
    }
    @if(!MULTISAMPLED_DEPTH) {
        let d = textureLoad(depth_texture, coord, 0);
        return d <= 1e-7;
    }
}
@if(!DETAILED_SHAFTS) {
    return true;
}
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let original = textureSample(screen_texture, screen_sampler, in.uv);
    let enabled = settings.sun_screen_pos.z;
    if (enabled < 0.5) {
        return original;
    }

    let sun_uv = settings.sun_screen_pos.xy;
    let intensity = settings.sun_screen_pos.w;
    let samples = i32(clamp(settings.params.w, 8.0, 64.0));
    let density = settings.params.x;
    let decay = settings.params.y;
    let weight = settings.params.z;
    let exposure = settings.params2.x;

    var delta = in.uv - sun_uv;
    delta.x *= settings.params2.w;
    let dist = length(delta);
    // Concentrated near sun, but still visible. Avoid full-screen overlay.
    let fade = 1.0 - smoothstep(0.10, 0.85, dist);
    if (fade <= 0.001) {
        return original;
    }

    let step_uv = (in.uv - sun_uv) * (density / f32(samples));
    var uv = in.uv;
    var illumination_decay = 1.0;
    var accum = 0.0;

    // No screen-space dither: it stays locked to pixels while the camera moves,
    // re-phasing every tap against geometry and producing shimmer. A small
    // amount of banding is preferable to temporal flicker.
    for (var i = 0; i < 64; i = i + 1) {
        if (i >= samples) { break; }
        uv -= step_uv;
        if (uv.x >= -0.05 && uv.x <= 1.05 && uv.y >= -0.05 && uv.y <= 1.05) {
            let sample_uv = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
            let mask = select(0.0, 1.0, depth_is_sky(sample_uv));
            accum += mask * illumination_decay * weight;
        }
        // Decay advances per tap, not per visible tap. Otherwise a tap crossing
        // the viewport edge shifts the weight of every later sample and flickers.
        illumination_decay *= decay;
    }

    let shafts = accum * exposure * fade * intensity;
    // Visible but restrained; cap prevents full-screen washout. Keep the result
    // HDR and let tonemapping handle the display transform. Clamping the sum to
    // [0, 1] here made tiny shaft changes produce whole-frame brightness steps.
    let shaft_color = settings.color.rgb * min(shafts, 0.28);
    return vec4<f32>(original.rgb + shaft_color, original.a);
}
