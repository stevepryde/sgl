# sgl-core

Deterministic game building blocks with no I/O, clocks, threads, graphics,
or dependencies on other SGL crates. The game supplies time, seeds, units,
and state. Native clients, browser clients, and headless servers share the
same API.

## Choose a module

| Need | API |
| --- | --- |
| Fixed-step simulation and interpolation | [`time::FixedClock`](src/time.rs) |
| Client prediction, server input queues and snapshot interpolation | [`prediction`](src/prediction.rs) |
| Schema-driven canonical hashing and raw byte digests | [`StateHasher`, `CanonicalWrite`, `Digest`](src/hash.rs) |
| Frozen seeded random stream | [`SplitMix64`, `derive_stream_seed`](src/rng.rs) |
| General seeded randomness | [`random::Rng`](src/random.rs) |
| Bounded row-major grid | [`Grid2`](src/grid.rs) |
| Frame animation and scripted sequences | [`anim`](src/anim.rs) |
| Shared vector and geometry types | [`math`](src/math.rs) |

`FixedClock::with_hz` is render-paced: it clamps each frame delta to one
fixed step and runs at most one simulation step per frame, so its cadence
stays steady but simulation slows when frames are longer than a step. A
simulation that must keep pace with elapsed time (an authoritative server or
a networked client) uses `FixedClock::with_catch_up` instead: a `CatchUp`
policy sets the most steps one frame runs and how many due steps carry to
later frames (`max_debt_steps: 0` discards everything past the frame's
budget). `begin_frame` takes the frame's elapsed `Duration` and the clock
accumulates it exactly in whole nanoseconds; `fixed_step()` is the exact step
and `fixed_dt()` the same in `f32` seconds. `dropped_dt` reports supplied time
that will never be simulated.
Use its interpolation fraction for presentation, keeping game simulation
separate from rendering.

`prediction` keeps the bookkeeping of a networked game on a shared fixed
step; what is predicted, the messages, authority and when to snap stay the
game's. A client's `Predictor` steps each tick's input at once with the
game's step function, keeps it until the server acknowledges that tick, and
on each snapshot replays the rest from the server's state (`reconcile`), or
says `Behind` or `Ahead` when it cannot. Its `sample()` is the previous and
current state to draw at the clock's `alpha`, and a `Correction` fades a
reconcile's visual jump over a time the game chooses. The server takes one
input a tick from each client's `InputQueue`, which repeats the last on a
gap and drops late ones; a server that falls behind reads the queued range
with `first_queued_tick` and `last_queued_tick` and jumps ahead with
`skip_to`, which discards what it passes without running or repeating it.
A `SnapshotBuffer` draws what the client does not
predict a few ticks behind with the game's interpolation function, and
extrapolates a bounded number of ticks when snapshots run out. Its `push`
and `sample` suit one stream drawn once a frame: a full buffer refuses, and
sampling drops what it has passed. A track that fills while nothing draws
it, that several views draw, or whose snapshots carry their own velocity
uses `insert_recent` and `sample_with` instead: the buffer keeps the newest
`capacity` ticks in tick order (a late snapshot takes its place, one at a
held tick replaces it), sampling changes nothing, and past the newest
snapshot the game's `extrapolate(newest, ticks)` moves that snapshot alone,
returned as `Sampled::Extrapolated` within the bound and `Sampled::Held`
beyond it, which a game may choose not to draw:

```rust
track.insert_recent(tick, state);
let drawn = track.sample_with(now_tick, alpha, State::lerp, |newest, ticks| {
    newest.moved(newest.velocity * ticks * TICK_SECONDS)
});
```

Capacities, delays and the fade time are the game's, and a full buffer
refuses unless `insert_recent` is what fills it. For
diagnostics: `Predictor::corrections` and `pending().len()`,
`InputQueue::gaps` and `late`, `SnapshotBuffer::len` and `extrapolations`.

`SplitMix64` and canonical hashes have frozen cross-target contracts.
`random::Rng` is deterministic for a seed, the same on every target, within
its dependency version;
its stream is not promised across dependency upgrades. Choose according to
the game's replay or persistence requirements. `Rng::u32`, `i32`, and `usize`
panic on an empty range; check that a slice is nonempty before drawing an index.

`anim::SequenceStep::RandomPause` holds for a duration in the inclusive
`[min, max]` range; floating-point rounding can reach `max`.

`FrameAnimation::tick` and `AnimationSequence::tick` advance only for positive,
finite elapsed seconds. Zero, negative, and non-finite deltas are ignored;
they do not create time debt or trigger sequence steps or random draws.
Each sequence tick still clears the previous completion edge and action queue.

Read the [core contract](../../specs/core.md) before changing deterministic
behavior. [Parity fixtures](../../tests/sgl-core/tests/parity.rs) and [property tests](../../tests/sgl-core/tests/properties.rs)
exercise the cross-target and algorithm boundaries.

Run `cargo test -p sgl-core -p sgl-core-tests` for focused native checks. The repository's
[required check](../../CONTRIBUTING.md#validate) also runs the WASM lane.
For dependency setup, see [Building games with SGL](../../docs/README.md).
