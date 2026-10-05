use crate::Renderer;
use crate::content::identity::Identity;
use crate::diagnostics::InstanceVisibilityReport;
use crate::settings::{Antialiasing, Bloom, InstanceVisibility, Settings};
use crate::{Camera, FrameInput, InstanceId, InstanceState, Mobility, ModelId, Scene, perspective};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];

/// The scene's geometry is each test's oracle: a wall that fills the view
/// stands between the camera and one box, another box stands in front of
/// it, and a third is behind the camera, outside the view. Each is the test
/// cube, 12 triangles.
struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    cube: ModelId,
    behind_wall: InstanceId,
    renderer: Renderer,
    settings: Settings,
    input: FrameInput,
    output: wgpu::TextureView,
}

impl Fixture {
    fn new() -> Option<Self> {
        let (device, queue) = crate::test_support::device()?;
        let mut scene = Scene::new(&device, &queue);
        let cube = scene
            .add_asset(&device, &queue, crate::test_support::cube())
            .unwrap()
            .model;
        let settings = Settings {
            antialiasing: Antialiasing::Off,
            bloom: Bloom::Off,
            atmosphere: false,
            ..Settings::default()
        };
        let renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        let output = crate::view::targets::target(
            &device,
            "frame output",
            SIZE,
            crate::shading::gbuffer::COLOR,
        );
        let mut place = |pose| place(&device, &queue, &mut scene, cube, pose);
        place(
            Mat4::from_translation(Vec3::new(0., 0., -6.))
                * Mat4::from_scale(Vec3::new(20., 20., 1.)),
        );
        let behind_wall = place(Mat4::from_translation(Vec3::new(0., 0., -10.)));
        place(Mat4::from_translation(Vec3::new(0., 0., -3.)));
        place(Mat4::from_translation(Vec3::new(0., 0., 5.)));
        Some(Self {
            device,
            queue,
            scene,
            cube,
            behind_wall,
            renderer,
            settings,
            input: FrameInput::new(Camera {
                view: Mat4::IDENTITY,
                projection: perspective(1., 1., 0.1),
                eye: Vec3::ZERO,
            }),
            output,
        })
    }

    /// A frame under `visibility`, submitted and finished or abandoned, and
    /// the camera's opaque triangles it drew and the reports read back
    /// after the device completed it.
    fn frame(
        &mut self,
        visibility: InstanceVisibility,
        submit: bool,
    ) -> (u64, Vec<InstanceVisibilityReport>) {
        self.settings.diagnostics.instance_visibility = visibility;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &self.input,
            &self.settings,
            &self.output,
            None,
        );
        if submit {
            self.queue.submit([encoder.finish()]);
            self.renderer.finish_frame(&mut self.scene);
        }
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let triangles = self.renderer.geometry_stats().total().1;
        (
            triangles,
            self.renderer.take_instance_visibility(&self.device),
        )
    }
}

/// A static `cube` at `pose` in `scene`.
fn place(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    cube: ModelId,
    pose: Mat4,
) -> InstanceId {
    let state = InstanceState {
        pose,
        ..InstanceState::new(cube)
    };
    scene
        .add_instance(device, queue, state, Mobility::Static)
        .unwrap()
}

// Plausible defects: the pass marks the wrong object for a source identity
// (it is the object's index plus one), the readback is never requested or
// never read without a blocking wait, the report counts instances the camera
// list did not draw, the oracle skips instances other than the hidden ones,
// or it skips by index alone, so new content that reuses a hidden
// instance's index stays missing.
#[test]
fn reports_and_skips_the_instances_a_frame_drew_without_a_pixel() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let (drawn, reports) = fixture.frame(InstanceVisibility::Observe, true);
    assert_eq!(drawn, 36, "the three boxes in view");
    assert_eq!(
        reports,
        [InstanceVisibilityReport {
            drawn_instances: 3,
            drawn_triangles: 36,
            hidden_instances: 1,
            hidden_triangles: 12,
        }]
    );
    let (drawn, reports) = fixture.frame(InstanceVisibility::SkipHidden, true);
    assert_eq!(drawn, 24, "the wall and the box before it");
    assert!(reports.is_empty(), "{reports:?}");
    // New content at the hidden box's index, in front of the wall.
    let behind_wall = fixture.behind_wall;
    fixture.scene.remove_instance(behind_wall).unwrap();
    let pose = Mat4::from_translation(Vec3::new(1.5, 0., -3.));
    let Fixture {
        device,
        queue,
        scene,
        cube,
        ..
    } = &mut fixture;
    let beside = place(device, queue, scene, *cube, pose);
    assert_eq!(beside.index(), behind_wall.index());
    let (drawn, _) = fixture.frame(InstanceVisibility::SkipHidden, true);
    assert_eq!(drawn, 36, "the wall and both boxes before it");
}

// Plausible defect: an observed frame abandoned before `finish_frame` leaves
// its readback pending, and the next submitted frame that observes nothing
// maps it though its copy never ran: a report of every instance hidden (or
// stale marks), which the next skipping frame would leave out entirely. No
// observed frame was submitted, so nothing is reported and nothing hidden.
#[test]
fn an_abandoned_observed_frame_reports_and_hides_nothing() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    fixture.frame(InstanceVisibility::Observe, false);
    let (drawn, reports) = fixture.frame(InstanceVisibility::SkipHidden, true);
    assert_eq!(drawn, 36, "the three boxes in view");
    assert!(reports.is_empty(), "{reports:?}");
    let (drawn, _) = fixture.frame(InstanceVisibility::SkipHidden, true);
    assert_eq!(drawn, 36, "the three boxes in view");
}
