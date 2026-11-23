//! What every view of a scene shares: the bind group layouts and the
//! geometry pipeline cache, prepare's scene part, the deform stage, the
//! dynamic GI stage with its probes and the device's ray-query form. A
//! frame's scene work runs once, in its primary view's stage order
//! (`view_stages::begin_view`, `Role`).
use super::effective;
use crate::FrameInput;
use crate::Scene;
use crate::stages::{deform::Deform, dynamic_gi::DynamicGi, prepare::Prepare};
use crate::view::bindings::Layouts;
use crate::view::effective::Effective;
use crate::view::pipelines::GeometryPipelines;
use crate::view::trace_paths::DeviceRayForm;

pub(super) struct Shared {
    pub layouts: Layouts,
    pub pipelines: GeometryPipelines,
    /// Prepare's scene part: the frame's acceleration structures.
    pub prepare: Prepare,
    pub deform: Deform,
    /// The dynamic GI stage, on the Extended binding tier alone.
    pub dynamic_gi: Option<DynamicGi>,
    /// The device traces rays in hardware, in this form.
    pub ray_form: Option<DeviceRayForm>,
}

impl Shared {
    /// The resources shared by every view on this device.
    pub fn new(device: &wgpu::Device) -> Self {
        let layouts = Layouts::new(device);
        let dynamic_gi = (layouts.lit.tier == crate::shading::bind::BindingTier::Extended)
            .then(|| DynamicGi::new(device));
        let pipelines = GeometryPipelines::new(
            device,
            &layouts.lit,
            [
                &layouts.shadow,
                &layouts.scene,
                &layouts.blended,
                &layouts.shadow_mask,
                &layouts.screen_indirect,
                &layouts.shadow_mask_screen_indirect,
                &layouts.contact_shadow,
                &layouts.contact_shadow_screen_indirect,
                &layouts.caster_positions,
            ],
        );
        // The form recorded for the device's backend
        // (`shading::scene_rays::METAL_FORM`, `LOWERED_FORM`).
        let ray_form = crate::scene::rays::acceleration::supported(device).then(|| {
            DeviceRayForm::new(crate::shading::RayQueryForm::of_backend(
                device.adapter_info().backend,
            ))
        });
        Self {
            layouts,
            pipelines,
            prepare: Prepare::default(),
            deform: Deform::new(device),
            dynamic_gi,
            ray_form,
        }
    }

    /// What the device gives a view's effective configuration, with that
    /// view's FSR and occlusion culling support.
    pub fn device(
        &self,
        fsr_running: Option<crate::settings::FsrVersion>,
        occlusion_supported: bool,
    ) -> effective::Device {
        effective::Device {
            fsr_running,
            fused_supported: self.pipelines.fused_supported,
            occlusion_supported,
            ray_queries: self.ray_form.as_ref().map(DeviceRayForm::form),
            tier: self.pipelines.tier,
        }
    }

    /// Select the primary view's geometry specialisation and material
    /// filtering, then prepare the scene. Update ray instances when any
    /// view traces (`Prepare::traces`).
    pub fn run_scene(
        &mut self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        scene: &mut Scene,
        effective: &Effective,
        input: &FrameInput,
        traced: bool,
    ) {
        self.pipelines.specialise(effective.layers, scene);
        scene
            .materials
            .set_anisotropy(device, &scene.render_textures, effective.anisotropy);
        self.prepare
            .run_scene(device, queue, scene, effective, input, traced);
    }
}
