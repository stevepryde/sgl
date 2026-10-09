use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;

use super::cookie::{ChallengeLimiter, ConfirmReplayCache, CookieKey};
use super::packet::{self, Ack, Item, Kind, Nonces, Parsed};
use super::peer::{CloseGrace, Handshake, Peer};
use super::transport::DatagramTransport;
use crate::{Delivery, DenyReason, DisconnectReason, Lane, ReliableCapacity, SendError};

const MAX_CONFIG_PEERS: usize = 256;
const MAX_CONFIG_DATAGRAMS_PER_POLL: usize = 4_096;
const MAX_CONFIG_PACKETS_PER_PEER_FLUSH: usize = 128;
const MAX_CONFIG_CHALLENGES_PER_POLL: usize = 256;
const MAX_CONFIG_PREFIX_BURST: u16 = 64;
const MAX_CONFIG_INTERVAL_MS: u64 = 120_000;
const MAX_CONFIG_CLOSE_RETRANSMITS: u8 = 8;
const MAX_GLOBAL_RELIABLE_ITEMS: usize = 65_536;
const MAX_GLOBAL_RELIABLE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Three-byte datagram magic supplied by the game.
    pub magic: [u8; 3],
    pub max_peers: usize,
    pub timeout_ms: u64,
    pub keepalive_ms: u64,
    pub handshake_retry_ms: u64,
    pub max_datagrams_per_poll: usize,
    pub max_packets_per_peer_flush: usize,
    pub challenge_responses_per_poll: usize,
    pub challenge_prefix_burst: u16,
    pub challenge_prefix_refill_ms: u64,
    pub close_grace_ms: u64,
    pub close_retransmits: u8,
    pub max_reliable_transmissions: u8,
    pub global_reliable_outbound_items: usize,
    pub global_reliable_outbound_bytes: usize,
    pub global_reliable_inbound_items: usize,
    pub global_reliable_inbound_bytes: usize,
}

impl EndpointConfig {
    /// Timing and bound defaults with caller-supplied datagram magic.
    #[must_use]
    pub const fn new(magic: [u8; 3]) -> Self {
        Self {
            magic,
            max_peers: 24,
            timeout_ms: 10_000,
            keepalive_ms: 200,
            handshake_retry_ms: 200,
            max_datagrams_per_poll: 256,
            max_packets_per_peer_flush: 64,
            challenge_responses_per_poll: 32,
            challenge_prefix_burst: 4,
            challenge_prefix_refill_ms: 1_000,
            close_grace_ms: 1_000,
            close_retransmits: 2,
            max_reliable_transmissions: 12,
            global_reliable_outbound_items: 4_096,
            global_reliable_outbound_bytes: 4 * 1024 * 1024,
            global_reliable_inbound_items: 4_096,
            global_reliable_inbound_bytes: 4 * 1024 * 1024,
        }
    }
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self::new(*b"TST")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRole {
    Server,
    Client,
}

/// Clockless endpoint construction or local connection-start failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    InvalidConfig,
    WrongRole,
    NonceZero,
    ZeroCookieKey,
    PeerIdExhausted,
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfig => "invalid UDP endpoint bounds",
            Self::WrongRole => "endpoint cannot start another client connection",
            Self::NonceZero => "UDP client nonce must be nonzero",
            Self::ZeroCookieKey => "UDP cookie key must not be all zeroes",
            Self::PeerIdExhausted => "UDP peer identity space exhausted",
        })
    }
}

impl std::error::Error for EndpointError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointEvent {
    Connected {
        peer: u64,
    },
    Message {
        peer: u64,
        delivery: Delivery,
        payload: Vec<u8>,
    },
    Denied {
        peer: u64,
        reason: DenyReason,
    },
    Disconnected {
        peer: u64,
        reason: DisconnectReason,
    },
}

fn reliable_event_usage(events: &[EndpointEvent]) -> (usize, usize) {
    events.iter().fold((0, 0), |(items, bytes), event| {
        if let EndpointEvent::Message {
            delivery: Delivery::Reliable(_),
            payload,
            ..
        } = event
        {
            (items.saturating_add(1), bytes.saturating_add(payload.len()))
        } else {
            (items, bytes)
        }
    })
}

fn peer_reliable_event_usage(events: &[EndpointEvent], peer: u64) -> (usize, usize) {
    events.iter().fold((0, 0), |(items, bytes), event| {
        if let EndpointEvent::Message {
            peer: event_peer,
            delivery: Delivery::Reliable(_),
            payload,
        } = event
            && *event_peer == peer
        {
            (items.saturating_add(1), bytes.saturating_add(payload.len()))
        } else {
            (items, bytes)
        }
    })
}

#[derive(Clone, Copy)]
struct InboundLimits {
    global_items: usize,
    global_bytes: usize,
}

#[derive(Clone, Copy)]
struct InboundUsage {
    retained_items: usize,
    retained_bytes: usize,
    delivered_items: usize,
    delivered_bytes: usize,
    peer_delivered_items: usize,
    peer_delivered_bytes: usize,
}

struct StagedPayload {
    reliable: Vec<Vec<u8>>,
    latest: Option<Vec<u8>>,
}

fn receive_payload_items(
    peer: &mut Peer,
    items: packet::ItemIter<'_>,
    mut usage: InboundUsage,
    limits: InboundLimits,
) -> Result<StagedPayload, DisconnectReason> {
    let mut staged_reliable = Vec::new();
    let mut staged_latest = None;
    for item in items {
        match item {
            Item::Reliable {
                sequence,
                more,
                payload,
            } => {
                let before_items = peer.reliable.inbound_items();
                let before_bytes = peer.reliable.inbound_bytes();
                let mut messages = Vec::new();
                peer.reliable
                    .receive(sequence, more, payload, &mut messages)
                    .map_err(|()| DisconnectReason::InboundOverflow)?;
                usage.retained_items = usage
                    .retained_items
                    .saturating_sub(before_items)
                    .saturating_add(peer.reliable.inbound_items());
                usage.retained_bytes = usage
                    .retained_bytes
                    .saturating_sub(before_bytes)
                    .saturating_add(peer.reliable.inbound_bytes());
                let message_items = messages.len();
                let message_bytes = messages.iter().map(Vec::len).sum::<usize>();
                usage.delivered_items = usage.delivered_items.saturating_add(message_items);
                usage.delivered_bytes = usage.delivered_bytes.saturating_add(message_bytes);
                usage.peer_delivered_items =
                    usage.peer_delivered_items.saturating_add(message_items);
                usage.peer_delivered_bytes =
                    usage.peer_delivered_bytes.saturating_add(message_bytes);
                let exceeded = usage.retained_items.saturating_add(usage.delivered_items)
                    > limits.global_items
                    || usage.retained_bytes.saturating_add(usage.delivered_bytes)
                        > limits.global_bytes
                    || peer
                        .reliable
                        .inbound_items()
                        .saturating_add(usage.peer_delivered_items)
                        > crate::RELIABLE_INBOUND_MESSAGES
                    || peer
                        .reliable
                        .inbound_bytes()
                        .saturating_add(usage.peer_delivered_bytes)
                        > crate::RELIABLE_INBOUND_BYTES;
                if exceeded {
                    return Err(DisconnectReason::InboundOverflow);
                }
                staged_reliable.extend(messages);
            }
            Item::Latest { sequence, payload } => {
                if payload.len() > crate::MAX_LATEST_STATE_BYTES {
                    return Err(DisconnectReason::ProtocolViolation);
                }
                if let Some(payload) = peer.latest.receive(sequence, payload) {
                    staged_latest = Some(payload);
                }
            }
        }
    }
    Ok(StagedPayload {
        reliable: staged_reliable,
        latest: staged_latest,
    })
}

pub struct Endpoint<T: DatagramTransport> {
    transport: T,
    config: EndpointConfig,
    role: EndpointRole,
    peers: BTreeMap<u64, Peer>,
    routes: BTreeMap<(SocketAddr, u64), u64>,
    next_peer: Option<u64>,
    receive_buffer: Vec<u8>,
    scratch: Vec<u8>,
    pending_events: VecDeque<EndpointEvent>,
    cookie_key: Option<CookieKey>,
    challenge_limiter: ChallengeLimiter,
    confirm_replays: ConfirmReplayCache,
    remaining_challenges: usize,
    now_ms: u64,
    accepting_connections: bool,
}

impl<T: DatagramTransport> Endpoint<T> {
    /// Creates a server endpoint with caller-supplied cookie-key entropy.
    pub fn server(
        transport: T,
        config: EndpointConfig,
        cookie_key: [u8; 32],
    ) -> Result<Self, EndpointError> {
        if cookie_key == [0; 32] {
            return Err(EndpointError::ZeroCookieKey);
        }
        Self::new(
            transport,
            config,
            EndpointRole::Server,
            Some(CookieKey::new(cookie_key)),
        )
    }

    pub fn client(transport: T, config: EndpointConfig) -> Result<Self, EndpointError> {
        Self::new(transport, config, EndpointRole::Client, None)
    }

    fn new(
        transport: T,
        config: EndpointConfig,
        role: EndpointRole,
        cookie_key: Option<CookieKey>,
    ) -> Result<Self, EndpointError> {
        if !(1..=MAX_CONFIG_PEERS).contains(&config.max_peers)
            || config.timeout_ms == 0
            || config.timeout_ms > MAX_CONFIG_INTERVAL_MS
            || config.keepalive_ms == 0
            || config.keepalive_ms > MAX_CONFIG_INTERVAL_MS
            || config.keepalive_ms >= config.timeout_ms
            || config.handshake_retry_ms == 0
            || config.handshake_retry_ms > MAX_CONFIG_INTERVAL_MS
            || config.handshake_retry_ms >= config.timeout_ms
            || !(1..=MAX_CONFIG_DATAGRAMS_PER_POLL).contains(&config.max_datagrams_per_poll)
            || !(2..=MAX_CONFIG_PACKETS_PER_PEER_FLUSH).contains(&config.max_packets_per_peer_flush)
            || !(1..=MAX_CONFIG_CHALLENGES_PER_POLL).contains(&config.challenge_responses_per_poll)
            || !(1..=MAX_CONFIG_PREFIX_BURST).contains(&config.challenge_prefix_burst)
            || config.challenge_prefix_refill_ms == 0
            || config.challenge_prefix_refill_ms > MAX_CONFIG_INTERVAL_MS
            || config.close_grace_ms == 0
            || config.close_grace_ms > MAX_CONFIG_INTERVAL_MS
            || !(1..=MAX_CONFIG_CLOSE_RETRANSMITS).contains(&config.close_retransmits)
            || !(1..=64).contains(&config.max_reliable_transmissions)
            || !(1..=MAX_GLOBAL_RELIABLE_ITEMS).contains(&config.global_reliable_outbound_items)
            || !(1..=MAX_GLOBAL_RELIABLE_BYTES).contains(&config.global_reliable_outbound_bytes)
            || !(1..=MAX_GLOBAL_RELIABLE_ITEMS).contains(&config.global_reliable_inbound_items)
            || !(1..=MAX_GLOBAL_RELIABLE_BYTES).contains(&config.global_reliable_inbound_bytes)
        {
            return Err(EndpointError::InvalidConfig);
        }
        let remaining_challenges = config.challenge_responses_per_poll;
        Ok(Self {
            transport,
            config,
            role,
            peers: BTreeMap::new(),
            routes: BTreeMap::new(),
            next_peer: Some(1),
            receive_buffer: vec![0; packet::DATAGRAM_BYTES + 1],
            scratch: Vec::with_capacity(packet::DATAGRAM_BYTES),
            pending_events: VecDeque::new(),
            cookie_key,
            challenge_limiter: ChallengeLimiter::default(),
            confirm_replays: ConfirmReplayCache::default(),
            remaining_challenges,
            now_ms: 0,
            accepting_connections: true,
        })
    }

    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Returns the number of allocated peers. Handshake requests never change it.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Returns the socket-observed address for an allocated peer.
    #[must_use]
    pub fn peer_addr(&self, peer: u64) -> Option<SocketAddr> {
        self.peers.get(&peer).map(|state| state.addr)
    }

    /// Returns one peer's shared RTT estimate.
    #[must_use]
    pub fn rtt(&self, peer: u64) -> crate::RttEstimate {
        self.peers
            .get(&peer)
            .map_or(crate::RttEstimate::default(), Peer::rtt)
    }

    /// Starts a connection with a caller-supplied nonzero nonce.
    pub fn start_connect(
        &mut self,
        server: SocketAddr,
        now_ms: u64,
        client_nonce: u64,
    ) -> Result<u64, EndpointError> {
        if self.role != EndpointRole::Client || !self.peers.is_empty() {
            return Err(EndpointError::WrongRole);
        }
        if client_nonce == 0 {
            return Err(EndpointError::NonceZero);
        }
        self.now_ms = self.now_ms.max(now_ms);
        let now_ms = self.now_ms;
        let id = self.allocate_id()?;
        let peer = Peer::new(
            server,
            Nonces {
                client: client_nonce,
                server: 0,
            },
            Handshake::ClientRequest,
            now_ms,
        );
        self.routes.insert((server, client_nonce), id);
        self.peers.insert(id, peer);
        self.send_control(
            server,
            Kind::ConnectRequest,
            Nonces {
                client: client_nonce,
                server: 0,
            },
            now_ms,
        );
        Ok(id)
    }

    fn allocate_id(&mut self) -> Result<u64, EndpointError> {
        let id = self.next_peer.ok_or(EndpointError::PeerIdExhausted)?;
        self.next_peer = id.checked_add(1);
        Ok(id)
    }

    /// Queues a payload for `peer`, or refuses it whole: a full lane or
    /// global ceiling returns [`SendError::WouldBlock`] and leaves every
    /// peer connected.
    pub fn send(&mut self, peer: u64, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        if self.peers.get(&peer).is_none_or(Peer::is_closing) {
            return Err(if self.role == EndpointRole::Client {
                SendError::Disconnected
            } else {
                SendError::UnknownConnection
            });
        }
        match delivery {
            Delivery::Reliable(_) => {
                if payload.len() > crate::MAX_RELIABLE_MESSAGE_BYTES {
                    return Err(SendError::PayloadTooLarge);
                }
                let fragments = payload.len().max(1).div_ceil(packet::MAX_ITEM_PAYLOAD);
                let items: usize = self
                    .peers
                    .values()
                    .map(|state| state.reliable.outbound_items())
                    .sum();
                let bytes: usize = self
                    .peers
                    .values()
                    .map(|state| state.reliable.outbound_bytes())
                    .sum();
                if items
                    .checked_add(fragments)
                    .is_none_or(|total| total > self.config.global_reliable_outbound_items)
                    || bytes
                        .checked_add(payload.len())
                        .is_none_or(|total| total > self.config.global_reliable_outbound_bytes)
                {
                    return Err(SendError::WouldBlock);
                }
                let state = self.peers.get_mut(&peer).expect("checked peer exists");
                state
                    .reliable
                    .enqueue(payload, packet::MAX_ITEM_PAYLOAD)
                    .map_err(|()| SendError::WouldBlock)
            }
            Delivery::LatestState => {
                if payload.len() > crate::MAX_LATEST_STATE_BYTES {
                    return Err(SendError::PayloadTooLarge);
                }
                let state = self.peers.get_mut(&peer).expect("checked peer exists");
                state.latest.replace(payload);
                Ok(())
            }
        }
    }

    /// What `lane` of `peer` admits now: the peer's own message, fragment and
    /// byte allowances and the endpoint's global outbound ceilings, whichever
    /// binds first. All zeros for an unknown or closing peer.
    #[must_use]
    pub fn capacity(&self, peer: u64, _lane: Lane) -> ReliableCapacity {
        let Some(state) = self.peers.get(&peer).filter(|state| !state.is_closing()) else {
            return ReliableCapacity::default();
        };
        let (global_items, global_bytes) =
            self.peers
                .values()
                .fold((0_usize, 0_usize), |(items, bytes), state| {
                    (
                        items.saturating_add(state.reliable.outbound_items()),
                        bytes.saturating_add(state.reliable.outbound_bytes()),
                    )
                });
        let (messages, fragments, bytes) = state.reliable.outbound_allowance();
        let fragments = fragments.min(
            self.config
                .global_reliable_outbound_items
                .saturating_sub(global_items),
        );
        let bytes = bytes
            .min(
                self.config
                    .global_reliable_outbound_bytes
                    .saturating_sub(global_bytes),
            )
            .min(fragments.saturating_mul(packet::MAX_ITEM_PAYLOAD));
        // Every message takes at least one fragment.
        ReliableCapacity::remaining(messages.min(fragments), bytes)
    }

    pub fn poll(&mut self, now_ms: u64) -> Vec<EndpointEvent> {
        self.now_ms = self.now_ms.max(now_ms);
        let now_ms = self.now_ms;
        self.remaining_challenges = self.config.challenge_responses_per_poll;
        let mut events: Vec<_> = self.pending_events.drain(..).collect();
        let mut buffer = std::mem::take(&mut self.receive_buffer);
        for _ in 0..self.config.max_datagrams_per_poll {
            let Some((length, source)) = self.transport.receive(&mut buffer, now_ms) else {
                break;
            };
            if length <= packet::DATAGRAM_BYTES {
                self.handle_datagram(&buffer[..length], source, now_ms, &mut events);
            }
        }
        self.receive_buffer = buffer;

        self.finish_closing(now_ms);
        let expired: Vec<_> = self
            .peers
            .iter()
            .filter_map(|(&id, peer)| {
                (!peer.is_closing()
                    && now_ms.saturating_sub(peer.last_receive_ms) >= self.config.timeout_ms)
                    .then_some(id)
            })
            .collect();
        for id in expired {
            self.remove_peer(id);
            events.push(EndpointEvent::Disconnected {
                peer: id,
                reason: DisconnectReason::TimedOut,
            });
        }
        events
    }

    pub fn flush(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        let now_ms = self.now_ms;
        let ids: Vec<_> = self.peers.keys().copied().collect();
        for id in ids {
            self.flush_peer(id, now_ms);
        }
        self.finish_closing(now_ms);
    }

    pub fn disconnect(&mut self, peer: u64, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        let now_ms = self.now_ms;
        let Some(handshake) = self.peers.get(&peer).map(|state| state.handshake) else {
            return;
        };
        if handshake != Handshake::Connected {
            self.remove_and_notify(peer, now_ms);
            return;
        }
        if let Some(state) = self.peers.get_mut(&peer) {
            state.latest.clear();
            state.close_grace.get_or_insert(CloseGrace {
                deadline_ms: now_ms.saturating_add(self.config.close_grace_ms),
            });
        }
        self.flush_peer(peer, now_ms);
        self.finish_closing(now_ms);
    }

    pub fn stop_admission(&mut self) {
        self.accepting_connections = false;
    }

    fn flush_peer(&mut self, id: u64, now_ms: u64) {
        let Some(handshake) = self.peers.get(&id).map(|peer| peer.handshake) else {
            return;
        };
        if handshake != Handshake::Connected {
            self.flush_handshake_peer(id, now_ms);
            return;
        }

        let retry_exhausted = self.peers.get(&id).is_some_and(|peer| {
            peer.reliable.retry_exhausted(
                now_ms,
                peer.rto_ms(),
                self.config.max_reliable_transmissions,
            )
        });
        if retry_exhausted {
            self.drop_peer(id, DisconnectReason::TimedOut, true);
            return;
        }

        let max_packets = self.config.max_packets_per_peer_flush;
        // Reserve one datagram for newest-wins state whenever it is pending.
        let reliable_budget = if self
            .peers
            .get(&id)
            .is_some_and(|peer| peer.latest.has_queued() && !peer.is_closing())
        {
            max_packets.saturating_sub(1)
        } else {
            max_packets
        };
        let packet_count = self.flush_reliable_packets(id, now_ms, reliable_budget);
        if self.peers.get(&id).is_some_and(Peer::is_closing) {
            return;
        }
        self.flush_latest_and_keepalive(id, now_ms, packet_count, max_packets);
    }

    fn flush_handshake_peer(&mut self, id: u64, now_ms: u64) {
        let retry = self.peers.get(&id).is_some_and(|peer| {
            now_ms.saturating_sub(peer.last_handshake_send_ms) >= self.config.handshake_retry_ms
        });
        if !retry {
            return;
        }
        let peer = &self.peers[&id];
        let (addr, nonces, kind) = (
            peer.addr,
            peer.nonces,
            match peer.handshake {
                Handshake::ClientRequest => Kind::ConnectRequest,
                Handshake::ClientConfirm => Kind::ConnectConfirm,
                Handshake::Connected => return,
            },
        );
        self.send_control(addr, kind, nonces, now_ms);
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.last_handshake_send_ms = now_ms;
            peer.last_send_ms = now_ms;
        }
    }

    fn flush_reliable_packets(&mut self, id: u64, now_ms: u64, budget: usize) -> usize {
        let mut packet_count = 0;
        while packet_count < budget {
            let sequence = {
                let peer = self.peers.get_mut(&id).expect("peer exists");
                let mut due = Vec::new();
                if peer.is_closing() {
                    let max_transmissions = self.config.close_retransmits.saturating_add(1);
                    peer.reliable
                        .due_bounded(now_ms, peer.rto_ms(), max_transmissions, &mut due);
                } else {
                    peer.reliable.due_bounded(
                        now_ms,
                        peer.rto_ms(),
                        self.config.max_reliable_transmissions,
                        &mut due,
                    );
                }
                due.into_iter().next().or_else(|| peer.reliable.admit())
            };
            let Some(sequence) = sequence else { break };
            let (addr, nonces, ack, more, bytes) = {
                let peer = &self.peers[&id];
                let slot = peer.reliable.slot(sequence).expect("admitted slot exists");
                (
                    peer.addr,
                    peer.nonces,
                    peer.reliable.ack(),
                    slot.more,
                    slot.bytes.clone(),
                )
            };
            packet::begin_payload(&mut self.scratch, self.config.magic, nonces, ack);
            packet::push_reliable(&mut self.scratch, sequence, more, &bytes);
            self.transport.send(addr, &self.scratch, now_ms);
            let peer = self.peers.get_mut(&id).expect("peer exists");
            peer.reliable.mark_sent(sequence, now_ms);
            peer.reliable.ack_dirty = false;
            peer.last_send_ms = now_ms;
            packet_count += 1;
        }
        packet_count
    }

    fn flush_latest_and_keepalive(
        &mut self,
        id: u64,
        now_ms: u64,
        mut packet_count: usize,
        max_packets: usize,
    ) {
        if packet_count < max_packets {
            let latest = self.peers.get_mut(&id).and_then(|peer| peer.latest.take());
            if let Some((sequence, payload)) = latest {
                let peer = &self.peers[&id];
                packet::begin_payload(
                    &mut self.scratch,
                    self.config.magic,
                    peer.nonces,
                    peer.reliable.ack(),
                );
                packet::push_latest(&mut self.scratch, sequence, &payload);
                self.transport.send(peer.addr, &self.scratch, now_ms);
                let peer = self.peers.get_mut(&id).expect("peer exists");
                peer.reliable.ack_dirty = false;
                peer.last_send_ms = now_ms;
                packet_count += 1;
            }
        }

        let keepalive = self.peers.get(&id).is_some_and(|peer| {
            peer.reliable.ack_dirty
                || (packet_count == 0
                    && now_ms.saturating_sub(peer.last_send_ms) >= self.config.keepalive_ms)
        });
        if keepalive && packet_count < max_packets {
            let peer = &self.peers[&id];
            packet::begin_payload(
                &mut self.scratch,
                self.config.magic,
                peer.nonces,
                peer.reliable.ack(),
            );
            self.transport.send(peer.addr, &self.scratch, now_ms);
            let peer = self.peers.get_mut(&id).expect("peer exists");
            peer.reliable.ack_dirty = false;
            peer.last_send_ms = now_ms;
        }
    }

    fn handle_datagram(
        &mut self,
        bytes: &[u8],
        source: SocketAddr,
        now_ms: u64,
        events: &mut Vec<EndpointEvent>,
    ) {
        let Some(parsed) = packet::parse(bytes, self.config.magic) else {
            if let Some(nonces) = packet::nonces(bytes, self.config.magic)
                && let Some(id) = self.match_peer(source, nonces)
            {
                self.drop_peer(id, DisconnectReason::ProtocolViolation, true);
            }
            return;
        };
        match parsed {
            Parsed::Control { kind, nonces } => {
                self.handle_control(kind, nonces, source, now_ms, events);
            }
            Parsed::Payload { nonces, ack, items } => {
                self.handle_payload(nonces, ack, items, source, now_ms, events);
            }
        }
    }

    fn handle_control(
        &mut self,
        kind: Kind,
        nonces: Nonces,
        source: SocketAddr,
        now_ms: u64,
        events: &mut Vec<EndpointEvent>,
    ) {
        match kind {
            Kind::ConnectRequest
                if self.role == EndpointRole::Server
                    && self.accepting_connections
                    && nonces.server == 0 =>
            {
                self.handle_connect_request(nonces.client, source, now_ms);
            }
            Kind::ConnectChallenge if self.role == EndpointRole::Client && nonces.server != 0 => {
                let Some(&id) = self.routes.get(&(source, nonces.client)) else {
                    return;
                };
                let peer = self.peers.get_mut(&id).expect("route points to peer");
                if peer.handshake == Handshake::Connected {
                    return;
                }
                peer.nonces = nonces;
                peer.handshake = Handshake::ClientConfirm;
                peer.last_receive_ms = now_ms;
                self.send_control(source, Kind::ConnectConfirm, nonces, now_ms);
            }
            Kind::ConnectConfirm
                if self.role == EndpointRole::Server && self.accepting_connections =>
            {
                self.handle_connect_confirm(nonces, source, now_ms, events);
            }
            Kind::ConnectAccept if self.role == EndpointRole::Client => {
                let Some(&id) = self.routes.get(&(source, nonces.client)) else {
                    return;
                };
                let peer = self.peers.get_mut(&id).expect("route points to peer");
                if peer.nonces != nonces {
                    return;
                }
                peer.last_receive_ms = now_ms;
                if peer.handshake != Handshake::Connected {
                    peer.handshake = Handshake::Connected;
                    events.push(EndpointEvent::Connected { peer: id });
                }
            }
            Kind::ConnectDeny if self.role == EndpointRole::Client => {
                let Some(&id) = self.routes.get(&(source, nonces.client)) else {
                    return;
                };
                self.remove_peer(id);
                events.push(EndpointEvent::Denied {
                    peer: id,
                    reason: DenyReason::ServerFull,
                });
            }
            Kind::Disconnect => {
                let Some(id) = self.match_peer(source, nonces) else {
                    return;
                };
                self.remove_peer(id);
                events.push(EndpointEvent::Disconnected {
                    peer: id,
                    reason: DisconnectReason::Peer,
                });
            }
            _ => {}
        }
    }

    fn handle_connect_request(&mut self, client_nonce: u64, source: SocketAddr, now_ms: u64) {
        if client_nonce == 0 {
            return;
        }
        if self.remaining_challenges == 0
            || !self.challenge_limiter.allow(
                source,
                now_ms,
                self.config.challenge_prefix_burst,
                self.config.challenge_prefix_refill_ms,
            )
        {
            return;
        }
        self.remaining_challenges -= 1;
        if let Some(&id) = self.routes.get(&(source, client_nonce)) {
            let nonces = self.peers[&id].nonces;
            self.send_control(source, Kind::ConnectAccept, nonces, now_ms);
            return;
        }
        let cookie = self
            .cookie_key
            .as_ref()
            .expect("server endpoints own a cookie key")
            .issue(source, client_nonce, now_ms);
        self.send_control(
            source,
            Kind::ConnectChallenge,
            Nonces {
                client: client_nonce,
                server: cookie,
            },
            now_ms,
        );
    }

    fn handle_connect_confirm(
        &mut self,
        nonces: Nonces,
        source: SocketAddr,
        now_ms: u64,
        events: &mut Vec<EndpointEvent>,
    ) {
        let Some(cookie_epoch) = self
            .cookie_key
            .as_ref()
            .expect("server endpoints own a cookie key")
            .validate(source, nonces.client, nonces.server, now_ms)
        else {
            return;
        };

        if let Some(&id) = self.routes.get(&(source, nonces.client)) {
            let response = {
                let peer = self.peers.get_mut(&id).expect("route points to peer");
                if peer
                    .verified_cookie_epoch
                    .is_none_or(|verified| cookie_epoch >= verified)
                {
                    peer.nonces = nonces;
                    peer.verified_cookie_epoch = Some(cookie_epoch);
                }
                peer.last_receive_ms = now_ms;
                peer.nonces
            };
            self.confirm_replays
                .remember(source, nonces.client, nonces.server);
            self.send_control(source, Kind::ConnectAccept, response, now_ms);
            return;
        }

        if self
            .confirm_replays
            .contains(source, nonces.client, nonces.server)
        {
            return;
        }
        if self.peers.len() >= self.config.max_peers {
            self.send_control(source, Kind::ConnectDeny, nonces, now_ms);
            return;
        }
        let Ok(id) = self.allocate_id() else {
            self.send_control(source, Kind::ConnectDeny, nonces, now_ms);
            return;
        };
        let mut peer = Peer::new(source, nonces, Handshake::Connected, now_ms);
        peer.verified_cookie_epoch = Some(cookie_epoch);
        self.routes.insert((source, nonces.client), id);
        self.peers.insert(id, peer);
        self.confirm_replays
            .remember(source, nonces.client, nonces.server);
        self.send_control(source, Kind::ConnectAccept, nonces, now_ms);
        events.push(EndpointEvent::Connected { peer: id });
    }

    fn handle_payload(
        &mut self,
        nonces: Nonces,
        ack: Ack,
        items: packet::ItemIter<'_>,
        source: SocketAddr,
        now_ms: u64,
        events: &mut Vec<EndpointEvent>,
    ) {
        let Some(id) = self.match_peer(source, nonces) else {
            return;
        };
        if self.peers[&id].handshake != Handshake::Connected {
            return;
        }
        let retained_items: usize = self
            .peers
            .values()
            .map(|state| state.reliable.inbound_items())
            .sum();
        let retained_bytes: usize = self
            .peers
            .values()
            .map(|state| state.reliable.inbound_bytes())
            .sum();
        let (delivered_items, delivered_bytes) = reliable_event_usage(events);
        let (peer_delivered_items, peer_delivered_bytes) = peer_reliable_event_usage(events, id);
        let usage = InboundUsage {
            retained_items,
            retained_bytes,
            delivered_items,
            delivered_bytes,
            peer_delivered_items,
            peer_delivered_bytes,
        };
        let limits = InboundLimits {
            global_items: self.config.global_reliable_inbound_items,
            global_bytes: self.config.global_reliable_inbound_bytes,
        };
        let staged = {
            let peer = self.peers.get_mut(&id).expect("matched peer exists");
            peer.last_receive_ms = now_ms;
            let mut samples = Vec::new();
            peer.reliable.acknowledge(ack, now_ms, &mut samples);
            peer.update_rtt(&samples);
            if peer.is_closing() {
                return;
            }
            receive_payload_items(peer, items, usage, limits)
        };
        let staged = match staged {
            Ok(staged) => staged,
            Err(reason) => {
                self.drop_peer(id, reason, true);
                return;
            }
        };
        events.extend(
            staged
                .reliable
                .into_iter()
                .map(|payload| EndpointEvent::Message {
                    peer: id,
                    delivery: Delivery::RELIABLE_ORDERED,
                    payload,
                }),
        );
        if let Some(payload) = staged.latest {
            if let Some(EndpointEvent::Message {
                payload: existing, ..
            }) = events.iter_mut().rev().find(|event| {
                matches!(
                    event,
                    EndpointEvent::Message {
                        peer,
                        delivery: Delivery::LatestState,
                        ..
                    } if *peer == id
                )
            }) {
                *existing = payload;
            } else {
                events.push(EndpointEvent::Message {
                    peer: id,
                    delivery: Delivery::LatestState,
                    payload,
                });
            }
        }
    }

    fn match_peer(&self, source: SocketAddr, nonces: Nonces) -> Option<u64> {
        let id = *self.routes.get(&(source, nonces.client))?;
        (self.peers.get(&id)?.nonces == nonces).then_some(id)
    }

    fn send_control(&mut self, addr: SocketAddr, kind: Kind, nonces: Nonces, now_ms: u64) {
        packet::control(&mut self.scratch, self.config.magic, kind, nonces);
        self.transport.send(addr, &self.scratch, now_ms);
    }

    fn drop_peer(&mut self, id: u64, reason: DisconnectReason, notify_remote: bool) {
        if let Some(peer) = self.peers.get(&id)
            && notify_remote
        {
            let (addr, nonces) = (peer.addr, peer.nonces);
            self.send_control(addr, Kind::Disconnect, nonces, self.now_ms);
        }
        if self.remove_peer(id).is_some() {
            self.pending_events
                .push_back(EndpointEvent::Disconnected { peer: id, reason });
        }
    }

    fn finish_closing(&mut self, now_ms: u64) {
        let finished: Vec<_> = self
            .peers
            .iter()
            .filter_map(|(&id, peer)| {
                let grace = peer.close_grace?;
                (peer.reliable.outbound_is_idle() || now_ms >= grace.deadline_ms).then_some(id)
            })
            .collect();
        for id in finished {
            self.remove_and_notify(id, now_ms);
        }
    }

    fn remove_and_notify(&mut self, id: u64, now_ms: u64) {
        let Some(peer) = self.remove_peer(id) else {
            return;
        };
        for _ in 0..3 {
            self.send_control(peer.addr, Kind::Disconnect, peer.nonces, now_ms);
        }
    }

    fn remove_peer(&mut self, id: u64) -> Option<Peer> {
        let peer = self.peers.remove(&id)?;
        self.routes.remove(&(peer.addr, peer.nonces.client));
        if self.role == EndpointRole::Server && peer.verified_cookie_epoch.is_some() {
            self.confirm_replays
                .remember(peer.addr, peer.nonces.client, peer.nonces.server);
        }
        Some(peer)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #254: handshake datagrams meant for the other role are ignored — a
    /// server never reacts to a challenge, accept, or deny, and a client
    /// never reacts to a request or confirm.
    #[wasm_bindgen_test(unsupported = test)]
    fn handshake_datagrams_for_the_other_role_are_ignored() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let stranger = address(192, 0, 2, 91, 41_000);
        for kind in [
            Kind::ConnectChallenge,
            Kind::ConnectAccept,
            Kind::ConnectDeny,
        ] {
            server.transport.receive_control(stranger, kind, nonces);
            server.transport.receive_control(source, kind, nonces);
        }
        assert!(server.poll(5).is_empty());
        assert_eq!(server.peer_count(), 1);
        assert!(
            server.transport.sent.is_empty(),
            "{:?}",
            server.transport.sent
        );

        let server_addr = address(192, 0, 2, 1, 7_000);
        let mut client =
            Endpoint::client(RecordingTransport::default(), EndpointConfig::default()).unwrap();
        client.start_connect(server_addr, 0, 77).unwrap();
        client.transport.sent.clear();
        for kind in [Kind::ConnectRequest, Kind::ConnectConfirm] {
            client.transport.receive_control(
                server_addr,
                kind,
                Nonces {
                    client: 77,
                    server: 5,
                },
            );
        }
        assert!(client.poll(1).is_empty());
        assert!(
            client.transport.sent.is_empty(),
            "{:?}",
            client.transport.sent
        );
    }

    /// #254: inbound reliable allowances admit exactly the cap in one poll
    /// and refuse one more, for the per-peer message cap and the global
    /// item cap alike.
    #[wasm_bindgen_test(unsupported = test)]
    fn inbound_reliable_caps_admit_exactly_the_cap() {
        fn items(
            server: &mut Endpoint<RecordingTransport>,
            source: SocketAddr,
            nonces: Nonces,
            count: u16,
        ) {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
            for sequence in 0..count {
                packet::push_reliable(&mut bytes, sequence, false, b"m");
            }
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
        }
        let per_peer = u16::try_from(crate::RELIABLE_INBOUND_MESSAGES).unwrap();
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        items(&mut server, source, nonces, per_peer);
        let events = server.poll(2);
        assert_eq!(
            events.len(),
            usize::from(per_peer),
            "exactly the cap is delivered"
        );
        assert!(
            events
                .iter()
                .all(|e| matches!(e, EndpointEvent::Message { .. }))
        );

        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        items(&mut server, source, nonces, per_peer + 1);
        assert!(server.poll(2).is_empty());
        assert_eq!(
            server.poll(3),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::InboundOverflow,
            }]
        );

        let global = EndpointConfig {
            global_reliable_inbound_items: 3,
            ..EndpointConfig::default()
        };
        let (mut server, source, nonces) = connected_server(global.clone());
        items(&mut server, source, nonces, 3);
        assert_eq!(server.poll(2).len(), 3);
        let (mut server, source, nonces) = connected_server(global);
        items(&mut server, source, nonces, 4);
        assert!(server.poll(2).is_empty());
        assert_eq!(server.peer_count(), 0);
    }

    /// #254, #267: the global outbound allowance admits exactly its item and
    /// byte caps and refuses the next send with `WouldBlock` without touching
    /// the peer.
    #[wasm_bindgen_test(unsupported = test)]
    fn outbound_global_caps_admit_exactly_the_cap() {
        let (mut server, _, _) = connected_server(EndpointConfig {
            global_reliable_outbound_items: 2,
            ..EndpointConfig::default()
        });
        assert_eq!(server.send(1, Delivery::RELIABLE_ORDERED, b"a"), Ok(()));
        assert_eq!(server.send(1, Delivery::RELIABLE_ORDERED, b"b"), Ok(()));
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"c"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.peer_count(), 1);

        let (mut server, _, _) = connected_server(EndpointConfig {
            global_reliable_outbound_bytes: 6,
            ..EndpointConfig::default()
        });
        assert_eq!(server.send(1, Delivery::RELIABLE_ORDERED, b"abc"), Ok(()));
        assert_eq!(server.capacity(1, Lane::DEFAULT).bytes, 3);
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"defg"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.send(1, Delivery::RELIABLE_ORDERED, b"def"), Ok(()));
        assert_eq!(server.peer_count(), 1);
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"g"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.peer_count(), 1);
    }

    /// #254: an idle peer gets a keepalive exactly `keepalive_ms` after the
    /// last datagram and not one millisecond earlier.
    #[wasm_bindgen_test(unsupported = test)]
    fn keepalive_fires_exactly_at_the_interval() {
        let keepalive = EndpointConfig::default().keepalive_ms;
        let (mut server, _, _) = connected_server(EndpointConfig::default());
        server.flush(1_000);
        assert_eq!(server.transport.sent.len(), 1, "overdue keepalive");
        server.transport.sent.clear();
        server.flush(1_000 + keepalive - 1);
        assert!(server.transport.sent.is_empty(), "one millisecond early");
        server.flush(1_000 + keepalive);
        assert_eq!(server.transport.sent.len(), 1, "exactly on time");
    }

    /// #254: a flush sends at most `max_packets_per_peer_flush` datagrams;
    /// the rest wait for the next flush.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_flush_honours_the_per_peer_packet_budget() {
        let (mut server, _, _) = connected_server(EndpointConfig {
            max_packets_per_peer_flush: 2,
            ..EndpointConfig::default()
        });
        let big = vec![7u8; packet::MAX_ITEM_PAYLOAD];
        for _ in 0..3 {
            server.send(1, Delivery::RELIABLE_ORDERED, &big).unwrap();
        }
        server.flush(10);
        assert_eq!(server.transport.sent.len(), 2);
        server.flush(11);
        assert_eq!(server.transport.sent.len(), 3);
    }

    use super::*;

    const MAGIC: [u8; 3] = *b"TST";

    #[derive(Debug)]
    struct ReceivedDatagram {
        source: SocketAddr,
        bytes: Vec<u8>,
    }

    #[derive(Debug)]
    struct SentDatagram {
        destination: SocketAddr,
        bytes: Vec<u8>,
        now_ms: u64,
    }

    #[derive(Default)]
    struct RecordingTransport {
        received: VecDeque<ReceivedDatagram>,
        sent: Vec<SentDatagram>,
    }

    impl RecordingTransport {
        fn receive_control(&mut self, source: SocketAddr, kind: Kind, nonces: Nonces) -> usize {
            let mut bytes = Vec::new();
            packet::control(&mut bytes, MAGIC, kind, nonces);
            let length = bytes.len();
            self.received.push_back(ReceivedDatagram { source, bytes });
            length
        }

        fn receive_ack(&mut self, source: SocketAddr, nonces: Nonces, next: u16) {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next, bits: 0 });
            self.received.push_back(ReceivedDatagram { source, bytes });
        }
    }

    impl DatagramTransport for RecordingTransport {
        fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) {
            self.sent.push(SentDatagram {
                destination,
                bytes: payload.to_vec(),
                now_ms,
            });
        }

        fn receive(&mut self, output: &mut [u8], _now_ms: u64) -> Option<(usize, SocketAddr)> {
            let datagram = self.received.pop_front()?;
            output[..datagram.bytes.len()].copy_from_slice(&datagram.bytes);
            Some((datagram.bytes.len(), datagram.source))
        }
    }

    fn address(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port).into()
    }

    fn parsed_control(datagram: &SentDatagram) -> (Kind, Nonces) {
        let Parsed::Control { kind, nonces } =
            packet::parse(&datagram.bytes, MAGIC).expect("recorded control parses")
        else {
            panic!("expected a control datagram");
        };
        (kind, nonces)
    }

    fn reliable_payload(datagram: &SentDatagram) -> Option<Vec<u8>> {
        let Parsed::Payload { mut items, .. } = packet::parse(&datagram.bytes, MAGIC)? else {
            return None;
        };
        items.find_map(|item| match item {
            Item::Reliable { payload, .. } => Some(payload.to_vec()),
            Item::Latest { .. } => None,
        })
    }

    fn challenge_for(
        endpoint: &Endpoint<RecordingTransport>,
        destination: SocketAddr,
        client_nonce: u64,
    ) -> Nonces {
        endpoint
            .transport
            .sent
            .iter()
            .rev()
            .find_map(|datagram| {
                let (kind, nonces) = parsed_control(datagram);
                (datagram.destination == destination
                    && kind == Kind::ConnectChallenge
                    && nonces.client == client_nonce)
                    .then_some(nonces)
            })
            .expect("challenge was sent")
    }

    fn request(
        endpoint: &mut Endpoint<RecordingTransport>,
        source: SocketAddr,
        client_nonce: u64,
    ) -> usize {
        endpoint.transport.receive_control(
            source,
            Kind::ConnectRequest,
            Nonces {
                client: client_nonce,
                server: 0,
            },
        )
    }

    fn confirm(
        endpoint: &mut Endpoint<RecordingTransport>,
        source: SocketAddr,
        nonces: Nonces,
    ) -> usize {
        endpoint
            .transport
            .receive_control(source, Kind::ConnectConfirm, nonces)
    }

    fn connected_server(
        config: EndpointConfig,
    ) -> (Endpoint<RecordingTransport>, SocketAddr, Nonces) {
        let source = address(192, 0, 2, 90, 40_000);
        let mut server = Endpoint::server(RecordingTransport::default(), config, [9; 32]).unwrap();
        request(&mut server, source, 55);
        assert!(server.poll(0).is_empty());
        let nonces = challenge_for(&server, source, 55);
        confirm(&mut server, source, nonces);
        assert_eq!(server.poll(1), vec![EndpointEvent::Connected { peer: 1 }]);
        server.transport.sent.clear();
        (server, source, nonces)
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn graceful_close_delivers_reliable_reason_before_disconnect_on_ack() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        server
            .send(1, Delivery::RELIABLE_ORDERED, b"ServerClosing")
            .unwrap();
        server.flush(10);
        server.disconnect(1, 10);

        assert_eq!(
            reliable_payload(&server.transport.sent[0]),
            Some(b"ServerClosing".to_vec())
        );
        assert!(server.peers[&1].is_closing());
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"late"),
            Err(SendError::UnknownConnection)
        );

        server.transport.receive_ack(source, nonces, 1);
        assert!(server.poll(50).is_empty());
        assert!(!server.peers.contains_key(&1));
        assert_eq!(
            server
                .transport
                .sent
                .iter()
                .skip(1)
                .map(parsed_control)
                .collect::<Vec<_>>(),
            vec![(Kind::Disconnect, nonces); 3]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn graceful_close_retransmits_twice_then_expires_at_fixed_deadline() {
        let config = EndpointConfig {
            close_grace_ms: 1_000,
            close_retransmits: 2,
            ..EndpointConfig::default()
        };
        let (mut server, _source, nonces) = connected_server(config);
        server
            .send(1, Delivery::RELIABLE_ORDERED, b"Denied: deterministic")
            .unwrap();
        server.flush(10);
        server.disconnect(1, 10);
        server.transport.sent.clear();

        server.flush(5);
        server.flush(209);
        assert!(server.transport.sent.is_empty());
        server.flush(210);
        server.flush(410);
        server.flush(610);
        assert_eq!(
            server
                .transport
                .sent
                .iter()
                .filter_map(reliable_payload)
                .collect::<Vec<_>>(),
            vec![
                b"Denied: deterministic".to_vec(),
                b"Denied: deterministic".to_vec()
            ]
        );
        assert!(server.peers.contains_key(&1));
        assert!(server.poll(1_009).is_empty());
        assert!(server.peers.contains_key(&1));

        assert!(server.poll(1_010).is_empty());
        assert!(!server.peers.contains_key(&1));
        assert_eq!(
            server
                .transport
                .sent
                .iter()
                .filter_map(|datagram| match packet::parse(&datagram.bytes, MAGIC) {
                    Some(Parsed::Control { kind, nonces }) => Some((kind, nonces)),
                    Some(Parsed::Payload { .. }) | None => None,
                })
                .collect::<Vec<_>>(),
            vec![(Kind::Disconnect, nonces); 3]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn stopped_server_admission_allocates_no_new_peer_or_response() {
        let mut server = Endpoint::server(
            RecordingTransport::default(),
            EndpointConfig::default(),
            [10; 32],
        )
        .unwrap();
        server.stop_admission();
        request(&mut server, address(192, 0, 2, 91, 40_001), 77);

        assert!(server.poll(0).is_empty());
        assert!(server.peers.is_empty());
        assert!(server.routes.is_empty());
        assert!(server.transport.sent.is_empty());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn client_disconnect_drains_queued_reliable_data_before_close() {
        let server_address = address(192, 0, 2, 92, 32_167);
        let mut client =
            Endpoint::client(RecordingTransport::default(), EndpointConfig::default()).unwrap();
        let peer = client.start_connect(server_address, 0, 94).unwrap();
        let nonces = Nonces {
            client: client.peers[&peer].nonces.client,
            server: 93,
        };
        client
            .transport
            .receive_control(server_address, Kind::ConnectChallenge, nonces);
        assert!(client.poll(1).is_empty());
        client
            .transport
            .receive_control(server_address, Kind::ConnectAccept, nonces);
        assert_eq!(client.poll(2), vec![EndpointEvent::Connected { peer }]);
        client.transport.sent.clear();
        client
            .send(peer, Delivery::RELIABLE_ORDERED, b"abandoned")
            .unwrap();

        client.disconnect(peer, 3);

        assert!(client.peers[&peer].is_closing());
        assert_eq!(
            client
                .transport
                .sent
                .iter()
                .filter_map(reliable_payload)
                .collect::<Vec<_>>(),
            vec![b"abandoned".to_vec()]
        );
        client.transport.receive_ack(server_address, nonces, 1);
        assert!(client.poll(4).is_empty());
        assert!(!client.peers.contains_key(&peer));
        assert_eq!(
            client
                .transport
                .sent
                .iter()
                .filter_map(|datagram| match packet::parse(&datagram.bytes, MAGIC) {
                    Some(Parsed::Control { kind, nonces }) => Some((kind, nonces)),
                    Some(Parsed::Payload { .. }) | None => None,
                })
                .collect::<Vec<_>>(),
            vec![(Kind::Disconnect, nonces); 3]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn spoof_flood_allocates_nothing_and_another_prefix_still_connects() {
        let config = EndpointConfig {
            max_peers: 1,
            max_datagrams_per_poll: 4_096,
            challenge_responses_per_poll: 16,
            challenge_prefix_burst: 2,
            ..EndpointConfig::default()
        };
        let mut server = Endpoint::server(RecordingTransport::default(), config, [3; 32]).unwrap();
        let mut request_length = 0;
        for index in 0..2_000_u16 {
            let host = u8::try_from(index % 250 + 1).unwrap();
            request_length = request(
                &mut server,
                address(198, 51, 100, host, 20_000 + index),
                u64::from(index) + 1,
            );
        }
        let legitimate = address(203, 0, 113, 7, 40_000);
        request(&mut server, legitimate, 9_001);

        assert!(server.poll(0).is_empty());
        assert!(server.peers.is_empty());
        assert!(server.routes.is_empty());
        assert_eq!(server.challenge_limiter.occupied(), 2);
        assert_eq!(server.transport.sent.len(), 3);
        assert!(
            server
                .transport
                .sent
                .iter()
                .all(|datagram| datagram.bytes.len() <= request_length)
        );
        let sent_after_requests = server.transport.sent.len();
        for now_ms in [200, 400, 1_000, 9_999] {
            server.flush(now_ms);
        }
        assert_eq!(server.transport.sent.len(), sent_after_requests);

        let challenge = challenge_for(&server, legitimate, 9_001);
        confirm(&mut server, legitimate, challenge);
        let events = server.poll(1);
        assert_eq!(events, vec![EndpointEvent::Connected { peer: 1 }]);
        assert_eq!(server.peers.len(), 1);
        assert_eq!(server.routes.len(), 1);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn global_challenge_budget_bounds_many_spoofed_prefixes() {
        let config = EndpointConfig {
            max_datagrams_per_poll: 2_000,
            challenge_responses_per_poll: 7,
            ..EndpointConfig::default()
        };
        let mut server = Endpoint::server(RecordingTransport::default(), config, [4; 32]).unwrap();
        for index in 0..1_000_u16 {
            request(
                &mut server,
                address(
                    10,
                    u8::try_from(index >> 8).unwrap(),
                    u8::try_from(index & 0xff).unwrap(),
                    1,
                    30_000,
                ),
                u64::from(index) + 1,
            );
        }

        assert!(server.poll(0).is_empty());
        assert_eq!(server.transport.sent.len(), 7);
        assert_eq!(server.challenge_limiter.occupied(), 7);
        assert!(server.peers.is_empty());
        assert!(server.routes.is_empty());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn invalid_expired_cross_address_and_cross_nonce_confirms_allocate_nothing() {
        let source = address(192, 0, 2, 10, 40_000);
        let mut server = Endpoint::server(
            RecordingTransport::default(),
            EndpointConfig::default(),
            [5; 32],
        )
        .unwrap();
        request(&mut server, source, 41);
        server.poll(0);
        let challenge = challenge_for(&server, source, 41);
        server.transport.sent.clear();

        confirm(&mut server, address(192, 0, 2, 10, 40_001), challenge);
        confirm(
            &mut server,
            source,
            Nonces {
                client: challenge.client + 1,
                server: challenge.server,
            },
        );
        confirm(
            &mut server,
            source,
            Nonces {
                client: challenge.client,
                server: challenge.server ^ 1,
            },
        );
        confirm(&mut server, source, challenge);

        assert!(
            server
                .poll(super::super::cookie::COOKIE_EPOCH_MS * 2)
                .is_empty()
        );
        assert!(server.transport.sent.is_empty());
        assert!(server.peers.is_empty());
        assert!(server.routes.is_empty());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reordered_epoch_confirm_and_replays_emit_connected_once() {
        let source = address(192, 0, 2, 20, 40_000);
        let mut server = Endpoint::server(
            RecordingTransport::default(),
            EndpointConfig::default(),
            [6; 32],
        )
        .unwrap();
        let boundary = super::super::cookie::COOKIE_EPOCH_MS;
        request(&mut server, source, 77);
        server.poll(boundary - 1);
        let old = challenge_for(&server, source, 77);
        request(&mut server, source, 77);
        server.poll(boundary);
        let current = challenge_for(&server, source, 77);
        assert_ne!(old.server, current.server);

        confirm(&mut server, source, old);
        confirm(&mut server, source, current);
        confirm(&mut server, source, old);
        let events = server.poll(boundary + 1);
        assert_eq!(events, vec![EndpointEvent::Connected { peer: 1 }]);
        assert_eq!(server.peers.len(), 1);
        assert_eq!(server.routes.len(), 1);
        assert_eq!(server.peers[&1].nonces, current);

        request(&mut server, source, 77);
        assert!(server.poll(boundary + 2).is_empty());
        assert_eq!(server.peers.len(), 1);
        assert_eq!(server.routes.len(), 1);
        assert_eq!(server.peers[&1].nonces, current);

        server.disconnect(1, boundary + 3);
        server.transport.sent.clear();
        confirm(&mut server, source, current);
        assert!(server.poll(boundary + 4).is_empty());
        assert!(server.transport.sent.is_empty());
        assert!(server.peers.is_empty());
        assert!(server.routes.is_empty());
        assert_eq!(server.confirm_replays.occupied(), 2);

        confirm(&mut server, source, old);
        assert!(server.poll(boundary + 5).is_empty());
        assert!(server.transport.sent.is_empty());
        assert!(server.peers.is_empty());
        assert!(server.routes.is_empty());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn verified_capacity_denial_does_not_allocate_a_pending_peer() {
        let first = address(192, 0, 2, 30, 40_000);
        let second = address(198, 51, 100, 30, 40_000);
        let config = EndpointConfig {
            max_peers: 1,
            ..EndpointConfig::default()
        };
        let mut server = Endpoint::server(RecordingTransport::default(), config, [7; 32]).unwrap();

        request(&mut server, first, 1);
        server.poll(0);
        let first_cookie = challenge_for(&server, first, 1);
        confirm(&mut server, first, first_cookie);
        assert_eq!(server.poll(1), vec![EndpointEvent::Connected { peer: 1 }]);

        request(&mut server, second, 2);
        server.poll(2);
        let second_cookie = challenge_for(&server, second, 2);
        server.transport.sent.clear();
        let confirm_length = confirm(&mut server, second, second_cookie);
        assert!(server.poll(3).is_empty());

        assert_eq!(server.peers.len(), 1);
        assert_eq!(server.routes.len(), 1);
        assert_eq!(server.transport.sent.len(), 1);
        assert!(server.transport.sent[0].bytes.len() <= confirm_length);
        assert_eq!(
            parsed_control(&server.transport.sent[0]),
            (Kind::ConnectDeny, second_cookie)
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn invalid_config_and_peer_id_exhaustion_fail_without_panicking() {
        assert!(matches!(
            Endpoint::server(
                RecordingTransport::default(),
                EndpointConfig::default(),
                [0; 32]
            ),
            Err(EndpointError::ZeroCookieKey)
        ));
        let invalid = EndpointConfig {
            max_peers: MAX_CONFIG_PEERS + 1,
            ..EndpointConfig::default()
        };
        assert!(matches!(
            Endpoint::client(RecordingTransport::default(), invalid),
            Err(EndpointError::InvalidConfig)
        ));
        let invalid = EndpointConfig {
            max_datagrams_per_poll: MAX_CONFIG_DATAGRAMS_PER_POLL + 1,
            ..EndpointConfig::default()
        };
        assert!(matches!(
            Endpoint::client(RecordingTransport::default(), invalid),
            Err(EndpointError::InvalidConfig)
        ));
        for invalid in [
            EndpointConfig {
                max_packets_per_peer_flush: 1,
                ..EndpointConfig::default()
            },
            EndpointConfig {
                keepalive_ms: EndpointConfig::default().timeout_ms,
                ..EndpointConfig::default()
            },
            EndpointConfig {
                handshake_retry_ms: EndpointConfig::default().timeout_ms,
                ..EndpointConfig::default()
            },
            EndpointConfig {
                max_reliable_transmissions: 0,
                ..EndpointConfig::default()
            },
        ] {
            assert!(matches!(
                Endpoint::client(RecordingTransport::default(), invalid),
                Err(EndpointError::InvalidConfig)
            ));
        }

        let mut client =
            Endpoint::client(RecordingTransport::default(), EndpointConfig::default()).unwrap();
        client.next_peer = Some(u64::MAX);
        assert_eq!(client.allocate_id(), Ok(u64::MAX));
        assert_eq!(client.allocate_id(), Err(EndpointError::PeerIdExhausted));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn client_start_and_timeout_use_monotonic_caller_time_and_typed_events() {
        let server_address = address(192, 0, 2, 93, 32_167);
        let config = EndpointConfig {
            timeout_ms: 100,
            keepalive_ms: 10,
            handshake_retry_ms: 10,
            ..EndpointConfig::default()
        };
        let mut client = Endpoint::client(RecordingTransport::default(), config).unwrap();
        assert!(client.poll(500).is_empty());
        let peer = client.start_connect(server_address, 10, 95).unwrap();
        assert_eq!(client.transport.sent.last().unwrap().now_ms, 500);
        assert_eq!(client.peers[&peer].last_receive_ms, 500);
        assert_eq!(
            client.send(peer + 1, Delivery::RELIABLE_ORDERED, b"missing"),
            Err(SendError::Disconnected)
        );
        assert!(client.poll(599).is_empty());
        assert_eq!(
            client.poll(600),
            vec![EndpointEvent::Disconnected {
                peer,
                reason: DisconnectReason::TimedOut,
            }]
        );
        assert_eq!(
            client.send(peer, Delivery::RELIABLE_ORDERED, b"after-timeout"),
            Err(SendError::Disconnected)
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn inbound_latest_coalesces_to_one_newest_event_per_peer_per_poll() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        for sequence in 0..100_u16 {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
            packet::push_latest(&mut bytes, sequence, &sequence.to_le_bytes());
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
        }

        assert_eq!(
            server.poll(2),
            vec![EndpointEvent::Message {
                peer: 1,
                delivery: Delivery::LatestState,
                payload: 99_u16.to_le_bytes().to_vec(),
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn complete_zero_length_messages_count_toward_per_peer_inbound_cap() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
        for sequence in 0..=u16::try_from(crate::RELIABLE_INBOUND_MESSAGES).unwrap() {
            packet::push_reliable(&mut bytes, sequence, false, b"");
        }
        assert!(bytes.len() <= packet::DATAGRAM_BYTES);
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });

        assert!(server.poll(2).is_empty());
        assert_eq!(server.peer_count(), 0);
        assert_eq!(
            server.poll(3),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::InboundOverflow,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn zero_byte_continuations_across_polls_consume_the_fragment_cap() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        for sequence in 0..u16::try_from(crate::RELIABLE_INBOUND_MESSAGES).unwrap() {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
            packet::push_reliable(&mut bytes, sequence, true, b"");
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
            assert!(server.poll(u64::from(sequence) + 2).is_empty());
            assert_eq!(server.peer_count(), 1);
        }

        let mut overflow = Vec::new();
        packet::begin_payload(&mut overflow, MAGIC, nonces, Ack { next: 0, bits: 0 });
        packet::push_reliable(
            &mut overflow,
            u16::try_from(crate::RELIABLE_INBOUND_MESSAGES).unwrap(),
            true,
            b"",
        );
        server.transport.received.push_back(ReceivedDatagram {
            source,
            bytes: overflow,
        });
        assert!(server.poll(200).is_empty());
        assert_eq!(server.peer_count(), 0);
        assert_eq!(
            server.poll(201),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::InboundOverflow,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn reliable_retry_exhaustion_disconnects_instead_of_wedging() {
        let config = EndpointConfig {
            max_reliable_transmissions: 2,
            ..EndpointConfig::default()
        };
        let (mut server, _source, _nonces) = connected_server(config);
        server
            .send(1, Delivery::RELIABLE_ORDERED, b"never-acked")
            .unwrap();
        server.flush(10);
        server.flush(210);
        assert_eq!(
            server
                .transport
                .sent
                .iter()
                .filter_map(reliable_payload)
                .count(),
            2
        );
        server.flush(410);
        assert_eq!(server.peer_count(), 0);
        assert_eq!(
            server.poll(411),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::TimedOut,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn oversized_datagram_is_dropped_before_parse() {
        let mut server = Endpoint::server(
            RecordingTransport::default(),
            EndpointConfig::default(),
            [11; 32],
        )
        .unwrap();
        server.transport.received.push_back(ReceivedDatagram {
            source: address(192, 0, 2, 1, 40_000),
            bytes: vec![0; packet::DATAGRAM_BYTES + 1],
        });
        assert!(server.poll(0).is_empty());
        assert_eq!(server.peer_count(), 0);
    }

    /// #267: a full per-peer allowance, or a global ceiling a multi-fragment
    /// message cannot fit, refuses the send with `WouldBlock`; the peer stays
    /// connected, nothing is announced, and its queued messages still go out.
    #[wasm_bindgen_test(unsupported = test)]
    fn per_peer_and_global_outbound_saturation_refuses_without_disconnecting() {
        let (mut server, _, _) = connected_server(EndpointConfig::default());
        for _ in 0..crate::RELIABLE_OUTBOUND_MESSAGES {
            server.send(1, Delivery::RELIABLE_ORDERED, b"").unwrap();
        }
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"refused"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.peer_count(), 1);
        assert!(server.poll(2).is_empty(), "no disconnect is announced");
        server.flush(3);
        assert!(
            server
                .transport
                .sent
                .iter()
                .any(|datagram| reliable_payload(datagram).is_some()),
            "the accepted backlog still flushes"
        );

        let config = EndpointConfig {
            global_reliable_outbound_items: 1,
            ..EndpointConfig::default()
        };
        let (mut server, _, _) = connected_server(config);
        assert_eq!(
            server.send(
                1,
                Delivery::RELIABLE_ORDERED,
                &vec![7; packet::MAX_ITEM_PAYLOAD + 1]
            ),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.peer_count(), 1);
        assert_eq!(
            server.send(
                1,
                Delivery::RELIABLE_ORDERED,
                &[7; packet::MAX_ITEM_PAYLOAD]
            ),
            Ok(()),
            "a one-fragment message still fits"
        );
    }

    /// #267: a global ceiling filled by one peer's backlog refuses another
    /// peer's send with `WouldBlock` and closes nobody; once the backlog is
    /// acknowledged the refused send succeeds.
    #[wasm_bindgen_test(unsupported = test)]
    fn global_outbound_saturation_closes_no_peer_and_recovers_after_acks() {
        let config = EndpointConfig {
            global_reliable_outbound_items: 4,
            ..EndpointConfig::default()
        };
        let (mut server, source, nonces) = connected_server(config);
        let second_source = address(192, 0, 2, 91, 40_001);
        request(&mut server, second_source, 66);
        assert!(server.poll(2).is_empty());
        let second_nonces = challenge_for(&server, second_source, 66);
        confirm(&mut server, second_source, second_nonces);
        assert_eq!(server.poll(3), vec![EndpointEvent::Connected { peer: 2 }]);

        for _ in 0..4 {
            server
                .send(1, Delivery::RELIABLE_ORDERED, b"backlog")
                .unwrap();
        }
        assert_eq!(
            server.send(2, Delivery::RELIABLE_ORDERED, b"healthy"),
            Err(SendError::WouldBlock)
        );
        assert!(server.poll(4).is_empty(), "no peer is closed");
        assert_eq!(server.peer_count(), 2);

        server.flush(5);
        server.transport.receive_ack(source, nonces, 4);
        assert!(server.poll(6).is_empty());
        server
            .send(2, Delivery::RELIABLE_ORDERED, b"healthy")
            .expect("the acknowledged backlog returned the allowance");
    }

    /// The highest reliable sequence sent to `destination`, read off the wire.
    #[cfg(not(target_arch = "wasm32"))]
    fn highest_reliable_sequence(
        endpoint: &Endpoint<RecordingTransport>,
        destination: SocketAddr,
    ) -> Option<u16> {
        endpoint
            .transport
            .sent
            .iter()
            .filter(|datagram| datagram.destination == destination)
            .filter_map(|datagram| match packet::parse(&datagram.bytes, MAGIC)? {
                Parsed::Payload { items, .. } => items
                    .filter_map(|item| match item {
                        Item::Reliable { sequence, .. } => Some(sequence),
                        Item::Latest { .. } => None,
                    })
                    .max(),
                Parsed::Control { .. } => None,
            })
            .max()
    }

    /// Defect (#267): a capacity report that disagrees with admission (a
    /// forgotten fragment or global bound, an off-by-one at a cap), or a
    /// refused send that still closes a peer. Oracle: `send`, the atomic
    /// admission path, over random sends, flushes and wire-derived acks on
    /// two peers sharing small global ceilings: the send succeeds exactly
    /// when the capacity read just before it says the payload fits, and a
    /// refusal is `WouldBlock` with both peers still connected.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn capacity_predicts_admission_across_peer_and_global_bounds() {
        use crate::proptest_support::check;
        use proptest::prelude::*;

        #[derive(Clone, Debug)]
        enum Op {
            Send(u64, usize),
            Flush,
            Ack(u64),
        }
        let fragment = packet::MAX_ITEM_PAYLOAD;
        let length = prop_oneof![
            Just(0),
            Just(1),
            Just(fragment),
            Just(fragment + 1),
            Just(2 * fragment + 1),
            0..=crate::MAX_RELIABLE_MESSAGE_BYTES,
        ];
        let op = prop_oneof![
            6 => (1..=2_u64, length).prop_map(|(peer, len)| Op::Send(peer, len)),
            2 => Just(Op::Flush),
            1 => (1..=2_u64).prop_map(Op::Ack),
        ];
        let strategy = (
            1..=300_usize,
            1..=400_000_usize,
            prop::collection::vec(op, 1..300),
        );
        check(strategy, |(items, bytes, ops)| {
            let config = EndpointConfig {
                global_reliable_outbound_items: items,
                global_reliable_outbound_bytes: bytes,
                ..EndpointConfig::default()
            };
            let (mut server, first_source, first_nonces) = connected_server(config);
            let second_source = address(192, 0, 2, 91, 40_001);
            request(&mut server, second_source, 66);
            assert!(server.poll(2).is_empty());
            let second_nonces = challenge_for(&server, second_source, 66);
            confirm(&mut server, second_source, second_nonces);
            assert_eq!(server.poll(3), vec![EndpointEvent::Connected { peer: 2 }]);
            let mut now = 3;
            for op in ops {
                match op {
                    Op::Send(peer, len) => {
                        let capacity = server.capacity(peer, Lane::DEFAULT);
                        let fits = capacity.messages >= 1 && capacity.bytes >= len;
                        let result = server.send(peer, Delivery::RELIABLE_ORDERED, &vec![3; len]);
                        prop_assert_eq!(result.is_ok(), fits, "{:?} for {} bytes", capacity, len);
                        if result.is_err() {
                            prop_assert_eq!(result, Err(SendError::WouldBlock));
                        }
                        prop_assert_eq!(server.peer_count(), 2);
                    }
                    Op::Flush => {
                        now += 1;
                        server.flush(now);
                    }
                    Op::Ack(peer) => {
                        let (source, nonces) = if peer == 1 {
                            (first_source, first_nonces)
                        } else {
                            (second_source, second_nonces)
                        };
                        if let Some(highest) = highest_reliable_sequence(&server, source) {
                            server
                                .transport
                                .receive_ack(source, nonces, highest.wrapping_add(1));
                        }
                        now += 1;
                        prop_assert!(server.poll(now).is_empty());
                    }
                }
            }
            Ok(())
        });
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn inbound_global_overflow_cleans_peer_without_partial_delivery() {
        let config = EndpointConfig {
            global_reliable_inbound_bytes: 1,
            ..EndpointConfig::default()
        };
        let (mut server, source, nonces) = connected_server(config);
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
        packet::push_reliable(&mut bytes, 0, true, b"ab");
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });

        assert!(server.poll(2).is_empty());
        assert_eq!(server.peer_count(), 0);
        assert_eq!(
            server.poll(3),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::InboundOverflow,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn over_cap_reassembly_cleans_peer_without_partial_delivery() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        for sequence in 0..56_u16 {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
            packet::push_reliable(
                &mut bytes,
                sequence,
                true,
                &vec![0; packet::MAX_ITEM_PAYLOAD],
            );
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
        }
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
        packet::push_reliable(&mut bytes, 56, false, &[0; 200]);
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });

        assert!(server.poll(4).is_empty());
        assert_eq!(server.peer_count(), 0);
        assert_eq!(
            server.poll(5),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::InboundOverflow,
            }]
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn malformed_item_suffix_disconnects_without_delivering_valid_prefix() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 0, bits: 0 });
        packet::push_reliable(&mut bytes, 0, false, b"must-not-deliver");
        bytes.extend_from_slice(&[99, 0, 0, 0, 0]);
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });

        assert!(server.poll(6).is_empty());
        assert_eq!(server.peer_count(), 0);
        assert_eq!(
            server.poll(7),
            vec![EndpointEvent::Disconnected {
                peer: 1,
                reason: DisconnectReason::ProtocolViolation,
            }]
        );
    }
}
