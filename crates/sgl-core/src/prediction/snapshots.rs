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
#[derive(Debug, Clone)]
pub struct SnapshotBuffer<S> {
    /// Strictly increasing ticks.
    snapshots: VecDeque<(u64, S)>,
    capacity: usize,
    playback: Playback,
    extrapolations: u64,
}

/// What [`SnapshotBuffer::sample`] drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampled<S> {
    /// The instant is before the oldest snapshot held: that snapshot.
    Early(S),
    /// The instant lies between two snapshots, or on one.
    Interpolated(S),
    /// The instant is past the newest snapshot, within the extrapolation
    /// bound: extrapolated from the last two.
    Extrapolated(S),
    /// The instant is past the bound (or past the only snapshot): held where
    /// extrapolation stopped (or at that snapshot).
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

    /// Samples that ran past the newest snapshot ([`Sampled::Extrapolated`]
    /// or [`Sampled::Held`] there).
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
            return Some(Sampled::Early(self.snapshots[0].1.clone()));
        };
        // The newest snapshot at or before the instant; ticks are integers
        // and `alpha < 1`, so comparing whole ticks is exact.
        let at = self.snapshots.partition_point(|&(tick, _)| tick <= base) - 1;
        if at + 1 < self.snapshots.len() {
            self.snapshots.drain(..at);
            let ((a_tick, a), (b_tick, b)) = (&self.snapshots[0], &self.snapshots[1]);
            let t = ((base - a_tick) as f64 + alpha) / (b_tick - a_tick) as f64;
            return Some(Sampled::Interpolated(interpolate(a, b, t as f32)));
        }

        // Past the newest: keep it and the one before for extrapolation.
        self.snapshots.drain(..at.saturating_sub(1));
        let newest_tick = self.newest_tick().expect("checked nonempty");
        let past = (base - newest_tick) as f64 + alpha;
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
        let ((before_tick, before), (_, newest)) = (&self.snapshots[0], &self.snapshots[1]);
        let span = (newest_tick - before_tick) as f64;
        let bound = self.playback.max_extrapolation_ticks as f64;
        let along = |past: f64| (1.0 + past / span) as f32;
        Some(if past <= bound {
            Sampled::Extrapolated(interpolate(before, newest, along(past)))
        } else {
            Sampled::Held(interpolate(before, newest, along(bound)))
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
