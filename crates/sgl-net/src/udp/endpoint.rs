use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;

use super::cookie::{ChallengeLimiter, ConfirmReplayCache, CookieKey};
use super::packet::{self, Acks, Item, Kind, Nonces, Parsed};
use super::peer::{CloseGrace, Handshake, Outgoing, Peer};
use super::reliable::MAX_RTO_MS;
use super::transport::DatagramTransport;
use crate::lanes::InboundUsage;
use crate::{
    DEFAULT_LANE_INBOUND_BYTES, DEFAULT_LANE_INBOUND_MESSAGES, DEFAULT_LANE_OUTBOUND_BYTES,
    DEFAULT_LANE_OUTBOUND_MESSAGES, Delivery, DenyReason, DisconnectReason, Lane, RELIABLE_LANES,
    ReliableCapacity, ReliableConfig, SendError,
};

const MAX_CONFIG_PEERS: usize = 256;
const MAX_CONFIG_DATAGRAMS_PER_POLL: usize = 4_096;
const MAX_CONFIG_PACKETS_PER_PEER_FLUSH: usize = 128;
const MAX_CONFIG_CHALLENGES_PER_POLL: usize = 256;
const MAX_CONFIG_PREFIX_BURST: u16 = 64;
const MAX_CONFIG_INTERVAL_MS: u64 = 120_000;
const MAX_CONFIG_CLOSE_RETRANSMITS: u8 = 8;
const MAX_GLOBAL_RELIABLE_MESSAGES: usize = 1 << 20;
const MAX_GLOBAL_RELIABLE_BYTES: usize = 1 << 30;
const DEFAULT_PEERS: usize = 24;
const DEFAULT_LANES: usize = DEFAULT_PEERS * RELIABLE_LANES;

#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Three-byte datagram magic supplied by the game.
    pub magic: [u8; 3],
    pub max_peers: usize,
    /// How long a connected peer may go unheard before it is closed with
    /// `TimedOut`. An unacknowledged fragment is resent, with backoff, for
    /// as long as the peer is heard, unless its lane makes no progress for
    /// two seconds more than this, which no stall this tolerates causes.
    pub timeout_ms: u64,
    pub keepalive_ms: u64,
    pub handshake_retry_ms: u64,
    pub max_datagrams_per_poll: usize,
    /// Datagrams sent to one peer per flush, each packing as many items as
    /// fit.
    pub max_packets_per_peer_flush: usize,
    pub challenge_responses_per_poll: usize,
    pub challenge_prefix_burst: u16,
    pub challenge_prefix_refill_ms: u64,
    pub close_grace_ms: u64,
    pub close_retransmits: u8,
    /// Every peer's reliable message cap, lane weights and per-lane bounds.
    pub reliable: ReliableConfig,
    /// Reliable messages held for every peer until acknowledged, across
    /// lanes. A send past it is refused with `WouldBlock`.
    pub global_reliable_outbound_messages: usize,
    /// Reliable bytes held for every peer until acknowledged; at least
    /// `reliable.max_message_bytes`.
    pub global_reliable_outbound_bytes: usize,
    /// Completed reliable messages from every peer in one poll. Past it, a
    /// lane holds its next message until a later poll, as a full lane does.
    pub global_reliable_inbound_messages: usize,
    /// Inbound reliable bytes across peers: each lane's buffered window
    /// (`WINDOW` fragments, including one held for room) and partial
    /// message, plus completed messages in one poll; at least
    /// `reliable.max_message_bytes`, with room beside a message being
    /// reassembled for the lane's window and the other lanes' and peers'
    /// traffic. It is the one inbound bound that closes a peer rather than
    /// holding it: the peer that exceeds it is disconnected with
    /// `InboundOverflow`, so it must cover every lane of `max_peers` for a
    /// healthy peer never to be. It counts bytes received: a reassembly
    /// buffer may reserve up to twice what has arrived, never past its
    /// message's declared total.
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
            reliable: ReliableConfig::DEFAULT,
            // Sized so the shared ceilings do not bind before every lane of
            // the default peer count is full.
            global_reliable_outbound_messages: DEFAULT_LANES * DEFAULT_LANE_OUTBOUND_MESSAGES,
            global_reliable_outbound_bytes: DEFAULT_LANES * DEFAULT_LANE_OUTBOUND_BYTES,
            global_reliable_inbound_messages: DEFAULT_LANES * DEFAULT_LANE_INBOUND_MESSAGES,
            global_reliable_inbound_bytes: DEFAULT_LANES * DEFAULT_LANE_INBOUND_BYTES,
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

/// What one payload datagram may add to this side's inbound reliable state.
struct InboundLimits<'a> {
    reliable: &'a ReliableConfig,
    global_messages: usize,
    global_bytes: usize,
}

impl InboundLimits<'_> {
    /// Admits a completed `len`-byte message on `lane` if the lane's usage
    /// and the poll's global message count leave room, counting it in both.
    /// A refused message is held, not an overflow.
    fn admit(
        &self,
        lane: Lane,
        usage: &mut InboundUsage,
        delivered: &mut (usize, usize),
        len: usize,
    ) -> bool {
        let bounds = &self.reliable.lanes[lane.index()];
        if !usage.admits(len, bounds) || delivered.0 >= self.global_messages {
            return false;
        }
        usage.add(len, bounds);
        *delivered = (delivered.0 + 1, delivered.1 + len);
        true
    }
}

struct StagedPayload {
    /// Reliable and unreliable messages, in item order.
    messages: Vec<(Delivery, Vec<u8>)>,
    latest: Option<Vec<u8>>,
    /// Completed messages and bytes across peers in this poll, with this
    /// datagram's.
    delivered: (usize, usize),
    /// This peer's per-lane completed messages its caller still holds.
    peer_delivered: [InboundUsage; RELIABLE_LANES],
}

/// Applies one datagram's items to `peer`, keeping `retained` (inbound bytes
/// retained across peers) current. Nothing it completes is delivered unless
/// every item is accepted; a message its lane cannot take now is held in
/// the lane's window, not refused.
fn receive_payload_items(
    peer: &mut Peer,
    items: packet::ItemIter<'_>,
    retained: &mut usize,
    mut delivered: (usize, usize),
    limits: &InboundLimits<'_>,
) -> Result<StagedPayload, DisconnectReason> {
    let mut staged_reliable = Vec::new();
    let mut staged_latest = None;
    let mut peer_delivered = peer.delivered;
    for item in items {
        match item {
            Item::Reliable {
                lane,
                sequence,
                fragment,
                payload,
            } => {
                let state = &mut peer.reliable[lane.index()];
                let usage = &mut peer_delivered[lane.index()];
                let before = state.retained_bytes();
                let mut messages = Vec::new();
                let received = state.receive(
                    sequence,
                    fragment,
                    payload,
                    &mut |len| limits.admit(lane, usage, &mut delivered, len),
                    &mut messages,
                );
                *retained = *retained - before + state.retained_bytes();
                received.map_err(|_| DisconnectReason::ProtocolViolation)?;
                staged_reliable.extend(
                    messages
                        .into_iter()
                        .map(|message| (Delivery::Reliable(lane), message)),
                );
                if retained.saturating_add(delivered.1) > limits.global_bytes {
                    return Err(DisconnectReason::InboundOverflow);
                }
            }
            // Unreliable messages wait in no queue here, and each poll
            // reads a bounded number of datagrams, so they need no inbound
            // allowance; network duplicates are dropped.
            Item::Unreliable {
                lane,
                sequence,
                payload,
            } => {
                if payload.len() > crate::MAX_UNRELIABLE_BYTES {
                    return Err(DisconnectReason::ProtocolViolation);
                }
                if peer.unreliable[lane.index()].accept(sequence) {
                    staged_reliable.push((Delivery::Unreliable(lane), payload.to_vec()));
                }
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
        messages: staged_reliable,
        latest: staged_latest,
        delivered,
        peer_delivered,
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
    /// Reliable messages and bytes held for every peer until acknowledged.
    outbound: (usize, usize),
    /// Inbound reliable bytes retained across peers (out-of-order fragments
    /// and partial messages).
    inbound_retained: usize,
    /// Completed reliable messages and bytes delivered in the current poll.
    poll_delivered: (usize, usize),
    /// The held lane released first in the last poll; the next poll starts
    /// after it.
    last_released_first: Option<(u64, usize)>,
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
            || config.reliable.validate().is_err()
            || !(1..=MAX_GLOBAL_RELIABLE_MESSAGES)
                .contains(&config.global_reliable_outbound_messages)
            || !(1..=MAX_GLOBAL_RELIABLE_MESSAGES)
                .contains(&config.global_reliable_inbound_messages)
            // A shared byte ceiling below one message would refuse it forever.
            || !(config.reliable.max_message_bytes..=MAX_GLOBAL_RELIABLE_BYTES)
                .contains(&config.global_reliable_outbound_bytes)
            || !(config.reliable.max_message_bytes..=MAX_GLOBAL_RELIABLE_BYTES)
                .contains(&config.global_reliable_inbound_bytes)
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
            outbound: (0, 0),
            inbound_retained: 0,
            poll_delivered: (0, 0),
            last_released_first: None,
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
            &self.config.reliable,
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
        let Some(state) = self
            .peers
            .get_mut(&peer)
            .filter(|state| !state.is_closing())
        else {
            return Err(if self.role == EndpointRole::Client {
                SendError::Disconnected
            } else {
                SendError::UnknownConnection
            });
        };
        match delivery {
            Delivery::Reliable(lane) => {
                if payload.len() > self.config.reliable.max_message_bytes {
                    return Err(SendError::PayloadTooLarge);
                }
                let bounds = &self.config.reliable.lanes[lane.index()];
                let state = &mut state.reliable[lane.index()];
                if !bounds.outbound_admits(state.held(), payload.len())
                    || self.outbound.0 >= self.config.global_reliable_outbound_messages
                    || self.outbound.1 + payload.len() > self.config.global_reliable_outbound_bytes
                {
                    return Err(SendError::WouldBlock);
                }
                state.enqueue(payload);
                self.outbound = (self.outbound.0 + 1, self.outbound.1 + payload.len());
                Ok(())
            }
            Delivery::Unreliable(lane) => {
                if payload.len() > crate::MAX_UNRELIABLE_BYTES {
                    return Err(SendError::PayloadTooLarge);
                }
                let bounds = &self.config.reliable.lanes[lane.index()];
                let queue = &mut state.unreliable[lane.index()];
                let (messages, bytes) = queue.queued();
                if messages >= bounds.unreliable_messages
                    || bytes + payload.len() > bounds.unreliable_bytes
                {
                    return Err(SendError::WouldBlock);
                }
                queue.push(payload);
                Ok(())
            }
            Delivery::LatestState => {
                if payload.len() > crate::MAX_LATEST_STATE_BYTES {
                    return Err(SendError::PayloadTooLarge);
                }
                state.latest.replace(payload);
                Ok(())
            }
        }
    }

    /// What `lane` of `peer` admits now: the lane's own message and byte
    /// allowances (up to the message cap while the lane holds nothing) and
    /// the endpoint's global outbound ceilings, whichever binds first. All
    /// zeros for an unknown or closing peer.
    #[must_use]
    pub fn capacity(&self, peer: u64, lane: Lane) -> ReliableCapacity {
        let Some(state) = self.peers.get(&peer).filter(|state| !state.is_closing()) else {
            return ReliableCapacity::default();
        };
        let lane = self.config.reliable.lanes[lane.index()].outbound_capacity(
            state.reliable[lane.index()].held(),
            self.config.reliable.max_message_bytes,
        );
        ReliableCapacity::remaining(
            lane.messages.min(
                self.config
                    .global_reliable_outbound_messages
                    .saturating_sub(self.outbound.0),
            ),
            lane.bytes.min(
                self.config
                    .global_reliable_outbound_bytes
                    .saturating_sub(self.outbound.1),
            ),
        )
    }

    /// Receives and returns what arrived. A lane delivers completed reliable
    /// messages up to its inbound bounds and the global message ceiling per
    /// poll; past them it holds the next one, unacknowledged, until a later
    /// poll, so the peer's window closes instead of the peer being closed.
    pub fn poll(&mut self, now_ms: u64) -> Vec<EndpointEvent> {
        // Messages from earlier polls are the caller's now.
        self.poll_within(now_ms, |_, _| InboundUsage::default())
    }

    /// [`Self::poll`] for a caller that still holds completed messages from
    /// earlier polls, `holding(peer, lane)` on each lane: a lane delivers
    /// only what fits its inbound bounds beside them.
    pub(crate) fn poll_within(
        &mut self,
        now_ms: u64,
        holding: impl Fn(u64, Lane) -> InboundUsage,
    ) -> Vec<EndpointEvent> {
        self.now_ms = self.now_ms.max(now_ms);
        let now_ms = self.now_ms;
        self.remaining_challenges = self.config.challenge_responses_per_poll;
        self.poll_delivered = (0, 0);
        for (&id, peer) in &mut self.peers {
            peer.delivered = std::array::from_fn(|index| holding(id, lane_at(index)));
        }
        let mut events: Vec<_> = self.pending_events.drain(..).collect();
        self.release_held(&mut events);
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

    /// Delivers the held messages each lane now has room for, in order. The
    /// held lanes take turns going first, so when the global message
    /// ceiling binds every one of them progresses. A peer whose released
    /// fragments break the framing rules is closed and delivers none.
    fn release_held(&mut self, events: &mut Vec<EndpointEvent>) {
        let held: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.handshake == Handshake::Connected && !peer.is_closing())
            .flat_map(|(&id, peer)| {
                (0..RELIABLE_LANES)
                    .filter(|&index| peer.reliable[index].holding())
                    .map(move |index| (id, index))
            })
            .collect();
        let split = self
            .last_released_first
            .map_or(0, |last| held.partition_point(|&stream| stream <= last));
        let order = held[split..].iter().chain(&held[..split]);
        if let Some(&first) = order.clone().next() {
            self.last_released_first = Some(first);
        }

        let limits = InboundLimits {
            reliable: &self.config.reliable,
            global_messages: self.config.global_reliable_inbound_messages,
            global_bytes: self.config.global_reliable_inbound_bytes,
        };
        let mut released = Vec::new();
        let mut violators = BTreeSet::new();
        for &(id, index) in order {
            if violators.contains(&id) {
                continue;
            }
            let peer = self.peers.get_mut(&id).expect("listed from peers");
            let lane = lane_at(index);
            let state = &mut peer.reliable[index];
            let before = state.retained_bytes();
            let mut messages = Vec::new();
            let consumed = state.consume(
                &mut |len| {
                    limits.admit(
                        lane,
                        &mut peer.delivered[index],
                        &mut self.poll_delivered,
                        len,
                    )
                },
                &mut messages,
            );
            self.inbound_retained = self.inbound_retained - before + state.retained_bytes();
            if consumed.is_err() {
                violators.insert(id);
            }
            released.extend(messages.into_iter().map(|payload| (id, lane, payload)));
        }
        events.extend(
            released
                .into_iter()
                .filter(|(id, _, _)| !violators.contains(id))
                .map(|(peer, lane, payload)| EndpointEvent::Message {
                    peer,
                    delivery: Delivery::Reliable(lane),
                    payload,
                }),
        );
        for id in violators {
            self.drop_peer(id, DisconnectReason::ProtocolViolation, true);
        }
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

    /// Stops admission to `peer` and closes it once what it accepted is
    /// acknowledged, or `close_grace_ms` from now.
    pub fn disconnect(&mut self, peer: u64, now_ms: u64) {
        let deadline_ms = self
            .now_ms
            .max(now_ms)
            .saturating_add(self.config.close_grace_ms);
        self.disconnect_by(peer, now_ms, deadline_ms);
    }

    /// [`Self::disconnect`] for a close its caller began earlier: whatever
    /// is still unacknowledged at `deadline_ms` is abandoned.
    pub(crate) fn disconnect_by(&mut self, peer: u64, now_ms: u64, deadline_ms: u64) {
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
            state.close_grace.get_or_insert(CloseGrace { deadline_ms });
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

        // A peer heard throughout a stall `timeout_ms` tolerates has a
        // fragment resent within `MAX_RTO_MS` of answering again and
        // acknowledged within a round trip, which the measured timeout's
        // ceiling bounds; a lane that waits longer than that holds a
        // fragment the peer will not take.
        let bound_ms = self.config.timeout_ms + 2 * MAX_RTO_MS;
        if self
            .peers
            .get(&id)
            .is_some_and(|peer| !peer.is_closing() && peer.stalled(now_ms, bound_ms))
        {
            self.drop_peer(id, DisconnectReason::TimedOut, true);
            return;
        }

        let max_packets = self.config.max_packets_per_peer_flush;
        let packet_count = self.flush_payloads(id, now_ms, max_packets);
        if !self.peers[&id].is_closing() {
            self.flush_acks_and_keepalive(id, now_ms, packet_count, max_packets);
        }
        self.peers.get_mut(&id).expect("peer exists").end_flush();
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

    /// Sends up to `budget` payload datagrams, packing items into each in
    /// order while the next fits beside every acknowledgement the peer
    /// carries: the pending latest state first, then lane items, the lane of
    /// each chosen by deficit round robin and, within a lane, a due
    /// retransmission first (netcode.md 12, 14). An item that does not fit
    /// starts the next datagram, beside the acknowledgements that fit when
    /// it is too large for all of them; one left when the budget is spent
    /// is neither taken nor charged and waits for a later flush.
    fn flush_payloads(&mut self, id: u64, now_ms: u64, budget: usize) -> usize {
        let peer = self.peers.get_mut(&id).expect("peer exists");
        // A live peer's fragments are resent until acknowledged.
        let max_transmissions = peer
            .is_closing()
            .then(|| self.config.close_retransmits.saturating_add(1));
        let rto_ms = peer.rto_ms();
        // Every lane that has received anything acknowledges in every
        // datagram, so the room beside them is the same all flush.
        let room = packet::DATAGRAM_BYTES
            - packet::BASE_HEADER_LEN
            - packet::acks_len(&peer.acks(packet::ALL_ACKS_LEN));
        let mut latest = peer.latest.take();
        let mut items = Vec::new();
        let mut packet_count = 0;
        while packet_count < budget {
            let mut used = latest
                .as_ref()
                .map_or(0, |(_, payload)| packet::item_len(payload.len()));
            loop {
                let mut scheduler = peer.scheduler.clone();
                let Some(index) = scheduler
                    .next(|lane| peer.peek(lane, now_ms, rto_ms, max_transmissions).is_some())
                else {
                    break;
                };
                let (next, len) = peer
                    .peek(index, now_ms, rto_ms, max_transmissions)
                    .expect("the scheduler picked a sendable lane");
                if used > 0 && used + len > room {
                    break;
                }
                peer.scheduler = scheduler;
                items.push((index, peer.take(index, next, now_ms)));
                used += len;
            }
            if used == 0 {
                break;
            }
            let acks = peer.acks(packet::DATAGRAM_BYTES - packet::BASE_HEADER_LEN - used);
            packet::begin_payload(&mut self.scratch, self.config.magic, peer.nonces, &acks);
            if let Some((sequence, payload)) = latest.take() {
                packet::push_latest(&mut self.scratch, sequence, &payload);
            }
            for (index, outgoing) in items.drain(..) {
                let lane = lane_at(index);
                match outgoing {
                    Outgoing::Reliable(sequence) => {
                        let (fragment, bytes) = peer.reliable[index]
                            .fragment(sequence)
                            .expect("a sent fragment is in flight");
                        packet::push_reliable(&mut self.scratch, lane, sequence, fragment, bytes);
                    }
                    Outgoing::Unreliable(sequence, payload) => {
                        packet::push_unreliable(&mut self.scratch, lane, sequence, &payload);
                    }
                }
            }
            self.transport.send(peer.addr, &self.scratch, now_ms);
            peer.sent_acks(&acks);
            peer.last_send_ms = now_ms;
            packet_count += 1;
        }
        packet_count
    }

    /// Sends the acknowledgements no payload datagram had room for, or a
    /// keepalive when nothing else went out, within the budget.
    fn flush_acks_and_keepalive(
        &mut self,
        id: u64,
        now_ms: u64,
        packet_count: usize,
        max_packets: usize,
    ) {
        let peer = self.peers.get_mut(&id).expect("peer exists");
        let keepalive = peer.ack_dirty()
            || (packet_count == 0
                && now_ms.saturating_sub(peer.last_send_ms) >= self.config.keepalive_ms);
        if keepalive && packet_count < max_packets {
            let acks = peer.acks(packet::ALL_ACKS_LEN);
            packet::begin_payload(&mut self.scratch, self.config.magic, peer.nonces, &acks);
            self.transport.send(peer.addr, &self.scratch, now_ms);
            peer.sent_acks(&acks);
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
            Parsed::Payload {
                nonces,
                acks,
                items,
            } => {
                self.handle_payload(nonces, &acks, items, source, now_ms, events);
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
        let mut peer = Peer::new(
            source,
            nonces,
            Handshake::Connected,
            now_ms,
            &self.config.reliable,
        );
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
        acks: &Acks,
        items: packet::ItemIter<'_>,
        source: SocketAddr,
        now_ms: u64,
        events: &mut Vec<EndpointEvent>,
    ) {
        let Some(id) = self.match_peer(source, nonces) else {
            return;
        };
        let peer = self.peers.get_mut(&id).expect("matched peer exists");
        if peer.handshake != Handshake::Connected {
            return;
        }
        peer.last_receive_ms = now_ms;
        let held = peer.held();
        let samples = peer.acknowledge(acks, now_ms);
        let still_held = peer.held();
        self.outbound = (
            self.outbound.0 - (held.0 - still_held.0),
            self.outbound.1 - (held.1 - still_held.1),
        );
        peer.update_rtt(&samples);
        if peer.is_closing() {
            return;
        }
        let limits = InboundLimits {
            reliable: &self.config.reliable,
            global_messages: self.config.global_reliable_inbound_messages,
            global_bytes: self.config.global_reliable_inbound_bytes,
        };
        let staged = receive_payload_items(
            peer,
            items,
            &mut self.inbound_retained,
            self.poll_delivered,
            &limits,
        );
        let staged = match staged {
            Ok(staged) => staged,
            Err(reason) => {
                self.drop_peer(id, reason, true);
                return;
            }
        };
        peer.delivered = staged.peer_delivered;
        self.poll_delivered = staged.delivered;
        events.extend(staged.messages.into_iter().map(|(delivery, payload)| {
            EndpointEvent::Message {
                peer: id,
                delivery,
                payload,
            }
        }));
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
                (peer.outbound_is_idle() || now_ms >= grace.deadline_ms).then_some(id)
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
        let held = peer.held();
        self.outbound = (self.outbound.0 - held.0, self.outbound.1 - held.1);
        self.inbound_retained -= peer.retained_bytes();
        self.routes.remove(&(peer.addr, peer.nonces.client));
        if self.role == EndpointRole::Server && peer.verified_cookie_epoch.is_some() {
            self.confirm_replays
                .remember(peer.addr, peer.nonces.client, peer.nonces.server);
        }
        Some(peer)
    }
}

fn lane_at(index: usize) -> Lane {
    Lane::new(u8::try_from(index).expect("lane index fits")).expect("index below RELIABLE_LANES")
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
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

    /// Defect (#254, #268): a poll that delivers past a lane's message cap
    /// or the global per-poll cap; or a receiver that closes the peer,
    /// drops, reorders or acknowledges the excess instead of holding it.
    /// Oracle: netcode.md 11 and 12 — a poll delivers exactly the cap; the
    /// next message waits, unacknowledged and reported held on the wire,
    /// and the next poll delivers it in order with the peer still
    /// connected.
    #[wasm_bindgen_test(unsupported = test)]
    fn inbound_message_caps_deliver_exactly_the_cap_and_hold_the_rest() {
        fn items(
            server: &mut Endpoint<RecordingTransport>,
            source: SocketAddr,
            nonces: Nonces,
            count: u16,
        ) {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
            for sequence in 0..count {
                packet::push_reliable(
                    &mut bytes,
                    lane(3),
                    sequence,
                    Fragment::Whole,
                    &sequence.to_le_bytes(),
                );
            }
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
        }
        fn delivered(events: &[EndpointEvent]) -> Vec<u16> {
            events
                .iter()
                .map(|event| match event {
                    EndpointEvent::Message {
                        delivery, payload, ..
                    } if *delivery == Delivery::Reliable(lane(3)) => {
                        u16::from_le_bytes(payload[..].try_into().unwrap())
                    }
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        }
        /// The lane 3 acknowledgement the next flush sends.
        fn ack(server: &mut Endpoint<RecordingTransport>, now_ms: u64) -> packet::Ack {
            server.transport.sent.clear();
            server.flush(now_ms);
            server
                .transport
                .sent
                .iter()
                .find_map(|datagram| match packet::parse(&datagram.bytes, MAGIC) {
                    Some(Parsed::Payload { acks, .. }) => acks[3],
                    _ => None,
                })
                .expect("the lane is acknowledged")
        }

        let cap = u16::try_from(crate::DEFAULT_LANE_INBOUND_MESSAGES).unwrap();
        let global = EndpointConfig {
            global_reliable_inbound_messages: 3,
            ..EndpointConfig::default()
        };
        for (config, cap) in [(EndpointConfig::default(), cap), (global, 3)] {
            let (mut server, source, nonces) = connected_server(config);
            items(&mut server, source, nonces, cap + 1);
            assert_eq!(delivered(&server.poll(2)), (0..cap).collect::<Vec<_>>());
            assert_eq!(
                ack(&mut server, 2),
                packet::Ack {
                    next: cap,
                    bits: 0,
                    held: true,
                }
            );
            assert_eq!(delivered(&server.poll(3)), vec![cap]);
            assert_eq!(
                ack(&mut server, 3),
                packet::Ack {
                    next: cap + 1,
                    bits: 0,
                    held: false,
                }
            );
            assert_eq!(server.peer_count(), 1);
        }
    }

    /// Defect: held lanes released in a fixed order, so when the global
    /// per-poll message ceiling binds one backlogged peer takes it every
    /// poll and another, held behind it, never progresses. Oracle:
    /// netcode.md 11 (other connections are never affected; a held peer is
    /// slowed, not stopped) — two peers each holding a full window of
    /// messages under a ceiling of two per poll both receive theirs in
    /// order, and the gap between their shares stays within two polls'
    /// ceiling (the poll they arrived in, and one turn) however many polls
    /// pass.
    #[wasm_bindgen_test(unsupported = test)]
    fn held_peers_share_a_binding_global_message_ceiling() {
        const CEILING: usize = 2;
        let (mut server, first, first_nonces) = connected_server(EndpointConfig {
            global_reliable_inbound_messages: CEILING,
            ..EndpointConfig::default()
        });
        let second = address(192, 0, 2, 91, 40_001);
        request(&mut server, second, 66);
        assert!(server.poll(2).is_empty());
        let second_nonces = challenge_for(&server, second, 66);
        confirm(&mut server, second, second_nonces);
        assert_eq!(server.poll(3), vec![EndpointEvent::Connected { peer: 2 }]);
        for (source, nonces) in [(first, first_nonces), (second, second_nonces)] {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
            for sequence in 0..super::super::reliable::WINDOW {
                packet::push_reliable(
                    &mut bytes,
                    Lane::DEFAULT,
                    sequence,
                    Fragment::Whole,
                    &sequence.to_le_bytes(),
                );
            }
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
        }
        let mut received: BTreeMap<u64, Vec<u16>> = BTreeMap::new();
        for now in 4..18 {
            let events = server.poll(now);
            assert_eq!(events.len(), CEILING, "the ceiling binds every poll");
            for event in events {
                let EndpointEvent::Message { peer, payload, .. } = event else {
                    panic!("unexpected {event:?}");
                };
                received
                    .entry(peer)
                    .or_default()
                    .push(u16::from_le_bytes(payload[..].try_into().unwrap()));
            }
        }
        for (peer, got) in &received {
            assert!(
                got.iter().zip(0..).all(|(&a, b)| a == b),
                "peer {peer} in order, once: {got:?}"
            );
        }
        let counts: Vec<_> = [1, 2]
            .iter()
            .map(|peer| received.get(peer).map_or(0, Vec::len))
            .collect();
        assert!(
            counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 2 * CEILING,
            "unfair split: {counts:?}"
        );
        assert_eq!(server.peer_count(), 2);
    }

    /// #254, #267: the global outbound allowance admits exactly its message
    /// and byte caps and refuses the next send with `WouldBlock` without
    /// touching the peer.
    #[wasm_bindgen_test(unsupported = test)]
    fn outbound_global_caps_admit_exactly_the_cap() {
        let (mut server, _, _) = connected_server(EndpointConfig {
            global_reliable_outbound_messages: 2,
            ..EndpointConfig::default()
        });
        assert_eq!(server.send(1, Delivery::RELIABLE_ORDERED, b"a"), Ok(()));
        assert_eq!(server.send(1, Delivery::RELIABLE_ORDERED, b"b"), Ok(()));
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"c"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.peer_count(), 1);

        let mut config = EndpointConfig {
            global_reliable_outbound_bytes: 6,
            ..EndpointConfig::default()
        };
        config.reliable.max_message_bytes = 6;
        let (mut server, _, _) = connected_server(config);
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
        let big = vec![7u8; packet::MAX_RELIABLE_ITEM_PAYLOAD];
        for _ in 0..3 {
            server.send(1, Delivery::RELIABLE_ORDERED, &big).unwrap();
        }
        server.flush(10);
        assert_eq!(server.transport.sent.len(), 2);
        server.flush(11);
        assert_eq!(server.transport.sent.len(), 3);
    }

    use super::*;
    use crate::lanes::Fragment;

    const MAGIC: [u8; 3] = *b"TST";

    fn lane(index: u8) -> Lane {
        Lane::new(index).expect("test lanes exist")
    }

    /// A payload datagram acknowledging `next` on lane 0 only.
    fn lane0_ack(next: u16) -> packet::Acks {
        let mut acks = [None; RELIABLE_LANES];
        acks[0] = Some(packet::Ack {
            next,
            bits: 0,
            held: false,
        });
        acks
    }

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

        fn receive_acks(&mut self, source: SocketAddr, nonces: Nonces, acks: &packet::Acks) {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, acks);
            self.received.push_back(ReceivedDatagram { source, bytes });
        }

        fn receive_ack(&mut self, source: SocketAddr, nonces: Nonces, next: u16) {
            self.receive_acks(source, nonces, &lane0_ack(next));
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

    /// Every item `endpoint` put on the wire, in order.
    fn wire_items(endpoint: &Endpoint<RecordingTransport>) -> Vec<Item<'_>> {
        endpoint
            .transport
            .sent
            .iter()
            .filter_map(|datagram| match packet::parse(&datagram.bytes, MAGIC)? {
                Parsed::Payload { items, .. } => Some(items),
                Parsed::Control { .. } => None,
            })
            .flatten()
            .collect()
    }

    fn reliable_payload(datagram: &SentDatagram) -> Option<Vec<u8>> {
        let Parsed::Payload { mut items, .. } = packet::parse(&datagram.bytes, MAGIC)? else {
            return None;
        };
        items.find_map(|item| match item {
            Item::Reliable { payload, .. } => Some(payload.to_vec()),
            Item::Unreliable { .. } | Item::Latest { .. } => None,
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
        ] {
            assert!(matches!(
                Endpoint::client(RecordingTransport::default(), invalid),
                Err(EndpointError::InvalidConfig)
            ));
        }
        // #269: a message cap outside its range, or one that a shared byte
        // ceiling could never hold, is refused; a ceiling of exactly one
        // message is not.
        let with_cap = |cap: usize, outbound: usize, inbound: usize| {
            let mut config = EndpointConfig {
                global_reliable_outbound_bytes: outbound,
                global_reliable_inbound_bytes: inbound,
                ..EndpointConfig::default()
            };
            config.reliable.max_message_bytes = cap;
            Endpoint::client(RecordingTransport::default(), config).map(drop)
        };
        let (mib, limit) = (1 << 20, crate::RELIABLE_MESSAGE_BYTES_LIMIT);
        for (cap, outbound, inbound) in [
            (0, mib, mib),
            (limit + 1, 2 * limit, 2 * limit),
            (mib, mib - 1, mib),
            (mib, mib, mib - 1),
        ] {
            assert_eq!(
                with_cap(cap, outbound, inbound),
                Err(EndpointError::InvalidConfig),
                "{cap} {outbound} {inbound}"
            );
        }
        assert_eq!(with_cap(mib, mib, mib), Ok(()));
        assert_eq!(with_cap(limit, limit, limit), Ok(()));

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
            packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
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

    /// Defect (#269): a receiver that holds a large message back, or closes
    /// the peer, when it is followed or preceded in the same poll by
    /// smaller ones on its lane (a lane holding a message refusing
    /// everything else); or a lane that delivers past its bounds, or closes
    /// the peer instead of holding the excess. Oracle: the inbound rule of
    /// netcode.md 11 — beside messages within `inbound_bytes` a lane holds
    /// at most one larger message: on a lane bounded at 4 KiB, a 10 KiB
    /// message and 4 KiB of smaller ones arrive in one poll in either
    /// order; one byte more, or a second 10 KiB message, waits for the next
    /// poll, and the peer stays.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_lane_takes_one_message_past_its_inbound_bytes_beside_smaller_ones() {
        let mut config = EndpointConfig::default();
        config.reliable.lanes[1].inbound_bytes = 4 * 1024;
        let large: Vec<u8> = (0..10 * 1024_u32).map(|i| (i % 251) as u8).collect();
        let small = |len: usize| vec![7; len];
        // Each case's messages, and how many of them the first poll takes.
        let cases = [
            (vec![large.clone(), small(2_048), small(2_048)], 3),
            (vec![small(2_048), small(2_048), large.clone()], 3),
            (vec![large.clone(), small(2_048), small(2_049)], 2),
            (vec![large.clone(), large.clone()], 1),
        ];
        let payloads = |events: Vec<EndpointEvent>| -> Vec<Vec<u8>> {
            events
                .into_iter()
                .map(|event| match event {
                    EndpointEvent::Message { payload, .. } => payload,
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        };
        for (messages, first_poll) in cases {
            let (mut server, source, nonces) = connected_server(config.clone());
            let mut sequence = 0;
            for message in &messages {
                let mut start = 0;
                loop {
                    let (fragment, end) = Fragment::at(
                        message.len(),
                        start,
                        packet::MAX_RELIABLE_ITEM_PAYLOAD,
                        packet::TOTAL_LEN,
                    );
                    let mut bytes = Vec::new();
                    packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
                    packet::push_reliable(
                        &mut bytes,
                        lane(1),
                        sequence,
                        fragment,
                        &message[start..end],
                    );
                    server
                        .transport
                        .received
                        .push_back(ReceivedDatagram { source, bytes });
                    sequence += 1;
                    start = end;
                    if end == message.len() {
                        break;
                    }
                }
            }
            assert_eq!(payloads(server.poll(2)), messages[..first_poll]);
            assert_eq!(payloads(server.poll(3)), messages[first_poll..]);
            assert_eq!(server.peer_count(), 1);
        }
    }

    /// Pushes datagrams carrying the first `count` of `message`'s fragments
    /// on lane 1 from `source`, one per datagram; all of them with `None`.
    fn push_fragments(
        server: &mut Endpoint<RecordingTransport>,
        source: SocketAddr,
        nonces: Nonces,
        message: &[u8],
        count: Option<u16>,
    ) {
        let (mut start, mut sequence) = (0, 0);
        while count.is_none_or(|count| sequence < count) {
            let (fragment, end) = Fragment::at(
                message.len(),
                start,
                packet::MAX_RELIABLE_ITEM_PAYLOAD,
                packet::TOTAL_LEN,
            );
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
            packet::push_reliable(
                &mut bytes,
                lane(1),
                sequence,
                fragment,
                &message[start..end],
            );
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });
            if end == message.len() {
                break;
            }
            (start, sequence) = (end, sequence + 1);
        }
    }

    /// Defect (#269): a peer's partial message outliving the peer — still
    /// counted against the endpoint's inbound ceiling, or kept anywhere
    /// else — after the peer times out mid-message or disconnects, so later
    /// peers find the ceiling spent. Oracle: netcode.md 15 (partial messages
    /// go with their connection, whatever ends it): once a peer that sent
    /// half of a message is gone, a new peer completes a message that needs
    /// the whole ceiling.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_peer_that_stops_mid_message_takes_its_partial_message_with_it() {
        const CAP: usize = 16 * 1024;
        let mut config = EndpointConfig {
            global_reliable_inbound_bytes: CAP,
            timeout_ms: 1_000,
            ..EndpointConfig::default()
        };
        config.reliable.max_message_bytes = CAP;
        let message: Vec<u8> = (0..CAP).map(|i| (i % 253) as u8).collect();
        for timed_out in [true, false] {
            let (mut server, source, nonces) = connected_server(config.clone());
            push_fragments(&mut server, source, nonces, &message, Some(7));
            assert!(server.poll(2).is_empty());
            let ended = if timed_out {
                server.poll(2 + config.timeout_ms)
            } else {
                server
                    .transport
                    .receive_control(source, Kind::Disconnect, nonces);
                server.poll(3)
            };
            let reason = if timed_out {
                DisconnectReason::TimedOut
            } else {
                DisconnectReason::Peer
            };
            assert_eq!(ended, vec![EndpointEvent::Disconnected { peer: 1, reason }]);
            assert_eq!(server.peer_count(), 0);

            let now = 3 + config.timeout_ms;
            let second = address(192, 0, 2, 91, 40_001);
            request(&mut server, second, 66);
            assert!(server.poll(now).is_empty());
            let second_nonces = challenge_for(&server, second, 66);
            confirm(&mut server, second, second_nonces);
            assert_eq!(server.poll(now), vec![EndpointEvent::Connected { peer: 2 }]);
            push_fragments(&mut server, second, second_nonces, &message, None);
            assert_eq!(
                server.poll(now + 1),
                vec![EndpointEvent::Message {
                    peer: 2,
                    delivery: Delivery::Reliable(lane(1)),
                    payload: message.clone(),
                }],
                "timed out: {timed_out}"
            );
            assert!(server.poll(now + 2).is_empty());
            assert_eq!(server.peer_count(), 1);
        }
    }

    /// Defect: zero-length messages escaping a lane's message cap. Oracle:
    /// netcode.md 11 — every completed message counts, so a poll delivers
    /// the cap and the next poll the rest.
    #[wasm_bindgen_test(unsupported = test)]
    fn complete_zero_length_messages_count_toward_per_peer_inbound_cap() {
        let cap = crate::DEFAULT_LANE_INBOUND_MESSAGES;
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        for sequence in 0..=u16::try_from(cap).unwrap() {
            packet::push_reliable(&mut bytes, Lane::DEFAULT, sequence, Fragment::Whole, b"");
        }
        assert!(bytes.len() <= packet::DATAGRAM_BYTES);
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });

        assert_eq!(server.poll(2).len(), cap);
        assert_eq!(
            server.poll(3),
            vec![EndpointEvent::Message {
                peer: 1,
                delivery: Delivery::RELIABLE_ORDERED,
                payload: Vec::new(),
            }]
        );
        assert_eq!(server.peer_count(), 1);
    }

    /// #268: fragments that break the reassembly table — here a
    /// continuation with no first fragment, buffered behind a whole message
    /// on another lane in the same datagram — close the peer as a protocol
    /// violation and deliver nothing from that datagram.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_continuation_without_a_first_fragment_is_a_protocol_violation() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        packet::push_reliable(&mut bytes, lane(0), 0, Fragment::Whole, b"must-not-deliver");
        packet::push_reliable(&mut bytes, lane(2), 0, Fragment::Middle, b"orphan");
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
                reason: DisconnectReason::ProtocolViolation,
            }]
        );
    }

    /// Defect (#303): an unacknowledged fragment resent at a flat interval,
    /// or its resends ending the connection before `timeout_ms`. Oracle:
    /// RFC 6298 §5.5 (the timer doubles on each expiry) with the
    /// `MAX_RTO_MS` ceiling, and `timeout_ms` bounding a silent peer: from
    /// the unmeasured 200 ms timeout the resends fall 200, 400 and 800 ms
    /// apart, then every second, and the peer stays until its silence
    /// reaches `timeout_ms`.
    #[wasm_bindgen_test(unsupported = test)]
    fn unacknowledged_fragments_back_off_to_the_timeout_ceiling() {
        let (mut server, _source, _nonces) = connected_server(EndpointConfig::default());
        server
            .send(1, Delivery::RELIABLE_ORDERED, b"never-acked")
            .unwrap();
        for now in 10..=9_000 {
            server.flush(now);
        }
        assert_eq!(
            server
                .transport
                .sent
                .iter()
                .filter(|datagram| reliable_payload(datagram).is_some())
                .map(|datagram| datagram.now_ms)
                .collect::<Vec<_>>(),
            [
                10, 210, 610, 1_410, 2_410, 3_410, 4_410, 5_410, 6_410, 7_410, 8_410
            ]
        );
        assert!(server.poll(9_000).is_empty());
        assert_eq!(server.peer_count(), 1);
    }

    /// Defect (#303 review): a peer that keeps answering but never
    /// acknowledges a fragment wedging its lane forever, or being closed
    /// before a stall `timeout_ms` tolerates could have ended. Oracle: the
    /// liveness bound of netcode.md 12, `timeout_ms` plus twice
    /// `MAX_RTO_MS` without progress on a lane holding an unheld fragment.
    /// The client sends a keepalive every 500 ms and never acknowledges the
    /// fragment first sent at 10 ms: the server keeps the peer through
    /// 12,009 ms and closes it `TimedOut` at 12,010 ms.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_heard_peer_that_never_acknowledges_times_out_at_the_stall_bound() {
        let config = EndpointConfig::default();
        let bound = config.timeout_ms + 2 * MAX_RTO_MS;
        let (mut server, source, nonces) = connected_server(config);
        server
            .send(1, Delivery::RELIABLE_ORDERED, b"never-acked")
            .unwrap();
        for now in 10..10 + bound {
            if now % 500 == 0 {
                server
                    .transport
                    .receive_acks(source, nonces, &[None; RELIABLE_LANES]);
            }
            assert!(server.poll(now).is_empty(), "at {now}");
            server.flush(now);
        }
        assert_eq!(server.peer_count(), 1);
        server.flush(10 + bound);
        assert_eq!(
            server.poll(10 + bound),
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

    /// #267, #268: a full lane refuses the send with `WouldBlock`; the peer
    /// stays connected, nothing is announced, its queued messages still go
    /// out, and every other lane still admits — the lane bounds of
    /// netcode.md 11 are per lane. A global ceiling refuses every lane.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_full_lane_refuses_alone_and_a_global_ceiling_refuses_every_lane() {
        let (mut server, _, _) = connected_server(EndpointConfig::default());
        for _ in 0..crate::DEFAULT_LANE_OUTBOUND_MESSAGES {
            server.send(1, Delivery::RELIABLE_ORDERED, b"").unwrap();
        }
        assert_eq!(
            server.send(1, Delivery::RELIABLE_ORDERED, b"refused"),
            Err(SendError::WouldBlock)
        );
        assert_eq!(server.capacity(1, Lane::DEFAULT).messages, 0);
        for index in 1..RELIABLE_LANES as u8 {
            assert!(server.capacity(1, lane(index)).messages > 0);
            server
                .send(1, Delivery::Reliable(lane(index)), b"other lane")
                .expect("a full lane leaves the others admitting");
        }
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
            global_reliable_outbound_messages: 1,
            ..EndpointConfig::default()
        };
        let (mut server, _, _) = connected_server(config);
        let long = vec![7; 3 * packet::MAX_RELIABLE_ITEM_PAYLOAD];
        server
            .send(1, Delivery::RELIABLE_ORDERED, &long)
            .expect("a multi-fragment message is one message");
        for index in 0..RELIABLE_LANES as u8 {
            assert_eq!(
                server.send(1, Delivery::Reliable(lane(index)), b"x"),
                Err(SendError::WouldBlock)
            );
        }
        assert_eq!(server.peer_count(), 1);
    }

    /// #267: a global ceiling filled by one peer's backlog refuses another
    /// peer's send with `WouldBlock` and closes nobody; once the backlog is
    /// acknowledged the refused send succeeds.
    #[wasm_bindgen_test(unsupported = test)]
    fn global_outbound_saturation_closes_no_peer_and_recovers_after_acks() {
        let config = EndpointConfig {
            global_reliable_outbound_messages: 4,
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

    /// Acknowledgements of every reliable fragment sent to `destination`,
    /// per lane, read off the wire.
    #[cfg(not(target_arch = "wasm32"))]
    fn ack_everything_sent(
        endpoint: &Endpoint<RecordingTransport>,
        destination: SocketAddr,
    ) -> packet::Acks {
        let mut acks = [None; RELIABLE_LANES];
        for datagram in &endpoint.transport.sent {
            if datagram.destination != destination {
                continue;
            }
            let Some(Parsed::Payload { items, .. }) = packet::parse(&datagram.bytes, MAGIC) else {
                continue;
            };
            for item in items {
                if let Item::Reliable { lane, sequence, .. } = item {
                    let next = sequence.wrapping_add(1);
                    let ack: &mut Option<packet::Ack> = &mut acks[lane.index()];
                    if ack.is_none_or(|ack| super::super::sequence::newer(next, ack.next)) {
                        *ack = Some(packet::Ack {
                            next,
                            bits: 0,
                            held: false,
                        });
                    }
                }
            }
        }
        acks
    }

    /// Defect (#267, #268, #269): a capacity report that disagrees with
    /// admission (a forgotten lane or global bound, the one-message rule in
    /// one and not the other, a cap other than the configured one, a
    /// maintained counter that drifts from what is held as fragments are
    /// acknowledged, an off-by-one at a cap), or a refused send that still
    /// closes a peer. Oracle: `send`, the atomic admission path, over random
    /// sends of up to the configured cap on random lanes whose byte bounds
    /// are often below it, flushes and wire-derived acks on two peers
    /// sharing small global ceilings: the send succeeds exactly when the
    /// capacity read just before it says the payload fits, and a refusal is
    /// `WouldBlock` with both peers still connected.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn capacity_predicts_admission_across_lane_and_global_bounds() {
        use crate::proptest_support::check;
        use proptest::prelude::*;

        #[derive(Clone, Debug)]
        enum Op {
            Send(u64, u8, usize),
            Flush,
            Ack(u64),
        }
        const CAP: usize = 128 * 1024;
        let fragment = packet::MAX_RELIABLE_ITEM_PAYLOAD;
        let length = prop_oneof![
            Just(0),
            Just(1),
            Just(fragment),
            Just(fragment + 1),
            Just(2 * fragment + 1),
            Just(CAP),
            0..=CAP,
        ];
        let op = prop_oneof![
            6 => (1..=2_u64, 0..RELIABLE_LANES as u8, length)
                .prop_map(|(peer, lane, len)| Op::Send(peer, lane, len)),
            2 => Just(Op::Flush),
            1 => (1..=2_u64).prop_map(Op::Ack),
        ];
        let strategy = (
            1..=300_usize,
            CAP..=1_200_000_usize,
            prop::array::uniform4(1..=2 * CAP),
            prop::collection::vec(op, 1..300),
        );
        check(strategy, |(messages, bytes, lane_bytes, ops)| {
            let mut config = EndpointConfig {
                global_reliable_outbound_messages: messages,
                global_reliable_outbound_bytes: bytes,
                ..EndpointConfig::default()
            };
            config.reliable.max_message_bytes = CAP;
            for (lane, bytes) in config.reliable.lanes.iter_mut().zip(lane_bytes) {
                lane.outbound_bytes = bytes;
            }
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
                    Op::Send(peer, index, len) => {
                        let capacity = server.capacity(peer, lane(index));
                        let fits = capacity.messages >= 1 && capacity.bytes >= len;
                        let result =
                            server.send(peer, Delivery::Reliable(lane(index)), &vec![3; len]);
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
                        let acks = ack_everything_sent(&server, source);
                        server.transport.receive_acks(source, nonces, &acks);
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
        let mut config = EndpointConfig {
            global_reliable_inbound_bytes: 4,
            ..EndpointConfig::default()
        };
        config.reliable.max_message_bytes = 4;
        let (mut server, source, nonces) = connected_server(config);
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        // Two partial messages retain five bytes between them.
        packet::push_reliable(
            &mut bytes,
            Lane::DEFAULT,
            0,
            Fragment::First { total: 3 },
            b"ab",
        );
        packet::push_reliable(&mut bytes, lane(1), 0, Fragment::First { total: 4 }, b"abc");
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

    /// #268, #269: a first fragment declaring more than the configured
    /// message cap closes the peer as a protocol violation when it arrives,
    /// before anything is buffered, and a run of fragments overrunning its
    /// declared total does too; neither delivers anything. A total of the
    /// cap itself, past the default cap, waits for the rest of its message.
    #[wasm_bindgen_test(unsupported = test)]
    fn over_cap_or_overrun_totals_close_the_peer_without_delivery() {
        const CAP: u32 = 1 << 20;
        let mut config = EndpointConfig::default();
        config.reliable.max_message_bytes = CAP as usize;
        let datagram = |nonces, fragments: &[(Fragment, &[u8])]| {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
            packet::push_reliable(&mut bytes, lane(1), 0, Fragment::Whole, b"must-not-deliver");
            for (sequence, (fragment, payload)) in (0..).zip(fragments) {
                packet::push_reliable(&mut bytes, lane(2), sequence, *fragment, payload);
            }
            bytes
        };

        let (mut server, source, nonces) = connected_server(config.clone());
        let bytes = datagram(nonces, &[(Fragment::First { total: CAP }, b"ab")]);
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });
        assert_eq!(server.poll(4).len(), 1, "only the whole message");
        assert_eq!(server.peer_count(), 1);

        let overrun: [(Fragment, &[u8]); 3] = [
            (Fragment::First { total: 4 }, b"ab"),
            (Fragment::Middle, b"c"),
            (Fragment::Middle, b"d"),
        ];
        for fragments in [
            &[(Fragment::First { total: CAP + 1 }, b"ab".as_slice())][..],
            &overrun,
        ] {
            let (mut server, source, nonces) = connected_server(config.clone());
            let bytes = datagram(nonces, fragments);
            server
                .transport
                .received
                .push_back(ReceivedDatagram { source, bytes });

            assert!(server.poll(4).is_empty());
            assert_eq!(server.peer_count(), 0);
            assert_eq!(
                server.inbound_retained, 0,
                "the peer's partial bytes went with it"
            );
            assert_eq!(
                server.poll(5),
                vec![EndpointEvent::Disconnected {
                    peer: 1,
                    reason: DisconnectReason::ProtocolViolation,
                }]
            );
        }
    }

    /// #268 (design §11–12): an unreliable payload over
    /// `MAX_UNRELIABLE_BYTES` is too large and a full unreliable lane queue
    /// refuses with `WouldBlock`; the peer stays connected, other lanes and
    /// the reliable allowance still admit, and every accepted message goes
    /// on the wire exactly once, however many flushes follow.
    #[wasm_bindgen_test(unsupported = test)]
    fn unreliable_admission_refuses_whole_and_sends_each_message_once() {
        let (mut server, _, _) = connected_server(EndpointConfig::default());
        let unreliable = Delivery::Unreliable(lane(2));
        assert_eq!(
            server.send(1, unreliable, &[0; crate::MAX_UNRELIABLE_BYTES + 1]),
            Err(SendError::PayloadTooLarge)
        );
        let accepted = crate::DEFAULT_LANE_UNRELIABLE_MESSAGES;
        for index in 0..accepted {
            server
                .send(1, unreliable, &u32::try_from(index).unwrap().to_le_bytes())
                .expect("within the unreliable queue");
        }
        assert_eq!(server.send(1, unreliable, b"x"), Err(SendError::WouldBlock));
        server
            .send(1, Delivery::Unreliable(lane(3)), b"other lane")
            .expect("another lane's queue admits");
        server
            .send(1, Delivery::Reliable(lane(2)), b"reliable")
            .expect("the reliable allowance is separate");
        assert!(server.poll(2).is_empty(), "no disconnect is announced");
        assert_eq!(server.peer_count(), 1);

        for now in [3, 4, 5, 500, 1_000, 2_000] {
            server.flush(now);
        }
        let mut sent: Vec<u32> = server
            .transport
            .sent
            .iter()
            .filter_map(|datagram| match packet::parse(&datagram.bytes, MAGIC)? {
                Parsed::Payload { items, .. } => Some(items.collect::<Vec<_>>()),
                Parsed::Control { .. } => None,
            })
            .flatten()
            .filter_map(|item| match item {
                Item::Unreliable {
                    lane: got, payload, ..
                } if got == lane(2) => Some(u32::from_le_bytes(payload.try_into().unwrap())),
                _ => None,
            })
            .collect();
        sent.sort_unstable();
        let expected = u32::try_from(accepted).unwrap();
        assert_eq!(sent, (0..expected).collect::<Vec<_>>());
        server
            .send(1, unreliable, b"room again")
            .expect("sent messages leave the queue");
    }

    /// #268 (netcode.md 14): within one lane a new reliable fragment and an
    /// unreliable message take turns, so neither waits behind more than one
    /// of the other. Oracle: that bound, read off the wire order of one
    /// flush that carries a lane's reliable backlog and its unreliable
    /// queue.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_lane_alternates_new_reliable_fragments_and_unreliable_messages() {
        let (mut server, _, _) = connected_server(EndpointConfig::default());
        for _ in 0..3 {
            server
                .send(
                    1,
                    Delivery::Reliable(lane(2)),
                    &[1; 3 * packet::MAX_RELIABLE_ITEM_PAYLOAD],
                )
                .unwrap();
        }
        for index in 0..4u8 {
            server
                .send(1, Delivery::Unreliable(lane(2)), &[index])
                .unwrap();
        }
        server.flush(3);
        let order: Vec<bool> = wire_items(&server)
            .iter()
            .map(|item| matches!(item, Item::Unreliable { .. }))
            .collect();
        assert_eq!(order.iter().filter(|&&unreliable| unreliable).count(), 4);
        let last_unreliable = order.iter().rposition(|&unreliable| unreliable).unwrap();
        let mut reliable_run = 0;
        for &unreliable in &order[..last_unreliable] {
            reliable_run = if unreliable { 0 } else { reliable_run + 1 };
            assert!(
                reliable_run <= 1,
                "{order:?}: two reliable fragments before a waiting unreliable message"
            );
        }
    }

    /// #268 (design §11): a network duplicate of an unreliable datagram is
    /// delivered once, and an earlier message that arrives after it still
    /// arrives. Oracle: at most once per sent message, no order promised.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_duplicated_unreliable_datagram_is_delivered_once() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let datagram = |sequence: u16, payload: &[u8]| {
            let mut bytes = Vec::new();
            packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
            packet::push_unreliable(&mut bytes, lane(1), sequence, payload);
            ReceivedDatagram { source, bytes }
        };
        for (sequence, payload) in [(5, b"five"), (5, b"five"), (4, b"four"), (5, b"five")] {
            server
                .transport
                .received
                .push_back(datagram(sequence, payload));
        }
        let message = |payload: &[u8]| EndpointEvent::Message {
            peer: 1,
            delivery: Delivery::Unreliable(lane(1)),
            payload: payload.to_vec(),
        };
        assert_eq!(server.poll(2), vec![message(b"five"), message(b"four")]);
    }

    /// #268 (netcode.md 12): a maximal latest-state datagram has room for
    /// one lane's acknowledgement; the other lanes that owe one get it in
    /// the acknowledgement datagram of the same flush, and no datagram
    /// exceeds `MAX_DATAGRAM_BYTES`.
    #[wasm_bindgen_test(unsupported = test)]
    fn acks_that_do_not_fit_beside_latest_state_follow_in_the_same_flush() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        for index in [0, 1, 3] {
            packet::push_reliable(&mut bytes, lane(index), 0, Fragment::Whole, &[index]);
        }
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });
        assert_eq!(server.poll(2).len(), 3);
        server
            .send(
                1,
                Delivery::LatestState,
                &[9; crate::MAX_LATEST_STATE_BYTES],
            )
            .unwrap();
        server.flush(3);
        let mut acked = [false; RELIABLE_LANES];
        let mut latest = 0;
        for datagram in &server.transport.sent {
            assert!(datagram.bytes.len() <= packet::DATAGRAM_BYTES);
            let Some(Parsed::Payload { acks, items, .. }) = packet::parse(&datagram.bytes, MAGIC)
            else {
                panic!("expected payload datagrams");
            };
            for (lane, ack) in acks.iter().enumerate() {
                if let Some(ack) = ack {
                    assert_eq!(
                        *ack,
                        packet::Ack {
                            next: 1,
                            bits: 0,
                            held: false,
                        },
                        "lane {lane}"
                    );
                    acked[lane] = true;
                }
            }
            latest += items.count();
        }
        assert_eq!(latest, 1, "the state went out once");
        assert_eq!(acked, [true, true, false, true]);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn malformed_item_suffix_disconnects_without_delivering_valid_prefix() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        packet::push_reliable(
            &mut bytes,
            Lane::DEFAULT,
            0,
            Fragment::Whole,
            b"must-not-deliver",
        );
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

    /// Defect: items sent one per datagram, a datagram closed while the next
    /// item still fits, an item lost or repeated by packing, an owed
    /// acknowledgement left out, or a datagram past the limit. Oracle: the
    /// version-3 layout (netcode.md 12) — a 21-byte header (magic 3, version
    /// 1, kind 1, two 8-byte nonces), 6 bytes per acknowledgement and 5 per
    /// item header — so 30 reliable and 90 unreliable twenty-byte messages
    /// (the reliable ones within the lane's window) and a 40-byte latest
    /// state beside two lanes' acknowledgements fill ceil(3,045 / 1,167) = 3
    /// datagrams of at most 1,200 bytes; items this small leave less than
    /// one item's slack in each.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_flush_packs_small_items_into_as_few_datagrams_as_fit() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        // The peer sends on lanes 0 and 3, so every datagram back carries
        // both acknowledgements.
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        for index in [0, 3] {
            packet::push_reliable(&mut bytes, lane(index), 0, Fragment::Whole, b"in");
        }
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });
        assert_eq!(server.poll(2).len(), 2);

        server.send(1, Delivery::LatestState, &[9; 40]).unwrap();
        for index in 0..90u8 {
            if index < 30 {
                server
                    .send(1, Delivery::Reliable(lane(0)), &[index; 20])
                    .unwrap();
            }
            server
                .send(1, Delivery::Unreliable(lane(1)), &[index; 20])
                .unwrap();
        }
        server.flush(3);

        let (header, ack, item): (usize, usize, usize) = (21, 6, 5);
        let total = item + 40 + 120 * (item + 20);
        let room = 1_200 - header - 2 * ack;
        assert_eq!(server.transport.sent.len(), total.div_ceil(room));
        let (mut reliable, mut unreliable, mut latest) = (Vec::new(), Vec::new(), 0);
        for datagram in &server.transport.sent {
            assert!(datagram.bytes.len() <= 1_200);
            let Some(Parsed::Payload { acks, items, .. }) = packet::parse(&datagram.bytes, MAGIC)
            else {
                panic!("expected payload datagrams");
            };
            assert!(acks[0].is_some() && acks[3].is_some(), "{acks:?}");
            for item in items {
                match item {
                    Item::Reliable {
                        lane: got,
                        fragment: Fragment::Whole,
                        payload,
                        ..
                    } if got == lane(0) => reliable.push(payload[0]),
                    Item::Unreliable {
                        lane: got, payload, ..
                    } if got == lane(1) => unreliable.push(payload[0]),
                    Item::Latest { payload, .. } if payload == [9; 40] => latest += 1,
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        reliable.sort_unstable();
        unreliable.sort_unstable();
        assert_eq!(
            (reliable, unreliable, latest),
            ((0..30).collect(), (0..90).collect(), 1)
        );
    }

    /// Defect: newest-wins state starved behind a lane backlog that takes
    /// the whole datagram budget. Oracle: netcode.md 14 — the state queued
    /// before a flush goes out in it — with a budget of two datagrams and a
    /// reliable backlog of eight full fragments.
    #[wasm_bindgen_test(unsupported = test)]
    fn latest_state_goes_out_in_every_flush_however_full_the_lanes() {
        let (mut server, _, _) = connected_server(EndpointConfig {
            max_packets_per_peer_flush: 2,
            ..EndpointConfig::default()
        });
        server
            .send(
                1,
                Delivery::RELIABLE_ORDERED,
                &[1; 8 * packet::MAX_RELIABLE_ITEM_PAYLOAD],
            )
            .unwrap();
        for tick in 0..3u8 {
            server.send(1, Delivery::LatestState, &[tick; 600]).unwrap();
            server.transport.sent.clear();
            server.flush(10 + u64::from(tick));
            assert_eq!(server.transport.sent.len(), 2);
            let latest: Vec<u8> = wire_items(&server)
                .into_iter()
                .filter_map(|item| match item {
                    Item::Latest { payload, .. } => Some(payload[0]),
                    _ => None,
                })
                .collect();
            assert_eq!(latest, [tick]);
        }
    }

    /// Defect: packing that sends a fragment twice in one flush (one taken
    /// but not yet marked sent still counting as due), puts a lane's new
    /// fragments before its due retransmissions, or retransmits before the
    /// timeout. Oracle: netcode.md 14 (within a lane due retransmissions go
    /// first) and the 200 ms retransmission timeout a lane uses before any
    /// round trip is measured: nothing goes at 209 ms, and at 210 ms the
    /// three fragments sent at 10 ms go again, oldest first, before the
    /// three queued since, each once.
    #[wasm_bindgen_test(unsupported = test)]
    fn due_retransmissions_lead_their_lane_and_go_once_per_flush() {
        let (mut server, _, _) = connected_server(EndpointConfig::default());
        let sequences = |server: &Endpoint<RecordingTransport>| -> Vec<u16> {
            wire_items(server)
                .into_iter()
                .filter_map(|item| match item {
                    Item::Reliable { sequence, .. } => Some(sequence),
                    _ => None,
                })
                .collect()
        };
        for index in 0..3u8 {
            server
                .send(1, Delivery::RELIABLE_ORDERED, &[index])
                .unwrap();
        }
        server.flush(10);
        assert_eq!(sequences(&server), [0, 1, 2]);
        for index in 3..6u8 {
            server
                .send(1, Delivery::RELIABLE_ORDERED, &[index])
                .unwrap();
        }
        server.transport.sent.clear();
        server.flush(209);
        // The three new fragments went at 209 ms; none was due yet.
        assert_eq!(sequences(&server), [3, 4, 5]);
        for index in 6..9u8 {
            server
                .send(1, Delivery::RELIABLE_ORDERED, &[index])
                .unwrap();
        }
        server.transport.sent.clear();
        server.flush(210);
        assert_eq!(sequences(&server), [0, 1, 2, 6, 7, 8]);
    }

    /// Defect: an arrival acknowledged once only, so a peer whose whole
    /// window rode one datagram waits for its retransmission timeout when
    /// that acknowledgement is lost, or one acknowledged in every flush for
    /// ever. Oracle: netcode.md 12 — a lane acknowledges an arrival in the
    /// next two flushes — on a server with nothing else to send.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_arrival_is_acknowledged_in_the_next_two_flushes() {
        let (mut server, source, nonces) = connected_server(EndpointConfig::default());
        let mut bytes = Vec::new();
        packet::begin_payload(&mut bytes, MAGIC, nonces, &[None; RELIABLE_LANES]);
        packet::push_reliable(&mut bytes, lane(2), 0, Fragment::Whole, b"x");
        server
            .transport
            .received
            .push_back(ReceivedDatagram { source, bytes });
        assert_eq!(server.poll(2).len(), 1);
        let mut acknowledged = Vec::new();
        for now in 3..7 {
            server.transport.sent.clear();
            server.flush(now);
            acknowledged.push(
                server
                    .transport
                    .sent
                    .iter()
                    .filter(|datagram| {
                        matches!(
                            packet::parse(&datagram.bytes, MAGIC),
                            Some(Parsed::Payload { acks, .. })
                                if acks[2]
                                    == Some(packet::Ack {
                                        next: 1,
                                        bits: 0,
                                        held: false,
                                    })
                        )
                    })
                    .count(),
            );
        }
        assert_eq!(acknowledged, [1, 1, 0, 0]);
    }
}
