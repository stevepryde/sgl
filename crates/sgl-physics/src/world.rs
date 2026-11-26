//! The one trait every world answers through, and the worlds that need no
//! storage of their own: a slice of colliders and a pair of worlds.

use core::fmt;

use crate::shape::Collider;
use crate::space::{self, Vector};

/// A query's region was larger than the world will walk, so the query was
/// not answered. The caller moves nothing; what it does next (wait for a
/// region to load, shorten the motion) is its own policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unanswered;

impl fmt::Display for Unanswered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the query's region is larger than the world walks for one query")
    }
}

impl std::error::Error for Unanswered {}

/// What a query asks a world: the colliders near a region.
///
/// Implemented for [`ColliderSet`](crate::ColliderSet) (a broadphase grid),
/// a slice of colliders (every collider, every time), [`CellWorld`](crate::CellWorld)
/// (a tile or voxel grid the game answers cell by cell), a pair `(A, B)` of
/// worlds (both), and a reference to any of them. Every query takes
/// `&W where W: World<V> + ?Sized`.
///
/// A world must offer every collider whose box, grown on each axis by the
/// collider's own rounding allowance `4ε(|position| + half)` (`ε` is
/// `f32::EPSILON`), meets the region. A query computes faces relative to the
/// body, where a face may lie that far from where it lies in world
/// coordinates, so a collider reached exactly at the end of a motion is
/// offered whichever coordinates the world reads.
///
/// Results never depend on the order a world visits its colliders in (the
/// crate breaks every tie by id and position), or on how many times it
/// visits one, so a world may visit in any order, may visit a collider more
/// than once, and may visit one that is not near the region at all.
pub trait World<V: Vector> {
    /// Calls `visit` with every collider that may overlap the closed box
    /// `min..=max` (it may visit more), or returns [`Unanswered`] when the
    /// region is more than the world will walk; the query then ignores
    /// whatever was visited.
    fn colliders_near(
        &self,
        min: V,
        max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered>;

    /// Calls `visit` with every collider that may overlap the closed box
    /// `min..=max` swept by `motion`, or returns [`Unanswered`] when the
    /// sweep is more than the world will walk. A moving query ([`cast`],
    /// [`ray`], [`ray_spans`], [`slide`], [`snap_to_ground`],
    /// [`move_and_collide`]) asks through it.
    ///
    /// The default asks [`Self::colliders_near`] for the box enclosing the
    /// start and the end. A world whose cost grows with the region, such as
    /// a [`CellWorld`](crate::CellWorld), walks only what lies along the
    /// motion.
    ///
    /// [`cast`]: crate::cast
    /// [`ray`]: crate::ray
    /// [`ray_spans`]: crate::ray_spans
    /// [`slide`]: crate::slide
    /// [`snap_to_ground`]: crate::snap_to_ground
    /// [`move_and_collide`]: crate::move_and_collide
    fn colliders_along(
        &self,
        min: V,
        max: V,
        motion: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.colliders_near(
            space::min_each(min, min + motion),
            space::max_each(max, max + motion),
            visit,
        )
    }
}

impl<V: Vector, W: World<V> + ?Sized> World<V> for &W {
    fn colliders_near(
        &self,
        min: V,
        max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        (**self).colliders_near(min, max, visit)
    }

    fn colliders_along(
        &self,
        min: V,
        max: V,
        motion: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        (**self).colliders_along(min, max, motion, visit)
    }
}

/// Every collider in the slice, whatever the region: no broadphase. Suits a
/// handful of colliders, or a list the game has already narrowed.
impl<V: Vector> World<V> for [Collider<V>] {
    fn colliders_near(
        &self,
        _min: V,
        _max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.iter().for_each(visit);
        Ok(())
    }
}

/// Both worlds: a voxel grid and the moving things in it, say. Unanswered
/// when either is.
impl<V: Vector, A: World<V>, B: World<V>> World<V> for (A, B) {
    fn colliders_near(
        &self,
        min: V,
        max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.0.colliders_near(min, max, visit)?;
        self.1.colliders_near(min, max, visit)
    }

    fn colliders_along(
        &self,
        min: V,
        max: V,
        motion: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.0.colliders_along(min, max, motion, visit)?;
        self.1.colliders_along(min, max, motion, visit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MoverConfig, MoverSettings};
    use crate::query::{cast, overlaps, ray};
    use crate::shape::Shape;
    use crate::slide::{slide, snap_to_ground};
    use crate::{CellColliders, CellWorld};
    use sgl_core::math::{IVec2, Vec2};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A query a world will not walk answers `Unanswered`, never a partial
    /// result: each of the five queries, through a cell world asked about a
    /// body 200 cells across (40 000 cells, over the cap), alone and as the
    /// second half of a pair whose first half holds a wall the body would
    /// otherwise meet.
    #[wasm_bindgen_test(unsupported = test)]
    fn every_query_through_a_world_that_will_not_walk_is_unanswered() {
        let wall = [Collider::new(
            1,
            Shape::Box {
                half: Vec2::splat(0.5),
            },
            Vec2::new(150.0, 0.0),
        )];
        let cells = CellWorld::new(1.0, |_: IVec2, _: &mut CellColliders<'_, Vec2>| {});
        let pair = (&wall[..], &cells);
        let config = MoverConfig::new(MoverSettings {
            up: Vec2::Y,
            skin: 0.01,
            snap_distance: 0.1,
            normal_threshold: 0.7,
            block_epsilon: 1e-4,
        });
        let body = Shape::Box {
            half: Vec2::splat(100.0),
        };
        let motion = Vec2::new(60.0, 0.0);
        let worlds: [&dyn World<Vec2>; 2] = [&cells, &pair];
        for world in worlds {
            let moved = slide(&body, Vec2::ZERO, motion, world, u32::MAX, &config);
            assert_eq!(moved, Err(Unanswered));
            let snapped = snap_to_ground(&body, Vec2::ZERO, world, u32::MAX, &config);
            assert_eq!(snapped, Err(Unanswered));
            assert_eq!(
                cast(&body, Vec2::ZERO, motion, world, u32::MAX),
                Err(Unanswered)
            );
            assert_eq!(
                overlaps(&body, Vec2::ZERO, world, u32::MAX),
                Err(Unanswered)
            );
            // A ray of 5000 cells walks more than the cap along its path.
            let far = Vec2::new(5000.0, 0.0);
            assert_eq!(ray(Vec2::ZERO, far, world, u32::MAX), Err(Unanswered));
        }
    }
}
