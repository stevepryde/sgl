//! Bounded in-process transport used by solo play and transport tests.
//!
//! Each direction has a reliable FIFO plus one replaceable latest-state slot.
//! `send` only stages data; `flush(now_ms)` stamps it and makes it visible to
//! the peer. A poll therefore measures elapsed virtual time from real item
//! metadata instead of special-casing the in-memory transport.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use crate::{
    ClientEvent, ClientIo, ConnectionId, Delivery, DisconnectReason, MAX_LATEST_STATE_BYTES,
    MAX_RELIABLE_MESSAGE_BYTES, RELIABLE_OUTBOUND_BYTES, RELIABLE_OUTBOUND_MESSAGES, RttEstimate,
    RttEstimator, SendError, ServerEvent, ServerIo,
};

/// Maximum reliable messages queued in either direction.
pub const MAX_RELIABLE_QUEUED: usize = RELIABLE_OUTBOUND_MESSAGES;

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

#[derive(Debug)]
struct LaneQueue<T> {
    reliable: VecDeque<(usize, T)>,
    reliable_bytes: usize,
    latest: Option<T>,
}

impl<T> Default for LaneQueue<T> {
    fn default() -> Self {
        Self {
            reliable: VecDeque::new(),
            reliable_bytes: 0,
            latest: None,
        }
    }
}

impl<T> LaneQueue<T> {
    fn reliable_len(&self) -> usize {
        self.reliable.len()
    }

    fn reliable_bytes(&self) -> usize {
        self.reliable_bytes
    }

    fn push_reliable(&mut self, item: T, bytes: usize) -> Result<(), T> {
        if self.reliable.len() >= MAX_RELIABLE_QUEUED
            || self.reliable_bytes.saturating_add(bytes) > RELIABLE_OUTBOUND_BYTES
        {
            return Err(item);
        }
        self.reliable_bytes += bytes;
        self.reliable.push_back((bytes, item));
        Ok(())
    }

    fn push_latest(&mut self, item: T) {
        self.latest = Some(item);
    }

    /// Reliable transitions always drain before the newest replaceable state.
    fn pop(&mut self) -> Option<T> {
        if let Some((bytes, item)) = self.reliable.pop_front() {
            self.reliable_bytes -= bytes;
            Some(item)
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

type SharedQueue = Rc<RefCell<LaneQueue<SentItem>>>;

#[derive(Debug)]
struct DuplexEnd {
    outbound: SharedQueue,
    inbound: SharedQueue,
    pending: LaneQueue<PendingItem>,
    open: Rc<Cell<bool>>,
    local_ended: bool,
}

impl DuplexEnd {
    fn connected(&self) -> bool {
        self.open.get()
            && Rc::strong_count(&self.outbound) > 1
            && Rc::strong_count(&self.inbound) > 1
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        if !self.connected() {
            return Err(SendError::Disconnected);
        }
        let cap = match delivery {
            Delivery::ReliableOrdered => MAX_RELIABLE_MESSAGE_BYTES,
            Delivery::LatestState => MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > cap {
            return Err(SendError::PayloadTooLarge);
        }
        let item = PendingItem {
            delivery,
            payload: payload.to_vec(),
        };
        match delivery {
            Delivery::ReliableOrdered => {
                let in_flight = self.outbound.borrow().reliable_len();
                let in_flight_bytes = self.outbound.borrow().reliable_bytes();
                if in_flight
                    .checked_add(self.pending.reliable_len())
                    .is_none_or(|total| total >= MAX_RELIABLE_QUEUED)
                    || in_flight_bytes
                        .checked_add(self.pending.reliable_bytes())
                        .and_then(|total| total.checked_add(payload.len()))
                        .is_none_or(|total| total > RELIABLE_OUTBOUND_BYTES)
                {
                    return Err(SendError::ReliableOverflow);
                }
                self.pending
                    .push_reliable(item, payload.len())
                    .map_err(|_| SendError::ReliableOverflow)
            }
            Delivery::LatestState => {
                self.pending.push_latest(item);
                Ok(())
            }
        }
    }

    fn flush(&mut self, now_ms: u64) {
        if !self.connected() {
            self.pending.clear();
            return;
        }
        let mut outbound = self.outbound.borrow_mut();
        while let Some((bytes, item)) = self.pending.reliable.pop_front() {
            self.pending.reliable_bytes -= bytes;
            let sent = SentItem {
                delivery: item.delivery,
                sent_at_ms: now_ms,
                payload: item.payload,
            };
            // `send` admission already counted this item against the shared
            // reliable bounds, so the staged push cannot overflow.
            if outbound.push_reliable(sent, bytes).is_err() {
                unreachable!("send admission bounds staged reliable items");
            }
        }
        if let Some(item) = self.pending.latest.take() {
            outbound.push_latest(SentItem {
                delivery: item.delivery,
                sent_at_ms: now_ms,
                payload: item.payload,
            });
        }
    }

    fn drain(&mut self) -> Vec<SentItem> {
        let mut inbound = self.inbound.borrow_mut();
        std::iter::from_fn(|| inbound.pop()).collect()
    }

    fn disconnect(&mut self, now_ms: u64) {
        self.flush(now_ms);
        self.open.set(false);
        self.local_ended = true;
    }
}

/// Creates the client and server halves of one bounded in-memory connection.
#[must_use]
pub fn memory_duplex() -> (MemoryClientIo, MemoryServerIo) {
    let client_to_server = Rc::new(RefCell::new(LaneQueue::default()));
    let server_to_client = Rc::new(RefCell::new(LaneQueue::default()));
    let open = Rc::new(Cell::new(true));

    let client = DuplexEnd {
        outbound: Rc::clone(&client_to_server),
        inbound: Rc::clone(&server_to_client),
        pending: LaneQueue::default(),
        open: Rc::clone(&open),
        local_ended: false,
    };
    let server = DuplexEnd {
        outbound: server_to_client,
        inbound: client_to_server,
        pending: LaneQueue::default(),
        open,
        local_ended: false,
    };

    (
        MemoryClientIo {
            duplex: client,
            rtt: RttEstimator::new(),
            announced: false,
            peer_disconnect_announced: false,
        },
        MemoryServerIo {
            duplex: server,
            announced: false,
            peer_disconnect_announced: false,
        },
    )
}

/// Client half of an in-memory duplex.
#[derive(Debug)]
pub struct MemoryClientIo {
    duplex: DuplexEnd,
    rtt: RttEstimator,
    announced: bool,
    peer_disconnect_announced: bool,
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
        if self.announced
            && !self.duplex.local_ended
            && !self.duplex.connected()
            && !self.peer_disconnect_announced
        {
            self.peer_disconnect_announced = true;
            events.push(ClientEvent::Disconnected {
                reason: DisconnectReason::Peer,
            });
        }
        events
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        self.duplex.send(delivery, payload)
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
    peer_disconnect_announced: bool,
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
        if self.announced
            && !self.duplex.local_ended
            && !self.duplex.connected()
            && !self.peer_disconnect_announced
        {
            self.peer_disconnect_announced = true;
            events.push(ServerEvent::Disconnected {
                conn: SOLO_CONNECTION,
                reason: DisconnectReason::Peer,
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
            client.send(Delivery::ReliableOrdered, &big).unwrap();
        }
        assert_eq!(
            client.send(Delivery::ReliableOrdered, &big),
            Err(SendError::ReliableOverflow)
        );
        client.flush(0);
        assert_eq!(server.poll(1).len(), 5, "connected plus four payloads");
        client
            .send(Delivery::ReliableOrdered, &big)
            .expect("allowance returned after the drain");
    }

    /// #237: a half that closes (or is dropped) before the other half's first
    /// poll must still surface the whole lifecycle — `Connected`, the data it
    /// flushed, then `Disconnected { Peer }` — instead of staying silent
    /// forever.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_peer_gone_before_the_first_poll_still_yields_the_lifecycle() {
        let (mut client, mut server) = memory_duplex();
        client.send(Delivery::ReliableOrdered, b"bye").unwrap();
        client.disconnect(0);
        assert_eq!(
            server.poll(1),
            vec![
                ServerEvent::Connected {
                    conn: SOLO_CONNECTION
                },
                ServerEvent::Message {
                    conn: SOLO_CONNECTION,
                    delivery: Delivery::ReliableOrdered,
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
            client.send(Delivery::ReliableOrdered, b"x"),
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
            .send(SOLO_CONNECTION, Delivery::ReliableOrdered, b"event")
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
            .send(Delivery::ReliableOrdered, b"first")
            .expect("first");
        client
            .send(Delivery::LatestState, b"new")
            .expect("replace latest");
        client
            .send(Delivery::ReliableOrdered, b"second")
            .expect("second");
        client.flush(1);
        assert_eq!(
            messages(&server.poll(1)),
            vec![
                (Delivery::ReliableOrdered, b"first".to_vec()),
                (Delivery::ReliableOrdered, b"second".to_vec()),
                (Delivery::LatestState, b"new".to_vec()),
            ]
        );

        for _ in 0..MAX_RELIABLE_QUEUED {
            client
                .send(Delivery::ReliableOrdered, b"x")
                .expect("within bound");
        }
        assert_eq!(
            client.send(Delivery::ReliableOrdered, b"overflow"),
            Err(SendError::ReliableOverflow)
        );
        client
            .send(Delivery::LatestState, b"still replaceable")
            .expect("latest has an independent slot");
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn shared_payload_and_reliable_byte_caps_fail_before_allocation_growth() {
        let (mut client, _server) = memory_duplex();
        assert_eq!(
            client.send(Delivery::LatestState, &vec![0; MAX_LATEST_STATE_BYTES + 1]),
            Err(SendError::PayloadTooLarge)
        );
        assert_eq!(
            client.send(
                Delivery::ReliableOrdered,
                &vec![0; MAX_RELIABLE_MESSAGE_BYTES + 1]
            ),
            Err(SendError::PayloadTooLarge)
        );

        let payload = vec![0; MAX_RELIABLE_MESSAGE_BYTES];
        for _ in 0..(RELIABLE_OUTBOUND_BYTES / MAX_RELIABLE_MESSAGE_BYTES) {
            client
                .send(Delivery::ReliableOrdered, &payload)
                .expect("within byte allowance");
        }
        assert_eq!(
            client.send(Delivery::ReliableOrdered, b"one byte too many"),
            Err(SendError::ReliableOverflow)
        );
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
            .send(SOLO_CONNECTION, Delivery::ReliableOrdered, b"existing")
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
    use crate::{ClientEvent, ClientIo, ServerIo};
    use proptest::prelude::*;
    use std::collections::VecDeque;

    #[derive(Debug, Clone)]
    enum Op {
        ClientSend(Delivery, Vec<u8>),
        ServerSend(Delivery, Vec<u8>),
        ClientFlush,
        ServerFlush,
        ClientPoll,
        ServerPoll,
    }

    fn delivery() -> impl Strategy<Value = Delivery> {
        prop_oneof![Just(Delivery::ReliableOrdered), Just(Delivery::LatestState)]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (delivery(), bytes(32)).prop_map(|(d, p)| Op::ClientSend(d, p)),
            4 => (delivery(), bytes(32)).prop_map(|(d, p)| Op::ServerSend(d, p)),
            2 => Just(Op::ClientFlush),
            2 => Just(Op::ServerFlush),
            2 => Just(Op::ClientPoll),
            2 => Just(Op::ServerPoll),
        ]
    }

    /// One direction of the model: staged until flushed, then visible to
    /// the peer's next poll; reliable in order, latest newest-wins.
    #[derive(Default)]
    struct Lane {
        staged_reliable: VecDeque<Vec<u8>>,
        staged_latest: Option<Vec<u8>>,
        visible_reliable: VecDeque<Vec<u8>>,
        visible_latest: Option<Vec<u8>>,
    }

    impl Lane {
        fn send(&mut self, delivery: Delivery, payload: Vec<u8>) -> Result<(), SendError> {
            match delivery {
                Delivery::ReliableOrdered => {
                    if self.staged_reliable.len() + self.visible_reliable.len()
                        >= MAX_RELIABLE_QUEUED
                    {
                        return Err(SendError::ReliableOverflow);
                    }
                    self.staged_reliable.push_back(payload);
                }
                Delivery::LatestState => self.staged_latest = Some(payload),
            }
            Ok(())
        }

        fn flush(&mut self) {
            self.visible_reliable.append(&mut self.staged_reliable);
            if let Some(latest) = self.staged_latest.take() {
                self.visible_latest = Some(latest);
            }
        }

        fn drain(&mut self) -> Vec<(Delivery, Vec<u8>)> {
            let mut out: Vec<_> = self
                .visible_reliable
                .drain(..)
                .map(|p| (Delivery::ReliableOrdered, p))
                .collect();
            out.extend(
                self.visible_latest
                    .take()
                    .map(|p| (Delivery::LatestState, p)),
            );
            out
        }
    }

    /// Defect: a lane that reorders reliable messages, shows a stale latest
    /// state, leaks staged data before `flush`, or keeps refusing sends
    /// after a drain. Oracle: two model lanes with a stage/flush step, a
    /// FIFO, and one slot; the reliable cap returns `ReliableOverflow` and
    /// the connection stays usable.
    #[test]
    fn memory_duplex_matches_the_staged_lane_model_in_both_directions() {
        check(prop::collection::vec(op(), 1..400), |ops| {
            let (mut client, mut server) = memory_duplex();
            assert!(matches!(&client.poll(0)[..], [ClientEvent::Connected]));
            assert!(matches!(
                &server.poll(0)[..],
                [ServerEvent::Connected { .. }]
            ));
            let (mut to_server, mut to_client) = (Lane::default(), Lane::default());
            for op in ops {
                match op {
                    Op::ClientSend(delivery, payload) => {
                        prop_assert_eq!(
                            client.send(delivery, &payload),
                            to_server.send(delivery, payload)
                        );
                    }
                    Op::ServerSend(delivery, payload) => {
                        prop_assert_eq!(
                            server.send(SOLO_CONNECTION, delivery, &payload),
                            to_client.send(delivery, payload)
                        );
                    }
                    Op::ClientFlush => {
                        client.flush(1);
                        to_server.flush();
                    }
                    Op::ServerFlush => {
                        server.flush(1);
                        to_client.flush();
                    }
                    Op::ClientPoll => {
                        let got: Vec<_> = client
                            .poll(2)
                            .into_iter()
                            .map(|e| match e {
                                ClientEvent::Message { delivery, payload } => (delivery, payload),
                                other => panic!("unexpected {other:?}"),
                            })
                            .collect();
                        prop_assert_eq!(got, to_client.drain());
                    }
                    Op::ServerPoll => {
                        let got: Vec<_> = server
                            .poll(2)
                            .into_iter()
                            .map(|e| match e {
                                ServerEvent::Message {
                                    delivery, payload, ..
                                } => (delivery, payload),
                                other => panic!("unexpected {other:?}"),
                            })
                            .collect();
                        prop_assert_eq!(got, to_server.drain());
                    }
                }
            }
            Ok(())
        });
    }
}
