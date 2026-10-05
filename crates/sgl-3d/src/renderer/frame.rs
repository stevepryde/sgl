//! One frame: the one ordered render body.
use super::Renderer;
use crate::settings::{ReflectionMethod, Settings};
use crate::stages::opaque::Opaque;
use crate::stages::shadows::traced::TracedShadows;
use crate::stages::shadows::traced::slots::SlotLights;
use crate::timing::GpuTiming;
use crate::view::frame::{Completed, FrameContext, HardwareRays};
use crate::view::pipelines::GeometryPipelines;
use crate::view::post_fx::PostFx;
use crate::view::targets::SharedTargets;
use crate::{FrameInput, Scene};

/// Encodes one frame of `scene` seen as `input` into `output`: prepare (with
/// its deformation, acceleration structures and the cull stage's early
/// phase), dynamic GI, shadows, volumetric fog, opaque (with the ray-traced
/// shadows between its G-buffer and lighting while they run), the transparent
/// stage's receivers, reflections with the transparent stage drawn into
/// their input (while a screen-space method traces it) and onto their
/// result, heat, exposure, antialiasing, motion blur, then post.
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
    output: &wgpu::TextureView,
    timing: Option<&GpuTiming>,
) {
    // History restarts for a camera cut, a resize or another scene.
    let mut history = renderer.begin_history(scene, input);
    let Renderer {
        sizes,
        targets,
        views,
        bindings,
        pipelines,
        post_fx,
        prepare,
        deform,
        cull,
        statistics,
        dynamic_gi,
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
        ray_form,
        #[cfg(feature = "diagnostics")]
        probe,
        ..
    } = renderer;
    // The surface's own targets, from a scene's first receiver on.
    if scene.materials.holds_receivers() {
        targets.hold_surface(device);
    }
    let targets: &SharedTargets = targets;
    let effective = super::effective::resolve(
        settings,
        input,
        super::effective::SceneContent::of(scene),
        super::effective::Device {
            fsr2_running: antialiasing.fsr2_running(),
            fused_supported: pipelines.fused_supported,
            ray_queries: ray_form.as_ref().map(|form| form.form()),
        },
    );
    pipelines.specialise(
        device,
        effective.layers,
        scene,
        effective.ray_traced_shadows,
    );
    let pipelines: &GeometryPipelines = pipelines;
    scene.materials.set_anisotropy(device, effective.anisotropy);
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
        *post_fx = Some(PostFx::new(device, queue, effective.taa));
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
        post_fx.as_mut(),
        context_jitter,
    );
    // The camera history commits the jitter this frame applies.
    history.camera.jitter = jitter.map_or([0.; 2], |jitter| jitter.ndc);
    *rendered = Some((history, scene.id));
    // The camera's statistics: those completed so far, and none copied yet
    // this frame.
    statistics.poll(device);
    statistics.begin();
    let values = prepare.run(
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
        values.frame.visibility_mask,
        effective.local_lights,
    );
    // Every list the frame draws is built.
    views.instances.upload(device, queue);
    fog.prepare(device, effective.fog, sizes.render);
    dynamic_gi.prepare(device, scene, &effective);
    // The ray-traced shadows run where the frame's rays trace in hardware
    // and its slots hold a light: the directional light with cascades and
    // the local lights the atlas placed.
    let slot_lights = SlotLights::of(input, &values.frame, scene, shadows.local.ranking());
    let effective =
        super::effective::traced_shadows(effective, prepare.hardware_rays(), &slot_lights);
    // After prepare, the local shadows', the fog's and dynamic GI's, which
    // may replace a light cluster buffer, a shadow map, the shadow records,
    // the fog volume or the probes group 0 binds.
    bindings.refresh(
        device,
        scene,
        input.environment,
        views,
        shadows.maps(),
        fog.volume(),
        dynamic_gi.probes(),
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
        scene: &*scene,
        values: &values,
        input,
        views: &*views,
        bindings,
        pipelines,
        history,
        hardware_rays: HardwareRays::of(
            &effective,
            scene,
            prepare.hardware_rays(),
            ray_form.as_ref(),
        ),
    };
    // Prepare's GPU steps, before any pass draws scene geometry or traces:
    // the deformations, then the acceleration structures over them, then
    // the cull stage's early phase for every GPU-built view.
    deform.encode(&mut ctx);
    prepare.encode_acceleration_structures(&mut ctx);
    cull.encode_early(&mut ctx);
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
    // First after prepare: every pass that shades reads the probes.
    dynamic_gi.encode(&mut ctx);
    // The stage order's: the local-light atlas, then the directional cascades.
    shadows.encode_local(&mut ctx);
    shadows.encode_directional(&mut ctx);
    fog.encode(&mut ctx);
    encode_opaque(opaque, traced_shadows, &slot_lights, &mut ctx);
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
    let reflected = reflections.resolve(&mut ctx, post_fx.as_mut(), ambient_occlusion);
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
    if effective.heat {
        transparent.encode_heat(&mut ctx, &targets.composite);
    }
    // The frame's one exposure, which FSR2 and the tone map read.
    exposure.encode(&mut ctx, &targets.composite);
    let completed_kind = antialiasing.encode(&mut ctx, post_fx.as_mut(), exposure.texture());
    if antialiasing.take_failure() {
        post.forget_inputs();
        motion_blur.forget_inputs();
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
            sizes.output == sizes.scene,
        );
        probe.finish(ctx.encoder);
    }
}

/// The opaque stage in the stage order's named parts: the G-buffer, then,
/// while they run, the ray-traced shadows of the frame's slots, which may
/// hold `slot_lights`, the lighting at its depth, which takes their mask,
/// then ambient occlusion over it.
pub(super) fn encode_opaque(
    opaque: &mut Opaque,
    traced_shadows: &mut TracedShadows,
    slot_lights: &SlotLights,
    ctx: &mut FrameContext<'_>,
) {
    opaque.encode_gbuffer(ctx);
    let shadow_mask = traced_shadows.encode(ctx, slot_lights);
    opaque.encode_lighting(ctx, shadow_mask);
    opaque.encode_ambient_occlusion(ctx);
}

/// The view holding `kind`.
fn completed<'a>(
    kind: Completed,
    post_fx: &'a Option<PostFx>,
    antialiasing: &'a crate::stages::antialiasing::Antialiasing,
    targets: &'a SharedTargets,
) -> &'a wgpu::TextureView {
    match kind {
        Completed::Taa => post_fx.as_ref().unwrap().taa_output(),
        Completed::Fsr2 => antialiasing.output(),
        Completed::Composite => &targets.composite,
    }
}
