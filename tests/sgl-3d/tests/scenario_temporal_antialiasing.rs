//! The temporal antialiasing suite of the scenario harness
//! (`specs/testing.md`, "SGL3D scenario harness"): TAA, and FSR2 and FSR3
//! where the device runs them, judged against analytic truth, a supersampled
//! reference, equivalent scripts, bounds and the numbers the spec states.
//! Case 12, the bound, is checked over every antialiased frame each case
//! read, after the case's own assertions (`Failures::bounded`). Cases 3, 4
//! and 7 dump their frames and metrics with `SGL_SCENARIO_DUMP`.
//!
//! The fixtures' truth assumes `rig::fixture_settings`: unlit cards, no
//! bloom, fog, ambient occlusion, reflections or motion blur, and a fixed
//! exposure, so what antialiasing receives is exactly their radiance.
//!
//! Not covered yet: the resolve under camera rotation (case 1 checks its
//! motion only), FSR at its upscaling qualities (it runs at Native AA, where
//! the render and scene sizes agree), and FSR with sharpening on (RCAS moves
//! edges away from their coverage on purpose, so the reference does not
//! measure it). Each needs its own fixture or oracle.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose, View, coverage};
use scenario::image::{self, Image, Region, centre, error, error_to, pixels};
use scenario::rig::{Dump, Gpu, HALF_STEP, Rig, Shape, fsr_gpu, gpu};
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;

/// 192 by 108 at a one-radian vertical field: a fixture 4 m ahead spans
/// tens of pixels, and the 8 times reference stays small.
const VIEW: View = View {
    size: [192, 108],
    fov_y: 1.,
    near: 0.1,
};
/// The fixtures' distance ahead of the camera.
const DEPTH: f32 = 4.;
/// The fixtures' radiances: a dark sky and background beneath mid-grey
/// cards, values a lit scene at exposure 0 puts on screen below white,
/// where the tone map's mid-tones are, with contrasts far above half-float
/// rounding.
const SKY: f32 = 0.05;
const BACKGROUND: f32 = 0.1;
const CARD: f32 = 0.5;
const GRATING: f32 = 0.1;
const STRIPS: f32 = 0.5;
/// TAA's jitter cycle: the frame's "16-sample Halton jitter" (sgl-3d's
/// README, TAA), DiligentFX's Halton (2, 3) sequence (`sgl-post-fx`'s
/// `TemporalAntiAliasing::get_jitter_offset`), in which every jitter
/// position contributes once a cycle.
const JITTER_CYCLE: u32 = 16;
/// The recovery hold of `specs/sgl3d-architecture.md` Frame item 10 ("a
/// twelve-frame recovery hold"): after a reset, motion or a re-arming
/// change, rejection is restored for that long.
const HOLD: u32 = 12;
/// Frames after a history restart until a resolve at rest has had its hold
/// expire and then a whole jitter cycle.
const SETTLE: u32 = HOLD + JITTER_CYCLE;
/// The reference oracle's supersampling: 8 by 8 samples a pixel, so its
/// own estimate of a tilted sub-pixel strip's coverage errs well below the
/// differences the comparisons resolve.
const SUPERSAMPLE: u32 = 8;
/// A fifth of TAA's still-pixel classification (`TAA_STILL_MOTION_PIXELS`,
/// 0.01 px, DFX-19 in `sgl-post-fx`'s PROVENANCE.md) at this view's width,
/// in UV: motion the resolve could mistake for movement exceeds it.
const STILL_UV: f32 = 0.01 / 5. / VIEW.size[0] as f32;
/// Depth motion beside its half-float step (`HALF_STEP` of the value): the
/// f32 rounding of two view depths of a few metres differenced, far below
/// the slowest scripted depth motion, 0.05 m a frame.
const STILL_DEPTH: f32 = 1e-4;

/// The temporal methods this device runs, each on a device that has its
/// features; FSR is skipped, saying so, where the adapter lacks them.
fn methods() -> Vec<(Antialiasing, Gpu)> {
    let mut out = Vec::new();
    if let Some(gpu) = gpu() {
        out.push((Antialiasing::Taa, gpu));
    }
    match fsr_gpu() {
        Some(gpu) => {
            out.push((Antialiasing::Fsr2, gpu.clone()));
            out.push((Antialiasing::Fsr3, gpu));
        }
        None => eprintln!("FSR2 and FSR3 scenarios skipped on this device"),
    }
    out
}

/// The jitter phases `method` cycles through at `rig`'s sizes: TAA's
/// `JITTER_CYCLE`, and FSR's own count at its render and display widths, as
/// the AMD SDK port the renderer uses gives it (`8 * (display / render)^2`,
/// 8 at Native AA).
fn phases(method: Antialiasing, rig: &Rig) -> u32 {
    let render = rig.renderer.render_size()[0] as i32;
    let display = rig.renderer.scene_size()[0] as i32;
    let count = match method {
        Antialiasing::Taa => return JITTER_CYCLE,
        Antialiasing::Fsr2 => sp_fidelity::fsr2::ffx_fsr2_get_jitter_phase_count(render, display),
        Antialiasing::Fsr3 => {
            sp_fidelity::fsr3upscaler::ffx_fsr3_upscaler_get_jitter_phase_count(render, display)
        }
        other => panic!("{other:?} does not jitter"),
    };
    u32::try_from(count).expect("a positive phase count")
}

/// The kind of edge pixel a resolve is judged on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Edge {
    /// A pixel crossed by sub-pixel strips.
    Strips,
    /// A pixel crossed by the outline of a surface wider than a pixel.
    Outline,
}

/// How many times below the single centre sample's RMSE a resolve that has
/// averaged all `phases` (N) must be. N uncorrelated samples at random
/// positions in a pixel estimate its coverage with 1 / N of one such
/// sample's mean squared error (a low-discrepancy sequence does better).
/// Where sub-pixel strips cross a pixel the centre sample hits or misses
/// like a random one, so the bar is sqrt(N). Where an edge wider than the
/// pixel crosses it at a uniformly distributed position, the centre sample
/// errs by 1/12 of the contrast squared against coverage and a random one by
/// 1/6, so N samples are sqrt(N / 2) better than the centre: 4 and 2.83 for
/// TAA's 16 phases, 2.83 and 2 for FSR's 8 at Native AA.
fn bar(method: Antialiasing, rig: &Rig, edge: Edge) -> f32 {
    let n = phases(method, rig) as f32;
    match edge {
        Edge::Strips => n.sqrt(),
        Edge::Outline => (n / 2.).sqrt(),
    }
}

/// The half-float stores a uniform radiance passes through on its way to
/// the `Antialiased` target, each of which may leave it one step off (WGSL
/// lets a conversion take either neighbour; this suite's Metal device takes
/// the lower, storing 0.15 as 0.14990234): the composite, then TAA's
/// accumulation, which is its output, or FSR's prepared input colour, its
/// upscaled history (both RGBA16F in the SDK port's resources) and its
/// output. Their arithmetic between stores is f32, far below a step, and at
/// a uniform pixel the current frame's weight holds the history within a
/// step of the composite rather than letting it drift.
fn stores(method: Antialiasing) -> f32 {
    match method {
        Antialiasing::Taa => 2.,
        Antialiasing::Fsr2 | Antialiasing::Fsr3 => 4.,
        Antialiasing::Off => 1.,
        other => panic!("{other:?} is not under test"),
    }
}

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

/// A pixel's width in metres at `DEPTH`.
fn px() -> f32 {
    VIEW.pixel_metres(DEPTH)
}

fn card() -> Shape {
    Shape::card(Vec2::new(1.4, 0.9), 0.3, CARD)
}

/// Strips 0.4 px wide every 1.6 px at `DEPTH`.
fn grating() -> Shape {
    Shape::grating(
        Vec2::new(1.4, 0.9),
        0.25,
        GRATING,
        STRIPS,
        0.4 * px(),
        1.6 * px(),
    )
}

/// A card 6 by 4 m at 8 m, centred, of `BACKGROUND`, with the sky beside
/// it.
fn background() -> (Shape, Pose) {
    (
        Shape::card(Vec2::new(6., 4.), 0., BACKGROUND),
        Pose::at(Vec3::new(0., 0., -8.)),
    )
}

/// Each shape's pose at a frame.
type Script<'a> = &'a dyn Fn(u32) -> Vec<Pose>;

/// A scene of `shapes`, seen from `camera`'s pose at each frame, with the
/// shapes at `script`'s.
struct Scenario<'a> {
    shapes: Vec<Shape>,
    camera: &'a dyn Fn(u32) -> Pose,
    script: Script<'a>,
}

impl Scenario<'_> {
    fn rig(&self, gpu: &Gpu, view: View, method: Antialiasing) -> Rig {
        let mut rig = Rig::new(gpu, view, method, SKY);
        self.populate(&mut rig);
        rig
    }

    /// Gives `rig` a new scene of this scenario's shapes, which restarts
    /// its history: case 10 checks that a frame after a scene switch does
    /// not depend on the frames before it.
    fn restage(&self, rig: &mut Rig) {
        rig.new_scene();
        self.populate(rig);
    }

    fn populate(&self, rig: &mut Rig) {
        for (shape, pose) in self.shapes.iter().zip((self.script)(0)) {
            rig.add(shape, pose, 0);
        }
    }

    /// Poses `rig` for `frame` and renders it. Only shapes whose pose
    /// changed are placed, so a still one is no scene edit.
    fn render(&self, rig: &mut Rig, frame: u32) {
        rig.look((self.camera)(frame));
        let poses = (self.script)(frame);
        let before = frame.checked_sub(1).map(|f| (self.script)(f));
        for (i, id) in rig.instances().into_iter().enumerate() {
            if before.as_ref().is_none_or(|b| b[i] != poses[i]) {
                rig.place(id, poses[i]);
            }
        }
        rig.render(frame);
    }

    /// Each shape's quads, projected at `frame`.
    fn polygons(&self, frame: u32) -> Vec<Vec<Vec<Vec2>>> {
        let camera = VIEW.camera((self.camera)(frame));
        self.shapes
            .iter()
            .zip((self.script)(frame))
            .map(|(shape, pose)| {
                shape
                    .quads_at(pose)
                    .iter()
                    .map(|quad| VIEW.project_polygon(&camera, quad).unwrap())
                    .collect()
            })
            .collect()
    }

    /// The pixels whose box straddles an edge of any quad of `shapes` at
    /// `frame`: where a single sample errs and antialiasing works.
    fn edges(&self, frame: u32, shapes: &[usize]) -> Region {
        let polygons = self.polygons(frame);
        Region::from_fn(VIEW.size, |pixel| {
            shapes.iter().any(|&s| {
                polygons[s].iter().any(|polygon| {
                    let c = coverage(polygon, pixel, 0.);
                    c > 0. && c < 1.
                })
            })
        })
    }

    /// The pixels wholly inside `shape`'s outline crossed by an edge of its
    /// other quads (a grating's strips) at `frame`: `Edge::Strips`.
    fn strip_edges(&self, frame: u32, shape: usize) -> Region {
        let polygons = &self.polygons(frame)[shape];
        Region::from_fn(VIEW.size, |pixel| {
            coverage(&polygons[0], pixel, 0.) >= 1.
                && polygons[1..].iter().any(|strip| {
                    let c = coverage(strip, pixel, 0.);
                    c > 0. && c < 1.
                })
        })
    }

    /// The pixels crossed by the outline (first quad) of any of `shapes` at
    /// `frame` and met by no other quad: `Edge::Outline`.
    fn outline_edges(&self, frame: u32, shapes: &[usize]) -> Region {
        let polygons = self.polygons(frame);
        Region::from_fn(VIEW.size, |pixel| {
            let crosses = shapes.iter().any(|&s| {
                let c = coverage(&polygons[s][0], pixel, 0.);
                c > 0. && c < 1.
            });
            crosses
                && polygons
                    .iter()
                    .flat_map(|quads| &quads[1..])
                    .all(|strip| coverage(strip, pixel, 0.) == 0.)
        })
    }

    /// The pixels whose box, grown by `margin` pixels, lies wholly inside
    /// `shape`'s outline (its first quad) at `frame`.
    fn inside(&self, frame: u32, shape: usize, margin: f32) -> Region {
        let outline = &self.polygons(frame)[shape][0];
        Region::from_fn(VIEW.size, |pixel| coverage(outline, pixel, margin) >= 1.)
    }

    /// The pixels whose box, grown by `margin` pixels, meets no shape at
    /// `frame`: the sky.
    fn outside(&self, frame: u32, margin: f32) -> Region {
        let polygons = self.polygons(frame);
        Region::from_fn(VIEW.size, |pixel| {
            polygons
                .iter()
                .all(|quads| coverage(&quads[0], pixel, margin) == 0.)
        })
    }
}

/// The poses of the stage most cases share: the card and the grating side
/// by side `DEPTH` ahead over the sky, both still.
fn stage_poses(_: u32) -> Vec<Pose> {
    vec![
        Pose::at(Vec3::new(-0.9, 0.1, -DEPTH)),
        Pose::at(Vec3::new(0.9, -0.1, -DEPTH)),
    ]
}

fn stage(camera: &dyn Fn(u32) -> Pose) -> Scenario<'_> {
    Scenario {
        shapes: vec![card(), grating()],
        camera,
        script: &stage_poses,
    }
}

fn still(_: u32) -> Pose {
    Pose::at(Vec3::ZERO)
}

/// Frames of a scenario without temporal antialiasing: the reference
/// oracle, rendered with antialiasing off at `SUPERSAMPLE` times the size,
/// which no temporal path touches, and box-filtered down in the space the
/// resolves average in; and the single unjittered sample a pixel that
/// antialiasing must beat. Neither keeps history, so any frame renders
/// alone. TAA averages in v / (1 + v) per channel (`HDRToSDR` in
/// `TAA_ComputeTemporalAccumulation.wgsl`), and FSR2 and FSR3 in AMD's
/// `Tonemap`, rgb / (1 + max(rgb)) (`ffx_fsr2_accumulate.h` and
/// `ffx_fsr3upscaler_accumulate.h`, "YCoCg -> RGB -> Tonemap -> YCoCg"),
/// the same map for the grey fixtures, so one reference serves all three.
struct Oracles {
    reference: Rig,
    single: Rig,
}

impl Oracles {
    fn new(scenario: &Scenario, gpu: &Gpu) -> Oracles {
        Oracles {
            reference: scenario.rig(gpu, VIEW.scaled(SUPERSAMPLE), Antialiasing::Off),
            single: scenario.rig(gpu, VIEW, Antialiasing::Off),
        }
    }

    fn restage(&mut self, scenario: &Scenario) {
        scenario.restage(&mut self.reference);
        scenario.restage(&mut self.single);
    }

    fn reference(&mut self, scenario: &Scenario, frame: u32) -> Image {
        scenario.render(&mut self.reference, frame);
        self.reference.antialiased().downsampled_through(
            SUPERSAMPLE,
            |v| v / (1. + v),
            |t| t / (1. - t),
        )
    }

    fn single(&mut self, scenario: &Scenario, frame: u32) -> Image {
        scenario.render(&mut self.single, frame);
        self.single.antialiased()
    }
}

/// A case's failures across its methods and runs, asserted once at its
/// end, so one run reports every method's numbers.
#[derive(Default)]
struct Failures(Vec<String>);

impl Failures {
    fn check(&mut self, holds: bool, message: impl FnOnce() -> String) {
        if !holds {
            self.0.push(message());
        }
    }

    /// Case 12 over every antialiased frame `rig` read. Plausible defects:
    /// a resolve or an upscaler that rings, overshoots or writes NaN
    /// creates radiance an unlit fixture cannot. The oracle is the bound
    /// (`Rig::excursion`), within the rounding of the method's `stores`.
    fn bounded(&mut self, rig: &Rig, what: impl std::fmt::Display) {
        let rounding = stores(rig.settings.antialiasing) * HALF_STEP;
        if let Some((beyond, at)) = rig.excursion(rounding) {
            self.0.push(format!(
                "{what}: an antialiased value left the fixture's radiance range by {beyond} of the bound it crossed: {at} (case 12)"
            ));
        }
    }

    fn assert(self) {
        assert!(self.0.is_empty(), "\n{}", self.0.join("\n"));
    }
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
#[test]
fn jitter_is_sub_pixel_and_never_in_the_motion() {
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
    for (method, gpu) in methods() {
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
}

/// Case 3. Plausible defects: the resolve does not accumulate the jittered
/// samples (no jitter, history discarded, a wrong blend). The oracle is the
/// reference: after the hold and a whole jitter cycle every edge pixel has
/// averaged every jitter phase, so its RMSE against the supersampled frame
/// is at least `bar` times below the single centre sample's, on the
/// grating's strips and on the outlines alike. FSR runs without sharpening
/// (`fixture_settings`): RCAS deliberately moves edges away from their
/// coverage, which is what the reference measures.
#[test]
fn at_rest_the_resolve_converges_on_coverage() {
    let Some(plain) = gpu() else { return };
    let scenario = stage(&still);
    let mut oracles = Oracles::new(&scenario, &plain);
    let truth = oracles.reference(&scenario, 0);
    let single = oracles.single(&scenario, 0);
    let regions = [
        (Edge::Strips, scenario.strip_edges(0, 1)),
        (Edge::Outline, scenario.outline_edges(0, &[0, 1])),
    ];
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
        let mut rig = scenario.rig(&gpu, VIEW, method);
        for frame in 0..SETTLE {
            scenario.render(&mut rig, frame);
        }
        let resolved = rig.antialiased();
        let mut dump = Dump::new(&format!("at_rest_{method:?}"));
        dump.image("converged", &resolved);
        dump.image("reference", &truth);
        for (edge, region) in &regions {
            let ours = error(&resolved, &truth, region).rmse;
            let theirs = error(&single, &truth, region).rmse;
            let bar = bar(method, &rig, *edge);
            dump.metric(format!("{edge:?}_rmse"), ours);
            dump.metric(format!("{edge:?}_single_sample_rmse"), theirs);
            eprintln!(
                "{method:?} at rest, {edge:?} ({} pixels): RMSE {ours}, a single sample's {theirs}, {} times below against the bar {bar}",
                region.len(),
                theirs / ours
            );
            failures.check(ours * bar <= theirs, || {
                format!(
                    "{method:?} at rest, {edge:?}: the converged resolve's RMSE {ours} is not {bar} times below a single sample's {theirs}"
                )
            });
        }
        // #793 adds FSR2's bound back.
        if method != Antialiasing::Fsr2 {
            failures.bounded(&rig, format!("{method:?} at rest"));
        }
    }
    failures.assert();
}

/// Case 4, for TAA, whose resting blend the spec states (FSR's is AMD's,
/// with no stated number). Plausible defect: the resolve keeps rejecting
/// history at rest, so edges shimmer. The oracle is the requirement of
/// Frame item 10: at rest TAA keeps partial rectification and blends in the
/// current frame at 1 - 0.88 times Filament's alpha floor 0.3, its variance
/// box at Filament's floor 0.1 (`sgl-post-fx` PROVENANCE.md,
/// "Motion-adaptive TAA"), in its blend space v / (1 + v). From one frame
/// to the next a pixel then moves at most 0.036 of the way to the current
/// sample plus 0.964 times 0.1 of the way to its clipped history, each
/// within the fixture's contrast in that space, plus a half-float step for
/// each frame's store. Both terms belong in it: the bound is tight, and the
/// feedback term alone (0.0043 here) sits below the 0.0050 a resting edge
/// pixel reaches through the partial rectification the spec keeps. The bound
/// holds over `region` for the two jitter cycles after the resolve settled.
fn taa_holds_still_at_rest(name: &str, region: impl Fn(&Scenario) -> Region) {
    let Some(gpu) = gpu() else { return };
    let scenario = stage(&still);
    let mut rig = scenario.rig(&gpu, VIEW, Antialiasing::Taa);
    let mut resolved = Vec::new();
    for frame in 0..SETTLE + 2 * JITTER_CYCLE {
        scenario.render(&mut rig, frame);
        if frame + 1 >= SETTLE {
            resolved.push(rig.antialiased());
        }
    }
    let sdr = |v: f32| v / (1. + v);
    let [low, high] = rig.radiance_range();
    let feedback = (1. - 0.88) * 0.3;
    let bound = (feedback + (1. - feedback) * 0.1) * (sdr(high) - sdr(low)) + 2. * high * HALF_STEP;
    let stability = image::stability(&resolved, &region(&scenario), sdr);
    let mut dump = Dump::new(&format!("at_rest_stability_{}", name.replace(' ', "_")));
    dump.metric("max_step_sdr", stability.max_step);
    dump.metric("std_dev_sdr", stability.std_dev);
    dump.metric("bound_sdr", bound);
    eprintln!("TAA at rest, {name}: {stability:?} against the bound {bound}");
    let mut failures = Failures::default();
    failures.check(stability.max_step <= bound, || {
        format!(
            "TAA at rest changed a pixel of the {name} by {} in its blend space, above the resting blend's {bound}",
            stability.max_step
        )
    });
    failures.bounded(&rig, "TAA at rest");
    failures.assert();
}

/// Case 4 where both sides of each edge are geometry: the grating's strips
/// on its card, two pixels inside the card's outline.
#[test]
fn at_rest_taa_holds_edges_between_surfaces_still() {
    taa_holds_still_at_rest("edges between surfaces", |scenario| {
        scenario.edges(0, &[1]).and(&scenario.inside(0, 1, 2.))
    });
}

// #791 adds case 4 at silhouettes against the sky.

/// Case 5. Plausible defects: history lost or misplaced under camera
/// translation (reprojection off by the jitter or a pixel, rejection of
/// motion that is only the camera's). The oracles: at every frame of a pan
/// at an integer and a fractional rate, after the settle, the RMSE over the
/// edges against that frame's reference is never above a single sample's
/// (reference); and a pixel whose box, grown by the 2 px reach of the
/// bicubic history fetch, has lain wholly inside the card, or wholly in the
/// sky, for the hold shows that radiance within the method's `stores`
/// (analytic).
#[test]
fn panning_keeps_history() {
    let Some(plain) = gpu() else { return };
    let mut oracles: Option<Oracles> = None;
    let mut failures = Failures::default();
    let mut rigs: Vec<(Antialiasing, Option<Rig>, Gpu)> = methods()
        .into_iter()
        .map(|(m, gpu)| (m, None, gpu))
        .collect();
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
            None => oracles.insert(Oracles::new(&scenario, &plain)),
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
        for (method, rig, gpu) in &mut rigs {
            let method = *method;
            let rig = match rig {
                Some(rig) => {
                    scenario.restage(rig);
                    rig
                }
                None => rig.insert(scenario.rig(gpu, VIEW, method)),
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
                // #793 adds FSR2's held sky back.
                if method == Antialiasing::Fsr2 && name == "sky" {
                    continue;
                }
                failures.check(error <= stores(method) * HALF_STEP, || {
                    format!(
                        "{method:?} panning {rate} px a frame: a pixel held in the {name} for the hold is {error} of its radiance off, frame {frame} pixel {pixel:?}"
                    )
                });
            }
        }
    }
    for (method, rig, _) in &rigs {
        let method = *method;
        // #792 and #793 add TAA's and FSR2's bounds back.
        if !matches!(method, Antialiasing::Taa | Antialiasing::Fsr2) {
            failures.bounded(rig.as_ref().unwrap(), format!("{method:?} panning"));
        }
    }
    failures.assert();
}

/// Case 6. Plausible defect: history ghosts where a moving surface uncovers
/// another (no disocclusion, no rejection). The oracle is analytic with the
/// spec's hold: a pixel whose grown box met the moving card and has since
/// lain wholly on the still background behind it for the hold shows the
/// background's radiance within the method's `stores`.
#[test]
fn a_disocclusion_recovers() {
    let (back, back_pose) = background();
    let across = Motion::translate(Vec3::new(1.5 * px(), 0., 0.));
    let start = Pose::at(Vec3::new(-1.2, 0., -DEPTH));
    let script = move |f: u32| vec![back_pose, across.pose(start, f)];
    let scenario = Scenario {
        shapes: vec![back, card()],
        camera: &still,
        script: &script,
    };
    let frames = SETTLE + 2 * HOLD;
    // Each frame's pixels whose grown box meets the moving card, and those
    // vacated for the hold up to it: met before the hold, not since, and on
    // the background.
    let met: Vec<Region> = (0..frames)
        .map(|f| {
            let card = &scenario.polygons(f)[1][0];
            Region::from_fn(VIEW.size, |p| coverage(card, p, 2.) > 0.)
        })
        .collect();
    let mut earlier = Region::from_fn(VIEW.size, |_| false);
    let vacated: Vec<Region> = (HOLD..frames)
        .map(|frame| {
            earlier = earlier.or(&met[(frame - HOLD) as usize]);
            met[(frame + 1 - HOLD) as usize..=frame as usize]
                .iter()
                .fold(earlier.clone(), |a, b| a.and(&b.not()))
                .and(&scenario.inside(frame, 0, 2.))
        })
        .collect();
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
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
            let e = error_to(&resolved, BACKGROUND, vacated);
            if e.max > residual.0 {
                residual = (e.max, frame, e.worst);
            }
            checked += e.pixels;
        }
        eprintln!(
            "{method:?} disocclusion: {checked} pixel-frames vacated for the hold, residual at most {} (frame {}, pixel {:?})",
            residual.0, residual.1, residual.2
        );
        assert!(checked > 0, "the card vacated no pixel for the hold");
        failures.check(residual.0 <= stores(method) * BACKGROUND * HALF_STEP, || {
            format!(
                "{method:?}: a pixel vacated for the hold is {} off the background, frame {} pixel {:?}",
                residual.0, residual.1, residual.2
            )
        });
        // #792 and #793 add TAA's and FSR2's bounds back.
        if !matches!(method, Antialiasing::Taa | Antialiasing::Fsr2) {
            failures.bounded(&rig, format!("{method:?} disocclusion"));
        }
    }
    failures.assert();
}

/// The pose `distance` ahead of and slightly below `camera`, facing it.
fn ahead(camera: Pose, distance: f32) -> Pose {
    camera.then(Pose::at(Vec3::new(0., -0.2, -distance)))
}

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
#[test]
fn a_surface_the_camera_follows_keeps_its_history() {
    let Some(plain) = gpu() else { return };
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
    let single = Oracles::new(&following, &plain).single(&following, last);
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
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
        // #793 adds FSR2's bound back.
        if method != Antialiasing::Fsr2 {
            failures.bounded(&rig, format!("{method:?} following"));
        }
    }
    failures.assert();
}

/// Case 8 (#768). Plausible defect: the same rejection on a surface
/// approaching a still camera. A grating centred ahead comes 3 mm a frame
/// closer from 0.6 m: past 0.55 m its depth changes by 0.1 x 0.003 / z^2 of
/// normalized depth a frame, above the adaptive depth threshold 0.001 (Frame
/// item 10), throughout its interior, where every depth around a pixel is
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
#[test]
fn a_surface_moving_along_the_view_keeps_its_history() {
    let Some(plain) = gpu() else { return };
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
    let mut oracles = Oracles::new(&scenario, &plain);
    let truths: Vec<(Image, Image)> = (SETTLE - 1..frames)
        .map(|f| {
            (
                oracles.reference(&scenario, f),
                oracles.single(&scenario, f),
            )
        })
        .collect();
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
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
        // #793 adds FSR2's bound back.
        if method != Antialiasing::Fsr2 {
            failures.bounded(&rig, format!("{method:?} approaching"));
        }
    }
    failures.assert();
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
/// because nothing re-arms rejection. The card lies 0.2 m before a
/// background 8 m away, a layer on a wall: their normalized depths differ
/// by 0.1 / 7.8 - 0.1 / 8 = 3.2e-4, below the adaptive depth threshold
/// 0.001, so depth disocclusion cannot reject and only the view-wide re-arm
/// can. The oracle is analytic with the spec's rule that appearances,
/// departures and visibility changes re-arm the view (Frame item 10): the
/// hold after each event, the pixels whose box grown by 2 px lies wholly
/// inside the card show their new truth (the background, or the card)
/// within the method's `stores`. Each event follows the last by the
/// settle and the hold, so the hold has expired; every event and method
/// runs before the assertion, so one failure reports them all.
#[test]
fn appearance_and_departure_at_rest_resolve_within_the_hold() {
    let (back, back_pose) = background();
    let layer = Shape::card(Vec2::new(2.4, 1.6), 0.3, CARD);
    let layer_pose = Pose::at(Vec3::new(0.6, 0.2, -7.8));
    let script = move |_| vec![back_pose, layer_pose];
    let scenario = Scenario {
        shapes: vec![back.clone(), layer.clone()],
        camera: &still,
        script: &script,
    };
    let inside = scenario.inside(0, 1, 2.);
    let events = [
        Event::Hidden,
        Event::Shown,
        Event::Masked,
        Event::Unmasked,
        Event::Removed,
        Event::Added,
    ];
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
        let mut rig = Rig::new(&gpu, VIEW, method, SKY);
        rig.add(&back, back_pose, 0);
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
                Event::Masked => rig.input.visibility_mask &= !1,
                Event::Unmasked => rig.input.visibility_mask |= 1,
                Event::Removed => rig.remove(instance.take().unwrap()),
                Event::Added => instance = Some(rig.add(&layer, layer_pose, 1)),
            }
            let truth = match event {
                Event::Hidden | Event::Masked | Event::Removed => BACKGROUND,
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
                "{method:?} {event:?}: residual {residual} the hold after it, within {epsilon} from frame {converged:?} after it"
            );
            failures.check(residual <= epsilon, || {
                format!(
                    "{method:?} {event:?}: the card's pixels are {residual} off their new truth {truth} the hold after the event; residuals by frame {residuals:?}"
                )
            });
        }
        // #793 adds FSR2's bound back.
        if method != Antialiasing::Fsr2 {
            failures.bounded(&rig, format!("{method:?} events"));
        }
    }
    failures.assert();
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
#[test]
fn a_cut_a_resize_and_another_scene_restart_history() {
    let Some(plain) = gpu() else { return };
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
    let mut unantialiased = panned.rig(&plain, VIEW, Antialiasing::Off);
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
        let mut runs = [&panned, &held].map(|scenario| scenario.rig(&gpu, VIEW, method));
        for frame in 0..=restarts[2].0 {
            let restart = restarts.iter().find(|r| r.0 == frame).map(|r| r.1);
            let mut images = Vec::new();
            for (rig, scenario) in runs.iter_mut().zip([&panned, &held]) {
                match restart {
                    Some(Restart::Cut) => rig.input.camera_cut = true,
                    Some(Restart::Resize) => rig.view = resized,
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
                unantialiased.view = runs[0].view;
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
#[test]
fn moving_the_origin_during_a_pan_changes_nothing() {
    let pan = VIEW.pan(-0.37, DEPTH);
    let camera = move |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    let scenario = stage(&camera);
    let moves = [(20, Vec3::new(8., 0., -4.)), (30, Vec3::new(-3., 1., 2.))];
    let mut failures = Failures::default();
    for (method, gpu) in methods() {
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
        // #792 and #793 add TAA's and FSR2's bounds back.
        if !matches!(method, Antialiasing::Taa | Antialiasing::Fsr2) {
            failures.bounded(&plain, format!("{method:?} panning"));
            failures.bounded(&moved, format!("{method:?} panning with origin moves"));
        }
    }
    failures.assert();
}
