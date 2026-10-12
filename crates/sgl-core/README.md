# sgl-core

Deterministic game building blocks with no I/O, clocks, threads, graphics,
or dependencies on other SGL crates. The game supplies time, seeds, units,
and state. Native clients, browser clients, and headless servers share the
same API.

## Choose a module

| Need | API |
| --- | --- |
| Fixed-step simulation and interpolation | [`time::FixedClock`](src/time.rs) |
| Schema-driven canonical hashing and raw byte digests | [`StateHasher`, `CanonicalWrite`, `Digest`](src/hash.rs) |
| Frozen seeded random stream | [`SplitMix64`, `derive_stream_seed`](src/rng.rs) |
| General seeded randomness | [`random::Rng`](src/random.rs) |
| Bounded row-major grid | [`Grid2`](src/grid.rs) |
| Frame animation and scripted sequences | [`anim`](src/anim.rs) |
| Swept AABB, kinematic sliding, sensors, and one-way platforms | [`collision`](src/collision.rs) |
| Shared 2D vector and geometry helpers | [`math`](src/math.rs) |

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

`SplitMix64` and canonical hashes have frozen cross-target contracts.
`random::Rng` is deterministic for a seed, the same on every target, within
its dependency version;
its stream is not promised across dependency upgrades. Choose according to
the game's replay or persistence requirements.

`anim::SequenceStep::RandomPause` holds for a duration in the inclusive
`[min, max]` range; floating-point rounding can reach `max`.

`FrameAnimation::tick` and `AnimationSequence::tick` advance only for positive,
finite elapsed seconds. Zero, negative, and non-finite deltas are ignored;
they do not create time debt or trigger sequence steps or random draws.
Each sequence tick still clears the previous completion edge and action queue.

Collision units and the up axis belong to the game. Supply them through
`CollisionConfig`; SGL does not choose a world scale or entity layout.
Its `up` direction must have a finite, nonzero normalizable length;
`CollisionConfig::new` panics if the length overflows even when its components are finite.
Contacts leave the configured `skin` gap along the surface normal by stopping
earlier on the swept path. A diagonal slide keeps the tangential motion left
after the contact time; the backed-off motion is discarded, so it also stops
earlier along the surface. Ground snapping backs off along its probe too.
Contacts at the exact end of a sweep are included, even on a broadphase
cell boundary. Every finite nonzero motion is swept, including motion below
`f32::EPSILON` in the game's units; unobstructed motion is applied in full.
`Aabb::from_min_max` computes center/half-extents, so its
reconstructed corners may differ from the supplied corners by float rounding.
`collision` is 2D arcade physics: it moves bodies the game drives, with no
forces, mass or rotation. See [Physics](../../docs/README.md#physics) to
choose a game's physics.

Read the [core contract](../../specs/core.md) before changing deterministic
behavior. [Parity fixtures](tests/parity.rs) and [property tests](tests/properties.rs)
exercise the cross-target and algorithm boundaries.

Run `cargo test -p sgl-core` for focused native checks. The repository's
[required check](../../CONTRIBUTING.md#validate) also runs the WASM lane.
For dependency setup, see [Building games with SGL](../../docs/README.md).
