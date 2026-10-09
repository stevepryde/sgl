//! One lane's unreliable messages: a bounded send queue, each message sent
//! once under the lane's next unreliable sequence, and a receive window that
//! drops network duplicates so each message is delivered at most once.

use std::collections::VecDeque;

use super::sequence;

/// Sequences the receive window remembers. A datagram reordered behind more
/// than this many later unreliable messages of its lane cannot be told from
/// a duplicate and is treated as lost.
pub const RECEIVE_WINDOW: u16 = 1024;
const WORDS: usize = RECEIVE_WINDOW as usize / 64;

#[derive(Debug, Default)]
pub struct Unreliable {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    next_sequence: u16,
    /// The newest sequence received, and which of the window's sequences
    /// arrived, indexed by sequence modulo the window.
    newest: Option<u16>,
    seen: [u64; WORDS],
}

impl Unreliable {
    /// Messages and bytes waiting to be sent.
    pub fn queued(&self) -> (usize, usize) {
        (self.queue.len(), self.bytes)
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Queues one admitted message; admission is the caller's.
    pub fn push(&mut self, payload: &[u8]) {
        self.bytes += payload.len();
        self.queue.push_back(payload.to_vec());
    }

    /// The next message to send, with its sequence. It is sent once.
    pub fn pop(&mut self) -> Option<(u16, Vec<u8>)> {
        let payload = self.queue.pop_front()?;
        self.bytes -= payload.len();
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        Some((sequence, payload))
    }

    fn bit(sequence: u16) -> (usize, u64) {
        let index = usize::from(sequence % RECEIVE_WINDOW);
        (index / 64, 1 << (index % 64))
    }

    fn mark(&mut self, sequence: u16, seen: bool) {
        let (word, bit) = Self::bit(sequence);
        if seen {
            self.seen[word] |= bit;
        } else {
            self.seen[word] &= !bit;
        }
    }

    /// Whether a received message with `sequence` is new: false for a
    /// duplicate and for one older than the window.
    pub fn accept(&mut self, sequence: u16) -> bool {
        let Some(newest) = self.newest else {
            self.newest = Some(sequence);
            self.mark(sequence, true);
            return true;
        };
        if sequence::newer(sequence, newest) {
            let advance = sequence::diff(sequence, newest);
            if advance >= RECEIVE_WINDOW {
                self.seen = [0; WORDS];
            } else {
                // Slots the window moves onto held sequences a window older.
                for step in 1..=advance {
                    self.mark(newest.wrapping_add(step), false);
                }
            }
            self.newest = Some(sequence);
            self.mark(sequence, true);
            return true;
        }
        let (word, bit) = Self::bit(sequence);
        if sequence::diff(newest, sequence) >= RECEIVE_WINDOW || self.seen[word] & bit != 0 {
            return false;
        }
        self.seen[word] |= bit;
        true
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod properties {
    use super::*;
    use crate::proptest_support::check;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    /// Defect: a duplicate delivered twice (a slot not marked, or a stale
    /// mark cleared too late or too early as the window slides, including
    /// across the u16 wrap), or a new message inside the window refused.
    /// Oracle: a brute-force model on unwrapped integers — a message is new
    /// exactly when it was never accepted and is less than a window behind
    /// the newest accepted.
    #[test]
    fn the_receive_window_accepts_each_sequence_once() {
        let arrivals = prop::collection::vec(
            prop_oneof![
                3 => 0i64..40,
                1 => 0i64..3_000,
            ],
            1..300,
        );
        check((any::<u16>(), arrivals), |(base, steps)| {
            let mut window = Unreliable::default();
            let mut accepted = BTreeSet::new();
            let mut newest: Option<i64> = None;
            // A walk that mostly moves forward, sometimes back, and repeats.
            let mut at = 0i64;
            for (i, step) in steps.into_iter().enumerate() {
                at = if i % 3 == 2 {
                    (at - step / 2).max(0)
                } else {
                    at + step % 50
                };
                let wire = base.wrapping_add((at % 65_536) as u16);
                let fresh = !accepted.contains(&at)
                    && newest.is_none_or(|newest| at > newest - i64::from(RECEIVE_WINDOW));
                prop_assert_eq!(
                    window.accept(wire),
                    fresh,
                    "sequence {} after {:?}",
                    at,
                    newest
                );
                if fresh {
                    accepted.insert(at);
                    newest = Some(newest.map_or(at, |newest| newest.max(at)));
                }
            }
            Ok(())
        });
    }
}
