//! Bounded multiplexing of several server transports into one connection namespace.
//!
//! Child connection identifiers are meaningful only inside their source transport.
//! The mux translates `(source index, child id)` into a stable process-global id.
//! A tiny orphan queue tolerates children that publish a message immediately
//! before `Connected`; an orphan must be claimed in the same or following mux poll.

use std::collections::BTreeMap;

use crate::{ConnectionId, Delivery, SendError, ServerEvent, ServerIo};

/// Maximum orphan messages retained across all child transports.
pub const MAX_MUX_ORPHAN_MESSAGES: usize = 64;
/// Maximum orphan payload bytes retained across all child transports.
pub const MAX_MUX_ORPHAN_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug)]
struct Route {
    source: usize,
    child: ConnectionId,
}

#[derive(Debug)]
struct BufferedMessage {
    delivery: Delivery,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct OrphanMessages {
    first_poll: u64,
    bytes: usize,
    messages: Vec<BufferedMessage>,
}

/// Several [`ServerIo`] implementations presented as one process-global server.
pub struct ServerIoMux {
    sources: Vec<Box<dyn ServerIo>>,
    routes: BTreeMap<ConnectionId, Route>,
    reverse: BTreeMap<(usize, ConnectionId), ConnectionId>,
    orphans: BTreeMap<(usize, ConnectionId), OrphanMessages>,
    orphan_messages: usize,
    orphan_bytes: usize,
    retired_until: BTreeMap<(usize, ConnectionId), u64>,
    poll_generation: u64,
    next_connection: Option<ConnectionId>,
    admitting: bool,
}

impl std::fmt::Debug for ServerIoMux {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerIoMux")
            .field("source_count", &self.sources.len())
            .field("routes", &self.routes)
            .field("orphans", &self.orphans)
            .field("orphan_messages", &self.orphan_messages)
            .field("orphan_bytes", &self.orphan_bytes)
            .field("poll_generation", &self.poll_generation)
            .field("next_connection", &self.next_connection)
            .field("admitting", &self.admitting)
            .finish_non_exhaustive()
    }
}

impl Default for ServerIoMux {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerIoMux {
    /// Creates an empty admitting mux.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sources: Vec::new(),
            routes: BTreeMap::new(),
            reverse: BTreeMap::new(),
            orphans: BTreeMap::new(),
            orphan_messages: 0,
            orphan_bytes: 0,
            retired_until: BTreeMap::new(),
            poll_generation: 0,
            next_connection: Some(ConnectionId::MIN),
            admitting: true,
        }
    }

    /// Adds one child transport. Its index remains stable for this mux's lifetime.
    pub fn push(&mut self, source: impl ServerIo + 'static) {
        self.sources.push(Box::new(source));
    }

    /// Adds one already-erased child transport.
    pub fn push_boxed(&mut self, source: Box<dyn ServerIo>) {
        self.sources.push(source);
    }

    /// Returns the number of child transports.
    #[must_use]
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    fn remove_route(&mut self, source: usize, child: ConnectionId) -> Option<ConnectionId> {
        let public = self.reverse.remove(&(source, child))?;
        self.routes.remove(&public);
        Some(public)
    }

    fn remove_orphan(&mut self, source: usize, child: ConnectionId) -> Option<OrphanMessages> {
        let orphan = self.orphans.remove(&(source, child))?;
        self.orphan_messages = self.orphan_messages.saturating_sub(orphan.messages.len());
        self.orphan_bytes = self.orphan_bytes.saturating_sub(orphan.bytes);
        Some(orphan)
    }

    fn retire(&mut self, source: usize, child: ConnectionId) {
        self.remove_orphan(source, child);
        self.retired_until
            .insert((source, child), self.poll_generation.saturating_add(1));
    }

    fn disconnect_child(&mut self, source: usize, child: ConnectionId, now_ms: u64) {
        self.remove_route(source, child);
        self.retire(source, child);
        self.sources[source].disconnect(child, now_ms);
    }

    fn buffer_message(
        &mut self,
        source: usize,
        child: ConnectionId,
        delivery: Delivery,
        payload: Vec<u8>,
        now_ms: u64,
    ) {
        if self.retired_until.contains_key(&(source, child)) {
            return;
        }
        let within_messages = self
            .orphan_messages
            .checked_add(1)
            .is_some_and(|count| count <= MAX_MUX_ORPHAN_MESSAGES);
        let within_bytes = self
            .orphan_bytes
            .checked_add(payload.len())
            .is_some_and(|bytes| bytes <= MAX_MUX_ORPHAN_BYTES);
        if !within_messages || !within_bytes {
            self.disconnect_child(source, child, now_ms);
            return;
        }

        let payload_bytes = payload.len();
        let orphan = self
            .orphans
            .entry((source, child))
            .or_insert_with(|| OrphanMessages {
                first_poll: self.poll_generation,
                bytes: 0,
                messages: Vec::new(),
            });
        orphan.bytes = orphan.bytes.saturating_add(payload_bytes);
        orphan.messages.push(BufferedMessage { delivery, payload });
        self.orphan_messages += 1;
        self.orphan_bytes += payload_bytes;
    }

    fn publish_connected(
        &mut self,
        source: usize,
        child: ConnectionId,
        now_ms: u64,
        output: &mut Vec<ServerEvent>,
    ) {
        if self.reverse.contains_key(&(source, child)) {
            return;
        }
        if !self.admitting || self.retired_until.contains_key(&(source, child)) {
            self.disconnect_child(source, child, now_ms);
            return;
        }
        let Some(public) = self.next_connection else {
            self.stop_admission();
            self.disconnect_child(source, child, now_ms);
            return;
        };
        self.next_connection = public.checked_next();
        self.routes.insert(public, Route { source, child });
        self.reverse.insert((source, child), public);
        output.push(ServerEvent::Connected { conn: public });
        if let Some(orphan) = self.remove_orphan(source, child) {
            output.extend(
                orphan
                    .messages
                    .into_iter()
                    .map(|message| ServerEvent::Message {
                        conn: public,
                        delivery: message.delivery,
                        payload: message.payload,
                    }),
            );
        }
    }

    fn expire_orphans(&mut self, now_ms: u64) {
        let expired: Vec<_> = self
            .orphans
            .iter()
            .filter_map(|(&(source, child), orphan)| {
                (orphan.first_poll < self.poll_generation).then_some((source, child))
            })
            .collect();
        for (source, child) in expired {
            self.disconnect_child(source, child, now_ms);
        }
        self.retired_until
            .retain(|_, until| *until > self.poll_generation);
    }
}

impl ServerIo for ServerIoMux {
    fn poll(&mut self, now_ms: u64) -> Vec<ServerEvent> {
        self.poll_generation = self.poll_generation.saturating_add(1);
        let mut output = Vec::new();
        for source in 0..self.sources.len() {
            for event in self.sources[source].poll(now_ms) {
                match event {
                    ServerEvent::Connected { conn: child } => {
                        self.publish_connected(source, child, now_ms, &mut output);
                    }
                    ServerEvent::Message {
                        conn: child,
                        delivery,
                        payload,
                    } => {
                        if let Some(&public) = self.reverse.get(&(source, child)) {
                            output.push(ServerEvent::Message {
                                conn: public,
                                delivery,
                                payload,
                            });
                        } else {
                            self.buffer_message(source, child, delivery, payload, now_ms);
                        }
                    }
                    ServerEvent::Disconnected {
                        conn: child,
                        reason,
                    } => {
                        if let Some(public) = self.remove_route(source, child) {
                            self.retire(source, child);
                            output.push(ServerEvent::Disconnected {
                                conn: public,
                                reason,
                            });
                        } else {
                            self.retire(source, child);
                        }
                    }
                }
            }
        }
        self.expire_orphans(now_ms);
        output
    }

    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError> {
        let route = *self.routes.get(&conn).ok_or(SendError::UnknownConnection)?;
        self.sources[route.source].send(route.child, delivery, payload)
    }

    fn flush(&mut self, now_ms: u64) {
        for source in &mut self.sources {
            source.flush(now_ms);
        }
    }

    fn disconnect(&mut self, conn: ConnectionId, now_ms: u64) {
        let Some(route) = self.routes.remove(&conn) else {
            return;
        };
        self.reverse.remove(&(route.source, route.child));
        self.retire(route.source, route.child);
        self.sources[route.source].disconnect(route.child, now_ms);
    }

    fn stop_admission(&mut self) {
        if !self.admitting {
            return;
        }
        self.admitting = false;
        for source in &mut self.sources {
            source.stop_admission();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #254: an orphan is claimed by a `Connected` in the next poll but not
    /// the one after, and a child that overflows the orphan message cap is
    /// retired — its later `Connected` is refused.
    #[wasm_bindgen_test(unsupported = test)]
    fn orphan_timing_and_message_cap_are_exact() {
        let message = |child: ConnectionId| ServerEvent::Message {
            conn: child,
            delivery: Delivery::ReliableOrdered,
            payload: b"early".to_vec(),
        };
        let mut source = ScriptedServer::default();
        source.polls.push_back(vec![message(id(4))]);
        source.polls.push_back(vec![]);
        source
            .polls
            .push_back(vec![ServerEvent::Connected { conn: id(4) }]);
        let mut mux = ServerIoMux::new();
        mux.push(source);
        assert!(mux.poll(0).is_empty());
        assert!(mux.poll(1).is_empty(), "the orphan waits one more poll");
        assert!(
            mux.poll(2).is_empty(),
            "expired: the child was retired, not connected"
        );

        let mut source = ScriptedServer::default();
        let flood: Vec<ServerEvent> = (0..=MAX_MUX_ORPHAN_MESSAGES)
            .map(|_| message(id(5)))
            .collect();
        source.polls.push_back(flood);
        source
            .polls
            .push_back(vec![ServerEvent::Connected { conn: id(5) }]);
        let mut mux = ServerIoMux::new();
        mux.push(source);
        assert!(mux.poll(0).is_empty());
        assert!(mux.poll(1).is_empty(), "the flooding child is retired");

        let mut mux = ServerIoMux::new();
        mux.push(ScriptedServer::default());
        mux.push(ScriptedServer::default());
        assert_eq!(mux.source_count(), 2);
        mux.stop_admission();
        mux.stop_admission();
        assert!(mux.poll(0).is_empty());
    }

    use super::*;
    use crate::DisconnectReason;

    #[derive(Debug, Default)]
    struct ScriptedServer {
        polls: VecDeque<Vec<ServerEvent>>,
        sent: Vec<(ConnectionId, Delivery, Vec<u8>)>,
        disconnected: Vec<ConnectionId>,
        stopped: bool,
    }

    impl ServerIo for ScriptedServer {
        fn poll(&mut self, _now_ms: u64) -> Vec<ServerEvent> {
            self.polls.pop_front().unwrap_or_default()
        }

        fn send(
            &mut self,
            conn: ConnectionId,
            delivery: Delivery,
            payload: &[u8],
        ) -> Result<(), SendError> {
            self.sent.push((conn, delivery, payload.to_vec()));
            Ok(())
        }

        fn flush(&mut self, _now_ms: u64) {}

        fn disconnect(&mut self, conn: ConnectionId, _now_ms: u64) {
            self.disconnected.push(conn);
        }

        fn stop_admission(&mut self) {
            self.stopped = true;
        }
    }

    fn id(raw: u64) -> ConnectionId {
        ConnectionId::from_raw(raw).expect("nonzero fixture id")
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn default_mux_accepts_the_memory_transport() {
        let (_client, server) = crate::memory::memory_duplex();
        let mut mux = ServerIoMux::new();
        mux.push(server);
        assert_eq!(mux.source_count(), 1);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn child_namespaces_route_independently() {
        let mut one = ScriptedServer::default();
        one.polls
            .push_back(vec![ServerEvent::Connected { conn: id(1) }]);
        let mut two = ScriptedServer::default();
        two.polls
            .push_back(vec![ServerEvent::Connected { conn: id(1) }]);
        let mut mux = ServerIoMux::new();
        mux.push(one);
        mux.push(two);

        let events = mux.poll(0);
        assert_eq!(
            events,
            vec![
                ServerEvent::Connected { conn: id(1) },
                ServerEvent::Connected { conn: id(2) },
            ]
        );
        mux.send(id(2), Delivery::ReliableOrdered, b"two")
            .expect("route second source");
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn orphan_is_ordered_after_connected_same_or_next_poll() {
        let orphan = ServerEvent::Message {
            conn: id(7),
            delivery: Delivery::ReliableOrdered,
            payload: b"early".to_vec(),
        };
        let connected = ServerEvent::Connected { conn: id(7) };

        for polls in [
            VecDeque::from([vec![orphan.clone(), connected.clone()]]),
            VecDeque::from([vec![orphan.clone()], vec![connected.clone()]]),
        ] {
            let source = ScriptedServer {
                polls,
                ..ScriptedServer::default()
            };
            let mut mux = ServerIoMux::new();
            mux.push(source);
            let mut events = mux.poll(10);
            if events.is_empty() {
                events = mux.poll(11);
            }
            assert_eq!(
                events,
                vec![
                    ServerEvent::Connected { conn: id(1) },
                    ServerEvent::Message {
                        conn: id(1),
                        delivery: Delivery::ReliableOrdered,
                        payload: b"early".to_vec(),
                    },
                ]
            );
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn unclaimed_or_overflowing_orphan_is_peer_local() {
        let healthy = id(8);
        let bad = id(9);
        let mut source = ScriptedServer::default();
        source.polls.push_back(vec![
            ServerEvent::Connected { conn: healthy },
            ServerEvent::Message {
                conn: bad,
                delivery: Delivery::ReliableOrdered,
                payload: vec![0; MAX_MUX_ORPHAN_BYTES + 1],
            },
        ]);
        source.polls.push_back(vec![ServerEvent::Message {
            conn: healthy,
            delivery: Delivery::ReliableOrdered,
            payload: b"alive".to_vec(),
        }]);
        let mut mux = ServerIoMux::new();
        mux.push(source);

        assert_eq!(mux.poll(0), vec![ServerEvent::Connected { conn: id(1) }]);
        assert_eq!(
            mux.poll(1),
            vec![ServerEvent::Message {
                conn: id(1),
                delivery: Delivery::ReliableOrdered,
                payload: b"alive".to_vec(),
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn disconnect_retires_child_key_against_late_events() {
        let child = id(3);
        let mut source = ScriptedServer::default();
        source.polls.push_back(vec![
            ServerEvent::Connected { conn: child },
            ServerEvent::Disconnected {
                conn: child,
                reason: DisconnectReason::Transport,
            },
            ServerEvent::Message {
                conn: child,
                delivery: Delivery::ReliableOrdered,
                payload: b"late".to_vec(),
            },
        ]);
        let mut mux = ServerIoMux::new();
        mux.push(source);

        let events = mux.poll(0);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ServerEvent::Connected { .. }));
        assert!(matches!(events[1], ServerEvent::Disconnected { .. }));
        assert!(mux.poll(1).is_empty());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn duplicate_connected_does_not_break_the_route() {
        let child = id(4);
        let mut source = ScriptedServer::default();
        source.polls.push_back(vec![
            ServerEvent::Connected { conn: child },
            ServerEvent::Connected { conn: child },
        ]);
        let mut mux = ServerIoMux::new();
        mux.push(source);

        assert_eq!(mux.poll(0).len(), 1);
        assert_eq!(
            mux.send(id(1), Delivery::ReliableOrdered, b"still routed"),
            Ok(())
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
mod properties {
    use super::*;
    use crate::DisconnectReason;
    use crate::proptest_support::check;
    use proptest::prelude::*;
    use std::cell::RefCell;
    use std::collections::{BTreeSet, VecDeque};
    use std::rc::Rc;

    #[derive(Debug, Clone)]
    enum Child {
        Connected(u8),
        Message(u8),
        Disconnected(u8),
    }

    fn child() -> impl Strategy<Value = Child> {
        prop_oneof![
            2 => (1u8..4).prop_map(Child::Connected),
            4 => (1u8..4).prop_map(Child::Message),
            1 => (1u8..4).prop_map(Child::Disconnected),
        ]
    }

    /// One poll's script per source.
    type Poll = Vec<Vec<Child>>;

    #[derive(Default)]
    struct Recorded {
        sent: Vec<(ConnectionId, Delivery, Vec<u8>)>,
    }

    /// A child transport replaying a script and recording what the mux sends.
    struct Scripted {
        polls: VecDeque<Vec<ServerEvent>>,
        recorded: Rc<RefCell<Recorded>>,
    }

    impl ServerIo for Scripted {
        fn poll(&mut self, _now_ms: u64) -> Vec<ServerEvent> {
            self.polls.pop_front().unwrap_or_default()
        }

        fn send(
            &mut self,
            conn: ConnectionId,
            delivery: Delivery,
            payload: &[u8],
        ) -> Result<(), SendError> {
            self.recorded
                .borrow_mut()
                .sent
                .push((conn, delivery, payload.to_vec()));
            Ok(())
        }

        fn flush(&mut self, _now_ms: u64) {}

        fn disconnect(&mut self, _conn: ConnectionId, _now_ms: u64) {}

        fn stop_admission(&mut self) {}
    }

    fn cid(raw: u8) -> ConnectionId {
        ConnectionId::from_raw(u64::from(raw)).unwrap()
    }

    /// Payload tagging a message with its origin so misrouting is visible.
    fn tag(source: usize, child: u8, index: usize) -> Vec<u8> {
        vec![source as u8, child, index as u8]
    }

    /// Defect: a public id reused after a disconnect, a message attributed
    /// to another source's child, per-child order lost in the merge, or a
    /// send routed to the wrong child transport. Oracle: the merged stream
    /// is well-formed per public id (Connected first, Disconnected last),
    /// every message's tag names the public id's own (source, child) and
    /// messages of one child keep their order, ids are never reused, and a
    /// send lands at exactly the tagged child transport.
    #[test]
    fn mux_merges_children_into_unique_well_formed_connections() {
        let strategy = prop::collection::vec(
            prop::collection::vec(prop::collection::vec(child(), 0..5), 2),
            1..6,
        );
        check(strategy, |polls: Vec<Poll>| {
            let sources = 2;
            let recorders: Vec<_> = (0..sources)
                .map(|_| Rc::new(RefCell::new(Recorded::default())))
                .collect();
            let mut scripts: Vec<VecDeque<Vec<ServerEvent>>> = vec![VecDeque::new(); sources];
            let mut index = 0usize;
            for poll in &polls {
                for (source, events) in poll.iter().enumerate() {
                    let events = events
                        .iter()
                        .map(|event| match event {
                            Child::Connected(c) => ServerEvent::Connected { conn: cid(*c) },
                            Child::Message(c) => {
                                index += 1;
                                ServerEvent::Message {
                                    conn: cid(*c),
                                    delivery: Delivery::ReliableOrdered,
                                    payload: tag(source, *c, index),
                                }
                            }
                            Child::Disconnected(c) => ServerEvent::Disconnected {
                                conn: cid(*c),
                                reason: DisconnectReason::Peer,
                            },
                        })
                        .collect();
                    scripts[source].push_back(events);
                }
            }
            let mut mux = ServerIoMux::new();
            for (source, script) in scripts.into_iter().enumerate() {
                mux.push(Scripted {
                    polls: script,
                    recorded: Rc::clone(&recorders[source]),
                });
            }

            let mut seen_ids = BTreeSet::new();
            let mut live: BTreeMap<ConnectionId, (u8, u8)> = BTreeMap::new();
            let mut last_index: BTreeMap<(u8, u8), u8> = BTreeMap::new();
            for (poll_index, _) in polls.iter().enumerate() {
                let events = mux.poll(poll_index as u64 * 10);
                for event in events {
                    match event {
                        ServerEvent::Connected { conn } => {
                            prop_assert!(seen_ids.insert(conn), "public id {conn:?} reused");
                            // Resolve the origin from the first tagged message; until
                            // then, any send proves routing (see below).
                            live.insert(conn, (u8::MAX, u8::MAX));
                        }
                        ServerEvent::Message { conn, payload, .. } => {
                            let origin = live
                                .get_mut(&conn)
                                .expect("message on a connection that is not live");
                            let (source, child, index) = (payload[0], payload[1], payload[2]);
                            if *origin == (u8::MAX, u8::MAX) {
                                *origin = (source, child);
                            }
                            prop_assert_eq!(
                                *origin,
                                (source, child),
                                "message misattributed to {:?}",
                                conn
                            );
                            let last = last_index.entry((source, child)).or_insert(0);
                            prop_assert!(
                                index > *last,
                                "order lost for child {child} of source {source}"
                            );
                            *last = index;
                        }
                        ServerEvent::Disconnected { conn, .. } => {
                            prop_assert!(
                                live.remove(&conn).is_some(),
                                "disconnect for a non-live id {conn:?}"
                            );
                        }
                    }
                }
                // Sends: every live id routes to exactly its own child; retired
                // and unknown ids are refused.
                for (conn, origin) in live.clone() {
                    let payload = vec![0xEE];
                    prop_assert_eq!(mux.send(conn, Delivery::LatestState, &payload), Ok(()));
                    let total: usize = recorders.iter().map(|r| r.borrow().sent.len()).sum();
                    prop_assert!(total > 0);
                    if origin != (u8::MAX, u8::MAX) {
                        let recorded = recorders[usize::from(origin.0)].borrow();
                        let last = recorded.sent.last().expect("the send reached its source");
                        prop_assert_eq!(last.0, cid(origin.1));
                        prop_assert_eq!(&last.2, &payload);
                    }
                }
                for retired in seen_ids.iter().filter(|id| !live.contains_key(id)) {
                    prop_assert_eq!(
                        mux.send(*retired, Delivery::LatestState, &[1]),
                        Err(SendError::UnknownConnection)
                    );
                }
            }
            Ok(())
        });
    }
}
