//! One frame: the frame's scene work once (`shared::Shared::run_scene`),
//! then the one ordered render body for each of its views (`render_view`),
//! the primary view's order running the scene's GPU work.
use super::shared::Shared;
use super::view::{Pending, View, ViewFrame};
use crate::settings::{ReflectionMethod, Settings};
use crate::stages::cull::Cull;
use crate::stages::opaque::Opaque;
use crate::stages::prepare::Prepare;
use crate::stages::shadows::traced::TracedShadows;
use crate::stages::shadows::traced::slots::SlotLights;
use crate::timing::GpuTiming;
use crate::view::draw_list::gpu::Phase;
use crate::view::effective::Effective;
use crate::view::frame::{Completed, FrameContext, HardwareRays};
use crate::view::history::HistoryFrame;
use crate::view::pipelines::GeometryPipelines;
use crate::view::post_fx::PostFx;
use crate::view::targets::SharedTargets;
use crate::{FrameInput, Scene};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Which view of a frame `render_view` renders. The primary view, the
/// first, runs the frame's scene work in its stage order: the deform pass,
/// the acceleration-structure builds and the dynamic GI update. Every view
/// binds the dynamic GI probes it left.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Primary,
    Secondary,
}

/// What a view draws in a frame: its input, its output and its timing.
pub(super) struct ViewOutput<'a> {
    pub input: &'a FrameInput,
    pub output: &'a wgpu::TextureView,
    pub timing: Option<&'a GpuTiming>,
}

/// A secondary view's `settings` with what a scene has one of taken from
/// the frame's `primary` view's: hardware ray tracing, dynamic GI and
/// anisotropic filtering. The diagnostics layers follow in `secondary`.
fn scene_wide(settings: &Settings, primary: &Settings) -> Settings {
    Settings {
        hardware_ray_tracing: primary.hardware_ray_tracing,
        dynamic_gi: primary.dynamic_gi,
        anisotropic_filtering: primary.anisotropic_filtering,
        ..*settings
    }
}

/// A secondary view's `effective` configuration with the primary view's
/// pipeline layers and material filtering, which the scene and the shared
/// pipelines have one of.
fn secondary(effective: Effective, primary: &Effective) -> Effective {
    Effective {
        layers: primary.layers,
        anisotropy: primary.anisotropy,
        ..effective
    }
}

/// Encodes one frame of `scene` seen by `views`, the first the primary:
/// the scene's frame begins, each view begins its frame (settling its last
/// render and beginning its history), the frame's scene work runs once,
/// steered by the primary view, then each view renders in order, holding
/// `finished`, the flag `Renderer::finish_frame` sets for this attempt.
/// Returns the scene's frame attempt.
pub(super) fn render(
    shared: &mut Shared,
    (device, queue, encoder): (&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder),
    scene: &mut Scene,
    views: &mut [ViewFrame<'_>],
    finished: &Arc<AtomicBool>,
) -> u64 {
    let attempt = scene.begin_frame(device, queue);
    let primary_settings = *views[0].settings;
    let mut begun: Vec<(HistoryFrame, Effective)> = Vec::with_capacity(views.len());
    for (index, frame) in views.iter_mut().enumerate() {
        let settings = match index {
            0 => primary_settings,
            _ => scene_wide(frame.settings, &primary_settings),
        };
        let (history, effective) = frame
            .view
            .begin(device, shared, scene, frame.input, &settings);
        let effective = match begun.first() {
            Some((_, primary)) => secondary(effective, primary),
            None => effective,
        };
        begun.push((history, effective));
    }
    // The ray instances are updated and the acceleration structures built
    // when any view of the frame traces; the shadow-mask pipelines exist
    // once any runs ray-traced shadows, and screen-space indirect light's
    // once any runs it.
    let traced = begun
        .iter()
        .any(|(_, effective)| Prepare::traces(scene, effective));
    let shadow_mask = begun
        .iter()
        .any(|(_, effective)| effective.ray_traced_shadows);
    let screen_indirect = begun
        .iter()
        .any(|(_, effective)| effective.screen_indirect.is_some());
    shared.run_scene(
        (device, queue),
        scene,
        &begun[0].1,
        views[0].input,
        (traced, (shadow_mask, screen_indirect)),
    );
    for (index, (frame, begun)) in views.iter_mut().zip(begun).enumerate() {
        render_view(
            shared,
            frame.view,
            (device, queue, encoder),
            scene,
            ViewOutput {
                input: frame.input,
                output: frame.output,
                timing: frame.timing,
            },
            begun,
            (
                if index == 0 {
                    Role::Primary
                } else {
                    Role::Secondary
                },
                attempt,
                finished,
            ),
        );
    }
    attempt
}

/// Encodes `view`'s frame of `scene` as `drawn` describes it, after the
/// frame's scene work, with its begun history and effective
/// configuration: prepare's view part (and in the primary view the
/// deformation and acceleration structures, then the cull stage's early
/// phase), the sky's tables, dynamic GI in the primary view, shadows,
/// volumetric fog, opaque (with the cull stage's late phase and pyramid
/// while occlusion culling runs, and the ray-traced shadows between its G-buffer and lighting while
/// they run), the transparent stage's receivers and volume layers,
/// reflections with the transparent stage drawn into their input (while a
/// screen-space method traces it) and onto their result, heat, exposure,
/// antialiasing, motion blur, then post.
pub(super) fn render_view(
    shared: &mut Shared,
    view: &mut View,
    (device, queue, encoder): (&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder),
    scene: &Scene,
    drawn: ViewOutput<'_>,
    (mut history, effective): (HistoryFrame, Effective),
    (role, attempt, finished): (Role, u64, &Arc<AtomicBool>),
) {
    let ViewOutput {
        input,
        output,
        timing,
    } = drawn;
    let primary = role == Role::Primary;
    let Shared {
        pipelines,
        prepare,
        deform,
        dynamic_gi,
        ray_form,
        ..
    } = shared;
    let pipelines: &GeometryPipelines = pipelines;
    let View {
        sizes,
        targets,
        views,
        bindings,
        post_fx,
        cull,
        sky,
        statistics,
        shadows,
        traced_shadows,
        fog,
        opaque,
        reflections,
        transparent,
        exposure,
        antialiasing,
        motion_blur,
        post,
        rendered,
        targets_generation,
        #[cfg(feature = "diagnostics")]
        probe,
        #[cfg(any(test, feature = "diagnostics"))]
            completed: diagnostic_completed,
        ..
    } = view;
    let targets: &SharedTargets = targets;
    #[cfg(feature = "diagnostics")]
    let mut probe =
        crate::stages::frame_probe::FrameProbe::for_frame(probe, device, effective.frame_probe);
    #[cfg(feature = "diagnostics")]
    if let Some(probe) = probe.as_deref_mut() {
        let metadata = crate::stages::frame_probe::metadata(*sizes, input, history);
        probe.begin(device, encoder, metadata);
    }
    // DiligentFX's effects share one post-effect context, as in Hydrogent.
    let crystal = effective
        .screen_space
        .is_some_and(|ssr| ssr.method == ReflectionMethod::Crystal);
    if !effective.post_fx {
        if post_fx.take().is_some() {
            // Cached presentation groups may hold TAA's outputs.
            post.forget_inputs();
            motion_blur.forget_inputs();
        }
    } else if post_fx
        .as_ref()
        .is_none_or(|post_fx| post_fx.taa() != effective.taa)
    {
        *post_fx = Some(Box::new(PostFx::new(device, queue, effective.taa)));
        post.forget_inputs();
        motion_blur.forget_inputs();
    }
    if let Some(post_fx) = post_fx.as_mut()
        && !crystal
    {
        post_fx.release_screen_space_reflections();
    }
    let context_jitter = post_fx.as_mut().map(|post_fx| {
        post_fx.prepare(device, encoder, sizes.render, history.frames, history.valid)
    });
    let jitter = antialiasing.prepare(
        &effective,
        *sizes,
        history,
        post_fx.as_deref_mut(),
        context_jitter,
    );
    // The camera history commits the jitter this frame applies.
    history.camera.jitter = jitter.map_or([0.; 2], |jitter| jitter.ndc);
    *rendered = Some(Pending {
        attempt,
        finished: finished.clone(),
        history,
        generation: *targets_generation,
    });
    // The camera's statistics: those completed so far, and none copied yet
    // this frame.
    statistics.poll(device);
    statistics.begin();
    let values = Prepare::run_view(
        device,
        queue,
        scene,
        input,
        history,
        &effective,
        jitter,
        sizes.render,
        views,
        &bindings.frame,
    );
    shadows.resize(device, effective.shadow_quality);
    shadows.local.prepare(
        device,
        queue,
        bindings,
        (scene, &mut views.instances),
        (input.camera.view, input.camera.projection),
        crate::stages::shadows::local::ShadowFrame {
            mask: values.frame.visibility_mask,
            time: input.elapsed_seconds,
            layers: effective.layers,
        },
        effective.local_lights,
    );
    // Every list the frame draws is built.
    views.instances.upload(device, queue);
    fog.prepare(
        device,
        (effective.fog, effective.aerial_perspective),
        sizes.render,
    );
    if let Some(dynamic_gi) = dynamic_gi.as_mut().filter(|_| primary) {
        dynamic_gi.prepare(device, scene, &effective);
    }
    // The ray-traced shadows run where the frame's rays trace in hardware
    // and its slots hold a light: the directional light with cascades and
    // the local lights the atlas placed.
    let slot_lights = SlotLights::of(input, &values.frame, scene, shadows.local.ranking());
    let effective =
        super::effective::traced_shadows(effective, prepare.hardware_rays(), &slot_lights);
    // The sky stage's tables, made on its first sky frame and dropped on a
    // frame without one, which the environment's resolver binds.
    sky.prepare(
        device,
        input,
        input.camera.eye,
        (!history.valid, effective.aerial_perspective),
    );
    // After prepare, the local shadows', the fog's, dynamic GI's and the
    // sky's, which may replace a light cluster buffer, a shadow map, the
    // shadow records, the fog volume, the probes or the sky-view table group
    // 0 binds.
    bindings.refresh(
        device,
        scene,
        crate::view::bindings::EnvironmentViews::resolve(
            &scene.environments,
            &input.environment,
            sky.views(),
        ),
        views,
        shadows.maps(),
        fog.volume(),
        dynamic_gi
            .as_ref()
            .map(crate::stages::dynamic_gi::DynamicGi::probes),
    );
    let mut ctx = FrameContext {
        device,
        queue,
        encoder,
        timing,
        effective: &effective,
        sizes: *sizes,
        targets,
        surface: targets.surface(false),
        scene,
        values: &values,
        input,
        views: &*views,
        bindings,
        pipelines,
        history,
        environment_revision: sky.environment_revision(),
        hardware_rays: HardwareRays::of(
            &effective,
            scene,
            prepare.hardware_rays(),
            ray_form.as_ref(),
        ),
    };
    // Prepare's GPU steps, before any pass draws scene geometry or traces:
    // the deformations, then the acceleration structures over them, in the
    // primary view, then the cull stage's early phase for every GPU-built
    // view.
    if primary {
        deform.encode(&mut ctx);
        prepare.encode_acceleration_structures(&mut ctx);
    }
    cull.encode_early(&mut ctx);
    // First after prepare: the sky's tables, which everything that lights or
    // draws the environment reads.
    sky.encode(&mut ctx);
    // Then dynamic GI: every pass that shades reads the probes.
    if let Some(dynamic_gi) = dynamic_gi.as_mut().filter(|_| primary) {
        dynamic_gi.encode(&mut ctx);
    }
    // The stage order's: the local-light atlas, then the directional cascades.
    shadows.encode_local(&mut ctx);
    shadows.encode_directional(&mut ctx);
    // The fog over the sky stage's aerial volume, which the renderer lends.
    fog.encode(&mut ctx, sky.aerial_volume());
    encode_opaque(opaque, cull, traced_shadows, &slot_lights, &mut ctx);
    // The camera's statistics, once both phases have appended to its sets.
    statistics.copy(
        device,
        ctx.encoder,
        &ctx.views.camera.list,
        ctx.views.blended.stats,
        #[cfg(feature = "diagnostics")]
        crate::view::draw_list::readback::ModelStatistics {
            slots: ctx.scene.candidates.models(),
            blended: ctx.views.blended.stats_by_model(ctx.scene),
        },
    );
    #[cfg(feature = "diagnostics")]
    if let Some(probe) = probe.as_deref() {
        probe.observe(
            device,
            ctx.encoder,
            0,
            &targets.color,
            &targets.color,
            false,
        );
        probe.coverage(device, ctx.encoder, &targets.depth, &targets.source_id);
    }
    // The surface reflections and the temporal consumers read from here on:
    // the nearest receiver's over the opaque depth.
    let receivers = transparent.encode_receivers(&mut ctx);
    ctx.surface = targets.surface(receivers);
    // The volume layers and the mist's order both blended draws read.
    transparent.encode_volumes(&mut ctx);
    transparent.order_mist(&ctx);
    // Every reflection producer initializes composite before reading it.
    if let Some(post_fx) = post_fx.as_mut() {
        let camera = ctx.views.reflection_camera;
        post_fx.begin(
            device,
            queue,
            ctx.encoder,
            targets,
            ctx.surface,
            sizes.render,
            glam::Mat4::from_cols_array_2d(&camera.view),
            glam::Mat4::from_cols_array_2d(&camera.proj),
            history.previous_camera,
            timing,
        );
    }
    let ambient_occlusion = opaque.visibility();
    reflections.complete(&mut ctx, ambient_occlusion);
    // Blended surfaces, additive effects and mist belong to both what the
    // camera sees and what reflections see, where a screen-space method
    // traces them.
    if let Some(incident) = reflections.incident() {
        transparent.encode(
            &mut ctx,
            crate::stages::transparent::Beauty::Incident(incident),
        );
    }
    let reflected = reflections.resolve(&mut ctx, post_fx.as_deref_mut(), ambient_occlusion);
    transparent.encode(
        &mut ctx,
        crate::stages::transparent::Beauty::Composite {
            reflections: reflected.as_ref(),
        },
    );
    #[cfg(feature = "diagnostics")]
    if let Some(probe) = probe.as_deref() {
        probe.observe(
            device,
            ctx.encoder,
            2,
            &targets.composite,
            &targets.color,
            true,
        );
    }
    if effective.composite.heat {
        transparent.encode_heat(&mut ctx, &targets.composite);
    }
    // The frame's one exposure, which FSR and the tone map read.
    exposure.encode(&mut ctx, &targets.composite);
    let completed_kind = antialiasing.encode(&mut ctx, post_fx.as_deref_mut(), exposure.texture());
    if antialiasing.take_failure() {
        post.forget_inputs();
        motion_blur.forget_inputs();
    }
    #[cfg(any(test, feature = "diagnostics"))]
    {
        *diagnostic_completed = Some(completed_kind);
    }
    let antialiased = completed(completed_kind, post_fx, antialiasing, targets);
    let completed_view = motion_blur
        .encode(&mut ctx, antialiased)
        .unwrap_or(antialiased);
    #[cfg(feature = "diagnostics")]
    if let Some(probe) = probe.as_deref() {
        probe.observe(
            device,
            ctx.encoder,
            3,
            completed_view,
            completed_view,
            false,
        );
    }
    post.encode(
        &mut ctx,
        completed_view,
        completed_kind,
        exposure.view(),
        output,
    );
    #[cfg(feature = "diagnostics")]
    if let Some(probe) = probe.as_deref() {
        probe.observe(
            device,
            ctx.encoder,
            4,
            post.tone_mapped(),
            completed_view,
            post.tone_mapped().texture().size() == completed_view.texture().size(),
        );
        probe.finish(ctx.encoder);
    }
}

/// The opaque stage in the stage order's named parts: the G-buffer, then,
/// while they run, the ray-traced shadows of the frame's slots, which may
/// hold `slot_lights`, the lighting at its depth, which takes their mask,
/// then ambient occlusion over it, then screen-space indirect light. While
/// occlusion culling runs, the
/// G-buffer part runs per phase with the cull stage between them: the
/// G-buffer over the early set, the cull stage's late phase (the pyramid
/// from that depth, then the late cull), the G-buffer over the late set,
/// then the cull stage's pyramid from the complete depth, for the next
/// frame; the shadows trace that complete depth.
pub(super) fn encode_opaque(
    opaque: &mut Opaque,
    cull: &mut Cull,
    traced_shadows: &mut TracedShadows,
    slot_lights: &SlotLights,
    ctx: &mut FrameContext<'_>,
) {
    opaque.encode_gbuffer(ctx, Phase::Early);
    if ctx.effective.occlusion_culling {
        cull.encode_late(ctx);
        opaque.encode_gbuffer(ctx, Phase::Late);
        cull.encode_pyramid(ctx);
    }
    let shadow_mask = traced_shadows.encode(ctx, slot_lights);
    opaque.encode_lighting(ctx, shadow_mask);
    opaque.encode_ambient_occlusion(ctx);
    opaque.encode_screen_indirect(ctx);
}

/// The view holding `kind`.
pub(super) fn completed<'a>(
    kind: Completed,
    post_fx: &'a Option<Box<PostFx>>,
    antialiasing: &'a crate::stages::antialiasing::Antialiasing,
    targets: &'a SharedTargets,
) -> &'a wgpu::TextureView {
    match kind {
        Completed::Taa => post_fx.as_ref().unwrap().taa_output(),
        Completed::Fsr => antialiasing.output(),
        Completed::Composite => &targets.composite,
    }
}
