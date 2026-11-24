//! Opaque: the sky, the G-buffer and lit colour (direct, baked, emitted and
//! ambient light) over one depth, then ambient occlusion over the G-buffer's
//! depth and normals, then, while it runs, screen-space indirect light over
//! ambient occlusion's depth pyramid, the G-buffer's normals and the lit
//! colour, kept for the next frame's lighting, which binds it at its group 3. Where the device has the colour attachments for it, the
//! G-buffer and lighting are one fused pass; otherwise a G-buffer pass and a
//! lighting pass at its depth write the same targets. Lit colour keeps its
//! ambient light whole and records it apart, with the multiple scattering's
//! share of it in its alpha, for source completion to occlude by this
//! stage's visibility.
//!
//! The renderer encodes it in the stage order's named parts: the G-buffer
//! (`encode_gbuffer`), per phase of the camera's draw list, which leaves
//! the G-buffer complete; the lighting (`encode_lighting`), the sky and the
//! lighting pass at its depth over every phase's sets; then ambient
//! occlusion (`encode_ambient_occlusion`), which builds the depth pyramid
//! while either it or screen-space indirect light runs; then screen-space
//! indirect light (`encode_screen_indirect`). The fused form's pass is its
//! G-buffer part, and its lighting part encodes nothing. While occlusion
//! culling runs, the stage takes its two-pass form, and the renderer encodes
//! the cull stage's late phase between the early and late G-buffer passes,
//! the late one loading the early one's targets. While ray-traced shadows
//! run, the stage takes its two-pass form, the renderer encodes the
//! ray-traced shadow stage between its G-buffer and lighting parts, and the
//! lighting pass takes the lights the stage's mask holds from it, bound at
//! its group 3. While contact shadows run, likewise the contact shadow
//! stage, whose mask the lighting pass takes the directional light's
//! shadow with, bound there.
//!
//! Reads: the camera view and its draw list, group 0's camera lit and unlit
//! groups, the geometry pipelines, the ray-traced shadow mask and slot table
//! while they run, the contact mask while contact shadows run; for a probe
//! capture face (`encode_capture`), the capture's draw list and the face's
//! groups.
//! Writes: the shared G-buffer, colour, ambient light, source identity and
//! depth; its own ambient occlusion and screen-space indirect light
//! targets.
//! Honours: ambient occlusion (quality and radius), screen-space indirect
//! light (level and radius), the fused form (device capability,
//! diagnostics), the diagnostics layers compiled into the geometry
//! pipelines.
//! Timing groups: fused `sky`, `opaque geometry + lighting`; split
//! `geometry`, `geometry late` (occlusion culling), `sky`,
//! `opaque lighting`; then `ambient occlusion` (with the depth pyramid
//! while it runs) and `screen-space indirect` (with the pyramid while
//! ambient occlusion is off).
pub(crate) mod ambient_occlusion;
pub(crate) mod backdrop;
pub(crate) mod screen_indirect;

use crate::counters::Moment;
use crate::view::cached_group::CachedGroup;
use crate::view::compile::{Compiled, Compiler};
use crate::view::draw_list::gpu::Phase;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::frame::{CameraMask, FrameContext};
use crate::view::pipelines::{CameraShadows, GeometryPass, GeometryPipelines};
use crate::view::targets::{CaptureFace, attachment};

pub(crate) struct Opaque {
    backdrop: backdrop::BackdropDraw,
    ambient_occlusion: Compiled<ambient_occlusion::AmbientOcclusion>,
    /// Whether ambient occlusion ran this frame.
    ambient_occlusion_ran: bool,
    /// Screen-space indirect light, made the first frame it runs.
    screen_indirect: Option<screen_indirect::ScreenIndirect>,
    /// Whether the frame's camera lighting takes the last finished frame's
    /// screen-space indirect light (`ScreenIndirect::begin`).
    screen_indirect_read: bool,
    /// The lighting pass's group 3 while ray-traced shadows run
    /// (`shading::bind::shadow_mask`).
    shadow_mask: CachedGroup,
    /// The lighting pass's group 3 while contact shadows run
    /// (`shading::bind::contact_shadow`).
    contact_shadow: CachedGroup,
    /// The fused and lighting passes' group 3 while they take screen-space
    /// indirect light, without a mask, with the shadow mask and with the
    /// contact mask (`shading::bind::screen_indirect`,
    /// `shading::bind::shadow_mask_screen_indirect`,
    /// `shading::bind::contact_shadow_screen_indirect`), for each half of
    /// its pair they read.
    screen_indirect_groups: [[CachedGroup; 2]; 3],
}

/// Which of `Opaque::screen_indirect_groups` binds `mask` beside
/// screen-space indirect light.
fn mask_index(mask: Option<CameraMask<'_>>) -> usize {
    match mask {
        None => 0,
        Some(CameraMask::Traced(_)) => 1,
        Some(CameraMask::Contact(_)) => 2,
    }
}

impl Opaque {
    /// The stage over `unlit` group 0 for its sky, and the group 3 layouts
    /// of its camera lighting: the lighting pass's while ray-traced shadows
    /// run (`shadow_mask`) and while contact shadows run (`contact_shadow`),
    /// the fused and lighting passes' while they take screen-space indirect
    /// light (`screen_indirect`), and the lighting pass's with it and either
    /// mask (`shadow_mask_screen_indirect`, `contact_screen_indirect`).
    pub fn new(
        unlit: &wgpu::BindGroupLayout,
        [
            shadow_mask,
            contact_shadow,
            screen_indirect,
            shadow_mask_screen_indirect,
            contact_screen_indirect,
        ]: [&wgpu::BindGroupLayout; 5],
    ) -> Self {
        Self {
            backdrop: backdrop::BackdropDraw::new(unlit),
            ambient_occlusion: Compiled::default(),
            ambient_occlusion_ran: false,
            screen_indirect: None,
            screen_indirect_read: false,
            shadow_mask: CachedGroup::new(shadow_mask.clone()),
            contact_shadow: CachedGroup::new(contact_shadow.clone()),
            screen_indirect_groups: [
                screen_indirect,
                shadow_mask_screen_indirect,
                contact_screen_indirect,
            ]
            .map(|layout| std::array::from_fn(|_| CachedGroup::new(layout.clone()))),
        }
    }

    /// Begins the frame's screen-space indirect light, before any pass
    /// lights a surface: whether the camera lighting takes the last
    /// finished frame's (SI-1, SI-2).
    fn begin_screen_indirect(&mut self, ctx: &mut FrameContext<'_>) {
        let settings = ctx.effective.screen_indirect;
        ctx.pipelines.plans_screen_indirect = false;
        if settings.is_none() && self.screen_indirect.is_none() {
            self.screen_indirect_read = false;
            return;
        }
        let stage = self
            .screen_indirect
            .get_or_insert_with(|| screen_indirect::ScreenIndirect::new(ctx.device));
        self.screen_indirect_read = stage.begin(
            ctx.device,
            ctx.queue,
            settings,
            ctx.sizes.render,
            &ctx.history,
        );
        // A frame that runs the pass without reading it yet is followed
        // by one that reads it, with other lighting pipelines.
        ctx.pipelines.plans_screen_indirect = settings.is_some() && !self.screen_indirect_read;
    }

    /// The fused or lighting pass's group 3 while it takes the last
    /// finished frame's screen-space indirect light, with `mask` where the
    /// lighting pass takes the ray-traced shadow mask or the contact mask
    /// too.
    fn screen_indirect_group(
        &mut self,
        device: &wgpu::Device,
        mask: Option<CameraMask<'_>>,
    ) -> Option<&wgpu::BindGroup> {
        if !self.screen_indirect_read {
            return None;
        }
        let read = self.screen_indirect.as_ref()?.read()?;
        let mut entries = vec![
            (
                crate::shading::bind::screen_indirect::KEPT,
                wgpu::BindingResource::TextureView(read.kept),
            ),
            (
                crate::shading::bind::screen_indirect::VIEW,
                read.view.as_entire_binding(),
            ),
        ];
        match mask {
            None => {}
            Some(CameraMask::Traced(shadow_mask)) => entries.extend([
                (
                    crate::shading::bind::shadow_mask::MASK,
                    wgpu::BindingResource::TextureView(shadow_mask.mask),
                ),
                (
                    crate::shading::bind::shadow_mask::SLOTS,
                    shadow_mask.slots.as_entire_binding(),
                ),
            ]),
            Some(CameraMask::Contact(contact)) => entries.push((
                crate::shading::bind::contact_shadow::MASK,
                wgpu::BindingResource::TextureView(contact),
            )),
        }
        let groups = &mut self.screen_indirect_groups[mask_index(mask)];
        Some(groups[read.half].get(device, "screen-space indirect light", &entries))
    }

    /// The G-buffer part of `phase`: the sky and the fused pass, which
    /// lights the surfaces as it writes the G-buffer, or the G-buffer pass
    /// over the phase's sets and, after the frame's last, where the device
    /// cannot write anisotropy in it, the anisotropy fallback at `Equal`
    /// over every phase's. The fused form has the early phase alone.
    pub fn encode_gbuffer(&mut self, ctx: &mut FrameContext<'_>, phase: Phase) {
        if phase == Phase::Early {
            self.begin_screen_indirect(ctx);
        }
        if ctx.effective.fused {
            if phase == Phase::Early {
                self.encode_fused(ctx);
            }
        } else {
            Self::encode_gbuffer_pass(ctx, phase);
        }
    }

    /// The lighting part: in the two-pass form, the sky and the lighting
    /// pass at the G-buffer's depth, taking the lights the ray-traced
    /// shadow mask holds from it where the ray-traced shadow stage ran, or
    /// the directional light's shadow with the contact mask where the
    /// contact shadow stage ran (`mask`); nothing in the fused form, whose
    /// G-buffer part lit the surfaces. Without a contact mask, its groups
    /// let the last one go, which the contact shadow stage dropped.
    pub fn encode_lighting(&mut self, ctx: &mut FrameContext<'_>, mask: Option<CameraMask<'_>>) {
        if !matches!(mask, Some(CameraMask::Contact(_))) {
            self.contact_shadow.forget();
            self.screen_indirect_groups[2]
                .iter_mut()
                .for_each(CachedGroup::forget);
        }
        if !ctx.effective.fused {
            self.encode_lighting_pass(ctx, mask);
        }
    }

    /// Ambient occlusion over the G-buffer's depth and normals, where the
    /// settings run it, after the depth pyramid, which it builds at its
    /// radius, or at screen-space indirect light's where only that runs.
    pub fn encode_ambient_occlusion(&mut self, ctx: &mut FrameContext<'_>) {
        self.ambient_occlusion_ran = ctx.effective.ambient_occlusion.is_some();
        let pyramid = match (
            ctx.effective.ambient_occlusion,
            ctx.effective.screen_indirect,
        ) {
            (Some(settings), _) => (settings.radius, ambient_occlusion::TIMING_GROUP),
            (None, Some(settings)) => (settings.pyramid_radius, screen_indirect::TIMING_GROUP),
            (None, None) => {
                // Release the pyramid and the stage when only screen-space
                // indirect light made them; ambient occlusion keeps its own.
                if self.ambient_occlusion.ready().is_some_and(|ao| !ao.ran()) {
                    self.ambient_occlusion.clear();
                }
                return;
            }
        };
        let targets = ctx.targets;
        let Some(ao) = self
            .ambient_occlusion
            .get(ctx.compiler, "ambient occlusion", || {
                let queue = ctx.queue.clone();
                move |device: &wgpu::Device| {
                    ambient_occlusion::AmbientOcclusion::new(device, &queue)
                }
            })
        else {
            return;
        };
        ao.encode_depth_pyramid(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &targets.depth,
            &ctx.views.camera.view.uniform,
            pyramid.0,
            (ctx.timing, pyramid.1),
        );
        if let Some(settings) = ctx.effective.ambient_occlusion {
            ao.encode_visibility(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &targets.normal,
                &ctx.views.camera.view.uniform,
                settings.quality,
                settings.radius,
                ctx.history.frames,
                ctx.timing,
            );
            ao.resolve(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &targets.depth,
                &targets.motion,
                ctx.history,
                settings.quality,
                settings.radius,
                ctx.timing,
            );
        }
    }

    /// Screen-space indirect light over ambient occlusion's depth pyramid,
    /// the G-buffer and the lit colour, kept for the next frame's lighting,
    /// where the settings run it.
    pub fn encode_screen_indirect(&mut self, ctx: &mut FrameContext<'_>) {
        let Some(settings) = ctx.effective.screen_indirect else {
            return;
        };
        let bounce = self
            .screen_indirect
            .as_mut()
            .expect("the frame began screen-space indirect light");
        // A planning pass whose depth pyramid's stage is still compiling
        // asks for the pass's programs and leaves it out.
        let Some(ao) = self.ambient_occlusion.ready() else {
            bounce.plan(ctx.compiler);
            return;
        };
        let targets = ctx.targets;
        let inputs = screen_indirect::Inputs {
            depth_pyramid: ao.depth_view().expect("the frame built the depth pyramid"),
            hilbert: ao.hilbert(),
            depth: &targets.depth,
            normals: &targets.normal,
            color: &targets.color,
            motion: &targets.motion,
        };
        bounce.encode(
            (ctx.device, ctx.compiler),
            ctx.queue,
            ctx.encoder,
            &inputs,
            &ctx.views.camera.view.uniform,
            &ctx.history,
            settings,
            ctx.timing,
        );
    }

    /// Settles the last render's ambient occlusion and screen-space
    /// indirect light histories: see `Kept::settle`.
    pub fn settle(&mut self, finished: bool) {
        if let Some(ambient_occlusion) = self.ambient_occlusion.ready_mut() {
            ambient_occlusion.settle(finished);
        }
        if let Some(screen_indirect) = &mut self.screen_indirect {
            screen_indirect.settle(finished);
        }
    }

    /// Whether it holds ambient occlusion's stage (its depth pyramid among
    /// it) and screen-space indirect light's targets.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn holds_occlusion_and_bounce(&self) -> (bool, bool) {
        (
            self.ambient_occlusion.ready().is_some(),
            self.screen_indirect
                .as_ref()
                .is_some_and(screen_indirect::ScreenIndirect::holds_targets),
        )
    }

    /// This frame's ambient visibility, when ambient occlusion ran.
    pub fn visibility(&self) -> Option<&wgpu::TextureView> {
        self.ambient_occlusion
            .ready()
            .filter(|_| self.ambient_occlusion_ran)
            .and_then(|ambient_occlusion| ambient_occlusion.output())
    }

    fn encode_fused(&mut self, ctx: &mut FrameContext<'_>) {
        let targets = ctx.targets;
        {
            let backdrop = self.backdrop.pipeline(ctx.compiler);
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
            if let Some(backdrop) = &backdrop {
                backdrop::BackdropDraw::draw(backdrop, &mut pass, ctx.bindings.camera_unlit());
            }
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
            let screen_indirect = self.screen_indirect_group(ctx.device, None);
            if let Some(group) = screen_indirect {
                pass.set_bind_group(3, group, &[]);
            }
            ctx.views.camera.list.draw(
                ctx.scene,
                ctx.pipelines,
                &mut pass,
                GeometryPass::Fused {
                    screen_indirect: screen_indirect.is_some(),
                },
            );
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

    /// The sky, then the lighting pass at the G-buffer's depth, with
    /// `mask` at its group 3 where ray-traced or contact shadows ran.
    fn encode_lighting_pass(&mut self, ctx: &mut FrameContext<'_>, mask: Option<CameraMask<'_>>) {
        let targets = ctx.targets;
        // The sky writes color and motion; opaque geometry then writes color,
        // ambient light and identity together once, and its motion over the
        // sky's.
        {
            let mut motion = attachment(&targets.motion);
            motion.as_mut().unwrap().ops.load = wgpu::LoadOp::Load;
            let backdrop = self.backdrop.pipeline(ctx.compiler);
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
            if let Some(backdrop) = &backdrop {
                backdrop::BackdropDraw::draw(backdrop, &mut pass, ctx.bindings.camera_unlit());
            }
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
        let screen_indirect = self.screen_indirect_group(ctx.device, mask);
        let takes_screen_indirect = screen_indirect.is_some();
        if let Some(group) = screen_indirect {
            pass.set_bind_group(3, group, &[]);
        } else {
            match mask {
                None => {}
                Some(CameraMask::Traced(shadow_mask)) => {
                    let group = self.shadow_mask.get(
                        ctx.device,
                        "ray-traced shadow mask",
                        &[
                            (
                                crate::shading::bind::shadow_mask::MASK,
                                wgpu::BindingResource::TextureView(shadow_mask.mask),
                            ),
                            (
                                crate::shading::bind::shadow_mask::SLOTS,
                                shadow_mask.slots.as_entire_binding(),
                            ),
                        ],
                    );
                    pass.set_bind_group(3, group, &[]);
                }
                Some(CameraMask::Contact(contact)) => {
                    let group = self.contact_shadow.get(
                        ctx.device,
                        "contact shadow mask",
                        &[(
                            crate::shading::bind::contact_shadow::MASK,
                            wgpu::BindingResource::TextureView(contact),
                        )],
                    );
                    pass.set_bind_group(3, group, &[]);
                }
            }
        }
        let shadows = match mask {
            None => CameraShadows::Maps,
            Some(CameraMask::Traced(_)) => CameraShadows::ShadowMask,
            Some(CameraMask::Contact(_)) => CameraShadows::ContactMask,
        };
        ctx.views.camera.list.draw(
            ctx.scene,
            ctx.pipelines,
            &mut pass,
            GeometryPass::Lighting {
                shadows,
                screen_indirect: takes_screen_indirect,
            },
        );
        drop(pass);
        ctx.views.camera.recorded_since(started);
    }

    /// Probe capture face `face`: the sky under `unlit`, then `list`'s lit
    /// colour and motion with depth under `lit`, from the capture's draw
    /// instances; a live refresh's `timing` times it as `probe refresh
    /// faces`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_capture(
        &mut self,
        compiler: &Compiler,
        encoder: &mut wgpu::CommandEncoder,
        scene: &crate::Scene,
        pipelines: &mut GeometryPipelines,
        list: (&DrawList, &DrawInstances),
        face: &CaptureFace,
        unlit: &wgpu::BindGroup,
        lit: &wgpu::BindGroup,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("probe capture face"),
            timestamp_writes: timing.and_then(|timing| timing.render_pass("probe refresh faces")),
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
        self.draw_backdrop(compiler, &mut pass, unlit);
        pass.set_bind_group(0, lit, &[]);
        list.0
            .draw(scene, pipelines, list.1, &mut pass, GeometryPass::Forward);
    }

    /// The sky under `unlit`, in the caller's pass, as a probe capture face
    /// draws it first.
    pub fn draw_backdrop(
        &mut self,
        compiler: &Compiler,
        pass: &mut wgpu::RenderPass<'_>,
        unlit: &wgpu::BindGroup,
    ) {
        if let Some(backdrop) = self.backdrop.pipeline(compiler) {
            backdrop::BackdropDraw::draw(&backdrop, pass, unlit);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod test_geometry;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
