//! Static specular probe capture, a renderer operation for authoring: the
//! prepare stage builds the capture's six face views, the shadow and opaque
//! stages render the scene's static content, sky and lights into them, and
//! the probe prefilter GGX-prefilters the cube and reads it back.
use super::Renderer;
use crate::baked_specular_probe::{ProbeError, SpecularProbeRadiance, SpecularProbeTexels};
use crate::scene::probes::validate_face_size;
use crate::settings::Settings;
use crate::stages::probe_prefilter::ProbePrefilter;
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
    /// shadow cache, so the next frame draws every shadow again. This blocks
    /// for GPU readback; it is for asset authoring, never a runtime loop.
    /// WebGPU cannot block, so in a browser it fails with
    /// `ProbeError::Readback`: bake natively and load the result.
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
        if !center.is_finite() {
            return Err(ProbeError::InvalidProbe("coordinates must be finite"));
        }
        validate_face_size(face_size, &device.limits())?;
        // Decals added since the last frame take their atlas first.
        scene.upload_decals(device, queue);
        // Materials sample as the settings filter them, as a frame does.
        scene
            .materials
            .set_anisotropy(device, settings.anisotropic_filtering.clamp());
        let scene = &*scene;
        self.pipelines.specialise(
            device,
            LayerConstants::new(&settings.diagnostics_in_effect().disable),
            scene,
        );
        let prefilter = ProbePrefilter::new(device, face_size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("author static specular probe"),
        });
        // The capture draws into the frame's shadow maps, at the settings'
        // quality, and its groups bind them.
        self.shadows.resize(device, settings.shadow_quality);
        self.bindings.shadow_maps = self.shadows.maps();
        // The volume the committed probes light, and those probes: what the
        // last submitted frame left, never a fresher frame's abandoned ones.
        let dynamic_gi = self
            .dynamic_gi
            .lights(scene)
            .filter(|_| settings.dynamic_gi.rays().is_some());
        let mut views = self.prepare.capture(
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
        let local_records = self.shadows.local.plan_capture(
            device,
            queue,
            &self.bindings,
            (scene, &mut views.instances),
            center,
            input.visibility_mask,
            !settings.diagnostics_in_effect().disable.local_lights,
        );
        // Every list the capture draws is built.
        views.instances.upload(device, queue);
        let cascades: Vec<_> = views
            .cascades
            .iter()
            .map(|view| self.bindings.shadow_group(device, view, &views.frame))
            .collect();
        self.shadows.directional.encode_capture(
            &mut encoder,
            scene,
            &self.pipelines,
            (&views.casters, &views.instances),
            &cascades,
        );
        self.shadows
            .local
            .encode_capture(&mut encoder, (scene, &self.pipelines), &views.instances);
        // Surfaces here sample the installed probes (surface.wgsl).
        let probes = scene
            .specular_probes()
            .unwrap_or(&self.bindings.empty_probes);
        for (index, view) in views.faces.iter().enumerate() {
            let lit = self.bindings.lit_group(
                device,
                scene,
                input.environment,
                [view, &views.frame],
                probes,
                views.clusters.buffer(),
                self.bindings.static_local_shadows(&local_records),
                dynamic_gi.map_or(self.dynamic_gi.stand_in(), |(_, probes)| probes),
            );
            let sky =
                self.bindings
                    .unlit_group(device, scene, input.environment, view, &views.frame);
            self.opaque.encode_capture(
                &mut encoder,
                scene,
                &self.pipelines,
                (&views.list, &views.instances),
                &prefilter.face(index),
                &sky,
                &lit,
            );
        }
        prefilter.encode(device, &mut encoder);
        let rgba16 = prefilter.read(device, queue, encoder)?;
        self.shadows.local.finish_capture();
        Ok(SpecularProbeRadiance {
            face_size,
            texels: SpecularProbeTexels::Rgba16Float(rgba16),
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
