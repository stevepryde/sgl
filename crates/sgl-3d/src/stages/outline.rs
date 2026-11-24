//! Outline: a line around a selection's instances (`FrameInput::outline`,
//! `Scene::add_selection`) over the view's finished output, after post and
//! before the game's HUD. The game chooses which instances, when, the
//! width and the colours; the stage owns how the line is made.
//!
//! Three passes, while a selection is outlined. The mask: only the selected
//! instances, through the CPU draw-list builder (`Population::Selection`)
//! and the indexed casters' pipelines (`GeometryPass::LocalShadow`),
//! depth-only, unjittered, at the output size: each at this frame's
//! deformation, a masked material's cut-out texels discarded, as the
//! camera culls and LOD-selects its blended surfaces. The rows: each
//! pixel's nearest covered mask pixel along its row within the reach, and
//! whether it is in sight of the opaque depth. The composite: the nearest
//! over the pixel's column, covering it by `width + 1 − d` of the colour of
//! that pixel's visibility, blended over the output. The rows and the
//! composite are scissored to the selection's projected bounds grown by
//! the reach.
//!
//! Follows three.js r187's `OutlineNode` (`examples/jsm/tsl/display/`
//! `OutlineNode.js`, MIT): a mask of only the selected objects, a depth
//! comparison into visible and hidden, an edge outside the mask composited
//! with a visible and a hidden colour. Departs (D-79): the frame's own
//! opaque depth instead of a pre-pass of the other objects, an exact
//! distance for a width in output pixels instead of a half-resolution edge
//! detect and blur, and its place after the tone map, where the colour is
//! the game's and the width keeps to output pixels at any render or scene
//! scale. No code is copied.
//!
//! Reads: the selection's instances, the camera's unjittered view, the
//! opaque depth at the render size (`SharedTargets::depth`) and the frame
//! uniform. Writes: the view's output, blended. Holds: the mask
//! (Depth32Float) and the rows (R8Uint) at the output size, and two
//! uniform buffers, only while a frame draws an outline; its programs from
//! its first such frame. No history.
//! Timing group: `outline`.
use crate::content::identity::InstanceId;
use crate::content::outline::Outline;
use crate::scene::static_edits::posed_bounds;
use crate::shading::uniforms::{FrameValues, ViewUniform};
use crate::view::ViewData;
use crate::view::compile::Compiled;
use crate::view::culling::clip_intersects;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::frame::FrameContext;
use crate::view::lod::LodSelector;
use crate::view::pipelines::GeometryPass;
use crate::view::population::Population;
use crate::view::targets::{Sizes, target};
use crate::{FrameInput, Scene};
use glam::{Mat4, Vec3};

pub(crate) static OUTLINE: crate::shading::Module = crate::shading::Module {
    name: "outline",
    source: sgl_post_fx::embed_wgsl!("outline.wgsl"),
    deps: &[&crate::shading::FULLSCREEN_VS, &crate::shading::SRGB],
};
/// The row pass's entry point.
pub(crate) const ROWS_ENTRY: &str = "outline_rows";
/// The composite's entry point.
pub(crate) const COMPOSITE_ENTRY: &str = "outline_composite";

/// The mask's format: the casters' depth.
const MASK: wgpu::TextureFormat = crate::shading::gbuffer::DEPTH;
/// The rows' format: a distance and a visibility bit a pixel.
const ROWS: wgpu::TextureFormat = wgpu::TextureFormat::R8Uint;
/// A row value with no covered pixel within the reach (`OUTLINE_NONE`).
const ROW_NONE: u32 = 127;
/// The searches' cap (`OUTLINE_REACH_MAX`, AR-12): the reach of the widest
/// outline.
const REACH_MAX: u32 = Outline::MAX_WIDTH as u32;
const TIMING: &str = "outline";

/// `Outline` in `outline.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct OutlineUniform {
    visible: [f32; 4],
    hidden: [f32; 4],
    width: f32,
    reach: u32,
    depth_scale: [f32; 2],
    depth_shift: [f32; 2],
    padding: [f32; 2],
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "outline",
        "Outline",
        OutlineUniform,
        [
            visible,
            hidden,
            width,
            reach,
            depth_scale,
            depth_shift,
            padding
        ]
    )]
}

/// The constants `outline.wgsl` twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 2] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        Constant::new("outline", "OUTLINE_REACH_MAX", U32(REACH_MAX)),
        Constant::new("outline", "OUTLINE_NONE", U32(ROW_NONE)),
    ]
}

impl OutlineUniform {
    /// `outline` drawn into an output of `sizes`, over opaque depth drawn
    /// with `camera`'s jitter.
    fn new(outline: &Outline, sizes: Sizes, camera: &ViewUniform) -> Self {
        let color = |color: [f32; 4]| color.map(|c| if c.is_nan() { 0. } else { c.clamp(0., 1.) });
        let width = outline.drawn_width();
        Self {
            visible: color(outline.visible_color),
            hidden: color(outline.hidden_color),
            width,
            reach: (width.ceil() as u32).min(REACH_MAX),
            depth_scale: [0, 1].map(|axis| sizes.render[axis] as f32 / sizes.output[axis] as f32),
            // Where the jitter moved the depth's samples, in render pixels:
            // the raster offsets clip xy by 2 · jitter · w (vertex.wgsl), so
            // a texel's centre sees the unjittered picture this far back,
            // y flipped from NDC to pixels.
            depth_shift: [
                camera.jitter[0] * sizes.render[0] as f32,
                -camera.jitter[1] * sizes.render[1] as f32,
            ],
            padding: [0.; 2],
        }
    }
}

/// The stage: nothing until a frame outlines a selection, then its state,
/// made there rather than with the view, whose construction runs deep on a
/// browser's debug stack (#893).
#[derive(Default)]
pub(crate) struct OutlineStage(Option<Box<Outlining>>);

/// The stage's state from its first outlined frame.
#[derive(Default)]
struct Outlining {
    /// The mask's draws, built by `prepare` for the frame.
    list: DrawList,
    /// The frame's outline, from `prepare` until `encode`.
    planned: Option<Planned>,
    /// Targets, buffers and groups while frames outline a selection.
    held: Option<Held>,
    /// The programs for the view's output, from its first outlined frame:
    /// a frame creates them in place, and a planning pass queues them as a
    /// compile job.
    programs: Compiled<Programs>,
}

/// One frame's outline as `prepare` found it.
struct Planned {
    uniform: OutlineUniform,
    /// The camera's view, unjittered, at the output size.
    view: ViewUniform,
    /// The output pixels the outline may cover, `[x, y, width, height]`.
    scissor: [u32; 4],
}

/// What the stage holds while a selection is outlined.
struct Held {
    size: [u32; 2],
    view: wgpu::Buffer,
    params: wgpu::Buffer,
    mask: wgpu::TextureView,
    rows: wgpu::TextureView,
    /// The mask pass's group 0 and the frame buffer it binds.
    shadow: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    /// The rows' and composite's groups and the opaque depth they bind.
    groups: Option<(wgpu::TextureView, [wgpu::BindGroup; 2])>,
}

impl Held {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
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
            size,
            view: buffer("outline view", size_of::<ViewUniform>() as u64),
            params: buffer("outline", size_of::<OutlineUniform>() as u64),
            mask: target(device, "outline mask", size, MASK),
            rows: target(device, "outline rows", size, ROWS),
            shadow: None,
            groups: None,
        }
    }
}

/// The stage's programs for one output format and transfer.
struct Programs {
    rows_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    rows: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
}

impl Programs {
    /// The programs for an output of `format` that stores the sRGB encoding
    /// written to it where `encodes_srgb`.
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, encodes_srgb: bool) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let uniform = entry(
            0,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
        );
        let texture = |binding, sample_type| {
            entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let rows_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("outline rows"),
            entries: &[
                uniform,
                texture(1, wgpu::TextureSampleType::Depth),
                texture(2, wgpu::TextureSampleType::Depth),
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("outline composite"),
            entries: &[uniform, texture(3, wgpu::TextureSampleType::Uint)],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("outline"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&OUTLINE]).into()),
        });
        let pipeline = |label,
                        layout: &wgpu::BindGroupLayout,
                        fragment,
                        constants: &[(&str, f64)],
                        target: wgpu::ColorTargetState| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(crate::shading::FULLSCREEN_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants,
                        ..Default::default()
                    },
                    targets: &[Some(target)],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let rows = pipeline("outline rows", &rows_layout, ROWS_ENTRY, &[], ROWS.into());
        // Straight alpha over the finished picture.
        let composite = pipeline(
            "outline composite",
            &composite_layout,
            COMPOSITE_ENTRY,
            &[("OUTLINE_ENCODES_SRGB", f64::from(u8::from(encodes_srgb)))],
            wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        Self {
            rows_layout,
            composite_layout,
            rows,
            composite,
        }
    }
}

impl OutlineStage {
    /// `Outlining::prepare`, making the stage's state at the first frame
    /// that names an outline.
    pub fn prepare(
        &mut self,
        scene: &Scene,
        input: &FrameInput,
        values: &FrameValues,
        sizing: (Sizes, bool),
        drawn: &mut DrawInstances,
    ) {
        if input.outline.is_none() && self.0.is_none() {
            return;
        }
        self.0
            .get_or_insert_with(Box::default)
            .prepare(scene, input, values, sizing, drawn);
    }

    /// `Outlining::encode`, once a frame has named an outline.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        output: &wgpu::TextureView,
        format: (wgpu::TextureFormat, bool),
    ) {
        if let Some(outlining) = &mut self.0 {
            outlining.encode(ctx, output, format);
        }
    }

    /// Whether the stage holds its targets.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn holds_targets(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|outlining| outlining.held.is_some())
    }
}

impl Outlining {
    /// Builds the frame's mask list into `drawn`, before the frame's draw
    /// instances are uploaded, when `input` outlines a selection `scene`
    /// holds: from the camera's view in `values` unjittered at `sizes`'
    /// output size, the camera's LOD at the render size unless `full_detail`
    /// (as the camera's blended surfaces keep it).
    pub fn prepare(
        &mut self,
        scene: &Scene,
        input: &FrameInput,
        values: &FrameValues,
        (sizes, full_detail): (Sizes, bool),
        drawn: &mut DrawInstances,
    ) {
        self.planned = None;
        let Some((outline, instances)) = scene.drawn_outline(input.outline) else {
            return;
        };
        let camera = values.view;
        let stable = Mat4::from_cols_array_2d(&camera.stable_view_projection);
        let view = ViewUniform {
            view_projection: camera.stable_view_projection,
            inverse_view_projection: stable.inverse().to_cols_array_2d(),
            mip_bias: 0.,
            jitter: [0.; 2],
            viewport: sizes.output.map(|size| size as f32),
            ..camera
        };
        let lod = if full_detail {
            None
        } else {
            LodSelector::new(input.camera.view, input.camera.projection, sizes.render)
        };
        self.list.build(
            drawn,
            scene,
            &ViewData::camera(view),
            Some(values.frame.visibility_mask),
            Population::Selection { instances, lod },
        );
        let uniform = OutlineUniform::new(&outline, sizes, &camera);
        // Nothing selected is drawn: the frame outlines nothing.
        let Some(scissor) = (!self.list.is_empty())
            .then(|| scissor(scene, instances, stable, sizes.output, uniform.reach))
            .flatten()
        else {
            return;
        };
        self.planned = Some(Planned {
            uniform,
            view,
            scissor,
        });
    }

    /// Outlines the planned selection over `output`, an output of
    /// `format` that stores the sRGB encoding written to it where
    /// `encodes_srgb`; a frame with none, or whose output the device
    /// cannot blend into (`Rgba32Float` without `FLOAT32_BLENDABLE`),
    /// releases what the stage holds but its programs.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        output: &wgpu::TextureView,
        (format, encodes_srgb): (wgpu::TextureFormat, bool),
    ) {
        let blendable = format
            .guaranteed_format_features(ctx.device.features())
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE);
        let Some(planned) = self.planned.take().filter(|_| blendable) else {
            if self.held.take().is_some() {
                self.list = DrawList::default();
            }
            return;
        };
        let size = ctx.sizes.output;
        let held = match &mut self.held {
            Some(held) if held.size == size => held,
            held => held.insert(Held::new(ctx.device, size)),
        };
        let scissor = planned.scissor;
        crate::counters::write_buffer(ctx.queue, &held.view, 0, bytemuck::bytes_of(&planned.view));
        crate::counters::write_buffer(
            ctx.queue,
            &held.params,
            0,
            bytemuck::bytes_of(&planned.uniform),
        );
        let frame = &ctx.bindings.frame;
        if held.shadow.as_ref().is_none_or(|(bound, _)| bound != frame) {
            let group = ctx.bindings.shadow_group(ctx.device, &held.view, frame);
            held.shadow = Some((frame.clone(), group));
        }
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("outline mask"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &held.mask,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|timing| timing.render_pass(TIMING)),
                ..Default::default()
            });
            pass.set_bind_group(0, &held.shadow.as_ref().unwrap().1, &[]);
            self.list.draw(
                ctx.scene,
                ctx.pipelines,
                &ctx.views.instances,
                &mut pass,
                GeometryPass::LocalShadow,
            );
        }
        let Some(programs) = self.programs.get(ctx.compiler, "outline", || {
            let device = ctx.device.clone();
            move || Programs::new(&device, format, encodes_srgb)
        }) else {
            // A planning pass: the job is queued and the passes left out.
            return;
        };
        let depth = &ctx.targets.depth;
        if held.groups.as_ref().is_none_or(|(bound, _)| bound != depth) {
            let group = |layout, entries: &[wgpu::BindGroupEntry<'_>]| {
                ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("outline"),
                    layout,
                    entries,
                })
            };
            let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
            let params = || entry(0, held.params.as_entire_binding());
            let view = wgpu::BindingResource::TextureView;
            let groups = [
                group(
                    &programs.rows_layout,
                    &[params(), entry(1, view(&held.mask)), entry(2, view(depth))],
                ),
                group(
                    &programs.composite_layout,
                    &[params(), entry(3, view(&held.rows))],
                ),
            ];
            held.groups = Some((depth.clone(), groups));
        }
        let [rows_group, composite_group] = &held.groups.as_ref().unwrap().1;
        let [x, y, width, height] = scissor;
        let pass = |encoder: &mut wgpu::CommandEncoder, label, target, load, pipeline, group| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                timestamp_writes: ctx.timing.and_then(|timing| timing.render_pass(TIMING)),
                ..Default::default()
            });
            pass.set_scissor_rect(x, y, width, height);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        };
        let none = wgpu::Color {
            r: f64::from(ROW_NONE),
            ..wgpu::Color::TRANSPARENT
        };
        pass(
            ctx.encoder,
            "outline rows",
            &held.rows,
            wgpu::LoadOp::Clear(none),
            &programs.rows,
            rows_group,
        );
        pass(
            ctx.encoder,
            "outline composite",
            output,
            wgpu::LoadOp::Load,
            &programs.composite,
            composite_group,
        );
    }
}

/// The output pixels `[x, y, width, height]` of an output of `size` that
/// the outline of `instances` of `scene` may cover, seen through
/// `clip_from_world` and grown by `reach` (and a pixel for the mask's
/// raster): the projected bounds of their visible instances that reach the
/// clip volume, the whole output where one of those reaches behind the
/// camera, None where none is on it.
fn scissor(
    scene: &Scene,
    instances: &[InstanceId],
    clip_from_world: Mat4,
    size: [u32; 2],
    reach: u32,
) -> Option<[u32; 4]> {
    let mut min = glam::Vec2::INFINITY;
    let mut max = glam::Vec2::NEG_INFINITY;
    for &id in instances {
        let Some(instance) = scene.instances.slots.get(id) else {
            continue;
        };
        if !instance.state.visible {
            continue;
        }
        let model = scene.drawn_model(instance.state.model);
        let bounds = scene.caster_bounds(instance, model);
        if !clip_intersects(bounds, clip_from_world * instance.state.pose) {
            continue;
        }
        let [low, high] = posed_bounds(bounds, instance.state.pose);
        for corner in 0..8 {
            let pick = |bit, axis: usize| {
                if corner & bit == 0 {
                    low[axis]
                } else {
                    high[axis]
                }
            };
            let world = Vec3::new(pick(1, 0), pick(2, 1), pick(4, 2));
            let clip = clip_from_world * world.extend(1.);
            if clip.w <= f32::EPSILON {
                return Some([0, 0, size[0], size[1]]);
            }
            let ndc = clip.truncate().truncate() / clip.w;
            let pixel = glam::Vec2::new(
                (ndc.x * 0.5 + 0.5) * size[0] as f32,
                (0.5 - ndc.y * 0.5) * size[1] as f32,
            );
            min = min.min(pixel);
            max = max.max(pixel);
        }
    }
    let grow = (reach + 1) as f32;
    let low = (min - grow).floor().max(glam::Vec2::ZERO);
    let high = (max + grow)
        .ceil()
        .min(glam::Vec2::new(size[0] as f32, size[1] as f32));
    (low.x < high.x && low.y < high.y).then(|| {
        let (x, y) = (low.x as u32, low.y as u32);
        [x, y, high.x as u32 - x, high.y as u32 - y]
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
