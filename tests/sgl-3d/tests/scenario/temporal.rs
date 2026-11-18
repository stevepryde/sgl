//! The temporal antialiasing suites' shared stage, scripts and oracles:
//! the view and the fixtures of known radiance they share, the frames a
//! resolve takes to settle, the jitter samples a method gives a pixel and
//! the bar they set, the supersampled reference and the single sample a
//! resolve must beat, the device a method needs, the tests a case
//! instantiates, and the failures a case collects across its runs.
//! `scenario_temporal_antialiasing` judges TAA and FSR at Native AA with
//! them, `scenario_fsr_upscaling` FSR at its upscaling qualities and every
//! method under rotation.
use super::geometry::{Pose, View, coverage};
use super::image::{Image, Region, centre};
use super::rig::{Gpu, HALF_STEP, Rig, Shape, fsr_gpu, gpu};
use sgl_3d::Camera;
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;

/// 192 by 108 at a one-radian vertical field: a fixture 4 m ahead spans
/// tens of pixels, and the 8 times reference stays small.
pub const VIEW: View = View {
    size: [192, 108],
    fov_y: 1.,
    near: 0.1,
};
/// The fixtures' distance ahead of the camera.
pub const DEPTH: f32 = 4.;
/// The fixtures' radiances: a dark sky and background beneath mid-grey
/// cards, values a lit scene at exposure 0 puts on screen below white,
/// where the tone map's mid-tones are, with contrasts far above half-float
/// rounding.
pub const SKY: f32 = 0.05;
pub const BACKGROUND: f32 = 0.1;
pub const CARD: f32 = 0.5;
pub const GRATING: f32 = 0.1;
pub const STRIPS: f32 = 0.5;
/// TAA's jitter cycle: the frame's "16-sample Halton jitter" (sgl-3d's
/// README, TAA), DiligentFX's Halton (2, 3) sequence (`sgl-post-fx`'s
/// `TemporalAntiAliasing::get_jitter_offset`), in which every jitter
/// position contributes once a cycle.
pub const JITTER_CYCLE: u32 = 16;
/// The recovery hold of `specs/sgl3d-architecture.md` Frame item 10 ("a
/// twelve-frame recovery hold"): after a reset, motion or a re-arming
/// change, rejection is restored for that long.
pub const HOLD: u32 = 12;
/// Frames after a history restart until a resolve at rest has had its hold
/// expire and then a whole jitter cycle.
pub const SETTLE: u32 = HOLD + JITTER_CYCLE;
/// The reference oracle's supersampling: 8 by 8 samples a pixel, so its
/// own estimate of a tilted sub-pixel strip's coverage errs well below the
/// differences the comparisons resolve.
pub const SUPERSAMPLE: u32 = 8;

/// A device that runs `method`: FSR's features for FSR, or `None` after
/// saying why the case is skipped.
pub fn device(method: Antialiasing) -> Option<Gpu> {
    match method {
        Antialiasing::Taa => gpu(),
        _ => fsr_gpu(),
    }
}

/// One test per `$name`, each calling the case's function `super::$case`
/// with its arguments: a method, and a variant where the case has them. A
/// suite invokes it in a module named after the case, so each test runs
/// alone by name, `<case>::<name>`. The temporal suites use it; the other
/// binaries that include the harness do not.
#[allow(unused_macros)]
macro_rules! case_tests {
    ($case:ident; $($name:ident: ($($arg:expr),* $(,)?)),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                super::$case($($arg),*)
            }
        )*
    };
}
#[allow(unused_imports)]
pub(crate) use case_tests;

/// The jitter phases `method` cycles through at `rig`'s sizes: TAA's
/// `JITTER_CYCLE`, and FSR's own count at its render and display widths, as
/// the AMD SDK port the renderer uses gives it (`8 * (display / render)^2`,
/// 8 at Native AA).
pub fn phases(method: Antialiasing, rig: &Rig) -> u32 {
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
pub enum Edge {
    /// A pixel crossed by sub-pixel strips.
    Strips,
    /// A pixel crossed by the outline of a surface wider than a pixel.
    Outline,
}

/// The jitter samples `method` gives each pixel of the scene size over its
/// `phases`: each phase puts one sample in every render pixel, which spans
/// (scene / render)^2 scene pixels, so a scene pixel receives
/// phases x (render / scene)^2 of them, measured by area from the actual
/// sizes. Without upscaling that is the phases; FSR's
/// `8 * (display / render)^2` phases make it about 8 at every quality.
pub fn samples(method: Antialiasing, rig: &Rig) -> f32 {
    let [render, scene] =
        [rig.renderer.render_size(), rig.renderer.scene_size()].map(|[w, h]| w as f32 * h as f32);
    phases(method, rig) as f32 * render / scene
}

/// How many times below the single centre sample's RMSE a resolve that has
/// averaged all its jitter `samples` (N) a pixel must be. N uncorrelated
/// samples at random positions in a pixel estimate its coverage with 1 / N
/// of one such sample's mean squared error (a low-discrepancy sequence does
/// better). Where sub-pixel strips cross a pixel the centre sample hits or
/// misses like a random one, so the bar is sqrt(N). Where an edge wider
/// than the pixel crosses it at a uniformly distributed position, the centre
/// sample errs by 1/12 of the contrast squared against coverage and a random
/// one by 1/6, so N samples are sqrt(N / 2) better than the centre: 4 and
/// 2.83 for TAA's 16 phases, 2.83 and 2 for FSR's 8 at Native AA.
pub fn bar(method: Antialiasing, rig: &Rig, edge: Edge) -> f32 {
    let n = samples(method, rig);
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
pub fn stores(method: Antialiasing) -> f32 {
    match method {
        Antialiasing::Taa => 2.,
        Antialiasing::Fsr2 | Antialiasing::Fsr3 => 4.,
        Antialiasing::Off => 1.,
        other => panic!("{other:?} is not under test"),
    }
}

/// A pixel's width in metres at `DEPTH`.
pub fn px() -> f32 {
    VIEW.pixel_metres(DEPTH)
}

pub fn card() -> Shape {
    Shape::card(Vec2::new(1.4, 0.9), 0.3, CARD)
}

/// Strips 0.4 px wide every 1.6 px at `DEPTH`.
pub fn grating() -> Shape {
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
pub fn background() -> (Shape, Pose) {
    (
        Shape::card(Vec2::new(6., 4.), 0., BACKGROUND),
        Pose::at(Vec3::new(0., 0., -8.)),
    )
}

/// Each shape's pose at a frame.
pub type Script<'a> = &'a dyn Fn(u32) -> Vec<Pose>;

/// A scene of `shapes`, seen from `camera`'s pose at each frame, with the
/// shapes at `script`'s.
pub struct Scenario<'a> {
    pub shapes: Vec<Shape>,
    pub camera: &'a dyn Fn(u32) -> Pose,
    pub script: Script<'a>,
}

impl Scenario<'_> {
    pub fn rig(&self, gpu: &Gpu, view: View, method: Antialiasing) -> Rig {
        let mut rig = Rig::new(gpu, view, method, SKY);
        self.populate(&mut rig);
        rig
    }

    /// Gives `rig` a new scene of this scenario's shapes, which restarts
    /// its history: case 10 checks that a frame after a scene switch does
    /// not depend on the frames before it.
    pub fn restage(&self, rig: &mut Rig) {
        rig.new_scene();
        self.populate(rig);
    }

    pub fn populate(&self, rig: &mut Rig) {
        for (shape, pose) in self.shapes.iter().zip((self.script)(0)) {
            rig.add(shape, pose, 0);
        }
    }

    /// Poses `rig` for `frame` and renders it.
    pub fn render(&self, rig: &mut Rig, frame: u32) {
        self.pose(rig, frame);
        rig.render(frame);
    }

    /// Poses `rig`'s camera and shapes for `frame`. Only shapes whose pose
    /// changed are placed, so a still one is no scene edit; shapes a case
    /// adds after the scenario's are its own.
    pub fn pose(&self, rig: &mut Rig, frame: u32) {
        rig.look((self.camera)(frame));
        let poses = (self.script)(frame);
        let before = frame.checked_sub(1).map(|f| (self.script)(f));
        for (i, id) in rig.instances().into_iter().take(poses.len()).enumerate() {
            if before.as_ref().is_none_or(|b| b[i] != poses[i]) {
                rig.place(id, poses[i]);
            }
        }
    }

    /// Each shape's quads, projected at `frame`: the part of each that lies
    /// beyond the near plane, which is what the frame draws of a surface
    /// passing beneath or beside the camera.
    pub fn polygons(&self, frame: u32) -> Vec<Vec<Vec<Vec2>>> {
        let camera = VIEW.camera((self.camera)(frame));
        self.shapes
            .iter()
            .zip((self.script)(frame))
            .map(|(shape, pose)| {
                shape
                    .quads_at(pose)
                    .iter()
                    .map(|quad| {
                        VIEW.project_polygon(&camera, &beyond_near(&camera, quad))
                            .unwrap()
                    })
                    .collect()
            })
            .collect()
    }

    /// The pixels whose box straddles an edge of any quad of `shapes` at
    /// `frame`: where a single sample errs and antialiasing works.
    pub fn edges(&self, frame: u32, shapes: &[usize]) -> Region {
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
    pub fn strip_edges(&self, frame: u32, shape: usize) -> Region {
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
    pub fn outline_edges(&self, frame: u32, shapes: &[usize]) -> Region {
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
    pub fn inside(&self, frame: u32, shape: usize, margin: f32) -> Region {
        let outline = &self.polygons(frame)[shape][0];
        Region::from_fn(VIEW.size, |pixel| coverage(outline, pixel, margin) >= 1.)
    }

    /// The pixels whose box, grown by `margin` pixels, meets no shape at
    /// `frame`: the sky.
    pub fn outside(&self, frame: u32, margin: f32) -> Region {
        let polygons = self.polygons(frame);
        Region::from_fn(VIEW.size, |pixel| {
            polygons
                .iter()
                .all(|quads| coverage(&quads[0], pixel, margin) == 0.)
        })
    }
}

/// The world polygon `corners` (convex) clipped to `VIEW`'s near plane
/// from `camera` (Sutherland–Hodgman): unchanged where it lies wholly
/// beyond it, empty where wholly before it.
fn beyond_near(camera: &Camera, corners: &[Vec3]) -> Vec<Vec3> {
    let beyond = |p: Vec3| -camera.view.transform_point3(p).z - VIEW.near;
    let mut kept = Vec::with_capacity(corners.len() + 1);
    for (i, &a) in corners.iter().enumerate() {
        let b = corners[(i + 1) % corners.len()];
        let (da, db) = (beyond(a), beyond(b));
        if da >= 0. {
            kept.push(a);
        }
        if (da >= 0.) != (db >= 0.) {
            kept.push(a + (b - a) * (da / (da - db)));
        }
    }
    kept
}

/// A fifth of TAA's still-pixel classification (`TAA_STILL_MOTION_PIXELS`,
/// 0.01 px, DFX-19 in `sgl-post-fx`'s PROVENANCE.md) at `VIEW`'s width, in
/// UV: motion the resolve could mistake for movement exceeds it.
pub const STILL_UV: f32 = 0.01 / 5. / VIEW.size[0] as f32;
/// Depth motion beside its half-float step (`HALF_STEP` of the value): the
/// f32 rounding of two view depths of a few metres differenced, far below
/// the slowest scripted depth motion, 0.05 m a frame.
pub const STILL_DEPTH: f32 = 1e-4;

/// The square's corners of `pixel`.
fn corners(pixel: [u32; 2]) -> [Vec2; 4] {
    [(0., 0.), (1., 0.), (0., 1.), (1., 1.)]
        .map(|(x, y)| Vec2::new(pixel[0] as f32 + x, pixel[1] as f32 + y))
}

/// Case 1's analytic oracle (`scenario_temporal_antialiasing`,
/// `motion_vectors_match_the_script`) over `rig`'s last frame, `frame`, of
/// `scenario` at `VIEW`: each shape's interior (its source id's pixels
/// eroded by one) wrote the current-minus-previous UV and
/// previous-minus-current view depth that the script's poses at `frame - 1`
/// and `frame` give through the cameras at those frames, unjittered at the
/// pixel's centre, with depth validity; the sky wrote the motion of the
/// camera's rotation alone, without it. The tolerance is the truth's spread
/// over the pixel's corners, a half-float step of the value, and
/// `STILL_UV` or `STILL_DEPTH`. Returns the largest errors, in UV and
/// metres, or the first failure, labelled `name`.
pub fn motion_follows_the_script(
    rig: &Rig,
    scenario: &Scenario,
    frame: u32,
    name: &str,
) -> Result<[f32; 2], String> {
    let motion = rig.read(DiagnosticTarget::Motion);
    let sources = rig.read(DiagnosticTarget::SourceId);
    let now = VIEW.camera((scenario.camera)(frame));
    let before = VIEW.camera((scenario.camera)(frame - 1));
    let mut worst = [0f32; 2];
    for (i, (shape, id)) in scenario.shapes.iter().zip(rig.instances()).enumerate() {
        let [current, previous] = [frame, frame - 1].map(|f| (scenario.script)(f)[i].matrix());
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
        if interior.is_empty() {
            return Err(format!("{name}: instance {i} drew no interior"));
        }
        for pixel in interior.iter() {
            let expected = truth(centre(pixel));
            let spread = corners(pixel)
                .iter()
                .map(|&c| (truth(c) - expected).abs())
                .fold(Vec3::ZERO, Vec3::max);
            let [r, g, b, a] = motion.at(pixel);
            let off = (Vec3::new(r, g, b) - expected).abs();
            let tolerance =
                spread + expected.abs() * HALF_STEP + Vec3::new(STILL_UV, STILL_UV, STILL_DEPTH);
            worst = [worst[0].max(off.x.max(off.y)), worst[1].max(off.z)];
            if !(off.cmple(tolerance).all() && a == 1.) {
                return Err(format!(
                    "{name}: instance {i} at {pixel:?} wrote motion {:?} where the script gives {expected} ± {tolerance}",
                    [r, g, b, a]
                ));
            }
        }
    }
    let sky = sources.region_of(0).eroded(1);
    for pixel in sky.iter() {
        let expected = VIEW.sky_motion(&now, &before, centre(pixel));
        let spread = corners(pixel)
            .iter()
            .map(|&c| (VIEW.sky_motion(&now, &before, c) - expected).abs())
            .fold(Vec2::ZERO, Vec2::max);
        let [r, g, _, a] = motion.at(pixel);
        let off = (Vec2::new(r, g) - expected).abs();
        worst[0] = worst[0].max(off.x.max(off.y));
        if !(off
            .cmple(spread + expected.abs() * HALF_STEP + STILL_UV)
            .all()
            && a == 0.)
        {
            return Err(format!(
                "{name}: the sky at {pixel:?} wrote motion {:?} where the camera's rotation gives {expected}",
                motion.at(pixel)
            ));
        }
    }
    Ok(worst)
}

/// The poses of the stage most cases share: the card and the grating side
/// by side `DEPTH` ahead over the sky, both still.
pub fn stage_poses(_: u32) -> Vec<Pose> {
    vec![
        Pose::at(Vec3::new(-0.9, 0.1, -DEPTH)),
        Pose::at(Vec3::new(0.9, -0.1, -DEPTH)),
    ]
}

pub fn stage(camera: &dyn Fn(u32) -> Pose) -> Scenario<'_> {
    Scenario {
        shapes: vec![card(), grating()],
        camera,
        script: &stage_poses,
    }
}

pub fn still(_: u32) -> Pose {
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
pub struct Oracles {
    reference: Rig,
    single: Rig,
}

impl Oracles {
    pub fn new(scenario: &Scenario, gpu: &Gpu) -> Oracles {
        Oracles {
            reference: scenario.rig(gpu, VIEW.scaled(SUPERSAMPLE), Antialiasing::Off),
            single: scenario.rig(gpu, VIEW, Antialiasing::Off),
        }
    }

    /// Takes the single sample at the render size `render` from now on,
    /// which must keep the view's aspect, so its camera is the view's.
    pub fn set_render_size(&mut self, render: [u32; 2]) {
        let view = View {
            size: render,
            ..VIEW
        };
        assert_eq!(
            view.aspect(),
            VIEW.aspect(),
            "a render size of the view's aspect"
        );
        self.single.view = view;
    }

    pub fn restage(&mut self, scenario: &Scenario) {
        scenario.restage(&mut self.reference);
        scenario.restage(&mut self.single);
    }

    pub fn reference(&mut self, scenario: &Scenario, frame: u32) -> Image {
        scenario.render(&mut self.reference, frame);
        self.reference.antialiased().downsampled_through(
            SUPERSAMPLE,
            |v| v / (1. + v),
            |t| t / (1. - t),
        )
    }

    /// The single unjittered sample a pixel; below the view's size, the
    /// frame at that size enlarged to the view's, each pixel showing the
    /// render pixel under its centre (`Image::enlarged`): what a game that
    /// renders smaller without an upscaler shows.
    pub fn single(&mut self, scenario: &Scenario, frame: u32) -> Image {
        scenario.render(&mut self.single, frame);
        self.single.antialiased().enlarged(VIEW.size)
    }
}

/// A case's failures across its runs, asserted once at its end, so one
/// test reports every run's numbers.
#[derive(Default)]
pub struct Failures(Vec<String>);

impl Failures {
    pub fn check(&mut self, holds: bool, message: impl FnOnce() -> String) {
        if !holds {
            self.0.push(message());
        }
    }

    /// Case 12 over every antialiased frame `rig` read. Plausible defects:
    /// a resolve or an upscaler that rings, overshoots or writes NaN
    /// creates radiance an unlit fixture cannot. The oracle is the bound
    /// (`Rig::excursion`), within the rounding of the method's `stores`.
    pub fn bounded(&mut self, rig: &Rig, what: impl std::fmt::Display) {
        let rounding = stores(rig.settings.antialiasing) * HALF_STEP;
        if let Some((beyond, at)) = rig.excursion(rounding) {
            self.0.push(format!(
                "{what}: an antialiased value left the fixture's radiance range by {beyond} of the bound it crossed: {at} (case 12)"
            ));
        }
    }

    pub fn assert(self) {
        assert!(self.0.is_empty(), "\n{}", self.0.join("\n"));
    }
}

/// The pixels at each frame from `HOLD` on that `shape`'s outline, grown by
/// `reach` pixels (what the resolve reads around a pixel: 2 px, the bicubic
/// history fetch's, without upscaling), met at some frame before the hold
/// up to it and at none since: what a moving surface vacated for the hold.
pub fn vacated(scenario: &Scenario, shape: usize, frames: u32, reach: f32) -> Vec<Region> {
    let met: Vec<Region> = (0..frames)
        .map(|f| {
            let outline = &scenario.polygons(f)[shape][0];
            Region::from_fn(VIEW.size, |p| coverage(outline, p, reach) > 0.)
        })
        .collect();
    let mut earlier = Region::from_fn(VIEW.size, |_| false);
    (HOLD..frames)
        .map(|frame| {
            earlier = earlier.or(&met[(frame - HOLD) as usize]);
            met[(frame + 1 - HOLD) as usize..=frame as usize]
                .iter()
                .fold(earlier.clone(), |a, b| a.and(&b.not()))
        })
        .collect()
}

/// The pose `distance` ahead of and slightly below `camera`, facing it.
pub fn ahead(camera: Pose, distance: f32) -> Pose {
    camera.then(Pose::at(Vec3::new(0., -0.2, -distance)))
}
