//! The diagnostic accessors: a view's last frame's targets, draws and view
//! times, and the readback reports of its geometry and frame probe; the
//! renderer's dynamic GI reports.
#[cfg(feature = "diagnostics")]
use super::Renderer;
use super::{View, frame};
#[cfg(feature = "diagnostics")]
use crate::{ModelId, Scene, SceneError};

impl View {
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
            DiagnosticTarget::Antialiased => {
                frame::completed(self.completed?, &self.post_fx, &self.antialiasing, targets)
            }
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

    /// Polls once for the camera's latest completed per-model geometry
    /// readback. Returns `None` until the first successful readback. If a
    /// newer readback is pending, the previous completed report remains
    /// available. All lookups on this snapshot use the same frame.
    #[cfg(feature = "diagnostics")]
    pub fn geometry_stats_by_model(
        &mut self,
        device: &wgpu::Device,
    ) -> Option<crate::diagnostics::ModelGeometryStats<'_>> {
        self.settle(false);
        self.statistics.poll(device);
        self.statistics
            .latest_by_model()
            .map(|by_model| crate::diagnostics::ModelGeometryStats { by_model })
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
        Ok(self
            .geometry_stats_by_model(device)
            .map(|report| report.for_model(model)))
    }

    /// The numerical frame probe's reports of finished frames read back
    /// since the last call, one JSON object per frame, oldest first
    /// (`Diagnostics::frame_probe`). Readback is asynchronous: a frame's
    /// report arrives once the device completed it. Reports wait here until
    /// taken.
    #[cfg(feature = "diagnostics")]
    pub fn take_frame_probe_reports(&mut self, device: &wgpu::Device) -> Vec<serde_json::Value> {
        self.settle(false);
        self.probe
            .as_mut()
            .map_or_else(Vec::new, |probe| probe.take_reports(device))
    }
}

#[cfg(feature = "diagnostics")]
impl Renderer {
    /// The dynamic GI stage's observed frames (`Diagnostics::dynamic_gi`)
    /// read back since the last call, oldest first. Readback is
    /// asynchronous: a frame's report arrives once the device completed it,
    /// and waits here until taken.
    pub fn take_dynamic_gi_reports(
        &mut self,
        device: &wgpu::Device,
    ) -> Vec<crate::diagnostics::DynamicGiReport> {
        self.shared
            .dynamic_gi
            .as_mut()
            .map_or_else(Vec::new, |dynamic_gi| dynamic_gi.take_reports(device))
    }
}

#[cfg(all(test, feature = "diagnostics", not(target_arch = "wasm32")))]
mod tests {
    use crate::{
        Camera, FrameInput, InstanceState, Mobility, Scene, TestRenderer,
        glam::{Mat4, Vec3},
        settings::{RenderPreset, Settings},
        test_support,
    };

    #[test]
    fn model_geometry_snapshot_follows_the_completed_camera_view() {
        let Some((device, queue)) = test_support::device() else {
            return;
        };
        let size = [16, 16];
        let format = crate::shading::gbuffer::COLOR;
        let settings = Settings {
            preset: RenderPreset::Low,
            ..Settings::default()
        };
        let output = crate::view::targets::target(&device, "geometry snapshot", size, format);
        let mut scene = Scene::new(&device, &queue);
        let mut renderer = TestRenderer::new(&device, &queue, format, size, 1., &settings).unwrap();
        let positions = [-3., 3.].map(|x| Vec3::new(x, 0., 0.));
        let models = positions.map(|position| {
            let model = scene
                .add_asset(&device, &queue, test_support::cube())
                .unwrap()
                .model;
            let mut instance = InstanceState::new(model);
            instance.pose = Mat4::from_translation(position);
            scene
                .add_instance(&device, &queue, instance, Mobility::Static)
                .unwrap();
            model
        });
        assert!(renderer.view.geometry_stats_by_model(&device).is_none());

        for (visible, position) in positions.into_iter().enumerate() {
            let eye = position + Vec3::Z * 3.;
            let input = FrameInput::new(Camera {
                eye,
                view: crate::glam::camera::rh::view::look_at_mat4(eye, position, Vec3::Y),
                projection: crate::perspective(0.8, 1., 0.1),
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.render(
                &device,
                &queue,
                &mut encoder,
                &mut scene,
                &input,
                &settings,
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            renderer.finish_frame(&mut scene);
            renderer.view.geometry_stats_by_model(&device);
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let report = renderer.view.geometry_stats_by_model(&device).unwrap();
            assert_eq!(report.for_model(models[visible]).1, 12);
            assert_eq!(report.for_model(models[1 - visible]), (0, 0));
        }
    }
}
