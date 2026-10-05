//! World-space reflection rays observed in the reflection composite of real
//! frames.
use crate::asset::{CpuMesh, Vertex};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::{Backdrop, Camera, FrameInput, InstanceState, Mobility, Scene, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// A mirror floor facing +Y at height 0, 50 m from the origin to each side.
fn floor() -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, z)| Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * 50., 0., z * 50.],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    let material = &mut asset.materials[0];
    material.base = [1.; 4];
    material.metallic = 1.;
    material.roughness = 0.05;
    asset
}

/// An unlit white unit cube that does not emit into global illumination,
/// which reflections still show glowing.
fn wall() -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.materials[0].unlit = true;
    asset.materials[0].base = [1.; 4];
    asset.materials[0].emits_into_gi = false;
    asset
}

/// The reflection composite's red at every pixel.
fn composite(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &Renderer) -> Vec<f32> {
    let texels = test_support::read(device, queue, renderer.targets().composite.texture(), 8);
    texels.chunks_exact(8).map(test_support::half).collect()
}

/// `asset` deformed: each vertex whole on one joint, at its bind pose.
fn deforming(mut asset: crate::asset::Asset) -> crate::asset::Asset {
    for mesh in &mut asset.meshes {
        mesh.deformation.influences = vec![
            crate::deformation::Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            mesh.vertices.len()
        ];
    }
    asset
}

/// The reflection composite's red at every pixel of two frames on `device`
/// with world-space rays and the hardware path as `hardware` says: a
/// camera 2 m above a mirror floor looking down 45° at a moving unlit white
/// wall (`deforms` where it deforms) 300 m ahead, then 1.1 km ahead, with
/// an instance out of sight added between them, which grows the instance
/// entries and so replaces the hardware path's TLAS.
fn reflected_wall(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    hardware: bool,
    deforms: bool,
) -> [Vec<f32>; 2] {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        screen_space_reflections: settings::ScreenSpaceReflections::Half,
        world_space_reflections: true,
        hardware_ray_tracing: hardware,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    test_support::add_static(device, queue, &mut scene, floor());
    let wall = if deforms { deforming(wall()) } else { wall() };
    let model = scene.add_asset(device, queue, wall).unwrap().model;
    let wall_at = |z: f32| InstanceState {
        model,
        pose: Mat4::from_scale_rotation_translation(
            Vec3::new(1200., 1100., 10.),
            glam::Quat::IDENTITY,
            Vec3::new(0., 560., z),
        ),
        visible: true,
        capture_visible: true,
    };
    let wall = scene
        .add_instance(device, queue, wall_at(-300.), Mobility::Moving)
        .unwrap();
    let eye = Vec3::new(0., 2., 0.);
    let mut input = FrameInput::new(Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::new(0., -1., -1.), Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    let output = crate::view::targets::target(
        device,
        "world reflection frames",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    let frame = |scene: &mut Scene, renderer: &mut Renderer, input: &FrameInput| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        composite(device, queue, renderer)
    };
    let near = frame(&mut scene, &mut renderer, &input);
    assert_eq!(renderer.ray_tracing_in_effect(&settings), hardware);
    scene.set_instance(queue, wall, wall_at(-1100.)).unwrap();
    let cube = scene
        .add_asset(device, queue, test_support::cube())
        .unwrap()
        .model;
    let below = InstanceState {
        pose: Mat4::from_translation(Vec3::new(0., -50., 0.)),
        ..InstanceState::new(cube)
    };
    scene
        .add_instance(device, queue, below, Mobility::Static)
        .unwrap();
    input.camera_cut = true;
    let far = frame(&mut scene, &mut renderer, &input);
    [near, far]
}

/// Whether the floor reflects the wall: at more than a quarter of the
/// pixels near, and nowhere far (beyond the rays' 1000 m).
fn reflects([near, far]: &[Vec<f32>; 2], label: &str) -> bool {
    assert!(
        far.iter().all(|&red| red < 0.01),
        "{label}: the floor reflects the wall 1.1 km away, beyond the rays' 1000 m"
    );
    near.iter().filter(|&&red| red > 0.25).count() > near.len() / 4
}

// Plausible defects: world-space rays stopping short of Wicked's 1000 m
// range (`Postprocess_RTReflection`), such as at the 100 m they reached
// before, or a range lost between the parameters and the trace. The oracle
// is geometric: a camera 2 m above a mirror floor looks down 45°, so the
// floor it sees reflects directions 16° to 74° above the horizon, which the
// frame does not show and screen-space reflections cannot trace. A moving
// unlit white wall 300 m ahead spans those directions up to about 1000 m
// away, so the floor reflects it through world-space rays. Moved 1.1 km
// away it still spans some of those directions, but every ray to it is longer
// than 1100 m, so a range of 1000 m reflects nothing there and the floor shows
// only the black backdrop; an unlimited trace would still reflect it. The
// wall does not emit into GI, which keeps its light out of the dynamic GI
// probes alone: a reflection ray's hit that dropped it would show it black.
#[test]
fn world_space_rays_reflect_a_moving_wall_300_metres_away() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    assert!(
        reflects(&reflected_wall((&device, &queue), false, false), "portable"),
        "the floor reflects the wall 300 m away"
    );
}

// The same rays on the hardware path (the architecture's Hardware ray
// tracing), on a device with ray queries; reported unsupported, never
// passed, elsewhere. Plausible defects: the trace's pipeline composing the
// portable function set, binding no TLAS, or selecting static instances
// rather than moving ones on the hardware path, which then reflects
// nothing or something else; its cached group binding a TLAS the scene
// replaced, which still holds the wall 300 m ahead; a deforming instance missing from the
// hardware path's rays although its TLAS holds it as a moving instance; or
// one reaching the portable path's, which sees no deforming instance. The
// oracle is the geometry above: the hardware path reflects the wall as the
// portable path does, and a deforming wall in its place only on the
// hardware path.
#[test]
fn world_space_rays_on_the_hardware_path_reflect_deforming_instances() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    for (hardware, deforms, reflected) in [
        (true, false, true),
        (true, true, true),
        (false, true, false),
    ] {
        let label = format!("hardware {hardware}, deforming {deforms}");
        assert_eq!(
            reflects(
                &reflected_wall((&device, &queue), hardware, deforms),
                &label
            ),
            reflected,
            "{label}"
        );
    }
}
