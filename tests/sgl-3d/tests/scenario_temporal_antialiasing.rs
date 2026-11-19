//! The temporal antialiasing suite of the scenario harness
//! (`specs/testing.md`, "SGL3D scenario harness"): TAA, and FSR2 and FSR3
//! where the device runs them, judged against analytic truth, a supersampled
//! reference, equivalent scripts, bounds and the numbers the spec states.
//! Case 12, the bound, is checked over every antialiased frame each test
//! read, after the case's own assertions (`Failures::bounded`). Cases 3, 4
//! and 7 dump their frames and metrics with `SGL_SCENARIO_DUMP`.
//!
//! Each case runs alone, one test per method (and per variant where its
//! variants together would take longer than a test may alone), named
//! `<case>::<method>…`, so a change to one resolve reruns only its tests:
//! `cargo test -p sgl-3d-tests --test scenario_temporal_antialiasing
//! panning_keeps_history::fsr2`. FSR's tests skip, saying so, where the
//! device lacks its features. Each test renders its own oracles. A case
//! that runs TAA alone is one test, or one per variant.
//!
//! Each rule of TAA's motion-adaptive resolve (Frame item 11) has a test
//! that fails with that rule alone removed (#816): the camera test
//! (`a_camera_step_rearms_the_whole_view`), the hold carried over its
//! frames (`the_hold_lasts_its_frames_after_a_camera_step`), the rigid
//! signal (case 9 and its material edits), the visibility mask (case 9's
//! masking), the pixel's own motion (case 13), depth disocclusion
//! (`depth_disocclusion_rejects_beyond_the_reach`), the sky a surface
//! leaves (`the_sky_a_far_surface_leaves_is_a_disocclusion`), the
//! responsive masks' strength and full hold (cases 14 and 15 and case 9's
//! dissolves), and the motion reach
//! (`a_surface_its_morph_target_moves_from_rest_beyond_the_search`, and
//! case 4 beside motion for its threshold). The sky a surface arrives over
//! has none: whatever covers a neighbourhood that held only sky moved
//! there, so its own motion re-arms the hold; or it appeared, which sets
//! the rigid signal; or a shader drew it, which marks the masks. No scene
//! the public API builds depends on that rule alone.
//!
//! The fixtures' truth assumes `rig::fixture_settings`: unlit cards, no
//! bloom, fog, ambient occlusion, reflections or motion blur, and a fixed
//! exposure, so what antialiasing receives is exactly their radiance.
//!
//! FSR runs here at Native AA, where the render and scene sizes agree; case
//! 1 checks the motion under rotation. FSR never sharpens: post sharpens
//! after the antialiased frame these cases read (`scenario_sharpening`).
//! `scenario_fsr_upscaling` covers FSR at its upscaling qualities and every
//! method's resolve under rotation. The
//! stage, scripts and oracles both share are `scenario::temporal`'s.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{FRAME_RATE, Motion, Pose, View, coverage};
use scenario::image::{self, Image, Region, centre, error, error_to, pixels};
use scenario::rig::{Dump, Gpu, HALF_STEP, Rig, Shape, gpu};
use scenario::temporal::{
    BACKGROUND, CARD, DEPTH, Edge, Failures, GRATING, HOLD, JITTER_CYCLE, Oracles, SETTLE, SKY,
    STRIPS, Scenario, Script, VIEW, ahead, background, bar, card, device, grating, px, stage,
    stage_poses, still, stores, vacated,
};
use sgl_3d::ShaderSource;
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;

/// A fifth of TAA's still-pixel classification (`TAA_STILL_MOTION_PIXELS`,
/// 0.01 px, DFX-19 in `sgl-post-fx`'s PROVENANCE.md) at this view's width,
/// in UV: motion the resolve could mistake for movement exceeds it.
const STILL_UV: f32 = 0.01 / 5. / VIEW.size[0] as f32;
/// Depth motion beside its half-float step (`HALF_STEP` of the value): the
/// f32 rounding of two view depths of a few metres differenced, far below
/// the slowest scripted depth motion, 0.05 m a frame.
const STILL_DEPTH: f32 = 1e-4;
/// How much farther the far variants place a fixture: every shape and
/// position scaled by it about the still camera, so the far fixture
/// projects to the same pixels and only its depth's magnitude changes.
/// Filament's sky override (`skyDepthThreshold`, reversed normalized depth
/// 0.001), which TAA ported until #791, took a surface as far beyond
/// `VIEW.near / 0.001`, 100 m. The stage at `DEPTH` lies at normalized
/// depth 0.1 / 4 = 0.025, 25 times above it; scaled to 2 km, at
/// 0.1 / 2000 - 0.1 / 10 000 = 4e-5 (the effect camera's far plane is
/// 10 km), 25 times below it; and the far wall at 4 km stays within that
/// far plane, so none of them is the sky.
const FAR: f32 = 500.;

/// Of `stores`, those made after the motion target is read, which a change
/// of the motion below a step may tip to a value's other neighbour: TAA's
/// accumulation (its output), and FSR's upscaled history and output (its
/// prepared input colour is the input frame's, whatever the motion).
fn stores_after_motion(method: Antialiasing) -> f32 {
    match method {
        Antialiasing::Taa => 1.,
        Antialiasing::Fsr2 | Antialiasing::Fsr3 => 2.,
        other => panic!("{other:?} is not under test"),
    }
}

/// A wall of `BACKGROUND` filling the view `depth` metres ahead.
fn wall(depth: f32) -> (Shape, Pose) {
    let scale = depth / 8.;
    (
        Shape::card(Vec2::new(20., 12.), 0., BACKGROUND).scaled(scale),
        Pose::at(Vec3::new(0., 0., -depth)),
    )
}

/// `pose` `factor` times as far from the camera at the origin.
fn farther(pose: Pose, factor: f32) -> Pose {
    Pose {
        position: pose.position * factor,
        ..pose
    }
}

/// The stage's shapes, `scale` times the size.
fn stage_shapes(scale: f32) -> Vec<Shape> {
    vec![card().scaled(scale), grating().scaled(scale)]
}

/// The stage's poses `scale` times as far, and `behind` after them.
fn stage_poses_at(scale: f32, behind: Option<Pose>) -> impl Fn(u32) -> Vec<Pose> {
    move |f| {
        let mut poses: Vec<Pose> = stage_poses(f)
            .into_iter()
            .map(|p| farther(p, scale))
            .collect();
        poses.extend(behind);
        poses
    }
}

/// The stage at rest `scale` times as far, before `behind`.
fn far_stage<'a>(scale: f32, behind: Option<&(Shape, Pose)>, script: Script<'a>) -> Scenario<'a> {
    let mut shapes = stage_shapes(scale);
    shapes.extend(behind.map(|b| b.0.clone()));
    Scenario {
        shapes,
        camera: &still,
        script,
    }
}

/// The two distances each far case runs at: the stage's, and `FAR` times
/// as far.
const NEAR: (&str, f32) = ("near", 1.);
const DISTANT: (&str, f32) = ("far", FAR);
const DISTANCES: [(&str, f32); 2] = [NEAR, DISTANT];

/// A module named after a case with one test per method, each calling the
/// case's function, of the same name, with the method.
macro_rules! every_method {
    ($case:ident) => {
        mod $case {
            use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
            crate::scenario::temporal::case_tests!($case; taa: (Taa), fsr2: (Fsr2), fsr3: (Fsr3));
        }
    };
}

/// The square's corners of `pixel`.
fn corners(pixel: [u32; 2]) -> [Vec2; 4] {
    [(0., 0.), (1., 0.), (0., 1.), (1., 1.)]
        .map(|(x, y)| Vec2::new(pixel[0] as f32 + x, pixel[1] as f32 + y))
}

/// Case 1. Plausible defects: motion that includes the jitter, the wrong
/// sign or axis, a missing camera or object term, depth motion of the wrong
/// sign or validity, or sky motion that follows translation. The oracle is
/// analytic: a surface pixel's current-minus-previous UV and
/// previous-minus-current view depth from the script's poses and the
/// unjittered projection at its centre. The jittered sample lies anywhere
/// in the pixel, so the tolerance is the truth's spread over the pixel's
/// corners, a half-float step of the value, and `STILL_UV` or
/// `STILL_DEPTH`. The sky moves by the camera's rotation alone and has no
/// depth validity; a still scene's motion is zero.
#[test]
fn motion_vectors_match_the_script() {
    let Some(gpu) = gpu() else { return };
    let card_start = Pose::at(Vec3::new(-0.6, 0.2, -DEPTH));
    let (back, back_pose) = background();
    let cases: [(&str, Motion, Option<Motion>); 7] = [
        ("still", Motion::STILL, Some(Motion::STILL)),
        ("pan", VIEW.pan(0.37, DEPTH), Some(Motion::STILL)),
        (
            "dolly",
            Motion::translate(Vec3::new(0., 0., -0.05)),
            Some(Motion::STILL),
        ),
        ("yaw", Motion::yaw(0.004), Some(Motion::STILL)),
        (
            "across",
            Motion::STILL,
            Some(Motion::translate(Vec3::new(0.02, 0., 0.))),
        ),
        (
            "along",
            Motion::STILL,
            Some(Motion::translate(Vec3::new(0., 0., 0.05))),
        ),
        // The card rigidly attached to a dollying camera.
        ("follow", Motion::translate(Vec3::new(0., 0., -0.1)), None),
    ];
    let frame = 3;
    let mut rig: Option<Rig> = None;
    for (name, camera, card_motion) in cases {
        let camera_at = |f: u32| camera.pose(Pose::at(Vec3::ZERO), f);
        let card_at = |f: u32| match card_motion {
            Some(motion) => motion.pose(card_start, f),
            None => camera_at(f).then(card_start),
        };
        let script = |f: u32| vec![back_pose, card_at(f)];
        let scenario = Scenario {
            shapes: vec![back.clone(), card()],
            camera: &camera_at,
            script: &script,
        };
        let rig = match &mut rig {
            Some(rig) => {
                scenario.restage(rig);
                rig
            }
            None => rig.insert(scenario.rig(&gpu, VIEW, Antialiasing::Taa)),
        };
        for f in 0..=frame {
            scenario.render(rig, f);
        }
        let motion = rig.read(DiagnosticTarget::Motion);
        let sources = rig.read(DiagnosticTarget::SourceId);
        let now = VIEW.camera(camera_at(frame));
        let before = VIEW.camera(camera_at(frame - 1));
        let mut worst = [0f32; 2];
        for (i, (shape, id)) in scenario.shapes.iter().zip(rig.instances()).enumerate() {
            let [current, previous] = [frame, frame - 1].map(|f| script(f)[i].matrix());
            let truth = |pixel: Vec2| {
                VIEW.surface_motion(
                    &now,
                    &before,
                    shape.quads[0].corners,
                    current,
                    previous,
                    pixel,
                )
                .expect("the surface under its own pixel")
            };
            let interior = sources.region_of(diagnostics::source_id(id)).eroded(1);
            assert!(
                !interior.is_empty(),
                "{name}: instance {i} drew no interior"
            );
            for pixel in interior.iter() {
                let expected = truth(centre(pixel));
                let spread = corners(pixel)
                    .iter()
                    .map(|&c| (truth(c) - expected).abs())
                    .fold(Vec3::ZERO, Vec3::max);
                let [r, g, b, a] = motion.at(pixel);
                let off = (Vec3::new(r, g, b) - expected).abs();
                let tolerance = spread
                    + expected.abs() * HALF_STEP
                    + Vec3::new(STILL_UV, STILL_UV, STILL_DEPTH);
                worst = [worst[0].max(off.x.max(off.y)), worst[1].max(off.z)];
                assert!(
                    off.cmple(tolerance).all() && a == 1.,
                    "{name}: instance {i} at {pixel:?} wrote motion {:?} where the script gives {expected} ± {tolerance}",
                    [r, g, b, a]
                );
            }
        }
        let sky = sources.region_of(0).eroded(1);
        assert!(!sky.is_empty(), "{name}: no sky in view");
        for pixel in sky.iter() {
            let expected = VIEW.sky_motion(&now, &before, centre(pixel));
            let spread = corners(pixel)
                .iter()
                .map(|&c| (VIEW.sky_motion(&now, &before, c) - expected).abs())
                .fold(Vec2::ZERO, Vec2::max);
            let [r, g, _, a] = motion.at(pixel);
            let off = (Vec2::new(r, g) - expected).abs();
            worst[0] = worst[0].max(off.x.max(off.y));
            assert!(
                off.cmple(spread + expected.abs() * HALF_STEP + STILL_UV)
                    .all()
                    && a == 0.,
                "{name}: the sky at {pixel:?} wrote motion {:?} where the camera's rotation gives {expected}",
                motion.at(pixel)
            );
        }
        eprintln!(
            "motion {name}: largest error {} px, {} m",
            worst[0] * VIEW.size[0] as f32,
            worst[1]
        );
    }
}

/// Case 1 far from the render origin (M-20, B-12 of the 2026-11-18 history
/// audit). Plausible defect: the previous clip position composed in another
/// order than the current one (`(previous view-projection × previous
/// model) × vertex` against `view-projection × (model × vertex)`), whose
/// f32 rounding differs where coordinates are large, so a still camera and
/// still geometry 100 m sideways of the origin wrote 1.9e-4 px of motion.
/// The oracle is analytic: nothing moved, so every pixel's motion is
/// exactly 0, the stage's and the sky's, with the camera bit-identical
/// every frame, at the origin, 100 m sideways and kilometres away (the
/// stage `FAR` times as far there).
#[test]
fn a_still_scene_far_from_the_origin_writes_no_motion() {
    let Some(gpu) = gpu() else { return };
    let mut rig: Option<Rig> = None;
    for ((distance, scale), offset) in [
        (NEAR, Vec3::ZERO),
        (NEAR, Vec3::new(100., 0., 0.)),
        (DISTANT, Vec3::new(3000., 1500., -4000.)),
    ] {
        let script = move |f: u32| {
            stage_poses_at(scale, None)(f)
                .into_iter()
                .map(|p| Pose::at(offset).then(p))
                .collect()
        };
        let camera = move |_: u32| Pose::at(offset);
        let scenario = Scenario {
            shapes: stage_shapes(scale),
            camera: &camera,
            script: &script,
        };
        let rig = match &mut rig {
            Some(rig) => {
                scenario.restage(rig);
                rig
            }
            None => rig.insert(scenario.rig(&gpu, VIEW, Antialiasing::Taa)),
        };
        let mut largest = [0f32; 2];
        for frame in 0..4 {
            scenario.render(rig, frame);
            if frame == 0 {
                continue;
            }
            for texel in &rig.read(DiagnosticTarget::Motion).texels {
                largest[0] = largest[0].max(texel[0].abs().max(texel[1].abs()));
                largest[1] = largest[1].max(texel[2].abs());
            }
        }
        eprintln!(
            "{distance} stage seen from {offset}: largest motion {} px, depth motion {} m",
            largest[0] * VIEW.size[0] as f32,
            largest[1]
        );
        assert_eq!(
            largest, [0.; 2],
            "a still {distance} stage seen from {offset} by a still camera wrote motion"
        );
    }
}

/// Case 2. Plausible defects: jitter of a pixel or more, no jitter (no
/// resolve converges on coverage), or jitter leaking into the motion
/// target, which the resolve would read as motion. The oracles: the
/// `Motion` target of a still scene is zero within `STILL_UV` and
/// `STILL_DEPTH` on every frame of a cycle (analytic); a pixel's silhouette
/// in the `Depth` target changes between frames only where its box
/// straddles the card's projected edge, since a sub-pixel jitter keeps each
/// sample inside its pixel (bound); and some silhouette pixel changes over
/// a cycle, since the antialiasing in effect jitters the projection
/// (Temporal design).
fn jitter_is_sub_pixel_and_never_in_the_motion(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let one = |_| vec![Pose::at(Vec3::new(0., 0., -DEPTH))];
    let scenario = Scenario {
        shapes: vec![card()],
        camera: &still,
        script: &one,
    };
    let outline = &scenario.polygons(0)[0][0];
    let straddles = Region::from_fn(VIEW.size, |pixel| {
        let c = coverage(outline, pixel, 0.);
        c > 0. && c < 1.
    });
    let mut rig = scenario.rig(&gpu, VIEW, method);
    let mut last: Option<Region> = None;
    let mut changed = 0;
    for frame in 0..=JITTER_CYCLE + 1 {
        scenario.render(&mut rig, frame);
        let motion = rig.read(DiagnosticTarget::Motion);
        for pixel in pixels(VIEW.size) {
            let [r, g, b, _] = motion.at(pixel);
            assert!(
                r.abs() <= STILL_UV && g.abs() <= STILL_UV && b.abs() <= STILL_DEPTH,
                "{method:?} frame {frame}: a still scene wrote motion {:?} at {pixel:?}",
                motion.at(pixel)
            );
        }
        let depth = rig.read(DiagnosticTarget::Depth);
        let silhouette = Region::from_fn(VIEW.size, |pixel| depth.at(pixel)[0] > 0.);
        if let Some(last) = &last {
            for pixel in pixels(VIEW.size) {
                if last.contains(pixel) != silhouette.contains(pixel) {
                    changed += 1;
                    assert!(
                        straddles.contains(pixel),
                        "{method:?} frame {frame}: pixel {pixel:?}, whose box misses the card's edge, changed silhouette: the jitter left the pixel"
                    );
                }
            }
        }
        last = Some(silhouette);
    }
    eprintln!("{method:?}: {changed} silhouette changes over a cycle");
    assert!(changed > 0, "{method:?} never jittered the projection");
}
every_method!(jitter_is_sub_pixel_and_never_in_the_motion);

/// Case 3. Plausible defects: the resolve does not accumulate the jittered
/// samples (no jitter, history discarded, a wrong blend), or, with the
/// stage `FAR` times as far, discards it there because depth's magnitude
/// selects its rejection (#791). The oracle is the reference: after the
/// hold and a whole jitter cycle every edge pixel has averaged every jitter
/// phase, so its RMSE against the supersampled frame is at least `bar`
/// times below the single centre sample's, on the grating's strips and on
/// the outlines alike, at either distance.
fn at_rest_the_resolve_converges_on_coverage(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let mut failures = Failures::default();
    for (distance, scale) in DISTANCES {
        let script = stage_poses_at(scale, None);
        let scenario = far_stage(scale, None, &script);
        let mut oracles = Oracles::new(&scenario, &gpu);
        let truth = oracles.reference(&scenario, 0);
        let single = oracles.single(&scenario, 0);
        let regions = [
            (Edge::Strips, scenario.strip_edges(0, 1)),
            (Edge::Outline, scenario.outline_edges(0, &[0, 1])),
        ];
        let mut rig = scenario.rig(&gpu, VIEW, method);
        for frame in 0..SETTLE {
            scenario.render(&mut rig, frame);
        }
        let resolved = rig.antialiased();
        let mut dump = Dump::new(&format!("at_rest_{distance}_{method:?}"));
        dump.image("converged", &resolved);
        dump.image("reference", &truth);
        for (edge, region) in &regions {
            let ours = error(&resolved, &truth, region).rmse;
            let theirs = error(&single, &truth, region).rmse;
            let bar = bar(method, &rig, *edge);
            dump.metric(format!("{edge:?}_rmse"), ours);
            dump.metric(format!("{edge:?}_single_sample_rmse"), theirs);
            eprintln!(
                "{method:?} at rest, {distance}, {edge:?} ({} pixels): RMSE {ours}, a single sample's {theirs}, {} times below against the bar {bar}",
                region.len(),
                theirs / ours
            );
            failures.check(ours * bar <= theirs, || {
                format!(
                    "{method:?} at rest, {distance}, {edge:?}: the converged resolve's RMSE {ours} is not {bar} times below a single sample's {theirs}"
                )
            });
        }
        failures.bounded(&rig, format!("{method:?} at rest, {distance}"));
    }
    failures.assert();
}
every_method!(at_rest_the_resolve_converges_on_coverage);

/// What a case adds to a rig's frame beside its scenario, after the
/// scenario posed the frame's camera and shapes and before the frame
/// renders.
type Elsewhere<'a> = &'a dyn Fn(&mut Rig, u32);

/// Case 4, for TAA, whose resting blend the spec states (FSR's is AMD's,
/// with no stated number). Plausible defects: the resolve keeps rejecting
/// history at rest, so edges shimmer; or does so where depth is far, the
/// sky's included, so far detail and every outline against the sky or a far
/// surface shimmer (#791). The oracle is the requirement of
/// Frame item 11: at rest TAA keeps partial rectification and blends in the
/// current frame at 1 - 0.88 times Filament's alpha floor 0.3, its variance
/// box at Filament's floor 0.1 (`sgl-post-fx` PROVENANCE.md,
/// "Motion-adaptive TAA"), in its blend space v / (1 + v), whatever the
/// depth. From one frame
/// to the next a pixel then moves at most 0.036 of the way to the current
/// sample plus 0.964 times 0.1 of the way to its clipped history, each
/// within the fixture's contrast in that space, plus a half-float step for
/// each frame's store. Both terms belong in it: the bound is tight, and the
/// feedback term alone (0.0043 here) sits below the 0.0050 a resting edge
/// pixel reaches through the partial rectification the spec keeps. The bound
/// holds over `region` for the two jitter cycles after the resolve settled.
/// `elsewhere` adds to the frame what the scenario does not hold before each
/// frame renders (content beside the region that changes while it rests,
/// or the camera's rounding).
fn taa_holds_still_at_rest(
    failures: &mut Failures,
    gpu: &Gpu,
    name: &str,
    scenario: &Scenario,
    region: &Region,
    elsewhere: Elsewhere,
) {
    let mut rig = scenario.rig(gpu, VIEW, Antialiasing::Taa);
    let mut resolved = Vec::new();
    for frame in 0..SETTLE + 2 * JITTER_CYCLE {
        scenario.pose(&mut rig, frame);
        elsewhere(&mut rig, frame);
        rig.render(frame);
        if frame + 1 >= SETTLE {
            resolved.push(rig.antialiased());
        }
    }
    let sdr = |v: f32| v / (1. + v);
    let [low, high] = rig.radiance_range();
    let feedback = (1. - 0.88) * 0.3;
    let bound = (feedback + (1. - feedback) * 0.1) * (sdr(high) - sdr(low)) + 2. * high * HALF_STEP;
    assert!(!region.is_empty(), "{name}: an empty region");
    let stability = image::stability(&resolved, region, sdr);
    let mut dump = Dump::new(&format!(
        "at_rest_stability_{}",
        name.replace([' ', ','], "_")
    ));
    dump.metric("max_step_sdr", stability.max_step);
    dump.metric("std_dev_sdr", stability.std_dev);
    dump.metric("bound_sdr", bound);
    eprintln!("TAA at rest, {name}: {stability:?} against the bound {bound}");
    failures.check(stability.max_step <= bound, || {
        format!(
            "TAA at rest changed a pixel of the {name} by {} in its blend space, above the resting blend's {bound}",
            stability.max_step
        )
    });
    failures.bounded(&rig, format!("TAA at rest, {name}"));
}

/// Case 4 where both sides of each edge are geometry: the grating's strips
/// on its card, two pixels inside the card's outline, near and far.
#[test]
fn at_rest_taa_holds_edges_between_surfaces_still() {
    let Some(gpu) = gpu() else { return };
    let mut failures = Failures::default();
    for (distance, scale) in DISTANCES {
        let script = stage_poses_at(scale, None);
        let scenario = far_stage(scale, None, &script);
        let region = scenario.edges(0, &[1]).and(&scenario.inside(0, 1, 2.));
        let name = format!("edges between {distance} surfaces");
        taa_holds_still_at_rest(&mut failures, &gpu, &name, &scenario, &region, &|_, _| {});
    }
    failures.assert();
}

/// Case 4 at the stage's outlines, near and far, before `behind` or the
/// sky: the edge pixels within two pixels of the cards' outlines, outside
/// them.
fn taa_holds_outlines_still(what: &str, behind: Option<(Shape, Pose)>) {
    let Some(gpu) = gpu() else { return };
    let mut failures = Failures::default();
    for (distance, scale) in DISTANCES {
        let script = stage_poses_at(scale, behind.as_ref().map(|b| b.1));
        let scenario = far_stage(scale, behind.as_ref(), &script);
        let near_outline = scenario
            .inside(0, 0, 2.)
            .or(&scenario.inside(0, 1, 2.))
            .not();
        let region = scenario.edges(0, &[0, 1]).and(&near_outline);
        let name = format!("{distance} silhouettes against {what}");
        taa_holds_still_at_rest(&mut failures, &gpu, &name, &scenario, &region, &|_, _| {});
    }
    failures.assert();
}

/// Case 4 at silhouettes against the sky (#791).
#[test]
fn at_rest_taa_holds_silhouettes_against_the_sky_still() {
    taa_holds_outlines_still("the sky", None);
}

/// Case 4 at silhouettes against a far surface (#791): a wall at 4 km,
/// behind the stage near and far.
#[test]
fn at_rest_taa_holds_silhouettes_against_a_far_surface_still() {
    taa_holds_outlines_still("a far surface", Some(wall(8. * FAR)));
}

/// Case 5. Plausible defects: history lost or misplaced under camera
/// translation (reprojection off by the jitter or a pixel, rejection of
/// motion that is only the camera's). The oracles: at every frame of a pan
/// at an integer and a fractional rate, after the settle, the RMSE over the
/// edges against that frame's reference is never above a single sample's
/// (reference); and a pixel whose box, grown by the 2 px reach of the
/// bicubic history fetch, has lain wholly inside the card, or wholly in the
/// sky, for the hold shows that radiance within the method's `stores`
/// (analytic).
fn panning_keeps_history(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let mut oracles: Option<Oracles> = None;
    let mut failures = Failures::default();
    let mut rig: Option<Rig> = None;
    for rate in [1., 0.37] {
        // Leftward, so the stage crosses the view without leaving it.
        let pan = VIEW.pan(-rate, DEPTH);
        let camera = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
        let scenario = stage(&camera);
        let oracles = match &mut oracles {
            Some(oracles) => {
                oracles.restage(&scenario);
                oracles
            }
            None => oracles.insert(Oracles::new(&scenario, &gpu)),
        };
        let frames = SETTLE + 2 * HOLD;
        let truths: Vec<(Image, Image)> = (SETTLE..frames)
            .map(|f| {
                (
                    oracles.reference(&scenario, f),
                    oracles.single(&scenario, f),
                )
            })
            .collect();
        // Each measured frame's edges, and the pixels held inside the card
        // and in the sky for the hold up to it.
        let inside: Vec<Region> = (0..frames).map(|f| scenario.inside(f, 0, 2.)).collect();
        let outside: Vec<Region> = (0..frames).map(|f| scenario.outside(f, 2.)).collect();
        let held = |regions: &[Region], frame: u32| {
            regions[(frame + 1 - HOLD) as usize..=frame as usize]
                .iter()
                .fold(regions[frame as usize].clone(), |a, b| a.and(b))
        };
        let measured: Vec<(Region, [(Region, f32); 2])> = (SETTLE..frames)
            .map(|f| {
                let held = [(held(&inside, f), CARD), (held(&outside, f), SKY)];
                (scenario.edges(f, &[0, 1]), held)
            })
            .collect();
        let rig = match &mut rig {
            Some(rig) => {
                scenario.restage(rig);
                rig
            }
            None => rig.insert(scenario.rig(&gpu, VIEW, method)),
        };
        // The largest RMSE ratio to a single sample's, and in each held
        // region the largest error relative to its radiance, with where.
        let mut worst = (0f32, 0);
        let mut held_error = [(0f32, 0, [0; 2]); 2];
        for frame in 0..frames {
            scenario.render(rig, frame);
            if frame < SETTLE {
                continue;
            }
            let resolved = rig.antialiased();
            let (truth, single) = &truths[(frame - SETTLE) as usize];
            let (edges, held) = &measured[(frame - SETTLE) as usize];
            let [ours, theirs] = [&resolved, single].map(|i| error(i, truth, edges).rmse);
            if ours / theirs > worst.0 {
                worst = (ours / theirs, frame);
            }
            for ((region, radiance), held_error) in held.iter().zip(&mut held_error) {
                let e = error_to(&resolved, *radiance, region);
                if e.max / radiance > held_error.0 {
                    *held_error = (e.max / radiance, frame, e.worst);
                }
            }
        }
        eprintln!(
            "{method:?} panning {rate} px a frame: RMSE at most {} of a single sample's (frame {}); held pixels within {:?} relative (card, sky: error, frame, pixel)",
            worst.0, worst.1, held_error
        );
        failures.check(worst.0 <= 1., || {
            format!(
                "{method:?} panning {rate} px a frame: RMSE {} times a single sample's at frame {}",
                worst.0, worst.1
            )
        });
        for (name, (error, frame, pixel)) in ["card", "sky"].into_iter().zip(held_error) {
            failures.check(error <= stores(method) * HALF_STEP, || {
                format!(
                    "{method:?} panning {rate} px a frame: a pixel held in the {name} for the hold is {error} of its radiance off, frame {frame} pixel {pixel:?}"
                )
            });
        }
    }
    failures.bounded(rig.as_ref().unwrap(), format!("{method:?} panning"));
    failures.assert();
}
every_method!(panning_keeps_history);

/// What stands behind a case's moving or changing card: the background
/// card (the near variant's) or the sky.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Behind {
    Background,
    Sky,
}

/// The variants of cases 6 and 9: the card before the background, and
/// against the sky near and `FAR` times as far.
const AGAINST: [(&str, f32, Behind); 3] = [
    ("before the background", 1., Behind::Background),
    ("against the sky", 1., Behind::Sky),
    ("far, against the sky", FAR, Behind::Sky),
];

/// Case 6. Plausible defect: history ghosts where a moving surface uncovers
/// another or the sky (no disocclusion, no rejection). The oracle is
/// analytic with the spec's hold: a pixel `vacated` by the moving card for
/// the hold shows what lies behind, the still background or the sky, within
/// the method's `stores`; against the sky near and far (#791).
fn a_disocclusion_recovers(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let (back, back_pose) = background();
    let mut failures = Failures::default();
    for (name, scale, behind) in AGAINST {
        let across = Motion::translate(Vec3::new(1.5 * px() * scale, 0., 0.));
        let start = farther(Pose::at(Vec3::new(-1.2, 0., -DEPTH)), scale);
        let script = move |f: u32| {
            let mut poses = vec![across.pose(start, f)];
            if behind == Behind::Background {
                poses.push(back_pose);
            }
            poses
        };
        let mut shapes = vec![card().scaled(scale)];
        if behind == Behind::Background {
            shapes.push(back.clone());
        }
        let scenario = Scenario {
            shapes,
            camera: &still,
            script: &script,
        };
        let frames = SETTLE + 2 * HOLD;
        let truth = match behind {
            Behind::Background => BACKGROUND,
            Behind::Sky => SKY,
        };
        let vacated: Vec<Region> = vacated(&scenario, 0, frames, 2.)
            .into_iter()
            .enumerate()
            .map(|(i, region)| match behind {
                Behind::Background => region.and(&scenario.inside(HOLD + i as u32, 1, 2.)),
                Behind::Sky => region,
            })
            .collect();
        let mut rig = scenario.rig(&gpu, VIEW, method);
        let mut checked = 0;
        let mut residual = (0f32, 0, [0; 2]);
        for frame in 0..frames {
            scenario.render(&mut rig, frame);
            let resolved = rig.antialiased();
            if frame < HOLD {
                continue;
            }
            let vacated = &vacated[(frame - HOLD) as usize];
            if vacated.is_empty() {
                continue;
            }
            let e = error_to(&resolved, truth, vacated);
            if e.max > residual.0 {
                residual = (e.max, frame, e.worst);
            }
            checked += e.pixels;
        }
        eprintln!(
            "{method:?} disocclusion {name}: {checked} pixel-frames vacated for the hold, residual at most {} (frame {}, pixel {:?})",
            residual.0, residual.1, residual.2
        );
        assert!(
            checked > 0,
            "{name}: the card vacated no pixel for the hold"
        );
        failures.check(residual.0 <= stores(method) * truth * HALF_STEP, || {
            format!(
                "{method:?} {name}: a pixel vacated for the hold is {} off what lies behind, frame {} pixel {:?}",
                residual.0, residual.1, residual.2
            )
        });
        failures.bounded(&rig, format!("{method:?} disocclusion {name}"));
    }
    failures.assert();
}
every_method!(a_disocclusion_recovers);

/// Case 7 (#768). Plausible defect: history rejected on a surface whose
/// view depth and screen position stay fixed while the camera moves, as a
/// chase camera's craft does, because the depth test expects the camera's
/// motion alone. The oracle is a closeness bound: a grating rigidly attached
/// to a camera dollying 0.2 m a frame over a still background keeps its
/// screen position and depth, so in its interior it is sampled exactly as at
/// rest, and a resolve that keeps its history converges there on the still
/// run's picture. One that discards it shows a single jittered sample. So
/// after the settle and a cycle, on the strips two pixels inside its
/// outline (whose neighbourhoods see no moving background), the following
/// run is at least case 3's strip `bar` times closer to the still run than
/// the single centre sample is.
fn a_surface_the_camera_follows_keeps_its_history(method: Antialiasing) {
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
    let last = SETTLE + JITTER_CYCLE - 1;
    let interior = following
        .strip_edges(last, 1)
        .and(&following.inside(last, 1, 2.));
    let single = Oracles::new(&following, &gpu).single(&following, last);
    let mut failures = Failures::default();
    let mut rig = resting.rig(&gpu, VIEW, method);
    let [still_image, follow_image] = [&resting, &following].map(|scenario| {
        scenario.restage(&mut rig);
        for frame in 0..=last {
            scenario.render(&mut rig, frame);
        }
        rig.antialiased()
    });
    let ours = error(&follow_image, &still_image, &interior);
    let theirs = error(&single, &still_image, &interior);
    let mut dump = Dump::new(&format!("follow_{method:?}"));
    dump.image("still", &still_image);
    dump.image("following", &follow_image);
    dump.metric("following_rmse", ours.rmse);
    dump.metric("single_sample_rmse", theirs.rmse);
    eprintln!("{method:?} following: {ours:?} from the still run; a single sample {theirs:?}");
    let bar = bar(method, &rig, Edge::Strips);
    eprintln!(
        "{method:?} following: {} times closer than a single sample, against the bar {bar}",
        theirs.rmse / ours.rmse
    );
    failures.check(ours.rmse * bar <= theirs.rmse, || {
        format!(
            "{method:?}: the followed grating is {} from its still resolve, not {bar} times below a single sample's {} (#768)",
            ours.rmse, theirs.rmse
        )
    });
    failures.bounded(&rig, format!("{method:?} following"));
    failures.assert();
}
every_method!(a_surface_the_camera_follows_keeps_its_history);

/// Case 8 (#768). Plausible defect: the same rejection on a surface
/// approaching a still camera. A grating centred ahead comes 3 mm a frame
/// closer from 0.6 m: past 0.55 m its depth changes by 0.1 x 0.003 / z^2 of
/// normalized depth a frame, above the adaptive depth threshold 0.001 (Frame
/// item 11), throughout its interior, where every depth around a pixel is
/// the grating's (at its outline the neighbourhood holds the background's
/// depth too, and the depth test cannot reject there). Its strips are 0.4 px
/// wide where it ends. The oracle is the reference, on the strips whose
/// script moves them under 0.1 px a frame (those near the view axis): over
/// the frames after the settle their RMSE against each frame's reference
/// stays case 3's strip `bar` times below the single centre sample's,
/// relaxed by the RMSE between that frame's reference and the last one's,
/// the most a resolve one frame behind could lose. That relaxation stays
/// below what the bar leaves of a single sample's error, so the scenario
/// fails both antialiasing off and a resolve that discards its history, a
/// single jittered sample; the case checks that it does.
fn a_surface_moving_along_the_view_keeps_its_history(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let (back, back_pose) = background();
    let approach = Motion::translate(Vec3::new(0., 0., 0.003));
    let start = Pose::at(Vec3::new(0., 0., -0.6));
    let script = move |f: u32| vec![back_pose, approach.pose(start, f)];
    let px = VIEW.pixel_metres(0.5);
    let near_grating = Shape::grating(
        Vec2::new(0.3, 0.2),
        0.25,
        GRATING,
        STRIPS,
        0.4 * px,
        1.6 * px,
    );
    let scenario = Scenario {
        shapes: vec![back, near_grating],
        camera: &still,
        script: &script,
    };
    let frames = SETTLE + 4;
    // The strips two pixels inside the outline whose script moves them
    // under 0.1 px a frame.
    let measured = |frame: u32| {
        let camera = VIEW.camera(still(frame));
        let [now, before] = [frame, frame - 1].map(|f| script(f)[1].matrix());
        let corners = scenario.shapes[1].quads[0].corners;
        let size = Vec2::new(VIEW.size[0] as f32, VIEW.size[1] as f32);
        let slow = Region::from_fn(VIEW.size, |pixel| {
            VIEW.surface_motion(&camera, &camera, corners, now, before, centre(pixel))
                .is_some_and(|motion| (motion.truncate() * size).length() < 0.1)
        });
        scenario
            .strip_edges(frame, 1)
            .and(&scenario.inside(frame, 1, 2.))
            .and(&slow)
    };
    let mut oracles = Oracles::new(&scenario, &gpu);
    let truths: Vec<(Image, Image)> = (SETTLE - 1..frames)
        .map(|f| {
            (
                oracles.reference(&scenario, f),
                oracles.single(&scenario, f),
            )
        })
        .collect();
    let mut failures = Failures::default();
    let mut rig = scenario.rig(&gpu, VIEW, method);
    let bar = bar(method, &rig, Edge::Strips);
    for frame in 0..frames {
        scenario.render(&mut rig, frame);
        if frame < SETTLE {
            continue;
        }
        let resolved = rig.antialiased();
        let index = (frame + 1 - SETTLE) as usize;
        let (truth, single) = &truths[index];
        let strips = measured(frame);
        let ours = error(&resolved, truth, &strips).rmse;
        let theirs = error(single, truth, &strips).rmse;
        let change = error(&truths[index - 1].0, truth, &strips).rmse;
        eprintln!(
            "{method:?} approaching, frame {frame} ({} pixels): RMSE {ours}, a single sample's {theirs}, the reference's change {change}, bar {bar}",
            strips.len()
        );
        failures.check(change < theirs * (1. - 1. / bar), || {
            format!(
                "approaching, frame {frame}: the reference changes by {change}, too much for a single sample's {theirs} to fail the bar {bar}"
            )
        });
        failures.check(ours <= theirs / bar + change, || {
            format!(
                "{method:?} approaching, frame {frame}: the strips' RMSE {ours} is not {bar} times below a single sample's {theirs} plus the frame's change {change} (#768)"
            )
        });
    }
    failures.bounded(&rig, format!("{method:?} approaching"));
    failures.assert();
}
every_method!(a_surface_moving_along_the_view_keeps_its_history);

/// A game's shader (`Scene::add_shader`) whose vertex function moves every
/// vertex by its parameters' `velocity` metres a second from `start`
/// seconds on and rests before: a surface that starts moving from rest with
/// no pose change, so nothing sets the scene's view-wide rigid signal
/// (Frame item 11: procedural material deformation does not), while its
/// motion, measured from the function's evaluation at the last submitted
/// frame (`VertexContext::previous`), is in the motion target.
const DEPARTING_SHADER: &str = r#"
struct ShaderParams {
 velocity:vec3<f32>,
 start:f32,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 out.position+=params.velocity*max(ctx.time-params.start,0.);
 return out;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
"#;

/// What lies behind the card case 13 and #802's scenarios move.
#[derive(Clone, Copy, Debug)]
enum Over {
    Sky,
    /// A wall at 4 km.
    FarWall,
    /// A wall at 8 m.
    NearWall,
}

impl Over {
    /// Its name and its wall.
    fn wall(self) -> (&'static str, Option<(Shape, Pose)>) {
        match self {
            Over::Sky => ("over the sky", None),
            Over::FarWall => ("over a far wall", Some(wall(8. * FAR))),
            Over::NearWall => ("over a near wall", Some(wall(8.))),
        }
    }
}

/// What moves the card of case 13 and #802's scenarios: with no pose
/// change, or by its pose.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mover {
    /// Its material's vertex function (`DEPARTING_SHADER`). A shaded
    /// material also marks the transparency and composition mask wherever
    /// it is drawn (case 9's dissolves).
    Shader,
    /// A morph target its weight moves (`Rig::add_morphing`): no shader and
    /// no mask, so only motion re-arms rejection.
    Morph,
    /// Its pose, as a moving instance (`Rig::add`, `Rig::place`): no shader
    /// and no mask, and since its last submitted pose applies, no view-wide
    /// signal (M-9), so only motion re-arms rejection.
    Pose,
}

/// Case 13 and #802's scenarios: `shape` (`card()` unless a case says
/// otherwise) at `DEPTH`, `scale` times as far, before `wall` or the sky
/// (named `behind`), that `mover` starts moving from rest at `rate` pixels
/// a frame at frame `start_frame`, after the hold has expired, rendered with
/// `method` to `frames`, its failures added to `failures`. The oracle is
/// analytic with the spec's hold, as case 6's: a pixel `vacated` by the
/// card, whose pose the shader's motion, the morph weight or the pose gives
/// exactly (the frame's time is `FrameInput::elapsed_seconds` in f32), shows
/// what lies behind within the method's `stores`.
fn departures(
    failures: &mut Failures,
    (method, gpu): (Antialiasing, &Gpu),
    (behind, scale, wall): (&str, f32, Option<&(Shape, Pose)>),
    (shape, mover, rate): (&Shape, Mover, f32),
    (start_frame, frames): (u32, u32),
) {
    let name = format!("{behind} at {rate} px a frame, moved by {mover:?}");
    let shape = shape.scaled(scale);
    let rest = farther(Pose::at(Vec3::new(-0.6, 0., -DEPTH)), scale);
    let velocity = Vec3::X * rate * px() * scale * FRAME_RATE as f32;
    let start = (f64::from(start_frame) / FRAME_RATE) as f32;
    let moved = move |f: u32| {
        let time = (f64::from(f) / FRAME_RATE) as f32;
        vec![Pose::at(rest.position + velocity * (time - start).max(0.))]
    };
    let truth = Scenario {
        shapes: vec![shape.clone()],
        camera: &still,
        script: &moved,
    };
    let vacated = vacated(&truth, 0, frames, 2.);
    let value = if wall.is_some() { BACKGROUND } else { SKY };
    let params = [velocity.x, velocity.y, velocity.z, start];
    let bound = velocity.length() * frames as f32 / FRAME_RATE as f32;
    let mut rig = Rig::new(gpu, VIEW, method, SKY);
    if let Some((wall, pose)) = wall {
        rig.add(wall, *pose, 0);
    }
    // The morph target moves the card as far as the shader does by the
    // last frame, at weight 1.
    let span = frames as f32 / FRAME_RATE as f32;
    let (morphing, posed) = match mover {
        Mover::Shader => {
            let shader = rig
                .scene
                .add_shader(ShaderSource {
                    wgsl: DEPARTING_SHADER.into(),
                    label: "departing".into(),
                })
                .unwrap_or_else(|error| panic!("{error}"));
            rig.add_shaded(
                &shape,
                rest,
                0,
                (shader, bound),
                bytemuck::bytes_of(&params),
            );
            (None, None)
        }
        Mover::Morph => (Some(rig.add_morphing(&shape, rest, velocity * span)), None),
        Mover::Pose => (None, Some(rig.add(&shape, rest, 0))),
    };
    rig.look(still(0));
    let mut checked = 0;
    let mut residual = (0f32, 0, [0; 2]);
    for frame in 0..frames {
        if let Some(id) = morphing
            && frame > start_frame
        {
            let time = (f64::from(frame) / FRAME_RATE) as f32;
            rig.scene
                .set_instance_deformation(&rig.gpu.queue, id, &[], &[(time - start) / span])
                .unwrap();
        }
        if let Some(id) = posed
            && frame > start_frame
        {
            rig.place(id, moved(frame)[0]);
        }
        rig.render(frame);
        let resolved = rig.antialiased();
        if frame < HOLD {
            continue;
        }
        let vacated = &vacated[(frame - HOLD) as usize];
        if vacated.is_empty() {
            continue;
        }
        let e = error_to(&resolved, value, vacated);
        if e.max > residual.0 {
            residual = (e.max, frame, e.worst);
        }
        checked += e.pixels;
    }
    eprintln!(
        "{method:?} {name}: {checked} pixel-frames vacated for the hold, residual at most {} (frame {}, pixel {:?})",
        residual.0, residual.1, residual.2
    );
    assert!(
        checked > 0,
        "{name}: the card vacated no pixel for the hold"
    );
    failures.check(residual.0 <= stores(method) * value * HALF_STEP, || {
        format!(
            "{method:?} {name}: a pixel vacated for the hold is {} off what lies behind, frame {} pixel {:?}",
            residual.0, residual.1, residual.2
        )
    });
    failures.bounded(&rig, format!("{method:?} {name}"));
}

/// Case 13 (#791). Plausible defect: a surface its material's vertex
/// function starts moving from rest, after the hold has expired, leaves a
/// ghost on what it uncovers, because no rigid signal re-arms the view and
/// depth's magnitude no longer restores rejection. Near, depth
/// disocclusion sees the sky or the wall behind the card; far, the card's
/// normalized depth (4e-5) differs from the sky's or the far wall's by less
/// than the depth threshold. At 0.5 px a frame, under a pixel a frame,
/// every pixel the card uncovers lies within the 3x3 closest-motion search
/// of the moving card, so its own motion re-arms the hold there (faster:
/// #802's scenarios below). The oracle is `departures`': the sky, a wall at
/// 4 km behind the card near and far, and a wall at 8 m behind the near
/// card, one test each.
fn a_surface_its_vertex_function_moves_from_rest_uncovers_what_lies_behind(
    method: Antialiasing,
    (distance, scale): (&str, f32),
    over: Over,
) {
    let Some(gpu) = device(method) else { return };
    let (behind, wall) = over.wall();
    let name = format!("{distance} {behind}");
    let mut failures = Failures::default();
    departures(
        &mut failures,
        (method, &gpu),
        (&name, scale, wall.as_ref()),
        (&card(), Mover::Shader, 0.5),
        (SETTLE, SETTLE + 3 * HOLD),
    );
    failures.assert();
}

mod a_surface_its_vertex_function_moves_from_rest_uncovers_what_lies_behind {
    use super::Over::{FarWall, NearWall, Sky};
    use super::{DISTANT, NEAR};
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
    crate::scenario::temporal::case_tests!(a_surface_its_vertex_function_moves_from_rest_uncovers_what_lies_behind;
        taa_near_over_the_sky: (Taa, NEAR, Sky),
        taa_far_over_the_sky: (Taa, DISTANT, Sky),
        taa_near_over_a_far_wall: (Taa, NEAR, FarWall),
        taa_far_over_a_far_wall: (Taa, DISTANT, FarWall),
        taa_near_over_a_near_wall: (Taa, NEAR, NearWall),
        fsr2_near_over_the_sky: (Fsr2, NEAR, Sky),
        fsr2_far_over_the_sky: (Fsr2, DISTANT, Sky),
        fsr2_near_over_a_far_wall: (Fsr2, NEAR, FarWall),
        fsr2_far_over_a_far_wall: (Fsr2, DISTANT, FarWall),
        fsr2_near_over_a_near_wall: (Fsr2, NEAR, NearWall),
        fsr3_near_over_the_sky: (Fsr3, NEAR, Sky),
        fsr3_far_over_the_sky: (Fsr3, DISTANT, Sky),
        fsr3_near_over_a_far_wall: (Fsr3, NEAR, FarWall),
        fsr3_far_over_a_far_wall: (Fsr3, DISTANT, FarWall),
        fsr3_near_over_a_near_wall: (Fsr3, NEAR, NearWall),
    );
}

/// The frames #802's scenarios run: the first hold, which every history
/// starts with, expires, then the card moves for three holds, so pixels it
/// vacated for a hold are judged.
const DEPARTURE_FRAMES: (u32, u32) = (HOLD + 1, HOLD + 1 + 3 * HOLD);

/// #802 at `rate` pixels a frame, over the sky, a wall at 4 km and a wall
/// at 8 m behind the card at `DEPTH`: faster than the 3x3 closest-motion
/// search reaches, so under TAA only the reach of the moving card (Frame
/// item 11) re-arms the hold on what it uncovers, and FSR2 and FSR3 resolve
/// it with their own reprojection and locks. FSR2's radiance bound at 4 px
/// a frame, where the card leaves the frame at its edge, guards #808. The
/// oracle is `departures`'.
fn a_surface_its_vertex_function_moves_from_rest_beyond_the_search(
    method: Antialiasing,
    rate: f32,
) {
    let Some(gpu) = device(method) else { return };
    let mut failures = Failures::default();
    for over in [Over::Sky, Over::FarWall, Over::NearWall] {
        let (behind, wall) = over.wall();
        departures(
            &mut failures,
            (method, &gpu),
            (behind, 1., wall.as_ref()),
            (&card(), Mover::Shader, rate),
            DEPARTURE_FRAMES,
        );
    }
    failures.assert();
}

mod a_surface_its_vertex_function_moves_from_rest_at_1_5_px_a_frame {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
    crate::scenario::temporal::case_tests!(a_surface_its_vertex_function_moves_from_rest_beyond_the_search;
        taa: (Taa, 1.5),
        fsr2: (Fsr2, 1.5),
        fsr3: (Fsr3, 1.5),
    );
}

mod a_surface_its_vertex_function_moves_from_rest_at_4_px_a_frame {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
    crate::scenario::temporal::case_tests!(a_surface_its_vertex_function_moves_from_rest_beyond_the_search;
        taa: (Taa, 4.),
        fsr2: (Fsr2, 4.),
        fsr3: (Fsr3, 4.),
    );
}

/// #802's departure at 1.5 px a frame over the sky, its card moved by a
/// morph target (#816). Plausible defect: the motion reach does not arm the
/// hold at a pixel a frame (Frame item 11: "Where that is at least the
/// speed its own closest-motion search covers (a pixel a frame), it arms
/// the recovery hold"), so the band the card leaves in its first frame of
/// motion, beyond the closest-motion search and held by nothing while the
/// card rested, keeps its ghost. The game shader of #802's scenarios also
/// marks the transparency and composition mask wherever it draws, whose
/// hold cleans that band without the reach; a morph target marks nothing.
/// TAA only: the reach is TAA's. The oracle is `departures`'.
#[test]
fn a_surface_its_morph_target_moves_from_rest_beyond_the_search() {
    let Some(gpu) = gpu() else { return };
    let mut failures = Failures::default();
    departures(
        &mut failures,
        (Antialiasing::Taa, &gpu),
        ("over the sky", 1., None),
        (&card(), Mover::Morph, 1.5),
        DEPARTURE_FRAMES,
    );
    failures.assert();
}

/// A strip 2 px wide at `DEPTH` and as tall as `card()`: the thin strip
/// `sgl-post-fx`'s PROVENANCE.md cites for the view-wide rigid signal
/// (`taa_adaptive_resting_thin_occluder_departure_does_not_leave_a_ghost`),
/// whose whole width a move of 2 px a frame or more vacates at once.
fn thin_card() -> Shape {
    Shape::card(Vec2::new(2. * px(), 0.9), 0., CARD)
}

/// A rigid moving instance moved by its pose (M-9, T-21). Plausible defect:
/// with the view-wide signal left to changes without a motion vector, the
/// pixels a moving instance's pose change vacates keep its ghost, because
/// its own motion does not arm them: the closest-motion search below a pixel
/// a frame, the motion reach from a pixel to a tile (32 px) a frame (Frame
/// item 11). The fixtures: `card()` and the thin strip, moving from rest
/// after the hold has expired at speeds the search covers (0.5 px a frame)
/// and the reach covers (1.5 to 16 px a frame), over the sky, a wall at
/// 4 km and a wall at 8 m behind them, and `FAR` times as far over the 4 km
/// wall, where the depth test cannot see the step (case 13). TAA only: the
/// signal is TAA's. The oracle is `departures`'.
fn a_rigid_card_moves_from_rest(shape: fn() -> Shape, rate: f32) {
    let Some(gpu) = gpu() else { return };
    let mut failures = Failures::default();
    let shape = shape();
    for ((distance, scale), over) in [
        (NEAR, Over::Sky),
        (NEAR, Over::FarWall),
        (NEAR, Over::NearWall),
        (DISTANT, Over::FarWall),
    ] {
        let (behind, wall) = over.wall();
        departures(
            &mut failures,
            (Antialiasing::Taa, &gpu),
            (&format!("{distance} {behind}"), scale, wall.as_ref()),
            (&shape, Mover::Pose, rate),
            DEPARTURE_FRAMES,
        );
    }
    failures.assert();
}

mod a_rigid_card_moves_from_rest {
    use super::{card, thin_card};
    crate::scenario::temporal::case_tests!(a_rigid_card_moves_from_rest;
        card_at_1_5_px_a_frame: (card, 1.5),
        thin_at_0_5_px_a_frame: (thin_card, 0.5),
        thin_at_1_5_px_a_frame: (thin_card, 1.5),
        thin_at_4_px_a_frame: (thin_card, 4.),
        thin_at_16_px_a_frame: (thin_card, 16.),
    );
}

/// What appears or departs in case 9, in the order it happens.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Event {
    Hidden,
    Shown,
    Masked,
    Unmasked,
    Removed,
    Added,
}

/// Case 9 (#769 and its siblings). Plausible defect: an appearance or a
/// departure at rest, after the hold has expired, fades over many frames
/// because nothing re-arms rejection. Before the background the card lies
/// 0.2 m before a background 8 m away, a layer on a wall: their normalized
/// depths differ by 0.1 / 7.8 - 0.1 / 8 = 3.2e-4, below the adaptive depth
/// threshold 0.001, so depth disocclusion cannot reject and only the
/// view-wide re-arm can. Against the sky, near and far, nothing else
/// restores rejection either now that depth's magnitude does not (#791).
/// The oracle is analytic with the spec's rule that appearances,
/// departures and visibility changes re-arm the view (Frame item 11): the
/// hold after each event, the pixels whose box grown by 2 px lies wholly
/// inside the card show their new truth (what lies behind, or the card)
/// within the method's `stores`. Each event follows the last by the
/// settle and the hold, so the hold has expired; every event and method
/// runs before the assertion, so one failure reports them all.
fn appearance_and_departure_at_rest_resolve_within_the_hold(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let (back, back_pose) = background();
    let events = [
        Event::Hidden,
        Event::Shown,
        Event::Masked,
        Event::Unmasked,
        Event::Removed,
        Event::Added,
    ];
    let mut failures = Failures::default();
    for (name, scale, behind) in AGAINST {
        let layer = Shape::card(Vec2::new(2.4, 1.6), 0.3, CARD).scaled(scale);
        let layer_pose = farther(Pose::at(Vec3::new(0.6, 0.2, -7.8)), scale);
        let script = move |_| vec![layer_pose];
        let scenario = Scenario {
            shapes: vec![layer.clone()],
            camera: &still,
            script: &script,
        };
        let inside = scenario.inside(0, 0, 2.);
        let gone = match behind {
            Behind::Background => BACKGROUND,
            Behind::Sky => SKY,
        };
        let mut rig = Rig::new(&gpu, VIEW, method, SKY);
        if behind == Behind::Background {
            rig.add(&back, back_pose, 0);
        }
        // Visibility group 1, which the rig's mask enables.
        let mut instance = Some(rig.add(&layer, layer_pose, 1));
        rig.look(still(0));
        let mut frame = 0;
        for event in events {
            let at = frame + SETTLE + HOLD;
            while frame < at {
                rig.render(frame);
                frame += 1;
            }
            match event {
                Event::Hidden => rig.set_visible(instance.unwrap(), false),
                Event::Shown => rig.set_visible(instance.unwrap(), true),
                Event::Masked => rig.views[0].input.visibility_mask &= !1,
                Event::Unmasked => rig.views[0].input.visibility_mask |= 1,
                Event::Removed => rig.remove(instance.take().unwrap()),
                Event::Added => instance = Some(rig.add(&layer, layer_pose, 1)),
            }
            let truth = match event {
                Event::Hidden | Event::Masked | Event::Removed => gone,
                Event::Shown | Event::Unmasked | Event::Added => CARD,
            };
            let mut residuals = Vec::new();
            while frame <= at + HOLD {
                rig.render(frame);
                residuals.push(error_to(&rig.antialiased(), truth, &inside).max);
                frame += 1;
            }
            let residual = *residuals.last().unwrap();
            let epsilon = stores(method) * truth * HALF_STEP;
            let converged = image::frames_to_converge(&residuals, epsilon);
            eprintln!(
                "{method:?} {name}, {event:?}: residual {residual} the hold after it, within {epsilon} from frame {converged:?} after it"
            );
            failures.check(residual <= epsilon, || {
                format!(
                    "{method:?} {name}, {event:?}: the card's pixels are {residual} off their new truth {truth} the hold after the event; residuals by frame {residuals:?}"
                )
            });
        }
        failures.bounded(&rig, format!("{method:?} events {name}"));
    }
    failures.assert();
}
every_method!(appearance_and_departure_at_rest_resolve_within_the_hold);

/// Case 9 for a static instance moved (M-4, M-9). Plausible defect: a
/// `Mobility::Static` instance moved at rest, after the hold has expired,
/// fades over many frames where it left and where it arrived, because it
/// writes no motion and nothing re-arms rejection. The fixtures, truth,
/// oracle and epsilon are case 9's, with the layer a static instance moved
/// 1.2 m sideways (about 15 px) and back: the hold after each move, the
/// pixels whose box grown by 2 px lies wholly inside the layer where it now
/// is show the card, and those grown by 2 px wholly inside where it was and
/// clear of where it is show what lies behind, within the method's
/// `stores`.
fn a_static_instance_moved_at_rest_resolves_within_the_hold(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let (back, back_pose) = background();
    let mut failures = Failures::default();
    for (name, scale, behind) in AGAINST {
        let layer = Shape::card(Vec2::new(2.4, 1.6), 0.3, CARD).scaled(scale);
        let poses = [0.6, -0.6].map(|x| farther(Pose::at(Vec3::new(x, 0.2, -7.8)), scale));
        let gone = match behind {
            Behind::Background => BACKGROUND,
            Behind::Sky => SKY,
        };
        // Where the layer is at each pose, with the 2 px margin inside it
        // and around it.
        let regions = poses.map(|pose| {
            let script = move |_| vec![pose];
            let scenario = Scenario {
                shapes: vec![layer.clone()],
                camera: &still,
                script: &script,
            };
            let met = Region::from_fn(VIEW.size, |pixel| {
                coverage(&scenario.polygons(0)[0][0], pixel, 2.) > 0.
            });
            (scenario.inside(0, 0, 2.), met)
        });
        let mut rig = Rig::new(&gpu, VIEW, method, SKY);
        if behind == Behind::Background {
            rig.add(&back, back_pose, 0);
        }
        let id = rig.add_static(&layer, poses[0], 1);
        rig.look(still(0));
        let mut frame = 0;
        for (to, from) in [(1, 0), (0, 1)] {
            let at = frame + SETTLE + HOLD;
            while frame < at {
                rig.render(frame);
                frame += 1;
            }
            rig.place(id, poses[to]);
            let left = regions[from].0.and(&regions[to].1.not());
            let mut residuals = Vec::new();
            while frame <= at + HOLD {
                rig.render(frame);
                let resolved = rig.antialiased();
                residuals.push([
                    error_to(&resolved, CARD, &regions[to].0).max,
                    error_to(&resolved, gone, &left).max,
                ]);
                frame += 1;
            }
            for (i, (what, truth)) in [("where it arrived", CARD), ("where it left", gone)]
                .into_iter()
                .enumerate()
            {
                let residuals: Vec<f32> = residuals.iter().map(|r| r[i]).collect();
                let residual = *residuals.last().unwrap();
                let epsilon = stores(method) * truth * HALF_STEP;
                let converged = image::frames_to_converge(&residuals, epsilon);
                eprintln!(
                    "{method:?} {name}, a static move {what}: residual {residual} the hold after it, within {epsilon} from frame {converged:?} after it"
                );
                failures.check(residual <= epsilon, || {
                    format!(
                        "{method:?} {name}, a static move: the pixels {what} are {residual} off their new truth {truth} the hold after it; residuals by frame {residuals:?}"
                    )
                });
            }
        }
        failures.bounded(&rig, format!("{method:?} static moves {name}"));
    }
    failures.assert();
}
every_method!(a_static_instance_moved_at_rest_resolves_within_the_hold);

/// What case 9's material edits do, in the order they happen.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Edit {
    /// Its visibility group becomes one the frame's mask lacks.
    Regrouped,
    /// Its visibility group is the mask's again.
    Ungrouped,
    /// Opaque becomes masked at a cutoff above its base alpha.
    CutOut,
    /// The cutoff falls below its base alpha.
    Uncut,
}

/// Case 9's material edits (#798). Plausible defect: a material edit that
/// adds or removes what its users draw, at rest after the hold has
/// expired, fades over many frames because nothing re-arms rejection, as
/// an instance's appearance or departure would without it. The fixtures,
/// truth, oracle and epsilon are case 9's: Frame item 11 re-arms the view
/// for material edits that change what is drawn, so the hold after each
/// edit the pixels whose box grown by 2 px lies wholly inside the card show
/// their new truth within the method's `stores`. The card's base alpha is
/// 0.5, which its opaque mode ignores, so a cutoff of 0.75 cuts out every
/// texel and one of 0.25 none.
fn material_edits_at_rest_resolve_within_the_hold(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    use sgl_3d::{AlphaMode, SurfaceMaterial};
    let (back, back_pose) = background();
    let edits = [Edit::Regrouped, Edit::Ungrouped, Edit::CutOut, Edit::Uncut];
    let mut failures = Failures::default();
    for (name, scale, behind) in AGAINST {
        let layer = Shape::card(Vec2::new(2.4, 1.6), 0.3, CARD).scaled(scale);
        let layer_pose = farther(Pose::at(Vec3::new(0.6, 0.2, -7.8)), scale);
        let script = move |_| vec![layer_pose];
        let scenario = Scenario {
            shapes: vec![layer.clone()],
            camera: &still,
            script: &script,
        };
        let inside = scenario.inside(0, 0, 2.);
        let gone = match behind {
            Behind::Background => BACKGROUND,
            Behind::Sky => SKY,
        };
        let mut rig = Rig::new(&gpu, VIEW, method, SKY);
        if behind == Behind::Background {
            rig.add(&back, back_pose, 0);
        }
        // Visibility group 1, which the rig's mask enables; the mask
        // lacks the top byte's groups.
        let (_, materials) = rig.add_editable(&layer, layer_pose, 1);
        let material = materials[0];
        let mut values = rig.scene.material(material).unwrap();
        values.base[3] = 0.5;
        let set = |rig: &mut Rig, values: SurfaceMaterial| {
            rig.scene
                .set_material(&rig.gpu.queue, material, values)
                .unwrap();
        };
        set(&mut rig, values);
        rig.look(still(0));
        let mut frame = 0;
        for edit in edits {
            let at = frame + SETTLE + HOLD;
            while frame < at {
                rig.render(frame);
                frame += 1;
            }
            match edit {
                Edit::Regrouped => values.visibility_group = 1 << 24,
                Edit::Ungrouped => values.visibility_group = 1,
                Edit::CutOut => values.alpha = AlphaMode::Mask { cutoff: 0.75 },
                Edit::Uncut => values.alpha = AlphaMode::Mask { cutoff: 0.25 },
            }
            set(&mut rig, values);
            let truth = match edit {
                Edit::Regrouped | Edit::CutOut => gone,
                Edit::Ungrouped | Edit::Uncut => CARD,
            };
            let mut residuals = Vec::new();
            while frame <= at + HOLD {
                rig.render(frame);
                residuals.push(error_to(&rig.antialiased(), truth, &inside).max);
                frame += 1;
            }
            let residual = *residuals.last().unwrap();
            let epsilon = stores(method) * truth * HALF_STEP;
            let converged = image::frames_to_converge(&residuals, epsilon);
            eprintln!(
                "{method:?} {name}, {edit:?}: residual {residual} the hold after it, within {epsilon} from frame {converged:?} after it"
            );
            failures.check(residual <= epsilon, || {
                format!(
                    "{method:?} {name}, {edit:?}: the card's pixels are {residual} off their new truth {truth} the hold after the edit; residuals by frame {residuals:?}"
                )
            });
        }
        failures.bounded(&rig, format!("{method:?} material edits {name}"));
    }
    failures.assert();
}
every_method!(material_edits_at_rest_resolve_within_the_hold);

/// A game's shader (`Scene::add_shader`) that dissolves a masked card from
/// its parameters' `cut` or its instance's data (`SurfaceContext::instance`
/// x), whichever is larger: a texel is cut out, its alpha 0 under the
/// card's cutoff of 0.5, where its key is below the cut. The key is 0 left
/// of the world's x = `split` and 1 right of it, so a cut of 0 cuts out
/// nothing, 0.5 the left half and 2 every texel.
const DISSOLVE_SHADER: &str = r#"
struct ShaderParams {
 cut:f32,
 split:f32,
 unused:vec2<f32>,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 return v;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 let key=select(1.,0.,ctx.position.x<params.split);
 out.base_color.a=select(1.,0.,key<max(params.cut,ctx.instance.x));
 return out;
}
"#;

/// What drives `DISSOLVE_SHADER`'s cut.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Dissolve {
    /// `Scene::set_shader_parameters`.
    Parameters,
    /// `Scene::set_instance_shader_data`.
    InstanceData,
}

/// Case 9's dissolves (#807). Plausible defect: a masked card whose game
/// shader cuts its texels out by a parameter or its instance's data, edited
/// at rest after the hold has expired, fades over many frames because
/// nothing re-arms rejection where the texels vanish or return: neither
/// write is a material edit, sets the view-wide signal (Frame item 11) or
/// moves anything. Frame item 11's rule for content that changes without a
/// motion vector of its own covers it: a shaded material's surface marks
/// the transparency-and-composition mask wherever it is drawn, so a texel
/// cut out was marked the frame before and is held at full strength. The
/// fixture is case 9's card before the background, where depth
/// disocclusion cannot reject (against the sky the sky's categorical
/// disocclusion would); the edits cut out every texel, none, then the half
/// left of the card's centre. The oracle is case 9's: the hold after each
/// edit the pixels whose box grown by 2 px lies wholly inside the card,
/// and on one side of the cut, show their new truth (the background or the
/// card) within the method's `stores`. TAA only: the rule is TAA's, while
/// FSR reads the masks by AMD's rules.
fn dissolve_at_rest_resolves_within_the_hold(dissolve: Dissolve) {
    let Some(gpu) = gpu() else { return };
    let (back, back_pose) = background();
    let layer = Shape::card(Vec2::new(2.4, 1.6), 0.3, CARD);
    let layer_pose = Pose::at(Vec3::new(0.6, 0.2, -7.8));
    let split = layer_pose.position.x;
    let script = move |_| vec![layer_pose];
    let scenario = Scenario {
        shapes: vec![layer.clone()],
        camera: &still,
        script: &script,
    };
    let inside = scenario.inside(0, 0, 2.);
    // The split's column: the world's x = `split` at the card's depth,
    // which the still camera sees as a vertical line.
    let (at, _) = VIEW
        .project(&VIEW.camera(still(0)), layer_pose.position)
        .unwrap();
    let left = inside.and(&Region::from_fn(VIEW.size, |p| p[0] as f32 + 3. <= at.x));
    let right = inside.and(&Region::from_fn(VIEW.size, |p| p[0] as f32 - 2. >= at.x));
    assert!(
        !left.is_empty() && !right.is_empty(),
        "the cut halves the card"
    );
    let edits = [
        (2., vec![(&inside, BACKGROUND)]),
        (0., vec![(&inside, CARD)]),
        (0.5, vec![(&left, BACKGROUND), (&right, CARD)]),
    ];
    let mut failures = Failures::default();
    {
        let method = Antialiasing::Taa;
        let mut rig = Rig::new(&gpu, VIEW, method, SKY);
        rig.add(&back, back_pose, 0);
        let shader = rig
            .scene
            .add_shader(ShaderSource {
                wgsl: DISSOLVE_SHADER.into(),
                label: "dissolve".into(),
            })
            .unwrap_or_else(|error| panic!("{error}"));
        let params = |cut: f32| [cut, split, 0., 0.];
        // Visibility group 1, which the rig's mask enables.
        let (id, materials) = rig.add_shaded(
            &layer,
            layer_pose,
            1,
            (shader, 0.),
            bytemuck::bytes_of(&params(0.)),
        );
        let material = materials[0];
        let mut values = rig.scene.material(material).unwrap();
        values.alpha = sgl_3d::AlphaMode::Mask { cutoff: 0.5 };
        rig.scene
            .set_material(&rig.gpu.queue, material, values)
            .unwrap();
        rig.look(still(0));
        let mut frame = 0;
        for (cut, truths) in &edits {
            let at = frame + SETTLE + HOLD;
            while frame < at {
                rig.render(frame);
                frame += 1;
            }
            let queue = &rig.gpu.queue;
            match dissolve {
                Dissolve::Parameters => rig.scene.set_shader_parameters(
                    queue,
                    material,
                    bytemuck::bytes_of(&params(*cut)),
                ),
                Dissolve::InstanceData => {
                    rig.scene
                        .set_instance_shader_data(queue, id, [*cut, 0., 0., 0.])
                }
            }
            .unwrap();
            let mut residuals = vec![Vec::new(); truths.len()];
            while frame <= at + HOLD {
                rig.render(frame);
                let resolved = rig.antialiased();
                for ((region, truth), residuals) in truths.iter().zip(&mut residuals) {
                    residuals.push(error_to(&resolved, *truth, region).max);
                }
                frame += 1;
            }
            for ((_, truth), residuals) in truths.iter().zip(&residuals) {
                let residual = *residuals.last().unwrap();
                let epsilon = stores(method) * truth * HALF_STEP;
                let converged = image::frames_to_converge(residuals, epsilon);
                eprintln!(
                    "{method:?} {dissolve:?}, cut {cut} towards {truth}: residual {residual} the hold after it, within {epsilon} from frame {converged:?} after it"
                );
                failures.check(residual <= epsilon, || {
                    format!(
                        "{method:?} {dissolve:?}, cut {cut}: pixels are {residual} off their new truth {truth} the hold after the edit; residuals by frame {residuals:?}"
                    )
                });
            }
        }
        failures.bounded(&rig, format!("{method:?} {dissolve:?} dissolve"));
    }
    failures.assert();
}

#[test]
fn a_dissolve_by_shader_parameters_at_rest_resolves_within_the_hold() {
    dissolve_at_rest_resolves_within_the_hold(Dissolve::Parameters);
}

#[test]
fn a_dissolve_by_instance_data_at_rest_resolves_within_the_hold() {
    dissolve_at_rest_resolves_within_the_hold(Dissolve::InstanceData);
}

/// How case 10 restarts history.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Restart {
    Cut,
    Resize,
    Scene,
}

/// Case 10. Plausible defect: history survives a camera cut, a resize that
/// changed the targets or a switch to another scene, which the History
/// contract restarts with one reset, so the new view opens with a ghost of
/// the old. The oracle is equivalence: the restarted frame does not depend
/// on what came before, so a run that panned and a run held still
/// elsewhere, given the same restart in the same frame, render it exactly
/// alike (a new renderer's first frame would not do for FSR, whose jitter
/// sequence runs on across resets as AMD's samples run it); and for TAA,
/// whose restart frame is unjittered and passes the current frame through,
/// it is the frame without antialiasing. The restarts follow each other a
/// hold apart, the runs building history in between.
fn a_cut_a_resize_and_another_scene_restart_history(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let restarts = [
        (HOLD, Restart::Cut),
        (2 * HOLD, Restart::Resize),
        (3 * HOLD, Restart::Scene),
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
    let resized = View {
        size: [160, 90],
        ..VIEW
    };
    let mut unantialiased = panned.rig(&gpu, VIEW, Antialiasing::Off);
    let mut failures = Failures::default();
    let mut runs = [&panned, &held].map(|scenario| scenario.rig(&gpu, VIEW, method));
    for frame in 0..=restarts[2].0 {
        let restart = restarts.iter().find(|r| r.0 == frame).map(|r| r.1);
        let mut images = Vec::new();
        for (rig, scenario) in runs.iter_mut().zip([&panned, &held]) {
            match restart {
                Some(Restart::Cut) => rig.views[0].input.camera_cut = true,
                Some(Restart::Resize) => rig.views[0].lens = resized,
                Some(Restart::Scene) => rig.replace_scene(),
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
        eprintln!("{method:?} {restart:?}: {apart:?} between runs with different pasts");
        failures.check(apart.max == 0., || {
            format!(
                "{method:?} {restart:?}: the restarted frame depends on the frames before it, by up to {}",
                apart.max
            )
        });
        if method == Antialiasing::Taa {
            unantialiased.views[0].lens = runs[0].views[0].lens;
            panned.render(&mut unantialiased, frame);
            let off = error(&images[0], &unantialiased.antialiased(), &everywhere);
            failures.check(off.max == 0., || {
                format!(
                    "TAA {restart:?}: the restarted frame differs from the unjittered single sample by up to {}",
                    off.max
                )
            });
        }
    }
    for rig in &runs {
        failures.bounded(rig, format!("{method:?} restarts"));
    }
    failures.assert();
}
every_method!(a_cut_a_resize_and_another_scene_restart_history);

/// How case 10b leaves a frame unfinished.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unfinished {
    /// Its encoder dropped (`Rig::render_unsubmitted`).
    Unsubmitted,
    /// Its commands submitted (`Rig::render_unfinished`).
    Submitted,
}

/// Case 10b (F-3; B-4 of the 2026-11-18 history audit). Plausible defects: a
/// frame rendered and not finished, its encoder dropped or its commands
/// submitted, restarts TAA's history (the History contract restarts
/// nothing for it but FSR), leaves its camera or its accumulation as the
/// next frame's history, or the frame rendered again writes the half of a
/// pair its history is in. The oracle is equivalence: during a pan, the run
/// with an unfinished attempt at frame 6, from a camera 0.3 m above, equals
/// exactly the run without it, in every frame from the one rendered again
/// on.
#[test]
fn an_unfinished_frame_restarts_nothing() {
    let method = Antialiasing::Taa;
    let Some(gpu) = device(method) else { return };
    let pan = VIEW.pan(-0.5, DEPTH);
    let panning = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    let scenario = stage(&panning);
    let attempt = 6;
    let last = attempt + 3;
    let run = |unfinished: Option<Unfinished>| {
        let mut rig = scenario.rig(&gpu, VIEW, method);
        let mut images = Vec::new();
        for frame in 0..=last {
            if frame == attempt
                && let Some(unfinished) = unfinished
            {
                scenario.pose(&mut rig, frame);
                rig.look(Pose::at(panning(frame).position + Vec3::Y * 0.3));
                match unfinished {
                    Unfinished::Unsubmitted => rig.render_unsubmitted(frame),
                    Unfinished::Submitted => rig.render_unfinished(frame),
                }
            }
            scenario.render(&mut rig, frame);
            if frame >= attempt {
                images.push(rig.antialiased());
            }
        }
        (images, rig)
    };
    let (plain, plain_rig) = run(None);
    let mut failures = Failures::default();
    for unfinished in [Unfinished::Unsubmitted, Unfinished::Submitted] {
        let (images, rig) = run(Some(unfinished));
        for (offset, (plain, image)) in plain.iter().zip(&images).enumerate() {
            let everywhere = Region::from_fn(plain.size, |_| true);
            let apart = error(plain, image, &everywhere);
            failures.check(apart.max == 0., || {
                format!(
                    "{unfinished:?}: frame {} differs from the run without the unfinished attempt by up to {}",
                    attempt + offset as u32,
                    apart.max
                )
            });
        }
        failures.bounded(&rig, format!("{unfinished:?} unfinished attempt"));
    }
    failures.bounded(&plain_rig, "without an unfinished attempt");
    failures.assert();
}

/// Case 10c (F-1, F-3: a reset holds until a frame is finished). Plausible
/// defects: a frame rendered with `camera_cut` and not finished, its encoder
/// dropped or its commands submitted, is followed by a frame without the cut
/// that continues history, so the cut the game asked for is never shown: the
/// retry continues from the frame before the cut through DiligentFX's
/// index-gap reset with a jittered, accumulated history (the attempt ran at
/// frame index 0), or, where exactly one frame was finished since the last
/// reset, continues at index 1 from the half the submitted attempt
/// overwrote. The oracle is equivalence: the run with the unfinished cut
/// attempt (from a camera 0.3 m above) and the retry without the cut equals
/// exactly, from the retry on, the run without the attempt whose frame has
/// the cut; at rest and in a pan, and with the attempt at frame 1, the
/// first after the reset frame 0 was finished.
#[test]
fn an_unfinished_cut_holds_until_a_frame_is_finished() {
    let method = Antialiasing::Taa;
    let Some(gpu) = device(method) else { return };
    let pan = VIEW.pan(-0.5, DEPTH);
    let panning = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    let resting = |_: u32| Pose::at(Vec3::ZERO);
    let cases = [
        ("at rest", &resting as &dyn Fn(u32) -> Pose, 4),
        ("in a pan", &panning, 4),
        ("in a pan, one frame after the reset", &panning, 1),
    ];
    let mut failures = Failures::default();
    for (name, camera, attempt) in cases {
        let scenario = stage(camera);
        let last = attempt + 1;
        let run = |unfinished: Option<Unfinished>| {
            let mut rig = scenario.rig(&gpu, VIEW, method);
            let mut images = Vec::new();
            for frame in 0..=last {
                if frame == attempt {
                    if let Some(unfinished) = unfinished {
                        scenario.pose(&mut rig, frame);
                        rig.look(Pose::at(camera(frame).position + Vec3::Y * 0.3));
                        rig.views[0].input.camera_cut = true;
                        match unfinished {
                            Unfinished::Unsubmitted => rig.render_unsubmitted(frame),
                            Unfinished::Submitted => rig.render_unfinished(frame),
                        }
                    }
                    // The run without the attempt cuts here; the retry does
                    // not ask for the cut again.
                    rig.views[0].input.camera_cut = unfinished.is_none();
                }
                scenario.render(&mut rig, frame);
                if frame >= attempt {
                    images.push(rig.antialiased());
                }
            }
            (images, rig)
        };
        let (cut, cut_rig) = run(None);
        for unfinished in [Unfinished::Unsubmitted, Unfinished::Submitted] {
            let (images, rig) = run(Some(unfinished));
            for (offset, (cut, image)) in cut.iter().zip(&images).enumerate() {
                let everywhere = Region::from_fn(cut.size, |_| true);
                let apart = error(cut, image, &everywhere);
                let differing = cut
                    .texels
                    .iter()
                    .zip(&image.texels)
                    .filter(|(a, b)| a != b)
                    .count();
                eprintln!(
                    "{name}, {unfinished:?}: frame {} differs from the cut run in {differing} px, by up to {}",
                    attempt + offset as u32,
                    apart.max
                );
                failures.check(differing == 0, || {
                    format!(
                        "{name}, {unfinished:?}: frame {} differs from the run that cut there by up to {} in {differing} px",
                        attempt + offset as u32,
                        apart.max
                    )
                });
            }
            failures.bounded(&rig, format!("{name}, {unfinished:?} unfinished cut"));
        }
        failures.bounded(&cut_rig, format!("{name}, cut without an attempt"));
    }
    failures.assert();
}

/// Case 11. Plausible defect: moving the render origin during a pan cuts or
/// misplaces history (the History contract translates retained state and
/// restarts nothing). The oracle is equivalence with the run that never
/// moves it, frame by frame. The moves change only the f32 rounding of
/// positions of a few metres (about 1e-6 m, 3e-5 px here), which reaches the
/// motion target as at most its last half-float step at a few pixels (6e-8
/// UV, measured). A resolve is continuous in its motion, so that moves a
/// pixel's value by far less than a step, but each store after the motion is
/// read may then land on the other neighbour: each pixel of the two runs
/// agrees within `stores_after_motion` steps of its value.
fn moving_the_origin_during_a_pan_changes_nothing(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let pan = VIEW.pan(-0.37, DEPTH);
    let camera = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    let scenario = stage(&camera);
    let moves = [(20, Vec3::new(8., 0., -4.)), (30, Vec3::new(-3., 1., 2.))];
    let mut failures = Failures::default();
    let mut plain = scenario.rig(&gpu, VIEW, method);
    let mut moved = scenario.rig(&gpu, VIEW, method);
    // The largest difference relative to the pixel's value, with where.
    let mut largest = (0f32, 0, [0; 2], [0f32; 2]);
    for frame in 0..SETTLE + HOLD {
        for &(at, by) in &moves {
            if frame == at {
                moved.move_origin(by);
            }
        }
        scenario.render(&mut plain, frame);
        scenario.render(&mut moved, frame);
        let [a, b] = [&moved, &plain].map(|rig| rig.antialiased());
        for pixel in pixels(VIEW.size) {
            let values = [a.value(pixel), b.value(pixel)];
            let relative = (values[0] - values[1]).abs() / values[0].max(values[1]);
            if relative > largest.0 {
                largest = (relative, frame, pixel, values);
            }
        }
    }
    let (relative, frame, pixel, values) = largest;
    eprintln!(
        "{method:?} origin moves: largest difference {relative} of the value (frame {frame}, pixel {pixel:?}, {values:?})"
    );
    let bound = stores_after_motion(method) * HALF_STEP;
    failures.check(relative <= bound, || {
        format!(
            "{method:?}: moving the origin changed pixel {pixel:?} at frame {frame} from {} to {}, {relative} of its value, above {bound}",
            values[1], values[0]
        )
    });
    failures.bounded(&plain, format!("{method:?} panning"));
    failures.bounded(&moved, format!("{method:?} panning with origin moves"));
    failures.assert();
}
every_method!(moving_the_origin_during_a_pan_changes_nothing);

/// The motion-adaptive resolve's blend (Frame item 11; `sgl-post-fx`
/// PROVENANCE.md, "Motion-adaptive TAA"): the current frame's weight at
/// full strength, 1 - 0.88, and Filament's floors at rest, 0.3 of that
/// weight and 0.1 of the variance box's rectification.
const FEEDBACK: f32 = 1. - 0.88;
const FEEDBACK_FLOOR: f32 = 0.3;
const BOX_FLOOR: f32 = 0.1;

/// The share of a pixel's error against the current frame, in the blend
/// space v / (1 + v), that one resolve at `strength` (0 at rest, 1 full)
/// leaves where its 3x3 neighbourhood is uniform: the variance box then has
/// no extent, so rectification moves history `BOX_FLOOR + (1 - BOX_FLOOR)
/// strength` of the way to the current sample, and the blend keeps `1 -
/// FEEDBACK (FEEDBACK_FLOOR + (1 - FEEDBACK_FLOOR) strength)` of what
/// remains. Frame item 11 sets both at full strength in proportion to the
/// mask; at rest unmarked, 0.9 x 0.964 = 0.868 survives, the "13% a frame"
/// of #795.
fn survives(strength: f32) -> f32 {
    let rectified = BOX_FLOOR + (1. - BOX_FLOOR) * strength;
    let current = FEEDBACK * (FEEDBACK_FLOOR + (1. - FEEDBACK_FLOOR) * strength);
    (1. - rectified) * (1. - current)
}

fn sdr(v: f32) -> f32 {
    v / (1. + v)
}

/// What marks the temporal masks in cases 14 and 15 (Frame item 9).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Marked {
    /// Additive glow of this grey radiance at alpha 1.
    Glow(f32),
    /// The card drawn blended at this alpha (unlit, `CARD`).
    Blended(f32),
}

impl Marked {
    /// Its radiance over `behind`.
    fn over(self, behind: f32) -> f32 {
        match self {
            Marked::Glow(radiance) => behind + radiance,
            Marked::Blended(alpha) => alpha * CARD + (1. - alpha) * behind,
        }
    }

    /// The larger of the masks item 9 has it write where it covers a pixel,
    /// AMD's values: a glow's reactivity is its largest channel times its
    /// alpha, a blended surface's its own share of the pixel (an unlit
    /// one's alpha), each capped at 0.9, and a blended surface's
    /// transparency and composition its coverage, its alpha. Less an R8
    /// step, since the targets store it in eight bits.
    fn mask(self) -> f32 {
        let mask = match self {
            Marked::Glow(radiance) => radiance.min(0.9),
            Marked::Blended(alpha) => alpha,
        };
        mask - 1. / 255.
    }
}

/// `shape`'s quads at `pose` as glow of grey `radiance`: two triangles each.
fn glow(shape: &Shape, pose: Pose, radiance: f32) -> Vec<sgl_3d::effects::Glow> {
    shape
        .quads_at(pose)
        .iter()
        .flat_map(|corners| {
            [0, 1, 2, 0, 2, 3].map(|i| sgl_3d::effects::Glow {
                position: corners[i].to_array(),
                color: [radiance, radiance, radiance, 1.],
                kind: sgl_3d::effects::GlowKind::Uniform,
                soft_distance: 0.,
            })
        })
        .collect()
}

/// A shape moved by `DEPARTING_SHADER` from `rest` at `rate` pixels a frame
/// along +X from frame `from` on: no pose changes, so nothing sets the
/// scene's rigid signal, as a game's deforming or animated surface sets
/// none. Returns its pose at each frame, which the analytic regions use,
/// and the shader's parameters.
fn departing(rest: Pose, rate: f32, from: u32, scale: f32) -> (impl Fn(u32) -> Pose, [f32; 4]) {
    let velocity = Vec3::X * rate * px() * scale * FRAME_RATE as f32;
    let start = (f64::from(from) / FRAME_RATE) as f32;
    let pose = move |f: u32| {
        let time = (f64::from(f) / FRAME_RATE) as f32;
        Pose::at(rest.position + velocity * (time - start).max(0.))
    };
    (pose, [velocity.x, velocity.y, velocity.z, start])
}

/// Adds `shape` at `rest` to `rig`, drawn through `DEPARTING_SHADER` with
/// `params`, moving at most `frames` frames' worth.
fn add_departing(rig: &mut Rig, shape: &Shape, rest: Pose, params: [f32; 4], frames: u32) {
    let shader = rig
        .scene
        .add_shader(ShaderSource {
            wgsl: DEPARTING_SHADER.into(),
            label: "departing".into(),
        })
        .unwrap_or_else(|error| panic!("{error}"));
    let bound =
        Vec3::new(params[0], params[1], params[2]).length() * frames as f32 / FRAME_RATE as f32;
    rig.add_shaded(shape, rest, 0, (shader, bound), bytemuck::bytes_of(&params));
}

/// Case 14's card speed, in pixels a frame.
const MARKED_RATE: f32 = 1.5;

/// Case 14 (#795). Plausible defect: content that changes with no motion
/// vector of its own (an additive glow, a blended card, which write no
/// depth or motion) is resolved at rest with the relaxed blend, so it
/// arrives dim and leaves a trail tens of frames long. The fixture: with
/// the camera still and the hold expired, a card at `DEPTH` starts moving
/// 1.5 px a frame over the background card's right edge into the sky,
/// moved by its vertex function or, as glow, by the frame's effects, so no
/// rigid signal re-arms the view. Pixels are judged where their box grown
/// by 2 px lies over one radiance behind the card, where every value the
/// resolve blends lies between that radiance and the card over it, so a
/// pixel is never further from either than the step between them. The
/// oracles are analytic, with the spec's blend and hold (Frame item 11):
/// (1) arriving: at a pixel whose box grown by 2 px has lain wholly inside
/// the card for n frames, the 3x3 neighbourhood is uniform and marked, so
/// the resolve is within `survives(mask)^n` of the step, plus a store's
/// rounding; (2) departing, as case 6: a pixel the card `vacated` for the
/// hold shows what lies behind within the method's stores, since a marked
/// pixel arms the whole hold, which gives full strength once it is no
/// longer marked, however dim the content was.
#[test]
fn marked_content_at_rest_responds_and_leaves_no_trail() {
    let Some(gpu) = gpu() else { return };
    let (back, back_pose) = background();
    let card = Shape::card(Vec2::new(1., 0.8), 0.3, CARD);
    // Its right edge near the background's, 3 m to the right at 8 m.
    let rest = Pose::at(Vec3::new(1.5 - 0.6, 0., -DEPTH));
    // It rests until the first hold, which every history starts with, has
    // expired, then moves for two holds.
    let start = HOLD + 1;
    let frames = start + 2 * HOLD;
    let (card_at, params) = departing(rest, MARKED_RATE, start, 1.);
    let script = |f: u32| vec![back_pose, card_at(f)];
    let truth = Scenario {
        shapes: vec![back.clone(), card.clone()],
        camera: &still,
        script: &script,
    };
    // Each frame's pixels over one radiance behind the card, with it, and
    // the pixels wholly inside the card with a margin of 2 px.
    struct Frame {
        behind: [(Region, f32); 2],
        inside: Region,
    }
    let regions: Vec<Frame> = (0..frames)
        .map(|f| {
            let polygons = truth.polygons(f);
            let sky = Region::from_fn(VIEW.size, |p| coverage(&polygons[0][0], p, 2.) == 0.);
            Frame {
                behind: [(truth.inside(f, 0, 2.), BACKGROUND), (sky, SKY)],
                inside: truth.inside(f, 1, 2.),
            }
        })
        .collect();
    let vacated = vacated(&truth, 1, frames, 2.);
    let mut failures = Failures::default();
    for marked in [Marked::Glow(0.5), Marked::Blended(0.5)] {
        let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
        rig.add(&back, back_pose, 0);
        match marked {
            Marked::Glow(radiance) => rig.admit(BACKGROUND + radiance),
            Marked::Blended(alpha) => {
                add_departing(&mut rig, &card.blended(alpha), rest, params, frames)
            }
        }
        rig.look(still(0));
        let mask = marked.mask();
        let pixel_count = (VIEW.size[0] * VIEW.size[1]) as usize;
        // Per pixel, frames inside the card in a row.
        let mut inside = vec![0u32; pixel_count];
        // The largest excess over each bound, with frame, pixel, error and
        // bound; and pixel-frames judged.
        let mut excess = [(f32::NEG_INFINITY, 0, [0; 2], 0f32, 0f32); 2];
        let mut checked = [0usize; 2];
        for frame in 0..frames {
            if let Marked::Glow(radiance) = marked {
                let quads = glow(&card, card_at(frame), radiance);
                rig.scene
                    .update_effects(&rig.gpu.device, &rig.gpu.queue, &quads)
                    .unwrap();
            }
            rig.render(frame);
            let resolved = rig.antialiased();
            let now = &regions[frame as usize];
            for (region, value) in &now.behind {
                let over = marked.over(*value);
                let step = sdr(over) - sdr(*value);
                for pixel in region.iter() {
                    let i = (pixel[1] * VIEW.size[0] + pixel[0]) as usize;
                    let value_now = sdr(resolved.value(pixel));
                    if now.inside.contains(pixel) {
                        inside[i] += 1;
                        let bound = survives(mask).powi(inside[i] as i32) * step
                            + stores(Antialiasing::Taa) * over * HALF_STEP;
                        let off = (value_now - sdr(over)).abs();
                        checked[0] += 1;
                        if off - bound > excess[0].0 {
                            excess[0] = (off - bound, frame, pixel, off, bound);
                        }
                    } else {
                        inside[i] = 0;
                    }
                    // Linear, as case 6 judges what a surface left.
                    let left = frame >= HOLD && vacated[(frame - HOLD) as usize].contains(pixel);
                    if left {
                        let bound = stores(Antialiasing::Taa) * value * HALF_STEP;
                        let off = (resolved.value(pixel) - value).abs();
                        checked[1] += 1;
                        if off - bound > excess[1].0 {
                            excess[1] = (off - bound, frame, pixel, off, bound);
                        }
                    }
                }
            }
        }
        eprintln!(
            "TAA {marked:?} at rest: arriving, {} pixel-frames, largest excess over the bound {} (frame {}, pixel {:?}: off {} against {}); vacated for the hold, {} pixel-frames, largest excess {} (frame {}, pixel {:?}: off {} against {})",
            checked[0],
            excess[0].0,
            excess[0].1,
            excess[0].2,
            excess[0].3,
            excess[0].4,
            checked[1],
            excess[1].0,
            excess[1].1,
            excess[1].2,
            excess[1].3,
            excess[1].4
        );
        assert!(
            checked[0] > 0 && checked[1] > 0,
            "{marked:?}: nothing judged"
        );
        for (which, (excess, frame, pixel, off, bound)) in
            ["arriving", "departing"].into_iter().zip(excess)
        {
            failures.check(excess <= 0., || {
                format!(
                    "TAA {marked:?} at rest, {which}: a pixel is {off} off its truth at frame {frame} pixel {pixel:?}, above the bound {bound} (mask {mask}, #795)"
                )
            });
        }
        failures.bounded(&rig, format!("TAA {marked:?} at rest"));
    }
    failures.assert();
}

/// A tangent-space normal map 64 texels square whose normal leans along U
/// by `slope` times a sine of one period across it, unchanged along V: a
/// repeating wave.
fn wave_normals(slope: f32) -> sgl_3d::asset::Image {
    let image = ::image::RgbaImage::from_fn(64, 64, |x, _| {
        let lean = slope * (std::f32::consts::TAU * (x as f32 + 0.5) / 64.).sin();
        let normal = Vec3::new(lean, 0., 1.).normalize();
        let byte = |c: f32| ((c * 0.5 + 0.5) * 255.).round() as u8;
        ::image::Rgba([byte(normal.x), byte(normal.y), byte(normal.z), 255])
    });
    sgl_3d::asset::Image::Rgba8(image)
}

/// A lit grey plane 10 by 6 m facing +Z, filling the view at `DEPTH`, its
/// U across it, whose normal map scrolls along U: one layer `repeats`
/// times across the plane moving `velocity` UV a second, the other still
/// and of no strength.
fn scrolling_plane(repeats: f32, velocity: f32) -> sgl_3d::asset::Asset {
    use sgl_3d::asset::{Asset, CpuMesh, Material, Vertex};
    let (half_x, half_y) = (5., 3.);
    let vertices = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
        .map(|(x, y): (f32, f32)| Vertex {
            position: [x * half_x, y * half_y, 0.],
            normal: [0., 0., 1.],
            uv: [(x + 1.) / 2., (1. - y) / 2.],
            color: [1.; 4],
            tangent: [1., 0., 0., 1.],
            lightmap_bounds: [0., 0., 1., 1.],
            lightmap_uv: [0.; 2],
        })
        .to_vec();
    Asset {
        meshes: vec![CpuMesh {
            vertices,
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "scrolling plane".into(),
            base: [0.5, 0.5, 0.5, 1.],
            metallic: 0.,
            roughness: 0.6,
            normal_texture: Some(0),
            normal_layers: Some([
                sgl_3d::NormalLayer {
                    velocity: [velocity, 0.],
                    scale: repeats,
                    strength: 1.,
                },
                sgl_3d::NormalLayer {
                    velocity: [0., 0.],
                    scale: repeats,
                    strength: 0.,
                },
            ]),
            ..Material::default()
        }],
        images: vec![wave_normals(0.6)],
        rig: Default::default(),
        ignored: Vec::new(),
    }
}

/// Case 15 (#795). Plausible defect: an opaque surface whose shading moves
/// where its geometry stands still (scrolling normal layers, water) is
/// resolved at rest with the relaxed blend, which averages its moving
/// highlights away once the camera has stopped. The fixture: the plane
/// under one directional light, its wave about 6 px long scrolling 0.3 px a
/// frame, with the camera still from the start. The oracle is the
/// requirement of Frame item 11 that a marked pixel feeds back at full
/// strength: over the plane's interior, the mean temporal standard
/// deviation (in the blend space) under TAA is at least what a filter
/// keeping 0.88 of its last value and taking 0.12 of each new one leaves of
/// the unantialiased frames' at each pixel, the filter run on those frames
/// (a brute-force model of the spec's feedback alone, which full
/// rectification only brings closer to the current frame).
#[test]
fn scrolling_normals_at_rest_keep_their_motion() {
    let Some(gpu) = gpu() else { return };
    // 10 m across 247 px at 4 m: 40 repeats is a wave of 6.2 px, and 0.3 px
    // a frame is 0.3 / 247 of the plane's U a frame, 60 frames a second.
    let plane = || scrolling_plane(40., 0.3 / 247. * FRAME_RATE as f32);
    let frames = SETTLE + 4 * JITTER_CYCLE;
    let measured = SETTLE..frames;
    let series = |antialiasing| {
        let mut rig = Rig::new(&gpu, VIEW, antialiasing, SKY);
        rig.add_model(plane(), Pose::at(Vec3::new(0., 0., -DEPTH)));
        rig.views[0].input.directional_lights[0] = Some(sgl_3d::DirectionalLight {
            direction: Vec3::new(-0.4, -0.3, -1.),
            color: [1.; 3],
            illuminance: 2.,
            shadow: None,
            ..Default::default()
        });
        rig.look(still(0));
        (0..frames)
            .map(|frame| {
                rig.render(frame);
                rig.antialiased()
            })
            .collect::<Vec<Image>>()
    };
    let resolved = series(Antialiasing::Taa);
    let plain = series(Antialiasing::Off);
    let region = Region::from_fn(VIEW.size, |_| true).eroded(4);
    let std_dev = |values: &[f32]| {
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / values.len() as f32).sqrt()
    };
    let (mut taa, mut filtered, mut unantialiased) = (0f64, 0f64, 0f64);
    for pixel in region.iter() {
        let at =
            |frames: &[Image]| -> Vec<f32> { frames.iter().map(|f| sdr(f.value(pixel))).collect() };
        let ours = at(&resolved);
        let theirs = at(&plain);
        let mut last = theirs[0];
        let model: Vec<f32> = theirs
            .iter()
            .map(|&v| {
                last = FEEDBACK * v + (1. - FEEDBACK) * last;
                last
            })
            .collect();
        let window = measured.start as usize..measured.end as usize;
        taa += f64::from(std_dev(&ours[window.clone()]));
        filtered += f64::from(std_dev(&model[window.clone()]));
        unantialiased += f64::from(std_dev(&theirs[window]));
    }
    let n = region.len() as f64;
    let [taa, filtered, unantialiased] = [taa, filtered, unantialiased].map(|v| (v / n) as f32);
    eprintln!(
        "TAA scrolling normals at rest: mean temporal standard deviation {taa}, the full-strength feedback's {filtered}, unantialiased {unantialiased}"
    );
    assert!(
        unantialiased > 0.002,
        "the plane's shading does not move: {unantialiased}"
    );
    assert!(
        taa >= filtered,
        "TAA at rest keeps {taa} of the scrolling normals' temporal deviation, below the {filtered} full-strength feedback keeps of the unantialiased {unantialiased} (#795)"
    );
}

/// Case 4 beside marked content (#795). Plausible defect: the mask bleeds
/// past the pixels it marks, so static outlines beside an effect lose their
/// resting stability. The fixture: the stage, with a glow card moving 1.5
/// px a frame along the view below it. The region: case 4's silhouettes,
/// less every pixel within 2 px of the glow at any frame.
#[test]
fn at_rest_taa_holds_silhouettes_beside_marked_content_still() {
    let Some(gpu) = gpu() else { return };
    let scenario = far_stage(1., None, &stage_poses);
    let glow_card = Shape::card(Vec2::new(0.6, 0.4), 0., 0.);
    let start = Pose::at(Vec3::new(-2.5, -1.45, -DEPTH));
    let across = Motion::translate(Vec3::X * MARKED_RATE * px());
    let near_glow = Region::from_fn(VIEW.size, |pixel| {
        (0..SETTLE + 2 * JITTER_CYCLE).any(|f| {
            let quad = &glow_card.quads_at(across.pose(start, f))[0];
            let projected = VIEW.project_polygon(&VIEW.camera(still(f)), quad).unwrap();
            coverage(&projected, pixel, 2.) > 0.
        })
    });
    let near_outline = scenario
        .inside(0, 0, 2.)
        .or(&scenario.inside(0, 1, 2.))
        .not();
    let region = scenario
        .edges(0, &[0, 1])
        .and(&near_outline)
        .and(&near_glow.not());
    let mut failures = Failures::default();
    let elsewhere = |rig: &mut Rig, frame: u32| {
        if frame == 0 {
            rig.admit(SKY + 0.5);
        }
        let quads = glow(&glow_card, across.pose(start, frame), 0.5);
        rig.scene
            .update_effects(&rig.gpu.device, &rig.gpu.queue, &quads)
            .unwrap();
    };
    taa_holds_still_at_rest(
        &mut failures,
        &gpu,
        "silhouettes beside a moving glow",
        &scenario,
        &region,
        &elsewhere,
    );
    failures.assert();
}

/// The motion reach's tile (Frame item 11: "a tile (32 pixels)").
const REACH_TILE: u32 = 32;

/// Case 4 beyond the motion reach (#802). Plausible defect: the reach
/// raises rejection beyond the tile neighbourhood of what moves, so
/// anything moving anywhere costs the whole view its resting stability.
/// The fixture: the stage at rest, with a small card its vertex function
/// moves 1.5 px a frame down the view's left edge, inside the first
/// column of tiles. The region: case 4's silhouettes in tile columns two
/// or more from it, beyond the 3x3 tile neighbourhood of every tile the
/// moving card touches.
#[test]
fn at_rest_taa_holds_silhouettes_beyond_the_reach_of_motion_still() {
    let Some(gpu) = gpu() else { return };
    let scenario = far_stage(1., None, &stage_poses);
    let mover = Shape::card(Vec2::new(0.5, 0.5), 0., CARD);
    let rest = Pose::at(Vec3::new(-3.5, 1.4, -DEPTH));
    let frames = SETTLE + 2 * JITTER_CYCLE;
    let velocity = -Vec3::Y * MARKED_RATE * px() * FRAME_RATE as f32;
    let params = [velocity.x, velocity.y, velocity.z, 0.];
    let mover_at = |f: u32| {
        let time = (f64::from(f) / FRAME_RATE) as f32;
        Pose::at(rest.position + velocity * time)
    };
    // The tile columns the moving card's pixels lie in, at any frame.
    let mut columns = std::collections::BTreeSet::new();
    for f in 0..frames {
        let quad = &mover.quads_at(mover_at(f))[0];
        let projected = VIEW.project_polygon(&VIEW.camera(still(f)), quad).unwrap();
        for pixel in pixels(VIEW.size) {
            if coverage(&projected, pixel, 1.) > 0. {
                columns.insert(pixel[0] / REACH_TILE);
            }
        }
    }
    assert_eq!(
        columns.iter().copied().collect::<Vec<_>>(),
        [0],
        "the mover leaves the first tile column"
    );
    let beyond = Region::from_fn(VIEW.size, |pixel| pixel[0] / REACH_TILE >= 2);
    let near_outline = scenario
        .inside(0, 0, 2.)
        .or(&scenario.inside(0, 1, 2.))
        .not();
    let region = scenario.edges(0, &[0, 1]).and(&near_outline).and(&beyond);
    let mut failures = Failures::default();
    let elsewhere = |rig: &mut Rig, frame: u32| {
        if frame == 0 {
            add_departing(rig, &mover, rest, params, frames);
        }
    };
    taa_holds_still_at_rest(
        &mut failures,
        &gpu,
        "silhouettes beyond the reach of a moving card",
        &scenario,
        &region,
        &elsewhere,
    );
    failures.assert();
}

/// Case 4 within the reach of slow deformation (#795). Plausible defect: a
/// surface deforming slower than a pixel a frame, which the resolve's own
/// 3x3 closest-motion search already follows (#802 measured no ghost at 0.5
/// px a frame), arms the hold across its whole tile neighbourhood, costing
/// static outlines up to 63 pixels away their resting stability. The
/// fixture: the stage at rest, with a small card its vertex function moves
/// 0.5 px a frame across the view below it. The region: case 4's
/// silhouettes in the 3x3 tile neighbourhood of a tile the moving card
/// touches, less every pixel within 2 px of it at any frame. The oracle is
/// case 4's bound, under Frame item 11's rule that slower motion is the
/// search's.
#[test]
fn at_rest_taa_holds_silhouettes_within_the_reach_of_slow_motion_still() {
    let Some(gpu) = gpu() else { return };
    let scenario = far_stage(1., None, &stage_poses);
    let mover = Shape::card(Vec2::new(0.4, 0.4), 0., CARD);
    let rest = Pose::at(Vec3::new(-0.9, -1.2, -DEPTH));
    let frames = SETTLE + 2 * JITTER_CYCLE;
    let velocity = Vec3::X * 0.5 * px() * FRAME_RATE as f32;
    let params = [velocity.x, velocity.y, velocity.z, 0.];
    let mover_at = |f: u32| {
        let time = (f64::from(f) / FRAME_RATE) as f32;
        Pose::at(rest.position + velocity * time)
    };
    // The tiles the moving card touches, and the pixels within 2 px of it,
    // at any frame.
    let mut tiles = std::collections::BTreeSet::new();
    let mut near = Region::from_fn(VIEW.size, |_| false);
    for f in 0..frames {
        let quad = &mover.quads_at(mover_at(f))[0];
        let projected = VIEW.project_polygon(&VIEW.camera(still(f)), quad).unwrap();
        near = near.or(&Region::from_fn(VIEW.size, |pixel| {
            coverage(&projected, pixel, 2.) > 0.
        }));
        for pixel in pixels(VIEW.size) {
            if coverage(&projected, pixel, 0.) > 0. {
                tiles.insert([pixel[0] / REACH_TILE, pixel[1] / REACH_TILE]);
            }
        }
    }
    let within = Region::from_fn(VIEW.size, |pixel| {
        let tile = pixel.map(|v| (v / REACH_TILE) as i64);
        tiles
            .iter()
            .any(|&[x, y]| (tile[0] - x as i64).abs() <= 1 && (tile[1] - y as i64).abs() <= 1)
    });
    let near_outline = scenario
        .inside(0, 0, 2.)
        .or(&scenario.inside(0, 1, 2.))
        .not();
    let region = scenario
        .edges(0, &[0, 1])
        .and(&near_outline)
        .and(&within)
        .and(&near.not());
    let mut failures = Failures::default();
    let elsewhere = |rig: &mut Rig, frame: u32| {
        if frame == 0 {
            add_departing(rig, &mover, rest, params, frames);
        }
    };
    taa_holds_still_at_rest(
        &mut failures,
        &gpu,
        "silhouettes within the reach of slow motion",
        &scenario,
        &region,
        &elsewhere,
    );
    failures.assert();
}

/// Where case 4's rigid mover moves (M-9, T-21).
#[derive(Clone, Copy, Debug)]
enum RigidMover {
    /// Down the view's left edge, inside the first column of tiles.
    InView,
    /// Behind the camera, outside the view.
    Behind,
}

/// Case 4 beside a rigid mover (M-9, T-21). Plausible defect: a moving
/// instance's pose change, whose motion vectors already describe it, raises
/// the view-wide signal, so anything moving anywhere, on screen or not,
/// costs the whole view its resting stability (B-1 of the 2026-11-18
/// history audit). The fixture: the stage at rest, with a small card, a
/// moving instance, posed every frame at 1.5 px a frame down the view's left
/// edge inside the first column of tiles, as
/// `at_rest_taa_holds_silhouettes_beyond_the_reach_of_motion_still`'s vertex
/// function moves it; or moving 0.1 m a frame 3 m behind the camera. The
/// region: case 4's silhouettes in tile columns two or more from the mover
/// in view, beyond the 3x3 tile neighbourhood of every tile it touches, and
/// every silhouette with the mover behind. The oracle is case 4's bound.
fn at_rest_taa_holds_silhouettes_beside_a_rigid_mover_still(mover_at: RigidMover) {
    let Some(gpu) = gpu() else { return };
    let scenario = far_stage(1., None, &stage_poses);
    let mover = Shape::card(Vec2::new(0.5, 0.5), 0., CARD);
    let frames = SETTLE + 2 * JITTER_CYCLE;
    let (rest, velocity) = match mover_at {
        RigidMover::InView => (
            Pose::at(Vec3::new(-3.5, 1.4, -DEPTH)),
            -Vec3::Y * MARKED_RATE * px(),
        ),
        RigidMover::Behind => (Pose::at(Vec3::new(0., 0., 3.)), Vec3::X * 0.1),
    };
    let pose = move |f: u32| Pose::at(rest.position + velocity * f as f32);
    let near_outline = scenario
        .inside(0, 0, 2.)
        .or(&scenario.inside(0, 1, 2.))
        .not();
    let mut region = scenario.edges(0, &[0, 1]).and(&near_outline);
    if let RigidMover::InView = mover_at {
        // The tile columns the moving card's pixels lie in, at any frame.
        let mut columns = std::collections::BTreeSet::new();
        for f in 0..frames {
            let quad = &mover.quads_at(pose(f))[0];
            let projected = VIEW.project_polygon(&VIEW.camera(still(f)), quad).unwrap();
            for pixel in pixels(VIEW.size) {
                if coverage(&projected, pixel, 1.) > 0. {
                    columns.insert(pixel[0] / REACH_TILE);
                }
            }
        }
        assert_eq!(
            columns.iter().copied().collect::<Vec<_>>(),
            [0],
            "the mover leaves the first tile column"
        );
        region = region.and(&Region::from_fn(VIEW.size, |pixel| {
            pixel[0] / REACH_TILE >= 2
        }));
    }
    let mut failures = Failures::default();
    let elsewhere = |rig: &mut Rig, frame: u32| {
        if frame == 0 {
            rig.add(&mover, pose(0), 0);
        } else {
            let id = *rig.instances().last().unwrap();
            rig.place(id, pose(frame));
        }
    };
    taa_holds_still_at_rest(
        &mut failures,
        &gpu,
        &format!("silhouettes beside a rigid mover {mover_at:?}"),
        &scenario,
        &region,
        &elsewhere,
    );
    failures.assert();
}

mod at_rest_taa_holds_silhouettes_beside_a_rigid_mover_still {
    use super::RigidMover::{Behind, InView};
    crate::scenario::temporal::case_tests!(at_rest_taa_holds_silhouettes_beside_a_rigid_mover_still;
        in_view: (InView),
        behind_the_camera: (Behind),
    );
}

/// The rounding steps by which the camera's eye position is off its value
/// at each frame of a jitter cycle's half: one to three steps either way,
/// one or two steps from the last frame's.
const ROUNDING: [i32; 8] = [0, 1, 3, 2, 0, -1, -3, -2];

/// `value` `steps` f32 rounding steps up (or down, if negative).
fn stepped(mut value: f32, steps: i32) -> f32 {
    for _ in 0..steps.unsigned_abs() {
        value = if steps > 0 {
            value.next_up()
        } else {
            value.next_down()
        };
    }
    value
}

/// The yaw of the turned cameras of the far-from-origin case.
const TURNED: f32 = 0.7;

/// `distance` metres along the turned camera's forward axis (-Z turned by
/// `TURNED`) when `forward`, else along its right axis (+X turned), and
/// `up` metres up: an eye at which the view translation element of the
/// other axis, `-dot(axis, eye)`, cancels to near zero.
fn turned_eye(forward: bool, distance: f32, up: f32) -> Vec3 {
    let axis = if forward { -Vec3::Z } else { Vec3::X };
    sgl_3d::glam::Mat4::from_rotation_y(TURNED).transform_vector3(axis) * distance + Vec3::Y * up
}

/// Case 4 with a camera at rest away from the render origin whose view a
/// game rebuilds each frame from an eye position carrying rounding (T-20).
/// Plausible defect: the camera test takes rounding for motion (an
/// absolute threshold of 1e-6 is one rounding step at a magnitude of 16; a
/// tolerance relative to each element's own magnitude rejects a view
/// translation element that cancels to near zero, whose noise is steps of
/// the eye's magnitude), so a camera at rest tens of metres or more from the
/// render origin keeps the whole view at full strength (B-2 of the
/// 2026-11-18 history audit). The fixture: the stage before the sky, carried
/// by a camera at `eye` turned by `yaw`, whose eye position each frame is
/// one to three rounding steps off in each component (`ROUNDING`,
/// alternating in sign between components) and whose view the rig rebuilds
/// from it (`Rig::look`). Near the stage is 4 m ahead; at kilometres it is
/// `FAR` times as far, because there three steps of the eye (1.5 mm) are
/// 0.04 px at 4 m, a real motion above the still-pixel threshold that arms
/// its pixels as it should, and 0.0001 px at 2 km. The region and oracle
/// are case 4's at the outlines, the bound (case 12) included.
///
/// At 100 m the case also guards the still-pixel rule's read position
/// (DFX-19, B-12 of the audit): the eye's rounding moves the stage by about
/// 1e-4 px a frame, under the stillness threshold. Reprojecting history by
/// that put the Catmull-Rom fetch a fraction of a texel off, whose small
/// over- and undershoot the relaxed rectification at rest let accumulate
/// past the bound; a still pixel reads its own texel instead.
fn at_rest_taa_holds_silhouettes_still_far_from_the_origin(
    (distance, scale): (&str, f32),
    (eye, yaw): (Vec3, f32),
) {
    let Some(gpu) = gpu() else { return };
    let rest = Pose { position: eye, yaw };
    let script = move |f: u32| {
        stage_poses_at(scale, None)(f)
            .into_iter()
            .map(|p| rest.then(p))
            .collect()
    };
    let camera = move |_: u32| rest;
    let scenario = Scenario {
        shapes: stage_shapes(scale),
        camera: &camera,
        script: &script,
    };
    let near_outline = scenario
        .inside(0, 0, 2.)
        .or(&scenario.inside(0, 1, 2.))
        .not();
    let region = scenario.edges(0, &[0, 1]).and(&near_outline);
    let elsewhere = |rig: &mut Rig, frame: u32| {
        let steps = ROUNDING[frame as usize % ROUNDING.len()];
        let noisy = Vec3::new(
            stepped(eye.x, steps),
            stepped(eye.y, -steps),
            stepped(eye.z, steps),
        );
        rig.look(Pose {
            position: noisy,
            yaw,
        });
    };
    let mut failures = Failures::default();
    taa_holds_still_at_rest(
        &mut failures,
        &gpu,
        &format!("{distance} silhouettes seen from {eye} m from the origin turned {yaw}"),
        &scenario,
        &region,
        &elsewhere,
    );
    failures.assert();
}

mod at_rest_taa_holds_silhouettes_still_far_from_the_origin {
    use super::{DISTANT, NEAR, TURNED, turned_eye};
    use sgl_3d::glam::Vec3;
    crate::scenario::temporal::case_tests!(at_rest_taa_holds_silhouettes_still_far_from_the_origin;
        at_100_m: (NEAR, (Vec3::new(80., 40., -50.), 0.)),
        at_100_m_along_the_forward_axis: (NEAR, (turned_eye(true, 100., 20.), TURNED)),
        at_5_km_along_the_right_axis: (DISTANT, (turned_eye(false, 5000., 1500.), TURNED)),
    );
}

/// The frame #816's scenarios act at: the first hold, which every history
/// starts with, has expired.
const ACTS: u32 = HOLD + 1;

/// #816's sky: nothing but the sky, so every pixel's motion is zero
/// whatever the camera's translation (the sky moves by its rotation alone,
/// case 1), its depth is the far plane's and its 3x3 neighbourhood is
/// uniform. The camera rests, steps 0.5 m sideways at frame `ACTS` and
/// rests again; `after` frames later the sky's radiance becomes `CARD`, a
/// change with no motion, depth, rigid signal or mask of its own, which
/// only the camera test and the hold it arms reject. Returns, at the
/// frame of the change, the largest error over the view against `CARD` in
/// TAA's blend space v / (1 + v), the change's step in that space, and a
/// store's rounding at `CARD` for each of TAA's `stores`.
fn the_sky_changes_after_a_camera_step(gpu: &Gpu, after: u32) -> (f32, f32, f32) {
    let mut rig = Rig::new(gpu, VIEW, Antialiasing::Taa, SKY);
    rig.admit(CARD);
    let changed = ACTS + after;
    for frame in 0..=changed {
        let x = if frame < ACTS { 0. } else { 0.5 };
        rig.look(Pose::at(Vec3::new(x, 0., 0.)));
        if frame == changed {
            rig.views[0].input.backdrop = sgl_3d::Backdrop::Color([CARD; 3]);
        }
        rig.render(frame);
    }
    let resolved = rig.antialiased();
    let error = pixels(VIEW.size)
        .map(|pixel| (sdr(resolved.value(pixel)) - sdr(CARD)).abs())
        .fold(0f32, f32::max);
    let mut failures = Failures::default();
    failures.bounded(&rig, "TAA's sky after a camera step");
    failures.assert();
    (
        error,
        sdr(CARD) - sdr(SKY),
        stores(Antialiasing::Taa) * CARD * HALF_STEP,
    )
}

/// #816. Plausible defect: the camera test misses a translation, or arms
/// only pixels whose own motion is non-zero, so content that changes where
/// nothing moves on screen while the camera moves (here the sky) is
/// resolved at the resting blend. The oracle is the requirement of Frame
/// item 11, "Camera motion rearms the whole view", with the resolve's blend
/// at full strength: in the frame the camera steps, the sky changes; its
/// neighbourhood is uniform, so the error left is `survives(1)` = 0 of the
/// step, plus the stores' rounding. At the resting blend `survives(0)`,
/// 0.868 of it, would be left.
#[test]
fn a_camera_step_rearms_the_whole_view() {
    let Some(gpu) = gpu() else { return };
    let (error, step, rounding) = the_sky_changes_after_a_camera_step(&gpu, 0);
    let bound = survives(1.) * step + rounding;
    eprintln!(
        "TAA, the sky changing as the camera steps: error {error} of a step {step}, against {bound}"
    );
    assert!(
        error <= bound,
        "TAA left {error} of the sky's step {step} in the frame the camera stepped, above {bound}: the camera test did not re-arm the whole view"
    );
}

/// #816. Plausible defect: the recovery hold is not carried from frame to
/// frame (it lasts the frame that armed it), so a change soon after a
/// re-arming event with nothing re-arming it since, here half a hold after
/// the camera stopped, is resolved at the resting blend. The oracle is the
/// spec's hold (Frame item 11, "a twelve-frame recovery hold"; `HOLD`),
/// which ramps from full strength down over its frames (`sgl-post-fx`
/// PROVENANCE.md, "Motion-adaptive TAA", Responsive inputs: "where the
/// other holds ramp"): `HOLD / 2` frames after the step the hold left is
/// half of it, strength 0.5, and the sky's uniform neighbourhood leaves
/// `survives(0.5)` = 0.415 of the step, plus the stores' rounding. A hold
/// of a frame leaves `survives(0)`, 0.868 of it, and any hold that ends by
/// then leaves more than 0.415.
#[test]
fn the_hold_lasts_its_frames_after_a_camera_step() {
    let Some(gpu) = gpu() else { return };
    let after = HOLD / 2;
    let (error, step, rounding) = the_sky_changes_after_a_camera_step(&gpu, after);
    let strength = (HOLD - after) as f32 / HOLD as f32;
    let bound = survives(strength) * step + rounding;
    eprintln!(
        "TAA, the sky changing {after} frames after the camera stepped: error {error} of a step {step}, against {bound}"
    );
    assert!(
        error <= bound,
        "TAA left {error} of the sky's step {step} {after} frames after the camera stepped, above {bound}: the hold did not last"
    );
}

/// #816's jumps: a card 0.5 m square at `DEPTH`, `scale` times as far,
/// before `wall` or the sky, that a morph target moves 5.8 m (130 px)
/// across the view in one frame at `ACTS`, from rest: no pose change, no
/// shader and no mask, nor any motion where it was, since what it
/// uncovered is still and it rested before. It leaves the view's first
/// column of the motion reach's tiles for its fifth and sixth, so the reach
/// (the 3x3 tile neighbourhood) does not cover where it was, and the
/// closest-motion search does not reach it. Returns, at that frame, the
/// largest linear error against what lies behind over the pixels whose box
/// grown by 2 px lay wholly inside the card before (the history fetch's
/// reach and the depth test's neighbourhood with its jitter alignment),
/// and the bound of the method's `stores` at that radiance.
fn a_deformation_jumps_beyond_the_reach(
    gpu: &Gpu,
    scale: f32,
    wall: Option<(Shape, Pose)>,
) -> (f32, f32) {
    let shape = Shape::card(Vec2::new(0.5, 0.5), 0., CARD).scaled(scale);
    let rest = farther(Pose::at(Vec3::new(-3.2, 0.3, -DEPTH)), scale);
    let by = Vec3::X * 5.8 * scale;
    let script = move |f: u32| {
        let at = if f < ACTS { 0. } else { 1. };
        vec![Pose::at(rest.position + by * at)]
    };
    let truth = Scenario {
        shapes: vec![shape.clone()],
        camera: &still,
        script: &script,
    };
    let columns = |frame: u32| {
        let polygon = &truth.polygons(frame)[0][0];
        pixels(VIEW.size)
            .filter(|&p| coverage(polygon, p, 1.) > 0.)
            .map(|p| p[0] / REACH_TILE)
            .collect::<std::collections::BTreeSet<u32>>()
    };
    let (before, after) = (columns(ACTS - 1), columns(ACTS));
    assert!(
        after.first().unwrap() >= &(before.last().unwrap() + 2),
        "the card jumps beyond the reach's tile neighbourhood: tile columns {before:?} to {after:?}"
    );
    let left = truth.inside(ACTS - 1, 0, 2.);
    assert!(!left.is_empty(), "the card covers pixels with their margin");
    let value = if wall.is_some() { BACKGROUND } else { SKY };
    let mut rig = Rig::new(gpu, VIEW, Antialiasing::Taa, SKY);
    if let Some((wall, pose)) = &wall {
        rig.add(wall, *pose, 0);
    }
    let id = rig.add_morphing(&shape, rest, by);
    rig.look(still(0));
    for frame in 0..=ACTS {
        if frame == ACTS {
            rig.scene
                .set_instance_deformation(&rig.gpu.queue, id, &[], &[1.])
                .unwrap();
        }
        rig.render(frame);
    }
    let error = error_to(&rig.antialiased(), value, &left).max;
    let mut failures = Failures::default();
    failures.bounded(&rig, "TAA, a deformation's jump");
    failures.assert();
    (error, stores(Antialiasing::Taa) * value * HALF_STEP)
}

/// #816. Plausible defect: the depth test does not reject history whose
/// depth differs from the surface the pixel now shows, so a surface that
/// leaves a pixel faster than the motion reach covers (a tile, 32 px, a
/// frame), from rest, leaves its ghost there: nothing else restores
/// rejection (`a_deformation_jumps_beyond_the_reach`). The card at 4 m and
/// the wall at 8 m behind it differ by 0.1 / 4 - 0.1 / 8 = 0.0125 in
/// normalized depth, above the depth threshold 0.001 (`sgl-post-fx`
/// PROVENANCE.md, "Motion-adaptive TAA"), and their previous 3x3 depth
/// neighbourhood held the card alone. The oracle is analytic with Frame
/// item 11's full rejection on depth disocclusion: in the frame the card
/// leaves, the pixels it covered show the wall within TAA's `stores`.
#[test]
fn depth_disocclusion_rejects_beyond_the_reach() {
    let Some(gpu) = gpu() else { return };
    let (error, bound) = a_deformation_jumps_beyond_the_reach(&gpu, 1., Some(wall(8.)));
    eprintln!("TAA, a card's jump uncovering a wall: error {error} against {bound}");
    assert!(
        error <= bound,
        "TAA left {error} of the card where it uncovered the wall, above {bound}: depth disocclusion did not reject"
    );
}

/// #816. Plausible defect: the sky is not a categorical disocclusion where
/// a surface leaves it (Frame item 11: sky "where no texel of the previous
/// neighbourhood was"), so a far surface that leaves a pixel faster than
/// the motion reach covers, from rest, leaves its ghost on the sky: at
/// `FAR` times as far, 2 km, the card's normalized depth (4e-5) differs
/// from the sky's (0) by less than the depth threshold 0.001, and nothing
/// else restores rejection (`a_deformation_jumps_beyond_the_reach`). The
/// oracle is analytic with Frame item 11's categorical sky: in the frame
/// the card leaves, the pixels it covered show the sky within TAA's
/// `stores`.
#[test]
fn the_sky_a_far_surface_leaves_is_a_disocclusion() {
    let Some(gpu) = gpu() else { return };
    let (error, bound) = a_deformation_jumps_beyond_the_reach(&gpu, FAR, None);
    eprintln!("TAA, a far card's jump uncovering the sky: error {error} against {bound}");
    assert!(
        error <= bound,
        "TAA left {error} of the far card where it uncovered the sky, above {bound}: the sky was not a disocclusion"
    );
}
