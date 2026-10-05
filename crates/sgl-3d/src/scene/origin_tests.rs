//! Moving the render origin (`Scene::move_origin`): what the scene holds
//! becomes what the game would have given it in the new frame, motion and
//! the renderer's camera history carry across the move, and nothing is
//! translated twice by an abandoned frame.
use super::content_tests::uploaded_record;
use crate::asset::Image;
use crate::baked_specular_probe::{
    BakedSpecularProbe, SpecularProbeBox, SpecularProbeRadiance, SpecularProbeTexels,
};
use crate::content::transient::{FogVolume, Glow, GlowKind, HeatDistortion};
use crate::renderer::Renderer;
use crate::settings::Settings;
use crate::*;
use glam::{Mat4, Quat, Vec3};

fn state(model: ModelId, pose: Mat4) -> InstanceState {
    InstanceState {
        model,
        pose,
        visible: true,
        capture_visible: true,
    }
}

/// A chunk-aligned move far from the scene's first origin, which is no
/// whole number of any position's units.
const MOVE: Vec3 = Vec3::new(4096., -64., -8192.);

// Plausible defects: a move that translates an instance's current pose but
// not the pose its motion is measured from (a frame of motion -MOVE on every
// moving instance), writes a static instance motion, or forgets to rewrite
// the records. The oracle is the vertex shader's own record: the model
// origin less the move, and the motion the instance had.
#[test]
fn a_move_translates_records_and_keeps_their_motion() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let at = |position: Vec3| state(model, Mat4::from_translation(position));
    let fixed_at = Vec3::new(4100.5, 2., -8190.25);
    let fixed = scene
        .add_instance(&device, &queue, at(fixed_at), Mobility::Static)
        .unwrap();
    let moving = scene
        .add_instance(
            &device,
            &queue,
            at(Vec3::new(4090., 1., -8200.)),
            Mobility::Moving,
        )
        .unwrap();
    scene.finish_frame();
    scene
        .set_instance(&queue, moving, at(Vec3::new(4092., 1., -8201.)))
        .unwrap();
    scene.move_origin(&device, &queue, MOVE).unwrap();
    let record = uploaded_record(&device, &queue, &scene, fixed);
    assert_eq!(record.position, (fixed_at - MOVE).to_array());
    assert_eq!(record.motion, [0.; 3], "a static instance writes no motion");
    let record = uploaded_record(&device, &queue, &scene, moving);
    assert_eq!(record.position, [-4., 65., -9.]);
    assert_eq!(record.motion, [2., 0., -1.], "the move kept the motion");
    // The game poses it in the new frame: its motion is measured from the
    // last submitted pose, translated.
    scene.finish_frame();
    scene
        .set_instance(&queue, moving, at(Vec3::new(-1., 65., -9.)))
        .unwrap();
    let record = uploaded_record(&device, &queue, &scene, moving);
    assert_eq!(record.motion, [3., 0., 0.]);
    assert!(matches!(
        scene.move_origin(&device, &queue, Vec3::new(f32::NAN, 0., 0.)),
        Err(SceneError::InvalidOrigin)
    ));
}

/// A probe at `center` whose influence spans 2 m about it.
fn probe(center: Vec3) -> BakedSpecularProbe {
    let face_size = 64;
    let texels: usize = (0..7)
        .map(|level| (face_size >> level) * (face_size >> level) * 6)
        .sum();
    BakedSpecularProbe {
        center,
        world_to_local: Mat4::from_translation(-center),
        influence: SpecularProbeBox {
            min: Vec3::splat(-2.),
            max: Vec3::splat(2.),
        },
        blend: Vec3::splat(0.5),
        proxy: None,
        radiance: SpecularProbeRadiance {
            face_size: face_size as u32,
            texels: SpecularProbeTexels::Rgba16Float(vec![0; texels * 4]),
        },
    }
}

/// A scene holding an instance, a light, a decal, a fog volume, mist and a
/// probe at positions `shift` less than their first ones.
fn content(device: &wgpu::Device, queue: &wgpu::Queue, shift: Vec3) -> Scene {
    let mut scene = Scene::new(device, queue);
    let model = scene
        .add_asset(device, queue, test_support::cube())
        .unwrap()
        .model;
    let rotation = Quat::from_rotation_y(0.7);
    scene
        .add_instance(
            device,
            queue,
            state(
                model,
                Mat4::from_rotation_translation(rotation, Vec3::new(4101.3, 2.7, -8189.9) - shift),
            ),
            Mobility::Static,
        )
        .unwrap();
    scene
        .add_light(
            device,
            queue,
            Light {
                position: Vec3::new(4095.6, 6.1, -8197.3) - shift,
                range: 12.,
                ..Default::default()
            },
        )
        .unwrap();
    let image = scene
        .add_decal_image(Image::Rgba8(image::RgbaImage::new(4, 4)))
        .unwrap();
    scene
        .add_decal(
            device,
            queue,
            Decal {
                position: Vec3::new(4099.9, 0.3, -8194.4) - shift,
                ..Decal::new(image)
            },
        )
        .unwrap();
    scene
        .update_fog_volumes(
            device,
            queue,
            &[FogVolume {
                center: Vec3::new(4093.7, 3.3, -8201.1) - shift,
                rotation,
                size: Vec3::new(4., 2., 6.),
                density: 0.1,
                albedo: [1.; 3],
                edge_fade: 0.1,
            }],
        )
        .unwrap();
    scene.update_mist(
        device,
        queue,
        &[(Vec3::new(4097.1, 0.2, -8195.7) - shift).to_array()],
    );
    let at = |position: Vec3| (position - shift).to_array();
    let glow = |position: Vec3, kind: GlowKind| Glow {
        position: at(position),
        color: [1., 0.5, 0.25, 1.],
        kind,
        soft_distance: 0.5,
    };
    let line = |other: Vec3, offset: f32| GlowKind::Line {
        other: at(other),
        offset,
    };
    scene.update_effects(
        device,
        queue,
        &[
            glow(Vec3::new(4094.3, 1.1, -8199.7), GlowKind::Uniform),
            glow(Vec3::new(4095.3, 2.1, -8198.7), GlowKind::Uniform),
            glow(Vec3::new(4094.8, 3.1, -8199.2), GlowKind::Uniform),
            glow(
                Vec3::new(4090.6, 1.5, -8196.1),
                line(Vec3::new(4093.9, 4.2, -8191.3), -0.5),
            ),
            glow(
                Vec3::new(4093.9, 4.2, -8191.3),
                line(Vec3::new(4090.6, 1.5, -8196.1), 0.5),
            ),
            glow(
                Vec3::new(4090.6, 1.5, -8196.1),
                line(Vec3::new(4093.9, 4.2, -8191.3), 0.5),
            ),
        ],
    );
    let heat = |position: Vec3| HeatDistortion {
        position: at(position),
        displacement: [2., -1.],
        weight: 0.5,
    };
    scene
        .update_heat_distortion(
            queue,
            &[
                heat(Vec3::new(4099.2, 0.5, -8201.4)),
                heat(Vec3::new(4100.2, 0.5, -8201.4)),
                heat(Vec3::new(4099.7, 1.5, -8201.4)),
            ],
        )
        .unwrap();
    scene
        .set_baked_specular_probes(
            device,
            queue,
            &[probe(Vec3::new(4098.3, 1.9, -8193.6) - shift)],
        )
        .unwrap();
    scene
}

// Plausible defects: a move that leaves a light, a decal, a fog volume, the
// mist, glow (a line's other endpoint too) or heat geometry, a probe, its
// world grid, an object record or the ray source where it was, translates
// one the wrong way, or forgets a record's GPU mirror. The
// oracle is the same content given by the game in the new frame: each
// position less the move. The two scenes' GPU records match word for word.
#[test]
fn a_move_holds_what_the_game_would_give_in_the_new_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut moved = content(&device, &queue, Vec3::ZERO);
    moved.move_origin(&device, &queue, MOVE).unwrap();
    let mut given = content(&device, &queue, MOVE);
    for scene in [&mut moved, &mut given] {
        scene.prepare_frame(&device, &queue, Vec3::ZERO);
        scene.update_rays(&device, &queue, !0, false);
    }
    let storage = |scene: &Scene| {
        [
            scene.instances.objects.buffer(),
            scene.lights.buffer(),
            scene.decals.buffer(),
            &scene.transient.fog_volumes,
            &scene.specular_probes().unwrap().metadata,
            scene.ray_instances.buffer(),
        ]
        .map(|buffer| test_support::storage_words(&device, &queue, buffer))
    };
    let labels = [
        "object records",
        "lights",
        "decals",
        "fog volumes",
        "probes and their grid",
        "ray entries",
    ];
    for ((label, moved), given) in labels.iter().zip(storage(&moved)).zip(storage(&given)) {
        assert_eq!(moved, given, "{label}");
    }
    let copied = |scene: &Scene| {
        [
            scene.rays.source(),
            &scene.transient.glow,
            &scene.transient.heat,
        ]
        .map(|buffer| test_support::read_words(&device, &queue, buffer))
    };
    let labels = ["the ray source and its instance BVHs", "glow", "heat"];
    for ((label, moved), given) in labels.iter().zip(copied(&moved)).zip(copied(&given)) {
        assert_eq!(moved, given, "{label}");
    }
    assert_eq!(
        moved.transient.mist_positions,
        given.transient.mist_positions
    );
    assert_eq!(
        moved.transient.fog_volume_corners,
        given.transient.fog_volume_corners
    );
}

/// The render size of the motion test's frames.
const SIZE: u32 = 32;

/// Renders `scene` seen by `camera` into `output`, and submits and finishes
/// the frame unless it is `abandoned`.
fn frame(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut Renderer,
    scene: &mut Scene,
    camera: Camera,
    output: &wgpu::TextureView,
    abandoned: bool,
) {
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        &FrameInput::new(camera),
        &Settings::default(),
        output,
        None,
    );
    if !abandoned {
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
    }
}

// Plausible defects: the renderer's camera history kept in the old render
// frame (a frame in which all static content moves by the move), translated
// the wrong way, translated again by a frame that was abandoned, or cut by
// the move. The oracle is a camera that stays where it was in the world:
// static content has no motion in the frame after the move.
#[test]
fn a_still_camera_sees_no_motion_across_a_move() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    for x in [-1.5, 0., 1.5] {
        let pose = Mat4::from_translation(Vec3::new(4096. + x, -64., -8195.));
        scene
            .add_instance(&device, &queue, state(model, pose), Mobility::Static)
            .unwrap();
    }
    let mut renderer = Renderer::for_test(&device, &queue, [SIZE; 2], &Settings::default());
    let output = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("origin move frames"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::shading::gbuffer::COLOR,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default());
    let camera = |origin: Vec3| {
        let eye = Vec3::new(4096., -62., -8190.) - origin;
        Camera {
            view: glam::camera::rh::view::look_at_mat4(
                eye,
                Vec3::new(4096., -64., -8195.) - origin,
                Vec3::Y,
            ),
            projection: crate::perspective(1., 1., 0.1),
            eye,
        }
    };
    let gpu = (&device, &queue);
    for _ in 0..3 {
        frame(
            gpu,
            &mut renderer,
            &mut scene,
            camera(Vec3::ZERO),
            &output,
            false,
        );
    }
    scene.move_origin(&device, &queue, MOVE).unwrap();
    frame(gpu, &mut renderer, &mut scene, camera(MOVE), &output, true);
    frame(gpu, &mut renderer, &mut scene, camera(MOVE), &output, false);
    let motion = test_support::read(&device, &queue, renderer.targets().motion.texture(), 4);
    let largest = motion
        .chunks_exact(2)
        .map(|half| test_support::half(half).abs())
        .fold(0f32, f32::max);
    assert!(
        largest < 1e-3,
        "static content moved {largest} after the origin move"
    );
    // The cubes are in view: without the camera history's translation the
    // frame would show them moving.
    let depth = test_support::read(&device, &queue, renderer.targets().depth.texture(), 4);
    assert!(
        depth
            .chunks_exact(4)
            .any(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()) > 0.),
        "the cubes were not drawn"
    );
}
