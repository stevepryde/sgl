//! Sprite batches in the scenario harness (`specs/testing.md`, "SGL3D
//! scenario harness"; the register's M-21 and M-22 in
//! `specs/sgl3d-history.md`): a sprite moving across the view under TAA,
//! masked and blended, judged by what the resolve shows where the sprite is
//! and where it was. A sprite facing the still camera is the card of the
//! same size at its position, whose outline is the analytic truth.
//!
//! Covered: a sprite moving across the view at rest, its appearance (the
//! batch is added before the first frame) and the pixels it departs. Not
//! covered here: camera translation and rotation (the sprite's motion is
//! the camera's reprojection, `stages/transparent/sprites_tests.rs`), a
//! sprite moving along the view, a camera cut and a resize (sprites keep
//! no history of their own; TAA's own cases cover its restarts), and FSR,
//! whose masks the sprites write as for TAA.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{FRAME_RATE, Pose, coverage};
use scenario::image::{Region, stability};
use scenario::rig::{HALF_STEP, Rig, Shape, gpu};
use scenario::temporal::{
    BACKGROUND, CARD, DEPTH, Failures, GRATING, HOLD, JITTER_CYCLE, SETTLE, SKY, STRIPS, Scenario,
    VIEW, background, px, sdr, still, stores, survives, vacated,
};
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;
use sgl_3d::sprite::{Sprite, SpriteBatch, SpriteBlend};

/// The sprite's width and height in metres at `DEPTH`.
const SIZE: Vec2 = Vec2::new(1.4, 0.9);
/// When it starts moving, after the resolve has settled at rest, and the
/// frame after which it is judged.
const FRAMES: (u32, u32) = (HOLD + 1, HOLD + 1 + 3 * HOLD);

/// Plausible defects: a moving sprite smearing (its pixels taking history
/// it does not own: a masked sprite whose batch changed left unmarked, a
/// blended one marking nothing) or leaving a ghost where it was (no mark
/// arming the recovery hold where it stood, no depth for disocclusion). The
/// fixture: the background wall 8 m away, the camera still, a sprite of
/// `CARD` 4 m away that rests until the first hold has expired and then
/// moves along +X at `rate` pixels a frame, blended at `alpha` or masked.
/// Pixels are judged where their box grown by 2 px lies inside the wall.
/// The oracles are analytic, with the bounds of
/// `scenario_temporal_antialiasing`'s case 14 for marked content: (1)
/// arriving, a pixel whose box grown by 2 px has lain inside the sprite for
/// n frames is within `survives(mask)^n` of the step from the wall to the
/// sprite over it (in v / (1 + v)), plus a store's rounding, where mask is
/// the larger mask the sprite writes (1 for a masked sprite that changed,
/// its alpha for a blended one); (2) departing, a pixel the sprite
/// `vacated` for the hold shows the wall within TAA's `stores`; and every
/// frame stays within the fixture's radiance range.
fn a_moving_sprite_shows_itself_and_leaves_no_ghost(blend: Option<f32>, rate: f32) {
    let Some(gpu) = gpu() else { return };
    let method = Antialiasing::Taa;
    let rest = Pose::at(Vec3::new(-0.6, 0., -DEPTH));
    let velocity = Vec3::X * rate * px() * FRAME_RATE as f32;
    let start = (f64::from(FRAMES.0) / FRAME_RATE) as f32;
    let at = move |f: u32| {
        let time = (f64::from(f) / FRAME_RATE) as f32;
        rest.position + velocity * (time - start).max(0.)
    };
    let (wall, wall_pose) = background();
    let script = move |f: u32| vec![wall_pose, Pose::at(at(f))];
    let truth = Scenario {
        shapes: vec![wall.clone(), Shape::card(SIZE, 0., CARD)],
        camera: &still,
        script: &script,
    };
    let vacated = vacated(&truth, 1, FRAMES.1, 2.);
    let mut rig = Rig::new(&gpu, VIEW, method, SKY);
    rig.add(&wall, wall_pose, 0);
    rig.admit(CARD);
    let (device, queue) = (rig.gpu.device.clone(), rig.gpu.queue.clone());
    let white =
        sgl_3d::asset::Image::Rgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4])));
    let image = rig.scene.add_sprite_image(&device, &queue, &white).unwrap();
    let batch = rig
        .scene
        .add_sprite_batch(&SpriteBatch {
            blend: match blend {
                Some(_) => SpriteBlend::Blended,
                None => SpriteBlend::Masked { cutoff: 0.5 },
            },
            ..SpriteBatch::new(image)
        })
        .unwrap();
    let alpha = blend.unwrap_or(1.);
    let sprite = |frame| Sprite {
        tint: [CARD, CARD, CARD, alpha],
        ..Sprite::new(at(frame), SIZE)
    };
    let over = alpha * CARD + (1. - alpha) * BACKGROUND;
    // Case 14's mask, less an R8 step.
    let mask = blend.unwrap_or(1.) - 1. / 255.;
    let step = sdr(over) - sdr(BACKGROUND);
    rig.look(still(0));
    let name = format!(
        "{} sprite at {rate} px a frame",
        match blend {
            Some(alpha) => format!("a blended ({alpha})"),
            None => "a masked".into(),
        }
    );
    let mut inside_for = vec![0u32; (VIEW.size[0] * VIEW.size[1]) as usize];
    // The largest excess over each bound, with its frame and pixel, and the
    // pixel-frames judged: arriving, then departing.
    let mut excess = [(f32::NEG_INFINITY, 0, [0; 2]); 2];
    let mut checked = [0usize; 2];
    for frame in 0..FRAMES.1 {
        rig.scene
            .set_sprites(&device, &queue, batch, &[sprite(frame)])
            .unwrap();
        rig.render(frame);
        let resolved = rig.antialiased();
        let on_wall = truth.inside(frame, 0, 2.);
        let inside = truth.inside(frame, 1, 2.);
        for pixel in on_wall.iter() {
            let i = (pixel[1] * VIEW.size[0] + pixel[0]) as usize;
            if inside.contains(pixel) {
                inside_for[i] += 1;
                let bound = survives(mask).powi(inside_for[i] as i32) * step
                    + stores(method) * over * HALF_STEP;
                let off = (sdr(resolved.value(pixel)) - sdr(over)).abs();
                checked[0] += 1;
                if off - bound > excess[0].0 {
                    excess[0] = (off - bound, frame, pixel);
                }
            } else {
                inside_for[i] = 0;
            }
            if frame >= HOLD && vacated[(frame - HOLD) as usize].contains(pixel) {
                let bound = stores(method) * BACKGROUND * HALF_STEP;
                let off = (resolved.value(pixel) - BACKGROUND).abs();
                checked[1] += 1;
                if off - bound > excess[1].0 {
                    excess[1] = (off - bound, frame, pixel);
                }
            }
        }
    }
    eprintln!(
        "{name}: judged {checked:?} pixel-frames arriving and departing, largest excess over the bounds {excess:?}"
    );
    assert!(checked[0] > 0 && checked[1] > 0, "{name}: nothing judged");
    let mut failures = Failures::default();
    for (which, (excess, frame, pixel)) in ["arriving", "departing"].into_iter().zip(excess) {
        failures.check(excess <= 0., || {
            format!("{name}, {which}: {excess} above the bound at frame {frame}, pixel {pixel:?}")
        });
    }
    failures.bounded(&rig, &name);
    failures.assert();
}

mod a_moving_sprite_shows_itself_and_leaves_no_ghost {
    crate::scenario::temporal::case_tests!(a_moving_sprite_shows_itself_and_leaves_no_ghost;
        masked_at_1_5_px_a_frame: (None, 1.5),
        masked_at_4_px_a_frame: (None, 4.),
        blended_at_1_5_px_a_frame: (Some(0.5), 1.5),
        blended_at_4_px_a_frame: (Some(0.5), 4.),
    );
}

/// Plausible defect: removing a masked batch re-arming TAA across the
/// whole view (the rigid-change signal), so every still surface shimmers
/// for a frame's hold whenever an effect ends, though the removal touched
/// only the batch's pixels, which its footprint's mark covers (M-21). The
/// fixture: a still grating of sub-pixel strips filling the view 8 m away,
/// the camera still, and a masked sprite 4 m away, removed once the
/// resolve has settled. The oracle is case 4's
/// (`scenario_temporal_antialiasing`, `taa_holds_still_at_rest`), as
/// `scenario_history_masks` takes it: at rest TAA blends in the current
/// frame at 1 - 0.88 times Filament's alpha floor 0.3 and clips its history
/// at Filament's variance floor 0.1, so in its blend space a pixel moves
/// from one frame to the next by at most (0.036 + 0.964 x 0.1) of the
/// fixture's contrast, plus a half-float step for each frame's store, from
/// the frame before the removal through a jitter cycle after it. The
/// region: the pixels the strips' edges cross whose box grown by 8 px
/// meets no part of the sprite.
#[test]
fn removing_a_masked_batch_keeps_the_rest_of_the_views_history() {
    let Some(gpu) = gpu() else { return };
    let far = VIEW.pixel_metres(8.);
    let grating = Shape::grating(
        Vec2::new(16., 9.),
        0.25,
        GRATING,
        STRIPS,
        0.4 * far,
        1.6 * far,
    );
    let grating_pose = Pose::at(Vec3::new(0., 0., -8.));
    let sprite_pose = Pose::at(Vec3::new(-0.6, 0., -DEPTH));
    let script = move |_| vec![grating_pose, sprite_pose];
    let truth = Scenario {
        shapes: vec![grating.clone(), Shape::card(SIZE, 0., CARD)],
        camera: &still,
        script: &script,
    };
    let footprint = &truth.polygons(0)[1][0];
    let region = truth
        .strip_edges(0, 0)
        .and(&Region::from_fn(VIEW.size, |p| {
            coverage(footprint, p, 8.) == 0.
        }));
    assert!(!region.is_empty(), "strip edges away from the sprite");
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
    rig.add(&grating, grating_pose, 0);
    rig.admit(CARD);
    let (device, queue) = (rig.gpu.device.clone(), rig.gpu.queue.clone());
    let white =
        sgl_3d::asset::Image::Rgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4])));
    let sheet = rig.scene.add_sprite_image(&device, &queue, &white).unwrap();
    let batch = rig
        .scene
        .add_sprite_batch(&SpriteBatch {
            blend: SpriteBlend::Masked { cutoff: 0.5 },
            ..SpriteBatch::new(sheet)
        })
        .unwrap();
    let sprite = Sprite {
        tint: [CARD, CARD, CARD, 1.],
        ..Sprite::new(sprite_pose.position, SIZE)
    };
    rig.scene
        .set_sprites(&device, &queue, batch, &[sprite])
        .unwrap();
    rig.look(still(0));
    let mut resolved = Vec::new();
    for frame in 0..SETTLE + JITTER_CYCLE {
        if frame == SETTLE {
            rig.scene.remove_sprite_batch(batch).unwrap();
        }
        rig.render(frame);
        if frame + 1 >= SETTLE {
            resolved.push(rig.antialiased());
        }
    }
    let [low, high] = rig.radiance_range();
    let feedback = (1. - 0.88) * 0.3;
    let bound = (feedback + (1. - feedback) * 0.1) * (sdr(high) - sdr(low)) + 2. * high * HALF_STEP;
    let stability = stability(&resolved, &region, sdr);
    eprintln!(
        "TAA after a masked batch's removal, away from it: {stability:?} against the bound {bound}"
    );
    let mut failures = Failures::default();
    failures.check(stability.max_step <= bound, || {
        format!(
            "removing a masked batch changed a still strip's edge, away from it, by {} in TAA's blend space, above the resting blend's {bound}",
            stability.max_step
        )
    });
    failures.bounded(&rig, "a masked batch's removal");
    failures.assert();
}
