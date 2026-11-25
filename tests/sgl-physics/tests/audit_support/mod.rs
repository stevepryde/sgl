//! Shared by the audit suites (#924): seeded random levels in 2D and 3D,
//! the worlds a level can be given through, and the geometric oracle every
//! "never inside a solid" check reads. The oracle is the shapes' own
//! definitions worked in `f64`, never a crate query.
#![allow(
    dead_code,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names
)]

pub mod flat;
pub mod solid;

use std::collections::HashMap;

use sgl_core::SplitMix64;
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts, Cut,
    Floor, MoverConfig, MoverOptions, MoverSettings, MoverState, Shape, Unanswered, Vector, World,
    cast, find_ledge, move_and_collide, move_and_slide, overlaps, ray, slide, snap_to_ground,
};

pub const SKIN: f32 = 0.01;

/// A seeded stream of the values the generators draw.
pub struct Rng(SplitMix64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(SplitMix64::new(seed))
    }

    /// An integer in `lo..=hi`.
    pub fn int(&mut self, lo: i32, hi: i32) -> i32 {
        let width = (hi - lo) as u32;
        lo + self.0.range_inclusive(0, width) as i32
    }

    pub fn below(&mut self, n: usize) -> usize {
        self.0.range_inclusive(0, n as u32 - 1) as usize
    }

    pub fn chance(&mut self, percent: u32) -> bool {
        self.0.range_inclusive(0, 99) < percent
    }

    /// A float in `[lo, hi)` with 24 random bits.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        let unit = (self.0.next_u64() >> 40) as f32 / 16_777_216.0;
        lo + (hi - lo) * unit
    }

    /// A multiple of `step` in `lo..=hi` steps.
    pub fn grid(&mut self, lo: i32, hi: i32, step: f32) -> f32 {
        self.int(lo, hi) as f32 * step
    }

    pub fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len())]
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }

    pub fn vector<V: Vector>(&mut self, lo: f32, hi: f32) -> V {
        let mut out = V::ZERO;
        for axis in 0..V::AXES {
            out[axis] = self.range(lo, hi);
        }
        out
    }
}

pub fn config<V: Vector>(up: V) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    })
}

pub fn up<V: Vector>() -> V {
    let mut out = V::ZERO;
    out[1] = 1.0;
    out
}

pub fn splat<V: Vector>(value: f32) -> V {
    let mut out = V::ZERO;
    for axis in 0..V::AXES {
        out[axis] = value;
    }
    out
}

pub fn bits<V: Vector>(v: V) -> Vec<u32> {
    (0..V::AXES).map(|axis| v[axis].to_bits()).collect()
}

pub fn is_finite<V: Vector>(v: V) -> bool {
    (0..V::AXES).all(|axis| v[axis].is_finite())
}

pub fn dot<V: Vector>(a: V, b: V) -> f32 {
    (0..V::AXES).map(|axis| a[axis] * b[axis]).sum()
}

// ----------------------------------------------------------------- battery

/// The mover's step in the battery.
pub const DT: f32 = 0.25;

/// One case of the query battery: a body, a motion and the options the
/// mover takes.
#[derive(Debug, Clone, Copy)]
pub struct Case<V> {
    pub half: V,
    pub at: V,
    pub delta: V,
    pub mask: u32,
    pub walking: bool,
    pub options: MoverOptions,
    pub toward: V,
}

/// What every public query answers for one case through one world: exact
/// values as text (`Debug` prints every distinct `f32` distinctly), and
/// the absolute positions it ends at less `shift`, kept apart because a
/// translated scene rounds them at its own magnitude.
#[derive(Debug, Default, PartialEq)]
pub struct Answers<V> {
    pub exact: Vec<String>,
    pub places: Vec<(&'static str, V)>,
}

/// Runs every query once, and the mover for three steps. `skip_overlaps`
/// leaves out the one query that lists sensors.
pub fn battery<V: Vector, W: World<V> + ?Sized>(
    case: &Case<V>,
    world: &W,
    shift: V,
    mask: u32,
) -> Result<Answers<V>, Unanswered> {
    battery_steps(case, world, shift, mask, 3)
}

/// The battery with `steps` mover steps. With one step the mover's
/// blocked flags are left out of `exact` (its `effective` is always in
/// `places`): see `near_is_as_good_as_far`.
pub fn battery_steps<V: Vector, W: World<V> + ?Sized>(
    case: &Case<V>,
    world: &W,
    shift: V,
    mask: u32,
    steps: usize,
) -> Result<Answers<V>, Unanswered> {
    battery_up(case, world, shift, mask, steps, up::<V>())
}

/// The battery in a world whose up axis is `up`: gravity and a walking
/// body's floor follow it.
pub fn battery_up<V: Vector, W: World<V> + ?Sized>(
    case: &Case<V>,
    world: &W,
    shift: V,
    mask: u32,
    steps: usize,
    up: V,
) -> Result<Answers<V>, Unanswered> {
    let config = config(up);
    let body = Shape::Box { half: case.half };
    let at = case.at + shift;
    let mut out = Answers::default();
    let (moved, contacts) = slide(&body, at, case.delta, world, mask, &config)?;
    out.exact.push(format!("slide {moved:?} {contacts:?}"));
    out.places.push(("slide", at + moved - shift));
    let (snap, snapped) = snap_to_ground(&body, at, world, mask, &config)?;
    out.exact.push(format!("snap {snap:?} {snapped:?}"));
    out.places.push(("snap", at + snap - shift));
    out.exact.push(format!(
        "cast {:?}",
        cast(&body, at, case.delta, world, mask)?
    ));
    out.exact
        .push(format!("ray {:?}", ray(at, case.delta, world, mask)?));
    let (collided, hit) = move_and_collide(&body, at, case.delta, world, mask, &config)?;
    out.exact.push(format!("collide {collided:?} {hit:?}"));
    out.places.push(("collide", at + collided - shift));
    // Half the motion on a sixteenth grid, rounded toward zero, so the
    // place translates and mirrors exactly.
    let mut half_way = case.delta * 0.5;
    for axis in 0..V::AXES {
        half_way[axis] = (half_way[axis] * 16.0).trunc() / 16.0;
    }
    let listed = overlaps(&body, at + half_way, world, mask)?;
    for c in listed {
        out.exact
            .push(format!("overlap {} {:?} {:?}", c.id, c.kind, c.layers));
        out.places.push(("overlap", c.position - shift));
    }
    if let Some(ledge) = find_ledge(&body, at, case.toward, world, &config, 2.0, mask)? {
        out.exact.push(format!("ledge {}", ledge.id));
        out.places.push(("hang", ledge.hang - shift));
        out.places.push(("stand", ledge.stand - shift));
    }
    let mut state = MoverState {
        position: at,
        velocity: case.delta * (1.0 / DT),
        floor: case.walking.then_some(Floor { normal: up, id: 0 }),
    };
    let options = MoverOptions {
        mask,
        ..case.options
    };
    for _ in 0..steps {
        let contacts = move_and_slide(&mut state, &body, DT, world, &config, &options)?;
        let relative = steps == 1;
        out.exact.push(format!(
            "step {:?} {:?} {:?}",
            state.velocity,
            state.floor,
            Contacts {
                effective: V::ZERO,
                down_blocked: contacts.down_blocked && !relative,
                up_blocked: contacts.up_blocked && !relative,
                ..contacts
            }
        ));
        out.places.push(("step", state.position - shift));
        out.places.push(("effective", contacts.effective));
        state.velocity = state.velocity - up * (10.0 * DT);
    }
    Ok(out)
}

// ------------------------------------------------------------------ oracle

/// Whether a collider occupies space, by the contract's own definition
/// (physics.md 9): a finite position and finite, non-negative half-extents,
/// and, when cut, a cut that leaves something on a collider that is not
/// one-way.
pub fn occupies<V: Vector>(c: &Collider<V>) -> bool {
    let half = c.shape.half_extents();
    let sound =
        is_finite(c.position) && is_finite(half) && (0..V::AXES).all(|axis| half[axis] >= 0.0);
    match c.cut {
        None => sound,
        Some(cut) => {
            let reach: f64 = (0..V::AXES)
                .map(|axis| f64::from(cut.normal()[axis].abs()) * f64::from(half[axis]))
                .sum();
            sound && c.kind != ColliderKind::OneWay && f64::from(cut.offset()) >= -reach
        }
    }
}

/// The interval a collider's solid part covers along a cardinal axis, in
/// `f64`: its box, less what its cut takes from that end (the solid part is
/// the box where `normal · (p - centre) <= offset`, physics.md 16).
fn solid_span<V: Vector>(c: &Collider<V>, axis: usize) -> (f64, f64) {
    let half = c.shape.half_extents();
    let (centre, h) = (f64::from(c.position[axis]), f64::from(half[axis]));
    let (mut lo, mut hi) = (h, h);
    if let Some(cut) = c.cut {
        let n = cut.normal();
        let others: f64 = (0..V::AXES)
            .filter(|&j| j != axis)
            .map(|j| f64::from(n[j].abs()) * f64::from(half[j]))
            .sum();
        let room = f64::from(cut.offset()) + others;
        let ni = f64::from(n[axis]);
        if ni > 0.0 {
            hi = hi.min(room / ni);
        } else if ni < 0.0 {
            lo = lo.min(room / -ni);
        }
    }
    (centre - lo, centre + hi)
}

/// How deep a box body at `at` of half-extents `half` is inside a
/// collider's solid part: the least overlap over the separating axes of the
/// two shapes (the cardinal axes and, for a cut box, its normal: physics.md
/// 16 says they separate on one of those exactly). Zero or less is apart
/// or touching.
pub fn depth<V: Vector>(at: V, half: V, c: &Collider<V>) -> f64 {
    if !occupies(c) {
        return f64::NEG_INFINITY;
    }
    let mut least = f64::INFINITY;
    for axis in 0..V::AXES {
        let (lo, hi) = solid_span(c, axis);
        let (b, h) = (f64::from(at[axis]), f64::from(half[axis]));
        least = least.min(hi.min(b + h) - lo.max(b - h));
    }
    if let Some(cut) = c.cut {
        let n = cut.normal();
        let ch = c.shape.half_extents();
        let along = |v: V| -> f64 {
            (0..V::AXES)
                .map(|j| f64::from(n[j]) * f64::from(v[j]))
                .sum()
        };
        let reach = |h: V| -> f64 {
            (0..V::AXES)
                .map(|j| f64::from(n[j].abs()) * f64::from(h[j]))
                .sum()
        };
        let (sc, rc) = (along(c.position), reach(ch));
        let solid = (sc - rc, sc + f64::from(cut.offset()).min(rc));
        let (sb, rb) = (along(at), reach(half));
        least = least.min(solid.1.min(sb + rb) - solid.0.max(sb - rb));
    }
    least
}

/// What one rounding step is at these coordinates: `16ε` of the largest
/// magnitude in play. Far below any skin, so a skin applied with the wrong
/// sign still shows.
pub fn rounding<V: Vector>(at: V, half: V, c: &Collider<V>) -> f64 {
    let mut largest = 1.0f64;
    for axis in 0..V::AXES {
        largest = largest
            .max(f64::from(at[axis].abs()) + f64::from(half[axis]))
            .max(f64::from(c.position[axis].abs()) + f64::from(c.shape.half_extents()[axis]));
    }
    16.0 * f64::from(f32::EPSILON) * largest
}

/// The solids on `mask` the body at `at` is inside by more than a rounding
/// step.
pub fn solids_inside<V: Vector>(
    at: V,
    half: V,
    colliders: &[Collider<V>],
    mask: u32,
) -> Vec<(u64, f64)> {
    colliders
        .iter()
        .filter(|c| c.kind == ColliderKind::Solid && c.layers & mask != 0)
        .map(|c| (c.id, depth(at, half, c), rounding(at, half, c)))
        .filter(|&(_, d, tol)| d > tol)
        .map(|(id, d, _)| (id, d))
        .collect()
}

/// Whether the body overlaps a collider's solid part (touching is not
/// overlap), in the oracle's `f64`.
pub fn overlapping<V: Vector>(at: V, half: V, c: &Collider<V>) -> bool {
    depth(at, half, c) > 0.0
}

// ------------------------------------------------------------------ levels

/// A random level: colliders that each lie inside one unit cell (given in
/// that cell's local units, so a `CellWorld` can hold them), and larger
/// ones a cell world cannot. Positions and sizes are on a quarter-unit
/// grid, so a power-of-two translation is exact.
pub struct Level<V: Vector> {
    pub cells: HashMap<V::Cell, Collider<V>>,
    pub big: Vec<Collider<V>>,
}

pub fn cell_of<V: Vector>(cell: V::Cell) -> V {
    let mut out = V::ZERO;
    for axis in 0..V::AXES {
        out[axis] = cell[axis] as f32;
    }
    out
}

/// A collider in world units from one in a cell's local units, at a cell
/// size of one: `(cell + local)`.
pub fn in_world<V: Vector>(cell: V::Cell, local: &Collider<V>) -> Collider<V> {
    Collider {
        position: cell_of::<V>(cell) + local.position,
        ..*local
    }
}

/// A random cut through a box of half-extents `half`: in 3D one component
/// of the normal is zero (physics.md 16).
fn random_cut<V: Vector>(rng: &mut Rng, half: V) -> Cut<V> {
    let mut normal = V::ZERO;
    let flat = if V::AXES == 3 { rng.below(3) } else { 9 };
    // Two non-zero components from 1 and 2, never both 2, so the normal a
    // test reads back names the one it was built from (`given_normal`).
    let mut twos = 0;
    for axis in 0..V::AXES {
        if axis != flat {
            let size = if twos == 0 && rng.chance(40) {
                2.0
            } else {
                1.0
            };
            twos += usize::from(size == 2.0);
            normal[axis] = if rng.chance(50) { size } else { -size };
        }
    }
    if V::AXES == 2 && rng.chance(15) {
        normal[rng.below(2)] = 0.0;
    }
    let unit = Cut::new(normal, 0.0).normal();
    let reach: f32 = (0..V::AXES).map(|a| unit[a].abs() * half[a]).sum();
    Cut::new(normal, reach * rng.grid(-6, 4, 0.125))
}

/// The normal a generated cut was built from, read back from its unit
/// normal: its components over the smallest non-zero one, which are 1 or 2.
pub fn given_normal<V: Vector>(cut: &Cut<V>) -> V {
    let n = cut.normal();
    let smallest = (0..V::AXES)
        .map(|axis| n[axis].abs())
        .filter(|&a| a > 0.0)
        .fold(f32::INFINITY, f32::min);
    let mut out = V::ZERO;
    for axis in 0..V::AXES {
        out[axis] = (n[axis] / smallest).round();
    }
    out
}

fn random_kind(rng: &mut Rng) -> ColliderKind {
    match rng.below(10) {
        0 | 1 => ColliderKind::OneWay,
        2 => ColliderKind::Sensor,
        _ => ColliderKind::Solid,
    }
}

fn dress<V: Vector>(rng: &mut Rng, collider: Collider<V>, ramps: bool) -> Collider<V> {
    let kind = random_kind(rng);
    let layers = rng.pick(&[1, 2, 3, ALL_LAYERS]);
    let mut out = collider.with_layers(layers);
    if ramps && kind == ColliderKind::Solid && rng.chance(35) {
        out = out.with_cut(random_cut(rng, out.shape.half_extents()));
    }
    // A one-way platform takes no cut (physics.md 16): it stays solid.
    if out.cut.is_none() || kind != ColliderKind::OneWay {
        out = out.with_kind(kind);
    }
    out
}

pub fn level<V: Vector>(rng: &mut Rng, ramps: bool) -> Level<V> {
    let count = rng.int(3, 10) as usize;
    let mut ids: Vec<u64> = (0..count as u64 * 2).map(|i| i * 7 + 3).collect();
    rng.shuffle(&mut ids);
    let mut cells = HashMap::new();
    let mut big = Vec::new();
    for (n, id) in ids.into_iter().enumerate() {
        if n % 2 == 0 {
            let mut cell = V::Cell::default();
            let mut half = V::ZERO;
            let mut local = V::ZERO;
            for axis in 0..V::AXES {
                cell[axis] = rng.int(-3, 2);
                half[axis] = rng.pick(&[0.25, 0.5]);
                // Centre a quarter-step multiple with the box inside 0..1.
                let room = ((1.0 - 2.0 * half[axis]) / 0.25) as i32;
                local[axis] = half[axis] + rng.grid(0, room, 0.25);
            }
            let collider = dress(rng, Collider::new(id, Shape::Box { half }, local), ramps);
            cells.insert(cell, collider);
        } else {
            let mut half = V::ZERO;
            let mut at = V::ZERO;
            for axis in 0..V::AXES {
                half[axis] = rng.grid(1, 10, 0.25);
                at[axis] = rng.grid(-16, 16, 0.25);
            }
            if n == 1 && rng.chance(60) {
                // A floor under most of the level.
                half[0] = 6.0;
                half[1] = rng.grid(1, 4, 0.25);
                at[0] = 0.0;
                at[1] = rng.grid(-16, -8, 0.25);
            }
            big.push(dress(
                rng,
                Collider::new(id, Shape::Box { half }, at),
                ramps,
            ));
        }
    }
    Level { cells, big }
}

impl<V: Vector> Level<V> {
    /// Every collider in world units, cell ones first by cell, then the big
    /// ones in the order drawn.
    pub fn all(&self) -> Vec<Collider<V>> {
        let mut cells: Vec<_> = self.cells.iter().map(|(&c, l)| in_world(c, l)).collect();
        cells.sort_by_key(|c| c.id);
        cells.extend(&self.big);
        cells
    }

    pub fn cell_world(&self) -> CellWorld<V, impl Fn(V::Cell, &mut CellColliders<'_, V>) + '_> {
        CellWorld::new(1.0, move |cell: V::Cell, out: &mut CellColliders<'_, V>| {
            if let Some(local) = self.cells.get(&cell) {
                out.push(*local);
            }
        })
    }
}

/// A body start not inside any solid of `colliders` (by the oracle), or
/// `None` after a few tries.
pub fn free_start<V: Vector>(
    rng: &mut Rng,
    half: V,
    colliders: &[Collider<V>],
    lo: f32,
    hi: f32,
) -> Option<V> {
    (0..20).find_map(|_| {
        let at: V = rng.vector(lo, hi);
        colliders
            .iter()
            .filter(|c| c.kind == ColliderKind::Solid)
            .all(|c| depth(at, half, c) <= 0.0)
            .then_some(at)
    })
}

pub fn set_of<V: Vector>(colliders: &[Collider<V>], cell_size: f32) -> ColliderSet<V> {
    let mut set = ColliderSet::new(cell_size);
    for c in colliders {
        set.insert(*c);
    }
    set
}

/// A world that offers what `inner` offers and then refuses, from its
/// `refuse_at`-th query on: what any world may do (physics.md 10).
pub struct Refusing<'a, V: Vector> {
    pub inner: &'a [Collider<V>],
    pub refuse_at: usize,
    pub asked: std::cell::Cell<usize>,
}

impl<V: Vector> Refusing<'_, V> {
    fn answer(&self, visit: &mut dyn FnMut(&Collider<V>)) -> Result<(), Unanswered> {
        let n = self.asked.get();
        self.asked.set(n + 1);
        for c in self.inner {
            visit(c);
        }
        if n >= self.refuse_at {
            Err(Unanswered)
        } else {
            Ok(())
        }
    }
}

impl<V: Vector> World<V> for Refusing<'_, V> {
    fn colliders_near(
        &self,
        _min: V,
        _max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.answer(visit)
    }

    fn colliders_along(
        &self,
        _min: V,
        _max: V,
        _motion: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        self.answer(visit)
    }
}
