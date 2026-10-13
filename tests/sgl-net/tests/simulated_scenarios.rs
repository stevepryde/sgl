//! Acceptance scenarios over the seeded virtual network (netcode.md 8 and
//! acceptance): the session adapters in `udp::session` over `SimulatedNetwork`
//! endpoints, driven on virtual time with no sockets or sleeps. Every
//! assertion is about what a game observes: bytes received, event order,
//! errors returned, and the virtual instant at which lifecycle events fire.
#![allow(clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use sgl_net::RELIABLE_LANES;
use sgl_net::udp::simulated::{SimulatedConfig, SimulatedNetwork, SimulatedTransport};
use sgl_net::udp::{ClientEndpoint, EndpointConfig, MAX_RELIABLE_FRAGMENT_BYTES, ServerEndpoint};
use sgl_net::{
    ClientEvent, ClientIo, ConnectionId, DEFAULT_RELIABLE_MESSAGE_BYTES, Delivery, DenyReason,
    DisconnectReason, SendError, ServerEvent, ServerIo,
};

const TICK_MS: u64 = 16;
const SERVER: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7_000));

fn client_addr(index: u16) -> SocketAddr {
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, 40_000 + index).into()
}

fn clean() -> SimulatedConfig {
    SimulatedConfig {
        one_way_latency_ms: 5,
        jitter_ms: 0,
        loss_per_10k: 0,
        duplicate_per_10k: 0,
        reorder_per_10k: 0,
        reorder_extra_ms: 0,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k: [0; RELIABLE_LANES],
    }
}

/// One server and any number of clients on one virtual network, stepped
/// together on virtual time.
struct World {
    network: SimulatedNetwork,
    server: ServerEndpoint<SimulatedTransport>,
    clients: Vec<ClientEndpoint<SimulatedTransport>>,
    /// Virtual address of each client, indexed like `clients`.
    addrs: Vec<SocketAddr>,
    config: EndpointConfig,
    now: u64,
    next_nonce: u64,
}

impl World {
    fn new(network_config: SimulatedConfig, seed: u64, config: EndpointConfig) -> Self {
        let network = SimulatedNetwork::new(network_config, seed).expect("valid network config");
        let server =
            ServerEndpoint::new(network.transport(SERVER), config.clone(), [seed as u8; 32])
                .expect("server endpoint");
        Self {
            network,
            server,
            clients: Vec::new(),
            addrs: Vec::new(),
            config,
            now: 0,
            next_nonce: 1,
        }
    }

    fn connect(&mut self, index: u16) -> usize {
        self.next_nonce += 1;
        let client = ClientEndpoint::connect(
            self.network.transport(client_addr(index)),
            SERVER,
            self.config.clone(),
            self.now,
            self.next_nonce,
        )
        .expect("client endpoint");
        self.clients.push(client);
        self.addrs.push(client_addr(index));
        self.clients.len() - 1
    }

    /// Advance one tick: poll and flush every endpoint, returning the events.
    fn tick(&mut self) -> (Vec<ServerEvent>, Vec<Vec<ClientEvent>>) {
        self.now += TICK_MS;
        let server_events = self.server.poll(self.now);
        let client_events = self
            .clients
            .iter_mut()
            .map(|client| client.poll(self.now))
            .collect();
        self.server.flush(self.now);
        for client in &mut self.clients {
            client.flush(self.now);
        }
        (server_events, client_events)
    }

    /// The client (by index) behind a server-side connection id, resolved
    /// through the peer's virtual address.
    fn client_of(&self, conn: ConnectionId) -> usize {
        let addr = self
            .server
            .endpoint()
            .peer_addr(conn.raw())
            .expect("connection has a peer address");
        self.addrs
            .iter()
            .position(|a| *a == addr)
            .expect("peer address belongs to a client")
    }

    /// Tick until every client has reported `Connected`; returns the
    /// server-side connection id of each client, indexed like `clients`.
    fn connect_all(&mut self, deadline_ms: u64) -> Vec<ConnectionId> {
        let mut connected = vec![false; self.clients.len()];
        let mut ids = vec![None; self.clients.len()];
        while self.now < deadline_ms && (ids.contains(&None) || connected.contains(&false)) {
            let (server_events, client_events) = self.tick();
            for event in server_events {
                if let ServerEvent::Connected { conn } = event {
                    let index = self.client_of(conn);
                    ids[index] = Some(conn);
                }
            }
            for (i, events) in client_events.iter().enumerate() {
                if events.contains(&ClientEvent::Connected) {
                    connected[i] = true;
                }
            }
        }
        assert!(
            connected.iter().all(|c| *c),
            "not every client connected by {deadline_ms} ms"
        );
        ids.into_iter()
            .map(|id| id.expect("every client connected"))
            .collect()
    }
}

/// The reliable payload corpus: tiny, mid, exactly one fragment, one byte
/// over a fragment, and (once per stream) the largest message allowed.
fn message(index: u32) -> Vec<u8> {
    let size = match index % 5 {
        0 => 1,
        1 => 300,
        2 => MAX_RELIABLE_FRAGMENT_BYTES,
        3 => MAX_RELIABLE_FRAGMENT_BYTES + 1,
        _ => 17,
    };
    let mut payload = index.to_le_bytes().to_vec();
    payload.resize(size.max(4), index as u8);
    payload
}

fn index_of(payload: &[u8]) -> u32 {
    u32::from_le_bytes(payload[..4].try_into().unwrap())
}

/// A client streams `count` reliable messages (keeping at most `window`
/// unacknowledged so a slow link never trips the outbound cap) plus one
/// maximum-size message, and a latest-state value every tick. Returns the
/// reliable indices and latest values the server observed, in order, and
/// whether the stream completed before `deadline_ms`.
fn stream(
    world: &mut World,
    client: usize,
    count: u32,
    window: u32,
    deadline_ms: u64,
) -> (Vec<u32>, Vec<u32>, bool) {
    let total = count + 1;
    let (mut sent, mut received, mut latest) = (0u32, Vec::new(), Vec::new());
    let mut tick_counter = 0u32;
    while received.len() < total as usize && world.now < deadline_ms {
        let (server_events, _) = world.tick();
        for event in server_events {
            match event {
                ServerEvent::Message {
                    delivery: Delivery::Reliable(_),
                    payload,
                    ..
                } => received.push(index_of(&payload)),
                ServerEvent::Message {
                    delivery: Delivery::LatestState,
                    payload,
                    ..
                } => latest.push(index_of(&payload)),
                ServerEvent::Message {
                    delivery: Delivery::Unreliable(_),
                    ..
                } => panic!("nothing is sent unreliably here"),
                ServerEvent::Disconnected { reason, .. } => panic!("disconnected: {reason:?}"),
                ServerEvent::Connected { .. } => {}
            }
        }
        tick_counter += 1;
        world.clients[client]
            .send(Delivery::LatestState, &tick_counter.to_le_bytes())
            .expect("latest state is always accepted");
        if sent < total && sent.saturating_sub(received.len() as u32) < window {
            let payload = if sent == count {
                let mut big = sent.to_le_bytes().to_vec();
                big.resize(DEFAULT_RELIABLE_MESSAGE_BYTES, 0xA5);
                big
            } else {
                message(sent)
            };
            world.clients[client]
                .send(Delivery::RELIABLE_ORDERED, &payload)
                .expect("windowed sends never overflow");
            sent += 1;
        }
    }
    let complete = received.len() == total as usize;
    (received, latest, complete)
}

/// Defect: fragment reassembly under reorder and duplication, RTO growth
/// under loss, or a latest-state lane that regresses. Oracle: across every
/// network condition and seed, reliable delivery is exact and in order
/// (including one maximum-size message) and the latest state observed by
/// the server is strictly increasing.
#[test]
fn reliable_order_and_latest_progress_hold_across_the_network_matrix() {
    let mut runs = 0;
    for &loss in &[0u32, 500, 2_000] {
        for &reorder in &[0u32, 2_000] {
            for &duplicate in &[0u32, 1_000] {
                for &jitter in &[0u64, 40] {
                    for &latency in &[5u64, 100] {
                        for seed in 1..=3u64 {
                            let network = SimulatedConfig {
                                one_way_latency_ms: latency,
                                jitter_ms: jitter,
                                loss_per_10k: loss,
                                duplicate_per_10k: duplicate,
                                reorder_per_10k: reorder,
                                reorder_extra_ms: 40,
                                max_in_flight_datagrams: 4_096,
                                lane_loss_per_10k: [0; RELIABLE_LANES],
                            };
                            let label = format!(
                                "loss {loss} reorder {reorder} dup {duplicate} jitter {jitter} latency {latency} seed {seed}"
                            );
                            let mut world = World::new(network, seed, EndpointConfig::default());
                            let client = world.connect(1);
                            world.connect_all(20_000);
                            let deadline = world.now + 240_000;
                            let (received, latest, complete) =
                                stream(&mut world, client, 60, 8, deadline);
                            assert!(complete, "{label}: only {} of 61 messages", received.len());
                            assert_eq!(received, (0..61).collect::<Vec<_>>(), "{label}");
                            assert!(
                                latest.windows(2).all(|w| w[0] < w[1]),
                                "{label}: latest state regressed"
                            );
                            assert!(latest.len() > 10, "{label}: latest state starved");
                            runs += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(runs, 144);
}

/// Sends `next..total` to `send` in order until it refuses with
/// `WouldBlock`, retaining the refused index; any other error fails.
fn send_until_blocked(
    next: &mut u32,
    total: u32,
    mut send: impl FnMut(&[u8]) -> Result<(), SendError>,
) -> bool {
    while *next < total {
        match send(&message(*next)) {
            Ok(()) => *next += 1,
            Err(SendError::WouldBlock) => return true,
            Err(error) => panic!("send {next} failed with {error:?}"),
        }
    }
    false
}

/// Defect (#267): a saturated sender closed instead of refused, a refused
/// message lost or duplicated on retry, a per-peer bound leaking into the
/// shared budget, or a flooding peer starving the poll loop. Oracle: the
/// flooder and the server toward it are refused with `WouldBlock`, nobody
/// is disconnected, every stream — the flooder's both ways and the other
/// clients' — arrives exact and in order on the same server.
#[test]
fn a_saturated_client_is_refused_alone_and_its_streams_stay_exact() {
    const FLOOD: u32 = 400;
    let mut world = World::new(SimulatedConfig::default(), 7, EndpointConfig::default());
    let clients: Vec<usize> = (0..6).map(|i| world.connect(i)).collect();
    let ids = world.connect_all(20_000);
    let by_id: BTreeMap<ConnectionId, usize> = ids.iter().copied().zip(0..).collect();
    let flooder = clients[0];

    // Flood both ways without draining until each side is refused.
    let (mut up, mut down) = (0u32, 0u32);
    assert!(send_until_blocked(&mut up, FLOOD, |payload| {
        world.clients[flooder].send(Delivery::RELIABLE_ORDERED, payload)
    }));
    assert!(send_until_blocked(&mut down, FLOOD, |payload| {
        world
            .server
            .send(ids[flooder], Delivery::RELIABLE_ORDERED, payload)
    }));
    assert!(up < FLOOD && down < FLOOD);

    let mut per_client: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
    let mut flooder_received = Vec::new();
    let mut sent = vec![0u32; clients.len()];
    let deadline = world.now + 120_000;
    while world.now < deadline {
        let (server_events, client_events) = world.tick();
        for event in server_events {
            match event {
                ServerEvent::Message {
                    conn,
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                } => per_client
                    .entry(by_id[&conn])
                    .or_default()
                    .push(index_of(&payload)),
                ServerEvent::Disconnected { conn, reason } => {
                    panic!("server closed {conn:?}: {reason:?}")
                }
                _ => {}
            }
        }
        for (client, events) in client_events.iter().enumerate() {
            for event in events {
                match event {
                    ClientEvent::Message {
                        delivery: Delivery::RELIABLE_ORDERED,
                        payload,
                    } if client == flooder => flooder_received.push(index_of(payload)),
                    ClientEvent::Disconnected { reason } => {
                        panic!("client {client} closed: {reason:?}")
                    }
                    _ => {}
                }
            }
        }
        // The flooder retries its retained message, then continues.
        send_until_blocked(&mut up, FLOOD, |payload| {
            world.clients[flooder].send(Delivery::RELIABLE_ORDERED, payload)
        });
        send_until_blocked(&mut down, FLOOD, |payload| {
            world
                .server
                .send(ids[flooder], Delivery::RELIABLE_ORDERED, payload)
        });
        for &client in &clients[1..] {
            let delivered = per_client.get(&client).map_or(0, Vec::len) as u32;
            if sent[client] < 40 && sent[client].saturating_sub(delivered) < 8 {
                world.clients[client]
                    .send(Delivery::RELIABLE_ORDERED, &message(sent[client]))
                    .expect("healthy clients keep sending");
                world
                    .server
                    .send(ids[client], Delivery::RELIABLE_ORDERED, b"ok")
                    .expect("the server keeps sending to healthy clients");
                sent[client] += 1;
            }
        }
        let all_delivered = clients[1..]
            .iter()
            .all(|c| per_client.get(c).map_or(0, Vec::len) == 40);
        if all_delivered
            && per_client.get(&flooder).map_or(0, Vec::len) == FLOOD as usize
            && flooder_received.len() == FLOOD as usize
        {
            break;
        }
    }
    assert_eq!(per_client[&flooder], (0..FLOOD).collect::<Vec<_>>());
    assert_eq!(flooder_received, (0..FLOOD).collect::<Vec<_>>());
    for &client in &clients[1..] {
        assert_eq!(
            per_client[&client],
            (0..40).collect::<Vec<_>>(),
            "client {client}"
        );
    }
}

/// Defect (#267): backpressure masking a dead link, so a sender that keeps
/// retrying refused messages never learns the peer is gone. Oracle: the
/// configured liveness bound — once the server falls silent, the saturated
/// client reports `TimedOut` within `timeout_ms` (to tick granularity) and
/// its sends never fail with anything but `WouldBlock` before that.
#[test]
fn a_saturated_sender_still_times_out_when_the_peer_falls_silent() {
    let config = EndpointConfig {
        timeout_ms: 1_000,
        keepalive_ms: 200,
        ..EndpointConfig::default()
    };
    let mut world = World::new(clean(), 13, config.clone());
    let client = world.connect(1);
    world.connect_all(5_000);
    let silent_from = world.now;
    let mut next = 0u32;
    let mut timed_out_at = None;
    while timed_out_at.is_none() && world.now < silent_from + 10 * config.timeout_ms {
        // Only the client runs: the server never polls or answers again.
        world.now += TICK_MS;
        for event in world.clients[client].poll(world.now) {
            if let ClientEvent::Disconnected { reason } = event {
                assert_eq!(reason, DisconnectReason::TimedOut);
                timed_out_at = Some(world.now);
            }
        }
        if timed_out_at.is_none() {
            send_until_blocked(&mut next, u32::MAX, |payload| {
                world.clients[client].send(Delivery::RELIABLE_ORDERED, payload)
            });
        }
        world.clients[client].flush(world.now);
    }
    let elapsed = timed_out_at.expect("the sender must time out") - silent_from;
    assert!(
        elapsed <= config.timeout_ms + 2 * TICK_MS,
        "timed out after {elapsed} ms"
    );
    assert!(next > 0, "the sender was admitting before the link died");
}

/// Defect (#309): a caller-polled UDP connection both ends close in the
/// same tick reporting nothing, or `Disconnected { Peer }` when the other
/// end's close lands in its grace period. Oracle: the `disconnect` rule of
/// netcode.md 2: a connection the caller disconnects reports exactly one
/// `Disconnected`, with reason `Local`. Each end queues a reliable message,
/// so both closes wait out their grace, and then both disconnect in the
/// same tick; over the next three seconds each end reports only `Local`.
#[test]
fn a_udp_connection_both_ends_close_at_once_reports_local_on_each() {
    let mut world = World::new(clean(), 23, EndpointConfig::default());
    let client = world.connect(1);
    let conn = world.connect_all(5_000)[client];
    world
        .server
        .send(conn, Delivery::RELIABLE_ORDERED, b"server's last")
        .unwrap();
    world.clients[client]
        .send(Delivery::RELIABLE_ORDERED, b"client's last")
        .unwrap();
    world.server.disconnect(conn, world.now);
    world.clients[client].disconnect(world.now);

    let (mut server_events, mut client_events) = (Vec::new(), Vec::new());
    let end = world.now + 3_000;
    while world.now < end {
        let (server, mut clients) = world.tick();
        server_events.extend(server);
        client_events.append(&mut clients[client]);
    }
    assert_eq!(
        server_events,
        [ServerEvent::Disconnected {
            conn,
            reason: DisconnectReason::Local,
        }]
    );
    assert_eq!(
        client_events,
        [ClientEvent::Disconnected {
            reason: DisconnectReason::Local,
        }]
    );
}

/// Defect (#303): a caller-polled receiver that stalls with reliable data
/// in flight being dropped once the sender has resent a fragment a fixed
/// number of times, well inside `timeout_ms`. Oracle: netcode.md 11 (a
/// receiver that polls slowly makes the sender slower, not disconnected)
/// and netcode.md 12 (a stall shorter than `timeout_ms` never closes the
/// peer). On a 1 ms link whose
/// round trips the server has measured, the client stops polling for 5 s
/// (more than twelve retransmissions even at the unmeasured 200 ms
/// timeout, and under the 10 s `timeout_ms`) just after the server sends a
/// message; neither end disconnects and the message arrives once the
/// client polls again.
#[test]
fn a_caller_polled_receiver_that_stalls_keeps_its_connection_and_data() {
    const STALL_MS: u64 = 5_000;
    let network = SimulatedConfig {
        one_way_latency_ms: 1,
        ..clean()
    };
    let mut world = World::new(network, 17, EndpointConfig::default());
    let client = world.connect(1);
    let conn = world.connect_all(5_000)[client];
    for _ in 0..20 {
        world
            .server
            .send(conn, Delivery::RELIABLE_ORDERED, b"warm-up")
            .unwrap();
        world.tick();
    }
    world.tick();
    world
        .server
        .send(conn, Delivery::RELIABLE_ORDERED, b"sent into the stall")
        .unwrap();
    let stall_end = world.now + STALL_MS;
    while world.now < stall_end {
        // Only the server runs: the client's caller is busy.
        world.now += TICK_MS;
        assert!(world.server.poll(world.now).is_empty(), "at {}", world.now);
        world.server.flush(world.now);
    }
    let mut received = Vec::new();
    while world.now < stall_end + 2_000 {
        let (server_events, client_events) = world.tick();
        assert!(server_events.is_empty(), "{server_events:?}");
        for event in client_events.into_iter().flatten() {
            match event {
                ClientEvent::Message { payload, .. } => received.push(payload),
                other => panic!("client: {other:?} at {}", world.now),
            }
        }
    }
    assert_eq!(received, [b"sent into the stall".to_vec()]);
}

/// Defect: a datagram from a previous connection (old nonce/epoch) surfacing
/// on the new one after a reconnect from the same address. Oracle: the new
/// connection gets a fresh id and only the bytes sent on it; the old epoch
/// may drain its queued reliable data during its close grace but never after
/// the server has closed it.
#[test]
fn a_reconnect_from_the_same_address_never_receives_the_old_epoch() {
    let mut world = World::new(SimulatedConfig::default(), 11, EndpointConfig::default());
    let first = world.connect(1);
    let first_id = world.connect_all(20_000)[0];
    for i in 0..20u32 {
        world.clients[first]
            .send(Delivery::RELIABLE_ORDERED, &[0xF0, i as u8])
            .unwrap();
    }
    world.clients[first].disconnect(world.now);
    for _ in 0..3 {
        world.tick();
    }
    world.clients.clear();
    world.addrs.clear();
    let second = world.connect(1);
    let mut second_id = None;
    let mut first_closed = false;
    let mut received = Vec::new();
    let deadline = world.now + 30_000;
    let mut sent = 0u32;
    while world.now < deadline && (received.len() < 20 || !first_closed) {
        let (server_events, client_events) = world.tick();
        for event in server_events {
            match event {
                ServerEvent::Connected { conn } => second_id = Some(conn),
                ServerEvent::Message { conn, payload, .. } if Some(conn) == second_id => {
                    received.push(payload);
                }
                ServerEvent::Message { conn, .. } => {
                    assert_eq!(conn, first_id);
                    assert!(!first_closed, "old epoch delivered after its close");
                }
                ServerEvent::Disconnected { conn, .. } if conn == first_id => first_closed = true,
                ServerEvent::Disconnected { .. } => {}
            }
        }
        if (client_events[second].contains(&ClientEvent::Connected) || second_id.is_some())
            && sent < 20
        {
            world.clients[second]
                .send(Delivery::RELIABLE_ORDERED, &[0x0F, sent as u8])
                .unwrap();
            sent += 1;
        }
    }
    let second_id = second_id.expect("second connection");
    assert_ne!(second_id, first_id, "connection ids are never reused");
    assert!(first_closed, "the server must close the first epoch");
    assert_eq!(received.len(), 20);
    assert!(
        received.iter().all(|p| p[0] == 0x0F),
        "old-epoch bytes leaked: {received:?}"
    );
}

/// Defect: a timer compared in the wrong unit or never re-armed. Oracle:
/// with `timeout_ms = 1000`, a silent peer is dropped as `TimedOut` one
/// virtual second after its last datagram (to tick granularity) and not
/// before; a graceful server disconnect reaches the client as `Peer` within
/// the close grace; a full server denies with `ServerFull`; a server that
/// stopped admitting stays silent and the client times out.
#[test]
fn lifecycle_events_fire_at_the_configured_virtual_instants() {
    let config = EndpointConfig {
        timeout_ms: 1_000,
        keepalive_ms: 200,
        max_peers: 1,
        ..EndpointConfig::default()
    };

    // Idle timeout: connect, then the client falls silent (never ticked).
    let mut world = World::new(clean(), 3, config.clone());
    let client = world.connect(1);
    let id = world.connect_all(5_000)[0];
    let connected_at = world.now;
    let silent = world.clients.remove(client);
    let mut timed_out_at = None;
    while world.now < connected_at + 5_000 && timed_out_at.is_none() {
        let (events, _) = world.tick();
        if let Some(ServerEvent::Disconnected { conn, reason }) = events.first() {
            assert_eq!((*conn, *reason), (id, DisconnectReason::TimedOut));
            timed_out_at = Some(world.now);
        }
    }
    let elapsed = timed_out_at.expect("silent peer must time out") - connected_at;
    // The peer's last datagram landed at most a couple of ticks before the
    // harness observed the connection, hence the tick-granular window.
    assert!(
        elapsed + 2 * TICK_MS >= 1_000 && elapsed < 1_000 + 2 * TICK_MS + 200,
        "timed out after {elapsed} ms"
    );
    drop(silent);

    // Graceful server disconnect: the client learns within the close grace.
    let mut world = World::new(clean(), 4, config.clone());
    let client = world.connect(1);
    let id = world.connect_all(5_000)[0];
    world.server.disconnect(id, world.now);
    let started = world.now;
    let mut reason = None;
    while world.now < started + config.close_grace_ms + 1_000 && reason.is_none() {
        let (_, events) = world.tick();
        for event in &events[client] {
            if let ClientEvent::Disconnected { reason: r } = event {
                reason = Some(*r);
            }
        }
    }
    assert_eq!(reason, Some(DisconnectReason::Peer));
    assert!(
        world.now - started <= config.close_grace_ms + TICK_MS,
        "took {} ms",
        world.now - started
    );

    // Denials: a full server and a server that stopped admitting.
    let mut world = World::new(clean(), 5, config.clone());
    let _first = world.connect(1);
    world.connect_all(5_000);
    let second = world.connect(2);
    let mut denied = None;
    while world.now < 10_000 && denied.is_none() {
        let (_, events) = world.tick();
        for event in &events[second] {
            if let ClientEvent::Denied { reason } = event {
                denied = Some(*reason);
            }
        }
    }
    assert_eq!(denied, Some(DenyReason::ServerFull));

    // Stopped admission: the UDP server answers nothing (no amplification
    // toward peers it will not admit), so the late client never connects and
    // gives up on its own clock as `TimedOut`.
    let mut world = World::new(clean(), 6, config.clone());
    world.server.stop_admission();
    let late = world.connect(3);
    let mut outcome = None;
    while world.now < 10_000 && outcome.is_none() {
        let (server_events, events) = world.tick();
        assert!(
            server_events.is_empty(),
            "server admitted after stop: {server_events:?}"
        );
        for event in &events[late] {
            match event {
                ClientEvent::Connected => panic!("connected after stop_admission"),
                ClientEvent::Denied { reason } => outcome = Some(Err(*reason)),
                ClientEvent::Disconnected { reason } => outcome = Some(Ok(*reason)),
                ClientEvent::Message { .. } | ClientEvent::Reconnecting { .. } => {}
            }
        }
    }
    assert_eq!(outcome, Some(Ok(DisconnectReason::TimedOut)));
    assert!(
        world.now >= config.timeout_ms
            && world.now < config.timeout_ms + 2 * TICK_MS + config.handshake_retry_ms,
        "gave up at {} ms",
        world.now
    );
}

/// Defect: a slow leak in retransmit bookkeeping or an RTT estimate that
/// runs away over a long session. Oracle: ten virtual minutes of steady
/// traffic at twenty percent loss with a small in-flight budget produce no
/// send error, no disconnect, an exact reliable stream, and a finite RTT.
#[test]
fn ten_virtual_minutes_at_twenty_percent_loss_stay_bounded() {
    let network = SimulatedConfig {
        max_in_flight_datagrams: 64,
        ..SimulatedConfig::default()
    };
    let mut world = World::new(network, 99, EndpointConfig::default());
    let client = world.connect(1);
    world.connect_all(20_000);
    let mut sent = 0u32;
    let mut received = Vec::new();
    let end = world.now + 600_000;
    let mut tick = 0u32;
    while world.now < end {
        let (server_events, client_events) = world.tick();
        for event in server_events {
            match event {
                ServerEvent::Message {
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                    ..
                } => received.push(index_of(&payload)),
                ServerEvent::Disconnected { reason, .. } => {
                    panic!("server dropped the peer: {reason:?}")
                }
                _ => {}
            }
        }
        assert!(
            !client_events[client]
                .iter()
                .any(|e| matches!(e, ClientEvent::Disconnected { .. })),
            "client dropped at {} ms",
            world.now
        );
        tick += 1;
        world.clients[client]
            .send(Delivery::LatestState, &tick.to_le_bytes())
            .expect("latest state accepted");
        if sent.saturating_sub(received.len() as u32) < 4 {
            world.clients[client]
                .send(Delivery::RELIABLE_ORDERED, &message(sent))
                .expect("windowed reliable sends accepted");
            sent += 1;
        }
    }
    assert!(
        received.len() > 1_000,
        "only {} messages in ten minutes",
        received.len()
    );
    assert_eq!(received, (0..received.len() as u32).collect::<Vec<_>>());
    let rtt = world.clients[client].rtt();
    assert!(rtt.srtt_ms > 0 && rtt.srtt_ms < 5_000, "rtt {rtt:?}");
}

/// Defect: hidden nondeterminism (a real clock, an unseeded RNG, hash-map
/// iteration order) in the simulated stack. Oracle: the same seed and the
/// same script produce the same event trace, tick for tick.
#[test]
fn the_same_seed_replays_the_same_event_trace() {
    let run = || {
        let mut world = World::new(SimulatedConfig::default(), 42, EndpointConfig::default());
        let client = world.connect(1);
        world.connect_all(20_000);
        let mut trace = Vec::new();
        let mut sent = 0u32;
        for _ in 0..600 {
            let (server_events, client_events) = world.tick();
            trace.push((
                world.now,
                format!("{server_events:?}"),
                format!("{client_events:?}"),
            ));
            if sent < 200 {
                world.clients[client]
                    .send(Delivery::RELIABLE_ORDERED, &message(sent))
                    .unwrap();
                world.clients[client]
                    .send(Delivery::LatestState, &sent.to_le_bytes())
                    .unwrap();
                sent += 1;
            }
        }
        trace
    };
    let first = run();
    assert_eq!(first, run());
    assert!(
        first
            .iter()
            .filter(|(_, s, _)| s.contains("Message"))
            .count()
            > 50
    );
}
