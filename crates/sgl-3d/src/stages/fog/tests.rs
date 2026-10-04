//! The volumetric fog in real frames: a medium without density leaves the
//! frame as no fog does; a point light's scattering along a froxel column
//! matches a numerical single-scattering integral along its view ray; a
//! shadowed light, local or directional, scatters nothing in the medium its
//! occluder hides from it; and the filter blurs each slice by Godot's
//! Gaussian while the history stays unfiltered.
use super::froxels;
use crate::renderer::Renderer;
use crate::settings::{self, FogQuality, Settings};
use crate::shading::gbuffer;
use crate::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, Fog, FrameInput, HemisphereLight,
    InstanceState, Light, LightShape, Mobility, Scene, test_support,
};
use glam::{Mat4, Vec2, Vec3};

const SIZE: [u32; 2] = [160, 90];
const QUALITY: FogQuality = FogQuality::High;

fn projection() -> Mat4 {
    crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1)
}

fn settings(atmosphere: bool) -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere,
        fog_quality: QUALITY,
        ..Settings::default()
    }
}

/// A camera at the origin looking down -Z over a black backdrop.
fn input(fog: Fog) -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: projection(),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.fog = fog;
    input
}

/// `frames` frames of `scene` seen as `input`, the first a camera cut.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
    frames: usize,
) -> Renderer {
    let mut renderer = Renderer::for_test(device, queue, SIZE, settings);
    let output = crate::view::targets::target(device, "fog frames", SIZE, gbuffer::COLOR);
    for frame in 0..frames {
        let mut input = *input;
        input.camera_cut = frame == 0;
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
    }
    renderer
}

fn texels(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|channel| test_support::half(&texel[channel * 2..])))
        .collect()
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) -> Vec<u8> {
    test_support::read(device, queue, view.texture(), 8)
}

/// A static occluder: `test_support::cube` scaled by `scale` about `center`.
fn add_box(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    center: Vec3,
    scale: Vec3,
) {
    let ids = scene
        .add_asset(device, queue, test_support::cube())
        .unwrap();
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                model: ids.model,
                pose: Mat4::from_translation(center) * Mat4::from_scale(scale),
                visible: true,
                capture_visible: true,
            },
            Mobility::Static,
        )
        .unwrap();
}

/// The world position of froxel `index` of a volume of `size` seen by
/// `input`'s camera at the origin, without jitter: the centre of its
/// column at the centre of its slice.
fn froxel_center(input: &FrameInput, size: [u32; 3], index: [u32; 3]) -> Vec3 {
    let (ndc, depth) = froxel_ray(input, size, [index[0], index[1]], index[2]);
    view_ray(input, ndc) * depth
}

/// Column `column`'s frame position and slice `slice`'s view depth, from
/// the frame's geometry: columns split the frame evenly, and slices split
/// the fog's length as its detail spread spaces them.
fn froxel_ray(input: &FrameInput, size: [u32; 3], column: [u32; 2], slice: u32) -> (Vec2, f32) {
    let u = (column[0] as f32 + 0.5) / size[0] as f32;
    let v = (column[1] as f32 + 0.5) / size[1] as f32;
    let unit = (slice as f32 + 0.5) / size[2] as f32;
    let depth = input.fog.length * unit.powf(input.fog.detail_spread);
    (Vec2::new(u * 2. - 1., 1. - v * 2.), depth)
}

/// The view-space point a metre deep under frame position `ndc`, which the
/// camera's projection maps there.
fn view_ray(input: &FrameInput, ndc: Vec2) -> Vec3 {
    let p = input.camera.projection;
    Vec3::new(
        (ndc.x + p.z_axis.x) / p.x_axis.x,
        (ndc.y + p.z_axis.y) / p.y_axis.y,
        -1.,
    )
}

// Defect: the fog adds light or dims where the medium has no density, or
// the composition is not color * transmittance + scattering, so the frame
// with an empty medium, skipped or run, differs from the frame with no fog.
// The run's integrated volume must be exactly clear, and a denser medium
// must change the frame.
#[test]
fn an_empty_medium_leaves_the_frame_as_no_fog_does() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    add_box(
        &device,
        &queue,
        &mut scene,
        Vec3::new(0., -0.5, -6.),
        Vec3::new(3., 1., 3.),
    );
    let mut frame = input(Fog {
        density: 0.,
        length: 30.,
        anisotropy: 0.5,
        ..Fog::default()
    });
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(0.3, -1., -0.4),
        color: [1., 0.9, 0.8],
        illuminance: 4.,
        shadow: Some(DirectionalShadow {
            distance: 30.,
            cascades: 2,
            first_split: 8.,
        }),
    });
    frame.hemisphere_light = HemisphereLight {
        sky_color: [0.4, 0.5, 0.7],
        ground_color: [0.1, 0.1, 0.1],
        intensity: 1.,
    };
    frame.backdrop = Backdrop::Color([0.2, 0.3, 0.45]);
    let composite = |renderer: &Renderer| read(&device, &queue, &renderer.targets().composite);
    let unfogged = composite(&render(
        &device,
        &queue,
        &mut scene,
        &frame,
        &settings(false),
        1,
    ));
    // Without a medium the fog has nothing to do.
    let skipped = composite(&render(
        &device,
        &queue,
        &mut scene,
        &frame,
        &settings(true),
        1,
    ));
    assert!(skipped == unfogged, "no medium changed the frame");
    // An empty fog volume runs the fog over a medium without density; the
    // second frame reprojects the first's volume.
    scene
        .update_fog_volumes(
            &device,
            &queue,
            &[crate::FogVolume {
                center: Vec3::new(0., 0., -6.),
                rotation: glam::Quat::IDENTITY,
                size: Vec3::splat(4.),
                density: 0.,
                albedo: [1.; 3],
                edge_fade: 0.,
            }],
        )
        .unwrap();
    let empty = render(&device, &queue, &mut scene, &frame, &settings(true), 2);
    let integrated = texels(&read(&device, &queue, empty.fog_volumes()[2]));
    assert!(
        integrated.iter().all(|texel| *texel == [0., 0., 0., 1.]),
        "an empty medium scattered or dimmed"
    );
    assert!(
        composite(&empty) == unfogged,
        "an empty medium changed the frame"
    );
    frame.fog.density = 0.05;
    let dense = composite(&render(
        &device,
        &queue,
        &mut scene,
        &frame,
        &settings(true),
        1,
    ));
    assert!(dense != unfogged, "a dense medium left the frame unfogged");
}

/// Henyey-Greenstein's phase function.
fn henyey_greenstein(cos_theta: f32, g: f32) -> f32 {
    (1. - g * g) / (4. * std::f32::consts::PI * (1. + g * g - 2. * g * cos_theta).powf(1.5))
}

// Defect: the injection's units, phase, light attenuation or albedo, or the
// integration's step, extinction or energy-conserving weight, are wrong. The
// reference integrates single scattering along each column's view ray from
// the camera to a slice's centre by brute force: a homogeneous medium's
// transmittance exp(-sigma t) (Beer-Lambert) times what it scatters from a
// point light of known intensity, inverse-square falloff and range window
// (the light's documented attenuation), weighted by the phase function.
#[test]
fn point_light_scattering_matches_a_single_scattering_integral() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let light_position = Vec3::new(1.2, 1.2, -4.5);
    let intensity = 40.;
    let range = 25.;
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: light_position,
                shape: LightShape::Point,
                color: [1.; 3],
                intensity,
                range,
                baked: false,
                specular: 1.,
                casts_shadow: false,
            },
        )
        .unwrap();
    let fog = Fog {
        density: 0.06,
        albedo: [1., 0.5, 0.25],
        anisotropy: 0.4,
        length: 10.,
        detail_spread: 1.,
        ..Fog::default()
    };
    let frame = input(fog);
    let renderer = render(&device, &queue, &mut scene, &frame, &settings(true), 1);
    let size = froxels(QUALITY, SIZE);
    let integrated = texels(&read(&device, &queue, renderer.fog_volumes()[2]));
    let mut checked = 0;
    for column in [
        [size[0] / 2, size[1] / 2],
        [size[0] * 3 / 4, size[1] / 2],
        [size[0] / 3, size[1] / 3],
        [size[0] * 2 / 3, size[1] * 2 / 3],
    ] {
        let along = view_ray(&frame, froxel_ray(&frame, size, column, 0).0);
        let direction = along.normalize();
        // Close passes concentrate the light into a few slices, where the
        // froxels' resolution, not the method, limits agreement.
        let closest = (light_position - direction * light_position.dot(direction)).length();
        assert!(
            closest > 1.,
            "column {column:?} passes {closest} m from the light"
        );
        for slice in [size[2] / 4, size[2] / 2, size[2] * 3 / 4, size[2] - 1] {
            let distance = froxel_ray(&frame, size, column, slice).1 * along.length();
            let steps = 20_000;
            let step = distance / steps as f32;
            let mut reference = [0f32; 3];
            for index in 0..steps {
                let t = (index as f32 + 0.5) * step;
                let point = direction * t;
                let to_light = light_position - point;
                let square = to_light.length_squared();
                let window = (1. - (square / (range * range)).powi(2))
                    .clamp(0., 1.)
                    .powi(2);
                let irradiance = intensity * window / square.max(1e-4);
                let phase = henyey_greenstein(direction.dot(to_light.normalize()), fog.anisotropy);
                let attenuated = irradiance * phase * (-fog.density * t).exp() * step;
                for (sum, albedo) in reference.iter_mut().zip(fog.albedo) {
                    *sum += fog.density * albedo * attenuated;
                }
            }
            let texel = integrated[((slice * size[1] + column[1]) * size[0] + column[0]) as usize];
            for channel in 0..3 {
                let error = (texel[channel] - reference[channel]).abs();
                assert!(
                    error <= 0.03 * reference[channel] + 1e-4,
                    "column {column:?} slice {slice} channel {channel}: {} against the integral's {}",
                    texel[channel],
                    reference[channel]
                );
            }
            let transmittance = (-fog.density * distance).exp();
            assert!(
                (texel[3] - transmittance).abs() <= 2e-3,
                "column {column:?} slice {slice}: transmittance {} against Beer-Lambert's {transmittance}",
                texel[3]
            );
            checked += usize::from(reference[0] > 1e-3);
        }
    }
    assert!(
        checked >= 8,
        "too few samples carry measurable light: {checked}"
    );
}

/// Godot's filter weights (b130438 `volumetric_fog_process.glsl`
/// MODE_FILTER `gauss`), from three froxels before to three after.
const GODOT_GAUSS: [f32; 7] = [
    0.071303, 0.131514, 0.189879, 0.214607, 0.189879, 0.131514, 0.071303,
];

/// `volume` of `size` blurred along `axis` (0 x, 1 y) by Godot's weights,
/// its coordinates clamped to the volume as Godot clamps them.
fn godot_gaussian(volume: &[[f32; 4]], size: [u32; 3], axis: usize) -> Vec<[f32; 4]> {
    let index = |at: [u32; 3]| ((at[2] * size[1] + at[1]) * size[0] + at[0]) as usize;
    let mut blurred = vec![[0.; 4]; volume.len()];
    for z in 0..size[2] {
        for y in 0..size[1] {
            for x in 0..size[0] {
                let at = [x, y, z];
                let sum = &mut blurred[index(at)];
                for (tap, weight) in GODOT_GAUSS.iter().enumerate() {
                    let mut from = at;
                    from[axis] =
                        (at[axis] as i32 + tap as i32 - 3).clamp(0, size[axis] as i32 - 1) as u32;
                    for (channel, value) in volume[index(from)].iter().enumerate() {
                        sum[channel] += weight * value;
                    }
                }
            }
        }
    }
    blurred
}

// Defect: the filter takes other weights, axes or edges than Godot's, the
// integration reads the unfiltered froxels, or the filter reaches the history
// the next frame reprojects, which Godot copies before it filters. Over two
// frames of a point light in the medium, the froxels the frame wrote are the
// same with the filter as without, the froxels it integrates are Godot's
// Gaussian of them along x and then y within half-float rounding, and the
// integrated volume differs from the unfiltered one.
#[test]
fn the_filter_blurs_each_slice_by_godots_gaussian_and_leaves_the_history_unfiltered() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: Vec3::new(0.5, 0.3, -3.),
                shape: LightShape::Point,
                color: [1.; 3],
                intensity: 40.,
                range: 25.,
                baked: false,
                specular: 1.,
                casts_shadow: false,
            },
        )
        .unwrap();
    let frame = input(Fog {
        density: 0.06,
        length: 10.,
        detail_spread: 1.,
        ..Fog::default()
    });
    let mut volumes = |fog_filter: bool| {
        let settings = Settings {
            fog_filter,
            ..settings(true)
        };
        let renderer = render(&device, &queue, &mut scene, &frame, &settings, 2);
        renderer
            .fog_volumes()
            .map(|view| texels(&read(&device, &queue, view)))
    };
    let [written, filtered, integrated] = volumes(true);
    let [unfiltered_written, _, unfiltered_integrated] = volumes(false);
    assert!(
        written == unfiltered_written,
        "the filter changed the froxels the next frame reprojects"
    );
    assert!(
        integrated != unfiltered_integrated,
        "the integration ignored the filter"
    );
    let size = froxels(QUALITY, SIZE);
    let expected = godot_gaussian(&godot_gaussian(&written, size, 0), size, 1);
    // Two half-float roundings of non-negative sums, one per pass.
    let tolerance = |expected: f32| 2e-3 * expected + 1e-6;
    // Froxels the filter moved by far more than that, where a wrong kernel
    // would show.
    let mut spread = 0;
    for (index, ((filtered, expected), written)) in
        filtered.iter().zip(&expected).zip(&written).enumerate()
    {
        for channel in 0..4 {
            assert!(
                (filtered[channel] - expected[channel]).abs() <= tolerance(expected[channel]),
                "froxel {index} channel {channel}: {} against Godot's Gaussian {}",
                filtered[channel],
                expected[channel]
            );
        }
        spread += usize::from((expected[0] - written[0]).abs() > 10. * tolerance(expected[0]));
    }
    assert!(spread > 1_000, "the filter moved only {spread} froxels");
}

// Defect: the injection ignores a light's shadow, or samples it where the
// medium is not, so light leaks into the medium beneath an opaque slab.
// The occluder's geometry decides: froxels well below the slab, within the
// light's reach, see none of it; froxels above it see it.
#[test]
fn shadowed_lights_scatter_nothing_behind_their_occluder() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let fog = Fog {
        density: 0.1,
        anisotropy: 0.,
        length: 20.,
        detail_spread: 1.,
        ..Fog::default()
    };
    // The slab spans y 1.4..1.6 and x and z ±20 about (0, -10).
    let slab = |device: &wgpu::Device, queue: &wgpu::Queue| {
        let mut scene = Scene::new(device, queue);
        add_box(
            device,
            queue,
            &mut scene,
            Vec3::new(0., 1.5, -10.),
            Vec3::new(40., 0.2, 40.),
        );
        scene
    };
    let spot = Vec3::new(0., 4., -10.);
    // Where each light reaches the medium.
    let reached = |local: bool, point: Vec3| {
        if local {
            // Within the spot's inner cone and range.
            let below = spot.y - point.y;
            let across = Vec3::new(point.x - spot.x, 0., point.z - spot.z).length();
            below > 0.5 && across < below * 0.6f32.tan() && (spot - point).length() < 12.
        } else {
            // Beneath the slab, away from its edges along the light.
            point.x.abs() < 15. && point.z < -2. && point.z > -25.
        }
    };
    for (label, local) in [("local atlas", true), ("directional cascades", false)] {
        let mut scene = slab(&device, &queue);
        let mut frame = input(fog);
        if local {
            scene
                .add_light(
                    &device,
                    &queue,
                    Light {
                        position: spot,
                        shape: LightShape::Spot {
                            direction: Vec3::NEG_Y,
                            inner_angle: 0.6,
                            outer_angle: 0.9,
                        },
                        color: [1.; 3],
                        intensity: 100.,
                        range: 15.,
                        baked: false,
                        specular: 1.,
                        casts_shadow: true,
                    },
                )
                .unwrap();
        } else {
            frame.directional_lights[0] = Some(DirectionalLight {
                direction: Vec3::new(0.2, -1., 0.1),
                color: [1.; 3],
                illuminance: 5.,
                shadow: Some(DirectionalShadow {
                    distance: 40.,
                    cascades: 2,
                    first_split: 10.,
                }),
            });
        }
        let renderer = render(&device, &queue, &mut scene, &frame, &settings(true), 1);
        let size = froxels(QUALITY, SIZE);
        let froxels = texels(&read(&device, &queue, renderer.fog_volumes()[0]));
        let (mut below, mut above) = (Vec::new(), Vec::new());
        for z in 0..size[2] {
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let point = froxel_center(&frame, size, [x, y, z]);
                    if !reached(local, point) || point.z > -1. {
                        continue;
                    }
                    let light = froxels[((z * size[1] + y) * size[0] + x) as usize][0];
                    if point.y < 0.4 {
                        below.push(light);
                    } else if point.y > 2.6 {
                        above.push(light);
                    }
                }
            }
        }
        assert!(
            below.len() > 100 && above.len() > 10,
            "{label}: {} froxels below and {} above",
            below.len(),
            above.len()
        );
        let darkest_above = above.iter().copied().fold(f32::MAX, f32::min);
        let brightest_below = below.iter().copied().fold(0., f32::max);
        assert!(darkest_above > 0., "{label}: lit medium scattered nothing");
        assert!(
            brightest_below == 0.,
            "{label}: {brightest_below} scattered beneath the occluder"
        );
    }
}

// Defect: a fog volume's box is placed, turned or sized wrongly, or its
// density does not add to the medium's. The box's own geometry decides:
// froxels well inside it hold both densities, froxels well outside only the
// medium's.
#[test]
fn a_fog_volume_adds_its_density_inside_its_box() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let volume = crate::FogVolume {
        center: Vec3::new(1., 0.5, -8.),
        rotation: glam::Quat::from_rotation_y(0.6) * glam::Quat::from_rotation_x(0.3),
        size: Vec3::new(4., 2., 6.),
        density: 0.2,
        albedo: [1.; 3],
        edge_fade: 0.,
    };
    scene
        .update_fog_volumes(&device, &queue, &[volume])
        .unwrap();
    let fog = Fog {
        density: 0.01,
        length: 20.,
        detail_spread: 1.,
        ..Fog::default()
    };
    let frame = input(fog);
    let renderer = render(&device, &queue, &mut scene, &frame, &settings(true), 1);
    let size = froxels(QUALITY, SIZE);
    let froxels = texels(&read(&device, &queue, renderer.fog_volumes()[0]));
    let local_from_world =
        Mat4::from_rotation_translation(volume.rotation, volume.center).inverse();
    let (mut inside, mut outside) = (0, 0);
    for z in 0..size[2] {
        for y in 0..size[1] {
            for x in 0..size[0] {
                let point = froxel_center(&frame, size, [x, y, z]);
                let local = local_from_world.transform_point3(point).abs() - volume.size * 0.5;
                let extinction = froxels[((z * size[1] + y) * size[0] + x) as usize][3];
                if local.max_element() < -0.3 {
                    inside += 1;
                    assert!(
                        (extinction - 0.21).abs() < 1e-3,
                        "{point} inside the volume: extinction {extinction}"
                    );
                } else if local.max_element() > 0.3 {
                    outside += 1;
                    assert!(
                        (extinction - 0.01).abs() < 1e-4,
                        "{point} outside the volume: extinction {extinction}"
                    );
                }
            }
        }
    }
    assert!(
        inside > 100 && outside > 100,
        "{inside} inside, {outside} outside"
    );
}

// Defect: a blended surface fogs itself at the wrong distance (a fragment's
// position w is one over its view depth). With no light in the medium, an
// unlit blended quad at a known depth keeps exactly the share of its colour
// that Beer-Lambert's law passes along its view ray.
#[test]
fn a_blended_surface_is_fogged_at_its_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let depth = 30.;
    let mut quad = test_support::cube();
    quad.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| crate::asset::Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * 4., y * 4., -depth],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    quad.materials[0].unlit = true;
    quad.materials[0].base = [1., 0.6, 0.3, 1.];
    quad.materials[0].alpha = crate::AlphaMode::Blend;
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, quad);
    let fog = Fog {
        density: 0.02,
        length: 60.,
        detail_spread: 1.,
        ..Fog::default()
    };
    let frame = input(fog);
    let centre = |atmosphere: bool| {
        let renderer = render(
            &device,
            &queue,
            &mut scene,
            &frame,
            &settings(atmosphere),
            1,
        );
        let composite = texels(&read(&device, &queue, &renderer.targets().composite));
        composite[(SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) as usize]
    };
    let [clear, fogged] = [false, true].map(centre);
    let (ndc, _) = froxel_ray(&frame, [SIZE[0], SIZE[1], 1], [SIZE[0] / 2, SIZE[1] / 2], 0);
    let transmittance = (-fog.density * depth * view_ray(&frame, ndc).length()).exp();
    for channel in 0..3 {
        let share = fogged[channel] / clear[channel];
        assert!(
            (share - transmittance).abs() < 5e-3,
            "channel {channel}: {} of the quad's colour against Beer-Lambert's {transmittance}",
            share
        );
    }
}
