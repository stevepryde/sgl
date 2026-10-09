//! Lanes over the seeded virtual network (netcode.md 14, #268, #269): a
//! small realtime message every tick on lane 0, reliable or unreliable,
//! while lane 1 streams reliable messages (60 KiB, or 4 MiB) as fast as it
//! is admitted. Every bound asserted here comes from the network's
//! parameters, the tick, and deficit round robin's gap bound, never from
//! the scheduler's state.

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

/// The client's datagram transport, recording when each realtime message
/// went on the wire, reliable or unreliable.
struct Tap {
    inner: SimulatedTransport,
    transmissions: Rc<RefCell<BTreeMap<u32, Vec<u64>>>>,
}

impl DatagramTransport for Tap {
    fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) {
        if let Some(Parsed::Payload { items, .. }) = packet::parse(payload, MAGIC) {
            for item in items {
                if let Item::Reliable { lane, payload, .. } | Item::Unreliable { lane, payload, .. } =
                    item
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

/// Bulk message `index`: seeded pseudo-random bytes after the index, so a
/// misplaced fragment cannot match.
fn bulk_payload(index: u32, len: usize) -> Vec<u8> {
    let mut state = u64::from(index) | 1 << 40;
    let mut payload: Vec<u8> = (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
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
    /// When the server's poll returned each realtime message, in the order
    /// it did.
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

/// The two streams: bulk messages of `bulk_bytes` from the start, and from
/// `bulk_lead_ms` later one `realtime_bytes` message per tick for
/// `realtime_ms` with `delivery` on lane 0.
struct Workload {
    bulk_bytes: usize,
    realtime_bytes: usize,
    bulk_lead_ms: u64,
    realtime_ms: u64,
    delivery: Delivery,
}

/// 60 KiB bulk messages and 100-byte realtime messages.
const fn streams(bulk_lead_ms: u64, realtime_ms: u64, delivery: Delivery) -> Workload {
    Workload {
        bulk_bytes: BULK_BYTES,
        realtime_bytes: REALTIME_BYTES,
        bulk_lead_ms,
        realtime_ms,
        delivery,
    }
}

/// The index of a delivered bulk message, which must be byte for byte the
/// message sent under it.
fn checked_bulk(payload: &[u8], len: usize) -> u32 {
    let index = index_of(payload);
    assert!(
        payload == bulk_payload(index, len),
        "bulk {index} arrived altered"
    );
    index
}

/// Sends bulk messages from `first` until the lane refuses one, which waits
/// in `next` for the next call; returns how many were sent.
fn send_bulk(
    client: &mut Endpoint<Tap>,
    peer: u64,
    next: &mut Option<Vec<u8>>,
    first: u32,
    len: usize,
) -> u32 {
    let mut sent = 0;
    loop {
        let message = next.get_or_insert_with(|| bulk_payload(first + sent, len));
        if client
            .send(peer, Delivery::Reliable(bulk()), message)
            .is_err()
        {
            return sent;
        }
        *next = None;
        sent += 1;
    }
}

/// Runs `workload`; then both streams drain (an unreliable stream for two
/// more seconds). Every bulk message must arrive intact.
fn run(network: SimulatedConfig, seed: u64, config: &EndpointConfig, workload: &Workload) -> Run {
    let &Workload {
        bulk_bytes,
        realtime_bytes,
        bulk_lead_ms,
        realtime_ms,
        delivery,
    } = workload;
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
    // The bulk message waiting for admission, built once.
    let mut next_bulk = None;
    let realtime_from = start + bulk_lead_ms;
    let realtime_until = realtime_from + realtime_ms;
    loop {
        let (events, _) = tick(&mut now, &mut server, &mut client);
        for event in events {
            match event {
                EndpointEvent::Message {
                    delivery: got,
                    payload,
                    ..
                } if got == delivery => result.delivered.push((index_of(&payload), now)),
                EndpointEvent::Message {
                    delivery: Delivery::Reliable(lane),
                    payload,
                    ..
                } if lane == bulk() => {
                    result
                        .bulk_delivered
                        .push(checked_bulk(&payload, bulk_bytes));
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        let sending = now < realtime_until;
        if sending && now >= realtime_from {
            let index = u32::try_from(result.sent.len()).expect("few messages");
            client
                .send(peer, delivery, &indexed(index, realtime_bytes))
                .expect("one message per tick fits the realtime lane");
            result.sent.push(now);
        }
        if sending {
            let first = result.bulk_sent;
            result.bulk_sent += send_bulk(&mut client, peer, &mut next_bulk, first, bulk_bytes);
        }
        let realtime_drained = match delivery {
            Delivery::Unreliable(_) => now >= realtime_until + 2_000,
            _ => result.delivered.len() == result.sent.len(),
        };
        let drained = realtime_drained && result.bulk_delivered.len() == result.bulk_sent as usize;
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
        let run = run(
            network.clone(),
            seed,
            &config,
            &streams(bulk_lead_ms, 20_000, Delivery::Reliable(realtime())),
        );
        assert_exact(&run);
        assert_scheduling_bound(&run, worst_delay, &format!("lead {bulk_lead_ms}"));
    }
}

/// Every realtime message was first transmitted in the flush right after
/// its `send`, and delivered no later than the last transmission of it or
/// an earlier realtime message plus the network's `worst_delay` and one
/// poll tick.
fn assert_scheduling_bound(run: &Run, worst_delay: u64, label: &str) {
    let mut deadline = 0;
    for (&(index, delivered_at), &sent_at) in run.delivered.iter().zip(&run.sent) {
        let transmitted = &run.transmissions[&index];
        assert_eq!(
            transmitted[0],
            sent_at + TICK_MS,
            "{label}: realtime {index} waited for a later flush"
        );
        deadline = deadline.max(transmitted.last().expect("transmitted") + worst_delay);
        assert!(
            delivered_at <= deadline + TICK_MS,
            "{label}: realtime {index} delivered at {delivered_at}, \
             bound {} (sent {sent_at}, transmitted {transmitted:?})",
            deadline + TICK_MS
        );
    }
}

/// Defect (#269): a message far larger than its lane's byte allowance and
/// the reliable window — 4 MiB on a lane admitting 64 KiB — that holds the
/// realtime lane back beyond deficit round robin's bound (a scheduler
/// charging whole messages instead of fragments, or a message fragmented
/// all at once ahead of other lanes), or that arrives altered, twice or
/// out of order. Oracle: the bound of
/// `realtime_under_bulk_keeps_the_scheduling_bound` over a network with
/// 3 % loss, 10 % reordering and 2 % duplication, with a 1 KiB realtime
/// message every tick; each 4 MiB message arrives once, in order, byte for
/// byte.
#[wasm_bindgen_test(unsupported = test)]
fn realtime_beside_4_mib_messages_keeps_the_scheduling_bound() {
    const MESSAGE_BYTES: usize = 4 << 20;
    let network = SimulatedConfig {
        one_way_latency_ms: 30,
        jitter_ms: 5,
        loss_per_10k: 300,
        duplicate_per_10k: 200,
        reorder_per_10k: 1_000,
        reorder_extra_ms: 20,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k: [0; RELIABLE_LANES],
    };
    let worst_delay = network.one_way_latency_ms + network.jitter_ms + network.reorder_extra_ms;
    // Four datagrams a flush: fewer than the bulk window, so the bulk lane
    // always has a fragment ready and only the scheduler leaves lane 0 room.
    let mut config = endpoint_config(4, 12);
    config.reliable.max_message_bytes = MESSAGE_BYTES;
    config.reliable.lanes[bulk().index()].outbound_bytes = 64 * 1024;
    let run = run(
        network,
        51,
        &config,
        &Workload {
            bulk_bytes: MESSAGE_BYTES,
            realtime_bytes: 1024,
            bulk_lead_ms: 0,
            realtime_ms: 40_000,
            delivery: Delivery::Reliable(realtime()),
        },
    );
    let sent = u32::try_from(run.sent.len()).expect("few messages");
    let realtime: Vec<_> = run.delivered.iter().map(|&(index, _)| index).collect();
    assert_eq!(realtime, (0..sent).collect::<Vec<_>>());
    assert!(run.bulk_sent >= 2, "bulk barely ran: {}", run.bulk_sent);
    assert_eq!(run.bulk_delivered, (0..run.bulk_sent).collect::<Vec<_>>());
    assert_scheduling_bound(&run, worst_delay, "4 MiB bulk");
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
    let run = run(
        network,
        21,
        &config,
        &streams(0, 20_000, Delivery::Reliable(realtime())),
    );
    assert_exact(&run);
    for (&(index, delivered_at), &sent_at) in run.delivered.iter().zip(&run.sent) {
        assert!(
            delivered_at - sent_at <= bound,
            "realtime {index} took {} ms; bound {bound}",
            delivered_at - sent_at
        );
    }
}

/// Defect (#268, design §12): an unreliable message retransmitted, sent
/// twice, delivered twice after a network duplicate, dropped by SGL, or
/// queued behind the bulk lane's backlog. Oracle: the wire and the network
/// parameters — with no loss but 5 % duplication and 5 % reordering, every
/// accepted lane-0 unreliable message goes on the wire exactly once, in the
/// flush right after its `send` (deficit round robin at 8:1 and two
/// datagrams per flush), and is delivered exactly once within the
/// network's worst delay and one poll tick of that transmission; the bulk
/// lane stays exact.
#[wasm_bindgen_test(unsupported = test)]
fn unreliable_under_bulk_is_sent_once_and_keeps_the_scheduling_bound() {
    let network = SimulatedConfig {
        one_way_latency_ms: 30,
        jitter_ms: 5,
        loss_per_10k: 0,
        duplicate_per_10k: 500,
        reorder_per_10k: 500,
        reorder_extra_ms: 20,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k: [0; RELIABLE_LANES],
    };
    let worst_delay = network.one_way_latency_ms + network.jitter_ms + network.reorder_extra_ms;
    let run = run(
        network,
        31,
        &endpoint_config(2, 12),
        &streams(0, 20_000, Delivery::Unreliable(realtime())),
    );
    assert_eq!(run.bulk_delivered, (0..run.bulk_sent).collect::<Vec<_>>());
    assert!(run.bulk_sent > 10, "bulk barely ran: {}", run.bulk_sent);
    let mut delivered: Vec<_> = run.delivered.iter().map(|&(index, _)| index).collect();
    delivered.sort_unstable();
    let sent = u32::try_from(run.sent.len()).expect("few messages");
    assert_eq!(
        delivered,
        (0..sent).collect::<Vec<_>>(),
        "each exactly once"
    );
    for &(index, delivered_at) in &run.delivered {
        let sent_at = run.sent[index as usize];
        assert_eq!(
            run.transmissions[&index],
            [sent_at + TICK_MS],
            "unreliable {index} goes out once, in the next flush"
        );
        assert!(
            delivered_at <= sent_at + TICK_MS + worst_delay + TICK_MS,
            "unreliable {index} delivered at {delivered_at}, sent at {sent_at}"
        );
    }
}

/// Defect: an unreliable message retransmitted after loss, or delivered
/// twice when the network duplicates it. Oracle: the wire — over 20 % loss,
/// 10 % duplication and 10 % reordering, every accepted message is
/// transmitted exactly once, and the server delivers only messages that
/// were sent, none twice.
#[wasm_bindgen_test(unsupported = test)]
fn unreliable_over_a_lossy_network_is_at_most_once() {
    let network = SimulatedConfig {
        one_way_latency_ms: 30,
        jitter_ms: 5,
        loss_per_10k: 2_000,
        duplicate_per_10k: 1_000,
        reorder_per_10k: 1_000,
        reorder_extra_ms: 20,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k: [0; RELIABLE_LANES],
    };
    let run = run(
        network,
        41,
        &endpoint_config(64, 64),
        &streams(0, 10_000, Delivery::Unreliable(realtime())),
    );
    let sent = u32::try_from(run.sent.len()).expect("few messages");
    for index in 0..sent {
        assert_eq!(
            run.transmissions.get(&index).map(Vec::len),
            Some(1),
            "unreliable {index} transmitted once"
        );
    }
    let mut delivered: Vec<_> = run.delivered.iter().map(|&(index, _)| index).collect();
    delivered.sort_unstable();
    let before = delivered.len();
    delivered.dedup();
    assert_eq!(
        delivered.len(),
        before,
        "an unreliable message delivered twice"
    );
    assert!(delivered.iter().all(|&index| index < sent));
}
