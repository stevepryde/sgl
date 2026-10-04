//! The volumetric fog in real frames: a medium without density leaves the
//! frame as no fog does; a point light's scattering along a froxel column
//! matches a numerical single-scattering integral along its view ray; a
//! shadowed light, local or directional, scatters nothing in the medium its
//! occluder hides from it; the filter blurs each slice by Godot's Gaussian
//! while the history stays unfiltered; a light's fog energy scales its
//! light in the medium and nowhere else; fog volumes add their medium to
//! every froxel they reach; and the sky takes its sky affect of the fog.
use super::froxels;
use crate::renderer::Renderer;
use crate::settings::{self, FogQuality, Settings};
use crate::shading::gbuffer;
use crate::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, Fog, FrameInput, HemisphereLight,
    InstanceState, Light, LightShape, Mobility, Scene, test_support,
};
use glam::{Mat4, Quat, Vec2, Vec3};

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

/// A camera at the origin looking down -Z over a black backdrop, its
/// atmosphere on so that the settings decide whether the fog runs.
fn input(fog: Fog) -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: projection(),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.atmosphere = true;
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
/// the fog's length as the volume's detail spread spaces them.
fn froxel_ray(input: &FrameInput, size: [u32; 3], column: [u32; 2], slice: u32) -> (Vec2, f32) {
    let u = (column[0] as f32 + 0.5) / size[0] as f32;
    let v = (column[1] as f32 + 0.5) / size[1] as f32;
    let unit = (slice as f32 + 0.5) / size[2] as f32;
    let depth = input.fog.length * unit.powf(crate::shading::fog::DETAIL_SPREAD);
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
    // All of the hemisphere fill's ambient light, so the ambient's
    // scattering is among what an empty medium must not add.
    let mut frame = input(Fog {
        density: 0.,
        length: 30.,
        anisotropy: 0.5,
        ambient: 1.,
        ..Fog::default()
    });
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(0.3, -1., -0.4),
        color: [1., 0.9, 0.8],
        illuminance: 4.,
        shadow: Some(DirectionalShadow {
            distance: 30.,
            cascades: 2,
        }),
        ..Default::default()
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
// integration's step or extinction, are wrong. The
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
                ..Default::default()
            },
        )
        .unwrap();
    let fog = Fog {
        density: 0.06,
        albedo: [1., 0.5, 0.25],
        anisotropy: 0.4,
        length: 10.,
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
// the next frame reprojects, which Godot copies before it filters, or it
// leaves a channel out. Over two frames of a point light in a medium whose
// density falls with height, so its light and its extinction both vary
// across the frame, the froxels the frame wrote are the same with the filter
// as without, the froxels it integrates are Godot's Gaussian of them along x
// and then y within half-float rounding, and the integrated volume differs
// from the unfiltered one.
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
                ..Default::default()
            },
        )
        .unwrap();
    let frame = input(Fog {
        density: 0.06,
        height: -1.,
        height_falloff: 3.,
        length: 10.,
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
    // Froxels the filter moved by far more than that, in the light (red) and
    // the extinction, where a wrong kernel would show.
    let mut spread = [0; 2];
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
        for (moved, channel) in spread.iter_mut().zip([0, 3]) {
            *moved += usize::from(
                (expected[channel] - written[channel]).abs() > 10. * tolerance(expected[channel]),
            );
        }
    }
    assert!(
        spread.iter().all(|moved| *moved > 10_000),
        "the filter moved only {spread:?} froxels' light and extinction"
    );
}

// Defect: the injection ignores a light's shadow, or samples it where the
// medium is not, so light leaks into the medium beneath an opaque slab.
// The occluder's geometry decides: froxels well below the slab, within the
// light's reach, see none of a local light and, through Godot's fog tap,
// at most exp(-10 x the metres they lie behind the slab's top) of the
// directional light; froxels above it see it.
#[test]
fn shadowed_lights_scatter_almost_nothing_behind_their_occluder() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let fog = Fog {
        density: 0.1,
        anisotropy: 0.,
        length: 20.,
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
                        ..Default::default()
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
                }),
                ..Default::default()
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
        // Froxels below y 0.4 lie at least 1.2 m behind the slab's top.
        let most_below = if local {
            0.
        } else {
            darkest_above * (-10f32 * 1.2).exp()
        };
        assert!(
            brightest_below <= most_below,
            "{label}: {brightest_below} scattered beneath the occluder, against {darkest_above} above it"
        );
    }
}

// Defect: a light's fog energy does not scale what it scatters in the
// medium, a light at 0 still scatters there, or the energy reaches its
// surface lighting. The reference is the same light's in-scatter at fog
// energy 1, measured froxel by froxel: at 2 each froxel holds twice it, at
// 0 none, and without fog the frame is the same, and lit, at 0 as at 1.
#[test]
fn fog_energy_scales_a_light_in_the_medium_alone() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let fog = Fog {
        density: 0.05,
        anisotropy: 0.3,
        length: 20.,
        ..Fog::default()
    };
    for (label, local) in [("scene light", true), ("directional light", false)] {
        // Each froxel's in-scatter, and the frame without fog, with the light
        // at `fog_energy` over a floor beneath it.
        let observe = |fog_energy: f32| {
            let mut scene = Scene::new(&device, &queue);
            add_box(
                &device,
                &queue,
                &mut scene,
                Vec3::new(0., -2., -10.),
                Vec3::new(20., 1., 20.),
            );
            let mut frame = input(fog);
            if local {
                scene
                    .add_light(
                        &device,
                        &queue,
                        Light {
                            position: Vec3::new(0., 3., -8.),
                            shape: LightShape::Spot {
                                direction: Vec3::NEG_Y,
                                inner_angle: 0.5,
                                outer_angle: 0.8,
                            },
                            intensity: 60.,
                            range: 12.,
                            casts_shadow: true,
                            fog_energy,
                            ..Light::default()
                        },
                    )
                    .unwrap();
            } else {
                frame.directional_lights[0] = Some(DirectionalLight {
                    direction: Vec3::new(0.2, -1., 0.1),
                    illuminance: 3.,
                    shadow: Some(DirectionalShadow {
                        distance: 30.,
                        cascades: 2,
                    }),
                    fog_energy,
                    ..DirectionalLight::default()
                });
            }
            let fogged = render(&device, &queue, &mut scene, &frame, &settings(true), 1);
            let in_scatter: Vec<[f32; 3]> = texels(&read(&device, &queue, fogged.fog_volumes()[0]))
                .into_iter()
                .map(|froxel| [froxel[0], froxel[1], froxel[2]])
                .collect();
            let unfogged = render(&device, &queue, &mut scene, &frame, &settings(false), 1);
            let surfaces = read(&device, &queue, &unfogged.targets().composite);
            (in_scatter, surfaces)
        };
        let (single, surfaces) = observe(1.);
        let lit = single
            .iter()
            .filter(|froxel| froxel.iter().any(|&channel| channel > 1e-3))
            .count();
        assert!(lit > 100, "{label}: {lit} froxels carry measurable light");
        let (double, _) = observe(2.);
        for (index, (twice, once)) in double.iter().zip(&single).enumerate() {
            for channel in 0..3 {
                let expected = 2. * once[channel];
                assert!(
                    (twice[channel] - expected).abs() <= 1e-3 * expected + 1e-6,
                    "{label}: froxel {index} channel {channel} holds {} at fog energy 2, twice {} is {expected}",
                    twice[channel],
                    once[channel]
                );
            }
        }
        let (none, surfaces_at_zero) = observe(0.);
        assert!(
            none.iter().flatten().all(|&channel| channel == 0.),
            "{label}: the medium scattered light at fog energy 0"
        );
        assert!(
            texels(&surfaces)
                .iter()
                .any(|texel| texel[..3].iter().any(|&channel| channel > 0.)),
            "{label}: the light lit no surface"
        );
        assert!(
            surfaces_at_zero == surfaces,
            "{label}: fog energy 0 changed the surfaces' light"
        );
    }
}

// Defect: a fog volume's box is placed, turned or sized wrongly, or its
// density does not add to the medium's; or its froxel bounds leave out
// froxels it reaches, at its faces or where it crosses the frame's edge,
// the camera's plane or the volume's far end, or holds the camera; or a
// froxel reads another volume's record for bounds that skip culled
// volumes; or the bounds misplace the camera (its position, turn or
// off-centre projection). The boxes' definitions decide: a froxel holds the
// frame's medium plus each box's density where its centre lies, and
// scatters an isotropic, unshadowed directional light by the albedos those
// densities weight, Godot's scattering.
#[test]
fn fog_volumes_add_their_medium_to_every_froxel_they_reach() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    // A camera away from the origin, turned, with an off-centre projection;
    // each box is placed relative to it.
    let turn = Quat::from_rotation_y(0.7);
    let world_from_camera = Mat4::from_rotation_translation(turn, Vec3::new(12., 1.5, -40.));
    let volume = |center: Vec3, rotation: Quat, size: Vec3, density: f32, albedo: [f32; 3]| {
        crate::FogVolume {
            center: world_from_camera.transform_point3(center),
            rotation: turn * rotation,
            size,
            density,
            albedo,
            edge_fade: 0.,
        }
    };
    let volumes = [
        // Behind the camera, beside the frame and beyond the volume: none
        // reaches a froxel.
        volume(
            Vec3::new(0., 0., 6.),
            Quat::IDENTITY,
            Vec3::splat(3.),
            0.5,
            [1., 0., 0.],
        ),
        volume(
            Vec3::new(-30., 0., -8.),
            Quat::IDENTITY,
            Vec3::splat(2.),
            0.5,
            [0., 1., 0.],
        ),
        volume(
            Vec3::new(0., 0., -30.),
            Quat::IDENTITY,
            Vec3::splat(4.),
            0.5,
            [0., 0., 1.],
        ),
        // Beside the camera, from behind it to in front of it.
        volume(
            Vec3::new(1.5, -1., 0.),
            Quat::IDENTITY,
            Vec3::new(1., 1., 12.),
            0.3,
            [0.2, 0.9, 0.5],
        ),
        // A tunnel holding the camera.
        volume(
            Vec3::new(0., 0., -4.),
            Quat::from_rotation_y(0.1),
            Vec3::new(6., 4., 20.),
            0.05,
            [0.5, 0.5, 1.],
        ),
        // Across the frame's left edge.
        volume(
            Vec3::new(-7.5, 0.5, -8.),
            Quat::from_rotation_y(0.4),
            Vec3::new(3., 2., 3.),
            0.2,
            [0.9, 0.3, 0.6],
        ),
        // Across the volume's far end.
        volume(
            Vec3::new(1., 1., -19.5),
            Quat::IDENTITY,
            Vec3::new(2., 2., 3.),
            0.15,
            [0.4, 0.8, 0.2],
        ),
        // Two overlapping boxes.
        volume(
            Vec3::new(0.5, 0., -9.),
            Quat::from_rotation_y(0.6) * Quat::from_rotation_x(0.3),
            Vec3::new(4., 3., 5.),
            0.2,
            [1., 0.5, 0.25],
        ),
        volume(
            Vec3::new(1.8, 0.6, -10.5),
            Quat::IDENTITY,
            Vec3::splat(3.),
            0.1,
            [0.2, 0.9, 0.6],
        ),
    ];
    let mut scene = Scene::new(&device, &queue);
    scene.update_fog_volumes(&device, &queue, &volumes).unwrap();
    let fog = Fog {
        density: 0.01,
        albedo: [1., 0.7, 0.4],
        anisotropy: 0.,
        ambient: 0.,
        length: 20.,
        ..Fog::default()
    };
    let mut frame = input(fog);
    let mut projection = projection();
    projection.z_axis.x = 0.1;
    projection.z_axis.y = -0.05;
    frame.camera = Camera {
        view: world_from_camera.inverse(),
        projection,
        eye: world_from_camera.w_axis.truncate(),
    };
    // An isotropic phase scatters 1 / (4 PI) of the illuminance toward the
    // camera, so each froxel scatters its albedo-weighted density.
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(0.3, -1., -0.4),
        color: [1.; 3],
        illuminance: 4. * std::f32::consts::PI,
        shadow: None,
        ..Default::default()
    });
    let renderer = render(&device, &queue, &mut scene, &frame, &settings(true), 1);
    let size = froxels(QUALITY, SIZE);
    let froxels = texels(&read(&device, &queue, renderer.fog_volumes()[0]));
    let local_from_world = volumes
        .map(|volume| Mat4::from_rotation_translation(volume.rotation, volume.center).inverse());
    let world_from_view = frame.camera.view.inverse();
    let mut reached = [0; 9];
    for z in 0..size[2] {
        for y in 0..size[1] {
            for x in 0..size[0] {
                let point =
                    world_from_view.transform_point3(froxel_center(&frame, size, [x, y, z]));
                let mut density = fog.density;
                let mut scattering = Vec3::from(fog.albedo) * fog.density;
                for (index, volume) in volumes.iter().enumerate() {
                    let local = local_from_world[index].transform_point3(point);
                    let added = volume.density * box_density(local, volume.size * 0.5);
                    density += added;
                    scattering += Vec3::from(volume.albedo) * added;
                    reached[index] += usize::from(added > 0.);
                }
                let froxel = froxels[((z * size[1] + y) * size[0] + x) as usize];
                assert!(
                    (froxel[3] - density).abs() <= 1e-3 * density + 1e-4,
                    "{point}: extinction {} against the boxes' {density}",
                    froxel[3]
                );
                for channel in 0..3 {
                    assert!(
                        (froxel[channel] - scattering[channel]).abs()
                            <= 2e-3 * scattering[channel] + 1e-4,
                        "{point} channel {channel}: scattering {} against the boxes' {}",
                        froxel[channel],
                        scattering[channel]
                    );
                }
            }
        }
    }
    assert!(
        reached[3..].iter().all(|&count| count > 50),
        "froxels inside each box: {reached:?}"
    );
}

/// The share of a box fog volume's density at `local` in the box of
/// `half_size` without edge fade: Godot's box `FogVolume`
/// (`volumetric_fog.glsl`), whose density fades in over the 0.1 m inside its
/// faces by its signed distance.
fn box_density(local: Vec3, half_size: Vec3) -> f32 {
    let q = local.abs() - half_size;
    let distance = q.max(Vec3::ZERO).length() + q.max_element().min(0.);
    let t = ((distance + 0.1) / 0.1).clamp(0., 1.);
    1. - t * t * (3. - 2. * t)
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

// Defect: the sky ignores `Fog::sky_affect`, it reaches the surfaces too, or
// it scales one part of the fog (its scattering or its transmittance) alone.
// Godot's sky affect (b130438 sky.glsl) mixes the sky with the fogged sky:
// at 0 the sky stays as without fog while surfaces fog as at 1, and at 0.5
// each sky texel lies halfway between those two frames.
#[test]
fn the_sky_takes_its_sky_affect_of_the_fog() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    add_box(
        &device,
        &queue,
        &mut scene,
        Vec3::new(0., -1., -6.),
        Vec3::new(4., 1., 3.),
    );
    // The medium scatters the hemisphere fill, so the sky's fog both dims it
    // and adds light.
    let mut frame = input(Fog {
        density: 0.05,
        length: 30.,
        ambient: 1.,
        ..Fog::default()
    });
    frame.hemisphere_light = HemisphereLight {
        sky_color: [0.4, 0.5, 0.7],
        ground_color: [0.1, 0.1, 0.1],
        intensity: 1.,
    };
    frame.backdrop = Backdrop::Color([0.2, 0.3, 0.45]);
    let mut composite = |frame: &FrameInput, atmosphere: bool| {
        let renderer = render(&device, &queue, &mut scene, frame, &settings(atmosphere), 1);
        texels(&read(&device, &queue, &renderer.targets().composite))
    };
    let clear = composite(&frame, false);
    let whole = composite(&frame, true);
    frame.fog.sky_affect = 0.;
    let none = composite(&frame, true);
    frame.fog.sky_affect = 0.5;
    let half = composite(&frame, true);
    // The frame's top row looks up into the sky, whose backdrop is one colour.
    let sky_colour = clear[0];
    let sky: Vec<_> = (0..clear.len())
        .filter(|&texel| clear[texel] == sky_colour)
        .collect();
    assert!(
        sky.len() < clear.len(),
        "the frame holds no surface: {sky_colour:?}"
    );
    assert!(whole[0] != clear[0], "the whole fog left the sky clear");
    for texel in 0..clear.len() {
        if sky.contains(&texel) {
            assert_eq!(
                none[texel], clear[texel],
                "texel {texel}: no sky affect fogged the sky"
            );
            for channel in 0..3 {
                let midway = (clear[texel][channel] + whole[texel][channel]) / 2.;
                assert!(
                    (half[texel][channel] - midway).abs() < 2e-3,
                    "texel {texel} channel {channel}: half the sky affect gave {} against {midway}",
                    half[texel][channel]
                );
            }
        } else {
            assert_eq!(
                none[texel], whole[texel],
                "texel {texel}: the sky affect fogged a surface"
            );
        }
    }
}
