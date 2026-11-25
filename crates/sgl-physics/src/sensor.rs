//! Triggers: which sensors a body is in, and which it entered and left
//! between two steps. Pure functions over lists the game keeps, so a
//! trigger's state rolls back with the rest of the game's.

use crate::query::overlaps;
use crate::shape::{ColliderKind, Shape};
use crate::space::Vector;
use crate::world::{Unanswered, World};

/// The ids of the sensors on a layer in `mask` that `shape` at `position`
/// overlaps, ascending, each once however many sensors share it: what a
/// game keeps for each body from one step to the next and hands to
/// [`sensor_events`]. Touching is not overlap ([`overlaps`]). Every sensor
/// is taken where it stands.
///
/// A list rather than a fixed array: a body may stand in any number of
/// sensors, and a list never drops one to fit.
///
/// [`Unanswered`] when the world will not walk the shape's region.
pub fn sensors_at<V: Vector, W: World<V> + ?Sized>(
    shape: &Shape<V>,
    position: V,
    world: &W,
    mask: u32,
) -> Result<Vec<u64>, Unanswered> {
    let mut ids: Vec<u64> = overlaps(shape, position, world, mask)?
        .iter()
        .filter(|collider| collider.kind == ColliderKind::Sensor)
        .map(|collider| collider.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// The sensors a body entered and left between two steps, given the ids it
/// was in before and is in after (as [`sensors_at`] lists them; any order,
/// repeats ignored): `(entered, left)`, each ascending. A sensor entered and
/// left in consecutive steps is reported entered once and left once; one
/// the body stays in is reported neither. `Box2D`'s sensor begin and end
/// events, over lists the game holds.
#[must_use]
pub fn sensor_events(before: &[u64], after: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let sorted = |ids: &[u64]| {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let (before, after) = (sorted(before), sorted(after));
    let entered = after
        .iter()
        .copied()
        .filter(|id| before.binary_search(id).is_err())
        .collect();
    let left = before
        .iter()
        .copied()
        .filter(|id| after.binary_search(id).is_err())
        .collect();
    (entered, left)
}
