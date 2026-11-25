//! The ground under a point: what a wheel, a hovering racer, a boat or a
//! thing placed on terrain stands on.

use sgl_core::math::Vec3;
use sgl_core::{CanonicalWrite, StateHasher};

use crate::query::ray;
use crate::space;
use crate::world::{Unanswered, World};

/// The surface [`ground_under`] found.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ground {
    /// The y the downward ray met the surface at, in world units.
    pub height: f32,
    /// The surface's unit normal there, pointing up (its y is positive),
    /// whichever way a triangle is wound.
    pub normal: Vec3,
    /// The id of the collider met: a triangle's or a cell's surface kind.
    pub id: u64,
}

impl Ground {
    /// The largest postcard encoding: an `f32`, a `Vec3` and a `u64`
    /// varint (26 bytes).
    pub const MAX_POSTCARD_BYTES: usize = 4 + 12 + 10;
}

impl CanonicalWrite for Ground {
    /// `height`'s bits, the normal's bits in axis order, then `id`.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u32(self.height.to_bits());
        space::write(out, self.normal);
        out.u64(self.id);
    }
}

/// The highest surface on a layer in `mask` straight down (along `-y`;
/// SGL3D is y-up) through `point`, from `max_height` above it to `depth`
/// below it: the ground a wheel at `point` stands on, which a deck higher
/// than `max_height` above it, the bridge over a road say, is not.
///
/// It is a [`ray`] from `max_height` above `point` down by `max_height +
/// depth`, and asks any `World<Vec3>`: a [`TriangleSet`](crate::TriangleSet),
/// a [`HeightField`](crate::HeightField), a set of boxes and ramps, or a
/// pair of them. So it meets what the ray meets, exactly: the height is the
/// surface's own, to a few units in the last place of the coordinates
/// involved, and a triangle's normal is its own plane's, so coplanar
/// triangles give one normal across their seam and a wheel rolling over it
/// neither jumps nor rocks. Only a 3D convex hull of four or more points is
/// met through the kernel, a slop short (`specs/physics.md`, 15). A surface
/// the ray starts inside is passed, as a ray passes it, and a surface
/// exactly at `max_height` above `point` is met.
///
/// `Ok(None)` when nothing is there, or when `point`, `max_height` or
/// `depth` is not finite, either length is negative, or both are zero;
/// [`Unanswered`] when the world will not walk the ray.
///
/// ```
/// use sgl_core::math::Vec3;
/// use sgl_physics::{ALL_LAYERS, TriangleSet, ground_under};
///
/// // A road at y = 0 and a bridge deck at y = 3 over it, as triangles.
/// let positions = [
///     Vec3::new(-10.0, 0.0, -2.0), Vec3::new(10.0, 0.0, -2.0),
///     Vec3::new(10.0, 0.0, 2.0), Vec3::new(-10.0, 0.0, 2.0),
///     Vec3::new(-2.0, 3.0, -3.0), Vec3::new(2.0, 3.0, -3.0),
///     Vec3::new(2.0, 3.0, 3.0), Vec3::new(-2.0, 3.0, 3.0),
/// ];
/// let indices = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
/// let (road, bridge) = (1, 2);
/// let world = TriangleSet::new(&positions, &indices, &[road, road, bridge, bridge])?;
///
/// // A wheel under the bridge, reaching half a metre up, is on the road.
/// let wheel = Vec3::new(0.5, 0.2, 0.5);
/// let ground = ground_under(wheel, 0.5, 1.0, &world, ALL_LAYERS)?.expect("the road");
/// assert_eq!(ground.id, road);
/// // The road's own height, to the rounding of the coordinates involved,
/// // and its own normal.
/// assert!(ground.height.abs() < 1e-6);
/// assert_eq!(ground.normal, Vec3::Y);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn ground_under<W: World<Vec3> + ?Sized>(
    point: Vec3,
    max_height: f32,
    depth: f32,
    world: &W,
    mask: u32,
) -> Result<Option<Ground>, Unanswered> {
    let sound = space::is_finite(point)
        && max_height.is_finite()
        && depth.is_finite()
        && max_height >= 0.0
        && depth >= 0.0;
    if !sound {
        return Ok(None);
    }
    let from = Vec3::new(point.x, point.y + max_height, point.z);
    let reach = max_height + depth;
    let found = ray(from, Vec3::new(0.0, -reach, 0.0), world, mask)?;
    Ok(found.map(|hit| Ground {
        height: from.y - reach * hit.t,
        normal: hit.normal,
        id: hit.id,
    }))
}
