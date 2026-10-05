//! FSR2's transparency and composition mask over opaque surfaces whose
//! shading moves where their geometry stands still, observed in real
//! frames.
use crate::asset::{Asset, Image};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::{
    AlphaMode, Camera, FrameInput, InstanceState, Mobility, NormalLayer, Scene, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// Two layers that move: 180 and 108 whole repeats an hour.
const MOVING: [NormalLayer; 2] = [
    NormalLayer {
        velocity: [0.05, 0.],
        scale: 1.,
        strength: 1.,
    },
    NormalLayer {
        velocity: [0., 0.03],
        scale: 1.,
        strength: 1.,
    },
];

/// Two layers that stand still.
const STILL: [NormalLayer; 2] = [NormalLayer {
    velocity: [0.; 2],
    scale: 1.,
    strength: 1.,
}; 2];

/// A square facing +Z at depth `z`, `half` metres from its centre to each
/// side, its U rising along +X, with the fixture material over a flat
/// normal map that repeats.
fn square(z: f32, half: f32) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| crate::asset::Vertex {
                tangent: [1., 0., 0., 1.],
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
    let flat = image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 128, 255, 255]));
    asset.images.push(Image::Rgba8(flat));
    asset.materials[0].normal_texture = Some(asset.images.len() - 1);
    asset
}

/// `asset` placed whole at `offset` as a static instance.
fn place(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    asset: Asset,
    offset: Vec3,
) {
    let model = scene.add_asset(device, queue, asset).unwrap().model;
    let state = InstanceState {
        model,
        pose: Mat4::from_translation(offset),
        visible: true,
        capture_visible: true,
    };
    scene
        .add_instance(device, queue, state, Mobility::Static)
        .unwrap();
}

/// A renderer running FSR2 at the scene size, or `None` where it cannot.
fn fsr2_renderer(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<(Renderer, Settings)> {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Fsr2,
        fsr2_quality: settings::Fsr2Quality::NativeAa,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let renderer = Renderer::for_test(device, queue, SIZE, &settings);
    if renderer.antialiasing_in_effect(&settings) != settings::Antialiasing::Fsr2 {
        eprintln!(
            "skipping: FSR2 does not run here: {:?}",
            renderer.fsr2_error()
        );
        return None;
    }
    Some((renderer, settings))
}

/// One frame of `scene` from the origin looking down -Z, and FSR2's
/// reactive and transparency and composition masks after it, each texel in
/// 0..=1.
fn masks(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (renderer, settings): (&mut Renderer, &Settings),
    scene: &mut Scene,
    elapsed_seconds: f64,
) -> [Vec<f32>; 2] {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.camera_cut = true;
    input.elapsed_seconds = elapsed_seconds;
    let output = crate::view::targets::target(device, "FSR2 masks frame", SIZE, gbuffer::COLOR);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        &input,
        settings,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(scene);
    renderer.targets().fsr2_masks.each_ref().map(|mask| {
        test_support::read(device, queue, mask.texture(), 1)
            .into_iter()
            .map(|byte| f32::from(byte) / 255.)
            .collect()
    })
}

/// Asserts that `masks` hold `expected` (reactive, transparency and
/// composition) at each texel (column, row), within an 8-bit step.
fn assert_masks(masks: &[Vec<f32>; 2], expected: &[((u32, u32), [f32; 2], &str)]) {
    for &((column, row), expected, what) in expected {
        let at = (row * SIZE[0] + column) as usize;
        let actual = [masks[0][at], masks[1][at]];
        for (name, actual, expected) in [
            ("reactive", actual[0], expected[0]),
            ("transparency and composition", actual[1], expected[1]),
        ] {
            assert!(
                (actual - expected).abs() <= 1. / 255.,
                "{what} at ({column}, {row}): {name} mask {actual}, expected {expected}"
            );
        }
    }
}

// Plausible defects: opaque or masked surfaces whose normal layers move
// leave FSR2's transparency and composition mask unmarked, or write the
// reactive mask; surfaces whose shading does not move (layers standing
// still, an unlit material, a masked material's cut-out texels, the floor)
// are marked, or a moving surface where an opaque one hides it; or the
// blended surfaces drawn after clear what the opaque ones marked. The
// oracle is AMD's FSR documentation, which names animated textures for this
// mask, and its FSR sample, whose animated textures write 1 to it and leave
// the reactive mask, before its translucency writes alpha to both with
// `m (1 - mask) + mask` (AnimatedTexture.hlsl; fsrapirendermodule.cpp):
// a moving surface reads (0, 1), behind a blended square of alpha 0.4
// (0.4, 1), and the floor beside it (0.4, 0.4).
//
// The camera at the origin looks down -Z with cot(0.5) = 1.830, so at 3 m
// a metre spans 0.610 of NDC, 19.5 of the 64 texels: squares 0.8 m wide
// centred 1.2 m apart across and 1.2 m apart up span 15.6 texels about
// columns 8.6, 32 and 55.4 and rows 20.3 and 43.7. At 2.5 m a metre spans
// 23.4 texels: the occluder, 0.4 m wide at (0.854, -0.512), spans columns
// 47.3 to 56.7 and rows 39.3 to 48.7. At 2 m a metre spans 29.3 texels:
// the blended square, 0.4 m wide at (0.273, -0.4), spans columns 34.1 to
// 45.9 and rows 37.8 to 49.6. Sampled texels lie at least 2.5 texels from
// every edge, past FSR2's half-texel jitter.
#[test]
fn moving_opaque_surfaces_mark_fsr2s_composition_mask() {
    let Some((device, queue)) = test_support::fsr2_device() else {
        return;
    };
    let Some((mut renderer, settings)) = fsr2_renderer(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, square(-6., 8.));
    let layered = |layers: [NormalLayer; 2], unlit: bool| {
        let mut asset = square(-3., 0.4);
        asset.materials[0].normal_layers = Some(layers);
        asset.materials[0].unlit = unlit;
        asset
    };
    let top = 0.6;
    let bottom = -0.6;
    for (asset, x, y) in [
        (layered(MOVING, false), -1.2, top),
        (layered(STILL, false), 0., top),
        (layered(MOVING, true), 1.2, top),
        (layered(MOVING, false), 0., bottom),
        (layered(MOVING, false), 1.2, bottom),
        (square(-2.5, 0.2), 0.854, -0.512),
    ] {
        place(&device, &queue, &mut scene, asset, Vec3::new(x, y, 0.));
    }
    // Masked over a base map cut out over its left half, u < 0.5.
    let mut cut = layered(MOVING, false);
    cut.images.push(Image::Rgba8(test_support::half_cut_out()));
    cut.materials[0].base_texture = Some(cut.images.len() - 1);
    cut.materials[0].alpha = AlphaMode::Mask { cutoff: 0.5 };
    place(
        &device,
        &queue,
        &mut scene,
        cut,
        Vec3::new(-1.2, bottom, 0.),
    );
    let mut glass = square(-2., 0.2);
    glass.materials[0].base = [0.2, 0.9, 0.3, 0.4];
    glass.materials[0].alpha = AlphaMode::Blend {
        receives_screen_space_reflections: false,
    };
    place(
        &device,
        &queue,
        &mut scene,
        glass,
        Vec3::new(0.273, -0.4, 0.),
    );
    let masks = masks(&device, &queue, (&mut renderer, &settings), &mut scene, 1.);
    assert_masks(
        &masks,
        &[
            ((8, 20), [0., 1.], "moving layers"),
            ((20, 20), [0., 0.], "the floor"),
            ((32, 20), [0., 0.], "still layers"),
            ((55, 20), [0., 0.], "an unlit material's moving layers"),
            ((4, 44), [0., 0.], "a masked material's cut-out texels"),
            ((12, 44), [0., 1.], "a masked material's moving layers"),
            ((28, 44), [0., 1.], "moving layers"),
            ((37, 44), [0.4, 1.], "a blended square over moving layers"),
            ((43, 44), [0.4, 0.4], "a blended square over the floor"),
            ((52, 44), [0., 0.], "moving layers an opaque square hides"),
            ((60, 44), [0., 1.], "moving layers"),
        ],
    );
}

// Plausible defect: the scene's count of moving opaque materials misses a
// material whose layers an edit sets moving, so the frame draws no moving
// surface. The oracle is the documented contract: after
// `Scene::set_material` gives a still square moving layers, its texels read
// (0, 1) where they read (0, 0) before.
#[test]
fn a_material_set_moving_marks_fsr2s_composition_mask() {
    let Some((device, queue)) = test_support::fsr2_device() else {
        return;
    };
    let Some((mut renderer, settings)) = fsr2_renderer(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, square(-6., 8.));
    let mut still = square(-3., 0.4);
    still.materials[0].normal_layers = Some(STILL);
    let ids = scene.add_asset(&device, &queue, still).unwrap();
    scene
        .add_instance(
            &device,
            &queue,
            InstanceState::new(ids.model),
            Mobility::Static,
        )
        .unwrap();
    let centre = [((32, 32), [0., 0.], "still layers")];
    let before = masks(&device, &queue, (&mut renderer, &settings), &mut scene, 1.);
    assert_masks(&before, &centre);
    let material = ids.materials[0];
    let mut values = scene.material(material).unwrap();
    values.normal_layers = Some(MOVING);
    scene.set_material(&queue, material, values).unwrap();
    let after = masks(&device, &queue, (&mut renderer, &settings), &mut scene, 2.);
    assert_masks(&after, &[((32, 32), [0., 1.], "layers set moving")]);
}
