//! The I/O worker behind one native WebSocket server or client.
//!
//! One thread waits on socket readiness (mio, edge-triggered) and runs
//! [`socket_tick`] for each connection with something to do: a readiness
//! event, a turn the caller asked for through the [`Waker`]
//! ([`SharedPeer::request_turn`]), or a turn that stopped short (its read
//! bound reached, or a close under way). Each such connection gets one turn
//! per round, so a busy peer cannot starve the others. With nothing ready and
//! nothing requested the worker sleeps until the nearest handshake deadline
//! or accept retry.
//!
//! A server's worker also accepts, rate-limits and upgrades connections
//! without blocking, and admits them to the caller's registry. After an
//! interrupted accept, or one that took a connection the client had already
//! reset, it accepts again at once; after any other accept error it retries
//! after [`ACCEPT_RETRY`]. The listener closes only when admission stops.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use tungstenite::client::client_with_config;
use tungstenite::handshake::client::{Request as ClientRequest, Response as ClientResponse};
use tungstenite::handshake::server::ServerHandshake;
use tungstenite::handshake::{HandshakeError, MidHandshake};
use tungstenite::protocol::Message;
use tungstenite::{WebSocket, accept_hdr_with_config};

use super::{
    AcceptRateLimiter, Admission, ListenerContext, ServerPeer, SharedPeer, SocketTick, lock,
    socket_tick, websocket_config,
};
use crate::{ConnectionId, DisconnectReason};

const WAKER: Token = Token(0);
const LISTENER: Token = Token(1);
const FIRST_SOCKET: usize = 2;
const EVENT_CAPACITY: usize = 1_024;
/// How long accepting waits after an accept error that may leave the
/// connection in the backlog.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

type ServerUpgrade =
    Result<WebSocket<TcpStream>, HandshakeError<ServerHandshake<TcpStream, Admission>>>;

/// What an owner and its worker share besides each peer's state.
#[derive(Default)]
struct Signal {
    /// A wake is in flight; the worker clears it before taking requests.
    woken: AtomicBool,
    /// The owner is being dropped: close every socket and exit.
    stop: AtomicBool,
}

/// The owner's end of a worker. Dropping it stops and joins the worker.
pub(super) struct WorkerHandle {
    waker: Waker,
    signal: Arc<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Wakes the worker to take turn requests and to see admission stop. A
    /// wake already in flight covers this one.
    pub(super) fn wake(&self) {
        if !self.signal.woken.swap(true, Ordering::SeqCst) && self.waker.wake().is_err() {
            self.signal.woken.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.signal.stop.store(true, Ordering::SeqCst);
        let _ = self.waker.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Connection {
    socket: WebSocket<TcpStream>,
    shared: Arc<SharedPeer>,
    /// A message the socket could not take yet (see `drain_outbound`).
    pending: Option<Message>,
    /// Waiting in `IoWorker::runnable`.
    queued: bool,
}

impl Connection {
    fn new(socket: WebSocket<TcpStream>, shared: Arc<SharedPeer>) -> Self {
        Self {
            socket,
            shared,
            pending: None,
            queued: false,
        }
    }
}

/// A server-side upgrade in progress.
struct Handshake {
    conn: ConnectionId,
    deadline: Instant,
    mid: MidHandshake<ServerHandshake<TcpStream, Admission>>,
}

/// A server's listener and what its upgrades need.
struct Acceptor {
    /// Closed once admission stops.
    listener: Option<TcpListener>,
    context: ListenerContext,
    rate: AcceptRateLimiter,
    next_connection: Option<ConnectionId>,
    /// When to accept again after an accept error. A connection left in the
    /// backlog raises no new readiness edge.
    retry_at: Option<Instant>,
}

/// One server's or client's sockets and the loop that serves them.
pub(super) struct IoWorker {
    poll: Poll,
    events: Events,
    signal: Arc<Signal>,
    acceptor: Option<Acceptor>,
    handshakes: BTreeMap<Token, Handshake>,
    connections: BTreeMap<Token, Connection>,
    /// Connections owed a turn, each at most once.
    runnable: VecDeque<Token>,
    next_token: usize,
}

impl IoWorker {
    /// A worker with nothing to serve yet, and the waker its handle will hold.
    pub(super) fn new() -> io::Result<(Self, Waker)> {
        let poll = Poll::new()?;
        let waker = Waker::new(poll.registry(), WAKER)?;
        Ok((
            Self {
                poll,
                events: Events::with_capacity(EVENT_CAPACITY),
                signal: Arc::default(),
                acceptor: None,
                handshakes: BTreeMap::new(),
                connections: BTreeMap::new(),
                runnable: VecDeque::new(),
                next_token: FIRST_SOCKET,
            },
            waker,
        ))
    }

    /// Serves a server's nonblocking listener.
    pub(super) fn listen(
        &mut self,
        listener: std::net::TcpListener,
        context: ListenerContext,
    ) -> io::Result<()> {
        let mut listener = TcpListener::from_std(listener);
        self.poll
            .registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        self.acceptor = Some(Acceptor {
            listener: Some(listener),
            rate: AcceptRateLimiter::new(context.max_accepts_per_second),
            context,
            next_connection: Some(ConnectionId::MIN),
            retry_at: None,
        });
        Ok(())
    }

    /// Upgrades a client's nonblocking `stream` on the caller's thread,
    /// waiting on this worker's readiness for at most `timeout`, and keeps the
    /// socket for the worker to serve. The socket never changes hands, so
    /// bytes read past the upgrade response stay with it.
    pub(super) fn connect(
        &mut self,
        request: ClientRequest,
        stream: std::net::TcpStream,
        timeout: Duration,
        shared: Arc<SharedPeer>,
    ) -> io::Result<ClientResponse> {
        let deadline = Instant::now() + timeout;
        let token = self.next_token();
        let mut stream = TcpStream::from_std(stream);
        self.poll.registry().register(
            &mut stream,
            token,
            Interest::READABLE | Interest::WRITABLE,
        )?;
        let mut attempt = client_with_config(request, stream, Some(websocket_config()));
        loop {
            match attempt {
                Ok((socket, response)) => {
                    self.connections
                        .insert(token, Connection::new(socket, shared));
                    self.queue(token);
                    return Ok(response);
                }
                Err(HandshakeError::Interrupted(mid)) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(handshake_timed_out());
                    }
                    if let Err(error) = self.poll.poll(&mut self.events, Some(deadline - now))
                        && error.kind() != io::ErrorKind::Interrupted
                    {
                        return Err(error);
                    }
                    attempt = mid.handshake();
                }
                Err(HandshakeError::Failure(tungstenite::Error::Io(error)))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(handshake_timed_out());
                }
                Err(HandshakeError::Failure(tungstenite::Error::Io(error))) => return Err(error),
                Err(HandshakeError::Failure(error)) => return Err(io::Error::other(error)),
            }
        }
    }

    /// Starts the worker thread.
    pub(super) fn spawn(self, waker: Waker) -> io::Result<WorkerHandle> {
        let signal = Arc::clone(&self.signal);
        let thread = thread::Builder::new()
            .name("sgl-ws-io".to_owned())
            .spawn(move || self.run())?;
        Ok(WorkerHandle {
            waker,
            signal,
            thread: Some(thread),
        })
    }

    fn run(mut self) {
        let mut ready = Vec::new();
        loop {
            let timeout = if self.runnable.is_empty() {
                self.handshakes
                    .values()
                    .map(|handshake| handshake.deadline)
                    .chain(
                        self.acceptor
                            .as_ref()
                            .and_then(|acceptor| acceptor.retry_at),
                    )
                    .min()
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            } else {
                Some(Duration::ZERO)
            };
            if let Err(error) = self.poll.poll(&mut self.events, timeout) {
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                // Readiness itself failed: nothing can be served, and
                // dropping the worker ends every peer.
                return;
            }
            if self.signal.stop.load(Ordering::SeqCst) {
                self.finish();
                return;
            }
            ready.clear();
            ready.extend(self.events.iter().map(Event::token));
            for &token in &ready {
                match token {
                    WAKER => self.take_requests(),
                    LISTENER => self.accept(),
                    token if self.handshakes.contains_key(&token) => self.resume_handshake(token),
                    token => self.queue(token),
                }
            }
            self.retry_accept();
            self.expire_handshakes();
            self.close_listener_if_stopped();
            self.run_round();
        }
    }

    /// Queues every connection the caller asked a turn for.
    fn take_requests(&mut self) {
        self.signal.woken.store(false, Ordering::SeqCst);
        for (&token, connection) in &mut self.connections {
            if connection
                .shared
                .turn_requested
                .swap(false, Ordering::SeqCst)
                && !connection.queued
            {
                connection.queued = true;
                self.runnable.push_back(token);
            }
        }
    }

    fn queue(&mut self, token: Token) {
        if let Some(connection) = self.connections.get_mut(&token)
            && !connection.queued
        {
            connection.queued = true;
            self.runnable.push_back(token);
        }
    }

    /// Gives each runnable connection one turn. A turn that stopped short
    /// (`idle: false`) goes round again after the others: readiness is
    /// edge-triggered, so only a turn that read until the socket would block
    /// can wait for the next event.
    fn run_round(&mut self) {
        for _ in 0..self.runnable.len() {
            let Some(token) = self.runnable.pop_front() else {
                break;
            };
            let Some(connection) = self.connections.get_mut(&token) else {
                continue;
            };
            connection.queued = false;
            match socket_tick(
                &mut connection.socket,
                &connection.shared,
                &mut connection.pending,
            ) {
                SocketTick::Stop => self.release(token),
                SocketTick::Continue { idle: false } => self.queue(token),
                SocketTick::Continue { idle: true } => {}
            }
        }
    }

    /// Lets a finished connection go; its socket closes here.
    fn release(&mut self, token: Token) {
        if let Some(mut connection) = self.connections.remove(&token) {
            let _ = self.poll.registry().deregister(connection.socket.get_mut());
            // Nothing serves it any more, so later requests wake nobody.
            connection
                .shared
                .turn_requested
                .store(true, Ordering::SeqCst);
        }
    }

    /// Accepts until the listener would block, as admission allows.
    fn accept(&mut self) {
        if let Some(acceptor) = &mut self.acceptor {
            acceptor.retry_at = None;
        }
        loop {
            let Some(acceptor) = &mut self.acceptor else {
                return;
            };
            if !acceptor.context.accepting.load(Ordering::Acquire) {
                self.close_listener();
                return;
            }
            let Some(listener) = &acceptor.listener else {
                return;
            };
            let stream = match listener.accept() {
                Ok((stream, _remote)) => stream,
                Err(error) => match AcceptError::of(error.kind()) {
                    AcceptError::Drained => return,
                    AcceptError::AcceptNext => continue,
                    AcceptError::BackOff => {
                        acceptor.retry_at = Some(Instant::now() + ACCEPT_RETRY);
                        return;
                    }
                },
            };
            if !acceptor.rate.allow(Instant::now()) {
                continue;
            }
            let context = &acceptor.context;
            if context.occupancy.load(Ordering::Acquire) >= context.max_connections {
                continue;
            }
            let Some(conn) = acceptor.next_connection else {
                context.accepting.store(false, Ordering::Release);
                self.close_listener();
                return;
            };
            acceptor.next_connection = conn.checked_next();
            context.occupancy.fetch_add(1, Ordering::AcqRel);
            self.start_handshake(conn, stream);
        }
    }

    /// Accepts again once an accept error's back-off has passed.
    fn retry_accept(&mut self) {
        if self
            .acceptor
            .as_ref()
            .and_then(|acceptor| acceptor.retry_at)
            .is_some_and(|retry_at| Instant::now() >= retry_at)
        {
            self.accept();
        }
    }

    fn close_listener(&mut self) {
        if let Some(acceptor) = &mut self.acceptor
            && let Some(mut listener) = acceptor.listener.take()
        {
            acceptor.retry_at = None;
            let _ = self.poll.registry().deregister(&mut listener);
        }
    }

    /// Once admission stops, new connections are refused.
    fn close_listener_if_stopped(&mut self) {
        if self.acceptor.as_ref().is_some_and(|acceptor| {
            acceptor.listener.is_some() && !acceptor.context.accepting.load(Ordering::Acquire)
        }) {
            self.close_listener();
        }
    }

    fn start_handshake(&mut self, conn: ConnectionId, mut stream: TcpStream) {
        let token = self.next_token();
        if self
            .poll
            .registry()
            .register(&mut stream, token, Interest::READABLE | Interest::WRITABLE)
            .is_err()
        {
            self.release_slot();
            return;
        }
        let Some(acceptor) = &self.acceptor else {
            return;
        };
        let deadline = Instant::now() + acceptor.context.handshake_timeout;
        let admission = acceptor.context.admission.clone();
        let upgrade = accept_hdr_with_config(stream, admission, Some(websocket_config()));
        self.step_handshake(token, conn, deadline, upgrade);
    }

    fn resume_handshake(&mut self, token: Token) {
        if let Some(Handshake {
            conn,
            deadline,
            mid,
        }) = self.handshakes.remove(&token)
        {
            self.step_handshake(token, conn, deadline, mid.handshake());
        }
    }

    fn step_handshake(
        &mut self,
        token: Token,
        conn: ConnectionId,
        deadline: Instant,
        upgrade: ServerUpgrade,
    ) {
        match upgrade {
            Ok(socket) => self.admit(token, conn, deadline, socket),
            Err(HandshakeError::Interrupted(mid)) => {
                self.handshakes.insert(
                    token,
                    Handshake {
                        conn,
                        deadline,
                        mid,
                    },
                );
            }
            Err(HandshakeError::Failure(_)) => self.release_slot(),
        }
    }

    fn admit(
        &mut self,
        token: Token,
        conn: ConnectionId,
        deadline: Instant,
        mut socket: WebSocket<TcpStream>,
    ) {
        let Some(acceptor) = &self.acceptor else {
            return;
        };
        let context = &acceptor.context;
        if Instant::now() > deadline {
            context.occupancy.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        let shared = SharedPeer::new(
            context.magic,
            context.ping_interval_ms,
            context.timeout_ms,
            &context.reliable,
        );
        // Admission is decided under the registry lock so a peer can never be
        // inserted after `stop_admission` or `Drop` has swept the registry.
        let mut registry = lock(&context.registry);
        if !context.accepting.load(Ordering::Acquire) {
            drop(registry);
            context.occupancy.fetch_sub(1, Ordering::AcqRel);
            let _ = socket.close(None);
            return;
        }
        registry.insert(
            conn,
            ServerPeer {
                shared: Arc::clone(&shared),
                connected_announced: false,
                disconnected_announced: false,
            },
        );
        drop(registry);
        self.connections
            .insert(token, Connection::new(socket, shared));
        // Turn at once rather than wait for an edge. Tungstenite rejects
        // bytes that arrive with the request, so anything here reached the
        // socket after the upgrade's last read and its edge may already be
        // spent; on Windows, mio only starts polling a socket at the next
        // `Poll::poll`.
        self.queue(token);
    }

    /// Drops upgrades past their deadline and gives back their slots.
    fn expire_handshakes(&mut self) {
        if self.handshakes.is_empty() {
            return;
        }
        let now = Instant::now();
        let before = self.handshakes.len();
        self.handshakes
            .retain(|_, handshake| now < handshake.deadline);
        let expired = before - self.handshakes.len();
        if expired > 0
            && let Some(acceptor) = &self.acceptor
        {
            acceptor
                .context
                .occupancy
                .fetch_sub(expired, Ordering::AcqRel);
        }
    }

    /// Gives back the slot an unfinished upgrade held.
    fn release_slot(&self) {
        if let Some(acceptor) = &self.acceptor {
            acceptor.context.occupancy.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn next_token(&mut self) -> Token {
        let token = Token(self.next_token);
        self.next_token += 1;
        token
    }

    /// The owner is gone: a last turn writes a Close on every socket.
    fn finish(&mut self) {
        for connection in self.connections.values_mut() {
            connection.shared.shutdown.store(true, Ordering::Release);
            let _ = socket_tick(
                &mut connection.socket,
                &connection.shared,
                &mut connection.pending,
            );
        }
    }

    /// Ends every peer still here as `Transport`. A peer that already has a
    /// reason (`Local` from its owner's drop, say) keeps it, since a close
    /// keeps the first reason. Runs from `Drop`, possibly while unwinding, so
    /// it must not panic: `lock` tolerates poisoning, and a close only sets
    /// the reason and clears queues.
    fn fail(&self) {
        for connection in self.connections.values() {
            connection.shared.close(DisconnectReason::Transport);
        }
    }
}

impl Drop for IoWorker {
    /// However the worker ends — stopped by its owner, readiness failing, or
    /// a panic unwinding through it — no peer it served is left looking
    /// alive until a caller-clock timeout that may never come.
    fn drop(&mut self) {
        self.fail();
    }
}

/// What a failed `accept` means for accepting. No accept error closes the
/// listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcceptError {
    /// The backlog is empty; the next connection raises a readiness edge.
    Drained,
    /// A signal interrupted the call, or the connection it took had already
    /// been reset by the client: accept again at once.
    AcceptNext,
    /// Anything else, such as running out of file descriptors or socket
    /// buffers, a firewall refusal or a pending network error. Some fail
    /// before the connection leaves the backlog, so accepting again at once
    /// would spin: retry after [`ACCEPT_RETRY`].
    BackOff,
}

impl AcceptError {
    fn of(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::WouldBlock => Self::Drained,
            io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted => Self::AcceptNext,
            _ => Self::BackOff,
        }
    }
}

fn handshake_timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "WebSocket handshake timed out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_interrupted_or_reset_accepts_retry_at_once() {
        let cases = [
            (io::ErrorKind::WouldBlock, AcceptError::Drained),
            (io::ErrorKind::ConnectionAborted, AcceptError::AcceptNext),
            (io::ErrorKind::ConnectionReset, AcceptError::AcceptNext),
            (io::ErrorKind::Interrupted, AcceptError::AcceptNext),
            (io::ErrorKind::OutOfMemory, AcceptError::BackOff),
            (io::ErrorKind::PermissionDenied, AcceptError::BackOff),
            (io::ErrorKind::NetworkDown, AcceptError::BackOff),
            (io::ErrorKind::HostUnreachable, AcceptError::BackOff),
            (io::ErrorKind::InvalidInput, AcceptError::BackOff),
        ];
        for (kind, expected) in cases {
            assert_eq!(AcceptError::of(kind), expected, "{kind:?}");
        }
    }
}
