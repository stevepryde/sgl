//! Static specular probe capture, a renderer operation for authoring: the
//! prepare stage builds the capture's six face views, the shadow and opaque
//! stages render the scene's static content, sky and lights into them, and
//! the probe prefilter GGX-prefilters the cube and reads it back.
use super::Renderer;
use crate::baked_specular_probe::{ProbeError, SpecularProbeRadiance, SpecularProbeTexels};
use crate::scene::probes::validate_face_size;
use crate::settings::Settings;
use crate::stages::probe_prefilter::{ProbePrefilter, capture_size, validate_readback_size};
use crate::view::pipelines::LayerConstants;
use crate::{FrameInput, Scene};
use glam::Vec3;

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
    /// unused. It shares the frame's shadow views and maps, so call it
    /// between frames, not between `render` and `finish_frame`. Those maps
    /// follow `settings.shadow_quality`: a capture at another quality than
    /// the frames reallocates the frame's maps and resets the local-light
    /// shadow cache, so the next frame draws every shadow again. It is for
    /// authoring and level building, never a per-frame loop. Each face
    /// renders at 2048 texels a side, or `face_size` if larger, and is
    /// averaged to `face_size` (`stages::probe_prefilter`), one face per
    /// submission.
    ///
    /// It holds the renderer and `scene` until it resolves, so the game
    /// draws no frame meanwhile; in a browser the page shows what it likes.
    /// Natively it waits on the device before it awaits its readback, so any
    /// executor finishes it; in a browser the page maps the readback once
    /// control returns to it. That readback is its only suspension, and
    /// comes after it has submitted every face and changed all it changes in
    /// the renderer: a capture dropped there leaves what a finished one
    /// does, the frame's shadow maps at `settings.shadow_quality`, the
    /// capture's static local-light layers committed to the shadow cache
    /// and the pipelines specialised for its settings, which the next frame
    /// specialises, resizes and places again for its own as after any
    /// capture.
    #[allow(clippy::too_many_arguments)]
    pub async fn capture_specular_probe_async(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        input: &FrameInput,
        settings: &Settings,
        center: Vec3,
        face_size: u32,
    ) -> Result<SpecularProbeRadiance, ProbeError> {
        if !center.is_finite() {
            return Err(ProbeError::InvalidProbe("coordinates must be finite"));
        }
        validate_face_size(face_size, &device.limits())?;
        validate_readback_size(face_size, &device.limits())?;
        // Decals added since the last frame take their atlas first.
        scene.upload_decals(device, queue);
        // Materials sample as the settings filter them, as a frame does.
        scene
            .materials
            .set_anisotropy(device, settings.anisotropic_filtering.clamp());
        let scene = &*scene;
        self.shared.pipelines.specialise(
            device,
            LayerConstants::new(&settings.diagnostics_in_effect().disable),
            scene,
            false,
        );
        let prefilter = ProbePrefilter::new(device, face_size, capture_size(face_size));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("author static specular probe"),
        });
        // The capture draws into the frame's shadow maps, at the settings'
        // quality, and its groups bind them.
        self.view.shadows.resize(device, settings.shadow_quality);
        self.view.bindings.shadow_maps = self.view.shadows.maps();
        // The volume the committed probes light, and those probes: what the
        // last submitted frame left, never a fresher frame's abandoned ones,
        // while dynamic GI is in effect.
        let dynamic_gi = self
            .shared
            .dynamic_gi
            .as_ref()
            .and_then(|dynamic_gi| dynamic_gi.lights(scene))
            .filter(|_| self.dynamic_gi_in_effect(settings, scene).rays().is_some());
        let mut views = self.shared.prepare.capture(
            device,
            queue,
            scene,
            input,
            !settings.diagnostics_in_effect().disable.local_lights,
            center,
            settings.shadow_quality.cascade_size(),
            dynamic_gi.map(|(volume, _)| volume).as_ref(),
        );
        // Their lights' shadows sample static layers placed for the capture.
        // They draw at the capture's time, from its frame uniform.
        let local = self.view.shadows.local.plan_capture(
            device,
            queue,
            &self.view.bindings,
            (scene, &mut views.instances),
            (center, &views.frame),
            crate::stages::shadows::local::ShadowFrame {
                mask: input.visibility_mask,
                time: input.elapsed_seconds,
                layers: LayerConstants::new(&settings.diagnostics_in_effect().disable),
            },
            !settings.diagnostics_in_effect().disable.local_lights,
        );
        // Every list the capture draws is built.
        views.instances.upload(device, queue);
        let cascades: Vec<_> = views
            .cascades
            .iter()
            .map(|view| self.view.bindings.shadow_group(device, view, &views.frame))
            .collect();
        self.view.shadows.directional.encode_capture(
            &mut encoder,
            scene,
            &self.shared.pipelines,
            (&views.casters, &views.instances),
            &cascades,
        );
        self.view.shadows.local.encode_capture(
            &mut encoder,
            &local,
            (scene, &self.shared.pipelines),
            &views.instances,
        );
        // Surfaces here sample the installed probes (surface.wgsl).
        let probes = scene
            .specular_probes()
            .unwrap_or(&self.view.bindings.empty_probes);
        // Each face renders at the capture size and is averaged into the
        // cube in its own submission, so no command buffer holds the GPU for
        // the whole capture.
        for (index, view) in (0..).zip(&views.faces) {
            let lit = self.view.bindings.lit_group(
                device,
                scene,
                input.environment,
                [view, &views.frame],
                probes,
                views.clusters.buffer(),
                self.view.bindings.static_local_shadows(&local.records),
                dynamic_gi.map(|(_, probes)| probes).or_else(|| {
                    self.shared
                        .dynamic_gi
                        .as_ref()
                        .map(crate::stages::dynamic_gi::DynamicGi::stand_in)
                }),
            );
            let sky = self.view.bindings.unlit_group(
                device,
                scene,
                input.environment,
                view,
                &views.frame,
            );
            self.view.opaque.encode_capture(
                &mut encoder,
                scene,
                &self.shared.pipelines,
                (&views.list, &views.instances),
                &prefilter.face(),
                &sky,
                &lit,
            );
            prefilter.resolve_face(device, &mut encoder, index);
            let face = std::mem::replace(
                &mut encoder,
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("author static specular probe"),
                }),
            );
            queue.submit([face.finish()]);
        }
        // Commit the submitted capture's shadow layers before fallible readback.
        self.view.shadows.local.finish_capture();
        prefilter.encode(device, &mut encoder);
        let rgba16 = prefilter.read(device, queue, encoder).await?;
        Ok(SpecularProbeRadiance {
            face_size,
            texels: SpecularProbeTexels::Rgba16Float(rgba16),
        })
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
        input: &FrameInput,
        settings: &Settings,
        center: Vec3,
        face_size: u32,
    ) -> Result<SpecularProbeRadiance, ProbeError> {
        pollster::block_on(
            self.capture_specular_probe_async(
                device, queue, scene, input, settings, center, face_size,
            ),
        )
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
