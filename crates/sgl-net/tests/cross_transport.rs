//! netcode.md acceptance: "the same game-owned encoded payload crosses UDP,
//! WebSocket, and the memory adapter". One script and one payload corpus run
//! through every transport behind the `ClientIo` / `ServerIo` traits; the
//! observable traces must be identical once connection ids and timing are
//! normalised. Each transport is the independent reference for the others.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::cast_possible_truncation, clippy::too_many_lines)]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use sgl_net::udp::simulated::{SimulatedConfig, SimulatedNetwork};
use sgl_net::udp::{
    ClientEndpoint, EndpointConfig, MAX_DATAGRAM_BYTES, ServerEndpoint, UdpClient, UdpServer,
};
use sgl_net::websocket::{
    GAME_PATH, NativeWebSocketClient, NativeWebSocketClientConfig, NativeWebSocketServer,
    NativeWebSocketServerConfig, OriginPolicy, WebSocketIdentity,
};
use sgl_net::{
    ClientEvent, ClientIo, ConnectionId, DEFAULT_RELIABLE_MESSAGE_BYTES, Delivery,
    DisconnectReason, Lane, MAX_LATEST_STATE_BYTES, RELIABLE_LANES, SendError, ServerEvent,
    ServerIo, memory_duplex,
};

const MAGIC: [u8; 3] = *b"XPT";

/// The game-owned payloads: edge sizes around the datagram and both caps,
/// plus binary that would break a text-frame or string-based path.
fn corpus() -> Vec<Vec<u8>> {
    let mut binary = vec![0u8, 0xFF, 0xFE, 0x80, b'\n', 0, 0xC0, 0xAF];
    binary.extend((0..300u32).map(|i| (i * 37 % 256) as u8));
    vec![
        Vec::new(),
        vec![7],
        vec![1; MAX_DATAGRAM_BYTES - 1],
        vec![2; MAX_DATAGRAM_BYTES],
        vec![3; MAX_DATAGRAM_BYTES + 1],
        vec![4; MAX_LATEST_STATE_BYTES],
        vec![5; DEFAULT_RELIABLE_MESSAGE_BYTES],
        vec![0xFF; 512],
        binary,
    ]
}

/// What a game observes, with connection ids and timing removed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Observed {
    Connected,
    Reliable(Vec<u8>),
    /// The newest latest-state value seen once the burst had settled.
    LatestSettled(Vec<u8>),
    Refused(Delivery, SendError),
    /// Every message one lane delivered in the lanes phase, in order.
    Lane(Lane, Vec<Vec<u8>>),
    Disconnected(DisconnectReason),
}

struct Trace {
    server: Vec<Observed>,
    client: Vec<Observed>,
}

/// A per-tick check that can also send between ticks.
type Driver<'a> =
    dyn FnMut(&mut dyn ClientIo, &mut dyn ServerIo, &[ServerEvent], &[ClientEvent]) -> bool + 'a;

/// A transport pair plus how to wait for it: virtual transports settle in a
/// bounded number of ticks, real ones within a wall-clock deadline.
struct Pair {
    name: &'static str,
    client: Box<dyn ClientIo>,
    server: Box<dyn ServerIo>,
    real_time: bool,
    now: u64,
}

impl Pair {
    /// Tick until `done` returns true or the transport's budget is spent.
    fn settle(&mut self, done: &mut dyn FnMut(&[ServerEvent], &[ClientEvent]) -> bool) {
        self.drive(&mut |_, _, server_events, client_events| done(server_events, client_events));
    }

    /// Like [`Self::settle`], with the transports available to `done` so it
    /// can send between ticks.
    fn drive(&mut self, done: &mut Driver<'_>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..20_000 {
            self.now += 16;
            let server_events = self.server.poll(self.now);
            let client_events = self.client.poll(self.now);
            self.server.flush(self.now);
            self.client.flush(self.now);
            if done(
                &mut *self.client,
                &mut *self.server,
                &server_events,
                &client_events,
            ) {
                return;
            }
            if self.real_time {
                assert!(
                    Instant::now() < deadline,
                    "{}: timed out settling",
                    self.name
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        panic!("{}: never settled", self.name);
    }
}

fn run(mut pair: Pair) -> Trace {
    let mut trace = Trace {
        server: Vec::new(),
        client: Vec::new(),
    };
    let name = pair.name;

    // Connect.
    let (mut server_up, mut client_up) = (false, false);
    pair.settle(&mut |s, c| {
        if s.iter().any(|e| matches!(e, ServerEvent::Connected { .. })) {
            server_up = true;
        }
        if c.contains(&ClientEvent::Connected) {
            client_up = true;
        }
        server_up && client_up
    });
    trace.server.push(Observed::Connected);
    trace.client.push(Observed::Connected);
    let conn = {
        // The server side needs the id for its own sends: re-derive it from a
        // probe so the harness does not depend on event ordering above.
        let mut id = None;
        pair.client
            .send(Delivery::RELIABLE_ORDERED, b"probe")
            .unwrap();
        pair.settle(&mut |s, _| {
            for event in s {
                if let ServerEvent::Message { conn, payload, .. } = event
                    && payload == b"probe"
                {
                    id = Some(*conn);
                }
            }
            id.is_some()
        });
        id.unwrap()
    };

    // Client → server: the reliable corpus, then a latest-state burst.
    let corpus = corpus();
    for payload in &corpus {
        pair.client
            .send(Delivery::RELIABLE_ORDERED, payload)
            .unwrap();
    }
    let mut received = Vec::new();
    pair.settle(&mut |s, _| {
        for event in s {
            if let ServerEvent::Message {
                delivery: Delivery::RELIABLE_ORDERED,
                payload,
                ..
            } = event
            {
                received.push(payload.clone());
            }
        }
        received.len() >= corpus.len()
    });
    trace
        .server
        .extend(received.into_iter().map(Observed::Reliable));
    for i in 0..50u8 {
        pair.client.send(Delivery::LatestState, &[b'L', i]).unwrap();
    }
    let mut newest = None;
    let mut waited = 0;
    pair.drive(&mut |client, _, s, _| {
        for event in s {
            if let ServerEvent::Message {
                delivery: Delivery::LatestState,
                payload,
                ..
            } = event
            {
                newest = Some(payload.clone());
            }
        }
        let landed = newest.as_deref() == Some(&[b'L', 49][..]);
        if !landed && reoffer(&mut waited) {
            client.send(Delivery::LatestState, &[b'L', 49]).unwrap();
        }
        landed
    });
    trace.server.push(Observed::LatestSettled(newest.unwrap()));

    // Over-cap payloads are refused before any I/O, on both lanes.
    for (delivery, size) in [
        (Delivery::LatestState, MAX_LATEST_STATE_BYTES + 1),
        (
            Delivery::RELIABLE_ORDERED,
            DEFAULT_RELIABLE_MESSAGE_BYTES + 1,
        ),
    ] {
        let error = pair
            .client
            .send(delivery, &vec![9; size])
            .expect_err("over-cap payload must be refused");
        trace.client.push(Observed::Refused(delivery, error));
        let error = pair
            .server
            .send(conn, delivery, &vec![9; size])
            .expect_err("over-cap payload must be refused");
        trace.server.push(Observed::Refused(delivery, error));
    }

    // Server → client: a subset of the corpus and a latest burst.
    let back: Vec<Vec<u8>> = vec![
        corpus[1].clone(),
        corpus[3].clone(),
        corpus[6].clone(),
        corpus[8].clone(),
    ];
    for payload in &back {
        pair.server
            .send(conn, Delivery::RELIABLE_ORDERED, payload)
            .unwrap();
    }
    for i in 0..10u8 {
        pair.server
            .send(conn, Delivery::LatestState, &[b'S', i])
            .unwrap();
    }
    let mut received = Vec::new();
    let mut newest = None;
    let mut waited = 0;
    pair.drive(&mut |_, server, _, c| {
        for event in c {
            match event {
                ClientEvent::Message {
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                } => received.push(payload.clone()),
                ClientEvent::Message {
                    delivery: Delivery::LatestState,
                    payload,
                } => newest = Some(payload.clone()),
                _ => {}
            }
        }
        if newest.as_deref() != Some(&[b'S', 9][..]) && reoffer(&mut waited) {
            server
                .send(conn, Delivery::LatestState, &[b'S', 9])
                .unwrap();
        }
        received.len() >= back.len() && newest.as_deref() == Some(&[b'S', 9][..])
    });
    trace
        .client
        .extend(received.into_iter().map(Observed::Reliable));
    trace.client.push(Observed::LatestSettled(newest.unwrap()));

    // Saturation (#267): each side sends indexed reliable messages without
    // flushing until refused, keeps the refused one, ticks with nobody
    // disconnected, then retries it and sends the rest as capacity returns.
    let (refusal, received) = saturate(&mut pair, Direction::ClientToServer, conn);
    trace.client.push(refusal);
    trace
        .server
        .extend(received.into_iter().map(Observed::Reliable));
    let (refusal, received) = saturate(&mut pair, Direction::ServerToClient, conn);
    trace.server.push(refusal);
    trace
        .client
        .extend(received.into_iter().map(Observed::Reliable));

    // Lanes (#268): interleaved messages on three lanes, each direction;
    // each lane must arrive whole and in its own order.
    trace
        .server
        .extend(lanes_phase(&mut pair, Direction::ClientToServer, conn));
    trace
        .client
        .extend(lanes_phase(&mut pair, Direction::ServerToClient, conn));

    // The client hangs up; the server must learn it was the peer.
    let now = pair.now;
    pair.client.disconnect(now);
    let mut reason = None;
    pair.settle(&mut |s, _| {
        for event in s {
            if let ServerEvent::Disconnected { reason: r, .. } = event {
                reason = Some(*r);
            }
        }
        reason.is_some()
    });
    trace.server.push(Observed::Disconnected(reason.unwrap()));
    eprintln!(
        "{name}: {} server events, {} client events",
        trace.server.len(),
        trace.client.len()
    );
    trace
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    ClientToServer,
    ServerToClient,
}

/// Ticks a latest-state phase waits before offering the newest value again.
/// Latest state is never retransmitted, so a lossy network may drop it; a
/// game would offer it again. Each offer waits longer than any pair's round
/// trip, so an offer made right after a flush never displaces one a
/// transport's worker has not written yet.
const LATEST_REOFFER_TICKS: u32 = 32;

/// Whether this tick offers the newest latest-state value again.
fn reoffer(waited: &mut u32) -> bool {
    *waited += 1;
    waited.is_multiple_of(LATEST_REOFFER_TICKS)
}

/// Messages in the saturation phase: more than any transport's per-peer
/// reliable allowance, so every transport refuses at least once.
const SATURATION_MESSAGES: u32 = 160;

fn indexed(index: u32) -> Vec<u8> {
    let mut payload = vec![0xB7; 200];
    payload[..4].copy_from_slice(&index.to_le_bytes());
    payload
}

fn send_indexed(
    direction: Direction,
    client: &mut dyn ClientIo,
    server: &mut dyn ServerIo,
    conn: ConnectionId,
    next: &mut u32,
) -> Option<SendError> {
    while *next < SATURATION_MESSAGES {
        let payload = indexed(*next);
        let result = match direction {
            Direction::ClientToServer => client.send(Delivery::RELIABLE_ORDERED, &payload),
            Direction::ServerToClient => server.send(conn, Delivery::RELIABLE_ORDERED, &payload),
        };
        match result {
            Ok(()) => *next += 1,
            Err(error) => return Some(error),
        }
    }
    None
}

/// Runs one direction of the saturation phase; returns the first refusal and
/// the payloads the receiver observed.
fn saturate(pair: &mut Pair, direction: Direction, conn: ConnectionId) -> (Observed, Vec<Vec<u8>>) {
    let name = pair.name;
    let mut next = 0;
    let refusal = send_indexed(
        direction,
        &mut *pair.client,
        &mut *pair.server,
        conn,
        &mut next,
    )
    .unwrap_or_else(|| panic!("{name}: {SATURATION_MESSAGES} sends were never refused"));
    let capacity = match direction {
        Direction::ClientToServer => pair.client.capacity(Lane::DEFAULT),
        Direction::ServerToClient => pair.server.capacity(conn, Lane::DEFAULT),
    };
    assert!(
        capacity.messages == 0 || capacity.bytes < indexed(next).len(),
        "{name}: capacity {capacity:?} admits the refused message"
    );
    let mut received = Vec::new();
    let mut ticks = 0;
    pair.drive(&mut |client, server, server_events, client_events| {
        assert!(
            !server_events
                .iter()
                .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
                && !client_events
                    .iter()
                    .any(|e| matches!(e, ClientEvent::Disconnected { .. })),
            "{name}: a refused send ended the connection"
        );
        let payloads = match direction {
            Direction::ClientToServer => server_events
                .iter()
                .filter_map(|event| match event {
                    ServerEvent::Message { payload, .. } => Some(payload.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Direction::ServerToClient => client_events
                .iter()
                .filter_map(|event| match event {
                    ClientEvent::Message { payload, .. } => Some(payload.clone()),
                    _ => None,
                })
                .collect(),
        };
        received.extend(payloads);
        ticks += 1;
        // Hold the refused message for 50 ticks, then retry and continue.
        if ticks > 50 {
            match send_indexed(direction, client, server, conn, &mut next) {
                None | Some(SendError::WouldBlock) => {}
                Some(error) => panic!("{name}: retry refused with {error:?}"),
            }
        }
        received.len() >= SATURATION_MESSAGES as usize
    });
    (
        Observed::Refused(Delivery::RELIABLE_ORDERED, refusal),
        received,
    )
}

/// The lanes the lanes phase uses, and its messages per lane.
const PHASE_LANES: [u8; 3] = [0, 1, 3];
const PER_LANE: u8 = 12;

/// Message `index` of `lane`: small on lane 0; on lane 1 sizes around the
/// UDP fragment and past one WebSocket fragment; on lane 3 empty and
/// around the WebSocket fragment boundary.
fn lane_message(lane: u8, index: u8) -> Vec<u8> {
    let len = match lane {
        0 => 2 + usize::from(index),
        1 => [3, 1_151, 17_000, 40_000][usize::from(index % 4)],
        _ => [2, 16 * 1024, 16 * 1024 + 1][usize::from(index % 3)],
    };
    let mut payload: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
    if len >= 2 {
        payload[..2].copy_from_slice(&[lane, index]);
    }
    payload
}

fn lane_of(index: u8) -> Lane {
    Lane::new(index).expect("phase lanes exist")
}

/// Sends every phase message interleaved across lanes in one burst, then
/// returns each lane's deliveries in lane order.
fn lanes_phase(pair: &mut Pair, direction: Direction, conn: ConnectionId) -> Vec<Observed> {
    for index in 0..PER_LANE {
        for lane in PHASE_LANES {
            let payload = lane_message(lane, index);
            let delivery = Delivery::Reliable(lane_of(lane));
            match direction {
                Direction::ClientToServer => pair.client.send(delivery, &payload),
                Direction::ServerToClient => pair.server.send(conn, delivery, &payload),
            }
            .expect("the phase fits every lane's bounds");
        }
    }
    let mut lanes: [Vec<Vec<u8>>; RELIABLE_LANES] = Default::default();
    let expected = PHASE_LANES.len() * usize::from(PER_LANE);
    let mut received = 0;
    pair.settle(&mut |server_events, client_events| {
        let messages = match direction {
            Direction::ClientToServer => server_events
                .iter()
                .filter_map(|event| match event {
                    ServerEvent::Message {
                        delivery, payload, ..
                    } => Some((*delivery, payload.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Direction::ServerToClient => client_events
                .iter()
                .filter_map(|event| match event {
                    ClientEvent::Message { delivery, payload } => {
                        Some((*delivery, payload.clone()))
                    }
                    _ => None,
                })
                .collect(),
        };
        for (delivery, payload) in messages {
            let Delivery::Reliable(lane) = delivery else {
                panic!("latest state in the lanes phase");
            };
            lanes[lane.index()].push(payload);
            received += 1;
        }
        received >= expected
    });
    PHASE_LANES
        .iter()
        .map(|&lane| Observed::Lane(lane_of(lane), std::mem::take(&mut lanes[usize::from(lane)])))
        .collect()
}

fn memory() -> Pair {
    let (client, server) = memory_duplex();
    Pair {
        name: "memory",
        client: Box::new(client),
        server: Box::new(server),
        real_time: false,
        now: 0,
    }
}

fn simulated() -> Pair {
    let network = SimulatedNetwork::new(
        SimulatedConfig {
            one_way_latency_ms: 20,
            jitter_ms: 4,
            loss_per_10k: 300,
            duplicate_per_10k: 100,
            reorder_per_10k: 500,
            reorder_extra_ms: 30,
            max_in_flight_datagrams: 4_096,
            lane_loss_per_10k: [0; RELIABLE_LANES],
        },
        77,
    )
    .unwrap();
    let server_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_000).into();
    let client_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_001).into();
    let config = EndpointConfig::new(MAGIC);
    let server =
        ServerEndpoint::new(network.transport(server_addr), config.clone(), [7; 32]).unwrap();
    let client =
        ClientEndpoint::connect(network.transport(client_addr), server_addr, config, 0, 3).unwrap();
    Pair {
        name: "simulated udp",
        client: Box::new(client),
        server: Box::new(server),
        real_time: false,
        now: 0,
    }
}

fn loopback_udp() -> Pair {
    let config = EndpointConfig::new(MAGIC);
    let server = UdpServer::bind_with_key(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        config.clone(),
        [8; 32],
    )
    .unwrap();
    let client = UdpClient::connect_with_nonce(server.local_addr().unwrap(), config, 0, 4).unwrap();
    Pair {
        name: "loopback udp",
        client: Box::new(client),
        server: Box::new(server),
        real_time: true,
        now: 0,
    }
}

fn websocket() -> Pair {
    let identity = WebSocketIdentity::new(MAGIC, GAME_PATH, "xpt.v1");
    let origin = "http://localhost";
    let config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity.clone())
        .with_origin_policy(OriginPolicy::exact([origin.to_owned()]).unwrap());
    let server = NativeWebSocketServer::bind(config).unwrap();
    let url = format!("ws://{}{GAME_PATH}", server.local_addr());
    let client =
        NativeWebSocketClient::connect(NativeWebSocketClientConfig::new(url, origin, identity))
            .unwrap();
    Pair {
        name: "native websocket",
        client: Box::new(client),
        server: Box::new(server),
        real_time: true,
        now: 0,
    }
}

/// Defect: one transport truncating at its frame limit, mangling binary,
/// coalescing the delivery classes differently, reporting a cap with a
/// different error, closing, losing or duplicating when a saturated lane
/// refuses a send (#267), mixing, reordering or misrouting messages between
/// lanes or splitting a fragmented message wrongly (#268), or ending a
/// peer-initiated close with a different reason. Oracle: the memory
/// transport, run through the identical script and checked against the
/// messages the script sent.
#[test]
fn the_same_payloads_produce_the_same_trace_on_every_transport() {
    let reference = run(memory());
    let saturated: Vec<_> = (0..SATURATION_MESSAGES)
        .map(|index| Observed::Reliable(indexed(index)))
        .collect();
    let reliable = |trace: &[Observed]| -> Vec<Observed> {
        trace
            .iter()
            .filter(|o| matches!(o, Observed::Reliable(_)))
            .cloned()
            .collect()
    };
    let server_reliable = reliable(&reference.server);
    assert_eq!(
        server_reliable.len(),
        corpus().len() + saturated.len(),
        "the memory reference delivers the corpus and the saturation phase"
    );
    assert!(
        server_reliable.ends_with(&saturated) && reliable(&reference.client).ends_with(&saturated),
        "the memory reference delivers every saturating message once, in order"
    );
    let phase: Vec<_> = PHASE_LANES
        .iter()
        .map(|&lane| {
            Observed::Lane(
                lane_of(lane),
                (0..PER_LANE)
                    .map(|index| lane_message(lane, index))
                    .collect(),
            )
        })
        .collect();
    for side in [&reference.server, &reference.client] {
        assert!(
            side.windows(phase.len())
                .any(|window| window == phase.as_slice()),
            "the memory reference delivers every lane whole and in its own order"
        );
    }
    for pair in [simulated(), loopback_udp(), websocket()] {
        let name = pair.name;
        let trace = run(pair);
        for (side, got, want) in [
            ("server", &trace.server, &reference.server),
            ("client", &trace.client, &reference.client),
        ] {
            if got != want {
                let first = got
                    .iter()
                    .zip(want.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(got.len().min(want.len()));
                panic!(
                    "{name} {side} trace diverges from memory at event {first}: got {:?}, memory {:?} (lengths {} vs {})",
                    got.get(first),
                    want.get(first),
                    got.len(),
                    want.len()
                );
            }
        }
    }
}
