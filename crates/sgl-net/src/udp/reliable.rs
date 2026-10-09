//! One reliable lane: a bounded 33-slot selective-repeat ARQ with its own
//! sequence space, and in-order reassembly. Outbound messages are kept
//! whole until acknowledged; each fragment in flight is a range of its
//! message, so outbound memory is the held messages' bytes.

use std::collections::{BTreeMap, VecDeque};

use super::packet::{Ack, TOTAL_LEN};
use super::sequence;
use crate::lanes::{Fragment, FramingViolation, Reassembly};

pub const WINDOW: u16 = 32;

/// A received fragment waiting, out of order, for the gap before it.
#[derive(Debug)]
pub struct Slot {
    pub fragment: Fragment,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
struct InFlight {
    sequence: u16,
    /// The message this fragment belongs to, by its position in the lane.
    message: u64,
    start: usize,
    end: usize,
    fragment: Fragment,
    sent_at: Option<u64>,
    transmissions: u8,
    acknowledged: bool,
}

impl InFlight {
    /// Whether this fragment ends its message.
    const fn ends_message(&self) -> bool {
        matches!(self.fragment, Fragment::Whole | Fragment::Last)
    }
}

#[derive(Debug)]
pub struct Reliable {
    next_sequence: u16,
    fragment_bytes: usize,
    /// Held messages, oldest first: every one with a fragment not yet sent
    /// or not yet acknowledged.
    messages: VecDeque<Vec<u8>>,
    /// The position of `messages[0]` in the lane.
    first_message: u64,
    /// The position and offset of the next fragment to send.
    next_fragment: (u64, usize),
    in_flight: VecDeque<InFlight>,
    /// Messages and bytes held until the peer acknowledges them.
    held_messages: usize,
    held_bytes: usize,
    receive_next: u16,
    receive_buffer: BTreeMap<u16, Slot>,
    buffered_bytes: usize,
    reassembly: Reassembly,
    max_message_bytes: usize,
    received: bool,
    pub ack_dirty: bool,
}

impl Reliable {
    /// A lane that sends fragments of at most `fragment_bytes` and accepts
    /// messages of at most `max_message_bytes`.
    pub fn new(max_message_bytes: usize, fragment_bytes: usize) -> Self {
        Self {
            next_sequence: 0,
            fragment_bytes,
            messages: VecDeque::new(),
            first_message: 0,
            next_fragment: (0, 0),
            in_flight: VecDeque::new(),
            held_messages: 0,
            held_bytes: 0,
            receive_next: 0,
            receive_buffer: BTreeMap::new(),
            buffered_bytes: 0,
            reassembly: Reassembly::default(),
            max_message_bytes,
            received: false,
            ack_dirty: false,
        }
    }

    /// Queues one admitted message whole; admission is the caller's.
    pub fn enqueue(&mut self, payload: &[u8]) {
        self.messages.push_back(payload.to_vec());
        self.held_messages += 1;
        self.held_bytes += payload.len();
    }

    /// Messages and bytes held until acknowledged: queued, in flight and
    /// unacknowledged alike.
    pub const fn held(&self) -> (usize, usize) {
        (self.held_messages, self.held_bytes)
    }

    /// The held message at lane position `message`.
    fn message(&self, message: u64) -> &[u8] {
        let index = usize::try_from(message - self.first_message).expect("a held message");
        &self.messages[index]
    }

    /// The fragment to send now: the oldest due retransmission, else the
    /// next queued fragment if the window admits it.
    #[cfg(test)]
    pub fn next_sendable(
        &mut self,
        now_ms: u64,
        rto_ms: u64,
        max_transmissions: u8,
    ) -> Option<u16> {
        self.first_due(now_ms, rto_ms, max_transmissions)
            .or_else(|| self.admit())
    }

    /// Whether a fragment not yet sent fits the window now.
    pub fn window_open(&self) -> bool {
        let unsent = self.next_fragment.0 < self.first_message + self.messages.len() as u64;
        unsent
            && self
                .in_flight
                .front()
                .is_none_or(|first| sequence::diff(self.next_sequence, first.sequence) <= WINDOW)
    }

    /// Moves the next fragment into the window, if it fits.
    pub fn admit(&mut self) -> Option<u16> {
        if !self.window_open() {
            return None;
        }
        let (message, start) = self.next_fragment;
        let len = self.message(message).len();
        let (fragment, end) = Fragment::at(len, start, self.fragment_bytes, TOTAL_LEN);
        self.next_fragment = if end == len {
            (message + 1, 0)
        } else {
            (message, end)
        };
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        self.in_flight.push_back(InFlight {
            sequence,
            message,
            start,
            end,
            fragment,
            sent_at: None,
            transmissions: 0,
            acknowledged: false,
        });
        Some(sequence)
    }

    /// The oldest in-flight fragment due for (re)transmission.
    pub fn first_due(&self, now_ms: u64, rto_ms: u64, max_transmissions: u8) -> Option<u16> {
        self.in_flight.iter().find_map(|item| {
            (!item.acknowledged
                && item.transmissions < max_transmissions
                && item
                    .sent_at
                    .is_none_or(|sent| now_ms.saturating_sub(sent) >= rto_ms))
            .then_some(item.sequence)
        })
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

    /// The in-flight fragment `sequence` and its bytes.
    pub fn fragment(&self, sequence: u16) -> Option<(Fragment, &[u8])> {
        let item = self
            .in_flight
            .iter()
            .find(|item| item.sequence == sequence)?;
        Some((
            item.fragment,
            &self.message(item.message)[item.start..item.end],
        ))
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
        // Fragments leave the window in sequence order, which is message
        // order, so a message's last fragment leaves after all its others
        // and the message is then the oldest held.
        while let Some(item) = self.in_flight.pop_front_if(|item| item.acknowledged) {
            self.held_bytes -= item.end - item.start;
            if item.ends_message() {
                self.held_messages -= 1;
                self.messages.pop_front();
                self.first_message += 1;
            }
        }
    }

    #[must_use]
    pub fn outbound_is_idle(&self) -> bool {
        self.messages.is_empty() && self.in_flight.is_empty()
    }

    /// Applies one received fragment. Fragments ahead of the next expected
    /// sequence wait, within the window, for the gap to fill; the messages
    /// this one completes in order are appended to `output`. A first
    /// fragment declaring more than the message cap is refused on arrival.
    pub fn receive(
        &mut self,
        sequence: u16,
        fragment: Fragment,
        payload: &[u8],
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), FramingViolation> {
        self.ack_dirty = true;
        self.received = true;
        if fragment
            .total()
            .is_some_and(|total| total as usize > self.max_message_bytes)
        {
            return Err(FramingViolation);
        }
        if sequence == self.receive_next {
            self.deliver(fragment, payload, output)?;
            self.receive_next = self.receive_next.wrapping_add(1);
            while let Some(slot) = self.receive_buffer.remove(&self.receive_next) {
                self.buffered_bytes -= slot.bytes.len();
                self.deliver(slot.fragment, &slot.bytes, output)?;
                self.receive_next = self.receive_next.wrapping_add(1);
            }
        } else if sequence::newer(sequence, self.receive_next)
            && sequence::diff(sequence, self.receive_next) <= WINDOW
            && !self.receive_buffer.contains_key(&sequence)
        {
            self.buffered_bytes += payload.len();
            self.receive_buffer.insert(
                sequence,
                Slot {
                    fragment,
                    bytes: payload.to_vec(),
                },
            );
        }
        Ok(())
    }

    fn deliver(
        &mut self,
        fragment: Fragment,
        payload: &[u8],
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), FramingViolation> {
        output.extend(
            self.reassembly
                .push(fragment, payload, self.max_message_bytes)?,
        );
        Ok(())
    }

    /// Inbound bytes retained: out-of-order fragments and the message being
    /// assembled.
    pub fn retained_bytes(&self) -> usize {
        self.buffered_bytes + self.reassembly.retained_bytes()
    }

    /// Whether this lane has received anything, so its acknowledgement
    /// means something to the peer.
    pub const fn has_received(&self) -> bool {
        self.received
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

    fn lane() -> Reliable {
        Reliable::new(64 * 1024, 100)
    }

    /// #254: a stale fragment (already delivered in order) is dropped, a
    /// fragment exactly one window ahead is buffered, and one beyond the
    /// window is ignored.
    #[wasm_bindgen_test(unsupported = test)]
    fn receive_gates_stale_windowed_and_beyond_window_fragments() {
        let mut lane = lane();
        let mut out = Vec::new();
        lane.receive(0, Fragment::Whole, b"first", &mut out)
            .unwrap();
        assert_eq!(out, vec![b"first".to_vec()]);
        lane.receive(0, Fragment::Whole, b"first", &mut out)
            .unwrap();
        assert_eq!(out.len(), 1, "a duplicate is not delivered again");
        assert_eq!(lane.retained_bytes(), 0, "nor retained");
        lane.receive(1 + WINDOW, Fragment::Whole, b"edge", &mut out)
            .unwrap();
        assert_eq!(
            lane.retained_bytes(),
            4,
            "exactly one window ahead is buffered"
        );
        lane.receive(2 + WINDOW, Fragment::Whole, b"beyond", &mut out)
            .unwrap();
        assert_eq!(lane.retained_bytes(), 4, "beyond the window is ignored");
        assert_eq!(out.len(), 1);
    }

    /// A first fragment declaring more than the cap is refused when it
    /// arrives, even out of order, before anything is buffered.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_over_cap_total_is_refused_on_arrival() {
        let mut lane = Reliable::new(100, 100);
        let mut out = Vec::new();
        assert_eq!(
            lane.receive(5, Fragment::First { total: 101 }, b"ab", &mut out),
            Err(FramingViolation)
        );
        assert_eq!(lane.retained_bytes(), 0);
        lane.receive(5, Fragment::First { total: 100 }, b"ab", &mut out)
            .unwrap();
        assert_eq!(lane.retained_bytes(), 2);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn selective_ack_and_karn_sampling_are_exact() {
        let mut reliable = lane();
        for payload in [b"zero".as_slice(), b"one", b"two"] {
            reliable.enqueue(payload);
        }
        for sequence in 0..3 {
            assert_eq!(reliable.next_sendable(10, 100, 12), Some(sequence));
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
        assert_eq!(reliable.held(), (3, 10));

        reliable.acknowledge(Ack { next: 3, bits: 0 }, 60, &mut samples);
        assert_eq!(samples, vec![30, 50]);
        assert!(reliable.outbound_is_idle());
        assert_eq!(reliable.held(), (0, 0));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reliable_sequence_wrap_delivers_and_acks_inside_half_range() {
        let mut reliable = lane();
        reliable.next_sequence = u16::MAX - 1;
        for _ in 0..3 {
            reliable.enqueue(b"");
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
            .receive(0, Fragment::Whole, b"after", &mut delivered)
            .unwrap();
        reliable
            .receive(u16::MAX, Fragment::Whole, b"before", &mut delivered)
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

    type Datagram = (u16, Fragment, Vec<u8>);

    /// One sender turn: transmit everything due or admitted, and return the
    /// datagrams put on the wire.
    fn transmit(sender: &mut Reliable, now_ms: u64) -> Vec<Datagram> {
        let mut wire = Vec::new();
        while let Some(sequence) = sender.next_sendable(now_ms, RTO_MS, MAX_TRANSMISSIONS) {
            sender.mark_sent(sequence, now_ms);
            let (fragment, bytes) = sender
                .fragment(sequence)
                .expect("sent fragment is in flight");
            wire.push((sequence, fragment, bytes.to_vec()));
        }
        wire
    }

    /// Defect: a window or ack-bitmap off-by-one, a reassembly that splices
    /// fragments from different messages, a retransmit path that delivers a
    /// duplicate, or held-message accounting that never returns the
    /// allowance. Oracle: whatever the channel drops, duplicates or
    /// reorders, the receiver's output is always a prefix of the sent
    /// messages, at most `WINDOW + 1` fragments are ever in flight and they
    /// span at most one window, and once the channel is clean every message
    /// arrives and the sender goes idle holding nothing.
    #[test]
    fn reliable_lane_delivers_exactly_once_in_order_over_a_hostile_channel() {
        check(prop::collection::vec(round(), 1..60), |rounds| {
            let (mut sender, mut receiver) = (
                Reliable::new(4 * FRAGMENT, FRAGMENT),
                Reliable::new(4 * FRAGMENT, FRAGMENT),
            );
            let mut sent = Vec::new();
            let mut delivered = Vec::new();
            let mut now_ms = 0;

            let deliver = |lane: &mut Reliable, datagram: &Datagram, out: &mut Vec<Vec<u8>>| {
                lane.receive(datagram.0, datagram.1, &datagram.2, out)
                    .expect("the sender only sends valid fragments");
            };

            for round in &rounds {
                if let Some(message) = &round.enqueue
                    && sender.held().0 < 64
                {
                    sender.enqueue(message);
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
            prop_assert_eq!(sender.held(), (0, 0));
            prop_assert_eq!(receiver.retained_bytes(), 0);
            prop_assert_eq!(delivered, sent);
            Ok(())
        });
    }
}
