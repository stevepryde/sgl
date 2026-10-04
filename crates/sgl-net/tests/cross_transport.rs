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
    ClientEvent, ClientIo, Delivery, DisconnectReason, MAX_LATEST_STATE_BYTES,
    MAX_RELIABLE_MESSAGE_BYTES, SendError, ServerEvent, ServerIo, memory_duplex,
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
        vec![5; MAX_RELIABLE_MESSAGE_BYTES],
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
    Disconnected(DisconnectReason),
}

struct Trace {
    server: Vec<Observed>,
    client: Vec<Observed>,
}

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
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..20_000 {
            self.now += 16;
            let server_events = self.server.poll(self.now);
            let client_events = self.client.poll(self.now);
            self.server.flush(self.now);
            self.client.flush(self.now);
            if done(&server_events, &client_events) {
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
            .send(Delivery::ReliableOrdered, b"probe")
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
            .send(Delivery::ReliableOrdered, payload)
            .unwrap();
    }
    let mut received = Vec::new();
    pair.settle(&mut |s, _| {
        for event in s {
            if let ServerEvent::Message {
                delivery: Delivery::ReliableOrdered,
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
    pair.settle(&mut |s, _| {
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
        newest.as_deref() == Some(&[b'L', 49][..])
    });
    trace.server.push(Observed::LatestSettled(newest.unwrap()));

    // Over-cap payloads are refused before any I/O, on both lanes.
    for (delivery, size) in [
        (Delivery::LatestState, MAX_LATEST_STATE_BYTES + 1),
        (Delivery::ReliableOrdered, MAX_RELIABLE_MESSAGE_BYTES + 1),
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
            .send(conn, Delivery::ReliableOrdered, payload)
            .unwrap();
    }
    for i in 0..10u8 {
        pair.server
            .send(conn, Delivery::LatestState, &[b'S', i])
            .unwrap();
    }
    let mut received = Vec::new();
    let mut newest = None;
    pair.settle(&mut |_, c| {
        for event in c {
            match event {
                ClientEvent::Message {
                    delivery: Delivery::ReliableOrdered,
                    payload,
                } => received.push(payload.clone()),
                ClientEvent::Message {
                    delivery: Delivery::LatestState,
                    payload,
                } => newest = Some(payload.clone()),
                _ => {}
            }
        }
        received.len() >= back.len() && newest.as_deref() == Some(&[b'S', 9][..])
    });
    trace
        .client
        .extend(received.into_iter().map(Observed::Reliable));
    trace.client.push(Observed::LatestSettled(newest.unwrap()));

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
/// coalescing the two lanes differently, reporting a cap with a different
/// error, or ending a peer-initiated close with a different reason. Oracle:
/// the other three transports, run through the identical script.
#[test]
fn the_same_payloads_produce_the_same_trace_on_every_transport() {
    let reference = run(memory());
    let corpus_len = corpus().len();
    assert_eq!(
        reference
            .server
            .iter()
            .filter(|o| matches!(o, Observed::Reliable(_)))
            .count(),
        corpus_len
    );
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
