//! The character step: Godot's `move_and_slide` over a plain state the game
//! owns, with a step-up, a ledge guard, one-way drop-through and push-out,
//! built on [`slide`](crate::slide) and [`snap_to_ground`].

use sgl_core::{CanonicalWrite, StateHasher};

use crate::config::MoverConfig;
use crate::contacts::Contacts;
use crate::query::{overlaps, pad};
use crate::shape::{ALL_LAYERS, Collider, ColliderKind, Shape};
use crate::slide::{Faces, clip_velocity, ground, slide_faces};
use crate::space::{self, Vector};
use crate::sweep;
use crate::world::{Unanswered, World};

/// Most distances along each axis the push-out tries, the nearest first, so
/// it tests at most `MAX_PUSH_OUT_PLANES` to the power of the number of axes
/// places (64 in 2D, 512 in 3D), and one more along each ramp's normal.
/// Eight covers both faces of three colliders within the body's reach on
/// one axis, a ramp's plane, and staying put.
pub const MAX_PUSH_OUT_PLANES: usize = 8;

/// How many times the ledge guard halves the stretch in which a body's
/// support ends: it then stops within `1/65536` of the motion along that
/// axis of the last supported point.
pub const LEDGE_SEARCH_STEPS: u32 = 16;

/// What a walking, flying or swimming body keeps between steps: plain data
/// the game owns, sends and restores.
///
/// The game sets `velocity` (gravity, jumping, speeds, flying and water are
/// its own); [`move_and_slide`] moves `position`, removes the part of
/// `velocity` that points into what it touched, and sets `floor`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoverState<V> {
    /// The body's centre, in world units.
    pub position: V,
    /// World units per second.
    pub velocity: V,
    /// What the body stands on after the last step, or `None` when it is not
    /// on the ground.
    pub floor: Option<Floor<V>>,
}

impl<V: Vector> MoverState<V> {
    /// The largest postcard encoding: two vectors, an option tag and a
    /// [`Floor`] (47 bytes in 3D, 35 in 2D).
    pub const MAX_POSTCARD_BYTES: usize =
        2 * space::postcard_bytes::<V>() + 1 + Floor::<V>::MAX_POSTCARD_BYTES;
}

impl<V: Vector> CanonicalWrite for MoverState<V> {
    /// `position`'s and `velocity`'s bits in axis order, then a bool tag for
    /// `floor` followed, when present, by the floor.
    fn canonical_write(&self, out: &mut StateHasher) {
        space::write(out, self.position);
        space::write(out, self.velocity);
        out.bool(self.floor.is_some());
        if let Some(floor) = &self.floor {
            floor.canonical_write(out);
        }
    }
}

/// The floor a body stands on.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Floor<V> {
    /// Unit normal of the floor face, pointing toward the body.
    pub normal: V,
    /// The id of the floor's collider.
    pub id: u64,
}

impl<V: Vector> Floor<V> {
    /// The largest postcard encoding: the normal and a `u64` varint (22
    /// bytes in 3D, 18 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = space::postcard_bytes::<V>() + 10;
}

impl<V: Vector> CanonicalWrite for Floor<V> {
    /// The normal's bits in axis order, then `id`.
    fn canonical_write(&self, out: &mut StateHasher) {
        space::write(out, self.normal);
        out.u64(self.id);
    }
}

/// How one [`move_and_slide`] step treats the world, chosen per call
/// because it changes with the body's state (crouching, a held key).
///
/// The default climbs nothing, guards no ledge, lands on one-way platforms
/// and meets every layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoverOptions {
    /// Tallest step a walking body climbs without a jump, in world units.
    /// `0` climbs nothing.
    pub step_height: f32,
    /// Keeps a walking body from leaving a floor that drops further than
    /// this, in world units: it stops over the edge, the way a crouching
    /// player does. A drop within the snap distance is never a ledge, so a
    /// guarded body still walks down ramps. `None` guards nothing.
    pub ledge_drop: Option<f32>,
    /// Passes through one-way platforms for this step: how a body drops
    /// through the platform it stands on.
    pub drop_through: bool,
    /// The layers the body collides with ([`ALL_LAYERS`] for every one).
    pub mask: u32,
}

impl Default for MoverOptions {
    fn default() -> Self {
        Self {
            step_height: 0.0,
            ledge_drop: None,
            drop_through: false,
            mask: ALL_LAYERS,
        }
    }
}

impl MoverOptions {
    /// Panics on a negative or non-finite length: an authoring error, as in
    /// [`MoverConfig::new`].
    fn validate(&self) {
        let lengths = [
            ("step height", self.step_height),
            ("ledge drop", self.ledge_drop.unwrap_or(0.0)),
        ];
        for (name, value) in lengths {
            assert!(
                value.is_finite() && value >= 0.0,
                "MoverOptions: {name} must be finite and non-negative, got {value}"
            );
        }
    }
}

/// One fixed step of a character: Godot's `move_and_slide`. Moves `state`'s
/// body by `velocity * dt`, sliding along what it meets, then removes from
/// `velocity` the part that points into what it touched and sets `floor`.
/// Returns what the step ran into.
///
/// In order:
///
/// 1. **Push-out.** A body that starts inside a solid collider is moved out
///    the shortest way, at most its own size along each axis (its full
///    extent on that axis), and the step goes on from there
///    (`pushed_out`). One that cannot be freed so stays where it is,
///    untouched, and the step reports `stuck`: what to do then is the
///    game's. One-way platforms and sensors never push. The
///    search tries at most [`MAX_PUSH_OUT_PLANES`] distances per axis, the
///    nearest (staying put among them, and of two equally near the
///    negative one), one of which may follow a ramp's plane, and a move
///    along each ramp's normal. Of two equally short ways out, the one most
///    along `up` wins, then the lowest component by component.
/// 2. **Move** with [`slide`](crate::slide), by `velocity * dt`. A walking
///    body is one on a floor at the start of the step and not rising.
/// 3. **Step-up.** When a walking body is stopped by a wall and `step_height` is not zero,
///    it lifts by up to `step_height` (a ceiling shortens the lift), makes
///    the same move across `up` (a ball or a capsule a little further, so
///    its rounded bottom clears the step's edge), and settles back down by
///    the lift plus the snap distance. The climb is kept (`stepped`) only
///    if it ends on a floor and gains more than `block_epsilon` of distance
///    along the requested direction across `up` over the plain move.
/// 4. **Ledge guard.** When `ledge_drop` is set and a walking body's move
///    ends where no floor lies within `ledge_drop` below it (or within the
///    snap distance, if that is further: a drop the snap follows, such as
///    the ground falling away under a body walking down a ramp, is not a
///    ledge), the move is
///    made again across `up` one world axis at a time, each stopping at the
///    last point where a floor still does (`ledge_guarded`). The body may
///    overhang until its trailing edge reaches the edge, and motion along
///    the edge is kept. The search halves [`LEDGE_SEARCH_STEPS`] times.
/// 5. **Snap.** A walking body is snapped to the ground where it ends
///    ([`snap_to_ground`](crate::snap_to_ground)), so it follows seams and
///    small steps down.
/// 6. **Velocity.** Every face the step met, in the order met, removes the
///    part of `velocity` that points into it, by the rules
///    [`slide`](crate::slide) applies to the motion it has left: a floor removes the part against
///    `up` (a landing zeroes the fall, and a body standing on a ramp does
///    not slide down it); a wall or a ceiling projects it onto the face (a
///    slide keeps the part along the wall, a head-bump zeroes the rise); a
///    slope too steep to stand on does too, gaining no height; and a later
///    face never turns it back into an earlier one, so in a corner or a
///    wedge it follows the crease or stops.
/// 7. **Floor.** A walking body's `floor` is the one the snap found under
///    it where it ends, or `None` when it has walked off its floor; any
///    other body's is the last floor it met, so a landing sets it.
///
/// `drop_through` passes every one-way platform for the step, landing and
/// snapping included. `Contacts::effective` is the whole step's
/// translation, push-out and snap included.
///
/// `dt` is the game's fixed step. A negative or non-finite `dt`, a position,
/// velocity or body size that is not finite, or a step that would carry the
/// body where `f32` cannot hold its position (a finite position and motion
/// whose sum is not finite), changes nothing and reports no contact.
///
/// The step keeps what it applies as a translation from where the body
/// started: each stage starts from the translation so far, and
/// `Contacts::effective` and the blocked flags read it, so they do not
/// depend on how far the body is from the world's origin. The position
/// written back is the start plus that translation.
///
/// [`Unanswered`] when the world will not walk a query's region: the state
/// is then left exactly as it was. Nothing is cached between calls, so the
/// same state and arguments give the same result.
///
/// # Panics
/// If `options.step_height` or `options.ledge_drop` is negative or not
/// finite.
pub fn move_and_slide<V: Vector, W: World<V> + ?Sized>(
    state: &mut MoverState<V>,
    body: &Shape<V>,
    dt: f32,
    world: &W,
    config: &MoverConfig<V>,
    options: &MoverOptions,
) -> Result<Contacts<V>, Unanswered> {
    options.validate();
    if !(dt.is_finite() && dt >= 0.0) {
        return Ok(Contacts::asked(V::ZERO));
    }
    let mover = Mover {
        body,
        world: Passable {
            world,
            drop_through: options.drop_through,
        },
        config,
        mask: options.mask,
    };
    match mover.step(*state, dt, options) {
        Ok((next, contacts)) => {
            *state = next;
            Ok(contacts)
        }
        Err(Halt::Unanswered) => Err(Unanswered),
        Err(Halt::Unsound) => Ok(Contacts::asked(state.velocity * dt)),
    }
}

/// Why a step stopped before its end, leaving the state as it was.
enum Halt {
    /// A query was [`Unanswered`].
    Unanswered,
    /// The body's place, size or motion is not finite, or a stage would
    /// carry it where `f32` cannot hold its position.
    Unsound,
}

impl From<Unanswered> for Halt {
    fn from(_: Unanswered) -> Self {
        Self::Unanswered
    }
}

/// The world as one step sees it: without its one-way platforms when the
/// body drops through them.
struct Passable<'a, W: ?Sized> {
    world: &'a W,
    drop_through: bool,
}

impl<W: ?Sized> Passable<'_, W> {
    fn passes(&self, collider: &Collider<impl Vector>) -> bool {
        !(self.drop_through && collider.kind == ColliderKind::OneWay)
    }
}

impl<V: Vector, W: World<V> + ?Sized> World<V> for Passable<'_, W> {
    fn colliders_near(
        &self,
        min: V,
        max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.world.colliders_near(min, max, &mut |collider| {
            if self.passes(collider) {
                visit(collider);
            }
        })
    }

    fn colliders_along(
        &self,
        min: V,
        max: V,
        motion: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.world
            .colliders_along(min, max, motion, &mut |collider| {
                if self.passes(collider) {
                    visit(collider);
                }
            })
    }
}

/// Where a push-out leaves the body.
enum Escape<V> {
    /// It is inside nothing solid.
    Free,
    /// It is out, after this translation.
    Moved(V),
    /// Nothing within its own size frees it.
    Stuck,
}

/// One body, world and configuration for the length of a step.
struct Mover<'a, V: Vector, W: ?Sized> {
    body: &'a Shape<V>,
    world: Passable<'a, W>,
    config: &'a MoverConfig<V>,
    mask: u32,
}

impl<V: Vector, W: World<V> + ?Sized> Mover<'_, V, W> {
    fn step(
        &self,
        state: MoverState<V>,
        dt: f32,
        options: &MoverOptions,
    ) -> Result<(MoverState<V>, Contacts<V>), Halt> {
        let config = self.config;
        let up = config.up();
        let delta = state.velocity * dt;
        let mut contacts = Contacts::asked(delta);
        let start = state.position;
        if !(space::is_finite(delta)
            && space::is_finite(start)
            && space::is_finite(self.body.half_extents()))
        {
            return Err(Halt::Unsound);
        }
        // Every stage below works in translations from `start`: `at` is
        // where the body is now, relative to it.
        let mut at = V::ZERO;
        match self.push_out(start)? {
            Escape::Free => {}
            Escape::Moved(by) => {
                at = by;
                contacts.pushed_out = true;
            }
            Escape::Stuck => {
                contacts.stuck = true;
                contacts.applied(V::ZERO, config);
                return Ok((state, contacts));
            }
        }

        let walking = state.floor.is_some() && space::dot(state.velocity, up) <= 0.0;
        let (moved, mut path) = self.slide(start, at, delta)?;
        let mut end = at + moved;
        if walking
            && options.step_height > 0.0
            && path.contacts.wall
            && let Some((stepped, climb)) =
                self.step_up(start, at, delta, end, options.step_height)?
        {
            (end, path) = (stepped, climb);
            contacts.stepped = true;
        }
        if let Some(drop) = options.ledge_drop {
            let depth = space::max(drop, config.snap_distance()) + config.skin();
            if walking && !self.floor_within(start, end, depth)? {
                let (guarded, along, shortened) = self.guarded(start, at, delta, depth)?;
                (end, path) = (guarded, along);
                contacts.ledge_guarded = shortened;
            }
        }
        contacts.absorb(&path.contacts);
        at = end;
        // A walking body stands on what lies under it where it ends, not on
        // what it touched on the way; one that is not walking lands on the
        // last floor it met.
        let floor = if walking {
            let (snapped, ground) = self.ground(start, at, config)?;
            at = at + snapped;
            contacts.absorb(&ground);
            if ground.floor {
                path.faces.push(ground.last_floor_normal);
            }
            floor_of(&ground)
        } else {
            floor_of(&path.contacts)
        };

        let position = start + at;
        if !space::is_finite(position) {
            return Err(Halt::Unsound);
        }
        contacts.applied(at, config);
        let next = MoverState {
            position,
            velocity: clip_velocity(state.velocity, path.faces.as_slice(), config),
            floor,
        };
        Ok((next, contacts))
    }

    /// [`slide`](crate::slide) of the body at `start + at` by `delta`.
    fn slide(&self, start: V, at: V, delta: V) -> Result<(V, Path<V>), Halt> {
        let world = &self.world;
        let (body, mask, config) = (self.body, self.mask, self.config);
        match slide_faces(body, start, at, delta, world, mask, config)? {
            Some((moved, contacts, faces)) => Ok((moved, Path { contacts, faces })),
            None => Err(Halt::Unsound),
        }
    }

    /// [`snap_to_ground`](crate::snap_to_ground) of the body at
    /// `start + at`, probing as `config` says.
    fn ground(&self, start: V, at: V, config: &MoverConfig<V>) -> Result<(V, Contacts<V>), Halt> {
        ground(self.body, start, at, &self.world, self.mask, config)?.ok_or(Halt::Unsound)
    }

    /// Whether a floor lies within `depth` below the body at `start + at`,
    /// as the ground snap finds one.
    fn floor_within(&self, start: V, at: V, depth: f32) -> Result<bool, Halt> {
        let probe = self.config.with_snap_distance(depth);
        let (_, ground) = self.ground(start, at, &probe)?;
        Ok(ground.floor)
    }

    /// The step-up from `start + at`: lift, the same move across `up`,
    /// settle. The end, relative to `start`, and its contacts when it lands
    /// on a floor further across `up` than the plain move's `plain` end.
    fn step_up(
        &self,
        start: V,
        at: V,
        delta: V,
        plain: V,
        height: f32,
    ) -> Result<Option<(V, Path<V>)>, Halt> {
        let up = self.config.up();
        let across = space::reject(delta, up);
        let length = space::dot(across, across).sqrt();
        if length <= 0.0 {
            return Ok(None);
        }
        let (lift, mut climb) = self.slide(start, at, up * height)?;
        let lifted = at + lift;
        let (forward, ahead) = self.slide(start, lifted, self.over(across, length))?;
        climb.absorb(&ahead);
        let over = lifted + forward;
        let settle = space::dot(lift, up) + self.config.snap_distance();
        let (down, landing) = self.slide(start, over, -up * settle)?;
        if !landing.contacts.floor {
            return Ok(None);
        }
        climb.absorb(&landing);
        let end = over + down;
        // The distance gained along the requested direction.
        let gain = (space::dot(end - at, across) - space::dot(plain - at, across)) / length;
        Ok((gain > self.config.block_epsilon()).then_some((end, climb)))
    }

    /// How far a step-up moves the lifted body across: `across`, and for a
    /// body with a radius (a ball, a capsule) further by enough that its
    /// lowest point, a radius behind its leading surface, passes the step's
    /// edge far enough for the edge to be a floor under it when it settles:
    /// `radius * (1 - sqrt(1 - normal_threshold²))` past it, plus the skin
    /// it was stopped at and a skin to spare. A box's lowest face reaches its
    /// leading surface, so it moves `across` only. `length` is `across`'s.
    fn over(&self, across: V, length: f32) -> V {
        let radius = self.body.radius();
        if radius <= 0.0 {
            return across;
        }
        let threshold = self.config.normal_threshold();
        let inset = radius * (1.0 - (1.0 - threshold * threshold).sqrt());
        across * (1.0 + (inset + 2.0 * self.config.skin()) / length)
    }

    /// The move across `up` from `start + at`, one world axis at a time,
    /// each stopped at the last point where a floor lies within `depth`
    /// below the body. Returns the end, relative to `start`, its contacts,
    /// and whether any axis was shortened.
    fn guarded(&self, start: V, at: V, delta: V, depth: f32) -> Result<(V, Path<V>, bool), Halt> {
        let up = self.config.up();
        let across = space::reject(delta, up);
        let mut here = at;
        let mut path = Path::default();
        let mut shortened = false;
        for &axis in &self.config.tie_order()[..V::AXES] {
            let part = space::reject(space::unit::<V>(axis, true) * across[axis], up);
            if part == V::ZERO {
                continue;
            }
            let (moved, along) = self.slide(start, here, part)?;
            if self.floor_within(start, here + moved, depth)? {
                here = here + moved;
                path.absorb(&along);
                continue;
            }
            // The start is supported and the end is not: halve the stretch
            // between the last supported fraction and the first unsupported.
            shortened = true;
            let (mut supported, mut unsupported) = (0.0f32, 1.0f32);
            let mut kept = (V::ZERO, Path::default());
            for _ in 0..LEDGE_SEARCH_STEPS {
                let mid = (supported + unsupported) * 0.5;
                let (moved, along) = self.slide(start, here, part * mid)?;
                if self.floor_within(start, here + moved, depth)? {
                    supported = mid;
                    kept = (moved, along);
                } else {
                    unsupported = mid;
                }
            }
            here = here + kept.0;
            path.absorb(&kept.1);
        }
        Ok((here, path, shortened))
    }

    /// Moves a body that starts inside a solid collider out by the shortest
    /// translation, at most its own size along each axis, that leaves it
    /// inside none. Candidates put the body a `skin` from a face of a solid
    /// it could reach on each axis, or leave that axis alone; on one axis a
    /// candidate may instead put it a `skin` out of a ramp's plane, by
    /// however far that is given its other axes' distances; and one moves it
    /// along each ramp's normal a `skin` out of its plane, and one along the
    /// normal out of each round or convex overlap whose cores are apart a
    /// `skin` clear of it. The nearest free one wins, ties going to the one
    /// most along `up`, then the lowest component by component. Whether a
    /// place is free is decided by each collider's real shape, cut included,
    /// and a place `f32` cannot hold is none. Planes come from each
    /// collider's bounding box, so a round or convex collider offers the
    /// faces of the box that encloses it.
    fn push_out(&self, at: V) -> Result<Escape<V>, Unanswered> {
        let near = overlaps(self.body, at, &self.world, self.mask)?;
        let cores: Vec<Collider<V>> = near.iter().filter_map(solid_core).collect();
        if !self.inside(at, &cores)? {
            return Ok(Escape::Free);
        }
        let outs = self.ways_out(at, &cores);
        let skin = self.config.skin();

        // Everything the body could overlap anywhere within its reach.
        let half = self.body.half_extents();
        let reach = half * 2.0;
        let (min, max) = (at - half - reach, at + half + reach);
        let grow = pad(min, max);
        let mut solids = Vec::new();
        self.world
            .colliders_near(min - grow, max + grow, &mut |collider| {
                if collider.kind == ColliderKind::Solid
                    && collider.layers & self.mask != 0
                    && collider.occupies()
                {
                    solids.push(*collider);
                }
            })?;
        solids.sort_by(Collider::order);
        solids.dedup_by(|a, b| a.order(b).is_eq());
        let cores: Vec<Collider<V>> = solids.iter().filter_map(solid_core).collect();

        // Each ramp's unit normal, and how far the body must move along it
        // for its deepest point to be a skin out of the plane.
        let ramps: Vec<(V, f32)> = solids
            .iter()
            .filter_map(|solid| {
                let cut = solid.cut?;
                let n = cut.normal();
                let depth = cut.offset() + skin + space::reach(half, n)
                    - space::dot(n, at - solid.position);
                Some((n, depth))
            })
            .collect();

        // The distances each axis tries, nearest first by how far each moves
        // the body when the other axes stay.
        let mut planes: [Vec<(f32, Offset)>; 3] = Default::default();
        for (axis, offsets) in planes.iter_mut().enumerate().take(V::AXES) {
            offsets.push((0.0, Offset::By(0.0)));
            for solid in &solids {
                let rel = solid.position[axis] - at[axis];
                let span = solid.shape.half_extents()[axis] + half[axis] + skin;
                for offset in [rel - span, rel + span] {
                    if offset.abs() <= reach[axis] {
                        offsets.push((offset, Offset::By(offset)));
                    }
                }
            }
            for (index, &(n, depth)) in ramps.iter().enumerate() {
                if n[axis] != 0.0 {
                    let offset = depth / n[axis];
                    if offset.abs() <= reach[axis] {
                        offsets.push((offset, Offset::Plane(index)));
                    }
                }
            }
            offsets.sort_by(|(a, _), (b, _)| a.abs().total_cmp(&b.abs()).then(a.total_cmp(b)));
            offsets.dedup_by(|(a, x), (b, y)| {
                matches!((x, y), (Offset::By(_), Offset::By(_))) && a.to_bits() == b.to_bits()
            });
            offsets.truncate(MAX_PUSH_OUT_PLANES);
        }

        let within = |by: V| (0..V::AXES).all(|axis| by[axis].abs() <= reach[axis]);
        let mut candidates = Vec::new();
        let mut push = |by: V| {
            if by != V::ZERO && within(by) && space::is_finite(at + by) {
                let distance = (0..V::AXES).fold(0.0, |sum, axis| sum + by[axis] * by[axis]);
                candidates.push((distance, by));
            }
        };
        let mut index = [0usize; 3];
        'all: loop {
            if let Some(by) = resolve(&planes, &index, &ramps) {
                push(by);
            }
            for axis in 0..V::AXES {
                index[axis] += 1;
                if index[axis] < planes[axis].len() {
                    continue 'all;
                }
                index[axis] = 0;
            }
            break;
        }
        for &(n, depth) in &ramps {
            push(n * depth);
        }
        for &out in &outs {
            push(out);
        }

        let up = self.config.up();
        candidates.sort_by(|(da, a), (db, b)| {
            da.total_cmp(db)
                .then_with(|| space::dot(*b, up).total_cmp(&space::dot(*a, up)))
                .then_with(|| space::compare(*a, *b))
        });
        for (_, by) in candidates {
            if !self.inside(at + by, &cores)? {
                return Ok(Escape::Moved(by));
            }
        }
        Ok(Escape::Stuck)
    }

    /// For each of `cores` the body at `at` overlaps through the kernel with
    /// their cores apart, the move along the normal between them that
    /// leaves it a `skin` clear.
    fn ways_out(&self, at: V, cores: &[Collider<V>]) -> Vec<V> {
        let skin = self.config.skin();
        cores
            .iter()
            .filter_map(|core| {
                let rel = core.position - at;
                sweep::way_out(self.body, &core.shape, core.cut.as_ref(), rel)
            })
            .map(|(normal, depth)| normal * (depth + skin))
            .collect()
    }

    /// Whether the body at `at` is inside any of `cores`.
    fn inside(&self, at: V, cores: &[Collider<V>]) -> Result<bool, Unanswered> {
        Ok(!overlaps(self.body, at, cores, ALL_LAYERS)?.is_empty())
    }
}

/// One distance a push-out tries along one axis.
#[derive(Clone, Copy)]
enum Offset {
    /// This far.
    By(f32),
    /// However far puts the body a `skin` out of the plane of the ramp at
    /// this index, given the distances on the other axes.
    Plane(usize),
}

/// The push-out candidate that takes, on each axis, the distance at `index`
/// in `planes`: `None` when more than one axis follows a ramp's plane.
fn resolve<V: Vector>(
    planes: &[Vec<(f32, Offset)>; 3],
    index: &[usize; 3],
    ramps: &[(V, f32)],
) -> Option<V> {
    let mut by = V::ZERO;
    let mut plane = None;
    for axis in 0..V::AXES {
        match planes[axis][index[axis]].1 {
            Offset::By(offset) => by[axis] = offset,
            Offset::Plane(ramp) => {
                if plane.is_some() {
                    return None;
                }
                plane = Some((axis, ramp));
            }
        }
    }
    if let Some((axis, ramp)) = plane {
        let (n, depth) = ramps[ramp];
        let mut rest = depth;
        for other in 0..V::AXES {
            if other != axis {
                rest -= n[other] * by[other];
            }
        }
        by[axis] = rest / n[axis];
    }
    Some(by)
}

/// A solid collider less its rounding allowance on each side, its cut
/// included, or `None` for any other kind. A body whose edge equals a face
/// in world coordinates can lie a rounding step inside it once taken
/// relative to the body: that is touching, and a push-out must neither
/// start from it nor count it against a place to move to. A ball or a
/// capsule loses the largest allowance from its radius; a convex shape is
/// kept whole, since the mover leaves a body a skin from one.
fn solid_core<V: Vector>(collider: &Collider<V>) -> Option<Collider<V>> {
    if collider.kind != ColliderKind::Solid {
        return None;
    }
    let rounding = collider.rounding();
    let shape = match collider.shape {
        Shape::Box { half } => Shape::Box {
            half: half - rounding,
        },
        Shape::Ball { radius } => Shape::Ball {
            radius: radius - space::largest_abs(rounding),
        },
        Shape::Capsule { half_axis, radius } => Shape::Capsule {
            half_axis,
            radius: radius - space::largest_abs(rounding),
        },
        convex @ Shape::Convex(_) => convex,
    };
    let cut = collider
        .cut
        .map(|cut| cut.moved_in(space::reach(rounding, cut.normal())));
    Some(Collider {
        shape,
        cut,
        ..*collider
    })
}

/// The floor `contacts` last met, or `None`.
fn floor_of<V: Vector>(contacts: &Contacts<V>) -> Option<Floor<V>> {
    contacts
        .last_floor_id
        .filter(|_| contacts.floor)
        .map(|id| Floor {
            normal: contacts.last_floor_normal,
            id,
        })
}

/// What part of a step's move met: its contacts, and the faces it met in
/// order, which the velocity is clipped against.
#[derive(Clone, Copy)]
struct Path<V> {
    contacts: Contacts<V>,
    faces: Faces<V>,
}

impl<V: Vector> Default for Path<V> {
    fn default() -> Self {
        Self {
            contacts: Contacts::default(),
            faces: Faces::new(),
        }
    }
}

impl<V: Vector> Path<V> {
    /// Adds a later part of the same move.
    fn absorb(&mut self, later: &Self) {
        self.contacts.absorb(&later.contacts);
        self.faces.extend(&later.faces);
    }
}
