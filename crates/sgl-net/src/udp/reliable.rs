//! One reliable lane: a bounded `WINDOW`-fragment selective-repeat ARQ with
//! its own sequence space, and in-order reassembly. Outbound messages are
//! kept whole until acknowledged; each fragment in flight is a range of its
//! message, so outbound memory is the held messages' bytes.
//!
//! Receive flow control: the receiver consumes a fragment only when the
//! message it completes fits the room its caller gives it. Otherwise the
//! fragment waits in the window, the window stops advancing, and the
//! acknowledgement reports the fragment held, so the sender neither
//! retransmits it nor counts it toward retry exhaustion.

use std::collections::{BTreeMap, VecDeque};

use super::packet::{Ack, TOTAL_LEN};
use super::sequence;
use crate::lanes::{Fragment, FramingViolation, Reassembly};

/// Fragments in flight per lane; the receiver buffers the same span.
pub const WINDOW: u16 = 32;
/// Flushes that carry a lane's acknowledgement after a fragment arrives: the
/// next and the one after, so one lost acknowledgement does not leave a
/// sender whose whole window rode one datagram waiting for its timeout.
pub const ACK_FLUSHES: u8 = 2;

/// A received fragment waiting for the gap before it, or for room.
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
    /// Arrived: before the peer's `next`, or marked in its bits. It stays
    /// in the window until `next` passes it, since the peer may yet hold it.
    acknowledged: bool,
    /// The peer has it and holds it until its caller makes room.
    peer_holds: bool,
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
    /// Flushes that must still carry this lane's acknowledgement.
    pub acks_owed: u8,
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
            acks_owed: 0,
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
                .is_none_or(|first| sequence::diff(self.next_sequence, first.sequence) < WINDOW)
    }

    /// The fragment [`Self::admit`] moves into the window next and the
    /// range of its message it carries, if the window admits it.
    pub fn upcoming(&self) -> Option<(Fragment, usize, usize)> {
        if !self.window_open() {
            return None;
        }
        let (message, start) = self.next_fragment;
        let len = self.message(message).len();
        let (fragment, end) = Fragment::at(len, start, self.fragment_bytes, TOTAL_LEN);
        Some((fragment, start, end))
    }

    /// Moves the next fragment into the window, if it fits.
    pub fn admit(&mut self) -> Option<u16> {
        let (fragment, start, end) = self.upcoming()?;
        let message = self.next_fragment.0;
        self.next_fragment = if matches!(fragment, Fragment::Whole | Fragment::Last) {
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
            peer_holds: false,
        });
        Some(sequence)
    }

    /// The oldest in-flight fragment due for (re)transmission. A fragment
    /// the peer holds is not: it has arrived.
    pub fn first_due(&self, now_ms: u64, rto_ms: u64, max_transmissions: u8) -> Option<u16> {
        self.in_flight.iter().find_map(|item| {
            (!item.acknowledged
                && !item.peer_holds
                && item.transmissions < max_transmissions
                && item
                    .sent_at
                    .is_none_or(|sent| now_ms.saturating_sub(sent) >= rto_ms))
            .then_some(item.sequence)
        })
    }

    /// Whether a fragment went unacknowledged through `maximum`
    /// transmissions. One the peer holds waits for the peer's caller, not
    /// the network, so it never counts.
    pub fn retry_exhausted(&self, now_ms: u64, rto_ms: u64, maximum: u8) -> bool {
        self.in_flight.iter().any(|item| {
            !item.acknowledged
                && !item.peer_holds
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

    /// Applies the peer's acknowledgement: fragments before `next` and those
    /// its bits mark have arrived and are not sent again, and those before
    /// `next` leave the window. The window follows `next`, the front of the
    /// peer's own window, so it never reaches past it: a fragment the peer
    /// buffered out of order may later be the one it holds.
    pub fn acknowledge(&mut self, ack: Ack, now_ms: u64, rtt_samples: &mut Vec<u64>) {
        if sequence::newer(ack.next, self.next_sequence) {
            return;
        }
        for item in &mut self.in_flight {
            let distance = sequence::diff(item.sequence, ack.next);
            let received = sequence::newer(ack.next, item.sequence)
                || ((1..WINDOW).contains(&distance) && ack.bits & (1_u32 << (distance - 1)) != 0);
            if received && !item.acknowledged {
                item.acknowledged = true;
                // A held fragment's acknowledgement waited on the peer's
                // caller, so it measures no round trip.
                if item.transmissions == 1
                    && !item.peer_holds
                    && let Some(sent_at) = item.sent_at
                {
                    rtt_samples.push(now_ms.saturating_sub(sent_at));
                }
            } else if ack.held && distance == 0 {
                // Arrival is final: a later acknowledgement that predates
                // the hold does not undo it.
                item.peer_holds = true;
            }
        }
        // Fragments leave the window in sequence order, which is message
        // order, so a message's last fragment leaves after all its others
        // and the message is then the oldest held.
        while let Some(item) = self
            .in_flight
            .pop_front_if(|item| sequence::newer(ack.next, item.sequence))
        {
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

    /// Applies one received fragment: one in the window (the next expected
    /// sequence and the `WINDOW - 1` after it) is buffered once, then the
    /// window is consumed as far as `admit` allows ([`Self::consume`]). A
    /// first fragment declaring more than the message cap is refused on
    /// arrival.
    pub fn receive(
        &mut self,
        sequence: u16,
        fragment: Fragment,
        payload: &[u8],
        admit: &mut impl FnMut(usize) -> bool,
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), FramingViolation> {
        self.acks_owed = ACK_FLUSHES;
        self.received = true;
        if fragment
            .total()
            .is_some_and(|total| total as usize > self.max_message_bytes)
        {
            return Err(FramingViolation);
        }
        if sequence::diff(sequence, self.receive_next) < WINDOW
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
        self.consume(admit, output)
    }

    /// Consumes buffered fragments in sequence order up to the first gap,
    /// appending the messages they complete to `output`. A fragment that
    /// would complete a message `admit` refuses stays at the front of the
    /// window, held, until a later call admits it; `admit` counts what it
    /// accepts.
    pub fn consume(
        &mut self,
        admit: &mut impl FnMut(usize) -> bool,
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), FramingViolation> {
        while let Some(slot) = self.receive_buffer.get(&self.receive_next) {
            if self
                .reassembly
                .completes(slot.fragment, slot.bytes.len())
                .is_some_and(|len| !admit(len))
            {
                break;
            }
            let slot = self
                .receive_buffer
                .remove(&self.receive_next)
                .expect("the front slot is buffered");
            self.buffered_bytes -= slot.bytes.len();
            self.receive_next = self.receive_next.wrapping_add(1);
            self.acks_owed = ACK_FLUSHES;
            output.extend(self.reassembly.push(
                slot.fragment,
                &slot.bytes,
                self.max_message_bytes,
            )?);
        }
        Ok(())
    }

    /// Inbound bytes retained: buffered fragments and the message being
    /// assembled.
    pub fn retained_bytes(&self) -> usize {
        self.buffered_bytes + self.reassembly.retained_bytes()
    }

    /// Whether the window's front fragment waits for room: buffered at the
    /// front only while `admit` refuses it.
    pub fn holding(&self) -> bool {
        self.receive_buffer.contains_key(&self.receive_next)
    }

    /// Whether this lane has received anything, so its acknowledgement
    /// means something to the peer.
    pub const fn has_received(&self) -> bool {
        self.received
    }

    pub fn ack(&self) -> Ack {
        let mut bits = 0_u32;
        for offset in 0..WINDOW - 1 {
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
            held: self.holding(),
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

    /// The caller takes everything.
    fn all(_: usize) -> bool {
        true
    }

    /// #254: a stale fragment (already delivered in order) is dropped, the
    /// window's last fragment is buffered, and one beyond the window is
    /// ignored.
    #[wasm_bindgen_test(unsupported = test)]
    fn receive_gates_stale_windowed_and_beyond_window_fragments() {
        let mut lane = lane();
        let mut out = Vec::new();
        lane.receive(0, Fragment::Whole, b"first", &mut all, &mut out)
            .unwrap();
        assert_eq!(out, vec![b"first".to_vec()]);
        lane.receive(0, Fragment::Whole, b"first", &mut all, &mut out)
            .unwrap();
        assert_eq!(out.len(), 1, "a duplicate is not delivered again");
        assert_eq!(lane.retained_bytes(), 0, "nor retained");
        lane.receive(WINDOW, Fragment::Whole, b"edge", &mut all, &mut out)
            .unwrap();
        assert_eq!(
            lane.retained_bytes(),
            4,
            "the window's last fragment is buffered"
        );
        lane.receive(1 + WINDOW, Fragment::Whole, b"beyond", &mut all, &mut out)
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
            lane.receive(5, Fragment::First { total: 101 }, b"ab", &mut all, &mut out),
            Err(FramingViolation)
        );
        assert_eq!(lane.retained_bytes(), 0);
        lane.receive(5, Fragment::First { total: 100 }, b"ab", &mut all, &mut out)
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
                held: false,
            },
            40,
            &mut samples,
        );
        assert_eq!(samples, vec![30]);
        assert_eq!(reliable.held(), (3, 10));

        reliable.acknowledge(
            Ack {
                next: 3,
                bits: 0,
                held: false,
            },
            60,
            &mut samples,
        );
        assert_eq!(samples, vec![30, 50]);
        assert!(reliable.outbound_is_idle());
        assert_eq!(reliable.held(), (0, 0));
    }

    /// Defect: a receiver whose caller has no room delivering anyway or
    /// dropping the fragment, an acknowledgement that advances past it or
    /// hides that it arrived, a sender that keeps retransmitting a held
    /// fragment, counts it toward its retry limit (so a stalled receiver
    /// times a healthy sender out), lets a reordered older acknowledgement
    /// undo the hold, or samples the hold as round-trip time. Oracle:
    /// netcode.md 12 and 15 on held fragments — with no room the message
    /// waits, `next` stays put and is reported held while later fragments
    /// are acknowledged as usual; the sender sends nothing more and is not
    /// exhausted at a limit of one transmission however long the hold
    /// lasts; once room returns both messages arrive once, in order, and
    /// only the promptly acknowledged fragment yields an RTT sample.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_refused_message_holds_the_window_without_exhausting_the_sender() {
        let (mut sender, mut receiver) = (lane(), lane());
        for payload in [b"first".as_slice(), b"second"] {
            sender.enqueue(payload);
        }
        let mut out = Vec::new();
        for sequence in [0, 1] {
            assert_eq!(sender.next_sendable(0, 100, 1), Some(sequence));
            sender.mark_sent(sequence, 0);
            let (fragment, bytes) = sender.fragment(sequence).unwrap();
            receiver
                .receive(sequence, fragment, bytes, &mut |_| false, &mut out)
                .unwrap();
        }
        assert!(out.is_empty(), "no room, no delivery");
        let held = receiver.ack();
        assert_eq!(
            held,
            Ack {
                next: 0,
                bits: 1,
                held: true,
            }
        );
        let mut samples = Vec::new();
        sender.acknowledge(held, 10, &mut samples);
        let stale = Ack {
            next: 0,
            bits: 0,
            held: false,
        };
        sender.acknowledge(stale, 11, &mut samples);
        for now_ms in [200, 5_000, 60_000] {
            assert_eq!(sender.next_sendable(now_ms, 100, 1), None, "at {now_ms}");
            assert!(!sender.retry_exhausted(now_ms, 100, 1), "at {now_ms}");
        }

        receiver.consume(&mut all, &mut out).unwrap();
        assert_eq!(out, vec![b"first".to_vec(), b"second".to_vec()]);
        let released = receiver.ack();
        assert_eq!(
            released,
            Ack {
                next: 2,
                bits: 0,
                held: false,
            }
        );
        sender.acknowledge(released, 60_010, &mut samples);
        assert!(sender.outbound_is_idle());
        assert_eq!(samples, vec![10], "only the second fragment's round trip");
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
        reliable.acknowledge(
            Ack {
                next: 1,
                bits: 0,
                held: false,
            },
            1,
            &mut Vec::new(),
        );
        assert!(reliable.outbound_is_idle());

        reliable.receive_next = u16::MAX;
        let mut delivered = Vec::new();
        reliable
            .receive(0, Fragment::Whole, b"after", &mut all, &mut delivered)
            .unwrap();
        reliable
            .receive(
                u16::MAX,
                Fragment::Whole,
                b"before",
                &mut all,
                &mut delivered,
            )
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
        /// Messages queued this round, enough to keep the window full.
        enqueue: Vec<Vec<u8>>,
        drop_mask: u64,
        duplicate_mask: u64,
        reverse: bool,
        drop_ack: bool,
        /// Messages the receiver's caller takes this round; `None` for all.
        room: Option<u8>,
    }

    fn round() -> impl Strategy<Value = Round> {
        (
            prop::collection::vec(bytes(3 * FRAGMENT), 0..4),
            any::<u64>(),
            any::<u64>(),
            any::<bool>(),
            prop::bool::weighted(0.3),
            prop::option::of(0..3_u8),
        )
            .prop_map(
                |(enqueue, drop_mask, duplicate_mask, reverse, drop_ack, room)| Round {
                    enqueue,
                    drop_mask,
                    duplicate_mask,
                    reverse,
                    drop_ack,
                    room,
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
    /// duplicate, a hold that loses, duplicates or never releases a
    /// fragment, a sender window that slides past a held fragment it saw
    /// selectively acknowledged, or held-message accounting that never
    /// returns the allowance. Oracle: whatever the channel drops,
    /// duplicates or reorders, and however little the receiver's caller
    /// takes each round, the receiver's output is always a prefix of the
    /// sent messages, at most `WINDOW` fragments are ever in flight, they
    /// span less than a window and none lies past the receiver's window
    /// (where it would be dropped), and once the channel is clean and the
    /// caller takes everything, every message arrives and the sender goes
    /// idle holding nothing.
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

            for round in &rounds {
                let mut room = round.room;
                let mut admit = |_: usize| match &mut room {
                    None => true,
                    Some(0) => false,
                    Some(left) => {
                        *left -= 1;
                        true
                    }
                };
                receiver
                    .consume(&mut admit, &mut delivered)
                    .expect("the sender only sends valid fragments");
                for message in &round.enqueue {
                    if sender.held().0 < 64 {
                        sender.enqueue(message);
                        sent.push(message.clone());
                    }
                }
                let mut wire = transmit(&mut sender, now_ms);
                prop_assert!(
                    wire.len() <= usize::from(WINDOW),
                    "{} fragments in flight",
                    wire.len()
                );
                for (sequence, _, _) in &wire {
                    let ahead = sequence::diff(*sequence, receiver.receive_next);
                    prop_assert!(
                        !(WINDOW..0x8000).contains(&ahead),
                        "{sequence} is past the receiver's window at {}",
                        receiver.receive_next
                    );
                }
                for (first, _, _) in &wire {
                    for (second, _, _) in &wire {
                        prop_assert!(
                            sequence::diff(*first, *second) < WINDOW
                                || sequence::diff(*second, *first) < WINDOW
                        );
                    }
                }
                if round.reverse {
                    wire.reverse();
                }
                for (i, (sequence, fragment, bytes)) in wire.iter().enumerate() {
                    let bit = 1u64 << (i % 64);
                    if round.drop_mask & bit != 0 {
                        continue;
                    }
                    let copies = 1 + usize::from(round.duplicate_mask & bit != 0);
                    for _ in 0..copies {
                        receiver
                            .receive(*sequence, *fragment, bytes, &mut admit, &mut delivered)
                            .expect("the sender only sends valid fragments");
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

            // A clean channel and a caller that takes everything: whatever
            // is outstanding must land.
            for _ in 0..600 {
                receiver
                    .consume(&mut |_| true, &mut delivered)
                    .expect("the sender only sends valid fragments");
                let mut samples = Vec::new();
                sender.acknowledge(receiver.ack(), now_ms, &mut samples);
                if sender.outbound_is_idle() {
                    break;
                }
                now_ms += RTO_MS;
                for (sequence, fragment, bytes) in transmit(&mut sender, now_ms) {
                    receiver
                        .receive(sequence, fragment, &bytes, &mut |_| true, &mut delivered)
                        .expect("the sender only sends valid fragments");
                }
            }
            prop_assert!(sender.outbound_is_idle(), "sender never drained");
            prop_assert_eq!(sender.held(), (0, 0));
            prop_assert_eq!(receiver.retained_bytes(), 0);
            prop_assert_eq!(delivered, sent);
            Ok(())
        });
    }
}
