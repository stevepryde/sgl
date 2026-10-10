# Core

`sgl-core` is the deterministic foundation: fixed-step time, hashing, seeded
RNG, grids, frame animation, and swept-AABB collision. A native server and a
browser client must get identical results from it.

## Requirements

1. `sgl-core` has no I/O, no clocks, no threads, and no other SGL crate as a
   dependency. Every operation takes its `dt`, seed, or `Rng` from the caller.
2. **Time.** `FixedClock` runs the simulation in constant `1/hz` steps from
   caller-supplied frame time. Time is exact: the step is
   `Duration::from_secs_f64(1.0 / f64::from(hz))` and frame time accumulates
   as whole-nanosecond `Duration`s, so equal inputs give equal step counts on
   every target. Default rate is 60 Hz; the game may set any.
   The default policy is render-paced: the per-frame delta is clamped to one
   fixed step, so a frame runs at most one step and the cadence never
   oscillates near the fixed rate, but simulation falls behind real time
   whenever frames are longer than a step. The opt-in `CatchUp` policy
   accumulates the whole delta, so simulation follows elapsed time: a frame
   runs at most `max_steps_per_frame` steps, carries at most
   `max_debt_steps` whole due steps to later frames, and discards the rest.
   An authoritative or networked simulation chooses `CatchUp`; a client
   whose simulation only feeds its own presentation keeps the default.
   Supplied time is exactly simulated, held (under a step plus any debt), or
   reported in `dropped_dt`. `alpha` is the fractional overstep in `[0, 1)`,
   excluding whole steps of debt.
3. **Hashing.** `StateHasher` is a canonical, schema-driven BLAKE3 encoding:
   each write appends its fixed-width little-endian bytes untagged, and byte
   strings and sequences carry a `u32` length prefix. The schema is the shape
   of the primitive writes (which write methods, in what order). Two write
   sequences with the same schema produce different digests whenever their
   values differ. A `CanonicalWrite` impl is self-delimiting: its write shape
   depends only on values it has already written, so it writes a length
   before variable-length data and a tag before an optional value or enum
   variant. Sequences with different schemas may encode alike (`u16(0x1234)`
   equals `u8(0x34); u8(0x12)`), so a caller that hashes several kinds of
   state in one stream or changes its schema writes its own leading tag or
   version. The digest for a given sequence is frozen across versions and
   targets. `Digest::hash_bytes` is raw BLAKE3 of the bytes.
4. **RNG.** `SplitMix64` is a frozen stream: a seed produces the same values on
   every version and target. `derive_stream_seed` absorbs `base`, `domain`,
   `a` and `b` in turn, each mixed by a `SplitMix64` step: changing any one
   of them always changes the seed, and no two can cancel (inputs differing
   in several share a seed only by 64-bit chance). `Rng` (the `fastrand` seam) is deterministic per
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
   construction or reset. Ping-pong turns on the first and last frame steps
   and never plays either end frame twice; steps outside them play once per
   turnaround.
   Every tick returns: a non-finite `dt` is ignored, and in a repeating loop
   a tick whose duration is too small for `f32` to subtract from the
   accumulated time drops the remainder. So does a repeating cycle that
   consumes no time, after as many zero-time advances as it has playback
   positions (frames plus other steps, doubled for ping-pong), by when every
   position has played at least once.
7. **Collision.** `sweep_aabb` is a closed-form swept AABB test returning
   `t ∈ [0, 1]` and an axis-aligned unit normal pointing from the surface
   toward the body. `move_and_collide` slides a kinematic body against a
   `ColliderSet` and never moves it more than a rounding step into a solid
   collider; a body that starts inside one may move out or along it, and its
   least penetration depth never grows. It does not push bodies out: the game
   keeps them clear. Sensors never block; one-way platforms block only a
   landing on the face along the configured up axis, including a body within
   `skin` (plus rounding) below that face, never a body deeper inside them.
   `CollisionConfig` names the up axis, skin, snap distance, and
   thresholds in the caller's units, rejecting negative or non-finite
   lengths; results mirror exactly between y-up and y-down worlds.
   `ColliderSet::query` returns every collider overlapping the region (it may
   return more). Neither `insert` nor `query` walks an unbounded cell range:
   an insert buckets at most a fixed number of cells, and a query visits at
   most about as many cells as the set holds colliders, plus the oversized
   colliders. An inverted box (negative half-extents) addresses no cells.
8. Overflow checks are on in every profile; arithmetic on caller sizes must
   fail as an error or be checked, not wrap.

## Acceptance

- The frozen fixtures in `crates/sgl-core/tests/parity.rs` produce identical
  values on native and `wasm32`.
- A seeded `AnimationSequence`, `FixedClock`, and `SplitMix64` replay
  identically from the same inputs.
