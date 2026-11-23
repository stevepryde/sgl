//! The HDR effect suite of the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): `Scene::add_hdr_effect` and
//! `FrameInput::hdr_effect` (Frame item 12) through the public API, on
//! unlit planes at known metric depths, reading `DiagnosticTarget::HdrEffect`
//! (the effect's frame) against `DiagnosticTarget::Antialiased` (what it was
//! given), both linear. The effect is Beer-Lambert attenuation by distance,
//! `color × exp(−σ d)` with a coefficient `σ` a channel, as an underwater
//! colour shift takes it; the oracle is that attenuation computed from the
//! fixture: `d` is a plane's view depth over the cosine between the
//! pixel's view ray and the camera's axis, the ray from the lens's field of
//! view (`View::ray`). Each case runs alone by name:
//! `cargo test -p sgl-3d-tests --test scenario_hdr_effect
//! a_far_surface_is_attenuated_more_than_a_near_one`.
//!
//! The effect keeps no history: it is a function of the frame, its depth,
//! the camera and the effect's parameters, so moving objects, an
//! appearance and a departure reach it only as a different frame, and a
//! camera cut needs nothing of it; the suite runs at rest. The camera
//! reaches it through each pixel's ray and the eye, which a moved and
//! turned camera covers (each pixel placed in the world, the sky marked),
//! as an orthographic one does. The context's other values (exposure, the
//! pixel's place, time and phase past an hour) and the contract's
//! functions against the context are cases; so are FSR's render-size depth
//! under a larger frame, a resize, two views of one frame, and the stages
//! after reading the effect's frame. A view that draws a render texture is
//! `renderer::render_texture_tests`'. Not covered: an off-centre
//! projection, whose rays the inverse projection gives as it does these.
//!
//! Not covered: whether an effect looks right, which the owner judges.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::{Image, Region, centre};
use scenario::rig::{Gpu, Quad, Rig, Shape, fsr_gpu, gpu};
use scenario::temporal::SETTLE;
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec3, camera};
use sgl_3d::settings::{Antialiasing, DepthOfField, FsrQuality};
use sgl_3d::{DepthOfFieldParameters, Exposure, HdrEffect, ShaderSource};

/// A wide lens, so the distance along the view ray differs from the view
/// depth by up to a third toward the corners.
const LENS: View = View {
    size: [160, 96],
    fov_y: 1.2,
    near: 0.1,
};
/// The grey the sky shows where no plane covers it (none does here).
const SKY: f32 = 0.5;
/// The planes' radiance.
const RADIANCE: f32 = 2.;
/// Metres to the near plane, left of the centre column, and the far one,
/// right of it.
const NEAR: f32 = 2.;
const FAR: f32 = 9.;
/// Attenuation per metre, red, green and blue: blue light carries least
/// far, as under water it is red that goes first; any three serve.
const COEFFICIENT: [f32; 3] = [0.02, 0.08, 0.2];

/// The effect a game would write for an underwater colour shift by
/// distance (the package README's example takes the same form).
const ATTENUATION: &str = r#"
struct EffectParams {
 coefficient:vec3<f32>,
 unused:f32,
}
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 return color*exp(-params.coefficient*ctx.distance);
}
"#;

/// `ATTENUATION` added to `rig`'s scene, with `coefficient`.
fn attenuation(rig: &mut Rig, coefficient: [f32; 3]) -> HdrEffect {
    let effect = rig
        .scene
        .add_hdr_effect(ShaderSource {
            wgsl: ATTENUATION.into(),
            label: "attenuation".into(),
        })
        .unwrap();
    let block = [coefficient[0], coefficient[1], coefficient[2], 0.];
    rig.scene
        .hdr_effect(effect, bytemuck::cast_slice(&block))
        .unwrap()
}

/// An unlit plane facing the camera `depth` metres ahead, of `RADIANCE`,
/// from the view's centre column toward `side` (-1 left, +1 right), far
/// wider and taller than the view.
fn plane(depth: f32, side: f32) -> (Shape, Pose) {
    let extent = 4. * depth;
    let (low, high) = if side < 0. {
        (-extent, 0.)
    } else {
        (0., extent)
    };
    let quad = Quad {
        corners: [
            Vec3::new(low, -extent, 0.),
            Vec3::new(high, -extent, 0.),
            Vec3::new(high, extent, 0.),
            Vec3::new(low, extent, 0.),
        ],
        radiance: RADIANCE,
    };
    (
        Shape {
            quads: vec![quad],
            blend: None,
        },
        Pose::at(Vec3::new(0., 0., -depth)),
    )
}

/// A rig of `lens` under `antialiasing` with the near plane on the left
/// and the far one on the right, view 0 applying `ATTENUATION` at
/// `COEFFICIENT`.
fn rig(gpu: &Gpu, lens: View, antialiasing: Antialiasing) -> Rig {
    let mut rig = Rig::new(gpu, lens, antialiasing, SKY);
    let (near, at) = plane(NEAR, -1.);
    rig.add(&near, at, 0);
    let (far, at) = plane(FAR, 1.);
    rig.add(&far, at, 0);
    rig.views[0].input.hdr_effect = Some(attenuation(&mut rig, COEFFICIENT));
    rig
}

/// The view depth of the plane at pixel column `x` of a frame `width`
/// wide.
fn depth_at(x: u32, width: u32) -> f32 {
    if x < width / 2 { NEAR } else { FAR }
}

/// The metres along `lens`'s view ray through pixel `pixel`, from the eye
/// at the origin looking down -Z, to the plane at view depth `depth`.
fn distance(lens: &View, pixel: [u32; 2], depth: f32) -> f32 {
    let camera = lens.camera(Pose::at(Vec3::ZERO));
    let ray = lens.ray(&camera, centre(pixel));
    depth / -ray.z
}

/// The pixels of a frame `size` wide at least `margin` pixels from the
/// centre column, where the planes meet.
fn away_from_the_split(size: [u32; 2], margin: u32) -> Region {
    let split = size[0] / 2;
    Region::from_fn(size, |[x, _]| x + margin < split || x >= split + margin)
}

/// The largest error of `effect`, the effect's frame of `lens`, relative
/// to `given`, the antialiased frame it was given, attenuated by `σ` along
/// each pixel's ray to its plane, over `region`.
fn worst_error(lens: &View, given: &Image, effect: &Image, region: &Region) -> f32 {
    let mut worst = 0f32;
    for pixel in region.iter() {
        let d = distance(lens, pixel, depth_at(pixel[0], lens.size[0]));
        let [input, output] = [given.at(pixel), effect.at(pixel)];
        for c in 0..3 {
            let expected = input[c] * (-COEFFICIENT[c] * d).exp();
            worst = worst.max((output[c] - expected).abs() / expected);
        }
    }
    worst
}

/// What a half-float holds of a value: within 2^-11 of it, relative. The
/// given and the effect's frames are each stored in one.
const HALF_ROUNDING: f32 = 1. / 2048.;

// Plausible defects: the effect given the wrong distance (the view depth
// instead of the length along the ray, the depth of another pixel, or
// none), the wrong colour (exposed, tone-mapped or of another stage), or
// its result not reaching the frame. The oracle is the fixture's
// attenuation: each pixel's antialiased colour times exp(−σ d), d the
// plane's depth over the ray's cosine with the axis, from the lens's field
// of view; within the two half-floats' rounding. The far plane's pixels so
// keep less of their light than the near one's in every channel, by
// exp(−σ (d_far − d_near)), the more for blue.
#[test]
fn a_far_surface_is_attenuated_more_than_a_near_one() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    rig.render(0);
    let given = rig.read(DiagnosticTarget::Antialiased);
    let effect = rig.read(DiagnosticTarget::HdrEffect);
    let region = away_from_the_split(LENS.size, 1);
    let worst = worst_error(&LENS, &given, &effect, &region);
    assert!(
        worst <= 2. * HALF_ROUNDING,
        "off the fixture's attenuation by {worst}, relative"
    );
    let row = LENS.size[1] / 2;
    let [near, far] = [[LENS.size[0] / 4, row], [3 * LENS.size[0] / 4, row]];
    for c in 0..3 {
        let kept = |pixel: [u32; 2]| effect.at(pixel)[c] / given.at(pixel)[c];
        assert!(
            kept(far) < kept(near),
            "channel {c}: the far plane keeps {} of its light, the near one {}",
            kept(far),
            kept(near)
        );
    }
}

// Plausible defect: the effect reading the depth at its own pixel's
// position in the render-size target (or the frame's at the depth's), so
// under FSR, whose render size is half the frame's here, a pixel left of
// the centre reads the depth of one twice as far right, across the split
// onto the far plane. The oracle is the fixture's attenuation, as above,
// with FSR2 at its Performance quality (half the frame's size a side),
// away from the split by two render texels, which FSR's jitter and its
// resampling of the colour reach.
#[test]
fn under_fsr_each_pixel_reads_the_depth_under_it() {
    let Some(gpu) = fsr_gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Fsr2);
    rig.views[0].settings.fsr_quality = FsrQuality::Performance;
    for frame in 0..SETTLE {
        rig.render(frame);
    }
    let render = rig.render_size();
    assert_eq!(render, LENS.size.map(|s| s / 2), "FSR renders at half size");
    let given = rig.read(DiagnosticTarget::Antialiased);
    let effect = rig.read(DiagnosticTarget::HdrEffect);
    assert_eq!(
        effect.size, LENS.size,
        "the effect runs at the frame's size"
    );
    let region = away_from_the_split(LENS.size, 4);
    let worst = worst_error(&LENS, &given, &effect, &region);
    assert!(
        worst <= 2. * HALF_ROUNDING,
        "off the fixture's attenuation by {worst}, relative"
    );
}

// Plausible defect: the effect's target or frame values kept at the size
// they were made for, so a resized view applies its effect at the old size
// (a validation error) or with the old frame's rays. The oracle is the
// fixture's attenuation at the new size.
#[test]
fn a_resized_view_applies_its_effect_at_its_new_size() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    rig.render(0);
    let small = View {
        size: LENS.size.map(|s| s / 2),
        ..LENS
    };
    rig.views[0].lens = small;
    rig.render(1);
    let given = rig.read(DiagnosticTarget::Antialiased);
    let effect = rig.read(DiagnosticTarget::HdrEffect);
    assert_eq!(effect.size, small.size);
    let worst = worst_error(&small, &given, &effect, &away_from_the_split(small.size, 1));
    assert!(
        worst <= 2. * HALF_ROUNDING,
        "off the fixture's attenuation by {worst}, relative"
    );
}

// Plausible defect: state shared between views (one parameter buffer,
// pipeline map or target for the renderer), so the last view's effect or
// parameters apply to every view of a frame. The oracle is equivalence:
// two views of one frame of different sizes, one attenuating and one
// applying a second effect, each present what a rig of that view alone
// presents, and a third with no effect runs none.
#[test]
fn views_apply_their_own_effects() {
    let Some(gpu) = gpu() else {
        return;
    };
    let small = View {
        size: [96, 64],
        ..LENS
    };
    const DESATURATE: &str = r#"
struct EffectParams {
 amount:f32,
}
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 let grey=dot(color,vec3(0.2126,0.7152,0.0722));
 return mix(color,vec3(grey*0.5),params.amount);
}
"#;
    let desaturate = |rig: &mut Rig| {
        let effect = rig
            .scene
            .add_hdr_effect(ShaderSource {
                wgsl: DESATURATE.into(),
                label: "desaturate".into(),
            })
            .unwrap();
        rig.scene
            .hdr_effect(effect, &0.75f32.to_le_bytes())
            .unwrap()
    };
    let alone = {
        let mut first = rig(&gpu, LENS, Antialiasing::Off);
        first.render(0);
        let mut second = rig(&gpu, small, Antialiasing::Off);
        second.views[0].input.hdr_effect = Some(desaturate(&mut second));
        second.render(0);
        [
            first.read(DiagnosticTarget::HdrEffect),
            second.read(DiagnosticTarget::HdrEffect),
        ]
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    let second = rig.add_view(small, Antialiasing::Off);
    rig.views[second].input.hdr_effect = Some(desaturate(&mut rig));
    let third = rig.add_view(small, Antialiasing::Off);
    rig.views[third].input.hdr_effect = None;
    rig.render(0);
    assert!(
        rig.read_view(0, DiagnosticTarget::HdrEffect) == alone[0],
        "view 0 differs"
    );
    assert!(
        rig.read_view(second, DiagnosticTarget::HdrEffect) == alone[1],
        "view 1 differs"
    );
    assert!(
        rig.views[third]
            .view
            .diagnostic_target(DiagnosticTarget::HdrEffect)
            .is_none(),
        "the view without an effect ran one"
    );
}

// Plausible defect: the stages after the effect (depth of field, bloom and
// the tone map) reading the antialiased frame instead of the effect's, so
// the effect has no effect, or depth of field running first. The oracle
// is the stage order: under an effect that darkens to nothing, depth of
// field's focused frame is black, as is the tone-mapped frame (within its
// dither, an 8-bit code), though both planes and the sky are lit.
#[test]
fn the_stages_after_read_the_effects_frame() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    rig.views[0].input.hdr_effect = Some(attenuation(&mut rig, [1e4; 3]));
    rig.views[0].settings.depth_of_field = DepthOfField::Medium;
    rig.views[0].input.depth_of_field = DepthOfFieldParameters {
        focus_distance: NEAR,
        f_number: 1.,
        sensor_height: 0.024,
    };
    rig.views[0].settings.diagnostics.capture_tone_target = true;
    rig.render(0);
    let given = rig.read(DiagnosticTarget::Antialiased);
    assert!(
        given
            .texels
            .iter()
            .all(|texel| texel[0] >= SKY.min(RADIANCE)),
        "the fixture is lit"
    );
    let focused = rig.read(DiagnosticTarget::DepthOfField);
    assert!(
        focused.texels.iter().all(|texel| texel[..3] == [0.; 3]),
        "depth of field did not focus the effect's frame"
    );
    let tone_mapped = rig.read(DiagnosticTarget::ToneMapped);
    assert!(
        tone_mapped
            .texels
            .iter()
            .all(|texel| texel[..3].iter().all(|&c| c < 1. / 255.)),
        "the tone map did not take the effect's frame"
    );
}

/// `wgsl` added to `rig`'s scene and applied by view 0 with `params`.
fn apply(rig: &mut Rig, wgsl: &str, params: [f32; 4]) {
    let effect = rig
        .scene
        .add_hdr_effect(ShaderSource {
            wgsl: wgsl.into(),
            label: "context".into(),
        })
        .unwrap();
    rig.views[0].input.hdr_effect = Some(
        rig.scene
            .hdr_effect(effect, bytemuck::cast_slice(&params))
            .unwrap(),
    );
}

/// An effect that writes the pixel's world position, `eye + ray ×
/// distance`, offset by its parameters' xyz into positive values, and 0 at
/// the sky.
const WORLD: &str = r#"
struct EffectParams { offset:vec4<f32>, }
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 if ctx.view_depth>=SCENE_DEPTH_FAR {
  return vec3(0.);
 }
 return ctx.eye+ctx.ray*ctx.distance+params.offset.xyz;
}
"#;

// Plausible defects: the view's inverse not taken (or its transpose), the
// ray's y flipped or taken in view space, the eye of the render frame's
// origin rather than the camera's, or the sky given a distance. The
// oracle is the fixture: a camera 2 m right, 1.5 m up and 3 m back, turned
// 0.4 rad about +Y, sees a plane across z = −6, 14 m wide and 20 m high,
// and the sky about it; each pixel's ray from the lens's field of view and
// the camera's pose meets the plane where the effect must place it, and a
// pixel whose ray passes it shows the sky's mark. Pixels within 0.3 m of
// the plane's edges are left out; the half-float output holds positions
// offset by 20 m, 10 to 30, to within 0.008 m.
#[test]
fn the_context_places_each_pixel_in_the_world() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = Rig::new(&gpu, LENS, Antialiasing::Off, SKY);
    let [left, right, half] = [-10., 4., 10.];
    let quad = Quad {
        corners: [
            Vec3::new(left, -half, 0.),
            Vec3::new(right, -half, 0.),
            Vec3::new(right, half, 0.),
            Vec3::new(left, half, 0.),
        ],
        radiance: RADIANCE,
    };
    let shape = Shape {
        quads: vec![quad],
        blend: None,
    };
    rig.add(&shape, Pose::at(Vec3::new(0., 0., -6.)), 0);
    let pose = Pose {
        position: Vec3::new(2., 1.5, 3.),
        yaw: 0.4,
    };
    rig.look(pose);
    let offset = 20.;
    apply(&mut rig, WORLD, [offset, offset, offset, 0.]);
    rig.render(0);
    let effect = rig.read(DiagnosticTarget::HdrEffect);
    let camera = LENS.camera(pose);
    let (mut plane, mut sky) = (0, 0);
    for y in 0..LENS.size[1] {
        for x in 0..LENS.size[0] {
            let ray = LENS.ray(&camera, centre([x, y]));
            let hit = (ray.z < 0.).then(|| pose.position + ray * ((-6. - pose.position.z) / ray.z));
            let texel = effect.at([x, y]);
            let inside = |hit: Vec3, by: f32| {
                hit.x > left + by && hit.x < right - by && hit.y.abs() < half - by
            };
            match hit {
                Some(hit) if inside(hit, 0.3) => {
                    plane += 1;
                    for c in 0..3 {
                        let expected = hit[c] + offset;
                        assert!(
                            (texel[c] - expected).abs() <= 0.02,
                            "pixel {x},{y} channel {c}: {} for {expected}",
                            texel[c]
                        );
                    }
                }
                Some(hit) if inside(hit, -0.3) => {}
                _ => {
                    sky += 1;
                    assert_eq!(texel[..3], [0.; 3], "pixel {x},{y} is sky");
                }
            }
        }
    }
    assert!(
        plane > 1000 && sky > 1000,
        "{plane} plane and {sky} sky pixels"
    );
}

// Plausible defects: the exposure read from another texel or taken before
// the frame's stops, the contract's functions disagreeing with the
// context they say they give (a colour sampled half a pixel off, a depth
// of another texel), or the pixel's place in the wrong convention. The
// oracle is the contract: at an exposure of +1 stop the multiplier is 2;
// effect_color, effect_view_depth and effect_distance at the pixel's uv
// equal its colour, view depth and distance; uv × size is the pixel,
// whose centre lies at half a pixel; and size is the frame's, which the
// parameters carry. The effect writes the multiplier and the sums of the
// differences, which must read 2, and 0 and 0 to within f32 rounding, at
// every pixel, sky included.
#[test]
fn the_contracts_functions_and_values_agree_with_the_frame() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    rig.views[0].input.exposure = Exposure {
        stops: 1.,
        automatic: None,
    };
    let size = LENS.size.map(|s| s as f32);
    apply(
        &mut rig,
        r#"
struct EffectParams { size:vec4<f32>, }
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 let sampled=effect_color(ctx.uv);
 let colour=abs(sampled.r-color.r)+abs(sampled.g-color.g)+abs(sampled.b-color.b);
 let place=abs(ctx.uv.x*ctx.size.x-ctx.pixel.x)+abs(ctx.uv.y*ctx.size.y-ctx.pixel.y)
  +abs(fract(ctx.pixel.x)-0.5)+abs(fract(ctx.pixel.y)-0.5)
  +abs(ctx.size.x-params.size.x)+abs(ctx.size.y-params.size.y);
 let depth=abs(effect_distance(ctx.uv)-ctx.distance)+abs(effect_view_depth(ctx.uv)-ctx.view_depth);
 return vec3(ctx.exposure,colour+place,depth);
}
"#,
        [size[0], size[1], 0., 0.],
    );
    rig.render(0);
    let effect = rig.read(DiagnosticTarget::HdrEffect);
    for (i, texel) in effect.texels.iter().enumerate() {
        assert_eq!(texel[0], 2., "texel {i}'s exposure");
        // Within f32 rounding of uv × size and of bilinear weights at a
        // texel's centre: a few ulps at these magnitudes.
        assert!(
            texel[1] <= 1e-4 && texel[2] <= 1e-4,
            "texel {i}: {:?}",
            &texel[..3]
        );
    }
}

// Plausible defect: the phase not reaching the effect, or taken from the
// f32 time, which past an hour has lost the precision the phase keeps. The
// oracle is the contract: 3601.5 s into a session the time is 3601.5,
// which the half-float output holds to 2, and the phase is where it falls
// in the hour, 1.5 s, which the effect scales back to seconds.
#[test]
fn the_time_and_phase_reach_the_effect() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    apply(
        &mut rig,
        r#"
struct EffectParams { unused:vec4<f32>, }
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 return vec3(ctx.time,ctx.phase*3600.,0.);
}
"#,
        [0.; 4],
    );
    // Frames at 60 a second: 3601.5 s.
    rig.render(216_090);
    let texel = rig.read(DiagnosticTarget::HdrEffect).at([0, 0]);
    assert!((texel[0] - 3601.5).abs() <= 2., "time {}", texel[0]);
    assert!((texel[1] - 1.5).abs() <= 0.01, "phase {} s", texel[1]);
}

// Plausible defects: an orthographic camera's rays taken from its origin,
// so distances grow off the axis, or its sky given the far plane's depth
// (the device depth's clear value read as a surface). The oracle is the
// contract: under an orthographic projection a pixel's distance is its view
// depth, from the camera's plane, so each plane reads its own depth across
// it, and beyond the near plane's left edge, where neither plane lies,
// the sky reads SCENE_DEPTH_FAR, which the output holds as binary16's
// largest value.
#[test]
fn an_orthographic_camera_measures_from_its_plane() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off);
    // 24 m across: the near plane spans 8 m left of the centre.
    let half = [12., 12. * LENS.size[1] as f32 / LENS.size[0] as f32];
    rig.views[0].input.camera.projection =
        camera::rh::proj::directx::orthographic(-half[0], half[0], -half[1], half[1], 100., 0.1);
    apply(
        &mut rig,
        r#"
struct EffectParams { unused:vec4<f32>, }
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 return vec3(ctx.distance,ctx.view_depth,select(0.,1.,ctx.view_depth>=SCENE_DEPTH_FAR));
}
"#,
        [0.; 4],
    );
    rig.render(0);
    let effect = rig.read(DiagnosticTarget::HdrEffect);
    let metres = 2. * half[0] / LENS.size[0] as f32;
    let (mut near, mut far, mut sky) = (0, 0, 0);
    for y in 0..LENS.size[1] {
        for x in 0..LENS.size[0] {
            let at = (x as f32 + 0.5) * metres - half[0];
            let expected = if at < -4. * NEAR - 0.2 {
                sky += 1;
                [65504., 65504., 1.]
            } else if (-4. * NEAR + 0.2..-0.2).contains(&at) {
                near += 1;
                [NEAR, NEAR, 0.]
            } else if at > 0.2 {
                far += 1;
                [FAR, FAR, 0.]
            } else {
                continue;
            };
            let texel = effect.at([x, y]);
            for c in 0..3 {
                assert!(
                    (texel[c] - expected[c]).abs() <= 0.01 * expected[c].max(1.),
                    "pixel {x},{y}: {:?} for {expected:?}",
                    &texel[..3]
                );
            }
        }
    }
    assert!(
        near > 100 && far > 100 && sky > 100,
        "{near} near, {far} far, {sky} sky pixels"
    );
}
