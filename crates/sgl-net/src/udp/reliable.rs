//! Bounded 33-slot selective-repeat ARQ and ordered reassembly.

use std::collections::{BTreeMap, VecDeque};

use super::packet::Ack;
use super::sequence;

pub const WINDOW: u16 = 32;
pub const MAX_OUTBOUND_FRAGMENTS: usize = 256;

#[derive(Debug)]
pub struct Slot {
    pub more: bool,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
struct InFlight {
    sequence: u16,
    slot: Slot,
    sent_at: Option<u64>,
    transmissions: u8,
    acknowledged: bool,
}

#[derive(Debug)]
pub struct Reliable {
    next_sequence: u16,
    queued: VecDeque<Slot>,
    in_flight: VecDeque<InFlight>,
    receive_next: u16,
    receive_buffer: BTreeMap<u16, Slot>,
    assembling: Vec<u8>,
    assembly_active: bool,
    assembly_items: usize,
    max_message_bytes: usize,
    max_queued_messages: usize,
    max_queued_bytes: usize,
    max_receive_items: usize,
    max_receive_bytes: usize,
    pub ack_dirty: bool,
}

impl Reliable {
    pub fn new(
        max_message_bytes: usize,
        max_queued_messages: usize,
        max_queued_bytes: usize,
        max_receive_items: usize,
        max_receive_bytes: usize,
    ) -> Self {
        Self {
            next_sequence: 0,
            queued: VecDeque::new(),
            in_flight: VecDeque::new(),
            receive_next: 0,
            receive_buffer: BTreeMap::new(),
            assembling: Vec::new(),
            assembly_active: false,
            assembly_items: 0,
            max_message_bytes,
            max_queued_messages,
            max_queued_bytes,
            max_receive_items,
            max_receive_bytes,
            ack_dirty: false,
        }
    }

    pub fn enqueue(&mut self, payload: &[u8], fragment_bytes: usize) -> Result<(), ()> {
        if !self.can_enqueue(payload.len(), fragment_bytes) {
            return Err(());
        }
        if payload.is_empty() {
            self.queued.push_back(Slot {
                more: false,
                bytes: Vec::new(),
            });
            return Ok(());
        }
        let mut chunks = payload.chunks(fragment_bytes).peekable();
        while let Some(chunk) = chunks.next() {
            self.queued.push_back(Slot {
                more: chunks.peek().is_some(),
                bytes: chunk.to_vec(),
            });
        }
        Ok(())
    }

    pub fn can_enqueue(&self, payload_len: usize, fragment_bytes: usize) -> bool {
        if fragment_bytes == 0 {
            return false;
        }
        let fragment_count = payload_len.max(1).div_ceil(fragment_bytes);
        if payload_len > self.max_message_bytes
            || self.outbound_messages() >= self.max_queued_messages
            || self
                .outbound_bytes()
                .checked_add(payload_len)
                .is_none_or(|bytes| bytes > self.max_queued_bytes)
            || self
                .outbound_items()
                .checked_add(fragment_count)
                .is_none_or(|items| items > MAX_OUTBOUND_FRAGMENTS)
        {
            return false;
        }
        true
    }

    pub fn admit(&mut self) -> Option<u16> {
        if self.queued.is_empty()
            || self
                .in_flight
                .front()
                .is_some_and(|first| sequence::diff(self.next_sequence, first.sequence) > WINDOW)
        {
            return None;
        }
        let slot = self.queued.pop_front()?;
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        self.in_flight.push_back(InFlight {
            sequence,
            slot,
            sent_at: None,
            transmissions: 0,
            acknowledged: false,
        });
        Some(sequence)
    }

    pub fn due_bounded(
        &self,
        now_ms: u64,
        rto_ms: u64,
        max_transmissions: u8,
        output: &mut Vec<u16>,
    ) {
        output.extend(self.in_flight.iter().filter_map(|item| {
            (!item.acknowledged
                && item.transmissions < max_transmissions
                && item
                    .sent_at
                    .is_none_or(|sent| now_ms.saturating_sub(sent) >= rto_ms))
            .then_some(item.sequence)
        }));
    }

    pub fn retry_exhausted(&self, now_ms: u64, rto_ms: u64, maximum: u8) -> bool {
        self.in_flight.iter().any(|item| {
            !item.acknowledged
                && item.transmissions >= maximum
                && item
                    .sent_at
                    .is_some_and(|sent| now_ms.saturating_sub(sent) >= rto_ms)
        })
    }

    pub fn slot(&self, sequence: u16) -> Option<&Slot> {
        self.in_flight
            .iter()
            .find(|item| item.sequence == sequence)
            .map(|item| &item.slot)
    }

    pub fn mark_sent(&mut self, sequence: u16, now_ms: u64) {
        if let Some(item) = self
            .in_flight
            .iter_mut()
            .find(|item| item.sequence == sequence)
        {
            item.transmissions = item.transmissions.saturating_add(1);
            item.sent_at = Some(now_ms);
        }
    }

    pub fn acknowledge(&mut self, ack: Ack, now_ms: u64, rtt_samples: &mut Vec<u64>) {
        if sequence::newer(ack.next, self.next_sequence) {
            return;
        }
        for item in &mut self.in_flight {
            let received = sequence::newer(ack.next, item.sequence) || {
                let distance = sequence::diff(item.sequence, ack.next);
                (1..=WINDOW).contains(&distance) && ack.bits & (1_u32 << (distance - 1)) != 0
            };
            if !item.acknowledged && received {
                item.acknowledged = true;
                if item.transmissions == 1
                    && let Some(sent_at) = item.sent_at
                {
                    rtt_samples.push(now_ms.saturating_sub(sent_at));
                }
            }
        }
        while self.in_flight.front().is_some_and(|item| item.acknowledged) {
            self.in_flight.pop_front();
        }
    }

    #[must_use]
    pub fn outbound_is_idle(&self) -> bool {
        self.queued.is_empty() && self.in_flight.is_empty()
    }

    pub fn outbound_items(&self) -> usize {
        self.queued.len().saturating_add(self.in_flight.len())
    }

    pub fn outbound_messages(&self) -> usize {
        self.queued.iter().filter(|slot| !slot.more).count()
            + self.in_flight.iter().filter(|item| !item.slot.more).count()
    }

    pub fn outbound_bytes(&self) -> usize {
        self.queued
            .iter()
            .map(|slot| slot.bytes.len())
            .chain(self.in_flight.iter().map(|item| item.slot.bytes.len()))
            .sum()
    }

    pub fn inbound_items(&self) -> usize {
        self.receive_buffer
            .len()
            .saturating_add(self.assembly_items)
    }

    pub fn inbound_bytes(&self) -> usize {
        self.assembling.len().saturating_add(
            self.receive_buffer
                .values()
                .map(|slot| slot.bytes.len())
                .sum(),
        )
    }

    pub fn receive(
        &mut self,
        sequence: u16,
        more: bool,
        payload: &[u8],
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), ()> {
        self.ack_dirty = true;
        let retains = more || sequence != self.receive_next || self.assembly_active;
        if retains
            && (self
                .inbound_items()
                .checked_add(1)
                .is_none_or(|items| items > self.max_receive_items)
                || self
                    .inbound_bytes()
                    .checked_add(payload.len())
                    .is_none_or(|bytes| bytes > self.max_receive_bytes))
        {
            return Err(());
        }
        if sequence == self.receive_next {
            self.deliver(more, payload, output)?;
            self.receive_next = self.receive_next.wrapping_add(1);
            while let Some(slot) = self.receive_buffer.remove(&self.receive_next) {
                self.deliver(slot.more, &slot.bytes, output)?;
                self.receive_next = self.receive_next.wrapping_add(1);
            }
        } else if sequence::newer(sequence, self.receive_next)
            && sequence::diff(sequence, self.receive_next) <= WINDOW
        {
            self.receive_buffer.entry(sequence).or_insert_with(|| Slot {
                more,
                bytes: payload.to_vec(),
            });
        }
        Ok(())
    }

    fn deliver(&mut self, more: bool, payload: &[u8], output: &mut Vec<Vec<u8>>) -> Result<(), ()> {
        if self
            .assembling
            .len()
            .checked_add(payload.len())
            .is_none_or(|bytes| bytes > self.max_message_bytes)
        {
            return Err(());
        }
        if more {
            self.assembling.extend_from_slice(payload);
            self.assembly_active = true;
            self.assembly_items = self.assembly_items.saturating_add(1);
        } else if !self.assembly_active {
            output.push(payload.to_vec());
        } else {
            self.assembling.extend_from_slice(payload);
            output.push(std::mem::take(&mut self.assembling));
            self.assembly_active = false;
            self.assembly_items = 0;
        }
        Ok(())
    }

    pub fn ack(&self) -> Ack {
        let mut bits = 0_u32;
        for offset in 0..WINDOW {
            if self
                .receive_buffer
                .contains_key(&self.receive_next.wrapping_add(1 + offset))
            {
                bits |= 1_u32 << offset;
            }
        }
        Ack {
            next: self.receive_next,
            bits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #254: admission is exact at the message-size, queued-message and
    /// queued-byte caps, and `outbound_messages` counts messages, not
    /// fragments.
    #[wasm_bindgen_test(unsupported = test)]
    fn admission_caps_are_exact() {
        let mut lane = Reliable::new(10, 2, 15, 128, 1 << 20);
        assert!(lane.can_enqueue(10, 4));
        assert!(!lane.can_enqueue(11, 4));
        lane.enqueue(&[1; 10], 4).unwrap();
        assert_eq!(lane.outbound_messages(), 1, "three fragments, one message");
        assert_eq!(lane.outbound_items(), 3);
        assert!(lane.can_enqueue(5, 4), "exactly at the byte cap");
        assert!(!lane.can_enqueue(6, 4));
        lane.enqueue(&[2; 5], 4).unwrap();
        assert_eq!(lane.outbound_messages(), 2);
        assert!(!lane.can_enqueue(0, 4), "message cap reached");
    }

    /// #254: a stale fragment (already delivered in order) is dropped, a
    /// fragment exactly one window ahead is buffered, and one beyond the
    /// window is ignored.
    #[wasm_bindgen_test(unsupported = test)]
    fn receive_gates_stale_windowed_and_beyond_window_fragments() {
        let mut lane = lane();
        let mut out = Vec::new();
        lane.receive(0, false, b"first", &mut out).unwrap();
        assert_eq!(out, vec![b"first".to_vec()]);
        lane.receive(0, false, b"first", &mut out).unwrap();
        assert_eq!(out.len(), 1, "a duplicate is not delivered again");
        assert_eq!(lane.inbound_items(), 0, "nor retained");
        lane.receive(1 + WINDOW, false, b"edge", &mut out).unwrap();
        assert_eq!(
            lane.inbound_items(),
            1,
            "exactly one window ahead is buffered"
        );
        lane.receive(2 + WINDOW, false, b"beyond", &mut out)
            .unwrap();
        assert_eq!(lane.inbound_items(), 1, "beyond the window is ignored");
        assert_eq!(out.len(), 1);
    }

    fn lane() -> Reliable {
        Reliable::new(64 * 1024, 128, 256 * 1024, 128, 256 * 1024)
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn selective_ack_and_karn_sampling_are_exact() {
        let mut reliable = lane();
        for payload in [b"zero".as_slice(), b"one", b"two"] {
            reliable.enqueue(payload, 100).unwrap();
        }
        for sequence in 0..3 {
            assert_eq!(reliable.admit(), Some(sequence));
            reliable.mark_sent(sequence, 10);
        }
        reliable.mark_sent(1, 20);

        let mut samples = Vec::new();
        reliable.acknowledge(
            Ack {
                next: 0,
                bits: 1 << 1,
            },
            40,
            &mut samples,
        );
        assert_eq!(samples, vec![30]);
        assert_eq!(reliable.outbound_items(), 3);

        reliable.acknowledge(Ack { next: 3, bits: 0 }, 60, &mut samples);
        assert_eq!(samples, vec![30, 50]);
        assert!(reliable.outbound_is_idle());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reliable_sequence_wrap_delivers_and_acks_inside_half_range() {
        let mut reliable = lane();
        reliable.next_sequence = u16::MAX - 1;
        for _ in 0..3 {
            reliable.enqueue(b"", 100).unwrap();
        }
        assert_eq!(reliable.admit(), Some(u16::MAX - 1));
        assert_eq!(reliable.admit(), Some(u16::MAX));
        assert_eq!(reliable.admit(), Some(0));
        for sequence in [u16::MAX - 1, u16::MAX, 0] {
            reliable.mark_sent(sequence, 0);
        }
        reliable.acknowledge(Ack { next: 1, bits: 0 }, 1, &mut Vec::new());
        assert!(reliable.outbound_is_idle());

        reliable.receive_next = u16::MAX;
        let mut delivered = Vec::new();
        reliable
            .receive(0, false, b"after", &mut delivered)
            .unwrap();
        reliable
            .receive(u16::MAX, false, b"before", &mut delivered)
            .unwrap();
        assert_eq!(delivered, vec![b"before".to_vec(), b"after".to_vec()]);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;

    const FRAGMENT: usize = 100;
    const RTO_MS: u64 = 100;
    const MAX_TRANSMISSIONS: u8 = 200;

    #[derive(Debug, Clone)]
    struct Round {
        enqueue: Option<Vec<u8>>,
        drop_mask: u64,
        duplicate_mask: u64,
        reverse: bool,
        drop_ack: bool,
    }

    fn round() -> impl Strategy<Value = Round> {
        (
            prop::option::of(bytes(3 * FRAGMENT)),
            any::<u64>(),
            any::<u64>(),
            any::<bool>(),
            prop::bool::weighted(0.3),
        )
            .prop_map(
                |(enqueue, drop_mask, duplicate_mask, reverse, drop_ack)| Round {
                    enqueue,
                    drop_mask,
                    duplicate_mask,
                    reverse,
                    drop_ack,
                },
            )
    }

    fn lane() -> Reliable {
        Reliable::new(4 * FRAGMENT, 64, 1 << 20, 256, 1 << 20)
    }

    /// One sender turn: admit queued fragments, transmit what is due, and
    /// return the datagrams put on the wire.
    fn transmit(sender: &mut Reliable, now_ms: u64) -> Vec<(u16, bool, Vec<u8>)> {
        while sender.admit().is_some() {}
        let mut due = Vec::new();
        sender.due_bounded(now_ms, RTO_MS, MAX_TRANSMISSIONS, &mut due);
        due.iter()
            .map(|&sequence| {
                sender.mark_sent(sequence, now_ms);
                let slot = sender.slot(sequence).expect("due fragment is in flight");
                (sequence, slot.more, slot.bytes.clone())
            })
            .collect()
    }

    /// Defect: a window or ack-bitmap off-by-one, a reassembly that splices
    /// fragments from different messages, or a retransmit path that
    /// delivers a duplicate. Oracle: whatever the channel drops, duplicates
    /// or reorders, the receiver's output is always a prefix of the sent
    /// messages, at most `WINDOW + 1` fragments are ever in flight and they
    /// span at most one window, and once the channel is clean every message
    /// arrives and the sender goes idle.
    #[test]
    fn reliable_lane_delivers_exactly_once_in_order_over_a_hostile_channel() {
        check(prop::collection::vec(round(), 1..60), |rounds| {
            let (mut sender, mut receiver) = (lane(), lane());
            let mut sent = Vec::new();
            let mut delivered = Vec::new();
            let mut now_ms = 0;

            let deliver =
                |lane: &mut Reliable, datagram: &(u16, bool, Vec<u8>), out: &mut Vec<Vec<u8>>| {
                    lane.receive(datagram.0, datagram.1, &datagram.2, out)
                        .expect("caps are generous in this model");
                };

            for round in &rounds {
                if let Some(message) = &round.enqueue
                    && sender.can_enqueue(message.len(), FRAGMENT)
                {
                    sender.enqueue(message, FRAGMENT).unwrap();
                    sent.push(message.clone());
                }
                let mut wire = transmit(&mut sender, now_ms);
                prop_assert!(
                    wire.len() <= usize::from(WINDOW) + 1,
                    "{} fragments in flight",
                    wire.len()
                );
                for (first, _, _) in &wire {
                    for (second, _, _) in &wire {
                        prop_assert!(
                            sequence::diff(*first, *second) <= WINDOW
                                || sequence::diff(*second, *first) <= WINDOW
                        );
                    }
                }
                if round.reverse {
                    wire.reverse();
                }
                for (i, datagram) in wire.iter().enumerate() {
                    let bit = 1u64 << (i % 64);
                    if round.drop_mask & bit != 0 {
                        continue;
                    }
                    deliver(&mut receiver, datagram, &mut delivered);
                    if round.duplicate_mask & bit != 0 {
                        deliver(&mut receiver, datagram, &mut delivered);
                    }
                }
                if !round.drop_ack {
                    let mut samples = Vec::new();
                    sender.acknowledge(receiver.ack(), now_ms, &mut samples);
                }
                prop_assert!(
                    sent.starts_with(&delivered),
                    "delivered {delivered:?} is not a prefix of {sent:?}"
                );
                now_ms += 50;
            }

            // A clean channel: everything outstanding must land.
            for _ in 0..600 {
                if sender.outbound_is_idle() {
                    break;
                }
                now_ms += RTO_MS;
                for datagram in transmit(&mut sender, now_ms) {
                    deliver(&mut receiver, &datagram, &mut delivered);
                }
                let mut samples = Vec::new();
                sender.acknowledge(receiver.ack(), now_ms, &mut samples);
            }
            prop_assert!(sender.outbound_is_idle(), "sender never drained");
            prop_assert_eq!(delivered, sent);
            Ok(())
        });
    }
}
