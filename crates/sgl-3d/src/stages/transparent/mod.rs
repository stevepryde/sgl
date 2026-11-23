//! Transparent: blended receivers as the surface, the volume layers, the
//! transmission copy, blended, transmissive and additive surfaces, mist and
//! distortion.
pub(crate) mod effects;
pub(crate) mod footprints;
pub(crate) mod heat;
pub(crate) mod mist;
pub(crate) mod sprites;
pub(crate) mod transmission;
pub(crate) mod volumes;

use crate::content::material::AlphaMode;
use crate::scene::shaders::ShadingChange;
use crate::shading::bind::{self, BindingTier, BlendedTrace};
use crate::view::cached_group::CachedGroup;
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
    /// the surface compose into their traced lobe. While the frame has the
    /// temporal masks (`Effective::temporal_masks`), these draws also write
    /// them, AMD's reactive and transparency and composition masks, which
    /// are cleared here and, while the scene holds an opaque surface whose
    /// shading moves or the view draws an instance footprint, marked by
    /// them first.
    Composite {
        reflections: Option<&'a wgpu::TextureView>,
    },
}

/// Transparent: the camera's masked sprite batches' depth and motion over
/// the opaque depth (`encode_sprite_depth`, `sprites`); the receiver pass
/// (`encode_receivers`), which draws the
/// blended receivers of screen-space reflections as the surface; the volume
/// layers (`encode_volumes`), while volume paths are in effect, of the
/// blended materials whose shader reads its volume path, which both blended
/// draws read; then masked sprites' colour, blended surfaces,
/// transmissive ones among them, and blended sprite batches back to
/// front, and additive glow and ground mist, drawn (`encode`) into the
/// reflections' incident radiance while they trace it and onto the
/// composite, each fogged from the frame's fog volume where it lies, then
/// heat distortion (`encode_heat`). On a device of the Extended binding tier, in a frame
/// whose blended list holds a transmissive material, the draw onto the
/// composite first copies it with its mips (`transmission`), which the
/// transmissive surfaces sample; elsewhere, and in the draw into the
/// incident radiance, they blend the light behind them through. While the frame has the
/// temporal masks and the scene
/// holds an opaque or masked material whose shading may change where its
/// geometry stands still (its normal layers move or it has a game's
/// shader), the draw onto the composite first marks the surfaces whose
/// shading changes in this frame (`Scene::shading_changes`) in the
/// transparency and composition mask, at the opaque depth (a masked
/// material whose shader's inputs were edited over its footprint in front
/// of it), as AMD's FSR
/// sample marks its animated textures after its lighting and before its
/// translucency (`samples/fsrapi/config/fsrapiconfig.json`, its render
/// modules' order; SDK 1.1.4, MIT, see LICENSE-amd-fidelityfx.txt); in the
/// same pass, where the view draws them, the instance footprints
/// (`footprints`, the register's M-23): where changed instances stood, at
/// or in front of the opaque depth, and where they stand.
///
/// Reads: the camera's blended draw list with its lit group 0 and the
/// geometry pipelines, the camera's opaque draw list (its sets whose
/// material's shading changes, with the temporal masks), its unlit group 0 (with the
/// fog volume), the view's footprint draws with the scene's footprint
/// records and object records (with the temporal masks), the opaque depth
/// (written by masked sprites' depth, copied into the surface depth, and
/// tested by every other draw but the receiver pass), the surface depth
/// and the screen-space method's result (receivers that are the surface),
/// the scene's transient geometry (glow, heat and mist) and the camera's
/// sprite batches (`view::sprites`), the beauty it draws
/// onto (the composite copied for transmission), its volume layers (the
/// blended draws, and the second exit layer's pass the exit layer).
/// Writes: the opaque depth and the G-buffer's motion (masked sprites);
/// the surface depth, the receiver layer and the G-buffer's motion
/// (the receiver pass); its volume layers (copies of the opaque depth with
/// the volumes' faces drawn over them); its transmission copy; its mist
/// positions and its blended sprites' order, ordered from the view; that beauty
/// in place; the temporal masks while the frame has them; the completed scene in place
/// (heat) through its own snapshot of it.
/// Blended surfaces write no other depth, motion or G-buffer.
/// Honours: the receiver pass and volume paths (the effective
/// configuration's), atmosphere (mist), heat distortion, the temporal masks,
/// the effects and atmosphere diagnostics layers.
/// Timing groups: `sprites` (masked sprites' depth and colour),
/// `receivers`, `volume layers`, `temporal composition` (the
/// moving opaque surfaces' mask and the instance footprints),
/// `transmission copy`, `blended` (blended surfaces and blended sprites,
/// both draws), `transparent` (glow and mist, both draws), `heat
/// distortion`.
/// History: none; the receiver pass rebuilds the surface, the volume layers
/// are drawn from the frame's geometry and the copy is made again every
/// frame they run.
pub(crate) struct Transparent {
    /// Glow pipelines and the group binding the current opaque depth and exposure.
    glow: Option<(effects::Effects, wgpu::BindGroup)>,
    mist: Option<mist::Mist>,
    /// Sprite batches' pipelines and this view's orders of them.
    sprites: sprites::Sprites,
    /// The instance footprints' pipelines.
    footprints: footprints::Footprints,
    heat: Option<heat::Heat>,
    /// The frame exposure that glow reads.
    exposure: wgpu::TextureView,
    /// No screen-space result, which the blended draws bind while none
    /// composes, and no transmission copy, which they bind on the Extended
    /// binding tier while none holds the frame: 1×1, zero.
    no_reflections: wgpu::TextureView,
    /// The blended group 3 of the draw into the incident radiance, which
    /// composes nothing, and of the draw onto the composite.
    blended: [BlendedGroup; 2],
    /// The copy of the composed frame transmissive surfaces sample.
    transmission: Option<transmission::Transmission>,
    /// The volume layers a game's shader measures its volume path from.
    volumes: volumes::Volumes,
    /// The device's binding tier, which binds the copy on `Extended`.
    tier: BindingTier,
}

/// A blended draw's group 3 (`shading::bind::blended`), kept while it binds
/// the same method result and surface depth, and its `BlendedTrace`
/// uniform, written when the values change.
struct BlendedGroup {
    group: CachedGroup,
    trace: wgpu::Buffer,
    written: Option<BlendedTrace>,
}

impl Transparent {
    /// The stage over `blended`
    /// group 3 (`shading::bind::blended`) of the device's binding `tier` for
    /// its blended draws, its glow reading the frame's `exposure`
    /// multiplier (`stages::exposure::Exposure::view`).
    pub fn new(
        device: &wgpu::Device,
        blended: &wgpu::BindGroupLayout,
        exposure: &wgpu::TextureView,
        tier: BindingTier,
    ) -> Self {
        let group = |label| BlendedGroup {
            group: CachedGroup::new(blended.clone()),
            trace: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(label),
                    size: size_of::<BlendedTrace>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            written: None,
        };
        Self {
            exposure: exposure.clone(),
            glow: None,
            mist: None,
            sprites: Default::default(),
            footprints: Default::default(),
            heat: None,
            no_reflections: crate::view::targets::target(
                device,
                "no screen-space reflections",
                [1, 1],
                crate::shading::gbuffer::COLOR,
            ),
            blended: [
                group("blended incident trace"),
                group("blended composite trace"),
            ],
            transmission: None,
            volumes: volumes::Volumes::new(device, blended),
            tier,
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

    /// The Receivers and volume layers step's second part, after the
    /// receivers: when the effective
    /// configuration has volume paths and the camera's blended list holds a
    /// material whose shader reads its volume path, copies the opaque depth
    /// into each volume layer and draws those materials' batches over it
    /// (`volumes`), which this frame's blended draws then bind; otherwise
    /// they bind the opaque depth in their place. Returns whether it drew.
    /// Without volume paths it frees the layers, and the blended groups
    /// that bound them let go of them.
    pub fn encode_volumes(&mut self, ctx: &mut FrameContext<'_>) -> bool {
        if !ctx.effective.volume_paths {
            if self.volumes.release() {
                for blended in &mut self.blended {
                    blended.group.forget();
                }
            }
            return false;
        }
        self.volumes.encode(ctx, &self.no_reflections)
    }

    /// The Masked sprites step, first of Frame item 7: the camera's masked
    /// sprite batches' depth over the opaque depth and their motion into
    /// the G-buffer's (`sprites::Sprites::encode_depth`), before the
    /// receiver pass copies the opaque depth. Returns whether it drew.
    pub fn encode_sprite_depth(&mut self, ctx: &mut FrameContext<'_>) -> bool {
        self.sprites.encode_depth(ctx)
    }

    /// Orders the camera's blended sprite batches from the view's eye for
    /// the frame's draws (`encode`).
    pub fn order_sprites(&mut self, ctx: &FrameContext<'_>) {
        self.sprites.order(ctx);
    }

    /// Orders the scene's mist from the view's eye for the frame's draws
    /// (`encode`), in a frame that draws it.
    pub fn order_mist(&mut self, ctx: &FrameContext<'_>) {
        if ctx.effective.composite.mist {
            self.mist
                .get_or_insert_with(|| mist::Mist::new(ctx.device, &ctx.bindings.unlit))
                .order(
                    ctx.device,
                    ctx.queue,
                    &ctx.scene.transient,
                    &glam::Mat4::from_cols_array_2d(&ctx.values.view.view),
                );
        }
    }

    /// Rebinds the depth the glow reads, beside the frame's `exposure`.
    pub fn resize(&mut self, device: &wgpu::Device, targets: &SharedTargets) {
        if let Some((effects, group)) = &mut self.glow {
            *group = effects.depth_group(device, &targets.depth, &self.exposure);
        }
    }

    /// Blended surfaces, glow and mist onto `beauty`.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>, beauty: Beauty<'_>) {
        let targets: &SharedTargets = ctx.targets;
        // The draw's slot in `blended`, and the method's result it composes.
        let (beauty, temporal_masks, (trace, reflections)) = match beauty {
            Beauty::Incident(view) => (view, None, (0, None)),
            Beauty::Composite { reflections } => {
                (&targets.composite, composite_masks(ctx), (1, reflections))
            }
        };
        // A view whose antialiasing reads the masks makes the footprints'
        // programs in its first frame, planned (`Renderer::compile`) or
        // not, since any instance edit brings them into a frame (M-23).
        if trace == 1 && (ctx.effective.taa || ctx.effective.fsr) {
            self.footprints.programs(ctx);
        }
        // The first pass that writes the temporal masks clears them: the
        // temporal composition's while the scene holds a moving opaque
        // surface or the view draws a footprint, else the blended
        // surfaces', else the additive effects'.
        let composition = temporal_masks.filter(|_| {
            ctx.effective.composite.moving_surfaces || !ctx.views.footprints.is_empty()
        });
        if let Some(masks) = composition {
            self.encode_composition(ctx, masks);
        }
        let mut cleared = composition.is_some();
        // Masked sprites' colour, at the depth their depth pass wrote, before
        // the transmission copy, so transmissive surfaces show them.
        cleared |= self
            .sprites
            .encode_masked(ctx, beauty, temporal_masks, cleared)
            && temporal_masks.is_some();
        let sprites = self.sprites.ready_blended(ctx);
        if !ctx.views.blended.is_empty() || sprites {
            // The copy of the composed frame, before anything is drawn onto
            // it, where a transmissive surface shows.
            let copy = (trace == 1
                && self.tier == BindingTier::Extended
                && ctx.views.blended.holds_transmissive(ctx.scene))
            .then(|| {
                self.transmission
                    .get_or_insert_with(|| transmission::Transmission::new(ctx.device))
                    .encode(ctx.device, ctx.encoder, &targets.composite, ctx.timing)
                    .clone()
            });
            let group = (!ctx.views.blended.is_empty()).then(|| {
                Self::blended_group(
                    ctx,
                    &mut self.blended[trace],
                    &self.no_reflections,
                    (reflections, copy.as_ref(), self.volumes.held()),
                    self.tier,
                )
                .clone()
            });
            Self::encode_blended(
                ctx,
                beauty,
                temporal_masks,
                cleared,
                group.as_ref(),
                &self.sprites,
            );
            cleared = true;
        }
        let draw = ctx.effective.composite.glow;
        if draw || (temporal_masks.is_some() && !cleared) {
            let mut color = attachment(beauty).unwrap();
            color.ops.load = wgpu::LoadOp::Load;
            let mut attachments = vec![Some(color)];
            attachments.extend(
                temporal_masks
                    .into_iter()
                    .flatten()
                    .map(|mask| loaded(attachment(mask), cleared)),
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
                let (effects, depth_group) = self.glow.get_or_insert_with(|| {
                    let effects = effects::Effects::new(ctx.device, &ctx.bindings.unlit);
                    let group = effects.depth_group(ctx.device, &targets.depth, &self.exposure);
                    (effects, group)
                });
                effects.draw(
                    &mut pass,
                    ctx.bindings.camera_unlit(),
                    depth_group,
                    &ctx.scene.transient,
                    temporal_masks.is_some(),
                );
            }
        }
        if ctx.effective.composite.mist {
            self.mist
                .as_ref()
                .expect("mist ordered before drawing")
                .encode(
                    ctx.encoder,
                    ctx.bindings.camera_unlit(),
                    beauty,
                    &targets.depth,
                    temporal_masks,
                    ctx.timing,
                    "transparent",
                );
        }
    }

    /// A blended draw's group 3 `blended`: the screen-space method's
    /// `reflections` with its cutoff and fade where the draw composes them,
    /// else `no_reflections` and no trace, and the frame's surface depth;
    /// on a device of `tier` `Extended`, the transmission `copy` where it
    /// holds the frame, else `no_reflections` in its place, the opaque
    /// depth, and the volume layers where they hold the frame (`volumes`),
    /// else the opaque depth in their place.
    fn blended_group<'a>(
        ctx: &FrameContext<'_>,
        blended: &'a mut BlendedGroup,
        no_reflections: &wgpu::TextureView,
        (reflections, copy, volumes): (
            Option<&wgpu::TextureView>,
            Option<&wgpu::TextureView>,
            Option<&volumes::Layers>,
        ),
        tier: BindingTier,
    ) -> &'a wgpu::BindGroup {
        // A blended receiver composes a screen-space method's result, which
        // traced it as the surface.
        let traced = ctx
            .effective
            .trace
            .filter(|_| ctx.effective.screen_space.is_some())
            .zip(reflections);
        let mut values = traced.map_or_else(BlendedTrace::default, |(trace, _)| BlendedTrace {
            cutoff: trace.cutoff,
            fade: trace.fade,
            ..BlendedTrace::default()
        });
        values.transmission = u32::from(copy.is_some());
        values.volumes = u32::from(volumes.is_some());
        if blended.written != Some(values) {
            crate::counters::write_buffer(
                ctx.queue,
                &blended.trace,
                0,
                bytemuck::bytes_of(&values),
            );
            blended.written = Some(values);
        }
        let reflections = traced.map_or(no_reflections, |(_, view)| view);
        let texture = wgpu::BindingResource::TextureView;
        let mut entries = vec![
            (bind::blended::REFLECTIONS, texture(reflections)),
            (bind::blended::SURFACE_DEPTH, texture(ctx.surface.depth)),
            (bind::blended::TRACE, blended.trace.as_entire_binding()),
        ];
        if bind::blended::tier(bind::blended::TRANSMISSION) <= tier {
            entries.push((
                bind::blended::TRANSMISSION,
                texture(copy.unwrap_or(no_reflections)),
            ));
        }
        // The opaque depth, which the draws test read-only, as the glow
        // samples it: a game's shader's scene depth (shader_scene_depth.wgsl).
        if bind::blended::tier(bind::blended::SCENE_DEPTH) <= tier {
            entries.push((bind::blended::SCENE_DEPTH, texture(&ctx.targets.depth)));
        }
        // The volume layers a game's shader measures its path from
        // (shader_scene_depth.wgsl's scene_volume_path).
        if bind::blended::tier(bind::blended::VOLUME_ENTRY) <= tier {
            let layer = |held: fn(&volumes::Layers) -> &wgpu::TextureView| {
                texture(volumes.map_or(&ctx.targets.depth, held))
            };
            entries.extend([
                (bind::blended::VOLUME_ENTRY, layer(|layers| &layers.entry)),
                (bind::blended::VOLUME_EXIT, layer(|layers| &layers.exit)),
                (
                    bind::blended::VOLUME_SECOND_EXIT,
                    layer(|layers| &layers.second_exit),
                ),
            ]);
        }
        blended
            .group
            .get(ctx.device, "blended reflections", &entries)
    }

    /// The temporal `masks`, cleared, with the camera's opaque and masked
    /// surfaces whose material's shading changes in this frame where their
    /// geometry stands still (`Scene::shading_changes`) marked in its
    /// transparency and composition mask, drawn at the opaque depth, which
    /// they test for equality without writing it; a masked material whose
    /// shader's inputs were edited is drawn over its footprint at or in
    /// front of the opaque depth, its cut-out texels included
    /// (`GeometryPass::TemporalComposition`); then the view's instance
    /// footprints (`footprints`, the register's M-23).
    fn encode_composition(&mut self, ctx: &mut FrameContext<'_>, masks: &[wgpu::TextureView; 2]) {
        let programs = if ctx.views.footprints.is_empty() {
            None
        } else {
            self.footprints.programs(ctx)
        };
        let attachments = masks.each_ref().map(attachment);
        let started = crate::counters::Moment::now();
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("temporal composition of moving surfaces"),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &ctx.targets.depth,
                depth_ops: None,
                stencil_ops: None,
            }),
            timestamp_writes: ctx
                .timing
                .and_then(|t| t.render_pass("temporal composition")),
            ..Default::default()
        });
        if let Some(programs) = programs {
            footprints::Footprints::draw(
                programs,
                (ctx.scene, ctx.views, ctx.bindings.camera_unlit()),
                &mut pass,
            );
        }
        if !ctx.effective.composite.moving_surfaces {
            drop(pass);
            ctx.views.camera.recorded_since(started);
            return;
        }
        pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
        let scene = ctx.scene;
        let changes = scene.shading_changes();
        // A planar reflector's surface the view shows in this frame: its
        // picture moves while its surface's motion vectors stand still.
        let mirrors = &ctx.views.mirrors;
        let mirrored = |material| {
            scene
                .drawn_material(material)
                .values
                .planar_reflector
                .is_some_and(|reflector| mirrors.contains(&reflector))
        };
        // Whether a material's edit marks its footprint: a masked one's, whose
        // coverage the edit may have changed.
        let footprint = |material| {
            changes.of(scene, material) == ShadingChange::Edited
                && matches!(
                    scene.drawn_material(material).values.alpha,
                    AlphaMode::Mask { .. }
                )
        };
        ctx.views.camera.list.draw_materials(
            scene,
            ctx.pipelines,
            &mut pass,
            GeometryPass::TemporalComposition { footprint: false },
            &|material| {
                (changes.of(scene, material) != ShadingChange::None || mirrored(material))
                    && !footprint(material)
            },
        );
        ctx.views.camera.list.draw_materials(
            scene,
            ctx.pipelines,
            &mut pass,
            GeometryPass::TemporalComposition { footprint: true },
            &footprint,
        );
        drop(pass);
        ctx.views.camera.recorded_since(started);
    }

    /// The camera's blended surfaces and blended sprite batches onto
    /// `beauty`, back to front, tested against the opaque depth without
    /// writing it, the surfaces with their group 3 `group` (none while the
    /// list is empty), each sprite batch between the surfaces farther and
    /// nearer than it (`DrawList::batches_before`), and onto the
    /// `temporal_masks`, which this pass clears unless they were `cleared`.
    fn encode_blended(
        ctx: &mut FrameContext<'_>,
        beauty: &wgpu::TextureView,
        temporal_masks: Option<&[wgpu::TextureView; 2]>,
        cleared: bool,
        group: Option<&wgpu::BindGroup>,
        sprites: &sprites::Sprites,
    ) {
        let color = loaded(attachment(beauty), true);
        let mut attachments = vec![color];
        attachments.extend(
            temporal_masks
                .into_iter()
                .flatten()
                .map(|mask| loaded(attachment(mask), cleared)),
        );
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
        let masked = temporal_masks.is_some();
        let kind = GeometryPass::Blended {
            temporal_masks: masked,
        };
        let list = &ctx.views.blended;
        // The surfaces' batches `range`, their groups bound again after a
        // sprite batch's.
        let mut surfaces = |pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<usize>| {
            if let Some(group) = group.filter(|_| !range.is_empty()) {
                pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
                pass.set_bind_group(3, group, &[]);
                list.draw_batches(
                    ctx.scene,
                    ctx.pipelines,
                    &ctx.views.instances,
                    pass,
                    kind,
                    range,
                );
            }
        };
        let mut drawn = 0;
        for listed in ctx.views.sprites.blended() {
            let farther = list.batches_before(listed.depth);
            surfaces(&mut pass, drawn..farther);
            drawn = drawn.max(farther);
            sprites.draw_blended(
                &mut pass,
                ctx.scene,
                ctx.bindings.camera_unlit(),
                listed,
                masked,
            );
        }
        surfaces(&mut pass, drawn..list.batch_count());
    }

    /// Heat distortion of the completed scene `color` in place, marking
    /// the temporal masks under the condition the draws onto the composite
    /// mark them (`composite_masks`).
    pub fn encode_heat(&mut self, ctx: &mut FrameContext<'_>, color: &wgpu::TextureView) {
        self.heat
            .get_or_insert_with(|| heat::Heat::new(ctx.device))
            .encode(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &ctx.scene.transient,
                &ctx.values.view.view_projection,
                ctx.sizes.scene,
                color,
                &ctx.targets.depth,
                composite_masks(ctx),
                ctx.timing,
            );
    }
}

/// The temporal masks, while the frame's draws onto the composite write
/// them (`Effective::temporal_masks`): the one condition the blended
/// surfaces, additive effects, mist and heat distortion all follow.
fn composite_masks<'a>(ctx: &FrameContext<'a>) -> Option<&'a [wgpu::TextureView; 2]> {
    ctx.targets
        .temporal_masks
        .as_ref()
        .filter(|_| ctx.effective.temporal_masks)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod blended_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod composition_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod effects_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod footprints_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod receiver_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod sprites_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod transmission_tests;

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
