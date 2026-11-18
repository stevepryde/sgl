//! PROVENANCE.md DFX-25 and the SGL3D temporal history register's R-9:
//! Crystal's production temporal pass discards all history at a pixel where
//! it takes the surface's (incident point) reprojection and that history's
//! squared distance from the 3x3 mean of this frame's radiance reaches 1.5
//! times the length of the neighbourhood's variance. A pixel that discards
//! stores this frame's radiance and variance 1; one that keeps history
//! stores variance 0.1 × 0.25 + 0.9 × 1 = 0.925 here (the fixture's current
//! variance is 0.25 and the reset frame's 1).
use super::followed_surface_tests::{Followed, checkerboard, followed};
use super::temporal_neighborhood_tests::{SIZE, device};

/// Variance a pixel that kept its history stores; one that discarded it
/// stores 1. Binary16 steps near these values are under 0.001.
const KEPT: f32 = 0.925;

// Plausible defects: the discard applies whichever candidate the pass takes,
// or ties between the candidates go to the surface's, so a smooth mirror at
// rest restarts wherever the reflected light changes. At rest the surface's
// and the reflection's reprojections land on the same previous point, the
// jitter of each frame removed (`ComputeReflectionHitPosition`), so their
// luminance ties and the pass takes the reflection's, which the discard does
// not test: R-9's measured "at rest, no pixel discarded, with or without a
// light changing". The fixture: a mirror plane 2 m ahead, the camera still
// and jittered by different offsets in the two frames, the reflected light
// dimming from a gradient of 0.5 to 1.13 to a uniform 0.25, which the
// discard would reject at every pixel (the variance is 0). The oracle: every
// pixel keeps history, under both depth conventions.
#[test]
fn a_still_mirror_under_jitter_discards_no_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let (_, variance) = followed(
            &device,
            &queue,
            &Followed {
                reversed,
                z: 2.,
                travel: 0.,
                jitter: [
                    [0.3 / SIZE[0] as f32, -0.2 / SIZE[1] as f32],
                    [-0.4 / SIZE[0] as f32, 0.35 / SIZE[1] as f32],
                ],
                motion: [0., 0., 0., 1.],
                history: |x, _| 0.5 + 0.01 * x as f32,
                current: |_, _| 0.25,
            },
        );
        let discarded = variance
            .iter()
            .filter(|&&v| (v - KEPT).abs() > 0.002)
            .count();
        assert_eq!(
            discarded, 0,
            "reversed={reversed}: {discarded} pixels of a still mirror lost their history"
        );
    }
}

/// The history's columns in blocks of `SLIDE` pixels: a highlight of 1.5 in
/// even blocks, a brighter one of 4 in odd blocks.
const SLIDE: u32 = 4;
fn highlights(x: u32, _: u32) -> f32 {
    if (x / SLIDE).is_multiple_of(2) {
        1.5
    } else {
        4.
    }
}

// Plausible defect: the surface's history is kept wherever the luminance
// choice prefers it, so a reflected highlight that the surface carried
// away, and that is no longer at the pixel, smears over the frames that
// follow (DFX-25's reason: bright fixtures' reflections smearing in fast
// motion). The fixture: a mirror sliding in its own plane `SLIDE` pixels a
// frame under a still camera, so its motion vectors place the surface's
// history `SLIDE` pixels back while its reflection stays where it was (the
// reflection's reprojection is the pixel itself). The history holds
// highlights of 1.5 and 4 in alternate blocks of `SLIDE` columns; this
// frame's radiance is a checkerboard of 0 and 1 (mean 4/9 to 5/9, variance
// about 0.25 a channel), where neither highlight is. In an odd block the
// surface's history (1.5) is nearer the mean than the reflection's (4), so
// the pass takes it, and it lies beyond the discard's tolerance (squared
// distance at least 2.7 against at most 0.65): the pixel discards, storing
// this frame's radiance. In an even block the pass takes the reflection's
// history (1.5 against the surface's 4), which the discard does not test,
// and keeps it.
#[test]
fn a_highlight_the_surface_carried_away_is_discarded() {
    let Some((device, queue)) = device() else {
        return;
    };
    let (radiance, variance) = followed(
        &device,
        &queue,
        &Followed {
            reversed: true,
            z: 2.,
            travel: 0.,
            jitter: [[0.; 2]; 2],
            // NDC motion: SLIDE pixels to the right since the last frame.
            motion: [2. * SLIDE as f32 / SIZE[0] as f32, 0., 0., 1.],
            history: highlights,
            current: checkerboard,
        },
    );
    let mut wrong = Vec::new();
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let pixel = (y * SIZE[0] + x) as usize;
            let discards = !(x / SLIDE).is_multiple_of(2);
            let ok = if discards {
                variance[pixel] == 1. && radiance[pixel][0] == checkerboard(x, y)
            } else {
                (variance[pixel] - KEPT).abs() <= 0.002
            };
            if !ok {
                wrong.push((x, y, discards, radiance[pixel][0], variance[pixel]));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} pixels where the discard should{} have fired, first (x, y, should discard, radiance, variance): {:?}",
        wrong.len(),
        if wrong.first().is_some_and(|w| w.2) {
            ""
        } else {
            " not"
        },
        wrong.first()
    );
}
