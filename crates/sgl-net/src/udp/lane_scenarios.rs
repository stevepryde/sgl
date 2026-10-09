//! Lanes over the seeded virtual network (netcode.md 14, #268): a small
//! realtime message every tick on lane 0 while lane 1 streams 60 KiB
//! messages as fast as it is admitted. Every bound asserted here comes from
//! the network's parameters, the tick, and deficit round robin's gap bound,
//! never from the scheduler's state.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::rc::Rc;

use wasm_bindgen_test::wasm_bindgen_test;

use super::packet::{self, Item, Parsed};
use super::simulated::{SimulatedConfig, SimulatedNetwork, SimulatedTransport};
use super::{DatagramTransport, Endpoint, EndpointConfig, EndpointEvent};
use crate::{Delivery, Lane, RELIABLE_LANES};

const TICK_MS: u64 = 16;
const MAGIC: [u8; 3] = *b"LNS";
const SERVER: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7_100));
const CLIENT: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 41_000));
const REALTIME_BYTES: usize = 100;
const BULK_BYTES: usize = 60 * 1024;

fn realtime() -> Lane {
    Lane::DEFAULT
}

fn bulk() -> Lane {
    Lane::new(1).expect("lane 1 exists")
}

/// The client's datagram transport, recording when each realtime message's
/// fragment went on the wire.
struct Tap {
    inner: SimulatedTransport,
    transmissions: Rc<RefCell<BTreeMap<u32, Vec<u64>>>>,
}

impl DatagramTransport for Tap {
    fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) {
        if let Some(Parsed::Payload { items, .. }) = packet::parse(payload, MAGIC) {
            for item in items {
                if let Item::Reliable { lane, payload, .. } = item
                    && lane == realtime()
                {
                    let index = u32::from_le_bytes(payload[..4].try_into().expect("indexed"));
                    self.transmissions
                        .borrow_mut()
                        .entry(index)
                        .or_default()
                        .push(now_ms);
                }
            }
        }
        self.inner.send(destination, payload, now_ms);
    }

    fn receive(&mut self, output: &mut [u8], now_ms: u64) -> Option<(usize, SocketAddr)> {
        self.inner.receive(output, now_ms)
    }
}

fn indexed(index: u32, len: usize) -> Vec<u8> {
    let mut payload = vec![0x5A; len];
    payload[..4].copy_from_slice(&index.to_le_bytes());
    payload
}

fn index_of(payload: &[u8]) -> u32 {
    u32::from_le_bytes(payload[..4].try_into().expect("indexed"))
}

/// What one run observed.
struct Run {
    /// When each realtime message was handed to `send`.
    sent: Vec<u64>,
    /// When the server's poll returned each realtime message, in order.
    delivered: Vec<(u32, u64)>,
    /// When each realtime message's fragment was transmitted.
    transmissions: BTreeMap<u32, Vec<u64>>,
    bulk_sent: u32,
    bulk_delivered: Vec<u32>,
}

/// Lane 0 weight 8, lane 1 weight 1 (the 8:1 split of netcode.md 14).
fn endpoint_config(
    max_packets_per_peer_flush: usize,
    max_reliable_transmissions: u8,
) -> EndpointConfig {
    let mut config = EndpointConfig {
        max_packets_per_peer_flush,
        max_reliable_transmissions,
        ..EndpointConfig::new(MAGIC)
    };
    config.reliable.lanes[realtime().index()].weight = 8;
    config.reliable.lanes[bulk().index()].weight = 1;
    config
}

/// One tick: advance the clock, poll both ends, then flush both.
fn tick(
    now: &mut u64,
    server: &mut Endpoint<SimulatedTransport>,
    client: &mut Endpoint<Tap>,
) -> (Vec<EndpointEvent>, Vec<EndpointEvent>) {
    *now += TICK_MS;
    let events = server.poll(*now);
    let client_events = client.poll(*now);
    server.flush(*now);
    client.flush(*now);
    (events, client_events)
}

/// Bulk streams from the start; realtime starts `bulk_lead_ms` later and
/// sends one message per tick for `realtime_ms`; then both drain.
fn run(
    network: SimulatedConfig,
    seed: u64,
    config: &EndpointConfig,
    bulk_lead_ms: u64,
    realtime_ms: u64,
) -> Run {
    let network = SimulatedNetwork::new(network, seed).expect("valid network");
    let mut server = Endpoint::server(
        network.transport(SERVER),
        config.clone(),
        [u8::try_from(seed).expect("small seeds") | 1; 32],
    )
    .expect("server");
    let transmissions = Rc::new(RefCell::new(BTreeMap::new()));
    let tap = Tap {
        inner: network.transport(CLIENT),
        transmissions: Rc::clone(&transmissions),
    };
    let mut client = Endpoint::client(tap, config.clone()).expect("client");
    let peer = client.start_connect(SERVER, 0, 7).expect("connect");

    let mut now = 0;
    let mut connected = (false, false);
    while connected != (true, true) {
        let (events, client_events) = tick(&mut now, &mut server, &mut client);
        connected.0 |= events
            .iter()
            .any(|event| matches!(event, EndpointEvent::Connected { .. }));
        connected.1 |= client_events.contains(&EndpointEvent::Connected { peer });
        assert!(now < 5_000, "never connected");
    }

    let mut result = Run {
        sent: Vec::new(),
        delivered: Vec::new(),
        transmissions: BTreeMap::new(),
        bulk_sent: 0,
        bulk_delivered: Vec::new(),
    };
    let start = now;
    let realtime_from = start + bulk_lead_ms;
    let realtime_until = realtime_from + realtime_ms;
    loop {
        let (events, _) = tick(&mut now, &mut server, &mut client);
        for event in events {
            match event {
                EndpointEvent::Message {
                    delivery: Delivery::Reliable(lane),
                    payload,
                    ..
                } if lane == realtime() => result.delivered.push((index_of(&payload), now)),
                EndpointEvent::Message {
                    delivery: Delivery::Reliable(lane),
                    payload,
                    ..
                } if lane == bulk() => result.bulk_delivered.push(index_of(&payload)),
                other => panic!("unexpected {other:?}"),
            }
        }
        let sending = now < realtime_until;
        if sending && now >= realtime_from {
            let index = u32::try_from(result.sent.len()).expect("few messages");
            client
                .send(
                    peer,
                    Delivery::Reliable(realtime()),
                    &indexed(index, REALTIME_BYTES),
                )
                .expect("one message per tick fits the realtime lane");
            result.sent.push(now);
        }
        while sending
            && client
                .send(
                    peer,
                    Delivery::Reliable(bulk()),
                    &indexed(result.bulk_sent, BULK_BYTES),
                )
                .is_ok()
        {
            result.bulk_sent += 1;
        }
        let drained = result.delivered.len() == result.sent.len()
            && result.bulk_delivered.len() == result.bulk_sent as usize;
        if !sending && drained {
            break;
        }
        assert!(
            now < realtime_until + 60_000,
            "never drained: {} of {} realtime, {} of {} bulk",
            result.delivered.len(),
            result.sent.len(),
            result.bulk_delivered.len(),
            result.bulk_sent
        );
    }
    result.transmissions = transmissions.take();
    result
}

/// Every message on each lane arrived once and in order.
fn assert_exact(run: &Run) {
    let realtime: Vec<_> = run.delivered.iter().map(|&(index, _)| index).collect();
    let sent = u32::try_from(run.sent.len()).expect("few messages");
    assert_eq!(realtime, (0..sent).collect::<Vec<_>>());
    assert_eq!(run.bulk_delivered, (0..run.bulk_sent).collect::<Vec<_>>());
    assert!(run.bulk_sent > 10, "bulk barely ran: {}", run.bulk_sent);
}

/// Defect: realtime fragments queued behind the bulk lane's backlog at the
/// sender (a FIFO across lanes, a starved or strictly lower-priority lane),
/// or held at the receiver behind bulk fragments still being retransmitted
/// (one sequence space or one reassembly for every lane). Oracle: with each
/// flush sending two datagrams, deficit round robin at 8:1 lets at most one
/// bulk fragment go before a waiting realtime fragment, so every realtime
/// message is first transmitted in the flush right after its `send`; and it
/// is delivered no later than the last transmission of it or an earlier
/// realtime message plus the network's worst delay
/// (`one_way + jitter + reorder_extra`) and one poll tick — lane 0's own
/// retransmissions are the only wait. Run with bulk starting together with
/// realtime and with bulk saturated a second earlier, over 2 % loss and
/// 5 % reordering.
#[wasm_bindgen_test(unsupported = test)]
fn realtime_under_bulk_keeps_the_scheduling_bound() {
    let network = SimulatedConfig {
        one_way_latency_ms: 30,
        jitter_ms: 5,
        loss_per_10k: 200,
        duplicate_per_10k: 0,
        reorder_per_10k: 500,
        reorder_extra_ms: 20,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k: [0; RELIABLE_LANES],
    };
    let worst_delay = network.one_way_latency_ms + network.jitter_ms + network.reorder_extra_ms;
    let config = endpoint_config(2, 12);
    for (seed, bulk_lead_ms) in [(11, 0), (12, 1_000)] {
        let run = run(network.clone(), seed, &config, bulk_lead_ms, 20_000);
        assert_exact(&run);
        let mut deadline = 0;
        for (&(index, delivered_at), &sent_at) in run.delivered.iter().zip(&run.sent) {
            let transmitted = &run.transmissions[&index];
            assert_eq!(
                transmitted[0],
                sent_at + TICK_MS,
                "lead {bulk_lead_ms}: realtime {index} waited for a later flush"
            );
            deadline = deadline.max(transmitted.last().expect("transmitted") + worst_delay);
            assert!(
                delivered_at <= deadline + TICK_MS,
                "lead {bulk_lead_ms}: realtime {index} delivered at {delivered_at}, \
                 bound {} (sent {sent_at}, transmitted {transmitted:?})",
                deadline + TICK_MS
            );
        }
    }
}

/// Defect: a lost bulk fragment delaying another lane — one sequence space,
/// window or reassembly shared by every lane, so lane 0 waits for lane 1's
/// retransmissions. Oracle: per-lane independence (netcode.md 10): with
/// half of the bulk lane's datagrams dropped and lane 0's never, every
/// realtime message arrives within `one_way + jitter` of the flush after
/// its `send`, to poll granularity, and the bulk lane still completes
/// exactly and in order.
#[wasm_bindgen_test(unsupported = test)]
fn bulk_loss_never_delays_the_realtime_lane() {
    let mut lane_loss_per_10k = [0; RELIABLE_LANES];
    lane_loss_per_10k[bulk().index()] = 5_000;
    let network = SimulatedConfig {
        one_way_latency_ms: 30,
        jitter_ms: 5,
        loss_per_10k: 0,
        duplicate_per_10k: 0,
        reorder_per_10k: 0,
        reorder_extra_ms: 0,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k,
    };
    let bound = network.one_way_latency_ms + network.jitter_ms + 2 * TICK_MS;
    // Sixty-four transmissions: half the bulk datagrams vanish, and the
    // connection must outlive that rather than time out on retry exhaustion.
    let config = endpoint_config(64, 64);
    let run = run(network, 21, &config, 0, 20_000);
    assert_exact(&run);
    for (&(index, delivered_at), &sent_at) in run.delivered.iter().zip(&run.sent) {
        assert!(
            delivered_at - sent_at <= bound,
            "realtime {index} took {} ms; bound {bound}",
            delivered_at - sent_at
        );
    }
}
