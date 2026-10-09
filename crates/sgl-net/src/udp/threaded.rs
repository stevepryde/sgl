//! Thread-owned native UDP session transport.
//!
//! The worker owns [`UdpServer`] and its socket. The simulation interacts only
//! with bounded queues, matching the WebSocket worker ownership model.
//!
//! The command queue is the only admission point the caller sees: `send`
//! refuses with [`SendError::WouldBlock`] once a peer's reliable allowance is
//! full, and the worker moves a message to the endpoint only when the
//! endpoint reports room for it, so a refusal never ends a connection. A
//! peer's reliable traffic is therefore buffered at most twice: once here
//! and once in the endpoint.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{EndpointConfig, UdpServer};
use crate::{
    ConnectionId, Delivery, DisconnectReason, Lane, ReliableCapacity, SendError, ServerEvent,
    ServerIo,
};

#[derive(Debug, Clone, Copy)]
pub struct ThreadedUdpConfig {
    pub reliable_queue_messages: usize,
    pub reliable_queue_bytes: usize,
    pub event_queue_messages: usize,
    pub poll_interval: Duration,
}

impl Default for ThreadedUdpConfig {
    fn default() -> Self {
        Self {
            reliable_queue_messages: crate::RELIABLE_OUTBOUND_MESSAGES,
            reliable_queue_bytes: crate::RELIABLE_OUTBOUND_BYTES,
            event_queue_messages: crate::RELIABLE_INBOUND_MESSAGES,
            poll_interval: Duration::from_millis(2),
        }
    }
}

struct CommandState {
    /// Connections the worker has announced and not yet seen end.
    live: BTreeSet<ConnectionId>,
    peers: BTreeMap<ConnectionId, PeerCommands>,
    disconnects: BTreeSet<ConnectionId>,
    stop_admission: bool,
}

#[derive(Default)]
struct PeerCommands {
    reliable: VecDeque<Vec<u8>>,
    reliable_bytes: usize,
    latest: Option<Vec<u8>>,
}

struct CommandQueue {
    state: Mutex<CommandState>,
    max_messages: usize,
    max_bytes: usize,
}

struct PeerIngress {
    reliable: VecDeque<Vec<u8>>,
    reliable_bytes: usize,
    latest: Option<Vec<u8>>,
}

struct IngressState {
    lifecycle: VecDeque<ServerEvent>,
    peers: BTreeMap<ConnectionId, PeerIngress>,
    active: BTreeSet<ConnectionId>,
}

/// Shared worker-to-simulation ingress with independent quotas per peer.
///
/// A noisy peer can exhaust only its own reliable allowance. Snapshot traffic
/// always occupies one overwrite slot, so a delayed simulation never builds a
/// stale-state backlog.
struct IngressHub {
    state: Mutex<IngressState>,
    max_reliable_messages: usize,
    max_reliable_bytes: usize,
}

impl IngressHub {
    fn new(config: &ThreadedUdpConfig) -> Self {
        Self {
            state: Mutex::new(IngressState {
                lifecycle: VecDeque::new(),
                peers: BTreeMap::new(),
                active: BTreeSet::new(),
            }),
            max_reliable_messages: config.event_queue_messages,
            max_reliable_bytes: config.reliable_queue_bytes,
        }
    }

    fn connected(&self, conn: ConnectionId) {
        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        if state.active.insert(conn) {
            state.peers.insert(
                conn,
                PeerIngress {
                    reliable: VecDeque::new(),
                    reliable_bytes: 0,
                    latest: None,
                },
            );
            state.lifecycle.push_back(ServerEvent::Connected { conn });
        }
    }

    fn disconnected(&self, conn: ConnectionId, reason: DisconnectReason) {
        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        state.peers.remove(&conn);
        if state.active.remove(&conn) {
            state
                .lifecycle
                .push_back(ServerEvent::Disconnected { conn, reason });
        }
    }

    /// Returns false when this peer exceeded its reliable ingress allowance.
    fn message(&self, conn: ConnectionId, delivery: Delivery, payload: Vec<u8>) -> bool {
        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        let Some(peer) = state.peers.get_mut(&conn) else {
            return true;
        };
        match delivery {
            Delivery::Reliable(_) => {
                if peer.reliable.len() >= self.max_reliable_messages
                    || peer.reliable_bytes.saturating_add(payload.len()) > self.max_reliable_bytes
                {
                    return false;
                }
                peer.reliable_bytes += payload.len();
                peer.reliable.push_back(payload);
            }
            Delivery::LatestState => peer.latest = Some(payload),
        }
        true
    }

    fn drain(&self) -> Vec<ServerEvent> {
        const RELIABLE_PER_PEER_PER_POLL: usize = 32;

        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        let mut output: Vec<_> = state.lifecycle.drain(..).collect();
        for (&conn, peer) in &mut state.peers {
            for _ in 0..RELIABLE_PER_PEER_PER_POLL {
                let Some(payload) = peer.reliable.pop_front() else {
                    break;
                };
                peer.reliable_bytes -= payload.len();
                output.push(ServerEvent::Message {
                    conn,
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                });
            }
            if let Some(payload) = peer.latest.take() {
                output.push(ServerEvent::Message {
                    conn,
                    delivery: Delivery::LatestState,
                    payload,
                });
            }
        }
        output
    }
}

impl CommandQueue {
    fn new(config: &ThreadedUdpConfig) -> Self {
        Self {
            state: Mutex::new(CommandState {
                live: BTreeSet::new(),
                peers: BTreeMap::new(),
                disconnects: BTreeSet::new(),
                stop_admission: false,
            }),
            max_messages: config.reliable_queue_messages,
            max_bytes: config.reliable_queue_bytes,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CommandState> {
        self.state.lock().expect("UDP command queue poisoned")
    }

    fn send(&self, id: ConnectionId, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        let mut state = self.lock();
        if !state.live.contains(&id) {
            return Err(SendError::UnknownConnection);
        }
        let max_bytes = match delivery {
            Delivery::Reliable(_) => crate::MAX_RELIABLE_MESSAGE_BYTES,
            Delivery::LatestState => crate::MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > max_bytes {
            return Err(SendError::PayloadTooLarge);
        }
        let peer = state.peers.entry(id).or_default();
        match delivery {
            Delivery::Reliable(_) => {
                // Each peer owns its allowance; a full one refuses this send
                // and nothing else.
                if peer.reliable.len() >= self.max_messages
                    || peer.reliable_bytes.saturating_add(payload.len()) > self.max_bytes
                {
                    return Err(SendError::WouldBlock);
                }
                peer.reliable_bytes += payload.len();
                peer.reliable.push_back(payload.to_vec());
            }
            Delivery::LatestState => {
                // One overwrite slot per peer keeps only current state.
                peer.latest = Some(payload.to_vec());
            }
        }
        Ok(())
    }

    fn capacity(&self, id: ConnectionId) -> ReliableCapacity {
        let state = self.lock();
        if !state.live.contains(&id) {
            return ReliableCapacity::default();
        }
        let (messages, bytes) = state
            .peers
            .get(&id)
            .map_or((0, 0), |peer| (peer.reliable.len(), peer.reliable_bytes));
        ReliableCapacity::remaining(
            self.max_messages.saturating_sub(messages),
            self.max_bytes.saturating_sub(bytes),
        )
    }

    /// The worker announced `id`: the caller may now send to it.
    fn connected(&self, id: ConnectionId) {
        self.lock().live.insert(id);
    }

    /// The connection ended at the worker: its queued commands go with it.
    fn ended(&self, id: ConnectionId) {
        let mut state = self.lock();
        state.live.remove(&id);
        state.peers.remove(&id);
    }

    /// The caller ended the connection.
    fn disconnect(&self, id: ConnectionId) {
        let mut state = self.lock();
        if state.live.remove(&id) {
            state.peers.remove(&id);
            state.disconnects.insert(id);
        }
    }

    fn stop_admission(&self) {
        self.lock().stop_admission = true;
    }

    /// Applies the queued commands to `endpoint` and returns the connections
    /// the caller disconnected. Reliable messages move in order, each only
    /// once the endpoint reports room for it; that report is advisory, so a
    /// message the endpoint still refuses stays at the front for a later
    /// turn.
    fn apply<S: ServerIo>(&self, endpoint: &mut S, now_ms: u64) -> Vec<ConnectionId> {
        let mut state = self.lock();
        if std::mem::take(&mut state.stop_admission) {
            endpoint.stop_admission();
        }
        let disconnects: Vec<_> = std::mem::take(&mut state.disconnects).into_iter().collect();
        for &conn in &disconnects {
            endpoint.disconnect(conn, now_ms);
        }
        let mut gone = Vec::new();
        for (&conn, peer) in &mut state.peers {
            let mut ended = false;
            while let Some(payload) = peer.reliable.front() {
                let capacity = endpoint.capacity(conn, Lane::DEFAULT);
                if capacity.messages == 0 || capacity.bytes < payload.len() {
                    break;
                }
                match endpoint.send(conn, Delivery::RELIABLE_ORDERED, payload) {
                    // Admission already bounded the size, so a payload the
                    // endpoint calls too large is dropped rather than left to
                    // wedge the lane.
                    Ok(()) | Err(SendError::PayloadTooLarge) => {
                        peer.reliable_bytes -= payload.len();
                        peer.reliable.pop_front();
                    }
                    Err(SendError::WouldBlock) => break,
                    Err(SendError::UnknownConnection | SendError::Disconnected) => {
                        ended = true;
                        break;
                    }
                }
            }
            if !ended && let Some(payload) = peer.latest.take() {
                ended = matches!(
                    endpoint.send(conn, Delivery::LatestState, &payload),
                    Err(SendError::UnknownConnection | SendError::Disconnected)
                );
            }
            if ended {
                // Its `Disconnected` event reaches the caller through ingress.
                gone.push(conn);
            }
        }
        for conn in gone {
            state.live.remove(&conn);
            state.peers.remove(&conn);
        }
        state
            .peers
            .retain(|_, peer| !peer.reliable.is_empty() || peer.latest.is_some());
        disconnects
    }
}

/// Simulation-side handle for the UDP worker.
pub struct ThreadedUdpServer {
    local_addr: SocketAddr,
    commands: Arc<CommandQueue>,
    ingress: Arc<IngressHub>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ThreadedUdpServer {
    /// Bind synchronously so startup failure remains fatal to the caller.
    pub fn bind(
        port: u16,
        net_config: EndpointConfig,
        worker_config: ThreadedUdpConfig,
    ) -> io::Result<Self> {
        Self::bind_addr(
            SocketAddr::from(([0, 0, 0, 0], port)),
            net_config,
            worker_config,
        )
    }

    /// Bind an operator-selected address synchronously before starting the worker.
    pub fn bind_addr(
        addr: SocketAddr,
        net_config: EndpointConfig,
        worker_config: ThreadedUdpConfig,
    ) -> io::Result<Self> {
        let endpoint = UdpServer::bind(addr, net_config)?;
        let local_addr = endpoint.local_addr()?;
        let commands = Arc::new(CommandQueue::new(&worker_config));
        let worker_commands = Arc::clone(&commands);
        let ingress = Arc::new(IngressHub::new(&worker_config));
        let worker_ingress = Arc::clone(&ingress);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("sgl-udp-server".into())
            .spawn(move || {
                run_worker(
                    endpoint,
                    &worker_commands,
                    &worker_ingress,
                    &worker_shutdown,
                    worker_config.poll_interval,
                );
            })?;
        Ok(Self {
            local_addr,
            commands,
            ingress,
            shutdown,
            worker: Some(worker),
        })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ServerIo for ThreadedUdpServer {
    fn poll(&mut self, _now_ms: u64) -> Vec<ServerEvent> {
        self.ingress.drain()
    }

    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError> {
        self.commands.send(conn, delivery, payload)
    }

    fn capacity(&self, conn: ConnectionId, _lane: Lane) -> ReliableCapacity {
        self.commands.capacity(conn)
    }

    fn flush(&mut self, _now_ms: u64) {}

    fn disconnect(&mut self, conn: ConnectionId, _now_ms: u64) {
        self.commands.disconnect(conn);
    }

    fn stop_admission(&mut self) {
        self.commands.stop_admission();
    }
}

impl Drop for ThreadedUdpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(
    mut endpoint: UdpServer,
    commands: &CommandQueue,
    ingress: &IngressHub,
    shutdown: &AtomicBool,
    poll_interval: Duration,
) {
    let started = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        worker_tick(&mut endpoint, commands, ingress, now_ms);
        thread::sleep(poll_interval);
    }
}

/// One worker turn at `now_ms`: surface the endpoint's events into the
/// ingress queues, apply the simulation's queued commands to the endpoint,
/// then flush. The thread calls this on its own clock; tests drive it on
/// theirs over any [`ServerIo`].
fn worker_tick<S: ServerIo>(
    endpoint: &mut S,
    commands: &CommandQueue,
    ingress: &IngressHub,
    now_ms: u64,
) {
    for event in endpoint.poll(now_ms) {
        match event {
            ServerEvent::Connected { conn } => {
                commands.connected(conn);
                ingress.connected(conn);
            }
            ServerEvent::Disconnected { conn, reason } => {
                commands.ended(conn);
                ingress.disconnected(conn, reason);
            }
            ServerEvent::Message {
                conn,
                delivery,
                payload,
            } => {
                if ingress.message(conn, delivery, payload) {
                    continue;
                }
                endpoint.disconnect(conn, now_ms);
                commands.ended(conn);
                ingress.disconnected(conn, DisconnectReason::InboundOverflow);
            }
        }
    }

    for conn in commands.apply(endpoint, now_ms) {
        ingress.disconnected(conn, DisconnectReason::Local);
    }
    endpoint.flush(now_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{SOLO_CONNECTION, memory_duplex};
    use crate::{ClientEvent, ClientIo};

    fn payloads(events: &[ClientEvent], wanted: Delivery) -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|event| match event {
                ClientEvent::Message { delivery, payload } if *delivery == wanted => {
                    Some(payload.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The worker turn is the whole thread minus its clock: endpoint events
    /// reach the ingress queues, queued commands reach the endpoint (latest
    /// state coalesced, reliable in order), and a disconnect command closes
    /// the peer and reports it — all at an injected `now_ms`, over the
    /// memory duplex, with no socket or thread.
    #[test]
    fn worker_tick_relays_events_in_and_commands_out() {
        let config = ThreadedUdpConfig::default();
        let commands = CommandQueue::new(&config);
        let ingress = IngressHub::new(&config);
        let (mut client, mut server) = memory_duplex();

        client
            .send(Delivery::RELIABLE_ORDERED, b"hello")
            .expect("queue hello");
        client.flush(0);
        worker_tick(&mut server, &commands, &ingress, 0);
        let events = ingress.drain();
        assert!(
            matches!(
                &events[..],
                [
                    ServerEvent::Connected { conn },
                    ServerEvent::Message { conn: from, delivery: Delivery::RELIABLE_ORDERED, payload },
                ] if *conn == SOLO_CONNECTION && *from == SOLO_CONNECTION && payload == b"hello"
            ),
            "{events:?}"
        );

        commands
            .send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, b"one")
            .unwrap();
        commands
            .send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, b"two")
            .unwrap();
        commands
            .send(SOLO_CONNECTION, Delivery::LatestState, b"stale")
            .unwrap();
        commands
            .send(SOLO_CONNECTION, Delivery::LatestState, b"fresh")
            .unwrap();
        worker_tick(&mut server, &commands, &ingress, 5);
        let received = client.poll(5);
        assert_eq!(
            payloads(&received, Delivery::RELIABLE_ORDERED),
            vec![b"one".to_vec(), b"two".to_vec()]
        );
        assert_eq!(
            payloads(&received, Delivery::LatestState),
            vec![b"fresh".to_vec()]
        );

        commands.disconnect(SOLO_CONNECTION);
        worker_tick(&mut server, &commands, &ingress, 10);
        assert!(matches!(
            &ingress.drain()[..],
            [ServerEvent::Disconnected { conn, reason: DisconnectReason::Local }]
                if *conn == SOLO_CONNECTION
        ));
        assert!(
            client
                .poll(10)
                .iter()
                .any(|event| matches!(event, ClientEvent::Disconnected { .. })),
            "the peer must observe the worker-side disconnect"
        );
    }

    /// A peer that fills its ingress allowance is disconnected at the
    /// endpoint and reported as `InboundOverflow`; its buffered ingress is
    /// discarded with it (`IngressHub::disconnected`), so the simulation sees
    /// the lifecycle pair and no half-delivered stream, and the peer itself
    /// observes the disconnect.
    #[test]
    fn worker_tick_disconnects_a_peer_that_overflows_ingress() {
        let config = ThreadedUdpConfig {
            event_queue_messages: 2,
            ..ThreadedUdpConfig::default()
        };
        let commands = CommandQueue::new(&config);
        let ingress = IngressHub::new(&config);
        let (mut client, mut server) = memory_duplex();
        assert!(
            matches!(&client.poll(0)[..], [ClientEvent::Connected]),
            "the memory client is connected from the start"
        );
        for payload in [b"m1", b"m2", b"m3"] {
            client.send(Delivery::RELIABLE_ORDERED, payload).unwrap();
        }
        client.flush(0);

        worker_tick(&mut server, &commands, &ingress, 0);
        let events = ingress.drain();
        assert!(
            matches!(
                &events[..],
                [
                    ServerEvent::Connected { conn },
                    ServerEvent::Disconnected { conn: gone, reason: DisconnectReason::InboundOverflow },
                ] if *conn == SOLO_CONNECTION && *gone == SOLO_CONNECTION
            ),
            "{events:?}"
        );
        assert!(
            ingress.drain().is_empty(),
            "nothing lingers for a dropped peer"
        );
        assert!(
            client
                .poll(1)
                .iter()
                .any(|event| matches!(event, ClientEvent::Disconnected { .. })),
            "the overflowing peer must be told"
        );
    }

    fn cid(raw: u64) -> ConnectionId {
        ConnectionId::from_raw(raw).expect("test ids are nonzero")
    }

    /// A memory server end that admits at most `per_tick` reliable messages
    /// between two polls. Its `capacity` over-promises by one message — the
    /// report is advisory — so the worker also meets real refusals.
    struct Throttled {
        inner: crate::MemoryServerIo,
        per_tick: usize,
        admitted: usize,
    }

    impl ServerIo for Throttled {
        fn poll(&mut self, now_ms: u64) -> Vec<ServerEvent> {
            self.admitted = 0;
            self.inner.poll(now_ms)
        }

        fn send(
            &mut self,
            conn: ConnectionId,
            delivery: Delivery,
            payload: &[u8],
        ) -> Result<(), SendError> {
            if delivery != Delivery::LatestState && self.admitted >= self.per_tick {
                return Err(SendError::WouldBlock);
            }
            self.inner.send(conn, delivery, payload)?;
            if delivery != Delivery::LatestState {
                self.admitted += 1;
            }
            Ok(())
        }

        fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
            let inner = self.inner.capacity(conn, lane);
            ReliableCapacity::remaining(
                inner.messages.min(self.per_tick + 1 - self.admitted),
                inner.bytes,
            )
        }

        fn flush(&mut self, now_ms: u64) {
            self.inner.flush(now_ms);
        }

        fn disconnect(&mut self, conn: ConnectionId, now_ms: u64) {
            self.inner.disconnect(conn, now_ms);
        }

        fn stop_admission(&mut self) {
            self.inner.stop_admission();
        }
    }

    /// Defect (#267): the worker disconnecting a peer whose endpoint refuses
    /// a send, or dropping, duplicating or reordering the refused message.
    /// Oracle: the memory client's received sequence while the endpoint takes
    /// two messages per tick and refuses the third: every queued message
    /// arrives once, in order, nobody is disconnected, and the caller is
    /// refused only while its command allowance is full.
    #[test]
    fn worker_moves_messages_only_as_the_endpoint_admits_them() {
        let config = ThreadedUdpConfig {
            reliable_queue_messages: 50,
            ..ThreadedUdpConfig::default()
        };
        let commands = CommandQueue::new(&config);
        let ingress = IngressHub::new(&config);
        let (mut client, server) = memory_duplex();
        let mut server = Throttled {
            inner: server,
            per_tick: 2,
            admitted: 0,
        };
        worker_tick(&mut server, &commands, &ingress, 0);
        assert!(matches!(
            &ingress.drain()[..],
            [ServerEvent::Connected { conn }] if *conn == SOLO_CONNECTION
        ));

        let message = |index: u32| index.to_le_bytes().to_vec();
        for index in 0..50 {
            commands
                .send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, &message(index))
                .expect("within the command allowance");
        }
        assert_eq!(
            commands.send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, &message(50)),
            Err(SendError::WouldBlock)
        );
        assert_eq!(
            commands.capacity(SOLO_CONNECTION),
            ReliableCapacity::default()
        );

        let mut received = Vec::new();
        let mut next = 50;
        for tick in 1..=40 {
            worker_tick(&mut server, &commands, &ingress, tick);
            assert!(ingress.drain().is_empty(), "no lifecycle change");
            for event in client.poll(tick) {
                match event {
                    ClientEvent::Message { payload, .. } => {
                        received.push(u32::from_le_bytes(payload.try_into().unwrap()));
                    }
                    ClientEvent::Connected => {}
                    other => panic!("unexpected {other:?}"),
                }
            }
            // The worker freed exactly what the endpoint took; refill it.
            while next < 60 {
                match commands.send(SOLO_CONNECTION, Delivery::RELIABLE_ORDERED, &message(next)) {
                    Ok(()) => next += 1,
                    Err(SendError::WouldBlock) => {
                        assert_eq!(commands.capacity(SOLO_CONNECTION).messages, 0);
                        break;
                    }
                    Err(error) => panic!("refused with {error:?}"),
                }
            }
        }
        assert_eq!(received, (0..60).collect::<Vec<_>>());
    }

    /// Defect: a peer's full allowance refusing another peer, or a refusal
    /// that schedules a disconnect. Oracle: per-peer bounds (netcode.md 11).
    #[test]
    fn a_full_command_allowance_refuses_only_its_own_peer() {
        let config = ThreadedUdpConfig {
            reliable_queue_messages: 1,
            ..ThreadedUdpConfig::default()
        };
        let queue = CommandQueue::new(&config);
        let noisy = cid(7);
        let healthy = cid(8);
        queue.connected(noisy);
        queue.connected(healthy);
        queue
            .send(noisy, Delivery::RELIABLE_ORDERED, b"first")
            .unwrap();
        assert_eq!(
            queue.send(noisy, Delivery::RELIABLE_ORDERED, b"refused"),
            Err(SendError::WouldBlock)
        );
        queue
            .send(healthy, Delivery::RELIABLE_ORDERED, b"healthy")
            .unwrap();
        let state = queue.lock();
        assert!(state.disconnects.is_empty());
        assert!(state.live.contains(&noisy) && state.live.contains(&healthy));
        drop(state);

        // The byte allowance is exact: ten bytes admit 6 + 4, not 6 + 5.
        let config = ThreadedUdpConfig {
            reliable_queue_bytes: 10,
            ..ThreadedUdpConfig::default()
        };
        let queue = CommandQueue::new(&config);
        queue.connected(noisy);
        queue
            .send(noisy, Delivery::RELIABLE_ORDERED, &[0; 6])
            .unwrap();
        assert_eq!(queue.capacity(noisy).bytes, 4);
        assert_eq!(
            queue.send(noisy, Delivery::RELIABLE_ORDERED, &[0; 5]),
            Err(SendError::WouldBlock)
        );
        queue
            .send(noisy, Delivery::RELIABLE_ORDERED, &[0; 4])
            .unwrap();
    }

    /// Defect: commands accepted for a connection the worker never
    /// announced, or kept after it ended. Oracle: `ServerIo::send` and
    /// `capacity` contracts for unknown connections.
    #[test]
    fn unknown_and_ended_connections_are_refused() {
        let queue = CommandQueue::new(&ThreadedUdpConfig::default());
        let peer = cid(3);
        assert_eq!(
            queue.send(peer, Delivery::RELIABLE_ORDERED, b"early"),
            Err(SendError::UnknownConnection)
        );
        assert_eq!(queue.capacity(peer), ReliableCapacity::default());
        queue.connected(peer);
        queue
            .send(peer, Delivery::RELIABLE_ORDERED, b"queued")
            .unwrap();
        assert!(queue.capacity(peer).messages > 0);
        queue.ended(peer);
        assert_eq!(
            queue.send(peer, Delivery::RELIABLE_ORDERED, b"late"),
            Err(SendError::UnknownConnection)
        );
        assert_eq!(queue.capacity(peer), ReliableCapacity::default());
        assert!(
            queue.lock().peers.is_empty(),
            "queued commands went with it"
        );
    }

    #[test]
    fn ingress_coalesces_latest_state_per_peer() {
        let hub = IngressHub::new(&ThreadedUdpConfig::default());
        let peer = cid(3);
        hub.connected(peer);
        assert!(hub.message(peer, Delivery::LatestState, b"old".to_vec()));
        assert!(hub.message(peer, Delivery::LatestState, b"new".to_vec()));

        let events = hub.drain();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ServerEvent::Message { .. }))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ServerEvent::Message {
                conn,
                delivery: Delivery::LatestState,
                payload,
            } if *conn == peer && payload == b"new"
        )));
    }

    #[test]
    fn ingress_reliable_overflow_isolated_to_noisy_peer() {
        let config = ThreadedUdpConfig {
            event_queue_messages: 1,
            ..ThreadedUdpConfig::default()
        };
        let hub = IngressHub::new(&config);
        let noisy = cid(4);
        let healthy = cid(5);
        hub.connected(noisy);
        hub.connected(healthy);
        assert!(hub.message(noisy, Delivery::RELIABLE_ORDERED, b"first".to_vec()));
        assert!(!hub.message(noisy, Delivery::RELIABLE_ORDERED, b"overflow".to_vec()));
        assert!(hub.message(healthy, Delivery::RELIABLE_ORDERED, b"healthy".to_vec()));

        assert!(hub.drain().iter().any(|event| matches!(
            event,
            ServerEvent::Message {
                conn,
                payload,
                ..
            } if *conn == healthy && payload == b"healthy"
        )));
    }
}
