//! netcode.md acceptance: "a 4 MiB message crosses every transport within
//! the configured memory". One 4 MiB reliable message on a bulk lane whose
//! byte allowance is 64 KiB, beside a stream of small lane-0 messages, over
//! memory, the seeded virtual network with loss, reordering and
//! duplication, loopback UDP and native WebSocket. Every check is what a
//! game observes: the bytes delivered, how often, in what order, the
//! errors and capacities returned, and that nobody is disconnected.
#![cfg(not(target_arch = "wasm32"))]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use sgl_net::udp::simulated::{SimulatedConfig, SimulatedNetwork};
use sgl_net::udp::{ClientEndpoint, EndpointConfig, ServerEndpoint, UdpClient, UdpServer};
use sgl_net::websocket::{
    GAME_PATH, NativeWebSocketClient, NativeWebSocketClientConfig, NativeWebSocketServer,
    NativeWebSocketServerConfig, OriginPolicy, WebSocketIdentity,
};
use sgl_net::{
    ClientEvent, ClientIo, Delivery, Lane, RELIABLE_LANES, ReliableConfig, SendError, ServerEvent,
    ServerIo, memory_duplex_with,
};

const MAGIC: [u8; 3] = *b"LRG";
const MESSAGE_BYTES: usize = 4 << 20;
const BULK_LANE_BYTES: usize = 64 * 1024;
const SMALL_BYTES: usize = 1024;

fn bulk() -> Lane {
    Lane::new(1).expect("lane 1 exists")
}

/// Both ends: a 4 MiB cap, and a bulk lane admitting far less than the
/// message at a time. Inbound bounds stay at their defaults.
fn reliable() -> ReliableConfig {
    let mut reliable = ReliableConfig::DEFAULT;
    reliable.max_message_bytes = MESSAGE_BYTES;
    reliable.lanes[0].weight = 8;
    reliable.lanes[bulk().index()].outbound_bytes = BULK_LANE_BYTES;
    reliable
}

/// UDP: the shared outbound ceiling holds exactly one message.
fn endpoint_config() -> EndpointConfig {
    EndpointConfig {
        reliable: reliable(),
        global_reliable_outbound_bytes: MESSAGE_BYTES,
        ..EndpointConfig::new(MAGIC)
    }
}

/// Seeded pseudo-random bytes, so a misplaced fragment cannot match.
fn payload(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn small(index: u32) -> Vec<u8> {
    payload(SMALL_BYTES, u64::from(index) + 1)
}

struct Pair {
    name: &'static str,
    client: Box<dyn ClientIo>,
    server: Box<dyn ServerIo>,
    real_time: bool,
    /// Whether the receiver reassembles fragments (not the memory duplex).
    fragments: bool,
}

fn run(mut pair: Pair) {
    let name = pair.name;
    let deadline = Instant::now() + Duration::from_mins(1);
    let mut now = 0;
    let mut tick = |pair: &mut Pair| -> (Vec<ServerEvent>, Vec<ClientEvent>) {
        now += 16;
        let server_events = pair.server.poll(now);
        let client_events = pair.client.poll(now);
        pair.server.flush(now);
        pair.client.flush(now);
        if pair.real_time {
            assert!(Instant::now() < deadline, "{name}: timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        (server_events, client_events)
    };

    let (mut server_up, mut client_up) = (false, false);
    for _ in 0..1_000 {
        let (server_events, client_events) = tick(&mut pair);
        server_up |= server_events
            .iter()
            .any(|event| matches!(event, ServerEvent::Connected { .. }));
        client_up |= client_events.contains(&ClientEvent::Connected);
        if server_up && client_up {
            break;
        }
    }
    assert!(server_up && client_up, "{name}: never connected");

    // An idle lane admits one message of up to the cap, however small its
    // byte allowance; past the cap nothing is queued.
    let message = payload(MESSAGE_BYTES, 0x269);
    let idle = pair.client.capacity(bulk());
    assert_eq!(idle.bytes, MESSAGE_BYTES, "{name}: {idle:?}");
    assert_eq!(
        pair.client
            .send(Delivery::Reliable(bulk()), &vec![0; MESSAGE_BYTES + 1]),
        Err(SendError::PayloadTooLarge),
        "{name}"
    );
    assert_eq!(pair.client.capacity(bulk()), idle, "{name}");
    pair.client
        .send(Delivery::Reliable(bulk()), &message)
        .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    let busy = pair.client.capacity(bulk());
    assert!(busy.bytes < BULK_LANE_BYTES, "{name}: {busy:?}");

    // A small lane-0 message every tick until the bulk message lands,
    // retrying a refused one on the next tick.
    let (mut bulk_received, mut small_received) = (Vec::new(), Vec::new());
    let mut sent = 0;
    let mut settled = None;
    for ticks in 0..40_000 {
        if bulk_received.is_empty() {
            match pair.client.send(Delivery::RELIABLE_ORDERED, &small(sent)) {
                Ok(()) => sent += 1,
                Err(SendError::WouldBlock) => {}
                Err(error) => panic!("{name}: lane 0 refused with {error:?}"),
            }
        }
        let (server_events, client_events) = tick(&mut pair);
        assert!(
            !client_events
                .iter()
                .any(|event| matches!(event, ClientEvent::Disconnected { .. })),
            "{name}: {client_events:?}"
        );
        for event in server_events {
            match event {
                ServerEvent::Message {
                    delivery: Delivery::Reliable(lane),
                    payload,
                    ..
                } if lane == bulk() => bulk_received.push(payload),
                ServerEvent::Message {
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                    ..
                } => small_received.push(payload),
                other => panic!("{name}: unexpected {other:?}"),
            }
        }
        let done = !bulk_received.is_empty() && small_received.len() == sent as usize;
        match settled {
            // A few more ticks: nothing arrives twice.
            Some(at) if ticks >= at + 20 => break,
            None if done => settled = Some(ticks),
            _ => {}
        }
    }
    assert!(settled.is_some(), "{name}: never delivered");
    assert_eq!(bulk_received.len(), 1, "{name}: delivered once");
    let delivered = &bulk_received[0];
    assert!(delivered == &message, "{name}: the 4 MiB message differs");
    if pair.fragments {
        assert_eq!(
            delivered.capacity(),
            delivered.len(),
            "{name}: reassembly grew past the declared message"
        );
    }
    assert!(sent > 0, "{name}: lane 0 never sent");
    assert!(
        small_received == (0..sent).map(small).collect::<Vec<_>>(),
        "{name}: lane 0 lost, repeated or reordered a message"
    );
}

fn memory() -> Pair {
    let (client, server) = memory_duplex_with(&reliable()).expect("valid configuration");
    Pair {
        name: "memory",
        client: Box::new(client),
        server: Box::new(server),
        real_time: false,
        fragments: false,
    }
}

fn simulated() -> Pair {
    let network = SimulatedNetwork::new(
        SimulatedConfig {
            one_way_latency_ms: 30,
            jitter_ms: 5,
            loss_per_10k: 300,
            duplicate_per_10k: 200,
            reorder_per_10k: 1_000,
            reorder_extra_ms: 20,
            max_in_flight_datagrams: 4_096,
            lane_loss_per_10k: [0; RELIABLE_LANES],
        },
        0x269,
    )
    .expect("valid network");
    let server_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_100).into();
    let client_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_101).into();
    let server = ServerEndpoint::new(network.transport(server_addr), endpoint_config(), [9; 32])
        .expect("server");
    let client = ClientEndpoint::connect(
        network.transport(client_addr),
        server_addr,
        endpoint_config(),
        0,
        5,
    )
    .expect("client");
    Pair {
        name: "simulated udp",
        client: Box::new(client),
        server: Box::new(server),
        real_time: false,
        fragments: true,
    }
}

fn loopback_udp() -> Pair {
    let server = UdpServer::bind_with_key(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        endpoint_config(),
        [10; 32],
    )
    .expect("bind");
    let client =
        UdpClient::connect_with_nonce(server.local_addr().expect("bound"), endpoint_config(), 0, 6)
            .expect("connect");
    Pair {
        name: "loopback udp",
        client: Box::new(client),
        server: Box::new(server),
        real_time: true,
        fragments: true,
    }
}

fn websocket() -> Pair {
    let identity = WebSocketIdentity::new(MAGIC, GAME_PATH, "large.v1");
    let origin = "http://localhost";
    let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity.clone())
        .with_origin_policy(OriginPolicy::exact([origin.to_owned()]).expect("origin"));
    config.reliable = reliable();
    let server = NativeWebSocketServer::bind(config).expect("bind");
    let mut config = NativeWebSocketClientConfig::new(
        format!("ws://{}{GAME_PATH}", server.local_addr()),
        origin,
        identity,
    );
    config.reliable = reliable();
    let client = NativeWebSocketClient::connect(config).expect("connect");
    Pair {
        name: "native websocket",
        client: Box::new(client),
        server: Box::new(server),
        real_time: true,
        fragments: true,
    }
}

/// Defect (#269): a transport that truncates, reorders, repeats or splices
/// the fragments of a message far larger than one datagram or frame; that
/// refuses it because the lane's byte allowance (64 KiB) or the shared
/// ceiling (exactly 4 MiB on UDP) is smaller than the message; that
/// enforces a cap other than the configured one; whose receiver overflows
/// its default inbound bounds on it; or whose reassembly buffer grows past
/// the message; while small messages on another lane are lost, repeated or
/// reordered. Oracle: the seeded payload, delivered byte for byte exactly
/// once, and the lane-0 messages in send order.
#[test]
fn a_4_mib_message_crosses_every_transport_intact() {
    for pair in [memory(), simulated(), loopback_udp(), websocket()] {
        run(pair);
    }
}
