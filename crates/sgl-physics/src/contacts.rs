//! What a move ran into.

use sgl_core::{CanonicalWrite, StateHasher};

use crate::config::MoverConfig;
use crate::query::Hit;
use crate::space::{self, Vector};

/// What a resolved move ran into.
///
/// Normals are stored whole rather than as one component, so the game reads
/// them in its own axis convention. A game may keep the last step's contacts
/// as part of its state (whether the body is grounded, what it stands on):
/// they are plain data.
// The flags are independent observations of one move (a body can touch a
// floor and a wall in the same step), so they do not collapse into an enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Contacts<V> {
    /// A floor face was contacted.
    pub floor: bool,
    /// A ceiling face was contacted.
    pub ceiling: bool,
    /// A side face was contacted.
    pub wall: bool,
    /// Normal of the most recent floor contact, or zero if there was none.
    pub last_floor_normal: V,
    /// Normal of the most recent ceiling contact, or zero if there was none.
    pub last_ceiling_normal: V,
    /// Normal of the most recent wall contact, or zero if there was none.
    pub last_wall_normal: V,
    /// Id of the collider of the most recent floor contact.
    pub last_floor_id: Option<u64>,
    /// Id of the collider of the most recent wall contact.
    pub last_wall_id: Option<u64>,
    /// Id of the collider of the most recent ceiling contact.
    pub last_ceiling_id: Option<u64>,
    /// The motion the caller asked for.
    pub desired: V,
    /// The motion actually applied.
    pub effective: V,
    /// Motion against `up` was asked for and not achieved: the body is
    /// resting on something.
    pub down_blocked: bool,
    /// Motion along `up` was asked for and not achieved: a head-bump.
    pub up_blocked: bool,
    /// [`move_and_slide`](crate::move_and_slide) climbed a step.
    pub stepped: bool,
    /// [`move_and_slide`](crate::move_and_slide)'s ledge guard shortened the
    /// move to keep the body over a floor.
    pub ledge_guarded: bool,
    /// [`move_and_slide`](crate::move_and_slide) moved the body out of solid
    /// geometry it started inside.
    pub pushed_out: bool,
    /// [`move_and_slide`](crate::move_and_slide) found the body inside solid
    /// geometry it could not leave within its own size, and left it where it
    /// was.
    pub stuck: bool,
    /// [`move_and_slide`](crate::move_and_slide) found the body inside a
    /// moving collider that could not push it free (it is held against
    /// something else), pressed it as far as it goes, and stopped there.
    pub crushed: bool,
    /// The id of the moving collider [`move_and_slide`](crate::move_and_slide)
    /// carried the body on: its floor at the start of the step.
    pub carried_by: Option<u64>,
    /// The id of the collider that pushed the body: the moving collider
    /// [`move_and_slide`](crate::move_and_slide) pushed it out of, or else,
    /// when soft colliders moved it, the one along whose push it moved
    /// furthest; `None` when their pushes cancel or are blocked.
    pub pushed_by: Option<u64>,
}

impl<V: Vector> Contacts<V> {
    /// The largest postcard encoding: ten bools, five vectors, and five
    /// optional `u64` varints of up to eleven bytes each (125 bytes in 3D,
    /// 105 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 10 + 5 * space::postcard_bytes::<V>() + 5 * 11;

    /// Contacts of a move that asked for `desired` and has touched nothing.
    pub(crate) fn asked(desired: V) -> Self {
        Self {
            desired,
            ..Self::default()
        }
    }

    /// Records a contact as a floor, a ceiling or a wall.
    pub(crate) fn record(&mut self, hit: &Hit<V>, config: &MoverConfig<V>) {
        if config.is_floor(hit.normal) {
            self.floor = true;
            self.last_floor_normal = hit.normal;
            self.last_floor_id = Some(hit.id);
        } else if config.is_ceiling(hit.normal) {
            self.ceiling = true;
            self.last_ceiling_normal = hit.normal;
            self.last_ceiling_id = Some(hit.id);
        } else {
            self.wall = true;
            self.last_wall_normal = hit.normal;
            self.last_wall_id = Some(hit.id);
        }
    }

    /// Adds the contacts of a later part of the same move: its flags join
    /// these, and its most recent contacts replace these.
    pub(crate) fn absorb(&mut self, later: &Self) {
        if later.floor {
            self.floor = true;
            self.last_floor_normal = later.last_floor_normal;
            self.last_floor_id = later.last_floor_id;
        }
        if later.ceiling {
            self.ceiling = true;
            self.last_ceiling_normal = later.last_ceiling_normal;
            self.last_ceiling_id = later.last_ceiling_id;
        }
        if later.wall {
            self.wall = true;
            self.last_wall_normal = later.last_wall_normal;
            self.last_wall_id = later.last_wall_id;
        }
    }

    /// Records the motion applied and sets the blocked flags, which compare
    /// it with the motion asked for along `up`, so they mean the same in a
    /// y-up and a y-down world.
    pub(crate) fn applied(&mut self, effective: V, config: &MoverConfig<V>) {
        self.effective = effective;
        let desired_up = space::dot(self.desired, config.up());
        let effective_up = space::dot(effective, config.up());
        self.down_blocked = desired_up < 0.0 && effective_up > desired_up + config.block_epsilon();
        self.up_blocked = desired_up > 0.0 && effective_up < desired_up - config.block_epsilon();
    }
}

impl<V: Vector> CanonicalWrite for Contacts<V> {
    /// The fields in declaration order: bools as bools, vectors as their
    /// components' bits in axis order, and each optional id as a bool tag
    /// followed, when present, by the `u64`.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.bool(self.floor);
        out.bool(self.ceiling);
        out.bool(self.wall);
        space::write(out, self.last_floor_normal);
        space::write(out, self.last_ceiling_normal);
        space::write(out, self.last_wall_normal);
        for id in [self.last_floor_id, self.last_wall_id, self.last_ceiling_id] {
            out.bool(id.is_some());
            if let Some(id) = id {
                out.u64(id);
            }
        }
        space::write(out, self.desired);
        space::write(out, self.effective);
        out.bool(self.down_blocked);
        out.bool(self.up_blocked);
        out.bool(self.stepped);
        out.bool(self.ledge_guarded);
        out.bool(self.pushed_out);
        out.bool(self.stuck);
        out.bool(self.crushed);
        for id in [self.carried_by, self.pushed_by] {
            out.bool(id.is_some());
            if let Some(id) = id {
                out.u64(id);
            }
        }
    }
}
