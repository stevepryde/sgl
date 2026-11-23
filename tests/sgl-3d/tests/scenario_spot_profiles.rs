//! Spot lights' measured profiles (`Light::profile`, `SpotProfile`,
//! `LightProfile`) in the scenario harness (`specs/testing.md`, "SGL3D
//! scenario harness"): a spot shines with an LM-63 file's intensity by
//! direction, its nadir along the spot's direction and its 0° horizontal
//! angle toward `zero_azimuth`, the angles turning counterclockwise seen
//! looking along the direction (LM-63 type C, from above), on surfaces and
//! in the volumetric fog (the architecture's Lights and shadows). The
//! profiles are LM-63 files written here, whose candela values are the
//! oracle: a Lambertian wall under a light without specular, where the
//! profiled spot's frame is the same spot's frame without a profile times
//! the file's value toward each wall point over its peak, bilinear in the
//! vertical and horizontal angles between its measured ones (LM-63's
//! interpolation). No temporal feature runs: antialiasing is off and every
//! frame is a camera cut.
//!
//! Coverage (requirement 3): at rest, turned about its axis, across an
//! atlas repack, beside an ordinary spot, and in the fog. A profile has no
//! setting or level. Not covered: camera motion, an object moving through
//! the light, a resize and a cut, which cannot affect it: it is a function
//! of the light's position and frame and the receiver's world position
//! alone, sampled at the atlas's first level whatever the footprint.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::{Image, Region, centre};
use scenario::rig::{Dump, HALF_STEP, Rig, gpu};
use sgl_3d::asset::{Asset, CpuMesh, Image as Picture, Material, Vertex};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;
use sgl_3d::{Decal, FogVolume, Light, LightId, LightProfile, LightShape, SpotProfile};

/// The camera's lens: a pixel spans 1/16 m on the wall 6 m ahead.
const VIEW: View = View {
    size: [128, 96],
    fov_y: 0.927_295_2,
    near: 0.05,
};
/// The wall's plane, facing +Z, 6 m ahead of the camera at the origin.
const WALL_Z: f32 = -6.;
/// The spot, 4 m in front of the wall, shining at it along -Z, half a
/// pixel right of and above the camera's axis, so the wall's middle row and
/// column of pixel centres lie in its principal planes. The camera looks
/// along the spot's direction: from above, in LM-63's terms, so its
/// horizontal angles turn counterclockwise on screen.
const SPOT: Vec3 = Vec3::new(1. / 32., 1. / 32., -2.);
const INNER: f32 = 0.85;
const OUTER: f32 = 0.9;

/// The test file's vertical angles: every 10° from the nadir to 90°.
const VERTICAL: [f64; 10] = [0., 10., 20., 30., 40., 50., 60., 70., 80., 90.];
/// Its planes' horizontal angles.
const PLANES: [f64; 5] = [0., 90., 180., 270., 360.];
/// Its candela values on its planes at 0°, 90°, 180° and 270° (360° is 0°
/// again): each plane's own, alike only at the nadir, dark past 50°. The
/// peak, 1000, lies 40° from the nadir in the 0° plane.
const CANDELA: [[f64; 10]; 4] = [
    [600., 700., 800., 900., 1000., 800., 0., 0., 0., 0.],
    [600., 550., 500., 450., 400., 350., 0., 0., 0., 0.],
    [600., 500., 380., 260., 140., 50., 0., 0., 0., 0.],
    [600., 640., 680., 720., 760., 800., 0., 0., 0., 0.],
];
/// Its candela multiplier: its peak is 1000 × 0.02 = 20 cd.
const MULTIPLIER: f64 = 0.02;
const PEAK: f32 = 20.;

/// An LM-63-2002 type C file of `vertical` and `horizontal` angles whose
/// planes hold `candela`, times `multiplier`.
fn lm63(vertical: &[f64], horizontal: &[f64], candela: &[&[f64]], multiplier: f64) -> String {
    let list = |values: &[f64]| {
        values
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut text = format!(
        "IESNA:LM-63-2002\n[TEST] SGL scenario profile\nTILT=NONE\n1 -1 {multiplier} {} {} 1 2 0 0 0\n1 1 10\n{}\n{}\n",
        vertical.len(),
        horizontal.len(),
        list(vertical),
        list(horizontal),
    );
    for plane in candela {
        text.push_str(&list(plane));
        text.push('\n');
    }
    text
}

/// The asymmetric test file.
fn asymmetric() -> LightProfile {
    let planes: Vec<&[f64]> = CANDELA
        .iter()
        .chain([&CANDELA[0]])
        .map(|plane| plane.as_slice())
        .collect();
    LightProfile::from_ies(&lm63(&VERTICAL, &PLANES, &planes, MULTIPLIER)).unwrap()
}

/// The test file's value over its peak at vertical angle `polar` and
/// horizontal angle `azimuth` (degrees, 0 to 360): bilinear between its
/// measured angles.
fn expected(polar: f64, azimuth: f64) -> (f64, f64) {
    let corner = |plane: usize, at: usize| CANDELA[plane % 4][at] / 1000.;
    let v = ((polar / 10.).floor() as usize).min(8);
    let h = ((azimuth / 90.).floor() as usize).min(3);
    let (s, t) = (polar / 10. - v as f64, azimuth / 90. - h as f64);
    let corners = [
        corner(h, v),
        corner(h, v + 1),
        corner(h + 1, v),
        corner(h + 1, v + 1),
    ];
    let value = (corners[0] * (1. - s) + corners[1] * s) * (1. - t)
        + (corners[2] * (1. - s) + corners[3] * s) * t;
    let spread = corners.iter().copied().fold(f64::MIN, f64::max)
        - corners.iter().copied().fold(f64::MAX, f64::min);
    (value, spread)
}

/// A quad facing +Z centred on `center`, `half` metres from it to each side.
fn quad(center: Vec3, half: Vec2) -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                position: (center + Vec3::new(x * half.x, y * half.y, 0.)).to_array(),
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// A white, fully rough dielectric: a Lambertian receiver.
fn lit(mesh: CpuMesh) -> Asset {
    Asset {
        meshes: vec![mesh],
        materials: vec![Material {
            name: "scenario lit fixture".into(),
            base: [1.; 4],
            metallic: 0.,
            roughness: 1.,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
    }
}

/// The spot shining at the wall at `PEAK` candela, diffuse only, with
/// `profile`.
fn spot(profile: Option<SpotProfile>) -> Light {
    Light {
        position: SPOT,
        shape: LightShape::Spot {
            direction: Vec3::NEG_Z,
            inner_angle: INNER,
            outer_angle: OUTER,
            radius: 0.,
        },
        intensity: PEAK,
        range: 12.,
        specular: 0.,
        profile,
        ..Default::default()
    }
}

fn off(light: Light) -> Light {
    Light {
        intensity: 0.,
        ..light
    }
}

/// A rig of the camera at the origin over a black backdrop, holding
/// `meshes` as lit fixtures and `light`, with no other light.
struct Stage {
    rig: Rig,
    light: LightId,
}

impl Stage {
    fn new(meshes: Vec<CpuMesh>, light: Light) -> Option<Stage> {
        let gpu = gpu()?;
        let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, 0.);
        for mesh in meshes {
            rig.add_model(lit(mesh), Pose::at(Vec3::ZERO));
        }
        let light = rig
            .scene
            .add_light(&rig.gpu.device, &rig.gpu.queue, light)
            .unwrap();
        Some(Stage { rig, light })
    }

    fn wall(light: Light) -> Option<Stage> {
        Self::new(
            vec![quad(Vec3::new(0., 0., WALL_Z), Vec2::new(6., 4.5))],
            light,
        )
    }

    /// The linear scene of a camera cut with the light set to `light`.
    fn shot(&mut self, light: Light) -> Image {
        let rig = &mut self.rig;
        rig.scene
            .set_light(&rig.gpu.queue, self.light, light)
            .unwrap();
        self.frame()
    }

    fn frame(&mut self) -> Image {
        let rig = &mut self.rig;
        rig.views[0].input.camera_cut = true;
        rig.render(0);
        rig.read(DiagnosticTarget::Composite)
    }
}

/// Where the camera at the origin sees the wall through pixel `pixel`.
fn on_wall(pixel: [u32; 2]) -> Vec3 {
    let camera = VIEW.camera(Pose::at(Vec3::ZERO));
    let ray = VIEW.ray(&camera, centre(pixel));
    ray * (WALL_Z / ray.z)
}

/// A wall point's vertical angle from the spot's direction, in degrees,
/// and its offset across the direction (x, y).
fn polar(point: Vec3) -> (f64, Vec2) {
    let offset = point - SPOT;
    let across = offset.truncate();
    (
        f64::from(across.length().atan2(-offset.z)).to_degrees(),
        across,
    )
}

/// The wall pixels well inside the inner cone (nine tenths of its angle),
/// where the cone leaves the light whole.
fn in_cone(pixel: [u32; 2]) -> bool {
    polar(on_wall(pixel)).0 < 0.9 * f64::from(INNER).to_degrees()
}

/// The wall's row and column of pixels inside the cone whose centres lie
/// in the spot's principal planes, through its axis along X and along Y.
fn principal_planes() -> (Region, Region) {
    let on = |pixel: [u32; 2], axis: usize| {
        let offset = on_wall(pixel) - SPOT;
        offset[axis].abs() < 1e-4 && in_cone(pixel)
    };
    (
        Region::from_fn(VIEW.size, |pixel| on(pixel, 1)),
        Region::from_fn(VIEW.size, |pixel| on(pixel, 0)),
    )
}

/// A frame of no light.
fn black() -> Image {
    Image {
        size: VIEW.size,
        texels: vec![[0.; 4]; (VIEW.size[0] * VIEW.size[1]) as usize],
    }
}

/// Asserts the light brings light to every pixel of `region`.
fn assert_lit(lit: &Image, unlit: &Image, region: &Region, what: &str) {
    assert!(!region.is_empty(), "{what}: no pixels");
    for pixel in region.iter() {
        let (on, without) = (lit.value(pixel), unlit.value(pixel));
        assert!(
            on - without > 4. * HALF_STEP * on,
            "{what}: the light brings nothing at {pixel:?} ({on} against {without})"
        );
    }
}

/// Asserts `image` agrees with `oracle` over `region` to within one
/// half-float step of the larger value in each colour channel, the most a
/// store into the Rgba16Float target changes a value.
fn assert_agrees(image: &Image, oracle: &Image, region: &Region, what: &str) {
    assert!(!region.is_empty(), "{what}: no pixels");
    for pixel in region.iter() {
        let (a, b) = (image.at(pixel), oracle.at(pixel));
        for c in 0..3 {
            assert!(
                (a[c] - b[c]).abs() <= HALF_STEP * a[c].abs().max(b[c].abs()),
                "{what}: {} at {pixel:?}, where the oracle frame holds {}",
                a[c],
                b[c]
            );
        }
    }
}

/// Asserts that over the wall pixels `region` takes, `profiled` is
/// `plain` times the test file's value toward each wall point, its
/// horizontal angle `azimuth` of the point's offset across the spot's
/// direction (degrees, 0 to 360). The bound is the frames' and the atlas's
/// half-float stores (three steps of the value) and the sampler's
/// bilinear weights, which WebGPU leaves to the device and which common
/// GPUs (D3D11's minimum, Apple's and NVIDIA's) quantise to 8 bits of a
/// texel: the corners' spread over 256.
fn assert_profiled(
    profiled: &Image,
    plain: &Image,
    region: &Region,
    azimuth: impl Fn(Vec2) -> f64,
    what: &str,
) -> usize {
    assert!(!region.is_empty(), "{what}: no pixels");
    for pixel in region.iter() {
        let (polar, across) = polar(on_wall(pixel));
        let (value, spread) = expected(polar, azimuth(across).rem_euclid(360.));
        let base = plain.at(pixel);
        let got = profiled.at(pixel);
        for c in 0..3 {
            let want = f64::from(base[c]) * value;
            let bound = f64::from(base[c]) * (3. * f64::from(HALF_STEP) * value + spread / 256.);
            assert!(
                (f64::from(got[c]) - want).abs() <= bound,
                "{what}: channel {c} at {pixel:?} ({polar:.2}° from the axis, {:.2}° about it) is {}, not {want} (± {bound})",
                azimuth(across).rem_euclid(360.),
                got[c]
            );
        }
    }
    region.len()
}

/// The horizontal angle, degrees, of an offset across the spot's
/// direction, counterclockwise on screen from `zero` (both across the
/// direction, which points away from the camera: LM-63's view from above).
fn azimuth_from(zero: Vec2) -> impl Fn(Vec2) -> f64 {
    move |across: Vec2| {
        let turned = Vec2::new(across.dot(zero), across.dot(zero.perp()));
        f64::from(turned.y.atan2(turned.x)).to_degrees()
    }
}

// Plausible defects: the profile's planes read in the wrong order or on
// the wrong sides (a mirrored or turned horizontal angle, clockwise for
// counterclockwise), its vertical angle measured from the wrong pole, its
// samples stretched between the wrong angles or blurred across them, a
// profile that scales the light by other than its value over its peak, or
// an intensity that is not the candela at the peak. The oracle is the
// file: the profiled spot of 20 cd, the file's peak (1000 cd times its
// multiplier 0.02), equals the same spot without a profile times the
// file's value over 1000 toward each wall point, its vertical angle from
// the direction and its horizontal angle counterclockwise on screen from
// `zero_azimuth` +X, so +X takes the 0° plane, +Y the 90°, -X the 180° and
// -Y the 270°. The wall's middle row and column lie in those planes, where
// the values are the file's on each, linear between its 10° steps; every
// other point inside the cone takes the four measured values about it.
#[test]
fn a_profile_shines_the_file_s_candela_toward_each_point() {
    let Some(mut stage) = Stage::wall(spot(None)) else {
        return;
    };
    let plain = stage.shot(spot(None));
    let dark = stage.shot(off(spot(None)));
    let profile = asymmetric();
    assert!((profile.peak_candela() - PEAK).abs() <= 1e-4 * PEAK);
    let rig = &mut stage.rig;
    let id = rig.scene.add_light_profile(profile);
    let profiled = stage.shot(spot(Some(SpotProfile {
        profile: id,
        zero_azimuth: Vec3::X,
    })));
    let mut dump = Dump::new("a_profile_shines_the_file_s_candela_toward_each_point");
    dump.image("plain", &plain);
    dump.image("profiled", &profiled);
    let cone = Region::from_fn(VIEW.size, in_cone);
    assert_lit(&plain, &dark, &cone, "the plain spot");
    let (row, column) = principal_planes();
    // Each side's plane by the file's convention, written out.
    let side = |across: Vec2| {
        if across.x.abs() > across.y.abs() {
            if across.x > 0. { 0. } else { 180. }
        } else if across.y > 0. {
            90.
        } else {
            270.
        }
    };
    assert_profiled(&profiled, &plain, &row, side, "the 0° and 180° planes");
    assert_profiled(&profiled, &plain, &column, side, "the 90° and 270° planes");
    let compared = assert_profiled(&profiled, &plain, &cone, azimuth_from(Vec2::X), "the cone");
    dump.metric("cone pixels", compared);
}

// Plausible defects: a profile that replaces the spot's cone rather than
// multiplying it (as three.js r187's IES spot does), so light reaches past
// the outer angle or skips the falloff between the angles; or a uniform
// profile that is not exactly one. The oracle is the contract's product:
// a profile of 1000 cd in every direction is 1 everywhere, so the profiled
// spot's frame equals the plain spot's over the whole wall, inside its
// inner cone, across its falloff and, dark, beyond its outer angle, which
// this spot's narrow cone (0.4 and 0.5 rad, 2.2 m across at the wall)
// leaves most of the wall in.
#[test]
fn a_uniform_profile_leaves_the_spot_and_its_cone_as_they_were() {
    let narrow = |profile| Light {
        shape: LightShape::Spot {
            direction: Vec3::NEG_Z,
            inner_angle: 0.4,
            outer_angle: 0.5,
            radius: 0.,
        },
        ..spot(profile)
    };
    let Some(mut stage) = Stage::wall(narrow(None)) else {
        return;
    };
    let plain = stage.shot(narrow(None));
    let rig = &mut stage.rig;
    let uniform = LightProfile::from_ies(&lm63(&[0., 90.], &[0.], &[&[1000., 1000.]], 1.)).unwrap();
    let id = rig.scene.add_light_profile(uniform);
    let profiled = stage.shot(narrow(Some(SpotProfile {
        profile: id,
        zero_azimuth: Vec3::X,
    })));
    let dump = Dump::new("a_uniform_profile_leaves_the_spot_and_its_cone_as_they_were");
    dump.image("plain", &plain);
    dump.image("profiled", &profiled);
    let beyond = Region::from_fn(VIEW.size, |pixel| polar(on_wall(pixel)).0 > 30.);
    assert!(beyond.len() > 1000, "the wall beyond the cone is too small");
    assert_lit(
        &plain,
        &black(),
        &Region::from_fn(VIEW.size, |pixel| polar(on_wall(pixel)).0 < 20.),
        "the plain spot",
    );
    assert_agrees(
        &profiled,
        &plain,
        &Region::from_fn(VIEW.size, |_| true),
        "the uniformly profiled spot",
    );
}

// Plausible defects: `zero_azimuth` ignored, taken as the 90° axis, or
// turning the profile the other way. The oracle is the convention: with
// `zero_azimuth` +Y, the 0° plane lies up the wall, the 90° plane
// counterclockwise from it on screen, to the left (-X), the 180° plane
// down and the 270° plane right, so the middle row's right takes the
// file's 270° plane and its left the 90°.
#[test]
fn a_profile_turns_with_its_zero_azimuth() {
    let Some(mut stage) = Stage::wall(spot(None)) else {
        return;
    };
    let plain = stage.shot(spot(None));
    let rig = &mut stage.rig;
    let id = rig.scene.add_light_profile(asymmetric());
    let turned = stage.shot(spot(Some(SpotProfile {
        profile: id,
        zero_azimuth: Vec3::Y,
    })));
    let dump = Dump::new("a_profile_turns_with_its_zero_azimuth");
    dump.image("plain", &plain);
    dump.image("turned", &turned);
    let (row, column) = principal_planes();
    let side = |across: Vec2| {
        if across.x.abs() > across.y.abs() {
            if across.x > 0. { 270. } else { 90. }
        } else if across.y > 0. {
            0.
        } else {
            180.
        }
    };
    assert_profiled(&turned, &plain, &row, side, "the middle row, turned");
    assert_profiled(&turned, &plain, &column, side, "the middle column, turned");
}

// Plausible defects: a light without a profile shaded through the profile
// path once a profiled light makes the pipelines compile it in (a factor
// from another light's record, a stale lookup, a sample of the atlas for a
// light without one). The oracle is the contract's identity: a light
// without a profile multiplies by exactly one, so the ordinary spot's
// frame with a profiled spot in the scene, shining away from everything
// the camera sees, equals its frame with none.
#[test]
fn an_ordinary_spot_is_unchanged_beside_a_profiled_one() {
    let Some(mut stage) = Stage::wall(spot(None)) else {
        return;
    };
    let before = stage.frame();
    let rig = &mut stage.rig;
    let id = rig.scene.add_light_profile(asymmetric());
    rig.scene
        .add_light(
            &rig.gpu.device,
            &rig.gpu.queue,
            Light {
                position: Vec3::new(0., 0., 10.),
                shape: LightShape::Spot {
                    direction: Vec3::Z,
                    inner_angle: 0.3,
                    outer_angle: 0.4,
                    radius: 0.,
                },
                intensity: 100.,
                range: 2.,
                profile: Some(SpotProfile {
                    profile: id,
                    zero_azimuth: Vec3::X,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    let after = stage.frame();
    let dump = Dump::new("an_ordinary_spot_is_unchanged_beside_a_profiled_one");
    dump.image("before", &before);
    dump.image("after", &after);
    let everywhere = Region::from_fn(VIEW.size, |_| true);
    assert_lit(
        &before,
        &black(),
        &Region::from_fn(VIEW.size, in_cone),
        "the spot",
    );
    assert_agrees(&after, &before, &everywhere, "the ordinary spot");
}

// Plausible defect: a light's record left naming its profile's place in
// an atlas that has since been packed anew, so after an image joins the
// atlas the light samples whatever now lies there. The oracle is the
// record's invariant and the file: after a decal brings in a large image,
// which the packing places first and so moves the profile, the frame is
// still the spot's without a profile times the file's values, as before
// (a_profile_shines_the_file_s_candela_toward_each_point); the light is
// not edited between them. Equality with the frame before is not the
// oracle: the atlas's new size moves where the sampler's quantised
// weights fall. The decal lies 30 m behind the camera, where it reaches
// no visible surface, and its image is red, so a stale place changes the
// light's colour.
#[test]
fn a_profile_survives_its_atlas_being_packed_anew() {
    let Some(mut stage) = Stage::wall(spot(None)) else {
        return;
    };
    let plain = stage.shot(spot(None));
    let rig = &mut stage.rig;
    let id = rig.scene.add_light_profile(asymmetric());
    let light = spot(Some(SpotProfile {
        profile: id,
        zero_azimuth: Vec3::X,
    }));
    let before = stage.shot(light);
    let rig = &mut stage.rig;
    let red = rig
        .scene
        .add_decal_image(Picture::Rgba8(image::RgbaImage::from_pixel(
            256,
            256,
            image::Rgba([255, 0, 0, 255]),
        )))
        .unwrap();
    rig.scene
        .add_decal(
            &rig.gpu.device,
            &rig.gpu.queue,
            Decal {
                position: Vec3::new(0., 0., 30.),
                ..Decal::new(red)
            },
        )
        .unwrap();
    let after = stage.frame();
    let dump = Dump::new("a_profile_survives_its_atlas_being_packed_anew");
    dump.image("before", &before);
    dump.image("after", &after);
    let cone = Region::from_fn(VIEW.size, in_cone);
    assert_lit(&before, &black(), &cone, "the profiled spot");
    assert_profiled(
        &after,
        &plain,
        &cone,
        azimuth_from(Vec2::X),
        "after the repack",
    );
}

// Plausible defects: the medium taking a spot's light without its profile
// (the fog's injection compiled without it, or its own light path), or
// taking it mirrored. The oracle is the profile's identity in the medium:
// a fog volume around a spot shining down (-Y), its profile whole (1000
// cd) from 280° round through 0° to 80° and dark from 100° to 260°, its
// 0° toward +X, so its lit side is right of the beam's axis (the 90°
// plane lies along +X × -Y = -Z, away from the camera, and the 270° toward
// it). A view ray more than 16 pixels right of the frame's centre column
// meets the medium (5 m to 7 m away, where 16 pixels span at least 0.83
// m) only where the horizontal angle is within 51° of 0°, whose fog
// equals the light's without a profile, and one more than 16 pixels left
// only beyond 129°, whose fog equals the light's off. The
// margin covers the volume's froxels, its filter and the profile's ramps
// between 80° and 100° and between 260° and 280°.
#[test]
fn a_profile_shapes_its_light_in_the_fog() {
    let beam = Vec3::new(0., 3., -6.);
    let light = Light {
        position: beam,
        shape: LightShape::Spot {
            direction: Vec3::NEG_Y,
            inner_angle: 0.45,
            outer_angle: 0.5,
            radius: 0.,
        },
        intensity: 20.,
        range: 12.,
        ..Default::default()
    };
    let Some(mut stage) = Stage::new(Vec::new(), light) else {
        return;
    };
    let rig = &mut stage.rig;
    rig.scene
        .update_fog_volumes(
            &rig.gpu.device,
            &rig.gpu.queue,
            &[FogVolume {
                center: Vec3::new(0., 0., -6.),
                size: Vec3::new(5., 3., 2.),
                density: 1.,
                edge_fade: 0.,
                ..Default::default()
            }],
        )
        .unwrap();
    rig.views[0].settings.atmosphere = true;
    rig.views[0].input.atmosphere = true;
    let horizontal: Vec<f64> = (0..=36).map(|step| f64::from(step) * 10.).collect();
    let planes: Vec<[f64; 2]> = horizontal
        .iter()
        .map(|&angle| {
            let lit = !(100. ..=260.).contains(&angle);
            [if lit { 1000. } else { 0. }; 2]
        })
        .collect();
    let planes: Vec<&[f64]> = planes.iter().map(|plane| plane.as_slice()).collect();
    let half = LightProfile::from_ies(&lm63(&[0., 90.], &horizontal, &planes, 1.)).unwrap();
    let without = stage.shot(light);
    let dark = stage.shot(off(light));
    let rig = &mut stage.rig;
    let id = rig.scene.add_light_profile(half);
    let with = stage.shot(Light {
        profile: Some(SpotProfile {
            profile: id,
            zero_azimuth: Vec3::X,
        }),
        ..light
    });
    let dump = Dump::new("a_profile_shapes_its_light_in_the_fog");
    dump.image("without", &without);
    dump.image("profiled", &with);
    let camera = VIEW.camera(Pose::at(Vec3::ZERO));
    let in_beam = |pixel: [u32; 2]| {
        let ray = VIEW.ray(&camera, centre(pixel));
        (0..=64).any(|step| {
            let z = -5. - 2. * step as f32 / 64.;
            let point = ray * (z / ray.z);
            let depth = beam.y - point.y;
            point.y.abs() < 1.4
                && Vec2::new(point.x - beam.x, point.z - beam.z).length()
                    < 0.9 * depth * 0.45f32.tan()
        })
    };
    let middle = VIEW.size[0] / 2;
    let lit = Region::from_fn(VIEW.size, |pixel| pixel[0] >= middle + 16 && in_beam(pixel));
    let unlit = Region::from_fn(VIEW.size, |pixel| pixel[0] + 16 < middle && in_beam(pixel));
    assert_lit(&without, &dark, &lit, "the plain beam's right");
    assert_lit(&without, &dark, &unlit, "the plain beam's left");
    assert_agrees(&with, &without, &lit, "the fog on the profile's lit side");
    assert_agrees(&with, &dark, &unlit, "the fog on the profile's dark side");
}
