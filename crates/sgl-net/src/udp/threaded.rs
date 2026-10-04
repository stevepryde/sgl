//! Thread-owned native UDP session transport.
//!
//! The worker owns [`UdpServer`] and its socket. The simulation interacts only
//! with bounded queues, matching the WebSocket worker ownership model.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{EndpointConfig, UdpServer};
use crate::{ConnectionId, Delivery, DisconnectReason, SendError, ServerEvent, ServerIo};

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

enum Command {
    Send(ConnectionId, Delivery, Vec<u8>),
    Disconnect(ConnectionId),
    StopAdmission,
}

struct CommandState {
    peers: BTreeMap<ConnectionId, PeerCommands>,
    disconnects: BTreeSet<ConnectionId>,
    stop_admission: bool,
}

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
            Delivery::ReliableOrdered => {
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
                    delivery: Delivery::ReliableOrdered,
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
                peers: BTreeMap::new(),
                disconnects: BTreeSet::new(),
                stop_admission: false,
            }),
            max_messages: config.reliable_queue_messages,
            max_bytes: config.reliable_queue_bytes,
        }
    }

    fn send(&self, id: ConnectionId, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        let max_bytes = match delivery {
            Delivery::ReliableOrdered => crate::MAX_RELIABLE_MESSAGE_BYTES,
            Delivery::LatestState => crate::MAX_LATEST_STATE_BYTES,
        };
        if payload.len() > max_bytes {
            return Err(SendError::PayloadTooLarge);
        }
        let mut state = self.state.lock().expect("UDP command queue poisoned");
        if state.disconnects.contains(&id) {
            return Err(SendError::UnknownConnection);
        }
        let peer = state.peers.entry(id).or_insert_with(|| PeerCommands {
            reliable: VecDeque::new(),
            reliable_bytes: 0,
            latest: None,
        });
        match delivery {
            Delivery::ReliableOrdered => {
                if peer.reliable.len() >= self.max_messages
                    || peer.reliable_bytes.saturating_add(payload.len()) > self.max_bytes
                {
                    // Each peer owns its allowance. Overflow removes only the
                    // offender's pending traffic and schedules its disconnect.
                    state.peers.remove(&id);
                    state.disconnects.insert(id);
                    return Err(SendError::ReliableOverflow);
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

    fn disconnect(&self, id: ConnectionId) {
        let mut state = self.state.lock().expect("UDP command queue poisoned");
        state.peers.remove(&id);
        state.disconnects.insert(id);
    }

    fn stop_admission(&self) {
        self.state
            .lock()
            .expect("UDP command queue poisoned")
            .stop_admission = true;
    }

    fn drain(&self) -> Vec<Command> {
        const RELIABLE_PER_PEER_PER_DRAIN: usize = 32;

        let mut state = self.state.lock().expect("UDP command queue poisoned");
        let mut output = Vec::new();
        if std::mem::take(&mut state.stop_admission) {
            output.push(Command::StopAdmission);
        }
        output.extend(state.disconnects.iter().copied().map(Command::Disconnect));
        state.disconnects.clear();
        for (&id, peer) in &mut state.peers {
            for _ in 0..RELIABLE_PER_PEER_PER_DRAIN {
                let Some(payload) = peer.reliable.pop_front() else {
                    break;
                };
                peer.reliable_bytes -= payload.len();
                output.push(Command::Send(id, Delivery::ReliableOrdered, payload));
            }
            if let Some(payload) = peer.latest.take() {
                output.push(Command::Send(id, Delivery::LatestState, payload));
            }
        }
        state
            .peers
            .retain(|_, peer| !peer.reliable.is_empty() || peer.latest.is_some());
        output
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
            ServerEvent::Connected { conn } => ingress.connected(conn),
            ServerEvent::Disconnected { conn, reason } => {
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
                ingress.disconnected(conn, DisconnectReason::ReliableOverflow);
            }
        }
    }

    for command in commands.drain() {
        match command {
            Command::Send(conn, delivery, payload) => {
                if matches!(
                    endpoint.send(conn, delivery, &payload),
                    Err(SendError::ReliableOverflow)
                ) {
                    endpoint.disconnect(conn, now_ms);
                    ingress.disconnected(conn, DisconnectReason::ReliableOverflow);
                }
            }
            Command::Disconnect(conn) => {
                endpoint.disconnect(conn, now_ms);
                ingress.disconnected(conn, DisconnectReason::Local);
            }
            Command::StopAdmission => endpoint.stop_admission(),
        }
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
            .send(Delivery::ReliableOrdered, b"hello")
            .expect("queue hello");
        client.flush(0);
        worker_tick(&mut server, &commands, &ingress, 0);
        let events = ingress.drain();
        assert!(
            matches!(
                &events[..],
                [
                    ServerEvent::Connected { conn },
                    ServerEvent::Message { conn: from, delivery: Delivery::ReliableOrdered, payload },
                ] if *conn == SOLO_CONNECTION && *from == SOLO_CONNECTION && payload == b"hello"
            ),
            "{events:?}"
        );

        commands
            .send(SOLO_CONNECTION, Delivery::ReliableOrdered, b"one")
            .unwrap();
        commands
            .send(SOLO_CONNECTION, Delivery::ReliableOrdered, b"two")
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
            payloads(&received, Delivery::ReliableOrdered),
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
    /// endpoint and reported as `ReliableOverflow`; its buffered ingress is
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
            client.send(Delivery::ReliableOrdered, payload).unwrap();
        }
        client.flush(0);

        worker_tick(&mut server, &commands, &ingress, 0);
        let events = ingress.drain();
        assert!(
            matches!(
                &events[..],
                [
                    ServerEvent::Connected { conn },
                    ServerEvent::Disconnected { conn: gone, reason: DisconnectReason::ReliableOverflow },
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

    #[test]
    fn commands_preserve_reliable_and_coalesce_latest_per_peer() {
        let queue = CommandQueue::new(&ThreadedUdpConfig::default());
        let a = cid(1);
        let b = cid(2);
        queue.send(a, Delivery::ReliableOrdered, b"event").unwrap();
        queue.send(a, Delivery::LatestState, b"old").unwrap();
        queue.send(a, Delivery::LatestState, b"new").unwrap();
        queue.send(b, Delivery::LatestState, b"other").unwrap();
        let commands = queue.drain();
        assert_eq!(commands.len(), 3);
        assert!(matches!(
            &commands[0],
            Command::Send(_, Delivery::ReliableOrdered, payload) if payload == b"event"
        ));
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Send(id, Delivery::LatestState, payload) if *id == a && payload == b"new"
        )));
    }

    #[test]
    fn reliable_overflow_schedules_peer_disconnect() {
        let config = ThreadedUdpConfig {
            reliable_queue_messages: 1,
            ..ThreadedUdpConfig::default()
        };
        let queue = CommandQueue::new(&config);
        let peer = cid(7);
        queue
            .send(peer, Delivery::ReliableOrdered, b"first")
            .unwrap();
        assert_eq!(
            queue.send(peer, Delivery::ReliableOrdered, b"overflow"),
            Err(SendError::ReliableOverflow)
        );
        assert!(
            queue
                .drain()
                .iter()
                .any(|command| matches!(command, Command::Disconnect(id) if *id == peer))
        );
    }

    #[test]
    fn outbound_reliable_overflow_does_not_consume_another_peers_allowance() {
        let config = ThreadedUdpConfig {
            reliable_queue_messages: 1,
            ..ThreadedUdpConfig::default()
        };
        let queue = CommandQueue::new(&config);
        let noisy = cid(7);
        let healthy = cid(8);
        queue
            .send(noisy, Delivery::ReliableOrdered, b"first")
            .unwrap();
        assert_eq!(
            queue.send(noisy, Delivery::ReliableOrdered, b"overflow"),
            Err(SendError::ReliableOverflow)
        );
        queue
            .send(healthy, Delivery::ReliableOrdered, b"healthy")
            .unwrap();

        let commands = queue.drain();
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, Command::Disconnect(id) if *id == noisy))
        );
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Send(id, Delivery::ReliableOrdered, payload)
                if *id == healthy && payload == b"healthy"
        )));
        assert!(!commands.iter().any(|command| matches!(
            command,
            Command::Disconnect(id) if *id == healthy
        )));
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
        assert!(hub.message(noisy, Delivery::ReliableOrdered, b"first".to_vec()));
        assert!(!hub.message(noisy, Delivery::ReliableOrdered, b"overflow".to_vec()));
        assert!(hub.message(healthy, Delivery::ReliableOrdered, b"healthy".to_vec()));

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
