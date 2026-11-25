//! The state a game keeps is plain data it can send and restore
//! (physics.md, "pure and plain" and "ready for the network"). Runs natively
//! and in a wasm runtime.
#![allow(clippy::cast_precision_loss)]

use serde::{Deserialize, Serialize};
use sgl_core::math::{IVec3, Quat, Vec2, Vec3};
use sgl_core::{SplitMix64, digest_of};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts, Convex,
    Cut, Floor, Hit, Ledge, MoverConfig, MoverOptions, MoverSettings, MoverState, Push, Separation,
    Shape, Vector, World, move_and_slide, slide, snap_to_ground,
};

/// Mover settings from positional values, in `MoverSettings`' order.
fn mover<V: Vector>(
    up: V,
    skin: f32,
    snap_distance: f32,
    normal_threshold: f32,
    block_epsilon: f32,
) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin,
        snap_distance,
        normal_threshold,
        block_epsilon,
    })
}
use wasm_bindgen_test::wasm_bindgen_test;

/// A game's body as it might keep and send it: SGL's types and its own.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Body {
    position: Vec3,
    velocity: Vec3,
    shape: Shape<Vec3>,
    contacts: Contacts<Vec3>,
}

/// A room with a floor, two walls and a step, as a set.
fn room() -> ColliderSet<Vec3> {
    let mut set = ColliderSet::new(2.0);
    let slab = |id, center: Vec3, half: Vec3| Collider::new(id, Shape::Box { half }, center);
    set.insert(slab(1, Vec3::new(0.0, -0.5, 0.0), Vec3::new(8.0, 0.5, 8.0)));
    set.insert(slab(2, Vec3::new(8.5, 2.0, 0.0), Vec3::new(0.5, 2.0, 8.0)));
    set.insert(slab(3, Vec3::new(0.0, 2.0, -8.5), Vec3::new(8.0, 2.0, 0.5)));
    set.insert(slab(
        4,
        Vec3::new(3.0, 0.25, 3.0),
        Vec3::new(1.0, 0.25, 1.0),
    ));
    set.insert(
        slab(5, Vec3::new(-3.0, 1.0, 0.0), Vec3::new(1.0, 0.1, 1.0))
            .with_kind(ColliderKind::OneWay),
    );
    set
}

/// One fixed step of the game: the input for step `step` comes from the
/// step number alone, so a restored run is fed what the original was.
/// The rules read the last step's contacts, so a lost contact shows.
fn step(body: &mut Body, step: u64, world: &ColliderSet<Vec3>, config: &MoverConfig<Vec3>) {
    let mut rng = SplitMix64::new(step);
    let mut unit = || (rng.next_u64() >> 40) as f32 / 16_777_216.0;
    let grounded = body.contacts.floor;
    body.velocity.x = unit() * 0.8 - 0.3;
    body.velocity.z = unit() * 0.8 - 0.3;
    body.velocity.y = if grounded && unit() < 0.2 {
        0.5
    } else {
        body.velocity.y - 0.05
    };
    let (moved, contacts) = slide(
        &body.shape,
        body.position,
        body.velocity,
        world,
        ALL_LAYERS,
        config,
    )
    .unwrap();
    body.position += moved;
    if contacts.floor && contacts.down_blocked {
        body.velocity.y = 0.0;
    }
    if grounded && body.velocity.y <= 0.0 {
        let (snap, _) =
            snap_to_ground(&body.shape, body.position, world, ALL_LAYERS, config).unwrap();
        body.position += snap;
    }
    body.contacts = contacts;
}

/// Defect: a state type that loses something on the wire (a field left out
/// of serialisation, a float narrowed or printed, an id truncated), so a
/// restored body diverges from the one that was never sent. Oracle: the
/// round trip: a body serialised with postcard at step 40 and restored
/// continues bit-equal, step by step, to the one that was not.
#[wasm_bindgen_test(unsupported = test)]
fn a_restored_body_continues_bit_equal() {
    let world = room();
    let config = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
    let mut body = Body {
        position: Vec3::new(0.0, 1.5, 0.0),
        velocity: Vec3::ZERO,
        shape: Shape::Box {
            half: Vec3::new(0.3, 0.9, 0.3),
        },
        contacts: Contacts::default(),
    };
    for n in 0..40 {
        step(&mut body, n, &world, &config);
    }
    let bytes = postcard::to_allocvec(&body).unwrap();
    let mut restored: Body = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(postcard::to_allocvec(&restored).unwrap(), bytes);
    let mut stood = 0;
    for n in 40..120 {
        step(&mut body, n, &world, &config);
        step(&mut restored, n, &world, &config);
        assert_eq!(
            postcard::to_allocvec(&restored).unwrap(),
            postcard::to_allocvec(&body).unwrap(),
            "diverged at step {n}"
        );
        assert_eq!(digest_of(&restored.contacts), digest_of(&body.contacts));
        stood += u32::from(body.contacts.floor);
    }
    // The rules read `floor`, so the round trip is tested on a body that
    // stands, jumps and lands.
    assert!(stood > 10, "the walk stood on something only {stood} times");
}

/// One fixed step of a game using the mover: the input for step `step`
/// comes from the step number alone (a run, a jump, holding down to drop
/// through, a guarded stretch), so a restored run is fed what the original
/// was. Climbing, the guard and drop-through all read the state's `floor`,
/// so a lost floor shows.
fn mover_step<V: Vector, W: World<V> + ?Sized>(
    state: &mut MoverState<V>,
    step: u64,
    body: &Shape<V>,
    world: &W,
    config: &MoverConfig<V>,
) -> Contacts<V> {
    let mut rng = SplitMix64::new(step ^ 0x0916);
    let mut unit = || (rng.next_u64() >> 40) as f32 / 16_777_216.0;
    for axis in [0, 2].into_iter().take(V::AXES - 1) {
        state.velocity[axis] = unit() * 8.0 - 4.0;
    }
    state.velocity[1] = if state.floor.is_some() && unit() < 0.1 {
        7.0
    } else {
        state.velocity[1] - 20.0 / 60.0
    };
    let options = MoverOptions {
        step_height: 0.3,
        ledge_drop: (step % 60 < 30).then_some(0.4),
        drop_through: unit() < 0.2,
        mask: ALL_LAYERS,
    };
    move_and_slide(state, body, 1.0 / 60.0, world, config, &options).unwrap()
}

/// A voxel level answered cell by cell: ground, a few half-cell steps, a
/// pit, a run of one-way platforms and walls round the edge.
fn voxels(cell: IVec3, out: &mut CellColliders<'_, Vec3>) {
    let id = u64::from((cell.x + 64).unsigned_abs())
        | u64::from((cell.y + 64).unsigned_abs()) << 8
        | u64::from((cell.z + 64).unsigned_abs()) << 16;
    let (x, y, z) = (cell.x, cell.y, cell.z);
    let at = |half: Vec3, center: Vec3| Collider::new(id, Shape::Box { half }, center);
    let full = at(Vec3::splat(0.5), Vec3::splat(0.5));
    if x.abs() >= 6 || z.abs() >= 6 || (y == -1 && !(x == 2 && z == 2)) {
        out.push(full);
    } else if y == 0 && (x + z).rem_euclid(5) == 0 {
        out.push(at(Vec3::new(0.5, 0.125, 0.5), Vec3::new(0.5, 0.125, 0.5)));
    } else if y == 1 && x == -2 {
        out.push(
            at(Vec3::new(0.5, 0.1, 0.5), Vec3::new(0.5, 0.9, 0.5)).with_kind(ColliderKind::OneWay),
        );
    }
}

/// A 2D level: a floor with a pit, a low step, a one-way deck and walls.
fn level_2d() -> ColliderSet<Vec2> {
    let mut set = ColliderSet::new(2.0);
    let slab = |id, min: Vec2, max: Vec2| {
        Collider::new(
            id,
            Shape::Box {
                half: (max - min) * 0.5,
            },
            (min + max) * 0.5,
        )
    };
    set.insert(slab(1, Vec2::new(-8.0, -1.0), Vec2::new(1.0, 0.0)));
    set.insert(slab(2, Vec2::new(1.0, -3.0), Vec2::new(2.0, -2.0)));
    set.insert(slab(3, Vec2::new(2.0, -1.0), Vec2::new(8.0, 0.0)));
    set.insert(slab(4, Vec2::new(4.0, 0.0), Vec2::new(8.0, 0.25)));
    set.insert(slab(5, Vec2::new(-8.0, 0.0), Vec2::new(-7.0, 9.0)));
    set.insert(slab(6, Vec2::new(7.0, 0.0), Vec2::new(8.0, 9.0)));
    set.insert(slab(7, Vec2::new(-5.0, 1.9), Vec2::new(-1.0, 2.0)).with_kind(ColliderKind::OneWay));
    set
}

/// Steps a mover from `start` through 150 fixed steps, and again from the
/// state at step 50 sent through postcard, checking every later step is
/// bit-equal. Returns how many steps stood on a floor.
fn replay<V, W>(start: MoverState<V>, body: &Shape<V>, world: &W, config: &MoverConfig<V>) -> u32
where
    V: Vector + Serialize + for<'de> Deserialize<'de>,
    W: World<V> + ?Sized,
{
    let mut state = start;
    for n in 0..50 {
        mover_step(&mut state, n, body, world, config);
    }
    let bytes = postcard::to_allocvec(&state).unwrap();
    let mut restored: MoverState<V> = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(postcard::to_allocvec(&restored).unwrap(), bytes);
    let mut stood = 0;
    for n in 50..150 {
        let forward = mover_step(&mut state, n, body, world, config);
        let again = mover_step(&mut restored, n, body, world, config);
        assert_eq!(
            postcard::to_allocvec(&restored).unwrap(),
            postcard::to_allocvec(&state).unwrap(),
            "diverged at step {n}"
        );
        assert_eq!(digest_of(&restored), digest_of(&state));
        assert_eq!(digest_of(&again), digest_of(&forward));
        stood += u32::from(state.floor.is_some());
    }
    stood
}

/// Defect: a mover state that loses something on the wire (the floor's id
/// or normal, a velocity component), or a mover that keeps something
/// between calls, so a client replaying its inputs from a saved state
/// diverges from the run it replays. Oracle: the round trip: a state sent
/// through postcard at step 50 and stepped through the same inputs is
/// bit-equal, step by step, to the forward run, through a 2D set and a 3D
/// voxel cell world.
#[wasm_bindgen_test(unsupported = test)]
fn a_restored_mover_replays_its_inputs_bit_equal() {
    let start = MoverState {
        position: Vec2::new(-3.0, 1.0),
        ..MoverState::default()
    };
    let body = Shape::Box {
        half: Vec2::new(0.4, 0.5),
    };
    let stood = replay(
        start,
        &body,
        &level_2d(),
        &mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4),
    );
    assert!(stood > 10, "2D stood {stood} times");

    let start = MoverState {
        position: Vec3::new(0.5, 1.5, 0.5),
        ..MoverState::default()
    };
    let body = Shape::Box {
        half: Vec3::new(0.3, 0.9, 0.3),
    };
    let world = CellWorld::new(1.0, voxels);
    let stood = replay(start, &body, &world, &mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4));
    assert!(stood > 10, "3D stood {stood} times");
}

/// Defect: a config that comes back from the wire with a different `up`
/// (normalised twice, so rounded twice), so a client classifies a face as a
/// floor where its server saw a wall. Oracle: the round trip, by the bits of
/// the normalised `up` and the canonical digest.
#[wasm_bindgen_test(unsupported = test)]
fn a_config_round_trips_to_the_same_bits() {
    let config = mover(Vec3::new(0.2, 1.0, 0.3), 0.01, 0.1, 0.7, 1e-4);
    let restored: MoverConfig<Vec3> =
        postcard::from_bytes(&postcard::to_allocvec(&config).unwrap()).unwrap();
    let bits = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
    assert_eq!(bits(restored.up()), bits(config.up()));
    assert_eq!(digest_of(&restored), digest_of(&config));
}

/// A config on the wire is validated as `new` validates it: one with a
/// negative skin is refused, not built.
#[wasm_bindgen_test(unsupported = test)]
fn a_config_from_the_wire_is_validated() {
    let valid = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let mut bytes = postcard::to_allocvec(&valid).unwrap();
    // The skin's four bytes follow the two of `up`: make it -0.01.
    bytes[8..12].copy_from_slice(&(-0.01f32).to_le_bytes());
    assert!(postcard::from_bytes::<MoverConfig<Vec2>>(&bytes).is_err());
}

fn encoded<T: Serialize>(value: &T) -> usize {
    postcard::to_allocvec(value).unwrap().len()
}

/// The largest value of each state type in space `V`: every id and layer
/// mask at its widest varint, every optional field present, and the shape
/// the convex one of the most points, `most`.
fn largest<V: Vector + Serialize>(most: Convex<V>) -> [(usize, usize, &'static str); 13] {
    let half = V::ZERO;
    let shape = Shape::Convex(most);
    let kind = ColliderKind::Sensor;
    let mut up = V::ZERO;
    up[1] = 1.0;
    let cut = Cut::new(up, 0.0);
    // Built field by field: only a box takes a cut, so this collider
    // occupies nothing, but it is still data a game can send.
    let collider = Collider {
        id: u64::MAX,
        shape,
        position: half,
        kind,
        layers: u32::MAX,
        cut: Some(cut),
        velocity: half,
        push: Push::Soft(1.0),
    };
    let separation = Separation {
        distance: 1.0,
        normal: up,
    };
    let contacts = Contacts::<V> {
        last_floor_id: Some(u64::MAX),
        last_wall_id: Some(u64::MAX),
        last_ceiling_id: Some(u64::MAX),
        carried_by: Some(u64::MAX),
        pushed_by: Some(u64::MAX),
        ..Contacts::default()
    };
    let hit = Hit {
        t: 1.0,
        normal: half,
        id: u64::MAX,
    };
    let config = mover(up, 0.01, 0.1, 0.7, 1e-4);
    let floor = Floor {
        normal: up,
        id: u64::MAX,
    };
    let state = MoverState {
        floor: Some(floor),
        ..MoverState::default()
    };
    let ledge = Ledge {
        hang: up,
        stand: up,
        id: u64::MAX,
    };
    [
        (
            encoded(&state),
            MoverState::<V>::MAX_POSTCARD_BYTES,
            "MoverState",
        ),
        (encoded(&floor), Floor::<V>::MAX_POSTCARD_BYTES, "Floor"),
        (encoded(&ledge), Ledge::<V>::MAX_POSTCARD_BYTES, "Ledge"),
        (encoded(&shape), Shape::<V>::MAX_POSTCARD_BYTES, "Shape"),
        (encoded(&most), Convex::<V>::MAX_POSTCARD_BYTES, "Convex"),
        (
            encoded(&separation),
            Separation::<V>::MAX_POSTCARD_BYTES,
            "Separation",
        ),
        (
            encoded(&kind),
            ColliderKind::MAX_POSTCARD_BYTES,
            "ColliderKind",
        ),
        (encoded(&cut), Cut::<V>::MAX_POSTCARD_BYTES, "Cut"),
        (encoded(&Push::Soft(1.0)), Push::MAX_POSTCARD_BYTES, "Push"),
        (
            encoded(&collider),
            Collider::<V>::MAX_POSTCARD_BYTES,
            "Collider",
        ),
        (
            encoded(&contacts),
            Contacts::<V>::MAX_POSTCARD_BYTES,
            "Contacts",
        ),
        (encoded(&hit), Hit::<V>::MAX_POSTCARD_BYTES, "Hit"),
        (
            encoded(&config),
            MoverConfig::<V>::MAX_POSTCARD_BYTES,
            "MoverConfig",
        ),
    ]
}

/// Defect: a field added to a state type without its documented largest
/// size growing, so a game that budgets `sgl-net`'s latest-state payload by
/// the documented size overflows it. Oracle: postcard's own encoder, on the
/// largest value of each type, must produce exactly the documented size.
#[wasm_bindgen_test(unsupported = test)]
fn the_largest_of_each_state_type_encodes_at_its_documented_size() {
    // A convex octagon, the most vertices a 2D polygon takes, and sixteen
    // points, the most a 3D hull takes.
    let octagon = [
        (2.0, 0.0),
        (1.4, 1.4),
        (0.0, 2.0),
        (-1.4, 1.4),
        (-2.0, 0.0),
        (-1.4, -1.4),
        (0.0, -2.0),
        (1.4, -1.4),
    ]
    .map(|(x, y)| Vec2::new(x, y));
    let hull: Vec<Vec3> = (0u8..16)
        .map(|i| {
            let i = f32::from(i);
            Vec3::new(i, i * i, -i)
        })
        .collect();
    let flat = largest(Convex::polygon(&octagon, 1.0, 0.0));
    let deep = largest(Convex::hull(&hull, Quat::IDENTITY));
    for (size, documented, name) in flat.into_iter().chain(deep) {
        assert_eq!(size, documented, "{name}");
    }
}
