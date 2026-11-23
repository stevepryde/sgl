//! Contact shadows observed in real frames: the contact mask read back
//! against the fixture's geometry, projected through its camera toward its
//! light, and the lighting pass taking the mask. The stage keeps no
//! history, so each frame is its own: a camera cut, motion or an instance's
//! departure cannot reach it.
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, ContactShadows, Settings};
use crate::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, FrameInput, InstanceState, Mobility,
    Scene, test_support,
};
use glam::{DVec2, DVec3, DVec4, Mat4, Quat, Vec3};

/// Contact shadows at `level`, with nothing else that changes the frame
/// from one to the next or reads the lit colour back differently.
fn settings(level: ContactShadows) -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        contact_shadows: level,
        ..Settings::default()
    }
}

/// A floor quad facing +Y at height 0, `half` metres to each side.
fn floor(half: f32) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, z)| Vertex {
                uv1: [0.0; 2],
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * half, 0., z * half],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    asset
}

/// An upright post standing on the floor at the origin: `side` metres
/// square and `height` tall.
#[derive(Clone, Copy)]
struct Post {
    side: f32,
    height: f32,
}

impl Post {
    fn centre(self) -> Vec3 {
        Vec3::new(0., self.height / 2., 0.)
    }

    fn half(self) -> Vec3 {
        Vec3::new(self.side / 2., self.height / 2., self.side / 2.)
    }

    /// Whether the ray from `origin` along `direction` passes through the
    /// post grown by `grow` metres (shrunk where negative): the slab test.
    fn crosses(self, grow: f64, origin: DVec3, direction: DVec3) -> bool {
        let centre = self.centre().as_dvec3();
        let half = self.half().as_dvec3() + grow;
        let (mut near, mut far) = (1e-6_f64, f64::INFINITY);
        for axis in 0..3 {
            let (from, along) = (origin[axis] - centre[axis], direction[axis]);
            if along.abs() < 1e-12 {
                if from.abs() > half[axis] {
                    return false;
                }
                continue;
            }
            let (a, b) = ((-half[axis] - from) / along, (half[axis] - from) / along);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        near < far
    }
}

/// A scene of a floor of `half` metres and `post`, where one stands.
fn scene((device, queue): (&wgpu::Device, &wgpu::Queue), half: f32, post: Option<Post>) -> Scene {
    let mut scene = Scene::new(device, queue);
    test_support::add_static(device, queue, &mut scene, floor(half));
    if let Some(post) = post {
        let cube = scene
            .add_asset(device, queue, test_support::cube())
            .unwrap()
            .model;
        let state = InstanceState {
            pose: Mat4::from_scale_rotation_translation(
                post.half() * 2.,
                Quat::IDENTITY,
                post.centre(),
            ),
            ..InstanceState::new(cube)
        };
        scene
            .add_instance(device, queue, state, Mobility::Static)
            .unwrap();
    }
    scene
}

/// A camera at `eye` looking at `target` over an output of `size`.
fn camera(eye: Vec3, target: Vec3, size: [u32; 2]) -> Camera {
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: crate::perspective(0.9, size[0] as f32 / size[1] as f32, 0.1),
        eye,
    }
}

/// The unit direction toward a light `elevation` radians above the horizon
/// and `azimuth` radians about +Y from +Z.
fn toward(elevation: f32, azimuth: f32) -> Vec3 {
    Vec3::new(
        elevation.cos() * azimuth.sin(),
        elevation.sin(),
        elevation.cos() * azimuth.cos(),
    )
}

/// A frame's input: `camera`, a black backdrop and no ambient light, and a
/// sun shining from `toward_sun` with `shadow` at `opacity`.
fn input(camera: Camera, toward_sun: Vec3, shadow: DirectionalShadow, opacity: f32) -> FrameInput {
    let mut input = FrameInput::new(camera);
    input.backdrop = Backdrop::Color([0.; 3]);
    input.hemisphere_light.intensity = 0.;
    input.diffuse_environment.intensity = 0.;
    input.reflection_environment.intensity = 0.;
    input.directional_lights[0] = Some(DirectionalLight {
        direction: -toward_sun,
        illuminance: 3.,
        shadow: Some(shadow),
        shadow_opacity: opacity,
        angular_diameter: 0.,
        ..DirectionalLight::default()
    });
    input
}

/// What one frame left: its depth, its lit colour's red and the contact
/// mask's red, where the stage ran.
struct Observed {
    size: [u32; 2],
    depth: Vec<f32>,
    red: Vec<f32>,
    mask: Option<Vec<u8>>,
}

impl Observed {
    fn index(&self, [x, y]: [u32; 2]) -> usize {
        (y * self.size[0] + x) as usize
    }

    /// The contact mask's visibility at `pixel`, as a byte.
    fn mask(&self, pixel: [u32; 2]) -> u8 {
        self.mask.as_ref().expect("the contact shadow stage ran")[self.index(pixel) * 4]
    }

    /// The world position the depth puts at the centre of `pixel`, under
    /// `camera`'s unjittered view-projection, in f64; none where nothing
    /// was drawn.
    fn position(&self, camera: &Camera, pixel: [u32; 2]) -> Option<DVec3> {
        let [width, height] = self.size;
        let z = self.depth[self.index(pixel)];
        if z <= 0. {
            return None;
        }
        let inverse = (camera.projection.as_dmat4() * camera.view.as_dmat4()).inverse();
        let uv = [
            (f64::from(pixel[0]) + 0.5) / f64::from(width),
            (f64::from(pixel[1]) + 0.5) / f64::from(height),
        ];
        let h: DVec4 =
            inverse * DVec4::new(uv[0] * 2. - 1., (1. - uv[1]) * 2. - 1., f64::from(z), 1.);
        Some(h.truncate() / h.w)
    }

    /// The floor's world position at `pixel`, where it shows the floor
    /// outside `post` grown by `margin` metres.
    fn floor(&self, camera: &Camera, pixel: [u32; 2], post: Post, margin: f64) -> Option<DVec3> {
        let position = self.position(camera, pixel)?;
        let half = f64::from(post.side) / 2. + margin;
        (position.y.abs() < 1e-3 && (position.x.abs() > half || position.z.abs() > half))
            .then_some(position)
    }
}

/// Where `camera` puts world point `point` on a screen of `size`, in
/// pixels, y down.
fn project(camera: &Camera, size: [u32; 2], point: DVec3) -> DVec2 {
    let clip = camera.projection.as_dmat4() * camera.view.as_dmat4() * point.extend(1.);
    let ndc = clip.truncate().truncate() / clip.w;
    DVec2::new(
        (ndc.x + 1.) / 2. * f64::from(size[0]),
        (1. - ndc.y) / 2. * f64::from(size[1]),
    )
}

/// The direction toward the sun in `camera`'s clip space: its w, whose sign
/// the stage's dispatch takes (rays diverging, converging or, at 0,
/// parallel on screen).
fn clip_w(camera: &Camera, toward_sun: Vec3) -> f64 {
    (camera.projection.as_dmat4() * camera.view.as_dmat4() * toward_sun.as_dvec3().extend(0.)).w
}

/// Renders `input` of `scene` with `settings` and reads what it left.
fn render(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut TestRenderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
) -> Observed {
    let size = renderer.view.render_size();
    let output = crate::view::targets::target(
        device,
        "contact shadow frames",
        size,
        crate::shading::gbuffer::COLOR,
    );
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        input,
        settings,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(scene);
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
    let depth = test_support::read(device, queue, renderer.targets().depth.texture(), 4)
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let red = test_support::read(device, queue, renderer.targets().color.texture(), 8)
        .chunks_exact(8)
        .map(test_support::half)
        .collect();
    let mask = renderer.contact_shadows().map(|mask| {
        assert_eq!(
            [mask.texture().width(), mask.texture().height()],
            size,
            "the contact mask is at the render size"
        );
        test_support::read(device, queue, mask.texture(), 4)
    });
    Observed {
        size,
        depth,
        red,
        mask,
    }
}

/// The pixels along the screen line from `from` in unit `direction`, at
/// whole steps 1 to `steps`, inside the screen.
fn along(from: DVec2, direction: DVec2, steps: u32, size: [u32; 2]) -> Vec<[u32; 2]> {
    (1..=steps)
        .filter_map(|step| {
            let at = from + direction * f64::from(step);
            (at.x >= 0. && at.y >= 0. && at.x < f64::from(size[0]) && at.y < f64::from(size[1]))
                .then_some([at.x as u32, at.y as u32])
        })
        .collect()
}

/// The screen direction, from the post's base, of the floor away from the
/// sun (its shadow's) under `camera`.
fn shadow_direction(camera: &Camera, size: [u32; 2], toward_sun: Vec3) -> (DVec2, DVec2) {
    let away = DVec3::new(-f64::from(toward_sun.x), 0., -f64::from(toward_sun.z)).normalize();
    let base = project(camera, size, DVec3::ZERO);
    let farther = project(camera, size, away * 0.05);
    (base, (farther - base).normalize())
}

/// The post's short contact shadow and its lit side under `camera` and
/// a sun toward `toward_sun`: floor pixels within 8 pixels of the post's
/// base along the screen projection of the floor's direction away from the
/// sun, whose ray toward the sun the post blocks, are shadowed; the
/// mirrored floor pixels, whose ray it leaves clear, are 1. Shadowed is
/// below 0.8: past its first four (hard) tests a pixel averages four
/// buckets of its tests, so a caster the march crosses in one test darkens
/// one bucket, to 0.75 (bend_sss_gpu.h: "so one sample cannot fully show
/// the pixel").
fn assert_contact_shadow(observed: &Observed, camera: &Camera, toward_sun: Vec3, label: &str) {
    let post = POST;
    let sun = toward_sun.as_dvec3();
    let (base, direction) = shadow_direction(camera, observed.size, toward_sun);
    let mut shadowed = 0;
    for pixel in along(base, direction, 8, observed.size) {
        let Some(position) = observed.floor(camera, pixel, post, 2e-3) else {
            continue;
        };
        if !post.crosses(-2e-3, position, sun) {
            continue;
        }
        let mask = observed.mask(pixel);
        assert!(
            mask < 204,
            "{label}: {pixel:?} at {position:?} is in the post's shadow, mask {mask}"
        );
        shadowed += 1;
    }
    let mut lit = 0;
    for pixel in along(base, -direction, 8, observed.size) {
        let Some(position) = observed.floor(camera, pixel, post, 2e-3) else {
            continue;
        };
        if post.crosses(2e-3, position, sun) {
            continue;
        }
        let mask = observed.mask(pixel);
        assert_eq!(
            mask, 255,
            "{label}: {pixel:?} at {position:?} is on the lit side"
        );
        lit += 1;
    }
    assert!(
        shadowed >= 3 && lit >= 3,
        "{label}: {shadowed} shadowed and {lit} lit floor pixels tested"
    );
}

/// The thin post of the short-shadow fixtures: 2 cm square, 30 cm tall.
const POST: Post = Post {
    side: 0.02,
    height: 0.3,
};
const SIZE: [u32; 2] = [256, 192];
/// Close enough that the post is three pixels wide.
const EYE: Vec3 = Vec3::new(0., 0.6, 1.2);

/// The sun ahead of the camera (its rays diverge from its point on screen)
/// and behind it (they converge on it), each shadowing the post's base
/// obliquely on screen.
fn suns(camera: &Camera) -> [(&'static str, Vec3); 2] {
    let elevation = 35_f32.to_radians();
    let suns = [
        ("ahead", toward(elevation, 220_f32.to_radians())),
        ("behind", toward(elevation, 40_f32.to_radians())),
    ];
    // Two branches of the dispatch.
    assert!(clip_w(camera, suns[0].1) > 1e-3);
    assert!(clip_w(camera, suns[1].1) < -1e-3);
    suns
}

// Plausible defects: a ray marched on its own surface (the bilinear
// neighbour's depth taken as a caster, a plane's own slope read as a
// step), a march that strays off its line onto other depths, or reads
// outside the depth taken as near rather than far. The oracle: nothing
// occludes a plane, so with the sun at 30° and at a grazing 5° every floor
// pixel's mask is 1. The camera looks down at 45°, so the floor's depth
// changes by less than the edge threshold (2%) from one pixel to the next
// everywhere it sees, and the low sun stands straight ahead, so no march
// reads an edge across it: those are
// `a_plane_seen_grazing_casts_no_contact_shadow_on_itself` and
// `a_march_out_across_the_frame_s_border_casts_no_contact_shadow`.
#[test]
fn a_plane_casts_no_contact_shadow_on_itself() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = scene(gpu, 20., None);
    let settings = settings(ContactShadows::Long);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let camera = camera(Vec3::new(0., 2.5, 2.5), Vec3::ZERO, SIZE);
    for (elevation, azimuth) in [(30_f32, 200_f32), (5., 180.)] {
        let toward_sun = toward(elevation.to_radians(), azimuth.to_radians());
        let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
        let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
        let mut floor = 0;
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                if observed.position(&camera, [x, y]).is_none() {
                    continue;
                }
                assert_eq!(
                    observed.mask([x, y]),
                    255,
                    "the floor shadows itself at [{x}, {y}] with the sun {elevation}° up"
                );
                floor += 1;
            }
        }
        assert_eq!(
            floor,
            (SIZE[0] * SIZE[1]) as usize,
            "the floor fills the view"
        );
    }
}

/// The floor pixels of `observed` under `camera` whose mask is below 1,
/// with the least mask among them, and the floor pixels in all.
fn shadowed_floor(observed: &Observed, camera: &Camera) -> (usize, u8, usize) {
    let (mut shadowed, mut least, mut floor) = (0, 255, 0);
    for y in 0..observed.size[1] {
        for x in 0..observed.size[0] {
            if observed.position(camera, [x, y]).is_none() {
                continue;
            }
            floor += 1;
            let mask = observed.mask([x, y]);
            if mask < 255 {
                shadowed += 1;
                least = least.min(mask);
            }
        }
    }
    (shadowed, least, floor)
}

// Plausible defect: a plane seen near edge-on, whose depth changes by more
// than the edge threshold (2%) from one row to the next, read as edges
// whose unfiltered depths cast on it: a march across those rows rounds
// each read to a row, and the rounding stands above the ray (Bend's
// header on `IgnoreEdgePixels`: "the edge detect may incorrectly detect
// multiple 'edge' pixels along that flat surface"). As Godot b130438 has
// the method, without that option, these suns shadowed from about 40 to
// about 11,000 of the floor's 64,800 pixels, to a mask of 0. The fixture:
// a level camera 1.6 m above a floor that runs 4 km out, with a 55 degree
// field of view over 270 rows, where the floor's depth changes that much
// a row beyond about 8 m, out to the horizon (as beyond about 33 m at
// 1080 rows). The suns: ahead and to each side, whose marches cross the
// far rows sideways; straight ahead and high ahead, whose marches fan out
// from the sun's point; and low behind, whose marches leave the point
// opposite the sun, on the floor. The oracle: nothing stands on the floor,
// so every floor pixel's mask is 1, and a level camera sees floor in the
// lower half of its view.
#[test]
fn a_plane_seen_grazing_casts_no_contact_shadow_on_itself() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [480, 270];
    let mut scene = scene(gpu, 4000., None);
    let settings = settings(ContactShadows::Long);
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &settings);
    let eye = Vec3::new(0., 1.6, 0.);
    let camera = Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, eye - Vec3::Z, Vec3::Y),
        projection: crate::perspective(55_f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye,
    };
    for (elevation, azimuth) in [
        (20_f32, 120_f32),
        (20., 240.),
        (20., 180.),
        (35., 150.),
        (10., 0.),
        (3., 30.),
    ] {
        let toward_sun = toward(elevation.to_radians(), azimuth.to_radians());
        let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
        let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
        let (shadowed, least, floor) = shadowed_floor(&observed, &camera);
        assert_eq!(
            floor,
            (size[0] * size[1] / 2) as usize,
            "the floor fills the view below the horizon"
        );
        assert_eq!(
            shadowed, 0,
            "the floor shadows itself at {shadowed} pixels, to a mask of {least}, with the sun \
             {elevation}° up and {azimuth}° round"
        );
    }
}

// Plausible defect: the frame's outermost pixels, whose neighbour outside
// the target reads as the far depth, read as edges whose unfiltered depths
// cast on the rows inside them, where a march runs out across the border
// toward a light beyond it. As Godot b130438 has the method these suns
// shadowed from 3 to about 130 floor pixels in the three rows along the
// border, to a mask of 0. The fixture: the floor seen from above at 45
// degrees, where its depth changes by less than the edge threshold between
// rows, so only the border is an edge. The suns: four ahead, whose point on screen is
// above the frame, beside it and over its corners, so the marches of the
// top rows run sideways out across the top border; and one behind, whose
// opposite point is left of the frame at its top row, so the marches of
// the bottom rows run out across the bottom border. The oracle: nothing
// stands on the floor, so every floor pixel's mask is 1.
#[test]
fn a_march_out_across_the_frame_s_border_casts_no_contact_shadow() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = scene(gpu, 20., None);
    let settings = settings(ContactShadows::Long);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let camera = camera(Vec3::new(0., 2.5, 2.5), Vec3::ZERO, SIZE);
    let [width, height] = SIZE.map(f64::from);
    let inverse = (camera.projection.as_dmat4() * camera.view.as_dmat4()).inverse();
    for (point, ahead) in [
        ([-100., -150.], true),
        ([width + 100., -150.], true),
        ([0.5, -150.], true),
        ([-300., -200.], true),
        ([-150., 0.5], false),
    ] {
        // The direction of the view ray through `point`, in pixels: toward
        // the sun where it stands ahead, away from it where behind.
        let at = |depth: f64| {
            let h = inverse
                * DVec4::new(
                    point[0] / width * 2. - 1.,
                    1. - point[1] / height * 2.,
                    depth,
                    1.,
                );
            h.truncate() / h.w
        };
        let through = (at(0.25) - at(0.5)).normalize();
        let toward_sun = if ahead { through } else { -through }.as_vec3();
        assert!(toward_sun.y > 0.05, "the sun stands above the floor");
        assert_eq!(
            clip_w(&camera, toward_sun) > 0.,
            ahead,
            "the sun stands ahead of the camera or behind it"
        );
        let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
        let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
        let (shadowed, least, floor) = shadowed_floor(&observed, &camera);
        assert_eq!(
            floor,
            (SIZE[0] * SIZE[1]) as usize,
            "the floor fills the view"
        );
        assert_eq!(
            shadowed, 0,
            "the floor shadows itself at {shadowed} pixels, to a mask of {least}, with the \
             sun's point at {point:?}"
        );
    }
}

// Plausible defects: the light's screen position or its rays' direction on
// screen taken with y the other way up, with the wrong sign of w (rays
// marched from the light rather than toward it), or with the parallel case
// mishandled, so the shadow falls toward the light or nowhere; waves that
// miss the pixels about the post; a wave's shared reads tested from the
// wrong end. The oracle: the fixture's geometry projected through its
// camera: floor pixels next to the post's base along the projection of
// the direction away from the sun, whose ray toward it the post blocks,
// are shadowed, and the mirrored ones, whose ray is clear, are 1, with the
// sun ahead of the camera and behind it. Square to its view axis, Godot's
// thickness is a tenth as deep, too thin for the post: that case has a
// fixture of its own (`the_parallel_case_shadows_away_from_the_light`).
#[test]
fn a_post_s_contact_shadow_falls_away_from_the_light() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = scene(gpu, 20., Some(POST));
    let settings = settings(ContactShadows::Medium);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let camera = camera(EYE, Vec3::ZERO, SIZE);
    for (label, toward_sun) in suns(&camera) {
        let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
        let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
        assert_contact_shadow(&observed, &camera, toward_sun, label);
    }
}

// Plausible defects: with the sun square to the view axis, whose rays are
// parallel on screen (Godot's `organise_groups_orthogonal`), the rays'
// direction on screen taken with y the other way up or reversed, the waves
// laid out across the light rather than along it, or the dispatch's bounds
// leaving pixels out. Godot takes a tenth of the surface thickness there
// ("10x is used because it looked best visually"), so a caster shadows
// only within about 0.2% of the receiver's depth: here a plate 1 mm proud
// of a wall facing the camera. The oracle: the fixture's geometry
// projected through its camera: wall pixels whose ray toward the sun the
// plate blocks within 50 pixels are shadowed, and those whose ray it
// leaves clear are 1.
#[test]
fn the_parallel_case_shadows_away_from_the_light() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    let mut wall = floor(1.);
    for vertex in &mut wall.meshes[0].vertices {
        let [x, _, z] = vertex.position;
        vertex.position = [x, -z, -1.];
        vertex.normal = [0., 0., 1.];
    }
    wall.materials[0].double_sided = true;
    test_support::add_static(&device, &queue, &mut scene, wall);
    let plate = Post {
        side: 0.06,
        height: 0.001,
    };
    // The plate as a post turned to lie on the wall: its height along +Z.
    let centre = Vec3::new(0., 0., -1. + plate.height / 2.);
    let half = Vec3::new(plate.side / 2., plate.side / 2., plate.height / 2.);
    let cube = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let state = InstanceState {
        pose: Mat4::from_scale_rotation_translation(half * 2., Quat::IDENTITY, centre),
        ..InstanceState::new(cube)
    };
    scene
        .add_instance(&device, &queue, state, Mobility::Static)
        .unwrap();
    let settings = settings(ContactShadows::Medium);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let camera = camera(Vec3::new(0., 0., 0.3), Vec3::new(0., 0., -1.), SIZE);
    let toward_sun = Vec3::new(1., 0.6, 0.).normalize();
    assert_eq!(clip_w(&camera, toward_sun), 0., "rays parallel on screen");
    let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
    let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
    let sun = toward_sun.as_dvec3();
    // The plate's slab test, grown or shrunk by `grow` metres in the
    // wall's plane: the distance along the ray to it, where it meets it.
    let meets = |position: DVec3, grow: f64| -> Option<f64> {
        let low = (centre - half).as_dvec3() - DVec3::new(grow, grow, 0.);
        let high = (centre + half).as_dvec3() + DVec3::new(grow, grow, 0.);
        let (mut near, mut far) = (1e-6_f64, f64::INFINITY);
        for axis in 0..3 {
            let (from, along) = (position[axis], sun[axis]);
            if along.abs() < 1e-12 {
                if from < low[axis] || from > high[axis] {
                    return None;
                }
                continue;
            }
            let (a, b) = ((low[axis] - from) / along, (high[axis] - from) / along);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        (near < far).then_some(near)
    };
    let (mut shadowed, mut lit) = (0, 0);
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let Some(position) = observed.position(&camera, [x, y]) else {
                continue;
            };
            // The wall, beside the plate.
            if (position.z + 1.).abs() > 1e-4 {
                continue;
            }
            let pixel = DVec2::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            let mask = observed.mask([x, y]);
            if let Some(t) = meets(position, -0.01) {
                let reach = (project(&camera, SIZE, position + sun * t) - pixel)
                    .abs()
                    .max_element();
                if reach <= 50. {
                    assert!(
                        mask < 128,
                        "[{x}, {y}] {reach:.0} pixels from the plate: {mask}"
                    );
                    shadowed += 1;
                }
            } else if meets(position, 0.015).is_none() {
                assert_eq!(mask, 255, "[{x}, {y}] is clear of the plate");
                lit += 1;
            }
        }
    }
    assert!(
        shadowed >= 100 && lit > 10_000,
        "{shadowed} shadowed and {lit} lit wall pixels tested"
    );
}

// Plausible defects: the levels' read steps swapped or all alike, the
// reach counted in other units than render pixels, or the shared reads cut
// short. The oracle: the levels' reaches, 63, 127 and 191 pixels, against
// the fixture's projection: from floor pixels inside a 1 m post's shadow (a
// bollard 10 cm square) by a pixel under a grazing sun, whose ray toward
// the sun meets the post between 80 and 115 pixels away along the march's
// major axis, Medium and Long find it (below 0.8, as `assert_contact_shadow`
// takes shadowed) and Short does not; between 135 and 180 pixels, short of
// Long's fading tests, Long finds it and Medium and Short do not.
#[test]
fn a_longer_reach_shadows_farther() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let post = Post {
        side: 0.1,
        height: 1.,
    };
    let size = [320, 240];
    let mut scene = scene(gpu, 20., Some(post));
    // The camera above the side of a shadow running along -X, which it sees
    // a few pixels wide.
    let camera = camera(Vec3::new(-1.2, 2.5, 2.5), Vec3::new(-1.2, 0., 0.), size);
    let toward_sun = toward(12_f32.to_radians(), 90_f32.to_radians());
    let sun = toward_sun.as_dvec3();
    let mut renderer =
        TestRenderer::for_test(&device, &queue, size, &settings(ContactShadows::Off));
    let mut frames = Vec::new();
    for level in [
        ContactShadows::Short,
        ContactShadows::Medium,
        ContactShadows::Long,
    ] {
        let settings = settings(level);
        let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
        frames.push((
            level,
            render(gpu, &mut renderer, &mut scene, &input, &settings),
        ));
    }
    let observed = &frames[0].1;
    // Whether the floor at `pixel`'s ray toward the sun meets the post.
    let shadowed = |[x, y]: [u32; 2]| {
        observed
            .floor(&camera, [x, y], post, 0.05)
            .is_some_and(|position| post.crosses(-2e-3, position, sun))
    };
    // Floor pixels whose ray toward the sun meets the post, as do their
    // eight neighbours', away from the edges of the shadow, along which the
    // march's rounding to a pixel steps (Bend's aliasing), by how far along
    // the march's major axis on screen it meets it.
    let mut tested = [0; 2];
    for y in 1..size[1] - 1 {
        for x in 1..size[0] - 1 {
            let interior = (y - 1..=y + 1).all(|ny| (x - 1..=x + 1).all(|nx| shadowed([nx, ny])));
            if !interior {
                continue;
            }
            let position = observed.position(&camera, [x, y]).unwrap();
            // Where it meets the post's face toward it, the plane x = -side/2.
            let t = (-f64::from(post.side) / 2. - position.x) / sun.x;
            let hit = project(&camera, size, position + sun * t);
            let pixel = DVec2::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            let reach = (hit - pixel).abs().max_element();
            // The levels that find the post at this reach.
            let finds: &[ContactShadows] = if (80. ..=115.).contains(&reach) {
                tested[0] += 1;
                &[ContactShadows::Medium, ContactShadows::Long]
            } else if (135. ..=180.).contains(&reach) {
                tested[1] += 1;
                &[ContactShadows::Long]
            } else {
                continue;
            };
            for (level, frame) in &frames {
                let mask = frame.mask([x, y]);
                if finds.contains(level) {
                    assert!(
                        mask < 204,
                        "{level:?} misses the post {reach:.0} pixels from [{x}, {y}]: {mask}"
                    );
                } else {
                    assert_eq!(
                        mask, 255,
                        "{level:?} reaches the post {reach:.0} pixels from [{x}, {y}]"
                    );
                }
            }
        }
    }
    assert!(
        tested.iter().all(|&count| count >= 10),
        "{tested:?} floor pixels tested at each reach"
    );
}

// Plausible defects: the lighting pass ignores the mask, takes it at full
// strength whatever the light's shadow opacity, multiplies it in after the
// opacity blend, applies it where the frame has no contact mask, or reads
// another pixel's; the contact-shadowed lighting pass shades otherwise than
// the fused pass. The oracle is mix(1, visibility, opacity) and the two
// opaque forms writing the same targets: under cascades too coarse to hold
// a 2 cm post, with no other light, a floor pixel the mask darkens to v
// takes mix(1, v, opacity) of its light with the setting off, about 0 at
// opacity 1 and half at 0.5; a floor pixel the mask leaves at 1 takes all
// of it.
#[test]
fn the_lighting_takes_the_mask_at_the_light_s_shadow_opacity() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = scene(gpu, 20., Some(POST));
    let camera = camera(EYE, Vec3::ZERO, SIZE);
    let toward_sun = toward(35_f32.to_radians(), 40_f32.to_radians());
    let coarse = DirectionalShadow {
        distance: 1000.,
        cascades: 1,
    };
    let off = settings(ContactShadows::Off);
    let on = settings(ContactShadows::Medium);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &off);
    for opacity in [1_f32, 0.5] {
        let input = input(camera, toward_sun, coarse, opacity);
        let unmasked = render(gpu, &mut renderer, &mut scene, &input, &off);
        assert!(
            unmasked.mask.is_none(),
            "the stage runs with the setting off"
        );
        let masked = render(gpu, &mut renderer, &mut scene, &input, &on);
        let (mut darkened, mut whole) = (0, 0);
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                if masked.floor(&camera, [x, y], POST, 2e-3).is_none() {
                    continue;
                }
                let (before, after) = (
                    unmasked.red[unmasked.index([x, y])],
                    masked.red[masked.index([x, y])],
                );
                assert!(before > 0., "the floor at [{x}, {y}] is lit");
                let visibility = f32::from(masked.mask([x, y])) / 255.;
                let expected = before * (1. + (visibility - 1.) * opacity);
                assert!(
                    (after - expected).abs() <= 0.01 * before,
                    "[{x}, {y}] at opacity {opacity} and visibility {visibility}: {after}, \
                     {expected} expected of {before}"
                );
                if visibility < 0.05 {
                    darkened += 1;
                } else if visibility == 1. {
                    whole += 1;
                }
            }
        }
        assert!(
            darkened >= 5 && whole > 1000,
            "{darkened} darkened and {whole} whole floor pixels at opacity {opacity}"
        );
    }
}

// Plausible defects: the mask kept at its first size after a resize, so
// the lighting pass reads it at other pixels or the stage writes past it.
// The oracle: the post's contact shadow and lit side, projected at the new
// size, after `View::resize`, and the mask read back at the render size.
#[test]
fn the_mask_follows_the_render_size() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = scene(gpu, 20., Some(POST));
    let settings = settings(ContactShadows::Medium);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let toward_sun = toward(35_f32.to_radians(), 40_f32.to_radians());
    for size in [SIZE, [384, 256]] {
        renderer.view.resize(&device, size, 1., &settings);
        assert_eq!(renderer.view.render_size(), size);
        let camera = camera(EYE, Vec3::ZERO, size);
        let input = input(camera, toward_sun, DirectionalShadow::DEFAULT, 1.);
        let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
        assert_contact_shadow(&observed, &camera, toward_sun, &format!("{size:?}"));
    }
}
