/// A simulated body's state at the previous and the current fixed step, for
/// drawing between steps.
///
/// Draw `blend(alpha, lerp)` with the fixed clock's `alpha`. A
/// [`Predictor`](super::Predictor) keeps one for its predicted state and
/// replaces both halves on a reconcile, so interpolation never blends from a
/// state the server has corrected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample<S> {
    pub(super) previous: S,
    pub(super) current: S,
}

impl<S> Sample<S> {
    /// A sample at rest: previous and current are both `state`.
    #[must_use]
    pub fn new(state: S) -> Self
    where
        S: Clone,
    {
        Self {
            previous: state.clone(),
            current: state,
        }
    }

    /// Record the state after one more fixed step: the current state
    /// becomes the previous one.
    pub fn push(&mut self, next: S) {
        self.previous = std::mem::replace(&mut self.current, next);
    }

    /// Put the body at rest at `state` (a spawn or a teleport), so nothing
    /// is interpolated from where it was.
    pub fn reset(&mut self, state: S)
    where
        S: Clone,
    {
        *self = Self::new(state);
    }

    /// The state one fixed step before [`current`](Self::current).
    #[must_use]
    pub fn previous(&self) -> &S {
        &self.previous
    }

    /// The state after the latest fixed step.
    #[must_use]
    pub fn current(&self) -> &S {
        &self.current
    }

    /// `interpolate(previous, current, alpha)`: the body `alpha` of the way
    /// from the previous step to the current one.
    pub fn blend<T>(&self, alpha: f32, interpolate: impl FnOnce(&S, &S, f32) -> T) -> T {
        interpolate(&self.previous, &self.current, alpha)
    }
}
