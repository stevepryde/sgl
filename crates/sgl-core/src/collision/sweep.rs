//! Move-and-slide resolution of a kinematic body against a [`ColliderSet`].

use super::aabb::{Aabb, Hit, sweep_aabb};
use super::collider_set::ColliderSet;
use crate::math::Vec2;

/// Slide passes per [`move_and_collide`]. Two is enough to round an inside
/// corner (one surface, then the one the slide runs into); more passes only
/// let a body worm through geometry it should have stopped against.
const MAX_SLIDE_PASSES: u32 = 2;

/// The game's collision tunables, all in the caller's world units.
///
/// There is deliberately no `Default`: `skin` and `snap_distance` are lengths
/// in the game's own units and `up` is the game's axis convention, so a default
/// would silently bake one game's world into every other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionConfig {
    /// Unit vector naming the world's up axis. `Vec2::Y` for a y-up world,
    /// `Vec2::NEG_Y` for a y-down one. Every floor, ceiling, one-way, and
    /// ground-snap decision is made against this axis and nothing else.
    pub up: Vec2,
    /// Gap left between the body and a surface after a contact resolves, so
    /// the next frame's sweep starts clear of the surface instead of touching
    /// it. Small relative to the body. Finite and non-negative.
    pub skin: f32,
    /// How far [`snap_to_ground`] probes along `-up`. Finite and
    /// non-negative.
    pub snap_distance: f32,
    /// `normal · up` above which a face counts as a floor, and below whose
    /// negation it counts as a ceiling. `0.7` is roughly 45 degrees.
    pub normal_threshold: f32,
    /// Tolerance when comparing requested against achieved motion along `up`
    /// for the blocked flags. Must be well under `skin`, or a resolved contact
    /// reads as unblocked. Finite and non-negative.
    pub block_epsilon: f32,
}

impl CollisionConfig {
    /// Builds a config, normalizing `up`.
    ///
    /// # Panics
    /// If `up` is not a usable direction; if `normal_threshold` is outside
    /// `(0, 1)` — outside that range every face classifies the same way and
    /// floors, ceilings, and walls stop being distinguishable; or if `skin`,
    /// `snap_distance` or `block_epsilon` is negative or not finite — a
    /// negative skin backs a contact off *into* the surface. All are
    /// authoring errors.
    #[must_use]
    pub fn new(
        up: Vec2,
        skin: f32,
        snap_distance: f32,
        normal_threshold: f32,
        block_epsilon: f32,
    ) -> Self {
        assert!(
            up.is_finite() && up.length_squared() > 0.0,
            "CollisionConfig: up must be a finite non-zero direction, got {up}"
        );
        assert!(
            normal_threshold > 0.0 && normal_threshold < 1.0,
            "CollisionConfig: normal threshold must be in (0, 1), got {normal_threshold}"
        );
        for (name, value) in [
            ("skin", skin),
            ("snap distance", snap_distance),
            ("block epsilon", block_epsilon),
        ] {
            assert!(
                value.is_finite() && value >= 0.0,
                "CollisionConfig: {name} must be finite and non-negative, got {value}"
            );
        }
        Self {
            up: up.normalize(),
            skin,
            snap_distance,
            normal_threshold,
            block_epsilon,
        }
    }

    /// Whether `normal` belongs to a face the body can stand on.
    #[must_use]
    pub fn is_floor(&self, normal: Vec2) -> bool {
        normal.dot(self.up) > self.normal_threshold
    }

    /// Whether `normal` belongs to a face the body can bump its head on.
    #[must_use]
    pub fn is_ceiling(&self, normal: Vec2) -> bool {
        normal.dot(self.up) < -self.normal_threshold
    }
}

/// What a resolved move ran into.
///
/// Normals are stored whole rather than as a single component so the caller can
/// read them in its own axis convention.
// The flags are independent observations of one move — a body can touch a floor
// and a wall in the same step — so they do not collapse into a state enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Contacts {
    /// A floor face was contacted.
    pub floor: bool,
    /// A ceiling face was contacted.
    pub ceiling: bool,
    /// A side face was contacted.
    pub wall: bool,
    /// Normal of the most recent floor contact, or zero if there was none.
    pub last_floor_normal: Vec2,
    /// Normal of the most recent ceiling contact, or zero if there was none.
    pub last_ceiling_normal: Vec2,
    /// The delta the caller asked for.
    pub desired: Vec2,
    /// The delta actually applied.
    pub effective: Vec2,
    /// Motion against `up` was requested and not achieved — the body is
    /// resting on something.
    pub down_blocked: bool,
    /// Motion along `up` was requested and not achieved — a head-bump.
    pub up_blocked: bool,
}

/// Sweeps `body` by `delta` through `set`, sliding along the surfaces it meets,
/// and returns the translation to apply to `body.center` plus what it hit.
///
/// Up to two passes: stop a `skin` short of the earliest contact, project the
/// unspent motion onto that surface's tangent, and sweep again. Colliders
/// that are sensors, or not solid, are skipped; a one-way
/// platform blocks only a landing on its up-facing side (see
/// [`ColliderFlags::one_way`](super::ColliderFlags::one_way)).
///
/// A body that starts inside a solid collider (a spawn, a moved collider, or
/// float rounding at a contact) may move out of it or along it, never deeper:
/// motion toward the collider on its axis of least penetration is a contact
/// at the start, and only the slide along that face remains. A one-way
/// platform the body is inside blocks only a fall onto it from within `skin`
/// (plus float rounding) of its top face. `move_and_collide` does not push a
/// body out: the game keeps bodies clear itself (where it spawns them, and
/// carrying riders on moving platforms).
///
/// Ties between colliders contacted at the same instant go to the lowest
/// collider index, which [`ColliderSet::query`] makes deterministic.
///
/// A non-finite `delta` is refused outright — it has no meaningful destination,
/// and letting it through would ask the broadphase for an unbounded region.
#[must_use]
pub fn move_and_collide(
    body: Aabb,
    delta: Vec2,
    set: &ColliderSet,
    config: &CollisionConfig,
) -> (Vec2, Contacts) {
    let mut contacts = Contacts {
        desired: delta,
        ..Contacts::default()
    };
    if !delta.is_finite() {
        return (Vec2::ZERO, contacts);
    }

    let mut body = body;
    let mut remaining = delta;
    let mut total = Vec2::ZERO;

    for _ in 0..MAX_SLIDE_PASSES {
        if remaining.abs_diff_eq(Vec2::ZERO, f32::EPSILON) {
            break;
        }

        let Some(hit) = earliest_hit(&body, remaining, set, config, |_| true) else {
            // Nothing in the way: spend the rest of the motion.
            total += remaining;
            break;
        };

        let move_to = remaining * hit.t;
        let safe = back_off(move_to, hit.normal, config.skin);
        total += safe;
        body = body.translated(safe);
        classify(&mut contacts, hit.normal, config);

        // Slide: keep only the part of the unspent motion along the surface.
        let leftover = remaining - move_to;
        remaining = leftover - hit.normal * leftover.dot(hit.normal);
    }

    contacts.effective = total;

    // Blocked flags compare requested against achieved motion along `up`, so
    // they mean the same thing in a y-up and a y-down world.
    let desired_up = delta.dot(config.up);
    let effective_up = total.dot(config.up);
    contacts.down_blocked = desired_up < 0.0 && effective_up > desired_up + config.block_epsilon;
    contacts.up_blocked = desired_up > 0.0 && effective_up < desired_up - config.block_epsilon;

    (total, contacts)
}

/// Probes `snap_distance` along `-up` and returns the translation that puts the
/// body back on the ground, or zero if there is no ground within reach.
///
/// Only a floor-classified face grounds the body, so a body rising through a
/// one-way platform is never yanked back onto its underside. Useful after a
/// move that walked off a small lip: it keeps a body attached across seams and
/// downward steps instead of dropping it into a one-frame fall.
#[must_use]
pub fn snap_to_ground(body: Aabb, set: &ColliderSet, config: &CollisionConfig) -> (Vec2, Contacts) {
    let probe = -config.up * config.snap_distance;
    let mut contacts = Contacts {
        desired: probe,
        ..Contacts::default()
    };

    let Some(hit) = earliest_hit(&body, probe, set, config, |normal| config.is_floor(normal))
    else {
        return (Vec2::ZERO, contacts);
    };

    let safe = back_off(probe * hit.t, hit.normal, config.skin);
    contacts.effective = safe;
    classify(&mut contacts, hit.normal, config);
    (safe, contacts)
}

/// The first blocking contact of `body` moving by `delta`, ignoring faces that
/// `accept` rejects.
fn earliest_hit(
    body: &Aabb,
    delta: Vec2,
    set: &ColliderSet,
    config: &CollisionConfig,
    accept: impl Fn(Vec2) -> bool,
) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    for index in set.query(&swept_region(body, delta)) {
        let Some(collider) = set.get(index) else {
            continue;
        };
        if collider.flags.sensor || !collider.flags.solid {
            continue;
        }
        let hit = if body.overlaps(&collider.aabb) {
            if collider.flags.one_way {
                one_way_landing(body, delta, &collider.aabb, config)
            } else {
                embedded_hit(body, delta, &collider.aabb)
            }
        } else {
            sweep_aabb(body, delta, &collider.aabb)
        };
        let Some(hit) = hit else {
            continue;
        };
        if !accept(hit.normal) {
            continue;
        }
        if collider.flags.one_way && !one_way_blocks(hit.normal, delta, config) {
            continue;
        }
        if best.is_none_or(|previous| hit.t < previous.t) {
            best = Some(hit);
        }
    }
    best
}

/// The contact for a body that already overlaps `target` (a spawn, a moved
/// collider, or float rounding at a contact). It may move out of `target` or
/// along it, but not deeper: motion toward `target`'s center on the axis of
/// least penetration is a contact at `t = 0` on that axis's face, so the slide
/// keeps only the motion along it. Ties go to the horizontal face, as in
/// [`sweep_aabb`].
fn embedded_hit(body: &Aabb, delta: Vec2, target: &Aabb) -> Option<Hit> {
    let offset = body.center - target.center;
    let depth = body.half + target.half - offset.abs();
    let (along, motion, normal) = if depth.x < depth.y {
        (offset.x, delta.x, Vec2::new(offset.x.signum(), 0.0))
    } else {
        (offset.y, delta.y, Vec2::new(0.0, offset.y.signum()))
    };
    (along * motion < 0.0).then_some(Hit { t: 0.0, normal })
}

/// The contact for a body that already overlaps a one-way platform. A body
/// no deeper than `skin` (plus float rounding) below the platform's floor face
/// is resting on it, so falling lands it there, as Godot's one-way collision
/// margin does; deeper, it is passing through (part-way through a jump) and
/// the platform never blocks.
fn one_way_landing(
    body: &Aabb,
    delta: Vec2,
    target: &Aabb,
    config: &CollisionConfig,
) -> Option<Hit> {
    if delta.dot(config.up) >= 0.0 {
        return None;
    }
    let offset = body.center - target.center;
    let depth = body.half + target.half - offset.abs();
    let face = (target.center.abs() + target.half).max_element();
    let margin = config.skin + 4.0 * f32::EPSILON * face;
    [
        (depth.x, Vec2::new(offset.x.signum(), 0.0)),
        (depth.y, Vec2::new(0.0, offset.y.signum())),
    ]
    .into_iter()
    .find(|&(depth, normal)| depth <= margin && config.is_floor(normal))
    .map(|(_, normal)| Hit { t: 0.0, normal })
}

/// Whether a one-way platform blocks this motion: only when the body is coming
/// down onto the face that points along `up`.
fn one_way_blocks(normal: Vec2, motion: Vec2, config: &CollisionConfig) -> bool {
    config.is_floor(normal) && motion.dot(config.up) < 0.0
}

/// Records a contact normal as floor, ceiling, or wall.
fn classify(contacts: &mut Contacts, normal: Vec2, config: &CollisionConfig) {
    if config.is_floor(normal) {
        contacts.floor = true;
        contacts.last_floor_normal = normal;
    } else if config.is_ceiling(normal) {
        contacts.ceiling = true;
        contacts.last_ceiling_normal = normal;
    } else {
        contacts.wall = true;
    }
}

/// Shortens a move-to-contact by `skin` along the contact's entry axis, leaving
/// the body a hair clear of the surface.
fn back_off(move_to: Vec2, normal: Vec2, skin: f32) -> Vec2 {
    // Contact normals are axis-aligned, so the larger component names the axis
    // the body entered on.
    if normal.x.abs() > normal.y.abs() {
        Vec2::new(reduce_toward_zero(move_to.x, skin), move_to.y)
    } else {
        Vec2::new(move_to.x, reduce_toward_zero(move_to.y, skin))
    }
}

/// Moves `value` `amount` closer to zero, stopping at zero. Backing off is
/// never allowed to reverse the move: a contact within `skin` of the start
/// yields no motion rather than motion the caller never asked for.
fn reduce_toward_zero(value: f32, amount: f32) -> f32 {
    if value >= 0.0 {
        (value - amount).max(0.0)
    } else {
        (value + amount).min(0.0)
    }
}

/// The region covering `body` from the start to the end of `delta`.
fn swept_region(body: &Aabb, delta: Vec2) -> Aabb {
    let start_min = body.min();
    let start_max = body.max();
    Aabb::from_min_max(
        start_min.min(start_min + delta),
        start_max.max(start_max + delta),
    )
}

#[cfg(test)]
mod tests {
    use super::super::collider_set::ColliderFlags;
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: `Contacts::desired` records the requested motion for both
    /// entry points, and the floor/ceiling classification is strict at the
    /// configured threshold.
    #[wasm_bindgen_test(unsupported = test)]
    fn contacts_record_the_request_and_classification_is_strict() {
        let config = CollisionConfig::new(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
        let set = ColliderSet::new(1.0);
        let body = Aabb::new(Vec2::ZERO, Vec2::splat(0.5));
        let (_, contacts) = move_and_collide(body, Vec2::new(1.5, -2.0), &set, &config);
        assert_eq!(contacts.desired, Vec2::new(1.5, -2.0));
        let (_, contacts) = snap_to_ground(body, &set, &config);
        assert_eq!(contacts.desired, Vec2::new(0.0, -0.1));

        assert!(
            !config.is_floor(Vec2::new(0.3, 0.7)),
            "exactly at the threshold"
        );
        assert!(config.is_floor(Vec2::new(0.0, 0.7001)));
        assert!(
            !config.is_ceiling(Vec2::new(0.3, -0.7)),
            "exactly at the threshold"
        );
        assert!(config.is_ceiling(Vec2::new(0.0, -0.7001)));
    }

    /// #249: the blocked flags compare requested against achieved motion
    /// along `up`; a move that lands exactly at its requested distance is
    /// not blocked, one that falls short by more than `block_epsilon` is.
    #[wasm_bindgen_test(unsupported = test)]
    fn blocked_flags_need_a_shortfall_beyond_the_epsilon() {
        let config = CollisionConfig::new(Vec2::Y, 0.0, 0.1, 0.7, 0.0);
        let mut set = ColliderSet::new(1.0);
        // Floor top at y = -2, ceiling bottom at y = 2.
        set.insert(
            Aabb::new(Vec2::new(0.0, -3.0), Vec2::splat(1.0)),
            ColliderFlags::SOLID,
        );
        set.insert(
            Aabb::new(Vec2::new(0.0, 3.0), Vec2::splat(1.0)),
            ColliderFlags::SOLID,
        );
        let body = Aabb::new(Vec2::ZERO, Vec2::splat(1.0));
        let (moved, contacts) = move_and_collide(body, Vec2::new(0.0, -1.0), &set, &config);
        assert_eq!(moved, Vec2::new(0.0, -1.0));
        assert!(contacts.floor);
        assert!(!contacts.down_blocked, "landed exactly where asked");
        let (_, contacts) = move_and_collide(body, Vec2::new(0.0, -5.0), &set, &config);
        assert!(contacts.down_blocked);
        let (moved, contacts) = move_and_collide(body, Vec2::new(0.0, 1.0), &set, &config);
        assert_eq!(moved, Vec2::new(0.0, 1.0));
        assert!(contacts.ceiling);
        assert!(!contacts.up_blocked, "rose exactly as far as asked");
        let (_, contacts) = move_and_collide(body, Vec2::new(0.0, 5.0), &set, &config);
        assert!(contacts.up_blocked);
    }

    const SKIN: f32 = 0.01;
    /// 2 px in a 32 px-per-unit world, the shadow-sp ground probe.
    const SNAP: f32 = 2.0 / 32.0;

    fn aabb(cx: f32, cy: f32, hx: f32, hy: f32) -> Aabb {
        Aabb::new(Vec2::new(cx, cy), Vec2::new(hx, hy))
    }

    fn config(up: Vec2) -> CollisionConfig {
        CollisionConfig::new(up, SKIN, SNAP, 0.7, 0.0001)
    }

    fn set_with(colliders: &[(Aabb, ColliderFlags)]) -> ColliderSet {
        let mut set = ColliderSet::new(1.0);
        for &(aabb, flags) in colliders {
            set.insert(aabb, flags);
        }
        set
    }

    /// Falling onto a floor must stop the body exactly one skin above the
    /// surface and report both the floor contact and the blocked descent. A
    /// back-off on the wrong axis, or none at all, moves the resting height.
    #[wasm_bindgen_test(unsupported = test)]
    fn landing_rests_one_skin_above_the_floor() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID)]);
        let body = aabb(0.0, 2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, -3.0), &set, &config(Vec2::Y));

        // Floor top 0.5 + body half 0.5 + skin.
        let rest = 2.0 + effective.y;
        assert!((rest - (1.0 + SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.floor && contacts.down_blocked);
        assert!(!contacts.ceiling && !contacts.wall);
        assert_eq!(contacts.last_floor_normal, Vec2::Y);
    }

    /// Jumping into a ceiling must stop the body and report the head-bump. A
    /// normal signed the wrong way would classify this as a floor and leave the
    /// caller thinking it had landed.
    #[wasm_bindgen_test(unsupported = test)]
    fn head_bump_stops_below_the_ceiling() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID)]);
        let body = aabb(0.0, -2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, 3.0), &set, &config(Vec2::Y));

        let rest = -2.0 + effective.y;
        assert!((rest - (-1.0 - SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.ceiling && contacts.up_blocked);
        assert!(!contacts.floor);
        assert_eq!(contacts.last_ceiling_normal, Vec2::NEG_Y);
    }

    /// Walking diagonally into a wall must stop the horizontal motion a skin
    /// short and still spend the whole vertical motion on the slide pass.
    /// Without the second pass the vertical motion is thrown away.
    #[wasm_bindgen_test(unsupported = test)]
    fn wall_blocks_one_axis_and_the_slide_keeps_the_other() {
        let set = set_with(&[(aabb(2.0, 0.0, 0.5, 5.0), ColliderFlags::SOLID)]);
        let body = aabb(0.0, 0.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(3.0, 1.0), &set, &config(Vec2::Y));

        // Body right edge 0.5 to wall left edge 1.5, less the skin.
        assert!((effective.x - 0.99).abs() < 1e-4, "x was {}", effective.x);
        assert!((effective.y - 1.0).abs() < 1e-4, "y was {}", effective.y);
        assert!(contacts.wall);
        assert!(!contacts.floor && !contacts.ceiling);
    }

    /// Running down onto a floor must keep the full horizontal travel: the
    /// leftover motion is projected onto the floor rather than dropped.
    #[wasm_bindgen_test(unsupported = test)]
    fn landing_while_running_keeps_the_horizontal_travel() {
        let set = set_with(&[(aabb(0.0, 0.0, 10.0, 0.5), ColliderFlags::SOLID)]);
        let body = aabb(0.0, 2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(2.0, -3.0), &set, &config(Vec2::Y));

        assert!(contacts.floor);
        assert!((effective.x - 2.0).abs() < 1e-4, "x was {}", effective.x);
    }

    /// Among several colliders the *earliest* contact wins, not the first one
    /// queried. The lower floor is inserted first, so a search that keeps its
    /// first hit drops the body straight through the upper one.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_earliest_contact_wins_over_query_order() {
        let set = set_with(&[
            (aabb(0.0, -4.0, 5.0, 0.5), ColliderFlags::SOLID),
            (aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID),
        ]);
        let body = aabb(0.0, 2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, -6.0), &set, &config(Vec2::Y));

        let rest = 2.0 + effective.y;
        assert!((rest - (1.0 + SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.floor);
    }

    /// A contact reached exactly at the end of the delta must still be
    /// reported. Seeding the search with `t = 1` instead of "no hit yet" leaves
    /// the body flush against the floor with no contact recorded, so the caller
    /// spends a frame believing it is airborne.
    #[wasm_bindgen_test(unsupported = test)]
    fn contact_exactly_at_the_end_of_the_delta_is_reported() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID)]);
        // Contact happens when the body center reaches y = 1: exactly t = 1.
        let body = aabb(0.0, 2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, -1.0), &set, &config(Vec2::Y));

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
        let set = set_with(&[(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID)]);
        let body = aabb(-1.003, 0.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.006, 0.0), &set, &config(Vec2::Y));

        assert!(contacts.wall);
        assert!(effective.x >= 0.0, "moved backwards: {}", effective.x);
        assert!(effective.x <= 0.003, "moved past contact: {}", effective.x);
    }

    /// Sensors and non-solid colliders must never block, however squarely the
    /// body runs into them.
    #[wasm_bindgen_test(unsupported = test)]
    fn sensors_and_non_solid_colliders_do_not_block() {
        let set = set_with(&[
            (aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SENSOR),
            (
                aabb(0.0, -1.5, 5.0, 0.5),
                ColliderFlags {
                    solid: false,
                    one_way: false,
                    sensor: false,
                },
            ),
        ]);
        let body = aabb(0.0, 2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, -3.0), &set, &config(Vec2::Y));

        assert!((effective.y + 3.0).abs() < 1e-6, "y was {}", effective.y);
        assert!(!contacts.floor && !contacts.wall && !contacts.down_blocked);
    }

    /// A one-way platform blocks a landing on its top face and nothing else: a
    /// body rising through it must pass clean through.
    #[wasm_bindgen_test(unsupported = test)]
    fn one_way_blocks_only_a_landing_from_above() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderFlags::ONE_WAY)]);

        let rising = aabb(0.0, -2.0, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(rising, Vec2::new(0.0, 4.0), &set, &config(Vec2::Y));
        assert!(!contacts.ceiling && !contacts.up_blocked);
        assert!((effective.y - 4.0).abs() < 1e-6, "y was {}", effective.y);

        let falling = aabb(0.0, 2.0, 0.5, 0.5);
        let (_, contacts) = move_and_collide(falling, Vec2::new(0.0, -3.0), &set, &config(Vec2::Y));
        assert!(contacts.floor && contacts.down_blocked);
    }

    /// A non-finite delta must resolve to no motion at all. Passing it through
    /// asks the broadphase for an unbounded region and hands the caller a NaN
    /// position.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_finite_delta_moves_nothing() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID)]);
        let body = aabb(0.0, 2.0, 0.5, 0.5);
        for delta in [Vec2::new(f32::NAN, -1.0), Vec2::new(0.0, f32::NEG_INFINITY)] {
            let (effective, contacts) = move_and_collide(body, delta, &set, &config(Vec2::Y));
            assert_eq!(effective, Vec2::ZERO);
            assert!(!contacts.floor && !contacts.down_blocked);
        }
    }

    /// The snap distance is a parameter, not a constant: the same 1 px gap must
    /// be bridged by a 2 px probe and missed by a half-pixel one.
    #[wasm_bindgen_test(unsupported = test)]
    fn snap_reaches_exactly_as_far_as_it_is_configured_to() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID)]);
        let body = aabb(0.0, 1.0 + 1.0 / 32.0, 0.5, 0.5);

        let (snap, contacts) = snap_to_ground(body, &set, &config(Vec2::Y));
        assert!(contacts.floor, "a 2 px probe must bridge a 1 px gap");
        assert!(snap.y < 0.0, "snap pulls the body toward the floor");
        assert_eq!(contacts.last_floor_normal, Vec2::Y);

        let short = CollisionConfig::new(Vec2::Y, SKIN, 0.5 / 32.0, 0.7, 0.0001);
        let (snap, contacts) = snap_to_ground(body, &set, &short);
        assert!(!contacts.floor, "a half-pixel probe must not reach");
        assert_eq!(snap, Vec2::ZERO);
    }

    /// Snapping treats a one-way platform exactly like its landing face: it
    /// grounds a body hovering over the top, and grounds nothing when the body
    /// is under the platform. A one-way rule keyed to the wrong motion sign
    /// refuses the first case and sticks the body to the underside in the
    /// second.
    #[wasm_bindgen_test(unsupported = test)]
    fn snap_grounds_on_a_one_way_top_but_not_from_under_it() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderFlags::ONE_WAY)]);

        // Platform top 0.25 + body half 0.5, hovering a pixel above it.
        let above = aabb(0.0, 0.75 + 1.0 / 32.0, 0.5, 0.5);
        let (snap, contacts) = snap_to_ground(above, &set, &config(Vec2::Y));
        assert!(contacts.floor, "a one-way top still grounds a landing");
        assert!(snap.y < 0.0);

        // Body top just below the platform: a downward probe meets no floor.
        let below = aabb(0.0, -1.0, 0.5, 0.5);
        let (snap, contacts) = snap_to_ground(below, &set, &config(Vec2::Y));
        assert!(!contacts.floor);
        assert_eq!(snap, Vec2::ZERO);
    }

    /// The same geometry mirrored into a y-down world must classify the same
    /// way. Anything that reads a raw `y` instead of projecting onto `up`
    /// swaps floor and ceiling here.
    #[wasm_bindgen_test(unsupported = test)]
    fn y_down_worlds_classify_floors_and_ceilings_the_same() {
        let config = config(Vec2::NEG_Y);
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.5), ColliderFlags::SOLID)]);

        // "Above" in a y-down world is smaller y; falling is +y.
        let above = aabb(0.0, -2.0, 0.5, 0.5);
        let (effective, contacts) = move_and_collide(above, Vec2::new(0.0, 3.0), &set, &config);
        let rest = -2.0 + effective.y;
        assert!((rest - (-1.0 - SKIN)).abs() < 1e-4, "rested at {rest}");
        assert!(contacts.floor && contacts.down_blocked);
        assert!(!contacts.ceiling && !contacts.up_blocked);
        assert_eq!(contacts.last_floor_normal, Vec2::NEG_Y);

        let below = aabb(0.0, 2.0, 0.5, 0.5);
        let (_, contacts) = move_and_collide(below, Vec2::new(0.0, -3.0), &set, &config);
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
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderFlags::ONE_WAY)]);

        let rising = aabb(0.0, 2.0, 0.5, 0.5);
        let (effective, contacts) = move_and_collide(rising, Vec2::new(0.0, -4.0), &set, &config);
        assert!(!contacts.ceiling);
        assert!((effective.y + 4.0).abs() < 1e-6, "y was {}", effective.y);

        let falling = aabb(0.0, -2.0, 0.5, 0.5);
        let (_, contacts) = move_and_collide(falling, Vec2::new(0.0, 3.0), &set, &config);
        assert!(contacts.floor && contacts.down_blocked);

        // Resting a pixel above the platform's up-facing side, which is at
        // y = -0.25 in a y-down world.
        let hovering = aabb(0.0, -0.75 - 1.0 / 32.0, 0.5, 0.5);
        let (snap, contacts) = snap_to_ground(hovering, &set, &config);
        assert!(contacts.floor, "the probe must run along -up, toward +y");
        assert!(snap.y > 0.0, "snap pulls the body toward the platform");
    }

    /// #315: a body one float step inside a wall (rounding at a contact)
    /// cannot move deeper or through it; the motion along the wall survives.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_one_float_step_inside_a_wall_does_not_pass_through() {
        let wall = aabb(2.5, 0.0, 0.5, 5.0);
        let set = set_with(&[(wall, ColliderFlags::SOLID)]);
        // Right edge at the float just past the wall's left edge, x = 2.
        let body = aabb(2.0f32.next_up() - 0.5, 0.0, 0.5, 0.5);
        assert!(body.overlaps(&wall));
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(3.0, 1.0), &set, &config(Vec2::Y));
        assert!(effective.x <= 0.0, "moved {} into the wall", effective.x);
        assert!((effective.y - 1.0).abs() < 1e-6, "slid {}", effective.y);
        assert!(contacts.wall);
    }

    /// #315: an embedded body is still free to move out of the collider.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_embedded_body_can_move_out() {
        let wall = aabb(2.5, 0.0, 0.5, 5.0);
        let set = set_with(&[(wall, ColliderFlags::SOLID)]);
        let body = aabb(1.75, 0.0, 0.5, 0.5);
        let (effective, _) = move_and_collide(body, Vec2::new(-1.0, 0.0), &set, &config(Vec2::Y));
        assert_eq!(effective, Vec2::new(-1.0, 0.0));
    }

    /// #315: a body inside a one-way platform (part-way through a jump) that
    /// starts falling drops through it rather than landing inside it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_inside_a_one_way_platform_falls_through() {
        let set = set_with(&[(aabb(0.0, 0.0, 5.0, 0.25), ColliderFlags::ONE_WAY)]);
        let body = aabb(0.0, 0.5, 0.5, 0.5);
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, -2.0), &set, &config(Vec2::Y));
        assert_eq!(effective, Vec2::new(0.0, -2.0));
        assert!(!contacts.floor);
    }

    /// #315: a body rounded one float step into a one-way platform's top
    /// lands on it instead of falling through.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_one_float_step_into_a_one_way_platform_lands() {
        let platform = aabb(0.0, 0.0, 5.0, 0.25);
        let set = set_with(&[(platform, ColliderFlags::ONE_WAY)]);
        let body = aabb(0.0, 0.75f32.next_down(), 0.5, 0.5);
        assert!(body.overlaps(&platform));
        let (effective, contacts) =
            move_and_collide(body, Vec2::new(0.0, -2.0), &set, &config(Vec2::Y));
        assert!(effective.y >= 0.0, "fell {}", effective.y);
        assert!(contacts.floor && contacts.down_blocked);
    }

    /// #315: negative or non-finite lengths are authoring errors.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_config_rejects_negative_and_non_finite_lengths() {
        for bad in [-0.01, f32::NAN, f32::INFINITY] {
            for (skin, snap, epsilon) in [(bad, SNAP, 1e-4), (SKIN, bad, 1e-4), (SKIN, SNAP, bad)] {
                let built = std::panic::catch_unwind(|| {
                    CollisionConfig::new(Vec2::Y, skin, snap, 0.7, epsilon)
                });
                assert!(built.is_err(), "accepted {skin}, {snap}, {epsilon}");
            }
        }
    }
}
