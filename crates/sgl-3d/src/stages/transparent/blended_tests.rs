//! Blended surfaces observed in real frames: what they leave of the opaque
//! frame's targets.
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::{
    AlphaMode, Camera, DirectionalLight, FrameInput, InstanceState, Mobility, Scene, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];

/// A square facing +Z at depth `z`, `half` metres from its centre to each
/// side, with the fixture material.
fn square(z: f32, half: f32) -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| crate::asset::Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * half, y * half, z],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset
}

/// The depth, motion and composed colour of the second of two frames in
/// which a moving blended square crosses in front of an opaque floor,
/// drawn by the camera when `shown`.
fn frames(device: &wgpu::Device, queue: &wgpu::Queue, shown: bool) -> [Vec<u8>; 3] {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    test_support::add_static(device, queue, &mut scene, square(-6., 8.));
    // Authored opaque and made blended by an edit, as a game may.
    let glass = scene.add_asset(device, queue, square(-3., 0.5)).unwrap();
    let mut values = scene.material(glass.materials[0]).unwrap();
    values.base = [0.2, 0.9, 0.3, 0.5];
    values.alpha = AlphaMode::Blend;
    scene
        .set_material(queue, glass.materials[0], values)
        .unwrap();
    let model = glass.model;
    let mut state = InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible: shown,
        capture_visible: true,
    };
    let instance = scene
        .add_instance(device, queue, state, Mobility::Moving)
        .unwrap();
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::NEG_Z,
        color: [1.; 3],
        illuminance: 1.,
        shadow: None,
        ..Default::default()
    });
    let output = crate::view::targets::target(device, "blended frames", SIZE, gbuffer::COLOR);
    for x in [-0.3, 0.3] {
        state.pose = Mat4::from_translation(Vec3::X * x);
        scene.set_instance(queue, instance, state).unwrap();
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let targets = renderer.targets();
    [
        (&targets.depth, 4),
        (&targets.motion, 4),
        (&targets.composite, 8),
    ]
    .map(|(view, bytes)| test_support::read(device, queue, view.texture(), bytes))
}

// Plausible defects: blended surfaces drawn by an opaque pass, into its
// depth, motion or G-buffer, or by a pass that writes depth; or not drawn at
// all. The oracle is the same frames with the blended surface hidden: a
// moving blended square in front of an opaque floor leaves the frame's depth
// and motion exactly as they are without it, while the composed frame
// changes where the square is.
#[test]
fn blended_surfaces_leave_depth_and_motion_unchanged() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let [depth, motion, composed] = frames(&device, &queue, false);
    let [shown_depth, shown_motion, shown_composed] = frames(&device, &queue, true);
    assert!(depth == shown_depth, "a blended surface changed the depth");
    assert!(
        motion == shown_motion,
        "a blended surface changed the motion"
    );
    let changed = composed
        .chunks_exact(8)
        .zip(shown_composed.chunks_exact(8))
        .filter(|(a, b)| a != b)
        .count();
    assert!(changed > 0, "the blended surface was not drawn");
}

// Plausible defects: blended surfaces leave FSR2's masks unwritten, write
// them where they are not, write other values than AMD documents, or a later
// transparent pass clears what they wrote. The oracle is AMD's FSR
// documentation ("Reactive mask": alpha, clamped to about 0.9) and its
// sample's translucency (transparency and composition: alpha): behind a
// blended square of alpha 0.4 the masks hold 0.4 and 0.4, behind one of 0.95
// they hold 0.9 and 0.95, and between them nothing.
#[test]
fn blended_surfaces_write_fsr2s_masks() {
    let Some((device, queue)) = test_support::fsr2_device() else {
        return;
    };
    let settings = Settings {
        antialiasing: settings::Antialiasing::Fsr2,
        fsr2_quality: settings::Fsr2Quality::NativeAa,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    if renderer.antialiasing_in_effect(&settings) != settings::Antialiasing::Fsr2 {
        eprintln!(
            "skipping: FSR2 does not run here: {:?}",
            renderer.fsr2_error()
        );
        return;
    }
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, square(-6., 8.));
    for (x, alpha) in [(-0.7, 0.4), (0.7, 0.95)] {
        let mut glass = square(-3., 0.5);
        glass.materials[0].base = [0.2, 0.9, 0.3, alpha];
        glass.materials[0].alpha = AlphaMode::Blend;
        let model = scene.add_asset(&device, &queue, glass).unwrap().model;
        let state = InstanceState {
            model,
            pose: Mat4::from_translation(Vec3::X * x),
            visible: true,
            capture_visible: true,
        };
        scene
            .add_instance(&device, &queue, state, Mobility::Static)
            .unwrap();
    }
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.camera_cut = true;
    let output = crate::view::targets::target(&device, "FSR2 masks frame", SIZE, gbuffer::COLOR);
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
    let [reactive, composition] = renderer
        .targets()
        .fsr2_masks
        .each_ref()
        .map(|mask| test_support::read(&device, &queue, mask.texture(), 1));
    // Behind the squares' centres and between them, on the middle row. At
    // 3 m with cot(0.5) = 1.83, x = -0.7 and +0.7 m project to NDC -0.43 and
    // +0.43, texels 9.2 and 22.8 of 32; the squares' inner edges (x = -0.2
    // and +0.2 m) to texels 14.0 and 18.0.
    for (column, expected) in [(9, [0.4, 0.4]), (22, [0.9, 0.95]), (16, [0., 0.])] {
        let at = (SIZE[1] / 2 * SIZE[0] + column) as usize;
        let actual = [reactive[at], composition[at]].map(|byte| f32::from(byte) / 255.);
        for (name, actual, expected) in [
            ("reactive", actual[0], expected[0]),
            ("transparency and composition", actual[1], expected[1]),
        ] {
            assert!(
                (actual - expected).abs() <= 1. / 255.,
                "column {column}: {name} mask {actual}, expected {expected}"
            );
        }
    }
}
