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
/// covers after two frames of a static square through `projection`: the
/// camera at the identity pose, then turned by `turn`, facing the square.
fn motion_after_turn(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    projection: Mat4,
    turn: Mat4,
) -> (Vec<Vec2>, usize) {
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
        projection,
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
// to the other side (an unguarded divide by a negative w), given one
// direction whatever the turn, made infinite or NaN (a divide by zero), or
// kept too close to the screen: the reflections' 3x3 vicinity search moves
// the reprojected position up to three traced texels along each axis
// (world-space reflections trace at half resolution), so a predecessor that
// near the edge takes history back from the screen. The oracle is geometric:
// the camera turns a quarter turn either way or a half turn between two
// frames, more than its field of view, so nothing it sees now was on screen
// before. The square it then faces straddles the last camera's plane after a
// quarter turn, so geometry and sky predecessors lie both beside and behind
// that camera, and the half turn puts the middle pixel's predecessor
// straight behind it. A left turn moves everything right on screen, a right
// turn left.
#[test]
fn a_turn_past_the_view_moves_every_pixel_off_screen_along_the_turn() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = UVec2::from(TURN_SIZE);
    let search = 3. / (size / 2).as_vec2();
    // The sign of every motion's x: right after a left turn, left after a
    // right one.
    for (turn, along) in [(PI / 2., Some(1.)), (-PI / 2., Some(-1.)), (PI, None)] {
        let (motion, drawn) = motion_after_turn(
            &device,
            &queue,
            perspective(1., 1., 0.1),
            Mat4::from_rotation_y(turn),
        );
        let texels = motion.len();
        assert!(
            drawn > 0 && drawn < texels,
            "the turned view shows both the square and the sky ({drawn} of {texels} texels drawn)"
        );
        for (index, &m) in motion.iter().enumerate() {
            let pixel = UVec2::new(index as u32 % size.x, index as u32 / size.x);
            let uv = (pixel.as_vec2() + 0.5) / size.as_vec2();
            let previous = uv - m;
            assert!(
                m.is_finite(),
                "turn {turn}: motion {m} at {pixel} is not finite"
            );
            assert!(
                previous.cmplt(-search).any() || previous.cmpgt(1. + search).any(),
                "turn {turn}: motion {m} at {pixel} puts the previous position {previous} \
                 within the reflections' search of the screen"
            );
            assert!(
                along.is_none_or(|sign| m.x * sign > 0.),
                "turn {turn}: motion {m} at {pixel} is not along the turn"
            );
        }
    }
}

// Plausible defect: the sky's motion divides by a w its direction does not
// have. An orthographic camera gives every direction w = 0, so the sky would
// write infinite or NaN motion to the motion target. The oracle is the
// requirement that motion is finite; the camera turns between the two
// frames and the square keeps geometry in view beside the sky.
#[test]
fn an_orthographic_camera_records_finite_motion() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let projection = camera::rh::proj::directx::orthographic(-3., 3., -3., 3., 10., 0.1);
    let (motion, drawn) =
        motion_after_turn(&device, &queue, projection, Mat4::from_rotation_y(0.2));
    let texels = motion.len();
    assert!(
        drawn > 0 && drawn < texels,
        "the view shows both the square and the sky ({drawn} of {texels} texels drawn)"
    );
    for (index, &m) in motion.iter().enumerate() {
        let pixel = UVec2::new(index as u32 % TURN_SIZE[0], index as u32 / TURN_SIZE[0]);
        assert!(m.is_finite(), "motion {m} at {pixel} is not finite");
    }
}

/// Fresnel's equations: the reflectance of unpolarised light arriving at
/// cosine `cos_i` through a medium of index `n1` onto one of index `n2`, the
/// mean of its s- and p-polarised reflectances.
fn fresnel(n1: f64, n2: f64, cos_i: f64) -> f64 {
    let sin_t = n1 / n2 * (1. - cos_i * cos_i).sqrt();
    let cos_t = (1. - sin_t * sin_t).sqrt();
    let s = (n1 * cos_i - n2 * cos_t) / (n1 * cos_i + n2 * cos_t);
    let p = (n1 * cos_t - n2 * cos_i) / (n1 * cos_t + n2 * cos_i);
    (s * s + p * p) / 2.
}

// Defects: the G-buffer's F0 ignores a dielectric's IOR (0.04 whatever it
// is), its specular colour or its specular strength, clamps after the
// strength rather than before it, or an edit of them does not reach the
// record the G-buffer pass reads. The oracle is Fresnel's equations at
// normal incidence from air for the IOR, tinted, clamped and scaled as
// KHR_materials_specular defines: min(F0 * colour, 1) * specular.
#[test]
fn gbuffer_f0_is_fresnel_at_the_materials_ior() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut asset = test_support::cube();
    asset.meshes = vec![square(-3., 2.)];
    asset.materials[0].metallic = 0.;
    asset.materials[0].ior = 1.33;
    let (ids, _) = test_support::add_static(&device, &queue, &mut scene, asset);
    let material = ids.materials[0];
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let mut check = |scene: &mut Scene, expected: [f64; 3]| {
        let mut frame = renderer.prepare_test_frame(&device, &queue, scene, &input, &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_opaque(&device, &queue, &mut encoder, scene, &mut frame, false);
        queue.submit([encoder.finish()]);
        let f0 = test_support::read(&device, &queue, renderer.targets().f0.texture(), 4);
        let at = ((SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) * 4) as usize;
        for (channel, expected) in expected.into_iter().enumerate() {
            let recorded = f64::from(f0[at + channel]);
            assert!(
                (recorded - expected * 255.).abs() <= 0.6,
                "channel {channel}: F0 {recorded}/255, expected {expected}"
            );
        }
    };
    // Water: 0.020, half the default's 0.04.
    check(&mut scene, [fresnel(1., 1.33, 1.); 3]);
    let mut values = scene.material(material).unwrap();
    values.ior = 2.42;
    values.specular_color = [30., 0.5, 0.];
    values.specular = 0.6;
    scene.set_material(&queue, material, values).unwrap();
    let diamond = fresnel(1., 2.42, 1.);
    check(
        &mut scene,
        [30., 0.5, 0.].map(|tint| (diamond * tint).min(1.) * 0.6),
    );
}

// Defects: a lobe still reflects toward a grazing reflectance (F90) of 1
// where F0 is 0: direct light's Schlick Fresnel, the split-sum environment
// response or its multiple scattering, or a rectangle light's Fresnel
// weight. The oracle is KHR_materials_specular: at `specular` 0 a
// dielectric's F0 and F90 are both 0, so it reflects as Lambert alone, and a
// black one reflects nothing. A cube seen with two faces near grazing, lit
// from behind by the sun, a rectangle light and a uniform environment, must
// leave the frame black, opaque (its environment specular from source
// completion) and blended (from lit shading); at `specular` 1 it is lit.
#[test]
fn a_dielectric_without_specular_reflects_as_lambert_alone() {
    use crate::{DirectionalLight, EnvironmentLight, Light, LightShape};
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut asset = test_support::cube();
    asset.materials[0].base = [0., 0., 0., 1.];
    asset.materials[0].metallic = 0.;
    asset.materials[0].roughness = 0.3;
    asset.materials[0].specular = 0.;
    let (ids, _) = test_support::add_static(&device, &queue, &mut scene, asset);
    let environment = scene
        .add_environment(
            &device,
            &queue,
            &test_support::environment([255; 4], &0x3c00u16.to_le_bytes()),
        )
        .unwrap();
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: Vec3::new(0., 0.8, -1.5),
                shape: LightShape::Rect {
                    direction: Vec3::new(0., -0.3, 1.),
                    width_axis: Vec3::X,
                    width: 1.5,
                    height: 0.5,
                },
                color: [1.; 3],
                intensity: 20.,
                range: 10.,
                ..Default::default()
            },
        )
        .unwrap();
    let settings = Settings {
        antialiasing: Antialiasing::Off,
        ambient_occlusion: AmbientOcclusionQuality::Off,
        screen_space_reflections: crate::settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = crate::view::targets::target(&device, "lambert", SIZE, gbuffer::COLOR);
    // The +Y and +X faces lie near grazing from here.
    let eye = Vec3::new(0.25, 0.65, 3.);
    let mut input = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: perspective(0.8, 1., 0.1),
        eye,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.environment = Some(environment);
    input.reflection_environment = EnvironmentLight {
        yaw: 0.,
        intensity: 1.,
    };
    // From behind the cube, toward the camera, over its top.
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.3, -0.4, 1.),
        color: [1.; 3],
        illuminance: 3.,
        shadow: None,
        ..Default::default()
    });
    let mut composite = |scene: &mut Scene| -> Vec<f32> {
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
        test_support::read(&device, &queue, renderer.targets().composite.texture(), 8)
            .chunks_exact(8)
            .flat_map(|texel| (0..3).map(|c| test_support::half(&texel[c * 2..])))
            .collect()
    };
    for alpha in [
        crate::AlphaMode::Opaque,
        crate::AlphaMode::Blend {
            receives_screen_space_reflections: false,
        },
    ] {
        let mut values = scene.material(ids.materials[0]).unwrap();
        values.alpha = alpha;
        scene
            .set_material(&queue, ids.materials[0], values)
            .unwrap();
        let none = composite(&mut scene);
        let brightest = none.iter().copied().fold(0., f32::max);
        assert!(
            brightest == 0.,
            "a black dielectric without specular reflected {brightest} ({alpha:?})"
        );
    }
    let mut values = scene.material(ids.materials[0]).unwrap();
    values.specular = 1.;
    scene
        .set_material(&queue, ids.materials[0], values)
        .unwrap();
    let lit = composite(&mut scene);
    assert!(
        lit.iter().filter(|&&value| value > 0.01).count() > 300,
        "the fixture must reflect light at specular 1"
    );
}

// Defects: the G-buffer's grazing reflectance (F90) is derived from F0
// rather than taken from the specular strength (a partial strength reads
// near 1, a low-IOR dielectric below 1), ignores metallic, or mixes the
// wrong way. The oracle is KHR_materials_specular: a dielectric's F90 is its
// specular strength, whatever its IOR, mixed toward 1 by metallic.
#[test]
fn gbuffer_f90_is_the_specular_strength_mixed_toward_one_by_metallic() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut asset = test_support::cube();
    asset.meshes = vec![square(-3., 2.)];
    let (ids, _) = test_support::add_static(&device, &queue, &mut scene, asset);
    let material = ids.materials[0];
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    // (IOR, specular, metallic, KHR's F90): partial strengths, a metal, a
    // half metal and ice, whose F0 (0.018) is below water's.
    for (ior, specular, metallic, expected) in [
        (1.5, 0.25, 0., 0.25),
        (1.5, 0.5, 0., 0.5),
        (1.5, 0.25, 1., 1.),
        (1.5, 0.5, 0.5, 0.75),
        (1.31, 1., 0., 1.),
    ] {
        let mut values = scene.material(material).unwrap();
        values.ior = ior;
        values.specular = specular;
        values.metallic = metallic;
        scene.set_material(&queue, material, values).unwrap();
        let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, false);
        queue.submit([encoder.finish()]);
        let recorded =
            test_support::read(&device, &queue, renderer.targets().material.texture(), 8);
        let at = ((SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) * 8) as usize;
        let f90 = test_support::half(&recorded[at + 6..]);
        assert!(
            (f90 - expected).abs() < 1e-3,
            "IOR {ior}, specular {specular}, metallic {metallic}: F90 {f90}, expected {expected}"
        );
    }
}

// Defects: the environment scale, which the anisotropy target records
// beside the anisotropy, is lost on its way to source completion: read from
// another channel (the material target's F90, 1 for a metal), or left
// unwritten by the anisotropy pass of a device that cannot write that
// target with the G-buffer's others. The oracle is
// `SurfaceMaterial::environment_scale`, a multiplier of the environment's
// specular light: a smooth white metal lit by a uniform environment alone
// completes at half scale to half its radiance at scale 1, on the smallest
// attachment budget, whose anisotropy pass writes the scale, and on the
// device's own.
#[test]
fn the_environment_scale_reaches_completion_on_every_attachment_budget() {
    use crate::{EnvironmentLight, graphics_device};
    let Some(adapter) = test_support::adapter() else {
        return;
    };
    let largest = adapter.limits().max_color_attachment_bytes_per_sample;
    for budget in [32, largest] {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: graphics_device::features(&adapter),
            required_limits: wgpu::Limits {
                max_color_attachment_bytes_per_sample: budget,
                ..graphics_device::limits(&adapter)
            },
            ..Default::default()
        }))
        .unwrap();
        let mut scene = Scene::new(&device, &queue);
        let mut asset = test_support::cube();
        asset.materials[0].base = [1.; 4];
        asset.materials[0].metallic = 1.;
        asset.materials[0].roughness = 0.2;
        let (ids, _) = test_support::add_static(&device, &queue, &mut scene, asset);
        let environment = scene
            .add_environment(
                &device,
                &queue,
                &test_support::environment([255; 4], &0x3c00u16.to_le_bytes()),
            )
            .unwrap();
        let settings = Settings {
            antialiasing: Antialiasing::Off,
            ambient_occlusion: AmbientOcclusionQuality::Off,
            screen_space_reflections: crate::settings::ScreenSpaceReflections::Off,
            atmosphere: false,
            ..Settings::default()
        };
        let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        assert_eq!(
            renderer.test_anisotropy_inline(),
            budget == largest && largest > 32,
            "budget {budget} must take its own G-buffer form"
        );
        let output = crate::view::targets::target(&device, "scale", SIZE, gbuffer::COLOR);
        let eye = Vec3::new(1.6, 1.2, 2.4);
        let mut input = FrameInput::new(Camera {
            view: camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
            projection: perspective(0.9, 1., 0.1),
            eye,
        });
        input.backdrop = Backdrop::Color([0.; 3]);
        input.environment = Some(environment);
        input.diffuse_environment.intensity = 0.;
        input.reflection_environment = EnvironmentLight {
            yaw: 0.,
            intensity: 1.,
        };
        let mut composite = |scale: f32| -> Vec<f32> {
            let mut values = scene.material(ids.materials[0]).unwrap();
            values.environment_scale = scale;
            scene
                .set_material(&queue, ids.materials[0], values)
                .unwrap();
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
            test_support::read(&device, &queue, renderer.targets().composite.texture(), 8)
                .chunks_exact(8)
                .flat_map(|texel| (0..3).map(|c| test_support::half(&texel[c * 2..])))
                .collect()
        };
        let whole = composite(1.);
        assert!(
            whole.iter().filter(|&&value| value > 0.05).count() > 300,
            "budget {budget}: the metal must reflect its environment"
        );
        let half = composite(0.5);
        for (channel, (&whole, &half)) in whole.iter().zip(&half).enumerate() {
            assert!(
                (half - whole / 2.).abs() <= whole / 512. + 1e-5,
                "budget {budget}, channel {channel}: {half} at half scale for {whole}"
            );
        }
    }
}
