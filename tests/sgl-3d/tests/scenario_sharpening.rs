//! The sharpening suite of the scenario harness (`specs/testing.md`, "SGL3D
//! scenario harness"): post's RCAS (Frame item 15) under every antialiasing
//! method, TAA, SMAA, none, FSR2 and FSR3 (where the device runs them), on
//! the temporal suites' shared stage at rest (`scenario::temporal`). Each
//! case renders the same script three times, unsharpened and at sharpness
//! 0.8 and 1, and reads the last frame's `DiagnosticTarget::ToneMapped`, the
//! final image before the dither, and its `Antialiased` frame. Each case
//! runs alone by name, `<case>::<method>`: `cargo test -p sgl-3d-tests
//! --test scenario_sharpening sharpens_every_method::taa`.
//!
//! Sharpening keeps no history: motion, an appearance or a departure, a
//! camera cut and a resize reach it only through the image it is given,
//! which the antialiasing suites judge, so this suite runs at rest. The
//! output-size target it reads from is released and remade at a resize,
//! which the unit test `the_sharpening_input_is_held_only_while_rcas_reads_it`
//! (`stages::post::tests`) covers, as `sharpening_runs_in_every_final_path`
//! covers spatial upscaling and SMAA on a smaller scene, which this
//! suite's view at the output size does not run. The colour transfer
//! around RCAS, on display colour and after EASU, is checked on flat
//! colours by `a_flat_colour_sharpened_on_the_display_path_presents_unchanged`
//! and `spatial_upscaling_runs_on_baseline_device_and_releases_optional_targets`
//! (`stages::post::tests`).
//!
//! Not covered: whether the sharpened image looks right, which the owner
//! judges from captures.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::image::Image;
use scenario::rig::{Rig, fsr_gpu, gpu};
use scenario::temporal::{SETTLE, VIEW, stage, still};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::settings::{Antialiasing, Sharpening};

/// The sharpnesses each case renders, after the unsharpened frame:
/// `Settings::sharpness`'s default and AMD's most.
const SHARPNESSES: [f32; 2] = [0.8, 1.];

/// The last frame of the stage at rest under `method`, `Antialiased` and
/// `ToneMapped`, unsharpened and then at each of `SHARPNESSES`, from rigs
/// run identically but for the sharpening: a temporal method's after its
/// resolve settles, SMAA's and no antialiasing's first, since they keep no
/// history. `None` where the device does not run `method`.
fn frames(method: Antialiasing) -> Option<[(Image, Image); 3]> {
    let gpu = match method {
        Antialiasing::Fsr2 | Antialiasing::Fsr3 => fsr_gpu(),
        _ => gpu(),
    }?;
    let scenario = stage(&still);
    let run = |sharpness: Option<f32>| {
        let mut rig: Rig = scenario.rig(&gpu, VIEW, method);
        let settings = &mut rig.views[0].settings;
        settings.diagnostics.capture_tone_target = true;
        if let Some(sharpness) = sharpness {
            settings.sharpening = Sharpening::On;
            settings.sharpness = sharpness;
        }
        let frames = match method {
            Antialiasing::Smaa | Antialiasing::Off => 1,
            _ => SETTLE,
        };
        for frame in 0..frames {
            scenario.render(&mut rig, frame);
        }
        (
            rig.read(DiagnosticTarget::Antialiased),
            rig.read(DiagnosticTarget::ToneMapped),
        )
    };
    Some([None, Some(SHARPNESSES[0]), Some(SHARPNESSES[1])].map(run))
}

/// The summed absolute differences of `image`'s colour between
/// horizontally and vertically adjacent texels.
fn total_variation(image: &Image) -> f64 {
    let [width, height] = image.size;
    let mut sum = 0.;
    for y in 0..height {
        for x in 0..width {
            let here = image.at([x, y]);
            let neighbours = [(x > 0).then(|| [x - 1, y]), (y > 0).then(|| [x, y - 1])];
            for neighbour in neighbours.into_iter().flatten() {
                let there = image.at(neighbour);
                for (a, b) in here[..3].iter().zip(&there[..3]) {
                    sum += f64::from((a - b).abs());
                }
            }
        }
    }
    sum
}

/// Plausible defects: a method whose frame post does not sharpen (a final
/// path that skips RCAS), or a sharpness that is ignored or runs
/// backwards. RCAS raises the differences between neighbouring pixels, more
/// at a higher sharpness (AMD's FSR1 documentation: 0 the least, 1 the
/// most), so the final image's total variation orders unsharpened < 0.8 <
/// 1, under every method.
fn sharpens_every_method(method: Antialiasing) {
    let Some([off, default, most]) = frames(method) else {
        return;
    };
    let [off, default, most] = [&off.1, &default.1, &most.1].map(total_variation);
    eprintln!("{method:?}: total variation off {off}, 0.8 {default}, 1 {most}");
    assert!(
        off < default && default < most,
        "{method:?}: off {off}, 0.8 {default}, 1 {most}"
    );
}

/// Plausible defect: FSR still sharpens its own output, so an FSR frame is
/// sharpened twice, once by FSR's RCAS and again in post. The oracle is
/// the antialiased frame, which post's sharpening follows: it is the same,
/// texel for texel, with sharpening on and off. With
/// `sharpens_every_method` it shows the frame is sharpened once, in post.
fn fsr_output_is_not_sharpened(method: Antialiasing) {
    let Some([(off, _), (default, _), (most, _)]) = frames(method) else {
        return;
    };
    assert!(
        off == default && off == most,
        "{method:?}: the antialiased frame changes with the sharpening"
    );
}

mod sharpens_every_method {
    use sgl_3d::settings::Antialiasing::*;
    crate::scenario::temporal::case_tests!(sharpens_every_method;
        taa: (Taa), smaa: (Smaa), off: (Off), fsr2: (Fsr2), fsr3: (Fsr3));
}

mod fsr_output_is_not_sharpened {
    use sgl_3d::settings::Antialiasing::*;
    crate::scenario::temporal::case_tests!(fsr_output_is_not_sharpened; fsr2: (Fsr2), fsr3: (Fsr3));
}
