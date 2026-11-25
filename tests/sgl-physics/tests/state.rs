//! The state a game keeps is plain data it can send and restore
//! (physics.md, "pure and plain" and "ready for the network"). Runs natively
//! and in a wasm runtime.
#![allow(clippy::cast_precision_loss)]

use serde::{Deserialize, Serialize};
use sgl_core::math::{Vec2, Vec3};
use sgl_core::{SplitMix64, digest_of};
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderKind, ColliderSet, Contacts, Hit, MoverConfig, MoverSettings,
    Shape, Vector, move_and_slide, snap_to_ground,
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
    let (moved, contacts) = move_and_slide(
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
/// mask at its widest varint, every optional field present.
fn largest<V: Vector + Serialize>() -> [(usize, usize, &'static str); 6] {
    let half = V::ZERO;
    let shape = Shape::Box { half };
    let kind = ColliderKind::Sensor;
    let collider = Collider::new(u64::MAX, shape, half)
        .with_kind(kind)
        .with_layers(u32::MAX);
    let contacts = Contacts::<V> {
        last_floor_id: Some(u64::MAX),
        last_wall_id: Some(u64::MAX),
        last_ceiling_id: Some(u64::MAX),
        ..Contacts::default()
    };
    let hit = Hit {
        t: 1.0,
        normal: half,
        id: u64::MAX,
    };
    let mut up = V::ZERO;
    up[1] = 1.0;
    let config = mover(up, 0.01, 0.1, 0.7, 1e-4);
    [
        (encoded(&shape), Shape::<V>::MAX_POSTCARD_BYTES, "Shape"),
        (
            encoded(&kind),
            ColliderKind::MAX_POSTCARD_BYTES,
            "ColliderKind",
        ),
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
    for (size, documented, name) in largest::<Vec2>().into_iter().chain(largest::<Vec3>()) {
        assert_eq!(size, documented, "{name}");
    }
}
