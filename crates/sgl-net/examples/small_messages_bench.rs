//! Small-message measurements (netcode.md 12 and 14), client to server: every
//! 16 ms tick the client sends one 64-byte latest state and, depending on the
//! scenario, 100 unreliable 32-byte messages on lane 2, four reliable 32-byte
//! messages on each lane, or both, and flushes. Prints the client's datagrams and bytes
//! per second, and each class's p50, p99 and maximum latency from when the
//! message was due to its delivery (a refused send keeps waiting), on the
//! seeded virtual network (20 s of virtual time: 30 ms one way, 5 ms jitter,
//! 1 % loss, 5 % reordered by 20 ms) and loopback UDP (10 s of wall time).
//! Messages still undelivered when the run ends count only towards "of".
//!
//! `cargo run --release -p sgl-net --example small_messages_bench`
//!
//! A measurement, not a check: its numbers belong in a change description.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    bench::run();
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::cast_precision_loss)]
mod bench {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::rc::Rc;
    use std::time::Instant;

    use sgl_net::udp::simulated::{SimulatedConfig, SimulatedNetwork};
    use sgl_net::udp::{
        ClientEndpoint, DatagramTransport, EndpointConfig, ServerEndpoint, UdpSocketTransport,
    };
    use sgl_net::{
        ClientEvent, ClientIo, Delivery, Lane, RELIABLE_LANES, SendError, ServerEvent, ServerIo,
    };

    const MAGIC: [u8; 3] = *b"SMB";
    const TICK_MS: u64 = 16;
    const MESSAGE_BYTES: usize = 32;
    const LATEST_BYTES: usize = 64;
    const UNRELIABLE_PER_TICK: usize = 100;
    const RELIABLE_PER_LANE_PER_TICK: usize = 4;
    const UNRELIABLE: u8 = b'U';
    const RELIABLE: u8 = b'R';

    #[derive(Clone, Copy)]
    struct Scenario {
        name: &'static str,
        unreliable: bool,
        reliable: bool,
    }

    const SCENARIOS: [Scenario; 3] = [
        Scenario {
            name: "unreliable",
            unreliable: true,
            reliable: false,
        },
        Scenario {
            name: "reliable",
            unreliable: false,
            reliable: true,
        },
        Scenario {
            name: "both",
            unreliable: true,
            reliable: true,
        },
    ];

    fn unreliable_lane() -> Lane {
        Lane::new(2).expect("lane 2 exists")
    }

    fn lane(index: usize) -> Lane {
        Lane::new(u8::try_from(index).expect("few lanes")).expect("lane exists")
    }

    /// Counts the datagrams and bytes a transport sends.
    struct Counted<T> {
        inner: T,
        datagrams: Rc<Cell<u64>>,
        bytes: Rc<Cell<u64>>,
    }

    impl<T: DatagramTransport> DatagramTransport for Counted<T> {
        fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) {
            self.datagrams.set(self.datagrams.get() + 1);
            self.bytes.set(self.bytes.get() + payload.len() as u64);
            self.inner.send(destination, payload, now_ms);
        }

        fn receive(&mut self, output: &mut [u8], now_ms: u64) -> Option<(usize, SocketAddr)> {
            self.inner.receive(output, now_ms)
        }
    }

    /// Virtual time advances one tick per turn; wall time is read, and the
    /// loop polls as often as it can.
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

    fn payload(kind: u8, due_us: u64, len: usize) -> Vec<u8> {
        let mut payload = vec![0x5A; len];
        payload[0] = kind;
        payload[1..9].copy_from_slice(&due_us.to_le_bytes());
        payload
    }

    fn due_us(payload: &[u8]) -> u64 {
        u64::from_le_bytes(payload[1..9].try_into().unwrap())
    }

    /// Sends the queue's messages in order until `send` refuses one, which
    /// stays queued.
    fn drain(client: &mut dyn ClientIo, delivery: Delivery, kind: u8, queue: &mut VecDeque<u64>) {
        while let Some(&due) = queue.front() {
            match client.send(delivery, &payload(kind, due, MESSAGE_BYTES)) {
                Ok(()) => drop(queue.pop_front()),
                Err(SendError::WouldBlock) => break,
                Err(error) => panic!("send failed: {error}"),
            }
        }
    }

    #[derive(Default)]
    struct Latencies {
        due: usize,
        delivered_us: Vec<u64>,
    }

    impl Latencies {
        fn summary(&mut self) -> String {
            if self.due == 0 {
                return String::new();
            }
            self.delivered_us.sort_unstable();
            let ms = |us: u64| us as f64 / 1_000.0;
            let percentile = |p: usize| {
                self.delivered_us
                    .get(self.delivered_us.len().saturating_sub(1) * p / 100)
                    .map_or(f64::NAN, |&us| ms(us))
            };
            format!(
                "p50 {:>7.1} ms  p99 {:>7.1} ms  max {:>7.1} ms  ({} of {})",
                percentile(50),
                percentile(99),
                self.delivered_us.last().map_or(f64::NAN, |&us| ms(us)),
                self.delivered_us.len(),
                self.due,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn measure(
        transport: &str,
        scenario: Scenario,
        client: &mut dyn ClientIo,
        server: &mut dyn ServerIo,
        mut clock: Clock,
        duration_ms: u64,
        datagrams: &Cell<u64>,
        bytes: &Cell<u64>,
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
            assert!(now_ms < 10_000, "{transport}: never connected");
        }

        let begin_us = clock.now_us();
        let end_us = begin_us + duration_ms * 1_000;
        let (sent_datagrams, sent_bytes) = (datagrams.get(), bytes.get());
        let mut next_tick_us = begin_us;
        let mut unreliable_queue = VecDeque::new();
        let mut reliable_queues: [VecDeque<u64>; RELIABLE_LANES] = Default::default();
        let (mut unreliable, mut reliable) = (Latencies::default(), Latencies::default());
        loop {
            let now_us = clock.now_us();
            if now_us >= end_us {
                break;
            }
            let tick_ms = now_us / 1_000;
            for event in server.poll(tick_ms) {
                match event {
                    ServerEvent::Message {
                        delivery, payload, ..
                    } => {
                        let latency = clock.now_us() - due_us(&payload);
                        match delivery {
                            Delivery::Unreliable(_) => unreliable.delivered_us.push(latency),
                            Delivery::Reliable(_) => reliable.delivered_us.push(latency),
                            Delivery::LatestState => {}
                        }
                    }
                    ServerEvent::Disconnected { reason, .. } => {
                        panic!("{transport}: the server lost the client: {reason:?}")
                    }
                    ServerEvent::Connected { .. } => {}
                }
            }
            for event in client.poll(tick_ms) {
                if let ClientEvent::Disconnected { reason } = event {
                    panic!("{transport}: the client lost the server: {reason:?}");
                }
            }
            // The client sends and flushes once per tick, as a game does;
            // the server polls and flushes every turn.
            let tick = now_us >= next_tick_us;
            while now_us >= next_tick_us {
                if scenario.unreliable {
                    unreliable_queue.extend([next_tick_us; UNRELIABLE_PER_TICK]);
                    unreliable.due += UNRELIABLE_PER_TICK;
                }
                if scenario.reliable {
                    for queue in &mut reliable_queues {
                        queue.extend([next_tick_us; RELIABLE_PER_LANE_PER_TICK]);
                        reliable.due += RELIABLE_PER_LANE_PER_TICK;
                    }
                }
                client
                    .send(
                        Delivery::LatestState,
                        &payload(b'L', next_tick_us, LATEST_BYTES),
                    )
                    .expect("latest state is never refused");
                next_tick_us += TICK_MS * 1_000;
            }
            if tick {
                drain(
                    client,
                    Delivery::Unreliable(unreliable_lane()),
                    UNRELIABLE,
                    &mut unreliable_queue,
                );
                for (index, queue) in reliable_queues.iter_mut().enumerate() {
                    drain(client, Delivery::Reliable(lane(index)), RELIABLE, queue);
                }
                client.flush(tick_ms);
            }
            server.flush(tick_ms);
            clock.turn();
        }

        let seconds = duration_ms as f64 / 1_000.0;
        println!(
            "{transport:<13} {:<10} {:>7.0} datagrams/s {:>8.1} KiB/s",
            scenario.name,
            (datagrams.get() - sent_datagrams) as f64 / seconds,
            (bytes.get() - sent_bytes) as f64 / 1024.0 / seconds,
        );
        for (class, latencies) in [("unreliable", &mut unreliable), ("reliable", &mut reliable)] {
            let summary = latencies.summary();
            if !summary.is_empty() {
                println!("{:<24} {class:<10} {summary}", "");
            }
        }
    }

    fn virtual_udp(scenario: Scenario) {
        let network = SimulatedNetwork::new(
            SimulatedConfig {
                one_way_latency_ms: 30,
                jitter_ms: 5,
                loss_per_10k: 100,
                duplicate_per_10k: 0,
                reorder_per_10k: 500,
                reorder_extra_ms: 20,
                ..SimulatedConfig::default()
            },
            0x5a11,
        )
        .unwrap();
        let server_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_000).into();
        let client_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9_001).into();
        let (datagrams, bytes) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let mut server = ServerEndpoint::new(
            network.transport(server_addr),
            EndpointConfig::new(MAGIC),
            [7; 32],
        )
        .unwrap();
        let mut client = ClientEndpoint::connect(
            Counted {
                inner: network.transport(client_addr),
                datagrams: Rc::clone(&datagrams),
                bytes: Rc::clone(&bytes),
            },
            server_addr,
            EndpointConfig::new(MAGIC),
            0,
            3,
        )
        .unwrap();
        measure(
            "virtual udp",
            scenario,
            &mut client,
            &mut server,
            Clock::Virtual { now_us: 0 },
            20_000,
            &datagrams,
            &bytes,
        );
    }

    fn loopback_udp(scenario: Scenario) {
        let localhost = SocketAddr::from(([127, 0, 0, 1], 0));
        let server_transport = UdpSocketTransport::bind(localhost).unwrap();
        let server_addr = server_transport.local_addr().unwrap();
        let mut server =
            ServerEndpoint::new(server_transport, EndpointConfig::new(MAGIC), [8; 32]).unwrap();
        let (datagrams, bytes) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let mut client = ClientEndpoint::connect(
            Counted {
                inner: UdpSocketTransport::bind(localhost).unwrap(),
                datagrams: Rc::clone(&datagrams),
                bytes: Rc::clone(&bytes),
            },
            server_addr,
            EndpointConfig::new(MAGIC),
            0,
            4,
        )
        .unwrap();
        measure(
            "loopback udp",
            scenario,
            &mut client,
            &mut server,
            Clock::Wall {
                started: Instant::now(),
            },
            10_000,
            &datagrams,
            &bytes,
        );
    }

    pub fn run() {
        for scenario in SCENARIOS {
            virtual_udp(scenario);
        }
        for scenario in SCENARIOS {
            loopback_udp(scenario);
        }
    }
}
