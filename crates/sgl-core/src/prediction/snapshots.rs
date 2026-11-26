use std::collections::VecDeque;

/// The largest `f32` below 1, so a sampled instant stays inside its tick.
const ALPHA_MAX: f32 = 1.0 - f32::EPSILON / 2.0;

/// How a [`SnapshotBuffer`] plays its snapshots back, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playback {
    /// How far behind the game's current tick snapshots are drawn, so one
    /// arriving late is still in time.
    pub delay_ticks: u64,
    /// How far past the newest snapshot the buffer extrapolates before it
    /// holds still.
    pub max_extrapolation_ticks: u64,
}

/// Snapshots by tick of something the client does not predict (another
/// player, an actor, a boat), drawn a fixed delay in the past.
///
/// [`push`](Self::push) each snapshot as it arrives; only snapshots newer
/// than the newest held are kept, so a late or duplicate one never moves
/// time backwards. [`sample`](Self::sample) at the game's current tick and
/// `alpha` draws the instant [`Playback::delay_ticks`] earlier, with the
/// game's `interpolate(a, b, t)`: between two snapshots `t` is the fraction
/// of the way from `a` to `b`; past the newest snapshot the buffer has run
/// dry and passes `t > 1` along the last two snapshots to extrapolate, for at
/// most [`Playback::max_extrapolation_ticks`], and says that it did.
///
/// Sampling drops the snapshots that instant has passed. The buffer holds at
/// most `capacity` snapshots; when it is full a new one is refused.
///
/// A track that fills while nothing draws it, that several views draw, or
/// whose snapshots carry their own velocity takes the other pair instead.
/// [`insert_recent`](Self::insert_recent) holds the snapshots of the newest
/// `capacity` ticks, in tick order whatever order they arrive in.
/// [`sample_with`](Self::sample_with) draws the same instant without
/// changing the buffer, and past the newest snapshot calls the game's
/// `extrapolate(newest, ticks)` on that snapshot alone. Either pair works on
/// any buffer; a track normally keeps to one.
#[derive(Debug, Clone)]
pub struct SnapshotBuffer<S> {
    /// Strictly increasing ticks.
    snapshots: VecDeque<(u64, S)>,
    capacity: usize,
    playback: Playback,
    extrapolations: u64,
}

/// What [`SnapshotBuffer::sample`] or [`SnapshotBuffer::sample_with`] drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampled<S> {
    /// The instant is before the oldest snapshot held: that snapshot.
    Early(S),
    /// The instant lies between two snapshots, or on one.
    Interpolated(S),
    /// The instant is past the newest snapshot, within the extrapolation
    /// bound: extrapolated from the last two by `sample`, from the newest
    /// alone by `sample_with`.
    Extrapolated(S),
    /// The instant is past the bound: held where extrapolation stopped.
    /// `sample` also holds past the only snapshot it has, at that snapshot.
    Held(S),
}

impl<S> Sampled<S> {
    /// The state drawn, whatever its kind.
    #[must_use]
    pub fn into_state(self) -> S {
        match self {
            Self::Early(s) | Self::Interpolated(s) | Self::Extrapolated(s) | Self::Held(s) => s,
        }
    }
}

/// What [`SnapshotBuffer::insert_recent`] did with a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Inserted {
    /// Held at a tick not held before. A full buffer dropped its oldest
    /// snapshot for it.
    Added,
    /// Its tick was already held; it took that snapshot's place.
    Replaced,
    /// The buffer is full and every snapshot held is newer; dropped.
    TooOld,
}

/// Where a drawn instant falls among the snapshots held.
enum Place {
    /// Before the oldest snapshot.
    Early,
    /// `t` of the way from snapshot `at` to the one after it, `t` in
    /// `[0, 1)`.
    Between { at: usize, t: f32 },
    /// `ticks` past the newest snapshot; zero on it.
    PastNewest { ticks: f64 },
}

/// Why [`SnapshotBuffer::push`] refused a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRejected {
    /// Not newer than the newest snapshot held: late or a duplicate.
    NotNewer { tick: u64, newest: u64 },
    /// The buffer holds `capacity` snapshots.
    Full { capacity: usize },
}

impl<S: Clone> SnapshotBuffer<S> {
    /// An empty buffer holding at most `capacity` snapshots.
    ///
    /// # Panics
    ///
    /// If `capacity` is below 3: the two snapshots playback keeps to draw
    /// from, and room for the next to arrive.
    ///
    /// To accept every snapshot of a steady stream, size it for the delay:
    /// the snapshots that arrive in `delay_ticks`, plus three.
    #[must_use]
    pub fn new(capacity: usize, playback: Playback) -> Self {
        assert!(capacity >= 3, "SnapshotBuffer: capacity must be at least 3");
        Self {
            snapshots: VecDeque::with_capacity(capacity),
            capacity,
            playback,
            extrapolations: 0,
        }
    }

    /// The buffer's playback settings.
    #[must_use]
    pub fn playback(&self) -> Playback {
        self.playback
    }

    /// The most snapshots it holds.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Snapshots held: the buffer's depth.
    #[must_use]
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether no snapshot is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// The tick of the newest snapshot held.
    #[must_use]
    pub fn newest_tick(&self) -> Option<u64> {
        self.snapshots.back().map(|&(tick, _)| tick)
    }

    /// The newest snapshot held, with its tick.
    #[must_use]
    pub fn newest(&self) -> Option<(u64, &S)> {
        self.snapshots.back().map(|(tick, state)| (*tick, state))
    }

    /// [`sample`](Self::sample)s that ran past the newest snapshot
    /// ([`Sampled::Extrapolated`] or [`Sampled::Held`] there).
    /// [`sample_with`](Self::sample_with) cannot change the buffer and is
    /// not counted; a game counts the kinds it returns.
    #[must_use]
    pub fn extrapolations(&self) -> u64 {
        self.extrapolations
    }

    /// Drop every snapshot, keeping the settings and the counters. For when
    /// the stream starts again: a new match, a server whose ticks restarted,
    /// or a snapshot whose tick was wrong and now blocks every later one as
    /// [`SnapshotRejected::NotNewer`].
    pub fn clear(&mut self) {
        self.snapshots.clear();
    }

    /// Hold `snapshot`, the state at `tick`.
    pub fn push(&mut self, tick: u64, snapshot: S) -> Result<(), SnapshotRejected> {
        if let Some(newest) = self.newest_tick().filter(|&newest| tick <= newest) {
            return Err(SnapshotRejected::NotNewer { tick, newest });
        }
        if self.snapshots.len() == self.capacity {
            return Err(SnapshotRejected::Full {
                capacity: self.capacity,
            });
        }
        self.snapshots.push_back((tick, snapshot));
        Ok(())
    }

    /// Hold `snapshot`, the state at `tick`, among the snapshots of the
    /// newest `capacity` ticks, in tick order. It never refuses the newest:
    /// a full buffer drops its oldest snapshot for a newer tick, whether or
    /// not anything has been sampled. A late snapshot takes its place in
    /// order, one at a tick already held replaces that snapshot, and one
    /// older than every tick of a full buffer is dropped.
    pub fn insert_recent(&mut self, tick: u64, snapshot: S) -> Inserted {
        let at = self.snapshots.partition_point(|&(held, _)| held < tick);
        if let Some((_, held)) = self.snapshots.get_mut(at).filter(|(held, _)| *held == tick) {
            *held = snapshot;
            return Inserted::Replaced;
        }
        if self.snapshots.len() < self.capacity {
            self.snapshots.insert(at, (tick, snapshot));
            return Inserted::Added;
        }
        if at == 0 {
            return Inserted::TooOld;
        }
        self.snapshots.pop_front();
        self.snapshots.insert(at - 1, (tick, snapshot));
        Inserted::Added
    }

    /// Where the instant `now_tick + alpha - delay_ticks` falls. `None`
    /// when no snapshot is held.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn place(&self, now_tick: u64, alpha: f32) -> Option<Place> {
        let alpha = if alpha.is_nan() {
            0.0
        } else {
            f64::from(alpha.clamp(0.0, ALPHA_MAX))
        };
        let &(oldest, _) = self.snapshots.front()?;
        let Some(base) = now_tick
            .checked_sub(self.playback.delay_ticks)
            .filter(|&base| base >= oldest)
        else {
            return Some(Place::Early);
        };
        // The newest snapshot at or before the instant; ticks are integers
        // and `alpha < 1`, so comparing whole ticks is exact.
        let at = self.snapshots.partition_point(|&(tick, _)| tick <= base) - 1;
        let a_tick = self.snapshots[at].0;
        let ticks = (base - a_tick) as f64 + alpha;
        Some(match self.snapshots.get(at + 1) {
            Some(&(b_tick, _)) => Place::Between {
                at,
                t: (ticks / (b_tick - a_tick) as f64) as f32,
            },
            None => Place::PastNewest { ticks },
        })
    }

    /// Draw the instant `now_tick + alpha - delay_ticks` with
    /// `interpolate(a, b, t)`. `alpha` is the fixed clock's, clamped to
    /// `[0, 1)` (NaN reads as 0). `None` when no snapshot is held.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn sample(
        &mut self,
        now_tick: u64,
        alpha: f32,
        interpolate: impl FnOnce(&S, &S, f32) -> S,
    ) -> Option<Sampled<S>> {
        let past = match self.place(now_tick, alpha)? {
            Place::Early => return Some(Sampled::Early(self.snapshots[0].1.clone())),
            Place::Between { at, t } => {
                self.snapshots.drain(..at);
                let (a, b) = (&self.snapshots[0].1, &self.snapshots[1].1);
                return Some(Sampled::Interpolated(interpolate(a, b, t)));
            }
            Place::PastNewest { ticks } => ticks,
        };

        // Past the newest: keep it and the one before for extrapolation.
        self.snapshots
            .drain(..self.snapshots.len().saturating_sub(2));
        if past == 0.0 {
            return self
                .snapshots
                .back()
                .map(|(_, s)| Sampled::Interpolated(s.clone()));
        }
        self.extrapolations += 1;
        if self.snapshots.len() < 2 {
            return Some(Sampled::Held(self.snapshots[0].1.clone()));
        }
        let ((before_tick, before), (newest_tick, newest)) =
            (&self.snapshots[0], &self.snapshots[1]);
        let span = (newest_tick - before_tick) as f64;
        let bound = self.playback.max_extrapolation_ticks as f64;
        let along = |past: f64| (1.0 + past / span) as f32;
        Some(if past <= bound {
            Sampled::Extrapolated(interpolate(before, newest, along(past)))
        } else {
            Sampled::Held(interpolate(before, newest, along(bound)))
        })
    }

    /// Draw the instant [`sample`](Self::sample) draws, without changing
    /// the buffer: any number of views sample it, in any order, and each
    /// draws what it would have drawn alone.
    ///
    /// Between two snapshots it calls `interpolate(a, b, t)`. Past the
    /// newest it calls `extrapolate(newest, ticks)` on that snapshot alone,
    /// so one snapshot that carries its velocity is enough to move: `ticks`
    /// is how far past it the instant is, and never more than
    /// [`Playback::max_extrapolation_ticks`]. Within that bound the result
    /// is [`Sampled::Extrapolated`]; beyond it, [`Sampled::Held`] with the
    /// state at the bound, which a game may choose not to draw.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn sample_with(
        &self,
        now_tick: u64,
        alpha: f32,
        interpolate: impl FnOnce(&S, &S, f32) -> S,
        extrapolate: impl FnOnce(&S, f32) -> S,
    ) -> Option<Sampled<S>> {
        Some(match self.place(now_tick, alpha)? {
            Place::Early => Sampled::Early(self.snapshots[0].1.clone()),
            Place::Between { at, t } => Sampled::Interpolated(interpolate(
                &self.snapshots[at].1,
                &self.snapshots[at + 1].1,
                t,
            )),
            Place::PastNewest { ticks } => {
                let newest = &self.snapshots[self.snapshots.len() - 1].1;
                let bound = self.playback.max_extrapolation_ticks as f64;
                if ticks == 0.0 {
                    Sampled::Interpolated(newest.clone())
                } else if ticks <= bound {
                    Sampled::Extrapolated(extrapolate(newest, ticks as f32))
                } else {
                    Sampled::Held(extrapolate(newest, bound as f32))
                }
            }
        })
    }
}

impl std::fmt::Display for SnapshotRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotNewer { tick, newest } => {
                write!(
                    f,
                    "snapshot for tick {tick} is not newer than tick {newest}"
                )
            }
            Self::Full { capacity } => {
                write!(f, "snapshot buffer already holds {capacity} snapshots")
            }
        }
    }
}

impl std::error::Error for SnapshotRejected {}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A snapshot that remembers what it was made from: `(a, b, t)` for an
    /// interpolation, `(tick, tick, 0)` for a snapshot as received.
    type Made = (u64, u64, f32);

    fn interpolate(a: &Made, b: &Made, t: f32) -> Made {
        (a.0, b.0, t)
    }

    fn snapshot(tick: u64) -> Made {
        (tick, tick, 0.0)
    }

    /// Snapshots at ticks 10, 14 and 20, drawn 3 ticks behind; extrapolation
    /// stops 3 ticks past the newest.
    fn buffer() -> SnapshotBuffer<Made> {
        let playback = Playback {
            delay_ticks: 3,
            max_extrapolation_ticks: 3,
        };
        let mut buffer = SnapshotBuffer::new(4, playback);
        for tick in [10, 14, 20] {
            buffer.push(tick, snapshot(tick)).unwrap();
        }
        buffer
    }

    /// Oracle, by hand: at tick 15 and alpha 0.5 the instant is 12.5, which
    /// is (12.5 − 10) / (14 − 10) = 0.625 of the way from 10 to 14; at
    /// tick 19 and alpha 0.25 it is 16.25, (16.25 − 14) / 6 = 0.375 of the
    /// way from 14 to 20. Neither counts as extrapolation.
    #[wasm_bindgen_test(unsupported = test)]
    fn between_two_snapshots_it_interpolates_exactly_those_two() {
        let mut buffer = buffer();
        let sampled = buffer.sample(15, 0.5, interpolate);
        assert_eq!(sampled, Some(Sampled::Interpolated((10, 14, 0.625))));
        let sampled = buffer.sample(19, 0.25, interpolate);
        assert_eq!(sampled, Some(Sampled::Interpolated((14, 20, 0.375))));
        assert_eq!(buffer.extrapolations(), 0);
    }

    /// Before the oldest snapshot the oldest is drawn as it is, and exactly
    /// on the newest the newest is.
    #[wasm_bindgen_test(unsupported = test)]
    fn outside_the_pairs_it_draws_the_end_snapshots() {
        let mut buffer = buffer();
        assert_eq!(
            buffer.sample(12, 0.9, interpolate),
            Some(Sampled::Early(snapshot(10)))
        );
        assert_eq!(
            buffer.sample(23, 0.0, interpolate),
            Some(Sampled::Interpolated(snapshot(20)))
        );
        assert_eq!(buffer.extrapolations(), 0);
    }

    /// Oracle, by hand: past the newest (20), along the last pair (14 to
    /// 20, 6 ticks): the instant 21.5 is 1 + 1.5 / 6 = 1.25 of the way;
    /// 23 is the bound, 1 + 3 / 6 = 1.5; 25.5 is past it and holds at the
    /// bound. Each counts once.
    #[wasm_bindgen_test(unsupported = test)]
    fn past_the_newest_it_extrapolates_no_further_than_its_bound() {
        let mut buffer = buffer();
        assert_eq!(
            buffer.sample(24, 0.5, interpolate),
            Some(Sampled::Extrapolated((14, 20, 1.25)))
        );
        assert_eq!(
            buffer.sample(26, 0.0, interpolate),
            Some(Sampled::Extrapolated((14, 20, 1.5)))
        );
        assert_eq!(
            buffer.sample(28, 0.5, interpolate),
            Some(Sampled::Held((14, 20, 1.5)))
        );
        assert_eq!(buffer.extrapolations(), 3);
    }

    /// A late snapshot (older than the newest) and a duplicate (with other
    /// contents) are refused, and the buffer draws what it drew before.
    #[wasm_bindgen_test(unsupported = test)]
    fn late_and_duplicate_snapshots_do_not_move_time_backwards() {
        let mut buffer = buffer();
        let before = buffer.clone().sample(19, 0.25, interpolate);
        assert_eq!(
            buffer.push(12, snapshot(12)),
            Err(SnapshotRejected::NotNewer {
                tick: 12,
                newest: 20
            })
        );
        assert_eq!(
            buffer.push(20, (99, 99, 0.0)),
            Err(SnapshotRejected::NotNewer {
                tick: 20,
                newest: 20
            })
        );
        assert_eq!(buffer.newest_tick(), Some(20));
        assert_eq!(buffer.sample(19, 0.25, interpolate), before);
    }

    /// A full buffer refuses the newest snapshot rather than dropping any;
    /// once sampling has passed the oldest, there is room again.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_buffer_refuses_until_playback_frees_room() {
        let mut buffer = buffer();
        buffer.push(26, snapshot(26)).unwrap();
        assert_eq!(
            buffer.push(30, snapshot(30)),
            Err(SnapshotRejected::Full { capacity: 4 })
        );
        assert_eq!(buffer.len(), 4);
        let _ = buffer.sample(18, 0.0, interpolate);
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.push(30, snapshot(30)), Ok(()));
    }

    /// A buffer sized by the rule in [`SnapshotBuffer::new`] (the delay in
    /// snapshots, plus three) accepts every snapshot of a stream that is
    /// sampled as it arrives.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_buffer_sized_for_its_delay_accepts_every_snapshot() {
        for delay_ticks in 0..=4 {
            let playback = Playback {
                delay_ticks,
                max_extrapolation_ticks: 3,
            };
            let capacity = usize::try_from(delay_ticks).unwrap() + 3;
            let mut buffer = SnapshotBuffer::new(capacity, playback);
            for tick in 0..40 {
                assert_eq!(buffer.push(tick, snapshot(tick)), Ok(()), "tick {tick}");
                let _ = buffer.sample(tick, 0.0, interpolate);
            }
        }
    }

    /// The smallest buffer is too small for a delay of two ticks, so it
    /// refuses some snapshots, but never for good: sampling always frees
    /// room, and the stream is still being taken at the end.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_smallest_buffer_never_refuses_for_good() {
        let playback = Playback {
            delay_ticks: 2,
            max_extrapolation_ticks: 3,
        };
        let mut buffer = SnapshotBuffer::new(3, playback);
        let mut refused_in_a_row = 0;
        for tick in 0..40 {
            if buffer.push(tick, snapshot(tick)).is_ok() {
                refused_in_a_row = 0;
            } else {
                refused_in_a_row += 1;
            }
            assert!(refused_in_a_row < 2, "tick {tick}");
            let _ = buffer.sample(tick, 0.0, interpolate);
        }
    }

    /// What `extrapolate` was given: the snapshot's tick twice, and the
    /// ticks past it.
    fn extrapolate(newest: &Made, ticks: f32) -> Made {
        (newest.0, newest.0, ticks)
    }

    /// A position and the velocity the server sent with it, in units a
    /// tick.
    type Moving = (f64, f64);

    fn coast(newest: &Moving, ticks: f32) -> Moving {
        (newest.0 + newest.1 * f64::from(ticks), newest.1)
    }

    /// For samples that must not reach one of the two functions.
    fn never<S>(_: &S, _: &S, _: f32) -> S {
        unreachable!("no pair of snapshots to interpolate")
    }

    /// Oracle, by hand (#928): one snapshot at tick 10, at 5 and moving 1 a
    /// tick, drawn with no delay and a bound of 3 ticks. At tick 12 it has
    /// moved 2, to 7; at 11.5, 1.5, to 6.5; at 13, the bound, 3, to 8. At
    /// 13.5 and at 40 it is past the bound and holds at 8. `sample` cannot
    /// move one snapshot and holds it where it was; the buffer is the same
    /// after every `sample_with`.
    #[wasm_bindgen_test(unsupported = test)]
    fn one_snapshot_moves_along_its_own_velocity_up_to_the_bound() {
        let playback = Playback {
            delay_ticks: 0,
            max_extrapolation_ticks: 3,
        };
        let mut buffer: SnapshotBuffer<Moving> = SnapshotBuffer::new(4, playback);
        assert_eq!(buffer.sample_with(12, 0.0, never, coast), None);
        assert_eq!(buffer.insert_recent(10, (5.0, 1.0)), Inserted::Added);
        let drawn = |now_tick, alpha| buffer.sample_with(now_tick, alpha, never, coast);
        assert_eq!(drawn(10, 0.0), Some(Sampled::Interpolated((5.0, 1.0))));
        assert_eq!(drawn(12, 0.0), Some(Sampled::Extrapolated((7.0, 1.0))));
        assert_eq!(drawn(11, 0.5), Some(Sampled::Extrapolated((6.5, 1.0))));
        assert_eq!(drawn(13, 0.0), Some(Sampled::Extrapolated((8.0, 1.0))));
        assert_eq!(drawn(13, 0.5), Some(Sampled::Held((8.0, 1.0))));
        assert_eq!(drawn(40, 0.0), Some(Sampled::Held((8.0, 1.0))));
        assert_eq!(buffer.extrapolations(), 0);
        assert_eq!(
            buffer.sample(12, 0.0, never),
            Some(Sampled::Held((5.0, 1.0)))
        );
    }

    /// Oracle, by hand: snapshots at 10, 14 and 20, drawn 3 ticks behind
    /// with a bound of 3. Past the newest the extrapolator gets snapshot 20
    /// alone and the ticks past it: 1.5 at the instant 21.5, 3 at the bound
    /// (23), and still 3, held, at 25.5. A delay longer than the game's
    /// tick draws the oldest.
    #[wasm_bindgen_test(unsupported = test)]
    fn past_the_newest_the_extrapolator_gets_it_and_the_bounded_ticks() {
        let buffer = buffer();
        let drawn = |now_tick, alpha| buffer.sample_with(now_tick, alpha, interpolate, extrapolate);
        assert_eq!(drawn(24, 0.5), Some(Sampled::Extrapolated((20, 20, 1.5))));
        assert_eq!(drawn(26, 0.0), Some(Sampled::Extrapolated((20, 20, 3.0))));
        assert_eq!(drawn(28, 0.5), Some(Sampled::Held((20, 20, 3.0))));
        assert_eq!(drawn(23, 0.0), Some(Sampled::Interpolated(snapshot(20))));
        assert_eq!(drawn(2, 0.5), Some(Sampled::Early(snapshot(10))));
    }

    /// Three views draw one buffer (10, 14, 20; 3 ticks behind) at their own
    /// instants: 21.5 (1.5 past the newest), 12.5 (0.625 of the way from 10
    /// to 14) and 9.9 (before the oldest). Oracle: each draws what it would
    /// alone, whichever is drawn first and however often. `sample`, drawn
    /// latest first, has dropped snapshot 10 by the second view.
    #[wasm_bindgen_test(unsupported = test)]
    fn views_sampling_in_any_order_each_draw_their_own_instant() {
        let buffer = buffer();
        let views = [
            (24, 0.5, Sampled::Extrapolated((20, 20, 1.5))),
            (15, 0.5, Sampled::Interpolated((10, 14, 0.625))),
            (12, 0.9, Sampled::Early(snapshot(10))),
        ];
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2], [0, 0, 1]] {
            for (now_tick, alpha, expected) in order.map(|view| views[view]) {
                let drawn = buffer.sample_with(now_tick, alpha, interpolate, extrapolate);
                assert_eq!(drawn, Some(expected), "order {order:?}");
            }
        }
        assert_eq!(buffer.len(), 3);

        let mut consumed = buffer.clone();
        let _ = consumed.sample(24, 0.5, interpolate);
        assert_eq!(
            consumed.sample(15, 0.5, interpolate),
            Some(Sampled::Early(snapshot(14)))
        );
    }

    /// Oracle (#928): a track of 32 that nothing draws takes a 33rd, newer
    /// snapshot in place of its oldest. After ticks 1 to 33 it holds 32,
    /// its newest is 33 and its oldest is 2 (what a draw before every
    /// snapshot gets). `push` refuses the 33rd.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_buffer_takes_a_newer_snapshot_with_no_draw_between() {
        let playback = Playback {
            delay_ticks: 3,
            max_extrapolation_ticks: 6,
        };
        let mut rolling = SnapshotBuffer::new(32, playback);
        let mut strict = SnapshotBuffer::new(32, playback);
        assert_eq!(rolling.newest(), None);
        for tick in 1..=32 {
            assert_eq!(rolling.insert_recent(tick, snapshot(tick)), Inserted::Added);
            strict.push(tick, snapshot(tick)).unwrap();
        }
        assert_eq!(rolling.insert_recent(33, snapshot(33)), Inserted::Added);
        assert_eq!(rolling.len(), 32);
        assert_eq!(rolling.newest(), Some((33, &snapshot(33))));
        assert_eq!(
            rolling.sample_with(0, 0.0, interpolate, extrapolate),
            Some(Sampled::Early(snapshot(2)))
        );
        assert_eq!(
            strict.push(33, snapshot(33)),
            Err(SnapshotRejected::Full { capacity: 32 })
        );
    }

    /// Oracle, by hand: a buffer of 4 holding 10 and 20 takes a late 14 in
    /// tick order, so the instant 12 is halfway from 10 to 14 and 17 is
    /// halfway from 14 to 20. Full at 10, 14, 20, 26, a late 12 is among
    /// the newest four ticks seen (12, 14, 20, 26), so 10 goes and 13 is
    /// halfway from 12 to 14; a late 5 is older than all four and changes
    /// nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_late_snapshot_is_held_in_tick_order_unless_older_than_a_full_buffer() {
        let playback = Playback {
            delay_ticks: 0,
            max_extrapolation_ticks: 3,
        };
        let mut buffer = SnapshotBuffer::new(4, playback);
        for tick in [10, 20, 14] {
            assert_eq!(buffer.insert_recent(tick, snapshot(tick)), Inserted::Added);
        }
        let drawn = |buffer: &SnapshotBuffer<Made>, now_tick| {
            buffer.sample_with(now_tick, 0.0, interpolate, extrapolate)
        };
        assert_eq!(
            drawn(&buffer, 12),
            Some(Sampled::Interpolated((10, 14, 0.5)))
        );
        assert_eq!(
            drawn(&buffer, 17),
            Some(Sampled::Interpolated((14, 20, 0.5)))
        );

        assert_eq!(buffer.insert_recent(26, snapshot(26)), Inserted::Added);
        assert_eq!(buffer.insert_recent(12, snapshot(12)), Inserted::Added);
        assert_eq!(buffer.len(), 4);
        assert_eq!(drawn(&buffer, 0), Some(Sampled::Early(snapshot(12))));
        assert_eq!(
            drawn(&buffer, 13),
            Some(Sampled::Interpolated((12, 14, 0.5)))
        );

        let before = buffer.clone();
        assert_eq!(buffer.insert_recent(5, snapshot(5)), Inserted::TooOld);
        assert_eq!(buffer.len(), 4);
        for now_tick in [0, 13, 17, 23, 26] {
            assert_eq!(drawn(&buffer, now_tick), drawn(&before, now_tick));
        }
        assert_eq!(buffer.newest(), Some((26, &snapshot(26))));
    }

    /// A snapshot at a tick already held, newest or not, replaces the one
    /// held and adds none: the buffer draws the new contents at that tick.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_snapshot_at_a_held_tick_replaces_the_one_held() {
        let playback = Playback {
            delay_ticks: 0,
            max_extrapolation_ticks: 3,
        };
        let mut buffer = SnapshotBuffer::new(3, playback);
        for tick in [10, 14, 20] {
            assert_eq!(buffer.insert_recent(tick, snapshot(tick)), Inserted::Added);
        }
        assert_eq!(buffer.insert_recent(20, (99, 99, 0.0)), Inserted::Replaced);
        assert_eq!(buffer.insert_recent(10, (77, 77, 0.0)), Inserted::Replaced);
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.newest(), Some((20, &(99, 99, 0.0))));
        assert_eq!(
            buffer.sample_with(12, 0.0, interpolate, extrapolate),
            Some(Sampled::Interpolated((77, 14, 0.5)))
        );
        assert_eq!(
            buffer.sample_with(21, 0.0, interpolate, extrapolate),
            Some(Sampled::Extrapolated((99, 99, 1.0)))
        );
    }

    /// A snapshot with a wrong tick blocks every later one until the game
    /// clears the buffer; the counters survive the clear.
    #[wasm_bindgen_test(unsupported = test)]
    fn clearing_recovers_from_a_tick_that_blocks_the_stream() {
        let mut buffer = SnapshotBuffer::new(
            4,
            Playback {
                delay_ticks: 0,
                max_extrapolation_ticks: 2,
            },
        );
        buffer.push(1, snapshot(1)).unwrap();
        let _ = buffer.sample(2, 0.0, interpolate);
        assert_eq!(buffer.extrapolations(), 1);
        buffer.push(u64::MAX, snapshot(0)).unwrap();
        assert!(buffer.push(3, snapshot(3)).is_err());
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.extrapolations(), 1);
        assert_eq!(buffer.push(3, snapshot(3)), Ok(()));
    }
}
