//! One WebSocket connection's queues: per-lane outbound reliable messages
//! (released as fragments) and unreliable messages, interleaved by deficit
//! round robin; per-lane reassembly and inbound queues; and latest state
//! each way, outbound behind only the lane frames flushed before it. Shared
//! by the native and browser transports. Nothing `send` accepted is dropped
//! while the connection lives; an unpolled receiver sheds its oldest
//! unreliable messages. A reliable frame whose message its lane cannot take
//! yet is either held by the native worker, which stops reading until
//! `poll` makes room, or closes the peer.

use std::collections::VecDeque;

use super::codec::{ENVELOPE_HEADER_LEN, ENVELOPE_TOTAL_LEN, Envelope, WEBSOCKET_FRAGMENT_BYTES};
use crate::lanes::{Fragment, InboundUsage, LaneConfig, LaneScheduler, Reassembly};
use crate::{
    Delivery, DisconnectReason, Lane, MAX_LATEST_STATE_BYTES, MAX_UNRELIABLE_BYTES, RELIABLE_LANES,
    ReliableCapacity, ReliableConfig, SendError,
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

/// One FIFO of messages and their bytes.
#[derive(Debug)]
struct Fifo<T> {
    items: VecDeque<T>,
    bytes: usize,
}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
        }
    }
}

impl<T> Fifo<T> {
    fn admits(&self, len: usize, messages: usize, bytes: usize) -> bool {
        self.items.len() < messages && self.bytes + len <= bytes
    }

    fn push(&mut self, item: T, len: usize) {
        self.bytes += len;
        self.items.push_back(item);
    }
}

impl Fifo<Vec<u8>> {
    /// Queues a received unreliable message, first dropping the oldest
    /// unpolled ones until it fits, as a full socket buffer drops datagrams.
    /// The caller has checked that it fits an empty queue.
    fn push_dropping_oldest(&mut self, message: Vec<u8>, messages: usize, bytes: usize) {
        while !self.admits(message.len(), messages, bytes) {
            let dropped = self
                .items
                .pop_front()
                .expect("a message that fits an empty queue fits after drops");
            self.bytes -= dropped.len();
        }
        let len = message.len();
        self.push(message, len);
    }
}

/// One lane's outbound messages. The front reliable message leaves as
/// fragments and counts against the lane's bounds until its last fragment
/// is taken; unreliable messages leave whole. When both are released a new
/// reliable fragment and an unreliable message take turns.
#[derive(Debug, Default)]
struct OutboundLane {
    reliable: Fifo<Staged>,
    /// Bytes of the front reliable message already taken as fragments.
    taken: usize,
    unreliable: Fifo<Staged>,
    unreliable_turn: bool,
}

impl OutboundLane {
    fn reliable_released(&self, generation: u64) -> bool {
        self.reliable
            .items
            .front()
            .is_some_and(|message| message.generation <= generation)
    }

    fn unreliable_released(&self, generation: u64) -> bool {
        self.unreliable
            .items
            .front()
            .is_some_and(|message| message.generation <= generation)
    }

    fn released(&self, generation: u64) -> bool {
        self.reliable_released(generation) || self.unreliable_released(generation)
    }

    /// Whether the next frame is the front unreliable message.
    fn unreliable_next(&self, generation: u64) -> bool {
        self.unreliable_released(generation)
            && (self.unreliable_turn || !self.reliable_released(generation))
    }

    /// The front reliable message's next fragment and its end.
    fn next_fragment(&self) -> Option<(Fragment, usize)> {
        let front = self.reliable.items.front()?;
        Some(Fragment::at(
            front.payload.len(),
            self.taken,
            WEBSOCKET_FRAGMENT_BYTES,
            0,
        ))
    }

    /// The next frame's kind, fragment and payload length.
    fn peek(&self, generation: u64) -> Option<(bool, Fragment, usize)> {
        if self.unreliable_next(generation) {
            let front = self.unreliable.items.front()?;
            return Some((true, Fragment::Whole, front.payload.len()));
        }
        let (fragment, end) = self.next_fragment()?;
        Some((false, fragment, end - self.taken))
    }

    /// Takes the next released frame: whether it is unreliable, its
    /// fragment and its payload.
    fn take(&mut self, generation: u64) -> Option<(bool, Fragment, Vec<u8>)> {
        if self.unreliable_next(generation) {
            let message = self.unreliable.items.pop_front()?;
            self.unreliable.bytes -= message.payload.len();
            self.unreliable_turn = false;
            return Some((true, Fragment::Whole, message.payload));
        }
        let (fragment, end) = self.next_fragment()?;
        self.unreliable_turn = self.unreliable_released(generation);
        let front = self
            .reliable
            .items
            .front()
            .expect("a fragment has a message");
        if end < front.payload.len() {
            let payload = front.payload[self.taken..end].to_vec();
            self.taken = end;
            return Some((false, fragment, payload));
        }
        let message = self.reliable.items.pop_front().expect("checked above");
        self.reliable.bytes -= message.payload.len();
        let payload = if self.taken == 0 {
            message.payload
        } else {
            message.payload[self.taken..].to_vec()
        };
        self.taken = 0;
        Some((false, fragment, payload))
    }

    fn is_empty(&self) -> bool {
        self.reliable.items.is_empty() && self.unreliable.items.is_empty()
    }

    /// The generation of its oldest queued message.
    fn oldest_generation(&self) -> Option<u64> {
        let front = |fifo: &Fifo<Staged>| fifo.items.front().map(|message| message.generation);
        match (front(&self.reliable), front(&self.unreliable)) {
            (Some(reliable), Some(unreliable)) => Some(reliable.min(unreliable)),
            (reliable, unreliable) => reliable.or(unreliable),
        }
    }

    /// Whether it queues a message staged after generation `after`, up to
    /// `through`.
    fn queues_between(&self, after: u64, through: u64) -> bool {
        self.reliable
            .items
            .iter()
            .chain(&self.unreliable.items)
            .any(|message| message.generation > after && message.generation <= through)
    }
}

/// One lane's inbound reassembly and the messages waiting for `poll`.
#[derive(Debug, Default)]
struct InboundLane {
    reassembly: Reassembly,
    reliable: VecDeque<Vec<u8>>,
    /// The reliable messages against the lane's inbound bounds.
    usage: InboundUsage,
    unreliable: Fifo<Vec<u8>>,
    unreliable_turn: bool,
}

impl InboundLane {
    fn is_empty(&self) -> bool {
        self.reliable.is_empty() && self.unreliable.items.is_empty()
    }

    /// The next message, reliable and unreliable taking turns.
    fn pop(&mut self, bounds: &LaneConfig) -> Option<(bool, Vec<u8>)> {
        let unreliable =
            !self.unreliable.items.is_empty() && (self.unreliable_turn || self.reliable.is_empty());
        let message = if unreliable {
            let message = self.unreliable.items.pop_front()?;
            self.unreliable.bytes -= message.len();
            message
        } else {
            let message = self.reliable.pop_front()?;
            self.usage.remove(message.len(), bounds);
            message
        };
        self.unreliable_turn = !unreliable && !self.unreliable.items.is_empty();
        Some((unreliable, message))
    }
}

/// What [`PeerState::receive_or_hold`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Received {
    /// The frame was applied.
    Accepted,
    /// The frame completes a reliable message its lane cannot take until
    /// `poll` returns some of the lane's messages. Nothing changed: the
    /// caller keeps the frame, stops reading, and offers it again once
    /// [`PeerState::can_resume`] says it fits.
    Held,
}

#[derive(Debug)]
pub(super) struct PeerState {
    reliable: ReliableConfig,
    outbound: [OutboundLane; RELIABLE_LANES],
    /// Latest state sent since the last release.
    staged_latest: Option<OutboundFrame>,
    /// Released latest states, oldest first, each with the generation it
    /// was released in: one leaves once every lane frame of its generation
    /// or earlier has, and of those whose turn has come only the newest. A
    /// release drops the states nothing was flushed after, so this holds at
    /// most one state more than the lanes have queued generations.
    released_latest: VecDeque<(u64, OutboundFrame)>,
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
    /// The lane and length of the message a held frame completes.
    stalled: Option<(usize, usize)>,
}

impl PeerState {
    pub(super) fn new(reliable: &ReliableConfig) -> Self {
        Self {
            reliable: reliable.clone(),
            outbound: Default::default(),
            staged_latest: None,
            released_latest: VecDeque::new(),
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
            stalled: None,
        }
    }

    /// Queues a payload, or refuses it whole. A full reliable or unreliable
    /// lane queue returns `WouldBlock` and leaves the connection open; an
    /// accepted message is never dropped while the connection lives.
    pub(super) fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        if self.terminal.is_some() || self.graceful_closing {
            return Err(SendError::Disconnected);
        }
        let cap = match delivery {
            Delivery::Reliable(_) => self.reliable.max_message_bytes,
            Delivery::Unreliable(_) => MAX_UNRELIABLE_BYTES,
            Delivery::LatestState => MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > cap {
            return Err(SendError::PayloadTooLarge);
        }
        let staged = |payload: &[u8], generation| Staged {
            payload: payload.to_vec(),
            generation,
        };
        match delivery {
            Delivery::Reliable(lane) => {
                let queue = &mut self.outbound[lane.index()].reliable;
                if !self.reliable.lanes[lane.index()]
                    .outbound_admits((queue.items.len(), queue.bytes), payload.len())
                {
                    return Err(SendError::WouldBlock);
                }
                queue.push(staged(payload, self.staging_generation), payload.len());
            }
            Delivery::Unreliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let queue = &mut self.outbound[lane.index()].unreliable;
                if !queue.admits(
                    payload.len(),
                    bounds.unreliable_messages,
                    bounds.unreliable_bytes,
                ) {
                    return Err(SendError::WouldBlock);
                }
                queue.push(staged(payload, self.staging_generation), payload.len());
            }
            Delivery::LatestState => {
                let sequence = self.next_latest_sequence;
                let Some(next) = sequence.checked_add(1) else {
                    self.close(DisconnectReason::ProtocolViolation);
                    return Err(SendError::Disconnected);
                };
                self.next_latest_sequence = next;
                self.staged_latest = Some(OutboundFrame {
                    delivery,
                    sequence,
                    fragment: Fragment::Whole,
                    payload: payload.to_vec(),
                });
            }
        }
        Ok(())
    }

    /// What `lane` admits now for reliable messages; all zeros once closing
    /// or closed.
    pub(super) fn capacity(&self, lane: Lane) -> ReliableCapacity {
        if self.terminal.is_some() || self.graceful_closing {
            return ReliableCapacity::default();
        }
        let queue = &self.outbound[lane.index()].reliable;
        self.reliable.lanes[lane.index()].outbound_capacity(
            (queue.items.len(), queue.bytes),
            self.reliable.max_message_bytes,
        )
    }

    /// Applies one received frame. A reliable fragment goes through its
    /// lane's reassembly and a completed message joins the lane's inbound
    /// queue within its bounds, or the peer is closed with
    /// `InboundOverflow`. An unreliable message joins the lane's unreliable
    /// queue, dropping its oldest unpolled messages if the caller has not
    /// polled in time.
    #[cfg(any(test, target_arch = "wasm32"))]
    pub(super) fn receive(&mut self, envelope: Envelope<'_>) -> Result<(), DisconnectReason> {
        self.apply(envelope, false).map(drop)
    }

    /// Like [`Self::receive`], except that a reliable frame whose message
    /// the lane's inbound queue cannot take yet is [`Received::Held`]
    /// instead of closing the peer: read backpressure for a transport that
    /// can stop reading (netcode.md 13).
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn receive_or_hold(
        &mut self,
        envelope: Envelope<'_>,
    ) -> Result<Received, DisconnectReason> {
        self.apply(envelope, true)
    }

    /// Whether a frame is held because its lane was full.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) const fn read_stalled(&self) -> bool {
        self.stalled.is_some()
    }

    /// Whether the held frame's message now fits its lane.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn can_resume(&self) -> bool {
        self.stalled.is_some_and(|(lane, len)| {
            self.inbound[lane]
                .usage
                .admits(len, &self.reliable.lanes[lane])
        })
    }

    fn apply(&mut self, envelope: Envelope<'_>, hold: bool) -> Result<Received, DisconnectReason> {
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
                return Ok(Received::Accepted);
            }
            // An unreliable message the caller has not polled for in time
            // makes room by dropping the lane's oldest unpolled ones, as a
            // full UDP socket buffer would; only one larger than the whole
            // queue (which the codec's cap rules out) breaks the framing.
            Delivery::Unreliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                if envelope.payload.len() > bounds.unreliable_bytes {
                    DisconnectReason::ProtocolViolation
                } else {
                    self.inbound[lane.index()].unreliable.push_dropping_oldest(
                        envelope.payload.to_vec(),
                        bounds.unreliable_messages,
                        bounds.unreliable_bytes,
                    );
                    return Ok(Received::Accepted);
                }
            }
            Delivery::Reliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let inbound = &mut self.inbound[lane.index()];
                let completes = inbound
                    .reassembly
                    .completes(envelope.fragment, envelope.payload.len());
                match completes {
                    Some(len) if !inbound.usage.admits(len, bounds) && hold => {
                        self.stalled = Some((lane.index(), len));
                        return Ok(Received::Held);
                    }
                    Some(len) if !inbound.usage.admits(len, bounds) => {
                        DisconnectReason::InboundOverflow
                    }
                    _ => match inbound.reassembly.push(
                        envelope.fragment,
                        envelope.payload,
                        self.reliable.max_message_bytes,
                    ) {
                        Ok(message) => {
                            if let Some(message) = message {
                                inbound.usage.add(message.len(), bounds);
                                inbound.reliable.push_back(message);
                            }
                            self.stalled = None;
                            return Ok(Received::Accepted);
                        }
                        Err(_) => DisconnectReason::ProtocolViolation,
                    },
                }
            }
        };
        self.close(failure);
        Err(failure)
    }

    /// The next inbound message: lanes interleaved by weight, each lane's
    /// reliable and unreliable messages taking turns, then the newest latest
    /// state.
    pub(super) fn pop_inbound(&mut self) -> Option<(Delivery, Vec<u8>)> {
        let inbound = &self.inbound;
        if let Some(index) = self
            .inbound_scheduler
            .next(|lane| !inbound[lane].is_empty())
        {
            let (unreliable, message) = self.inbound[index]
                .pop(&self.reliable.lanes[index])
                .expect("the scheduler picked a backlog");
            let delivery = if unreliable {
                Delivery::Unreliable(lane(index))
            } else {
                Delivery::Reliable(lane(index))
            };
            return Some((delivery, message));
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
        if let Some(frame) = self.staged_latest.take() {
            let released = self.released_generation;
            // A state with nothing flushed after it would leave together
            // with this newer one.
            while self.released_latest.back().is_some_and(|&(generation, _)| {
                !self
                    .outbound
                    .iter()
                    .any(|lane| lane.queues_between(generation, released))
            }) {
                self.released_latest.pop_back();
            }
            self.released_latest.push_back((released, frame));
        }
        let Some(next) = self.staging_generation.checked_add(1) else {
            self.close(DisconnectReason::ProtocolViolation);
            return Err(DisconnectReason::ProtocolViolation);
        };
        self.staging_generation = next;
        Ok(())
    }

    /// The index in `released_latest` of the state that leaves next: the
    /// newest released before every lane frame still queued.
    fn due_latest(&self) -> Option<usize> {
        let oldest = self
            .outbound
            .iter()
            .filter_map(OutboundLane::oldest_generation)
            .min();
        self.released_latest
            .iter()
            .rposition(|&(generation, _)| oldest.is_none_or(|oldest| generation < oldest))
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
        if let Some(index) = self.due_latest() {
            let frame = &self.released_latest[index].1;
            return Some((frame.delivery, ENVELOPE_HEADER_LEN + frame.payload.len()));
        }
        if let Some(index) = self.released_lane() {
            let (unreliable, fragment, len) = self.outbound[index]
                .peek(self.released_generation)
                .expect("a released lane has a frame");
            let total = if fragment.total().is_some() {
                ENVELOPE_TOTAL_LEN
            } else {
                0
            };
            let delivery = if unreliable {
                Delivery::Unreliable(lane(index))
            } else {
                Delivery::Reliable(lane(index))
            };
            return Some((delivery, ENVELOPE_HEADER_LEN + total + len));
        }
        None
    }

    /// The next released frame: the latest state whose turn has come, once
    /// every lane frame flushed with or before it has gone, else a reliable
    /// fragment or an unreliable message from the lane deficit round robin
    /// picks.
    pub(super) fn pop_released_frame(&mut self) -> Option<OutboundFrame> {
        if let Some(index) = self.due_latest() {
            // Older states whose turn came with it are stale.
            self.released_latest.drain(..index);
            return self.released_latest.pop_front().map(|(_, frame)| frame);
        }
        let (outbound, released) = (&self.outbound, self.released_generation);
        if let Some(index) = self
            .outbound_scheduler
            .next(|lane| outbound[lane].released(released))
        {
            let (unreliable, fragment, payload) = self.outbound[index]
                .take(released)
                .expect("a released lane has a frame");
            return Some(OutboundFrame {
                delivery: if unreliable {
                    Delivery::Unreliable(lane(index))
                } else {
                    Delivery::Reliable(lane(index))
                },
                sequence: 0,
                fragment,
                payload,
            });
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

    /// A graceful close is under way and every frame has left the queue.
    pub(super) fn graceful_close_drained(&self) -> bool {
        self.graceful_closing
            && self.outbound.iter().all(OutboundLane::is_empty)
            && self.staged_latest.is_none()
            && self.released_latest.is_empty()
    }

    pub(super) fn finish_graceful_close(&mut self) {
        if self.graceful_close_drained() {
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
            self.stalled = None;
            self.outbound = Default::default();
            self.staged_latest = None;
            self.released_latest.clear();
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

    /// #254: a staged frame waits for its release; released latest state
    /// goes out once, at the last value of its flush; the graceful close
    /// expires exactly at its timeout; a lane admits exactly its message cap
    /// and only that lane refuses past it.
    #[wasm_bindgen_test(unsupported = test)]
    fn release_generations_expiry_and_lane_caps_are_exact() {
        let mut state = new_peer();
        state.send(Delivery::LatestState, b"s1").unwrap();
        assert!(!state.has_released_outbound(), "staged, not released");
        state.release_outbound().unwrap();
        state.send(Delivery::LatestState, b"s2").unwrap();
        state.send(Delivery::LatestState, b"s3").unwrap();
        state.release_outbound().unwrap();
        assert_eq!(
            state.next_released_frame_len(),
            Some((Delivery::LatestState, ENVELOPE_HEADER_LEN + 2))
        );
        // Nothing was flushed between the two states, so only the newer
        // leaves.
        let sent: Vec<_> = frames(&mut state)
            .into_iter()
            .map(|frame| frame.payload)
            .collect();
        assert_eq!(sent, [b"s3".to_vec()]);
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

    /// Defect: latest state starved behind a bulk lane its producer keeps
    /// full — written only once no released lane frame remains, so never
    /// while bulk flows. Oracle: netcode.md 13 — a flushed latest state
    /// waits only for the lane frames flushed before it. Each tick the game
    /// fills a bulk lane until it refuses, sends its state and flushes, and
    /// the socket takes two frames; every tick's state that leaves does so
    /// behind at most the lane frames its flush found waiting, and the last
    /// one leaves within that many writes of the producer stopping.
    #[wasm_bindgen_test(unsupported = test)]
    fn latest_state_keeps_flowing_beside_a_bulk_lane_kept_full() {
        /// What the socket wrote, against what each flush found waiting.
        #[derive(Default)]
        struct Socket {
            /// Lane frames accepted and not yet written.
            pending: usize,
            /// Per tick: the lane frames waiting when it flushed, and those
            /// written since.
            found: Vec<usize>,
            since: Vec<usize>,
            /// The ticks whose state was written, in order.
            written: Vec<u8>,
        }

        impl Socket {
            fn write(&mut self, peer: &mut PeerState) -> bool {
                let Some(frame) = peer.pop_released_frame() else {
                    return false;
                };
                if frame.delivery == Delivery::LatestState {
                    let tick = usize::from(frame.payload[0]);
                    assert!(
                        self.since[tick] <= self.found[tick],
                        "tick {tick}'s state went behind {} lane frames; its flush found {}",
                        self.since[tick],
                        self.found[tick]
                    );
                    self.written.push(frame.payload[0]);
                } else {
                    self.pending -= 1;
                    for count in &mut self.since {
                        *count += 1;
                    }
                }
                true
            }
        }

        let mut peer = new_peer();
        let bulk = Delivery::Reliable(lane(1));
        // Each bulk message is one frame.
        let message = [7; WEBSOCKET_FRAGMENT_BYTES];
        let mut socket = Socket::default();
        for tick in 0..60u8 {
            while peer.send(bulk, &message).is_ok() {
                socket.pending += 1;
            }
            peer.send(Delivery::LatestState, &[tick]).unwrap();
            peer.release_outbound().unwrap();
            socket.found.push(socket.pending);
            socket.since.push(0);
            for _ in 0..2 {
                assert!(socket.write(&mut peer), "bulk is always released");
            }
        }
        let during = socket.written.len();
        while socket.write(&mut peer) {}
        assert!(
            during > 10,
            "latest state starved while bulk flowed: {:?}",
            socket.written
        );
        assert_eq!(
            socket.written.last(),
            Some(&59),
            "the newest state left last"
        );
    }

    /// Defect: a latest state sent after a flush withholding the one that
    /// flush released, so a game that sends state every tick right after
    /// flushing never gets one written. Oracle: netcode.md 13 — the flushed
    /// value leaves, and the newer one waits for its own flush.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_newer_latest_state_waits_without_withholding_the_flushed_one() {
        let mut peer = new_peer();
        peer.send(Delivery::LatestState, b"flushed").unwrap();
        peer.release_outbound().unwrap();
        peer.send(Delivery::LatestState, b"newer").unwrap();
        let payloads = |peer: &mut PeerState| -> Vec<Vec<u8>> {
            frames(peer)
                .into_iter()
                .map(|frame| frame.payload)
                .collect()
        };
        assert_eq!(payloads(&mut peer), [b"flushed".to_vec()]);
        peer.release_outbound().unwrap();
        assert_eq!(payloads(&mut peer), [b"newer".to_vec()]);
    }

    /// #267: a full reliable lane refuses the send without closing the
    /// peer; a drained message returns the allowance and the retry succeeds.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_reliable_lane_refuses_without_closing() {
        const CAP: usize = crate::DEFAULT_RELIABLE_MESSAGE_BYTES;
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
        let full = crate::DEFAULT_LANE_OUTBOUND_BYTES / CAP;
        for _ in 1..full {
            peer.send(Delivery::RELIABLE_ORDERED, &vec![0; CAP])
                .unwrap();
        }
        peer.send(Delivery::RELIABLE_ORDERED, &vec![0; CAP - 10])
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

    /// Defect (design §12): an unreliable message held behind bulk data,
    /// dropped under backpressure, reordered or fragmented on WebSocket.
    /// Oracle: the scheduling bounds of netcode.md 14 — with bulk on lane 1
    /// at 8:1, deficit round robin lets at most one bulk fragment go before
    /// a released lane-0 unreliable message; with bulk on lane 0 itself, new
    /// reliable fragments and unreliable messages take turns, so again at
    /// most one goes first. The bulk lane is kept full and every accepted
    /// unreliable message leaves whole, once, in send order.
    #[wasm_bindgen_test(unsupported = test)]
    fn unreliable_frames_keep_their_lane_bound_behind_bulk() {
        let mut config = ReliableConfig::DEFAULT;
        config.lanes[0].weight = 8;
        config.lanes[1].weight = 1;
        for bulk_lane in [1, 0] {
            let mut peer = PeerState::new(&config);
            let bulk = Delivery::Reliable(lane(bulk_lane));
            let (mut bulk_sent, mut left) = (0u32, Vec::new());
            for index in 0..200u32 {
                while peer.send(bulk, &vec![bulk_sent as u8; 60 * 1024]).is_ok() {
                    bulk_sent += 1;
                }
                peer.send(Delivery::Unreliable(lane(0)), &index.to_le_bytes())
                    .expect("one message per round fits");
                peer.release_outbound().unwrap();
                // The socket takes frames until the unreliable one has gone.
                let mut bulk_ahead = 0;
                loop {
                    let frame = peer.pop_released_frame().expect("a released frame");
                    if frame.delivery == bulk {
                        bulk_ahead += 1;
                        assert!(
                            bulk_ahead <= 1,
                            "bulk on lane {bulk_lane}: message {index} waited for more"
                        );
                        continue;
                    }
                    assert_eq!(frame.delivery, Delivery::Unreliable(lane(0)));
                    assert_eq!(frame.fragment, Fragment::Whole);
                    left.push(u32::from_le_bytes(frame.payload.try_into().unwrap()));
                    break;
                }
                // And one more, so the bulk keeps moving.
                peer.pop_released_frame().expect("bulk is backlogged");
            }
            assert_eq!(
                left,
                (0..200).collect::<Vec<_>>(),
                "bulk on lane {bulk_lane}"
            );
        }
    }

    /// Defect (design §12 review): a receiver that is not polled closing
    /// the peer for unreliable traffic, dropping the newest instead of the
    /// oldest, or losing track of the queue's bytes as it sheds. Oracle: the
    /// UDP-socket rule — past either bound the oldest unpolled unreliable
    /// messages go and the newest that fit arrive in order, the peer stays
    /// open — while reliable overflow still closes it.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_unpolled_receiver_sheds_its_oldest_unreliable_messages() {
        fn unreliable(payload: &[u8]) -> Envelope<'_> {
            Envelope {
                delivery: Delivery::Unreliable(lane(2)),
                sequence: 0,
                fragment: Fragment::Whole,
                payload,
            }
        }
        let drain =
            |peer: &mut PeerState| std::iter::from_fn(|| peer.pop_inbound()).collect::<Vec<_>>();
        let mut config = ReliableConfig::DEFAULT;
        config.lanes[2].unreliable_messages = 3;
        config.lanes[2].inbound_messages = 1;
        let mut peer = PeerState::new(&config);
        for index in 0..7u8 {
            peer.receive(unreliable(&[index])).unwrap();
        }
        assert_eq!(peer.terminal(), None);
        assert_eq!(
            drain(&mut peer),
            [4, 5, 6].map(|index| (Delivery::Unreliable(lane(2)), vec![index]))
        );

        // The byte bound sheds too, and the queue's count recovers.
        config.lanes[2].unreliable_bytes = MAX_UNRELIABLE_BYTES;
        let mut peer = PeerState::new(&config);
        for index in 0..5u8 {
            peer.receive(unreliable(&[index; 500])).unwrap();
        }
        assert_eq!(
            drain(&mut peer),
            [3, 4].map(|index| (Delivery::Unreliable(lane(2)), vec![index; 500]))
        );
        peer.receive(unreliable(&[9; MAX_UNRELIABLE_BYTES]))
            .unwrap();
        assert_eq!(
            drain(&mut peer),
            [(Delivery::Unreliable(lane(2)), vec![9; MAX_UNRELIABLE_BYTES])]
        );

        // Reliable overflow still closes the peer.
        let reliable = |payload: &'static [u8]| Envelope {
            delivery: Delivery::Reliable(lane(2)),
            sequence: 0,
            fragment: Fragment::Whole,
            payload,
        };
        peer.receive(reliable(b"one")).unwrap();
        assert_eq!(
            peer.receive(reliable(b"two")),
            Err(DisconnectReason::InboundOverflow)
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
        /// Sends on a lane, unreliably when the flag is set.
        Send(usize, bool, Vec<u8>),
        SendLatest(Vec<u8>),
        Release,
        PopReleased,
        Receive(usize, bool, Vec<u8>),
        ReceiveLatest(u64),
        PopInbound,
    }

    /// The model's reliable message cap, and its lanes' outbound and
    /// inbound byte bounds: smaller than the cap, so the one-message rules
    /// of netcode.md 11 bind.
    const CAP: usize = 40 * 1024;
    const LANE_BYTES: usize = 8 * 1024;

    fn config() -> ReliableConfig {
        let mut config = ReliableConfig::DEFAULT;
        config.max_message_bytes = CAP;
        for lane in &mut config.lanes {
            lane.outbound_bytes = LANE_BYTES;
            lane.inbound_bytes = LANE_BYTES;
        }
        config
    }

    fn op() -> impl Strategy<Value = Op> {
        let lane = 0..RELIABLE_LANES;
        prop_oneof![
            4 => (lane.clone(), bytes(2_500)).prop_map(|(l, p)| Op::Send(l, false, p)),
            1 => (lane.clone(), 0..=CAP + 1)
                .prop_map(|(l, len)| Op::Send(l, false, vec![1; len])),
            3 => (lane.clone(), bytes(MAX_UNRELIABLE_BYTES + 1))
                .prop_map(|(l, p)| Op::Send(l, true, p)),
            2 => bytes(64).prop_map(Op::SendLatest),
            2 => Just(Op::Release),
            4 => Just(Op::PopReleased),
            2 => (lane.clone(), any::<bool>(), bytes(64))
                .prop_map(|(l, unreliable, p)| Op::Receive(l, unreliable, p)),
            1 => (lane.clone(), 0usize..200).prop_map(|(l, n)| Op::Receive(l, true, vec![2; n * 400])),
            1 => (lane, 0usize..40).prop_map(|(l, n)| Op::Receive(l, false, vec![3; n * 400])),
            1 => (0u64..4).prop_map(Op::ReceiveLatest),
            2 => Just(Op::PopInbound),
        ]
    }

    /// One outbound queue of the model: messages with the release
    /// generation they wait for, and how much of the front one has left as
    /// fragments.
    #[derive(Default)]
    struct ModelQueue {
        messages: VecDeque<(Vec<u8>, u64)>,
        taken: usize,
    }

    impl ModelQueue {
        fn bytes(&self) -> usize {
            self.messages.iter().map(|(payload, _)| payload.len()).sum()
        }
    }

    /// The model's queue index for a lane: reliable queues first.
    fn slot(index: usize, unreliable: bool) -> usize {
        if unreliable {
            RELIABLE_LANES + index
        } else {
            index
        }
    }

    /// The lane's message and byte bounds for one class and direction.
    fn bounds(
        config: &ReliableConfig,
        index: usize,
        unreliable: bool,
        inbound: bool,
    ) -> (usize, usize) {
        let lane = &config.lanes[index];
        match (unreliable, inbound) {
            (true, _) => (lane.unreliable_messages, lane.unreliable_bytes),
            (false, false) => (lane.outbound_messages, lane.outbound_bytes),
            (false, true) => (lane.inbound_messages, lane.inbound_bytes),
        }
    }

    type Outbound = [ModelQueue; 2 * RELIABLE_LANES];
    type Inbound = [VecDeque<Vec<u8>>; 2 * RELIABLE_LANES];

    /// Defect: a released frame leaking a staged one, a lane's fragments out
    /// of order or not covering its message, a frame past the fragment cap,
    /// an unreliable message dropped, duplicated, reordered or fragmented,
    /// latest state leaving before a lane frame flushed with or before it,
    /// waiting behind one flushed after it, or leaving stale, a cap enforced after
    /// the queue grew or on the wrong lane or class, a full queue that
    /// closes the peer, a capacity report that disagrees with reliable
    /// admission, reliable inbound queues reordered or past their bounds
    /// without closing the peer, unreliable inbound queues shedding anything
    /// but their oldest messages or closing the peer, or a stale latest
    /// sequence accepted. Oracle: a model with a reliable and an unreliable
    /// FIFO per lane plus one slot each way, the netcode.md 10, 11 and 13
    /// rules, and the module's release generations.
    #[test]
    fn peer_state_matches_the_lane_fifo_plus_slot_model() {
        check(prop::collection::vec(op(), 1..250), |ops| {
            let config = config();
            let mut state = PeerState::new(&config);
            let mut outbound: Outbound = Default::default();
            // Every latest state sent, with its staging generation, and the
            // sequence of the newest one written.
            let mut latest: Vec<(u64, Vec<u8>, u64)> = Vec::new();
            let (mut next_latest, mut last_latest) = (1u64, 0u64);
            let (mut staging, mut released) = (1u64, 0u64);
            let mut terminal: Option<DisconnectReason> = None;
            let mut inbound: Inbound = Default::default();
            let mut inbound_latest: Option<Vec<u8>> = None;
            let mut last_inbound_latest = 0u64;
            let close = |reason,
                         terminal: &mut Option<DisconnectReason>,
                         outbound: &mut Outbound,
                         latest: &mut Vec<(u64, Vec<u8>, u64)>,
                         inbound: &mut Inbound,
                         inbound_latest: &mut Option<Vec<u8>>| {
                *terminal = Some(reason);
                *outbound = Default::default();
                latest.clear();
                *inbound = Default::default();
                *inbound_latest = None;
            };

            for op in ops {
                match op {
                    Op::Send(index, unreliable, payload) => {
                        let delivery = if unreliable {
                            Delivery::Unreliable(lane(index))
                        } else {
                            Delivery::Reliable(lane(index))
                        };
                        let capacity = state.capacity(lane(index));
                        let result = state.send(delivery, &payload);
                        if !unreliable {
                            prop_assert_eq!(
                                result.is_ok(),
                                capacity.messages >= 1 && capacity.bytes >= payload.len()
                            );
                        }
                        let (messages, bytes) = bounds(&config, index, unreliable, false);
                        let model = &mut outbound[slot(index, unreliable)];
                        let cap = if unreliable {
                            MAX_UNRELIABLE_BYTES
                        } else {
                            config.max_message_bytes
                        };
                        // A reliable lane holding no bytes takes one message
                        // of any admitted size.
                        let one_message = !unreliable && model.bytes() == 0;
                        if terminal.is_some() {
                            prop_assert_eq!(result, Err(SendError::Disconnected));
                        } else if payload.len() > cap {
                            prop_assert_eq!(result, Err(SendError::PayloadTooLarge));
                        } else if model.messages.len() >= messages
                            || (!one_message && model.bytes() + payload.len() > bytes)
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
                            latest.push((next_latest, payload, staging));
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
                        let any_released = outbound.iter().any(|queue| {
                            queue.messages.front().is_some_and(|(_, g)| *g <= released)
                        });
                        // The newest latest state whose turn has come:
                        // released, the last sent before its flush, newer
                        // than any written, and with no lane frame of its
                        // flush or an earlier one still queued.
                        let due = latest
                            .iter()
                            .filter(|(sequence, _, generation)| {
                                *generation <= released
                                    && *sequence > last_latest
                                    && !latest
                                        .iter()
                                        .any(|(s, _, g)| g == generation && s > sequence)
                                    && !outbound.iter().any(|queue| {
                                        queue.messages.iter().any(|(_, g)| g <= generation)
                                    })
                            })
                            .max_by_key(|(sequence, _, _)| *sequence)
                            .cloned();
                        let lane_frame = match &got {
                            Some(frame) => match frame.delivery {
                                Delivery::Reliable(lane) => Some((lane.index(), false)),
                                Delivery::Unreliable(lane) => Some((lane.index(), true)),
                                Delivery::LatestState => None,
                            },
                            None => None,
                        };
                        match (got, lane_frame) {
                            (Some(frame), Some((index, unreliable))) => {
                                prop_assert!(
                                    due.is_none(),
                                    "a due latest state waited behind a later lane frame"
                                );
                                prop_assert_eq!(frame.sequence, 0);
                                prop_assert!(frame.payload.len() <= WEBSOCKET_FRAGMENT_BYTES);
                                let model = &mut outbound[slot(index, unreliable)];
                                let (message, generation) =
                                    model.messages.front().expect("a released message");
                                prop_assert!(*generation <= released, "staged frame leaked");
                                let start = model.taken;
                                let end = start + frame.payload.len();
                                prop_assert_eq!(&message[start..end], &frame.payload[..]);
                                let expected = match (start == 0, end == message.len()) {
                                    (true, true) => Fragment::Whole,
                                    (true, false) => Fragment::First {
                                        total: message.len() as u32,
                                    },
                                    (false, false) => Fragment::Middle,
                                    (false, true) => Fragment::Last,
                                };
                                prop_assert_eq!(frame.fragment, expected);
                                prop_assert!(
                                    !unreliable || frame.fragment == Fragment::Whole,
                                    "a fragmented unreliable message"
                                );
                                prop_assert!(
                                    !frame.payload.is_empty() || message.is_empty(),
                                    "an empty fragment of a non-empty message"
                                );
                                if end == message.len() {
                                    model.messages.pop_front();
                                    model.taken = 0;
                                } else {
                                    model.taken = end;
                                }
                            }
                            (Some(frame), None) => {
                                let Some((sequence, payload, _)) = due else {
                                    return Err(TestCaseError::fail(
                                        "a latest state left before its turn",
                                    ));
                                };
                                prop_assert_eq!(frame.sequence, sequence);
                                prop_assert_eq!(frame.payload, payload);
                                last_latest = sequence;
                            }
                            (None, _) => {
                                prop_assert!(!any_released);
                                prop_assert!(due.is_none(), "a due latest state was not sent");
                            }
                        }
                    }
                    Op::Receive(index, unreliable, payload) => {
                        let result = state.receive(Envelope {
                            delivery: if unreliable {
                                Delivery::Unreliable(lane(index))
                            } else {
                                Delivery::Reliable(lane(index))
                            },
                            sequence: 0,
                            fragment: Fragment::Whole,
                            payload: &payload,
                        });
                        let (messages, bytes) = bounds(&config, index, unreliable, true);
                        let queue = &mut inbound[slot(index, unreliable)];
                        let queued = |queue: &VecDeque<Vec<u8>>| -> usize {
                            queue.iter().map(Vec::len).sum()
                        };
                        let full = |queue: &VecDeque<Vec<u8>>| {
                            queue.len() >= messages || queued(queue) + payload.len() > bytes
                        };
                        // Reliable: one message larger than the byte bound
                        // may wait beside messages within it.
                        let reliable_full = |queue: &VecDeque<Vec<u8>>| {
                            let within = queue.iter().filter(|m| m.len() <= bytes);
                            queue.len() >= messages
                                || if payload.len() > bytes {
                                    queue.iter().any(|m| m.len() > bytes)
                                } else {
                                    within.map(Vec::len).sum::<usize>() + payload.len() > bytes
                                }
                        };
                        let failure = if terminal.is_some() {
                            prop_assert_eq!(result, Err(DisconnectReason::Peer));
                            None
                        } else if unreliable && payload.len() > bytes {
                            Some(DisconnectReason::ProtocolViolation)
                        } else if unreliable {
                            // An unpolled receiver sheds its oldest
                            // unreliable messages, as a full UDP socket
                            // buffer would.
                            while full(queue) {
                                queue.pop_front();
                            }
                            None
                        } else if reliable_full(queue) {
                            Some(DisconnectReason::InboundOverflow)
                        } else {
                            None
                        };
                        if let Some(reason) = failure {
                            prop_assert_eq!(result, Err(reason));
                            close(
                                reason,
                                &mut terminal,
                                &mut outbound,
                                &mut latest,
                                &mut inbound,
                                &mut inbound_latest,
                            );
                        } else if terminal.is_none() {
                            prop_assert_eq!(result, Ok(()));
                            inbound[slot(index, unreliable)].push_back(payload);
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
                        // Any lane and class may come next; within one, in
                        // order.
                        Some((Delivery::Reliable(lane), payload)) => {
                            prop_assert_eq!(
                                inbound[slot(lane.index(), false)].pop_front(),
                                Some(payload)
                            );
                        }
                        Some((Delivery::Unreliable(lane), payload)) => {
                            prop_assert_eq!(
                                inbound[slot(lane.index(), true)].pop_front(),
                                Some(payload)
                            );
                        }
                        // Lane messages drain before the latest slot.
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
