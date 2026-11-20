//! One camera's frame: its targets and sizes, its stages with their state,
//! group 0's bindings, and every history kept in screen space or fit to its
//! camera, which `frame::render_view` renders after the frame's shared
//! scene work (`shared::Shared`).
use super::Renderer;
use super::effective;
use super::shared::Shared;
use crate::settings::{Antialiasing, RenderPreset, Settings};
use crate::stages::shadows::local::LocalShadowStats;
use crate::stages::{
    antialiasing, cull::Cull, exposure::Exposure, fog::VolumetricFog, motion_blur::MotionBlur,
    opaque::Opaque, post::Post, reflections::Reflections, shadows::Shadows,
    shadows::contact::ContactShadowStage, shadows::traced::TracedShadows, sky::Sky,
    transparent::Transparent,
};
use crate::view::FrameViews;
use crate::view::bindings::FrameBindings;
use crate::view::draw_list::GeometryStats;
use crate::view::draw_list::readback::StatisticsReadback;
use crate::view::effective::Effective;
use crate::view::history::{CameraFrame, CameraHistory, HistoryFrame};
use crate::view::post_fx::PostFx;
use crate::view::targets::{SharedTargets, Sizes};
use crate::{FrameInput, Scene};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One view's part of a frame ([`Renderer::render`]): the view, its
/// camera and frame values, its settings and the texture it renders into,
/// a view of its output format and size (offscreen or a surface's). Scene
/// work is timed with the primary view's `timing`.
pub struct ViewFrame<'a> {
    pub view: &'a mut View,
    pub input: &'a FrameInput,
    pub settings: &'a Settings,
    pub output: &'a wgpu::TextureView,
    pub timing: Option<&'a crate::timing::GpuTiming>,
}

/// Why a view could not be created.
#[derive(Debug)]
pub enum ViewError {
    /// SMAA's lookup textures did not decode.
    LookupTextures(image::ImageError),
    /// The surface requires an unsupported transfer or format/colour-space pair.
    UnsupportedSurfaceOutput {
        format: wgpu::TextureFormat,
        color_space: wgpu::SurfaceColorSpace,
    },
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LookupTextures(error) => write!(f, "SMAA lookup textures: {error}"),
            Self::UnsupportedSurfaceOutput {
                format,
                color_space,
            } => {
                write!(
                    f,
                    "unsupported SGL3D surface output: {format:?} in {color_space:?}"
                )
            }
        }
    }
}

impl std::error::Error for ViewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LookupTextures(error) => Some(error),
            Self::UnsupportedSurfaceOutput { .. } => None,
        }
    }
}

/// One camera's frame of a scene: its targets and sizes, its stages with
/// their state, group 0's bindings, and every history kept in screen space
/// or fit to its camera. A game creates one for each image of the scene it
/// draws (a window, each half of a split screen, an inset, a rear view, an
/// editor's viewports), sizes it with [`View::resize`], lists it in
/// [`Renderer::render`] and drops it when it is done, which releases
/// everything it held. Its history continues only from its scene's last
/// finished frame, so a view that a finished frame of its scene did not
/// render restarts, as on a cut.
// The stages of more than 2 KB are boxed: a debug build copies a `View`
// several times on the stack as it is built and moved, in the game's code
// too, and the debug WASM stack is 1 MB (#839).
pub struct View {
    pub(crate) output_encodes_srgb: bool,
    pub(crate) sizes: Sizes,
    /// A change of preset restarts history even if the pixel sizes agree.
    preset: RenderPreset,
    pub(crate) targets: SharedTargets,
    pub(crate) views: Box<FrameViews>,
    pub(crate) bindings: FrameBindings,
    /// DiligentFX's post-effect context, while TAA or Crystal SSR runs.
    pub(crate) post_fx: Option<Box<PostFx>>,
    pub(crate) cull: Cull,
    /// The procedural sky's tables, while its frames name one.
    pub(crate) sky: Sky,
    /// The camera's geometry statistics, read back without blocking.
    pub(crate) statistics: StatisticsReadback,
    pub(crate) shadows: Shadows,
    pub(crate) traced_shadows: Box<TracedShadows>,
    pub(crate) contact_shadows: Box<ContactShadowStage>,
    pub(crate) fog: VolumetricFog,
    pub(crate) opaque: Opaque,
    pub(crate) reflections: Box<Reflections>,
    pub(crate) transparent: Transparent,
    pub(crate) exposure: Exposure,
    pub(crate) antialiasing: antialiasing::Antialiasing,
    pub(crate) motion_blur: MotionBlur,
    pub(crate) post: Box<Post>,
    pub(crate) history: CameraHistory,
    /// History restarts: the targets changed since the last finished frame
    /// was rendered, or the view's last render was a reset that was not
    /// finished. A reset holds until a frame is finished.
    pub(crate) pending_reset: bool,
    /// Counts target changes, so `settle` keeps a reset requested after the
    /// frame it commits was rendered.
    pub(crate) targets_generation: u64,
    /// The scene frame attempt of the view's last finished frame.
    pub(crate) finished: Option<u64>,
    /// The last rendered frame, which `settle` commits or drops.
    pub(crate) rendered: Option<Pending>,
    /// The identity of the renderer that created the view (`Renderer::id`).
    pub(crate) renderer: u64,
    /// The numerical frame probe, once a frame asked for it.
    #[cfg(feature = "diagnostics")]
    pub(crate) probe: Option<crate::stages::frame_probe::FrameProbe>,
    /// Which target completed the last rendered frame's scene
    /// (`DiagnosticTarget::Antialiased`).
    #[cfg(any(test, feature = "diagnostics"))]
    pub(crate) completed: Option<crate::view::frame::Completed>,
}

/// A rendered frame the view has not settled: the scene frame attempt it
/// was part of, the flag `Renderer::finish_frame` sets when that attempt is
/// finished, its history and the targets generation it rendered with.
pub(crate) struct Pending {
    pub attempt: u64,
    pub finished: Arc<AtomicBool>,
    pub history: HistoryFrame,
    pub generation: u64,
}

impl View {
    /// A view of `renderer`'s scenes rendering into a composition texture
    /// at `output_size` physical pixels at `device_scale`, with its stages
    /// built for the frame `settings` describe. Float textures hold
    /// display-referred linear sRGB on both platforms. An sRGB attachment
    /// performs its transfer; other unorm outputs receive sRGB-encoded
    /// colour. Use [`Self::new_surface`] for direct presentation.
    pub fn new(
        renderer: &Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_format: wgpu::TextureFormat,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, ViewError> {
        Self::with_output(
            renderer,
            (device, queue),
            crate::stages::post::Output::texture(output_format),
            output_size,
            device_scale,
            settings,
        )
    }

    /// A view of `renderer`'s scenes rendering directly to a surface
    /// configured with `config`. Its format and colour space determine the
    /// display transfer, including browser float canvases. Supports Auto,
    /// sRGB, extended sRGB and extended linear sRGB. Pass
    /// `surface.get_capabilities(&adapter)` from the configured surface so
    /// native Auto float output follows its actual linear/sRGB capability.
    /// Recreate the view when the surface format or colour space changes.
    pub fn new_surface(
        renderer: &Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, ViewError> {
        let output = crate::stages::post::Output::surface(
            config,
            capabilities,
            device.adapter_info().backend,
        )
        .ok_or(ViewError::UnsupportedSurfaceOutput {
            format: config.format,
            color_space: config.color_space,
        })?;
        Self::with_output(
            renderer,
            (device, queue),
            output,
            [config.width, config.height],
            device_scale,
            settings,
        )
    }

    fn with_output(
        renderer: &Renderer,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        output: crate::stages::post::Output,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, ViewError> {
        let shared = &renderer.shared;
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
        let views = Box::new(FrameViews::new(device));
        let shadows = Shadows::new(device, settings.shadow_quality);
        let fog = VolumetricFog::new(device);
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
            opaque: Opaque::new(
                device,
                &bindings.unlit,
                [
                    &bindings.shadow_mask,
                    &bindings.contact_shadow,
                    &bindings.screen_indirect,
                    &bindings.shadow_mask_screen_indirect,
                    &bindings.contact_shadow_screen_indirect,
                ],
            ),
            traced_shadows: Box::new(TracedShadows::new(device, &bindings.lit, &bindings.scene)),
            contact_shadows: Box::new(ContactShadowStage::new(device)),
            reflections: Box::new(Reflections::new(device, queue, render, &first_frame)),
            transparent: Transparent::new(
                device,
                &bindings.blended,
                exposure.view(),
                bindings.lit.tier,
            ),
            exposure,
            antialiasing,
            motion_blur: MotionBlur::new(device, first_frame.motion_blur.is_some()),
            post: Box::new(
                Post::new(
                    device,
                    queue,
                    output,
                    sizes,
                    first_frame.bloom,
                    (first_frame.antialiasing == Antialiasing::Smaa && first_frame.smaa)
                        .then_some(settings.smaa_quality),
                )
                .map_err(ViewError::LookupTextures)?,
            ),
            cull,
            sky: Sky::default(),
            statistics: StatisticsReadback::default(),
            sizes,
            preset: settings.preset,
            targets,
            views,
            bindings,
            post_fx: None,
            shadows,
            fog,
            history: CameraHistory::default(),
            pending_reset: false,
            targets_generation: 0,
            finished: None,
            rendered: None,
            renderer: renderer.id,
            #[cfg(feature = "diagnostics")]
            probe: None,
            #[cfg(any(test, feature = "diagnostics"))]
            completed: None,
        })
    }

    /// Sizes the targets for an output of `output_size` physical pixels at
    /// `device_scale` and the size-affecting settings, and starts or stops
    /// FSR. Cheap when nothing changed, so a game calls it every frame;
    /// when the targets change, the view's history restarts.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) {
        self.settle(false);
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
        if self.sizes == sizes && self.preset == settings.preset {
            return;
        }
        self.targets = SharedTargets::new(device, render, self.targets.surface.is_some());
        self.exposure.forget_inputs();
        self.motion_blur.forget_inputs();
        self.antialiasing.forget_inputs();
        self.post.resize(device, sizes, sizing.bloom_targets);
        self.transparent.resize(device, &self.targets);
        self.reflections.resize(device, render);
        self.sizes = sizes;
        self.preset = settings.preset;
        self.pending_reset = true;
        self.targets_generation = self.targets_generation.wrapping_add(1);
    }

    /// Whether shaders must write sRGB-encoded values into this view's
    /// output. Use the same transfer for UI composed directly over the
    /// scene; false means linear values (including hardware sRGB attachment
    /// encoding).
    pub fn output_encodes_srgb(&self) -> bool {
        self.output_encodes_srgb
    }

    /// The antialiasing that runs for `settings`: their choice resolved for
    /// the preset, with TAA where FSR is chosen but not running (see
    /// `fsr_error`, or awaiting `resize`). The saved choice is unchanged.
    pub fn antialiasing_in_effect(&self, settings: &Settings) -> Antialiasing {
        effective::antialiasing(settings, self.antialiasing.fsr_running())
    }

    /// Why the FSR version selected at the last `resize` is unavailable.
    /// Failures are retained independently for FSR2 and FSR3.
    pub fn fsr_error(&self) -> Option<&str> {
        self.antialiasing.error()
    }

    /// The size every pass up to antialiasing renders at: the scene size,
    /// or FSR's render size while it upscales.
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
    /// without blocking, the map requested once the frame was finished,
    /// with its blended surfaces' counts from the same frame. None until a
    /// frame's readback has arrived, which takes a few frames.
    pub fn geometry_stats(&mut self, device: &wgpu::Device) -> Option<GeometryStats> {
        self.settle(false);
        self.statistics.poll(device);
        self.statistics.latest()
    }

    /// The last rendered frame's local-light shadows: how many casting
    /// lights in the camera's view have a shadow, how many the atlas had no
    /// room for, and what the frame drew.
    pub fn local_shadow_stats(&self) -> LocalShadowStats {
        self.shadows.local.stats()
    }

    /// Whether `Settings::occlusion_culling` runs for `settings`: it is on,
    /// the device binds the depth pyramid's six storage textures a stage
    /// (`graphics_device::limits` requests the adapter's) and, with the
    /// `diagnostics` feature, the culling layer is on. Otherwise the camera
    /// culls by frustum alone. The saved choice is unchanged.
    pub fn occlusion_culling_in_effect(&self, settings: &Settings) -> bool {
        settings.occlusion_culling
            && self.cull.occlusion_supported()
            && !settings.diagnostics_in_effect().disable.culling
    }

    /// Begins the view's frame of `scene` seen as `input` with `settings`:
    /// settles its last render, begins its history (which restarts for a
    /// camera cut, a resize, an unfinished reset, and a frame that does not
    /// follow its own last finished frame), and resolves its effective
    /// configuration, holding the surface's targets from a scene's first
    /// receiver on and the temporal masks while a writer can mark them.
    pub(super) fn begin(
        &mut self,
        device: &wgpu::Device,
        shared: &Shared,
        scene: &Scene,
        input: &FrameInput,
        settings: &Settings,
    ) -> (HistoryFrame, Effective) {
        self.settle(true);
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
    /// render frame of the scene's origin. It continues only from the
    /// scene's last finished frame, when that frame rendered this view, and
    /// not while a reset is pending (a resize, or a reset rendered and not
    /// finished).
    pub(crate) fn begin_history(&self, scene: &Scene, input: &FrameInput) -> HistoryFrame {
        let reset = input.camera_cut
            || self.pending_reset
            || self.finished.is_none()
            || self.finished != scene.last_finished();
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
        self.history
            .set_backdrop(&mut frame, crate::view::backdrop_yaw(input));
        frame
    }

    /// Settles the last render, if one is pending, and the view renders
    /// `again` or it was finished (`Renderer::finish_frame` set its
    /// attempt's flag, whichever frames came after it). A finished render
    /// commits its camera history, its reset, its local shadow cache,
    /// culling pyramid, exposure, statistics and probe readback; an
    /// unfinished reset holds, so the next render resets too (a cut that
    /// was never shown has not happened yet). Then every stage that keeps
    /// what describes its GPU history as the last finished frame left it
    /// (`view::history::Kept`) settles it: exactly once per render, at the
    /// start of the view's first `&mut self` method after its frame was
    /// finished, or as unfinished when it renders again first. A method
    /// called between `render` and `finish_frame` leaves the render
    /// pending. The History contract's "What a frame commits".
    pub(crate) fn settle(&mut self, again: bool) {
        let Some(pending) = &self.rendered else {
            return;
        };
        let finished = pending.finished.load(Ordering::Acquire);
        if !finished && !again {
            return;
        }
        let Some(Pending {
            attempt,
            history,
            generation,
            ..
        }) = self.rendered.take()
        else {
            return;
        };
        if !finished && !history.valid {
            self.pending_reset = true;
        }
        if finished {
            self.history.finish(history);
            // A resize after the render still restarts the next frame.
            if generation == self.targets_generation {
                self.pending_reset = false;
            }
            self.finished = Some(attempt);
            self.shadows.local.finish_frame();
            self.cull.finish_frame();
            self.exposure.finish_frame();
            self.statistics.submitted();
            #[cfg(feature = "diagnostics")]
            if let Some(probe) = &mut self.probe {
                probe.submitted();
            }
        }
        if let Some(post_fx) = &mut self.post_fx {
            post_fx.settle(finished);
        }
        self.antialiasing.settle(finished);
        self.sky.settle(finished);
        self.reflections.settle(finished);
        self.opaque.settle(finished);
        self.traced_shadows.settle(finished);
    }
}
