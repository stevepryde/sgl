//! Volume paths (`scene_volume_path`, D-41) observed at real boundaries:
//! frames rendered, submitted and finished through `Renderer::render`, and
//! their composite read back. The fixture shader
//! (`test_support::VOLUME_SHADER`) writes its path's length, its bound and
//! its side into an unlit blended box's emission, which the composite holds
//! as written over black, showing one side of each box and hiding the other.
//! Each oracle is the test's own closed boxes, intersected on the CPU with
//! each pixel's ray through the test's camera, at pixels whose ray crosses
//! every face it meets clear of the faces' edges.
use super::shader_tests::{Harness, SIZE, frame, ray, settings, texel, wall};
use crate::asset::{self, Vertex};
use crate::settings::Settings;
use crate::test_support::{self, VOLUME_SHADER, VolumeShaderParams};
use crate::{
    AlphaMode, Camera, InstanceId, InstanceState, MaterialId, Mobility, ModelMesh, PreparedModel,
    Scene, ShaderId, ShaderSource,
};
use glam::{DVec3, Mat4, Quat, Vec3};

/// The contract's bounds (shader_contract.wgsl), as the fixture writes them.
const NONE: f32 = 0.;
const EXIT: f32 = 1.;
const OPAQUE: f32 = 2.;
const HIDDEN: f32 = 3.;
const ENTRY: f32 = 4.;
const EYE: f32 = 5.;

/// How far from a face's edges a pixel's ray must cross it to be checked.
const MARGIN: f64 = 0.03;

/// A closed box, its centre and half extents in its mesh's units, at a
/// rigid pose.
#[derive(Clone, Copy)]
struct Cuboid {
    center: Vec3,
    half: Vec3,
    pose: Mat4,
}

impl Cuboid {
    fn new(center: [f32; 3], half: [f32; 3]) -> Self {
        Self {
            center: Vec3::from_array(center),
            half: Vec3::from_array(half),
            pose: Mat4::IDENTITY,
        }
    }

    /// Its twelve triangles, wound counter-clockwise seen from outside.
    fn mesh(self) -> (Vec<Vertex>, Vec<u32>) {
        let cube = test_support::cube();
        let mesh = &cube.meshes[0];
        let vertices = mesh
            .vertices
            .iter()
            .map(|vertex| Vertex {
                position: (self.center + Vec3::from_array(vertex.position) * 2. * self.half)
                    .to_array(),
                ..*vertex
            })
            .collect();
        (vertices, mesh.indices.clone())
    }

    /// The box moved along `by` in its mesh's units, as the fixture's
    /// vertex function moves it before the pose.
    fn moved(self, by: Vec3) -> Self {
        Self {
            center: self.center + by,
            ..self
        }
    }

    /// Where `ray` (a world origin and unit direction) enters and leaves
    /// it, as distances along the ray from its origin, where it crosses it
    /// with every face it meets crossed clear of that face's edges by
    /// `margin`; none where it misses it or passes nearer an edge.
    fn crossing(self, (origin, direction): (DVec3, DVec3), margin: f64) -> Option<(f64, f64)> {
        let local = self.pose.as_dmat4().inverse();
        let origin = local.transform_point3(origin);
        let direction = local.transform_vector3(direction);
        let (center, half) = (self.center.as_dvec3(), self.half.as_dvec3());
        let (mut near, mut far) = (f64::NEG_INFINITY, f64::INFINITY);
        for axis in 0..3 {
            if direction[axis].abs() < 1e-12 {
                if (origin[axis] - center[axis]).abs() >= half[axis] {
                    return None;
                }
                continue;
            }
            let a = (center[axis] - half[axis] - origin[axis]) / direction[axis];
            let b = (center[axis] + half[axis] - origin[axis]) / direction[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        if near >= far || far <= 0. {
            return None;
        }
        // A point on a face lies at its half extent along one axis alone,
        // and within the others' by the margin.
        let clear = |t: f64| {
            let point = origin + direction * t - center;
            (0..3)
                .filter(|&axis| point[axis].abs() > half[axis] - margin)
                .count()
                <= 1
        };
        (clear(far) && (near <= 0. || clear(near))).then_some((near, far))
    }

    /// Whether `ray` passes within `margin` of it.
    fn near(self, ray: (DVec3, DVec3), margin: f64) -> bool {
        let grown = Self {
            half: self.half + Vec3::splat(margin as f32),
            ..self
        };
        grown.crossing(ray, -1.).is_some()
    }
}

/// A camera at `eye` looking along -Z.
fn camera_at(eye: Vec3) -> Camera {
    Camera {
        view: Mat4::from_translation(-eye),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    }
}

/// Unlit black: the composite holds a fixture box's emission as written.
fn black() -> asset::Material {
    asset::Material {
        base: [0., 0., 0., 1.],
        unlit: true,
        double_sided: true,
        ..Default::default()
    }
}

/// `VOLUME_SHADER` added to `scene`.
fn add_volume_shader(scene: &mut Scene) -> ShaderId {
    scene
        .add_shader(ShaderSource {
            wgsl: VOLUME_SHADER.into(),
            label: "volume shader".into(),
        })
        .unwrap_or_else(|error| panic!("{error}"))
}

/// A blended material through `VOLUME_SHADER` with `params`, whose
/// vertices move at most `bound`.
fn volume(
    harness: &mut Harness,
    shader: ShaderId,
    params: VolumeShaderParams,
    bound: f32,
) -> MaterialId {
    let material = harness.shaded(
        asset::Material {
            alpha: AlphaMode::Blend {
                receives_screen_space_reflections: false,
                keeps_specular: false,
            },
            ..black()
        },
        shader,
        bound,
    );
    harness
        .scene
        .set_shader_parameters(&harness.queue, material, bytemuck::bytes_of(&params))
        .unwrap();
    material
}

/// An instance of `cuboid` drawn with `material`, its shader data `shown`
/// (its front where x is positive, its back where y is).
fn place_box(
    harness: &mut Harness,
    cuboid: Cuboid,
    material: MaterialId,
    shown: [f32; 2],
) -> InstanceId {
    let (device, queue) = (&harness.device, &harness.queue);
    let (vertices, indices) = cuboid.mesh();
    let mesh = ModelMesh {
        vertices,
        indices,
        material,
        deformation: Default::default(),
    };
    let prepared = PreparedModel::with_shader_data(vec![mesh], vec![Vec::new()]).unwrap();
    let model = harness.scene.add_model(device, queue, prepared).unwrap();
    let state = InstanceState {
        pose: cuboid.pose,
        ..InstanceState::new(model)
    };
    let id = harness
        .scene
        .add_instance(device, queue, state, Mobility::Static)
        .unwrap();
    harness
        .scene
        .set_instance_shader_data(queue, id, [shown[0], shown[1], 0., 0.])
        .unwrap();
    id
}

/// An opaque unlit black wall facing the camera at depth `z`.
fn place_wall(harness: &mut Harness, z: f32) -> InstanceId {
    let material = harness.material(black());
    harness.place(wall([-40., 40.], [-40., 40.], z), material, [0.; 4])
}

/// What a pixel's fixture fragment should write: its path's length, its
/// bound and whether it is the front.
type Expected = Option<(f64, f32, bool)>;

/// Asserts the path the composite holds at each pixel `expected` gives one
/// for: its length within 1 % and the composite's half-float precision, its
/// bound and its side exactly. Returns how many pixels it checked.
fn check(composite: &[[f32; 4]], label: &str, expected: impl Fn([u32; 2]) -> Expected) -> usize {
    let mut checked = 0;
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let pixel = [x, y];
            let Some((length, bound, front)) = expected(pixel) else {
                continue;
            };
            let [observed, observed_bound, side, _] = texel(composite, pixel);
            let tolerance = 0.01 * length + 2e-3;
            assert!(
                (f64::from(observed) - length).abs() <= tolerance
                    && observed_bound == bound
                    && side == f32::from(u8::from(front)),
                "{label} {pixel:?}: length {observed}, bound {observed_bound}, front {side}; \
                 expected {length}, {bound}, {front}"
            );
            checked += 1;
        }
    }
    checked
}

// Plausible defects: the exit layer not drawn, drawn from the front faces or
// culled by the material's side, so a front fragment measures to the opaque
// surface behind its volume, or to its own face; a length in view depth
// rather than along the ray, or in device depth; the layers drawn without
// the material's vertex function, so they stay where the mesh rests while
// the blended draw moves. The oracle is a thin box turned about Y in front
// of a wall, intersected with each pixel's ray: its front fragments report
// the box's own along-ray thickness, bounded by its exit, with the wall at
// 9 m and again at 15 m; then, with the vertex function moving the box 1.5
// m along its turned x, the displaced box's thickness, at pixels among
// which some lie off the box at rest.
#[test]
fn a_front_fragment_measures_to_its_exit_whatever_lies_behind() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    if !harness.renderer.volume_paths_in_effect(&harness.settings) {
        eprintln!("skipping: the Basic tier measures no volume path");
        return;
    }
    let shader = add_volume_shader(&mut harness.scene);
    let still = VolumeShaderParams {
        direction: [1., 0., 0.],
        lift: 0.,
    };
    let material = volume(&mut harness, shader, still, 2.);
    let thin = Cuboid {
        pose: Mat4::from_translation(Vec3::new(0., 0., -5.)) * Mat4::from_rotation_y(0.5),
        ..Cuboid::new([0., 0., 0.], [1.2, 1.2, 0.3])
    };
    place_box(&mut harness, thin, material, [1., 0.]);
    let camera = camera_at(Vec3::ZERO);
    let thickness = |cuboid: Cuboid| {
        move |pixel| {
            cuboid
                .crossing(ray(&camera, pixel), MARGIN)
                .map(|(near, far)| (far - near, EXIT, true))
        }
    };
    let mut wall = place_wall(&mut harness, -9.);
    for depth in [9., 15.] {
        if depth != 9. {
            harness.scene.remove_instance(wall).unwrap();
            wall = place_wall(&mut harness, -depth);
        }
        harness.frame(&frame(camera, 0.));
        let checked = check(
            &harness.composite(),
            &format!("the wall at {depth} m"),
            thickness(thin),
        );
        assert!(checked >= 60, "the wall at {depth} m: {checked} pixels");
    }
    let lift = 1.5;
    harness
        .scene
        .set_shader_parameters(
            &harness.queue,
            material,
            bytemuck::bytes_of(&VolumeShaderParams { lift, ..still }),
        )
        .unwrap();
    harness.frame(&frame(camera, 0.));
    let moved = thin.moved(Vec3::X * lift);
    let checked = check(&harness.composite(), "displaced", thickness(moved));
    let off_rest = (0..SIZE[0] * SIZE[1])
        .map(|index| [index % SIZE[0], index / SIZE[0]])
        .filter(|&pixel| {
            let ray = ray(&camera, pixel);
            moved.crossing(ray, MARGIN).is_some() && !thin.near(ray, MARGIN)
        })
        .count();
    assert!(
        checked >= 60 && off_rest >= 20,
        "displaced: {checked} pixels, {off_rest} off the box at rest"
    );
}

// Plausible defects: the layers drawn without the opaque depth's test, so an
// exit hidden behind an opaque surface inside the volume bounds the path; a
// missing layer reported as an exit at the opaque depth (EXIT where the
// contract says OPAQUE). The oracle is an opaque cube inside a thick box:
// front fragments over the cube report the distance from the box's face to
// the cube's along each pixel's ray, bounded by the opaque surface; those
// beside it the box's thickness, bounded by its exit.
#[test]
fn an_opaque_surface_inside_the_volume_bounds_its_path() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    if !harness.renderer.volume_paths_in_effect(&harness.settings) {
        eprintln!("skipping: the Basic tier measures no volume path");
        return;
    }
    let shader = add_volume_shader(&mut harness.scene);
    let material = volume(&mut harness, shader, VolumeShaderParams::default(), 0.);
    let outer = Cuboid::new([0., 0., -6.], [2., 2., 2.]);
    let inner = Cuboid::new([0.2, -0.1, -6.], [0.7, 0.7, 0.7]);
    place_box(&mut harness, outer, material, [1., 0.]);
    let opaque = harness.material(black());
    let (vertices, indices) = inner.mesh();
    let (device, queue) = (&harness.device, &harness.queue);
    let model = harness
        .scene
        .add_model(
            device,
            queue,
            PreparedModel::new(vec![ModelMesh {
                vertices,
                indices,
                material: opaque,
                deformation: Default::default(),
            }])
            .unwrap(),
        )
        .unwrap();
    harness
        .scene
        .add_instance(device, queue, InstanceState::new(model), Mobility::Static)
        .unwrap();
    let camera = camera_at(Vec3::ZERO);
    harness.frame(&frame(camera, 0.));
    let composite = harness.composite();
    let at_inner = check(&composite, "over the opaque cube", |pixel| {
        let ray = ray(&camera, pixel);
        let (entry, _) = outer.crossing(ray, MARGIN)?;
        let (inside, _) = inner.crossing(ray, MARGIN)?;
        Some((inside - entry, OPAQUE, true))
    });
    let beside = check(&composite, "beside the opaque cube", |pixel| {
        let ray = ray(&camera, pixel);
        let (entry, exit) = outer.crossing(ray, MARGIN)?;
        (!inner.near(ray, MARGIN)).then_some((exit - entry, EXIT, true))
    });
    assert!(
        at_inner >= 20 && beside >= 40,
        "{at_inner} pixels over the cube, {beside} beside it"
    );
}

// Plausible defects: the second exit layer not peeled behind the first (it
// holds the first's faces again, or nothing), so a volume behind another's
// far side measures to the opaque surface or reports the other's exit; a
// third crossing given the opaque bound unmarked. The oracle is three boxes
// along the camera's axis, the two nearer hidden: the far box's front
// fragments report its own thickness, bounded by an exit, behind the middle
// box alone (the second exit layer's) and beside both (the exit layer's),
// and, behind both nearer boxes, the distance to the wall behind it,
// marked hidden.
#[test]
fn a_volume_behind_another_measures_its_own_exit() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    if !harness.renderer.volume_paths_in_effect(&harness.settings) {
        eprintln!("skipping: the Basic tier measures no volume path");
        return;
    }
    let shader = add_volume_shader(&mut harness.scene);
    let material = volume(&mut harness, shader, VolumeShaderParams::default(), 0.);
    let near = Cuboid::new([0.1, 0., -3.], [0.5, 0.5, 0.25]);
    let middle = Cuboid::new([0., 0.1, -5.], [1.4, 1.4, 0.3]);
    let far = Cuboid::new([0., 0., -8.], [3.5, 3.5, 0.5]);
    place_box(&mut harness, near, material, [0., 0.]);
    place_box(&mut harness, middle, material, [0., 0.]);
    place_box(&mut harness, far, material, [1., 0.]);
    place_wall(&mut harness, -12.);
    let camera = camera_at(Vec3::ZERO);
    harness.frame(&frame(camera, 0.));
    let composite = harness.composite();
    let counted =
        [(true, true), (false, true), (false, false)].map(|(behind_near, behind_middle)| {
            let label =
                format!("behind the near box {behind_near}, the middle box {behind_middle}");
            check(&composite, &label, |pixel| {
                let ray = ray(&camera, pixel);
                let crosses = |cuboid: Cuboid| {
                    if cuboid.crossing(ray, MARGIN).is_some() {
                        Some(true)
                    } else if cuboid.near(ray, MARGIN) {
                        None
                    } else {
                        Some(false)
                    }
                };
                if crosses(near)? != behind_near || crosses(middle)? != behind_middle {
                    return None;
                }
                let (entry, exit) = far.crossing(ray, MARGIN)?;
                if behind_near {
                    // To the wall at z = -12 along the ray.
                    let to_wall = (-12. - ray.0.z) / ray.1.z;
                    Some((to_wall - entry, HIDDEN, true))
                } else {
                    Some((exit - entry, EXIT, true))
                }
            })
        });
    assert!(
        counted.iter().all(|&count| count >= 15),
        "pixels behind both nearer boxes, the middle alone and neither: {counted:?}"
    );
}

// Plausible defects: the entry layer not drawn, or drawn from the back
// faces, so a back fragment seen from inside its volume measures from the
// eye, or from its own face; an eye length in view depth rather than along
// the ray. The oracle is a box seen from outside, its back fragments
// alone shown, which report its along-ray thickness, bounded by its entry;
// and from inside, which report the distance from the eye to where each
// pixel's ray leaves it, bounded by the eye.
#[test]
fn a_back_fragment_measures_from_its_entry_or_the_eye() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    if !harness.renderer.volume_paths_in_effect(&harness.settings) {
        eprintln!("skipping: the Basic tier measures no volume path");
        return;
    }
    let shader = add_volume_shader(&mut harness.scene);
    let material = volume(&mut harness, shader, VolumeShaderParams::default(), 0.);
    let cuboid = Cuboid {
        pose: Mat4::from_translation(Vec3::new(0., 0., -5.)) * Mat4::from_rotation_y(0.3),
        ..Cuboid::new([0., 0., 0.], [2., 2., 2.])
    };
    place_box(&mut harness, cuboid, material, [0., 1.]);
    place_wall(&mut harness, -14.);
    let outside = camera_at(Vec3::ZERO);
    harness.frame(&frame(outside, 0.));
    let from_entry = check(&harness.composite(), "from outside", |pixel| {
        let (entry, exit) = cuboid.crossing(ray(&outside, pixel), MARGIN)?;
        Some((exit - entry, ENTRY, false))
    });
    let eye = Vec3::new(0.4, -0.3, -4.2);
    let inside = camera_at(eye);
    harness.frame(&frame(inside, 0.));
    let from_eye = check(&harness.composite(), "from inside", |pixel| {
        let ray = ray(&inside, pixel);
        let (_, exit) = cuboid.crossing(ray, MARGIN)?;
        Some(((ray.0 + ray.1 * exit).distance(eye.as_dvec3()), EYE, false))
    });
    assert!(
        from_entry >= 60 && from_eye >= 200,
        "{from_entry} pixels from outside, {from_eye} from inside"
    );
}

// Plausible defects: the layers bound or measured where the binding tier's
// sampled textures are spent, or while the setting is off, so a shader
// reads a stale or garbage path where the contract promises none; the
// setting's resolution misreported. The oracle is the contract: on the
// Extended tier with `Settings::volume_paths` off, and on the Basic tier
// (a device of WebGPU's default limits) with it on, the box's front
// fragments report no path (VOLUME_NONE, length 0) and the renderer reports
// volume paths out of effect; on the Extended tier with it on, in effect.
#[test]
fn no_path_is_measured_on_the_basic_tier_or_with_the_setting_off() {
    let Some(adapter) = test_support::adapter() else {
        return;
    };
    let basic = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .unwrap();
    let off = Settings {
        volume_paths: false,
        ..settings()
    };
    for (label, device, settings) in [
        ("Extended, off", test_support::device(), off),
        ("Basic, on", Some(basic), settings()),
    ] {
        let Some(device) = device else {
            return;
        };
        let mut harness = Harness::on(device, settings);
        let extended =
            harness.renderer.binding_tier() == crate::graphics_device::BindingTier::Extended;
        assert_eq!(extended, label.starts_with("Extended"), "{label}: the tier");
        assert!(
            !harness.renderer.volume_paths_in_effect(&harness.settings),
            "{label}: volume paths in effect"
        );
        if extended {
            assert!(
                harness
                    .renderer
                    .volume_paths_in_effect(&super::shader_tests::settings()),
                "{label}: volume paths out of effect with the setting on"
            );
        }
        let shader = add_volume_shader(&mut harness.scene);
        let material = volume(&mut harness, shader, VolumeShaderParams::default(), 0.);
        let cuboid = Cuboid::new([0., 0., -5.], [1.5, 1.5, 0.5]);
        place_box(&mut harness, cuboid, material, [1., 0.]);
        place_wall(&mut harness, -9.);
        let camera = camera_at(Vec3::ZERO);
        harness.frame(&frame(camera, 0.));
        let checked = check(&harness.composite(), label, |pixel| {
            cuboid
                .crossing(ray(&camera, pixel), MARGIN)
                .map(|_| (0., NONE, true))
        });
        assert!(checked >= 60, "{label}: {checked} pixels");
    }
}

// Plausible defects: the volume layers drawn in frames that show no
// material whose shader reads its volume path, or while the setting is off,
// a cost D-41 rules out; or not drawn in one that shows one. The oracle is
// the requirement, observed in the GPU timings of the frames' passes: a
// frame facing a blended box whose shader reads its path times `volume
// layers` passes, the same scene seen the other way, facing a blended box
// whose shader reads none, times none, and so does the first view with the
// setting off.
#[test]
fn volume_layers_are_drawn_only_for_a_shader_that_reads_them() {
    let Some(gpu) = test_support::device_choosing(|adapter| {
        crate::graphics_device::features(adapter)
            | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
    }) else {
        return;
    };
    let mut harness = Harness::on(gpu, settings());
    let Some(mut timing) = crate::timing::GpuTiming::new(&harness.device, &harness.queue) else {
        eprintln!("skipping: the adapter has no timestamp queries");
        return;
    };
    if !harness.renderer.volume_paths_in_effect(&harness.settings) {
        eprintln!("skipping: the Basic tier measures no volume path");
        return;
    }
    let reading = add_volume_shader(&mut harness.scene);
    let reading = volume(&mut harness, reading, VolumeShaderParams::default(), 0.);
    let other = test_support::add_test_shader(&mut harness.scene);
    let other = harness.shaded(
        asset::Material {
            alpha: AlphaMode::Blend {
                receives_screen_space_reflections: false,
                keeps_specular: false,
            },
            ..black()
        },
        other,
        0.,
    );
    place_box(
        &mut harness,
        Cuboid::new([0., 0., -5.], [1., 1., 1.]),
        reading,
        [1., 0.],
    );
    place_box(
        &mut harness,
        Cuboid::new([0., 0., 5.], [1., 1., 1.]),
        other,
        [1., 0.],
    );
    // Whether a frame facing `facing` with the setting `on` timed a `volume
    // layers` pass: its timings are read back once it completes, a resolve
    // and a map later, within a few polls.
    let mut drawn = |facing: Quat, on: bool| {
        let settings = Settings {
            volume_paths: on,
            ..harness.settings
        };
        let input = frame(
            Camera {
                view: Mat4::from_quat(facing),
                ..camera_at(Vec3::ZERO)
            },
            0.,
        );
        let (device, queue) = (&harness.device, &harness.queue);
        let _ = timing.begin_frame(device, queue);
        let mut encoder = device.create_command_encoder(&Default::default());
        harness.renderer.render(
            device,
            queue,
            &mut encoder,
            &mut harness.scene,
            &input,
            &settings,
            &harness.output,
            Some(&timing),
        );
        queue.submit([encoder.finish()]);
        timing.submitted(queue);
        harness.renderer.finish_frame(&mut harness.scene);
        for _ in 0..8 {
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            if let Some(frame) = timing.begin_frame(device, queue).next_back() {
                return frame.passes.iter().any(|pass| pass.name == "volume layers");
            }
        }
        panic!("the frame's timings did not complete");
    };
    let away = Quat::from_rotation_y(std::f32::consts::PI);
    for (label, facing, on, expected) in [
        ("facing the reading box", Quat::IDENTITY, true, true),
        ("facing the other box", away, true, false),
        ("with the setting off", Quat::IDENTITY, false, false),
        ("facing the reading box again", Quat::IDENTITY, true, true),
    ] {
        assert_eq!(drawn(facing, on), expected, "{label}");
    }
}
