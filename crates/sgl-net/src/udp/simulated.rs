//! Seeded virtual datagram network for deterministic loss/latency tests.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use super::DatagramTransport;
use super::packet::{self, Item, Parsed};
use crate::RELIABLE_LANES;

const MAX_SIMULATED_DELAY_MS: u64 = 120_000;
const MAX_SIMULATED_DATAGRAMS: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatedConfigError {
    InvalidBounds,
}

impl std::fmt::Display for SimulatedConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid simulated UDP bounds")
    }
}

impl std::error::Error for SimulatedConfigError {}

#[derive(Debug, Clone)]
pub struct SimulatedConfig {
    pub one_way_latency_ms: u64,
    pub jitter_ms: u64,
    /// Drop probability in parts per 10,000.
    pub loss_per_10k: u32,
    /// Duplicate probability in parts per 10,000.
    pub duplicate_per_10k: u32,
    /// Datagrams receiving an additional delay, in parts per 10,000.
    pub reorder_per_10k: u32,
    pub reorder_extra_ms: u64,
    /// Hard bound for datagrams retained by the whole virtual network.
    pub max_in_flight_datagrams: usize,
    /// Extra drop probability, in parts per 10,000, for a datagram that
    /// carries a reliable or unreliable item of each lane, on top of
    /// `loss_per_10k`.
    pub lane_loss_per_10k: [u32; RELIABLE_LANES],
}

impl Default for SimulatedConfig {
    fn default() -> Self {
        Self {
            one_way_latency_ms: 50,
            jitter_ms: 12,
            loss_per_10k: 2_000,
            duplicate_per_10k: 500,
            reorder_per_10k: 1_000,
            reorder_extra_ms: 40,
            max_in_flight_datagrams: 4_096,
            lane_loss_per_10k: [0; RELIABLE_LANES],
        }
    }
}

impl SimulatedConfig {
    fn validate(&self) -> Result<(), SimulatedConfigError> {
        let jitter_width = self
            .jitter_ms
            .checked_mul(2)
            .and_then(|value| value.checked_add(1));
        if self.one_way_latency_ms > MAX_SIMULATED_DELAY_MS
            || self.jitter_ms > MAX_SIMULATED_DELAY_MS
            || self.reorder_extra_ms > MAX_SIMULATED_DELAY_MS
            || self.loss_per_10k > 10_000
            || self.duplicate_per_10k > 10_000
            || self.reorder_per_10k > 10_000
            || self.lane_loss_per_10k.iter().any(|&loss| loss > 10_000)
            || !(1..=MAX_SIMULATED_DATAGRAMS).contains(&self.max_in_flight_datagrams)
            || jitter_width.is_none()
        {
            return Err(SimulatedConfigError::InvalidBounds);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Datagram {
    source: SocketAddr,
    destination: SocketAddr,
    due_ms: u64,
    order: u64,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct Network {
    config: SimulatedConfig,
    rng: u64,
    order: u64,
    datagrams: Vec<Datagram>,
}

impl Network {
    fn random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn chance(&mut self, per_10k: u32) -> bool {
        self.random() % 10_000 < u64::from(per_10k)
    }

    /// Whether the per-lane loss drops a datagram: one draw for each lane
    /// with loss configured whose item it carries.
    fn lane_loss(&mut self, bytes: &[u8]) -> bool {
        let loss = self.config.lane_loss_per_10k;
        if loss == [0; RELIABLE_LANES] || bytes.len() < 3 {
            return false;
        }
        let magic = [bytes[0], bytes[1], bytes[2]];
        let Some(Parsed::Payload { items, .. }) = packet::parse(bytes, magic) else {
            return false;
        };
        let mut lanes = [false; RELIABLE_LANES];
        for item in items {
            if let Item::Reliable { lane, .. } | Item::Unreliable { lane, .. } = item {
                lanes[lane.index()] = true;
            }
        }
        lanes
            .iter()
            .zip(loss)
            .any(|(&carried, loss)| carried && loss > 0 && self.chance(loss))
    }

    fn schedule(&mut self, source: SocketAddr, destination: SocketAddr, bytes: &[u8], now_ms: u64) {
        if bytes.len() > super::MAX_DATAGRAM_BYTES
            || self.datagrams.len() >= self.config.max_in_flight_datagrams
            || self.chance(self.config.loss_per_10k)
            || self.lane_loss(bytes)
        {
            return;
        }
        let copies = 1 + usize::from(self.chance(self.config.duplicate_per_10k));
        for _ in 0..copies {
            if self.datagrams.len() >= self.config.max_in_flight_datagrams {
                break;
            }
            let delay = if self.config.jitter_ms == 0 {
                self.config.one_way_latency_ms
            } else {
                let width = self.config.jitter_ms * 2 + 1;
                let draw = self.random() % width;
                if draw <= self.config.jitter_ms {
                    self.config
                        .one_way_latency_ms
                        .saturating_sub(self.config.jitter_ms - draw)
                } else {
                    self.config
                        .one_way_latency_ms
                        .saturating_add(draw - self.config.jitter_ms)
                }
            };
            let reorder = if self.chance(self.config.reorder_per_10k) {
                self.config.reorder_extra_ms
            } else {
                0
            };
            let delay = delay.saturating_add(reorder);
            self.order = self.order.wrapping_add(1);
            self.datagrams.push(Datagram {
                source,
                destination,
                due_ms: now_ms.saturating_add(delay),
                order: self.order,
                bytes: bytes.to_vec(),
            });
        }
    }

    fn receive(&mut self, destination: SocketAddr, now_ms: u64) -> Option<Datagram> {
        let index = self
            .datagrams
            .iter()
            .enumerate()
            .filter(|(_, item)| item.destination == destination && item.due_ms <= now_ms)
            .min_by_key(|(_, item)| (item.due_ms, item.order))
            .map(|(index, _)| index)?;
        Some(self.datagrams.swap_remove(index))
    }
}

#[derive(Clone)]
pub struct SimulatedTransport {
    local: SocketAddr,
    network: Arc<Mutex<Network>>,
}

/// Shared virtual datagram network that can create any number of endpoints.
#[derive(Clone)]
pub struct SimulatedNetwork {
    network: Arc<Mutex<Network>>,
}

impl SimulatedNetwork {
    pub fn new(config: SimulatedConfig, seed: u64) -> Result<Self, SimulatedConfigError> {
        config.validate()?;
        Ok(Self {
            network: Arc::new(Mutex::new(Network {
                config,
                rng: seed.max(1),
                order: 0,
                datagrams: Vec::new(),
            })),
        })
    }

    #[must_use]
    pub fn transport(&self, local: SocketAddr) -> SimulatedTransport {
        SimulatedTransport {
            local,
            network: Arc::clone(&self.network),
        }
    }
}

impl DatagramTransport for SimulatedTransport {
    fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) {
        self.network
            .lock()
            .expect("simulated network poisoned")
            .schedule(self.local, destination, payload, now_ms);
    }

    fn receive(&mut self, output: &mut [u8], now_ms: u64) -> Option<(usize, SocketAddr)> {
        let item = self
            .network
            .lock()
            .expect("simulated network poisoned")
            .receive(self.local, now_ms)?;
        if item.bytes.len() > output.len() {
            return None;
        }
        output[..item.bytes.len()].copy_from_slice(&item.bytes);
        Some((item.bytes.len(), item.source))
    }
}

pub fn pair(
    first: SocketAddr,
    second: SocketAddr,
    config: SimulatedConfig,
    seed: u64,
) -> Result<(SimulatedTransport, SimulatedTransport), SimulatedConfigError> {
    let network = SimulatedNetwork::new(config, seed)?;
    Ok((network.transport(first), network.transport(second)))
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;
    use crate::Delivery;
    use crate::udp::{Endpoint, EndpointConfig, EndpointEvent};

    fn address(port: u16) -> SocketAddr {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reliable_close_reason_arrives_before_udp_disconnect() {
        let server_addr = address(32_167);
        let client_addr = address(40_000);
        let (server_transport, client_transport) = pair(
            server_addr,
            client_addr,
            SimulatedConfig {
                one_way_latency_ms: 0,
                jitter_ms: 0,
                loss_per_10k: 0,
                duplicate_per_10k: 0,
                reorder_per_10k: 0,
                reorder_extra_ms: 0,
                max_in_flight_datagrams: 256,
                lane_loss_per_10k: [0; RELIABLE_LANES],
            },
            21,
        )
        .unwrap();
        let config = EndpointConfig::default();
        let mut server = Endpoint::server(server_transport, config.clone(), [21; 32]).unwrap();
        let mut client = Endpoint::client(client_transport, config).unwrap();
        let client_peer = client.start_connect(server_addr, 0, 1).unwrap();

        assert!(server.poll(0).is_empty());
        assert!(client.poll(0).is_empty());
        let connected = server.poll(0);
        let EndpointEvent::Connected { peer: server_peer } = connected[0] else {
            panic!("server did not connect");
        };
        assert_eq!(
            client.poll(0),
            vec![EndpointEvent::Connected { peer: client_peer }]
        );

        server
            .send(server_peer, Delivery::RELIABLE_ORDERED, b"ServerClosing")
            .unwrap();
        server.flush(1);
        server.disconnect(server_peer, 1);
        assert_eq!(
            client.poll(1),
            vec![EndpointEvent::Message {
                peer: client_peer,
                delivery: Delivery::RELIABLE_ORDERED,
                payload: b"ServerClosing".to_vec(),
            }]
        );

        client.flush(1);
        assert!(server.poll(1).is_empty());
        assert_eq!(
            client.poll(1),
            vec![EndpointEvent::Disconnected {
                peer: client_peer,
                reason: crate::DisconnectReason::Peer,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn client_reliable_data_drains_before_udp_disconnect() {
        let server_addr = address(32_168);
        let client_addr = address(40_001);
        let network_config = SimulatedConfig {
            one_way_latency_ms: 0,
            jitter_ms: 0,
            loss_per_10k: 0,
            duplicate_per_10k: 0,
            reorder_per_10k: 0,
            reorder_extra_ms: 0,
            max_in_flight_datagrams: 256,
            lane_loss_per_10k: [0; RELIABLE_LANES],
        };
        let (server_transport, client_transport) =
            pair(server_addr, client_addr, network_config, 22).unwrap();
        let config = EndpointConfig::default();
        let mut server = Endpoint::server(server_transport, config.clone(), [22; 32]).unwrap();
        let mut client = Endpoint::client(client_transport, config).unwrap();
        let client_peer = client.start_connect(server_addr, 0, 2).unwrap();
        assert!(server.poll(0).is_empty());
        assert!(client.poll(0).is_empty());
        let server_peer = match server.poll(0).as_slice() {
            [EndpointEvent::Connected { peer }] => *peer,
            events => panic!("server did not connect: {events:?}"),
        };
        assert_eq!(
            client.poll(0),
            vec![EndpointEvent::Connected { peer: client_peer }]
        );

        client
            .send(client_peer, Delivery::RELIABLE_ORDERED, b"ClientClosing")
            .unwrap();
        client.disconnect(client_peer, 1);
        let events = server.poll(1);
        assert_eq!(
            events,
            vec![EndpointEvent::Message {
                peer: server_peer,
                delivery: Delivery::RELIABLE_ORDERED,
                payload: b"ClientClosing".to_vec(),
            }]
        );
        server.flush(1);
        assert!(client.poll(1).is_empty());
        assert_eq!(
            server.poll(1),
            vec![EndpointEvent::Disconnected {
                peer: server_peer,
                reason: crate::DisconnectReason::Peer,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reliable_exact_order_and_latest_progress_survive_twenty_percent_loss_reorder_and_duplication()
     {
        let server_addr = address(32167);
        let client_addr = address(40000);
        let (server_transport, client_transport) =
            pair(server_addr, client_addr, SimulatedConfig::default(), 0x5eed).unwrap();
        let config = EndpointConfig::default();
        let mut server = Endpoint::server(server_transport, config.clone(), [1; 32]).unwrap();
        let mut client = Endpoint::client(client_transport, config).unwrap();
        let client_peer = client.start_connect(server_addr, 0, 1).unwrap();
        let mut connected = false;
        let mut server_peer = None;
        let mut sent = 0_u32;
        let mut reliable = Vec::new();
        let mut latest = Vec::new();
        let mut now = 0_u64;

        while (reliable.len() < 1_000 || sent < 1_000) && now < 90_000 {
            now += 16;
            for event in client.poll(now) {
                if event == (EndpointEvent::Connected { peer: client_peer }) {
                    connected = true;
                }
            }
            for event in server.poll(now) {
                match event {
                    EndpointEvent::Connected { peer } => server_peer = Some(peer),
                    EndpointEvent::Message {
                        delivery: Delivery::RELIABLE_ORDERED,
                        payload,
                        ..
                    } => reliable.push(u32::from_le_bytes(payload.try_into().unwrap())),
                    EndpointEvent::Message {
                        delivery: Delivery::LatestState,
                        payload,
                        ..
                    } => latest.push(u32::from_le_bytes(payload.try_into().unwrap())),
                    _ => {}
                }
            }
            if connected {
                // 125 reliable messages/s is above expected gameplay event
                // volume while remaining below this small protocol's bounded
                // 33-fragment flight-window capacity at 100 ms RTT.
                for _ in 0..2 {
                    if sent >= 1_000 {
                        break;
                    }
                    client
                        .send(client_peer, Delivery::RELIABLE_ORDERED, &sent.to_le_bytes())
                        .unwrap();
                    client
                        .send(client_peer, Delivery::LatestState, &sent.to_le_bytes())
                        .unwrap();
                    sent += 1;
                }
            }
            client.flush(now);
            server.flush(now);
        }

        assert!(server_peer.is_some());
        assert_eq!(reliable, (0..1_000).collect::<Vec<_>>());
        assert!(latest.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(latest.last().is_some_and(|value| *value > 980));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reconnect_nonce_prevents_old_datagrams_from_matching_new_peer() {
        let server_addr = address(32167);
        let client_addr = address(40000);
        let (server_transport, client_transport) = pair(
            server_addr,
            client_addr,
            SimulatedConfig {
                one_way_latency_ms: 0,
                jitter_ms: 0,
                loss_per_10k: 0,
                duplicate_per_10k: 0,
                reorder_per_10k: 0,
                reorder_extra_ms: 0,
                max_in_flight_datagrams: 256,
                lane_loss_per_10k: [0; RELIABLE_LANES],
            },
            9,
        )
        .unwrap();
        let mut server =
            Endpoint::server(server_transport, EndpointConfig::default(), [2; 32]).unwrap();
        let mut first =
            Endpoint::client(client_transport.clone(), EndpointConfig::default()).unwrap();
        let first_id = first.start_connect(server_addr, 0, 1).unwrap();
        for now in 0..10 {
            first.poll(now);
            server.poll(now);
            first.flush(now);
            server.flush(now);
        }
        first
            .send(first_id, Delivery::LatestState, b"stale")
            .unwrap();
        first.flush(11);
        first.disconnect(first_id, 11);

        let mut second = Endpoint::client(client_transport, EndpointConfig::default()).unwrap();
        let second_id = second.start_connect(server_addr, 12, 2).unwrap();
        for now in 12..30 {
            second.poll(now);
            server.poll(now);
            second.flush(now);
            server.flush(now);
        }
        second
            .send(second_id, Delivery::LatestState, b"fresh")
            .unwrap();
        second.flush(31);
        let messages: Vec<_> = server
            .poll(31)
            .into_iter()
            .filter_map(|event| match event {
                EndpointEvent::Message { payload, .. } => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(messages, vec![b"fresh".to_vec()]);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn max_fragmented_message_round_trips_and_latest_uses_reserved_progress() {
        let server_addr = address(32_167);
        let client_addr = address(40_000);
        let config = SimulatedConfig {
            one_way_latency_ms: 0,
            jitter_ms: 0,
            loss_per_10k: 0,
            duplicate_per_10k: 0,
            reorder_per_10k: 0,
            reorder_extra_ms: 0,
            max_in_flight_datagrams: 512,
            lane_loss_per_10k: [0; RELIABLE_LANES],
        };
        let (server_transport, client_transport) =
            pair(server_addr, client_addr, config, 77).unwrap();
        let endpoint_config = EndpointConfig {
            max_packets_per_peer_flush: 64,
            ..EndpointConfig::default()
        };
        let mut server =
            Endpoint::server(server_transport, endpoint_config.clone(), [8; 32]).unwrap();
        let mut client = Endpoint::client(client_transport, endpoint_config).unwrap();
        let client_peer = client.start_connect(server_addr, 0, 88).unwrap();
        let mut server_peer = None;
        for now in 0..4 {
            client.flush(now);
            for event in server.poll(now) {
                if let EndpointEvent::Connected { peer } = event {
                    server_peer = Some(peer);
                }
            }
            server.flush(now);
            client.poll(now);
        }
        assert!(server_peer.is_some());

        let reliable = vec![0x5a; crate::DEFAULT_RELIABLE_MESSAGE_BYTES];
        client
            .send(client_peer, Delivery::RELIABLE_ORDERED, &reliable)
            .unwrap();
        client
            .send(client_peer, Delivery::LatestState, b"old")
            .unwrap();
        client
            .send(client_peer, Delivery::LatestState, b"new")
            .unwrap();
        client.flush(10);

        let events = server.poll(10);
        assert!(events.iter().any(|event| matches!(
            event,
            EndpointEvent::Message {
                delivery: Delivery::LatestState,
                payload,
                ..
            } if payload == b"new"
        )));
        let mut assembled = None;
        for now in 11..200 {
            server.flush(now);
            client.poll(now);
            client.flush(now);
            for event in server.poll(now) {
                if let EndpointEvent::Message {
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                    ..
                } = event
                {
                    assembled = Some(payload);
                }
            }
            if assembled.is_some() {
                break;
            }
        }
        assert_eq!(assembled, Some(reliable));
        assert_eq!(
            client.send(
                client_peer,
                Delivery::RELIABLE_ORDERED,
                &vec![0; crate::DEFAULT_RELIABLE_MESSAGE_BYTES + 1]
            ),
            Err(crate::SendError::PayloadTooLarge)
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn simulated_bounds_reject_invalid_math_and_count_duplicate_copies() {
        let first = address(32_169);
        let second = address(40_002);
        for config in [
            SimulatedConfig {
                jitter_ms: u64::MAX,
                ..SimulatedConfig::default()
            },
            SimulatedConfig {
                loss_per_10k: 10_001,
                ..SimulatedConfig::default()
            },
            SimulatedConfig {
                max_in_flight_datagrams: 0,
                ..SimulatedConfig::default()
            },
            SimulatedConfig {
                lane_loss_per_10k: [0, 0, 10_001, 0],
                ..SimulatedConfig::default()
            },
        ] {
            assert!(matches!(
                pair(first, second, config, 1),
                Err(SimulatedConfigError::InvalidBounds)
            ));
        }

        let (mut transport, _) = pair(
            first,
            second,
            SimulatedConfig {
                one_way_latency_ms: 0,
                jitter_ms: 0,
                loss_per_10k: 0,
                duplicate_per_10k: 10_000,
                reorder_per_10k: 0,
                reorder_extra_ms: 0,
                max_in_flight_datagrams: 1,
                lane_loss_per_10k: [0; RELIABLE_LANES],
            },
            2,
        )
        .unwrap();
        transport.send(second, b"bounded duplicate", 0);
        assert_eq!(transport.network.lock().unwrap().datagrams.len(), 1);
    }
}
