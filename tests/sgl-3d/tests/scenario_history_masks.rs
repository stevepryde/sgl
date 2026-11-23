//! The temporal masks' writers in the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"; the register's M-16, M-23 and T-28 in
//! `specs/sgl3d-history.md`): what they mark, judged by what TAA then holds,
//! with the bounds of `scenario_temporal_antialiasing`'s cases. The
//! masks' own values are judged where they are read, beside their writers
//! (`stages/transparent/composition_tests.rs`, `effects_tests.rs`,
//! `blended_tests.rs`, `footprints_tests.rs`).
//!
//! Instance footprints (M-23) are covered at rest with the camera still:
//! what they keep far from a change here, and the pixels a change leaves
//! and reaches by case 9 of `scenario_temporal_antialiasing`. Camera motion
//! re-arms the whole view whatever they mark (T-20), and a moving
//! instance's own move writes motion and leaves none (M-9's exception), so
//! neither bears on them; a camera cut and a resize restart the history
//! they protect.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::Pose;
use scenario::image;
use scenario::rig::{HALF_STEP, Rig, Shape, gpu};
use scenario::temporal::{
    CARD, DEPTH, Failures, JITTER_CYCLE, SETTLE, SKY, Scenario, VIEW, background, still,
};
use sgl_3d::ShaderSource;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::Antialiasing;

/// A game's shader that reads no time: its surface function scales the
/// base colour by its parameters' `gain` and its instance's data's x
/// (`SurfaceContext::instance`), so its shading changes only when the game
/// edits either.
const GAIN_SHADER: &str = r#"
struct ShaderParams {
 gain:f32,
 unused:f32,
 unused2:vec2<f32>,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 return v;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 out.base_color=vec4(s.base_color.rgb*params.gain*(1.+ctx.instance.x),s.base_color.a);
 return out;
}
"#;

/// B-3 (M-16, T-29). Plausible defect: an opaque material marks the
/// transparency and composition mask for having a game's shader, whether or
/// not anything its shader reads changes, so TAA holds its pixels at full
/// strength every frame (T-25) and its outline shimmers at rest; or a game
/// writing the same parameters and instance data every frame, as a game's
/// update loop does, marks it. The fixture: a card drawn through
/// `GAIN_SHADER` at gain 1 and instance data 0, before the background, the
/// camera still, with the parameters and instance data rewritten unchanged
/// every frame. The oracle is case 4's
/// (`scenario_temporal_antialiasing`, `taa_holds_still_at_rest`): at rest
/// TAA blends in the current frame at 1 - 0.88 times Filament's alpha floor
/// 0.3 and clips its history at Filament's variance floor 0.1 (Frame item
/// 10 of `specs/sgl3d-architecture.md`; `sgl-post-fx` PROVENANCE.md,
/// "Motion-adaptive TAA"), so in its blend space v / (1 + v) a pixel moves
/// from one frame to the next by at most (0.036 + 0.964 x 0.1) of the
/// fixture's contrast, plus a half-float step for each frame's store, over
/// the two jitter cycles after the resolve settled. The region: the pixels
/// the card's outline crosses whose box grown by 2 px lies inside the
/// background, so each blends the card and the background alone.
#[test]
fn a_still_shader_surface_holds_taas_resting_bound() {
    let Some(gpu) = gpu() else { return };
    let (back, back_pose) = background();
    let card = Shape::card(Vec2::new(1.4, 0.9), 0.3, CARD);
    let card_pose = Pose::at(Vec3::new(0., 0., -DEPTH));
    let script = move |_| vec![back_pose, card_pose];
    let scenario = Scenario {
        shapes: vec![back.clone(), card.clone()],
        camera: &still,
        script: &script,
    };
    let region = scenario.edges(0, &[1]).and(&scenario.inside(0, 0, 2.));
    assert!(
        !region.is_empty(),
        "the card's outline lies over the background"
    );
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
    rig.add(&back, back_pose, 0);
    let shader = rig
        .scene
        .add_shader(ShaderSource {
            wgsl: GAIN_SHADER.into(),
            label: "gain".into(),
        })
        .unwrap_or_else(|error| panic!("{error}"));
    let params = [1f32, 0., 0., 0.];
    let (instance, materials) = rig.add_shaded(
        &card,
        card_pose,
        0,
        (shader, 0.),
        bytemuck::bytes_of(&params),
    );
    rig.look(still(0));
    let mut resolved = Vec::new();
    for frame in 0..SETTLE + 2 * JITTER_CYCLE {
        let queue = &rig.gpu.queue;
        for &material in &materials {
            rig.scene
                .set_shader_parameters(queue, material, bytemuck::bytes_of(&params))
                .unwrap();
        }
        rig.scene
            .set_instance_shader_data(queue, instance, [0.; 4])
            .unwrap();
        rig.render(frame);
        if frame + 1 >= SETTLE {
            resolved.push(rig.antialiased());
        }
    }
    let sdr = |v: f32| v / (1. + v);
    let [low, high] = rig.radiance_range();
    let feedback = (1. - 0.88) * 0.3;
    let bound = (feedback + (1. - feedback) * 0.1) * (sdr(high) - sdr(low)) + 2. * high * HALF_STEP;
    let stability = image::stability(&resolved, &region, sdr);
    eprintln!(
        "TAA at rest, a still shader card's outline: {stability:?} against the bound {bound}"
    );
    let mut failures = Failures::default();
    failures.check(stability.max_step <= bound, || {
        format!(
            "TAA at rest changed a pixel of a still shader card's outline by {} in its blend space, above the resting blend's {bound}",
            stability.max_step
        )
    });
    failures.bounded(&rig, "TAA at rest, a still shader card");
    failures.assert();
}

/// A change of an instance no motion vector describes (M-23), made at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    Removed,
    Added,
    Hidden,
    Shown,
    /// Given the model of a card half as wide.
    Swapped,
    /// A static card moved 1.2 m sideways.
    Moved,
}

/// M-23 (#892). Plausible defect: a change of an instance that no motion
/// vector describes (removed, added, hidden, shown, given another model, a
/// static one moved) re-arming TAA across the whole view, as the
/// rigid-change signal did, so every still edge in the picture shimmers for
/// a hold whenever anything spawns or despawns, though the change touched
/// only the pixels where the instance stood and stands, which its
/// footprints mark. The fixture, as `scenario_sprites`'s
/// `removing_a_masked_batch_keeps_the_rest_of_the_views_history`: a still
/// grating of sub-pixel strips filling the view 8 m away, the camera still,
/// a static card 4 m away to the left, changed once the resolve has
/// settled. The oracle is case 4's (`scenario_temporal_antialiasing`,
/// `taa_holds_still_at_rest`): at rest TAA blends in the current frame at
/// 1 - 0.88 times Filament's alpha floor 0.3 and clips its history at
/// Filament's variance floor 0.1, so in its blend space a pixel moves from
/// one frame to the next by at most (0.036 + 0.964 x 0.1) of the fixture's
/// contrast, plus a half-float step for each frame's store, from the frame
/// before the change through a jitter cycle after it. The region: the
/// pixels the strips' edges cross whose box grown by 8 px meets the card
/// in none of its places and shapes. Whether the pixels the card left show
/// no ghost is case 9's (`scenario_temporal_antialiasing`), where depth
/// cannot reject.
fn a_changed_instance_keeps_the_rest_of_the_views_history(change: Change) {
    use scenario::geometry::coverage;
    use scenario::temporal::{GRATING, STRIPS};
    let Some(gpu) = gpu() else { return };
    let far = scenario::temporal::VIEW.pixel_metres(8.);
    let grating = Shape::grating(
        Vec2::new(16., 9.),
        0.25,
        GRATING,
        STRIPS,
        0.4 * far,
        1.6 * far,
    );
    let grating_pose = Pose::at(Vec3::new(0., 0., -8.));
    let wide = Shape::card(Vec2::new(1.4, 0.9), 0., CARD);
    let narrow = Shape::card(Vec2::new(0.7, 0.9), 0., CARD);
    let [left, right] = [-1., 0.2].map(|x| Pose::at(Vec3::new(x, 0., -DEPTH)));
    let script = move |_| vec![grating_pose, left, left, right];
    let truth = Scenario {
        shapes: vec![grating.clone(), wide.clone(), narrow.clone(), wide.clone()],
        camera: &still,
        script: &script,
    };
    let polygons = truth.polygons(0);
    let region = truth
        .strip_edges(0, 0)
        .and(&image::Region::from_fn(VIEW.size, |p| {
            polygons[1..]
                .iter()
                .all(|card| coverage(&card[0], p, 8.) == 0.)
        }));
    assert!(!region.is_empty(), "strip edges away from the card");
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, SKY);
    rig.add(&grating, grating_pose, 0);
    let mut card = (change != Change::Added).then(|| rig.add_static(&wide, left, 0));
    if change == Change::Shown {
        rig.set_visible(card.unwrap(), false);
    }
    rig.admit(CARD);
    rig.look(still(0));
    let mut resolved = Vec::new();
    for frame in 0..SETTLE + JITTER_CYCLE {
        if frame == SETTLE {
            match change {
                Change::Removed => rig.remove(card.take().unwrap()),
                Change::Added => card = Some(rig.add_static(&wide, left, 0)),
                Change::Hidden => rig.set_visible(card.unwrap(), false),
                Change::Shown => rig.set_visible(card.unwrap(), true),
                Change::Swapped => rig.swap(card.unwrap(), &narrow),
                Change::Moved => rig.place(card.unwrap(), right),
            }
        }
        rig.render(frame);
        if frame + 1 >= SETTLE {
            resolved.push(rig.antialiased());
        }
    }
    let sdr = |v: f32| v / (1. + v);
    let [low, high] = rig.radiance_range();
    let feedback = (1. - 0.88) * 0.3;
    let bound = (feedback + (1. - feedback) * 0.1) * (sdr(high) - sdr(low)) + 2. * high * HALF_STEP;
    let stability = image::stability(&resolved, &region, sdr);
    eprintln!("TAA after a card {change:?}, away from it: {stability:?} against the bound {bound}");
    let mut failures = Failures::default();
    failures.check(stability.max_step <= bound, || {
        format!(
            "a card {change:?} changed a still strip's edge, away from it, by {} in TAA's blend space, above the resting blend's {bound}",
            stability.max_step
        )
    });
    failures.bounded(&rig, format!("a card {change:?}"));
    failures.assert();
}

mod a_changed_instance_keeps_the_rest_of_the_views_history {
    use super::Change;
    crate::scenario::temporal::case_tests!(a_changed_instance_keeps_the_rest_of_the_views_history;
        removed: (Change::Removed),
        added: (Change::Added),
        hidden: (Change::Hidden),
        shown: (Change::Shown),
        swapped: (Change::Swapped),
        moved: (Change::Moved),
    );
}
