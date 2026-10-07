//! Transmissive surfaces observed in real frames: the light that reaches the
//! camera through them, on a device of either binding tier.
use crate::asset::{Asset, CpuMesh, Material, Vertex};
use crate::graphics_device::BindingTier;
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::{Camera, FrameInput, InstanceState, Mobility, Scene, test_support};
use glam::{DMat4, DVec3, DVec4, Mat4, Quat, Vec3};

const SIZE: [u32; 2] = [128, 128];

/// The camera at the origin looking down -Z.
fn projection() -> Mat4 {
    crate::perspective(1., 1., 0.1)
}

/// No antialiasing (and so no jitter), bloom or atmosphere; no light, and no
/// environment, so the sky is black.
fn settings() -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    }
}

/// A rectangle facing +Z on the plane z = 0, from `min` to `max` in x and y,
/// of `material`.
fn rectangle(min: [f32; 2], max: [f32; 2], material: Material) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(0, 0), (1, 0), (1, 1), (0, 1)]
            .map(|(x, y)| Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [[min[0], max[0]][x], [min[1], max[1]][y], 0.],
                normal: [0., 0., 1.],
                uv: [x as f32, 1. - y as f32],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }];
    asset.materials = vec![material];
    asset
}

/// An unlit material of linear colour `color`.
fn unlit(color: f32) -> Material {
    Material {
        base: [color, color, color, 1.],
        unlit: true,
        ..Material::default()
    }
}

/// A clear dielectric of `ior` that transmits everything behind it: white,
/// smooth, with no specular reflection (F = 0, which leaves the light it
/// transmits whole), single-sided and opaque, its coverage whole.
fn glass(ior: f32, thickness: f32, dispersion: f32) -> Material {
    Material {
        base: [1.; 4],
        metallic: 0.,
        roughness: 0.,
        ior,
        specular: 0.,
        transmission: 1.,
        thickness,
        dispersion,
        double_sided: false,
        ..Material::default()
    }
}

/// An unlit backdrop on the plane z = `depth`, white on the side of `edge`
/// (x, or y where `vertical` is false) below it and black above.
fn backdrop(depth: f32, edge: f32, vertical: bool) -> Vec<(Asset, Mat4)> {
    let [white, black] = if vertical {
        [([-40., -40.], [edge, 40.]), ([edge, -40.], [40., 40.])]
    } else {
        [([-40., -40.], [40., edge]), ([-40., edge], [40., 40.])]
    };
    let at = Mat4::from_translation(Vec3::Z * depth);
    vec![
        (rectangle(white.0, white.1, unlit(1.)), at),
        (rectangle(black.0, black.1, unlit(0.)), at),
    ]
}

/// The binding tier of `device` and the composed frame's texels, linear
/// RGBA, after one frame of `content` (each asset posed), seen from the
/// origin down -Z.
fn composed(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    content: Vec<(Asset, Mat4)>,
) -> (BindingTier, Vec<[f32; 4]>) {
    let settings = settings();
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    for (asset, pose) in content {
        let model = scene.add_asset(device, queue, asset).unwrap().model;
        let state = InstanceState {
            pose,
            ..InstanceState::new(model)
        };
        scene
            .add_instance(device, queue, state, Mobility::Static)
            .unwrap();
    }
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: projection(),
        eye: Vec3::ZERO,
    });
    let output = crate::view::targets::target(device, "transmission", SIZE, gbuffer::COLOR);
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
    let bytes = test_support::read(device, queue, renderer.targets().composite.texture(), 8);
    let texels = bytes
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|channel| test_support::half(&texel[channel * 2..])))
        .collect();
    (renderer.binding_tier(), texels)
}

/// A device at S3D-1's floor, WebGPU's default limits, which takes the Basic
/// binding tier; none without a GPU.
fn floor_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = test_support::adapter()?;
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .unwrap(),
    )
}

// Plausible defects: transmitted light not attenuated, attenuated by the
// thickness without the instance's scale, by a wrong coefficient (inverted,
// another logarithm), or a channel whose attenuation colour is 0 passing
// light or turning into NaN; or, below the Extended tier, the blend passing
// another share than the transmitted light carries. The oracle is
// KHR_materials_volume's Beer-Lambert law (Khronos glTF acfcbe65, README
// 148-168): after x metres a channel of attenuation colour c at attenuation
// distance d passes c^(x / d) of white light. A glass of IOR 1 (no bend) and
// thickness 0.25 posed at scale 2 is crossed over x = 0.5 m at normal
// incidence, its attenuation colour (0.5, 0.25, 0) at d = 0.5 m, in front of
// an unlit white backdrop: the frame behind it holds (0.5, 0.25, 0) where the
// copy holds the frame (Extended), and on the Basic tier, which blends the
// light behind it through unrefracted, their mean, 0.25, on every channel.
#[test]
fn transmitted_light_is_attenuated_by_beer_lambert() {
    let colour = [0.5, 0.25, 0.];
    let content = || {
        let mut slab = glass(1., 0.25, 0.);
        slab.attenuation_distance = 0.5;
        slab.attenuation_color = colour;
        let pose =
            Mat4::from_scale_rotation_translation(Vec3::splat(2.), Quat::IDENTITY, -3. * Vec3::Z);
        let mut content = vec![(rectangle([-0.5, -0.5], [0.5, 0.5], slab), pose)];
        content.push((
            rectangle([-40., -40.], [40., 40.], unlit(1.)),
            Mat4::from_translation(-8. * Vec3::Z),
        ));
        content
    };
    let devices = [test_support::device(), floor_device()];
    for (device, queue) in devices.into_iter().flatten() {
        let (tier, texels) = composed(&device, &queue, content());
        let centre = texels[(SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) as usize];
        let mean = colour.iter().sum::<f32>() / 3.;
        let expected = match tier {
            BindingTier::Extended => colour,
            BindingTier::Basic => [mean; 3],
        };
        for channel in 0..3 {
            assert!(
                centre[channel].is_finite() && (centre[channel] - expected[channel]).abs() <= 5e-3,
                "{tier:?}: channel {channel} passes {}, Beer-Lambert's {}",
                centre[channel],
                expected[channel]
            );
        }
    }
}

/// Where a view ray through frame position `pixel` (in texels, +Y down)
/// that refracts into a glass on the plane z = `glass`, of `ior`, and
/// crosses `thickness` metres of it, leaves it, projected back onto the
/// frame: the frame position it shows, by Snell's law in f64 through the
/// camera's projection. three.js r185, whose model this is, takes the light
/// behind a transmissive surface from where the refracted ray leaves its
/// volume (getVolumeTransmissionRay).
fn refracted(pixel: [f64; 2], glass: f64, ior: f64, thickness: f64) -> [f64; 2] {
    let projection = DMat4::from_cols_array(&projection().to_cols_array().map(f64::from));
    let ndc = [
        pixel[0] / f64::from(SIZE[0]) * 2. - 1.,
        1. - pixel[1] / f64::from(SIZE[1]) * 2.,
    ];
    let far = projection.inverse() * DVec4::new(ndc[0], ndc[1], 0.5, 1.);
    let direction = (far.truncate() / far.w).normalize();
    let entry = direction * (glass / direction.z);
    let normal = DVec3::Z;
    let eta = 1. / ior;
    let cosine = -normal.dot(direction);
    let k = 1. - eta * eta * (1. - cosine * cosine);
    let refracted = eta * direction + (eta * cosine - k.sqrt()) * normal;
    let exit = projection * (entry + refracted * thickness).extend(1.);
    [
        (exit.x / exit.w * 0.5 + 0.5) * f64::from(SIZE[0]),
        (0.5 - exit.y / exit.w * 0.5) * f64::from(SIZE[1]),
    ]
}

/// Where `values`, a line of texels from white to black, crosses one half,
/// between texel centres by linear interpolation; a texel's centre is its
/// index plus one half.
fn crossing(values: &[f32]) -> f64 {
    let at = values
        .windows(2)
        .position(|pair| pair[0] >= 0.5 && pair[1] < 0.5)
        .expect("the line crosses one half");
    let (a, b) = (f64::from(values[at]), f64::from(values[at + 1]));
    at as f64 + 0.5 + (a - 0.5) / (a - b)
}

/// The frame position in [low, high] whose refracted exit, by `shown` (its
/// coordinate along the line), lies at `target`, by bisection.
fn solve(shown: impl Fn(f64) -> f64, target: f64, [mut low, mut high]: [f64; 2]) -> f64 {
    for _ in 0..60 {
        let middle = (low + high) / 2.;
        if (shown(middle) < target) == (shown(low) < target) {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) / 2.
}

/// Where, along the frame's middle row (`vertical`) or column, an edge of
/// the backdrop is seen through a glass of `ior`, `thickness` and
/// `dispersion` 3 m ahead, by channel, and where Snell's law puts it for
/// each IOR of `iors`, on the default device; none without a GPU or on the
/// Basic tier, which refracts nothing.
fn edges(
    vertical: bool,
    (ior, thickness, dispersion): (f32, f32, f32),
    iors: [f64; 3],
) -> Option<[(f64, f64); 3]> {
    let (device, queue) = test_support::device()?;
    // The backdrop 9 m ahead, its edge where it shows about two thirds of the
    // way out from the middle without the glass.
    let edge = if vertical { 3. } else { -3. };
    let line = |texels: &[[f32; 4]], channel: usize| -> Vec<f32> {
        let middle = (SIZE[0] / 2) as usize;
        let width = SIZE[0] as usize;
        let values = (0..width).map(|at| {
            let (x, y) = if vertical { (at, middle) } else { (middle, at) };
            texels[y * width + x][channel]
        });
        // White below the edge, black above: along +X, or along -Y downward.
        let mut values: Vec<f32> = values.collect();
        if !vertical {
            values.reverse();
        }
        values
    };
    let (_, without) = composed(&device, &queue, backdrop(-9., edge, vertical));
    let mut content = backdrop(-9., edge, vertical);
    content.push((
        rectangle([-2.5, -2.5], [2.5, 2.5], glass(ior, thickness, dispersion)),
        Mat4::from_translation(-3. * Vec3::Z),
    ));
    let (tier, through) = composed(&device, &queue, content);
    if tier == BindingTier::Basic {
        eprintln!("skipping: the Basic tier refracts nothing");
        return None;
    }
    let size = f64::from(SIZE[0]);
    let middle = f64::from(SIZE[0] / 2) + 0.5;
    Some([0, 1, 2].map(|channel| {
        let mut shown_at = crossing(&line(&without, channel));
        let mut seen = crossing(&line(&through, channel));
        if !vertical {
            // Back from the reversed column to frame rows.
            shown_at = size - shown_at;
            seen = size - seen;
        }
        let along = |position: f64| {
            let pixel = if vertical {
                [position, middle]
            } else {
                [middle, position]
            };
            refracted(pixel, -3., iors[channel], f64::from(thickness))[usize::from(!vertical)]
        };
        // The edge lies right of the middle, or below it.
        (seen, solve(along, shown_at, [middle, size]))
    }))
}

// Plausible defects: the refracted ray bends the wrong way (the IOR where
// its reciprocal belongs) or not at all, about the wrong normal, without the
// thickness, or its exit projects through another transform than raster's
// (a flipped Y, a half-texel offset). The oracle is Snell's law and the
// camera's projection in f64, with the edge's position in the frame behind
// the glass measured without it: the edge of a backdrop 9 m ahead, seen
// through a glass of IOR 1.5 and 3 m thick 3 m ahead, lies where the
// refracted ray from that pixel leaves the glass over the edge, on a row and
// on a column, to a third of a texel. Without a bend it would lie about
// 8 texels nearer the middle.
#[test]
fn a_refracted_edge_lies_where_snells_law_puts_it() {
    for vertical in [true, false] {
        let Some(edges) = edges(vertical, (1.5, 3., 0.), [1.5; 3]) else {
            return;
        };
        for (channel, (seen, expected)) in edges.into_iter().enumerate() {
            assert!(
                (seen - expected).abs() <= 1. / 3.,
                "vertical {vertical}, channel {channel}: the edge is seen at {seen}, Snell's law puts it at {expected}"
            );
        }
    }
}

// Plausible defects: dispersion spreads the channels' IORs by another
// amount than KHR_materials_dispersion defines, spreads them the wrong way
// or not at all, or, past the point where the red channel's IOR would fall
// below 1, refracts it into total internal reflection, a NaN. The oracle is
// KHR's definition of dispersion as 20 over the Abbe number (Khronos glTF
// acfcbe65, KHR_materials_dispersion README 55-73, 126-135): the blue to red
// IOR spread is (ior - 1) / V, red at ior minus half of it and blue at ior
// plus half, red clamped to 1, and Snell's law for each. At dispersion 10 a
// glass of IOR 1.5 refracts red at 1.375 and blue at 1.625; at 120 red falls
// to 0, clamped to 1, unbent (not the record's 0, an infinite IOR), and blue
// rises to 3.
#[test]
fn dispersion_refracts_each_channel_at_its_own_ior() {
    for (dispersion, iors) in [(10., [1.375, 1.5, 1.625]), (120., [1., 1.5, 3.])] {
        let Some(edges) = edges(true, (1.5, 3., dispersion), iors) else {
            return;
        };
        for (channel, (seen, expected)) in edges.into_iter().enumerate() {
            assert!(
                seen.is_finite() && (seen - expected).abs() <= 1. / 3.,
                "dispersion {dispersion}, channel {channel}: the edge is seen at {seen}, Snell's law at IOR {} puts it at {expected}",
                iors[channel]
            );
        }
    }
}

// Plausible defects: below the Extended tier a transmissive surface's blend
// passes another share of the light behind it than its transmission carries
// (none, as an opaque surface, or more), or adds light of its own. The
// oracle is physics: a surface of IOR 1 with no specular reflection, no
// thickness and whole transmission, white, bends, reflects and absorbs
// nothing, so on the Basic tier, which blends the light behind it through
// unrefracted, the frame behind it is as it is without it, texel for texel.
#[test]
fn an_index_matched_surface_is_invisible_on_the_basic_tier() {
    let Some((device, queue)) = floor_device() else {
        return;
    };
    let (_, without) = composed(&device, &queue, backdrop(-9., 3., true));
    let mut content = backdrop(-9., 3., true);
    content.push((
        rectangle([-2.5, -2.5], [2.5, 2.5], glass(1., 0., 0.)),
        Mat4::from_translation(-3. * Vec3::Z),
    ));
    let (tier, through) = composed(&device, &queue, content);
    assert_eq!(tier, BindingTier::Basic);
    for (at, (a, b)) in without.iter().zip(&through).enumerate() {
        for channel in 0..3 {
            assert!(
                (a[channel] - b[channel]).abs() <= 1e-3,
                "texel {at} channel {channel}: {} through the surface, {} without it",
                b[channel],
                a[channel]
            );
        }
    }
}

// Plausible defects: the transparent stage copies the composed frame in
// frames that show no transmissive material, a cost the issue rules out, or
// does not copy it in one that does. The oracle is the requirement: a frame
// that shows a blended surface but no transmissive one, which lies behind
// the camera, allocates no copy, and the same scene seen the other way,
// showing the transmissive surface, does.
#[test]
fn the_frame_is_copied_only_where_a_transmissive_surface_shows() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = settings();
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    if renderer.binding_tier() == BindingTier::Basic {
        eprintln!("skipping: the Basic tier makes no copy");
        return;
    }
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(
            &device,
            &queue,
            rectangle([-1., -1.], [1., 1.], glass(1.5, 0.1, 0.)),
        )
        .unwrap()
        .model;
    let state = InstanceState {
        pose: Mat4::from_translation(-3. * Vec3::Z),
        ..InstanceState::new(model)
    };
    scene
        .add_instance(&device, &queue, state, Mobility::Static)
        .unwrap();
    // A coverage-blended pane 3 m behind the camera, seen from both sides.
    let pane = Material {
        alpha: crate::AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        },
        base: [0.5, 0.5, 0.5, 0.5],
        double_sided: true,
        ..Material::default()
    };
    let model = scene
        .add_asset(&device, &queue, rectangle([-1., -1.], [1., 1.], pane))
        .unwrap()
        .model;
    let state = InstanceState {
        pose: Mat4::from_translation(3. * Vec3::Z),
        ..InstanceState::new(model)
    };
    scene
        .add_instance(&device, &queue, state, Mobility::Static)
        .unwrap();
    let output = crate::view::targets::target(&device, "transmission", SIZE, gbuffer::COLOR);
    let mut copied = |facing: Quat| {
        let view = Mat4::from_quat(facing);
        let input = FrameInput::new(Camera {
            view,
            projection: projection(),
            eye: Vec3::ZERO,
        });
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
        renderer.transmission_copy().is_some()
    };
    assert!(
        !copied(Quat::from_rotation_y(std::f32::consts::PI)),
        "a frame that shows no transmissive surface copied the frame"
    );
    assert!(
        copied(Quat::IDENTITY),
        "a frame that shows a transmissive surface did not copy the frame"
    );
}
