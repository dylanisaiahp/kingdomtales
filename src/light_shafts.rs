//! Screen-space sun shafts (light shafts).
//!
//! Default mode is a stable simple fullscreen pass (no depth occlusion).
//! Optional detailed mode uses `ViewPrepassTextures` depth for terrain/opaque/alpha-masked
//! occlusion. Alpha-blended foliage is not represented in depth prepass by Bevy 0.20.1.
//!
//! Underground cameras do not render shafts: the pass uses the shared
//! underground/sky signal so cave mouths keep daylight shafts while enclosed
//! spaces fade them out.
//!
//! Runs in Core3d PostProcess, explicitly before tonemapping.

use std::collections::{HashMap, HashSet};

use bevy::camera::CameraUpdateSystems;
use bevy::core_pipeline::Core3d;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::core_pipeline::upscaling;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{
    sampler, texture_2d, texture_depth_2d, texture_depth_2d_multisampled, uniform_buffer,
};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::view::ViewTarget;
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderSystems};
use bevy::transform::TransformSystems;

use crate::day_night::DayNightCycle;
use crate::player::PlayerCamera;
use crate::settings::{GameSettings, UndergroundLighting};

pub struct LightShaftsPlugin;

impl Plugin for LightShaftsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightShaftsConfig>()
            .register_type::<LightShaftsConfig>()
            .add_plugins(ExtractComponentPlugin::<LightShaftsSettings>::default())
            // Camera transforms and projection are refreshed in PostUpdate; reading
            // them in Update made the sun position one frame stale during motion.
            .add_systems(
                PostUpdate,
                update_light_shafts_settings
                    .after(TransformSystems::Propagate)
                    .after(CameraUpdateSystems),
            );

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_gpu_resource::<LightShaftsPipeline>();
        render_app.add_systems(Render, prepare_light_shafts.in_set(RenderSystems::Prepare));
        render_app.add_systems(
            Core3d,
            light_shafts
                .in_set(bevy::core_pipeline::Core3dSystems::PostProcess)
                // Bloom writes into the same HDR main texture; run after it so
                // shafts see the bloomed image before tonemapping flips the target.
                .after(bevy::post_process::bloom::bloom)
                .before(tonemapping)
                .before(upscaling::upscaling),
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
pub enum LightShaftsMode {
    Off,
    #[default]
    Simple,
    /// Depth-prepass occlusion. May show artifacts around transparent effects.
    Detailed,
}

impl LightShaftsMode {
    pub(crate) fn is_detailed(self) -> bool {
        matches!(self, Self::Detailed)
    }

    pub(crate) fn renders_shafts(self) -> bool {
        matches!(self, Self::Simple | Self::Detailed)
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Off => Self::Simple,
            Self::Simple => Self::Detailed,
            Self::Detailed => Self::Off,
        }
    }
}

/// Tunable Light Shafts parameters. The selected mode is owned by
/// `GameSettings.light_shafts_mode`; this resource only holds render parameters.
#[derive(Resource, Clone, Debug, Reflect)]
#[reflect(Resource)]
pub struct LightShaftsConfig {
    pub intensity: f32,
    pub decay: f32,
    pub density: f32,
    pub weight: f32,
    pub samples: u32,
}

impl Default for LightShaftsConfig {
    fn default() -> Self {
        Self {
            intensity: 0.55,
            decay: 0.965,
            density: 0.84,
            weight: 0.014,
            samples: 24,
        }
    }
}

#[derive(Component, Clone, Copy, ShaderType, Default)]
pub struct LightShaftsSettings {
    pub(crate) sun_screen_pos: Vec4, // xy uv, z enabled, w intensity
    pub(crate) params: Vec4,         // density, decay, weight, num_samples
    pub(crate) params2: Vec4,        // exposure, sun_height, daylight, aspect
    pub(crate) color: Vec4,          // rgb, detailed mode (0 simple, 1 detailed)
}

impl SyncComponent<RenderApp> for LightShaftsSettings {
    type Target = Self;
}

impl ExtractComponent<RenderApp> for LightShaftsSettings {
    type QueryData = &'static Self;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(
        item: bevy::ecs::query::QueryItem<'_, '_, Self::QueryData>,
    ) -> Option<Self::Out> {
        Some(*item)
    }
}

#[derive(Clone, Copy)]
struct LightShaftsPipelineIds {
    no_depth: CachedRenderPipelineId,
    detailed: CachedRenderPipelineId,
    detailed_msaa: CachedRenderPipelineId,
}

#[derive(Resource)]
struct LightShaftsPipeline {
    layout_no_depth: BindGroupLayout,
    layout_detailed: BindGroupLayout,
    layout_detailed_msaa: BindGroupLayout,
    layout_desc_no_depth: BindGroupLayoutDescriptor,
    layout_desc_detailed: BindGroupLayoutDescriptor,
    layout_desc_detailed_msaa: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen_shader: FullscreenShader,
    shader: Handle<Shader>,
    // One pipeline set per view color format. The player camera is HDR while
    // the map camera renders to an offscreen RGBA8 target; sharing one format
    // slot would let the wrong camera determine the player's pipeline.
    pipelines: HashMap<TextureFormat, LightShaftsPipelineIds>,
}

impl FromWorld for LightShaftsPipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();
        let color_sampler = (
            texture_2d(TextureSampleType::Float { filterable: true }),
            sampler(SamplerBindingType::Filtering),
            uniform_buffer::<LightShaftsSettings>(false),
        );
        let layout_entries_no_depth =
            BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, color_sampler);
        let layout_entries_detailed = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<LightShaftsSettings>(false),
                texture_depth_2d(),
            ),
        );
        let layout_entries_detailed_msaa = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<LightShaftsSettings>(false),
                texture_depth_2d_multisampled(),
            ),
        );
        let layout_desc_no_depth = BindGroupLayoutDescriptor::new(
            "light_shafts_layout_no_depth",
            &layout_entries_no_depth,
        );
        let layout_desc_detailed = BindGroupLayoutDescriptor::new(
            "light_shafts_layout_detailed",
            &layout_entries_detailed,
        );
        let layout_desc_detailed_msaa = BindGroupLayoutDescriptor::new(
            "light_shafts_layout_detailed_msaa",
            &layout_entries_detailed_msaa,
        );
        let layout_no_depth = render_device
            .create_bind_group_layout("light_shafts_layout_no_depth", &layout_entries_no_depth);
        let layout_detailed = render_device
            .create_bind_group_layout("light_shafts_layout_detailed", &layout_entries_detailed);
        let layout_detailed_msaa = render_device.create_bind_group_layout(
            "light_shafts_layout_detailed_msaa",
            &layout_entries_detailed_msaa,
        );
        let sampler = render_device.create_sampler(&SamplerDescriptor::default());
        let shader = world
            .resource::<AssetServer>()
            .load("shaders/light_shafts.wesl");
        let fullscreen_shader = FullscreenShader::from_world(world);
        Self {
            layout_no_depth,
            layout_detailed,
            layout_detailed_msaa,
            layout_desc_no_depth,
            layout_desc_detailed,
            layout_desc_detailed_msaa,
            sampler,
            fullscreen_shader,
            shader,
            pipelines: HashMap::new(),
        }
    }
}

fn make_pipeline(
    pipeline: &LightShaftsPipeline,
    pipeline_cache: &PipelineCache,
    format: TextureFormat,
    layout: BindGroupLayoutDescriptor,
    label: &'static str,
    defs: Vec<bevy::shader::ShaderDefVal>,
) -> CachedRenderPipelineId {
    pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some(label.into()),
        layout: vec![layout],
        immediate_size: 0,
        vertex: pipeline.fullscreen_shader.to_vertex_state(),
        fragment: Some(FragmentState {
            shader: pipeline.shader.clone(),
            shader_defs: defs,
            entry_point: Some("fragment".into()),
            targets: vec![Some(ColorTargetState {
                format,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
            constants: vec![],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        zero_initialize_workgroup_memory: false,
    })
}

fn prepare_light_shafts(
    mut pipeline: ResMut<LightShaftsPipeline>,
    pipeline_cache: Res<PipelineCache>,
    views: Query<&ViewTarget>,
) {
    let formats: HashSet<TextureFormat> = views
        .iter()
        .map(|view_target| view_target.main_texture_format())
        .collect();

    for format in formats {
        if pipeline.pipelines.contains_key(&format) {
            continue;
        }

        let no_depth = make_pipeline(
            &pipeline,
            &pipeline_cache,
            format,
            pipeline.layout_desc_no_depth.clone(),
            "light_shafts_no_depth",
            vec![],
        );
        let detailed = make_pipeline(
            &pipeline,
            &pipeline_cache,
            format,
            pipeline.layout_desc_detailed.clone(),
            "light_shafts_detailed",
            vec![bevy::shader::ShaderDefVal::Bool(
                "DETAILED_SHAFTS".into(),
                true,
            )],
        );
        let detailed_msaa = make_pipeline(
            &pipeline,
            &pipeline_cache,
            format,
            pipeline.layout_desc_detailed_msaa.clone(),
            "light_shafts_detailed_msaa",
            vec![
                bevy::shader::ShaderDefVal::Bool("DETAILED_SHAFTS".into(), true),
                bevy::shader::ShaderDefVal::Bool("MULTISAMPLED_DEPTH".into(), true),
            ],
        );
        pipeline.pipelines.insert(
            format,
            LightShaftsPipelineIds {
                no_depth,
                detailed,
                detailed_msaa,
            },
        );
    }
}

fn light_shafts(
    view: ViewQuery<(
        &ViewTarget,
        &LightShaftsSettings,
        Option<&ViewPrepassTextures>,
    )>,
    pipeline_res: Res<LightShaftsPipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (view_target, settings, prepass) = view.into_inner();
    if settings.sun_screen_pos.z < 0.5 {
        return;
    }

    let detailed = settings.color.w > 0.5;
    let depth_view = prepass.and_then(|p| p.depth.as_ref());
    let depth_tex = depth_view.and_then(|d| d.depth_stencil_views().depth_only_view());
    let msaa = depth_view.is_some_and(|d| d.texture.texture.sample_count() > 1);

    let Some(ids) = pipeline_res
        .pipelines
        .get(&view_target.main_texture_format())
    else {
        return;
    };

    let (pipeline_id, layout, use_depth) = if detailed {
        if let Some(depth) = depth_tex {
            if msaa {
                (
                    Some(ids.detailed_msaa),
                    &pipeline_res.layout_detailed_msaa,
                    Some(depth),
                )
            } else {
                (
                    Some(ids.detailed),
                    &pipeline_res.layout_detailed,
                    Some(depth),
                )
            }
        } else {
            // Fallback to simple if detailed is requested but depth is unavailable.
            (Some(ids.no_depth), &pipeline_res.layout_no_depth, None)
        }
    } else {
        (Some(ids.no_depth), &pipeline_res.layout_no_depth, None)
    };

    let Some(pipeline_id) = pipeline_id else {
        return;
    };
    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_id) else {
        return;
    };

    let post_process = view_target.post_process_write();
    let settings_buffer = ctx
        .render_device()
        .create_buffer_with_data(&BufferInitDescriptor {
            label: Some("light_shafts_settings"),
            contents: unsafe {
                std::slice::from_raw_parts(
                    settings as *const LightShaftsSettings as *const u8,
                    std::mem::size_of::<LightShaftsSettings>(),
                )
            },
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        });

    let bind_group = if let Some(depth) = use_depth {
        ctx.render_device().create_bind_group(
            "light_shafts_bind_group_detailed",
            layout,
            &BindGroupEntries::sequential((
                post_process.source,
                &pipeline_res.sampler,
                settings_buffer.as_entire_binding(),
                depth,
            )),
        )
    } else {
        ctx.render_device().create_bind_group(
            "light_shafts_bind_group",
            layout,
            &BindGroupEntries::sequential((
                post_process.source,
                &pipeline_res.sampler,
                settings_buffer.as_entire_binding(),
            )),
        )
    };

    let mut pass = ctx
        .command_encoder()
        .begin_render_pass(&RenderPassDescriptor {
            label: Some("light_shafts_pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: post_process.destination,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

fn update_light_shafts_settings(
    mut commands: Commands,
    config: Res<LightShaftsConfig>,
    settings: Res<GameSettings>,
    underground: Res<UndergroundLighting>,
    cycle: Res<DayNightCycle>,
    camera_q: Query<(Entity, &Camera, &GlobalTransform), With<PlayerCamera>>,
) {
    let effective_mode = settings.light_shafts_mode;

    // `underground_target()` already forces the cave blend to 0 whenever an
    // upward probe sees sky, so smoothed surface visibility is the only gate
    // needed. The raw `sky_visible()` boolean popped shafts off when one probe
    // briefly hit geometry while the player moved.
    let outdoor_visibility = underground.surface_visibility();

    for (entity, camera, transform) in &camera_q {
        let sun_dir = (-cycle.sun_direction()).normalize_or_zero();
        let sun_height = sun_dir.y;
        let daylight = cycle.daylight_factor();
        let mut enabled = 0.0;
        let mut uv = Vec2::new(-1.0, -1.0);
        let mut intensity = 0.0;

        // The authoritative camera resolver supplies DepthPrepass and forces MSAA
        // Off while Detailed is active; render selection falls back to Simple if
        // depth is unexpectedly absent.
        let detailed_requested = effective_mode.is_detailed();
        let detailed_mode = if detailed_requested { 1.0 } else { 0.0 };
        let mode_enabled = effective_mode.renders_shafts();

        let should_compute =
            mode_enabled && outdoor_visibility > 0.0 && sun_height > -0.02 && daylight > 0.01;

        if should_compute {
            let world_pos = transform.translation() + sun_dir * 10_000.0;
            if let Some(ndc) = camera.world_to_ndc(transform, world_pos)
                && ndc.z > 0.0
                && ndc.x >= -1.2
                && ndc.x <= 1.2
                && ndc.y >= -1.2
                && ndc.y <= 1.2
            {
                uv = Vec2::new(ndc.x * 0.5 + 0.5, 1.0 - (ndc.y * 0.5 + 0.5));
                let edge_dist = uv.x.min(1.0 - uv.x).min(uv.y).min(1.0 - uv.y).max(0.0);
                let edge_fade = (edge_dist / 0.08).clamp(0.0, 1.0);
                let height_fade = ((sun_height + 0.02) / 0.12).clamp(0.0, 1.0);
                intensity = config.intensity
                    * daylight.clamp(0.0, 1.0)
                    * edge_fade
                    * height_fade
                    * outdoor_visibility;
                // Keep the pass active while the sun is in view. `intensity`
                // already fades smoothly to zero; a binary threshold here made
                // shafts pop on/off as movement nudged it across the cutoff.
                enabled = 1.0;
            }
        }
        let aspect = camera
            .logical_viewport_size()
            .map(|s| s.x / s.y.max(1.0))
            .unwrap_or(16.0 / 9.0);
        commands.entity(entity).insert(LightShaftsSettings {
            sun_screen_pos: Vec4::new(uv.x, uv.y, enabled, intensity),
            params: Vec4::new(
                config.density,
                config.decay,
                config.weight,
                config.samples.clamp(8, 64) as f32,
            ),
            params2: Vec4::new(0.55, sun_height, daylight, aspect),
            color: Vec4::new(1.0, 0.97, 0.90, detailed_mode),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_cycle_exposes_detailed_mode() {
        assert_eq!(LightShaftsMode::Simple.next(), LightShaftsMode::Detailed);
        assert_eq!(LightShaftsMode::Detailed.next(), LightShaftsMode::Off);
        assert_eq!(LightShaftsMode::Off.next(), LightShaftsMode::Simple);
    }

    #[test]
    fn mode_predicates_keep_simple_and_detailed_enabled() {
        assert!(LightShaftsMode::Simple.renders_shafts());
        assert!(LightShaftsMode::Detailed.renders_shafts());
        assert!(!LightShaftsMode::Off.renders_shafts());
        assert!(LightShaftsMode::Detailed.is_detailed());
        assert!(!LightShaftsMode::Simple.is_detailed());
    }
}
