use super::*;
use glam::camera;
use glam::{DVec3, DVec4};
use wasm_bindgen_test::wasm_bindgen_test;

const MAP: u32 = 2048;

fn shadow(cascades: u32) -> DirectionalShadow {
    DirectionalShadow {
        distance: 200.,
        cascades,
    }
}

/// A deterministic sequence in 0..1.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.next()
    }
}

/// `point` in `cascade`'s clip space, in double precision from the
/// matrix the shaders use.
fn clip(cascade: &Cascade, point: DVec3) -> DVec3 {
    let clip = cascade.clip_from_world.as_dmat4() * point.extend(1.);
    clip.truncate() / clip.w
}

/// Whether `cascade`'s map holds `point`: inside its square and its depth
/// range, as directional_shadow.wgsl tests it.
fn holds(cascade: &Cascade, point: DVec3) -> bool {
    let p = clip(cascade, point);
    p.x.abs() <= 1. && p.y.abs() <= 1. && (0. ..=1.).contains(&p.z)
}

// Plausible defects: a centre not snapped to the texel grid, snapped in a
// space the light's rotation does not keep, or a diameter that follows the
// camera's orientation, any of which makes a still shadow shimmer as the
// camera moves. The oracle is the map itself: a fixed world point, projected
// by each pose's cascade, lands on texel coordinates that differ between
// poses by whole texels.
#[wasm_bindgen_test(unsupported = test)]
fn a_fixed_point_moves_by_whole_texels_as_the_camera_moves_and_turns() {
    let projection = crate::perspective(1.1, 16. / 9., 0.3);
    let points = [
        DVec3::new(13.7, 2.3, -41.9),
        DVec3::new(-3.1, 0.2, -6.4),
        DVec3::new(120.5, -8.25, 77.125),
    ];
    for direction in [
        Vec3::new(0.6, -1., -0.4),
        Vec3::new(-180., 220., 260.).normalize() * -1.,
        Vec3::NEG_Y,
    ] {
        let mut sequence = Sequence(7);
        let poses: Vec<Mat4> = (0..12)
            .map(|_| {
                let eye = Vec3::new(
                    sequence.between(-60., 60.) as f32,
                    sequence.between(0.5, 30.) as f32,
                    sequence.between(-60., 60.) as f32,
                );
                let yaw = sequence.between(-3.1, 3.1) as f32;
                let pitch = sequence.between(-0.6, 0.4) as f32;
                let forward = Vec3::new(
                    yaw.sin() * pitch.cos(),
                    pitch.sin(),
                    -yaw.cos() * pitch.cos(),
                );
                camera::rh::view::look_to_mat4(eye, forward, Vec3::Y)
            })
            .collect();
        let fits: Vec<Cascades> = poses
            .iter()
            .map(|&view| Cascades::camera(view, projection, direction, &shadow(4), MAP).unwrap())
            .collect();
        for (index, first) in fits[0].as_slice().iter().enumerate() {
            for later in &fits[1..] {
                let later = &later.as_slice()[index];
                for point in points {
                    let texels = |cascade: &Cascade| {
                        let p = clip(cascade, point);
                        (p.truncate() * 0.5 + 0.5) * f64::from(MAP)
                    };
                    let moved = texels(later) - texels(first);
                    let fraction = (moved - moved.round()).abs();
                    assert!(
                        fraction.max_element() < 0.01,
                        "cascade {index}, light {direction:?}: {point:?} moved {moved:?} texels"
                    );
                }
            }
        }
    }
}

// Plausible defects: slice corners taken at the wrong depths or from the
// wrong projection, a diameter or centre that leaves part of a slice outside
// its map, a depth range that cuts receivers off, far bounds that leave a gap
// before the shadow distance, or an overlap the next cascade does not cover
// where the shading blends into it. The oracle draws points the camera sees,
// by projecting candidates through its own view and projection, and checks
// that the cascade the shading selects for each (the first whose far bound
// lies beyond its view depth) holds it, as does the next one across the
// overlap.
#[wasm_bindgen_test(unsupported = test)]
fn every_visible_point_within_the_shadow_distance_falls_inside_its_cascade() {
    // Each camera with the region its view space holds at a view depth d:
    // x within centre.x ± (slope.x d + half.x), y likewise.
    struct Camera {
        projection: Mat4,
        slope: DVec3,
        half: DVec3,
        centre: DVec3,
    }
    let perspective = |fov: f32, aspect: f32, near: f32| {
        let tan = f64::from((fov / 2.).tan());
        Camera {
            projection: crate::perspective(fov, aspect, near),
            slope: DVec3::new(tan * f64::from(aspect), tan, 0.),
            half: DVec3::ZERO,
            centre: DVec3::ZERO,
        }
    };
    let cameras = [
        perspective(1.1, 16. / 9., 0.3),
        perspective(0.4, 1., 0.05),
        perspective(2.2, 21. / 9., 1.),
        Camera {
            projection: camera::rh::proj::directx::orthographic(-30., 50., -20., 25., 400., 0.5),
            slope: DVec3::ZERO,
            half: DVec3::new(40., 22.5, 0.),
            centre: DVec3::new(10., 2.5, 0.),
        },
    ];
    let directions = [
        Vec3::new(0.6, -1., -0.4),
        Vec3::new(0.05, -1., 0.02),
        Vec3::new(-1., -0.1, 0.3),
    ];
    let mut sequence = Sequence(11);
    for camera in &cameras {
        let projection = camera.projection;
        for direction in directions {
            for count in 1..=4 {
                let eye = Vec3::new(17., 4., -230.);
                let view = camera::rh::view::look_to_mat4(eye, Vec3::new(0.3, -0.2, -1.), Vec3::Y);
                let shadow = shadow(count);
                let cascades = Cascades::camera(view, projection, direction, &shadow, MAP).unwrap();
                let cascades = cascades.as_slice();
                assert_eq!(cascades.len(), count as usize);
                let clip_from_world = (projection * view).as_dmat4();
                let world_from_view = view.inverse().as_dmat4();
                let mut seen = 0;
                while seen < 3000 {
                    // A candidate in the camera's view space, a little wider
                    // than what it sees, kept when the camera's clip volume
                    // holds it.
                    let depth = sequence.between(0., f64::from(shadow.distance));
                    let reach = (camera.slope * depth + camera.half) * 1.2;
                    let candidate = DVec4::new(
                        camera.centre.x + sequence.between(-reach.x, reach.x),
                        camera.centre.y + sequence.between(-reach.y, reach.y),
                        -depth,
                        1.,
                    );
                    let world = (world_from_view * candidate).truncate();
                    let c = clip_from_world * world.extend(1.);
                    let device = c.truncate() / c.w;
                    if c.w <= 0. || device.x.abs() > 1. || device.y.abs() > 1. || device.z > 1. {
                        continue;
                    }
                    seen += 1;
                    let index = cascades
                        .iter()
                        .position(|cascade| depth < f64::from(cascade.far_bound))
                        .unwrap_or(cascades.len() - 1);
                    assert!(
                        holds(&cascades[index], world),
                        "{count} cascades, light {direction:?}: cascade {index} misses a point at depth {depth}"
                    );
                    let overlap = (1. - f64::from(SHADOW_CASCADE_OVERLAP))
                        * f64::from(cascades[index].far_bound);
                    if index + 1 < cascades.len() && depth >= overlap {
                        assert!(
                            holds(&cascades[index + 1], world),
                            "{count} cascades: cascade {} misses a blended point at depth {depth}",
                            index + 1
                        );
                    }
                }
                let last = cascades.last().unwrap().far_bound;
                assert!(
                    (last - shadow.distance).abs() < 1e-3,
                    "the last cascade ends at {last}, not the shadow distance"
                );
            }
        }
    }
}

// A probe capture selects the first cascade that holds a position. Plausible
// defects: a cascade cube smaller than what the faces see out to its bound,
// or bounds that stop short of the distance. The oracle draws points in the
// cube around the capture's centre and checks that the first cascade whose
// bound exceeds the point's distance along every axis holds it.
#[wasm_bindgen_test(unsupported = test)]
fn every_point_a_capture_sees_within_the_shadow_distance_falls_inside_its_cascade() {
    let center = Vec3::new(-41., 3.5, 812.);
    let mut sequence = Sequence(3);
    for direction in [Vec3::new(0.6, -1., -0.4), Vec3::NEG_Y] {
        let cascades = Cascades::capture(center, direction, &shadow(4), MAP).unwrap();
        let cascades = cascades.as_slice();
        for _ in 0..4000 {
            let offset = DVec3::new(
                sequence.between(-200., 200.),
                sequence.between(-200., 200.),
                sequence.between(-200., 200.),
            );
            let reach = offset.abs().max_element();
            let index = cascades
                .iter()
                .position(|cascade| reach < f64::from(cascade.far_bound))
                .unwrap();
            assert!(
                holds(&cascades[index], center.as_dvec3() + offset),
                "cascade {index} misses a point {reach} m out"
            );
        }
    }
}

// Godot places the cascades' far bounds
// (`_light_instance_setup_directional_shadow` with `DirectionalLight3D`'s
// default splits of 0.1, 0.2 and 0.5): each but the last ends that share of the way from the camera's near plane
// to the shadow's distance, and the last at the distance; 2 and 3 cascades
// take the first one and two shares. Plausible defects: shares measured from
// the view origin rather than the near plane, the wrong shares for fewer
// cascades (the last ones, or rescaled), or the last cascade stopping short
// of or beyond the distance. The oracle is Godot's formula over its default
// shares, worked by hand: a camera whose near plane is 0.3 m with a 150.3 m
// shadow (a 150 m range), and a probe capture, whose cascades start at its
// centre, with a 40 m one.
#[wasm_bindgen_test(unsupported = test)]
fn cascade_bounds_are_godots_splits_of_the_range_from_the_near_plane() {
    let view = camera::rh::view::look_to_mat4(Vec3::new(3., 2., 1.), Vec3::NEG_Z, Vec3::Y);
    let projection = crate::perspective(1.1, 16. / 9., 0.3);
    let camera: [&[f32]; 4] = [
        &[150.3],
        &[15.3, 150.3],
        &[15.3, 30.3, 150.3],
        &[15.3, 30.3, 75.3, 150.3],
    ];
    let capture: [&[f32]; 4] = [&[40.], &[4., 40.], &[4., 8., 40.], &[4., 8., 20., 40.]];
    for (count, (camera, capture)) in (1..=4u32).zip(camera.into_iter().zip(capture)) {
        let fits = [
            (
                "camera",
                camera,
                Cascades::camera(
                    view,
                    projection,
                    Vec3::NEG_Y,
                    &DirectionalShadow {
                        distance: 150.3,
                        cascades: count,
                    },
                    MAP,
                ),
            ),
            (
                "capture",
                capture,
                Cascades::capture(
                    Vec3::new(5., 1., -3.),
                    Vec3::NEG_Y,
                    &DirectionalShadow {
                        distance: 40.,
                        cascades: count,
                    },
                    MAP,
                ),
            ),
        ];
        for (label, expected, cascades) in fits {
            let bounds: Vec<f32> = cascades
                .unwrap()
                .as_slice()
                .iter()
                .map(|cascade| cascade.far_bound)
                .collect();
            assert!(
                bounds.len() == expected.len()
                    && bounds
                        .iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 1e-3),
                "{label}, {count} cascades: bounds {bounds:?}, not {expected:?}"
            );
        }
    }
}
