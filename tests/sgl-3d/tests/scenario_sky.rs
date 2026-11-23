//! The procedural sky (`FrameEnvironment::Sky`, the architecture's Sky):
//! its backdrop's discs, its motion, the sun colour every surface takes
//! through the air, finite frames at every sun elevation, and its
//! environment light: a mirror's reflection, a diffuse plane's bound, the
//! atlas's lag behind a changing sun and a cut, and a probe capture's sky;
//! and its aerial perspective along the ground, near and kilometres away.
//! Scenario
//! harness suite (`specs/testing.md`, "SGL3D scenario harness"); each test
//! runs alone by name.
//!
//! Coverage (requirement 3): at rest (the disc, the sun colour), under
//! camera rotation (the motion), across a camera cut and a resize (the
//! sweep), at every sun elevation from below the planet to the zenith. The
//! sky has no settings level: `FrameEnvironment::Sky` is its only control.
//! Camera translation and object motion cannot affect the backdrop, which
//! lies at infinity and moves with the camera's rotation alone; the camera's
//! height, which the sky depends on, is the sun-colour scenario's. Aerial
//! perspective is covered at rest, with the atmosphere setting off (it runs
//! without the fog); its composition with the fog is the fog stage's
//! (`stages::fog` tests).
//!
//! The analytic oracles take the air's transmittance from the published
//! medium (Hillaire 2020's Earth, as Wicked Engine's
//! `ShaderInterop_Weather.h` lists it), integrated here independently of
//! SGL3D's tables: in closed form for a vertical column, and by a
//! 100,000-step integral through the planet's spherical shell for a slant
//! path. 1 % is the spec's agreement of the transmittance table with the
//! closed form.
//!
//! `sky_sweep` dumps its frames (`SGL_SCENARIO_DUMP`), each scaled for
//! viewing by the factor its metrics record.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose, View};
use scenario::image::pixels;
use scenario::rig::{Dump, Rig, Shape, gpu};
use sgl_3d::asset::{Asset, CpuMesh, Material, Vertex};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::Vec3;
use sgl_3d::settings::Antialiasing;
use sgl_3d::{Backdrop, Camera, DirectionalLight, FrameEnvironment, Sky, perspective};

/// The planet's radii, metres (the published Earth).
const GROUND_RADIUS: f64 = 6_360_000.;
const TOP_RADIUS: f64 = 6_460_000.;

/// The published medium's extinction per metre at `altitude` metres:
/// Rayleigh's (5.802, 13.558, 33.1)e-6 falling off over 8 km, Mie's 4.440e-6
/// over 1.2 km, and ozone's (0.650, 1.881, 0.085)e-6 in a tent from 10 to
/// 40 km peaking at 25 km.
fn extinction(altitude: f64) -> [f64; 3] {
    let rayleigh = [5.802e-6, 13.558e-6, 33.1e-6];
    let ozone = [0.650e-6, 1.881e-6, 0.085e-6];
    let rayleigh_density = (-altitude / 8000.).exp();
    let mie = 4.440e-6 * (-altitude / 1200.).exp();
    let ozone_density = (1. - (altitude - 25_000.).abs() / 15_000.).max(0.);
    std::array::from_fn(|c| rayleigh[c] * rayleigh_density + mie + ozone[c] * ozone_density)
}

/// The vertical column's transmittance from `altitude` metres, in closed
/// form: each exponential medium's coefficient times its scale height times
/// its density there, and ozone's whole tent (15 km of peak density).
fn column_transmittance(altitude: f64) -> [f64; 3] {
    let rayleigh = [5.802e-6, 13.558e-6, 33.1e-6];
    let ozone = [0.650e-6, 1.881e-6, 0.085e-6];
    std::array::from_fn(|c| {
        let depth = rayleigh[c] * 8000. * (-altitude / 8000.).exp()
            + 4.440e-6 * 1200. * (-altitude / 1200.).exp()
            + ozone[c] * 15_000.;
        (-depth).exp()
    })
}

/// The transmittance from `altitude` metres toward zenith cosine `mu` to the
/// top of the atmosphere, through the spherical shell: 100,000 midpoint
/// steps.
fn slant_transmittance(altitude: f64, mu: f64) -> [f64; 3] {
    let r = GROUND_RADIUS + altitude;
    let length = -r * mu + (r * r * (mu * mu - 1.) + TOP_RADIUS * TOP_RADIUS).sqrt();
    let steps = 100_000;
    let dt = length / f64::from(steps);
    let mut depth = [0.; 3];
    for i in 0..steps {
        let t = dt * (f64::from(i) + 0.5);
        let radius = (t * t + 2. * r * mu * t + r * r).sqrt();
        let sigma = extinction(radius - GROUND_RADIUS);
        for c in 0..3 {
            depth[c] += sigma[c] * dt;
        }
    }
    depth.map(|d| (-d).exp())
}

/// A sun `elevation` radians above the horizontal toward -Z of
/// `illuminance` lux above the air, white, without a shadow.
fn sun(elevation: f32, illuminance: f32) -> DirectionalLight {
    DirectionalLight {
        direction: -Vec3::new(0., elevation.sin(), -elevation.cos()),
        illuminance,
        ..DirectionalLight::default()
    }
}

/// A rig of one view through `lens` under Earth's sky with its ground at
/// world Y 0, with no antialiasing and the sky as the backdrop.
fn sky_rig(gpu: &scenario::rig::Gpu, lens: View) -> Rig {
    let mut rig = Rig::new(gpu, lens, Antialiasing::Off, 0.);
    let input = &mut rig.views[0].input;
    input.environment = FrameEnvironment::Sky(Sky::default());
    input.backdrop = Backdrop::Environment {
        yaw: 0.,
        brightness: 1.,
    };
    rig
}

/// A white lit square `size` metres across centred on `centre`, facing
/// along `normal`.
fn lit_square(centre: Vec3, normal: Vec3, size: f32) -> Asset {
    square(centre, normal, size, (0., 1.))
}

/// A white square `size` metres across centred on `centre`, facing along
/// `normal`, of `(metallic, roughness)`.
fn square(centre: Vec3, normal: Vec3, size: f32, (metallic, roughness): (f32, f32)) -> Asset {
    let normal = normal.normalize();
    let across = if normal.y.abs() > 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    }
    .cross(normal)
    .normalize();
    let up = normal.cross(across);
    let half = size * 0.5;
    let corners = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
        .map(|(a, b)| centre + (across * a + up * b) * half);
    let tangent = across.extend(1.).to_array();
    Asset {
        meshes: vec![CpuMesh {
            vertices: corners
                .map(|position| Vertex {
                    uv1: [0.0; 2],
                    position: position.to_array(),
                    normal: normal.to_array(),
                    uv: [0.; 2],
                    color: [1.; 4],
                    tangent,
                    lightmap_bounds: [0., 0., 1., 1.],
                    lightmap_uv: [0.; 2],
                })
                .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "sunlit square".into(),
            base: [1.; 4],
            metallic,
            roughness,
            double_sided: true,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    }
}

/// A camera at `eye` looking along `forward`.
fn looking(lens: &View, eye: Vec3, forward: Vec3) -> Camera {
    let up = if forward.y.abs() > 0.9 {
        Vec3::NEG_Z
    } else {
        Vec3::Y
    };
    Camera {
        view: sgl_3d::glam::camera::rh::view::look_to_mat4(eye, forward, up),
        projection: perspective(lens.fov_y, lens.aspect(), lens.near),
        eye,
    }
}

// Plausible defects: a disc of the wrong radiance (not divided by its solid
// angle, the transmittance applied twice or not at all, the light's colour
// above the air shown unattenuated), of the wrong size or in the wrong
// place. The fixture: a 1 lux white sun at the zenith seen straight up from
// 1 m above the ground through a narrow lens, its 0.53° disc about 15 px
// across. The oracle is analytic: the pixel at the disc's centre less the
// pixel 12 px from it, beyond its edge, is the disc alone, whose radiance is
// the illuminance times the vertical column's transmittance from 1 m (closed
// form) over the disc's solid angle, π (θ / 2)², the solid angle Bevy's
// `sample_sun_radiance` divides by.
#[test]
fn the_disc_shows_its_light_through_the_air() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [33, 33],
        fov_y: 0.02,
        near: 0.1,
    };
    let mut rig = sky_rig(&gpu, lens);
    let eye = Vec3::new(0., 1., 0.);
    rig.views[0].input.camera = looking(&lens, eye, Vec3::Y);
    rig.views[0].input.directional_lights[0] = Some(sun(std::f32::consts::FRAC_PI_2, 1.));
    rig.render(0);
    let image = rig.read(DiagnosticTarget::Composite);
    let centre = image.at([16, 16]);
    let outside = image.at([28, 16]);
    let diameter = f64::from(DirectionalLight::SUN_ANGULAR_DIAMETER);
    let solid_angle = std::f64::consts::PI * diameter * diameter * 0.25;
    let transmittance = column_transmittance(1.);
    for c in 0..3 {
        let disc = f64::from(centre[c] - outside[c]);
        let expected = transmittance[c] / solid_angle;
        assert!(
            (disc / expected - 1.).abs() < 0.01,
            "channel {c}: the disc shows {disc} over the sky, the air passes {expected}"
        );
    }
}

// Plausible defects: the sky written with motion of its own (a backdrop yaw
// turning a procedural sky, or a sky that writes the turn of the lights), so
// TAA, FSR and motion blur smear it as the camera turns. The oracle is
// equivalence: a turning camera's motion over the sky equals its motion over
// a colour backdrop, exactly, while the sky's backdrop yaw changes every
// frame (no yaw turns a procedural sky).
#[test]
fn the_sky_moves_with_the_camera_alone() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [48, 32],
        fov_y: 1.,
        near: 0.1,
    };
    let turning = Motion::yaw(0.02);
    let frame = 3;
    let motion = |sky: bool| {
        let mut rig = sky_rig(&gpu, lens);
        rig.views[0].input.directional_lights[0] = Some(sun(0.4, 1.));
        for f in 0..=frame {
            rig.look(turning.pose(Pose::at(Vec3::new(0., 2., 0.)), f));
            rig.views[0].input.backdrop = if sky {
                Backdrop::Environment {
                    yaw: 0.3 + 0.05 * f as f32,
                    brightness: 1.,
                }
            } else {
                Backdrop::Color([0.5; 3])
            };
            rig.render(f);
        }
        rig.read(DiagnosticTarget::Motion)
    };
    let (sky, colour) = (motion(true), motion(false));
    assert!(
        colour.texels.iter().any(|texel| texel[0] != 0.),
        "the turning camera writes motion over the colour backdrop"
    );
    assert_eq!(
        sky, colour,
        "the sky's motion differs from the colour backdrop's"
    );
}

// Plausible defects: surfaces lit by the light above the air (no
// transmittance), by a transmittance taken toward the wrong direction or
// from the wrong height, or by one that differs from the sky's own, so a
// low sun lights the world white beside a red sky. The fixture: a white lit
// square facing the sun, seen along its normal from 2 m, about 10 m above
// the ground, so the light meets it and leaves toward the camera at the same
// angles whatever the sun's elevation; no other light (the sky's
// environment light turned to 0, no hemisphere fill, ambient occlusion or
// reflections). The oracle is
// analytic: its radiance with the sun at 20° over its radiance at the
// zenith is, per channel, the ratio of the air's transmittance along the two
// paths from the camera's height through the spherical shell.
#[test]
fn a_surface_takes_the_sun_through_the_air() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [17, 17],
        fov_y: 0.3,
        near: 0.1,
    };
    let radiance = |elevation: f32| {
        let mut rig = sky_rig(&gpu, lens);
        let toward = Vec3::new(0., elevation.sin(), -elevation.cos());
        let centre = Vec3::new(0., 10., 0.);
        let eye = centre + toward * 2.;
        rig.add_model(lit_square(centre, toward, 1.), Pose::at(Vec3::ZERO));
        rig.views[0].input.camera = looking(&lens, eye, -toward);
        rig.views[0].input.directional_lights[0] = Some(sun(elevation, 1.));
        rig.views[0].input.diffuse_environment.intensity = 0.;
        rig.views[0].input.reflection_environment.intensity = 0.;
        rig.render(0);
        let texel = rig.read(DiagnosticTarget::Composite).at([8, 8]);
        (texel, eye.y)
    };
    let (zenith, zenith_height) = radiance(std::f32::consts::FRAC_PI_2);
    let low_elevation = 20f32.to_radians();
    let (low, low_height) = radiance(low_elevation);
    let expected_zenith = slant_transmittance(f64::from(zenith_height), 1.);
    let expected_low = slant_transmittance(f64::from(low_height), f64::from(low_elevation.sin()));
    for c in 0..3 {
        let ratio = f64::from(low[c] / zenith[c]);
        let expected = expected_low[c] / expected_zenith[c];
        assert!(
            zenith[c] > 0. && (ratio / expected - 1.).abs() < 0.01,
            "channel {c}: the square at 20° over at the zenith is {ratio}, the air's ratio {expected} ({} and {})",
            low[c],
            zenith[c]
        );
    }
}

// Plausible defects: a NaN or infinity from the sky's maths at the horizon
// or below it (a square root of a negative, a division by a zero distance),
// a disc beyond binary16 left unbounded, or tables left stale or unbound by a
// cut or a resize. The fixture: a lit square beside the sun and the sky
// toward the sun, which sweeps from -90° (below the planet) through the
// horizon to the zenith under a realistic 120,000 lux, then a camera cut,
// then a resize. The oracle is a bound: every pixel of every frame finite
// and non-negative.
#[test]
fn frames_stay_finite_at_every_sun_elevation_across_a_cut_and_a_resize() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [96, 54],
        fov_y: 1.2,
        near: 0.1,
    };
    let mut rig = sky_rig(&gpu, lens);
    // Lying beside the sun's azimuth below the eye, facing up.
    rig.add_model(
        lit_square(Vec3::new(-3., 0.5, -6.), Vec3::Y, 3.),
        Pose::at(Vec3::ZERO),
    );
    let mut dump = Dump::new("sky_sweep");
    let elevations = [
        -90., -60., -30., -10., -4., -2., 0., 2., 5., 10., 20., 30., 60., 90.,
    ];
    let illuminance = 120_000.;
    let mut check = |rig: &Rig, label: &str| {
        let image = rig.read(DiagnosticTarget::Composite);
        let mut sum = 0.;
        for pixel in pixels(image.size) {
            let texel = image.at(pixel);
            assert!(
                texel[..3].iter().all(|&c| c.is_finite() && c >= 0.),
                "{label}: {texel:?} at {pixel:?}"
            );
            sum += texel[..3].iter().map(|&c| f64::from(c)).sum::<f64>() / 3.;
        }
        // For viewing only: the frame scaled so its mean radiance shows mid
        // grey under the dump's v / (1 + v).
        let mean = sum / f64::from(image.size[0] * image.size[1]);
        let scale = if mean > 0. { (1. / mean) as f32 } else { 1. };
        let mut scaled = image.clone();
        for texel in &mut scaled.texels {
            for c in &mut texel[..3] {
                *c *= scale;
            }
        }
        dump.image(label, &scaled);
        dump.metric(format!("{label} mean radiance"), mean);
        dump.metric(format!("{label} viewing scale"), scale);
    };
    let mut frame = 0;
    for elevation in elevations {
        rig.look(Pose::at(Vec3::new(0., 1.7, 0.)));
        rig.views[0].input.directional_lights[0] =
            Some(sun(f32::to_radians(elevation), illuminance));
        rig.render(frame);
        check(&rig, &format!("elevation {elevation:+}"));
        frame += 1;
    }
    rig.views[0].input.camera_cut = true;
    rig.look(Pose::at(Vec3::new(5., 40., 3.)));
    rig.render(frame);
    check(&rig, "after a cut");
    frame += 1;
    rig.views[0].lens = View {
        size: [64, 64],
        ..lens
    };
    rig.render(frame);
    check(&rig, "after a resize");
}

/// One atlas texel's angular footprint at its sharp level: a face of 128
/// texels spans 90°.
const ATLAS_TEXEL: f32 = std::f32::consts::FRAC_PI_2 / 128.;

/// The direction at `elevation` radians above the horizontal and `azimuth`
/// radians about +Y from -Z toward +X.
fn toward(azimuth: f32, elevation: f32) -> Vec3 {
    Vec3::new(
        azimuth.sin() * elevation.cos(),
        elevation.sin(),
        -azimuth.cos() * elevation.cos(),
    )
}

/// Each channel's least and greatest radiance `image` shows, seen through
/// `lens` from `camera`, over the pixels whose ray lies within `radius`
/// radians of `direction`.
fn range_about(
    lens: &View,
    camera: &Camera,
    image: &scenario::image::Image,
    direction: Vec3,
    radius: f32,
) -> ([f32; 3], [f32; 3]) {
    let mut range = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for pixel in pixels(image.size) {
        let ray = lens.ray(camera, scenario::image::centre(pixel));
        if ray.dot(direction).clamp(-1., 1.).acos() <= radius {
            for (c, &value) in image.at(pixel)[..3].iter().enumerate() {
                range.0[c] = range.0[c].min(value);
                range.1[c] = range.1[c].max(value);
            }
        }
    }
    assert!(
        range.0[0].is_finite(),
        "no pixel within {radius} of {direction}"
    );
    range
}

/// Whether every channel of `texel` lies in `range`.
fn within(texel: [f32; 4], (low, high): ([f32; 3], [f32; 3])) -> bool {
    (0..3).all(|c| texel[c] >= low[c] && texel[c] <= high[c])
}

/// The rig's model instance `id` shown or hidden.
fn show(rig: &mut Rig, id: sgl_3d::InstanceId, visible: bool) {
    let state = *rig.scene.instance(id).expect("a fixture instance");
    rig.scene
        .set_instance(
            &rig.gpu.queue,
            id,
            sgl_3d::InstanceState { visible, ..state },
        )
        .expect("a fixture instance");
}

/// A rig under the sky with a smooth white metal mirror 400 m across lying
/// on the ground, and the camera 2 m above it through a lens 0.1 radians
/// across 33 pixels (0.17° a pixel).
struct Mirror {
    rig: Rig,
    lens: View,
    id: sgl_3d::InstanceId,
    eye: Vec3,
}

impl Mirror {
    fn new(gpu: &scenario::rig::Gpu) -> Mirror {
        let lens = View {
            size: [33, 33],
            fov_y: 0.1,
            near: 0.1,
        };
        let mut rig = sky_rig(gpu, lens);
        let id = rig.add_model(
            square(Vec3::ZERO, Vec3::Y, 400., (1., 0.)),
            Pose::at(Vec3::ZERO),
        );
        Mirror {
            rig,
            lens,
            id,
            eye: Vec3::new(0., 2., 0.),
        }
    }

    /// Renders frame `frame` looking down at the mirror along `toward`
    /// mirrored, and returns the centre pixel, whose reflection is `toward`.
    fn reflection(&mut self, frame: u32, toward: Vec3) -> [f32; 4] {
        show(&mut self.rig, self.id, true);
        let down = Vec3::new(toward.x, -toward.y, toward.z);
        self.rig.views[0].input.camera = looking(&self.lens, self.eye, down);
        self.rig.render(frame);
        self.rig.read(DiagnosticTarget::Composite).at([16, 16])
    }

    /// The environment's share of the mirror's reflection of `toward`, in
    /// frames `frame` and `frame + 1`: the reflection less the same with
    /// `reflection_environment.intensity` 0, which leaves the lights' own
    /// specular (the sun's highlight reaches 10° from it on a mirror whose
    /// roughness the shading floors).
    fn environment_reflection(&mut self, frame: u32, toward: Vec3) -> [f32; 4] {
        let with = self.reflection(frame, toward);
        let input = &mut self.rig.views[0].input;
        input.reflection_environment.intensity = 0.;
        let without = self.reflection(frame + 1, toward);
        self.rig.views[0].input.reflection_environment.intensity = 1.;
        std::array::from_fn(|c| with[c] - without[c])
    }

    /// Renders frame `frame` looking along `toward` with the mirror hidden,
    /// and returns the backdrop's range within `radius` of it.
    fn backdrop(&mut self, frame: u32, toward: Vec3, radius: f32) -> ([f32; 3], [f32; 3]) {
        show(&mut self.rig, self.id, false);
        let camera = looking(&self.lens, self.eye, toward);
        self.rig.views[0].input.camera = camera;
        self.rig.render(frame);
        let image = self.rig.read(DiagnosticTarget::Composite);
        range_about(&self.lens, &camera, &image, toward, radius)
    }
}

// Plausible defects: the environment atlas's sharp level holding the sky
// flipped, mirrored or turned (a face or the atlas's y convention wrong), a
// filtered level read where the sharp one should be, or the sky's atlas not
// bound where a map's is. The fixture: a smooth metal mirror (roughness 0,
// no screen-space or world-space reflections) under a sun at 30° and
// azimuth 45°, off the axes, seen 20° below the horizontal at azimuths
// toward, across and away from the sun.
// The oracle is equivalence: the mirror reflects the environment atlas's
// sharp level, which holds the sky-view table the backdrop shows, so the
// environment's share of its radiance (its reflection less the same with
// `reflection_environment.intensity` 0) lies within the range the backdrop
// shows over one atlas texel's footprint (0.7°) about the mirrored
// direction.
#[test]
fn a_mirror_reflects_the_sky_the_backdrop_shows() {
    let Some(gpu) = gpu() else { return };
    let mut mirror = Mirror::new(&gpu);
    // Off the axes, so a sky mirrored or turned about any of them differs.
    let sun_azimuth = 45f32;
    mirror.rig.views[0].input.directional_lights[0] = Some(DirectionalLight {
        direction: -toward(sun_azimuth.to_radians(), 30f32.to_radians()),
        illuminance: 1.,
        ..DirectionalLight::default()
    });
    let mut frame = 0;
    for azimuth in [45f32, 135., 225., 315.] {
        let reflected = toward(azimuth.to_radians(), 20f32.to_radians());
        let seen = mirror.environment_reflection(frame, reflected);
        let range = mirror.backdrop(frame + 2, reflected, ATLAS_TEXEL);
        frame += 3;
        assert!(
            within(seen, range),
            "azimuth {azimuth}°: the mirror shows {seen:?}, the backdrop {range:?} about the reflected direction"
        );
    }
}

// Plausible defects: the environment's diffuse light under a sky missing
// (the black stand-in still bound), not scaled by
// `diffuse_environment.intensity`, or creating energy (a level filtered
// without normalising its weights, a wrong level read as the roughest). The
// fixture: a white diffuse plane facing up, under a sun at 30° and a sky
// without discs. The oracle is a bound: the radiance it gains when
// `diffuse_environment.intensity` goes from 0 to 1 is positive and finite in
// every channel and at most its albedo (1) times the brightest radiance the
// backdrop shows over the upper hemisphere, since its irradiance from a sky
// no brighter than that is at most π times it; with no light it gains
// exactly 0.
#[test]
fn a_diffuse_plane_takes_no_more_than_the_sky_gives() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [48, 48],
        fov_y: 1.6,
        near: 0.1,
    };
    let gain = |light: Option<DirectionalLight>| {
        let mut rig = sky_rig(&gpu, lens);
        rig.views[0].input.environment = FrameEnvironment::Sky(Sky {
            sun_disc: 0.,
            ..Sky::default()
        });
        rig.views[0].input.directional_lights[0] = light;
        rig.views[0].input.reflection_environment.intensity = 0.;
        let eye = Vec3::new(0., 2., 0.);
        let plane = rig.add_model(lit_square(Vec3::ZERO, Vec3::Y, 400.), Pose::at(Vec3::ZERO));
        let mut taken = [[0.; 4]; 2];
        for (frame, intensity) in [0., 1.].into_iter().enumerate() {
            rig.views[0].input.diffuse_environment.intensity = intensity;
            rig.views[0].input.camera = looking(&lens, eye, Vec3::NEG_Y);
            rig.render(frame as u32);
            taken[frame] = rig.read(DiagnosticTarget::Composite).at([24, 24]);
        }
        // The brightest backdrop over the upper hemisphere: straight up and
        // toward the horizon at four azimuths, 92° across each.
        show(&mut rig, plane, false);
        let mut brightest = [0f32; 3];
        let directions = [Vec3::Y, Vec3::NEG_Z, Vec3::X, Vec3::Z, Vec3::NEG_X];
        for (index, forward) in directions.into_iter().enumerate() {
            let camera = looking(&lens, eye, forward);
            rig.views[0].input.camera = camera;
            rig.render(2 + index as u32);
            let image = rig.read(DiagnosticTarget::Composite);
            for pixel in pixels(image.size) {
                if lens.ray(&camera, scenario::image::centre(pixel)).y >= 0. {
                    let texel = image.at(pixel);
                    for c in 0..3 {
                        brightest[c] = brightest[c].max(texel[c]);
                    }
                }
            }
        }
        let gained: [f32; 3] = std::array::from_fn(|c| taken[1][c] - taken[0][c]);
        (gained, brightest)
    };
    let (gained, brightest) = gain(Some(sun(30f32.to_radians(), 1.)));
    for c in 0..3 {
        assert!(
            gained[c].is_finite() && gained[c] > 0. && gained[c] <= brightest[c],
            "channel {c}: the plane gains {} from the sky, whose brightest is {}",
            gained[c],
            brightest[c]
        );
    }
    let (gained, _) = gain(None);
    assert_eq!(gained, [0.; 3], "no light, no sky light");
}

// Plausible defects: the atlas left stale for good after the sun moves (a
// cycle never started, or a change during a cycle lost), refreshed sooner
// than its schedule (a second atlas, or every step a frame), or a cut that
// shows a stale sky. The fixture: the mirror under a sun nudged from 30° to
// 31° (starting a cycle with its capture), then dropped to 5° in the next
// frame, while the cycle runs. The oracle is the contract (X-5) with the
// mirror's equivalence to the backdrop: the mirror's sharp level still
// holds the 31° sky in the frame of the change and the fourteen after it,
// and the new sky in the sixteenth, when the next cycle captures it; with a
// cut, in the frame of the change.
#[test]
fn the_mirror_follows_the_sun_within_a_cycle_and_at_once_after_a_cut() {
    let Some(gpu) = gpu() else { return };
    let mut mirror = Mirror::new(&gpu);
    let reflected = toward(std::f32::consts::PI, 20f32.to_radians());
    let set_sun = |mirror: &mut Mirror, degrees: f32| {
        mirror.rig.views[0].input.directional_lights[0] = Some(sun(degrees.to_radians(), 1.));
    };
    set_sun(&mut mirror, 30.);
    mirror.reflection(0, reflected);
    mirror.reflection(1, reflected);
    set_sun(&mut mirror, 31.);
    mirror.reflection(2, reflected);
    set_sun(&mut mirror, 5.);
    let seen: Vec<[f32; 4]> = (3..=18)
        .map(|frame| mirror.reflection(frame, reflected))
        .collect();
    let range = mirror.backdrop(19, reflected, ATLAS_TEXEL);
    assert!(
        !within(seen[0], range) && !within(seen[14], range),
        "the mirror took the 5° sky before the next cycle's capture: {:?} and {:?} in {range:?}",
        seen[0],
        seen[14]
    );
    assert!(
        within(seen[15], range),
        "sixteen frames after the change the mirror shows {:?}, the backdrop {range:?}",
        seen[15]
    );
    set_sun(&mut mirror, 60.);
    mirror.rig.views[0].input.camera_cut = true;
    let after_cut = mirror.reflection(20, reflected);
    let range = mirror.backdrop(21, reflected, ATLAS_TEXEL);
    assert!(
        within(after_cut, range),
        "in the frame of a cut the mirror shows {after_cut:?}, the backdrop {range:?}"
    );
}

// Plausible defects: a probe capture under a sky recording the black
// stand-in, or an atlas of its own built for another position or left
// unfiltered or unfinished. The fixture: a capture 2 m above the ground under
// a sun at 30°, 128 texels a face. The oracle is equivalence: its zenith
// texel (its +Y face's texel beside the centre, half a texel off the
// zenith) lies within the range the camera's backdrop shows over one atlas
// texel's footprint plus that half texel about the zenith.
#[test]
fn a_probe_capture_records_the_sky_the_backdrop_shows() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [33, 33],
        fov_y: 0.1,
        near: 0.1,
    };
    let mut rig = sky_rig(&gpu, lens);
    let eye = Vec3::new(0., 2., 0.);
    rig.views[0].input.directional_lights[0] = Some(sun(30f32.to_radians(), 1.));
    let camera = looking(&lens, eye, Vec3::Y);
    rig.views[0].input.camera = camera;
    rig.render(0);
    let image = rig.read(DiagnosticTarget::Composite);
    let face = 128;
    let offset = (0.5f32 / 64.).atan() * std::f32::consts::SQRT_2;
    let range = range_about(&lens, &camera, &image, Vec3::Y, ATLAS_TEXEL + offset);
    let Rig {
        renderer,
        scene,
        views,
        gpu: scenario::rig::Gpu { device, queue },
        ..
    } = &mut rig;
    let view = &mut views[0];
    let probe = renderer
        .capture_specular_probe(
            device,
            queue,
            scene,
            &mut view.view,
            &view.input,
            &view.settings,
            eye,
            face,
        )
        .expect("a capture");
    let sgl_3d::baked_specular_probe::SpecularProbeTexels::Rgba16Float(texels) = probe.texels
    else {
        panic!("a capture returns half floats");
    };
    // Level 0, face 2 (+Y), texel (63, 63).
    let at = ((2 * face * face + 63 * face + 63) * 4) as usize;
    let zenith: [f32; 4] = std::array::from_fn(|c| half(texels[at + c]));
    assert!(
        within(zenith, range),
        "the capture's zenith is {zenith:?}, the backdrop's {range:?}"
    );
}

// Plausible defects: aerial perspective that reads near surfaces as hazy
// (the owner's direction: haze belongs to the far distance alone), the
// slices' distances in kilometres where the march takes metres or the
// reverse, slices spaced linearly where the reader takes them squared, the
// in-scatter accumulated without the transmittance in front of it, blended
// surfaces fogged from another place in the volume than opaque ones, or haze
// left on at a strength of 0. The fixture: a camera 1 m above the ground with
// the atmosphere setting off (aerial perspective runs without the fog),
// whose pixel row 15 of 32 is the aerial volume's row 15 of 32 and lies
// level, so its ray runs along the ground through uniform air; on that ray
// an unlit card of radiance 0, then of radiance 100, at 30 m and at two of
// the volume's slice depths, 945 m and 4.88 km; a sun from behind, which
// lights each step of the level ray alike. The oracles are analytic: the
// card of 0 shows the air's light alone, A(d), and of 100, 100 T(d) + A(d),
// so T(d) = (bright - black) / 100. At 30 m T lies within 0.002 of 1 and A
// below 0.5 % of the level sky's radiance; at the slice depths T meets the
// closed-form mean transmittance of ground-level air within 1 %, A lies
// between 0 and the level sky's radiance beside the card (the air to the sky
// holds more), and A grows between them as single scattering through
// uniform air does, A(d1) / A(d2) = (1 - exp(-sigma d1)) /
// (1 - exp(-sigma d2)) per channel, within 2 % (the volumes'
// interpolation). A blended card of radiance 100
// shows what the opaque one does, within 1 %, and at strength 0 the far card
// of 0 stays 0.
#[test]
fn aerial_haze_is_negligible_near_and_builds_over_kilometres() {
    let Some(gpu) = gpu() else { return };
    let lens = View {
        size: [16, 32],
        fov_y: 1.,
        near: 0.1,
    };
    let mut rig = sky_rig(&gpu, lens);
    let eye = Vec3::new(0., 1., 0.);
    let projection = perspective(lens.fov_y, lens.aspect(), lens.near);
    let pixel = [8, 15];
    let ndc_y = 1. - 2. * (pixel[1] as f32 + 0.5) / lens.size[1] as f32;
    let tilt = (ndc_y / projection.y_axis.y).atan();
    let forward = Vec3::new(0., -tilt.sin(), -tilt.cos());
    let camera = looking(&lens, eye, forward);
    rig.views[0].input.camera = camera;
    rig.views[0].input.directional_lights[0] = Some(DirectionalLight {
        direction: -Vec3::new(1., 1., 1.).normalize(),
        illuminance: 10_000.,
        ..DirectionalLight::default()
    });
    // The pixel's ray, level.
    let ndc_x = 2. * (pixel[0] as f32 + 0.5) / lens.size[0] as f32 - 1.;
    let view_ray = Vec3::new(
        ndc_x / projection.x_axis.x,
        ndc_y / projection.y_axis.y,
        -1.,
    );
    let ray = camera
        .view
        .inverse()
        .transform_vector3(view_ray)
        .normalize();
    assert!(ray.y.abs() < 1e-6, "the pixel's ray is level: {ray}");
    let mut frame = 0;
    let mut level = |rig: &mut Rig, depth: f32, card: Option<Shape>, strength: f32| {
        rig.views[0].input.environment = FrameEnvironment::Sky(Sky {
            aerial_perspective: strength,
            ..Sky::default()
        });
        let distance = depth / ray.dot(forward);
        let id = card.map(|card| {
            let size = lens.pixel_metres(depth) * 4.;
            rig.add(&card.scaled(size), Pose::at(eye + ray * distance), 0)
        });
        rig.render(frame);
        frame += 1;
        if let Some(id) = id {
            rig.remove(id);
        }
        let image = rig.read(DiagnosticTarget::Composite);
        // The level sky, beside the card.
        let sky = image.at([0, pixel[1]]);
        (image.at(pixel), f64::from(distance), sky)
    };
    let card = |radiance| Shape::card(sgl_3d::glam::Vec2::ONE, 0., radiance);
    let mut measured = Vec::new();
    for depth in [30., 945.3125, 4882.8125] {
        let (black, distance, sky) = level(&mut rig, depth, Some(card(0.)), 1.);
        let (bright, ..) = level(&mut rig, depth, Some(card(100.)), 1.);
        let transmittance: [f64; 3] =
            std::array::from_fn(|c| f64::from(bright[c] - black[c]) / 100.);
        measured.push((distance, black, bright, transmittance, sky));
    }
    // Ground-level air's extinction (ozone's tent is empty below 10 km).
    let sigma = extinction(0.);
    let closed_form = |distance: f64| sigma.iter().map(|s| (-s * distance).exp()).sum::<f64>() / 3.;
    let (_, near_black, _, near, sky) = measured[0];
    for c in 0..3 {
        assert!(
            (1. - near[c]).abs() < 0.002 && near_black[c] < 0.005 * sky[c],
            "channel {c} at 30 m: transmittance {}, added light {} against the level sky's {}",
            near[c],
            near_black[c],
            sky[c]
        );
    }
    for &(distance, black, _, transmittance, sky) in &measured[1..] {
        let expected = closed_form(distance);
        for c in 0..3 {
            assert!(
                (transmittance[c] / expected - 1.).abs() < 0.01
                    && black[c] > 0.
                    && black[c] < sky[c],
                "channel {c} at {distance} m: transmittance {}, the closed form {expected}; added light {}, the level sky's {}",
                transmittance[c],
                black[c],
                sky[c]
            );
        }
    }
    let [
        (near_distance, near_black, ..),
        (far_distance, far_black, far_bright, ..),
    ] = [measured[1], measured[2]];
    for c in 0..3 {
        let ratio = f64::from(far_black[c] / near_black[c]);
        let expected =
            (1. - (-sigma[c] * far_distance).exp()) / (1. - (-sigma[c] * near_distance).exp());
        assert!(
            (ratio / expected - 1.).abs() < 0.02,
            "channel {c}: the air's light grows {ratio} times from {near_distance} m to {far_distance} m, single scattering {expected}"
        );
    }
    let far = 4882.8125;
    let (blended, ..) = level(&mut rig, far, Some(card(100.).blended(1.)), 1.);
    for c in 0..3 {
        assert!(
            (blended[c] / far_bright[c] - 1.).abs() < 0.01,
            "channel {c}: the blended card shows {}, the opaque {}",
            blended[c],
            far_bright[c]
        );
    }
    let (clear, ..) = level(&mut rig, far, Some(card(0.)), 0.);
    assert_eq!(
        &clear[..3],
        &[0.; 3],
        "strength 0 leaves the far card clear"
    );
}

/// An IEEE-754 binary16 value.
fn half(bits: u16) -> f32 {
    let exponent = i32::from((bits >> 10) & 31);
    let fraction = f32::from(bits & 1023);
    let magnitude = if exponent == 0 {
        fraction * 2f32.powi(-24)
    } else {
        (1. + fraction / 1024.) * 2f32.powi(exponent - 15)
    };
    if bits & 0x8000 == 0 {
        magnitude
    } else {
        -magnitude
    }
}
