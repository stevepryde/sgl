use std::collections::VecDeque;

use crate::{
    Delivery, DisconnectReason, MAX_LATEST_STATE_BYTES, MAX_RELIABLE_MESSAGE_BYTES,
    RELIABLE_INBOUND_BYTES, RELIABLE_INBOUND_MESSAGES, RELIABLE_OUTBOUND_BYTES,
    RELIABLE_OUTBOUND_MESSAGES, ReliableCapacity, SendError,
};

#[derive(Clone, Debug)]
pub(super) struct QueuedFrame {
    pub(super) delivery: Delivery,
    pub(super) sequence: u64,
    pub(super) payload: Vec<u8>,
    pub(super) generation: u64,
}

#[derive(Debug)]
struct LaneQueue {
    reliable: VecDeque<QueuedFrame>,
    reliable_bytes: usize,
    latest: Option<QueuedFrame>,
    max_reliable_messages: usize,
    max_reliable_bytes: usize,
}

impl LaneQueue {
    fn new(max_reliable_messages: usize, max_reliable_bytes: usize) -> Self {
        Self {
            reliable: VecDeque::new(),
            reliable_bytes: 0,
            latest: None,
            max_reliable_messages,
            max_reliable_bytes,
        }
    }

    fn admits_reliable(&self, len: usize) -> bool {
        self.reliable.len() < self.max_reliable_messages
            && self
                .reliable_bytes
                .checked_add(len)
                .is_some_and(|bytes| bytes <= self.max_reliable_bytes)
    }

    fn reliable_capacity(&self) -> ReliableCapacity {
        ReliableCapacity::remaining(
            self.max_reliable_messages
                .saturating_sub(self.reliable.len()),
            self.max_reliable_bytes.saturating_sub(self.reliable_bytes),
        )
    }

    fn push(&mut self, frame: QueuedFrame) -> Result<(), ()> {
        match frame.delivery {
            Delivery::Reliable(_) => {
                if !self.admits_reliable(frame.payload.len()) {
                    return Err(());
                }
                self.reliable_bytes += frame.payload.len();
                self.reliable.push_back(frame);
            }
            Delivery::LatestState => self.latest = Some(frame),
        }
        Ok(())
    }

    fn pop(&mut self) -> Option<QueuedFrame> {
        if let Some(frame) = self.reliable.pop_front() {
            self.reliable_bytes -= frame.payload.len();
            Some(frame)
        } else {
            self.latest.take()
        }
    }

    fn clear(&mut self) {
        self.reliable.clear();
        self.reliable_bytes = 0;
        self.latest = None;
    }
}

#[derive(Debug)]
pub(super) struct PeerState {
    inbound: LaneQueue,
    outbound: LaneQueue,
    next_latest_sequence: u64,
    last_inbound_latest_sequence: u64,
    terminal: Option<DisconnectReason>,
    graceful_closing: bool,
    graceful_started_ms: Option<u64>,
    staging_generation: u64,
    released_generation: u64,
}

impl PeerState {
    pub(super) fn new() -> Self {
        Self {
            inbound: LaneQueue::new(RELIABLE_INBOUND_MESSAGES, RELIABLE_INBOUND_BYTES),
            outbound: LaneQueue::new(RELIABLE_OUTBOUND_MESSAGES, RELIABLE_OUTBOUND_BYTES),
            next_latest_sequence: 1,
            last_inbound_latest_sequence: 0,
            terminal: None,
            graceful_closing: false,
            graceful_started_ms: None,
            staging_generation: 1,
            released_generation: 0,
        }
    }

    /// Queues a payload, or refuses it whole. A full reliable queue returns
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
        let sequence = match delivery {
            Delivery::Reliable(_) => {
                if !self.outbound.admits_reliable(payload.len()) {
                    return Err(SendError::WouldBlock);
                }
                0
            }
            Delivery::LatestState => {
                let sequence = self.next_latest_sequence;
                let Some(next) = sequence.checked_add(1) else {
                    self.close(DisconnectReason::ProtocolViolation);
                    return Err(SendError::Disconnected);
                };
                self.next_latest_sequence = next;
                sequence
            }
        };
        let frame = QueuedFrame {
            delivery,
            sequence,
            payload: payload.to_vec(),
            generation: self.staging_generation,
        };
        self.outbound
            .push(frame)
            .map_err(|()| SendError::WouldBlock)
    }

    /// What the reliable lane admits now; all zeros once closing or closed.
    pub(super) fn capacity(&self) -> ReliableCapacity {
        if self.terminal.is_some() || self.graceful_closing {
            return ReliableCapacity::default();
        }
        self.outbound.reliable_capacity()
    }

    pub(super) fn receive(&mut self, frame: QueuedFrame) -> Result<(), DisconnectReason> {
        if self.terminal.is_some() {
            return Err(DisconnectReason::Peer);
        }
        if frame.delivery == Delivery::LatestState {
            if frame.sequence <= self.last_inbound_latest_sequence {
                self.close(DisconnectReason::ProtocolViolation);
                return Err(DisconnectReason::ProtocolViolation);
            }
            self.last_inbound_latest_sequence = frame.sequence;
        }
        if self.inbound.push(frame).is_err() {
            self.close(DisconnectReason::InboundOverflow);
            return Err(DisconnectReason::InboundOverflow);
        }
        Ok(())
    }

    pub(super) fn pop_inbound(&mut self) -> Option<QueuedFrame> {
        self.inbound.pop()
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

    pub(super) fn next_released_outbound(&self) -> Option<(Delivery, usize)> {
        self.outbound
            .reliable
            .front()
            .filter(|frame| frame.generation <= self.released_generation)
            .map(|frame| (frame.delivery, frame.payload.len()))
            .or_else(|| {
                self.outbound
                    .latest
                    .as_ref()
                    .filter(|frame| frame.generation <= self.released_generation)
                    .map(|frame| (frame.delivery, frame.payload.len()))
            })
    }

    pub(super) fn pop_released_outbound(&mut self) -> Option<QueuedFrame> {
        if self
            .outbound
            .reliable
            .front()
            .is_some_and(|frame| frame.generation <= self.released_generation)
        {
            let frame = self.outbound.reliable.pop_front().expect("front checked");
            self.outbound.reliable_bytes -= frame.payload.len();
            Some(frame)
        } else if self
            .outbound
            .latest
            .as_ref()
            .is_some_and(|frame| frame.generation <= self.released_generation)
        {
            self.outbound.latest.take()
        } else {
            None
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn peek_released_outbound(&self) -> Option<QueuedFrame> {
        self.outbound
            .reliable
            .front()
            .filter(|frame| frame.generation <= self.released_generation)
            .or_else(|| {
                self.outbound
                    .latest
                    .as_ref()
                    .filter(|frame| frame.generation <= self.released_generation)
            })
            .cloned()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn acknowledge_latest(&mut self, sequence: u64) {
        if self.outbound.latest.as_ref().is_some_and(|frame| {
            frame.sequence == sequence && frame.generation <= self.released_generation
        }) {
            self.outbound.latest = None;
        }
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
            && self.outbound.reliable.is_empty()
            && self.outbound.latest.is_none()
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
            self.outbound.clear();
            if matches!(
                reason,
                DisconnectReason::ProtocolViolation | DisconnectReason::InboundOverflow
            ) {
                self.inbound.clear();
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
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #254: the latest slot is acknowledged only for its own sequence once
    /// released; the graceful close expires exactly at its timeout; the
    /// reliable lane admits exactly its message cap.
    // `acknowledge_latest` only exists on native (the worker side).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn release_generations_acknowledgement_and_caps_are_exact() {
        let mut state = PeerState::new();
        state.send(Delivery::LatestState, b"s1").unwrap();
        assert!(
            state.next_released_outbound().is_none(),
            "staged, not released"
        );
        state.acknowledge_latest(1);
        state.release_outbound().unwrap();
        assert_eq!(
            state.next_released_outbound(),
            Some((Delivery::LatestState, 2))
        );
        state.acknowledge_latest(2);
        assert_eq!(
            state.next_released_outbound(),
            Some((Delivery::LatestState, 2)),
            "wrong sequence"
        );
        state.acknowledge_latest(1);
        assert!(state.next_released_outbound().is_none());

        let mut state = PeerState::new();
        state.begin_graceful_close(100);
        assert!(state.graceful_closing());
        assert!(
            !state.expire_graceful_close(100 + crate::websocket::GRACEFUL_CLOSE_TIMEOUT_MS - 1)
        );
        assert!(state.expire_graceful_close(100 + crate::websocket::GRACEFUL_CLOSE_TIMEOUT_MS));
        assert_eq!(state.terminal(), Some(DisconnectReason::Local));

        let mut state = PeerState::new();
        for _ in 0..RELIABLE_OUTBOUND_MESSAGES {
            state.send(Delivery::RELIABLE_ORDERED, b"x").unwrap();
        }
        assert_eq!(
            state.send(Delivery::RELIABLE_ORDERED, b"x"),
            Err(SendError::WouldBlock)
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn latest_is_one_replaceable_slot_until_drain() {
        let mut peer = PeerState::new();
        peer.send(Delivery::LatestState, b"old").unwrap();
        peer.send(Delivery::LatestState, b"middle").unwrap();
        peer.send(Delivery::LatestState, b"new").unwrap();
        peer.release_outbound().unwrap();
        let frame = peer.pop_released_outbound().unwrap();
        assert_eq!(frame.payload, b"new");
        assert_eq!(frame.sequence, 3);
        assert!(peer.pop_released_outbound().is_none());
    }

    /// #267: a full reliable queue refuses the send without closing the
    /// peer; a drained frame returns the allowance and the retry succeeds.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_reliable_queue_refuses_without_closing() {
        let mut peer = PeerState::new();
        for _ in 0..RELIABLE_OUTBOUND_MESSAGES {
            peer.send(Delivery::RELIABLE_ORDERED, b"x").unwrap();
        }
        assert_eq!(
            peer.send(Delivery::RELIABLE_ORDERED, b"retry"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(peer.terminal(), None);
        peer.release_outbound().unwrap();
        assert!(peer.pop_released_outbound().is_some());
        peer.send(Delivery::RELIABLE_ORDERED, b"retry").unwrap();

        // The byte allowance is exact: ten bytes short admits ten, not 11.
        let mut peer = PeerState::new();
        let full = RELIABLE_OUTBOUND_BYTES / MAX_RELIABLE_MESSAGE_BYTES;
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
        assert_eq!(peer.capacity().bytes, 10);
        assert_eq!(
            peer.send(Delivery::RELIABLE_ORDERED, &[0; 11]),
            Err(SendError::WouldBlock)
        );
        peer.send(Delivery::RELIABLE_ORDERED, &[0; 10]).unwrap();
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn sequence_exhaustion_is_checked_and_terminal() {
        let mut peer = PeerState::new();
        peer.set_next_latest_sequence(u64::MAX);
        assert_eq!(
            peer.send(Delivery::LatestState, b"state"),
            Err(SendError::Disconnected)
        );
        assert_eq!(peer.terminal(), Some(DisconnectReason::ProtocolViolation));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn stale_latest_is_a_protocol_violation() {
        let mut peer = PeerState::new();
        let latest = |sequence| QueuedFrame {
            delivery: Delivery::LatestState,
            sequence,
            payload: vec![],
            generation: 0,
        };
        peer.receive(latest(2)).unwrap();
        assert_eq!(
            peer.receive(latest(1)),
            Err(DisconnectReason::ProtocolViolation)
        );
        assert!(peer.pop_inbound().is_none());
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
        SendReliable(Vec<u8>),
        SendLatest(Vec<u8>),
        Release,
        PopReleased,
        ReceiveReliable(Vec<u8>),
        ReceiveLatest(u64),
        PopInbound,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => bytes(2_500).prop_map(Op::SendReliable),
            1 => (0..=MAX_RELIABLE_MESSAGE_BYTES).prop_map(|len| Op::SendReliable(vec![1; len])),
            2 => bytes(64).prop_map(Op::SendLatest),
            2 => Just(Op::Release),
            3 => Just(Op::PopReleased),
            2 => bytes(64).prop_map(Op::ReceiveReliable),
            1 => (0u64..4).prop_map(Op::ReceiveLatest),
            2 => Just(Op::PopInbound),
        ]
    }

    /// Defect: a released frame leaking a staged one, reliable frames
    /// reordered around the latest slot, a cap enforced after the queue
    /// grew, a full queue that closes the peer, a capacity report that
    /// disagrees with admission, or a stale latest sequence accepted.
    /// Oracle: a model with a FIFO plus one slot, the netcode.md 10–11
    /// rules, and the module's release generations.
    #[test]
    fn peer_state_matches_the_fifo_plus_slot_model() {
        check(prop::collection::vec(op(), 1..250), |ops| {
            let mut state = PeerState::new();
            // Model: staged reliable frames carry the release generation they
            // need; the latest slot holds (sequence, payload, generation).
            let mut reliable: VecDeque<(Vec<u8>, u64)> = VecDeque::new();
            let mut reliable_bytes = 0usize;
            let mut latest: Option<(u64, Vec<u8>, u64)> = None;
            let mut next_latest = 1u64;
            let (mut staging, mut released) = (1u64, 0u64);
            let mut terminal: Option<DisconnectReason> = None;
            let mut inbound: VecDeque<(Delivery, Vec<u8>)> = VecDeque::new();
            let mut last_inbound_latest = 0u64;

            for op in ops {
                match op {
                    Op::SendReliable(payload) => {
                        let capacity = state.capacity();
                        let result = state.send(Delivery::RELIABLE_ORDERED, &payload);
                        prop_assert_eq!(
                            result.is_ok(),
                            capacity.messages >= 1 && capacity.bytes >= payload.len()
                        );
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(SendError::Disconnected));
                        } else if reliable.len() >= RELIABLE_OUTBOUND_MESSAGES
                            || reliable_bytes + payload.len() > RELIABLE_OUTBOUND_BYTES
                        {
                            prop_assert_eq!(result, Err(SendError::WouldBlock));
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            reliable_bytes += payload.len();
                            reliable.push_back((payload, staging));
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
                        let expected = if reliable.front().is_some_and(|(_, g)| *g <= released) {
                            let (payload, _) = reliable.pop_front().unwrap();
                            reliable_bytes -= payload.len();
                            Some((Delivery::RELIABLE_ORDERED, 0, payload))
                        } else if latest.as_ref().is_some_and(|(_, _, g)| *g <= released) {
                            let (sequence, payload, _) = latest.take().unwrap();
                            Some((Delivery::LatestState, sequence, payload))
                        } else {
                            None
                        };
                        let got = state
                            .pop_released_outbound()
                            .map(|f| (f.delivery, f.sequence, f.payload));
                        prop_assert_eq!(got, expected);
                    }
                    Op::ReceiveReliable(payload) => {
                        let result = state.receive(QueuedFrame {
                            delivery: Delivery::RELIABLE_ORDERED,
                            sequence: 0,
                            payload: payload.clone(),
                            generation: 0,
                        });
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(DisconnectReason::Peer));
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            inbound.push_back((Delivery::RELIABLE_ORDERED, payload));
                        }
                    }
                    Op::ReceiveLatest(step) => {
                        // Steps of 0 replay the last sequence: a protocol violation.
                        let sequence = last_inbound_latest + step;
                        let result = state.receive(QueuedFrame {
                            delivery: Delivery::LatestState,
                            sequence,
                            payload: vec![step as u8],
                            generation: 0,
                        });
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(DisconnectReason::Peer));
                        } else if sequence <= last_inbound_latest {
                            prop_assert_eq!(result, Err(DisconnectReason::ProtocolViolation));
                            terminal = Some(DisconnectReason::ProtocolViolation);
                            inbound.clear();
                            reliable.clear();
                            reliable_bytes = 0;
                            latest = None;
                        } else {
                            prop_assert_eq!(result, Ok(()));
                            last_inbound_latest = sequence;
                            inbound.retain(|(d, _)| *d == Delivery::RELIABLE_ORDERED);
                            inbound.push_back((Delivery::LatestState, vec![step as u8]));
                        }
                    }
                    Op::PopInbound => {
                        // Reliable frames drain before the single latest slot.
                        let expected = if let Some(i) = inbound
                            .iter()
                            .position(|(d, _)| *d == Delivery::RELIABLE_ORDERED)
                        {
                            inbound.remove(i)
                        } else {
                            inbound.pop_front()
                        };
                        let got = state.pop_inbound().map(|f| (f.delivery, f.payload));
                        prop_assert_eq!(got, expected);
                    }
                }
                prop_assert_eq!(state.terminal(), terminal);
            }
            Ok(())
        });
    }
}
