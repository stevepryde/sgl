//! Blended receivers of screen-space reflections: the receiver pass and the
//! blended draw that composes the method's result.
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::test_support::{self, half, hdr_texture, read};
use crate::{AlphaMode, Camera, FrameInput, InstanceState, Mobility, Scene};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];
/// The receivers' alpha and the environment's radiance.
const ALPHA: f32 = 0.5;
const ENVIRONMENT: f32 = 1.;
/// The radiance the method's result holds where confident.
const REFLECTED: f32 = 4.;

/// A square facing +Z at depth `z`, `half` metres from its centre to each
/// side, drawn with `material`.
fn square(z: f32, half: f32, material: crate::asset::Material) -> crate::asset::Asset {
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
    asset.materials = vec![material];
    asset
}

/// The composite's middle row after the receiver pass and the blended draw
/// over a black floor, composing `reflections` (a texel for every pixel,
/// rgb premultiplied by a) where `Some`: a smooth white metal receiver at 3
/// m covering columns 10 to 21 in front of another at 4 m covering columns
/// 13 to 30, each of alpha `ALPHA`, in a uniform environment.
fn middle_row(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reflections: Option<[f32; 4]>,
) -> Vec<[f32; 4]> {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Full,
        reflection_method: settings::ReflectionMethod::Velvet,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    let black = crate::asset::Material {
        base: [0., 0., 0., 1.],
        unlit: true,
        ..Default::default()
    };
    test_support::add_static(device, queue, &mut scene, square(-6., 8., black));
    let metal = crate::asset::Material {
        base: [1., 1., 1., ALPHA],
        metallic: 1.,
        roughness: 0.05,
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: true,
        },
        ..Default::default()
    };
    // At cot(0.5) = 1.83, x from -0.6 to 0.6 m at 3 m projects to NDC
    // -0.37 to 0.37, columns 10 to 21; x from -0.4 to 2 m at 4 m to NDC
    // -0.18 to 0.92, columns 13 to 30.
    for (z, half, x) in [(-3., 0.6, 0.), (-4., 1.2, 0.8)] {
        let model = scene
            .add_asset(device, queue, square(z, half, metal.clone()))
            .unwrap()
            .model;
        let state = InstanceState {
            model,
            pose: Mat4::from_translation(Vec3::X * x),
            visible: true,
            capture_visible: true,
        };
        scene
            .add_instance(device, queue, state, Mobility::Static)
            .unwrap();
    }
    let radiance = test_support::to_half(ENVIRONMENT).to_le_bytes();
    let environment = scene
        .add_environment(
            device,
            queue,
            &test_support::environment([255; 4], &radiance.repeat(4)),
        )
        .unwrap();
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.environment = Some(environment);
    let reflections = reflections.map(|texel| {
        hdr_texture(
            device,
            queue,
            SIZE,
            &vec![texel; (SIZE[0] * SIZE[1]) as usize],
        )
    });
    let mut frame = renderer.prepare_test_frame(device, queue, &mut scene, &input, &settings);
    let mut encoder = device.create_command_encoder(&Default::default());
    let fused = renderer.test_fused_supported();
    renderer.encode_test_opaque(device, queue, &mut encoder, &scene, &mut frame, fused);
    renderer.encode_test_receivers(
        device,
        queue,
        &mut encoder,
        &scene,
        &frame,
        reflections.as_ref(),
    );
    queue.submit([encoder.finish()]);
    let composite = read(device, queue, renderer.targets().composite.texture(), 8);
    let row = (SIZE[1] / 2 * SIZE[0]) as usize * 8;
    composite[row..row + SIZE[0] as usize * 8]
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|channel| half(&texel[channel * 2..])))
        .collect()
}

// Plausible defects: the receiver pass draws its receivers where the blended
// draw does not rasterize them (another vertex path, view, primitive or depth
// state), so no receiver finds itself the surface and the method's result
// never composes; the blended draw adds the result to the receiver's
// environment specular rather than in its place, or composes it into a
// receiver behind the nearest; a result of no confidence changes anything.
// The oracle is coverage blending of a smooth white metal, which reflects
// its surroundings once: alone over black, a receiver of alpha a returns a
// times its specular response times the environment's radiance with no
// result, and in place of that radiance the method's where confident; where
// it covers the farther receiver, which keeps its environment, that one
// shows through by 1 - a.
#[test]
fn the_nearest_receiver_composes_the_reflection_in_place_of_its_environment() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let environment = middle_row(&device, &queue, None);
    let unconfident = middle_row(&device, &queue, Some([0.; 4]));
    let reflected = middle_row(&device, &queue, Some([REFLECTED, REFLECTED, REFLECTED, 1.]));
    // Near alone, both, far alone.
    let [near, both, far] = [11, 17, 26];
    for channel in 0..3 {
        let response = environment[near][channel] / (ALPHA * ENVIRONMENT);
        assert!(
            (0.9..=1.).contains(&response),
            "a smooth white metal's response {response}"
        );
        let expected = [
            (near, ALPHA * ENVIRONMENT, ALPHA * REFLECTED),
            (
                both,
                ALPHA * ENVIRONMENT + (1. - ALPHA) * ALPHA * ENVIRONMENT,
                ALPHA * REFLECTED + (1. - ALPHA) * ALPHA * ENVIRONMENT,
            ),
            (far, ALPHA * ENVIRONMENT, ALPHA * REFLECTED),
        ];
        for (column, without, with) in expected {
            let close = |actual: f32, expected: f32| (actual - expected).abs() <= 0.02 * expected;
            for (name, actual, expected) in [
                (
                    "no result",
                    environment[column][channel],
                    without * response,
                ),
                (
                    "no confidence",
                    unconfident[column][channel],
                    without * response,
                ),
                (
                    "a confident result",
                    reflected[column][channel],
                    with * response,
                ),
            ] {
                assert!(
                    close(actual, expected),
                    "column {column} with {name}: {actual}, expected {expected}"
                );
            }
        }
    }
}

/// A quad of `material` with corners `origin`, `origin + u`, `origin + u +
/// v` and `origin + v`, facing `u × v`.
fn quad(origin: Vec3, u: Vec3, v: Vec3, material: crate::asset::Material) -> crate::asset::Asset {
    let normal = u.cross(v).normalize().to_array();
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(0., 0.), (1., 0.), (1., 1.), (0., 1.)]
            .map(|(s, t)| crate::asset::Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (origin + u * s + v * t).to_array(),
                normal,
                uv: [s, 1. - t],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset.materials = vec![material];
    asset
}

/// The composite's column through the centre of a frame of a smooth white
/// metal sheet 1 m below the camera, over a rough floor 2 m below it, in
/// front of a bright wall 10 m ahead, with `method` tracing: the sheet a
/// receiver when `receives`, else blended like any glass.
fn reflection_column(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    method: settings::ReflectionMethod,
    receives: bool,
) -> Vec<f32> {
    const SIZE: [u32; 2] = [64, 64];
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Full,
        reflection_method: method,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    let floor = crate::asset::Material {
        base: [0.2, 0.2, 0.2, 1.],
        roughness: 0.9,
        ..Default::default()
    };
    let wall = crate::asset::Material {
        base: [20., 20., 20., 1.],
        unlit: true,
        ..Default::default()
    };
    let sheet = crate::asset::Material {
        base: [1., 1., 1., ALPHA],
        metallic: 1.,
        roughness: 0.05,
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: receives,
        },
        ..Default::default()
    };
    for asset in [
        quad(
            Vec3::new(-20., -2., 0.),
            Vec3::X * 40.,
            Vec3::NEG_Z * 30.,
            floor,
        ),
        quad(Vec3::new(-6., -2., -10.), Vec3::X * 12., Vec3::Y * 5., wall),
        quad(
            Vec3::new(-20., -1., -0.5),
            Vec3::X * 40.,
            Vec3::NEG_Z * 9.5,
            sheet,
        ),
    ] {
        test_support::add_static(device, queue, &mut scene, asset);
    }
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let output = crate::view::targets::target(
        device,
        "receiver frame",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    // The methods accumulate their hits over frames.
    for _ in 0..3 {
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
    let composite = read(device, queue, renderer.targets().composite.texture(), 8);
    (0..SIZE[1] as usize)
        .map(|row| half(&composite[(row * SIZE[0] as usize + SIZE[0] as usize / 2) * 8..]))
        .collect()
}

// Plausible defects: the screen-space method traces the opaque surface's
// G-buffer under a receiver rather than the receiver's lobe (the floor
// beneath, too rough to trace, leaves it untraced), or the receiver's
// pixels never compose what it found, with either method. The oracle is the mirror image of the
// wall, reasoned from the geometry: the sheet 1 m below the camera reflects
// the wall 10 m ahead between NDC y -0.18 (the wall's foot at the water
// line) and -0.92 (its top, 4 m up), rows 38 to 61 of 64 at cot(0.5) = 1.83.
// Row 46 sees the sheet 4 m ahead reflect the wall 1.5 m above the water: a
// receiver shows a sizeable share of the wall's radiance 20 there, and the
// same sheet unmarked, with no environment to reflect, almost none.
#[test]
fn a_receiver_over_a_rough_floor_reflects_what_lies_in_front_of_it() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    for method in [
        settings::ReflectionMethod::Crystal,
        settings::ReflectionMethod::Velvet,
    ] {
        let receiver = reflection_column(&device, &queue, method, true);
        let glass = reflection_column(&device, &queue, method, false);
        let row = 46;
        assert!(
            glass[row] < 0.05,
            "{method:?}: an unmarked sheet reflected {} with no environment",
            glass[row]
        );
        assert!(
            receiver[row] > 0.1 * ALPHA * 20.,
            "{method:?}: the receiver reflected {} of the wall's 20",
            receiver[row]
        );
    }
}

// Plausible defects: the receiver pass leaves the motion of what lies behind
// a receiver (TAA, FSR2 and motion blur would then reproject the receiver by
// its background and trail it), writes jittered motion, or draws into the
// opaque depth, which completion, probe culling and the transparent stage's
// depth tests keep reading. The oracle is the geometry: a receiver moving 0.6
// m across at 3 m in front of a floor at 6 m moves 0.6 / 3 × 1.83 / 2 = 0.183
// of the screen's width at its pixels; the surface depth there is the
// receiver's, near / 3 m, and the opaque depth stays the floor's, near / 6
// m, under `perspective`'s infinite reversed Z.
#[test]
fn a_receiver_writes_its_motion_and_the_surface_depth_not_the_opaque_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = Settings {
        antialiasing: settings::Antialiasing::Taa,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(
        &device,
        &queue,
        &mut scene,
        square(-6., 8., crate::asset::Material::default()),
    );
    let water = crate::asset::Material {
        base: [0.1, 0.3, 0.4, ALPHA],
        roughness: 0.05,
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: true,
        },
        ..Default::default()
    };
    let model = scene
        .add_asset(&device, &queue, square(-3., 0.5, water))
        .unwrap()
        .model;
    let mut state = InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible: true,
        capture_visible: true,
    };
    let instance = scene
        .add_instance(&device, &queue, state, Mobility::Moving)
        .unwrap();
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let output = crate::view::targets::target(
        &device,
        "receiver frame",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    for x in [-0.3, 0.3] {
        state.pose = Mat4::from_translation(Vec3::X * x);
        scene.set_instance(&queue, instance, state).unwrap();
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
    }
    let targets = renderer.targets();
    let surface = targets
        .surface
        .as_ref()
        .expect("the scene holds a receiver");
    let read_f32 = |view: &wgpu::TextureView| -> Vec<f32> {
        bytemuck::cast_slice(&read(&device, &queue, view.texture(), 4)).to_vec()
    };
    let opaque_depth = read_f32(&targets.depth);
    let surface_depth = read_f32(&surface.depth);
    let motion = read(&device, &queue, targets.motion.texture(), 4);
    // The receiver's centre, at x = 0.3 m, projects to NDC 0.18, column 18;
    // column 4 sees the floor alone.
    for (column, depth, motion_x) in [(18, 0.1 / 3., 0.6 / 3. * 1.83 / 2.), (4, 0.1 / 6., 0.)] {
        let at = (SIZE[1] / 2 * SIZE[0] + column) as usize;
        assert!(
            (opaque_depth[at] - 0.1 / 6.).abs() < 1e-6,
            "column {column}: the opaque depth {} is not the floor's",
            opaque_depth[at]
        );
        assert!(
            (surface_depth[at] - depth).abs() < 1e-6,
            "column {column}: the surface depth {}, expected {depth}",
            surface_depth[at]
        );
        let [x, y] = [0, 1].map(|axis| half(&motion[at * 4 + axis * 2..]));
        assert!(
            (x - motion_x).abs() < 0.002 && y.abs() < 0.002,
            "column {column}: motion ({x}, {y}), expected ({motion_x}, 0)"
        );
    }
}
