//! Render textures in the scenario harness (`specs/testing.md`, "SGL3D
//! scenario harness"): a view's picture as a material's emission map (the
//! architecture's Frame, "Render textures"; `specs/sgl3d.md`, S3D-3), judged
//! by numbers read from the frame of the view that shows the screen: which
//! of the source's known colours its texels hold, where a source object
//! lies on it, and how much light it emits.
//!
//! The suite covers the feature at rest and with a source object moving
//! across the producer's view, both listing orders and either view primary,
//! feedback, a producer the game does not list or the frame leaves out,
//! resize, output scaling, a second consumer, and frames that were not
//! submitted or not finished. The consumer's camera decides only whether a
//! screen is in its view, which the unseen cases cover; beyond that its
//! motion and a camera cut cannot affect which picture a screen shows: the
//! picture is the producer's, and the consumer's history is covered by the
//! views suite (`scenario_views.rs`). It does not cover the picture under
//! each antialiasing method of the consumer: the composition mask's rule
//! and a left-out producer's restart are tested in the crate
//! (`renderer::render_texture_tests`, `scene::render_textures::tests`).
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::rig::{Gpu, Rig, Shape, floor_gpu, gpu};
use scenario::temporal::{DEPTH, Failures, VIEW};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::graphics_device::BindingTier;
use sgl_3d::settings::{Antialiasing, SceneResolution};
use sgl_3d::{
    InstanceId, InstanceState, MaterialId, RenderTexture, RenderTextureId, SurfaceMaterial,
};
use std::f32::consts::PI;

/// The lens of a view that draws a render texture, and so the texture's
/// size.
const FEED: View = View {
    size: [64, 36],
    ..VIEW
};
/// A screen's size in metres and its emission.
const SCREEN: Vec2 = Vec2::new(3.2, 1.8);
const EMISSION: f32 = 1.;
/// A source that fills a feed's view from 6 to 8 m ahead of it.
const SOURCE: Vec2 = Vec2::new(24., 14.);
/// The grey a dark source emits behind a marker.
const DARK: f32 = 0.02;

/// A source's known colours: three in which one channel is strong, then
/// three in which one is weak.
const COLOURS: [[f32; 3]; 6] = [
    [0.6, 0.02, 0.02],
    [0.02, 0.6, 0.02],
    [0.02, 0.02, 0.6],
    [0.02, 0.6, 0.6],
    [0.6, 0.02, 0.6],
    [0.6, 0.6, 0.02],
];

/// Which of `COLOURS` a texel holds, by its channels' order alone, which a
/// tone map and the screen's emission keep: one channel at least 3 times
/// both others, or two at least 3 times the third. None for black (every
/// channel under 0.01, a tenth of the weakest strong channel a screen
/// emits) and for anything else.
fn colour(texel: [f32; 4]) -> Option<usize> {
    let [r, g, b, _] = texel;
    let channels = [r, g, b];
    let strong = |c: usize| (0..3).all(|o| o == c || channels[c] >= 3. * channels[o]);
    let weak = |c: usize| (0..3).all(|o| o == c || channels[o] >= 3. * channels[c]);
    if channels.iter().all(|&c| c < 0.01) {
        return None;
    }
    (0..3)
        .find(|&c| strong(c))
        .or_else(|| (0..3).find(|&c| weak(c)).map(|c| c + 3))
}

fn is_black(texel: [f32; 4]) -> bool {
    texel[..3].iter().all(|&c| c.abs() < 0.01)
}

/// Names `texture` as `material`'s emission map.
fn show(rig: &mut Rig, material: MaterialId, texture: RenderTextureId) {
    let values = rig.scene.material(material).expect("a screen's material");
    rig.scene
        .set_material(
            &rig.gpu.queue,
            material,
            SurfaceMaterial {
                emission_render_texture: Some(texture),
                ..values
            },
        )
        .expect("a screen that shows a render texture");
}

/// Gives `material` the flat emission `emission`.
fn emit(rig: &mut Rig, material: MaterialId, emission: [f32; 3]) {
    let values = rig.scene.material(material).expect("a source's material");
    rig.scene
        .set_material(
            &rig.gpu.queue,
            material,
            SurfaceMaterial { emission, ..values },
        )
        .expect("a source's colour");
}

/// The radiance `view`'s last frame holds where the world point `point`
/// projects (its `Composite` target).
fn radiance_at(rig: &Rig, view: usize, point: Vec3) -> [f32; 4] {
    let lens = rig.views[view].lens;
    let (pixel, _) = lens
        .project(&rig.views[view].input.camera, point)
        .expect("a point before the camera");
    rig.read_view(view, DiagnosticTarget::Composite)
        .at([pixel.x as u32, pixel.y as u32])
}

/// A monitor that shows an object behind the player's camera: view 0, the
/// main view, at the origin looking along -Z at a screen `DEPTH` ahead;
/// view `feed`, behind it and looking the other way at a source that fills
/// its view, drawing the render texture the screen shows.
struct Monitor {
    rig: Rig,
    feed: usize,
    texture: RenderTextureId,
    screen: (InstanceId, MaterialId),
    source: MaterialId,
}

const SCREEN_CENTRE: Vec3 = Vec3::new(0., 0., -DEPTH);
const FEED_POSE: Pose = Pose {
    position: Vec3::new(0., 0., 10.),
    yaw: PI,
};

impl Monitor {
    fn new(gpu: &Gpu) -> Monitor {
        let mut rig = Rig::new(gpu, VIEW, Antialiasing::Off, 0.);
        let texture = rig
            .scene
            .add_render_texture(&gpu.device, RenderTexture::new(FEED.size))
            .expect("a render texture");
        let feed = rig.add_producer(FEED, Antialiasing::Off, texture);
        rig.look_view(feed, FEED_POSE);
        let screen = rig.add_emissive(SCREEN, [EMISSION; 3], Pose::at(SCREEN_CENTRE));
        show(&mut rig, screen.1, texture);
        let (_, source) = rig.add_emissive(SOURCE, COLOURS[0], Pose::at(Vec3::new(0., 0., 16.)));
        Monitor {
            rig,
            feed,
            texture,
            screen,
            source,
        }
    }

    /// The source's colour becomes `COLOURS[index]`.
    fn source(&mut self, index: usize) {
        emit(&mut self.rig, self.source, COLOURS[index]);
    }

    /// Which colour the screen's centre holds in the main view's last
    /// frame.
    fn shown(&self) -> Option<usize> {
        colour(radiance_at(&self.rig, 0, SCREEN_CENTRE))
    }
}

/// Plausible defects: the views render as listed, so a main view listed
/// first shows the picture of the frame before; the producer is not ordered
/// before its consumer when the producer is the primary view; the scene's
/// frame work is not done before a producer that renders ahead of the
/// primary; the Basic binding tier's group 2 does not bind the picture. The
/// fixture: `Monitor`, the source's colour changing every frame, on a
/// device of the adapter's limits or, with `floor`, of S3D-1's floor, which
/// takes the Basic tier. The oracle is analytic: in every frame the
/// screen's centre holds that frame's colour, whichever view is listed
/// first.
fn a_monitor_shows_this_frames_picture(order: &[usize], floor: bool) {
    let Some(gpu) = (if floor { floor_gpu() } else { gpu() }) else {
        return;
    };
    let mut monitor = Monitor::new(&gpu);
    let tier = monitor.rig.renderer.binding_tier();
    assert!(
        !floor || tier == BindingTier::Basic,
        "a device at S3D-1's floor takes the Basic tier, not {tier:?}"
    );
    let mut failures = Failures::default();
    for frame in 0..5 {
        let index = frame as usize % 3;
        monitor.source(index);
        monitor.rig.render_order(frame, order);
        let shown = monitor.shown();
        failures.check(shown == Some(index), || {
            format!("listed {order:?}, {tier:?}, frame {frame}: the screen holds colour {shown:?}, not this frame's {index}")
        });
    }
    failures.assert();
}

mod a_monitor_shows_this_frames_picture {
    crate::scenario::temporal::case_tests!(
        a_monitor_shows_this_frames_picture;
        main_view_primary: (&[0, 1], false),
        producer_primary: (&[1, 0], false),
        basic_tier: (&[0, 1], true),
    );
}

/// Where the bright texels of `view`'s last frame lie on the screen whose
/// corners project to `low` and `high` (pixels): their centroid, of those
/// above 0.3, between a dark source's and a marker's radiance.
fn marker_centroid(rig: &Rig, view: usize, [low, high]: [Vec2; 2]) -> Option<Vec2> {
    let image = rig.read_view(view, DiagnosticTarget::Composite);
    let mut sum = Vec2::ZERO;
    let mut count = 0.;
    for y in low.y.ceil() as u32..high.y.floor() as u32 {
        for x in low.x.ceil() as u32..high.x.floor() as u32 {
            if image.value([x, y]) > 0.3 {
                sum += Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                count += 1.;
            }
        }
    }
    (count > 0.).then(|| sum / count)
}

/// `Monitor` with a dark source and a bright marker before it, 5.5 m ahead
/// of the feed, which `place_marker` moves.
fn marker_monitor(gpu: &Gpu) -> (Monitor, InstanceId) {
    let mut monitor = Monitor::new(gpu);
    emit(&mut monitor.rig, monitor.source, [DARK; 3]);
    let (marker, _) = monitor
        .rig
        .add_emissive(Vec2::splat(1.2), [0.8; 3], Pose::at(marker_at(0)));
    (monitor, marker)
}

/// The marker's centre at `frame`: 2 m further along +X each frame, 0.8 m
/// above the feed's axis.
fn marker_at(frame: u32) -> Vec3 {
    Vec3::new(-2. + 2. * frame as f32, 0.8, 15.5)
}

fn place_marker(monitor: &mut Monitor, marker: InstanceId, frame: u32) {
    let state = InstanceState {
        pose: Pose::at(marker_at(frame)).matrix(),
        ..*monitor.rig.scene.instance(marker).expect("the marker")
    };
    monitor
        .rig
        .scene
        .set_instance(&monitor.rig.gpu.queue, marker, state)
        .expect("the marker's pose");
}

/// Where the marker's centre at `frame` lies in the main view, in pixels:
/// through the feed's camera onto its picture, and from the picture's UV
/// onto the screen's rectangle in the main view. Also that rectangle.
fn marker_on_screen(monitor: &Monitor, frame: u32) -> (Vec2, [Vec2; 2]) {
    let rig = &monitor.rig;
    let feed = &rig.views[monitor.feed];
    let (in_feed, _) = feed
        .lens
        .project(&feed.input.camera, marker_at(frame))
        .expect("the marker before the feed");
    let uv = in_feed / Vec2::new(feed.lens.size[0] as f32, feed.lens.size[1] as f32);
    let corner = |x: f32, y: f32| {
        VIEW.project(
            &rig.views[0].input.camera,
            SCREEN_CENTRE + Vec3::new(x * SCREEN.x * 0.5, y * SCREEN.y * 0.5, 0.),
        )
        .expect("the screen before the main view")
        .0
    };
    // The screen's UV origin is its top left corner.
    let [low, high] = [corner(-1., 1.), corner(1., -1.)];
    (low + uv * (high - low), [low, high])
}

/// Plausible defects: the screen shows the source where it was a frame
/// ago; the picture is mirrored or upside down on the screen. The fixture:
/// `Monitor` with a bright marker that moves 2 m across the feed's view
/// each frame (about 15 px on the screen in the main view), above its
/// axis. The oracle is analytic: the marker's centroid on the screen is
/// where the script puts it this frame, through the feed's camera and the
/// screen's UV, within one of the picture's texels as the screen shows it
/// (1.24 px) and a pixel of the main view's rasterisation: 2 px.
#[test]
fn a_source_object_moves_on_the_monitor_in_its_own_frame() {
    let Some(gpu) = gpu() else { return };
    let (mut monitor, marker) = marker_monitor(&gpu);
    let mut failures = Failures::default();
    for frame in 0..3 {
        place_marker(&mut monitor, marker, frame);
        monitor.rig.render(frame);
        let (expected, screen) = marker_on_screen(&monitor, frame);
        let found = marker_centroid(&monitor.rig, 0, screen);
        eprintln!("frame {frame}: the marker at {found:?}, the script's {expected}");
        failures.check(
            found.is_some_and(|found| found.distance(expected) <= 2.),
            || {
                format!(
                    "frame {frame}: the marker lies at {found:?} on the screen, not at {expected}"
                )
            },
        );
    }
    failures.assert();
}

/// Plausible defect: the scene's GPU work of the frame (the deform pass,
/// in the primary view's stage order) runs after a producer that renders
/// ahead of the primary view, so the producer draws the deformations of the
/// frame before. The fixture: `Monitor` with a dark source and, where
/// `marker_at` puts the marker, a bright card that a morph target moves
/// there from its place at frame 0, the main view listed first, the
/// primary. The oracle is `a_source_object_moves_on_the_monitor_in_its_own_frame`'s:
/// the card's centroid on the screen is where the script's morph weight
/// puts it this frame, within 2 px.
#[test]
fn a_deforming_source_moves_on_the_monitor_in_its_own_frame() {
    let Some(gpu) = gpu() else { return };
    let mut monitor = Monitor::new(&gpu);
    emit(&mut monitor.rig, monitor.source, [DARK; 3]);
    let frames = 3;
    let reach = marker_at(frames - 1) - marker_at(0);
    let card = monitor.rig.add_morphing(
        &Shape::card(Vec2::splat(1.2), 0., 0.8),
        Pose::at(marker_at(0)),
        reach,
    );
    let mut failures = Failures::default();
    for frame in 0..frames {
        let weight = frame as f32 / (frames - 1) as f32;
        monitor
            .rig
            .scene
            .set_instance_deformation(&gpu.queue, card, &[], &[weight])
            .expect("the morph weight");
        monitor.rig.render_order(frame, &[0, monitor.feed]);
        let (expected, screen) = marker_on_screen(&monitor, frame);
        let found = marker_centroid(&monitor.rig, 0, screen);
        eprintln!("frame {frame}: the morphed card at {found:?}, the script's {expected}");
        failures.check(
            found.is_some_and(|found| found.distance(expected) <= 2.),
            || format!("frame {frame}: the morphed card lies at {found:?} on the screen, not at {expected}"),
        );
    }
    failures.assert();
}

/// IEC 61966-2-1's transfer functions, the oracle's external reference.
fn srgb_encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1. / 2.4) - 0.055
    }
}

fn srgb_decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// S3D-3. Plausible defects: the picture is stored encoded and sampled as
/// data, or encoded by the shader into an sRGB attachment, so the screen
/// emits the encoded value or one encoded twice; the picture is stored
/// before the display transform; either on the path that tone maps straight
/// into the render texture, a game's, or on the one that presents a
/// captured tone-mapped frame. The fixture: `Monitor` with a source of one
/// colour whose channels differ, once with the feed capturing its
/// tone-mapped frame (`DiagnosticTarget::ToneMapped`, the displayed picture
/// in linear values) and once without, which the capture's contract says
/// writes the same output. The oracle is analytic, from IEC 61966-2-1: in
/// both runs the screen emits its emission times the displayed picture,
/// within the 1.5 code values of an 8-bit sRGB texel that its rounding and
/// post's dither move a value. A value encoded once more, or not decoded,
/// lies far outside: 0.2 encodes to 0.48.
#[test]
fn a_screen_emits_the_displayed_picture_decoded_once() {
    let Some(gpu) = gpu() else { return };
    let mut failures = Failures::default();
    let mut displayed = None;
    for capture in [true, false] {
        let mut monitor = Monitor::new(&gpu);
        emit(&mut monitor.rig, monitor.source, [0.5, 0.25, 0.1]);
        let feed = monitor.feed;
        monitor.rig.views[feed]
            .settings
            .diagnostics
            .capture_tone_target = capture;
        monitor.rig.render(0);
        if capture {
            displayed = Some(
                monitor
                    .rig
                    .read_view(feed, DiagnosticTarget::ToneMapped)
                    .at([FEED.size[0] / 2, FEED.size[1] / 2]),
            );
        }
        let displayed = displayed.expect("the captured run comes first");
        let emitted = radiance_at(&monitor.rig, 0, SCREEN_CENTRE);
        eprintln!("capture {capture}: displayed {displayed:?}, emitted {emitted:?}");
        for channel in 0..3 {
            let encoded = srgb_encode(displayed[channel]);
            let step = 1.5 / 255.;
            let [low, high] = [encoded - step, encoded + step].map(|e| EMISSION * srgb_decode(e));
            failures.check(
                displayed[channel] > 0.02 && (low..=high).contains(&emitted[channel]),
                || {
                    format!(
                        "capture {capture}, channel {channel}: the screen emits {}, not the displayed {} within {low}..={high}",
                        emitted[channel], displayed[channel]
                    )
                },
            );
        }
    }
    failures.assert();
}

/// A stage of the feedback cases, `at` metres along +X, far outside the
/// other stages' views: a view at its origin looking along -Z that draws
/// `texture`, a source that fills its view 8 m ahead, and a small screen
/// up and to the left, 6 m ahead, clear of the view's centre, that shows
/// `shows`. Returns the view, the source's material and the screen's
/// centre, where the view's own frame holds the screen and whose picture's
/// centre is the source of the view that drew it.
fn feedback_stage(
    rig: &mut Rig,
    at: f32,
    texture: RenderTextureId,
    shows: RenderTextureId,
) -> (usize, MaterialId, Vec3) {
    let origin = Vec3::new(at, 0., 0.);
    let view = rig.add_producer(FEED, Antialiasing::Off, texture);
    rig.look_view(view, Pose::at(origin));
    let (_, source) = rig.add_emissive(
        SOURCE,
        COLOURS[0],
        Pose::at(origin + Vec3::new(0., 0., -8.)),
    );
    let centre = origin + Vec3::new(-2.5, 1.4, -6.);
    let (_, screen) = rig.add_emissive(Vec2::new(2., 1.2), [EMISSION; 3], Pose::at(centre));
    show(rig, screen, shows);
    (view, source, centre)
}

/// A main view at the origin whose screen, `DEPTH` ahead, shows `texture`.
fn main_screen(rig: &mut Rig, texture: RenderTextureId) {
    let (_, screen) = rig.add_emissive(SCREEN, [EMISSION; 3], Pose::at(SCREEN_CENTRE));
    show(rig, screen, texture);
}

fn feed_texture(rig: &mut Rig) -> RenderTextureId {
    rig.scene
        .add_render_texture(&rig.gpu.device, RenderTexture::new(FEED.size))
        .expect("a render texture")
}

/// Plausible defects: a view that samples its own render texture reads and
/// writes it in one pass (a validation error), is never rendered, or shows
/// something other than its last picture. The fixture: a main view whose
/// screen shows the texture view F draws, and in F's own view a second
/// screen that shows it too; F's source changes colour every frame. The
/// oracles are analytic: F's screen, in F's own frame, holds the colour of
/// the frame before (black in the first), its source this frame's, and the
/// main view's screen this frame's.
#[test]
fn a_view_that_shows_its_own_picture_shows_its_last() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, 0.);
    let texture = feed_texture(&mut rig);
    main_screen(&mut rig, texture);
    let (view, source, own_screen) = feedback_stage(&mut rig, 100., texture, texture);
    let behind = Vec3::new(100., 0., -8.);
    let mut failures = Failures::default();
    for frame in 0..5u32 {
        let index = frame as usize % 3;
        emit(&mut rig, source, COLOURS[index]);
        rig.render(frame);
        let own = radiance_at(&rig, view, own_screen);
        let last = frame.checked_sub(1).map(|last| last as usize % 3);
        failures.check(
            match last {
                Some(last) => colour(own) == Some(last),
                None => is_black(own),
            },
            || format!("frame {frame}: F's own screen holds {own:?}, not its last picture's colour {last:?}"),
        );
        let now = colour(radiance_at(&rig, view, behind));
        failures.check(now == Some(index), || {
            format!("frame {frame}: F's source holds colour {now:?}, not {index}")
        });
        let main = colour(radiance_at(&rig, 0, SCREEN_CENTRE));
        failures.check(main == Some(index), || {
            format!("frame {frame}: the main view's screen holds colour {main:?}, not this frame's {index}")
        });
    }
    failures.assert();
}

/// Plausible defects: views that show each other's pictures deadlock the
/// order, both show a stale picture, or the one listed first shows this
/// frame's. The fixture: a main view whose screen shows view A's texture;
/// A's own view holds a screen that shows B's, and B's one that shows A's.
/// A's source cycles through the colours with one strong channel and B's
/// through those with one weak channel. The oracles are analytic, from the
/// rule: of A and B, the one listed first shows the other's last picture
/// (black in the first frame) and the other shows this frame's; the main
/// view's screen shows A's of this frame either way.
fn views_that_show_each_other_follow_their_listing(a_first: bool) {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, 0.);
    let [texture_a, texture_b] = [feed_texture(&mut rig), feed_texture(&mut rig)];
    main_screen(&mut rig, texture_a);
    let (a, source_a, screen_in_a) = feedback_stage(&mut rig, 100., texture_a, texture_b);
    let (b, source_b, screen_in_b) = feedback_stage(&mut rig, 200., texture_b, texture_a);
    let order = if a_first { [0, a, b] } else { [0, b, a] };
    // The colour each source holds at a frame.
    let of_a = |frame: u32| frame as usize % 3;
    let of_b = |frame: u32| 3 + (frame as usize + 1) % 3;
    let mut failures = Failures::default();
    for frame in 0..5u32 {
        emit(&mut rig, source_a, COLOURS[of_a(frame)]);
        emit(&mut rig, source_b, COLOURS[of_b(frame)]);
        rig.render_order(frame, &order);
        // What A's screen (B's picture) and B's screen (A's) must hold.
        let last = frame.checked_sub(1);
        let (in_a, in_b) = if a_first {
            (last.map(of_b), Some(of_a(frame)))
        } else {
            (Some(of_b(frame)), last.map(of_a))
        };
        for (name, view, screen, expected) in
            [("A", a, screen_in_a, in_a), ("B", b, screen_in_b, in_b)]
        {
            let texel = radiance_at(&rig, view, screen);
            failures.check(
                match expected {
                    Some(expected) => colour(texel) == Some(expected),
                    None => is_black(texel),
                },
                || {
                    format!(
                        "listed {order:?}, frame {frame}: {name}'s screen holds {texel:?} (colour {:?}), not colour {expected:?}",
                        colour(texel)
                    )
                },
            );
        }
        let main = colour(radiance_at(&rig, 0, SCREEN_CENTRE));
        failures.check(main == Some(of_a(frame)), || {
            format!("listed {order:?}, frame {frame}: the main view's screen holds colour {main:?}, not A's of this frame")
        });
    }
    failures.assert();
}

mod views_that_show_each_other_follow_their_listing {
    crate::scenario::temporal::case_tests!(
        views_that_show_each_other_follow_their_listing;
        a_listed_first: (true),
        b_listed_first: (false),
    );
}

/// Plausible defects: a producer the game left out of a frame is rendered
/// anyway because a screen shows its texture, or its texture is cleared.
/// The fixture: `Monitor`; frame 1 lists the main view alone while the
/// source holds a colour of its own. The oracle is analytic: in frame 1
/// the screen still holds frame 0's colour, and in frame 2 frame 2's.
#[test]
fn a_producer_the_game_does_not_list_keeps_its_last_picture() {
    let Some(gpu) = gpu() else { return };
    let mut monitor = Monitor::new(&gpu);
    let mut failures = Failures::default();
    for (frame, order, expected) in [(0, &[0, 1][..], 0), (1, &[0], 0), (2, &[0, 1], 2)] {
        monitor.source(frame as usize);
        monitor.rig.render_order(frame, order);
        let shown = monitor.shown();
        failures.check(shown == Some(expected), || {
            format!(
                "frame {frame}, listed {order:?}: the screen holds colour {shown:?}, not {expected}"
            )
        });
    }
    failures.assert();
}

/// How a screen is out of the main view in a frame.
#[derive(Clone, Copy, Debug)]
enum Unseen {
    /// The main view turns away from it.
    TurnedAway,
    /// Its instance is hidden.
    Hidden,
    /// Its material's visibility group is outside the view's mask.
    Masked,
}

/// Plausible defect: a listed producer renders although no view shows its
/// texture, at a whole view's cost. The fixture: `Monitor`; in frame 1 the
/// screen is out of the main view (`unseen`) while both views are listed
/// and the source holds a colour of its own; frame 2 shows the screen again
/// and lists the main view alone. The oracle is analytic: frame 2's screen
/// holds frame 0's colour, which it could not had the producer rendered in
/// frame 1; and frame 3, listing both, holds frame 3's.
fn a_producer_no_view_shows_does_not_render(unseen: Unseen) {
    let Some(gpu) = gpu() else { return };
    let mut monitor = Monitor::new(&gpu);
    let (screen, screen_material) = monitor.screen;
    let see = |monitor: &mut Monitor, seen: bool| match unseen {
        Unseen::TurnedAway => monitor.rig.look(Pose {
            position: Vec3::ZERO,
            yaw: if seen { 0. } else { PI / 2. },
        }),
        Unseen::Hidden => {
            let rig = &mut monitor.rig;
            let state = InstanceState {
                visible: seen,
                ..*rig.scene.instance(screen).expect("the screen")
            };
            rig.scene
                .set_instance(&rig.gpu.queue, screen, state)
                .expect("the screen's visibility");
        }
        Unseen::Masked => {
            let rig = &mut monitor.rig;
            let values = rig.scene.material(screen_material).expect("the screen");
            // The rig's views enable groups 0..24.
            let visibility_group = if seen { 0 } else { 1 << 24 };
            rig.scene
                .set_material(
                    &rig.gpu.queue,
                    screen_material,
                    SurfaceMaterial {
                        visibility_group,
                        ..values
                    },
                )
                .expect("the screen's visibility group");
        }
    };
    let mut failures = Failures::default();
    for (frame, order, seen, expected) in [
        (0, &[0, 1][..], true, Some(0)),
        (1, &[0, 1], false, None),
        (2, &[0], true, Some(0)),
        (3, &[0, 1], true, Some(3)),
    ] {
        monitor.source(frame as usize);
        see(&mut monitor, seen);
        monitor.rig.render_order(frame, order);
        if let Some(expected) = expected {
            let shown = monitor.shown();
            failures.check(shown == Some(expected), || {
                format!(
                    "{unseen:?}, frame {frame}: the screen holds colour {shown:?}, not {expected}"
                )
            });
        }
    }
    failures.assert();
}

mod a_producer_no_view_shows_does_not_render {
    use super::Unseen::*;
    crate::scenario::temporal::case_tests!(
        a_producer_no_view_shows_does_not_render;
        turned_away: (TurnedAway),
        hidden: (Hidden),
        masked: (Masked),
    );
}

/// Plausible defects: a new render texture holds stale or undefined
/// texels; a resize leaves the screen's material bound to the texture it
/// replaced, so the screen freezes; the producer keeps drawing at the old
/// size. The fixture: `Monitor`; the texture is replaced by a larger one
/// between frames 1 and 2, and frames 0 and 2 list the main view alone. The
/// oracles are analytic: the screen is black before the producer's first
/// frame and after the replacement, and holds the frame's colour in each
/// frame the producer renders; and the scene reports the new size.
#[test]
fn a_render_texture_is_black_until_drawn_and_after_a_resize() {
    let Some(gpu) = gpu() else { return };
    let mut monitor = Monitor::new(&gpu);
    let larger = [96, 54];
    let mut failures = Failures::default();
    for (frame, order, expected) in [
        (0, &[0][..], None),
        (1, &[0, 1], Some(1)),
        (2, &[0], None),
        (3, &[0, 1], Some(0)),
    ] {
        if frame == 2 {
            let rig = &mut monitor.rig;
            rig.scene
                .set_render_texture(&gpu.device, monitor.texture, RenderTexture::new(larger))
                .expect("a larger render texture");
            rig.views[monitor.feed].lens = View {
                size: larger,
                ..FEED
            };
        }
        monitor.source(frame as usize % 3);
        monitor.rig.render_order(frame, order);
        let texel = radiance_at(&monitor.rig, 0, SCREEN_CENTRE);
        failures.check(
            match expected {
                Some(expected) => colour(texel) == Some(expected),
                None => is_black(texel),
            },
            || format!("frame {frame}: the screen holds {texel:?}, not colour {expected:?}"),
        );
    }
    let size = monitor
        .rig
        .scene
        .render_texture(monitor.texture)
        .expect("the render texture")
        .size;
    failures.check(size == larger, || {
        format!("the render texture's size is {size:?}, not {larger:?}")
    });
    failures.assert();
}

/// Plausible defects: a producer that renders its scene below its output
/// size writes a corner of the render texture, or a picture of another
/// size than the screen samples. The fixture: `marker_monitor` with the
/// feed at `SceneResolution::Half`. The oracles: the feed renders at half
/// the texture's size (the setting's meaning), and the marker lies where
/// the script puts it this frame (`marker_on_screen`), within two of the
/// half-size picture's texels as the screen shows them and a pixel: 4 px.
#[test]
fn a_producer_scales_its_scene_to_the_render_texture() {
    let Some(gpu) = gpu() else { return };
    let (mut monitor, marker) = marker_monitor(&gpu);
    let feed = monitor.feed;
    monitor.rig.views[feed].settings.scene_resolution = SceneResolution::Half;
    let mut failures = Failures::default();
    for frame in 0..3 {
        place_marker(&mut monitor, marker, frame);
        monitor.rig.render(frame);
        let (expected, screen) = marker_on_screen(&monitor, frame);
        let found = marker_centroid(&monitor.rig, 0, screen);
        eprintln!("frame {frame}: the marker at {found:?}, the script's {expected}");
        failures.check(
            found.is_some_and(|found| found.distance(expected) <= 4.),
            || {
                format!(
                    "frame {frame}: the marker lies at {found:?} on the screen, not at {expected}"
                )
            },
        );
    }
    let rendered = monitor.rig.views[feed].view.render_size();
    failures.check(rendered == FEED.size.map(|side| side / 2), || {
        format!(
            "the feed rendered at {rendered:?}, not half of {:?}",
            FEED.size
        )
    });
    failures.assert();
}

/// Plausible defect: only the first view that shows a render texture gets
/// this frame's picture, or the producer is ordered after a second
/// consumer. The fixture: `Monitor` with a second view of the screen from
/// another place, both listed before the producer. The oracle is analytic:
/// the screen's centre holds this frame's colour in both views' frames.
#[test]
fn two_views_show_one_producers_picture_of_this_frame() {
    let Some(gpu) = gpu() else { return };
    let mut monitor = Monitor::new(&gpu);
    let second = monitor.rig.add_view(VIEW, Antialiasing::Off);
    monitor
        .rig
        .look_view(second, Pose::at(Vec3::new(1., 0.3, 0.5)));
    let mut failures = Failures::default();
    for frame in 0..3 {
        let index = frame as usize % 3;
        monitor.source(index);
        monitor.rig.render_order(frame, &[0, second, monitor.feed]);
        for view in [0, second] {
            let shown = colour(radiance_at(&monitor.rig, view, SCREEN_CENTRE));
            failures.check(shown == Some(index), || {
                format!("frame {frame}, view {view}: the screen holds colour {shown:?}, not this frame's {index}")
            });
        }
    }
    failures.assert();
}

/// Plausible defects: a frame that was encoded and dropped changes what a
/// screen shows, or a frame that was submitted and not finished does not.
/// The fixture: `Monitor`; frame 1 is rendered and its encoder dropped,
/// frame 3 is submitted and not finished, each with a colour of its own,
/// and frames 2 and 4 list the main view alone. The oracles are the
/// contract's: frame 2's screen holds frame 0's colour, the last
/// submitted; frame 4's holds frame 3's.
#[test]
fn a_render_texture_holds_the_last_submitted_picture() {
    let Some(gpu) = gpu() else { return };
    let mut monitor = Monitor::new(&gpu);
    let mut failures = Failures::default();
    monitor.source(0);
    monitor.rig.render(0);
    monitor.source(1);
    monitor.rig.render_unsubmitted(1);
    monitor.rig.render_order(2, &[0]);
    let shown = monitor.shown();
    failures.check(shown == Some(0), || {
        format!("after a dropped frame the screen holds colour {shown:?}, not the last submitted 0")
    });
    monitor.source(2);
    monitor.rig.render_unfinished(3);
    monitor.rig.render_order(4, &[0]);
    let shown = monitor.shown();
    failures.check(shown == Some(2), || {
        format!("after a submitted, unfinished frame the screen holds colour {shown:?}, not its 2")
    });
    failures.assert();
}
