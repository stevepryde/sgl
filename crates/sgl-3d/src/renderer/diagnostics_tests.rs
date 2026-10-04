//! The diagnostics configuration through the real frame.
use super::Renderer;
use crate::diagnostics::DiagnosticTarget;
use crate::settings::{Antialiasing, Bloom, Settings};
use crate::test_support::{self, half, read};
use crate::{Backdrop, Camera, FrameInput, InstanceState, Mobility, Scene, perspective};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];

// Plausible defects: the geometry pipelines keep the constants they were
// created with when the diagnostics change, or are not restored; the frame
// probe never queues its readback at `finish_frame`, so no report reaches the
// game; the tone-mapped target is reported for frames that did not write it.
// The oracle: an instance's own emission is the only light in the frame, so
// switching instance emission off must darken it, and switching it on again
// must reproduce the first frame exactly.
#[test]
fn diagnostics_switch_layers_between_frames_and_return_probe_reports() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    // The world behind the camera; an emissive instance ahead of it.
    let mut world = test_support::cube();
    for vertex in &mut world.meshes[0].vertices {
        vertex.position[2] += 10.;
    }
    let mut emitter = test_support::cube();
    emitter.materials[0].emissive = [4.; 3];
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, world);
    let emitter = scene.add_asset(&device, &queue, emitter).unwrap();
    scene
        .add_instance(
            &device,
            &queue,
            InstanceState {
                model: emitter.model,
                pose: Mat4::from_translation(Vec3::new(0., 0., -3.)),
                visible: true,
                capture_visible: true,
            },
            Mobility::Moving,
        )
        .unwrap();
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.diffuse_environment.intensity = 0.;
    input.backdrop = Backdrop::Color([0.; 3]);
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
    let frame = |renderer: &mut Renderer, scene: &mut Scene, settings: &Settings| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            &input,
            settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        let composite = renderer
            .diagnostic_target(DiagnosticTarget::Composite)
            .unwrap();
        read(&device, &queue, composite.texture(), 8)
    };
    let center = |pixels: &[u8]| {
        let at = ((SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) * 8) as usize;
        half(&pixels[at..at + 2])
    };
    let emitting = frame(&mut renderer, &mut scene, &settings);
    assert!(center(&emitting) > 1., "{}", center(&emitting));
    assert!(
        renderer
            .diagnostic_target(DiagnosticTarget::ToneMapped)
            .is_none(),
        "the tone-mapped target was not captured"
    );
    settings.diagnostics.disable.instance_emission = true;
    let dark = frame(&mut renderer, &mut scene, &settings);
    assert!(center(&dark) < 0.01, "{}", center(&dark));
    settings.diagnostics.disable.instance_emission = false;
    settings.diagnostics.frame_probe = true;
    let restored = frame(&mut renderer, &mut scene, &settings);
    assert!(restored == emitting, "re-enabled emission differs");
    assert!(
        renderer
            .diagnostic_target(DiagnosticTarget::ToneMapped)
            .is_some()
    );
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let reports = renderer.take_frame_probe_reports(&device);
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0]["frame"], 0);
    assert_eq!(reports[0]["lit_scene"]["observed"], true);
    assert!(renderer.take_frame_probe_reports(&device).is_empty());
}
