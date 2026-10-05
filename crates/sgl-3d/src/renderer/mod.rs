//! The renderer: it owns the stages, selects and orders them each frame from
//! the effective configuration (`frame.rs`, resolved in `effective.rs`), and
//! owns and lends them what they share (`view`): the frame's targets and
//! sizes, group 0's bindings, the geometry pipeline cache, the frame's views,
//! the camera history and DiligentFX's post-effect context. A probe capture
//! (`probe_capture.rs`) orders the prepare, shadow and opaque stages and the
//! probe prefilter over its own views. It holds no pipelines or shaders of
//! its own.
pub(crate) mod effective;
pub(crate) mod frame;
pub(crate) mod probe_capture;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod test_frame;

#[cfg(all(test, feature = "diagnostics", not(target_arch = "wasm32")))]
mod diagnostics_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod floor_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod size_tests;

use crate::scene::rays::acceleration::RayTracingStats;
use crate::settings::{Antialiasing, RenderPreset, SceneResolution, Settings};
use crate::stages::shadows::local::LocalShadowStats;
use crate::stages::{
    antialiasing, cull::Cull, deform::Deform, dynamic_gi::DynamicGi, exposure::Exposure,
    fog::VolumetricFog, motion_blur::MotionBlur, opaque::Opaque, post::Post, prepare::Prepare,
    reflections::Reflections, shadows::Shadows, shadows::traced::TracedShadows,
    transparent::Transparent,
};
use crate::view::FrameViews;
use crate::view::bindings::FrameBindings;
use crate::view::draw_list::GeometryStats;
use crate::view::draw_list::readback::StatisticsReadback;
use crate::view::history::{CameraFrame, CameraHistory, HistoryFrame};
use crate::view::pipelines::{GeometryPipelines, LayerConstants};
use crate::view::post_fx::PostFx;
use crate::view::targets::{SharedTargets, Sizes};
use crate::{FrameInput, Scene};
#[cfg(feature = "diagnostics")]
use crate::{ModelId, SceneError};

/// Renders a [`Scene`] into a caller's texture, one frame at a time: call
/// `resize` (cheap when nothing changed), then `render` into an encoder, and
/// `finish_frame` after submitting it. It owns every target, pipeline and
/// history; the scene owns content.
pub struct Renderer {
    sizes: Sizes,
    /// Whether the post stage's bloom targets were made full size.
    bloom_targets: bool,
    targets: SharedTargets,
    views: FrameViews,
    bindings: FrameBindings,
    pipelines: GeometryPipelines,
    /// DiligentFX's post-effect context, while TAA or Crystal SSR runs.
    post_fx: Option<PostFx>,
    prepare: Prepare,
    deform: Deform,
    cull: Cull,
    /// The camera's geometry statistics, read back without blocking.
    statistics: StatisticsReadback,
    dynamic_gi: DynamicGi,
    shadows: Shadows,
    traced_shadows: TracedShadows,
    fog: VolumetricFog,
    opaque: Opaque,
    reflections: Reflections,
    transparent: Transparent,
    exposure: Exposure,
    antialiasing: antialiasing::Antialiasing,
    motion_blur: MotionBlur,
    post: Post,
    history: CameraHistory,
    /// The device traces rays in hardware, in this form.
    ray_queries: Option<crate::shading::RayQueryForm>,
    /// Targets changed since the last finished frame: history restarts.
    pending_reset: bool,
    /// The scene of the last finished frame.
    last_scene: Option<u64>,
    /// The last rendered frame's history and scene, which `finish_frame`
    /// commits.
    rendered: Option<(HistoryFrame, u64)>,
    /// The numerical frame probe, once a frame asked for it.
    #[cfg(feature = "diagnostics")]
    probe: Option<crate::stages::frame_probe::FrameProbe>,
}

/// Why a renderer could not be created.
#[derive(Debug)]
pub enum RendererError {
    /// SMAA's lookup textures did not decode.
    LookupTextures(image::ImageError),
}

impl std::fmt::Display for RendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LookupTextures(error) => write!(f, "SMAA lookup textures: {error}"),
        }
    }
}

impl std::error::Error for RendererError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LookupTextures(error) => Some(error),
        }
    }
}

/// The scene size for an output of `size`.
fn scene_size(
    size: [u32; 2],
    preset: RenderPreset,
    resolution: SceneResolution,
    device_scale: f32,
) -> [u32; 2] {
    let scale = match resolution {
        SceneResolution::Preset => {
            let cap = if preset == RenderPreset::Low {
                1.
            } else {
                1.75
            };
            (cap / device_scale.max(1.)).min(1.)
        }
        SceneResolution::Hd => (1280. / size[0].max(1) as f32)
            .min(720. / size[1].max(1) as f32)
            .min(1.),
        SceneResolution::FullHd => (1920. / size[0].max(1) as f32)
            .min(1080. / size[1].max(1) as f32)
            .min(1.),
        SceneResolution::Full => 1.,
        SceneResolution::ThreeQuarter => 0.75,
        SceneResolution::Half => 0.5,
    };
    size.map(|x| ((x as f32 * scale).floor() as u32).max(1))
}

impl Renderer {
    /// A renderer presenting to `output_format` at `output_size` physical
    /// pixels, for a window of `device_scale` physical pixels per logical
    /// pixel, sized for `settings`. Reflection source completion and SMAA are
    /// built for them, so a first frame from a `perspective` camera does not
    /// rebuild them.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_format: wgpu::TextureFormat,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, RendererError> {
        let scene = scene_size(
            output_size,
            settings.preset,
            settings.scene_resolution,
            device_scale,
        );
        let sizing = effective::sizing(settings);
        let mut antialiasing = antialiasing::Antialiasing::default();
        let (render, _) = antialiasing.resize(device, scene, sizing.fsr2);
        let sizes = Sizes {
            render,
            scene,
            output: output_size,
        };
        let views = FrameViews::new(device);
        let shadows = Shadows::new(device, settings.shadow_quality);
        let lit = crate::shading::bind::lit(device);
        let fog = VolumetricFog::new(device, &lit);
        let scene_layout = crate::shading::bind::scene(device);
        let dynamic_gi = DynamicGi::new(device, &lit, &scene_layout);
        let bindings = FrameBindings::new(
            device,
            lit,
            &views,
            shadows.maps(),
            fog.volume(),
            dynamic_gi.probes(),
        );
        let layers = LayerConstants::new(&settings.diagnostics_in_effect().disable);
        let pipelines = GeometryPipelines::new(
            device,
            [
                &bindings.lit,
                &bindings.shadow,
                &bindings.scene,
                &bindings.material,
                &bindings.blended,
                &bindings.shadow_mask,
            ],
            layers,
        );
        let targets = SharedTargets::new(device, render, false);
        // Every native backend runs the baseline form
        // (`shading::RayQueryForm`).
        let ray_queries = crate::scene::rays::acceleration::supported(device)
            .then_some(crate::shading::RayQueryForm::Baseline);
        let first_frame = effective::first_frame(
            settings,
            effective::Device {
                fsr2_running: antialiasing.fsr2_running(),
                fused_supported: pipelines.fused_supported,
                ray_queries,
            },
        );
        Ok(Self {
            opaque: Opaque::new(device, &bindings.unlit, &bindings.shadow_mask),
            traced_shadows: TracedShadows::new(device, &bindings.lit, &bindings.scene),
            reflections: Reflections::new(device, queue, render, &first_frame),
            transparent: Transparent::new(device, &bindings.unlit, &bindings.blended, &targets),
            exposure: Exposure::new(device),
            antialiasing,
            motion_blur: MotionBlur::new(device),
            post: Post::new(
                device,
                queue,
                output_format,
                sizes,
                sizing.bloom_targets,
                settings.smaa_quality,
            )
            .map_err(RendererError::LookupTextures)?,
            prepare: Prepare::default(),
            deform: Deform::new(device),
            cull: Cull::new(device),
            statistics: StatisticsReadback::default(),
            dynamic_gi,
            sizes,
            bloom_targets: sizing.bloom_targets,
            targets,
            views,
            bindings,
            pipelines,
            post_fx: None,
            shadows,
            fog,
            history: CameraHistory::default(),
            ray_queries,
            pending_reset: false,
            last_scene: None,
            rendered: None,
            #[cfg(feature = "diagnostics")]
            probe: None,
        })
    }

    /// Sizes the targets for an output of `output_size` physical pixels at
    /// `device_scale` and the size-affecting settings, and starts or stops
    /// FSR2. Cheap when nothing changed, so a game calls it every frame;
    /// when the targets change, history restarts.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) {
        let scene = scene_size(
            output_size,
            settings.preset,
            settings.scene_resolution,
            device_scale,
        );
        let sizing = effective::sizing(settings);
        let (render, restarted) = self.antialiasing.resize(device, scene, sizing.fsr2);
        if restarted {
            self.post.forget_inputs();
            self.motion_blur.forget_inputs();
        }
        let sizes = Sizes {
            render,
            scene,
            output: output_size,
        };
        if self.sizes == sizes && self.bloom_targets == sizing.bloom_targets {
            return;
        }
        self.targets = SharedTargets::new(device, render, self.targets.surface.is_some());
        self.exposure.forget_inputs();
        self.motion_blur.forget_inputs();
        self.post.resize(device, sizes, sizing.bloom_targets);
        self.transparent.resize(device, &self.targets);
        self.reflections.resize(device, render);
        self.sizes = sizes;
        self.bloom_targets = sizing.bloom_targets;
        self.pending_reset = true;
    }

    /// Encodes one frame of `scene` as `input` describes it into `output`, a
    /// view of the output format and size (offscreen or a surface's):
    /// prepare, shadows, fog, opaque, reflections and transparent, exposure,
    /// antialiasing, motion blur, post. History restarts for a camera cut, after a resize that changed
    /// the targets and for another scene. `scene` is mutable for its
    /// per-frame GPU mirrors.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &mut Scene,
        input: &FrameInput,
        settings: &Settings,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        frame::render(
            self, device, queue, encoder, scene, input, settings, output, timing,
        );
    }

    /// After the caller submitted the last rendered frame of `scene`: it
    /// becomes the history of the next, camera and instances alike. A frame
    /// rendered but never finished (an abandoned encoder) leaves history
    /// untouched.
    pub fn finish_frame(&mut self, scene: &mut Scene) {
        if let Some((frame, scene_id)) = self.rendered.take() {
            self.history.finish(frame);
            self.pending_reset = false;
            self.last_scene = Some(scene_id);
            scene.finish_frame();
            self.shadows.local.finish_frame();
            self.dynamic_gi.finish_frame();
            self.statistics.submitted();
            #[cfg(feature = "diagnostics")]
            if let Some(probe) = &mut self.probe {
                probe.submitted();
            }
        }
    }

    /// The history of a frame of `scene` seen by `input`'s camera, in the
    /// render frame of the scene's origin.
    fn begin_history(&self, scene: &Scene, input: &FrameInput) -> HistoryFrame {
        let reset = input.camera_cut || self.pending_reset || self.last_scene != Some(scene.id);
        let camera = CameraFrame {
            view: input.camera.view,
            projection: input.camera.projection,
            jitter: [0.; 2],
        };
        self.history.begin(camera, reset, scene.origin())
    }

    /// The antialiasing that runs for `settings`: their choice resolved for
    /// the preset, with TAA where FSR2 is chosen but not running (see
    /// `fsr2_error`, or awaiting `resize`). The saved choice is unchanged.
    pub fn antialiasing_in_effect(&self, settings: &Settings) -> Antialiasing {
        effective::antialiasing(settings, self.antialiasing.fsr2_running())
    }

    /// Why FSR2 is not running on this device although it was chosen.
    pub fn fsr2_error(&self) -> Option<&str> {
        self.antialiasing.error()
    }

    /// The size every pass up to antialiasing renders at: the scene size,
    /// or FSR2's render size while it upscales.
    pub fn render_size(&self) -> [u32; 2] {
        self.sizes.render
    }

    /// The size motion blur, bloom, SMAA and tone mapping run at
    /// (`SceneResolution`).
    pub fn scene_size(&self) -> [u32; 2] {
        self.sizes.scene
    }

    /// The camera's submitted geometry in the most recent completed frame:
    /// its opaque and masked surfaces counted on the GPU and read back
    /// without blocking, the map requested at `finish_frame`, with its
    /// blended surfaces' counts from the same frame. None until a frame's
    /// readback has arrived, which takes a few frames.
    pub fn geometry_stats(&mut self, device: &wgpu::Device) -> Option<GeometryStats> {
        self.statistics.poll(device);
        self.statistics.latest()
    }

    /// The last rendered frame's local-light shadows: how many casting
    /// lights in the camera's view have a shadow, how many the atlas had no
    /// room for, and what the frame drew.
    pub fn local_shadow_stats(&self) -> LocalShadowStats {
        self.shadows.local.stats()
    }

    /// What the last rendered frame's hardware ray tracing held: how many
    /// capture-visible instances the scene's TLAS held, how many that do not
    /// deform it did not hold, which the portable BVHs cover, and how many
    /// the device's limits or memory left out. Zero for a frame that built
    /// no acceleration structures: one without ray tracing hardware
    /// (`graphics_device::ray_tracing_features`), with
    /// `Settings::hardware_ray_tracing` off, or tracing no rays.
    pub fn ray_tracing_stats(&self) -> RayTracingStats {
        self.prepare.ray_tracing_stats()
    }

    /// Whether the hardware path traces the scene's rays for `settings`:
    /// `Settings::hardware_ray_tracing` is on, the device has ray queries
    /// (`graphics_device::ray_tracing_features`) and nothing stopped it
    /// (`ray_tracing_error`). Otherwise the portable BVHs trace them. The
    /// saved choice is unchanged.
    pub fn ray_tracing_in_effect(&self, settings: &Settings) -> bool {
        settings.hardware_ray_tracing
            && self.ray_queries.is_some()
            && self.prepare.ray_tracing_error().is_none()
    }

    /// Whether the camera's opaque surfaces take ray-traced shadows for
    /// `settings`: `Settings::ray_traced_shadows` is on and hardware ray
    /// tracing is in effect (`ray_tracing_in_effect`). Otherwise the shadow
    /// maps shadow them. The saved choice is unchanged.
    pub fn ray_traced_shadows_in_effect(&self, settings: &Settings) -> bool {
        settings.ray_traced_shadows && self.ray_tracing_in_effect(settings)
    }

    /// Why the hardware path did not trace the last rendered frame's rays
    /// although `Settings::hardware_ray_tracing` was on: the device has no
    /// ray queries, or its memory could not hold the scene's TLAS. The
    /// portable BVHs traced them instead.
    pub fn ray_tracing_error(&self) -> Option<&str> {
        self.prepare.ray_tracing_error()
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn targets(&self) -> &SharedTargets {
        &self.targets
    }

    /// The fog's froxels the last frame wrote, the froxels it integrated
    /// and its integrated volume.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn fog_volumes(&self) -> [&wgpu::TextureView; 3] {
        self.fog.test_volumes()
    }

    /// FSR2's upscaled frame of the last frame it ran.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn fsr2_output(&self) -> &wgpu::TextureView {
        self.antialiasing.output()
    }

    /// The motion-blurred frame, once motion blur has run.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn motion_blurred(&self) -> Option<&wgpu::TextureView> {
        self.motion_blur.output()
    }

    /// The tone-mapped scene before presentation, when diagnostics capture it.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn tone_mapped(&self) -> &wgpu::TextureView {
        self.post.tone_mapped()
    }
}

#[cfg(any(test, feature = "diagnostics"))]
impl Renderer {
    /// `target` as the last frame left it, at the render size unless the
    /// target says otherwise; `None` when it did not run.
    pub fn diagnostic_target(
        &self,
        target: crate::diagnostics::DiagnosticTarget,
    ) -> Option<&wgpu::TextureView> {
        use crate::diagnostics::DiagnosticTarget;
        let targets = &self.targets;
        Some(match target {
            DiagnosticTarget::Depth => &targets.depth,
            DiagnosticTarget::Normal => &targets.normal,
            DiagnosticTarget::Motion => &targets.motion,
            DiagnosticTarget::SourceId => &targets.source_id,
            DiagnosticTarget::Composite => &targets.composite,
            DiagnosticTarget::AmbientOcclusion => return self.opaque.visibility(),
            DiagnosticTarget::ToneMapped => return self.post.captured_tone_mapped(),
            DiagnosticTarget::Exposure => self.exposure.view(),
        })
    }

    /// The draws the last frame's camera, blended and directional-cascade
    /// views encoded: a CPU-built list's draws, and a GPU-built list's
    /// indirect draws, one per set; local-light shadow faces, probe captures
    /// and full-screen passes are not counted.
    pub fn diagnostic_draws(&self) -> crate::diagnostics::ViewDraws {
        let views = &self.views;
        crate::diagnostics::ViewDraws {
            camera: views.camera.list.draws(),
            blended: views.blended.draws(),
            cascades: views.cascades[..views.cascade_count]
                .iter()
                .map(|cascade| cascade.list.draws())
                .collect(),
        }
    }

    /// The CPU time the last frame took building and recording its camera's
    /// opaque and masked draw list and each directional cascade's.
    #[cfg(feature = "diagnostics")]
    pub fn diagnostic_view_times(&self) -> crate::diagnostics::ViewTimes {
        let views = &self.views;
        crate::diagnostics::ViewTimes {
            camera: views.camera.cpu_ms(),
            cascades: views.cascades[..views.cascade_count]
                .iter()
                .map(crate::view::ViewSlot::cpu_ms)
                .collect(),
        }
    }

    /// The camera's submitted geometry of `scene`'s instances of `model` in
    /// the most recent completed frame (`geometry_stats`): the sections its
    /// instances' candidates appended and their triangles, read back from
    /// the GPU, with the blended draws that hold one and the triangles they
    /// submit for them. None until a frame's readback has arrived.
    #[cfg(feature = "diagnostics")]
    pub fn geometry_stats_for_model(
        &mut self,
        device: &wgpu::Device,
        scene: &Scene,
        model: ModelId,
    ) -> Result<Option<(usize, u64)>, SceneError> {
        scene.models.get(model)?;
        self.statistics.poll(device);
        Ok(self.statistics.latest_for(model))
    }

    /// The dynamic GI stage's observed frames (`Diagnostics::dynamic_gi`)
    /// read back since the last call, oldest first. Readback is
    /// asynchronous: a frame's report arrives once the device completed it,
    /// and waits here until taken.
    #[cfg(feature = "diagnostics")]
    pub fn take_dynamic_gi_reports(
        &mut self,
        device: &wgpu::Device,
    ) -> Vec<crate::diagnostics::DynamicGiReport> {
        self.dynamic_gi.take_reports(device)
    }

    /// The numerical frame probe's reports of finished frames read back
    /// since the last call, one JSON object per frame, oldest first
    /// (`Diagnostics::frame_probe`). Readback is asynchronous: a frame's
    /// report arrives once the device completed it. Reports wait here until
    /// taken.
    #[cfg(feature = "diagnostics")]
    pub fn take_frame_probe_reports(&mut self, device: &wgpu::Device) -> Vec<serde_json::Value> {
        self.probe
            .as_mut()
            .map_or_else(Vec::new, |probe| probe.take_reports(device))
    }
}
