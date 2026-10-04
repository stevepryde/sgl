//! Transparent: blended receivers as the surface, blended and additive
//! surfaces, mist and distortion.
pub(crate) mod effects;
pub(crate) mod heat;
pub(crate) mod mist;

use crate::shading::bind::{self, BlendedTrace};
use crate::view::frame::FrameContext;
use crate::view::pipelines::GeometryPass;
use crate::view::targets::{SharedTargets, attachment};

/// The beauty a transparent draw composes onto.
pub(crate) enum Beauty<'a> {
    /// The reflections' incident radiance, before they trace it, so
    /// reflections show the draws.
    Incident(&'a wgpu::TextureView),
    /// The shared composite, after reflections are composed, with what the
    /// screen-space method returned while one ran, which receivers that are
    /// the surface compose into their traced lobe. While FSR2 runs, these
    /// draws also write its reactive and transparency and composition
    /// masks, which start clear here.
    Composite {
        reflections: Option<&'a wgpu::TextureView>,
    },
}

/// Transparent: the receiver pass (`encode_receivers`), which draws the
/// blended receivers of screen-space reflections as the surface; then
/// blended surfaces, back to front, and additive glow and ground mist, drawn
/// (`encode`) into the reflections' incident radiance while they trace it
/// and onto the composite, each fogged from the frame's fog volume where it
/// lies, then heat distortion (`encode_heat`).
///
/// Reads: the camera's blended draw list with its lit group 0 and the
/// geometry pipelines, its unlit group 0 (with the fog volume), the opaque
/// depth (copied into the surface depth, and tested by every draw but the
/// receiver pass, never written), the surface depth and the screen-space
/// method's result (receivers that are the surface), the scene's transient
/// geometry (glow, heat and mist), the beauty it draws onto.
/// Writes: the surface depth, the receiver layer and the G-buffer's motion
/// (the receiver pass); that beauty in place; FSR2's masks while FSR2 runs;
/// the completed scene in place (heat) through its own snapshot of it.
/// Blended surfaces write no other depth, motion or G-buffer.
/// Honours: the receiver pass (the effective configuration's), atmosphere
/// (mist), heat distortion, FSR2 (its masks), the effects and atmosphere
/// diagnostics layers.
/// Timing groups: `receivers`, `blended` (blended surfaces, both draws),
/// `transparent` (glow and mist, both draws), `heat distortion`.
/// History: none; the receiver pass rebuilds the surface every frame it
/// runs.
pub(crate) struct Transparent {
    effects: effects::Effects,
    mist: mist::Mist,
    heat: heat::Heat,
    /// The depth soft glow reads.
    depth_group: wgpu::BindGroup,
    /// No screen-space result, which the blended draws bind while none
    /// composes: 1×1, zero.
    no_reflections: wgpu::TextureView,
    /// The blended group 3's `BlendedTrace` of the draw into the incident
    /// radiance, which composes nothing, and of the draw onto the composite.
    traces: [wgpu::Buffer; 2],
}

impl Transparent {
    pub fn new(
        device: &wgpu::Device,
        unlit: &wgpu::BindGroupLayout,
        targets: &SharedTargets,
    ) -> Self {
        let effects = effects::Effects::new(device, unlit);
        let trace = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size_of::<BlendedTrace>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            depth_group: effects.depth_group(device, &targets.depth),
            effects,
            mist: mist::Mist::new(device, unlit),
            heat: heat::Heat::new(device),
            no_reflections: crate::view::targets::target(
                device,
                "no screen-space reflections",
                [1, 1],
                crate::shading::gbuffer::COLOR,
            ),
            traces: [
                trace("blended incident trace"),
                trace("blended composite trace"),
            ],
        }
    }

    /// The Receivers step: when the effective configuration runs it and the
    /// camera's blended list holds receivers, copies the opaque depth into
    /// the surface depth and draws the list's receiver batches over it with
    /// the frame's jittered view, tested strictly nearer and written: each
    /// receiver's traced lobe into the receiver layer and its unjittered
    /// motion into the G-buffer's. Returns whether it drew, after which the
    /// surface is the surface targets'.
    pub fn encode_receivers(&self, ctx: &mut FrameContext<'_>) -> bool {
        let targets: &SharedTargets = ctx.targets;
        let Some(surface) = &targets.surface else {
            return false;
        };
        if !ctx.effective.receivers || !ctx.views.blended.holds_receivers(ctx.scene) {
            return false;
        }
        ctx.encoder.copy_texture_to_texture(
            targets.depth.texture().as_image_copy(),
            surface.depth.texture().as_image_copy(),
            targets.depth.texture().size(),
        );
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blended receivers"),
            color_attachments: &[
                attachment(&surface.receivers),
                loaded(attachment(&targets.motion), true),
            ],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &surface.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: ctx.timing.and_then(|t| t.render_pass("receivers")),
            ..Default::default()
        });
        pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
        ctx.views.blended.draw(
            ctx.scene,
            ctx.pipelines,
            &ctx.views.instances,
            &mut pass,
            GeometryPass::Receivers,
        );
        true
    }

    /// Rebinds the depth the glow reads.
    pub fn resize(&mut self, device: &wgpu::Device, targets: &SharedTargets) {
        self.depth_group = self.effects.depth_group(device, &targets.depth);
    }

    /// Blended surfaces, glow and mist onto `beauty`.
    pub fn encode(&self, ctx: &mut FrameContext<'_>, beauty: Beauty<'_>) {
        let targets: &SharedTargets = ctx.targets;
        // The draw's slot in `traces`, and the method's result it composes.
        let (beauty, fsr2_masks, (trace, reflections)) = match beauty {
            Beauty::Incident(view) => (view, None, (0, None)),
            Beauty::Composite { reflections } => (
                &targets.composite,
                ctx.effective.fsr2.then_some(&targets.fsr2_masks),
                (1, reflections),
            ),
        };
        // The first pass that writes FSR2's masks clears them.
        let blended = !ctx.views.blended.is_empty();
        if blended {
            let group = self.blended_group(ctx, &self.traces[trace], reflections);
            Self::encode_blended(ctx, beauty, fsr2_masks, &group);
        }
        let draw = ctx.effective.effects;
        if draw || (fsr2_masks.is_some() && !blended) {
            let mut color = attachment(beauty).unwrap();
            color.ops.load = wgpu::LoadOp::Load;
            let mut attachments = vec![Some(color)];
            attachments.extend(
                fsr2_masks
                    .into_iter()
                    .flatten()
                    .map(|mask| loaded(attachment(mask), blended)),
            );
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("additive effects"),
                color_attachments: &attachments,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: None,
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("transparent")),
                ..Default::default()
            });
            if draw {
                self.effects.draw(
                    &mut pass,
                    ctx.bindings.camera_unlit(),
                    &self.depth_group,
                    &ctx.scene.transient,
                    fsr2_masks.is_some(),
                );
            }
        }
        if ctx.effective.atmosphere {
            self.mist.encode(
                ctx.encoder,
                ctx.bindings.camera_unlit(),
                &ctx.scene.transient,
                beauty,
                &targets.depth,
                fsr2_masks,
                ctx.timing,
                "transparent",
            );
        }
    }

    /// A blended draw's group 3, its `BlendedTrace` written to `trace`:
    /// the screen-space method's `reflections` with its cutoff and fade
    /// where the draw composes them, else no result, and the frame's surface
    /// depth.
    fn blended_group(
        &self,
        ctx: &FrameContext<'_>,
        trace: &wgpu::Buffer,
        reflections: Option<&wgpu::TextureView>,
    ) -> wgpu::BindGroup {
        let traced = ctx.effective.screen_space.zip(reflections);
        let values = traced.map_or_else(BlendedTrace::default, |(ssr, _)| BlendedTrace {
            cutoff: ssr.cutoff,
            fade: ssr.fade,
            padding: [0.; 2],
        });
        ctx.queue
            .write_buffer(trace, 0, bytemuck::bytes_of(&values));
        let reflections = traced.map_or(&self.no_reflections, |(_, view)| view);
        ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blended reflections"),
            layout: &ctx.bindings.blended,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: bind::blended::REFLECTIONS,
                    resource: wgpu::BindingResource::TextureView(reflections),
                },
                wgpu::BindGroupEntry {
                    binding: bind::blended::SURFACE_DEPTH,
                    resource: wgpu::BindingResource::TextureView(ctx.surface.depth),
                },
                wgpu::BindGroupEntry {
                    binding: bind::blended::TRACE,
                    resource: trace.as_entire_binding(),
                },
            ],
        })
    }

    /// The camera's blended surfaces onto `beauty`, back to front, tested
    /// against the opaque depth without writing it, with their group 3
    /// `group`, and, with `fsr2_masks`, onto FSR2's masks, which this pass
    /// clears.
    fn encode_blended(
        ctx: &mut FrameContext<'_>,
        beauty: &wgpu::TextureView,
        fsr2_masks: Option<&[wgpu::TextureView; 2]>,
        group: &wgpu::BindGroup,
    ) {
        let color = loaded(attachment(beauty), true);
        let mut attachments = vec![color];
        attachments.extend(fsr2_masks.into_iter().flatten().map(attachment));
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blended surfaces"),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &ctx.targets.depth,
                depth_ops: None,
                stencil_ops: None,
            }),
            timestamp_writes: ctx.timing.and_then(|t| t.render_pass("blended")),
            ..Default::default()
        });
        pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
        pass.set_bind_group(3, group, &[]);
        let kind = GeometryPass::Blended {
            fsr2_masks: fsr2_masks.is_some(),
        };
        ctx.views.blended.draw(
            ctx.scene,
            ctx.pipelines,
            &ctx.views.instances,
            &mut pass,
            kind,
        );
    }

    /// Heat distortion of the completed scene `color` in place.
    pub fn encode_heat(&mut self, ctx: &mut FrameContext<'_>, color: &wgpu::TextureView) {
        self.heat.encode(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &ctx.scene.transient,
            &ctx.values.view.view_projection,
            color,
            &ctx.targets.depth,
            ctx.timing,
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod blended_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod effects_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod receiver_tests;

/// `target`, keeping what it holds when `load`.
fn loaded(
    mut target: Option<wgpu::RenderPassColorAttachment<'_>>,
    load: bool,
) -> Option<wgpu::RenderPassColorAttachment<'_>> {
    if load {
        target.as_mut().unwrap().ops.load = wgpu::LoadOp::Load;
    }
    target
}
