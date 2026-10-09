//! Single-slot, newest-wins state lane.

use super::sequence;

#[derive(Debug, Default)]
pub struct Latest {
    next_sequence: u16,
    last_received: Option<u16>,
    queued: Option<Vec<u8>>,
}

impl Latest {
    pub fn replace(&mut self, payload: &[u8]) {
        self.queued = Some(payload.to_vec());
    }

    pub fn take(&mut self) -> Option<(u16, Vec<u8>)> {
        let payload = self.queued.take()?;
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        Some((sequence, payload))
    }

    pub fn clear(&mut self) {
        self.queued = None;
    }

    pub fn receive(&mut self, sequence: u16, payload: &[u8]) -> Option<Vec<u8>> {
        if self
            .last_received
            .is_some_and(|last| !sequence::newer(sequence, last))
        {
            return None;
        }
        self.last_received = Some(sequence);
        Some(payload.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn outbound_overwrites_and_inbound_drops_stale() {
        let mut lane = Latest::default();
        lane.replace(b"old");
        lane.replace(b"new");
        assert_eq!(lane.take(), Some((0, b"new".to_vec())));
        assert_eq!(lane.receive(7, b"seven"), Some(b"seven".to_vec()));
        assert_eq!(lane.receive(6, b"six"), None);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;

    /// RFC 1982 "newer" on plain integers, independent of `sequence`.
    fn model_newer(a: u16, b: u16) -> bool {
        (1..0x8000).contains(&(i32::from(a) - i32::from(b)).rem_euclid(65_536))
    }

    /// Defect: a stale state replacing a newer one at the wrap, or a
    /// coalesced slot sending an old payload. Oracle: the receiver accepts
    /// exactly the sequences newer than everything accepted so far; the
    /// sender's slot hands out the last payload under consecutive sequences.
    #[test]
    fn latest_lane_is_newest_wins_on_both_ends() {
        let strategy = (
            any::<u16>(),
            prop::collection::vec((0u16..0x8000, bytes(8)), 1..40),
            prop::collection::vec(prop::collection::vec(bytes(8), 1..4), 0..10),
        );
        check(strategy, |(base, arrivals, sends)| {
            let mut lane = Latest::default();
            let mut newest: Option<u16> = None;
            for (offset, payload) in &arrivals {
                let sequence = base.wrapping_add(*offset);
                let accepted = lane.receive(sequence, payload);
                let expected = newest.is_none_or(|last| model_newer(sequence, last));
                prop_assert_eq!(
                    accepted.is_some(),
                    expected,
                    "sequence {} after {:?}",
                    sequence,
                    newest
                );
                if expected {
                    prop_assert_eq!(accepted.as_deref(), Some(&payload[..]));
                    newest = Some(sequence);
                }
            }

            let mut sender = Latest::default();
            prop_assert!(sender.take().is_none());
            for (i, burst) in sends.iter().enumerate() {
                for payload in burst {
                    sender.replace(payload);
                }
                let (sequence, payload) = sender.take().expect("queued");
                prop_assert_eq!(usize::from(sequence), i);
                prop_assert_eq!(&payload, burst.last().unwrap());
                prop_assert!(sender.take().is_none());
            }
            Ok(())
        });
    }
}
