use std::collections::VecDeque;

/// A server's queue of one client's inputs by tick.
///
/// The server [`push`](Self::push)es each input as it arrives, in any order
/// and as often as the client resends it, and [`take`](Self::take)s one
/// each tick it runs. A tick whose input has not arrived repeats the last
/// input taken; an input for a tick already run is dropped. The queue holds
/// inputs for at most `capacity` ticks ahead of the next one to run and
/// refuses inputs beyond that with [`QueueFull`].
#[derive(Debug, Clone)]
pub struct InputQueue<I> {
    /// Slot `k` holds the input for tick `next_tick + k`.
    slots: VecDeque<Option<I>>,
    capacity: usize,
    next_tick: u64,
    last: Option<I>,
    queued: usize,
    gaps: u64,
    late: u64,
}

/// What [`InputQueue::push`] did with an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Pushed {
    /// Queued for its tick.
    Queued,
    /// Its tick already had an input; the first one is kept.
    Duplicate,
    /// Its tick has already run; dropped.
    Late,
}

/// The input for one tick, as [`InputQueue::take`] hands it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickInput<'a, I> {
    /// The client's input for this tick.
    Received(&'a I),
    /// No input arrived in time: the last one taken, repeated.
    Repeated(&'a I),
    /// No input arrived in time and none has been taken before.
    Missing,
}

impl<'a, I> TickInput<'a, I> {
    /// The input to run, received or repeated.
    #[must_use]
    pub fn input(self) -> Option<&'a I> {
        match self {
            Self::Received(input) | Self::Repeated(input) => Some(input),
            Self::Missing => None,
        }
    }
}

/// [`InputQueue::push`] refused an input more than `capacity` ticks ahead
/// of `next_tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFull {
    pub tick: u64,
    pub next_tick: u64,
    pub capacity: usize,
}

impl<I> InputQueue<I> {
    /// A queue whose first tick to run is `next_tick`, holding inputs for
    /// at most `capacity` ticks from the next one to run.
    ///
    /// # Panics
    ///
    /// If `capacity` is zero.
    #[must_use]
    pub fn new(next_tick: u64, capacity: usize) -> Self {
        assert!(capacity > 0, "InputQueue: capacity must be at least 1");
        Self {
            slots: VecDeque::with_capacity(capacity),
            capacity,
            next_tick,
            last: None,
            queued: 0,
            gaps: 0,
            late: 0,
        }
    }

    /// The tick the next [`take`](Self::take) runs.
    #[must_use]
    pub fn next_tick(&self) -> u64 {
        self.next_tick
    }

    /// The most ticks ahead the queue holds.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Inputs queued for ticks not yet run.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued
    }

    /// Whether no input is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued == 0
    }

    /// Ticks taken without their own input (repeated or missing).
    #[must_use]
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// Inputs dropped because their tick had already run.
    #[must_use]
    pub fn late(&self) -> u64 {
        self.late
    }

    /// Queue `input` for `tick`.
    pub fn push(&mut self, tick: u64, input: I) -> Result<Pushed, QueueFull> {
        let Some(ahead) = tick.checked_sub(self.next_tick) else {
            self.late += 1;
            return Ok(Pushed::Late);
        };
        let index = usize::try_from(ahead)
            .ok()
            .filter(|&index| index < self.capacity)
            .ok_or(QueueFull {
                tick,
                next_tick: self.next_tick,
                capacity: self.capacity,
            })?;
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || None);
        }
        let slot = &mut self.slots[index];
        if slot.is_some() {
            return Ok(Pushed::Duplicate);
        }
        *slot = Some(input);
        self.queued += 1;
        Ok(Pushed::Queued)
    }

    /// Run the next tick: returns it with its input. A queue started at a
    /// tick taken from a peer can reach `u64::MAX`; it stays there and never
    /// panics.
    pub fn take(&mut self) -> (u64, TickInput<'_, I>) {
        let tick = self.next_tick;
        self.next_tick = tick.saturating_add(1);
        if let Some(input) = self.slots.pop_front().flatten() {
            self.queued -= 1;
            return (tick, TickInput::Received(self.last.insert(input)));
        }
        self.gaps += 1;
        match &self.last {
            Some(last) => (tick, TickInput::Repeated(last)),
            None => (tick, TickInput::Missing),
        }
    }
}

impl std::fmt::Display for QueueFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "input for tick {} is more than {} ticks ahead of tick {}",
            self.tick, self.capacity, self.next_tick
        )
    }
}

impl std::error::Error for QueueFull {}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The window is `capacity` ticks from the next one to run: with
    /// capacity 3 at tick 5, ticks 5–7 are held and 8 is refused until 5
    /// has run.
    #[wasm_bindgen_test(unsupported = test)]
    fn inputs_beyond_the_window_are_refused() {
        let mut queue = InputQueue::new(5, 3);
        assert_eq!(queue.push(7, 'c'), Ok(Pushed::Queued));
        let full = QueueFull {
            tick: 8,
            next_tick: 5,
            capacity: 3,
        };
        assert_eq!(queue.push(8, 'd'), Err(full));
        assert_eq!(queue.take(), (5, TickInput::Missing));
        assert_eq!(queue.push(8, 'd'), Ok(Pushed::Queued));
        assert_eq!(queue.len(), 2);
    }
}
