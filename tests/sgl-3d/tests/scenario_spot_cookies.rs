//! Spot lights' cookies (`Light::cookie`, `LightCookie`) in the scenario
//! harness (`specs/testing.md`, "SGL3D scenario harness"): a spot projects
//! an image that multiplies its radiance wherever it reaches, on surfaces
//! and in the volumetric fog, independently of its shadow and of every
//! other light (the architecture's Lights and shadows). Every oracle is an
//! equivalence between frames of one scene: a white cookie against none,
//! the half of a split image that is white against the uncookied light and
//! the half that is black against the light off, and one world point seen
//! from two cameras. No temporal feature runs: antialiasing is off and
//! every frame is a camera cut, so the fog keeps no history and its froxels
//! do not jitter; each frame is its scene's alone.
//!
//! Coverage (requirement 3): at rest, under camera translation (the
//! projection stays on the world), across an atlas repack, and beside a
//! second light and a caster's shadow. A cookie has no setting or level, so
//! none is covered. Not covered: an object moving through the projection,
//! which is a function of world position alone that the camera's
//! translation already exercises; a resize, which changes only the
//! footprint that selects the image's level; a cut, which every frame is.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::{Image, Region, centre};
use scenario::rig::{Dump, HALF_STEP, Rig, gpu};
use sgl_3d::asset::{Asset, CpuMesh, Image as Picture, Material, Vertex};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;
use sgl_3d::{Decal, DecalImageId, FogVolume, Light, LightCookie, LightId, LightShape};

/// The camera's lens: a vertical field of view whose half-angle's tangent
/// is 1/2, so a pixel spans 1/16 m on the wall 6 m ahead (96 rows) and a
/// camera moved 0.5 m sideways moves the wall by 8 whole pixels.
const VIEW: View = View {
    size: [128, 96],
    fov_y: 0.927_295_2,
    near: 0.05,
};
/// The wall's plane, facing +Z, 6 m ahead of the camera at the origin.
const WALL_Z: f32 = -6.;
/// The spot's position, 4 m in front of the wall, shining at it along -Z.
const SPOT: Vec3 = Vec3::new(0., 0., -2.);
const INNER: f32 = 0.45;
const OUTER: f32 = 0.5;
/// The texels on a side of the cookies.
const COOKIE: u32 = 64;

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

/// A white, fully rough dielectric: a Lambertian receiver, the same from
/// every view under a light without specular.
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

/// A `COOKIE`-square image whose texel at `[x, y]` (from its top left) is
/// `texel(x, y)`.
fn picture(texel: impl Fn(u32, u32) -> [u8; 4]) -> Picture {
    Picture::Rgba8(image::RgbaImage::from_fn(COOKIE, COOKIE, |x, y| {
        image::Rgba(texel(x, y))
    }))
}

const WHITE: [u8; 4] = [255; 4];
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// White and opaque throughout.
fn white() -> Picture {
    picture(|_, _| WHITE)
}

/// Black on its left half, white on its right.
fn split() -> Picture {
    picture(|x, _| if x < COOKIE / 2 { BLACK } else { WHITE })
}

/// White on its top half, black on its bottom.
fn split_rows() -> Picture {
    picture(|_, y| if y < COOKIE / 2 { WHITE } else { BLACK })
}

/// A checker of 8 by 8 texel squares, white at its top left.
fn checker() -> Picture {
    picture(|x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            WHITE
        } else {
            BLACK
        }
    })
}

/// The spot shining at the wall, diffuse only, projecting `cookie` with its
/// top toward `up`.
fn spot(cookie: Option<DecalImageId>, up: Vec3) -> Light {
    Light {
        position: SPOT,
        shape: LightShape::Spot {
            direction: Vec3::NEG_Z,
            inner_angle: INNER,
            outer_angle: OUTER,
            radius: 0.,
        },
        intensity: 20.,
        range: 12.,
        specular: 0.,
        cookie: cookie.map(|image| LightCookie { image, up }),
        ..Default::default()
    }
}

/// `light`, off.
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

    /// The wall, 12 by 9 m, filling the view, lit by `light`.
    fn wall(light: Light) -> Option<Stage> {
        Self::new(vec![wall_mesh()], light)
    }

    fn image(&mut self, picture: Picture) -> DecalImageId {
        self.rig.scene.add_decal_image(picture).unwrap()
    }

    /// The linear scene of a camera cut with the light set to `light`.
    fn shot(&mut self, light: Light) -> Image {
        let rig = &mut self.rig;
        rig.scene
            .set_light(&rig.gpu.queue, self.light, light)
            .unwrap();
        self.frame()
    }

    /// The linear scene of a camera cut.
    fn frame(&mut self) -> Image {
        let rig = &mut self.rig;
        rig.views[0].input.camera_cut = true;
        rig.render(0);
        rig.read(DiagnosticTarget::Composite)
    }
}

fn wall_mesh() -> CpuMesh {
    quad(Vec3::new(0., 0., WALL_Z), Vec2::new(6., 4.5))
}

/// Where the camera at `eye` sees the wall through pixel `pixel`.
fn on_wall(eye: Vec3, pixel: [u32; 2]) -> Vec3 {
    let camera = VIEW.camera(Pose::at(eye));
    let ray = VIEW.ray(&camera, centre(pixel));
    eye + ray * ((WALL_Z - eye.z) / ray.z)
}

/// The pixels of the camera at the origin whose wall point `includes`
/// takes.
fn wall_region(includes: impl Fn(Vec3) -> bool) -> Region {
    Region::from_fn(VIEW.size, |pixel| includes(on_wall(Vec3::ZERO, pixel)))
}

/// Whether a wall point lies well inside the spot's inner cone (nine
/// tenths of its radius there), where the light is whole and its cookie
/// several texels from the image's edges.
fn in_cone(point: Vec3) -> bool {
    let depth = SPOT.z - point.z;
    (point - SPOT).truncate().length() < 0.9 * depth * INNER.tan()
}

/// The wall points well inside the cone and more than `margin` metres
/// right (`right`) or left of its axis.
fn half(right: bool, margin: f32) -> Region {
    wall_region(move |point| {
        let across = point.x - SPOT.x;
        in_cone(point)
            && if right {
                across > margin
            } else {
                across < -margin
            }
    })
}

/// The wall points well inside the cone and more than `margin` metres
/// above (`above`) or below its axis.
fn half_rows(above: bool, margin: f32) -> Region {
    wall_region(move |point| {
        let up = point.y - SPOT.y;
        in_cone(point) && if above { up > margin } else { up < -margin }
    })
}

/// How far from a split the halves are compared: two of the cookie's
/// texels on the wall (the image spans 2 tan(outer) times 4 m in
/// `COOKIE`), a bilinear tap and the next level's beside the edge.
fn split_margin() -> f32 {
    2. * 2. * 4. * OUTER.tan() / COOKIE as f32
}

/// A frame of no light.
fn black() -> Image {
    Image {
        size: VIEW.size,
        texels: vec![[0.; 4]; (VIEW.size[0] * VIEW.size[1]) as usize],
    }
}

/// Where `image` departs from `oracle` over `region` by more than one
/// half-float step of the larger value in any colour channel (the most a
/// store into the Rgba16Float target changes a value: two frames that
/// shade a point alike agree to it), and by how much; none where they
/// agree.
fn departure(image: &Image, oracle: &Image, region: &Region) -> Option<([u32; 2], f32, f32)> {
    assert!(!region.is_empty(), "a comparison over no pixels");
    region.iter().find_map(|pixel| {
        let (a, b) = (image.at(pixel), oracle.at(pixel));
        (0..3)
            .find(|&c| (a[c] - b[c]).abs() > HALF_STEP * a[c].abs().max(b[c].abs()))
            .map(|c| (pixel, a[c], b[c]))
    })
}

/// Asserts `image` agrees with `oracle` over `region` (`departure`).
fn assert_agrees(image: &Image, oracle: &Image, region: &Region, what: &str) {
    if let Some((pixel, got, want)) = departure(image, oracle, region) {
        panic!("{what}: {got} at {pixel:?}, where the oracle frame holds {want}");
    }
}

/// Asserts the light under test brings light to every pixel of `region`:
/// `lit` exceeds `unlit` there by more than rounding, so an equivalence
/// over it tells the light's presence from its absence.
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

// Plausible defects: a cookie that scales a light even where its image is
// white (a gain, a sample outside the image's place in the atlas, a lit
// pipeline whose cookie path changes shading whatever its image), or one
// that leaves part of the cone unscaled. The
// oracle is the contract's identity: a white opaque cookie multiplies the
// light by one, so the frame equals the frame without a cookie over the
// light's whole inner cone. The cone's outer falloff is left out: where the
// cone's circle meets the image's square, the atlas's coarser levels blend
// the image's edge with its transparent border, as Godot's atlas does,
// while the light fades out.
#[test]
fn a_white_cookie_leaves_the_light_as_without_one() {
    let Some(mut stage) = Stage::wall(spot(None, Vec3::Y)) else {
        return;
    };
    let without = stage.shot(spot(None, Vec3::Y));
    let dark = stage.shot(off(spot(None, Vec3::Y)));
    let image = stage.image(white());
    let with = stage.shot(spot(Some(image), Vec3::Y));
    let dump = Dump::new("a_white_cookie_leaves_the_light_as_without_one");
    dump.image("without", &without);
    dump.image("white", &with);
    let cone = wall_region(in_cone);
    assert_lit(&without, &dark, &cone, "the uncookied spot");
    assert_agrees(&with, &without, &cone, "a white cookie");
}

// Plausible defects: a cookie's texel read as linear rather than sRGB
// colour, its alpha ignored or taken as one of its colours, or its channels
// crossed. The oracle is the contract's rule with the sRGB transfer
// function (IEC 61966-2-1): under a cookie of one texel throughout, each
// colour channel of the frame over the cone is the uncookied frame's times
// that channel decoded from sRGB times the alpha, to within the atlas's
// half-float texel (one step) and the frame's store (another).
#[test]
fn a_cookie_scales_its_light_by_its_colour_times_its_alpha() {
    let Some(mut stage) = Stage::wall(spot(None, Vec3::Y)) else {
        return;
    };
    let without = stage.shot(spot(None, Vec3::Y));
    let dark = stage.shot(off(spot(None, Vec3::Y)));
    let texel = [188u8, 94, 255, 128];
    let image = stage.image(picture(|_, _| texel));
    let with = stage.shot(spot(Some(image), Vec3::Y));
    let decoded = |value: u8| {
        let value = f32::from(value) / 255.;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let alpha = f32::from(texel[3]) / 255.;
    let scale: [f32; 3] = std::array::from_fn(|c| decoded(texel[c]) * alpha);
    let cone = wall_region(in_cone);
    assert_lit(&without, &dark, &cone, "the uncookied spot");
    for pixel in cone.iter() {
        let (got, base) = (with.at(pixel), without.at(pixel));
        for c in 0..3 {
            let want = base[c] * scale[c];
            assert!(
                (got[c] - want).abs() <= 2. * HALF_STEP * want,
                "channel {c} at {pixel:?} is {}, not {want}",
                got[c]
            );
        }
    }
}

// Plausible defects: a cookie mirrored or turned (its right taken as up
// crossed with the direction, so the white half lands on the wrong side),
// stretched about the axis, or blurred across the cone; a
// black half that still lets light through. The oracle is the projection
// rule and the cookie's identities: with `up` +Y the image's right is the
// spot's direction crossed with it, -Z × +Y = +X, so the split image's
// white right half lies right of the axis on the wall, where the frame
// equals the uncookied spot's, and its black left half left of it, where
// the frame equals the spot's off; with `up` -Y the image turns half a turn
// and the halves trade sides. The halves are compared from two texels past
// the split (`split_margin`), clear of the filter's taps across it.
#[test]
fn a_split_cookie_lights_the_half_its_image_is_white() {
    let Some(mut stage) = Stage::wall(spot(None, Vec3::Y)) else {
        return;
    };
    let without = stage.shot(spot(None, Vec3::Y));
    let dark = stage.shot(off(spot(None, Vec3::Y)));
    let image = stage.image(split());
    let mut dump = Dump::new("a_split_cookie_lights_the_half_its_image_is_white");
    dump.image("without", &without);
    for (up, white_right, name) in [(Vec3::Y, true, "up"), (Vec3::NEG_Y, false, "turned")] {
        let with = stage.shot(spot(Some(image), up));
        dump.image(name, &with);
        let (lit, unlit) = (
            half(white_right, split_margin()),
            half(!white_right, split_margin()),
        );
        dump.metric(format!("{name} pixels"), lit.len().min(unlit.len()));
        assert_lit(&without, &dark, &lit, name);
        assert_lit(&without, &dark, &unlit, name);
        assert_agrees(&with, &without, &lit, &format!("{name}: the white half"));
        assert_agrees(&with, &dark, &unlit, &format!("{name}: the black half"));
    }
}

// Plausible defects: a cookie upside down (its v not flipped from the
// light's up, or its image's rows reversed on the way into the atlas),
// which a left and right split cannot tell from one the right way up. The
// oracle is the projection rule: an image's top lies toward `up`, so with
// `up` +Y the top-white image lights the wall above the spot's axis, where
// the frame equals the uncookied spot's, and leaves it dark below, where
// the frame equals the spot's off; with `up` -Y the halves trade places.
#[test]
fn a_cookie_s_top_lies_toward_its_up() {
    let Some(mut stage) = Stage::wall(spot(None, Vec3::Y)) else {
        return;
    };
    let without = stage.shot(spot(None, Vec3::Y));
    let dark = stage.shot(off(spot(None, Vec3::Y)));
    let image = stage.image(split_rows());
    let mut dump = Dump::new("a_cookie_s_top_lies_toward_its_up");
    dump.image("without", &without);
    for (up, white_above, name) in [(Vec3::Y, true, "up"), (Vec3::NEG_Y, false, "turned")] {
        let with = stage.shot(spot(Some(image), up));
        dump.image(name, &with);
        let (lit, unlit) = (
            half_rows(white_above, split_margin()),
            half_rows(!white_above, split_margin()),
        );
        dump.metric(format!("{name} pixels"), lit.len().min(unlit.len()));
        assert_lit(&without, &dark, &lit, name);
        assert_lit(&without, &dark, &unlit, name);
        assert_agrees(&with, &without, &lit, &format!("{name}: the white half"));
        assert_agrees(&with, &dark, &unlit, &format!("{name}: the black half"));
    }
}

// Plausible defects: the medium taking a spot's light without its cookie
// (the fog's own light path, or a sample along the fog's footprint that
// reads beyond the image), or taking it mirrored. The oracle is the same
// identity in the medium: a fog volume around a spot shining down, its
// split image's top toward -Z (away from the camera) so its white half is
// right of the axis, -Y × -Z = +X, in front of a black backdrop with no
// surface. A view ray right of the frame's centre column meets only medium
// right of the beam's axis (the axis lies in the plane x = 0 through the
// camera), so the pixels right of it see the uncookied light's fog and
// those left of it the fog with the light off. The comparison keeps 16
// pixels from the centre column: the volume's froxels are under two
// pixels wide, the fog's Gaussian filter reaches three froxels, and a
// froxel's cookie sample spans about two of the image's texels.
#[test]
fn a_split_cookie_lights_the_half_of_the_fog_its_image_is_white() {
    let beam = Vec3::new(0., 3., -6.);
    let light = Light {
        position: beam,
        shape: LightShape::Spot {
            direction: Vec3::NEG_Y,
            inner_angle: INNER,
            outer_angle: OUTER,
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
    let volume = FogVolume {
        center: Vec3::new(0., 0., -6.),
        size: Vec3::new(5., 3., 2.),
        density: 1.,
        edge_fade: 0.,
        ..Default::default()
    };
    rig.scene
        .update_fog_volumes(&rig.gpu.device, &rig.gpu.queue, &[volume])
        .unwrap();
    rig.views[0].settings.atmosphere = true;
    rig.views[0].input.atmosphere = true;
    let without = stage.shot(light);
    let dark = stage.shot(off(light));
    let image = stage.image(split());
    let with = stage.shot(Light {
        cookie: Some(LightCookie {
            image,
            up: Vec3::NEG_Z,
        }),
        ..light
    });
    let dump = Dump::new("a_split_cookie_lights_the_half_of_the_fog_its_image_is_white");
    dump.image("without", &without);
    dump.image("split", &with);
    // The pixels whose ray crosses the volume inside nine tenths of the
    // inner cone: where the beam lights the medium.
    let camera = VIEW.camera(Pose::at(Vec3::ZERO));
    let in_beam = |pixel: [u32; 2]| {
        let ray = VIEW.ray(&camera, centre(pixel));
        (0..=64).any(|step| {
            let z = -5. - 2. * step as f32 / 64.;
            let point = ray * (z / ray.z);
            let depth = beam.y - point.y;
            point.y.abs() < 1.4
                && Vec2::new(point.x - beam.x, point.z - beam.z).length()
                    < 0.9 * depth * INNER.tan()
        })
    };
    let middle = VIEW.size[0] / 2;
    let lit = Region::from_fn(VIEW.size, |pixel| pixel[0] >= middle + 16 && in_beam(pixel));
    let unlit = Region::from_fn(VIEW.size, |pixel| pixel[0] + 16 < middle && in_beam(pixel));
    assert_lit(&without, &dark, &lit, "the uncookied beam's right");
    assert_lit(&without, &dark, &unlit, "the uncookied beam's left");
    assert_agrees(&with, &without, &lit, "the fog under the white half");
    assert_agrees(&with, &dark, &unlit, "the fog under the black half");
}

// Plausible defect: the cookie projected from the camera rather than the
// light (a view-space or camera-relative position, a screen-space lookup),
// so the pattern slides over the world as the camera moves. The oracle is
// the projection's independence of the view: a Lambertian wall under a
// light without specular has one radiance at each world point, whatever
// camera sees it. A camera moved 0.5 m right sees through pixel [x, y] the
// wall point the camera at the origin sees through [x + 8, y] (`VIEW`), so
// under a checker cookie, whose squares a sliding projection would carry
// half their width, the two frames agree pixel for shifted pixel over the
// cone.
#[test]
fn a_cookie_stays_on_the_world_as_the_camera_moves() {
    let Some(mut stage) = Stage::wall(spot(None, Vec3::Y)) else {
        return;
    };
    let image = stage.image(checker());
    let light = spot(Some(image), Vec3::Y);
    let here = stage.shot(light);
    let moved = Vec3::new(0.5, 0., 0.);
    stage.rig.look(Pose::at(moved));
    let there = stage.shot(light);
    let dump = Dump::new("a_cookie_stays_on_the_world_as_the_camera_moves");
    dump.image("here", &here);
    dump.image("there", &there);
    let shift = 8;
    let mut compared = 0;
    for pixel in scenario::image::pixels(VIEW.size) {
        let point = on_wall(moved, pixel);
        let [x, y] = pixel;
        if x + shift >= VIEW.size[0] || !in_cone(point) {
            continue;
        }
        let seen = [x + shift, y];
        let (a, b) = (there.at(pixel), here.at(seen));
        for c in 0..3 {
            assert!(
                (a[c] - b[c]).abs() <= HALF_STEP * a[c].abs().max(b[c].abs()),
                "{point} is {} from the moved camera, {} from the first",
                a[c],
                b[c]
            );
        }
        compared += 1;
    }
    assert!(compared > 1000, "only {compared} wall points compared");
}

// Plausible defects: a cookie applied to every light a receiver takes (the
// sample's index confused, the factor kept across the light loop), or one
// that changes its light's shadow (a lookup skipped, moved or replaced for a
// cookied light). The oracle is the contract's independence: a split cookie
// on a shadowed spot, a small card between it and the wall whose shadow
// falls in the white half, and a second, uncookied point light near the
// wall in the black half. Over the white half the frame equals the spot's
// without a cookie, shadow and all; over the black half it equals the
// frame with the spot off, which the second light still lights.
#[test]
fn a_cookie_leaves_other_lights_and_its_shadow_alone() {
    // The card 1 m from the spot, 0.3 m right of its axis: its shadow on
    // the wall, 4 m from the spot, is four times as far out and as wide,
    // centred 1.2 m right, apart on screen from the card itself.
    let card = quad(Vec3::new(0.3, 0., -3.), Vec2::splat(0.08));
    let shadowed = |cookie| Light {
        casts_shadow: true,
        ..spot(cookie, Vec3::Y)
    };
    let Some(mut stage) = Stage::new(vec![wall_mesh(), card], shadowed(None)) else {
        return;
    };
    let rig = &mut stage.rig;
    let other = Vec3::new(-1.2, 0., -5.5);
    rig.scene
        .add_light(
            &rig.gpu.device,
            &rig.gpu.queue,
            Light {
                position: other,
                intensity: 1.,
                range: 1.5,
                specular: 0.,
                ..Default::default()
            },
        )
        .unwrap();
    let without = stage.shot(shadowed(None));
    let dark = stage.shot(off(shadowed(None)));
    let image = stage.image(split());
    let with = stage.shot(shadowed(Some(image)));
    let dump = Dump::new("a_cookie_leaves_other_lights_and_its_shadow_alone");
    dump.image("without", &without);
    dump.image("off", &dark);
    dump.image("split", &with);
    let margin = split_margin();
    let (lit, unlit) = (half(true, margin), half(false, margin));
    // The fixture holds what the oracle needs: a shadow inside the white
    // half (its umbra, 0.12 m in from the shadow's edges, darker than half
    // the white half's light) and the second light's light in the black
    // half.
    let umbra = wall_region(|point| (point.x - 1.2).abs() < 0.2 && point.y.abs() < 0.2);
    let mean = |image: &Image, region: &Region| {
        region.iter().map(|pixel| image.value(pixel)).sum::<f32>() / region.len() as f32
    };
    assert!(
        mean(&without, &umbra) < 0.5 * mean(&without, &lit),
        "no shadow in the white half"
    );
    let near_other = wall_region(|point| (point - other).truncate().length() < 0.8);
    assert_lit(&dark, &black(), &near_other.and(&unlit), "the second light");
    assert_agrees(&with, &without, &lit, "the white half, shadow and all");
    assert_agrees(&with, &dark, &unlit, "the black half, the second light's");
}

// Plausible defect: a light's record left naming its cookie's place in an
// atlas that has since been packed anew, so after another image joins the
// atlas the light samples whatever now lies there. The oracle is the
// record's invariant (a light's cookie names its image's place in the atlas
// bound with it): the frame after a decal brings in an image twice the
// cookie's size, which the packing places first and so moves the cookie,
// equals the frame before. The light is not edited between them. The decal
// lies 30 m behind the camera, where it reaches no visible surface, and its
// image is red, so a stale place changes the light's colour.
#[test]
fn a_cookie_survives_its_atlas_being_packed_anew() {
    let Some(mut stage) = Stage::wall(spot(None, Vec3::Y)) else {
        return;
    };
    let image = stage.image(split());
    let light = spot(Some(image), Vec3::Y);
    let before = stage.shot(light);
    let red = stage.image(Picture::Rgba8(image::RgbaImage::from_pixel(
        2 * COOKIE,
        2 * COOKIE,
        image::Rgba([255, 0, 0, 255]),
    )));
    let rig = &mut stage.rig;
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
    let dump = Dump::new("a_cookie_survives_its_atlas_being_packed_anew");
    dump.image("before", &before);
    dump.image("after", &after);
    assert_lit(
        &before,
        &black(),
        &half(true, split_margin()),
        "the white half",
    );
    let everywhere = Region::from_fn(VIEW.size, |_| true);
    assert_agrees(&after, &before, &everywhere, "the frame after the repack");
}
