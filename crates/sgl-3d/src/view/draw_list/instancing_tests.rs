//! One instanced draw, read back: each of its instances at its own pose,
//! with its own motion, source identity and baked light.
use crate::asset::{CpuMesh, Vertex};
use crate::settings::{Antialiasing, Settings};
use crate::static_lighting::AmbientCube;
use crate::{
    Backdrop, Camera, FrameInput, InstanceState, Mobility, Renderer, Scene, diagnostics,
    test_support,
};
use glam::{Mat4, Vec2, Vec3, Vec4Swizzles};

const SIZE: u32 = 64;

/// Where `point` lands on the render target, in texture coordinates.
fn screen(clip_from_world: Mat4, point: Vec3) -> Vec2 {
    let clip = clip_from_world * point.extend(1.);
    clip.xy() / clip.w * Vec2::new(0.5, -0.5) + 0.5
}

// Plausible defects: every instance of a draw drawn at one instance's pose
// (the draw's first, or the object record a draw binds), with its motion or
// identity; the draw's instance index read as the object index, so instances
// take another's record (the instances alternate between the static and
// moving draws after one the camera does not draw, so their places in the
// draws differ from their indices); a
// fragment reading another instance's record; a static instance merged into
// the moving instances' draw and given motion. The oracle projects each
// instance's authored poses through the camera independently of the
// renderer: at the pixel where its quad's centre lands, depth is the quad's
// plane there, motion is the screen offset back to the same point at its
// previous pose, and the source identity is the instance's own. Its colour,
// which only baked diffuse light reaches, is its own ambient cube's single
// primary for a moving instance, and black for a static one, which takes
// none.
#[test]
fn one_instanced_draw_renders_each_instance_at_its_own_pose() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut asset = test_support::cube();
    asset.materials[0].base = [1.; 4];
    asset.materials[0].metallic = 0.;
    // A quad facing the camera, in the model's XY plane.
    asset.meshes = vec![CpuMesh {
        vertices: [(-0.4, -0.4), (0.4, -0.4), (0.4, 0.4), (-0.4, 0.4)]
            .map(|(x, y)| Vertex {
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, y, 0.],
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }];
    let mut scene = Scene::new(&device, &queue);
    let model = scene.add_asset(&device, &queue, asset).unwrap().model;
    // Moving and static instances alternate; moving ones have an ambient
    // cube of one primary (red, green or blue) and move by their own offsets
    // between the frames. One is mirrored, which a double-sided material
    // draws with the same pipeline. Each is at its own depth.
    let moving = |offset, mirrored, primary| Some((offset, mirrored, primary));
    let placed = [
        (
            Vec3::new(-1.2, 0.6, -4.),
            moving(Vec3::new(0.05, 0., 0.), false, 0),
        ),
        (Vec3::new(0., 0.6, -4.5), None),
        (
            Vec3::new(1.2, 0.6, -5.),
            moving(Vec3::new(-0.06, 0.04, 0.), true, 1),
        ),
        (Vec3::new(-1.2, -0.6, -5.5), None),
        (
            Vec3::new(0., -0.6, -6.),
            moving(Vec3::new(0., -0.08, 0.), false, 2),
        ),
        (
            Vec3::new(1.2, -0.6, -6.5),
            moving(Vec3::new(0.1, 0.07, 0.), false, 0),
        ),
    ];
    // First, an instance the camera does not draw: the drawn instances'
    // places in the draws are then no permutation of their indices.
    let hidden = InstanceState {
        model,
        pose: Mat4::from_translation(Vec3::new(0., 0., -3.)),
        visible: false,
        capture_visible: false,
    };
    scene
        .add_instance(&device, &queue, hidden, Mobility::Static)
        .unwrap();
    let pose = |position: Vec3, mirrored: bool| {
        let scale = if mirrored {
            Vec3::new(-1., 1., 1.)
        } else {
            Vec3::ONE
        };
        Mat4::from_translation(position) * Mat4::from_scale(scale)
    };
    let instances: Vec<_> = placed
        .iter()
        .map(|&(position, motion)| {
            let state = InstanceState {
                model,
                pose: pose(position, motion.is_some_and(|(_, mirrored, _)| mirrored)),
                visible: true,
                capture_visible: true,
            };
            let mobility = if motion.is_some() {
                Mobility::Moving
            } else {
                Mobility::Static
            };
            let instance = scene
                .add_instance(&device, &queue, state, mobility)
                .unwrap();
            if let Some((_, _, primary)) = motion {
                let mut irradiance = [0.; 3];
                irradiance[primary] = 1.;
                scene
                    .set_instance_baked_irradiance(
                        &queue,
                        instance,
                        AmbientCube {
                            irradiance: [irradiance; 6],
                        },
                    )
                    .unwrap();
            }
            instance
        })
        .collect();
    let settings = Settings {
        antialiasing: Antialiasing::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, [SIZE; 2], &settings);
    let camera = Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    };
    let clip_from_world = camera.projection * camera.view;
    // Baked diffuse light is the only light.
    let mut input = FrameInput::new(camera);
    input.diffuse_environment.intensity = 0.;
    input.backdrop = Backdrop::Color([0.; 3]);
    let output = crate::view::targets::target(
        &device,
        "instanced output",
        [SIZE; 2],
        crate::shading::gbuffer::COLOR,
    );
    // The first frame, whose poses the second's motion is measured from.
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
    // The first frame's draws, read back once it completes: each quad one
    // section of two triangles, by mobility; the hidden instance none.
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let stats = renderer.geometry_stats(&device).expect("a completed frame");
    assert_eq!(stats.moving_instances, (4, 4 * 2), "the moving quads");
    assert_eq!(stats.static_instances, (2, 2 * 2), "the static quads");
    for (&(position, motion), &instance) in placed.iter().zip(&instances) {
        if let Some((offset, mirrored, _)) = motion {
            let state = InstanceState {
                model,
                pose: pose(position + offset, mirrored),
                visible: true,
                capture_visible: true,
            };
            scene.set_instance(&queue, instance, state).unwrap();
        }
    }
    let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
    let fused = renderer.test_fused_supported();
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
    queue.submit([encoder.finish()]);
    let targets = renderer.targets();
    let identities = test_support::read(&device, &queue, targets.source_id.texture(), 8);
    let motions = test_support::read(&device, &queue, targets.motion.texture(), 4);
    let depths = test_support::read(&device, &queue, targets.depth.texture(), 4);
    let colors = test_support::read(&device, &queue, targets.color.texture(), 8);
    for (index, &(position, motion)) in placed.iter().enumerate() {
        let offset = motion.map_or(Vec3::ZERO, |(offset, _, _)| offset);
        let current = position + offset;
        let texel = (screen(clip_from_world, current) * SIZE as f32).floor();
        let pixel = (texel.y as u32 * SIZE + texel.x as u32) as usize;
        // The point of the quad's plane under the pixel's centre, now and
        // at its previous pose.
        let ndc = (texel + 0.5) / SIZE as f32 * Vec2::new(2., -2.) + Vec2::new(-1., 1.);
        let ray = (clip_from_world.inverse() * ndc.extend(0.5).extend(1.)).xyz();
        let ray = ray / ray.z * current.z;
        let previous = ray - offset;
        let clip = clip_from_world * ray.extend(1.);
        let expected_depth = clip.z / clip.w;
        let expected_motion = screen(clip_from_world, ray) - screen(clip_from_world, previous);
        let word = |bytes: &[u8], at: usize| <[u8; 4]>::try_from(&bytes[at..at + 4]).unwrap();
        let identity = u32::from_le_bytes(word(&identities, pixel * 8));
        let depth = f32::from_le_bytes(word(&depths, pixel * 4));
        let motion_read = Vec2::new(
            test_support::half(&motions[pixel * 4..pixel * 4 + 2]),
            test_support::half(&motions[pixel * 4 + 2..pixel * 4 + 4]),
        );
        let color: Vec<f32> = (0..3)
            .map(|channel| {
                let at = pixel * 8 + channel * 2;
                test_support::half(&colors[at..at + 2])
            })
            .collect();
        assert_eq!(
            identity,
            diagnostics::source_id(instances[index]),
            "instance {index}'s identity"
        );
        assert!(
            (depth - expected_depth).abs() < 1e-6,
            "instance {index}: depth {depth}, expected {expected_depth}"
        );
        assert!(
            (motion_read - expected_motion).abs().max_element() < 2e-4,
            "instance {index}: motion {motion_read}, expected {expected_motion}"
        );
        for (channel, &value) in color.iter().enumerate() {
            let lit = motion.is_some_and(|(_, _, primary)| primary == channel);
            assert!(
                if lit { value > 0.1 } else { value < 1e-3 },
                "instance {index}: colour {color:?}"
            );
        }
    }
}
