//! Temporal history rules outside antialiasing (`specs/sgl3d-history.md`):
//! automatic exposure on a still jittered view (E-3).
//!
//! Each case runs alone by name: `cargo test -p sgl-3d-tests --test
//! history_misc still_exposure::taa`.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::Pose;
use scenario::rig::Shape;
use scenario::temporal::{DEPTH, GRATING, STRIPS, Scenario, VIEW, device, px, still};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;
use sgl_3d::{AutoExposure, Exposure};

/// Frames until the correction has followed the jittered frames' metering
/// from the reset frame's, which it snaps to (E-1): the reset frame meters
/// another frame (TAA's is unjittered), a few thousandths of a stop away
/// here, and the default brightening speed closes 1/30 of the gap a frame
/// at 60 frames a second, so 128 frames leave under 2% of it.
const SETTLE: u32 = 128;
/// The frames measured after the settle: whole jitter cycles of TAA (16)
/// and FSR at Native AA (8).
const MEASURED: u32 = 96;

/// The smallest change of exposure, in stops, that moves an 8-bit sRGB
/// output code: at white, where the sRGB curve's codes are densest relative
/// to the light they encode. With v = 1.055 L^(1/2.4) - 0.055, a relative
/// change r of L moves v by r (v + 0.055) / 2.4, one code (1/255) at v = 1
/// when r = 2.4 / (255 x 1.055).
fn visible_step() -> f32 {
    (1. + 2.4 / (255. * 1.055f32)).log2()
}

/// A frame filled with stripes 1.5 px wide every 4 px at `DEPTH`, between
/// the suites' grating radiances, turned 0.05 radians off the pixel grid:
/// fine detail the jitter samples differently every frame, which the meter
/// sees before antialiasing resolves it.
fn stripes() -> Scenario<'static> {
    Scenario {
        shapes: vec![Shape::grating(
            Vec2::new(12., 8.),
            0.05,
            GRATING,
            STRIPS,
            1.5 * px(),
            4. * px(),
        )],
        camera: &still,
        script: &|_| vec![Pose::at(Vec3::new(0., 0., -DEPTH))],
    }
}

/// Rule E-3: automatic exposure meters the jittered frame and follows its
/// target at the authored speeds, within the exponential transition
/// distance by a share of the remaining delta. On a still view it moves only
/// as far as the jitter moves the metered average. The bound is the number
/// the register states: neither a frame's step nor the movement across the
/// measured cycles reaches one 8-bit output code (`visible_step`), far below
/// FSR2's lock threshold (a 1.88x drift, 0.91 stops) and invisible to
/// FSR3's luma tests, which take the current exposure on both sides.
fn still_exposure(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let scenario = stripes();
    let mut rig = scenario.rig(&gpu, VIEW, method);
    rig.input.exposure = Exposure {
        stops: 0.,
        automatic: Some(AutoExposure::default()),
    };
    let mut stops = Vec::new();
    for frame in 0..SETTLE + MEASURED {
        scenario.render(&mut rig, frame);
        if frame >= SETTLE {
            stops.push(rig.read(DiagnosticTarget::Exposure).texels[0][0].log2());
        }
    }
    let step = stops
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0f32, f32::max);
    let low = stops.iter().copied().fold(f32::MAX, f32::min);
    let high = stops.iter().copied().fold(f32::MIN, f32::max);
    let bound = visible_step();
    eprintln!(
        "{method:?} still exposure {low}..{high} stops: largest frame step {step}, peak to peak {}, bound {bound}",
        high - low
    );
    assert!(
        step < bound && high - low < bound,
        "{method:?}: a still view's exposure stepped {step} stops in a frame and ranged {low}..{high}; one output code is {bound} stops"
    );
}

mod still_exposure {
    use super::Antialiasing;

    #[test]
    fn taa() {
        super::still_exposure(Antialiasing::Taa)
    }
    #[test]
    fn fsr2() {
        super::still_exposure(Antialiasing::Fsr2)
    }
    #[test]
    fn fsr3() {
        super::still_exposure(Antialiasing::Fsr3)
    }
}
