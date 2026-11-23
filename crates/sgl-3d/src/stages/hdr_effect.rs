//! HDR effect: the game's function (`Scene::add_hdr_effect`) at each pixel
//! of the antialiased HDR frame, in one full-screen fragment pass at the
//! scene size into one target the stage owns, which the stages after read
//! in place of the antialiased frame (the HDR effect contract,
//! `shading::hdr_effect`). The game supplies the effect and its parameters
//! for each view's frame (`FrameInput::hdr_effect`); the stage owns the
//! pass, its inputs, its target and its place.
//!
//! Placement: after antialiasing and before depth of field, motion blur,
//! bloom and tone mapping, first in the resolved HDR chain: what it models,
//! a medium or a tint between the camera and the surface, changes the light
//! the lens then focuses, and its depth is the surface's at the pixel,
//! which a focused or blurred pixel no longer has. three.js r187 draws a
//! game's node over its scene pass's colour and depth before its output
//! transform (`RenderPipeline.js` 190–207); Bevy 9d12036 lets a game's
//! post-process join `Core3dSystems::PostProcess`, where bloom, depth of
//! field and the tone map run, and order itself there
//! (`examples/shader_advanced/custom_post_processing.rs`, unordered).
//!
//! Reads: the antialiased HDR frame (`Completed`) at the scene size, each
//! texel at most `shading::SCENE_COLOR`'s bound, the surface's depth at the
//! render size (the Surface contract), the frame's exposure multiplier
//! (`stages::exposure`), the camera, the time and the effect's parameters.
//! Writes: the effect's frame, Rgba16Float at the scene size.
//! Honours: `FrameInput::hdr_effect` (the effective configuration's
//! `hdr_effect`). A view that never applies one holds nothing of it; a
//! frame without one runs no pass and holds no target or bind group, and
//! the view keeps its layouts, two uniform buffers and sampler and the
//! pipelines of the effects the scene still holds.
//! Timing group: `HDR effect`.
use crate::content::hdr_effect::HdrEffect;
use crate::content::identity::HdrEffectId;
use crate::shading::gbuffer::COLOR;
use crate::shading::hdr_effect::{FRAGMENT_ENTRY, HdrEffectFrameUniform, program};
use crate::view::compile::Compiled;
use crate::view::frame::FrameContext;
use crate::view::targets::target;
use std::collections::HashMap;

/// The parameter buffer's size: the most any effect's block takes.
const PARAMS_BYTES: u64 = crate::content::shader::HDR_EFFECT_PARAMS_MAX_BYTES as u64;

/// The stage: nothing until a frame applies an effect.
#[derive(Default)]
pub(crate) struct HdrEffectStage {
    pass: Option<Pass>,
    /// Each effect's pipeline, a slot an effect the scene holds: a frame
    /// creates a missing one in place, and a planning pass queues it as a
    /// compile job.
    pipelines: HashMap<HdrEffectId, Compiled<wgpu::RenderPipeline>>,
}

/// What the stage makes at its first frame with an effect, from the device
/// alone, and its target while effects run.
struct Pass {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    frame: wgpu::Buffer,
    params: wgpu::Buffer,
    sampler: wgpu::Sampler,
    /// The effect and parameters the parameter buffer holds.
    written: Option<HdrEffect>,
    /// The effect's frame, at the frame's size.
    output: Option<wgpu::TextureView>,
    /// The output was made or let go since `take_replaced`.
    replaced: bool,
    /// The group for each frame view (TAA alternates two), surface depth
    /// and exposure view bound, until the target or the views are replaced.
    groups: Vec<([wgpu::TextureView; 3], wgpu::BindGroup)>,
}

impl Pass {
    fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let texture = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("HDR effect"),
            entries: &[
                entry(0, uniform),
                entry(1, uniform),
                entry(
                    2,
                    texture(wgpu::TextureSampleType::Float { filterable: true }),
                ),
                entry(3, texture(wgpu::TextureSampleType::Depth)),
                entry(
                    4,
                    texture(wgpu::TextureSampleType::Float { filterable: false }),
                ),
                entry(
                    5,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("HDR effect"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let buffer = |label, size| {
            crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            )
        };
        Self {
            layout,
            pipeline_layout,
            frame: buffer(
                "HDR effect frame",
                size_of::<HdrEffectFrameUniform>() as u64,
            ),
            params: buffer("HDR effect parameters", PARAMS_BYTES),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("HDR effect linear clamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            written: None,
            output: None,
            replaced: false,
            groups: Vec::new(),
        }
    }

    /// Lets go of the output, which the stages after may have bound.
    fn release(&mut self) {
        if self.output.take().is_some() {
            self.replaced = true;
            self.groups.clear();
        }
    }
}

impl HdrEffectStage {
    /// The effect's frame, while the stage holds one.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn output(&self) -> Option<&wgpu::TextureView> {
        self.pass.as_ref()?.output.as_ref()
    }

    /// Drops the bind groups, which hold frame views and shared targets
    /// that a resize or a new antialiasing context replaces.
    pub fn forget_inputs(&mut self) {
        if let Some(pass) = &mut self.pass {
            pass.groups.clear();
        }
    }

    /// Whether the effect's frame was made or let go since the last call:
    /// the stages that read it then drop the groups that bind the old one.
    pub fn take_replaced(&mut self) -> bool {
        self.pass
            .as_mut()
            .is_some_and(|pass| std::mem::take(&mut pass.replaced))
    }

    /// Applies the frame's effect to `frame`, the antialiased HDR frame,
    /// with `exposure`, the frame's exposure multiplier: the effect's frame,
    /// or `None` when it did not run (no effect, or a planning pass that
    /// queued the effect's pipeline), in which case a frame without an
    /// effect releases the target and keeps the pipelines of the effects
    /// the scene still holds.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        frame: &wgpu::TextureView,
        exposure: &wgpu::TextureView,
    ) -> Option<&wgpu::TextureView> {
        if !self.pipelines.is_empty() {
            let effects = &ctx.scene.hdr_effects;
            self.pipelines.retain(|&id, _| effects.get(id).is_ok());
        }
        let applied = ctx
            .scene
            .applied_hdr_effect(ctx.input.hdr_effect)
            .filter(|_| ctx.effective.hdr_effect);
        let Some((effect, module)) = applied else {
            if let Some(pass) = &mut self.pass {
                pass.release();
            }
            return None;
        };
        let pass = self.pass.get_or_insert_with(|| Pass::new(ctx.device));
        let pipeline = self.pipelines.entry(effect.effect()).or_default().get(
            ctx.compiler,
            "HDR effect",
            || {
                let device = ctx.device.clone();
                let layout = pass.pipeline_layout.clone();
                let source = program(&module.source);
                let label = module.label.clone();
                move || create_pipeline(&device, &layout, &source, &label)
            },
        )?;
        pass.dispatch(ctx, effect, pipeline, frame, exposure);
        pass.output.as_ref()
    }
}

/// The effect's pipeline from its composed `source` over `layout`.
fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    source: &str,
    label: &str,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some(crate::shading::FULLSCREEN_VS_ENTRY),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(FRAGMENT_ENTRY),
            compilation_options: Default::default(),
            targets: &[Some(COLOR.into())],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl Pass {
    /// The pass of `pipeline` over `frame` with the surface depth and
    /// `exposure`, writing the frame's values, and `effect`'s parameters
    /// where they changed.
    fn dispatch(
        &mut self,
        ctx: &mut FrameContext<'_>,
        effect: HdrEffect,
        pipeline: &wgpu::RenderPipeline,
        frame: &wgpu::TextureView,
        exposure: &wgpu::TextureView,
    ) {
        let size = frame.texture().size();
        let camera = ctx.input.camera;
        let uniform = HdrEffectFrameUniform {
            projection: camera.projection.to_cols_array_2d(),
            view_from_clip: camera.projection.inverse().to_cols_array_2d(),
            world_from_view: camera.view.inverse().to_cols_array_2d(),
            eye: camera.eye.to_array(),
            time: ctx.input.elapsed_seconds as f32,
            size: [size.width as f32, size.height as f32],
            phase: crate::shading::material::animation_phase(ctx.input.elapsed_seconds),
            _padding: 0.,
        };
        crate::counters::write_buffer(ctx.queue, &self.frame, 0, bytemuck::bytes_of(&uniform));
        if self.written != Some(effect) {
            // A uniform block's size is a multiple of 16 bytes, which the
            // copy needs; its bytes past the parameters are padding.
            let mut block = effect.parameters().to_vec();
            block.resize(block.len().next_multiple_of(16).max(16), 0);
            crate::counters::write_buffer(ctx.queue, &self.params, 0, &block);
            self.written = Some(effect);
        }
        if self
            .output
            .as_ref()
            .is_none_or(|output| output.texture().size() != size)
        {
            self.output = Some(target(
                ctx.device,
                "HDR effect frame",
                [size.width, size.height],
                COLOR,
            ));
            self.replaced = true;
            self.groups.clear();
        }
        let group = self.group(ctx.device, [frame, ctx.surface.depth, exposure]);
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("HDR effect"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.output.as_ref().unwrap(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            timestamp_writes: ctx.timing.and_then(|t| t.render_pass("HDR effect")),
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The group for `views`, the frame, the surface depth (the opaque
    /// depth, or the receivers') and the exposure, made once while they are
    /// those bound.
    fn group(&mut self, device: &wgpu::Device, views: [&wgpu::TextureView; 3]) -> wgpu::BindGroup {
        if let Some((_, group)) = self
            .groups
            .iter()
            .find(|(bound, _)| bound.iter().zip(views).all(|(a, b)| a == b))
        {
            return group.clone();
        }
        let [frame, depth, exposure] = views;
        let view = wgpu::BindingResource::TextureView;
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("HDR effect"),
            layout: &self.layout,
            entries: &[
                entry(0, self.frame.as_entire_binding()),
                entry(1, self.params.as_entire_binding()),
                entry(2, view(frame)),
                entry(3, view(depth)),
                entry(4, view(exposure)),
                entry(5, wgpu::BindingResource::Sampler(&self.sampler)),
            ],
        });
        self.groups.push((views.map(Clone::clone), group.clone()));
        group
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
