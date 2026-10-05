use crate::Renderer;
use crate::content::identity::Identity;
use crate::diagnostics::InstanceVisibilityReport;
use crate::settings::{Antialiasing, Bloom, InstanceVisibility, Settings};
use crate::{Camera, FrameInput, InstanceState, Mobility, Scene, perspective};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];

// Plausible defects: the pass marks the wrong object for a source identity
// (it is the object's index plus one), the readback is never requested or
// never read without a blocking wait, the report counts instances the camera
// list did not draw, the oracle skips instances other than the hidden ones,
// or it skips by index alone, so new content that reuses a hidden
// instance's index stays missing. The oracle is the scene's geometry: a wall
// that fills the view stands between the camera and one box, another box
// stands in front of it, and a third is behind the camera, outside the
// view. Each is the test cube, 12 triangles.
#[test]
fn reports_and_skips_the_instances_a_frame_drew_without_a_pixel() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let cube = scene
        .add_asset(&device, &queue, crate::test_support::cube())
        .unwrap()
        .model;
    let place = |scene: &mut Scene, pose: Mat4| {
        let state = InstanceState {
            pose,
            ..InstanceState::new(cube)
        };
        scene
            .add_instance(&device, &queue, state, Mobility::Static)
            .unwrap()
    };
    place(
        &mut scene,
        Mat4::from_translation(Vec3::new(0., 0., -6.)) * Mat4::from_scale(Vec3::new(20., 20., 1.)),
    );
    let behind_wall = place(&mut scene, Mat4::from_translation(Vec3::new(0., 0., -10.)));
    place(&mut scene, Mat4::from_translation(Vec3::new(0., 0., -3.)));
    place(&mut scene, Mat4::from_translation(Vec3::new(0., 0., 5.)));
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let mut settings = Settings {
        antialiasing: Antialiasing::Off,
        bloom: Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = crate::view::targets::target(
        &device,
        "frame output",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    // A submitted frame under `visibility`, and the camera's opaque
    // triangles it drew.
    let mut frame = |scene: &mut Scene, visibility| {
        settings.diagnostics.instance_visibility = visibility;
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let triangles = renderer.geometry_stats().total().1;
        (triangles, renderer.take_instance_visibility(&device))
    };
    let (drawn, reports) = frame(&mut scene, InstanceVisibility::Observe);
    assert_eq!(drawn, 36, "the three boxes in view");
    assert_eq!(
        reports,
        [InstanceVisibilityReport {
            drawn: (3, 36),
            hidden: (1, 12),
        }]
    );
    let (drawn, reports) = frame(&mut scene, InstanceVisibility::SkipHidden);
    assert_eq!(drawn, 24, "the wall and the box before it");
    assert!(reports.is_empty(), "{reports:?}");
    // New content at the hidden box's index, in front of the wall.
    scene.remove_instance(behind_wall).unwrap();
    let beside = place(&mut scene, Mat4::from_translation(Vec3::new(1.5, 0., -3.)));
    assert_eq!(beside.index(), behind_wall.index());
    let (drawn, _) = frame(&mut scene, InstanceVisibility::SkipHidden);
    assert_eq!(drawn, 36, "the wall and both boxes before it");
}
