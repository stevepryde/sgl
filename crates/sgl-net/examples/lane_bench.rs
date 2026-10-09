//! Lane measurements (netcode.md 14): a 100-byte realtime message every
//! 16 ms while 60 KiB bulk messages stream as fast as they are admitted,
//! client to server. Prints the realtime messages' p50, p99 and maximum
//! latency, from when each was due to its delivery (a refused send keeps
//! waiting), and the bulk throughput on the seeded virtual network (20 s of
//! virtual time: 30 ms one way, 5 ms jitter, 2 % loss, 5 % reordered by
//! 20 ms), loopback UDP and loopback WebSocket (10 s of wall time each).
//! WebSocket bulk is paced to one message per 2 ms (about 30 MiB/s): the
//! native worker reads frames as fast as loopback TCP delivers them, and an
//! unpaced stream overruns the server's default 256 KiB inbound lane
//! between two polls, which disconnects the client (`InboundOverflow`).
//!
//! By default realtime uses lane 0 at weight 8 and bulk lane 1 at weight 1;
//! `--single-lane` puts both on lane 0 for comparison.
//!
//! `cargo run --release -p sgl-net --example lane_bench [-- --single-lane]`
//!
//! A measurement, not a check: its numbers belong in a change description.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    bench::run(std::env::args().any(|arg| arg == "--single-lane"));
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::cast_precision_loss)]
mod bench {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Instant;

    use sgl_net::udp::simulated::{SimulatedConfig, SimulatedNetwork};
    use sgl_net::udp::{ClientEndpoint, EndpointConfig, ServerEndpoint, UdpClient, UdpServer};
    use sgl_net::websocket::{
        GAME_PATH, NativeWebSocketClient, NativeWebSocketClientConfig, NativeWebSocketServer,
        NativeWebSocketServerConfig, OriginPolicy, WebSocketIdentity,
    };
    use sgl_net::{ClientEvent, ClientIo, Delivery, Lane, ReliableConfig, ServerEvent, ServerIo};

    const MAGIC: [u8; 3] = *b"LBN";
    const TICK_MS: u64 = 16;
    const REALTIME_BYTES: usize = 100;
    const BULK_BYTES: usize = 60 * 1024;

    /// The lanes of the two streams and the configuration both ends use.
    struct Setup {
        realtime: Delivery,
        bulk: Delivery,
        reliable: ReliableConfig,
    }

    fn setup(single_lane: bool) -> Setup {
        let mut reliable = ReliableConfig::DEFAULT;
        if single_lane {
            return Setup {
                realtime: Delivery::RELIABLE_ORDERED,
                bulk: Delivery::RELIABLE_ORDERED,
                reliable,
            };
        }
        let bulk = Lane::new(1).expect("lane 1 exists");
        reliable.lanes[0].weight = 8;
        reliable.lanes[bulk.index()].weight = 1;
        Setup {
            realtime: Delivery::RELIABLE_ORDERED,
            bulk: Delivery::Reliable(bulk),
            reliable,
        }
    }

    /// Virtual time advances one tick per turn; wall time is read, and the
    /// loop polls as often as it can so the server drains its inbound queues
    /// as fast as the socket fills them.
    enum Clock {
        Virtual { now_us: u64 },
        Wall { started: Instant },
    }

    impl Clock {
        fn now_us(&self) -> u64 {
            match self {
                Self::Virtual { now_us } => *now_us,
                Self::Wall { started } => u64::try_from(started.elapsed().as_micros()).unwrap(),
            }
        }

        fn turn(&mut self) {
            match self {
                Self::Virtual { now_us } => *now_us += TICK_MS * 1_000,
                Self::Wall { .. } => std::thread::yield_now(),
            }
        }
    }

    fn payload(kind: u8, index: u32, sent_us: u64, len: usize) -> Vec<u8> {
        let mut payload = vec![0x5A; len];
        payload[0] = kind;
        payload[1..5].copy_from_slice(&index.to_le_bytes());
        payload[5..13].copy_from_slice(&sent_us.to_le_bytes());
        payload
    }

    fn sent_us(payload: &[u8]) -> u64 {
        u64::from_le_bytes(payload[5..13].try_into().unwrap())
    }

    fn measure(
        name: &str,
        setup: &Setup,
        client: &mut dyn ClientIo,
        server: &mut dyn ServerIo,
        mut clock: Clock,
        duration_ms: u64,
        bulk_interval_us: u64,
    ) {
        let (mut server_up, mut client_up) = (false, false);
        while !(server_up && client_up) {
            let now_ms = clock.now_us() / 1_000;
            server_up |= server
                .poll(now_ms)
                .iter()
                .any(|event| matches!(event, ServerEvent::Connected { .. }));
            client_up |= client.poll(now_ms).contains(&ClientEvent::Connected);
            server.flush(now_ms);
            client.flush(now_ms);
            clock.turn();
            assert!(now_ms < 10_000, "{name}: never connected");
        }

        let begin_us = clock.now_us();
        let end_us = begin_us + duration_ms * 1_000;
        let (mut next_realtime_us, mut realtime_index, mut bulk_index) = (begin_us, 0u32, 0u32);
        let mut next_bulk_us = begin_us;
        let mut latencies_us = Vec::new();
        let mut bulk_bytes = 0usize;
        loop {
            let now_us = clock.now_us();
            if now_us >= end_us {
                break;
            }
            let tick_ms = now_us / 1_000;
            for event in server.poll(tick_ms) {
                match event {
                    ServerEvent::Message { payload, .. } if payload[0] == b'R' => {
                        latencies_us.push(clock.now_us() - sent_us(&payload));
                    }
                    ServerEvent::Message { payload, .. } => bulk_bytes += payload.len(),
                    ServerEvent::Disconnected { reason, .. } => {
                        panic!("{name}: the server lost the client: {reason:?}")
                    }
                    ServerEvent::Connected { .. } => {}
                }
            }
            for event in client.poll(tick_ms) {
                if let ClientEvent::Disconnected { reason } = event {
                    panic!("{name}: the client lost the server: {reason:?}");
                }
            }
            if now_us >= next_realtime_us {
                // Latency counts from when the message was due, so a send
                // refused while the lane is full counts its wait too.
                let message = payload(b'R', realtime_index, next_realtime_us, REALTIME_BYTES);
                // A refused realtime message is retried next turn.
                if client.send(setup.realtime, &message).is_ok() {
                    realtime_index += 1;
                    next_realtime_us += TICK_MS * 1_000;
                }
            }
            while now_us >= next_bulk_us
                && client
                    .send(setup.bulk, &payload(b'B', bulk_index, now_us, BULK_BYTES))
                    .is_ok()
            {
                bulk_index += 1;
                next_bulk_us = now_us + bulk_interval_us;
            }
            client.flush(tick_ms);
            server.flush(tick_ms);
            clock.turn();
        }

        latencies_us.sort_unstable();
        let ms = |us: u64| us as f64 / 1_000.0;
        let percentile = |p: usize| ms(latencies_us[(latencies_us.len() - 1) * p / 100]);
        println!(
            "{name:<18} realtime p50 {:>7.1} ms  p99 {:>7.1} ms  max {:>7.1} ms  ({} of {} delivered)  bulk {:>8.0} KiB/s",
            percentile(50),
            percentile(99),
            ms(*latencies_us.last().unwrap_or(&0)),
            latencies_us.len(),
            realtime_index,
            bulk_bytes as f64 / 1024.0 / (duration_ms as f64 / 1_000.0),
        );
    }

    fn endpoint_config(setup: &Setup) -> EndpointConfig {
        EndpointConfig {
            reliable: setup.reliable.clone(),
            ..EndpointConfig::new(MAGIC)
        }
    }

    pub fn run(single_lane: bool) {
        let setup = setup(single_lane);
        println!(
            "{}",
            if single_lane {
                "both streams on lane 0"
            } else {
                "realtime on lane 0 (weight 8), bulk on lane 1 (weight 1)"
            }
        );

        let network = SimulatedNetwork::new(
            SimulatedConfig {
                one_way_latency_ms: 30,
                jitter_ms: 5,
                loss_per_10k: 200,
                duplicate_per_10k: 0,
                reorder_per_10k: 500,
                reorder_extra_ms: 20,
                ..SimulatedConfig::default()
            },
            0x1a2e,
        )
        .unwrap();
        let server_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_000).into();
        let client_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_001).into();
        let mut server = ServerEndpoint::new(
            network.transport(server_addr),
            endpoint_config(&setup),
            [7; 32],
        )
        .unwrap();
        let mut client = ClientEndpoint::connect(
            network.transport(client_addr),
            server_addr,
            endpoint_config(&setup),
            0,
            3,
        )
        .unwrap();
        measure(
            "virtual udp",
            &setup,
            &mut client,
            &mut server,
            Clock::Virtual { now_us: 0 },
            20_000,
            0,
        );

        let mut server = UdpServer::bind_with_key(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            endpoint_config(&setup),
            [8; 32],
        )
        .unwrap();
        let mut client = UdpClient::connect_with_nonce(
            server.local_addr().unwrap(),
            endpoint_config(&setup),
            0,
            4,
        )
        .unwrap();
        measure(
            "loopback udp",
            &setup,
            &mut client,
            &mut server,
            Clock::Wall {
                started: Instant::now(),
            },
            10_000,
            0,
        );

        let identity = WebSocketIdentity::new(MAGIC, GAME_PATH, "lane-bench.v1");
        let origin = "http://localhost";
        let mut config =
            NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity.clone())
                .with_origin_policy(OriginPolicy::exact([origin.to_owned()]).unwrap());
        config.reliable = setup.reliable.clone();
        let mut server = NativeWebSocketServer::bind(config).unwrap();
        let mut config = NativeWebSocketClientConfig::new(
            format!("ws://{}{GAME_PATH}", server.local_addr()),
            origin,
            identity,
        );
        config.reliable = setup.reliable.clone();
        let mut client = NativeWebSocketClient::connect(config).unwrap();
        measure(
            "loopback websocket",
            &setup,
            &mut client,
            &mut server,
            Clock::Wall {
                started: Instant::now(),
            },
            10_000,
            2_000,
        );
    }
}
