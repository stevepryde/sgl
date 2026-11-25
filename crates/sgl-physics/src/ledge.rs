//! Finding a ledge a body could hang from and climb onto.

use sgl_core::{CanonicalWrite, StateHasher};

use crate::config::MoverConfig;
use crate::query::{move_and_collide, overlaps};
use crate::shape::{Collider, Shape};
use crate::slide::snap_to_ground;
use crate::space::{self, Vector};
use crate::world::{Unanswered, World};

/// A ledge [`find_ledge`] found: where the body would hang from it and
/// where it would stand on it. Hanging, climbing and their timing are the
/// game's; a game moves the body between these as it likes.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ledge<V> {
    /// The body's centre hanging from the edge: against the wall, its top
    /// level with the ledge's top (a `skin` above it).
    pub hang: V,
    /// The body's centre standing on the ledge: a `skin` above its top and
    /// a `skin` past its edge.
    pub stand: V,
    /// The id of the collider whose top the body would stand on.
    pub id: u64,
}

impl<V: Vector> Ledge<V> {
    /// The largest postcard encoding: two vectors and a `u64` varint (34
    /// bytes in 3D, 26 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 2 * space::postcard_bytes::<V>() + 10;
}

impl<V: Vector> CanonicalWrite for Ledge<V> {
    /// `hang`'s and `stand`'s bits in axis order, then `id`.
    fn canonical_write(&self, out: &mut StateHasher) {
        space::write(out, self.hang);
        space::write(out, self.stand);
        out.u64(self.id);
    }
}

/// The top edge of the wall `body` at `position` is pressed against, facing
/// `toward`, when that edge lies within `reach` of the body's top, above or
/// below, and the body has room to hang from it, rise beside it and stand
/// on it. Only colliders on a layer in `mask` take part.
///
/// "Pressed against" is within twice `skin` along `toward` across `up`, as
/// a slide into the wall leaves the body; a `skin` of zero finds nothing.
/// The wall blocks as it does for a move, so a one-way platform's side is
/// never a wall and its edge is never reported. The ledge is refused when
/// anything solid and rigid is where the body would hang, in the space it
/// rises through beside the wall, or in the space it would move into to
/// stand: an overhang above the ledge refuses it. A soft collider never
/// blocks it, and every collider is taken where it stands.
///
/// `Ok(None)` when there is no such ledge, or `toward` has no direction
/// across `up`; [`Unanswered`] when the world will not walk a query's
/// region.
///
/// # Panics
/// If `reach` is negative or not finite.
pub fn find_ledge<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    position: V,
    toward: V,
    world: &W,
    config: &MoverConfig<V>,
    reach: f32,
    mask: u32,
) -> Result<Option<Ledge<V>>, Unanswered> {
    assert!(
        reach.is_finite() && reach >= 0.0,
        "find_ledge: reach must be finite and non-negative, got {reach}"
    );
    let up = config.up();
    let skin = config.skin();
    let Some((ahead, _)) = space::direction(space::reject(toward, up)) else {
        return Ok(None);
    };
    let (_, wall) = move_and_collide(body, position, ahead * (2.0 * skin), world, mask, config)?;
    match wall {
        Some(wall) if !config.is_floor(wall.normal) && !config.is_ceiling(wall.normal) => {}
        _ => return Ok(None),
    }

    // Drop the body's footprint (the body flattened along the axis most
    // along `up`) from `reach` above its top to `reach` below it, where it
    // would stand: the first floor it meets is the ledge's top. It stops a
    // skin above it, where the body's bottom would be.
    let half = body.half_extents();
    let tall = extent(half, up);
    let across = ahead * (2.0 * (extent(half, ahead) + skin));
    let mut flat = half;
    flat[config.tie_order()[0]] = 0.0;
    let footprint = Shape::Box { half: flat };
    let from = position + up * (tall + reach) + across;
    let probe = config.with_snap_distance(2.0 * reach);
    let (dropped, top) = snap_to_ground(&footprint, from, world, mask, &probe)?;
    let Some(id) = top.last_floor_id.filter(|_| top.floor) else {
        return Ok(None);
    };

    // Hang with the body's top level with the footprint, rise by the body's
    // height beside the wall, and move over the ledge to stand.
    let hang = position + up * (reach + space::dot(dropped, up));
    let solid = |collider: &Collider<V>| collider.rigid_solid();
    if overlaps(body, hang, world, mask)?.iter().any(solid) {
        return Ok(None);
    }
    let rise = up * (2.0 * tall);
    if move_and_collide(body, hang, rise, world, mask, config)?
        .1
        .is_some()
    {
        return Ok(None);
    }
    let risen = hang + rise;
    if move_and_collide(body, risen, across, world, mask, config)?
        .1
        .is_some()
    {
        return Ok(None);
    }
    Ok(Some(Ledge {
        hang,
        stand: risen + across,
        id,
    }))
}

/// Half the size along the unit direction `d` of a box of half-extents
/// `half`.
fn extent<V: Vector>(half: V, d: V) -> f32 {
    (0..V::AXES).fold(0.0, |sum, axis| sum + (half[axis] * d[axis]).abs())
}
