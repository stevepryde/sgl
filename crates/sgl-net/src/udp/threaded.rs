//! Thread-owned native UDP session transport.
//!
//! The worker owns [`UdpServer`] and its socket. The simulation interacts only
//! with bounded queues, matching the WebSocket worker ownership model.
//!
//! The command queue is the only admission point the caller sees: `send`
//! refuses with [`SendError::WouldBlock`] once a lane's allowance is full,
//! and the worker moves a message to the endpoint only when the endpoint
//! has room for it on that lane, so a refusal never ends a connection and
//! nothing accepted is dropped. A lane's traffic is therefore buffered at
//! most twice: once here and once in the endpoint, each within the lane's
//! bounds.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{EndpointConfig, UdpServer};
use crate::{
    ConnectionId, Delivery, DisconnectReason, Lane, RELIABLE_LANES, ReliableCapacity,
    ReliableConfig, SendError, ServerEvent, ServerIo,
};

/// The worker's own settings; lane bounds come from the endpoint's
/// [`ReliableConfig`].
#[derive(Debug, Clone, Copy)]
pub struct ThreadedUdpConfig {
    /// How long the worker sleeps between turns.
    pub poll_interval: Duration,
}

impl Default for ThreadedUdpConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(2),
        }
    }
}

/// Every lane in order, starting at `first`.
fn lanes_from(first: usize) -> impl Iterator<Item = Lane> {
    (0..RELIABLE_LANES).map(move |offset| lane((first + offset) % RELIABLE_LANES))
}

fn lane(index: usize) -> Lane {
    Lane::new(u8::try_from(index).expect("lane index fits")).expect("index below RELIABLE_LANES")
}

/// One lane's messages and their bytes.
#[derive(Default)]
struct LaneQueue {
    messages: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl LaneQueue {
    fn push(&mut self, payload: Vec<u8>) {
        self.bytes += payload.len();
        self.messages.push_back(payload);
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        let payload = self.messages.pop_front()?;
        self.bytes -= payload.len();
        Some(payload)
    }
}

struct CommandState {
    /// Connections the worker has announced and not yet seen end.
    live: BTreeSet<ConnectionId>,
    peers: BTreeMap<ConnectionId, PeerCommands>,
    disconnects: BTreeSet<ConnectionId>,
    stop_admission: bool,
    /// The lane that started the last turn; the next turn starts after it.
    last_first: Option<(ConnectionId, Lane)>,
}

#[derive(Default)]
struct PeerCommands {
    reliable: [LaneQueue; RELIABLE_LANES],
    unreliable: [LaneQueue; RELIABLE_LANES],
    latest: Option<Vec<u8>>,
}

impl PeerCommands {
    fn is_empty(&self) -> bool {
        self.latest.is_none()
            && self
                .reliable
                .iter()
                .chain(&self.unreliable)
                .all(|lane| lane.messages.is_empty())
    }

    fn backlogged(&self, lane: Lane) -> bool {
        !self.reliable[lane.index()].messages.is_empty()
            || !self.unreliable[lane.index()].messages.is_empty()
    }
}

/// A delivery's lane queue among `reliable` and `unreliable`, with its
/// message and byte bounds; `None` for latest state.
fn lane_queue<'a>(
    reliable: &'a mut [LaneQueue; RELIABLE_LANES],
    unreliable: &'a mut [LaneQueue; RELIABLE_LANES],
    config: &ReliableConfig,
    delivery: Delivery,
    inbound: bool,
) -> Option<(&'a mut LaneQueue, usize, usize)> {
    match delivery {
        Delivery::Reliable(lane) => {
            let bounds = &config.lanes[lane.index()];
            let (messages, bytes) = if inbound {
                (bounds.inbound_messages, bounds.inbound_bytes)
            } else {
                (bounds.outbound_messages, bounds.outbound_bytes)
            };
            Some((&mut reliable[lane.index()], messages, bytes))
        }
        Delivery::Unreliable(lane) => {
            let bounds = &config.lanes[lane.index()];
            Some((
                &mut unreliable[lane.index()],
                bounds.unreliable_messages,
                bounds.unreliable_bytes,
            ))
        }
        Delivery::LatestState => None,
    }
}

struct CommandQueue {
    state: Mutex<CommandState>,
    reliable: ReliableConfig,
}

struct PeerIngress {
    reliable: [LaneQueue; RELIABLE_LANES],
    unreliable: [LaneQueue; RELIABLE_LANES],
    latest: Option<Vec<u8>>,
    /// The lane the next drain starts with.
    next_lane: usize,
}

struct IngressState {
    lifecycle: VecDeque<ServerEvent>,
    peers: BTreeMap<ConnectionId, PeerIngress>,
    active: BTreeSet<ConnectionId>,
}

/// Shared worker-to-simulation ingress with independent quotas per peer and
/// lane.
///
/// A noisy peer can exhaust only its own lanes' allowances. Snapshot traffic
/// always occupies one overwrite slot, so a delayed simulation never builds a
/// stale-state backlog.
struct IngressHub {
    state: Mutex<IngressState>,
    reliable: ReliableConfig,
}

impl IngressHub {
    fn new(reliable: &ReliableConfig) -> Self {
        Self {
            state: Mutex::new(IngressState {
                lifecycle: VecDeque::new(),
                peers: BTreeMap::new(),
                active: BTreeSet::new(),
            }),
            reliable: reliable.clone(),
        }
    }

    fn connected(&self, conn: ConnectionId) {
        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        if state.active.insert(conn) {
            state.peers.insert(
                conn,
                PeerIngress {
                    reliable: Default::default(),
                    unreliable: Default::default(),
                    latest: None,
                    next_lane: 0,
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

    /// Returns false when this peer exceeded a lane's ingress allowance:
    /// its reliable inbound bounds, or its unreliable bounds. Nothing is
    /// dropped instead.
    fn message(&self, conn: ConnectionId, delivery: Delivery, payload: Vec<u8>) -> bool {
        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        let Some(peer) = state.peers.get_mut(&conn) else {
            return true;
        };
        let Some((queue, messages, bytes)) = lane_queue(
            &mut peer.reliable,
            &mut peer.unreliable,
            &self.reliable,
            delivery,
            true,
        ) else {
            peer.latest = Some(payload);
            return true;
        };
        if queue.messages.len() >= messages || queue.bytes + payload.len() > bytes {
            return false;
        }
        queue.push(payload);
        true
    }

    /// Surfaces lifecycle events, then up to a bounded number of reliable
    /// and unreliable messages per peer taken from its lanes in turn, then
    /// its latest state.
    fn drain(&self) -> Vec<ServerEvent> {
        const RELIABLE_PER_PEER_PER_POLL: usize = 32;

        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        let mut output: Vec<_> = state.lifecycle.drain(..).collect();
        for (&conn, peer) in &mut state.peers {
            let first = peer.next_lane;
            peer.next_lane = (first + 1) % RELIABLE_LANES;
            let mut taken = 0;
            let mut progressed = true;
            while progressed && taken < RELIABLE_PER_PEER_PER_POLL {
                progressed = false;
                for lane in lanes_from(first) {
                    for (queue, delivery) in [
                        (&mut peer.reliable, Delivery::Reliable(lane)),
                        (&mut peer.unreliable, Delivery::Unreliable(lane)),
                    ] {
                        if taken == RELIABLE_PER_PEER_PER_POLL {
                            break;
                        }
                        if let Some(payload) = queue[lane.index()].pop() {
                            output.push(ServerEvent::Message {
                                conn,
                                delivery,
                                payload,
                            });
                            taken += 1;
                            progressed = true;
                        }
                    }
                }
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
    fn new(reliable: &ReliableConfig) -> Self {
        Self {
            state: Mutex::new(CommandState {
                live: BTreeSet::new(),
                peers: BTreeMap::new(),
                disconnects: BTreeSet::new(),
                stop_admission: false,
                last_first: None,
            }),
            reliable: reliable.clone(),
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
            Delivery::Unreliable(_) => crate::MAX_UNRELIABLE_BYTES,
            Delivery::LatestState => crate::MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > max_bytes {
            return Err(SendError::PayloadTooLarge);
        }
        let peer = state.peers.entry(id).or_default();
        let Some((queue, messages, bytes)) = lane_queue(
            &mut peer.reliable,
            &mut peer.unreliable,
            &self.reliable,
            delivery,
            false,
        ) else {
            // One overwrite slot per peer keeps only current state.
            peer.latest = Some(payload.to_vec());
            return Ok(());
        };
        // Each lane owns its allowances; a full one refuses this send and
        // nothing else.
        if queue.messages.len() >= messages || queue.bytes + payload.len() > bytes {
            return Err(SendError::WouldBlock);
        }
        queue.push(payload.to_vec());
        Ok(())
    }

    fn capacity(&self, id: ConnectionId, lane: Lane) -> ReliableCapacity {
        let state = self.lock();
        if !state.live.contains(&id) {
            return ReliableCapacity::default();
        }
        let (messages, bytes) = state.peers.get(&id).map_or((0, 0), |peer| {
            let queue = &peer.reliable[lane.index()];
            (queue.messages.len(), queue.bytes)
        });
        let bounds = &self.reliable.lanes[lane.index()];
        ReliableCapacity::remaining(
            bounds.outbound_messages.saturating_sub(messages),
            bounds.outbound_bytes.saturating_sub(bytes),
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

    /// Moves queued commands to `endpoint` and returns the connections the
    /// caller disconnected, for the worker to close after the lock is
    /// released (closing flushes and sends datagrams).
    ///
    /// Each lane's messages move in order, reliable ones only once the
    /// endpoint reports room for them on that lane; that report is advisory,
    /// and unreliable room is not reported, so a message the endpoint still
    /// refuses stays at the front for a later turn. Every pass moves at most
    /// one reliable and one unreliable message per backlogged peer and lane,
    /// starting from the one after the last turn's first, so when a shared
    /// endpoint ceiling binds, freed room is shared between every backlogged
    /// peer and lane instead of going to the lowest ids.
    fn apply<S: ServerIo>(&self, endpoint: &mut S) -> Vec<ConnectionId> {
        let mut state = self.lock();
        if std::mem::take(&mut state.stop_admission) {
            endpoint.stop_admission();
        }
        let disconnects: Vec<_> = std::mem::take(&mut state.disconnects).into_iter().collect();
        let backlogged: Vec<_> = state
            .peers
            .iter()
            .flat_map(|(&conn, peer)| {
                lanes_from(0)
                    .filter(|&lane| peer.backlogged(lane))
                    .map(move |lane| (conn, lane))
            })
            .collect();
        let split = state.last_first.map_or(0, |last| {
            backlogged.partition_point(|&stream| stream <= last)
        });
        let mut order = backlogged[split..].to_vec();
        order.extend_from_slice(&backlogged[..split]);
        if let Some(&first) = order.first() {
            state.last_first = Some(first);
        }

        let mut gone = BTreeSet::new();
        let mut blocked = BTreeSet::new();
        loop {
            let mut progressed = false;
            for &(conn, lane) in &order {
                for delivery in [Delivery::Reliable(lane), Delivery::Unreliable(lane)] {
                    if gone.contains(&conn) || blocked.contains(&(conn, delivery)) {
                        continue;
                    }
                    let peer = state.peers.get_mut(&conn).expect("ordered from peers");
                    let queue = match delivery {
                        Delivery::Unreliable(_) => &mut peer.unreliable[lane.index()],
                        _ => &mut peer.reliable[lane.index()],
                    };
                    let Some(payload) = queue.messages.front() else {
                        continue;
                    };
                    if let Delivery::Reliable(_) = delivery {
                        let capacity = endpoint.capacity(conn, lane);
                        if capacity.messages == 0 || capacity.bytes < payload.len() {
                            blocked.insert((conn, delivery));
                            continue;
                        }
                    }
                    match endpoint.send(conn, delivery, payload) {
                        // Admission already bounded the size, so a payload
                        // the endpoint calls too large is dropped rather than
                        // left to wedge the lane.
                        Ok(()) | Err(SendError::PayloadTooLarge) => {
                            queue.pop();
                            progressed = true;
                        }
                        Err(SendError::WouldBlock) => {
                            blocked.insert((conn, delivery));
                        }
                        Err(SendError::UnknownConnection | SendError::Disconnected) => {
                            gone.insert(conn);
                        }
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        let peers: Vec<_> = state.peers.keys().copied().collect();
        for conn in peers {
            if gone.contains(&conn) {
                continue;
            }
            let peer = state.peers.get_mut(&conn).expect("listed from peers");
            if let Some(payload) = peer.latest.take()
                && matches!(
                    endpoint.send(conn, Delivery::LatestState, &payload),
                    Err(SendError::UnknownConnection | SendError::Disconnected)
                )
            {
                gone.insert(conn);
            }
        }
        // A connection the endpoint no longer knows: its `Disconnected`
        // event reaches the caller through ingress.
        for conn in gone {
            state.live.remove(&conn);
            state.peers.remove(&conn);
        }
        state.peers.retain(|_, peer| !peer.is_empty());
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
    /// The worker's queues take their lane bounds from
    /// `net_config.reliable`.
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
        let reliable = net_config.reliable.clone();
        let endpoint = UdpServer::bind(addr, net_config)?;
        let local_addr = endpoint.local_addr()?;
        let commands = Arc::new(CommandQueue::new(&reliable));
        let worker_commands = Arc::clone(&commands);
        let ingress = Arc::new(IngressHub::new(&reliable));
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

    fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
        self.commands.capacity(conn, lane)
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

    for conn in commands.apply(endpoint) {
        endpoint.disconnect(conn, now_ms);
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
        let commands = CommandQueue::new(&ReliableConfig::DEFAULT);
        let ingress = IngressHub::new(&ReliableConfig::DEFAULT);
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
        for payload in [b"u1", b"u2"] {
            commands
                .send(SOLO_CONNECTION, Delivery::Unreliable(lane(2)), payload)
                .unwrap();
        }
        worker_tick(&mut server, &commands, &ingress, 5);
        let received = client.poll(5);
        assert_eq!(
            payloads(&received, Delivery::RELIABLE_ORDERED),
            vec![b"one".to_vec(), b"two".to_vec()]
        );
        assert_eq!(
            payloads(&received, Delivery::Unreliable(lane(2))),
            vec![b"u1".to_vec(), b"u2".to_vec()]
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
        let config = lanes(|lane| lane.inbound_messages = 2);
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

    /// Every lane at the defaults, then changed by `edit`.
    fn lanes(edit: impl Fn(&mut crate::LaneConfig)) -> ReliableConfig {
        let mut config = ReliableConfig::DEFAULT;
        config.lanes.iter_mut().for_each(edit);
        config
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
        let config = lanes(|lane| lane.outbound_messages = 50);
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
            commands.capacity(SOLO_CONNECTION, Lane::DEFAULT),
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
                        assert_eq!(
                            commands.capacity(SOLO_CONNECTION, Lane::DEFAULT).messages,
                            0
                        );
                        break;
                    }
                    Err(error) => panic!("refused with {error:?}"),
                }
            }
        }
        assert_eq!(received, (0..60).collect::<Vec<_>>());
    }

    /// An endpoint whose connections share one ceiling: it admits
    /// `per_tick` reliable messages between two polls across every
    /// connection and lane, and records what each connection received on
    /// each lane, in order.
    struct SharedCeiling {
        per_tick: usize,
        admitted: usize,
        announced: bool,
        received: BTreeMap<(ConnectionId, Lane), Vec<u32>>,
    }

    impl ServerIo for SharedCeiling {
        fn poll(&mut self, _now_ms: u64) -> Vec<ServerEvent> {
            self.admitted = 0;
            if std::mem::replace(&mut self.announced, true) {
                return Vec::new();
            }
            let conns: BTreeSet<_> = self.received.keys().map(|&(conn, _)| conn).collect();
            conns
                .into_iter()
                .map(|conn| ServerEvent::Connected { conn })
                .collect()
        }

        fn send(
            &mut self,
            conn: ConnectionId,
            delivery: Delivery,
            payload: &[u8],
        ) -> Result<(), SendError> {
            let Delivery::Reliable(lane) = delivery else {
                return Ok(());
            };
            let received = self
                .received
                .get_mut(&(conn, lane))
                .ok_or(SendError::UnknownConnection)?;
            if self.admitted >= self.per_tick {
                return Err(SendError::WouldBlock);
            }
            self.admitted += 1;
            received.push(u32::from_le_bytes(payload.try_into().unwrap()));
            Ok(())
        }

        fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
            if !self.received.contains_key(&(conn, lane)) {
                return ReliableCapacity::default();
            }
            ReliableCapacity::remaining(self.per_tick - self.admitted, usize::MAX)
        }

        fn flush(&mut self, _now_ms: u64) {}

        fn disconnect(&mut self, _conn: ConnectionId, _now_ms: u64) {}

        fn stop_admission(&mut self) {}
    }

    /// Defect (#267 review, #268): freed room under a binding shared ceiling
    /// going to the lowest connection id or lane every turn, starving a
    /// backlogged peer or lane with a higher one. Oracle: equal shares for
    /// equally backlogged streams — with two peers each keeping two lanes'
    /// command queues full and three slots per tick, every stream receives
    /// its messages in order and any two counts differ by at most one tick's
    /// worth.
    #[test]
    fn a_binding_shared_ceiling_is_shared_between_backlogged_peers_and_lanes() {
        let config = lanes(|lane| lane.outbound_messages = 16);
        let commands = CommandQueue::new(&config);
        let ingress = IngressHub::new(&config);
        let streams = [cid(1), cid(9)]
            .into_iter()
            .flat_map(|conn| [(conn, lane(0)), (conn, lane(2))]);
        let mut endpoint = SharedCeiling {
            per_tick: 3,
            admitted: 0,
            announced: false,
            received: streams.clone().map(|stream| (stream, Vec::new())).collect(),
        };
        let mut next: BTreeMap<_, u32> = streams.map(|stream| (stream, 0)).collect();
        for tick in 0..40 {
            worker_tick(&mut endpoint, &commands, &ingress, tick);
            // Keep every stream backlogged: refill each lane's command queue.
            for (&(conn, lane), index) in &mut next {
                while commands
                    .send(conn, Delivery::Reliable(lane), &index.to_le_bytes())
                    .is_ok()
                {
                    *index += 1;
                }
                assert_eq!(commands.capacity(conn, lane).messages, 0, "kept saturated");
            }
        }
        for (stream, got) in &endpoint.received {
            assert!(
                got.iter().zip(0..).all(|(&a, b)| a == b),
                "{stream:?} in order, once: {got:?}"
            );
        }
        let counts: Vec<_> = endpoint.received.values().map(Vec::len).collect();
        let (least, most) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
        assert!(*least > 0, "every stream made progress: {counts:?}");
        assert!(
            most - least <= endpoint.per_tick,
            "unfair split: {counts:?}"
        );
    }

    /// Defect: a peer's full allowance refusing another peer, or a refusal
    /// that schedules a disconnect. Oracle: per-peer bounds (netcode.md 11).
    #[test]
    fn a_full_command_allowance_refuses_only_its_own_peer_and_lane() {
        let config = lanes(|lane| {
            lane.outbound_messages = 1;
            lane.unreliable_messages = 1;
        });
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
        queue
            .send(noisy, Delivery::Reliable(lane(1)), b"another lane")
            .expect("a full lane leaves the peer's other lanes admitting");
        queue
            .send(noisy, Delivery::Unreliable(lane(0)), b"unreliable")
            .expect("the unreliable queue is separate");
        assert_eq!(
            queue.send(
                noisy,
                Delivery::Unreliable(lane(0)),
                &[0; crate::MAX_UNRELIABLE_BYTES + 1]
            ),
            Err(SendError::PayloadTooLarge)
        );
        assert_eq!(
            queue.send(noisy, Delivery::Unreliable(lane(0)), b"full"),
            Err(SendError::WouldBlock)
        );
        let state = queue.lock();
        assert!(state.disconnects.is_empty());
        assert!(state.live.contains(&noisy) && state.live.contains(&healthy));
        drop(state);

        // The byte allowance is exact: the lane admits up to its bound.
        let bound = crate::MAX_RELIABLE_MESSAGE_BYTES;
        let queue = CommandQueue::new(&lanes(|lane| lane.outbound_bytes = bound));
        queue.connected(noisy);
        queue
            .send(noisy, Delivery::RELIABLE_ORDERED, &vec![0; bound - 4])
            .unwrap();
        assert_eq!(queue.capacity(noisy, Lane::DEFAULT).bytes, 4);
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
        let queue = CommandQueue::new(&ReliableConfig::DEFAULT);
        let peer = cid(3);
        assert_eq!(
            queue.send(peer, Delivery::RELIABLE_ORDERED, b"early"),
            Err(SendError::UnknownConnection)
        );
        assert_eq!(
            queue.capacity(peer, Lane::DEFAULT),
            ReliableCapacity::default()
        );
        queue.connected(peer);
        queue
            .send(peer, Delivery::RELIABLE_ORDERED, b"queued")
            .unwrap();
        assert!(queue.capacity(peer, Lane::DEFAULT).messages > 0);
        queue.ended(peer);
        assert_eq!(
            queue.send(peer, Delivery::RELIABLE_ORDERED, b"late"),
            Err(SendError::UnknownConnection)
        );
        assert_eq!(
            queue.capacity(peer, Lane::DEFAULT),
            ReliableCapacity::default()
        );
        assert!(
            queue.lock().peers.is_empty(),
            "queued commands went with it"
        );
    }

    #[test]
    fn ingress_coalesces_latest_state_per_peer() {
        let hub = IngressHub::new(&ReliableConfig::DEFAULT);
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
        let hub = IngressHub::new(&lanes(|lane| lane.inbound_messages = 1));
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
