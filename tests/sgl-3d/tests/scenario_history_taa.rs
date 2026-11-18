//! TAA's history rules settled by the 2026-11-18 history audit
//! (`specs/sgl3d-history.md`): the history clip on content flat in a
//! channel (T-13, O-10), and the sky's motion under a turning backdrop
//! (M-10, O-11). Scenario harness suite (`specs/testing.md`, "SGL3D
//! scenario harness"); each test runs alone by name. The resolve's mask
//! dilation (T-25, B-15) is guarded at the resolve, in `sgl-post-fx`'s
//! `taa_adaptive_a_pixel_at_the_jittered_edge_of_still_marked_content_keeps_its_strength`.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose, coverage};
use scenario::image::{Region, centre, pixels};
use scenario::rig::{HALF_STEP, Rig, gpu};
use scenario::temporal::{DEPTH, JITTER_CYCLE, SETTLE, SKY, VIEW, px, still};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Mat3, Vec2, Vec3};
use sgl_3d::settings::Antialiasing;
use sgl_3d::{Backdrop, Camera};

/// A 64-texel square image whose red varies from texel to texel (a fixed
/// hash, bytes 40 to 220) and whose green and blue are `flat`.
fn red_noise(flat: u8) -> sgl_3d::asset::Image {
    let image = ::image::RgbaImage::from_fn(64, 64, |x, y| {
        let hash =
            (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
        let red = 40 + (hash >> 24) % 181;
        ::image::Rgba([red as u8, flat, flat, 255])
    });
    sgl_3d::asset::Image::Rgba8(image)
}

/// T-13 (O-10 of the 2026-11-18 history audit). Plausible defect: a change
/// to the history's round trip or to the box (a lossy history format, a
/// colour space or clip target the neighbourhood's samples do not hold)
/// leaves a channel that is flat over the neighbourhood a rounding step off
/// its zero-width box at rest, and the clip, which moves every channel
/// together towards the neighbourhood mean (Godot's `clip_aabb`), then
/// pulls the other channels' history with it every frame: the flicker
/// Filament's minimum box half-width (`kMinBoxHalfWidth`, `taa.md`) was
/// added against. The fixture: an unlit card at rest whose red varies
/// texel to texel at about a pixel a texel, so jitter moves its red from
/// frame to frame, with green and blue flat, judged where the card fills
/// the neighbourhood the resolve reads. The oracle is equivalence:
/// green and blue are flat, so their history equals their sample and the
/// box leaves them alone; the red the resolve writes is then the same
/// whatever level they are flat at, 0 (exact in every store) or 128 or 255
/// (rounded by the stores), at every pixel of every frame.
#[test]
fn a_channel_flat_at_rest_leaves_the_others_alone() {
    let Some(gpu) = gpu() else { return };
    // 64 texels across 64 px at DEPTH.
    let size = Vec2::splat(64. * px());
    let pose = Pose::at(Vec3::new(0., 0., -DEPTH));
    let reds = |flat: u8| -> Vec<Vec<f32>> {
        let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
        rig.add_textured(size, red_noise(flat), pose);
        rig.look(still(0));
        (0..SETTLE + 2 * JITTER_CYCLE)
            .map(|frame| {
                rig.render(frame);
                let image = rig.read(DiagnosticTarget::Antialiased);
                image.texels.iter().map(|texel| texel[0]).collect()
            })
            .collect()
    };
    let exact = reds(0);
    let camera = VIEW.camera(still(0));
    let corner = |x: f32, y: f32| pose.position + Vec3::new(x * size.x, y * size.y, 0.) / 2.;
    let outline = VIEW
        .project_polygon(
            &camera,
            &[
                corner(-1., -1.),
                corner(1., -1.),
                corner(1., 1.),
                corner(-1., 1.),
            ],
        )
        .unwrap();
    // The pixels whose box grown by 2 px (the bicubic fetch's reach) lies on
    // the card, where green and blue are flat over the neighbourhood.
    let card = Region::from_fn(VIEW.size, |p| coverage(&outline, p, 2.) >= 1.);
    for flat in [128, 255] {
        let rounded = reds(flat);
        let mut worst = (0f32, 0, [0; 2]);
        for (frame, (a, b)) in exact.iter().zip(&rounded).enumerate() {
            for pixel in card.iter() {
                let i = (pixel[1] * VIEW.size[0] + pixel[0]) as usize;
                let off = (a[i] - b[i]).abs();
                if off > worst.0 {
                    worst = (off, frame, pixel);
                }
            }
        }
        eprintln!(
            "green and blue flat at {flat}: largest difference in red from flat at 0 {} (frame {}, pixel {:?}) over {} pixels",
            worst.0,
            worst.1,
            worst.2,
            card.len()
        );
        assert_eq!(
            worst.0, 0.,
            "green and blue flat at {flat} changed the resolved red by {} at frame {} pixel {:?} (O-10)",
            worst.0, worst.1, worst.2
        );
    }
}

/// The sky's `Motion` truth at the pixel position `pixel` (RG,
/// current-minus-previous UV): the direction through it now, turned by
/// `turn` radians about +Y, which is where the same point of a panorama
/// turned from yaw `y0` to `y1` lay (`turn = y0 - y1`), projected through
/// the last frame's camera.
fn sky_motion(now: &Camera, before: &Camera, turn: f32, pixel: Vec2) -> Vec2 {
    let was = Mat3::from_rotation_y(turn) * VIEW.ray(now, pixel);
    let (then, _) = VIEW
        .project(before, before.eye + was)
        .expect("the turned direction in front of the camera");
    let size = Vec2::new(VIEW.size[0] as f32, VIEW.size[1] as f32);
    (pixel - then) / size
}

/// A fifth of TAA's still-pixel classification (0.01 px) at this view's
/// width, in UV: motion the resolve could mistake for movement exceeds it.
const STILL_UV: f32 = 0.01 / 5. / VIEW.size[0] as f32;

/// The backdrop's yaw at a frame: none for a colour.
type Yaw<'a> = &'a dyn Fn(u32) -> Option<f32>;

/// M-10 (O-11 of the 2026-11-18 history audit). Plausible defects: the sky
/// writes the camera's rotation alone while the game turns its panorama, so
/// TAA reprojects the sky from where it no longer is and FSR and motion
/// blur see it still; the turn applied the wrong way, from the wrong yaw,
/// or across a restart or a change from a colour, which has no direction.
/// The oracle is analytic, as case 1's (`motion_vectors_match_the_script`):
/// at every sky pixel, the panorama's point through it now projected where
/// it lay through the last frame's camera at the last frame's yaw, within
/// the truth's spread over the pixel (its jittered sample lies anywhere in
/// it), a half-float step and `STILL_UV`. Scripts: a still camera under a
/// panorama turning 0.004 rad a frame; a camera turning with it, which
/// sees it still; the same panorama, turning, after a camera cut, which
/// restarts history; and a colour backdrop replaced by a turned panorama.
#[test]
fn a_turning_backdrop_moves_the_sky() {
    let Some(gpu) = gpu() else { return };
    let rate = 0.004;
    let turning = |f: u32| Some(0.3 + rate * f as f32);
    let from_colour = |f: u32| (f >= 3).then_some(0.3);
    // Name, camera, backdrop, and whether the last frame cuts.
    let cases: [(&str, Motion, Yaw, bool); 4] = [
        ("still camera", Motion::STILL, &turning, false),
        ("camera turning with it", Motion::yaw(rate), &turning, false),
        ("after a cut", Motion::STILL, &turning, true),
        ("from a colour", Motion::STILL, &from_colour, false),
    ];
    let frame = 3;
    for (name, camera, yaw, cut) in cases {
        let camera_at = |f: u32| camera.pose(Pose::at(Vec3::ZERO), f);
        let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
        for f in 0..=frame {
            rig.look(camera_at(f));
            rig.views[0].input.backdrop = match yaw(f) {
                Some(yaw) => Backdrop::Environment {
                    yaw,
                    brightness: 1.,
                },
                None => Backdrop::Color([SKY; 3]),
            };
            rig.views[0].input.camera_cut = cut && f == frame;
            rig.render(f);
        }
        let motion = rig.read(DiagnosticTarget::Motion);
        let now = VIEW.camera(camera_at(frame));
        // After a restart the last frame's camera is this one's.
        let before = if cut {
            now
        } else {
            VIEW.camera(camera_at(frame - 1))
        };
        let turn = match (cut, yaw(frame - 1), yaw(frame)) {
            (false, Some(y0), Some(y1)) => y0 - y1,
            _ => 0.,
        };
        let (mut worst, mut largest) = (0f32, 0f32);
        for pixel in pixels(VIEW.size) {
            let at = |p: Vec2| sky_motion(&now, &before, turn, p);
            let expected = at(centre(pixel));
            let spread = [(0., 0.), (1., 0.), (0., 1.), (1., 1.)]
                .map(|(x, y)| Vec2::new(pixel[0] as f32 + x, pixel[1] as f32 + y))
                .iter()
                .map(|&c| (at(c) - expected).abs())
                .fold(Vec2::ZERO, Vec2::max);
            let [r, g, _, a] = motion.at(pixel);
            let off = (Vec2::new(r, g) - expected).abs();
            worst = worst.max(off.x.max(off.y));
            largest = largest.max(expected.abs().max_element());
            assert!(
                off.cmple(spread + expected.abs() * HALF_STEP + STILL_UV)
                    .all()
                    && a == 0.,
                "{name}: the sky at {pixel:?} wrote motion {:?} where the camera and the panorama's turn give {expected}",
                motion.at(pixel)
            );
        }
        eprintln!(
            "sky motion, {name}: largest truth {} px, largest error {} px",
            largest * VIEW.size[0] as f32,
            worst * VIEW.size[0] as f32
        );
    }
}

/// M-10b (O-11 of the 2026-11-18 history audit): a change of the backdrop's
/// colour, brightness or panorama is a shading change, which writes no
/// motion and marks no mask, and TAA follows it at rest through its box.
/// Plausible defect: the sky's history kept against the change (a rule that
/// trusts the sky's history more, or holds it), so a backdrop fade or swap
/// trails for seconds. The fixture: the camera still before a colour
/// backdrop alone, which steps from `SKY` to 0.2 once the hold has expired.
/// The oracle is analytic, with the spec's resting blend (Frame item 10):
/// every pixel's 3x3 neighbourhood is uniform, so its box has no extent
/// and each resolve leaves at most (1 - 0.1)(1 - 0.12 x 0.3) of the step
/// in the blend space v / (1 + v) (case 14's `survives(0)`), plus the
/// rounding of TAA's two stores.
#[test]
fn a_backdrop_that_changes_at_rest_converges_through_the_box() {
    let Some(gpu) = gpu() else { return };
    let after = 0.2;
    let sdr = |v: f32| v / (1. + v);
    let survives: f32 = (1. - 0.1) * (1. - 0.12 * 0.3);
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
    rig.admit(after);
    rig.look(still(0));
    let mut worst = (f32::NEG_INFINITY, 0, 0., 0.);
    for frame in 0..SETTLE + JITTER_CYCLE {
        if frame == SETTLE {
            rig.views[0].input.backdrop = Backdrop::Color([after; 3]);
        }
        rig.render(frame);
        let Some(n) = (frame + 1).checked_sub(SETTLE).filter(|&n| n > 0) else {
            continue;
        };
        let bound = survives.powi(n as i32) * (sdr(after) - sdr(SKY)) + 2. * after * HALF_STEP;
        let image = rig.antialiased();
        for pixel in pixels(VIEW.size) {
            let off = (sdr(image.value(pixel)) - sdr(after)).abs();
            if off - bound > worst.0 {
                worst = (off - bound, n, off, bound);
            }
        }
    }
    let (excess, n, off, bound) = worst;
    eprintln!(
        "backdrop step at rest: largest distance from the new colour {off} against {bound}, {n} resolves after it"
    );
    assert!(
        excess <= 0.,
        "{n} resolves after the backdrop changed the sky is {off} from it in the blend space, above the resting blend's {bound} (M-10b)"
    );
    let mut failures = scenario::temporal::Failures::default();
    failures.bounded(&rig, "backdrop step at rest");
    failures.assert();
}
