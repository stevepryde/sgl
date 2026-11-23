//! The renderer and its views. `Renderer` (`shared.rs`) is what every view
//! of a scene shares: the bind group layouts, the geometry pipeline cache,
//! prepare's scene part, the deform and dynamic GI stages, the device's
//! ray-query form and the frame boundary. `View` (`view.rs`) is one
//! camera's frame: its targets and sizes, group 0's bindings, its views,
//! its camera history, DiligentFX's post-effect context and every other
//! stage. A frame (`frame.rs`) runs the scene's frame work once, then
//! orders each view's stages (`view_stages.rs`) from its effective
//! configuration (resolved in `effective.rs`), the views in the order
//! `order.rs` gives them: those that draw render textures before the views
//! that show them. A probe capture (`probe_capture.rs`) orders the
//! prepare, shadow and opaque stages and the probe prefilter over its own
//! views. It holds no pipelines or shaders of its own.
#[cfg(any(test, feature = "diagnostics"))]
mod diagnostics;
pub(crate) mod effective;
pub(crate) mod frame;
mod order;
pub(crate) mod probe_capture;
mod shared;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod test_frame;
mod view;
mod view_stages;

#[cfg(all(test, feature = "diagnostics", not(target_arch = "wasm32")))]
mod diagnostics_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod floor_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod render_texture_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod shader_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod size_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod view_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod volume_path_tests;

use crate::Scene;
use crate::scene::rays::acceleration::RayTracingStats;
use crate::settings::{DynamicGiQuality, Settings};
use crate::shading::bind::BindingTier;
use shared::Shared;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
pub use view::{View, ViewError, ViewFrame, ViewOutput};

/// What every view of a [`Scene`] shares: the geometry pipelines and the
/// bind group layouts, the deform stage, the dynamic GI stage with its
/// probes, the device's ray-query form, and the frame boundary. Each frame,
/// after editing the scene: [`View::resize`] each view (cheap when nothing
/// changed), [`Renderer::render`] the scene's views into an encoder, submit
/// it, then [`Renderer::finish_frame`]. It reads the scene and does not
/// change its content. It shows one scene at a time: another scene
/// specialises its pipelines again and restarts its dynamic GI, so a game
/// that shows two scenes at once holds a renderer for each.
pub struct Renderer {
    /// What every view of the scene shares.
    shared: Shared,
    /// The last `render`, until `finish_frame` commits it: its scene frame
    /// attempt and the flag `finish_frame` sets, which each view the frame
    /// rendered holds with its pending render.
    rendered: Option<(u64, Arc<AtomicBool>)>,
    /// The renderer's identity, which each view it creates holds.
    pub(crate) id: u64,
}

impl Renderer {
    /// The shared part of rendering on `device`. Views of its scenes are
    /// [`View::new`] or [`View::new_surface`] with it.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            shared: Shared::new(device),
            rendered: None,
            id: crate::scene::next_generation(),
        }
    }

    /// Encodes one frame of `scene`: its frame work once (its uploads, the
    /// acceleration structures, the deform pass and the dynamic GI update),
    /// then each view, each as its `ViewFrame` describes it, into
    /// its output: prepare, shadows, fog, opaque, reflections and
    /// transparent, exposure, antialiasing, motion blur, post. `views[0]` is
    /// the primary view: its input and settings steer what a scene has one
    /// of (the architecture's Frame, "Views of one frame"). Nothing happens
    /// for an empty slice. Views render as listed, but a view that draws
    /// a render texture (`ViewOutput::RenderTexture`) renders before the
    /// views that show it, whichever is primary, and only in the frames one
    /// does: a surface then shows this frame's picture, and where views
    /// show each other's, or one its own, the view listed first shows the
    /// picture the other last drew (the architecture's Frame, "Render
    /// textures"). A view's history restarts for a camera cut, after
    /// a resize that changed its targets, for a frame that does not follow
    /// its own last finished frame (its first, one of another scene, or one
    /// after a finished frame of the scene that did not render it), and
    /// after a reset it rendered and was not finished: a reset holds until a
    /// frame is finished. List every view of the frame here: a second
    /// renderer's `render` of the same scene supersedes this one's frame,
    /// whose `finish_frame` then commits nothing.
    /// `scene` is mutable for its per-frame GPU mirrors.
    ///
    /// # Panics
    ///
    /// For a view created from another renderer; for a view that draws a
    /// render texture `scene` does not hold, or one of another size or
    /// format than its own; and for two views that draw one render texture.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &mut Scene,
        views: &mut [ViewFrame<'_>],
    ) {
        if views.is_empty() {
            return;
        }
        assert!(
            views.iter().all(|frame| frame.view.renderer == self.id),
            "every view rendered by a renderer is created from it (`View::new`)"
        );
        let finished = Arc::new(AtomicBool::new(false));
        let attempt = frame::render(
            &mut self.shared,
            (device, queue, encoder),
            scene,
            views,
            &finished,
        );
        self.rendered = Some((attempt, finished));
    }

    /// After the caller submitted the last rendered frame of `scene`: it
    /// becomes the history of the next, camera and instances alike, as the
    /// frame drew them, so an edit made since the submission belongs to the
    /// next frame; each view the frame rendered commits its histories the
    /// next time it is used. Does nothing if the last `render` was of
    /// another scene, or none: a frame rendered but never finished (an
    /// abandoned encoder) leaves history untouched.
    pub fn finish_frame(&mut self, scene: &mut Scene) {
        let Some((attempt, _)) = self.rendered else {
            return;
        };
        if scene.finish_frame(attempt) {
            let Some((_, finished)) = self.rendered.take() else {
                return;
            };
            if let Some(dynamic_gi) = &mut self.shared.dynamic_gi {
                dynamic_gi.finish_frame();
            }
            finished.store(true, Ordering::Release);
        }
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

    /// The dynamic GI quality that runs for the primary view's `settings`
    /// where the scene holds a volume: their choice on the Extended binding
    /// tier with ray data, `Off` on Basic or a scene created with
    /// `RayData::Disabled`. The saved choice is unchanged.
    pub fn dynamic_gi_in_effect(&self, settings: &Settings, scene: &Scene) -> DynamicGiQuality {
        effective::dynamic_gi(settings, self.binding_tier(), scene.ray_data())
    }

    /// The world-space reflection choice for this scene, while a view's
    /// camera supports screen-space reflections. Off without ray data or
    /// SSR. The saved choice is unchanged.
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

    /// Whether the hardware path traces the scene's rays for the primary
    /// view's `settings`: `Settings::hardware_ray_tracing` is on, the device
    /// has ray queries (`graphics_device::ray_tracing_features`) and nothing
    /// stopped it and this scene retains ray data. Nothing stopped it
    /// (`ray_tracing_error`, but for a fall back to the baseline form,
    /// which still traces). Otherwise the portable BVHs trace them. The
    /// saved choice is unchanged. No ray path is available without ray data.
    pub fn ray_tracing_in_effect(&self, settings: &Settings, scene: &Scene) -> bool {
        scene.ray_data() == crate::RayData::Enabled
            && settings.hardware_ray_tracing
            && self.shared.ray_form.is_some()
            && self.shared.prepare.ray_tracing_error().is_none()
    }

    /// Whether ray-traced shadows are in effect for a view's `settings` in
    /// a frame whose primary view's settings are `primary` (the same
    /// settings for the primary view itself), whose hardware ray tracing
    /// every view of the frame takes: `settings.ray_traced_shadows` is on
    /// and hardware ray tracing is in effect for `primary`
    /// (`ray_tracing_in_effect`). The view's opaque surfaces then take them
    /// on the frames where a light holds a slot; otherwise the shadow maps
    /// shadow them. The saved choices are unchanged.
    pub fn ray_traced_shadows_in_effect(
        &self,
        settings: &Settings,
        primary: &Settings,
        scene: &Scene,
    ) -> bool {
        settings.ray_traced_shadows && self.ray_tracing_in_effect(primary, scene)
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
