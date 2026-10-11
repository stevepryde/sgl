//! Bounded in-process transport used by solo play and transport tests.
//!
//! Each direction has a reliable and an unreliable FIFO per lane plus one
//! replaceable latest-state slot. `send` only stages data; `flush(now_ms)` stamps it and
//! makes it visible to the peer. A poll therefore measures elapsed virtual
//! time from real item metadata instead of special-casing the in-memory
//! transport. Messages cross whole: there is no loss to isolate lanes from,
//! and a poll returns every lane's messages, interleaved by the lanes'
//! weights. A peer that is not polled drops its oldest flushed unreliable
//! messages past a lane's unreliable bounds, as the network transports do.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use crate::lanes::LaneScheduler;
use crate::{
    ClientEvent, ClientIo, ConnectionId, Delivery, DisconnectReason, Lane, MAX_LATEST_STATE_BYTES,
    MAX_UNRELIABLE_BYTES, RELIABLE_LANES, ReliableCapacity, ReliableConfig, ReliableConfigError,
    RttEstimate, RttEstimator, SendError, ServerEvent, ServerIo,
};

/// The process-local identity of an in-memory duplex's single connection.
pub const SOLO_CONNECTION: ConnectionId = ConnectionId::MIN;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingItem {
    delivery: Delivery,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SentItem {
    delivery: Delivery,
    sent_at_ms: u64,
    payload: Vec<u8>,
}

/// One lane's FIFO and its bytes.
#[derive(Debug)]
struct LaneQueue<T> {
    items: VecDeque<(usize, T)>,
    bytes: usize,
}

impl<T> Default for LaneQueue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
        }
    }
}

impl<T> LaneQueue<T> {
    fn push(&mut self, item: T, bytes: usize) {
        self.bytes += bytes;
        self.items.push_back((bytes, item));
    }

    fn pop(&mut self) -> Option<(usize, T)> {
        let (bytes, item) = self.items.pop_front()?;
        self.bytes -= bytes;
        Some((bytes, item))
    }

    /// Pushes `item`, first dropping the oldest items until it fits within
    /// `messages` and `max_bytes` (netcode.md 11: an unpolled receiver
    /// sheds its oldest unreliable messages). Validation keeps `max_bytes`
    /// at least `MAX_UNRELIABLE_BYTES`, so an admitted item fits an empty
    /// queue.
    fn push_dropping_oldest(&mut self, item: T, bytes: usize, messages: usize, max_bytes: usize) {
        while self.items.len() >= messages || self.bytes + bytes > max_bytes {
            if self.pop().is_none() {
                break;
            }
        }
        self.push(item, bytes);
    }
}

/// One direction's reliable and unreliable lanes and latest-state slot.
#[derive(Debug)]
struct Queues<T> {
    lanes: [LaneQueue<T>; RELIABLE_LANES],
    unreliable: [LaneQueue<T>; RELIABLE_LANES],
    latest: Option<T>,
}

impl<T> Default for Queues<T> {
    fn default() -> Self {
        Self {
            lanes: std::array::from_fn(|_| LaneQueue::default()),
            unreliable: std::array::from_fn(|_| LaneQueue::default()),
            latest: None,
        }
    }
}

impl<T> Queues<T> {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

type SharedQueues = Rc<RefCell<Queues<SentItem>>>;

#[derive(Debug)]
struct DuplexEnd {
    outbound: SharedQueues,
    inbound: SharedQueues,
    pending: Queues<PendingItem>,
    reliable: ReliableConfig,
    /// Interleaves the inbound lanes when a poll drains them.
    scheduler: LaneScheduler,
    /// Per lane: whether its next drained message is unreliable, when it
    /// has both kinds.
    unreliable_turn: [bool; RELIABLE_LANES],
    open: Rc<Cell<bool>>,
    local_ended: bool,
}

impl DuplexEnd {
    fn new(
        outbound: SharedQueues,
        inbound: SharedQueues,
        reliable: &ReliableConfig,
        open: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            outbound,
            inbound,
            pending: Queues::default(),
            reliable: reliable.clone(),
            scheduler: LaneScheduler::new(reliable),
            unreliable_turn: [false; RELIABLE_LANES],
            open,
            local_ended: false,
        }
    }

    fn connected(&self) -> bool {
        self.open.get()
            && Rc::strong_count(&self.outbound) > 1
            && Rc::strong_count(&self.inbound) > 1
    }

    /// Why the connection ended, once it has: `Local` when this end
    /// disconnected first, whatever the other end did afterwards.
    fn ended(&self) -> Option<DisconnectReason> {
        if self.local_ended {
            Some(DisconnectReason::Local)
        } else if self.connected() {
            None
        } else {
            Some(DisconnectReason::Peer)
        }
    }

    /// Reliable messages and bytes `lane` still holds in this direction:
    /// staged here plus flushed but not yet polled by the peer.
    fn lane_usage(&self, lane: Lane) -> (usize, usize) {
        let staged = &self.pending.lanes[lane.index()];
        let flushed = &self.outbound.borrow().lanes[lane.index()];
        (
            staged.items.len() + flushed.items.len(),
            staged.bytes + flushed.bytes,
        )
    }

    fn capacity(&self, lane: Lane) -> ReliableCapacity {
        if !self.connected() {
            return ReliableCapacity::default();
        }
        self.reliable.lanes[lane.index()]
            .outbound_capacity(self.lane_usage(lane), self.reliable.max_message_bytes)
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        if !self.connected() {
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
        match delivery {
            Delivery::Reliable(lane) => {
                if !self.reliable.lanes[lane.index()]
                    .outbound_admits(self.lane_usage(lane), payload.len())
                {
                    return Err(SendError::WouldBlock);
                }
                let item = PendingItem {
                    delivery,
                    payload: payload.to_vec(),
                };
                self.pending.lanes[lane.index()].push(item, payload.len());
            }
            // Only staged unreliable messages count: once flushed they
            // are sent, and the peer sheds its oldest past the bounds.
            Delivery::Unreliable(lane) => {
                let staged = &self.pending.unreliable[lane.index()];
                let bounds = &self.reliable.lanes[lane.index()];
                if staged.items.len() >= bounds.unreliable_messages
                    || staged.bytes + payload.len() > bounds.unreliable_bytes
                {
                    return Err(SendError::WouldBlock);
                }
                let item = PendingItem {
                    delivery,
                    payload: payload.to_vec(),
                };
                self.pending.unreliable[lane.index()].push(item, payload.len());
            }
            Delivery::LatestState => {
                self.pending.latest = Some(PendingItem {
                    delivery,
                    payload: payload.to_vec(),
                });
            }
        }
        Ok(())
    }

    fn flush(&mut self, now_ms: u64) {
        if !self.connected() {
            self.pending.clear();
            return;
        }
        let mut outbound = self.outbound.borrow_mut();
        let outbound = &mut *outbound;
        let sent = |item: PendingItem| SentItem {
            delivery: item.delivery,
            sent_at_ms: now_ms,
            payload: item.payload,
        };
        // `send` admission counted staged and flushed reliable items against
        // the lane's bounds, so moving them keeps it within them.
        for (staged, flushed) in self.pending.lanes.iter_mut().zip(&mut outbound.lanes) {
            while let Some((bytes, item)) = staged.pop() {
                flushed.push(sent(item), bytes);
            }
        }
        let unreliable = self
            .pending
            .unreliable
            .iter_mut()
            .zip(&mut outbound.unreliable);
        for ((staged, flushed), bounds) in unreliable.zip(&self.reliable.lanes) {
            while let Some((bytes, item)) = staged.pop() {
                flushed.push_dropping_oldest(
                    sent(item),
                    bytes,
                    bounds.unreliable_messages,
                    bounds.unreliable_bytes,
                );
            }
        }
        if let Some(item) = self.pending.latest.take() {
            outbound.latest = Some(sent(item));
        }
    }

    /// Every flushed message: lanes interleaved by weight, each lane's
    /// reliable and unreliable messages taking turns, then the newest latest
    /// state.
    fn drain(&mut self) -> Vec<SentItem> {
        let mut inbound = self.inbound.borrow_mut();
        let inbound = &mut *inbound;
        let mut items = Vec::new();
        while let Some(lane) = self.scheduler.next(|lane| {
            !inbound.lanes[lane].items.is_empty() || !inbound.unreliable[lane].items.is_empty()
        }) {
            let turn = &mut self.unreliable_turn[lane];
            let (reliable, unreliable) = (&mut inbound.lanes[lane], &mut inbound.unreliable[lane]);
            let (_, item) = if (*turn || reliable.items.is_empty())
                && let Some(item) = unreliable.pop()
            {
                *turn = false;
                item
            } else {
                *turn = !unreliable.items.is_empty();
                reliable.pop().expect("the scheduler picked a backlog")
            };
            items.push(item);
        }
        items.extend(inbound.latest.take());
        items
    }

    fn disconnect(&mut self, now_ms: u64) {
        self.flush(now_ms);
        // Only a connection still open ends here; one the peer already
        // ended keeps its own end.
        self.local_ended |= self.connected();
        self.open.set(false);
    }
}

/// Creates the client and server halves of one bounded in-memory connection
/// with default lanes ([`ReliableConfig::DEFAULT`]).
#[must_use]
pub fn memory_duplex() -> (MemoryClientIo, MemoryServerIo) {
    memory_duplex_with(&ReliableConfig::DEFAULT).expect("the default configuration is valid")
}

/// Creates both halves of one in-memory connection whose ends both use
/// `reliable`'s lane weights and bounds.
pub fn memory_duplex_with(
    reliable: &ReliableConfig,
) -> Result<(MemoryClientIo, MemoryServerIo), ReliableConfigError> {
    reliable.validate()?;
    let client_to_server = Rc::new(RefCell::new(Queues::default()));
    let server_to_client = Rc::new(RefCell::new(Queues::default()));
    let open = Rc::new(Cell::new(true));

    let client = DuplexEnd::new(
        Rc::clone(&client_to_server),
        Rc::clone(&server_to_client),
        reliable,
        Rc::clone(&open),
    );
    let server = DuplexEnd::new(server_to_client, client_to_server, reliable, open);

    Ok((
        MemoryClientIo {
            duplex: client,
            rtt: RttEstimator::new(),
            announced: false,
            disconnect_announced: false,
        },
        MemoryServerIo {
            duplex: server,
            announced: false,
            disconnect_announced: false,
        },
    ))
}

/// Client half of an in-memory duplex.
#[derive(Debug)]
pub struct MemoryClientIo {
    duplex: DuplexEnd,
    rtt: RttEstimator,
    announced: bool,
    disconnect_announced: bool,
}

impl ClientIo for MemoryClientIo {
    fn poll(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        // The duplex is connected from creation, so the first poll always
        // announces it — even when the peer has already gone, in which case
        // the loss follows in the same poll (netcode.md 7, 9).
        if !self.announced {
            self.announced = true;
            events.push(ClientEvent::Connected);
        }
        for item in self.duplex.drain() {
            let elapsed = now_ms.saturating_sub(item.sent_at_ms);
            self.rtt.sample(u32::try_from(elapsed).unwrap_or(u32::MAX));
            events.push(ClientEvent::Message {
                delivery: item.delivery,
                payload: item.payload,
            });
        }
        if !self.disconnect_announced
            && let Some(reason) = self.duplex.ended()
        {
            self.disconnect_announced = true;
            events.push(ClientEvent::Disconnected { reason });
        }
        events
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        self.duplex.send(delivery, payload)
    }

    fn capacity(&self, lane: Lane) -> ReliableCapacity {
        self.duplex.capacity(lane)
    }

    fn flush(&mut self, now_ms: u64) {
        self.duplex.flush(now_ms);
    }

    fn disconnect(&mut self, now_ms: u64) {
        self.duplex.disconnect(now_ms);
    }

    fn rtt(&self) -> RttEstimate {
        self.rtt.estimate()
    }
}

/// Server half of an in-memory duplex.
#[derive(Debug)]
pub struct MemoryServerIo {
    duplex: DuplexEnd,
    announced: bool,
    disconnect_announced: bool,
}

impl MemoryServerIo {
    /// Returns this duplex's single process-local connection identity.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        SOLO_CONNECTION
    }
}

impl ServerIo for MemoryServerIo {
    fn poll(&mut self, _now_ms: u64) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        // See `MemoryClientIo::poll`: connected from creation, announced once.
        if !self.announced {
            self.announced = true;
            events.push(ServerEvent::Connected {
                conn: SOLO_CONNECTION,
            });
        }
        if self.announced {
            for item in self.duplex.drain() {
                events.push(ServerEvent::Message {
                    conn: SOLO_CONNECTION,
                    delivery: item.delivery,
                    payload: item.payload,
                });
            }
        }
        if !self.disconnect_announced
            && let Some(reason) = self.duplex.ended()
        {
            self.disconnect_announced = true;
            events.push(ServerEvent::Disconnected {
                conn: SOLO_CONNECTION,
                reason,
            });
        }
        events
    }

    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError> {
        if conn != SOLO_CONNECTION || !self.announced {
            return Err(SendError::UnknownConnection);
        }
        self.duplex.send(delivery, payload)
    }

    fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
        if conn != SOLO_CONNECTION || !self.announced {
            return ReliableCapacity::default();
        }
        self.duplex.capacity(lane)
    }

    fn flush(&mut self, now_ms: u64) {
        self.duplex.flush(now_ms);
    }

    fn disconnect(&mut self, conn: ConnectionId, now_ms: u64) {
        if conn == SOLO_CONNECTION && self.announced {
            self.duplex.disconnect(now_ms);
        }
    }

    fn stop_admission(&mut self) {
        // The pair's sole peer is already admitted when the duplex is created.
        // There is no listener and therefore no future memory peer to refuse.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #254: the reliable byte allowance is returned when the peer drains,
    /// so two payloads that would not fit together fit one after the other.
    #[wasm_bindgen_test(unsupported = test)]
    fn byte_allowance_returns_after_a_drain() {
        let (mut client, mut server) = memory_duplex();
        // Four messages fit the byte allowance together; a fifth does not.
        let big = vec![1u8; 60 * 1024];
        for _ in 0..4 {
            client.send(Delivery::RELIABLE_ORDERED, &big).unwrap();
        }
        assert_eq!(
            client.send(Delivery::RELIABLE_ORDERED, &big),
            Err(SendError::WouldBlock)
        );
        client.flush(0);
        assert_eq!(server.poll(1).len(), 5, "connected plus four payloads");
        client
            .send(Delivery::RELIABLE_ORDERED, &big)
            .expect("allowance returned after the drain");
    }

    /// #237: a half that closes (or is dropped) before the other half's first
    /// poll must still surface the whole lifecycle — `Connected`, the data it
    /// flushed, then `Disconnected { Peer }` — instead of staying silent
    /// forever.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_peer_gone_before_the_first_poll_still_yields_the_lifecycle() {
        let (mut client, mut server) = memory_duplex();
        client.send(Delivery::RELIABLE_ORDERED, b"bye").unwrap();
        client.disconnect(0);
        assert_eq!(
            server.poll(1),
            vec![
                ServerEvent::Connected {
                    conn: SOLO_CONNECTION
                },
                ServerEvent::Message {
                    conn: SOLO_CONNECTION,
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload: b"bye".to_vec(),
                },
                ServerEvent::Disconnected {
                    conn: SOLO_CONNECTION,
                    reason: DisconnectReason::Peer,
                },
            ]
        );
        assert!(server.poll(2).is_empty(), "the lifecycle is announced once");

        let (mut client, server) = memory_duplex();
        drop(server);
        assert_eq!(
            client.poll(0),
            vec![
                ClientEvent::Connected,
                ClientEvent::Disconnected {
                    reason: DisconnectReason::Peer,
                },
            ]
        );
        assert_eq!(
            client.send(Delivery::RELIABLE_ORDERED, b"x"),
            Err(SendError::Disconnected)
        );
    }

    fn messages(events: &[ServerEvent]) -> Vec<(Delivery, Vec<u8>)> {
        events
            .iter()
            .filter_map(|event| match event {
                ServerEvent::Message {
                    delivery, payload, ..
                } => Some((*delivery, payload.clone())),
                _ => None,
            })
            .collect()
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn virtual_time_round_trip_measures_zero_by_arithmetic() {
        let (mut client, mut server) = memory_duplex();
        assert_eq!(client.poll(5_000), vec![ClientEvent::Connected]);
        assert_eq!(
            server.poll(5_000),
            vec![ServerEvent::Connected {
                conn: SOLO_CONNECTION,
            }]
        );

        client
            .send(Delivery::LatestState, b"input")
            .expect("queue input");
        client.flush(5_000);
        assert_eq!(
            messages(&server.poll(5_000)),
            vec![(Delivery::LatestState, b"input".to_vec())]
        );

        server
            .send(SOLO_CONNECTION, Delivery::LatestState, b"snapshot")
            .expect("queue snapshot");
        server.flush(5_000);
        assert!(client.poll(5_000).iter().any(|event| matches!(
            event,
            ClientEvent::Message {
                delivery: Delivery::LatestState,
                payload
            } if payload == b"snapshot"
        )));
        assert_eq!(client.rtt(), RttEstimate::default());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn a_late_poll_changes_the_measured_rtt() {
        let (mut client, mut server) = memory_duplex();
        let _ = client.poll(0);
        let _ = server.poll(0);
        server
            .send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, b"event")
            .expect("queue event");
        server.flush(1_000);
        let _ = client.poll(1_040);
        assert_eq!(client.rtt().srtt_ms, 40);
        assert_eq!(client.rtt().min_ms, 40);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn one_hundred_latest_states_coalesce_to_the_newest() {
        let (mut client, mut server) = memory_duplex();
        let _ = server.poll(0);
        for value in 0_u8..100 {
            client
                .send(Delivery::LatestState, &[value])
                .expect("latest slot is replaceable");
        }
        client.flush(10);
        assert_eq!(
            messages(&server.poll(10)),
            vec![(Delivery::LatestState, vec![99])]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn latest_state_replaces_an_already_flushed_but_unread_slot() {
        let (mut client, mut server) = memory_duplex();
        let _ = server.poll(0);
        client.send(Delivery::LatestState, b"old").expect("old");
        client.flush(1);
        client.send(Delivery::LatestState, b"new").expect("new");
        client.flush(2);
        assert_eq!(
            messages(&server.poll(2)),
            vec![(Delivery::LatestState, b"new".to_vec())]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reliable_fifo_drains_before_latest_and_refuses_overflow() {
        let (mut client, mut server) = memory_duplex();
        let _ = server.poll(0);
        client.send(Delivery::LatestState, b"old").expect("latest");
        client
            .send(Delivery::RELIABLE_ORDERED, b"first")
            .expect("first");
        client
            .send(Delivery::LatestState, b"new")
            .expect("replace latest");
        client
            .send(Delivery::RELIABLE_ORDERED, b"second")
            .expect("second");
        client.flush(1);
        assert_eq!(
            messages(&server.poll(1)),
            vec![
                (Delivery::RELIABLE_ORDERED, b"first".to_vec()),
                (Delivery::RELIABLE_ORDERED, b"second".to_vec()),
                (Delivery::LatestState, b"new".to_vec()),
            ]
        );

        for _ in 0..crate::DEFAULT_LANE_OUTBOUND_MESSAGES {
            client
                .send(Delivery::RELIABLE_ORDERED, b"x")
                .expect("within bound");
        }
        assert_eq!(
            client.send(Delivery::RELIABLE_ORDERED, b"overflow"),
            Err(SendError::WouldBlock)
        );
        client
            .send(Delivery::LatestState, b"still replaceable")
            .expect("latest has an independent slot");
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn shared_payload_and_reliable_byte_caps_fail_before_allocation_growth() {
        const CAP: usize = crate::DEFAULT_RELIABLE_MESSAGE_BYTES;
        let (mut client, _server) = memory_duplex();
        assert_eq!(
            client.send(Delivery::LatestState, &vec![0; MAX_LATEST_STATE_BYTES + 1]),
            Err(SendError::PayloadTooLarge)
        );
        assert_eq!(
            client.send(Delivery::RELIABLE_ORDERED, &vec![0; CAP + 1]),
            Err(SendError::PayloadTooLarge)
        );

        // Fill the byte allowance to ten bytes short: capacity reports
        // exactly what is left, and admission agrees at the boundary.
        let full = crate::DEFAULT_LANE_OUTBOUND_BYTES / CAP;
        for _ in 1..full {
            client
                .send(Delivery::RELIABLE_ORDERED, &vec![0; CAP])
                .expect("within byte allowance");
        }
        client
            .send(Delivery::RELIABLE_ORDERED, &vec![0; CAP - 10])
            .expect("within byte allowance");
        assert_eq!(
            client.capacity(Lane::DEFAULT),
            ReliableCapacity {
                messages: crate::DEFAULT_LANE_OUTBOUND_MESSAGES - full,
                bytes: 10,
            }
        );
        assert_eq!(
            client.send(Delivery::RELIABLE_ORDERED, &[0; 11]),
            Err(SendError::WouldBlock)
        );
        client
            .send(Delivery::RELIABLE_ORDERED, &[0; 10])
            .expect("exactly the remaining bytes");
    }

    /// Defect (#309): a memory connection the caller closed while open
    /// reporting nothing, or reporting `Local` to an end that disconnected
    /// after its peer had already ended the connection. Oracle: the
    /// `disconnect` rule of netcode.md 2: a connection still open when the
    /// caller disconnects reports exactly one `Local`; one already ended
    /// reports that end. The client disconnects first, then the server in
    /// the same tick: the client reports `Local`, the server `Peer`, once.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_end_that_disconnects_first_reports_local_and_the_other_peer() {
        let (mut client, mut server) = memory_duplex();
        let _ = client.poll(0);
        let _ = server.poll(0);
        client.disconnect(5);
        server.disconnect(SOLO_CONNECTION, 5);
        assert_eq!(
            client.poll(6),
            [ClientEvent::Disconnected {
                reason: DisconnectReason::Local,
            }]
        );
        assert_eq!(
            server.poll(6),
            [ServerEvent::Disconnected {
                conn: SOLO_CONNECTION,
                reason: DisconnectReason::Peer,
            }]
        );
        assert!(client.poll(7).is_empty());
        assert!(server.poll(7).is_empty());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn disconnect_and_admission_are_observable_once() {
        let (mut client, mut server) = memory_duplex();
        let _ = client.poll(0);
        let _ = server.poll(0);
        client.disconnect(5);
        assert_eq!(
            server.poll(5),
            vec![ServerEvent::Disconnected {
                conn: SOLO_CONNECTION,
                reason: DisconnectReason::Peer,
            }]
        );
        assert!(server.poll(6).is_empty());

        let (mut client, mut server) = memory_duplex();
        server.stop_admission();
        assert_eq!(
            server.poll(0),
            vec![ServerEvent::Connected {
                conn: SOLO_CONNECTION,
            }],
            "the already-created memory peer remains admitted"
        );
        server
            .send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, b"existing")
            .expect("stop_admission preserves existing peers");
        drop(server);
        assert_eq!(
            client.send(Delivery::LatestState, b"peer gone"),
            Err(SendError::Disconnected)
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use crate::{ClientEvent, ClientIo, LaneConfig, ServerIo};
    use proptest::prelude::*;
    use std::collections::VecDeque;

    #[derive(Debug, Clone)]
    enum Op {
        /// Sends the payload this many times in a row.
        ClientSend(Delivery, Vec<u8>, usize),
        ServerSend(Delivery, Vec<u8>, usize),
        ClientFlush,
        ServerFlush,
        ClientPoll,
        ServerPoll,
    }

    fn delivery() -> impl Strategy<Value = Delivery> {
        let lane =
            || (0..RELIABLE_LANES).prop_map(|lane| Lane::new(u8::try_from(lane).unwrap()).unwrap());
        prop_oneof![
            lane().prop_map(Delivery::Reliable),
            lane().prop_map(Delivery::Unreliable),
            Just(Delivery::LatestState)
        ]
    }

    /// The largest reliable message cap the configurations use.
    const MAX_MESSAGE: usize = 64 * 1024;

    /// Mostly small payloads, with large ones (up to one byte past the
    /// largest reliable cap) so the byte allowance binds as well as the
    /// count.
    fn payload() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            6 => bytes(32),
            1 => (0..=MAX_MESSAGE + 1).prop_map(|len| vec![0xA5; len]),
        ]
    }

    /// Usually one send, sometimes a burst long enough to fill a lane.
    fn repeat() -> impl Strategy<Value = usize> {
        prop_oneof![9 => Just(1), 1 => 1..=40usize]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (delivery(), payload(), repeat()).prop_map(|(d, p, n)| Op::ClientSend(d, p, n)),
            4 => (delivery(), payload(), repeat()).prop_map(|(d, p, n)| Op::ServerSend(d, p, n)),
            2 => Just(Op::ClientFlush),
            2 => Just(Op::ServerFlush),
            2 => Just(Op::ClientPoll),
            2 => Just(Op::ServerPoll),
        ]
    }

    /// Small per-lane bounds so lanes fill independently, byte bounds
    /// from a byte to past the message cap so the one-message rule binds.
    fn config() -> impl Strategy<Value = ReliableConfig> {
        let lane = (
            1u16..=8,
            1usize..=24,
            1usize..=3 * MAX_MESSAGE,
            1usize..=24,
            0usize..3,
        )
            .prop_map(
                |(weight, messages, outbound_bytes, unreliable, extra)| LaneConfig {
                    weight,
                    outbound_messages: messages,
                    outbound_bytes,
                    unreliable_messages: unreliable,
                    unreliable_bytes: MAX_UNRELIABLE_BYTES * (1 + extra),
                    ..LaneConfig::DEFAULT
                },
            );
        (prop::array::uniform4(lane), 1..=MAX_MESSAGE).prop_map(|(lanes, max_message_bytes)| {
            ReliableConfig {
                max_message_bytes,
                lanes,
            }
        })
    }

    fn fits(capacity: ReliableCapacity, len: usize) -> bool {
        capacity.messages >= 1 && capacity.bytes >= len
    }

    /// One direction of the model: staged until flushed, then visible to
    /// the peer's next poll; each lane's reliable and unreliable messages
    /// whole, once and in order within their own bounds, latest newest-wins.
    /// Reliable bounds cover staged and visible messages; unreliable bounds
    /// refuse only staged ones, and the visible queue keeps its newest
    /// messages that fit them (netcode.md 10, 11).
    #[derive(Default)]
    struct Direction {
        staged: [VecDeque<Vec<u8>>; 2 * RELIABLE_LANES],
        staged_latest: Option<Vec<u8>>,
        visible: [VecDeque<Vec<u8>>; 2 * RELIABLE_LANES],
        visible_latest: Option<Vec<u8>>,
    }

    /// A delivery's queue in the model: reliable lanes, then unreliable.
    fn slot(delivery: Delivery) -> Option<usize> {
        match delivery {
            Delivery::Reliable(lane) => Some(lane.index()),
            Delivery::Unreliable(lane) => Some(RELIABLE_LANES + lane.index()),
            Delivery::LatestState => None,
        }
    }

    impl Direction {
        fn send(
            &mut self,
            config: &ReliableConfig,
            delivery: Delivery,
            payload: Vec<u8>,
        ) -> Result<(), SendError> {
            let (cap, messages, bytes) = match delivery {
                Delivery::Reliable(lane) => {
                    let bounds = &config.lanes[lane.index()];
                    let (messages, bytes) = (bounds.outbound_messages, bounds.outbound_bytes);
                    (config.max_message_bytes, messages, bytes)
                }
                Delivery::Unreliable(lane) => {
                    let bounds = &config.lanes[lane.index()];
                    let (messages, bytes) = (bounds.unreliable_messages, bounds.unreliable_bytes);
                    (MAX_UNRELIABLE_BYTES, messages, bytes)
                }
                Delivery::LatestState => (MAX_LATEST_STATE_BYTES, 0, 0),
            };
            if payload.len() > cap {
                return Err(SendError::PayloadTooLarge);
            }
            let Some(slot) = slot(delivery) else {
                self.staged_latest = Some(payload);
                return Ok(());
            };
            let visible = match delivery {
                Delivery::Reliable(_) => &self.visible[slot],
                _ => &VecDeque::new(),
            };
            let held = self.staged[slot].iter().chain(visible);
            let held_bytes: usize = held.clone().map(Vec::len).sum();
            // A reliable lane holding no bytes takes one message of any
            // admitted size (netcode.md 11).
            let one_message = matches!(delivery, Delivery::Reliable(_)) && held_bytes == 0;
            if held.count() >= messages || (!one_message && held_bytes + payload.len() > bytes) {
                return Err(SendError::WouldBlock);
            }
            self.staged[slot].push_back(payload);
            Ok(())
        }

        fn flush(&mut self, config: &ReliableConfig) {
            for (staged, visible) in self.staged.iter_mut().zip(&mut self.visible) {
                visible.append(staged);
            }
            for (visible, bounds) in self.visible[RELIABLE_LANES..].iter_mut().zip(&config.lanes) {
                while visible.len() > bounds.unreliable_messages
                    || visible.iter().map(Vec::len).sum::<usize>() > bounds.unreliable_bytes
                {
                    visible.pop_front();
                }
            }
            if let Some(latest) = self.staged_latest.take() {
                self.visible_latest = Some(latest);
            }
        }

        /// Checks one poll's messages: each lane's reliable and unreliable
        /// messages in order and complete, the latest state after them all.
        fn drain(&mut self, got: Vec<(Delivery, Vec<u8>)>) -> Result<(), TestCaseError> {
            let mut lanes: [Vec<Vec<u8>>; 2 * RELIABLE_LANES] = Default::default();
            let mut latest = None;
            for (delivery, payload) in got {
                prop_assert!(latest.is_none(), "a lane message after latest state");
                match slot(delivery) {
                    Some(slot) => lanes[slot].push(payload),
                    None => latest = Some(payload),
                }
            }
            for (got, visible) in lanes.iter().zip(&mut self.visible) {
                let want: Vec<_> = visible.drain(..).collect();
                prop_assert_eq!(got, &want);
            }
            prop_assert_eq!(latest, self.visible_latest.take());
            Ok(())
        }
    }

    /// Defect: a lane that reorders, drops or duplicates reliable or
    /// unreliable messages, one lane's bounds refusing another lane or
    /// class, an unpolled receiver refusing its sender's unreliable messages
    /// or keeping the oldest instead of the newest (#443), a stale latest
    /// state, staged data leaking before `flush`, a lane that keeps refusing
    /// after a drain, or a capacity that disagrees with admission. Oracle: a model with a stage/flush step, a reliable
    /// and an unreliable FIFO per lane bounded by that lane's configuration,
    /// and one slot; a reliable send succeeds exactly when the capacity
    /// reported just before it says the payload fits.
    #[test]
    fn memory_duplex_matches_the_staged_lane_model_in_both_directions() {
        check(
            (config(), prop::collection::vec(op(), 1..400)),
            |(config, ops)| {
                let (mut client, mut server) =
                    memory_duplex_with(&config).expect("valid configuration");
                assert!(matches!(&client.poll(0)[..], [ClientEvent::Connected]));
                assert!(matches!(
                    &server.poll(0)[..],
                    [ServerEvent::Connected { .. }]
                ));
                let (mut to_server, mut to_client) = (Direction::default(), Direction::default());
                for op in ops {
                    match op {
                        Op::ClientSend(delivery, payload, repeat) => {
                            for _ in 0..repeat {
                                let lane = match delivery {
                                    Delivery::Reliable(lane) => Some(lane),
                                    Delivery::Unreliable(_) | Delivery::LatestState => None,
                                };
                                let capacity = lane.map(|lane| client.capacity(lane));
                                let result = client.send(delivery, &payload);
                                if let Some(capacity) = capacity
                                    && payload.len() <= config.max_message_bytes
                                {
                                    prop_assert_eq!(result.is_ok(), fits(capacity, payload.len()));
                                }
                                prop_assert_eq!(
                                    result,
                                    to_server.send(&config, delivery, payload.clone())
                                );
                            }
                        }
                        Op::ServerSend(delivery, payload, repeat) => {
                            for _ in 0..repeat {
                                let lane = match delivery {
                                    Delivery::Reliable(lane) => Some(lane),
                                    Delivery::Unreliable(_) | Delivery::LatestState => None,
                                };
                                let capacity =
                                    lane.map(|lane| server.capacity(SOLO_CONNECTION, lane));
                                let result = server.send(SOLO_CONNECTION, delivery, &payload);
                                if let Some(capacity) = capacity
                                    && payload.len() <= config.max_message_bytes
                                {
                                    prop_assert_eq!(result.is_ok(), fits(capacity, payload.len()));
                                }
                                prop_assert_eq!(
                                    result,
                                    to_client.send(&config, delivery, payload.clone())
                                );
                            }
                        }
                        Op::ClientFlush => {
                            client.flush(1);
                            to_server.flush(&config);
                        }
                        Op::ServerFlush => {
                            server.flush(1);
                            to_client.flush(&config);
                        }
                        Op::ClientPoll => {
                            let got = client
                                .poll(2)
                                .into_iter()
                                .map(|e| match e {
                                    ClientEvent::Message { delivery, payload } => {
                                        (delivery, payload)
                                    }
                                    other => panic!("unexpected {other:?}"),
                                })
                                .collect();
                            to_client.drain(got)?;
                        }
                        Op::ServerPoll => {
                            let got = server
                                .poll(2)
                                .into_iter()
                                .map(|e| match e {
                                    ServerEvent::Message {
                                        delivery, payload, ..
                                    } => (delivery, payload),
                                    other => panic!("unexpected {other:?}"),
                                })
                                .collect();
                            to_server.drain(got)?;
                        }
                    }
                }
                Ok(())
            },
        );
    }
}
