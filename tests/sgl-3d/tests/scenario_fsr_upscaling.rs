//! The FSR upscaling suite of the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): FSR2 and FSR3, where the device runs them, at
//! the qualities where the render size is below the scene (display) size;
//! and every temporal method, TAA included, under camera rotation.
//! `scenario_temporal_antialiasing` judges FSR at Native AA, where the two
//! sizes agree; this suite shares its stage, scripts and oracles
//! (`scenario::temporal`).
//!
//! Each case runs alone, one test per FSR version and quality (or per
//! version, where the case is about changing quality), named
//! `<case>::<version>_<quality>`, so a change to one path reruns only its
//! tests: `cargo test -p sgl-3d-tests --test scenario_fsr_upscaling
//! at_rest::fsr3_balanced`. Every case but U6 checks case 12's bound
//! over the frames it read (`Failures::bounded`). U1 and U8 judge FSR at
//! rest at the first whole jitter cycle that meets their bars within a frame
//! budget (`settle_at_rest`). Case U1 dumps its frames and metrics with
//! `SGL_SCENARIO_DUMP`.
//!
//! The fixtures' truth assumes `rig::fixture_settings`: unlit cards, no
//! bloom, fog, ambient occlusion, reflections or motion blur, a fixed
//! exposure, and no sharpening. FSR never sharpens: post does
//! (`scenario_sharpening`), after the antialiased frame these cases read.
//!
//! An upscaled frame's reference is the supersampled frame at the scene
//! size, which FSR reconstructs; the single sample it must beat is the frame
//! rendered at the render size without FSR and enlarged to the scene size,
//! each pixel showing the render pixel under its centre, what a game that
//! renders smaller has without an upscaler (`Oracles::set_render_size`).
//! The scene size is the first suite's 192 by 108 view, whose render sizes
//! at every quality (128 by 72, 112 by 63, 96 by 54, 64 by 36) keep its
//! aspect, so the enlarged frame's camera is the view's. It is the smallest
//! that still holds the shared stage: at Ultra Performance its card spans
//! 12 render pixels, the grating's strips stay below a render pixel, and
//! the outline and strip regions hold some hundreds of pixels each.
//!
//! An object the camera follows is covered over a still, far background
//! (U4) and with near scenery streaming past it under fast forward travel
//! (U9).
//!
//! Not covered at the upscaling qualities yet: an object moving along the
//! view, an appearance or a departure, a resize and an origin move, which
//! the first suite covers at Native AA; rotation about
//! any axis but +Y; a followed object with near scenery streaming past
//! under sideways travel; and scenery passing nearer than a followed
//! object, where AMD's previous depth flags it disoccluded (see `WALL`).
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose, View, coverage};
use scenario::image::{Image, Region, error, error_to};
use scenario::rig::{Dump, HALF_STEP, Quad, Rig, Shape};
use scenario::temporal::{
    BACKGROUND, CARD, DEPTH, Edge, Failures, HOLD, Oracles, SETTLE, SKY, Scenario, VIEW, ahead,
    background, bar, card, device, grating, phases, px, samples, stage, still, stores, vacated,
};
use sgl_3d::asset::{CompressedFormat, CompressedImage, Image as AssetImage};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::{Antialiasing, FsrQuality};

/// Every FSR quality: Native AA and the upscaling ones.
const QUALITIES: [FsrQuality; 5] = [
    FsrQuality::NativeAa,
    FsrQuality::Quality,
    FsrQuality::Balanced,
    FsrQuality::Performance,
    FsrQuality::UltraPerformance,
];

/// The frames a moving case measures after the settle: a hold, which is
/// what its held regions span, at 1 px a frame 12 pixel positions and at
/// 0.37 px every sub-pixel offset to within 0.04 px.
const MEASURED: u32 = HOLD;

/// The per-dimension scaling factor AMD prescribes for `quality`: the
/// FidelityFX SDK's "Scaling modes" table (`super-resolution-temporal.md`
/// in `sp-fidelity`'s vendored SDK 1.1.4), and 1 for Native AA, the FSR3
/// upscaler's `NativeAA` mode.
fn ratio(quality: FsrQuality) -> f32 {
    match quality {
        FsrQuality::NativeAa => 1.,
        FsrQuality::Quality => 1.5,
        FsrQuality::Balanced => 1.7,
        FsrQuality::Performance => 2.,
        FsrQuality::UltraPerformance => 3.,
    }
}

/// Gives `rig` FSR's `quality` and a new scene of `scenario`'s shapes, both
/// of which restart history, and returns the render size.
fn at_quality(rig: &mut Rig, scenario: &Scenario, quality: FsrQuality) -> [u32; 2] {
    rig.views[0].settings.fsr_quality = quality;
    scenario.restage(rig);
    rig.render_size()
}

/// How far, in scene pixels, what FSR shows at a pixel can come from at
/// `rig`'s sizes: its upsample reads the 4x4 render pixels nearest the
/// pixel's centre at most (FSR3's `ffx_fsr3upscaler_upsample.h`; FSR2's
/// 3x3), whose jittered samples lie within 2.5 render pixels of it, and its
/// history fetch the 4x4 history pixels around the reprojected position
/// (`HistorySample`, Lanczos 2), 2 scene pixels.
fn reach(rig: &Rig) -> f32 {
    let [render, scene] = [
        rig.views[0].view.render_size(),
        rig.views[0].view.scene_size(),
    ];
    let scale = (0..2)
        .map(|i| scene[i] as f32 / render[i] as f32)
        .fold(0., f32::max);
    2.5 * scale + 2.
}

/// The frames FSR may render at rest after the hold to meet its at-rest
/// bars (`settle_at_rest`), in whole jitter cycles. A bound on the test's
/// cost, not an oracle: the defects the cases name (a mismatched render
/// size, phase count or jitter scale, a history discarded at rest or after
/// a turn) never reach the bar however long the case waits, and a
/// converging resolve meets it long before. In frames, not cycles, because
/// AMD's accumulation rate is per frame.
const REST_BUDGET: u32 = 240;

/// One edge's judgement at rest: its pixels, its RMSE against the reference
/// and the single sample's, and its `bar`.
struct Judged {
    edge: Edge,
    pixels: usize,
    ours: f32,
    theirs: f32,
    bar: f32,
}

/// What `settle_at_rest` judged: the rest frame's reference and single
/// sample, the resolve at the cycle end it stopped at, the frames it
/// rendered at rest after the hold, and each edge.
struct Rest {
    truth: Image,
    single: Image,
    resolved: Image,
    frames: u32,
    edges: [Judged; 2],
}

/// Renders `scenario` from `from`, from which nothing moves, for the hold
/// and then whole jitter cycles of `method`'s `phases`, and at each cycle's
/// end judges the resolve over the strips and the outline at `from`: each
/// edge's RMSE against the supersampled reference is at least its `bar`
/// below the single sample's, enlarged where FSR upscales. It stops at the
/// first cycle end where both edges meet their bars, and records a failure
/// under `name`, with the last cycle's ratios, if no cycle end within the
/// limit does.
///
/// TAA's limit is one cycle, case 3's settle: its blend schedule is SGL's
/// own. FSR's is the whole cycles that fit in `REST_BUDGET`, at least one:
/// how many frames FSR takes to approach its converged image is AMD's
/// accumulation schedule, fixed per frame whatever the cycle's length, so
/// one cycle cannot state it. FSR3 blends a frame into history at
/// w / (fAccumulation + w), w the sum of its 3x3 Lanczos weights times
/// `fAverageLanczosWeightPerFrame`, 0.74 / 16
/// (`ffx_fsr3upscaler_accumulate.h:35`, `ffx_fsr3upscaler_upsample.h:629`,
/// `ffx_fsr3upscaler_common.h:47-48`); `fAccumulation` reaches its ceiling
/// of 1 three frames after a reset
/// (`ffx_fsr3upscaler_prepare_reactivity.h:203-236`), and the reset frame
/// seeds history with the rectification box's centre at weight 1
/// (`ffx_fsr3upscaler_upsample.h:635-643`). So at rest a frame contributes
/// about 5% at most, where the Lanczos sum peaks. FSR2 weights a frame by 1/12 (`ffx_fsr2_upsample.h:452`,
/// `ffx_fsr2_common.h:51`), and its history weight, 1 - reactive, grows over
/// about nine frames as the temporal reactive factor squares down from 1
/// (`ffx_fsr2_accumulate.h:128`, `193-202`). The sources are `sp-fidelity`'s
/// vendored SDK 1.1.4 (`sdk/include/FidelityFX/gpu/{fsr2,fsr3upscaler}/`).
fn settle_at_rest(
    scenario: &Scenario,
    rig: &mut Rig,
    oracles: &mut Oracles,
    method: Antialiasing,
    from: u32,
    name: &str,
    failures: &mut Failures,
) -> Rest {
    let truth = oracles.reference(scenario, from);
    let single = oracles.single(scenario, from);
    let regions = [
        (Edge::Strips, scenario.strip_edges(from, 1)),
        (Edge::Outline, scenario.outline_edges(from, &[0, 1])),
    ];
    let n = phases(method, rig);
    let cycles = match method {
        Antialiasing::Taa => 1,
        _ => (REST_BUDGET / n).max(1),
    };
    for frame in from..from + HOLD {
        scenario.render(rig, frame);
    }
    for cycle in 1..=cycles {
        let start = from + HOLD + (cycle - 1) * n;
        for frame in start..start + n {
            scenario.render(rig, frame);
        }
        let resolved = rig.antialiased();
        let edges = regions.each_ref().map(|(edge, region)| Judged {
            edge: *edge,
            pixels: region.len(),
            ours: error(&resolved, &truth, region).rmse,
            theirs: error(&single, &truth, region).rmse,
            bar: bar(method, rig, *edge),
        });
        let met = edges.iter().all(|e| e.ours * e.bar <= e.theirs);
        if !met && cycle < cycles {
            continue;
        }
        let frames = cycle * n;
        for e in &edges {
            eprintln!(
                "{name} ({n} phases, {} samples a pixel), {:?} ({} pixels), cycle {cycle} of at most {cycles} ({frames} frames after the hold): RMSE {}, the single sample's {}, {} times below against the bar {}",
                samples(method, rig),
                e.edge,
                e.pixels,
                e.ours,
                e.theirs,
                e.theirs / e.ours,
                e.bar
            );
            failures.check(e.ours * e.bar <= e.theirs, || {
                format!(
                    "{name}, {:?}: RMSE {} is not {} times below the single sample's {} after {cycle} cycles ({frames} frames after the hold)",
                    e.edge, e.ours, e.bar, e.theirs
                )
            });
        }
        return Rest {
            truth,
            single,
            resolved,
            frames,
            edges,
        };
    }
    unreachable!("at least one cycle")
}

/// A module named after a case holding one test per FSR version and
/// upscaling quality.
macro_rules! upscaling {
    ($module:ident, $case:ident) => {
        mod $module {
            use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3};
            use sgl_3d::settings::FsrQuality::{Balanced, Performance, Quality, UltraPerformance};
            crate::scenario::temporal::case_tests!($case;
                fsr2_quality: (Fsr2, Quality),
                fsr2_balanced: (Fsr2, Balanced),
                fsr2_performance: (Fsr2, Performance),
                fsr2_ultra_performance: (Fsr2, UltraPerformance),
                fsr3_quality: (Fsr3, Quality),
                fsr3_balanced: (Fsr3, Balanced),
                fsr3_performance: (Fsr3, Performance),
                fsr3_ultra_performance: (Fsr3, UltraPerformance),
            );
        }
    };
}

/// `upscaling!` with Native AA for each version too, and `$extra` tests.
macro_rules! every_quality {
    ($module:ident, $case:ident $(, $extra:item)*) => {
        mod $module {
            use sgl_3d::settings::{Antialiasing, FsrQuality};
            use Antialiasing::{Fsr2, Fsr3};
            use FsrQuality::{Balanced, NativeAa, Performance, Quality, UltraPerformance};
            crate::scenario::temporal::case_tests!($case;
                fsr2_native_aa: (Fsr2, NativeAa),
                fsr2_quality: (Fsr2, Quality),
                fsr2_balanced: (Fsr2, Balanced),
                fsr2_performance: (Fsr2, Performance),
                fsr2_ultra_performance: (Fsr2, UltraPerformance),
                fsr3_native_aa: (Fsr3, NativeAa),
                fsr3_quality: (Fsr3, Quality),
                fsr3_balanced: (Fsr3, Balanced),
                fsr3_performance: (Fsr3, Performance),
                fsr3_ultra_performance: (Fsr3, UltraPerformance),
            );
            $($extra)*
        }
    };
}

/// Case U1. Plausible defects: FSR fed a render size, jitter phase count or
/// jitter scale that does not match what it renders (the jitter in scene
/// pixels instead of render pixels, the phase count for another ratio), so
/// its accumulation never covers a scene pixel's area; or a history
/// discarded at rest. The oracle is the reference, with a bar derived as
/// case 3's: over a cycle of FSR's `phases` (N, read from the SDK port at
/// the actual sizes, `8 * (display / render)^2`), each phase puts one sample
/// in every render pixel, so a scene pixel receives
/// M = N (render / display)^2 samples, about 8 at every quality
/// (`temporal::samples`). Averaged, they estimate its coverage with 1 / M of
/// one random sample's mean squared error: sqrt(M) times below a centre
/// sample's RMSE across sub-pixel strips and sqrt(M / 2) at outlines
/// (`temporal::bar`). The enlarged single sample is a point sample at a
/// render pixel's centre, which lies off the scene pixel's centre or outside
/// it: one sample at a fraction p across a pixel an outline crosses at a
/// uniform position errs by (p^3 + (1 - p)^3) / 3 of the contrast squared,
/// least, 1/12, at the centre, and by more outside, so it is no better than
/// the centre sample and the bar applies to it. At rest, at the first end
/// of a whole cycle after the hold that meets them within `REST_BUDGET`
/// (`settle_at_rest`), the edges' RMSE against the supersampled frame is at
/// least that bar below the enlarged sample's, on strips and outlines.
fn upscaled_at_rest(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let scenario = stage(&still);
    let mut rig = scenario.rig(&gpu, VIEW, method);
    let render = at_quality(&mut rig, &scenario, quality);
    let mut oracles = Oracles::new(&scenario, &gpu);
    oracles.set_render_size(render);
    let name = format!("{method:?} {quality:?} at rest ({render:?})");
    let mut failures = Failures::default();
    let rest = settle_at_rest(
        &scenario,
        &mut rig,
        &mut oracles,
        method,
        0,
        &name,
        &mut failures,
    );
    let mut dump = Dump::new(&format!("upscaled_at_rest_{method:?}_{quality:?}"));
    dump.image("converged", &rest.resolved);
    dump.image("reference", &rest.truth);
    dump.image("enlarged", &rest.single);
    dump.metric("frames_at_rest_after_hold", rest.frames);
    for e in &rest.edges {
        dump.metric(format!("{:?}_rmse", e.edge), e.ours);
        dump.metric(format!("{:?}_enlarged_rmse", e.edge), e.theirs);
    }
    failures.bounded(&rig, &name);
    failures.assert();
}
upscaling!(at_rest, upscaled_at_rest);

/// The pixels at each frame from `from` until `frames` whose box, grown by
/// `reach`, has lain for the hold up to it wholly inside the card (shape
/// 0), and those it has kept wholly clear of every shape: the sky.
fn held(scenario: &Scenario, from: u32, frames: u32, reach: f32) -> Vec<[(Region, f32); 2]> {
    let inside: Vec<Region> = (0..frames).map(|f| scenario.inside(f, 0, reach)).collect();
    let outside: Vec<Region> = (0..frames).map(|f| scenario.outside(f, reach)).collect();
    let over = |regions: &[Region], frame: u32| {
        regions[(frame + 1 - HOLD) as usize..=frame as usize]
            .iter()
            .fold(regions[frame as usize].clone(), |a, b| a.and(b))
    };
    (from..frames)
        .map(|f| [(over(&inside, f), CARD), (over(&outside, f), SKY)])
        .collect()
}

/// The worst a moving run did against its oracles: the largest ratio of
/// its edges' RMSE to the enlarged sample's, and in each held region the
/// largest error relative to its radiance, each with its frame (and
/// pixel), and the pixel-frames held.
#[derive(Default)]
struct Moving {
    ratio: (f32, u32),
    held: [(f32, u32, [u32; 2], usize); 2],
}

impl Moving {
    fn edges(&mut self, frame: u32, resolved: &Image, truth: &Image, single: &Image, at: &Region) {
        let [ours, theirs] = [resolved, single].map(|i| error(i, truth, at).rmse);
        if ours / theirs > self.ratio.0 {
            self.ratio = (ours / theirs, frame);
        }
    }

    fn held(&mut self, frame: u32, resolved: &Image, regions: &[(Region, f32); 2]) {
        for ((region, radiance), worst) in regions.iter().zip(&mut self.held) {
            if region.is_empty() {
                continue;
            }
            let e = error_to(resolved, *radiance, region);
            worst.3 += e.pixels;
            if e.max / radiance > worst.0 {
                *worst = (e.max / radiance, frame, e.worst, worst.3);
            }
        }
    }

    /// The reference and analytic checks, under `name`.
    fn check(&self, failures: &mut Failures, method: Antialiasing, name: &str) {
        eprintln!(
            "{name}: RMSE at most {} of the enlarged sample's (frame {}); held pixels within {:?} relative (card, sky: error, frame, pixel, pixel-frames)",
            self.ratio.0, self.ratio.1, self.held
        );
        failures.check(self.ratio.0 <= 1., || {
            format!(
                "{name}: RMSE {} times the enlarged sample's at frame {}",
                self.ratio.0, self.ratio.1
            )
        });
        failures.check(self.held[1].3 > 0, || {
            format!("{name}: no sky pixel was held")
        });
        for (what, (error, frame, pixel, _)) in ["card", "sky"].into_iter().zip(self.held) {
            failures.check(error <= stores(method) * HALF_STEP, || {
                format!(
                    "{name}: a pixel held in the {what} for the hold is {error} of its radiance off, frame {frame} pixel {pixel:?}"
                )
            });
        }
    }
}

/// Case U2. Plausible defects: history lost or misplaced under camera
/// translation at the render size: a motion scale of the scene size
/// instead of the render size, the motion's sign or its jitter wrong, so
/// FSR reprojects to the wrong place or rejects what only the camera moved.
/// The oracles, as case 5's: at every frame of a pan at 1 and at 0.37
/// scene pixels a frame, for `MEASURED` frames after the settle, the RMSE
/// over the edges against that frame's reference is never above the
/// enlarged sample's (reference); and a pixel whose box, grown by FSR's
/// `reach`, has lain wholly inside the card, or wholly in the sky, for the
/// hold shows that radiance within the method's `stores` (analytic). At
/// Ultra Performance the reach (9.5 px) leaves no pixel of the card held;
/// the sky is held at every quality.
fn upscaled_panning(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let mut failures = Failures::default();
    let mut runs: Option<(Rig, Oracles)> = None;
    for rate in [1., 0.37] {
        // Leftward, so the stage crosses the view without leaving it.
        let pan = VIEW.pan(-rate, DEPTH);
        let camera = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
        let scenario = stage(&camera);
        let (rig, oracles) = match &mut runs {
            Some((rig, oracles)) => {
                oracles.restage(&scenario);
                (rig, oracles)
            }
            None => {
                let (rig, oracles) = runs.insert((
                    scenario.rig(&gpu, VIEW, method),
                    Oracles::new(&scenario, &gpu),
                ));
                (rig, oracles)
            }
        };
        let render = at_quality(rig, &scenario, quality);
        oracles.set_render_size(render);
        let frames = SETTLE + MEASURED;
        let held = held(&scenario, SETTLE, frames, reach(rig));
        let mut moving = Moving::default();
        for frame in 0..frames {
            scenario.render(rig, frame);
            if frame < SETTLE {
                continue;
            }
            let resolved = rig.antialiased();
            let truth = oracles.reference(&scenario, frame);
            let single = oracles.single(&scenario, frame);
            let edges = scenario.edges(frame, &[0, 1]);
            moving.edges(frame, &resolved, &truth, &single, &edges);
            moving.held(frame, &resolved, &held[(frame - SETTLE) as usize]);
        }
        let name = format!("{method:?} {quality:?} panning {rate} px a frame");
        moving.check(&mut failures, method, &name);
    }
    let (rig, _) = runs.unwrap();
    failures.bounded(&rig, format!("{method:?} {quality:?} panning"));
    failures.assert();
}
upscaling!(panning, upscaled_panning);

/// Case U3. Plausible defect: history ghosts where a moving surface
/// uncovers another or the sky at the render size (no disocclusion found
/// from the render-size depth and motion, or found and not applied to the
/// scene pixels it covers). The oracle is analytic with the spec's hold, as
/// case 6's: a pixel the card, moving 1.5 px a frame, left `vacated`, grown
/// by FSR's `reach`, for the hold shows what lies behind, the still
/// background (`reach` inside its outline) or the sky, within the method's
/// `stores`. The run lasts the settle and `MEASURED` frames, by which the
/// card has vacated pixels for the hold at every quality (the case asserts
/// that it has).
fn upscaled_disocclusion(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let (back, back_pose) = background();
    let mut failures = Failures::default();
    for (name, before_background) in [("before the background", true), ("against the sky", false)] {
        let across = Motion::translate(Vec3::new(1.5 * px(), 0., 0.));
        let start = Pose::at(Vec3::new(-1.2, 0., -DEPTH));
        let script = move |f: u32| {
            let mut poses = vec![across.pose(start, f)];
            if before_background {
                poses.push(back_pose);
            }
            poses
        };
        let mut shapes = vec![card()];
        if before_background {
            shapes.push(back.clone());
        }
        let scenario = Scenario {
            shapes,
            camera: &still,
            script: &script,
        };
        let frames = SETTLE + MEASURED;
        let truth = if before_background { BACKGROUND } else { SKY };
        let mut rig = scenario.rig(&gpu, VIEW, method);
        at_quality(&mut rig, &scenario, quality);
        let reach = reach(&rig);
        let vacated: Vec<Region> = vacated(&scenario, 0, frames, reach)
            .into_iter()
            .enumerate()
            .map(|(i, region)| match before_background {
                true => region.and(&scenario.inside(HOLD + i as u32, 1, reach)),
                false => region,
            })
            .collect();
        let mut checked = 0;
        let mut residual = (0f32, 0, [0; 2]);
        for frame in 0..frames {
            scenario.render(&mut rig, frame);
            if frame < HOLD || vacated[(frame - HOLD) as usize].is_empty() {
                continue;
            }
            let e = error_to(&rig.antialiased(), truth, &vacated[(frame - HOLD) as usize]);
            if e.max > residual.0 {
                residual = (e.max, frame, e.worst);
            }
            checked += e.pixels;
        }
        eprintln!(
            "{method:?} {quality:?} disocclusion {name}: {checked} pixel-frames vacated for the hold, residual at most {} (frame {}, pixel {:?})",
            residual.0, residual.1, residual.2
        );
        failures.check(checked > 0, || {
            format!("{quality:?} {name}: the card vacated no pixel for the hold")
        });
        failures.check(residual.0 <= stores(method) * truth * HALF_STEP, || {
            format!(
                "{method:?} {quality:?} {name}: a pixel vacated for the hold is {} off what lies behind, frame {} pixel {:?}",
                residual.0, residual.1, residual.2
            )
        });
        failures.bounded(&rig, format!("{method:?} {quality:?} disocclusion {name}"));
    }
    failures.assert();
}
upscaling!(disocclusion, upscaled_disocclusion);

/// Case U4 (#768 at the upscaling qualities). Plausible defect: history
/// rejected on a surface whose view depth and screen position stay fixed
/// while the camera moves, as a chase camera's craft does, because the
/// render-size depth test expects the camera's motion alone. The oracle is
/// case 7's closeness bound: a grating rigidly attached to a camera dollying
/// 0.2 m a frame over a still background is sampled in its interior exactly
/// as at rest, so a resolve that keeps its history converges there on the
/// still run's picture, and one that discards it shows a single frame's
/// upsample. After a hold and a whole cycle, on the strips `reach` inside
/// its outline, the following run is at least U1's strip `bar` times closer
/// to the still run than the enlarged single sample is.
fn upscaled_following(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let (back, _) = background();
    let far = Pose::at(Vec3::new(0., 0., -30.));
    let dolly = Motion::translate(Vec3::new(0., 0., -0.2));
    let moving = move |f: u32| dolly.pose(Pose::at(Vec3::ZERO), f);
    let moving_script = move |f: u32| vec![far, ahead(moving(f), 3.)];
    let still_script = move |_| vec![far, ahead(Pose::at(Vec3::ZERO), 3.)];
    let resting = Scenario {
        shapes: vec![back.clone(), grating()],
        camera: &still,
        script: &still_script,
    };
    let following = Scenario {
        shapes: vec![back, grating()],
        camera: &moving,
        script: &moving_script,
    };
    let mut rig = resting.rig(&gpu, VIEW, method);
    let render = at_quality(&mut rig, &resting, quality);
    let last = HOLD + phases(method, &rig) - 1;
    let reach = reach(&rig);
    let interior = following
        .strip_edges(last, 1)
        .and(&following.inside(last, 1, reach));
    assert!(
        !interior.is_empty(),
        "{quality:?}: no strip lies {reach} px inside the grating"
    );
    let [still_image, follow_image] = [&resting, &following].map(|scenario| {
        at_quality(&mut rig, scenario, quality);
        for frame in 0..=last {
            scenario.render(&mut rig, frame);
        }
        rig.antialiased()
    });
    let mut oracles = Oracles::new(&following, &gpu);
    oracles.set_render_size(render);
    let single = oracles.single(&following, last);
    let ours = error(&follow_image, &still_image, &interior).rmse;
    let theirs = error(&single, &still_image, &interior).rmse;
    let bar = bar(method, &rig, Edge::Strips);
    eprintln!(
        "{method:?} {quality:?} following ({} pixels): {ours} from the still run, the enlarged sample {theirs}, {} times closer against the bar {bar}",
        interior.len(),
        theirs / ours
    );
    let mut failures = Failures::default();
    failures.check(ours * bar <= theirs, || {
        format!(
            "{method:?} {quality:?}: the followed grating is {ours} from its still resolve, not {bar} times below the enlarged sample's {theirs} (#768)"
        )
    });
    failures.bounded(&rig, format!("{method:?} {quality:?} following"));
    failures.assert();
}
upscaling!(following, upscaled_following);

/// What restarts history in case U5.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Restart {
    Cut,
    Quality(FsrQuality),
    /// The frame was rendered and submitted once before, without
    /// `finish_frame` (U-2).
    Unfinished,
}

/// Case U5. Plausible defect: history survives a camera cut at an
/// upscaling quality, or a change of quality, which changes the render
/// size and with it the targets, so the History contract restarts it
/// (`View::resize`; FSR resets on a new render size), and the restarted
/// frame opens with a ghost of the old. The oracle is equivalence, as case
/// 10's: a run that panned and a run held still elsewhere, given the same
/// restart in the same frame, render it exactly alike. The run starts at
/// Quality, cuts there, then steps through every other quality and back, a
/// hold apart, building history in between: one test per version, since
/// the case is the change between qualities. Last, a frame rendered and
/// submitted without `finish_frame` restarts FSR when it is rendered again,
/// the History contract's exception for AMD's context, which advanced when
/// the attempt's dispatch was recorded (U-2, D-58).
fn upscaled_restarts(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let restarts = [
        (HOLD, Restart::Cut),
        (2 * HOLD, Restart::Quality(FsrQuality::Balanced)),
        (3 * HOLD, Restart::Quality(FsrQuality::Performance)),
        (4 * HOLD, Restart::Quality(FsrQuality::UltraPerformance)),
        (5 * HOLD, Restart::Quality(FsrQuality::NativeAa)),
        (6 * HOLD, Restart::Quality(FsrQuality::Quality)),
        (7 * HOLD, Restart::Unfinished),
    ];
    let pan = VIEW
        .pan(-0.5, DEPTH)
        .jumping_at(HOLD, Vec3::new(0.4, 0.1, 0.));
    let panning = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    // Held still 0.3 m above where the pan is at the next restart.
    let holding = move |f: u32| {
        let next = restarts.iter().map(|r| r.0).find(|&r| r >= f).unwrap_or(f);
        let pose = panning(next);
        if next == f {
            pose
        } else {
            Pose::at(pose.position + Vec3::Y * 0.3)
        }
    };
    let [panned, held] = [&panning as &dyn Fn(u32) -> Pose, &holding].map(stage);
    let mut failures = Failures::default();
    let mut runs = [&panned, &held].map(|scenario| {
        let mut rig = scenario.rig(&gpu, VIEW, method);
        rig.views[0].settings.fsr_quality = FsrQuality::Quality;
        rig
    });
    for frame in 0..=restarts[restarts.len() - 1].0 {
        let restart = restarts.iter().find(|r| r.0 == frame).map(|r| r.1);
        let mut images = Vec::new();
        for (rig, scenario) in runs.iter_mut().zip([&panned, &held]) {
            match restart {
                Some(Restart::Cut) => rig.views[0].input.camera_cut = true,
                Some(Restart::Quality(quality)) => rig.views[0].settings.fsr_quality = quality,
                Some(Restart::Unfinished) => {
                    scenario.pose(rig, frame);
                    rig.render_unfinished(frame);
                }
                None => {}
            }
            scenario.render(rig, frame);
            if restart.is_some() {
                images.push(rig.antialiased());
            }
        }
        let Some(restart) = restart else { continue };
        let everywhere = Region::from_fn(images[0].size, |_| true);
        let apart = error(&images[0], &images[1], &everywhere);
        eprintln!(
            "{method:?} {restart:?} at {:?}: {apart:?} between runs with different pasts",
            runs[0].views[0].view.render_size()
        );
        failures.check(apart.max == 0., || {
            format!(
                "{method:?} {restart:?}: the restarted frame depends on the frames before it, by up to {}",
                apart.max
            )
        });
    }
    for rig in &runs {
        failures.bounded(rig, format!("{method:?} upscaled restarts"));
    }
    failures.assert();
}

mod restarts {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3};
    #[test]
    fn fsr2() {
        super::upscaled_restarts(Fsr2)
    }
    #[test]
    fn fsr3() {
        super::upscaled_restarts(Fsr3)
    }
}

/// The block of a BC7 image (`CompressedFormat::Bc7`) of one opaque colour
/// whose channels are each 0 or 1: mode 5 (one subset, 7-bit colour and
/// 8-bit alpha endpoints; bits from the least significant: the mode's 6, a
/// 2-bit rotation, R0, R1, G0, G1, B0, B1, A0 and A1, then the indices),
/// both endpoints the colour and every index 0. A 7-bit endpoint widens to
/// 8 bits by repeating its top bit, so 0 and 127 decode to exactly 0 and
/// 255, which sRGB decoding maps to exactly 0 and 1.
fn bc7_block([r, g, b]: [bool; 3]) -> [u8; 16] {
    let mut bits: u128 = 1 << 5;
    for (channel, on) in [r, g, b].into_iter().enumerate() {
        let value: u128 = if on { 127 } else { 0 };
        bits |= value << (8 + 14 * channel) | value << (15 + 14 * channel);
    }
    bits |= 255 << 50 | 255 << 58;
    bits.to_le_bytes()
}

/// The texels a side of case U6's texture.
const RAMP_SIDE: u32 = 512;

/// Case U6's texture: a full mip chain in which level k is one colour, G
/// from level 2 on, R from 3 on and B from 4 on. Filtered linearly between
/// levels, a sample at level of detail L in 1..=4 is then
/// G = clamp(L - 1), R = clamp(L - 2) and B = clamp(L - 3), so
/// L = 1 + R + G + B.
fn lod_ramp() -> AssetImage {
    let levels = (0..=RAMP_SIDE.ilog2())
        .map(|k| {
            let side = (RAMP_SIDE >> k).max(1);
            let blocks = side.div_ceil(4).pow(2) as usize;
            bc7_block([k >= 3, k >= 2, k >= 4]).repeat(blocks)
        })
        .collect();
    AssetImage::Compressed(CompressedImage {
        format: CompressedFormat::Bc7,
        width: RAMP_SIDE,
        height: RAMP_SIDE,
        levels,
        decoded_level_0: None,
    })
}

/// Case U6. Plausible defects: a render size other than AMD's for the
/// quality, or a texture mip bias other than AMD's for the sizes (the
/// ratio inverted, the -1 missing, TAA's bias, the bias of the previous
/// quality, or a bias the materials never receive), which blurs or aliases
/// textures FSR then reconstructs. The oracles are analytic, from AMD's
/// formulae (`super-resolution-temporal.md`), computed here from the scene
/// size: the render size is the scene size over the quality's `ratio`,
/// truncated (`ffxFsr2GetRenderResolutionFromQualityMode`), and the bias is
/// `log2(render / display) - 1` ("Mipmap biasing"). The bias is measured
/// where it acts: an unlit card facing the camera, `lod_ramp` across it,
/// samples its base map at level of detail log2(texels per render pixel) +
/// bias, which the `Composite` target at the render size shows in its
/// interior. Filtering snaps the weight between levels to 1/16 (Vulkan's
/// least `mipmapPrecisionBits`, 4), and each channel is stored in half
/// precision, so the measured level holds within 1/16 + 3 half-float steps
/// of AMD's; the nearest other candidate (Balanced's bias beside
/// Performance's) lies 0.23 away. One frame a quality suffices, since the
/// level of detail does not depend on history; one test per version walks
/// every quality, so it also catches a bias left from the quality before.
/// Skipped, saying so, without BC7.
fn render_size_and_mip_bias(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    if !gpu
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        eprintln!("the mip bias scenario skipped: the device lacks BC7");
        return;
    }
    // The card spans RAMP_SIDE / 2^3.5 scene pixels, so AMD's bias puts the
    // level of detail near 2.5 at every quality, inside the ramp.
    let side = RAMP_SIDE as f32 / 2f32.powf(3.5) * px();
    let pose = Pose::at(Vec3::new(0., 0., -DEPTH));
    let mut rig = Rig::new(&gpu, VIEW, method, SKY);
    let mut failures = Failures::default();
    for quality in QUALITIES {
        let display = rig.views[0].view.scene_size();
        let expected = display.map(|d| (d as f32 / ratio(quality)) as u32);
        rig.views[0].settings.fsr_quality = quality;
        rig.new_scene();
        rig.add_textured(Vec2::splat(side), lod_ramp(), pose);
        rig.look(still(0));
        let render = rig.render_size();
        failures.check(render == expected, || {
            format!(
                "{method:?} {quality:?}: renders at {render:?}, not AMD's {expected:?} for {display:?}"
            )
        });
        rig.render(0);
        let composite = rig.read(DiagnosticTarget::Composite);
        assert_eq!(composite.size, render, "the composite at the render size");
        let view = View {
            size: render,
            ..VIEW
        };
        let camera = view.camera(still(0));
        let corners = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| pose.position + Vec3::new(x, y, 0.) * side * 0.5);
        let outline = view.project_polygon(&camera, &corners).unwrap();
        // 1 px for the derivatives' 2x2 quads and half a pixel of jitter.
        let interior = Region::from_fn(render, |p| coverage(&outline, p, 1.5) >= 1.);
        assert!(!interior.is_empty(), "{quality:?}: no interior");
        let texels = RAMP_SIDE as f32 / (side / view.pixel_metres(DEPTH));
        let bias = (expected[0] as f32 / display[0] as f32).log2() - 1.;
        let lod = texels.log2() + bias;
        let tolerance = 1. / 16. + 3. * HALF_STEP;
        let measured = interior.iter().map(|p| {
            let [r, g, b, _] = composite.at(p);
            1. + r + g + b
        });
        let (low, high) = measured.fold((f32::INFINITY, f32::NEG_INFINITY), |(l, h), m| {
            (l.min(m), h.max(m))
        });
        eprintln!(
            "{method:?} {quality:?} at {render:?}: level of detail {low}..={high} over {} pixels, AMD's {lod} ({texels} texels a render pixel, bias {bias})",
            interior.len()
        );
        failures.check(
            (low - lod).abs() <= tolerance && (high - lod).abs() <= tolerance,
            || {
                format!(
                    "{method:?} {quality:?}: textures sampled at level {low}..={high}, not AMD's {lod} ± {tolerance} (mip bias {bias})"
                )
            },
        );
    }
    failures.assert();
}

mod mip_bias {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3};
    #[test]
    fn fsr2() {
        super::render_size_and_mip_bias(Fsr2)
    }
    #[test]
    fn fsr3() {
        super::render_size_and_mip_bias(Fsr3)
    }
}

/// Case U8. Plausible defects: history lost or misplaced under camera
/// rotation, which moves every pixel by a different amount (a reprojection
/// that treats rotation as translation, a rotation term missing from the
/// sky's motion, a jitter that turns with the camera), or a resolve that
/// does not settle again once the turn ends. Case 1 checks the motion
/// itself under yaw. The oracles, for TAA and for FSR2 and FSR3 at every
/// quality: at every frame of a turn about +Y at 0.37 px a frame at the
/// view's centre, for `MEASURED` frames after the settle, the RMSE over the
/// edges against that frame's reference is never above the single
/// sample's, enlarged where FSR upscales (reference, as case 5's); and once
/// the turn stops, the edges' RMSE is case 3's `bar` below it (reference, as
/// case 3's and U1's), for TAA after a hold and a whole cycle and for FSR at
/// the first whole cycle after the hold that meets it within `REST_BUDGET`
/// (`settle_at_rest`). The camera holds the turn's last pose, frame
/// `turning`'s, from which the rest is judged.
fn turning(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let pixel_angle = (2. * (VIEW.fov_y * 0.5).tan() / VIEW.size[1] as f32).atan();
    let turn = Motion::yaw(0.37 * pixel_angle);
    let turning = SETTLE + MEASURED;
    let camera = move |f: u32| turn.pose(Pose::at(Vec3::ZERO), f.min(turning - 1));
    let scenario = stage(&camera);
    let mut rig = scenario.rig(&gpu, VIEW, method);
    let render = at_quality(&mut rig, &scenario, quality);
    let mut oracles = Oracles::new(&scenario, &gpu);
    oracles.set_render_size(render);
    let name = format!("{method:?} {quality:?}");
    let mut moving = Moving::default();
    for frame in 0..turning {
        scenario.render(&mut rig, frame);
        if frame >= SETTLE {
            let truth = oracles.reference(&scenario, frame);
            let single = oracles.single(&scenario, frame);
            let edges = scenario.edges(frame, &[0, 1]);
            moving.edges(frame, &rig.antialiased(), &truth, &single, &edges);
        }
    }
    eprintln!(
        "{name} turning: RMSE at most {} of the single sample's (frame {})",
        moving.ratio.0, moving.ratio.1
    );
    let mut failures = Failures::default();
    failures.check(moving.ratio.0 <= 1., || {
        format!(
            "{name} turning: RMSE {} times the single sample's at frame {}",
            moving.ratio.0, moving.ratio.1
        )
    });
    settle_at_rest(
        &scenario,
        &mut rig,
        &mut oracles,
        method,
        turning,
        &format!("{name} at rest after turning"),
        &mut failures,
    );
    failures.bounded(&rig, format!("{name} turning"));
    failures.assert();
}

every_quality!(
    turning,
    turning,
    #[test]
    fn taa() {
        super::turning(Antialiasing::Taa, FsrQuality::NativeAa)
    }
);

/// Case U9's camera travel a frame: a racer's 96 m/s at 60 frames a second.
const TRAVEL: f32 = 1.6;
/// Where case U9's road lies about the camera's path: its floor 0.9 m
/// beneath it, 6 cm below the followed card's lowest corner, its walls 4 m
/// either side, and their tops 0.6 m above, from 10 m behind the camera's
/// start to 1 km ahead.
///
/// The walls stand where AMD's previous depth is exact for the followed
/// shapes. `ReconstructPrevDepth` writes each render pixel's current depth
/// at its previous position, so scenery that is nearer than a followed
/// surface now and was behind it on screen a frame ago lands its depth on
/// that surface and flags it disoccluded, with correct inputs. A wall point
/// nearer than the shapes' 3 m was at most 3 m + `TRAVEL` away a frame
/// ago, where it lay inside their 1.7 m reach from the axis only if the
/// wall is within 1.7 × (3 + `TRAVEL`) / 3 = 2.6 m of it. The floor's such
/// points land on the shapes' lowest 0.25 m, within `reach` of their
/// outlines, which oracle 1 does not judge.
const FLOOR: f32 = -0.9;
const WALL: f32 = 4.;
const TOP: f32 = 0.6;
const ROAD: [f32; 2] = [10., -1000.];

/// Case U9's floor and walls, of `BACKGROUND`, each its own shape, since
/// `Scenario::outline_edges` takes a shape's later quads for strips.
fn road() -> [Shape; 3] {
    let [near, far] = ROAD;
    let quad = |corners: [[f32; 3]; 4]| Shape {
        quads: vec![Quad {
            corners: corners.map(Vec3::from),
            radiance: BACKGROUND,
        }],
        blend: None,
    };
    let wall = |x: f32| {
        quad([
            [x, FLOOR, near],
            [x, FLOOR, far],
            [x, TOP, far],
            [x, TOP, near],
        ])
    };
    [
        quad([
            [-WALL, FLOOR, near],
            [WALL, FLOOR, near],
            [WALL, FLOOR, far],
            [-WALL, FLOOR, far],
        ]),
        wall(-WALL),
        wall(WALL),
    ]
}

/// Case U9 (the chase camera at speed). Plausible defects: history taken
/// from a followed surface because the scenery beside and beneath it moves
/// fast, which U4's far, nearly still background cannot show: FSR's
/// previous depth, which it scatters from each render pixel along its
/// motion (`ReconstructPrevDepth`), landing the near floor's or walls'
/// depth on the followed surface and flagging it disoccluded where SGL's
/// depth, motion scale or sign, or the inverted or infinite depth flags
/// are wrong; the motion divergence (`ComputeMotionDivergence`) or the
/// reactive and disocclusion masks, which FSR samples bilinearly at the
/// render size, carrying the neighbours' motion into the surface; or a
/// dilation or mask SGL writes from neighbouring motion bleeding onto it.
///
/// The card and the grating are rigidly attached 3 m ahead of a camera
/// travelling `TRAVEL` forward a frame down a road, a floor that passes
/// just beneath them and two walls beside them (`road`). The floor beneath the
/// card's lowest corner moves 19.4 scene pixels a frame, from 12.9 render
/// pixels at Quality to 6.5 at Ultra Performance; the walls beside the
/// shapes and the floor nearer the camera move faster. The case first
/// checks that the scenery within `reach` of the shapes really streams:
/// at the frame oracle 1 judges, the fastest motion the `Motion` target
/// holds there (current minus previous unjittered UV, in render pixels) is
/// at least the script's for that floor point. The followed shapes keep
/// their screen position and depth, so the oracles are U4's and U2's:
///
/// 1. Equivalence with a closeness bound, U4's: the followed grating's
///    interior is sampled exactly as at rest, so a resolve that keeps its
///    history converges there on the still run's picture. After a hold
///    and a whole cycle, on the strips `reach` inside its outline, the
///    travelling run is at least U1's strip `bar` times closer to the
///    still run than the enlarged single sample is.
/// 2. Reference, U2's: at every frame of `MEASURED` after the settle, the
///    RMSE over the followed shapes' outlines, against the streaming
///    scenery and the sky, against that frame's supersampled reference is
///    never above the enlarged single sample's.
fn upscaled_chase(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let road = road();
    let travel = Motion::translate(Vec3::new(0., 0., -TRAVEL));
    let moving = move |f: u32| travel.pose(Pose::at(Vec3::ZERO), f);
    // The card left of the grating, as on the stage.
    let followed =
        |camera: Pose| [-0.9, 0.9].map(|x| ahead(camera, 3.).then(Pose::at(Vec3::new(x, 0., 0.))));
    let script_of = |camera: &dyn Fn(u32) -> Pose, f: u32| {
        let mut poses = vec![Pose::at(Vec3::ZERO); road.len()];
        poses.extend(followed(camera(f)));
        poses
    };
    let moving_script = move |f: u32| script_of(&moving, f);
    let still_script = move |f: u32| script_of(&still, f);
    let shapes: Vec<Shape> = road.iter().cloned().chain([card(), grating()]).collect();
    let [card_shape, grating_shape] = [road.len(), road.len() + 1];
    let resting = Scenario {
        shapes: shapes.clone(),
        camera: &still,
        script: &still_script,
    };
    let chasing = Scenario {
        shapes,
        camera: &moving,
        script: &moving_script,
    };
    let mut rig = resting.rig(&gpu, VIEW, method);
    let render = at_quality(&mut rig, &resting, quality);
    let last = HOLD + phases(method, &rig) - 1;
    let reach = reach(&rig);
    let interior = chasing
        .strip_edges(last, grating_shape)
        .and(&chasing.inside(last, grating_shape, reach));
    assert!(
        !interior.is_empty(),
        "{quality:?}: no strip lies {reach} px inside the grating"
    );
    let name = format!("{method:?} {quality:?} chase");
    let mut failures = Failures::default();

    // The script's motion, in render pixels, of the floor point beneath
    // the card's lowest corner at `last`.
    let render_px = Vec2::new(render[0] as f32, render[1] as f32);
    let [now, before] = [last, last - 1].map(|f| VIEW.camera(moving(f)));
    let corner = lowest_corner(&chasing, card_shape, last);
    let beneath = Vec3::new(corner.x, FLOOR, corner.z);
    let (at, _) = VIEW.project(&now, beneath).expect("the floor ahead");
    let identity = Pose::at(Vec3::ZERO).matrix();
    let floor_motion = VIEW
        .surface_motion(
            &now,
            &before,
            road[0].quads[0].corners,
            identity,
            identity,
            at,
        )
        .expect("the floor under its own point");
    let streaming = (floor_motion.truncate() * render_px).length();

    let still_image = {
        for frame in 0..=last {
            resting.render(&mut rig, frame);
        }
        rig.antialiased()
    };
    at_quality(&mut rig, &chasing, quality);
    let mut oracles = Oracles::new(&chasing, &gpu);
    oracles.set_render_size(render);
    let mut moving_worst = Moving::default();
    let mut chase_image = None;
    for frame in 0..(last + 1).max(SETTLE + MEASURED) {
        chasing.render(&mut rig, frame);
        let measured = (SETTLE..SETTLE + MEASURED).contains(&frame);
        if frame != last && !measured {
            continue;
        }
        let resolved = rig.antialiased();
        if measured {
            let truth = oracles.reference(&chasing, frame);
            let single = oracles.single(&chasing, frame);
            let outline = chasing.outline_edges(frame, &[card_shape, grating_shape]);
            moving_worst.edges(frame, &resolved, &truth, &single, &outline);
        }
        if frame == last {
            // The scenery within `reach` of the followed shapes.
            let motion = rig.read(DiagnosticTarget::Motion).enlarged(VIEW.size);
            let polygons = chasing.polygons(frame);
            let outlines = [&polygons[card_shape][0], &polygons[grating_shape][0]];
            let beside = Region::from_fn(VIEW.size, |p| {
                outlines.iter().any(|o| coverage(o, p, reach) > 0.)
                    && outlines.iter().all(|o| coverage(o, p, 0.) == 0.)
                    && motion.at(p)[3] == 1.
            });
            let (fastest, pixel) = beside
                .iter()
                .map(|p| {
                    let [r, g, _, _] = motion.at(p);
                    ((Vec2::new(r, g) * render_px).length(), p)
                })
                .fold((0., [0; 2]), |a, b| if b.0 > a.0 { b } else { a });
            eprintln!(
                "{name} at {render:?}: the scenery within {reach} px of the followed shapes ({} pixels) moves up to {fastest} render px a frame (pixel {pixel:?}); the script's floor beneath the card {streaming} ({} scene px)",
                beside.len(),
                (floor_motion.truncate() * Vec2::new(VIEW.size[0] as f32, VIEW.size[1] as f32))
                    .length()
            );
            failures.check(fastest >= streaming * (1. - HALF_STEP), || {
                format!(
                    "{name}: the scenery beside the followed shapes moves at most {fastest} render px a frame, below the script's {streaming} for the floor beneath the card"
                )
            });
            chase_image = Some(resolved);
        }
    }
    let chase_image = chase_image.expect("the frame oracle 1 judges");
    let single = oracles.single(&chasing, last);
    let ours = error(&chase_image, &still_image, &interior).rmse;
    let theirs = error(&single, &still_image, &interior).rmse;
    let bar = bar(method, &rig, Edge::Strips);
    eprintln!(
        "{name} interior ({} pixels): {ours} from the still run, the enlarged sample {theirs}, {} times closer against the bar {bar}",
        interior.len(),
        theirs / ours
    );
    failures.check(ours * bar <= theirs, || {
        format!(
            "{name}: the followed grating is {ours} from its still resolve, not {bar} times below the enlarged sample's {theirs}"
        )
    });
    eprintln!(
        "{name} outlines: RMSE at most {} of the enlarged sample's (frame {})",
        moving_worst.ratio.0, moving_worst.ratio.1
    );
    failures.check(moving_worst.ratio.0 <= 1., || {
        format!(
            "{name} outlines: RMSE {} times the enlarged sample's at frame {}",
            moving_worst.ratio.0, moving_worst.ratio.1
        )
    });
    failures.bounded(&rig, &name);
    failures.assert();
}
upscaling!(chase, upscaled_chase);

/// The lowest corner of `shape`'s outline at `frame`, in world space.
fn lowest_corner(scenario: &Scenario, shape: usize, frame: u32) -> Vec3 {
    let pose = (scenario.script)(frame)[shape];
    scenario.shapes[shape].quads_at(pose)[0]
        .into_iter()
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap()
}
