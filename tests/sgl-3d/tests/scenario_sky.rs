//! The procedural sky (`FrameEnvironment::Sky`, the architecture's Sky):
//! its backdrop's discs, its motion, the sun colour every surface takes
//! through the air, and finite frames at every sun elevation. Scenario
//! harness suite (`specs/testing.md`, "SGL3D scenario harness"); each test
//! runs alone by name.
//!
//! Coverage (requirement 3): at rest (the disc, the sun colour), under
//! camera rotation (the motion), across a camera cut and a resize (the
//! sweep), at every sun elevation from below the planet to the zenith. The
//! sky has no settings level: `FrameEnvironment::Sky` is its only control.
//! Camera translation and object motion cannot affect the backdrop, which
//! lies at infinity and moves with the camera's rotation alone; the camera's
//! height, which the sky depends on, is the sun-colour scenario's. Not
//! covered yet: the sky's environment light and aerial perspective, which it
//! does not give yet.
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
use scenario::rig::{Dump, Rig, gpu};
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
            metallic: 0.,
            roughness: 1.,
            double_sided: true,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
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
// angles whatever the sun's elevation; no other light (no environment
// light, hemisphere fill, ambient occlusion or reflections). The oracle is
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
