//! One WebSocket connection's queues: per-lane outbound messages released
//! as fragments by deficit round robin, per-lane reassembly and inbound
//! queues, and a latest-state slot each way. Shared by the native and
//! browser transports.

use std::collections::VecDeque;

use super::codec::{ENVELOPE_HEADER_LEN, ENVELOPE_TOTAL_LEN, Envelope, WEBSOCKET_FRAGMENT_BYTES};
use crate::lanes::{Fragment, LaneScheduler, Reassembly};
use crate::{
    Delivery, DisconnectReason, Lane, MAX_LATEST_STATE_BYTES, MAX_RELIABLE_MESSAGE_BYTES,
    RELIABLE_LANES, ReliableCapacity, ReliableConfig, SendError,
};

fn lane(index: usize) -> Lane {
    Lane::new(u8::try_from(index).expect("lane index fits")).expect("index below RELIABLE_LANES")
}

/// One frame ready to encode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OutboundFrame {
    pub(super) delivery: Delivery,
    pub(super) sequence: u64,
    pub(super) fragment: Fragment,
    pub(super) payload: Vec<u8>,
}

impl OutboundFrame {
    pub(super) fn envelope(&self) -> Envelope<'_> {
        Envelope {
            delivery: self.delivery,
            sequence: self.sequence,
            fragment: self.fragment,
            payload: &self.payload,
        }
    }
}

/// A queued outbound message and the release generation it waits for.
#[derive(Debug)]
struct Staged {
    payload: Vec<u8>,
    generation: u64,
}

/// One lane's outbound messages. The front message leaves as fragments;
/// it counts against the lane's bounds until its last fragment is taken.
#[derive(Debug, Default)]
struct OutboundLane {
    messages: VecDeque<Staged>,
    /// Bytes of the front message already taken as fragments.
    taken: usize,
    bytes: usize,
}

impl OutboundLane {
    fn released(&self, generation: u64) -> bool {
        self.messages
            .front()
            .is_some_and(|message| message.generation <= generation)
    }

    /// The front message's next fragment and its end.
    fn next_fragment(&self) -> Option<(Fragment, usize)> {
        let front = self.messages.front()?;
        Some(Fragment::at(
            front.payload.len(),
            self.taken,
            WEBSOCKET_FRAGMENT_BYTES,
            0,
        ))
    }

    fn take_fragment(&mut self) -> Option<(Fragment, Vec<u8>)> {
        let (fragment, end) = self.next_fragment()?;
        let front = self.messages.front().expect("a fragment has a message");
        if end < front.payload.len() {
            let payload = front.payload[self.taken..end].to_vec();
            self.taken = end;
            return Some((fragment, payload));
        }
        let message = self.messages.pop_front().expect("checked above");
        self.bytes -= message.payload.len();
        let payload = if self.taken == 0 {
            message.payload
        } else {
            message.payload[self.taken..].to_vec()
        };
        self.taken = 0;
        Some((fragment, payload))
    }
}

/// One lane's inbound reassembly and completed messages.
#[derive(Debug, Default)]
struct InboundLane {
    reassembly: Reassembly,
    messages: VecDeque<Vec<u8>>,
    bytes: usize,
}

#[derive(Debug)]
pub(super) struct PeerState {
    reliable: ReliableConfig,
    outbound: [OutboundLane; RELIABLE_LANES],
    outbound_latest: Option<OutboundFrame>,
    /// Released latest state waits for this generation, like reliable.
    outbound_latest_generation: u64,
    outbound_scheduler: LaneScheduler,
    inbound: [InboundLane; RELIABLE_LANES],
    inbound_latest: Option<Vec<u8>>,
    inbound_scheduler: LaneScheduler,
    next_latest_sequence: u64,
    last_inbound_latest_sequence: u64,
    terminal: Option<DisconnectReason>,
    graceful_closing: bool,
    graceful_started_ms: Option<u64>,
    staging_generation: u64,
    released_generation: u64,
}

impl PeerState {
    pub(super) fn new(reliable: &ReliableConfig) -> Self {
        Self {
            reliable: reliable.clone(),
            outbound: Default::default(),
            outbound_latest: None,
            outbound_latest_generation: 0,
            outbound_scheduler: LaneScheduler::new(reliable),
            inbound: Default::default(),
            inbound_latest: None,
            inbound_scheduler: LaneScheduler::new(reliable),
            next_latest_sequence: 1,
            last_inbound_latest_sequence: 0,
            terminal: None,
            graceful_closing: false,
            graceful_started_ms: None,
            staging_generation: 1,
            released_generation: 0,
        }
    }

    /// Queues a payload, or refuses it whole. A full reliable lane returns
    /// `WouldBlock` and leaves the connection open.
    pub(super) fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        if self.terminal.is_some() || self.graceful_closing {
            return Err(SendError::Disconnected);
        }
        let cap = match delivery {
            Delivery::Reliable(_) => MAX_RELIABLE_MESSAGE_BYTES,
            Delivery::LatestState => MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > cap {
            return Err(SendError::PayloadTooLarge);
        }
        match delivery {
            Delivery::Reliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let queue = &mut self.outbound[lane.index()];
                if queue.messages.len() >= bounds.outbound_messages
                    || queue.bytes + payload.len() > bounds.outbound_bytes
                {
                    return Err(SendError::WouldBlock);
                }
                queue.bytes += payload.len();
                queue.messages.push_back(Staged {
                    payload: payload.to_vec(),
                    generation: self.staging_generation,
                });
            }
            Delivery::LatestState => {
                let sequence = self.next_latest_sequence;
                let Some(next) = sequence.checked_add(1) else {
                    self.close(DisconnectReason::ProtocolViolation);
                    return Err(SendError::Disconnected);
                };
                self.next_latest_sequence = next;
                self.outbound_latest = Some(OutboundFrame {
                    delivery,
                    sequence,
                    fragment: Fragment::Whole,
                    payload: payload.to_vec(),
                });
                self.outbound_latest_generation = self.staging_generation;
            }
        }
        Ok(())
    }

    /// What `lane` admits now; all zeros once closing or closed.
    pub(super) fn capacity(&self, lane: Lane) -> ReliableCapacity {
        if self.terminal.is_some() || self.graceful_closing {
            return ReliableCapacity::default();
        }
        let bounds = &self.reliable.lanes[lane.index()];
        let queue = &self.outbound[lane.index()];
        ReliableCapacity::remaining(
            bounds
                .outbound_messages
                .saturating_sub(queue.messages.len()),
            bounds.outbound_bytes.saturating_sub(queue.bytes),
        )
    }

    /// Applies one received frame. A reliable fragment goes through its
    /// lane's reassembly; a completed message joins the lane's inbound queue
    /// within the lane's bounds.
    pub(super) fn receive(&mut self, envelope: Envelope<'_>) -> Result<(), DisconnectReason> {
        if self.terminal.is_some() {
            return Err(DisconnectReason::Peer);
        }
        let failure = match envelope.delivery {
            Delivery::LatestState if envelope.sequence <= self.last_inbound_latest_sequence => {
                DisconnectReason::ProtocolViolation
            }
            Delivery::LatestState => {
                self.last_inbound_latest_sequence = envelope.sequence;
                self.inbound_latest = Some(envelope.payload.to_vec());
                return Ok(());
            }
            Delivery::Reliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let queue = &mut self.inbound[lane.index()];
                match queue.reassembly.push(
                    envelope.fragment,
                    envelope.payload,
                    MAX_RELIABLE_MESSAGE_BYTES,
                ) {
                    Ok(None) => return Ok(()),
                    Ok(Some(message))
                        if queue.messages.len() < bounds.inbound_messages
                            && queue.bytes + message.len() <= bounds.inbound_bytes =>
                    {
                        queue.bytes += message.len();
                        queue.messages.push_back(message);
                        return Ok(());
                    }
                    Ok(Some(_)) => DisconnectReason::InboundOverflow,
                    Err(_) => DisconnectReason::ProtocolViolation,
                }
            }
        };
        self.close(failure);
        Err(failure)
    }

    /// The next inbound message: reliable lanes interleaved by weight, then
    /// the newest latest state.
    pub(super) fn pop_inbound(&mut self) -> Option<(Delivery, Vec<u8>)> {
        let inbound = &self.inbound;
        if let Some(index) = self
            .inbound_scheduler
            .next(|lane| !inbound[lane].messages.is_empty())
        {
            let queue = &mut self.inbound[index];
            let message = queue
                .messages
                .pop_front()
                .expect("the scheduler picked a backlog");
            queue.bytes -= message.len();
            return Some((Delivery::Reliable(lane(index)), message));
        }
        self.inbound_latest
            .take()
            .map(|payload| (Delivery::LatestState, payload))
    }

    pub(super) fn release_outbound(&mut self) -> Result<(), DisconnectReason> {
        if self.terminal.is_some() {
            return Err(self.terminal.unwrap_or(DisconnectReason::Peer));
        }
        self.released_generation = self.staging_generation;
        let Some(next) = self.staging_generation.checked_add(1) else {
            self.close(DisconnectReason::ProtocolViolation);
            return Err(DisconnectReason::ProtocolViolation);
        };
        self.staging_generation = next;
        Ok(())
    }

    fn released_lane(&self) -> Option<usize> {
        let (outbound, released) = (&self.outbound, self.released_generation);
        self.outbound_scheduler
            .peek(|lane| outbound[lane].released(released))
    }

    /// Whether a released frame is waiting.
    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub(super) fn has_released_outbound(&self) -> bool {
        self.next_released_frame_len().is_some()
    }

    /// The delivery and encoded length of the frame
    /// [`Self::pop_released_frame`] would return.
    pub(super) fn next_released_frame_len(&self) -> Option<(Delivery, usize)> {
        if let Some(index) = self.released_lane() {
            let queue = &self.outbound[index];
            let (fragment, end) = queue
                .next_fragment()
                .expect("a released lane has a message");
            let total = if fragment.total().is_some() {
                ENVELOPE_TOTAL_LEN
            } else {
                0
            };
            return Some((
                Delivery::Reliable(lane(index)),
                ENVELOPE_HEADER_LEN + total + end - queue.taken,
            ));
        }
        self.outbound_latest
            .as_ref()
            .filter(|_| self.outbound_latest_generation <= self.released_generation)
            .map(|frame| (frame.delivery, ENVELOPE_HEADER_LEN + frame.payload.len()))
    }

    /// The next released frame: a fragment from the lane deficit round
    /// robin picks, else the latest state once every released reliable
    /// fragment has gone.
    pub(super) fn pop_released_frame(&mut self) -> Option<OutboundFrame> {
        let (outbound, released) = (&self.outbound, self.released_generation);
        if let Some(index) = self
            .outbound_scheduler
            .next(|lane| outbound[lane].released(released))
        {
            let (fragment, payload) = self.outbound[index]
                .take_fragment()
                .expect("a released lane has a message");
            return Some(OutboundFrame {
                delivery: Delivery::Reliable(lane(index)),
                sequence: 0,
                fragment,
                payload,
            });
        }
        if self.outbound_latest_generation <= self.released_generation {
            return self.outbound_latest.take();
        }
        None
    }

    pub(super) fn begin_graceful_close(&mut self, now_ms: u64) {
        if self.terminal.is_none() {
            self.graceful_closing = true;
            self.graceful_started_ms.get_or_insert(now_ms);
        }
    }

    pub(super) const fn graceful_closing(&self) -> bool {
        self.graceful_closing
    }

    pub(super) fn expire_graceful_close(&mut self, now_ms: u64) -> bool {
        if self.graceful_started_ms.is_some_and(|started_ms| {
            now_ms.saturating_sub(started_ms) >= crate::websocket::GRACEFUL_CLOSE_TIMEOUT_MS
        }) {
            self.close(DisconnectReason::Local);
            return true;
        }
        false
    }

    pub(super) fn finish_graceful_close(&mut self) {
        if self.graceful_closing
            && self.outbound.iter().all(|lane| lane.messages.is_empty())
            && self.outbound_latest.is_none()
        {
            self.terminal = Some(DisconnectReason::Local);
            self.graceful_closing = false;
            self.graceful_started_ms = None;
        }
    }

    pub(super) fn close(&mut self, reason: DisconnectReason) {
        if self.terminal.is_none() {
            self.terminal = Some(reason);
            self.graceful_closing = false;
            self.graceful_started_ms = None;
            self.outbound = Default::default();
            self.outbound_latest = None;
            if matches!(
                reason,
                DisconnectReason::ProtocolViolation | DisconnectReason::InboundOverflow
            ) {
                self.inbound = Default::default();
                self.inbound_latest = None;
            }
        }
    }

    pub(super) const fn terminal(&self) -> Option<DisconnectReason> {
        self.terminal
    }

    #[cfg(test)]
    pub(super) fn set_next_latest_sequence(&mut self, sequence: u64) {
        self.next_latest_sequence = sequence;
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::DEFAULT_LANE_OUTBOUND_MESSAGES;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn new_peer() -> PeerState {
        PeerState::new(&ReliableConfig::DEFAULT)
    }

    fn frames(peer: &mut PeerState) -> Vec<OutboundFrame> {
        std::iter::from_fn(|| peer.pop_released_frame()).collect()
    }

    /// #254: a staged frame waits for its release; the latest slot goes out
    /// once at its last value; the graceful close expires exactly at its
    /// timeout; a lane admits exactly its message cap and only that lane
    /// refuses past it.
    #[wasm_bindgen_test(unsupported = test)]
    fn release_generations_expiry_and_lane_caps_are_exact() {
        let mut state = new_peer();
        state.send(Delivery::LatestState, b"s1").unwrap();
        assert!(!state.has_released_outbound(), "staged, not released");
        state.release_outbound().unwrap();
        state.send(Delivery::LatestState, b"s2").unwrap();
        assert!(
            !state.has_released_outbound(),
            "the slot holds the staged value"
        );
        state.release_outbound().unwrap();
        assert_eq!(
            state.next_released_frame_len(),
            Some((Delivery::LatestState, ENVELOPE_HEADER_LEN + 2))
        );
        assert_eq!(frames(&mut state).len(), 1);
        assert!(!state.has_released_outbound());

        let mut state = new_peer();
        state.begin_graceful_close(100);
        assert!(state.graceful_closing());
        assert!(
            !state.expire_graceful_close(100 + crate::websocket::GRACEFUL_CLOSE_TIMEOUT_MS - 1)
        );
        assert!(state.expire_graceful_close(100 + crate::websocket::GRACEFUL_CLOSE_TIMEOUT_MS));
        assert_eq!(state.terminal(), Some(DisconnectReason::Local));

        let mut state = new_peer();
        for _ in 0..DEFAULT_LANE_OUTBOUND_MESSAGES {
            state.send(Delivery::RELIABLE_ORDERED, b"x").unwrap();
        }
        assert_eq!(
            state.send(Delivery::RELIABLE_ORDERED, b"x"),
            Err(SendError::WouldBlock)
        );
        state
            .send(Delivery::Reliable(lane(1)), b"x")
            .expect("another lane still admits");
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn latest_is_one_replaceable_slot_until_drain() {
        let mut peer = new_peer();
        peer.send(Delivery::LatestState, b"old").unwrap();
        peer.send(Delivery::LatestState, b"middle").unwrap();
        peer.send(Delivery::LatestState, b"new").unwrap();
        peer.release_outbound().unwrap();
        let frame = peer.pop_released_frame().unwrap();
        assert_eq!(frame.payload, b"new");
        assert_eq!(frame.sequence, 3);
        assert!(peer.pop_released_frame().is_none());
    }

    /// #267: a full reliable lane refuses the send without closing the
    /// peer; a drained message returns the allowance and the retry succeeds.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_reliable_lane_refuses_without_closing() {
        let mut peer = new_peer();
        for _ in 0..DEFAULT_LANE_OUTBOUND_MESSAGES {
            peer.send(Delivery::RELIABLE_ORDERED, b"x").unwrap();
        }
        assert_eq!(
            peer.send(Delivery::RELIABLE_ORDERED, b"retry"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(peer.terminal(), None);
        peer.release_outbound().unwrap();
        assert!(peer.pop_released_frame().is_some());
        peer.send(Delivery::RELIABLE_ORDERED, b"retry").unwrap();

        // The byte allowance is exact: ten bytes short admits ten, not 11.
        let mut peer = new_peer();
        let full = crate::DEFAULT_LANE_OUTBOUND_BYTES / MAX_RELIABLE_MESSAGE_BYTES;
        for _ in 1..full {
            peer.send(
                Delivery::RELIABLE_ORDERED,
                &vec![0; MAX_RELIABLE_MESSAGE_BYTES],
            )
            .unwrap();
        }
        peer.send(
            Delivery::RELIABLE_ORDERED,
            &vec![0; MAX_RELIABLE_MESSAGE_BYTES - 10],
        )
        .unwrap();
        assert_eq!(peer.capacity(Lane::DEFAULT).bytes, 10);
        assert_eq!(
            peer.send(Delivery::RELIABLE_ORDERED, &[0; 11]),
            Err(SendError::WouldBlock)
        );
        peer.send(Delivery::RELIABLE_ORDERED, &[0; 10]).unwrap();
    }

    /// Defect (#268): a long message holding a lane's frames back by more
    /// than one fragment (netcode.md 13), a frame past the fragment cap, or
    /// fragments that do not reassemble. Oracle: with a 40 KiB message on
    /// lane 1 queued before a small one on lane 0, the small one leaves
    /// after at most one lane-1 fragment, every frame fits
    /// `WEBSOCKET_FRAGMENT_BYTES`, and a receiving peer gets both messages
    /// intact.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_long_message_yields_to_other_lanes_after_one_fragment() {
        let mut sender = new_peer();
        let long: Vec<u8> = (0..40 * 1024_u32).map(|i| (i % 251) as u8).collect();
        sender.send(Delivery::Reliable(lane(1)), &long).unwrap();
        sender
            .send(Delivery::RELIABLE_ORDERED, b"realtime")
            .unwrap();
        sender.release_outbound().unwrap();
        let sent = frames(&mut sender);
        let realtime = sent
            .iter()
            .position(|frame| frame.delivery == Delivery::RELIABLE_ORDERED)
            .unwrap();
        assert!(realtime <= 1, "realtime waited behind {realtime} fragments");
        assert!(
            sent.iter()
                .all(|frame| frame.payload.len() <= WEBSOCKET_FRAGMENT_BYTES)
        );

        let mut receiver = new_peer();
        for frame in &sent {
            receiver.receive(frame.envelope()).unwrap();
        }
        let mut got = Vec::new();
        while let Some(message) = receiver.pop_inbound() {
            got.push(message);
        }
        got.sort();
        assert_eq!(
            got,
            vec![
                (Delivery::RELIABLE_ORDERED, b"realtime".to_vec()),
                (Delivery::Reliable(lane(1)), long),
            ]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn sequence_exhaustion_is_checked_and_terminal() {
        let mut peer = new_peer();
        peer.set_next_latest_sequence(u64::MAX);
        assert_eq!(
            peer.send(Delivery::LatestState, b"state"),
            Err(SendError::Disconnected)
        );
        assert_eq!(peer.terminal(), Some(DisconnectReason::ProtocolViolation));
    }

    /// A stale latest state and a fragment that breaks the reassembly
    /// table are protocol violations; a lane's completed messages past its
    /// inbound bound are an overflow. Each closes the peer and delivers
    /// nothing more.
    #[wasm_bindgen_test(unsupported = test)]
    fn inbound_violations_close_the_peer_with_their_reason() {
        let latest = |sequence| Envelope {
            delivery: Delivery::LatestState,
            sequence,
            fragment: Fragment::Whole,
            payload: b"",
        };
        let reliable = |fragment| Envelope {
            delivery: Delivery::Reliable(lane(2)),
            sequence: 0,
            fragment,
            payload: b"x",
        };
        let mut peer = new_peer();
        peer.receive(latest(2)).unwrap();
        assert_eq!(
            peer.receive(latest(1)),
            Err(DisconnectReason::ProtocolViolation)
        );
        assert!(peer.pop_inbound().is_none());

        let mut peer = PeerState::new(&ReliableConfig::DEFAULT);
        assert_eq!(
            peer.receive(reliable(Fragment::Last)),
            Err(DisconnectReason::ProtocolViolation)
        );

        let mut config = ReliableConfig::DEFAULT;
        config.lanes[2].inbound_messages = 1;
        let mut peer = PeerState::new(&config);
        peer.receive(reliable(Fragment::Whole)).unwrap();
        assert_eq!(
            peer.receive(reliable(Fragment::Whole)),
            Err(DisconnectReason::InboundOverflow)
        );
        assert!(
            peer.pop_inbound().is_none(),
            "nothing is delivered after it"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;
    use std::collections::VecDeque;

    #[derive(Debug, Clone)]
    enum Op {
        SendReliable(usize, Vec<u8>),
        SendLatest(Vec<u8>),
        Release,
        PopReleased,
        ReceiveReliable(usize, Vec<u8>),
        ReceiveLatest(u64),
        PopInbound,
    }

    fn op() -> impl Strategy<Value = Op> {
        let lane = 0..RELIABLE_LANES;
        prop_oneof![
            4 => (lane.clone(), bytes(2_500)).prop_map(|(l, p)| Op::SendReliable(l, p)),
            1 => (lane.clone(), 0..=MAX_RELIABLE_MESSAGE_BYTES)
                .prop_map(|(l, len)| Op::SendReliable(l, vec![1; len])),
            2 => bytes(64).prop_map(Op::SendLatest),
            2 => Just(Op::Release),
            4 => Just(Op::PopReleased),
            2 => (lane, bytes(64)).prop_map(|(l, p)| Op::ReceiveReliable(l, p)),
            1 => (0u64..4).prop_map(Op::ReceiveLatest),
            2 => Just(Op::PopInbound),
        ]
    }

    /// One outbound lane of the model: messages with the release generation
    /// they wait for, and how much of the front one has left as fragments.
    #[derive(Default)]
    struct ModelLane {
        messages: VecDeque<(Vec<u8>, u64)>,
        taken: usize,
    }

    impl ModelLane {
        fn bytes(&self) -> usize {
            self.messages.iter().map(|(payload, _)| payload.len()).sum()
        }
    }

    /// Defect: a released frame leaking a staged one, a lane's fragments out
    /// of order or not covering its message, a frame past the fragment cap,
    /// latest state overtaking released reliable frames, a cap enforced
    /// after the queue grew or on the wrong lane, a full lane that closes the
    /// peer, a capacity report that disagrees with admission, inbound lanes
    /// reordered or past their bounds, or a stale latest sequence accepted.
    /// Oracle: a model with a FIFO per lane plus one slot each way, the
    /// netcode.md 10, 11 and 13 rules, and the module's release generations.
    #[test]
    fn peer_state_matches_the_lane_fifo_plus_slot_model() {
        check(prop::collection::vec(op(), 1..250), |ops| {
            let config = ReliableConfig::DEFAULT;
            let mut state = PeerState::new(&config);
            let mut outbound: [ModelLane; RELIABLE_LANES] = Default::default();
            let mut latest: Option<(u64, Vec<u8>, u64)> = None;
            let mut next_latest = 1u64;
            let (mut staging, mut released) = (1u64, 0u64);
            let mut terminal: Option<DisconnectReason> = None;
            let mut inbound: [VecDeque<Vec<u8>>; RELIABLE_LANES] = Default::default();
            let mut inbound_latest: Option<Vec<u8>> = None;
            let mut last_inbound_latest = 0u64;
            let close = |reason,
                         terminal: &mut Option<DisconnectReason>,
                         outbound: &mut [ModelLane; RELIABLE_LANES],
                         latest: &mut Option<(u64, Vec<u8>, u64)>,
                         inbound: &mut [VecDeque<Vec<u8>>; RELIABLE_LANES],
                         inbound_latest: &mut Option<Vec<u8>>| {
                *terminal = Some(reason);
                *outbound = Default::default();
                *latest = None;
                *inbound = Default::default();
                *inbound_latest = None;
            };

            for op in ops {
                match op {
                    Op::SendReliable(index, payload) => {
                        let capacity = state.capacity(lane(index));
                        let result = state.send(Delivery::Reliable(lane(index)), &payload);
                        prop_assert_eq!(
                            result.is_ok(),
                            capacity.messages >= 1 && capacity.bytes >= payload.len()
                        );
                        let bounds = &config.lanes[index];
                        let model = &mut outbound[index];
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(SendError::Disconnected));
                        } else if model.messages.len() >= bounds.outbound_messages
                            || model.bytes() + payload.len() > bounds.outbound_bytes
                        {
                            prop_assert_eq!(result, Err(SendError::WouldBlock));
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            model.messages.push_back((payload, staging));
                        }
                    }
                    Op::SendLatest(payload) => {
                        let result = state.send(Delivery::LatestState, &payload);
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(SendError::Disconnected));
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            latest = Some((next_latest, payload, staging));
                            next_latest += 1;
                        }
                    }
                    Op::Release => {
                        let result = state.release_outbound();
                        if let Some(reason) = terminal {
                            prop_assert_eq!(result, Err(reason));
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            released = staging;
                            staging += 1;
                        }
                    }
                    Op::PopReleased => {
                        let peeked = state.next_released_frame_len();
                        let got = state.pop_released_frame();
                        prop_assert_eq!(
                            peeked,
                            got.as_ref().map(|frame| {
                                let encoded =
                                    super::super::encode_envelope(*b"TST", &frame.envelope())
                                        .expect("a queued frame encodes");
                                (frame.delivery, encoded.len())
                            })
                        );
                        let any_released = outbound
                            .iter()
                            .any(|lane| lane.messages.front().is_some_and(|(_, g)| *g <= released));
                        match got {
                            Some(OutboundFrame {
                                delivery: Delivery::Reliable(lane),
                                sequence,
                                fragment,
                                payload,
                            }) => {
                                prop_assert_eq!(sequence, 0);
                                prop_assert!(payload.len() <= WEBSOCKET_FRAGMENT_BYTES);
                                let model = &mut outbound[lane.index()];
                                let (message, generation) =
                                    model.messages.front().expect("a released message");
                                prop_assert!(*generation <= released, "staged frame leaked");
                                let start = model.taken;
                                let end = start + payload.len();
                                prop_assert_eq!(&message[start..end], &payload[..]);
                                let expected = match (start == 0, end == message.len()) {
                                    (true, true) => Fragment::Whole,
                                    (true, false) => Fragment::First {
                                        total: message.len() as u32,
                                    },
                                    (false, false) => Fragment::Middle,
                                    (false, true) => Fragment::Last,
                                };
                                prop_assert_eq!(fragment, expected);
                                prop_assert!(
                                    !payload.is_empty() || message.is_empty(),
                                    "an empty fragment of a non-empty message"
                                );
                                if end == message.len() {
                                    model.messages.pop_front();
                                    model.taken = 0;
                                } else {
                                    model.taken = end;
                                }
                            }
                            Some(frame) => {
                                prop_assert!(!any_released, "latest overtook released reliable");
                                let (sequence, payload, generation) =
                                    latest.take().expect("a latest frame");
                                prop_assert!(generation <= released);
                                prop_assert_eq!(frame.sequence, sequence);
                                prop_assert_eq!(frame.payload, payload);
                            }
                            None => {
                                prop_assert!(!any_released);
                                prop_assert!(latest.as_ref().is_none_or(|(_, _, g)| *g > released));
                            }
                        }
                    }
                    Op::ReceiveReliable(index, payload) => {
                        let result = state.receive(Envelope {
                            delivery: Delivery::Reliable(lane(index)),
                            sequence: 0,
                            fragment: Fragment::Whole,
                            payload: &payload,
                        });
                        let bounds = &config.lanes[index];
                        let queued: usize = inbound[index].iter().map(Vec::len).sum();
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(DisconnectReason::Peer));
                        } else if inbound[index].len() >= bounds.inbound_messages
                            || queued + payload.len() > bounds.inbound_bytes
                        {
                            prop_assert_eq!(result, Err(DisconnectReason::InboundOverflow));
                            close(
                                DisconnectReason::InboundOverflow,
                                &mut terminal,
                                &mut outbound,
                                &mut latest,
                                &mut inbound,
                                &mut inbound_latest,
                            );
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            inbound[index].push_back(payload);
                        }
                    }
                    Op::ReceiveLatest(step) => {
                        // Steps of 0 replay the last sequence: a protocol violation.
                        let sequence = last_inbound_latest + step;
                        let payload = [step as u8];
                        let result = state.receive(Envelope {
                            delivery: Delivery::LatestState,
                            sequence,
                            fragment: Fragment::Whole,
                            payload: &payload,
                        });
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(DisconnectReason::Peer));
                        } else if sequence <= last_inbound_latest {
                            prop_assert_eq!(result, Err(DisconnectReason::ProtocolViolation));
                            close(
                                DisconnectReason::ProtocolViolation,
                                &mut terminal,
                                &mut outbound,
                                &mut latest,
                                &mut inbound,
                                &mut inbound_latest,
                            );
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            last_inbound_latest = sequence;
                            inbound_latest = Some(payload.to_vec());
                        }
                    }
                    Op::PopInbound => match state.pop_inbound() {
                        // Any lane may come next; within a lane, in order.
                        Some((Delivery::Reliable(lane), payload)) => {
                            prop_assert_eq!(inbound[lane.index()].pop_front(), Some(payload));
                        }
                        // Reliable messages drain before the latest slot.
                        Some((Delivery::LatestState, payload)) => {
                            prop_assert!(inbound.iter().all(VecDeque::is_empty));
                            prop_assert_eq!(inbound_latest.take(), Some(payload));
                        }
                        None => {
                            prop_assert!(inbound.iter().all(VecDeque::is_empty));
                            prop_assert_eq!(inbound_latest.take(), None);
                        }
                    },
                }
                prop_assert_eq!(state.terminal(), terminal);
            }
            Ok(())
        });
    }
}
