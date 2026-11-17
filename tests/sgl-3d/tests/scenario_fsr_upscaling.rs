//! The FSR upscaling suite of the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): FSR2 and FSR3, where the device runs them, at
//! the qualities where the render size is below the scene (display) size,
//! and with RCAS sharpening; and every temporal method, TAA included, under
//! camera rotation. `scenario_temporal_antialiasing` judges FSR at Native AA,
//! where the two sizes agree; this suite shares its stage, scripts and
//! oracles (`scenario::temporal`).
//!
//! Each case runs alone, one test per FSR version and quality (or per
//! version, where the case is about changing quality), named
//! `<case>::<version>_<quality>`, so a change to one path reruns only its
//! tests: `cargo test -p sgl-3d-tests --test scenario_fsr_upscaling
//! at_rest::fsr3_balanced`. Every case but U6 and U7 checks case 12's bound
//! over the frames it read (`Failures::bounded`). #810 parks FSR3's strip
//! bar at the upscaling qualities in U1 and U8. Case U1 dumps its frames and
//! metrics with `SGL_SCENARIO_DUMP`.
//!
//! The fixtures' truth assumes `rig::fixture_settings`: unlit cards, no
//! bloom, fog, ambient occlusion, reflections or motion blur, a fixed
//! exposure, and FSR without sharpening but in U7.
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
//! Not covered at the upscaling qualities yet: an object moving along the
//! view, an appearance or a departure, a resize and an origin move, which
//! the first suite covers at Native AA; RCAS's effect on the reference
//! (it moves edges away from their coverage on purpose); and rotation about
//! any axis but +Y.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose, View, coverage};
use scenario::image::{Image, Region, error, error_to};
use scenario::rig::{Dump, HALF_STEP, Rig};
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
    rig.settings.fsr_quality = quality;
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
    let [render, scene] = [rig.renderer.render_size(), rig.renderer.scene_size()];
    let scale = (0..2)
        .map(|i| scene[i] as f32 / render[i] as f32)
        .fold(0., f32::max);
    2.5 * scale + 2.
}

/// The at-rest strip bars parked with #810: FSR3's at the upscaling
/// qualities, where after a hold and one cycle it has not yet reached them.
fn parked_strips(method: Antialiasing, quality: FsrQuality, edge: Edge) -> bool {
    method == Antialiasing::Fsr3 && quality != FsrQuality::NativeAa && edge == Edge::Strips
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
/// the centre sample and the bar applies to it. After a hold and a whole
/// cycle at rest (30 to 84 frames, the fewest in which every phase has
/// contributed after the hold) the edges' RMSE against the supersampled
/// frame is at least that bar below the enlarged sample's, on strips and
/// outlines.
fn upscaled_at_rest(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let scenario = stage(&still);
    let mut rig = scenario.rig(&gpu, VIEW, method);
    let render = at_quality(&mut rig, &scenario, quality);
    let mut oracles = Oracles::new(&scenario, &gpu);
    oracles.set_render_size(render);
    let truth = oracles.reference(&scenario, 0);
    let single = oracles.single(&scenario, 0);
    let n = phases(method, &rig);
    for frame in 0..HOLD + n {
        scenario.render(&mut rig, frame);
    }
    let resolved = rig.antialiased();
    let mut dump = Dump::new(&format!("upscaled_at_rest_{method:?}_{quality:?}"));
    dump.image("converged", &resolved);
    dump.image("reference", &truth);
    dump.image("enlarged", &single);
    let regions = [
        (Edge::Strips, scenario.strip_edges(0, 1)),
        (Edge::Outline, scenario.outline_edges(0, &[0, 1])),
    ];
    let mut failures = Failures::default();
    for (edge, region) in &regions {
        let ours = error(&resolved, &truth, region).rmse;
        let theirs = error(&single, &truth, region).rmse;
        let bar = bar(method, &rig, *edge);
        dump.metric(format!("{edge:?}_rmse"), ours);
        dump.metric(format!("{edge:?}_enlarged_rmse"), theirs);
        eprintln!(
            "{method:?} {quality:?} at rest ({render:?}, {n} phases, {} samples a pixel), {edge:?} ({} pixels): RMSE {ours}, the enlarged sample's {theirs}, {} times below against the bar {bar}",
            samples(method, &rig),
            region.len(),
            theirs / ours
        );
        // #810 adds FSR3's strips back.
        if parked_strips(method, quality, *edge) {
            continue;
        }
        failures.check(ours * bar <= theirs, || {
            format!(
                "{method:?} {quality:?} at rest, {edge:?}: the converged RMSE {ours} is not {bar} times below the enlarged sample's {theirs}"
            )
        });
    }
    failures.bounded(&rig, format!("{method:?} {quality:?} at rest"));
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
}

/// Case U5. Plausible defect: history survives a camera cut at an
/// upscaling quality, or a change of quality, which changes the render
/// size and with it the targets, so the History contract restarts it
/// (`Renderer::resize`; FSR resets on a new render size), and the restarted
/// frame opens with a ghost of the old. The oracle is equivalence, as case
/// 10's: a run that panned and a run held still elsewhere, given the same
/// restart in the same frame, render it exactly alike. The run starts at
/// Quality, cuts there, then steps through every other quality and back, a
/// hold apart, building history in between: one test per version, since
/// the case is the change between qualities.
fn upscaled_restarts(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let restarts = [
        (HOLD, Restart::Cut),
        (2 * HOLD, Restart::Quality(FsrQuality::Balanced)),
        (3 * HOLD, Restart::Quality(FsrQuality::Performance)),
        (4 * HOLD, Restart::Quality(FsrQuality::UltraPerformance)),
        (5 * HOLD, Restart::Quality(FsrQuality::NativeAa)),
        (6 * HOLD, Restart::Quality(FsrQuality::Quality)),
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
        rig.settings.fsr_quality = FsrQuality::Quality;
        rig
    });
    for frame in 0..=restarts[restarts.len() - 1].0 {
        let restart = restarts.iter().find(|r| r.0 == frame).map(|r| r.1);
        let mut images = Vec::new();
        for (rig, scenario) in runs.iter_mut().zip([&panned, &held]) {
            match restart {
                Some(Restart::Cut) => rig.input.camera_cut = true,
                Some(Restart::Quality(quality)) => rig.settings.fsr_quality = quality,
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
            runs[0].renderer.render_size()
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
        let display = rig.renderer.scene_size();
        let expected = display.map(|d| (d as f32 / ratio(quality)) as u32);
        rig.settings.fsr_quality = quality;
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

/// Case U7. Plausible defects: RCAS fed a sharpness outside AMD's range or
/// mapped wrongly into its strength, an exposure that is zero or not the
/// frame's (RCAS multiplies by it before and divides after), or sharpening
/// read from an uninitialised target, any of which writes infinities, NaN
/// or values far outside the frame. The oracle is the bound, from AMD's
/// definition of RCAS (`FsrRcasF`, `ffx_fsr1.h`): its output is
/// (e + w (n + s + e' + w')) / (1 + 4 w) over the centre e and its four
/// neighbours, with the lobe w limited to -FSR_RCAS_LIMIT x strength..=0,
/// FSR_RCAS_LIMIT = 0.25 - 1/16 and strength 2^-(2 - 2 sharpness) (the SDK
/// maps `sharpness` 0..=1 to 2..=0 stops). With every tap within the
/// frame's radiance range [low, high] of contrast c, that moves the centre
/// by at most k c beyond it, k = 4 a / (1 - 4 a) at a = FSR_RCAS_LIMIT x
/// strength: 1.32 at the default 0.8, 3 at 1. The division uses AMD's
/// medium-precision reciprocal, one Newton step that only lowers it, by at
/// most (1/8)^2 = 1/64, and each of the method's `stores` may leave a value
/// a half-float step off. So every value read at rest over the settle (the
/// sharpest edges a resolve reaches by its end), at both sharpnesses, is
/// finite and within [low - k c, high + k c], widened by that rounding.
fn sharpening(method: Antialiasing, quality: FsrQuality) {
    let Some(gpu) = device(method) else { return };
    let scenario = stage(&still);
    let mut rig = scenario.rig(&gpu, VIEW, method);
    rig.settings.fsr_sharpening = true;
    let [low, high] = rig.radiance_range();
    let contrast = high - low;
    let mut failures = Failures::default();
    for sharpness in [0.8, 1.] {
        rig.settings.fsr_sharpness = sharpness;
        let limit = (0.25 - 1. / 16.) * 2f32.powf(-(2. - 2. * sharpness));
        let k = 4. * limit / (1. - 4. * limit);
        let rounding = stores(method) * HALF_STEP * high;
        let floor = (low - k * contrast).min((low - k * contrast) * (1. - 1. / 64.)) - rounding;
        let ceiling = high + k * contrast + rounding;
        at_quality(&mut rig, &scenario, quality);
        let (mut least, mut most) = (f32::INFINITY, f32::NEG_INFINITY);
        let mut finite = true;
        for frame in 0..SETTLE {
            scenario.render(&mut rig, frame);
            for texel in rig.read(DiagnosticTarget::Antialiased).texels {
                for &c in &texel[..3] {
                    finite &= c.is_finite();
                    least = least.min(c);
                    most = most.max(c);
                }
            }
        }
        eprintln!(
            "{method:?} {quality:?} sharpness {sharpness}: values {least}..={most} against RCAS's bound {floor}..={ceiling} (k {k})"
        );
        failures.check(finite && least >= floor && most <= ceiling, || {
            format!(
                "{method:?} {quality:?} sharpness {sharpness}: values {least}..={most} (finite: {finite}) leave RCAS's bound {floor}..={ceiling}"
            )
        });
    }
    failures.assert();
}

every_quality!(sharpening, sharpening);

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
/// the turn stops, after a hold and a whole cycle at rest, the edges' RMSE
/// is case 3's `bar` below it (reference, as case 3's and U1's).
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
    let settled = turning + HOLD + phases(method, &rig);
    let mut moving = Moving::default();
    for frame in 0..settled {
        scenario.render(&mut rig, frame);
        if (SETTLE..turning).contains(&frame) {
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
    let resolved = rig.antialiased();
    let rest = oracles.reference(&scenario, turning);
    let single = oracles.single(&scenario, turning);
    for edge in [Edge::Strips, Edge::Outline] {
        let region = match edge {
            Edge::Strips => scenario.strip_edges(turning, 1),
            Edge::Outline => scenario.outline_edges(turning, &[0, 1]),
        };
        let ours = error(&resolved, &rest, &region).rmse;
        let theirs = error(&single, &rest, &region).rmse;
        let bar = bar(method, &rig, edge);
        eprintln!(
            "{name} at rest after turning, {edge:?} ({} pixels): RMSE {ours}, the single sample's {theirs}, {} times below against the bar {bar}",
            region.len(),
            theirs / ours
        );
        // #810 adds FSR3's strips at the upscaling qualities back.
        if parked_strips(method, quality, edge) {
            continue;
        }
        failures.check(ours * bar <= theirs, || {
            format!(
                "{name} at rest after turning, {edge:?}: RMSE {ours} is not {bar} times below the single sample's {theirs}"
            )
        });
    }
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
