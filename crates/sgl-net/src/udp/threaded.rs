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
//!
//! Inbound is paced the same way. The worker polls its endpoint within the
//! room each lane's ingress has left under `inbound_messages` and
//! `inbound_bytes`, so a lane whose ingress is full holds its next message
//! in the endpoint's window: the endpoint stops acknowledging that lane and
//! reports it held, the peer's window closes and its `send` eventually
//! returns `WouldBlock`, and the lane resumes once the caller's `poll`
//! drains the ingress. A held fragment is not retransmitted, and both ends
//! keep exchanging keepalives, so a healthy peer is slowed, never
//! disconnected, however late the caller polls.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::session::server_events;
use super::{DatagramTransport, Endpoint, EndpointConfig, UdpServer};
use crate::lanes::InboundUsage;
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

/// The server a worker drives: one whose poll delivers on each lane only
/// what fits beside the messages its ingress still holds.
trait WorkerEndpoint: ServerIo {
    /// [`ServerIo::poll`] with `holding(conn, lane)` still waiting for the
    /// caller.
    fn poll_within(
        &mut self,
        now_ms: u64,
        holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
    ) -> Vec<ServerEvent>;
}

fn endpoint_poll_within<T: DatagramTransport>(
    endpoint: &mut Endpoint<T>,
    now_ms: u64,
    holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
) -> Vec<ServerEvent> {
    server_events(endpoint.poll_within(now_ms, |peer, lane| {
        ConnectionId::from_raw(peer).map_or_else(InboundUsage::default, |conn| holding(conn, lane))
    }))
}

impl WorkerEndpoint for UdpServer {
    fn poll_within(
        &mut self,
        now_ms: u64,
        holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
    ) -> Vec<ServerEvent> {
        endpoint_poll_within(self.endpoint_mut(), now_ms, holding)
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

struct CommandQueue {
    state: Mutex<CommandState>,
    reliable: ReliableConfig,
}

struct PeerIngress {
    reliable: [LaneQueue; RELIABLE_LANES],
    /// Each reliable queue against its lane's inbound bounds.
    reliable_usage: [InboundUsage; RELIABLE_LANES],
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
                    reliable_usage: Default::default(),
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

    /// Each live peer's reliable messages waiting for the caller, per lane:
    /// what the worker's next endpoint poll delivers beside.
    fn holding(&self) -> BTreeMap<ConnectionId, [InboundUsage; RELIABLE_LANES]> {
        let state = self.state.lock().expect("UDP ingress hub poisoned");
        state
            .peers
            .iter()
            .map(|(&conn, peer)| (conn, peer.reliable_usage))
            .collect()
    }

    /// Queues one received message. A reliable one fits its lane's inbound
    /// bounds: the endpoint delivered it within the room [`Self::holding`]
    /// reported, and only the caller's drain has changed the queue since.
    /// An unreliable one makes room by dropping the lane's oldest unpolled
    /// unreliable messages, as a full UDP socket buffer would, and only one
    /// larger than the whole queue (which the endpoint's cap rules out) is
    /// refused.
    fn message(
        &self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: Vec<u8>,
    ) -> Result<(), DisconnectReason> {
        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        let Some(peer) = state.peers.get_mut(&conn) else {
            return Ok(());
        };
        match delivery {
            Delivery::Reliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let usage = &mut peer.reliable_usage[lane.index()];
                debug_assert!(
                    usage.admits(payload.len(), bounds),
                    "the endpoint delivered past the ingress's room"
                );
                usage.add(payload.len(), bounds);
                peer.reliable[lane.index()].push(payload);
            }
            Delivery::Unreliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let (messages, bytes) = (bounds.unreliable_messages, bounds.unreliable_bytes);
                if payload.len() > bytes {
                    return Err(DisconnectReason::ProtocolViolation);
                }
                let queue = &mut peer.unreliable[lane.index()];
                while queue.messages.len() >= messages || queue.bytes + payload.len() > bytes {
                    queue.pop();
                }
                queue.push(payload);
            }
            Delivery::LatestState => peer.latest = Some(payload),
        }
        Ok(())
    }

    /// Surfaces lifecycle events, then up to a bounded number of reliable
    /// and unreliable messages per peer taken from its lanes in turn, then
    /// its latest state.
    fn drain(&self) -> Vec<ServerEvent> {
        // Shared across a peer's lanes and both classes: the sustainable
        // per-poll rate above which its unreliable messages are shed.
        const LANE_MESSAGES_PER_PEER_PER_POLL: usize = 32;

        let mut state = self.state.lock().expect("UDP ingress hub poisoned");
        let mut output: Vec<_> = state.lifecycle.drain(..).collect();
        for (&conn, peer) in &mut state.peers {
            let first = peer.next_lane;
            peer.next_lane = (first + 1) % RELIABLE_LANES;
            let mut taken = 0;
            let mut progressed = true;
            while progressed && taken < LANE_MESSAGES_PER_PEER_PER_POLL {
                progressed = false;
                for lane in lanes_from(first) {
                    for (queue, delivery) in [
                        (&mut peer.reliable, Delivery::Reliable(lane)),
                        (&mut peer.unreliable, Delivery::Unreliable(lane)),
                    ] {
                        if taken == LANE_MESSAGES_PER_PEER_PER_POLL {
                            break;
                        }
                        if let Some(payload) = queue[lane.index()].pop() {
                            if let Delivery::Reliable(_) = delivery {
                                peer.reliable_usage[lane.index()]
                                    .remove(payload.len(), &self.reliable.lanes[lane.index()]);
                            }
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
            Delivery::Reliable(_) => self.reliable.max_message_bytes,
            Delivery::Unreliable(_) => crate::MAX_UNRELIABLE_BYTES,
            Delivery::LatestState => crate::MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > max_bytes {
            return Err(SendError::PayloadTooLarge);
        }
        let peer = state.peers.entry(id).or_default();
        // Each lane owns its allowances; a full one refuses this send and
        // nothing else.
        match delivery {
            Delivery::Reliable(lane) => {
                let queue = &mut peer.reliable[lane.index()];
                if !self.reliable.lanes[lane.index()]
                    .outbound_admits((queue.messages.len(), queue.bytes), payload.len())
                {
                    return Err(SendError::WouldBlock);
                }
                queue.push(payload.to_vec());
            }
            Delivery::Unreliable(lane) => {
                let bounds = &self.reliable.lanes[lane.index()];
                let queue = &mut peer.unreliable[lane.index()];
                if queue.messages.len() >= bounds.unreliable_messages
                    || queue.bytes + payload.len() > bounds.unreliable_bytes
                {
                    return Err(SendError::WouldBlock);
                }
                queue.push(payload.to_vec());
            }
            // One overwrite slot per peer keeps only current state.
            Delivery::LatestState => peer.latest = Some(payload.to_vec()),
        }
        Ok(())
    }

    fn capacity(&self, id: ConnectionId, lane: Lane) -> ReliableCapacity {
        let state = self.lock();
        if !state.live.contains(&id) {
            return ReliableCapacity::default();
        }
        let held = state.peers.get(&id).map_or((0, 0), |peer| {
            let queue = &peer.reliable[lane.index()];
            (queue.messages.len(), queue.bytes)
        });
        self.reliable.lanes[lane.index()].outbound_capacity(held, self.reliable.max_message_bytes)
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
///
/// Each lane's `inbound_messages` and `inbound_bytes` bound what waits for
/// [`ServerIo::poll`]; a peer that sends more between two polls is slowed
/// until the poll makes room, not disconnected.
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
/// ingress queues, within the room they have left, apply the simulation's
/// queued commands to the endpoint, then flush. The thread calls this on
/// its own clock; tests drive it on theirs.
fn worker_tick<S: WorkerEndpoint>(
    endpoint: &mut S,
    commands: &CommandQueue,
    ingress: &IngressHub,
    now_ms: u64,
) {
    let holding = ingress.holding();
    let holding = |conn: ConnectionId, lane: Lane| {
        holding
            .get(&conn)
            .map_or_else(InboundUsage::default, |usage| usage[lane.index()])
    };
    for event in endpoint.poll_within(now_ms, &holding) {
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
                let Err(reason) = ingress.message(conn, delivery, payload) else {
                    continue;
                };
                endpoint.disconnect(conn, now_ms);
                commands.ended(conn);
                ingress.disconnected(conn, reason);
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
    use crate::udp::simulated::{SimulatedConfig, SimulatedNetwork};
    use crate::udp::{ClientEndpoint, ServerEndpoint};
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

    /// Unreliable messages past a lane's ingress allowance shed the oldest
    /// unpolled ones and the peer stays.
    #[test]
    fn worker_tick_sheds_unpolled_unreliable_ingress_and_keeps_the_peer() {
        let config = lanes(|lane| lane.unreliable_messages = 2);
        let commands = CommandQueue::new(&config);
        let ingress = IngressHub::new(&config);
        let (mut client, mut server) = memory_duplex();
        assert!(
            matches!(&client.poll(0)[..], [ClientEvent::Connected]),
            "the memory client is connected from the start"
        );
        for payload in [b"u1", b"u2", b"u3", b"u4"] {
            client.send(Delivery::Unreliable(lane(1)), payload).unwrap();
        }
        client.flush(0);
        worker_tick(&mut server, &commands, &ingress, 0);
        let events = ingress.drain();
        assert!(
            matches!(&events[..], [ServerEvent::Connected { conn }, ..] if *conn == SOLO_CONNECTION),
            "{events:?}"
        );
        let unreliable: Vec<_> = events[1..]
            .iter()
            .map(|event| match event {
                ServerEvent::Message {
                    delivery: Delivery::Unreliable(_),
                    payload,
                    ..
                } => payload.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(unreliable, [b"u3".to_vec(), b"u4".to_vec()]);
        assert!(
            !client
                .poll(1)
                .iter()
                .any(|event| matches!(event, ClientEvent::Disconnected { .. })),
            "the peer stays"
        );
    }

    impl<T: DatagramTransport> WorkerEndpoint for ServerEndpoint<T> {
        fn poll_within(
            &mut self,
            now_ms: u64,
            holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
        ) -> Vec<ServerEvent> {
            endpoint_poll_within(self.endpoint_mut(), now_ms, holding)
        }
    }

    /// The memory transport delivers whole messages with no window to
    /// hold, so these tests keep within the ingress bounds.
    impl WorkerEndpoint for crate::MemoryServerIo {
        fn poll_within(
            &mut self,
            now_ms: u64,
            _holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
        ) -> Vec<ServerEvent> {
            self.poll(now_ms)
        }
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

    impl WorkerEndpoint for Throttled {
        fn poll_within(
            &mut self,
            now_ms: u64,
            _holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
        ) -> Vec<ServerEvent> {
            self.poll(now_ms)
        }
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

    impl WorkerEndpoint for SharedCeiling {
        fn poll_within(
            &mut self,
            now_ms: u64,
            _holding: &dyn Fn(ConnectionId, Lane) -> InboundUsage,
        ) -> Vec<ServerEvent> {
            self.poll(now_ms)
        }
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

    /// Defect (#269): the threaded server refusing, or never moving, a
    /// message larger than its lane's byte allowance (a cap other than the
    /// configured one, admission without the one-message rule, a worker
    /// waiting for room the endpoint never reports). Oracle: the one-message
    /// rule of netcode.md 11 — a 1 MiB message through a 64 KiB lane arrives
    /// intact.
    #[test]
    fn a_message_past_the_lane_allowance_crosses_the_worker() {
        let mut config = lanes(|lane| lane.outbound_bytes = 64 * 1024);
        config.max_message_bytes = 1 << 20;
        let commands = CommandQueue::new(&config);
        let ingress = IngressHub::new(&config);
        let (mut client, mut server) = crate::memory_duplex_with(&config).unwrap();
        worker_tick(&mut server, &commands, &ingress, 0);
        ingress.drain();
        let large: Vec<u8> = (0..1_u32 << 20).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            commands.send(
                SOLO_CONNECTION,
                Delivery::Reliable(lane(1)),
                &vec![0; (1 << 20) + 1]
            ),
            Err(SendError::PayloadTooLarge)
        );
        commands
            .send(SOLO_CONNECTION, Delivery::Reliable(lane(1)), &large)
            .expect("an idle lane takes one message of up to the cap");
        let mut received = Vec::new();
        for tick in 1..4 {
            worker_tick(&mut server, &commands, &ingress, tick);
            received.extend(payloads(&client.poll(tick), Delivery::Reliable(lane(1))));
        }
        assert!(received == [large], "the message crossed whole, once");
    }

    /// Client `tag`'s `len`-byte message `index`: tagged and indexed, the
    /// rest seeded bytes so a misplaced fragment cannot match.
    fn paced_message(tag: u8, index: u32, len: usize) -> Vec<u8> {
        let mut state = u64::from(index) << 8 | u64::from(tag) | 1 << 40;
        let mut payload: Vec<u8> = (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect();
        payload[0] = tag;
        payload[1..5].copy_from_slice(&index.to_le_bytes());
        payload
    }

    /// Defect: a threaded server that disconnects a healthy sender because
    /// its caller polls slowly — its ingress overflowing, or either end
    /// timing out while the caller stalls — or that loses, duplicates or reorders
    /// messages while pacing, or holds one peer back for another's full
    /// ingress. Oracle: netcode.md 11 (a receiver that polls slowly makes
    /// the sender slower, not disconnected; other connections are never
    /// affected). Over in-order and lossy seeded networks, to a server
    /// whose caller polls every 100 ms and once not at all for 3 s (longer
    /// than the 1 s timeout), one
    /// client sends 60 messages as fast as `send` admits, every third
    /// larger than the lane's 4 KiB inbound bound, and another sends five
    /// small ones 200 ms apart during the stall, one more than its lane
    /// holds, so its last waits as the newest fragment. The fast sender is
    /// refused with `WouldBlock`; nobody is disconnected; every message
    /// arrives once, in order; and the first poll after the stall finds
    /// the quiet client's first four messages, not held back by the fast
    /// client.
    #[test]
    fn a_slowly_polled_server_paces_a_fast_sender_without_disconnecting_it() {
        let in_order = SimulatedConfig {
            one_way_latency_ms: 20,
            jitter_ms: 0,
            loss_per_10k: 0,
            duplicate_per_10k: 0,
            reorder_per_10k: 0,
            ..SimulatedConfig::default()
        };
        let lossy = SimulatedConfig {
            one_way_latency_ms: 20,
            jitter_ms: 5,
            loss_per_10k: 200,
            duplicate_per_10k: 100,
            reorder_per_10k: 500,
            reorder_extra_ms: 20,
            ..SimulatedConfig::default()
        };
        for network in [in_order, lossy] {
            pace_a_fast_sender(network);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn pace_a_fast_sender(network: SimulatedConfig) {
        const TICK_MS: u64 = 2;
        const POLL_MS: u64 = 100;
        const STALL_MS: std::ops::Range<u64> = 1_000..4_000;
        const MESSAGES: u32 = 60;
        const QUIET_MESSAGES: u32 = 5;
        const QUIET_GAP_MS: u64 = 200;
        let paced = lane(1);
        let server_addr = SocketAddr::from(([10, 0, 0, 1], 7_000));
        let mut config = EndpointConfig {
            timeout_ms: 1_000,
            keepalive_ms: 100,
            ..EndpointConfig::new(*b"PCD")
        };
        let bounds = &mut config.reliable.lanes[paced.index()];
        bounds.inbound_messages = 4;
        bounds.inbound_bytes = 4 * 1024;
        bounds.outbound_bytes = 32 * 1024;
        let network = SimulatedNetwork::new(network, 0x7417).unwrap();
        let mut server =
            ServerEndpoint::new(network.transport(server_addr), config.clone(), [7; 32]).unwrap();
        let commands = CommandQueue::new(&config.reliable);
        let ingress = IngressHub::new(&config.reliable);
        let mut clients: Vec<_> = [(b'F', 2), (b'Q', 3)]
            .into_iter()
            .map(|(tag, host)| {
                let addr = SocketAddr::from(([10, 0, 0, host], 40_000));
                let client = ClientEndpoint::connect(
                    network.transport(addr),
                    server_addr,
                    config.clone(),
                    0,
                    u64::from(tag),
                )
                .unwrap();
                (tag, client, false)
            })
            .collect();

        let mut received: BTreeMap<u8, Vec<Vec<u8>>> = BTreeMap::new();
        let quiet_messages: Vec<_> = (0..QUIET_MESSAGES)
            .map(|index| paced_message(b'Q', index, 200))
            .collect();
        // Every third larger than the lane's inbound byte bound.
        let fast_message =
            |index| paced_message(b'F', index, [10 * 1024, 200, 200][index as usize % 3]);
        let (mut next, mut refused, mut quiet_sent) = (0, 0, 0);
        let mut now = 0;
        while received.get(&b'F').map_or(0, Vec::len) < MESSAGES as usize
            || received.get(&b'Q').map_or(0, Vec::len) < quiet_messages.len()
        {
            now += TICK_MS;
            assert!(
                now < 60_000,
                "stalled with {:?} received",
                received.values().map(Vec::len).collect::<Vec<_>>()
            );
            for (tag, client, connected) in &mut clients {
                for event in client.poll(now) {
                    match event {
                        ClientEvent::Connected => *connected = true,
                        other => panic!("client {}: {other:?} at {now}", *tag as char),
                    }
                }
            }
            let [(_, fast, fast_connected), (_, quiet, quiet_connected)] = &mut clients[..] else {
                unreachable!()
            };
            while *fast_connected && next < MESSAGES {
                match fast.send(Delivery::Reliable(paced), &fast_message(next)) {
                    Ok(()) => next += 1,
                    Err(SendError::WouldBlock) => {
                        refused += 1;
                        break;
                    }
                    Err(error) => panic!("refused with {error:?}"),
                }
            }
            if let Some(message) = quiet_messages.get(quiet_sent)
                && now == STALL_MS.start + QUIET_GAP_MS * quiet_sent as u64
            {
                assert!(*quiet_connected, "connected before the stall");
                quiet.send(Delivery::Reliable(paced), message).unwrap();
                quiet_sent += 1;
            }
            fast.flush(now);
            quiet.flush(now);
            worker_tick(&mut server, &commands, &ingress, now);

            if now % POLL_MS != 0 || STALL_MS.contains(&now) {
                continue;
            }
            for event in ingress.drain() {
                match event {
                    ServerEvent::Connected { .. } => {}
                    ServerEvent::Message {
                        delivery, payload, ..
                    } if delivery == Delivery::Reliable(paced) => {
                        received.entry(payload[0]).or_default().push(payload);
                    }
                    other => panic!("server: {other:?} at {now}"),
                }
            }
            if now == STALL_MS.end {
                assert_eq!(
                    received.get(&b'Q').map_or(0, Vec::len),
                    4,
                    "the quiet client's own room"
                );
            }
        }
        assert!(refused > 0, "the sender was never paced");
        assert_eq!(
            received[&b'F'],
            (0..MESSAGES).map(fast_message).collect::<Vec<_>>()
        );
        assert_eq!(received[&b'Q'], quiet_messages);
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
        let bound = crate::DEFAULT_RELIABLE_MESSAGE_BYTES;
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
        assert_eq!(
            hub.message(peer, Delivery::LatestState, b"old".to_vec()),
            Ok(())
        );
        assert_eq!(
            hub.message(peer, Delivery::LatestState, b"new".to_vec()),
            Ok(())
        );

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

    /// Defect (design §12 review): an unpolled receiver closing the peer for
    /// unreliable traffic, dropping the newest instead of the oldest, or
    /// letting the queue's byte count drift as it sheds. Oracle: the
    /// UDP-socket rule — past either bound, the oldest unpolled unreliable
    /// messages go and the newest that fit arrive in order.
    #[test]
    fn ingress_sheds_the_oldest_unreliable_messages_and_keeps_the_peer() {
        let peer = cid(6);
        let unreliable = |hub: &IngressHub| -> Vec<Vec<u8>> {
            hub.drain()
                .into_iter()
                .filter_map(|event| match event {
                    ServerEvent::Message {
                        delivery: Delivery::Unreliable(_),
                        payload,
                        ..
                    } => Some(payload),
                    _ => None,
                })
                .collect()
        };

        let hub = IngressHub::new(&lanes(|lane| lane.unreliable_messages = 3));
        hub.connected(peer);
        for index in 0..7u8 {
            assert_eq!(
                hub.message(peer, Delivery::Unreliable(lane(2)), vec![index]),
                Ok(())
            );
        }
        assert_eq!(unreliable(&hub), [vec![4], vec![5], vec![6]]);

        let hub = IngressHub::new(&lanes(|lane| {
            lane.unreliable_bytes = crate::MAX_UNRELIABLE_BYTES;
        }));
        hub.connected(peer);
        for index in 0..5u8 {
            assert_eq!(
                hub.message(peer, Delivery::Unreliable(lane(2)), vec![index; 500]),
                Ok(())
            );
        }
        // Two 500-byte messages fit 1,168 bytes; the queue's count recovers.
        assert_eq!(unreliable(&hub), [vec![3; 500], vec![4; 500]]);
        assert_eq!(
            hub.message(peer, Delivery::Unreliable(lane(2)), vec![9; 1_168]),
            Ok(())
        );
        assert_eq!(unreliable(&hub), [vec![9; 1_168]]);
        assert_eq!(
            hub.message(peer, Delivery::Unreliable(lane(2)), vec![0; 1_169]),
            Err(DisconnectReason::ProtocolViolation)
        );
    }
}
