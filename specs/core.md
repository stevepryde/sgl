# Core

`sgl-core` is the deterministic foundation: fixed-step time, hashing, seeded
RNG, grids and frame animation. A native server and a browser client must get
identical results from it.

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
   seed, with the same values on every target, but its stream is not frozen
   across dependency upgrades.
5. **Grid.** `Grid2` is row-major with `u16` dimensions; an oversized or
   overflowing construction returns `GridError`, never panics. `get` is `Some`
   exactly inside the bounds.
6. **Frame animation.** `FrameAnimation` plays an explicit frame-order array at
   a fixed fps with `Once` or `Repeat`; frame `i` shows for exactly one frame
   duration. `AnimationSequence` plays scripted steps (frame runs, pauses,
   random pauses, action hooks) with once, repeat, and ping-pong loops; only
   random-pause steps draw from the caller's `Rng`, so a seeded run replays
   exactly. A random pause lies in the inclusive `[min, max]` range;
   floating-point rounding can reach `max`. Pauses, action steps, and completion hold the last frame reached
   during playback, even when a tick crosses that frame without stopping on
   it. `current_frame()` is `None` only before the first frame is reached after
   construction or reset. Ping-pong turns on the first and last frame steps
   and never plays either end frame twice; steps outside them play once per
   turnaround.
   Every tick returns: a nonpositive or non-finite `dt` is ignored without
   accumulating time, advancing steps, or drawing from `Rng`. Each sequence
   tick still clears the prior completion edge and action queue. In a
   repeating loop a tick whose duration is too small for `f32` to subtract
   from the accumulated time drops the remainder. So does a repeating cycle that
   consumes no time, after as many zero-time advances as it has playback
   positions (frames plus other steps, doubled for ping-pong), by when every
   position has played at least once.
7. *Moved.* Collision is `sgl-physics`'s ([physics](physics.md),
   [D-94](decisions.md)).
8. Overflow checks are on in every profile; arithmetic on caller sizes must
   fail as an error or be checked, not wrap.
9. **Prediction.** `prediction` keeps the bookkeeping of client prediction
   and snapshot interpolation; the game gives its step and interpolation
   functions as closures and keeps its loop, messages, authority and when to
   snap (D-95). Ticks are `u64`: tick `t`'s input is applied by the step
   that runs `t`, and acknowledging `t` sends the state after that step.
   Every buffer has a capacity fixed at construction and never grows or
   drops the newest; a full one refuses with a typed error, except a
   `SnapshotBuffer` filled through `insert_recent`, which drops its oldest
   (D-97).
   `Predictor::reconcile` drops the inputs through the acknowledged tick,
   takes the server's state and replays the rest, so reconciling with the
   true state ends exactly where not reconciling would. Its sample's
   previous state becomes the replayed state a step before the end, so
   interpolation cannot hide a correction; when nothing replays, a state that
   agrees with the prediction leaves the sample as it was and one that
   differs becomes the sample at rest. No tick a peer supplies panics: a
   predictor at `u64::MAX` refuses the input and a queue there stays there. An acknowledgement older than the history or not yet
   predicted is refused as `Behind` or `Ahead` and changes nothing.
   `InputQueue` holds a client's inputs at most `capacity` ticks ahead and
   hands each tick out once, in order: its own input if it arrived, else the
   last one taken; it keeps the first of duplicates and drops inputs for
   ticks already run. `skip_to` moves only forward: it discards the inputs
   of the ticks it passes, which are never handed out or repeated, and
   keeps the rest. `Correction` and `SnapshotBuffer` are presentation and
   never change simulated state. A correction fades linearly from what was
   added to exactly zero over the game's fade time, never growing or
   changing sign. `SnapshotBuffer::push` keeps only snapshots newer than its
   newest and `sample` draws `delay_ticks` behind the game's tick with
   `interpolate(a, b, t)` on the two snapshots around the instant; past the
   newest it extrapolates along the last two (`t > 1`) for at most
   `max_extrapolation_ticks`, then holds, and counts each such sample. It
   drops the snapshots the instant has passed. `insert_recent` and
   `sample_with` are the other policy, for a track that fills while nothing
   draws it, that several views draw, or whose snapshots carry their own
   velocity. `insert_recent` holds the snapshots of the newest `capacity`
   ticks it was given, in tick order: a full buffer drops its oldest for a
   newer tick, a snapshot at a held tick replaces the one held, and one
   older than every tick of a full buffer is dropped. `sample_with` draws
   the same instant as `sample` and changes nothing, so samples in any
   order each draw what they would alone: `interpolate` between two
   snapshots, and past the newest `extrapolate(newest, ticks)` on that
   snapshot alone, one snapshot being enough, with `ticks` the time past it
   and at most `max_extrapolation_ticks`: `Extrapolated` within the bound,
   `Held` with the state at the bound beyond it.

## Acceptance

- The frozen fixtures in `tests/sgl-core/tests/parity.rs` produce identical
  values on native and `wasm32`.
- A seeded `AnimationSequence`, `FixedClock`, and `SplitMix64` replay
  identically from the same inputs.
