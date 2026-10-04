//! The opaque stage through the real stage: its two forms, and the motion
//! it records.
use crate::asset::{CpuMesh, Vertex};
use crate::diagnostics::DiagnosticTarget;
use crate::renderer::Renderer;
use crate::settings::{AmbientOcclusionQuality, Antialiasing, Settings};
use crate::shading::gbuffer;
use crate::test_support;
use crate::{Backdrop, Camera, FrameInput, InstanceState, Mobility, Scene, perspective};
use glam::camera;
use glam::{Mat4, UVec2, Vec2, Vec3};
use std::f32::consts::PI;

const SIZE: [u32; 2] = [64, 64];

/// A smooth-shaded UV sphere of radius 1: its normals and UVs vary across
/// every primitive edge.
fn sphere() -> crate::asset::Asset {
    let (rings, segments) = (12, 24);
    let mut vertices = Vec::new();
    for ring in 0..=rings {
        for segment in 0..=segments {
            let (u, v) = (segment as f32 / segments as f32, ring as f32 / rings as f32);
            let (theta, phi) = (u * 2. * PI, v * PI);
            let normal = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            vertices.push(Vertex {
                position: normal.to_array(),
                normal: normal.to_array(),
                uv: [u * 4., v * 4.],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [0.; 4],
            });
        }
    }
    let mut indices = Vec::new();
    for ring in 0..rings {
        for segment in 0..segments {
            let a = ring * (segments + 1) + segment;
            let b = a + segments + 1;
            indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices,
        indices,
        material: 0,
        deformation: Default::default(),
    }];
    // Smooth low roughness: the filtered roughness follows the normal's
    // screen-space derivatives.
    asset.materials[0].roughness = 0.05;
    asset
}

// AR-3: the fused pass writes the same targets as the split G-buffer and
// lighting passes, with ambient occlusion off and on. Plausible defects
// (#353): one form rasterizes another primitive stream, so derivative-filtered
// roughness, normals and texture LOD differ at primitive edges; a form encodes
// a target differently; a form shades with the occlusion, or runs it over
// other inputs (#393). The oracle is the other form's output for the same
// frame.
#[test]
fn fused_and_split_opaque_write_the_same_targets() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, sphere());
    let environment = scene
        .add_environment(
            &device,
            &queue,
            &test_support::environment([40, 60, 90, 255], &[0]),
        )
        .unwrap();
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &Settings::default());
    if !renderer.test_fused_supported() {
        let limits = device.limits();
        eprintln!(
            "skipping fused/split comparison: the device has no fused opaque pass \
             ({} colour attachments, {} attachment bytes per sample; it needs 8 and 64)",
            limits.max_color_attachments, limits.max_color_attachment_bytes_per_sample
        );
        return;
    }
    let eye = Vec3::new(0.4, 0.3, 2.6);
    let mut input = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: perspective(1., 1., 0.1),
        eye,
    });
    input.environment = Some(environment);
    input.ambient_occlusion_radius = 0.5;
    // Off after Medium: a frame without ambient occlusion reports none,
    // not the earlier frame's.
    for ambient_occlusion in [
        AmbientOcclusionQuality::Medium,
        AmbientOcclusionQuality::Off,
    ] {
        let settings = Settings {
            ambient_occlusion,
            ..Settings::default()
        };
        let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
        let mut targets = |fused| {
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
            queue.submit([encoder.finish()]);
            let visibility = renderer
                .diagnostic_target(DiagnosticTarget::AmbientOcclusion)
                .map(|view| test_support::read(&device, &queue, view.texture(), 4));
            assert_eq!(
                visibility.is_some(),
                ambient_occlusion != AmbientOcclusionQuality::Off,
                "ambient occlusion ran unless off"
            );
            let shared = renderer.targets();
            let targets = [
                ("depth", &shared.depth),
                ("normal", &shared.normal),
                ("material", &shared.material),
                ("f0", &shared.f0),
                ("anisotropy", &shared.anisotropy),
                ("motion", &shared.motion),
                ("color", &shared.color),
                ("ambient", &shared.ambient),
                ("source identity", &shared.source_id),
            ]
            .map(|(name, view)| {
                let texture = view.texture();
                let bpp = texture.format().block_copy_size(None).unwrap();
                (name, test_support::read(&device, &queue, texture, bpp))
            });
            (targets, visibility)
        };
        let (fused, fused_visibility) = targets(true);
        let (split, split_visibility) = targets(false);
        for ((name, fused), (_, split)) in fused.iter().zip(&split) {
            assert!(
                fused == split,
                "the fused and split forms wrote different {name} ({ambient_occlusion:?})"
            );
        }
        assert!(
            fused_visibility == split_visibility,
            "the fused and split forms' ambient occlusion differs"
        );
    }
}

/// A square facing +Z at depth `z`, `half` metres from its centre to each
/// side, its U along +X.
fn square(z: f32, half: f32) -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                position: [x * half, y * half, z],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [0.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

// Plausible defects: a masked material's cut-out texels written by the
// G-buffer, lighting or fused pass (no discard there, or one reading another
// texel's alpha), its opaque texels discarded, or the split form's lighting
// pass keeping what its G-buffer pass discarded. The oracle is geometric: a
// square whose base map is cut out over its left half stands in front of an
// opaque floor; the depth behind its cut-out half is the floor's and behind
// its opaque half its own, as the projection places them, and both forms
// write the same targets.
#[test]
fn masked_surfaces_are_cut_out_in_both_opaque_forms() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut floor = test_support::cube();
    floor.meshes = vec![square(-6., 8.)];
    test_support::add_static(&device, &queue, &mut scene, floor);
    let mut masked = test_support::cube();
    masked.meshes = vec![square(-3., 1.)];
    test_support::add_static(
        &device,
        &queue,
        &mut scene,
        test_support::masked(masked, 0.5),
    );
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &Settings::default());
    let projection = perspective(1., 1., 0.1);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection,
        eye: Vec3::ZERO,
    });
    let settings = Settings {
        ambient_occlusion: AmbientOcclusionQuality::Off,
        ..Settings::default()
    };
    let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
    let depth_at = |z: f32| projection.project_point3(Vec3::new(0., 0., z)).z;
    // Behind U = 0.26 (cut out) and U = 0.74 (opaque).
    let expected = [(22, depth_at(-6.)), (41, depth_at(-3.))];
    let forms = if renderer.test_fused_supported() {
        vec![true, false]
    } else {
        vec![false]
    };
    let mut written = Vec::new();
    for fused in forms {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
        queue.submit([encoder.finish()]);
        let shared = renderer.targets();
        let depth = test_support::read(&device, &queue, shared.depth.texture(), 4);
        for (x, expected) in expected {
            let at = ((SIZE[1] / 2 * SIZE[0] + x) * 4) as usize;
            let actual = f32::from_le_bytes(depth[at..at + 4].try_into().unwrap());
            assert!(
                (actual - expected).abs() < 1e-6,
                "fused {fused}: depth {actual} at x = {x}, expected {expected}"
            );
        }
        let targets = [
            &shared.depth,
            &shared.normal,
            &shared.material,
            &shared.motion,
            &shared.color,
            &shared.source_id,
        ]
        .map(|view| {
            let texture = view.texture();
            let bpp = texture.format().block_copy_size(None).unwrap();
            test_support::read(&device, &queue, texture, bpp)
        });
        written.push(targets);
    }
    if let [fused, split] = &written[..] {
        assert!(
            fused == split,
            "the fused and split forms cut out differently"
        );
    }
}

/// An odd size, so the middle pixel looks straight down the camera's axis.
const TURN_SIZE: [u32; 2] = [63, 63];

/// The motion target (x and y per texel) and the number of texels geometry
/// covers after two frames of a static square: the camera at the identity
/// pose, then turned by `turn`, facing the square.
fn motion_after_turn(device: &wgpu::Device, queue: &wgpu::Queue, turn: Mat4) -> (Vec<Vec2>, usize) {
    let mut scene = Scene::new(device, queue);
    let mut asset = test_support::cube();
    asset.meshes = vec![square(-5., 1.5)];
    let ids = scene.add_asset(device, queue, asset).unwrap();
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                model: ids.model,
                pose: turn,
                visible: true,
                capture_visible: true,
            },
            Mobility::Static,
        )
        .unwrap();
    // Without antialiasing's jitter, which would move the middle pixel off
    // the axis.
    let settings = Settings {
        antialiasing: Antialiasing::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, TURN_SIZE, &settings);
    let output = crate::view::targets::target(device, "turn", TURN_SIZE, gbuffer::COLOR);
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.2; 3]);
    for view in [Mat4::IDENTITY, turn.inverse()] {
        input.camera.view = view;
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
    let shared = renderer.targets();
    let motion = test_support::read(device, queue, shared.motion.texture(), 4)
        .chunks_exact(4)
        .map(|texel| {
            Vec2::new(
                test_support::half(&texel[0..2]),
                test_support::half(&texel[2..4]),
            )
        })
        .collect();
    let drawn = test_support::read(device, queue, shared.depth.texture(), 4)
        .chunks_exact(4)
        .filter(|texel| f32::from_le_bytes((*texel).try_into().unwrap()) > 0.)
        .count();
    (motion, drawn)
}

// Plausible defects (#4): a predecessor behind the last camera (clip w <= 0)
// left on screen (the sky's former zero motion, or an epsilon in place of w
// that leaves a point straight behind the camera near the middle), mirrored
// to the other side (an unguarded divide by a negative w) or made infinite
// or NaN (a divide by zero, or a half float overflowing) for the
// reflections, TAA, FSR2 and motion blur that read it. The oracle is
// geometric: the camera turns a quarter or a half turn between two frames,
// more than its field of view, so nothing it sees now was on screen before.
// The square it then faces straddles the last camera's plane after the
// quarter turn, so geometry and sky predecessors lie both beside and behind
// that camera, and the half turn puts the middle pixel's predecessor straight
// behind it. A left turn moves everything right on screen.
#[test]
fn a_turn_past_the_view_moves_every_pixel_off_screen_along_the_turn() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    for (turn, left) in [(PI / 2., true), (PI, false)] {
        let (motion, drawn) = motion_after_turn(&device, &queue, Mat4::from_rotation_y(turn));
        let texels = motion.len();
        assert!(
            drawn > 0 && drawn < texels,
            "the turned view shows both the square and the sky ({drawn} of {texels} texels drawn)"
        );
        for (index, &m) in motion.iter().enumerate() {
            let pixel = UVec2::new(index as u32 % TURN_SIZE[0], index as u32 / TURN_SIZE[0]);
            let uv = (pixel.as_vec2() + 0.5) / UVec2::from(TURN_SIZE).as_vec2();
            let previous = uv - m;
            assert!(
                m.is_finite(),
                "turn {turn}: motion {m} at {pixel} is not finite"
            );
            assert!(
                previous.cmplt(Vec2::ZERO).any() || previous.cmpgt(Vec2::ONE).any(),
                "turn {turn}: motion {m} at {pixel} puts the previous position {previous} on screen"
            );
            assert!(
                !left || m.x > 0.,
                "turn {turn}: motion {m} at {pixel} is not along the left turn"
            );
        }
    }
}
