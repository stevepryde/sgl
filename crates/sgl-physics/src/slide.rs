//! Move-and-slide for a kinematic body, and the ground snap.

use crate::config::MoverConfig;
use crate::contacts::Contacts;
use crate::query::{Hit, Rule, first_contact};
use crate::shape::Shape;
use crate::space::{self, Vector};
use crate::world::{Unanswered, World};

/// Most times a 2D [`slide`] slides: a floor and a wall.
pub const MAX_SLIDES_2D: usize = 2;

/// Most times a 3D [`slide`] slides: a floor and the two walls of a
/// corner.
pub const MAX_SLIDES_3D: usize = 3;

/// The slide budget of space `V`.
const fn max_slides<V: Vector>() -> usize {
    if V::AXES == 2 {
        MAX_SLIDES_2D
    } else {
        MAX_SLIDES_3D
    }
}

/// Sweeps `body` at `position` by `delta` through `world`, sliding along the
/// surfaces it meets, and returns the translation to apply to `position`
/// and what it touched. Only colliders on a layer in `mask` take part.
///
/// Each pass stops along the swept path a `skin` short of the earliest
/// contact (along its normal), then projects the motion left after the
/// contact time onto that surface and sweeps again; the backed-off portion is
/// discarded. A move slides at most a fixed number of times:
/// [`MAX_SLIDES_2D`] in 2D, [`MAX_SLIDES_3D`] in 3D. Sensors never block; a
/// one-way platform blocks only motion into its floor face.
///
/// A body that starts inside a solid collider (a spawn, a moved collider, or
/// rounding at a contact) may move out of it or along it, never deeper. A
/// body inside a one-way platform is stopped only by a fall onto it from
/// within `skin` (plus rounding) of its top face. Nothing pushes a body out:
/// the game keeps bodies clear itself.
///
/// Contacts at the same instant resolve to the lowest id, then the lowest
/// position, so the result never depends on the order the world visits its
/// colliders in. An exact tie between axes resolves to the axis most along
/// `up`, then y, x and z, so a body arriving exactly at a ledge's corner
/// lands. Every finite nonzero remainder is swept, however small. A
/// non-finite `delta` moves nothing.
///
/// [`Unanswered`] when the world will not walk a pass's region: the body
/// then moves nothing.
pub fn slide<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    position: V,
    delta: V,
    world: &W,
    mask: u32,
    config: &MoverConfig<V>,
) -> Result<(V, Contacts<V>), Unanswered> {
    let mut contacts = Contacts::asked(delta);
    if !space::is_finite(delta) {
        return Ok((V::ZERO, contacts));
    }
    let mut remaining = delta;
    let mut total = V::ZERO;
    for _ in 0..max_slides::<V>() {
        if remaining == V::ZERO {
            break;
        }
        let rule = Rule::Move(config);
        let Some(hit) = first_contact(body, position, total, remaining, world, mask, rule)? else {
            total = total + remaining;
            break;
        };
        let move_to = remaining * hit.t;
        total = total + back_off(remaining, &hit, config.skin());
        contacts.record(&hit, config);
        // Slide: keep only the part of the unspent motion along the surface.
        remaining = space::reject(remaining - move_to, hit.normal);
    }
    contacts.effective = total;

    // The blocked flags compare requested against achieved motion along `up`,
    // so they mean the same in a y-up and a y-down world.
    let desired_up = space::dot(delta, config.up());
    let effective_up = space::dot(total, config.up());
    contacts.down_blocked = desired_up < 0.0 && effective_up > desired_up + config.block_epsilon();
    contacts.up_blocked = desired_up > 0.0 && effective_up < desired_up - config.block_epsilon();
    Ok((total, contacts))
}

/// Probes `snap_distance` along `-up` from `position` and returns the
/// translation that puts `body` back on the ground, or zero when no floor
/// is within reach. Only colliders on a layer in `mask` take part.
///
/// Only a floor-classified face grounds the body, so a body rising through a
/// one-way platform is never pulled back onto its underside. After a move
/// that walked off a small lip, it keeps a body on the ground across seams
/// and down steps instead of dropping it into a one-step fall. It backs off
/// `skin` along its probe, and breaks ties between axes, as a move does.
pub fn snap_to_ground<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    position: V,
    world: &W,
    mask: u32,
    config: &MoverConfig<V>,
) -> Result<(V, Contacts<V>), Unanswered> {
    let probe = -config.up() * config.snap_distance();
    let mut contacts = Contacts::asked(probe);
    let rule = Rule::Snap(config);
    let Some(hit) = first_contact(body, position, V::ZERO, probe, world, mask, rule)? else {
        return Ok((V::ZERO, contacts));
    };
    let safe = back_off(probe, &hit, config.skin());
    contacts.effective = safe;
    contacts.record(&hit, config);
    Ok((safe, contacts))
}

/// Stops on the swept segment with a `skin` gap along the contact normal.
/// Shortening the whole motion keeps the body on the path the sweep cleared;
/// a contact within `skin` of the start yields zero, never reversed motion.
fn back_off<V: Vector>(delta: V, hit: &Hit<V>, skin: f32) -> V {
    // Every blocking contact has motion into its normal, embedded ones
    // included: they block only inward motion.
    let normal_motion = space::dot(delta, hit.normal).abs();
    delta * space::max(0.0, hit.t - skin / normal_motion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collider_set::ColliderSet;
    use crate::config::MoverSettings;
    use crate::shape::{ALL_LAYERS, Collider, ColliderKind};
    use sgl_core::math::{Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    const SKIN: f32 = 0.01;
    /// 2 px in a 32 px-per-unit world, the shadow-sp ground probe.
    const SNAP: f32 = 2.0 / 32.0;

    /// A box by centre and half-extents, with the plain geometry the tests'
    /// oracles read: no crate code.
    #[derive(Debug, Clone, Copy)]
    struct Aabb {
        center: Vec2,
        half: Vec2,
    }

    impl Aabb {
        fn from_min_max(min: Vec2, max: Vec2) -> Self {
            Self {
                center: (min + max) * 0.5,
                half: (max - min) * 0.5,
            }
        }

        fn translated(self, by: Vec2) -> Self {
            Self {
                center: self.center + by,
                ..self
            }
        }

        fn overlaps(&self, other: &Self) -> bool {
            let (a_min, a_max) = (self.center - self.half, self.center + self.half);
            let (b_min, b_max) = (other.center - other.half, other.center + other.half);
            a_min.x < b_max.x && a_max.x > b_min.x && a_min.y < b_max.y && a_max.y > b_min.y
        }

        fn shape(self) -> Shape<Vec2> {
            Shape::Box { half: self.half }
        }

        fn collider(self, kind: ColliderKind) -> Collider<Vec2> {
            Collider::new(0, self.shape(), self.center).with_kind(kind)
        }
    }

    fn aabb(cx: f32, cy: f32, hx: f32, hy: f32) -> Aabb {
        Aabb {
            center: Vec2::new(cx, cy),
            half: Vec2::new(hx, hy),
        }
    }

    /// Mover settings from positional values, in `MoverSettings`' order.
    fn settings<V>(
        up: V,
        skin: f32,
        snap_distance: f32,
        normal_threshold: f32,
        block_epsilon: f32,
    ) -> MoverConfig<V>
    where
        V: Vector,
    {
        MoverConfig::new(MoverSettings {
            up,
            skin,
            snap_distance,
            normal_threshold,
            block_epsilon,
        })
    }

    fn config(up: Vec2) -> MoverConfig<Vec2> {
        settings(up, SKIN, SNAP, 0.7, 0.0001)
    }

    fn set_with(colliders: &[(Aabb, ColliderKind)]) -> ColliderSet<Vec2> {
        let mut set = ColliderSet::new(1.0);
        for (id, &(aabb, kind)) in (0u64..).zip(colliders) {
            set.insert(Collider {
                id,
                ..aabb.collider(kind)
            });
        }
        set
    }

    fn mv(
        body: Aabb,
        delta: Vec2,
        set: &ColliderSet<Vec2>,
        config: &MoverConfig<Vec2>,
    ) -> (Vec2, Contacts<Vec2>) {
        slide(&body.shape(), body.center, delta, set, ALL_LAYERS, config)
            .expect("a set always answers")
    }

    fn snap(
        body: Aabb,
        set: &ColliderSet<Vec2>,
        config: &MoverConfig<Vec2>,
    ) -> (Vec2, Contacts<Vec2>) {
        snap_to_ground(&body.shape(), body.center, set, ALL_LAYERS, config)
            .expect("a set always answers")
    }

    /// Tiny inward motion from an overlap must not escape the embedded-body
    /// response when multiplying the offset and motion would underflow.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiny_embedded_motion_does_not_deepen_the_overlap() {
        for sign in [-1.0, 1.0] {
            let body = aabb(0.0, 0.0, 1e-24, 1e-24);
            let target = aabb(sign * 1e-24, 0.0, 1e-24, 1e-24);
            assert!(body.overlaps(&target));
            let mut set = ColliderSet::new(1e-24);
            set.insert(target.collider(ColliderKind::Solid));
            let config = settings(Vec2::Y, 0.0, 0.0, 0.7, 0.0);
            let (moved, contacts) = mv(body, Vec2::new(sign * 5e-25, 0.0), &set, &config);
            assert_eq!(moved, Vec2::ZERO, "inward motion deepened the overlap");
            assert!(contacts.wall);
        }
    }

    /// #707: sub-epsilon motion in empty space still reaches its destination,
    /// including at a jump apex with zero blocked-motion tolerance.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiny_unobstructed_motion_is_not_reported_blocked() {
        let body = aabb(0.0, 0.0, 0.5, 0.5);
        let set = ColliderSet::new(1.0);
        let config = settings(Vec2::Y, 0.0, 0.1, 0.7, 0.0);
        for delta in [Vec2::new(0.0, -1e-8), Vec2::new(5e-8, 1e-8)] {
            let (moved, contacts) = mv(body, delta, &set, &config);
            assert_eq!(moved, delta, "empty space must allow the entire motion");
            assert!(!contacts.down_blocked);
            assert!(!contacts.up_blocked);
        }
    }

    /// Tiny motion still sweeps against geometry rather than tunnelling
    /// through it merely because it is below `f32::EPSILON` in world units.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiny_motion_still_contacts_a_nearby_surface() {
        let body = aabb(0.0, 0.0, 1e-9, 1e-9);
        let floor = aabb(0.0, -5e-9, 1e-9, 1e-9);
        let mut set = ColliderSet::new(1e-8);
        set.insert(floor.collider(ColliderKind::Solid));
        let config = settings(Vec2::Y, 0.0, 0.0, 0.7, 0.0);
        let (moved, contacts) = mv(body, Vec2::new(0.0, -1e-8), &set, &config);
        assert!(contacts.floor);
        assert!(contacts.down_blocked);
        assert!((moved.y + 3e-9).abs() < 1e-15, "moved {moved:?}");
        assert!(!body.translated(moved).overlaps(&floor));
    }

    /// #706: a floor touched exactly at the end of the motion, on a grid-cell
    /// boundary, is still met, in either axis convention.
    #[wasm_bindgen_test(unsupported = test)]
    fn end_contact_on_a_cell_boundary_keeps_the_skin_gap() {
        for sign in [-1.0, 1.0] {
            let body = aabb(0.0, sign * 76.701_805, 0.5, 0.5);
            let floor = aabb(0.0, sign * 56.5, 50.0, 0.5);
            let delta = Vec2::new(0.0, -sign * 19.201_805);
            let config = settings(Vec2::new(0.0, sign), 0.01, 0.1, 0.7, 0.0);
            let set = set_with(&[(floor, ColliderKind::Solid)]);

            let (moved, contacts) = mv(body, delta, &set, &config);
            assert!(contacts.floor, "missed end contact with up={sign}");
            let gap = (body.translated(moved).center.y - floor.center.y).abs() - 1.0;
            assert!((gap - 0.01).abs() < 1e-5, "skin gap was {gap}");
        }
    }

    /// #249: the blocked flags compare requested against achieved motion
    /// along `up`; a move that lands exactly at its requested distance is
    /// not blocked, one that falls short by more than `block_epsilon` is.
    #[wasm_bindgen_test(unsupported = test)]
    fn blocked_flags_need_a_shortfall_beyond_the_epsilon() {
        let config = settings(Vec2::Y, 0.0, 0.1, 0.7, 0.0);
        // Floor top at y = -2, ceiling bottom at y = 2.
        let set = set_with(&[
            (aabb(0.0, -3.0, 1.0, 1.0), ColliderKind::Solid),
            (aabb(0.0, 3.0, 1.0, 1.0), ColliderKind::Solid),
        ]);
        let body = aabb(0.0, 0.0, 1.0, 1.0);
        let (moved, contacts) = mv(body, Vec2::new(0.0, -1.0), &set, &config);
        assert_eq!(moved, Vec2::new(0.0, -1.0));
        assert!(contacts.floor);
        assert!(!contacts.down_blocked, "landed exactly where asked");
        let (_, contacts) = mv(body, Vec2::new(0.0, -5.0), &set, &config);
        assert!(contacts.down_blocked);
        let (moved, contacts) = mv(body, Vec2::new(0.0, 1.0), &set, &config);
        assert_eq!(moved, Vec2::new(0.0, 1.0));
        assert!(contacts.ceiling);
        assert!(!contacts.up_blocked, "rose exactly as far as asked");
        let (_, contacts) = mv(body, Vec2::new(0.0, 5.0), &set, &config);
        assert!(contacts.up_blocked);
    }

    /// Falling onto a floor must stop the body exactly one skin above the
    /// surface and report both the floor contact and the blocked descent. A
    /// back-off on the wrong axis, or none at all, moves the resting height.
    #[wasm_bindgen_test(unsupported = test)]
    fn landing_rests_one_skin_above_the_floor() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid)]);
        let (effective, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -3.0),
            &set,
            &config(Vec2::Y),
        );

        // Floor top 0.5 + body half 0.5 + skin.
        let rest = 2.0 + effective.y;
        assert!((rest - (1.0 + SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.floor && contacts.down_blocked);
        assert!(!contacts.ceiling && !contacts.wall);
        assert_eq!(contacts.last_floor_normal, Vec2::Y);
        assert_eq!(contacts.last_floor_id, Some(0));
    }

    /// Jumping into a ceiling must stop the body and report the head-bump. A
    /// normal signed the wrong way would classify this as a floor and leave
    /// the caller thinking it had landed.
    #[wasm_bindgen_test(unsupported = test)]
    fn head_bump_stops_below_the_ceiling() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid)]);
        let (effective, contacts) = mv(
            aabb(0.0, -2.0, 0.5, 0.5),
            Vec2::new(0.0, 3.0),
            &set,
            &config(Vec2::Y),
        );

        let rest = -2.0 + effective.y;
        assert!((rest - (-1.0 - SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.ceiling && contacts.up_blocked);
        assert!(!contacts.floor);
        assert_eq!(contacts.last_ceiling_normal, Vec2::NEG_Y);
        assert_eq!(contacts.last_ceiling_id, Some(0));
    }

    /// Walking diagonally into a wall must stop the horizontal motion a skin
    /// short and spend the vertical motion remaining after the contact time.
    /// Without the second pass the vertical motion is thrown away.
    #[wasm_bindgen_test(unsupported = test)]
    fn wall_blocks_one_axis_and_the_slide_keeps_the_other() {
        let set = set_with(&[(aabb(2.0, 0.0, 0.5, 5.0), ColliderKind::Solid)]);
        let (effective, contacts) = mv(
            aabb(0.0, 0.0, 0.5, 0.5),
            Vec2::new(3.0, 1.0),
            &set,
            &config(Vec2::Y),
        );

        // Body right edge 0.5 to wall left edge 1.5, less the skin.
        assert!((effective.x - 0.99).abs() < 1e-4, "x was {}", effective.x);
        // Contact at (1, 1/3); backing off 0.01 in x also removes 1/300 in y.
        assert!(
            (effective.y - 299.0 / 300.0).abs() < 1e-4,
            "y was {}",
            effective.y
        );
        assert!(contacts.wall);
        assert_eq!(contacts.last_wall_id, Some(0));
        assert!(!contacts.floor && !contacts.ceiling);
    }

    /// #632: backing off a wall contact must stay on the tested diagonal.
    /// The ledge is cleared at x = 0.453125 before its top is reached at
    /// y = -0.46875. Backing off x alone instead embeds the body in it.
    #[wasm_bindgen_test(unsupported = test)]
    fn diagonal_skin_back_off_does_not_enter_a_ledge_the_sweep_cleared() {
        let body = aabb(0.0, 0.0, 0.5, 0.5);
        let wall = Aabb::from_min_max(Vec2::new(1.0, -2.0), Vec2::new(2.0, 2.0));
        let ledge = Aabb::from_min_max(Vec2::new(-2.0, -2.0), Vec2::new(-0.046_875, -0.96875));
        let set = set_with(&[(wall, ColliderKind::Solid), (ledge, ColliderKind::Solid)]);
        let config = settings(Vec2::Y, 0.125, 0.0, 0.7, 1e-4);

        let (effective, contacts) = mv(body, Vec2::new(1.0, -1.0), &set, &config);
        let moved = body.translated(effective);
        assert!(!moved.overlaps(&ledge), "ended inside ledge: {moved:?}");
        assert!(!moved.overlaps(&wall), "ended inside wall: {moved:?}");
        // Contact is at (0.5, -0.5), so leaving a 0.125 x-gap along the
        // diagonal stops at (0.375, -0.375). The slide then finds the ledge
        // only 0.09375 below the body's bottom, inside the skin: no advance.
        assert_eq!(effective, Vec2::new(0.375, -0.375));
        assert!(contacts.wall && contacts.floor);
    }

    /// #632: a diagonal ground probe must also remain on the swept segment.
    /// With diagonal `up`, the wall is floor-classified; backing off only x
    /// places the body below the ledge's top and inside its right edge.
    #[wasm_bindgen_test(unsupported = test)]
    fn diagonal_ground_snap_stays_clear_of_a_ledge_beside_its_probe() {
        let body = aabb(0.0, 0.0, 0.5, 0.5);
        let surface = Aabb::from_min_max(Vec2::new(1.0, -2.0), Vec2::new(2.0, 2.0));
        let ledge = Aabb::from_min_max(Vec2::new(-2.0, -2.0), Vec2::new(-0.046_875, -0.96875));
        let set = set_with(&[(surface, ColliderKind::Solid), (ledge, ColliderKind::Solid)]);
        let config = settings(Vec2::new(-1.0, 1.0), 0.125, 2.0f32.sqrt(), 0.7, 1e-4);

        let (snapped, contacts) = snap(body, &set, &config);
        let moved = body.translated(snapped);
        assert!(!moved.overlaps(&ledge), "snapped inside ledge: {moved:?}");
        assert!(snapped.abs_diff_eq(Vec2::new(0.375, -0.375), f32::EPSILON));
        assert!(contacts.floor);
    }

    /// Running down onto a floor keeps the horizontal travel after contact,
    /// discarding only the portion removed by skin back-off.
    #[wasm_bindgen_test(unsupported = test)]
    fn landing_while_running_keeps_the_horizontal_travel_after_contact() {
        let set = set_with(&[(aabb(0.0, 0.0, 10.0, 0.5), ColliderKind::Solid)]);
        let (effective, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(2.0, -3.0),
            &set,
            &config(Vec2::Y),
        );

        assert!(contacts.floor);
        // Contact at (2/3, -1); back-off removes 0.01 * 2/3 in x as well.
        assert!(
            (effective.x - 299.0 / 150.0).abs() < 1e-4,
            "x was {}",
            effective.x
        );
    }

    /// Among several colliders the earliest contact wins, not the first one
    /// visited. The lower floor is inserted first, so a search that keeps its
    /// first hit drops the body straight through the upper one.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_earliest_contact_wins_over_visit_order() {
        let set = set_with(&[
            (aabb(0.0, -4.0, 5.0, 0.5), ColliderKind::Solid),
            (aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid),
        ]);
        let (effective, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -6.0),
            &set,
            &config(Vec2::Y),
        );

        let rest = 2.0 + effective.y;
        assert!((rest - (1.0 + SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.floor);
        assert_eq!(contacts.last_floor_id, Some(1));
    }

    /// A contact reached exactly at the end of the delta must still be
    /// reported. Seeding the search with `t = 1` instead of "no hit yet"
    /// leaves the body flush against the floor with no contact recorded.
    #[wasm_bindgen_test(unsupported = test)]
    fn contact_exactly_at_the_end_of_the_delta_is_reported() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid)]);
        // Contact happens when the body centre reaches y = 1: exactly t = 1.
        let (effective, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -1.0),
            &set,
            &config(Vec2::Y),
        );

        assert!(contacts.floor && contacts.down_blocked);
        assert!(
            (effective.y - (-1.0 + SKIN)).abs() < 1e-4,
            "y was {}",
            effective.y
        );
    }

    /// Backing off must never push the body backwards. Moving 0.003 into a
    /// surface with a 0.01 skin leaves no room, so the move must collapse to
    /// zero rather than turning into motion away from the wall.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_skin_back_off_never_reverses_the_move() {
        let set = set_with(&[(aabb(0.0, 0.0, 0.5, 0.5), ColliderKind::Solid)]);
        let (effective, contacts) = mv(
            aabb(-1.003, 0.0, 0.5, 0.5),
            Vec2::new(0.006, 0.0),
            &set,
            &config(Vec2::Y),
        );

        assert!(contacts.wall);
        assert!(effective.x >= 0.0, "moved backwards: {}", effective.x);
        assert!(effective.x <= 0.003, "moved past contact: {}", effective.x);
    }

    /// Sensors must never block, however squarely the body runs into them.
    #[wasm_bindgen_test(unsupported = test)]
    fn sensors_do_not_block() {
        let set = set_with(&[
            (aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Sensor),
            (aabb(0.0, -1.5, 5.0, 0.5), ColliderKind::Sensor),
        ]);
        let (effective, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -3.0),
            &set,
            &config(Vec2::Y),
        );

        assert!((effective.y + 3.0).abs() < 1e-6, "y was {}", effective.y);
        assert!(!contacts.floor && !contacts.wall && !contacts.down_blocked);
    }

    /// A one-way platform blocks a landing on its top face and nothing else:
    /// a body rising through it must pass clean through.
    #[wasm_bindgen_test(unsupported = test)]
    fn one_way_blocks_only_a_landing_from_above() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderKind::OneWay)]);

        let (effective, contacts) = mv(
            aabb(0.0, -2.0, 0.5, 0.5),
            Vec2::new(0.0, 4.0),
            &set,
            &config(Vec2::Y),
        );
        assert!(!contacts.ceiling && !contacts.up_blocked);
        assert!((effective.y - 4.0).abs() < 1e-6, "y was {}", effective.y);

        let (_, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -3.0),
            &set,
            &config(Vec2::Y),
        );
        assert!(contacts.floor && contacts.down_blocked);
    }

    /// A one-way platform blocks motion into its floor face, not motion
    /// against a tilted `up`. Here `(1, -0.1)` descends onto the top face
    /// while rising along `up`, and `(-1, 0)` from within the landing margin
    /// runs along the face while descending along `up`.
    #[wasm_bindgen_test(unsupported = test)]
    fn one_way_platforms_with_a_tilted_up_block_motion_into_the_top_face() {
        let platform = aabb(0.0, 0.0, 5.0, 0.25);
        let set = set_with(&[(platform, ColliderKind::OneWay)]);
        let config = config(Vec2::new(0.2, 1.0));

        let above = aabb(0.0, 0.8, 0.5, 0.5);
        let (effective, contacts) = mv(above, Vec2::new(1.0, -0.1), &set, &config);
        assert!(
            !above.translated(effective).overlaps(&platform),
            "fell into the platform: {effective:?}"
        );
        assert!(contacts.floor);

        // Within the skin of the top face, as a resolved landing leaves it.
        let resting = aabb(0.0, 0.745, 0.5, 0.5);
        let (effective, _) = mv(resting, Vec2::new(-1.0, 0.0), &set, &config);
        assert_eq!(effective, Vec2::new(-1.0, 0.0));
    }

    /// A non-finite delta must resolve to no motion at all. Passing it
    /// through asks the world for an unbounded region and hands the caller a
    /// NaN position.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_finite_delta_moves_nothing() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid)]);
        for delta in [Vec2::new(f32::NAN, -1.0), Vec2::new(0.0, f32::NEG_INFINITY)] {
            let (effective, contacts) = mv(aabb(0.0, 2.0, 0.5, 0.5), delta, &set, &config(Vec2::Y));
            assert_eq!(effective, Vec2::ZERO);
            assert!(!contacts.floor && !contacts.down_blocked);
        }
    }

    /// The snap distance is a parameter, not a constant: the same 1 px gap
    /// must be bridged by a 2 px probe and missed by a half-pixel one.
    #[wasm_bindgen_test(unsupported = test)]
    fn snap_reaches_exactly_as_far_as_it_is_configured_to() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid)]);
        let body = aabb(0.0, 1.0 + 1.0 / 32.0, 0.5, 0.5);

        let (snapped, contacts) = snap(body, &set, &config(Vec2::Y));
        assert!(contacts.floor, "a 2 px probe must bridge a 1 px gap");
        assert!(snapped.y < 0.0, "snap pulls the body toward the floor");
        assert_eq!(contacts.last_floor_normal, Vec2::Y);

        let short = settings(Vec2::Y, SKIN, 0.5 / 32.0, 0.7, 0.0001);
        let (snapped, contacts) = snap(body, &set, &short);
        assert!(!contacts.floor, "a half-pixel probe must not reach");
        assert_eq!(snapped, Vec2::ZERO);
    }

    /// Snapping treats a one-way platform exactly like its landing face: it
    /// grounds a body hovering over the top, and grounds nothing when the
    /// body is under the platform.
    #[wasm_bindgen_test(unsupported = test)]
    fn snap_grounds_on_a_one_way_top_but_not_from_under_it() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderKind::OneWay)]);

        // Platform top 0.25 + body half 0.5, hovering a pixel above it.
        let (snapped, contacts) = snap(
            aabb(0.0, 0.75 + 1.0 / 32.0, 0.5, 0.5),
            &set,
            &config(Vec2::Y),
        );
        assert!(contacts.floor, "a one-way top still grounds a landing");
        assert!(snapped.y < 0.0);

        // Body top just below the platform: a downward probe meets no floor.
        let (snapped, contacts) = snap(aabb(0.0, -1.0, 0.5, 0.5), &set, &config(Vec2::Y));
        assert!(!contacts.floor);
        assert_eq!(snapped, Vec2::ZERO);
    }

    /// The same geometry mirrored into a y-down world must classify the same
    /// way. Anything that reads a raw `y` instead of projecting onto `up`
    /// swaps floor and ceiling here.
    #[wasm_bindgen_test(unsupported = test)]
    fn y_down_worlds_classify_floors_and_ceilings_the_same() {
        let config = config(Vec2::NEG_Y);
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderKind::Solid)]);

        // "Above" in a y-down world is smaller y; falling is +y.
        let (effective, contacts) = mv(
            aabb(0.0, -2.0, 0.5, 0.5),
            Vec2::new(0.0, 3.0),
            &set,
            &config,
        );
        let rest = -2.0 + effective.y;
        assert!((rest - (-1.0 - SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.floor && contacts.down_blocked);
        assert!(!contacts.ceiling && !contacts.up_blocked);
        assert_eq!(contacts.last_floor_normal, Vec2::NEG_Y);

        let (_, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -3.0),
            &set,
            &config,
        );
        assert!(contacts.ceiling && contacts.up_blocked);
        assert!(!contacts.floor && !contacts.down_blocked);
        assert_eq!(contacts.last_ceiling_normal, Vec2::Y);
    }

    /// One-way platforms and ground snapping must mirror into a y-down world
    /// too: the passable side is the one facing away from `up`, and the probe
    /// runs along `-up`.
    #[wasm_bindgen_test(unsupported = test)]
    fn y_down_worlds_mirror_one_way_and_snapping() {
        let config = config(Vec2::NEG_Y);
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderKind::OneWay)]);

        let (effective, contacts) = mv(
            aabb(0.0, 2.0, 0.5, 0.5),
            Vec2::new(0.0, -4.0),
            &set,
            &config,
        );
        assert!(!contacts.ceiling);
        assert!((effective.y + 4.0).abs() < 1e-6, "y was {}", effective.y);

        let (_, contacts) = mv(
            aabb(0.0, -2.0, 0.5, 0.5),
            Vec2::new(0.0, 3.0),
            &set,
            &config,
        );
        assert!(contacts.floor && contacts.down_blocked);

        // Resting a pixel above the platform's up-facing side, which is at
        // y = -0.25 in a y-down world.
        let (snapped, contacts) = snap(aabb(0.0, -0.75 - 1.0 / 32.0, 0.5, 0.5), &set, &config);
        assert!(contacts.floor, "the probe must run along -up, toward +y");
        assert!(snapped.y > 0.0, "snap pulls the body toward the platform");
    }

    /// #315: a body one float step inside a wall (rounding at a contact)
    /// cannot move deeper or through it; the motion along the wall survives.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_one_float_step_inside_a_wall_does_not_pass_through() {
        let wall = aabb(2.5, 0.0, 0.5, 5.0);
        let set = set_with(&[(wall, ColliderKind::Solid)]);
        // Right edge at the float just past the wall's left edge, x = 2.
        let body = aabb(2.0f32.next_up() - 0.5, 0.0, 0.5, 0.5);
        assert!(body.overlaps(&wall));
        let (effective, contacts) = mv(body, Vec2::new(3.0, 1.0), &set, &config(Vec2::Y));
        assert!(effective.x <= 0.0, "moved {} into the wall", effective.x);
        assert!((effective.y - 1.0).abs() < 1e-6, "slid {}", effective.y);
        assert!(contacts.wall);
    }

    /// #427: a body resting on a floor at coordinates that round (its bottom
    /// edge equals the floor's top edge in world coordinates) stays on it when
    /// it moves down, and snapping finds the floor.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_exactly_touching_a_floor_at_rounding_coordinates_stays_on_it() {
        let floor = aabb(1.090_340_4, 4.337_375_6, 50.0, 0.125);
        let set = set_with(&[(floor, ColliderKind::Solid)]);
        let body = aabb(1.090_340_4, 4.711_776_3, 0.4, 0.249_400_84);
        assert_eq!(
            (body.center.y - body.half.y).to_bits(),
            (floor.center.y + floor.half.y).to_bits()
        );
        let (effective, contacts) = mv(body, Vec2::new(0.0, -3.0), &set, &config(Vec2::Y));
        assert!(effective.y >= 0.0, "fell {}", effective.y);
        assert!(contacts.floor && contacts.down_blocked);
        let (_, contacts) = snap(body, &set, &config(Vec2::Y));
        assert!(contacts.floor, "snapping finds the floor it rests on");
    }

    /// #315: an embedded body is still free to move out of the collider.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_embedded_body_can_move_out() {
        let set = set_with(&[(aabb(2.5, 0.0, 0.5, 5.0), ColliderKind::Solid)]);
        let (effective, _) = mv(
            aabb(1.75, 0.0, 0.5, 0.5),
            Vec2::new(-1.0, 0.0),
            &set,
            &config(Vec2::Y),
        );
        assert_eq!(effective, Vec2::new(-1.0, 0.0));
    }

    /// #315: a body inside a one-way platform (part-way through a jump) that
    /// starts falling drops through it rather than landing inside it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_inside_a_one_way_platform_falls_through() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderKind::OneWay)]);
        let (effective, contacts) = mv(
            aabb(0.0, 0.5, 0.5, 0.5),
            Vec2::new(0.0, -2.0),
            &set,
            &config(Vec2::Y),
        );
        assert_eq!(effective, Vec2::new(0.0, -2.0));
        assert!(!contacts.floor);
    }

    /// #315: a body rounded one float step into a one-way platform's top
    /// lands on it instead of falling through.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_one_float_step_into_a_one_way_platform_lands() {
        let platform = aabb(0.0, 0.0, 5.0, 0.25);
        let set = set_with(&[(platform, ColliderKind::OneWay)]);
        let body = aabb(0.0, 0.75f32.next_down(), 0.5, 0.5);
        assert!(body.overlaps(&platform));
        let (effective, contacts) = mv(body, Vec2::new(0.0, -2.0), &set, &config(Vec2::Y));
        assert!(effective.y >= 0.0, "fell {}", effective.y);
        assert!(contacts.floor && contacts.down_blocked);
    }

    /// A layer mask leaves a wall out of the move: masked, the body walks
    /// through it; unmasked, it stops a skin short.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_layer_mask_leaves_a_wall_out_of_the_move() {
        let mut set = ColliderSet::new(1.0);
        set.insert(
            aabb(2.0, 0.0, 0.5, 5.0)
                .collider(ColliderKind::Solid)
                .with_layers(0b10),
        );
        let body = aabb(0.0, 0.0, 0.5, 0.5);
        let delta = Vec2::new(3.0, 0.0);
        let (through, _) = slide(
            &body.shape(),
            body.center,
            delta,
            &set,
            0b01,
            &config(Vec2::Y),
        )
        .unwrap();
        assert_eq!(through, delta);
        let (stopped, contacts) = slide(
            &body.shape(),
            body.center,
            delta,
            &set,
            0b11,
            &config(Vec2::Y),
        )
        .unwrap();
        assert!((stopped.x - 0.99).abs() < 1e-4, "x was {}", stopped.x);
        assert!(contacts.wall);
    }

    fn block3(id: u64, center: Vec3, half: Vec3) -> Collider<Vec3> {
        Collider::new(id, Shape::Box { half }, center)
    }

    /// Three slides in 3D, worked by hand with a skin of 0.25. A unit cube at
    /// the origin moves by (4, -4, 4) into a corner: a floor whose top is at
    /// y = -2.5, a wall whose face is at x = 3.5 and one at z = 3.875.
    /// Pass 1 meets the floor first, at t = 2/4 = 0.5 (the walls at 0.75
    /// and 0.84375); backing off 0.25 along y is 0.0625 of the motion, so
    /// the body moves (1.75, -1.75, 1.75) and slides on with (2, 0, 2).
    /// Pass 2 meets the x wall after 1.25 of 2 (t = 0.625, the z wall at
    /// 0.8125), backs off to t = 0.5, moves (1, 0, 1), and slides on with
    /// (0, 0, 0.75). Pass 3 meets the z wall after 0.625 of 0.75 and backs
    /// off to 0.375. The body ends at (2.75, -1.75, 3.125), a skin from each
    /// of the three faces.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_pushed_into_a_corner_slides_three_times() {
        let world = [
            block3(1, Vec3::new(0.0, -3.0, 0.0), Vec3::new(10.0, 0.5, 10.0)),
            block3(2, Vec3::new(4.0, 3.75, 0.0), Vec3::new(0.5, 6.25, 10.0)),
            block3(3, Vec3::new(0.0, 3.75, 4.375), Vec3::new(10.0, 6.25, 0.5)),
        ];
        let config = settings(Vec3::Y, 0.25, 0.0, 0.7, 1e-4);
        let body = Shape::Box {
            half: Vec3::splat(0.5),
        };
        let (moved, contacts) = slide(
            &body,
            Vec3::ZERO,
            Vec3::new(4.0, -4.0, 4.0),
            &world[..],
            ALL_LAYERS,
            &config,
        )
        .unwrap();
        assert_eq!((moved.x, moved.y), (2.75, -1.75));
        assert!((moved.z - 3.125).abs() < 1e-6, "z was {}", moved.z);
        assert!(contacts.floor && contacts.wall);
        assert_eq!(contacts.last_floor_id, Some(1));
        assert_eq!(contacts.last_wall_id, Some(3));
    }

    /// A tie between axes goes to the axis most along `up`. A cube falling
    /// by (4, -4, 0) toward a ledge whose corner is one unit off on x and on
    /// y enters both at t = 0.25, and lands (y-up). The same scene with y and
    /// z swapped, in a z-up world, must land as well: breaking the tie y,
    /// then x, then z would hand it to the x face, a wall.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_corner_arrival_lands_whichever_axis_is_up() {
        let swap = |v: Vec3| Vec3::new(v.x, v.z, v.y);
        let body = Shape::Box {
            half: Vec3::splat(0.5),
        };
        for up in [Vec3::Y, Vec3::Z] {
            let place = |v: Vec3| if up == Vec3::Z { swap(v) } else { v };
            let ledge = [block3(
                1,
                place(Vec3::new(2.0, -2.0, 0.0)),
                place(Vec3::new(0.5, 0.5, 5.0)),
            )];
            let config = settings(up, 0.01, 0.1, 0.7, 1e-4);
            let (_, contacts) = slide(
                &body,
                Vec3::ZERO,
                place(Vec3::new(4.0, -4.0, 0.0)),
                &ledge[..],
                ALL_LAYERS,
                &config,
            )
            .unwrap();
            assert!(contacts.floor, "up {up}: {contacts:?}");
            assert_eq!(contacts.last_floor_normal, up);
            assert!(!contacts.wall, "up {up}");
        }
    }
}
