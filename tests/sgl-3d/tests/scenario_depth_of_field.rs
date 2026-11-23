//! The depth of field suite of the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): `Settings::depth_of_field` and
//! `FrameInput::depth_of_field` (Frame item 13) through the public API, on
//! unlit planes at known metric depths under a fixed lens, reading
//! `DiagnosticTarget::DepthOfField`, the focused frame. The oracle is the
//! thin lens: a point `z` metres ahead blurs into a disc whose radius in
//! pixels is half of `f² |z − F| / (N z (F − f))` over the sensor's height,
//! times the image's height, with `f` the focal length the sensor and the
//! vertical field of view give (`radius`). Each case runs alone by name:
//! `cargo test -p sgl-3d-tests --test scenario_depth_of_field
//! blur_follows_the_thin_lens`.
//!
//! Depth of field keeps no history: it is a function of the frame and the
//! depth it is given, so camera translation and rotation, moving objects,
//! an appearance or a departure reach it only as a different frame, and
//! the suite runs at rest (under TAA and FSR the depth is jittered each
//! frame, so their cases settle first and judge by bounds that hold
//! within a pixel). A camera cut, a resize, a second view, each level, TAA's
//! alternating outputs and FSR's render-size depth under a larger frame are
//! cases here. The radius cases' planes have one circle of confusion
//! throughout, so they run the gather's cheap form; the silhouette cases'
//! depth edges run its weighted form, and the tone-mapped capture of one
//! shows the focused frame reaches the display.
//!
//! The blur's width is measured from an edge between two coplanar planes:
//! the derivative of a row across it is the blur's line spread, whose
//! standard deviation a disc of radius `R` gives as `R / 2`. A sharp edge
//! spreads one pixel's box, 1/12 px² of variance, which is taken off.
//!
//! Not covered: whether the result looks right (bokeh shape, the blend of a
//! blurred silhouette), which the owner judges from captures; blended
//! surfaces, which write no depth and take the focus of what lies behind
//! them, and receivers, which the receiver pass draws at their own depth
//! (`stages::depth_of_field::tests`).
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::Image;
use scenario::rig::{Gpu, Quad, Rig, Shape, fsr_gpu, gpu};
use scenario::temporal::SETTLE;
use sgl_3d::DepthOfFieldParameters;
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::Vec3;
use sgl_3d::settings::{Antialiasing, DepthOfField, FsrQuality};

/// A 95 mm lens on a sensor 24 mm high: 0.25 rad of vertical view.
const LENS: View = View {
    size: [256, 192],
    fov_y: 0.25,
    near: 0.1,
};
const SENSOR: f32 = 0.024;
/// Metres to the plane in focus.
const FOCUS: f32 = 4.;
/// The grey the sky shows where no plane covers it (none does here).
const SKY: f32 = 0.5;

/// The thin lens's blur radius, in pixels of a frame `height` high, of a
/// point `z` metres ahead at f-number `n` (the module's formula).
fn radius(z: f32, n: f32, height: u32) -> f32 {
    let f = 0.5 * SENSOR / (0.5 * LENS.fov_y).tan();
    let diameter = f * f * (z - FOCUS).abs() / (n * z * (FOCUS - f)) / SENSOR * height as f32;
    diameter / 2.
}

/// Unlit planes facing the camera, `depth` metres ahead: one of `left`'s
/// radiance left of the view's centre column and one of `right`'s right of
/// it (`None` leaves that side empty), each far wider than the view.
fn planes(depth: f32, left: Option<f32>, right: Option<f32>) -> (Shape, Pose) {
    let extent = depth;
    let quad = |low: f32, high: f32, radiance| Quad {
        corners: [
            Vec3::new(low, -extent, 0.),
            Vec3::new(high, -extent, 0.),
            Vec3::new(high, extent, 0.),
            Vec3::new(low, extent, 0.),
        ],
        radiance,
    };
    let quads = [
        left.map(|r| quad(-extent, 0., r)),
        right.map(|r| quad(0., extent, r)),
    ]
    .into_iter()
    .flatten()
    .collect();
    (
        Shape { quads, blend: None },
        Pose::at(Vec3::new(0., 0., -depth)),
    )
}

/// A rig of `lens` under `antialiasing` with depth of field at `level`,
/// focused at `FOCUS` through `SENSOR` at f-number `n`.
fn rig(gpu: &Gpu, lens: View, antialiasing: Antialiasing, level: DepthOfField, n: f32) -> Rig {
    let mut rig = Rig::new(gpu, lens, antialiasing, SKY);
    focus(&mut rig, 0, level, n);
    rig
}

/// View `index` of `rig` focused at `FOCUS`, at `level` and f-number `n`.
fn focus(rig: &mut Rig, index: usize, level: DepthOfField, n: f32) {
    let view = &mut rig.views[index];
    view.settings.depth_of_field = level;
    view.input.depth_of_field = DepthOfFieldParameters {
        focus_distance: FOCUS,
        f_number: n,
        sensor_height: SENSOR,
    };
}

/// The blur radius an edge down the frame's centre column shows: twice the
/// standard deviation of its rows' mean line spread, less a pixel's box.
fn measured_radius(image: &Image) -> f32 {
    let [width, height] = image.size;
    let centre = width / 2;
    let rows = height / 4..3 * height / 4;
    let span = centre - 60..centre + 60;
    let mut spread = vec![0f64; span.len()];
    for y in rows.clone() {
        for (i, x) in span.clone().enumerate() {
            spread[i] += f64::from(image.value([x, y]) - image.value([x + 1, y]));
        }
    }
    let total: f64 = spread.iter().sum();
    let mean: f64 = spread
        .iter()
        .enumerate()
        .map(|(i, d)| i as f64 * d)
        .sum::<f64>()
        / total;
    let variance: f64 = spread
        .iter()
        .enumerate()
        .map(|(i, d)| (i as f64 - mean).powi(2) * d)
        .sum::<f64>()
        / total;
    2. * ((variance - 1. / 12.).max(0.)).sqrt() as f32
}

/// The radius measured at an edge between planes of 1 and 0 at `depth`.
fn edge_radius(rig: &mut Rig, depth: f32) -> f32 {
    let (shape, pose) = planes(depth, Some(1.), Some(0.));
    let id = rig.add(&shape, pose, 0);
    rig.render(0);
    let image = rig.read(DiagnosticTarget::DepthOfField);
    rig.remove(id);
    measured_radius(&image)
}

/// The standard deviation, over the radius, of an edge spread by the
/// gather's kernel of `rings` rings (Jimenez 2014, as the stage places
/// them): ring k of n at k/n of the radius with 8k samples, and one at the
/// centre, spread (n + 1)² / (1 + 4n(n + 1)) of the radius squared along a
/// line (a uniform disc's 1/4); the prefilter's ring of 8 at an eighth of
/// the radius, weighted 8/9, adds (8/9)(1/8)²/2 of it.
fn kernel_spread(rings: f32) -> f32 {
    let n = rings;
    (n + 1.) * (n + 1.) / (1. + 4. * n * (n + 1.)) + 8. / 9. / 64. / 2.
}

/// The half-size resampling's spread, in px²: at most a bilinear tent on
/// each resampling grid, the prefilter's taps on the frame's (1/6) and the
/// cheap gather's taps and the composite's return on the half-size grid
/// (2/3 each).
const RESAMPLING: f32 = 1. / 6. + 2. / 3. + 2. / 3.;

/// The rings each level gathers at most, which it gathers at every radius
/// of at least that many half-size pixels.
fn level_rings(level: DepthOfField) -> f32 {
    match level {
        DepthOfField::Off => 0.,
        DepthOfField::Low => 2.,
        DepthOfField::Medium => 3.,
        DepthOfField::High => 4.,
    }
}

/// Whether a measured radius (twice the edge's standard deviation) is the
/// thin lens's `expected` gathered at `level`: at least the spread of the
/// kernel of its most rings, at most that of two rings (the fewest at a
/// radius of two half-size pixels or more, as every radius here is) plus
/// the resampling's, each within 2 % for the model's approximations (the
/// disc's third ring lies at 0.99 of the radius, not 1; bilinear weights'
/// precision). A uniform disc of the radius would measure exactly it: the
/// excess is the kernel's and the resampling's, the latter shrinking as a
/// share of a larger radius.
fn is_thin_lens(measured: f32, expected: f32, level: DepthOfField) -> bool {
    let low = 0.98 * 2. * (kernel_spread(level_rings(level)) * expected * expected).sqrt();
    let high = 1.02 * 2. * (kernel_spread(2.) * expected * expected + RESAMPLING).sqrt();
    (low..=high).contains(&measured)
}

// Plausible defects: a frame changed where everything is in focus (noise,
// rounding through the gather, a tile that blurs without a circle of
// confusion, a composite that mixes in the half-size image), or a focus
// distance taken along the wrong axis or in the wrong unit. The oracle is
// the thin lens: a point on the plane of focus is a point on the sensor, so
// an edge at `FOCUS` leaves the focused frame equal to the antialiased one,
// texel for texel.
#[test]
fn the_focus_plane_stays_sharp() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::High, 1.);
    let (shape, pose) = planes(FOCUS, Some(1.), Some(0.));
    rig.add(&shape, pose, 0);
    rig.render(0);
    let focused = rig.read(DiagnosticTarget::DepthOfField);
    let antialiased = rig.read(DiagnosticTarget::Antialiased);
    assert!(focused == antialiased, "the plane of focus changed");
}

// Plausible defects: a circle of confusion in the wrong unit (a diameter
// for a radius, half-size pixels for the frame's), Wicked's squared
// |1 − F/z| instead of the thin lens's, blur that grows on the wrong side
// of the focus or ignores the f-number, rings placed short of the radius
// their spread test assumes. The oracle is the thin lens (`radius`): edges
// in front of and behind the focus at two f-numbers spread by its radius
// (`is_thin_lens`) and, at 30 px, by its radius times the kernel's spread
// alone, and its ratios, which do not depend on how the gather
// approximates a disc: the radius halves from z = 4/3 to z = 2 in front,
// grows by half from 8 m to 16 m behind, and halves from f/1 to f/2.
#[test]
fn blur_follows_the_thin_lens() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::Medium, 1.);
    let mut measured = Vec::new();
    for (z, n) in [
        (4. / 3., 1.),
        (2., 1.),
        (8., 1.),
        (16., 1.),
        (2., 2.),
        (0.95, 1.),
    ] {
        focus(&mut rig, 0, DepthOfField::Medium, n);
        let r = edge_radius(&mut rig, z);
        let expected = radius(z, n, LENS.size[1]);
        eprintln!("z {z} f/{n}: measured {r:.2} px, thin lens {expected:.2} px");
        assert!(
            is_thin_lens(r, expected, DepthOfField::Medium),
            "z {z} f/{n}: measured {r} px for the thin lens's {expected} px"
        );
        measured.push(r);
    }
    // At 30 px (15 half-size pixels) Medium gathers its three rings and the
    // resampling is under 0.2 % of the spread, so the measured radius is the
    // three-ring kernel's own, 1.155 of the lens's: within 2 %, a scale
    // error in the circle of confusion (a diameter, half-size pixels, the
    // wrong image height) shows here as a constant factor.
    let scale = measured[5] / radius(0.95, 1., LENS.size[1]) / (2. * kernel_spread(3.).sqrt());
    assert!(
        (0.98..=1.02).contains(&scale),
        "at 30 px the blur is {scale} × the three-ring kernel's"
    );
    let ratio = |a: usize, b: usize| measured[a] / measured[b];
    // Each ratio within a fifth of the lens's: the gather's fixed pixel of
    // resampling is a smaller share of the larger radius.
    let near = ratio(0, 1);
    assert!(
        (1.6..=2.2).contains(&near),
        "nearer blurs {near} × more, not 2"
    );
    let far = ratio(3, 2);
    assert!(
        (1.2..=1.8).contains(&far),
        "farther blurs {far} × more, not 1.5"
    );
    let aperture = ratio(1, 4);
    assert!(
        (1.6..=2.2).contains(&aperture),
        "f/1 blurs {aperture} × f/2, not 2"
    );
}

// Plausible defects: a level that does not gather (its ring count lost or
// zero), gathers past the radius, or is ignored. The oracle is the thin
// lens, as `blur_follows_the_thin_lens`'s, at each level, 2 m ahead at f/1
// (a radius of 4.7 half-size pixels, which takes every level's rings), and
// the rings' geometry (`kernel_spread`): 0.61, 0.58 and 0.56 of the radius
// for 2, 3 and 4 rings, so more rings spread strictly less.
#[test]
fn every_level_follows_the_thin_lens() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::Low, 1.);
    let expected = radius(2., 1., LENS.size[1]);
    let mut measured = Vec::new();
    for level in [DepthOfField::Low, DepthOfField::Medium, DepthOfField::High] {
        focus(&mut rig, 0, level, 1.);
        let r = edge_radius(&mut rig, 2.);
        eprintln!("{level:?}: measured {r:.2} px, thin lens {expected:.2} px");
        assert!(
            is_thin_lens(r, expected, level),
            "{level:?}: {r} px for {expected} px"
        );
        measured.push(r);
    }
    assert!(
        measured[0] > measured[1] && measured[1] > measured[2],
        "Low, Medium and High spread {measured:?}, not strictly less with more rings"
    );
}

/// The value of row `y`'s pixels of `image` from column `from`, in order.
fn row(image: &Image, y: u32, from: u32) -> Vec<f32> {
    (from..image.size[0]).map(|x| image.value([x, y])).collect()
}

/// `blurred_over_a_sharp_background`'s focused frame and its tone-mapped
/// capture under `antialiasing`, with `settle` frames rendered: a black card
/// 2 m ahead covering the view's left half over a plane of 1 at the focus.
fn silhouette(gpu: &Gpu, antialiasing: Antialiasing, settle: u32) -> (Image, Image) {
    let mut rig = rig(gpu, LENS, antialiasing, DepthOfField::Medium, 1.);
    rig.views[0].settings.fsr_quality = FsrQuality::Quality;
    rig.views[0].settings.diagnostics.capture_tone_target = true;
    let (card, near) = planes(2., Some(0.), None);
    let (background, far) = planes(FOCUS, Some(1.), Some(1.));
    rig.add(&card, near, 0);
    rig.add(&background, far, 0);
    for frame in 0..settle {
        rig.render(frame);
    }
    (
        rig.read(DiagnosticTarget::DepthOfField),
        rig.read(DiagnosticTarget::ToneMapped),
    )
}

/// Plausible defects: a gather by each pixel's own circle of confusion
/// alone, which leaves an in-focus background sharp right up to a blurred
/// foreground's silhouette (no near field), or one that spreads it too far.
/// The oracle is the thin lens's scatter: every point of the card blurs
/// into a disc of radius `R` (`radius` at 2 m, 9.3 px), so a background
/// pixel `d` from the silhouette is covered by the card's blur over the
/// circular segment of a disc of `R` beyond `d`: 47 % of it at 0.5 px, 20 %
/// at `R / 2`. Every background pixel within `R / 2` is darkened below 0.9,
/// the segment's 0.8 with a tenth of the range for the gather's sampling,
/// and those beyond `R` and three pixels of resampling stay 1. Under FSR at
/// Quality the depth is at two-thirds of the frame's size: depth read at
/// the wrong texel moves the silhouette's blur. TAA alternates two outputs,
/// each bound in turn. Downstream, a stage that presents the antialiased
/// frame instead of the focused one leaves the displayed background beside
/// the card as bright as the unreached one; the tone map is monotonic, so
/// the darkened pixel displays darker.
fn blurred_over_a_sharp_background(antialiasing: Antialiasing) {
    let (gpu, settle) = match antialiasing {
        Antialiasing::Fsr2 => (fsr_gpu(), SETTLE),
        Antialiasing::Taa => (gpu(), SETTLE),
        _ => (gpu(), 1),
    };
    let Some(gpu) = gpu else {
        return;
    };
    let (image, displayed) = silhouette(&gpu, antialiasing, settle);
    let reach = radius(2., 1., LENS.size[1]);
    let centre = LENS.size[0] / 2;
    for y in [LENS.size[1] / 3, LENS.size[1] / 2, 2 * LENS.size[1] / 3] {
        let values = row(&image, y, centre);
        let beyond = (reach + 3.).ceil() as usize;
        // Pixel `d` of the row's background has its centre d + 0.5 from
        // the silhouette.
        for (d, &value) in values.iter().enumerate() {
            if d as f32 + 0.5 > reach / 2. {
                break;
            }
            assert!(
                value < 0.9,
                "{antialiasing:?} row {y}: the background {d} px beside the blurred card is {value}"
            );
        }
        let unreached = values[beyond..beyond + 20]
            .iter()
            .copied()
            .fold(f32::MAX, f32::min);
        assert!(
            unreached > 0.97,
            "{antialiasing:?} row {y}: the background {beyond} px beyond the card is {unreached}"
        );
        let shown = row(&displayed, y, centre);
        assert!(
            shown[0] < 0.95 * shown[beyond],
            "{antialiasing:?} row {y}: the display shows {} beside the card and {} beyond it",
            shown[0],
            shown[beyond]
        );
    }
}

mod blurred_over_a_sharp_background {
    use sgl_3d::settings::Antialiasing::*;
    crate::scenario::temporal::case_tests!(blurred_over_a_sharp_background;
        off: (Off), taa: (Taa), fsr2: (Fsr2));
}

// Plausible defects: a blurred background gathered over an in-focus
// subject in front of it (no near/far separation, three.js's far field),
// which haloes the subject's edge with the background's colour, or a sharp
// subject's colour gathered into the blurred background beside it (a far
// field that takes every sample, whatever its spread). The oracle is the
// thin lens: the subject at the focus is sharp and in front, so nothing
// behind it reaches its pixels, and its own points image as points, so
// nothing of it reaches the background's; its interior two pixels in from
// its edge stays black, and the uniform background two pixels out from
// it stays 1 (the composite's resampling pixel either side). A plane of 1
// lies 16 m ahead, blurring by 7 px.
#[test]
fn a_sharp_subject_keeps_its_edge_over_a_blurred_background() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::Medium, 1.);
    let (subject, near) = planes(FOCUS, Some(0.), None);
    let (background, far) = planes(16., Some(1.), Some(1.));
    rig.add(&subject, near, 0);
    rig.add(&background, far, 0);
    rig.render(0);
    let image = rig.read(DiagnosticTarget::DepthOfField);
    let centre = LENS.size[0] / 2;
    for y in [LENS.size[1] / 3, LENS.size[1] / 2, 2 * LENS.size[1] / 3] {
        for x in centre - 40..centre - 2 {
            let v = image.value([x, y]);
            assert!(v < 0.02, "the subject at [{x}, {y}] is {v}");
        }
        for x in centre + 2..centre + 40 {
            let v = image.value([x, y]);
            assert!(v > 0.97, "the background at [{x}, {y}] is {v}");
        }
    }
}

// Plausible defects: targets or a circle-of-confusion scale kept from the
// size before a resize, so the blur keeps its old radius in pixels or reads
// stale tiles. The oracle is the thin lens: the radius in pixels scales
// with the frame's height, so halving the view's size halves it.
#[test]
fn a_resize_rescales_the_blur() {
    let Some(gpu) = gpu() else {
        return;
    };
    let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::Medium, 1.);
    let large = edge_radius(&mut rig, 4. / 3.);
    rig.views[0].lens.size = LENS.size.map(|s| s / 2);
    let small = edge_radius(&mut rig, 4. / 3.);
    let expected = radius(4. / 3., 1., LENS.size[1] / 2);
    eprintln!("large {large:.2} px, small {small:.2} px, thin lens {expected:.2} px");
    assert!(
        is_thin_lens(small, expected, DepthOfField::Medium),
        "{small} px for {expected} px"
    );
    let ratio = large / small;
    assert!(
        (1.6..=2.2).contains(&ratio),
        "a frame half as high blurs {ratio} × less, not 2 ×"
    );
}

// Plausible defect: state shared between views (one uniform buffer or set
// of targets for the renderer), so the last view's lens or size focuses
// every view of a frame. The oracle is equivalence: two views of one frame,
// focused differently and of different sizes, each present what a rig of
// that view alone presents.
#[test]
fn two_views_focus_independently() {
    let Some(gpu) = gpu() else {
        return;
    };
    let (shape, pose) = planes(2., Some(1.), Some(0.));
    let small = View {
        size: [128, 96],
        ..LENS
    };
    let alone = |lens: View, n: f32| {
        let mut rig = rig(&gpu, lens, Antialiasing::Off, DepthOfField::Medium, n);
        rig.add(&shape, pose, 0);
        rig.render(0);
        rig.read(DiagnosticTarget::DepthOfField)
    };
    let expected = [alone(LENS, 1.), alone(small, 2.)];
    let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::Medium, 1.);
    let second = rig.add_view(small, Antialiasing::Off);
    focus(&mut rig, second, DepthOfField::Medium, 2.);
    rig.add(&shape, pose, 0);
    rig.render(0);
    assert!(
        rig.read_view(0, DiagnosticTarget::DepthOfField) == expected[0],
        "view 0 differs"
    );
    assert!(
        rig.read_view(1, DiagnosticTarget::DepthOfField) == expected[1],
        "view 1 differs"
    );
}

// Plausible defect: depth of field tied to history as motion blur is, so
// a camera cut's frame is not focused (its targets left holding the frame
// before). The oracle is equivalence: depth of field keeps no history, so
// the frame after a cut, whose lens opens from f/2 to f/1, is focused as
// the same frame without a cut.
#[test]
fn a_cut_frame_is_focused() {
    let Some(gpu) = gpu() else {
        return;
    };
    let run = |cut: bool| {
        let mut rig = rig(&gpu, LENS, Antialiasing::Off, DepthOfField::Medium, 2.);
        let (shape, pose) = planes(2., Some(1.), Some(0.));
        rig.add(&shape, pose, 0);
        rig.render(0);
        focus(&mut rig, 0, DepthOfField::Medium, 1.);
        rig.views[0].input.camera_cut = cut;
        rig.render(1);
        rig.read(DiagnosticTarget::DepthOfField)
    };
    assert!(
        run(true) == run(false),
        "the cut frame was focused differently"
    );
}
