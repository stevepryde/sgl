//! Frozen fixtures executed natively and in a wasm runtime: the values a
//! native server and a browser client must agree on (core.md acceptance).
//! Floats are compared by bit pattern.

use sgl_core::collision::{Aabb, sweep_aabb};
use sgl_core::math::Vec2;
use sgl_core::time::FixedClock;
use sgl_core::{SplitMix64, StateHasher};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test(unsupported = test)]
fn rng_and_digest_fixture_is_target_independent() {
    let mut rng = SplitMix64::new(0);
    let first = rng.next_u64();
    let second = rng.next_u64();
    let mut hasher = StateHasher::new();
    hasher.u64(first);
    hasher.u64(second);
    hasher.bool(true);
    hasher.bytes(b"wasm parity fixture");
    assert_eq!(first, 0xe220_a839_7b1d_cdaf);
    assert_eq!(second, 0x6e78_9e6a_a1b9_65f4);
    assert_eq!(
        hasher.finish().to_hex(),
        "af0a3288c337fbf4fc0d09befe8350acef89f0fffd1455bbbd4efa259f2a6dd0"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn typed_writes_and_late_rng_draws_are_target_independent() {
    let mut hasher = StateHasher::new();
    hasher.u8(7);
    hasher.u16(0xbeef);
    hasher.u32(0xdead_beef);
    hasher.i32(-12_345);
    hasher.i64(i64::MIN);
    hasher.bool(false);
    hasher.sequence(&[1u32, 2, 3]);
    hasher.bytes(&[]);
    assert_eq!(
        hasher.finish().to_hex(),
        "7a7373ca6ac1ed53e706076e11083911b4ffd9f213f807347a680e3154da995d"
    );
    let mut rng = SplitMix64::new(0x5eed);
    let thousandth = (0..1000).map(|_| rng.next_u64()).last().unwrap();
    assert_eq!(thousandth, 15_036_098_347_904_298_379);
    assert_eq!(rng.range_inclusive(10, 20), 10);
}

#[wasm_bindgen_test(unsupported = test)]
fn fixed_clock_cadence_is_target_independent() {
    let mut clock = FixedClock::with_hz(60.0);
    let mut steps = 0u32;
    for frame in 0..240u32 {
        // A render loop wobbling between 30 and 144 Hz with two stalls.
        let dt = match frame % 7 {
            0 => 1.0 / 30.0,
            1 => 1.0 / 144.0,
            2 => 0.25,
            _ => 1.0 / 59.94,
        };
        clock.begin_frame(dt);
        while clock.step() {
            steps += 1;
        }
        clock.finish();
    }
    assert_eq!(steps, 219);
    assert_eq!(clock.alpha.to_bits(), 1_058_362_684);
}

#[wasm_bindgen_test(unsupported = test)]
fn swept_aabb_hit_is_target_independent() {
    let body = Aabb {
        center: Vec2::new(-3.25, 0.5),
        half: Vec2::new(0.5, 0.75),
    };
    let target = Aabb {
        center: Vec2::new(1.0, 0.0),
        half: Vec2::new(1.0, 1.0),
    };
    let hit = sweep_aabb(&body, Vec2::new(7.3, 1.1), &target).expect("the sweep hits");
    assert_eq!(hit.t.to_bits(), 1_052_827_760);
    assert_eq!(hit.normal, Vec2::NEG_X);
}
