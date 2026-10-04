# Core

`sgl-core` is the deterministic foundation: fixed-step time, hashing, seeded
RNG, grids, frame animation, and swept-AABB collision. A native server and a
browser client must get identical results from it.

## Requirements

1. `sgl-core` has no I/O, no clocks, no threads, and no other SGL crate as a
   dependency. Every operation takes its `dt`, seed, or `Rng` from the caller.
2. **Time.** `FixedClock` runs the simulation in constant `1/hz` steps. The
   per-frame delta is clamped to one fixed step, so a frame runs at most one
   step and the cadence never oscillates near the fixed rate. `alpha` is the
   overstep fraction in `[0, 1)`. Default rate is 60 Hz; the game may set any.
3. **Hashing.** `StateHasher` is a canonical, length-prefixed BLAKE3 encoding
   of typed writes: different write sequences produce different digests, and
   the digest for a given sequence is frozen across versions and targets.
   `Digest::hash_bytes` is raw BLAKE3 of the bytes.
4. **RNG.** `SplitMix64` is a frozen stream: a seed produces the same values on
   every version and target. `derive_stream_seed` gives distinct seeds for
   distinct `(domain, a, b)`. `Rng` (the `fastrand` seam) is deterministic per
   seed but its stream is not frozen across dependency upgrades.
5. **Grid.** `Grid2` is row-major with `u16` dimensions; an oversized or
   overflowing construction returns `GridError`, never panics. `get` is `Some`
   exactly inside the bounds.
6. **Frame animation.** `FrameAnimation` plays an explicit frame-order array at
   a fixed fps with `Once` or `Repeat`; frame `i` shows for exactly one frame
   duration. `AnimationSequence` plays scripted steps (frame runs, pauses,
   random pauses, action hooks) with once, repeat, and ping-pong loops; only
   random-pause steps draw from the caller's `Rng`, so a seeded run replays
   exactly. Pauses, action steps, and completion hold the last frame reached
   during playback, even when a tick crosses that frame without stopping on
   it. `current_frame()` is `None` only before the first frame is reached after
   construction or reset. Ping-pong never plays either end frame twice.
7. **Collision.** `sweep_aabb` is a closed-form swept AABB test returning
   `t ∈ [0, 1]` and an axis-aligned unit normal pointing from the surface
   toward the body. `move_and_collide` slides a kinematic body against a
   `ColliderSet` and never leaves it overlapping a solid collider; sensors
   never block; one-way platforms block only a landing on the face along the
   configured up axis. `CollisionConfig` names the up axis, skin, snap
   distance, and thresholds in the caller's units; results mirror exactly
   between y-up and y-down worlds. `ColliderSet::query` returns every
   collider overlapping the region (it may return more).
8. Overflow checks are on in every profile; arithmetic on caller sizes must
   fail as an error or be checked, not wrap.

## Acceptance

- The frozen fixtures in `crates/sgl-core/tests/parity.rs` produce identical
  values on native and `wasm32`.
- A seeded `AnimationSequence`, `FixedClock`, and `SplitMix64` replay
  identically from the same inputs.
