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

use crate::settings::{Antialiasing, RenderPreset, SceneResolution, Settings};
use crate::stages::shadows::local::LocalShadowStats;
use crate::stages::{
    antialiasing, deform::Deform, exposure::Exposure, fog::VolumetricFog, motion_blur::MotionBlur,
    opaque::Opaque, post::Post, prepare::Prepare, reflections::Reflections, shadows::Shadows,
    transparent::Transparent,
};
use crate::view::FrameViews;
use crate::view::bindings::FrameBindings;
use crate::view::draw_list::GeometryStats;
use crate::view::history::{CameraHistory, HistoryFrame};
use crate::view::pipelines::{GeometryPipelines, LayerConstants};
use crate::view::post_fx::PostFx;
use crate::view::targets::{SharedTargets, Sizes};
use crate::{FrameInput, ModelId, Scene, SceneError};

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
    shadows: Shadows,
    fog: VolumetricFog,
    opaque: Opaque,
    reflections: Reflections,
    transparent: Transparent,
    exposure: Exposure,
    antialiasing: antialiasing::Antialiasing,
    motion_blur: MotionBlur,
    post: Post,
    history: CameraHistory,
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
    /// pixel, sized for `settings`.
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
        let shadows = Shadows::new(device);
        let lit = crate::shading::bind::lit(device);
        let fog = VolumetricFog::new(device, &lit);
        let bindings = FrameBindings::new(device, lit, &views, shadows.maps(), fog.volume());
        let layers = LayerConstants::new(&settings.diagnostics_in_effect().disable);
        let pipelines = GeometryPipelines::new(
            device,
            [
                &bindings.lit,
                &bindings.shadow,
                &bindings.scene,
                &bindings.material,
            ],
            layers,
        );
        let targets = SharedTargets::new(device, render);
        Ok(Self {
            opaque: Opaque::new(device, &bindings.unlit),
            reflections: Reflections::new(device, queue, render),
            transparent: Transparent::new(device, &bindings.unlit, &targets),
            exposure: Exposure::new(device),
            antialiasing,
            motion_blur: MotionBlur::new(device),
            post: Post::new(device, queue, output_format, sizes, sizing.bloom_targets)
                .map_err(RendererError::LookupTextures)?,
            prepare: Prepare,
            deform: Deform::new(device),
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
        self.targets = SharedTargets::new(device, render);
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
            #[cfg(feature = "diagnostics")]
            if let Some(probe) = &mut self.probe {
                probe.submitted();
            }
        }
    }

    /// The history of a frame of `scene` seen by `input`'s camera.
    fn begin_history(&self, scene: &Scene, input: &FrameInput) -> HistoryFrame {
        let reset = input.camera_cut || self.pending_reset || self.last_scene != Some(scene.id);
        self.history
            .begin(input.camera.projection * input.camera.view, reset)
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

    /// The last rendered camera's submitted draws.
    pub fn geometry_stats(&self) -> GeometryStats {
        self.views.camera.list.stats.with(&self.views.blended.stats)
    }

    /// The last rendered frame's local-light shadows: how many casting
    /// lights in the camera's view have a shadow, how many the atlas had no
    /// room for, and what the frame drew.
    pub fn local_shadow_stats(&self) -> LocalShadowStats {
        self.shadows.local.stats()
    }

    /// The last rendered camera's submitted draws of `scene`'s instances of
    /// `model`.
    pub fn geometry_stats_for_model(
        &self,
        scene: &Scene,
        model: ModelId,
    ) -> Result<(usize, u64), SceneError> {
        scene.models.get(model)?;
        let [opaque, blended] = [&self.views.camera.list, &self.views.blended]
            .map(|list| list.stats_for_model(scene, model));
        Ok((opaque.0 + blended.0, opaque.1 + blended.1))
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

    /// TAA's accumulated frame, with its history weight in alpha, while TAA
    /// runs.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn taa_output(&self) -> Option<&wgpu::TextureView> {
        self.post_fx
            .as_ref()
            .filter(|post_fx| post_fx.taa())
            .map(PostFx::taa_output)
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
