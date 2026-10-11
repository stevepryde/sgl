//! Native WebSocket server and client over the shared binary codec.
//!
//! Each server and each client owns one I/O worker thread ([`worker`]) that
//! serves its listener, every upgrade and every connection over nonblocking
//! sockets. The worker sleeps until a socket is ready or the caller wakes it;
//! the caller and the worker meet only in each peer's [`SharedPeer`].

#![allow(clippy::result_large_err)]

mod worker;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use tungstenite::WebSocket;
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::server::{Callback, ErrorResponse, Request, Response};
use tungstenite::http::header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL};
use tungstenite::http::{HeaderValue, StatusCode, Uri};
use tungstenite::protocol::{Message, WebSocketConfig};

use self::worker::{IoWorker, WorkerHandle};
use super::native_write::{DrainResult, drain_outbound, send_ping};
use super::queue::{PeerState, Received};
use super::{MAX_WEBSOCKET_FRAME_BYTES, WebSocketIdentity, decode_envelope, encode_envelope};
use crate::{
    ClientEvent, ClientIo, ConnectionId, Delivery, DisconnectReason, Lane, ReliableCapacity,
    ReliableConfig, RttEstimate, RttEstimator, SendError, ServerEvent, ServerIo,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hard ceiling for active plus pending native WebSocket connections.
pub const MAX_NATIVE_WEBSOCKET_CONNECTIONS: usize = 1_024;
/// Hard ceiling for accepted TCP connections in one rolling second.
pub const MAX_NATIVE_WEBSOCKET_ACCEPTS_PER_SECOND: usize = 4_096;
/// Hard ceiling for events surfaced by one native-server poll.
pub const MAX_NATIVE_WEBSOCKET_EVENTS_PER_POLL: usize = 4_096;
/// Hard ceiling for one HTTP upgrade attempt.
pub const MAX_WEBSOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Hard ceiling for caller-clock ping and inactivity intervals.
pub const MAX_WEBSOCKET_LIVENESS_MS: u64 = 300_000;

/// Explicit native-server Origin admission policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginPolicy(OriginPolicyKind);

#[derive(Clone, Debug, PartialEq, Eq)]
enum OriginPolicyKind {
    Exact(BTreeSet<String>),
    AllowAny,
}

impl OriginPolicy {
    /// Builds a canonical exact allowlist. Each origin must be written as a
    /// browser sends it: lowercase host, no default port, and an IPv6 literal
    /// in compressed bracketed form (`http://[::1]:3000`).
    pub fn exact(origins: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut canonical = BTreeSet::new();
        for origin in origins {
            let parsed = canonical_origin(&origin)?;
            if parsed != origin {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("WebSocket origin must be canonical: {parsed}"),
                ));
            }
            canonical.insert(parsed);
        }
        if canonical.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket origin allowlist must not be empty",
            ));
        }
        Ok(Self(OriginPolicyKind::Exact(canonical)))
    }

    /// Deliberately admits every canonical, present Origin value.
    #[must_use]
    pub const fn allow_any() -> Self {
        Self(OriginPolicyKind::AllowAny)
    }

    fn admits(&self, request: &Request) -> bool {
        let mut values = request.headers().get_all(ORIGIN).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let Ok(raw) = value.to_str() else {
            return false;
        };
        let Ok(canonical) = canonical_origin(raw) else {
            return false;
        };
        if canonical != raw {
            return false;
        }
        match &self.0 {
            OriginPolicyKind::Exact(allowed) => allowed.contains(&canonical),
            OriginPolicyKind::AllowAny => true,
        }
    }
}

fn canonical_origin(raw: &str) -> io::Result<String> {
    if raw.trim() != raw || raw.eq_ignore_ascii_case("null") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Origin",
        ));
    }
    let uri = raw
        .parse::<Uri>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "malformed Origin"))?;
    let scheme = uri
        .scheme_str()
        .filter(|scheme| matches!(*scheme, "http" | "https"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsupported Origin scheme"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Origin has no authority"))?;
    if authority.as_str().contains('@')
        || uri.path() != "/"
        || uri.query().is_some()
        || raw.ends_with('/')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Origin must contain only scheme and authority",
        ));
    }
    let host = authority.host();
    if host.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Origin host is empty",
        ));
    }
    // The authority keeps an IPv6 literal's brackets.
    let rendered_host = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(literal) => {
            let address = literal.parse::<Ipv6Addr>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "malformed Origin IPv6 address")
            })?;
            format!("[{}]", serialize_ipv6(address))
        }
        None => host.to_ascii_lowercase(),
    };
    let default_port = match scheme {
        "http" => 80,
        "https" => 443,
        _ => unreachable!("scheme filtered above"),
    };
    let port = authority.port_u16().filter(|port| *port != default_port);
    Ok(match port {
        Some(port) => format!("{scheme}://{rendered_host}:{port}"),
        None => format!("{scheme}://{rendered_host}"),
    })
}

/// The WHATWG URL IPv6 serialization browsers use for `Origin`. `Display`
/// follows RFC 5952, which matches it except that it writes an IPv4-mapped
/// address in dotted form, which WHATWG keeps in hex.
fn serialize_ipv6(address: Ipv6Addr) -> String {
    let [.., high, low] = address.segments();
    match address.to_ipv4_mapped() {
        Some(_) => format!("::ffff:{high:x}:{low:x}"),
        None => address.to_string(),
    }
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_WEBSOCKET_FRAME_BYTES))
        .max_frame_size(Some(MAX_WEBSOCKET_FRAME_BYTES))
}

struct SharedPeer {
    state: Mutex<PeerState>,
    liveness: Mutex<Liveness>,
    magic: [u8; 3],
    shutdown: AtomicBool,
    /// Set by the caller when it wants a worker turn for this peer, cleared
    /// by the worker when it takes the request, and left set once the worker
    /// has let the peer go so later requests wake nobody.
    turn_requested: AtomicBool,
}

impl SharedPeer {
    fn new(
        magic: [u8; 3],
        ping_interval_ms: u64,
        timeout_ms: u64,
        reliable: &ReliableConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PeerState::new(reliable)),
            liveness: Mutex::new(Liveness {
                now_ms: 0,
                last_activity_ms: 0,
                next_ping_ms: 0,
                ping_interval_ms,
                timeout_ms,
                initialized: false,
                activity_pending: false,
                outstanding_ping: None,
                pending_pong: None,
                rtt: RttEstimator::new(),
            }),
            magic,
            shutdown: AtomicBool::new(false),
            turn_requested: AtomicBool::new(false),
        })
    }

    fn close(&self, reason: DisconnectReason) {
        lock(&self.state).close(reason);
        self.shutdown.store(true, Ordering::Release);
    }

    fn advance(&self, now_ms: u64) {
        // While this side holds a frame for a full lane it reads nothing,
        // so the peer's silence is this side's doing, not the peer's: the
        // stall is not inactivity. The peer's own timeout bounds how long
        // it waits for a receiver that never polls.
        let stalled = lock(&self.state).read_stalled();
        let mut liveness = lock(&self.liveness);
        if stalled {
            liveness.observe_activity();
        }
        liveness.advance(now_ms);
        let graceful_closing = lock(&self.state).graceful_closing();
        if liveness.timed_out() && !graceful_closing {
            drop(liveness);
            self.close(DisconnectReason::TimedOut);
        } else {
            drop(liveness);
            lock(&self.state).expire_graceful_close(now_ms);
        }
    }

    fn observe_activity(&self) {
        lock(&self.liveness).observe_activity();
    }

    fn observe_pong(&self, payload: &[u8]) {
        lock(&self.liveness).observe_pong(payload);
    }

    fn take_ping(&self) -> Option<Vec<u8>> {
        lock(&self.liveness).take_ping()
    }

    fn rtt(&self) -> RttEstimate {
        lock(&self.liveness).rtt.estimate()
    }

    /// Whether a worker turn would act on something only the caller causes:
    /// a close to write, a graceful close to finish, released frames to send,
    /// a held frame that now fits because `poll` made room, or a ping the
    /// caller's clock made due. Socket readiness covers the rest.
    fn needs_turn(&self) -> bool {
        if self.shutdown.load(Ordering::Acquire) {
            return true;
        }
        {
            let state = lock(&self.state);
            if state.terminal().is_some()
                || state.graceful_closing()
                || state.has_released_outbound()
                || state.can_resume()
            {
                return true;
            }
        }
        lock(&self.liveness).ping_due()
    }

    /// Asks for a worker turn if one is needed. True when the worker must be
    /// woken for it, that is when no request was already pending.
    fn request_turn(&self) -> bool {
        self.needs_turn() && !self.turn_requested.swap(true, Ordering::SeqCst)
    }
}

struct Liveness {
    now_ms: u64,
    last_activity_ms: u64,
    next_ping_ms: u64,
    ping_interval_ms: u64,
    timeout_ms: u64,
    initialized: bool,
    activity_pending: bool,
    outstanding_ping: Option<u64>,
    pending_pong: Option<u64>,
    rtt: RttEstimator,
}

impl Liveness {
    fn advance(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        if !self.initialized {
            self.initialized = true;
            self.last_activity_ms = self.now_ms;
            self.next_ping_ms = self.now_ms.saturating_add(self.ping_interval_ms);
        }
        if self.activity_pending {
            self.last_activity_ms = self.now_ms;
            self.activity_pending = false;
        }
        if let Some(sent_ms) = self.pending_pong.take()
            && self.outstanding_ping == Some(sent_ms)
        {
            let elapsed = self.now_ms.saturating_sub(sent_ms);
            self.rtt.sample(u32::try_from(elapsed).unwrap_or(u32::MAX));
            self.outstanding_ping = None;
        }
    }

    fn observe_activity(&mut self) {
        self.activity_pending = true;
    }

    fn observe_pong(&mut self, payload: &[u8]) {
        self.activity_pending = true;
        let Ok(sent_bytes) = <[u8; 8]>::try_from(payload) else {
            return;
        };
        let sent_ms = u64::from_be_bytes(sent_bytes);
        self.pending_pong = Some(sent_ms);
    }

    fn timed_out(&self) -> bool {
        self.initialized
            && self.timeout_ms != 0
            && self.now_ms.saturating_sub(self.last_activity_ms) >= self.timeout_ms
    }

    fn ping_due(&self) -> bool {
        self.initialized
            && self.ping_interval_ms != 0
            && self.now_ms >= self.next_ping_ms
            && self.outstanding_ping.is_none()
    }

    fn take_ping(&mut self) -> Option<Vec<u8>> {
        if !self.ping_due() {
            return None;
        }
        let sent_ms = self.now_ms;
        self.outstanding_ping = Some(sent_ms);
        self.next_ping_ms = self.now_ms.saturating_add(self.ping_interval_ms);
        Some(sent_ms.to_be_bytes().to_vec())
    }
}

struct ServerPeer {
    shared: Arc<SharedPeer>,
    connected_announced: bool,
    disconnected_announced: bool,
}

type Registry = Arc<Mutex<BTreeMap<ConnectionId, ServerPeer>>>;

/// The upgrade checks every incoming handshake must pass.
#[derive(Clone)]
struct Admission(Arc<AdmissionRules>);

struct AdmissionRules {
    path: String,
    origin_policy: OriginPolicy,
    subprotocol: String,
}

impl Callback for Admission {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        let rules = &self.0;
        if request.uri().path() != rules.path
            || !rules.origin_policy.admits(request)
            || !has_subprotocol(request, &rules.subprotocol)
        {
            return Err(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Some("WebSocket admission rejected".into()))
                .expect("static rejection response"));
        }
        if let Ok(value) = HeaderValue::from_str(&rules.subprotocol) {
            response.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
        }
        Ok(response)
    }
}

/// What a server's worker needs to accept, upgrade and admit connections.
struct ListenerContext {
    admission: Admission,
    magic: [u8; 3],
    reliable: ReliableConfig,
    handshake_timeout: Duration,
    ping_interval_ms: u64,
    timeout_ms: u64,
    registry: Registry,
    accepting: Arc<AtomicBool>,
    occupancy: Arc<AtomicUsize>,
    max_connections: usize,
    max_accepts_per_second: usize,
}

struct AcceptRateLimiter {
    recent: VecDeque<Instant>,
    max_per_second: usize,
}

impl AcceptRateLimiter {
    fn new(max_per_second: usize) -> Self {
        Self {
            recent: VecDeque::new(),
            max_per_second,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .front()
            .is_some_and(|accepted| now.duration_since(*accepted) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.max_per_second {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

/// Native WebSocket listener configuration.
#[derive(Clone, Debug)]
pub struct NativeWebSocketServerConfig {
    /// Socket address to bind.
    pub bind_addr: SocketAddr,
    /// Required explicit Origin policy. `None` is a startup error.
    pub origin_policy: Option<OriginPolicy>,
    /// Framing and handshake identity supplied by the game.
    pub identity: WebSocketIdentity,
    /// Maximum admitted or handshaking connections.
    pub max_connections: usize,
    /// Maximum accepted TCP connections in any rolling one-second window.
    pub max_accepts_per_second: usize,
    /// Maximum lifecycle and message events surfaced by one [`ServerIo::poll`].
    pub max_events_per_poll: usize,
    /// Maximum wall-clock time spent completing an HTTP upgrade.
    pub handshake_timeout: Duration,
    /// Caller-clock interval between liveness pings. Zero disables pings.
    pub ping_interval_ms: u64,
    /// Caller-clock inactivity timeout. Zero disables the timeout. Time this
    /// side spends not reading because a lane is full does not count; the
    /// peer's own timeout bounds how long it waits for this side to poll.
    pub timeout_ms: u64,
    /// Every connection's reliable lanes: weights and per-lane bounds.
    pub reliable: ReliableConfig,
}

impl NativeWebSocketServerConfig {
    /// Creates a listener configuration that still requires an Origin policy.
    #[must_use]
    pub fn new(bind_addr: SocketAddr, identity: WebSocketIdentity) -> Self {
        Self {
            bind_addr,
            origin_policy: None,
            identity,
            max_connections: 32,
            max_accepts_per_second: 32,
            max_events_per_poll: 256,
            handshake_timeout: Duration::from_secs(2),
            ping_interval_ms: 5_000,
            timeout_ms: 15_000,
            reliable: ReliableConfig::DEFAULT,
        }
    }

    /// Sets the mandatory explicit Origin policy.
    #[must_use]
    pub fn with_origin_policy(mut self, policy: OriginPolicy) -> Self {
        self.origin_policy = Some(policy);
        self
    }
}

/// Native WebSocket server implementing [`ServerIo`].
pub struct NativeWebSocketServer {
    local_addr: SocketAddr,
    registry: Registry,
    accepting: Arc<AtomicBool>,
    occupancy: Arc<AtomicUsize>,
    worker: Option<WorkerHandle>,
    max_events_per_poll: usize,
    poll_cursor: usize,
}

impl NativeWebSocketServer {
    /// Binds the listener and starts its I/O worker.
    pub fn bind(config: NativeWebSocketServerConfig) -> io::Result<Self> {
        let origin_policy = config.origin_policy.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket Origin policy must be explicitly configured",
            )
        })?;
        if config.identity.path.is_empty() || config.identity.subprotocol.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket path and subprotocol must be non-empty",
            ));
        }
        if config.max_connections == 0 || config.max_connections > MAX_NATIVE_WEBSOCKET_CONNECTIONS
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket max_connections is outside the SGL bound",
            ));
        }
        if config.max_accepts_per_second == 0
            || config.max_accepts_per_second > MAX_NATIVE_WEBSOCKET_ACCEPTS_PER_SECOND
            || config.max_events_per_poll == 0
            || config.max_events_per_poll > MAX_NATIVE_WEBSOCKET_EVENTS_PER_POLL
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket accept or poll bound is outside the SGL limit",
            ));
        }
        if config.handshake_timeout.is_zero()
            || config.handshake_timeout > MAX_WEBSOCKET_HANDSHAKE_TIMEOUT
            || config.ping_interval_ms > MAX_WEBSOCKET_LIVENESS_MS
            || config.timeout_ms > MAX_WEBSOCKET_LIVENESS_MS
            || (config.ping_interval_ms != 0
                && config.timeout_ms != 0
                && config.timeout_ms <= config.ping_interval_ms)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid bounded WebSocket timeout configuration",
            ));
        }
        config
            .reliable
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let listener = TcpListener::bind(config.bind_addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let registry = Arc::new(Mutex::new(BTreeMap::new()));
        let accepting = Arc::new(AtomicBool::new(true));
        let occupancy = Arc::new(AtomicUsize::new(0));
        let context = ListenerContext {
            admission: Admission(Arc::new(AdmissionRules {
                path: config.identity.path,
                origin_policy,
                subprotocol: config.identity.subprotocol,
            })),
            magic: config.identity.magic,
            reliable: config.reliable,
            handshake_timeout: config.handshake_timeout,
            ping_interval_ms: config.ping_interval_ms,
            timeout_ms: config.timeout_ms,
            registry: Arc::clone(&registry),
            accepting: Arc::clone(&accepting),
            occupancy: Arc::clone(&occupancy),
            max_connections: config.max_connections,
            max_accepts_per_second: config.max_accepts_per_second,
        };
        let (mut worker, waker) = IoWorker::new()?;
        worker.listen(listener, context)?;
        Ok(Self {
            local_addr,
            registry,
            accepting,
            occupancy,
            worker: Some(worker.spawn(waker)?),
            max_events_per_poll: config.max_events_per_poll,
            poll_cursor: 0,
        })
    }

    /// Returns the bound address, including an OS-assigned port.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    fn wake_worker(&self) {
        if let Some(worker) = &self.worker {
            worker.wake();
        }
    }
}

impl ServerIo for NativeWebSocketServer {
    fn poll(&mut self, now_ms: u64) -> Vec<ServerEvent> {
        let mut registry = lock(&self.registry);
        let mut connections: Vec<_> = registry.keys().copied().collect();
        if connections.is_empty() {
            self.poll_cursor = 0;
            return Vec::new();
        }
        let count = connections.len();
        connections.rotate_left(self.poll_cursor % count);
        self.poll_cursor = (self.poll_cursor + 1) % count;
        let mut events = Vec::with_capacity(self.max_events_per_poll);
        let mut remove = Vec::new();
        let mut wake = false;
        // Only a close made before the drain is reported. The worker can
        // close a peer between two of its events, after this poll popped
        // messages the close would have purged; that close waits for the
        // next poll, so no poll returns a peer's messages beside its
        // `ProtocolViolation`.
        let closed: Vec<bool> = connections
            .iter()
            .map(|conn| {
                let shared = &registry
                    .get(conn)
                    .expect("connection key came from registry")
                    .shared;
                shared.advance(now_ms);
                lock(&shared.state).terminal().is_some()
            })
            .collect();

        // Drain in rounds so every active peer gets one turn before any peer
        // gets a second. Rotate the starting peer between polls as well.
        let mut progressed = true;
        while events.len() < self.max_events_per_poll && progressed {
            progressed = false;
            for (&conn, &closed) in connections.iter().zip(&closed) {
                if events.len() >= self.max_events_per_poll {
                    break;
                }
                let peer = registry
                    .get_mut(&conn)
                    .expect("connection key came from registry");
                let event = if peer.connected_announced {
                    let mut state = lock(&peer.shared.state);
                    if let Some((delivery, payload)) = state.pop_inbound() {
                        Some(ServerEvent::Message {
                            conn,
                            delivery,
                            payload,
                        })
                    } else if let Some(reason) = state.terminal()
                        && closed
                        && !peer.disconnected_announced
                    {
                        peer.disconnected_announced = true;
                        peer.shared.shutdown.store(true, Ordering::Release);
                        remove.push(conn);
                        Some(ServerEvent::Disconnected { conn, reason })
                    } else {
                        None
                    }
                } else {
                    peer.connected_announced = true;
                    Some(ServerEvent::Connected { conn })
                };
                if let Some(event) = event {
                    events.push(event);
                    progressed = true;
                }
            }
        }
        // After the drain, so a peer whose held frame now fits resumes
        // reading.
        for conn in &connections {
            let peer = registry
                .get(conn)
                .expect("connection key came from registry");
            wake |= peer.shared.request_turn();
        }
        for conn in remove {
            if let Some(peer) = registry.remove(&conn) {
                self.occupancy.fetch_sub(1, Ordering::AcqRel);
                // The worker closes its socket on its next turn.
                wake |= peer.shared.request_turn();
            }
        }
        drop(registry);
        if wake {
            self.wake_worker();
        }
        events
    }

    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError> {
        let registry = lock(&self.registry);
        let peer = registry.get(&conn).ok_or(SendError::UnknownConnection)?;
        let result = lock(&peer.shared.state).send(delivery, payload);
        // A send that ended the peer closes its socket promptly; any other
        // refusal changed nothing.
        let wake = result == Err(SendError::Disconnected) && peer.shared.request_turn();
        drop(registry);
        if wake {
            self.wake_worker();
        }
        result.map_err(|error| match error {
            SendError::Disconnected => SendError::UnknownConnection,
            error => error,
        })
    }

    fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
        lock(&self.registry)
            .get(&conn)
            .map_or_else(ReliableCapacity::default, |peer| {
                lock(&peer.shared.state).capacity(lane)
            })
    }

    fn flush(&mut self, now_ms: u64) {
        let mut wake = false;
        for peer in lock(&self.registry).values() {
            let _ = lock(&peer.shared.state).release_outbound();
            peer.shared.advance(now_ms);
            wake |= peer.shared.request_turn();
        }
        if wake {
            self.wake_worker();
        }
    }

    fn disconnect(&mut self, conn: ConnectionId, now_ms: u64) {
        let wake = lock(&self.registry).get(&conn).is_some_and(|peer| {
            {
                let mut state = lock(&peer.shared.state);
                let _ = state.release_outbound();
                state.begin_graceful_close(now_ms);
            }
            peer.shared.advance(now_ms);
            peer.shared.request_turn()
        });
        if wake {
            self.wake_worker();
        }
    }

    fn stop_admission(&mut self) {
        // Under the registry lock, so no upgrade completing on the worker is
        // admitted once this returns.
        let registry = lock(&self.registry);
        self.accepting.store(false, Ordering::Release);
        drop(registry);
        // The worker closes the listener.
        self.wake_worker();
    }
}

impl Drop for NativeWebSocketServer {
    fn drop(&mut self) {
        self.stop_admission();
        for peer in lock(&self.registry).values() {
            peer.shared.close(DisconnectReason::Local);
        }
        // Stops the worker, which writes a Close on every socket, and joins it.
        drop(self.worker.take());
    }
}

fn has_subprotocol(request: &Request, expected: &str) -> bool {
    request
        .headers()
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|protocol| protocol.trim() == expected)
}

/// Native WebSocket client configuration.
#[derive(Clone, Debug)]
pub struct NativeWebSocketClientConfig {
    /// `ws://` endpoint URL. TLS termination belongs at the deployment edge.
    pub url: String,
    /// Canonical Origin value sent during the handshake.
    pub origin: String,
    /// Framing and handshake identity supplied by the game.
    pub identity: WebSocketIdentity,
    /// Caller-clock interval between liveness pings. Zero disables pings.
    pub ping_interval_ms: u64,
    /// Caller-clock inactivity timeout. Zero disables the timeout. Time this
    /// side spends not reading because a lane is full does not count; the
    /// peer's own timeout bounds how long it waits for this side to poll.
    pub timeout_ms: u64,
    /// Maximum wall-clock time spent resolving the host, connecting across
    /// every resolved address tried in turn, and completing the HTTP upgrade.
    pub handshake_timeout: Duration,
    /// The connection's reliable lanes: weights and per-lane bounds.
    pub reliable: ReliableConfig,
}

impl NativeWebSocketClientConfig {
    /// Creates a native client configuration.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        origin: impl Into<String>,
        identity: WebSocketIdentity,
    ) -> Self {
        Self {
            url: url.into(),
            origin: origin.into(),
            identity,
            ping_interval_ms: 5_000,
            timeout_ms: 15_000,
            handshake_timeout: Duration::from_secs(2),
            reliable: ReliableConfig::DEFAULT,
        }
    }
}

/// Resolves `target` on a helper thread, waiting no later than `deadline`.
/// The standard resolver cannot be cancelled, so a lookup still running at
/// the deadline finishes on its own thread and its result is discarded.
fn resolve_by(
    target: impl ToSocketAddrs + Send + 'static,
    deadline: Instant,
) -> io::Result<Vec<SocketAddr>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("sgl-net-ws-resolve".into())
        .spawn(move || {
            let resolved = target.to_socket_addrs().map(Iterator::collect);
            // The caller has stopped waiting if the receiver is gone.
            let _ = sender.send(resolved);
        })?;
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(resolved) => resolved,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "WebSocket host resolution timed out",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("WebSocket host resolver stopped"))
        }
    }
}

/// Connects to the first reachable resolved address, trying each in order
/// until one accepts or `deadline` passes.
fn connect_first_reachable(
    addrs: impl IntoIterator<Item = SocketAddr>,
    deadline: Instant,
) -> io::Result<TcpStream> {
    let mut failures = Vec::new();
    let mut last_kind = io::ErrorKind::NotFound;
    for addr in addrs {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            last_kind = io::ErrorKind::TimedOut;
            failures.push(format!("{addr}: connection budget exhausted"));
            break;
        }
        match TcpStream::connect_timeout(&addr, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                last_kind = error.kind();
                failures.push(format!("{addr}: {error}"));
            }
        }
    }
    if failures.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "WebSocket host unresolved",
        ));
    }
    Err(io::Error::new(
        last_kind,
        format!("WebSocket connect failed: {}", failures.join("; ")),
    ))
}

/// Native WebSocket client implementing [`ClientIo`].
pub struct NativeWebSocketClient {
    shared: Arc<SharedPeer>,
    worker: WorkerHandle,
    connected_pending: bool,
    disconnected_announced: bool,
}

impl NativeWebSocketClient {
    /// Connects and completes the WebSocket handshake.
    pub fn connect(config: NativeWebSocketClientConfig) -> io::Result<Self> {
        if config.identity.path.is_empty() || config.identity.subprotocol.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket path and subprotocol must be non-empty",
            ));
        }
        if config.handshake_timeout.is_zero()
            || config.handshake_timeout > MAX_WEBSOCKET_HANDSHAKE_TIMEOUT
            || config.ping_interval_ms > MAX_WEBSOCKET_LIVENESS_MS
            || config.timeout_ms > MAX_WEBSOCKET_LIVENESS_MS
            || (config.ping_interval_ms != 0
                && config.timeout_ms != 0
                && config.timeout_ms <= config.ping_interval_ms)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid bounded WebSocket timeout configuration",
            ));
        }
        config
            .reliable
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let canonical = canonical_origin(&config.origin)?;
        if canonical != config.origin {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("WebSocket origin must be canonical: {canonical}"),
            ));
        }
        let mut request = config.url.into_client_request().map_err(io::Error::other)?;
        request.headers_mut().insert(
            ORIGIN,
            HeaderValue::from_str(&config.origin).map_err(io::Error::other)?,
        );
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&config.identity.subprotocol).map_err(io::Error::other)?,
        );
        let host = request.uri().host().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "WebSocket URL has no host")
        })?;
        // The URI keeps an IPv6 literal's brackets; the resolver takes it bare.
        let host = host
            .strip_prefix('[')
            .and_then(|literal| literal.strip_suffix(']'))
            .unwrap_or(host);
        let port = request.uri().port_u16().unwrap_or(80);
        // One budget covers host resolution, every address attempt and the
        // HTTP upgrade.
        let deadline = Instant::now() + config.handshake_timeout;
        let addrs = resolve_by((host.to_owned(), port), deadline)?;
        let stream = connect_first_reachable(addrs, deadline)?;
        stream.set_nonblocking(true)?;
        let shared = SharedPeer::new(
            config.identity.magic,
            config.ping_interval_ms,
            config.timeout_ms,
            &config.reliable,
        );
        let (mut worker, waker) = IoWorker::new()?;
        let response = worker.connect(request, stream, deadline, Arc::clone(&shared))?;
        let selected = response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok());
        if selected != Some(config.identity.subprotocol.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server did not select the required WebSocket subprotocol",
            ));
        }
        Ok(Self {
            shared,
            worker: worker.spawn(waker)?,
            connected_pending: true,
            disconnected_announced: false,
        })
    }

    fn request_turn(&self) {
        if self.shared.request_turn() {
            self.worker.wake();
        }
    }
}

impl ClientIo for NativeWebSocketClient {
    fn poll(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        self.shared.advance(now_ms);
        let mut events = Vec::new();
        if std::mem::take(&mut self.connected_pending) {
            events.push(ClientEvent::Connected);
        }
        {
            let mut state = lock(&self.shared.state);
            while let Some((delivery, payload)) = state.pop_inbound() {
                events.push(ClientEvent::Message { delivery, payload });
            }
            if let Some(reason) = state.terminal()
                && !self.disconnected_announced
            {
                self.disconnected_announced = true;
                self.shared.shutdown.store(true, Ordering::Release);
                events.push(ClientEvent::Disconnected { reason });
            }
        }
        self.request_turn();
        events
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        let result = lock(&self.shared.state).send(delivery, payload);
        // A send that ended the peer closes its socket promptly; any other
        // refusal changed nothing.
        if result == Err(SendError::Disconnected) {
            self.request_turn();
        }
        result
    }

    fn capacity(&self, lane: Lane) -> ReliableCapacity {
        lock(&self.shared.state).capacity(lane)
    }

    fn flush(&mut self, now_ms: u64) {
        let _ = lock(&self.shared.state).release_outbound();
        self.shared.advance(now_ms);
        self.request_turn();
    }

    fn disconnect(&mut self, now_ms: u64) {
        {
            let mut state = lock(&self.shared.state);
            let _ = state.release_outbound();
            state.begin_graceful_close(now_ms);
        }
        self.shared.advance(now_ms);
        self.request_turn();
    }

    fn rtt(&self) -> RttEstimate {
        self.shared.rtt()
    }
}

impl Drop for NativeWebSocketClient {
    fn drop(&mut self) {
        self.shared.close(DisconnectReason::Local);
        // The worker handle drops next: it stops the worker, which writes the
        // Close, and joins it.
    }
}

/// Maximum messages one socket turn drains before yielding to outbound work.
const MAX_READS_PER_WAKE: usize = 128;

/// What one [`socket_tick`] decided: stop serving the socket, or go round
/// again (waiting for readiness first only when the turn found nothing to do).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketTick {
    Continue { idle: bool },
    Stop,
}

/// Offers one binary frame to the peer state. Returns whether the frame is
/// held because its lane is full; any other refusal closes the peer state
/// with its reason, and a frame that does not decode breaks the framing.
fn deliver_frame(shared: &SharedPeer, bytes: &[u8]) -> Result<Received, ()> {
    let Ok(envelope) = decode_envelope(shared.magic, bytes) else {
        shared.close(DisconnectReason::ProtocolViolation);
        return Err(());
    };
    lock(&shared.state).receive_or_hold(envelope).map_err(drop)
}

/// One socket turn: read buffered inbound frames into the shared
/// peer state (bounded per turn so a flood cannot starve outbound work),
/// send a due ping, drain released outbound frames, and finish a graceful
/// close. A frame whose lane cannot take its message stays in `held` and
/// nothing more is read until the caller's `poll` makes room (read
/// backpressure: the kernel buffer fills and TCP stops the sender). The
/// I/O worker runs it for a connection on readiness or a caller request;
/// tests drive it over an in-memory stream. Time never enters here —
/// liveness is advanced by the caller's clock through
/// [`SharedPeer::advance`].
fn socket_tick<Stream>(
    socket: &mut WebSocket<Stream>,
    shared: &SharedPeer,
    pending: &mut Option<Message>,
    held: &mut Option<tungstenite::Bytes>,
) -> SocketTick
where
    Stream: Read + Write,
{
    if shared.shutdown.load(Ordering::Acquire) || lock(&shared.state).terminal().is_some() {
        let _ = socket.close(None);
        return SocketTick::Stop;
    }
    // Only a graceful close writes our Close frame while the peer is not yet
    // terminal (a received Close or a failure makes it terminal above).
    if !socket.can_write() {
        return flush_graceful_close(socket, shared);
    }
    let mut reads = 0;
    let mut closing = false;
    if let Some(bytes) = held.take() {
        match deliver_frame(shared, &bytes) {
            Ok(Received::Held) => *held = Some(bytes),
            // Reading resumes now: the peer's silence counts from here.
            Ok(Received::Accepted) => shared.observe_activity(),
            Err(()) => closing = true,
        }
    }
    while reads < MAX_READS_PER_WAKE && !closing && held.is_none() {
        match socket.read() {
            Ok(Message::Binary(bytes)) => {
                reads += 1;
                shared.observe_activity();
                match deliver_frame(shared, &bytes) {
                    Ok(Received::Held) => *held = Some(bytes),
                    Ok(Received::Accepted) => {}
                    Err(()) => closing = true,
                }
            }
            // Tungstenite queues the pong itself; the next read or flush
            // writes it.
            Ok(Message::Ping(_)) => {
                reads += 1;
                shared.observe_activity();
            }
            Ok(Message::Pong(payload)) => {
                reads += 1;
                shared.observe_pong(&payload);
            }
            Ok(Message::Close(_)) => {
                shared.close(DisconnectReason::Peer);
                closing = true;
            }
            // Text, and a frame past `MAX_WEBSOCKET_FRAME_BYTES`, break the
            // framing.
            Ok(Message::Text(_) | Message::Frame(_)) | Err(tungstenite::Error::Capacity(_)) => {
                shared.close(DisconnectReason::ProtocolViolation);
                closing = true;
            }
            Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                break;
            }
            Err(_) => {
                shared.close(DisconnectReason::Transport);
                closing = true;
            }
        }
    }
    if closing {
        return SocketTick::Continue { idle: false };
    }
    // A held frame waits for the caller's request, not for readiness.
    let saturated = reads >= MAX_READS_PER_WAKE && held.is_none();

    if let Some(payload) = shared.take_ping()
        && send_ping(socket, payload).is_err()
    {
        shared.close(DisconnectReason::Transport);
        return SocketTick::Continue { idle: false };
    }

    let magic = shared.magic;
    let drain = drain_outbound(socket, pending, || {
        let mut state = lock(&shared.state);
        let frame = state.pop_released_frame()?;
        let Ok(encoded) = encode_envelope(magic, &frame.envelope()) else {
            state.close(DisconnectReason::ProtocolViolation);
            return None;
        };
        Some(Message::Binary(encoded.into()))
    });
    let Ok(drain) = drain else {
        shared.close(DisconnectReason::Transport);
        return SocketTick::Stop;
    };
    // `Empty` means nothing is pending and tungstenite's write buffer has
    // flushed, so the Close frame follows every accepted frame.
    if drain == DrainResult::Empty && lock(&shared.state).graceful_close_drained() {
        return flush_graceful_close(socket, shared);
    }
    SocketTick::Continue { idle: !saturated }
}

/// Writes or keeps flushing our Close frame, then reads and discards what
/// the peer still sends until its Close reply or the end of the stream, as
/// tungstenite's close handshake asks. Flushed bytes are only in the
/// kernel: closing a socket with unread data resets the connection and can
/// discard them, and reading keeps two sides closing at once from stalling
/// each other. The close finishes as `Local`; the graceful-close deadline
/// ends a peer that never takes it or never answers.
fn flush_graceful_close<Stream>(socket: &mut WebSocket<Stream>, shared: &SharedPeer) -> SocketTick
where
    Stream: Read + Write,
{
    let flushed = match socket.close(None) {
        Ok(()) => true,
        Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(_) => {
            shared.close(DisconnectReason::Transport);
            return SocketTick::Stop;
        }
    };
    let finish = || {
        lock(&shared.state).finish_graceful_close();
        SocketTick::Stop
    };
    for _ in 0..MAX_READS_PER_WAKE {
        match socket.read() {
            // The peer's Close, a reply or one crossing ours: every data
            // frame was flushed before ours was queued.
            Ok(Message::Close(_)) => return finish(),
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                return SocketTick::Continue { idle: true };
            }
            // The peer ended the stream after taking everything we sent.
            Err(_) if flushed => return finish(),
            Err(_) => {
                shared.close(DisconnectReason::Transport);
                return SocketTick::Stop;
            }
        }
    }
    SocketTick::Continue { idle: false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websocket::{Fragment, WEBSOCKET_FRAGMENT_BYTES};
    use tungstenite::protocol::Role;

    #[test]
    fn connect_falls_back_past_an_unreachable_resolved_address() {
        // A port whose listener has closed refuses connections.
        let refused = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("bind closed port");
        let live = TcpListener::bind("127.0.0.1:0").expect("bind live port");
        let stream = connect_first_reachable(
            [refused, live.local_addr().expect("live addr")],
            // Windows can spend about 2 s retrying before it reports a refusal.
            Instant::now() + MAX_WEBSOCKET_HANDSHAKE_TIMEOUT,
        )
        .expect("fallback reaches the live address");
        let (_, peer) = live.accept().expect("live listener accepts");
        assert_eq!(peer, stream.local_addr().expect("client addr"));
    }

    /// A host lookup that blocks until its test releases it.
    struct StalledLookup(mpsc::Receiver<()>);

    impl ToSocketAddrs for StalledLookup {
        type Iter = std::vec::IntoIter<SocketAddr>;

        fn to_socket_addrs(&self) -> io::Result<Self::Iter> {
            let _ = self.0.recv();
            Ok(Vec::new().into_iter())
        }
    }

    #[test]
    fn a_stalled_host_lookup_ends_at_the_deadline() {
        let (release, released) = mpsc::channel();
        let error = resolve_by(
            StalledLookup(released),
            Instant::now() + Duration::from_millis(20),
        )
        .expect_err("a lookup that never answers must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        drop(release);
    }

    /// An in-memory socket: reads consume a scripted inbound buffer and
    /// report `WouldBlock` when it runs dry, like a non-blocking TCP stream;
    /// writes accumulate for inspection, or report `WouldBlock` while
    /// `writes_blocked` is set, like a peer that has stopped reading. A
    /// `write_budget` takes that many more bytes before blocking.
    #[derive(Default)]
    struct Scripted {
        inbound: VecDeque<u8>,
        outbound: Vec<u8>,
        writes_blocked: bool,
        write_budget: Option<usize>,
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.inbound.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = buf.len().min(self.inbound.len());
            for slot in &mut buf[..n] {
                *slot = self.inbound.pop_front().expect("length checked");
            }
            Ok(n)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = self
                .write_budget
                .map_or(buf.len(), |budget| budget.min(buf.len()));
            if self.writes_blocked || (n == 0 && !buf.is_empty()) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            if let Some(budget) = &mut self.write_budget {
                *budget -= n;
            }
            self.outbound.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    const MAGIC: [u8; 3] = *b"TST";

    /// Bytes a client would put on the wire for `messages` (masked frames).
    fn client_frames(messages: Vec<Message>) -> Vec<u8> {
        let mut client = WebSocket::from_raw_socket(Scripted::default(), Role::Client, None);
        for message in messages {
            client.send(message).expect("client frame");
        }
        std::mem::take(&mut client.get_mut().outbound)
    }

    /// Decode every frame the server wrote, as a client would read them.
    fn server_frames(bytes: Vec<u8>) -> Vec<Message> {
        let stream = Scripted {
            inbound: bytes.into(),
            ..Scripted::default()
        };
        let mut client = WebSocket::from_raw_socket(stream, Role::Client, None);
        let mut out = Vec::new();
        loop {
            match client.read() {
                Ok(message) => out.push(message),
                Err(_) => return out,
            }
        }
    }

    fn envelope(delivery: Delivery, sequence: u64, payload: &[u8]) -> Message {
        let envelope = crate::websocket::Envelope {
            delivery,
            sequence,
            fragment: crate::websocket::Fragment::Whole,
            payload,
        };
        Message::Binary(encode_envelope(MAGIC, &envelope).expect("envelope").into())
    }

    fn shared(ping_interval_ms: u64, timeout_ms: u64) -> Arc<SharedPeer> {
        SharedPeer::new(
            MAGIC,
            ping_interval_ms,
            timeout_ms,
            &ReliableConfig::DEFAULT,
        )
    }

    fn server_socket(inbound: Vec<u8>) -> WebSocket<Scripted> {
        let stream = Scripted {
            inbound: inbound.into(),
            ..Scripted::default()
        };
        WebSocket::from_raw_socket(stream, Role::Server, Some(websocket_config()))
    }

    fn tick(socket: &mut WebSocket<Scripted>, shared: &SharedPeer) -> SocketTick {
        let (mut pending, mut held) = (None, None);
        let tick = socket_tick(socket, shared, &mut pending, &mut held);
        assert!(held.is_none(), "these tests never fill a lane");
        tick
    }

    /// Inbound binary envelopes land in the peer's inbound queue in order,
    /// and count as activity at the caller's next clock, so a peer that is
    /// talking is never timed out.
    #[test]
    fn socket_tick_queues_inbound_envelopes_and_counts_them_as_activity() {
        let shared = shared(0, 50);
        shared.advance(0);
        let mut socket = server_socket(client_frames(vec![
            envelope(Delivery::RELIABLE_ORDERED, 0, b"first"),
            envelope(Delivery::LatestState, 1, b"state"),
        ]));
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: true }
        );
        let mut state = lock(&shared.state);
        assert_eq!(
            state.pop_inbound(),
            Some((Delivery::RELIABLE_ORDERED, b"first".to_vec()))
        );
        assert_eq!(
            state.pop_inbound(),
            Some((Delivery::LatestState, b"state".to_vec()))
        );
        assert!(state.pop_inbound().is_none());
        drop(state);
        shared.advance(60);
        assert_eq!(
            lock(&shared.state).terminal(),
            None,
            "activity read by the worker must reset the timeout"
        );
    }

    fn fragment_frame(delivery: Delivery, fragment: Fragment, payload: &[u8]) -> Message {
        let envelope = crate::websocket::Envelope {
            delivery,
            sequence: 0,
            fragment,
            payload,
        };
        Message::Binary(encode_envelope(MAGIC, &envelope).expect("envelope").into())
    }

    /// Defect (#269, design §13): a full inbound lane that closes a healthy
    /// peer, a frame read past a held one (reordering the lane), a held
    /// frame lost or delivered twice, a stall that never resumes once
    /// `poll` makes room or wakes the worker while nothing changed, or this
    /// side's own stall counted as the peer's inactivity — or the exemption
    /// outliving the stall. Oracle: netcode.md 13's read backpressure and
    /// the lane's order: with a one-message lane, three frames arrive one
    /// per poll in order, the peer stays open across a stall longer than
    /// its timeout, and once nothing is held its silence times it out.
    #[test]
    fn socket_tick_holds_a_frame_its_lane_cannot_take_until_poll_makes_room() {
        let mut config = ReliableConfig::DEFAULT;
        config.lanes[0].inbound_messages = 1;
        let shared = SharedPeer::new(MAGIC, 0, 50, &config);
        shared.advance(0);
        let mut socket = server_socket(client_frames(
            [b"r1", b"r2", b"r3"]
                .map(|payload| envelope(Delivery::RELIABLE_ORDERED, 0, payload))
                .into(),
        ));
        let (mut pending, mut held) = (None, None);
        let mut received = Vec::new();
        for now_ms in [100, 200, 300] {
            assert_eq!(
                socket_tick(&mut socket, &shared, &mut pending, &mut held),
                SocketTick::Continue { idle: true }
            );
            shared.advance(now_ms);
            if held.is_some() {
                // Longer than the timeout with nothing read: the stall.
                shared.advance(now_ms + 60);
            }
            assert_eq!(lock(&shared.state).terminal(), None, "at {now_ms}");
            assert!(!shared.needs_turn(), "nothing to do before a poll");
            let message = lock(&shared.state).pop_inbound();
            received.extend(message.map(|(_, payload)| payload));
            assert_eq!(shared.needs_turn(), held.is_some(), "at {now_ms}");
        }
        assert_eq!(received, [b"r1".to_vec(), b"r2".to_vec(), b"r3".to_vec()]);
        assert!(held.is_none());
        shared.advance(349);
        assert_eq!(lock(&shared.state).terminal(), None);
        shared.advance(350);
        assert_eq!(
            lock(&shared.state).terminal(),
            Some(DisconnectReason::TimedOut),
            "silence counts again once nothing is held"
        );

        // A peer that stops halfway through a message is not this side's
        // stall: its silence times it out.
        let shared = SharedPeer::new(MAGIC, 0, 50, &ReliableConfig::DEFAULT);
        shared.advance(0);
        let mut socket = server_socket(client_frames(vec![fragment_frame(
            Delivery::RELIABLE_ORDERED,
            Fragment::First { total: 40_000 },
            &[1; WEBSOCKET_FRAGMENT_BYTES],
        )]));
        tick(&mut socket, &shared);
        shared.advance(10);
        shared.advance(60);
        assert_eq!(
            lock(&shared.state).terminal(),
            Some(DisconnectReason::TimedOut)
        );
    }

    /// Defect (#269): a WebSocket receiver that checks declared totals
    /// against the default cap instead of its configured one. Oracle: the
    /// configured cap (netcode.md 15) — a first fragment declaring it,
    /// past the default, waits for its message; one declaring a byte more
    /// closes the peer as `ProtocolViolation` before anything is buffered,
    /// and a whole message received before it in the same turn is not
    /// delivered.
    #[test]
    fn a_declared_total_past_the_configured_cap_is_a_protocol_violation() {
        const CAP: u32 = 1 << 20;
        let mut config = ReliableConfig::DEFAULT;
        config.max_message_bytes = CAP as usize;
        for (total, violation) in [(CAP, false), (CAP + 1, true)] {
            let shared = SharedPeer::new(MAGIC, 0, 0, &config);
            let mut socket = server_socket(client_frames(vec![
                envelope(Delivery::Reliable(Lane::new(1).unwrap()), 0, b"whole"),
                fragment_frame(
                    Delivery::Reliable(Lane::new(2).unwrap()),
                    Fragment::First { total },
                    b"ab",
                ),
            ]));
            tick(&mut socket, &shared);
            let mut state = lock(&shared.state);
            if violation {
                assert_eq!(state.terminal(), Some(DisconnectReason::ProtocolViolation));
                assert_eq!(state.pop_inbound(), None);
            } else {
                assert_eq!(state.terminal(), None);
                assert_eq!(
                    state.pop_inbound(),
                    Some((Delivery::Reliable(Lane::new(1).unwrap()), b"whole".to_vec()))
                );
            }
        }
    }

    /// Text, malformed, and Close frames fail closed with the spec'd
    /// reason, and the following turn writes a Close frame and stops.
    #[test]
    fn socket_tick_fails_closed_on_text_malformed_and_close_frames() {
        let cases: [(Message, DisconnectReason); 3] = [
            (
                Message::Text("hi".into()),
                DisconnectReason::ProtocolViolation,
            ),
            (
                Message::Binary(vec![1, 2, 3].into()),
                DisconnectReason::ProtocolViolation,
            ),
            (Message::Close(None), DisconnectReason::Peer),
        ];
        for (message, reason) in cases {
            let shared = shared(0, 0);
            let mut socket = server_socket(client_frames(vec![message]));
            assert_eq!(
                tick(&mut socket, &shared),
                SocketTick::Continue { idle: false }
            );
            assert_eq!(lock(&shared.state).terminal(), Some(reason));
            assert_eq!(tick(&mut socket, &shared), SocketTick::Stop);
            let written = server_frames(std::mem::take(&mut socket.get_mut().outbound));
            assert!(
                written.iter().any(|m| matches!(m, Message::Close(_))),
                "{reason:?}: {written:?}"
            );
        }
    }

    /// Released outbound frames go on the wire in queue order as binary
    /// envelopes; the latest-state slot is sent once, at its final value,
    /// and acknowledged so the slot is free again.
    #[test]
    fn socket_tick_writes_released_outbound_in_order_and_acknowledges_latest() {
        let shared = shared(0, 0);
        {
            let mut state = lock(&shared.state);
            state.send(Delivery::RELIABLE_ORDERED, b"r1").unwrap();
            state.send(Delivery::LatestState, b"stale").unwrap();
            state.send(Delivery::RELIABLE_ORDERED, b"r2").unwrap();
            state.send(Delivery::LatestState, b"fresh").unwrap();
            state.release_outbound().unwrap();
        }
        let mut socket = server_socket(Vec::new());
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: true }
        );
        let frames: Vec<(Delivery, u64, Vec<u8>)> =
            server_frames(std::mem::take(&mut socket.get_mut().outbound))
                .into_iter()
                .map(|m| match m {
                    Message::Binary(bytes) => {
                        let e = decode_envelope(MAGIC, &bytes).expect("envelope");
                        (e.delivery, e.sequence, e.payload.to_vec())
                    }
                    other => panic!("unexpected frame {other:?}"),
                })
                .collect();
        // Reliable frames carry sequence 0; the latest slot is sent once, at
        // its final value, under a nonzero sequence (the exact number is the
        // queue's business).
        assert_eq!(frames.len(), 3, "{frames:?}");
        assert_eq!(
            (frames[0].0, frames[0].1, &frames[0].2[..]),
            (Delivery::RELIABLE_ORDERED, 0, &b"r1"[..])
        );
        assert_eq!(
            (frames[1].0, frames[1].1, &frames[1].2[..]),
            (Delivery::RELIABLE_ORDERED, 0, &b"r2"[..])
        );
        assert_eq!(frames[2].0, Delivery::LatestState);
        assert_ne!(frames[2].1, 0);
        assert_eq!(&frames[2].2[..], b"fresh");
        assert!(!lock(&shared.state).has_released_outbound());
        // The acknowledged slot accepts the next state with the next sequence.
        lock(&shared.state)
            .send(Delivery::LatestState, b"next")
            .expect("slot free after acknowledgement");
    }

    /// Pings leave on the caller's clock, carry the send time, and the pong
    /// read by a later turn is sampled into the RTT at the next clock.
    #[test]
    fn socket_tick_pings_on_the_caller_clock_and_samples_the_pong() {
        let shared = shared(10, 0);
        shared.advance(0);
        let mut socket = server_socket(Vec::new());
        tick(&mut socket, &shared);
        assert!(
            socket.get_ref().outbound.is_empty(),
            "no ping before the interval"
        );
        shared.advance(10);
        tick(&mut socket, &shared);
        let frames = server_frames(std::mem::take(&mut socket.get_mut().outbound));
        let payload = match &frames[..] {
            [Message::Ping(payload)] => payload.clone(),
            other => panic!("expected one ping, got {other:?}"),
        };
        assert_eq!(&payload[..], 10u64.to_be_bytes());
        socket
            .get_mut()
            .inbound
            .extend(client_frames(vec![Message::Pong(payload)]));
        tick(&mut socket, &shared);
        shared.advance(25);
        assert_eq!(shared.rtt().srtt_ms, 15);
    }

    /// A pong and a due ping that meet a peer which has stopped reading stay
    /// buffered rather than failing the peer, the turn waits for the writable
    /// edge, and both go out once the socket takes writes again.
    #[test]
    fn socket_tick_holds_control_frames_while_writes_block() {
        let shared = shared(10, 0);
        shared.advance(0);
        shared.advance(10);
        let mut socket = server_socket(client_frames(vec![Message::Ping(b"hi".to_vec().into())]));
        socket.get_mut().writes_blocked = true;
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: true }
        );
        assert_eq!(lock(&shared.state).terminal(), None);

        socket.get_mut().writes_blocked = false;
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: true }
        );
        let frames = server_frames(std::mem::take(&mut socket.get_mut().outbound));
        assert_eq!(
            frames,
            vec![
                Message::Pong(b"hi".to_vec().into()),
                Message::Ping(10u64.to_be_bytes().to_vec().into()),
            ]
        );
    }

    /// A flood is read at most `MAX_READS_PER_WAKE` frames per turn; the
    /// rest wait for the next turn, and a saturated turn does not idle.
    #[test]
    fn socket_tick_bounds_reads_per_turn() {
        let shared = shared(0, 0);
        let frames = (0..MAX_READS_PER_WAKE + 2)
            .map(|_| envelope(Delivery::RELIABLE_ORDERED, 0, b"x"))
            .collect();
        let mut socket = server_socket(client_frames(frames));
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: false }
        );
        let mut queued = 0;
        while lock(&shared.state).pop_inbound().is_some() {
            queued += 1;
        }
        assert_eq!(queued, MAX_READS_PER_WAKE);
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: true }
        );
        queued = 0;
        while lock(&shared.state).pop_inbound().is_some() {
            queued += 1;
        }
        assert_eq!(queued, 2);
    }

    /// A graceful close drains the released reliable frames first, then
    /// writes the Close frame and stops the worker as `Local` once the peer
    /// answers it.
    #[test]
    fn socket_tick_drains_before_finishing_a_graceful_close() {
        let shared = shared(0, 0);
        {
            let mut state = lock(&shared.state);
            state.send(Delivery::RELIABLE_ORDERED, b"bye").unwrap();
            state.begin_graceful_close(0);
            state.release_outbound().unwrap();
        }
        let mut socket = server_socket(Vec::new());
        assert_eq!(
            tick(&mut socket, &shared),
            SocketTick::Continue { idle: true }
        );
        let frames = server_frames(std::mem::take(&mut socket.get_mut().outbound));
        assert!(
            matches!(&frames[..], [Message::Binary(bytes), Message::Close(_)]
                if decode_envelope(MAGIC, bytes).map(|e| e.payload) == Ok(&b"bye"[..])),
            "{frames:?}"
        );
        socket
            .get_mut()
            .inbound
            .extend(client_frames(vec![Message::Close(None)]));
        assert_eq!(tick(&mut socket, &shared), SocketTick::Stop);
        assert_eq!(
            lock(&shared.state).terminal(),
            Some(DisconnectReason::Local)
        );
    }

    /// Defect (#304): a graceful close that stops the worker while a frame
    /// is still pending or buffered behind a blocked socket, before its Close
    /// frame has flushed, or with the peer's data unread (closing such a
    /// socket resets the connection, which can discard what was flushed);
    /// or a ping written after the Close, which fails the peer. Oracle: the
    /// bytes the peer receives and those left unread — while the socket
    /// blocks the close stays open, then the peer reads the frame and the
    /// Close and nothing else, and once the peer's data and Close reply have
    /// been read the worker stops as `Local`.
    #[test]
    fn a_graceful_close_waits_for_a_blocked_socket_to_take_every_frame_and_the_close() {
        let shared = shared(10, 0);
        shared.advance(0);
        {
            let mut state = lock(&shared.state);
            state.send(Delivery::RELIABLE_ORDERED, b"bye").unwrap();
            state.begin_graceful_close(0);
            state.release_outbound().unwrap();
        }
        let Message::Binary(bye) = envelope(Delivery::RELIABLE_ORDERED, 0, b"bye") else {
            unreachable!()
        };
        // An unmasked server frame under 126 bytes has a two-byte header.
        let bye_frame_len = 2 + bye.len();
        let mut socket = server_socket(Vec::new());
        let (mut pending, mut held) = (None, None);
        // Nothing is writable, then only the data frame is.
        for budget in [0, bye_frame_len] {
            socket.get_mut().write_budget = Some(budget);
            assert_eq!(
                socket_tick(&mut socket, &shared, &mut pending, &mut held),
                SocketTick::Continue { idle: true },
                "budget {budget}"
            );
            assert_eq!(lock(&shared.state).terminal(), None, "budget {budget}");
        }
        // A ping comes due and the peer sends data before it reads our Close.
        shared.advance(10);
        socket.get_mut().write_budget = None;
        socket.get_mut().inbound.extend(client_frames(vec![envelope(
            Delivery::RELIABLE_ORDERED,
            0,
            b"late",
        )]));
        assert_eq!(
            socket_tick(&mut socket, &shared, &mut pending, &mut held),
            SocketTick::Continue { idle: true }
        );
        assert_eq!(lock(&shared.state).terminal(), None);
        assert!(
            socket.get_ref().inbound.is_empty(),
            "the peer's data was left unread"
        );
        let frames = server_frames(std::mem::take(&mut socket.get_mut().outbound));
        assert!(
            matches!(&frames[..], [Message::Binary(bytes), Message::Close(_)] if *bytes == bye),
            "{frames:?}"
        );
        socket
            .get_mut()
            .inbound
            .extend(client_frames(vec![Message::Close(None)]));
        assert_eq!(
            socket_tick(&mut socket, &shared, &mut pending, &mut held),
            SocketTick::Stop
        );
        assert_eq!(
            lock(&shared.state).terminal(),
            Some(DisconnectReason::Local)
        );

        // A peer Close crossing our still-blocked Close finishes the close.
        let crossing = self::shared(0, 0);
        {
            let mut state = lock(&crossing.state);
            state.send(Delivery::RELIABLE_ORDERED, b"bye").unwrap();
            state.begin_graceful_close(0);
            state.release_outbound().unwrap();
        }
        let mut socket = server_socket(Vec::new());
        socket.get_mut().write_budget = Some(bye_frame_len);
        assert_eq!(
            tick(&mut socket, &crossing),
            SocketTick::Continue { idle: true }
        );
        socket
            .get_mut()
            .inbound
            .extend(client_frames(vec![Message::Close(None)]));
        assert_eq!(tick(&mut socket, &crossing), SocketTick::Stop);
        assert_eq!(
            lock(&crossing.state).terminal(),
            Some(DisconnectReason::Local)
        );

        // A socket that never drains is let go at the graceful-close deadline.
        let stuck = self::shared(0, 0);
        {
            let mut state = lock(&stuck.state);
            state.send(Delivery::RELIABLE_ORDERED, b"bye").unwrap();
            state.begin_graceful_close(0);
            state.release_outbound().unwrap();
        }
        let mut socket = server_socket(Vec::new());
        socket.get_mut().writes_blocked = true;
        assert_eq!(
            tick(&mut socket, &stuck),
            SocketTick::Continue { idle: true }
        );
        stuck.advance(crate::websocket::GRACEFUL_CLOSE_TIMEOUT_MS);
        assert_eq!(tick(&mut socket, &stuck), SocketTick::Stop);
    }

    fn test_identity() -> WebSocketIdentity {
        WebSocketIdentity::new(*b"TST", "/game/ws", "test.v1")
    }

    #[test]
    fn canonical_origin_matrix_rejects_lookalikes_and_null() {
        assert_eq!(
            canonical_origin("https://game.example").unwrap(),
            "https://game.example"
        );
        assert_eq!(
            canonical_origin("https://game.example:443").unwrap(),
            "https://game.example"
        );
        for rejected in [
            "null",
            " https://game.example",
            "https://GAME.example",
            "https://game.example/",
            "https://user@game.example",
            "file://game.example",
        ] {
            assert!(
                OriginPolicy::exact([rejected.to_owned()]).is_err(),
                "accepted {rejected}"
            );
        }
        assert!(OriginPolicy::exact(Vec::new()).is_err());
    }

    #[test]
    fn canonical_origin_keeps_ipv6_literals_in_browser_serialization() {
        // Expected values are the WHATWG URL origin serialization browsers send.
        for canonical in [
            "http://[::1]:3000",
            "https://[2001:db8::1]",
            "http://[1::]",
            "http://[::]:8080",
            "http://[1:0:0:2::3]",
        ] {
            assert!(
                OriginPolicy::exact([canonical.to_owned()]).is_ok(),
                "rejected {canonical}"
            );
        }
        assert_eq!(
            canonical_origin("https://[2001:db8::1]:443").unwrap(),
            "https://[2001:db8::1]"
        );
        assert_eq!(
            canonical_origin("http://[::FFFF:1.2.3.4]:3000").unwrap(),
            "http://[::ffff:102:304]:3000"
        );
        assert_eq!(
            canonical_origin("http://[0:0:0:0:0:0:0:1]").unwrap(),
            "http://[::1]"
        );
        assert_eq!(
            canonical_origin("http://[1:0:0:2:0:0:0:3]").unwrap(),
            "http://[1:0:0:2::3]"
        );
        assert_eq!(
            canonical_origin("http://[1:0:0:2:0:0:3:4]").unwrap(),
            "http://[1::2:0:0:3:4]"
        );
        assert_eq!(
            canonical_origin("http://[1:0:2:3:4:5:6:7]").unwrap(),
            "http://[1:0:2:3:4:5:6:7]"
        );
        for rejected in [
            "http://[0::1]:3000",
            "http://[::ABCD]",
            "http://[::1]:80",
            "http://[fe80::1%25en0]",
            "http://[v1.x]",
        ] {
            assert!(
                OriginPolicy::exact([rejected.to_owned()]).is_err(),
                "accepted {rejected}"
            );
        }
    }

    #[test]
    fn server_config_requires_an_explicit_origin_policy() {
        let result = NativeWebSocketServer::bind(NativeWebSocketServerConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            test_identity(),
        ));
        assert!(matches!(
            result,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn hostile_native_config_is_rejected_before_binding() {
        let mut config = NativeWebSocketServerConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            test_identity(),
        )
        .with_origin_policy(OriginPolicy::allow_any());
        config.max_connections = usize::MAX;
        assert!(matches!(
            NativeWebSocketServer::bind(config),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));

        let mut config = NativeWebSocketServerConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            test_identity(),
        )
        .with_origin_policy(OriginPolicy::allow_any());
        config.handshake_timeout = Duration::ZERO;
        assert!(matches!(
            NativeWebSocketServer::bind(config),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));

        let mut config = NativeWebSocketServerConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            test_identity(),
        )
        .with_origin_policy(OriginPolicy::allow_any());
        config.max_accepts_per_second = 0;
        assert!(matches!(
            NativeWebSocketServer::bind(config),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));

        let mut config = NativeWebSocketServerConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            test_identity(),
        )
        .with_origin_policy(OriginPolicy::allow_any());
        config.max_events_per_poll = 0;
        assert!(matches!(
            NativeWebSocketServer::bind(config),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn accept_rate_limiter_uses_a_rolling_window() {
        let started = Instant::now();
        let mut limiter = AcceptRateLimiter::new(2);
        assert!(limiter.allow(started));
        assert!(limiter.allow(started + Duration::from_millis(100)));
        assert!(!limiter.allow(started + Duration::from_millis(999)));
        assert!(limiter.allow(started + Duration::from_secs(1)));
    }

    #[test]
    fn server_poll_is_globally_bounded_and_round_robin() {
        fn peer(payloads: &[&[u8]]) -> ServerPeer {
            let shared = shared(0, 0);
            for payload in payloads {
                lock(&shared.state)
                    .receive(crate::websocket::Envelope {
                        delivery: Delivery::RELIABLE_ORDERED,
                        sequence: 0,
                        fragment: crate::websocket::Fragment::Whole,
                        payload,
                    })
                    .unwrap();
            }
            ServerPeer {
                shared,
                connected_announced: true,
                disconnected_announced: false,
            }
        }

        let first = ConnectionId::MIN;
        let second = first.checked_next().unwrap();
        let registry = Arc::new(Mutex::new(BTreeMap::from([
            (first, peer(&[b"a1", b"a2"])),
            (second, peer(&[b"b1", b"b2"])),
        ])));
        let mut server = NativeWebSocketServer {
            local_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            registry,
            accepting: Arc::new(AtomicBool::new(true)),
            occupancy: Arc::new(AtomicUsize::new(2)),
            worker: None,
            max_events_per_poll: 2,
            poll_cursor: 0,
        };

        let first_poll = server.poll(0);
        assert_eq!(first_poll.len(), 2);
        assert!(matches!(
            &first_poll[..],
            [
                ServerEvent::Message { conn: first_conn, payload: first_payload, .. },
                ServerEvent::Message { conn: second_conn, payload: second_payload, .. },
            ] if *first_conn == first
                && first_payload == b"a1"
                && *second_conn == second
                && second_payload == b"b1"
        ));

        let second_poll = server.poll(0);
        assert_eq!(second_poll.len(), 2);
        assert!(matches!(
            &second_poll[..],
            [
                ServerEvent::Message { conn: second_conn, payload: second_payload, .. },
                ServerEvent::Message { conn: first_conn, payload: first_payload, .. },
            ] if *second_conn == second
                && second_payload == b"b2"
                && *first_conn == first
                && first_payload == b"a2"
        ));
    }

    #[test]
    fn async_activity_and_pong_are_sampled_at_the_next_caller_clock() {
        let active = shared(0, 30);
        active.advance(0);
        active.observe_activity();
        active.advance(100);
        assert_eq!(lock(&active.state).terminal(), None);

        let pinging = shared(10, 0);
        pinging.advance(0);
        pinging.advance(10);
        let ping = pinging.take_ping().unwrap();
        pinging.observe_pong(&ping);
        pinging.advance(40);
        assert_eq!(pinging.rtt().srtt_ms, 30);
    }
}
