//! Local transport refusal is independently observable at the receiver and
//! on the accepted wire sends, before any retransmission timeout elapses.

use super::*;
use crate::udp::simulated::{SimulatedConfig, SimulatedNetwork, SimulatedTransport};
use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Default)]
struct Faults {
    after: usize,
    remaining: usize,
    error: Option<io::ErrorKind>,
    raw_error: Option<i32>,
    attempts: Vec<(u64, bool)>,
    discard: bool,
}

type Control = Rc<RefCell<Faults>>;

struct RefusingTransport {
    inner: SimulatedTransport,
    control: Control,
}

impl DatagramTransport for RefusingTransport {
    fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) -> io::Result<()> {
        let mut fault = self.control.borrow_mut();
        if fault.remaining > 0 {
            if fault.after == 0 {
                fault.remaining -= 1;
                fault.attempts.push((now_ms, false));
                let error = fault.raw_error.map_or_else(
                    || io::Error::from(fault.error.unwrap_or(io::ErrorKind::WouldBlock)),
                    io::Error::from_raw_os_error,
                );
                #[cfg(not(target_arch = "wasm32"))]
                let error = super::super::native::normalize_send_error(error);
                return Err(error);
            }
            fault.after -= 1;
        }
        if !fault.discard {
            self.inner.send(destination, payload, now_ms)?;
        }
        fault.attempts.push((now_ms, true));
        Ok(())
    }

    fn receive(
        &mut self,
        output: &mut [u8],
        now_ms: u64,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        self.inner.receive(output, now_ms)
    }
}

fn transport(network: &SimulatedNetwork, port: u16) -> (RefusingTransport, Control) {
    let control = Control::default();
    (
        RefusingTransport {
            inner: network.transport(addr(port)),
            control: control.clone(),
        },
        control,
    )
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn network() -> SimulatedNetwork {
    SimulatedNetwork::new(
        SimulatedConfig {
            one_way_latency_ms: 0,
            jitter_ms: 0,
            loss_per_10k: 0,
            duplicate_per_10k: 0,
            reorder_per_10k: 0,
            ..SimulatedConfig::default()
        },
        639,
    )
    .unwrap()
}

fn connect(
    server: &mut Endpoint<RefusingTransport>,
    network: &SimulatedNetwork,
    port: u16,
    config: &EndpointConfig,
) -> (Endpoint<RefusingTransport>, Control, u64, u64) {
    let (transport, control) = transport(network, port);
    let mut client = Endpoint::client(transport, config.clone()).unwrap();
    let client_peer = client
        .start_connect(addr(1000), 0, u64::from(port))
        .unwrap();
    assert!(server.poll(0).is_empty());
    assert!(client.poll(0).is_empty());
    let server_peer = match server.poll(0).as_slice() {
        [EndpointEvent::Connected { peer }] => *peer,
        events => panic!("handshake did not connect: {events:?}"),
    };
    assert_eq!(
        client.poll(0),
        vec![EndpointEvent::Connected { peer: client_peer }]
    );
    control.borrow_mut().attempts.clear();
    (client, control, client_peer, server_peer)
}

fn server(
    network: &SimulatedNetwork,
    config: &EndpointConfig,
) -> (Endpoint<RefusingTransport>, Control) {
    let (transport, control) = transport(network, 1000);
    (
        Endpoint::server(transport, config.clone(), [63; 32]).unwrap(),
        control,
    )
}

fn refuse(control: &Control, after: usize, remaining: usize) {
    let mut fault = control.borrow_mut();
    fault.after = after;
    fault.remaining = remaining;
    fault.error = None;
    fault.attempts.clear();
}

fn messages(events: Vec<EndpointEvent>, delivery: Delivery) -> Vec<Vec<u8>> {
    events
        .into_iter()
        .filter_map(|event| match event {
            EndpointEvent::Message {
                delivery: actual,
                payload,
                ..
            } if actual == delivery => Some(payload),
            _ => None,
        })
        .collect()
}

// The first packet carries latest state; the refused second carries a
// reliable first fragment and an unreliable item. Every accepted message
// must arrive on the immediately following flush, not after the ARQ timeout.
#[wasm_bindgen_test(unsupported = test)]
fn a_refused_second_datagram_preserves_every_lane_item() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    let reliable = vec![0xA5; 1500];
    client
        .send(peer, Delivery::LatestState, &vec![0xF0; 1000])
        .unwrap();
    client
        .send(peer, Delivery::RELIABLE_ORDERED, &reliable)
        .unwrap();
    for lane in 0..RELIABLE_LANES {
        for index in 0..12 {
            client
                .send(
                    peer,
                    Delivery::Unreliable(lane_at(lane)),
                    &[u8::try_from(lane).unwrap(), index, 9, 8, 7, 6, 5, 4, 3, 2],
                )
                .unwrap();
        }
    }
    refuse(&faults, 1, 1);
    client.flush(10);
    assert_eq!(
        faults.borrow().attempts,
        [(10, true), (10, false)],
        "the peer stops immediately, including ACK fallback"
    );
    let first = server.poll(10);
    assert_eq!(
        messages(first.clone(), Delivery::LatestState),
        [vec![0xF0; 1000]]
    );
    client.flush(11);
    let second = server.poll(11);
    assert_eq!(
        messages(second.clone(), Delivery::RELIABLE_ORDERED),
        [reliable]
    );
    for lane in 0..RELIABLE_LANES {
        let mut received = messages(first.clone(), Delivery::Unreliable(lane_at(lane)));
        received.extend(messages(
            second.clone(),
            Delivery::Unreliable(lane_at(lane)),
        ));
        received.sort();
        let expected: Vec<_> = (0..12)
            .map(|index| vec![u8::try_from(lane).unwrap(), index, 9, 8, 7, 6, 5, 4, 3, 2])
            .collect();
        assert_eq!(
            received, expected,
            "all unreliable messages arrive once on lane {lane}"
        );
    }
    client.flush(12);
    assert!(server.poll(12).is_empty(), "no duplicated accepted items");
}

// Restoring a refused packet must preserve admission bounds and allow a
// replacement latest state to supersede the old one before any wire send.
#[wasm_bindgen_test(unsupported = test)]
fn refused_work_keeps_queue_capacity_and_latest_coalescing() {
    let net = network();
    let mut config = EndpointConfig::default();
    config.reliable.lanes[0].unreliable_messages = 1;
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    client
        .send(peer, Delivery::LatestState, b"old state")
        .unwrap();
    client
        .send(peer, Delivery::Unreliable(lane_at(0)), b"retained")
        .unwrap();
    refuse(&faults, 0, 1);
    client.flush(10);
    assert_eq!(
        client.send(peer, Delivery::Unreliable(lane_at(0)), b"new message"),
        Err(SendError::WouldBlock)
    );
    client
        .send(peer, Delivery::LatestState, b"new state")
        .unwrap();
    client.flush(11);
    let received = server.poll(11);
    assert_eq!(
        messages(received.clone(), Delivery::LatestState),
        [b"new state".to_vec()]
    );
    assert_eq!(
        messages(received, Delivery::Unreliable(lane_at(0))),
        [b"retained".to_vec()]
    );
    client
        .send(peer, Delivery::Unreliable(lane_at(0)), b"new message")
        .unwrap();
}

// A locally refused resend must not consume the close's one allowed
// retransmission or advance its timeout/backoff. The original datagram is
// accepted locally but lost in the network; its next allowed resend lands.
#[wasm_bindgen_test(unsupported = test)]
fn a_refused_retransmission_remains_due_during_close() {
    let net = network();
    let config = EndpointConfig {
        close_retransmits: 1,
        ..EndpointConfig::default()
    };
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    faults.borrow_mut().discard = true;
    client
        .send(peer, Delivery::RELIABLE_ORDERED, b"retry during close")
        .unwrap();
    client.flush(10);
    client.disconnect(peer, 100);
    assert!(server.poll(100).is_empty());
    faults.borrow_mut().discard = false;
    refuse(&faults, 0, 1);
    client.flush(210);
    assert!(server.poll(210).is_empty());
    client.flush(211);
    assert_eq!(
        messages(server.poll(211), Delivery::RELIABLE_ORDERED),
        [b"retry during close".to_vec()]
    );
    server.flush(211);
    client.poll(211);
    client.flush(212);
    assert!(matches!(
        server.poll(212).as_slice(),
        [EndpointEvent::Disconnected {
            reason: DisconnectReason::Peer,
            ..
        }]
    ));
}

// Both acknowledgements owed for an arrival can be refused locally. They
// must still free the producer's lane immediately after the first success.
#[wasm_bindgen_test(unsupported = test)]
fn refused_ack_datagrams_still_release_sender_admission() {
    let net = network();
    let mut config = EndpointConfig::default();
    config.reliable.lanes[0].outbound_messages = 1;
    let (mut server, faults) = server(&net, &config);
    let (mut client, _, peer, _) = connect(&mut server, &net, 2000, &config);
    client
        .send(peer, Delivery::RELIABLE_ORDERED, b"first")
        .unwrap();
    client.flush(10);
    assert_eq!(
        messages(server.poll(10), Delivery::RELIABLE_ORDERED),
        [b"first".to_vec()]
    );
    refuse(&faults, 0, 2);
    for time in [10, 11] {
        server.flush(time);
        client.poll(time);
        assert_eq!(
            client.send(peer, Delivery::RELIABLE_ORDERED, b"second"),
            Err(SendError::WouldBlock)
        );
    }
    server.flush(12);
    client.poll(12);
    client
        .send(peer, Delivery::RELIABLE_ORDERED, b"second")
        .unwrap();
    client.flush(12);
    assert_eq!(
        messages(server.poll(12), Delivery::RELIABLE_ORDERED),
        [b"second".to_vec()]
    );
}

// A refused keepalive cannot reset last_send: another flush must try again
// before the remote inactivity timer ends a healthy connection.
#[wasm_bindgen_test(unsupported = test)]
fn a_refused_keepalive_is_retried_without_waiting_another_interval() {
    let net = network();
    let config = EndpointConfig {
        timeout_ms: 300,
        keepalive_ms: 100,
        handshake_retry_ms: 50,
        ..EndpointConfig::default()
    };
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, _, _) = connect(&mut server, &net, 2000, &config);
    refuse(&faults, 0, 1);
    client.flush(100);
    client.flush(101);
    assert!(server.poll(101).is_empty());
    assert!(
        server.poll(400).is_empty(),
        "the accepted keepalive renewed remote liveness"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn refused_disconnect_copies_land_after_the_peer_was_removed() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    refuse(&faults, 1, 1);
    client.disconnect(peer, 10);
    assert_eq!(client.peer_count(), 0);
    // Lose the first locally accepted copy by consuming it independently.
    let mut sink = net.transport(addr(1000));
    let mut bytes = [0; 1200];
    assert!(sink.receive(&mut bytes, 10).unwrap().is_some());
    assert!(server.poll(10).is_empty());
    client.flush(11);
    assert!(matches!(
        server.poll(11).as_slice(),
        [EndpointEvent::Disconnected {
            reason: DisconnectReason::Peer,
            ..
        }]
    ));
    assert_eq!(
        faults.borrow().attempts,
        [(10, true), (10, false), (11, true), (11, true)]
    );
}

// Refusal is peer-local: a server must still flush another connected peer
// in this call, even though the first peer's packet remains queued.
#[wasm_bindgen_test(unsupported = test)]
fn one_peers_refusal_does_not_stall_another_peer() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, faults) = server(&net, &config);
    let (mut first, _, _, first_id) = connect(&mut server, &net, 2000, &config);
    let (mut second, _, _, second_id) = connect(&mut server, &net, 3000, &config);
    server
        .send(first_id, Delivery::Unreliable(lane_at(0)), b"first peer")
        .unwrap();
    server
        .send(second_id, Delivery::Unreliable(lane_at(0)), b"second peer")
        .unwrap();
    refuse(&faults, 0, 1);
    server.flush(10);
    assert!(first.poll(10).is_empty());
    assert_eq!(
        messages(second.poll(10), Delivery::Unreliable(lane_at(0))),
        [b"second peer".to_vec()]
    );
    server.flush(11);
    assert_eq!(
        messages(first.poll(11), Delivery::Unreliable(lane_at(0))),
        [b"first peer".to_vec()]
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn a_locally_refused_initial_handshake_retries_on_the_next_flush() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (transport, faults) = transport(&net, 2000);
    refuse(&faults, 0, 1);
    let mut client = Endpoint::client(transport, config).unwrap();
    let peer = client.start_connect(addr(1000), 0, 2000).unwrap();
    assert!(server.poll(0).is_empty());
    client.flush(1);
    assert!(server.poll(1).is_empty());
    assert!(client.poll(1).is_empty());
    assert!(matches!(
        server.poll(1).as_slice(),
        [EndpointEvent::Connected { .. }]
    ));
    assert_eq!(client.poll(1), [EndpointEvent::Connected { peer }]);
}

// Permanent per-datagram errors keep the existing network-loss policy;
// retrying an invalid destination indefinitely would stall later messages.
#[wasm_bindgen_test(unsupported = test)]
fn a_permanent_datagram_error_does_not_retry_unreliable_work() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    client
        .send(
            peer,
            Delivery::Unreliable(lane_at(0)),
            b"rejected destination",
        )
        .unwrap();
    refuse(&faults, 0, 1);
    faults.borrow_mut().error = Some(io::ErrorKind::PermissionDenied);
    client.flush(10);
    client
        .send(peer, Delivery::Unreliable(lane_at(0)), b"next message")
        .unwrap();
    client.flush(11);
    assert_eq!(
        messages(server.poll(11), Delivery::Unreliable(lane_at(0))),
        [b"next message".to_vec()]
    );
}

// Darwin's SDK sys/errno.h defines ENOBUFS as 55. Exercise its real std
// error through the native adapter's normalization and endpoint delivery,
// rather than checking our errno constant against itself.
#[cfg(target_vendor = "apple")]
#[test]
fn native_enobufs_retains_the_refused_payload() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    client
        .send(
            peer,
            Delivery::Unreliable(lane_at(0)),
            b"kernel had no buffers",
        )
        .unwrap();
    refuse(&faults, 0, 1);
    faults.borrow_mut().raw_error = Some(55);
    client.flush(10);
    assert!(server.poll(10).is_empty());
    client.flush(11);
    assert_eq!(
        messages(server.poll(11), Delivery::Unreliable(lane_at(0))),
        [b"kernel had no buffers".to_vec()]
    );
}

// Refused confirmations did not reach the network, so they cannot exhaust
// the four unanswered confirmations allowed for a cookie. Five successive
// local refusals must still allow the next send to admit that same session.
#[wasm_bindgen_test(unsupported = test)]
fn refused_confirmations_do_not_exhaust_a_cookie() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (transport, faults) = transport(&net, 2000);
    let mut client = Endpoint::client(transport, config).unwrap();
    let peer = client.start_connect(addr(1000), 0, 2000).unwrap();
    assert!(server.poll(0).is_empty());
    refuse(&faults, 0, 5);
    assert!(client.poll(0).is_empty());
    for now in 1..5 {
        client.flush(now);
        assert!(server.poll(now).is_empty());
    }
    client.flush(5);
    assert!(matches!(
        server.poll(5).as_slice(),
        [EndpointEvent::Connected { .. }]
    ));
    assert_eq!(client.poll(5), [EndpointEvent::Connected { peer }]);
}

// A late notification must not outlive the remote session's timeout;
// expiry also bounds how long removed routes occupy retry storage.
#[wasm_bindgen_test(unsupported = test)]
fn an_expired_refused_disconnect_is_not_sent_to_a_late_peer() {
    let net = network();
    let config = EndpointConfig::default();
    let (mut server, _) = server(&net, &config);
    let (mut client, faults, peer, _) = connect(&mut server, &net, 2000, &config);
    refuse(&faults, 0, 1);
    client.disconnect(peer, 10);
    client.flush(10 + config.timeout_ms);
    assert!(matches!(
        server.poll(10 + config.timeout_ms).as_slice(),
        [EndpointEvent::Disconnected {
            reason: DisconnectReason::TimedOut,
            ..
        }]
    ));
}
