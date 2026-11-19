//! The local exposure suite of the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): `FrameInput::local_exposure` through the
//! public API, on a one-colour backdrop at a fixed exposure, reading
//! `DiagnosticTarget::ToneMapped`, the final image before the dither. The
//! oracle is the neutral frame (contrasts of 1) of the backdrop scaled by
//! Patry's formula: a uniform frame's base is its own V, k stops about
//! middle grey, so at contrast c on its side it presents as the neutral
//! frame scaled by 2^((c - 1) k). Each case runs alone by name:
//! `cargo test -p sgl-3d-tests --test scenario_local_exposure at_rest::taa`.
//!
//! Local exposure keeps no history: it is a function of the frame it is
//! given, so camera motion, moving objects, appearance, departure and cuts
//! reach it only as a different image. The unit cases in
//! `stages::post::local_exposure::tests` cover that image's behaviour: the
//! step edge (no ringing, a bounded halo), local detail kept whole, and a
//! feature moving within its cell changing nothing else.
//!
//! Not covered: whether the result looks right, which the owner judges from
//! captures; SMAA at a scene smaller than the output and spatial upscaling,
//! which tone map through the same `tone_map()` at the scene size.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::Image;
use scenario::rig::{Gpu, Rig, Shape, gpu};
use scenario::temporal::VIEW;
use sgl_3d::LocalExposure;
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;

/// Binary16 rounding of the capture and the grid's sums, far below the
/// smallest scaling asserted (a quarter stop).
const TOLERANCE: f32 = 2e-3;
/// Frames rendered before reading: TAA's resolve of a uniform frame is that
/// frame from the first, and a few more show nothing builds up.
const FRAMES: u32 = 3;

/// Linear grey `k` stops about middle grey.
fn grey(k: f32) -> f32 {
    0.18 * k.exp2()
}

/// The tone-mapped capture of `rig`'s view 0 after `FRAMES` frames, with
/// local exposure at `contrasts` (highlight, shadow).
fn capture(rig: &mut Rig, [highlight_contrast, shadow_contrast]: [f32; 2]) -> Image {
    let view = &mut rig.views[0];
    view.settings.diagnostics.capture_tone_target = true;
    view.input.local_exposure = LocalExposure {
        highlight_contrast,
        shadow_contrast,
    };
    for frame in 0..FRAMES {
        rig.render(frame);
    }
    rig.read(DiagnosticTarget::ToneMapped)
}

/// The capture of a backdrop of `sky` through `lens` under `method`.
fn backdrop(gpu: &Gpu, lens: View, method: Antialiasing, sky: f32, contrasts: [f32; 2]) -> Image {
    capture(&mut Rig::new(gpu, lens, method, sky), contrasts)
}

/// Asserts `actual` equals `expected` texel for texel within `TOLERANCE`.
fn assert_equal(actual: &Image, expected: &Image, case: &str) {
    assert_eq!(actual.size, expected.size, "{case}");
    for (i, (a, e)) in actual.texels.iter().zip(&expected.texels).enumerate() {
        for c in 0..3 {
            assert!(
                (a[c] - e[c]).abs() <= TOLERANCE,
                "{case}, texel {i}, channel {c}: {}, expected {}",
                a[c],
                e[c]
            );
        }
    }
}

/// The backdrop at k stops under `contrasts` against the neutral backdrop
/// scaled by 2^((c - 1) k), c the contrast on k's side.
fn check_formula(gpu: &Gpu, lens: View, method: Antialiasing, k: f32, contrasts: [f32; 2]) {
    let c = if k > 0. { contrasts[0] } else { contrasts[1] };
    let actual = backdrop(gpu, lens, method, grey(k), contrasts);
    let expected = backdrop(gpu, lens, method, grey(k) * ((c - 1.) * k).exp2(), [1., 1.]);
    assert_equal(
        &actual,
        &expected,
        &format!("{method:?}, k {k}, contrasts {contrasts:?}"),
    );
}

/// Plausible defects: local exposure lost or applied twice on a method's
/// path to the tone map (SMAA's scene-size tone map, TAA's), the frame's
/// exposure not applied before it, or the contrasts' sides swapped. Oracle:
/// the formula on a highlight backdrop (+2 stops) under a highlight
/// contrast of 0.5 and a shadow backdrop (-2 stops) under a shadow contrast
/// of 0.5, each with the other side's contrast at 1.
fn at_rest(method: Antialiasing) {
    let Some(gpu) = gpu() else { return };
    check_formula(&gpu, VIEW, method, 2., [0.5, 1.]);
    check_formula(&gpu, VIEW, method, -2., [1., 0.5]);
}

mod at_rest {
    use sgl_3d::settings::Antialiasing::*;
    crate::scenario::temporal::case_tests!(at_rest; taa: (Taa), smaa: (Smaa), off: (Off));
}

/// Plausible defect: a contrast that does not scale the base linearly (in
/// stops) or is clamped or inverted. Oracle: the formula at contrasts 1,
/// 0.75 and 0.5 on each side.
#[test]
fn levels() {
    let Some(gpu) = gpu() else { return };
    for c in [1., 0.75, 0.5] {
        check_formula(&gpu, VIEW, Antialiasing::Off, 2., [c, 1.]);
        check_formula(&gpu, VIEW, Antialiasing::Off, -2., [1., c]);
    }
}

/// Plausible defect: the grid keeps its dimensions or targets across a
/// resize. Oracles: the formula after a resize to another size and aspect;
/// and, on a scene a bright card divides, the resized view's frame equals
/// that of a view made at the new size, which a grid of the old size's
/// cells, sampled at the new size's uv, would misplace.
#[test]
fn resize() {
    let Some(gpu) = gpu() else { return };
    let resized = View {
        size: [160, 120],
        ..VIEW
    };
    let method = Antialiasing::Off;
    let contrasts = [0.5, 0.5];
    let mut rig = Rig::new(&gpu, VIEW, method, grey(2.));
    capture(&mut rig, contrasts);
    rig.views[0].lens = resized;
    let actual = capture(&mut rig, contrasts);
    let expected = backdrop(&gpu, resized, method, grey(1.), [1., 1.]);
    assert_equal(&actual, &expected, "uniform after resize");
    let card = Shape::card(Vec2::new(2., 3.), 0., grey(4.));
    let scene = |lens: View| {
        let mut rig = Rig::new(&gpu, lens, method, grey(-3.));
        rig.add(&card, Pose::at(Vec3::new(-0.8, 0., -4.)), 0);
        rig
    };
    let mut rig = scene(VIEW);
    capture(&mut rig, contrasts);
    rig.views[0].lens = resized;
    let actual = capture(&mut rig, contrasts);
    let expected = capture(&mut scene(resized), contrasts);
    assert_equal(&actual, &expected, "a card after resize");
}
