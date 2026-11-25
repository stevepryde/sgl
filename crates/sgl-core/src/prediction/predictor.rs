use std::collections::VecDeque;

use super::Sample;

/// A client's predicted state and the inputs the server has not yet
/// acknowledged.
///
/// Each fixed step the game calls [`predict`](Self::predict) with that
/// tick's input and its step function, sends [`pending`](Self::pending) to
/// the server (all of it, or a bounded tail), and on each authoritative
/// snapshot calls [`reconcile`](Self::reconcile) with the acknowledged tick
/// and the server's state. The step function is `step(state, tick, input)`
/// and must be the one the server runs.
///
/// The history holds at most `capacity` inputs. When the server has
/// acknowledged none of them, `predict` refuses the next with
/// [`PredictError::HistoryFull`]: the game decides whether to wait or to snap with
/// [`reset`](Self::reset).
#[derive(Debug, Clone)]
pub struct Predictor<I, S> {
    sample: Sample<S>,
    /// Unacknowledged inputs, oldest first, for the ticks just before
    /// `next_tick`.
    inputs: VecDeque<I>,
    capacity: usize,
    next_tick: u64,
    corrections: u64,
}

/// What a successful [`Predictor::reconcile`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconciled {
    /// Pending inputs stepped again from the server's state.
    pub replayed: usize,
    /// Whether the reconciled state differs from the one predicted before.
    pub corrected: bool,
}

/// Why [`Predictor::predict`] refused an input. The predictor is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictError {
    /// The history already holds `capacity` unacknowledged inputs.
    HistoryFull { capacity: usize },
    /// The next tick is `u64::MAX`, which has no successor. Only a tick
    /// taken from a peer ([`Predictor::reset`]) can get here.
    TicksExhausted,
}

/// Why [`Predictor::reconcile`] could not replay. The predictor is
/// unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileError {
    /// The acknowledgement is older than the history: inputs after
    /// `acked_tick` were already dropped by a newer one, so they cannot be
    /// replayed. Typically a snapshot that arrived late.
    Behind {
        acked_tick: u64,
        oldest_pending: u64,
    },
    /// The server acknowledged a tick this client has not predicted.
    Ahead { acked_tick: u64, next_tick: u64 },
}

impl<I, S: Clone> Predictor<I, S> {
    /// A predictor at `state` whose first input is for `next_tick`, holding
    /// at most `capacity` unacknowledged inputs.
    ///
    /// # Panics
    ///
    /// If `capacity` is zero.
    #[must_use]
    pub fn new(state: S, next_tick: u64, capacity: usize) -> Self {
        assert!(capacity > 0, "Predictor: capacity must be at least 1");
        Self {
            sample: Sample::new(state),
            inputs: VecDeque::with_capacity(capacity),
            capacity,
            next_tick,
            corrections: 0,
        }
    }

    /// The predicted state after every input recorded so far.
    #[must_use]
    pub fn state(&self) -> &S {
        &self.sample.current
    }

    /// The predicted state and the one a step before it, for drawing.
    #[must_use]
    pub fn sample(&self) -> &Sample<S> {
        &self.sample
    }

    /// The tick the next [`predict`](Self::predict) records.
    #[must_use]
    pub fn next_tick(&self) -> u64 {
        self.next_tick
    }

    /// The most unacknowledged inputs the history holds.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Unacknowledged inputs with their ticks, oldest first: what the game
    /// sends. Its length is the inputs pending.
    pub fn pending(&self) -> impl DoubleEndedIterator<Item = (u64, &I)> + ExactSizeIterator {
        let oldest = self.oldest_pending();
        self.inputs
            .iter()
            .enumerate()
            .map(move |(index, input)| (oldest + index as u64, input))
    }

    /// Reconciles that changed the predicted state.
    #[must_use]
    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    /// Record `input` for [`next_tick`](Self::next_tick) and step the state
    /// with it. Returns the tick it was recorded for.
    pub fn predict(
        &mut self,
        input: I,
        mut step: impl FnMut(&mut S, u64, &I),
    ) -> Result<u64, PredictError> {
        if self.inputs.len() == self.capacity {
            return Err(PredictError::HistoryFull {
                capacity: self.capacity,
            });
        }
        let tick = self.next_tick;
        self.next_tick = tick.checked_add(1).ok_or(PredictError::TicksExhausted)?;
        self.sample.previous.clone_from(&self.sample.current);
        step(&mut self.sample.current, tick, &input);
        self.inputs.push_back(input);
        Ok(tick)
    }

    /// Take the server's `state` after `acked_tick`, drop the inputs up to
    /// and including it, and replay the rest. The sample's previous state
    /// becomes the replayed state a step before the end. When nothing is
    /// replayed, a state that agrees with the prediction leaves the sample
    /// as it was, and one that differs becomes the sample at rest.
    ///
    /// `acked_tick` must lie between one tick before the oldest pending
    /// input and the newest predicted tick; otherwise the predictor is left
    /// unchanged and the error says which way it missed, handing `state`
    /// back for a [`reset`](Self::reset).
    pub fn reconcile(
        &mut self,
        acked_tick: u64,
        state: S,
        mut step: impl FnMut(&mut S, u64, &I),
    ) -> Result<Reconciled, (ReconcileError, S)>
    where
        S: PartialEq,
    {
        if acked_tick >= self.next_tick {
            return Err((
                ReconcileError::Ahead {
                    acked_tick,
                    next_tick: self.next_tick,
                },
                state,
            ));
        }
        // `acked_tick < next_tick`, so this cannot overflow.
        let first_replayed = acked_tick + 1;
        let oldest = self.oldest_pending();
        if first_replayed < oldest {
            return Err((
                ReconcileError::Behind {
                    acked_tick,
                    oldest_pending: oldest,
                },
                state,
            ));
        }
        let acknowledged = usize::try_from(first_replayed - oldest)
            .expect("acknowledged inputs are within the history");
        self.inputs.drain(..acknowledged);

        let mut current = state;
        let mut previous = None;
        let replayed = self.inputs.len();
        for (tick, input) in (first_replayed..).zip(&self.inputs) {
            if tick + 1 == self.next_tick {
                previous = Some(current.clone());
            }
            step(&mut current, tick, input);
        }
        let corrected = current != self.sample.current;
        self.corrections += u64::from(corrected);
        match previous {
            Some(previous) => self.sample = Sample { previous, current },
            // Nothing replayed and the server disagrees: snap, at rest.
            None if corrected => self.sample = Sample::new(current),
            // Nothing replayed and the server agrees: the sample stands, so
            // drawing still blends from the step before.
            None => {}
        }
        Ok(Reconciled {
            replayed,
            corrected,
        })
    }

    /// Snap to `state` with `next_tick` as the next input's tick, dropping
    /// every pending input. For when the game decides not to replay: a
    /// respawn, a teleport, or a reconcile that failed.
    pub fn reset(&mut self, state: S, next_tick: u64) {
        self.sample.reset(state);
        self.inputs.clear();
        self.next_tick = next_tick;
    }

    /// The tick of the oldest pending input, or `next_tick` when none is.
    fn oldest_pending(&self) -> u64 {
        // Each held input was recorded for one of the ticks just before
        // `next_tick`, so this never underflows.
        self.next_tick - self.inputs.len() as u64
    }
}

impl std::fmt::Display for PredictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HistoryFull { capacity } => write!(
                f,
                "prediction history is full: {capacity} inputs await acknowledgement"
            ),
            Self::TicksExhausted => write!(f, "the next tick is u64::MAX"),
        }
    }
}

impl std::error::Error for PredictError {}

impl std::fmt::Display for ReconcileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Behind {
                acked_tick,
                oldest_pending,
            } => write!(
                f,
                "acknowledged tick {acked_tick} is older than the history, \
                 which starts at tick {oldest_pending}"
            ),
            Self::Ahead {
                acked_tick,
                next_tick,
            } => write!(
                f,
                "acknowledged tick {acked_tick} has not been predicted; \
                 the next predicted tick is {next_tick}"
            ),
        }
    }
}

impl std::error::Error for ReconcileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A step whose result depends on the order of inputs and on the tick.
    /// The predictor hands it the input by reference, whatever its type.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn step(state: &mut i64, tick: u64, input: &i64) {
        *state = *state * 2 + input + i64::try_from(tick).unwrap() * 100;
    }

    /// Inputs 1, 2, 3 predicted at ticks 10, 11, 12 from 0.
    fn predicted() -> Predictor<i64, i64> {
        let mut predictor = Predictor::new(0, 10, 4);
        for input in [1, 2, 3] {
            predictor.predict(input, step).unwrap();
        }
        predictor
    }

    /// A wrong prediction is corrected: the server says the state after
    /// tick 10 is 5. Oracle, by hand: tick 11 gives 5·2 + 2 + 1100 = 1112,
    /// tick 12 gives 1112·2 + 3 + 1200 = 3427; the step before is 1112.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_wrong_prediction_is_replayed_from_the_servers_state() {
        let mut predictor = predicted();
        let reconciled = predictor.reconcile(10, 5, step).unwrap();
        assert_eq!(
            reconciled,
            Reconciled {
                replayed: 2,
                corrected: true
            }
        );
        assert_eq!(*predictor.state(), 3427);
        assert_eq!(*predictor.sample().previous(), 1112);
        assert_eq!(predictor.corrections(), 1);
        let pending: Vec<_> = predictor.pending().map(|(t, i)| (t, *i)).collect();
        assert_eq!(pending, [(11, 2), (12, 3)]);
    }

    /// Acknowledging every input with a state that differs leaves the
    /// server's state at rest, with nothing pending and nothing to
    /// interpolate from.
    #[wasm_bindgen_test(unsupported = test)]
    fn acknowledging_everything_takes_the_servers_state() {
        let mut predictor = predicted();
        let reconciled = predictor.reconcile(12, 7, step).unwrap();
        assert_eq!(reconciled.replayed, 0);
        assert_eq!(predictor.sample(), &Sample::new(7));
        assert_eq!(predictor.pending().len(), 0);
    }

    /// Acknowledging every input with the state the client predicted keeps
    /// the sample, so a client whose server answers within the tick still
    /// draws between steps. Oracle, by hand: tick 10 gives 0·2 + 1 + 1000 =
    /// 1001, tick 11 gives 2002 + 2 + 1100 = 3104, tick 12 gives 6208 + 3 +
    /// 1200 = 7411; the step before the end is 3104.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_agreeing_acknowledgement_of_everything_keeps_the_sample() {
        let mut predictor = predicted();
        let reconciled = predictor.reconcile(12, 7411, step).unwrap();
        assert_eq!(
            reconciled,
            Reconciled {
                replayed: 0,
                corrected: false
            }
        );
        assert_eq!(*predictor.sample().previous(), 3104);
        assert_eq!(*predictor.sample().current(), 7411);
    }

    /// A tick taken from a peer can be `u64::MAX`; predicting from it is
    /// refused, never a panic, and changes nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_last_tick_refuses_an_input_instead_of_panicking() {
        let mut predictor = predicted();
        predictor.reset(9, u64::MAX);
        assert_eq!(
            predictor.predict(1, step),
            Err(PredictError::TicksExhausted)
        );
        assert_eq!(*predictor.state(), 9);
        assert_eq!(predictor.next_tick(), u64::MAX);
        assert_eq!(predictor.pending().len(), 0);
    }

    /// A reconcile that cannot replay reports which way it missed and
    /// changes nothing. After acknowledging tick 11, tick 10 is behind (its
    /// successor, 11, is gone) but 11 again is replayable; tick 13 was
    /// never predicted.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_acknowledgement_outside_the_history_is_refused() {
        let mut predictor = predicted();
        predictor.reconcile(11, 50, step).unwrap();
        let before = predictor.clone();
        assert_eq!(
            predictor.reconcile(10, 77, step),
            Err((
                ReconcileError::Behind {
                    acked_tick: 10,
                    oldest_pending: 12
                },
                77
            ))
        );
        assert_eq!(
            predictor.reconcile(13, 78, step),
            Err((
                ReconcileError::Ahead {
                    acked_tick: 13,
                    next_tick: 13
                },
                78
            ))
        );
        assert_eq!(predictor.sample(), before.sample());
        assert_eq!(predictor.pending().len(), 1);
        assert_eq!(predictor.reconcile(11, 50, step).unwrap().replayed, 1);
    }

    /// A full history refuses the next input without stepping, and takes it
    /// once an acknowledgement frees a slot.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_history_refuses_the_next_input() {
        let mut predictor = Predictor::new(0, 0, 2);
        predictor.predict(1, step).unwrap();
        predictor.predict(1, step).unwrap();
        let state = *predictor.state();
        assert_eq!(
            predictor.predict(1, step),
            Err(PredictError::HistoryFull { capacity: 2 })
        );
        assert_eq!(*predictor.state(), state);
        assert_eq!(predictor.next_tick(), 2);
        predictor.reconcile(0, 1, step).unwrap();
        assert_eq!(predictor.predict(1, step), Ok(2));
    }
}
