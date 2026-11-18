//! The renderer: it owns the stages, selects and orders them each frame from
//! the effective configuration (`frame.rs`, resolved in `effective.rs`), and
//! owns and lends them what they share. What every view of a scene shares
//! (`shared.rs`): the bind group layouts, the geometry pipeline cache,
//! prepare's scene part, the deform and dynamic GI stages and the device's
//! ray-query form. What its one view holds (`view.rs`): the frame's targets
//! and sizes, group 0's bindings, the frame's views, the camera history,
//! DiligentFX's post-effect context and every other stage. A probe capture
//! (`probe_capture.rs`) orders the prepare, shadow and opaque stages and the
//! probe prefilter over its own views. It holds no pipelines or shaders of
//! its own.
#[cfg(any(test, feature = "diagnostics"))]
mod diagnostics;
pub(crate) mod effective;
pub(crate) mod frame;
pub(crate) mod probe_capture;
mod shared;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod test_frame;
mod view;

#[cfg(all(test, feature = "diagnostics", not(target_arch = "wasm32")))]
mod diagnostics_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod floor_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod shader_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod size_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod volume_path_tests;

use crate::scene::rays::acceleration::RayTracingStats;
use crate::settings::{Antialiasing, DynamicGiQuality, Settings};
use crate::shading::bind::BindingTier;
use crate::stages::shadows::local::LocalShadowStats;
use crate::view::draw_list::GeometryStats;
use crate::view::pipelines::LayerConstants;
use crate::{FrameInput, Scene};
use shared::Shared;
use view::View;

/// Renders a [`Scene`] into a caller's texture, one frame at a time: call
/// `resize` (cheap when nothing changed), then `render` into an encoder, and
/// `finish_frame` after submitting it. It owns every target, pipeline and
/// history; the scene owns content.
pub struct Renderer {
    /// What every view of the scene shares.
    shared: Shared,
    /// The one view it renders.
    view: View,
}

/// Why a renderer could not be created.
#[derive(Debug)]
pub enum RendererError {
    /// SMAA's lookup textures did not decode.
    LookupTextures(image::ImageError),
    /// The surface requires an unsupported transfer or format/colour-space pair.
    UnsupportedSurfaceOutput {
        format: wgpu::TextureFormat,
        color_space: wgpu::SurfaceColorSpace,
    },
}

impl std::fmt::Display for RendererError {
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

impl std::error::Error for RendererError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LookupTextures(error) => Some(error),
            Self::UnsupportedSurfaceOutput { .. } => None,
        }
    }
}

impl Renderer {
    /// Whether shaders must write sRGB-encoded values into this renderer's
    /// output. Use the same transfer for UI composed directly over the scene;
    /// false means linear values (including hardware sRGB attachment encoding).
    pub fn output_encodes_srgb(&self) -> bool {
        self.view.output_encodes_srgb
    }

    /// Renders into a composition texture at `output_size` physical pixels.
    /// Float textures hold display-referred linear sRGB on both platforms.
    /// An sRGB attachment performs its transfer; other unorm outputs receive
    /// sRGB-encoded colour. Use [`Self::new_surface`] for direct presentation.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_format: wgpu::TextureFormat,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, RendererError> {
        Self::with_output(
            device,
            queue,
            crate::stages::post::Output::texture(output_format),
            output_size,
            device_scale,
            settings,
        )
    }

    /// Renders directly to a surface configured with `config`. Its format and
    /// colour space determine the display transfer, including browser float
    /// canvases. Supports Auto, sRGB, extended sRGB and extended linear sRGB.
    /// Pass `surface.get_capabilities(&adapter)` from the configured surface so
    /// native Auto float output follows its actual linear/sRGB capability.
    /// Recreate the renderer when the surface format or colour space changes.
    pub fn new_surface(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, RendererError> {
        let output = crate::stages::post::Output::surface(
            config,
            capabilities,
            device.adapter_info().backend,
        )
        .ok_or(RendererError::UnsupportedSurfaceOutput {
            format: config.format,
            color_space: config.color_space,
        })?;
        Self::with_output(
            device,
            queue,
            output,
            [config.width, config.height],
            device_scale,
            settings,
        )
    }

    fn with_output(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output: crate::stages::post::Output,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, RendererError> {
        let shared = Shared::new(
            device,
            LayerConstants::new(&settings.diagnostics_in_effect().disable),
        );
        let view = View::new(
            device,
            queue,
            &shared,
            output,
            output_size,
            device_scale,
            settings,
        )?;
        Ok(Self { shared, view })
    }

    /// Sizes the targets for an output of `output_size` physical pixels at
    /// `device_scale` and the size-affecting settings, and starts or stops
    /// FSR. Cheap when nothing changed, so a game calls it every frame;
    /// when the targets change, history restarts.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        output_size: [u32; 2],
        device_scale: f32,
        settings: &Settings,
    ) {
        self.view
            .resize(device, output_size, device_scale, settings);
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
    /// becomes the history of the next, camera and instances alike, as the
    /// frame drew them, so an edit made since the submission belongs to the
    /// next frame. A frame rendered but never finished (an abandoned
    /// encoder) leaves history untouched.
    pub fn finish_frame(&mut self, scene: &mut Scene) {
        if self.view.finish() {
            self.shared.finish_frame(scene);
        }
    }

    /// The antialiasing that runs for `settings`: their choice resolved for
    /// the preset, with TAA where FSR is chosen but not running (see
    /// `fsr_error`, or awaiting `resize`). The saved choice is unchanged.
    pub fn antialiasing_in_effect(&self, settings: &Settings) -> Antialiasing {
        effective::antialiasing(settings, self.view.antialiasing.fsr_running())
    }

    /// The device's binding tier: `Extended` where it binds 48 or more
    /// sampled textures per shader stage, else `Basic`, on which baked
    /// light is non-directional, dynamic GI is off
    /// (`dynamic_gi_in_effect`), no volume path is measured
    /// (`volume_paths_in_effect`) and a material's anisotropy map gives way
    /// to its factors. Fixed for the device; `graphics_device::limits` requests
    /// the adapter's.
    pub fn binding_tier(&self) -> BindingTier {
        self.shared.pipelines.tier
    }

    /// The dynamic GI quality that runs for `settings` where the scene holds
    /// a volume: their choice on the Extended binding tier with ray data,
    /// `Off` on Basic or a scene created with `RayData::Disabled`.
    /// The saved choice is unchanged.
    pub fn dynamic_gi_in_effect(&self, settings: &Settings, scene: &Scene) -> DynamicGiQuality {
        effective::dynamic_gi(settings, self.binding_tier(), scene.ray_data())
    }

    /// The world-space reflection choice for this scene, while its camera
    /// supports screen-space reflections. Off without ray data or SSR.
    /// The saved choice is unchanged.
    pub fn world_space_reflections_in_effect(
        &self,
        settings: &Settings,
        scene: &Scene,
    ) -> crate::settings::WorldSpaceReflections {
        effective::world_space(
            settings,
            scene.ray_data(),
            settings.screen_space_reflections != crate::settings::ScreenSpaceReflections::Off,
        )
    }

    /// Whether volume paths are measured for `settings`
    /// (`Settings::volume_paths`): their choice on the Extended binding
    /// tier, off on Basic, where `scene_volume_path` reports `VOLUME_NONE`.
    /// The saved choice is unchanged.
    pub fn volume_paths_in_effect(&self, settings: &Settings) -> bool {
        effective::volume_paths(settings, self.binding_tier())
    }

    /// Why the FSR version selected at the last `resize` is unavailable.
    /// Failures are retained independently for FSR2 and FSR3.
    pub fn fsr_error(&self) -> Option<&str> {
        self.view.antialiasing.error()
    }

    /// The size every pass up to antialiasing renders at: the scene size,
    /// or FSR's render size while it upscales.
    pub fn render_size(&self) -> [u32; 2] {
        self.view.sizes.render
    }

    /// The size motion blur, bloom, SMAA and tone mapping run at
    /// (`SceneResolution`).
    pub fn scene_size(&self) -> [u32; 2] {
        self.view.sizes.scene
    }

    /// The camera's submitted geometry in the most recent completed frame:
    /// its opaque and masked surfaces counted on the GPU and read back
    /// without blocking, the map requested at `finish_frame`, with its
    /// blended surfaces' counts from the same frame. None until a frame's
    /// readback has arrived, which takes a few frames.
    pub fn geometry_stats(&mut self, device: &wgpu::Device) -> Option<GeometryStats> {
        self.view.statistics.poll(device);
        self.view.statistics.latest()
    }

    /// The last rendered frame's local-light shadows: how many casting
    /// lights in the camera's view have a shadow, how many the atlas had no
    /// room for, and what the frame drew.
    pub fn local_shadow_stats(&self) -> LocalShadowStats {
        self.view.shadows.local.stats()
    }

    /// What the last rendered frame's hardware ray tracing held: how many
    /// capture-visible instances the scene's TLAS held, how many that do not
    /// deform it did not hold, which the portable BVHs cover, and how many
    /// the device's limits or memory left out. Zero for a frame that built
    /// no acceleration structures: one without ray tracing hardware
    /// (`graphics_device::ray_tracing_features`), with
    /// `Settings::hardware_ray_tracing` off, or tracing no rays.
    pub fn ray_tracing_stats(&self) -> RayTracingStats {
        self.shared.prepare.ray_tracing_stats()
    }

    /// Whether the hardware path traces the scene's rays for `settings`:
    /// `Settings::hardware_ray_tracing` is on, the device has ray queries
    /// (`graphics_device::ray_tracing_features`) and nothing stopped it
    /// and this scene retains ray data. Nothing stopped it
    /// (`ray_tracing_error`, but for a fall back to the baseline form,
    /// which still traces). Otherwise the portable BVHs trace them. The
    /// saved choice is unchanged. No ray path is available without ray data.
    pub fn ray_tracing_in_effect(&self, settings: &Settings, scene: &Scene) -> bool {
        scene.ray_data() == crate::RayData::Enabled
            && settings.hardware_ray_tracing
            && self.shared.ray_form.is_some()
            && self.shared.prepare.ray_tracing_error().is_none()
    }

    /// Whether ray-traced shadows are in effect for `settings`:
    /// `Settings::ray_traced_shadows` is on and hardware ray tracing is in
    /// effect (`ray_tracing_in_effect`). The camera's opaque surfaces then
    /// take them on the frames where a light holds a slot; otherwise the
    /// shadow maps shadow them. The saved choice is unchanged.
    pub fn ray_traced_shadows_in_effect(&self, settings: &Settings, scene: &Scene) -> bool {
        settings.ray_traced_shadows && self.ray_tracing_in_effect(settings, scene)
    }

    /// Whether `Settings::occlusion_culling` runs for `settings`: it is on,
    /// the device binds the depth pyramid's six storage textures a stage
    /// (`graphics_device::limits` requests the adapter's) and, with the
    /// `diagnostics` feature, the culling layer is on. Otherwise the camera
    /// culls by frustum alone. The saved choice is unchanged.
    pub fn occlusion_culling_in_effect(&self, settings: &Settings) -> bool {
        settings.occlusion_culling
            && self.view.cull.occlusion_supported()
            && !settings.diagnostics_in_effect().disable.culling
    }

    /// Why the hardware path did not trace the last rendered frame's rays
    /// although `Settings::hardware_ray_tracing` was on: the device has no
    /// ray queries, its memory could not hold the scene's TLAS, or the scene
    /// permanently disabled ray data. Portable BVHs trace only when ray data
    /// is enabled. Also reports why the device fell back from
    /// the candidate form to the baseline form, which traced them.
    pub fn ray_tracing_error(&self) -> Option<&str> {
        let shared = &self.shared;
        shared.prepare.ray_tracing_error().or_else(|| {
            shared
                .prepare
                .hardware_requested()
                .then(|| shared.ray_form.as_ref()?.failure())
                .flatten()
        })
    }
}
