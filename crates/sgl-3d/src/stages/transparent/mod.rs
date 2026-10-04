//! Transparent: blended and additive surfaces, mist and distortion.
pub(crate) mod effects;
pub(crate) mod heat;
pub(crate) mod mist;

use crate::view::frame::FrameContext;
use crate::view::pipelines::GeometryPass;
use crate::view::targets::{SharedTargets, attachment};

/// The beauty a transparent draw composes onto.
pub(crate) enum Beauty<'a> {
    /// The reflections' incident radiance, before they trace it, so
    /// reflections show the draws.
    Incident(&'a wgpu::TextureView),
    /// The shared composite, after reflections are composed. While FSR2
    /// runs, these draws also write its reactive and transparency and
    /// composition masks, which start clear here.
    Composite,
}

/// Transparent: blended surfaces, back to front, then additive glow and
/// ground mist, drawn (`encode`) into the reflections' incident radiance
/// while they trace it and onto the composite, each fogged from the frame's
/// fog volume where it lies, then heat distortion (`encode_heat`).
///
/// Reads: the camera's blended draw list with its lit group 0 and the
/// geometry pipelines, its unlit group 0 (with the fog volume), depth
/// (tested, never written), the scene's transient geometry (glow, heat and
/// mist), the beauty it draws onto.
/// Writes: that beauty in place; FSR2's masks while FSR2 runs; the completed
/// scene in place (heat) through its own snapshot of it. Blended surfaces
/// write no depth, motion or G-buffer.
/// Honours: atmosphere (mist), heat distortion, FSR2 (its masks), the
/// effects and atmosphere diagnostics layers.
/// Timing groups: `blended` (blended surfaces, both draws), `transparent`
/// (glow and mist, both draws), `heat distortion`.
pub(crate) struct Transparent {
    effects: effects::Effects,
    mist: mist::Mist,
    heat: heat::Heat,
    /// The depth soft glow reads.
    depth_group: wgpu::BindGroup,
}

impl Transparent {
    pub fn new(
        device: &wgpu::Device,
        unlit: &wgpu::BindGroupLayout,
        targets: &SharedTargets,
    ) -> Self {
        let effects = effects::Effects::new(device, unlit);
        Self {
            depth_group: effects.depth_group(device, &targets.depth),
            effects,
            mist: mist::Mist::new(device, unlit),
            heat: heat::Heat::new(device),
        }
    }

    /// Rebinds the depth the glow reads.
    pub fn resize(&mut self, device: &wgpu::Device, targets: &SharedTargets) {
        self.depth_group = self.effects.depth_group(device, &targets.depth);
    }

    /// Blended surfaces, glow and mist onto `beauty`.
    pub fn encode(&self, ctx: &mut FrameContext<'_>, beauty: Beauty<'_>) {
        let targets: &SharedTargets = ctx.targets;
        let (beauty, fsr2_masks) = match beauty {
            Beauty::Incident(view) => (view, None),
            Beauty::Composite => (
                &targets.composite,
                ctx.effective.fsr2.then_some(&targets.fsr2_masks),
            ),
        };
        // The first pass that writes FSR2's masks clears them.
        let blended = !ctx.views.blended.is_empty();
        if blended {
            Self::encode_blended(ctx, beauty, fsr2_masks);
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

    /// The camera's blended surfaces onto `beauty`, back to front, tested
    /// against the opaque depth without writing it, and, with
    /// `fsr2_masks`, onto FSR2's masks, which this pass clears.
    fn encode_blended(
        ctx: &mut FrameContext<'_>,
        beauty: &wgpu::TextureView,
        fsr2_masks: Option<&[wgpu::TextureView; 2]>,
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
