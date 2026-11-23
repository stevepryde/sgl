//! Specular probe capture, a renderer operation between frames: the prepare
//! stage builds the capture's six face views, a sky stage of its own
//! computes a procedural sky's tables for its position, the shadow and
//! opaque stages render the scene's content, sky and lights into them, and
//! the probe prefilter GGX-prefilters the cube. A bake
//! (`capture_specular_probe_async`), for authoring, shows the static
//! content and reads the cube back; a live probe's refresh (`refresh`)
//! shows the moving content too and copies the cube into the scene's probe
//! array on the GPU. Both encode one capture (`encode_capture`).
mod refresh;

use super::{Renderer, View};
use crate::baked_specular_probe::{ProbeError, SpecularProbeRadiance, SpecularProbeTexels};
use crate::scene::probes::validate_face_size;
use crate::settings::Settings;
use crate::stages::prepare::CaptureViews;
use crate::stages::probe_prefilter::{ProbePrefilter, capture_size, validate_readback_size};
use crate::stages::shadows::local::RefreshRecords;
use crate::view::pipelines::LayerConstants;
use crate::view::population::Casters;
use crate::{FrameInput, Scene};
use glam::Vec3;
pub(crate) use refresh::LiveRefresh;

/// What a capture is for, which decides what it shows and whose
/// local-light shadows its surfaces take.
enum Capture<'a> {
    /// For exported content: the static instances, lit by static layers of
    /// the local lights' shadows placed for the capture.
    Bake,
    /// A live probe's refresh: the moving instances too, each face culled
    /// to its view and without the materials of the visibility groups
    /// `hidden`, lit by the borrowed view's local-light shadows and under
    /// its sky as its last frame left them, by the copy of that frame's
    /// shadow records `records` keeps.
    Refresh {
        hidden: u32,
        records: &'a mut RefreshRecords,
    },
}

impl Capture<'_> {
    /// The instances it shows, by whether they move.
    fn content(&self) -> Casters {
        match self {
            Self::Bake => Casters::Static,
            Self::Refresh { .. } => Casters::All,
        }
    }
}

impl Renderer {
    /// Captures the scene's static capture-visible instances and the
    /// reflection sky of `input`'s environment from `center` with the
    /// installed fixed irradiance atlases, `input`'s lights, the directional
    /// shadow in cascades about `center`, and the scene's lights with the
    /// static casters' shadows, then GGX-prefilters it. As in the frame,
    /// only materials whose visibility group the input's `visibility_mask`
    /// selects are drawn, so a caller leaves geometry out of a probe as a
    /// reflection probe's culling mask does (Godot's
    /// `ReflectionProbe.cull_mask`, Unity's culling mask). Surfaces'
    /// environment specular comes from the installed collection, so
    /// capturing again with a first pass's probes installed bakes one more
    /// bounce. Baked lights light only surfaces without baked lighting. Moving
    /// instances, effects and atmospheric post are excluded; the camera is
    /// unused. It borrows `view`'s shadow views and maps and its opaque
    /// stage, so call it between frames, not between `render` and
    /// `finish_frame`. It is not a frame: it takes nothing from, and commits
    /// nothing to, any history. Those maps follow `settings.shadow_quality`:
    /// a capture at another quality than `view`'s frames reallocates its
    /// maps and resets its local-light shadow cache, so its next frame draws
    /// every shadow again. It is for
    /// authoring and level building, never a per-frame loop. Each face
    /// renders at 2048 texels a side, or `face_size` if larger, and is
    /// averaged to `face_size` (`stages::probe_prefilter`), one face per
    /// submission.
    ///
    /// It holds the renderer, `view` and `scene` until it resolves, so the game
    /// draws no frame meanwhile; in a browser the page shows what it likes.
    /// Natively it waits on the device before it awaits its readback, so any
    /// executor finishes it; in a browser the page maps the readback once
    /// control returns to it. That readback is its only suspension, and
    /// comes after it has submitted every face and changed all it changes in
    /// the renderer: a capture dropped there leaves what a finished one
    /// does, `view`'s shadow maps at `settings.shadow_quality`, the
    /// capture's static local-light layers committed to its shadow cache
    /// and the pipelines specialised for its settings, which the next frame
    /// specialises, resizes and places again for its own as after any
    /// capture.
    #[allow(clippy::too_many_arguments)]
    pub async fn capture_specular_probe_async(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        view: &mut View,
        input: &FrameInput,
        settings: &Settings,
        center: Vec3,
        face_size: u32,
    ) -> Result<SpecularProbeRadiance, ProbeError> {
        assert!(
            view.renderer == self.id,
            "a view captures with the renderer it was created from (`View::new`)"
        );
        view.settle(false);
        if !center.is_finite() {
            return Err(ProbeError::InvalidProbe("coordinates must be finite"));
        }
        validate_face_size(face_size, &device.limits())?;
        validate_readback_size(face_size, &device.limits())?;
        self.ready_capture(device, queue, scene, view, (settings, None));
        let scene = &*scene;
        let prefilter = ProbePrefilter::new(device, face_size, capture_size(face_size));
        let label = wgpu::CommandEncoderDescriptor {
            label: Some("author static specular probe"),
        };
        let mut encoder = device.create_command_encoder(&label);
        // A sky stage of the capture's own computes its tables and its
        // environment atlas's whole cycle for its position. The capture's
        // backdrop draw records the reflection sky (`VIEW_PROBE_CAPTURE` in
        // backdrop.wgsl): under a procedural sky, that atlas.
        let mut sky = crate::stages::sky::Sky::default();
        sky.prepare(device, input, center, (true, false));
        sky.encode_planned((queue, &mut encoder), None);
        let sky = sky.views().map(|views| views.map(Clone::clone));
        // Each face renders at the capture size and is averaged into the
        // cube in its own submission, so no command buffer holds the GPU for
        // the whole capture.
        self.encode_capture(
            (device, queue),
            scene,
            view,
            (input, settings),
            (center, Capture::Bake),
            // A bake's views are its own: it is no part of a frame loop.
            &mut CaptureViews::new(device),
            (&prefilter, sky),
            (0..6, None),
            &mut encoder,
            |encoder| {
                let face = std::mem::replace(encoder, device.create_command_encoder(&label));
                queue.submit([face.finish()]);
            },
        );
        // Commit the submitted capture's shadow layers before fallible readback.
        view.shadows.local.finish_capture();
        prefilter.encode(&mut encoder, None);
        let rgba16 = prefilter.read(device, queue, encoder).await?;
        Ok(SpecularProbeRadiance {
            face_size,
            texels: SpecularProbeTexels::Rgba16Float(rgba16),
        })
    }

    /// Readies `scene`, `view` and the pipelines for a capture with
    /// `settings`, as a frame's prepare does for its own; a live refresh's
    /// `timing` times the mip chains it builds.
    fn ready_capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        view: &mut View,
        (settings, timing): (&Settings, Option<&crate::timing::GpuTiming>),
    ) {
        // Decals added since the last frame take their atlas first, and
        // decoded images their mip chains.
        scene.upload_decals(device, queue);
        self.shared.build_mip_chains((device, queue), scene, timing);
        // Materials sample as the settings filter them, as a frame does.
        scene.materials.set_anisotropy(
            device,
            &scene.render_textures,
            settings.anisotropic_filtering.clamp(),
        );
        self.shared.pipelines.specialise(
            LayerConstants::new(&settings.diagnostics_in_effect().disable),
            scene,
        );
        // The capture draws into the frame's shadow maps, at the settings'
        // quality, and its groups bind them once its shadows are placed.
        view.shadows.resize(device, settings.shadow_quality);
    }

    /// Encodes a capture at `center`, a bake or a live refresh
    /// (`capture`): its directional cascades, a bake's static local-light
    /// layers, and its faces `faces` (of 0..6), each rendered into
    /// `prefilter`'s face target under a procedural sky's views `sky` (its
    /// sky-view table and environment atlas), resolved into its cube and
    /// followed by `face_done`. It fills `views`, the capture's views and
    /// what they draw, and writes their buffers: a bake's own, or those a
    /// renderer keeps across its refreshes, which so create none. A bake's
    /// caller commits its shadow layers once they are submitted
    /// (`finish_capture`).
    #[allow(clippy::too_many_arguments)]
    fn encode_capture(
        &mut self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        scene: &Scene,
        view: &mut View,
        (input, settings): (&FrameInput, &Settings),
        (center, capture): (Vec3, Capture<'_>),
        views: &mut CaptureViews,
        (prefilter, sky): (&ProbePrefilter, Option<[wgpu::TextureView; 2]>),
        (faces, timing): (std::ops::Range<u32>, Option<&crate::timing::GpuTiming>),
        encoder: &mut wgpu::CommandEncoder,
        mut face_done: impl FnMut(&mut wgpu::CommandEncoder),
    ) {
        let local_lights = !settings.diagnostics_in_effect().disable.local_lights;
        // The volume the committed probes light, and those probes: what the
        // last submitted frame left, never a fresher frame's abandoned ones,
        // while dynamic GI is in effect.
        let dynamic_gi = self
            .shared
            .dynamic_gi
            .as_ref()
            .and_then(|dynamic_gi| dynamic_gi.lights(scene))
            .filter(|_| self.dynamic_gi_in_effect(settings, scene).rays().is_some());
        self.shared.prepare.capture(
            device,
            queue,
            scene,
            input,
            local_lights,
            (center, capture.content()),
            match &capture {
                Capture::Bake => (faces, false, 0),
                Capture::Refresh { hidden, .. } => (faces, true, *hidden),
            },
            settings.shadow_quality.cascade_size(),
            dynamic_gi.map(|(volume, _)| volume).as_ref(),
            views,
        );
        // A bake's lights' shadows sample static layers placed for the
        // capture. They draw at the capture's time, from its frame uniform.
        let local = matches!(capture, Capture::Bake).then(|| {
            view.shadows.local.plan_capture(
                device,
                queue,
                &view.bindings,
                (scene, &mut views.instances),
                (center, &views.frame),
                crate::stages::shadows::local::ShadowFrame {
                    mask: input.visibility_mask,
                    time: input.elapsed_seconds,
                    layers: LayerConstants::new(&settings.diagnostics_in_effect().disable),
                },
                local_lights,
            )
        });
        // A live refresh's lights' shadows are the view's, as its last
        // frame left them: it places and draws none.
        let frame_records = match capture {
            Capture::Bake => None,
            Capture::Refresh { records, .. } => {
                view.shadows
                    .local
                    .refresh_records(device, queue, scene, records);
                Some(records.buffer())
            }
        };
        // Placing a bake's created the local atlases where none were.
        view.bindings.shadow_maps = view.shadows.maps();
        // Every list the capture draws is built.
        views.instances.upload(device, queue);
        views.ready_cascade_groups(|cascade, frame| {
            view.bindings.shadow_group(device, cascade, frame)
        });
        view.shadows.directional.encode_capture(
            encoder,
            scene,
            &mut self.shared.pipelines,
            (&views.casters, &views.instances),
            views.cascade_groups(),
            timing,
        );
        if let Some(local) = &local {
            view.shadows.local.encode_capture(
                device,
                encoder,
                local,
                (scene, &mut self.shared.pipelines),
                &views.instances,
            );
        }
        let environment = crate::view::bindings::EnvironmentViews::resolve(
            &scene.environments,
            &input.environment,
            sky.as_ref().map(|[sky_view, atlas]| [sky_view, atlas]),
        );
        // Surfaces here sample the installed probes (surface.wgsl).
        let probes = scene
            .specular_probes()
            .unwrap_or(&view.bindings.empty_probes);
        for face in &views.faces {
            let lit = view.bindings.lit_group(
                device,
                scene,
                &environment,
                [views.face_view(*face), &views.frame],
                probes,
                views.clusters.buffer(),
                match (&local, &frame_records) {
                    (Some(local), _) => view.bindings.static_local_shadows(&local.records),
                    (None, Some(records)) => view.bindings.frame_local_shadows(records),
                    (None, None) => view.bindings.local_shadows(),
                },
                dynamic_gi.map(|(_, probes)| probes).or_else(|| {
                    self.shared
                        .dynamic_gi
                        .as_ref()
                        .map(crate::stages::dynamic_gi::DynamicGi::stand_in)
                }),
            );
            let unlit = view.bindings.unlit_group(
                device,
                scene,
                &environment,
                views.face_view(*face),
                &views.frame,
            );
            view.opaque.encode_capture(
                encoder,
                scene,
                &mut self.shared.pipelines,
                (&views.lists[face.list], &views.instances),
                prefilter.face(),
                &unlit,
                &lit,
                timing,
            );
            prefilter.resolve_face(encoder, timing, face.face);
            face_done(encoder);
        }
    }

    /// [`Self::capture_specular_probe_async`], blocking until it completes.
    /// Native-only: a browser cannot block on WebGPU, and awaits the
    /// asynchronous form.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_arguments)]
    pub fn capture_specular_probe(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        view: &mut View,
        input: &FrameInput,
        settings: &Settings,
        center: Vec3,
        face_size: u32,
    ) -> Result<SpecularProbeRadiance, ProbeError> {
        pollster::block_on(self.capture_specular_probe_async(
            device, queue, scene, view, input, settings, center, face_size,
        ))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
