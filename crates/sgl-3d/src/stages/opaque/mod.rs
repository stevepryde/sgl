//! Opaque: the sky, the G-buffer and lit colour (direct, baked, emitted and
//! ambient light) over one depth, then ambient occlusion over the G-buffer's
//! depth and normals. Where the device has the colour attachments for it, the
//! G-buffer and lighting are one fused pass; otherwise a G-buffer pass and a
//! lighting pass at its depth write the same targets. Lit colour keeps its
//! ambient diffuse whole and records it apart, for source completion to
//! occlude by this stage's visibility.
//!
//! The renderer encodes it in the stage order's named parts: the G-buffer
//! (`encode_gbuffer`), per phase of the camera's draw list, which leaves
//! the G-buffer complete; the lighting (`encode_lighting`), the sky and the
//! lighting pass at its depth over every phase's sets; then ambient
//! occlusion (`encode_ambient_occlusion`). The fused form's pass is its
//! G-buffer part, and its lighting part encodes nothing. While occlusion
//! culling runs, the stage takes its two-pass form, and the renderer encodes
//! the cull stage's late phase between the early and late G-buffer passes,
//! the late one loading the early one's targets.
//!
//! Reads: the camera view and its draw list, group 0's camera lit and unlit
//! groups, the geometry pipelines; for a probe capture face (`encode_capture`),
//! the capture's draw list and the face's groups.
//! Writes: the shared G-buffer, colour, ambient diffuse, source identity and
//! depth; its own ambient occlusion targets.
//! Honours: ambient occlusion (quality and radius), the fused form (device
//! capability, diagnostics), the diagnostics layers compiled into the
//! geometry pipelines.
//! Timing groups: fused `sky`, `opaque geometry + lighting`; split
//! `geometry`, `geometry late` (occlusion culling), `sky`,
//! `opaque lighting`; then `ambient occlusion`.
pub(crate) mod ambient_occlusion;
pub(crate) mod sky;

use crate::counters::Moment;
use crate::view::draw_list::gpu::Phase;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::frame::FrameContext;
use crate::view::pipelines::{GeometryPass, GeometryPipelines};
use crate::view::targets::{CaptureFace, attachment};

pub(crate) struct Opaque {
    sky: sky::Sky,
    ambient_occlusion: Option<ambient_occlusion::AmbientOcclusion>,
    /// Whether ambient occlusion ran this frame.
    ambient_occlusion_ran: bool,
}

impl Opaque {
    pub fn new(device: &wgpu::Device, unlit: &wgpu::BindGroupLayout) -> Self {
        Self {
            sky: sky::Sky::new(device, unlit),
            ambient_occlusion: None,
            ambient_occlusion_ran: false,
        }
    }

    /// The G-buffer part of `phase`: the sky and the fused pass, which
    /// lights the surfaces as it writes the G-buffer, or the G-buffer pass
    /// over the phase's sets and, after the frame's last, where the device
    /// cannot write anisotropy in it, the anisotropy fallback at `Equal`
    /// over every phase's. The fused form has the early phase alone.
    pub fn encode_gbuffer(&mut self, ctx: &mut FrameContext<'_>, phase: Phase) {
        if ctx.effective.fused {
            if phase == Phase::Early {
                self.encode_fused(ctx);
            }
        } else {
            Self::encode_gbuffer_pass(ctx, phase);
        }
    }

    /// The lighting part: in the two-pass form, the sky and the lighting
    /// pass at the G-buffer's depth; nothing in the fused form, whose
    /// G-buffer part lit the surfaces.
    pub fn encode_lighting(&mut self, ctx: &mut FrameContext<'_>) {
        if !ctx.effective.fused {
            self.encode_lighting_pass(ctx);
        }
    }

    /// Ambient occlusion over the G-buffer's depth and normals, where the
    /// settings run it.
    pub fn encode_ambient_occlusion(&mut self, ctx: &mut FrameContext<'_>) {
        self.ambient_occlusion_ran = ctx.effective.ambient_occlusion.is_some();
        if let Some(settings) = ctx.effective.ambient_occlusion {
            let targets = ctx.targets;
            self.ambient_occlusion
                .get_or_insert_with(|| ambient_occlusion::AmbientOcclusion::new(ctx.device))
                .encode(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    &targets.depth,
                    &targets.normal,
                    &ctx.views.camera.view.uniform,
                    settings.quality,
                    settings.radius,
                    ctx.timing,
                );
        }
    }

    /// This frame's ambient visibility, when ambient occlusion ran.
    pub fn visibility(&self) -> Option<&wgpu::TextureView> {
        self.ambient_occlusion
            .as_ref()
            .filter(|_| self.ambient_occlusion_ran)
            .and_then(|ambient_occlusion| ambient_occlusion.output())
    }

    fn encode_fused(&mut self, ctx: &mut FrameContext<'_>) {
        let targets = ctx.targets;
        {
            let sky = [attachment(&targets.color), attachment(&targets.motion)];
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fused scene sky and depth clear"),
                color_attachments: &sky,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("sky")),
                ..Default::default()
            });
            self.sky.draw(&mut pass, ctx.bindings.camera_unlit());
        }
        let views = [
            &targets.normal,
            &targets.material,
            &targets.motion,
            &targets.f0,
            &targets.color,
            &targets.ambient,
            &targets.source_id,
            &targets.anisotropy,
        ];
        // Motion and color (indices 2 and 4) keep the sky drawn above.
        let attachments = std::array::from_fn::<_, 8, _>(|index| {
            let mut target = attachment(views[index]);
            if matches!(index, 2 | 4) {
                target.as_mut().unwrap().ops.load = wgpu::LoadOp::Load;
            }
            target
        });
        let started = Moment::now();
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fused opaque material and lighting"),
                color_attachments: &attachments,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ctx
                    .timing
                    .and_then(|t| t.render_pass("opaque geometry + lighting")),
                ..Default::default()
            });
            pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
            ctx.views
                .camera
                .list
                .draw(ctx.scene, ctx.pipelines, &mut pass, GeometryPass::Fused);
        }
        ctx.views.camera.recorded_since(started);
    }

    /// The G-buffer pass over the camera's sets of `phase`: the early one
    /// clearing the targets, the late one loading them. After the frame's
    /// last (the late one, or the early one where the list culls no late
    /// phase), where the device cannot write anisotropy in it, the
    /// anisotropy fallback at `Equal` over every phase's sets.
    fn encode_gbuffer_pass(ctx: &mut FrameContext<'_>, phase: Phase) {
        let targets = ctx.targets;
        let anisotropy_inline = ctx.pipelines.anisotropy_inline;
        let late = phase == Phase::Late;
        let last = late || !ctx.views.camera.list.late();
        // The G-buffer and depth.
        let started = Moment::now();
        {
            let colors = [
                &targets.normal,
                &targets.material,
                &targets.motion,
                &targets.f0,
                &targets.anisotropy,
            ];
            // The late pass keeps what the early one wrote.
            let attachments = colors.map(|view| {
                let mut target = attachment(view);
                if late {
                    target.as_mut().unwrap().ops.load = wgpu::LoadOp::Load;
                }
                target
            });
            let (label, group) = if late {
                ("stable scene geometry, late set", "geometry late")
            } else {
                ("stable scene geometry", "geometry")
            };
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass(group)),
                color_attachments: &attachments[..if anisotropy_inline { 5 } else { 4 }],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: if late {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(0.0)
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
            ctx.views.camera.list.draw_phase(
                ctx.scene,
                ctx.pipelines,
                &mut pass,
                GeometryPass::GBuffer,
                phase,
            );
        }
        ctx.views.camera.recorded_since(started);
        if !anisotropy_inline && last {
            let started = Moment::now();
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stable anisotropy attachment fallback"),
                color_attachments: &[attachment(&targets.anisotropy)],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("geometry")),
                ..Default::default()
            });
            pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
            ctx.views.camera.list.draw(
                ctx.scene,
                ctx.pipelines,
                &mut pass,
                GeometryPass::GBufferAnisotropy,
            );
            drop(pass);
            ctx.views.camera.recorded_since(started);
        }
    }

    /// The sky, then the lighting pass at the G-buffer's depth.
    fn encode_lighting_pass(&mut self, ctx: &mut FrameContext<'_>) {
        let targets = ctx.targets;
        // The sky writes color and motion; opaque geometry then writes color,
        // ambient diffuse and identity together once, and its motion over the
        // sky's.
        {
            let mut motion = attachment(&targets.motion);
            motion.as_mut().unwrap().ops.load = wgpu::LoadOp::Load;
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("source sky background"),
                color_attachments: &[attachment(&targets.color), motion],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("sky")),
                ..Default::default()
            });
            self.sky.draw(&mut pass, ctx.bindings.camera_unlit());
        }
        let colors = [
            &targets.color,
            &targets.ambient,
            &targets.motion,
            &targets.source_id,
        ];
        // Color and motion (indices 0 and 2) keep the sky drawn above.
        let attachments = std::array::from_fn::<_, 4, _>(|index| {
            let mut target = attachment(colors[index]);
            if matches!(index, 0 | 2) {
                target.as_mut().unwrap().ops.load = wgpu::LoadOp::Load;
            }
            target
        });
        let started = Moment::now();
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("opaque HDR with stable depth ownership"),
            timestamp_writes: ctx.timing.and_then(|t| t.render_pass("opaque lighting")),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
        ctx.views
            .camera
            .list
            .draw(ctx.scene, ctx.pipelines, &mut pass, GeometryPass::Lighting);
        drop(pass);
        ctx.views.camera.recorded_since(started);
    }

    /// Probe capture face `face`: the sky under `unlit`, then `list`'s lit
    /// colour and motion with depth under `lit`, from the capture's draw
    /// instances.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_capture(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &crate::Scene,
        pipelines: &GeometryPipelines,
        list: (&DrawList, &DrawInstances),
        face: &CaptureFace,
        unlit: &wgpu::BindGroup,
        lit: &wgpu::BindGroup,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("static instances and sky capture"),
            color_attachments: &[attachment(&face.color), attachment(&face.motion)],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &face.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        self.encode_forward(&mut pass, scene, pipelines, list, unlit, lit);
    }

    /// The sky under `unlit`, in the caller's pass, as a probe capture face
    /// draws it first.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn draw_sky(&self, pass: &mut wgpu::RenderPass<'_>, unlit: &wgpu::BindGroup) {
        self.sky.draw(pass, unlit);
    }

    /// A probe capture face: the sky, then `list`'s lit colour and motion
    /// with depth (`GeometryPass::Forward`) from `drawn`, in the caller's
    /// pass.
    fn encode_forward(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &crate::Scene,
        pipelines: &GeometryPipelines,
        (list, drawn): (&DrawList, &DrawInstances),
        unlit: &wgpu::BindGroup,
        lit: &wgpu::BindGroup,
    ) {
        self.sky.draw(pass, unlit);
        pass.set_bind_group(0, lit, &[]);
        list.draw(scene, pipelines, drawn, pass, GeometryPass::Forward);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
