//! One camera's frame: its targets and sizes, its stages with their state,
//! group 0's bindings, and every history kept in screen space or fit to its
//! camera, which `frame::render_view` renders after the frame's shared
//! scene work (`shared::Shared`).
use super::RendererError;
use super::effective;
use super::shared::Shared;
use crate::settings::Settings;
use crate::stages::{
    antialiasing, cull::Cull, exposure::Exposure, fog::VolumetricFog, motion_blur::MotionBlur,
    opaque::Opaque, post::Post, reflections::Reflections, shadows::Shadows,
    shadows::traced::TracedShadows, transparent::Transparent,
};
use crate::view::FrameViews;
use crate::view::bindings::FrameBindings;
use crate::view::draw_list::readback::StatisticsReadback;
use crate::view::effective::Effective;
use crate::view::history::{CameraFrame, CameraHistory, HistoryFrame};
use crate::view::post_fx::PostFx;
use crate::view::targets::{SharedTargets, Sizes};
use crate::{FrameInput, Scene};

pub(super) struct View {
    pub output_encodes_srgb: bool,
    pub sizes: Sizes,
    /// Whether the post stage's bloom targets were made full size.
    pub bloom_targets: bool,
    pub targets: SharedTargets,
    pub views: FrameViews,
    pub bindings: FrameBindings,
    /// DiligentFX's post-effect context, while TAA or Crystal SSR runs.
    pub post_fx: Option<PostFx>,
    pub cull: Cull,
    /// The camera's geometry statistics, read back without blocking.
    pub statistics: StatisticsReadback,
    pub shadows: Shadows,
    pub traced_shadows: TracedShadows,
    pub fog: VolumetricFog,
    pub opaque: Opaque,
    pub reflections: Reflections,
    pub transparent: Transparent,
    pub exposure: Exposure,
    pub antialiasing: antialiasing::Antialiasing,
    pub motion_blur: MotionBlur,
    pub post: Post,
    pub history: CameraHistory,
    /// Targets changed since the last finished frame was rendered: history
    /// restarts.
    pub pending_reset: bool,
    /// Counts target changes, so `finish` keeps a reset requested after the
    /// frame it commits was rendered.
    pub targets_generation: u64,
    /// The scene of the last finished frame.
    pub last_scene: Option<u64>,
    /// The last rendered frame, which `finish` commits.
    pub rendered: Option<Pending>,
    /// The numerical frame probe, once a frame asked for it.
    #[cfg(feature = "diagnostics")]
    pub probe: Option<crate::stages::frame_probe::FrameProbe>,
    /// Which target completed the last rendered frame's scene
    /// (`DiagnosticTarget::Antialiased`).
    #[cfg(any(test, feature = "diagnostics"))]
    pub completed: Option<crate::view::frame::Completed>,
}

/// A rendered frame the view has not settled: its history, its scene and
/// the targets generation it rendered with.
pub(super) struct Pending {
    pub history: HistoryFrame,
    pub scene: u64,
    pub generation: u64,
}

impl View {
    /// A view rendering into `output` at `output_size` physical pixels at
    /// `device_scale`, with stages built for the frame `settings` describe
    /// over `shared`'s layouts and dynamic GI probes.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shared: &Shared,
        output: crate::stages::post::Output,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, RendererError> {
        let scene = effective::scene_size(
            output_size,
            settings.preset,
            settings.scene_resolution,
            device_scale,
        );
        let sizing = effective::sizing(settings);
        let mut antialiasing = antialiasing::Antialiasing::default();
        let (render, _) = antialiasing.resize(device, scene, sizing.fsr);
        let sizes = Sizes {
            render,
            scene,
            output: output_size,
        };
        let layouts = &shared.layouts;
        let views = FrameViews::new(device);
        let shadows = Shadows::new(device, settings.shadow_quality);
        let fog = VolumetricFog::new(device, &layouts.lit.layout);
        let bindings = FrameBindings::new(
            device,
            layouts,
            &views,
            shadows.maps(),
            fog.volume(),
            shared
                .dynamic_gi
                .as_ref()
                .map(crate::stages::dynamic_gi::DynamicGi::probes),
        );
        let targets = SharedTargets::new(device, render, false);
        let cull = Cull::new(device);
        let exposure = Exposure::new(device);
        let first_frame = effective::first_frame(
            settings,
            shared.device(antialiasing.fsr_running(), cull.occlusion_supported()),
        );
        Ok(Self {
            output_encodes_srgb: output.encodes_srgb,
            opaque: Opaque::new(device, &bindings.unlit, &bindings.shadow_mask),
            traced_shadows: TracedShadows::new(device, &bindings.lit, &bindings.scene),
            reflections: Reflections::new(device, queue, render, &first_frame),
            transparent: Transparent::new(
                device,
                &bindings.unlit,
                &bindings.blended,
                &targets,
                exposure.view(),
                bindings.lit.tier,
            ),
            exposure,
            antialiasing,
            motion_blur: MotionBlur::new(device),
            post: Post::new(
                device,
                queue,
                output,
                sizes,
                sizing.bloom_targets,
                settings.smaa_quality,
            )
            .map_err(RendererError::LookupTextures)?,
            cull,
            statistics: StatisticsReadback::default(),
            sizes,
            bloom_targets: sizing.bloom_targets,
            targets,
            views,
            bindings,
            post_fx: None,
            shadows,
            fog,
            history: CameraHistory::default(),
            pending_reset: false,
            targets_generation: 0,
            last_scene: None,
            rendered: None,
            #[cfg(feature = "diagnostics")]
            probe: None,
            #[cfg(any(test, feature = "diagnostics"))]
            completed: None,
        })
    }

    /// `Renderer::resize`: sizes the targets and starts or stops FSR.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) {
        let scene = effective::scene_size(
            output_size,
            settings.preset,
            settings.scene_resolution,
            device_scale,
        );
        let sizing = effective::sizing(settings);
        let (render, restarted) = self.antialiasing.resize(device, scene, sizing.fsr);
        if restarted {
            self.pending_reset = true;
            self.targets_generation = self.targets_generation.wrapping_add(1);
            self.post.forget_inputs();
            self.motion_blur.forget_inputs();
            self.antialiasing.forget_inputs();
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
        self.antialiasing.forget_inputs();
        self.post.resize(device, sizes, sizing.bloom_targets);
        self.transparent
            .resize(device, &self.targets, self.exposure.view());
        self.reflections.resize(device, render);
        self.sizes = sizes;
        self.bloom_targets = sizing.bloom_targets;
        self.pending_reset = true;
        self.targets_generation = self.targets_generation.wrapping_add(1);
    }

    /// Begins the view's frame of `scene` seen as `input` with `settings`:
    /// settles a render `finish` did not follow as unfinished, begins its
    /// history (which restarts for a camera cut, a resize or another
    /// scene), and resolves its effective configuration, holding the
    /// surface's targets from a scene's first receiver on and the temporal
    /// masks while a writer can mark them.
    pub fn begin(
        &mut self,
        device: &wgpu::Device,
        shared: &Shared,
        scene: &Scene,
        input: &FrameInput,
        settings: &Settings,
    ) -> (HistoryFrame, Effective) {
        if self.rendered.take().is_some() {
            self.settle(false);
        }
        let history = self.begin_history(scene, input);
        if scene.materials.holds_receivers() {
            self.targets.hold_surface(device);
        }
        let effective = effective::resolve(
            settings,
            input,
            effective::SceneContent::of(scene),
            shared.device(
                self.antialiasing.fsr_running(),
                self.cull.occlusion_supported(),
            ),
        );
        self.targets
            .set_temporal_masks(device, effective.temporal_masks);
        (history, effective)
    }

    /// The history of a frame of `scene` seen by `input`'s camera, in the
    /// render frame of the scene's origin.
    pub fn begin_history(&self, scene: &Scene, input: &FrameInput) -> HistoryFrame {
        let reset = input.camera_cut || self.pending_reset || self.last_scene != Some(scene.id);
        let camera = CameraFrame {
            view: input.camera.view,
            projection: input.camera.projection,
            jitter: [0.; 2],
        };
        let mut frame = self.history.begin(
            camera,
            reset,
            scene.origin(),
            input.elapsed_seconds,
            input.visibility_mask,
        );
        let yaw = match input.backdrop {
            crate::Backdrop::Environment { yaw, .. } => Some(yaw),
            crate::Backdrop::Color(_) => None,
        };
        self.history.set_backdrop(&mut frame, yaw);
        frame
    }

    /// Commits the last rendered frame after the caller submitted it, if
    /// one is pending: its camera history, its reset, its scene and every
    /// stage's committed state. Whether one was.
    pub fn finish(&mut self) -> bool {
        let Some(Pending {
            history,
            scene,
            generation,
        }) = self.rendered.take()
        else {
            return false;
        };
        self.history.finish(history);
        // A resize after the render still restarts the next frame.
        if generation == self.targets_generation {
            self.pending_reset = false;
        }
        self.last_scene = Some(scene);
        self.shadows.local.finish_frame();
        self.cull.finish_frame();
        self.exposure.finish_frame();
        self.statistics.submitted();
        #[cfg(feature = "diagnostics")]
        if let Some(probe) = &mut self.probe {
            probe.submitted();
        }
        self.settle(true);
        true
    }

    /// Settles the last render, `finished` or not, in every stage that keeps
    /// what describes its GPU history as the last finished frame left it
    /// (`view::history::Kept`): exactly once per render, before the next
    /// render or at `finish`. The History contract's "What a frame commits".
    pub fn settle(&mut self, finished: bool) {
        if let Some(post_fx) = &mut self.post_fx {
            post_fx.settle(finished);
        }
        self.antialiasing.settle(finished);
        self.reflections.settle(finished);
        self.opaque.settle(finished);
        self.traced_shadows.settle(finished);
    }
}
