//! Browser [`ClientIo`] backed by `web_sys::WebSocket`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use super::queue::PeerState;
use super::{
    MAX_BROWSER_BUFFERED_BYTES, MAX_BROWSER_RECONNECT_ATTEMPTS, MAX_BROWSER_RECONNECT_DELAY_MS,
    MAX_WEBSOCKET_FRAME_BYTES, ReconnectPolicy, ReconnectState, WebSocketIdentity, decode_envelope,
    encode_envelope,
};
use crate::{
    ClientEvent, ClientIo, Delivery, DisconnectReason, Lane, ReliableCapacity, ReliableConfig,
    RttEstimate, SendError,
};

fn copy_bounded_binary(buffer: &ArrayBuffer) -> Option<Vec<u8>> {
    (buffer.byte_length() as usize <= MAX_WEBSOCKET_FRAME_BYTES)
        .then(|| Uint8Array::new(buffer).to_vec())
}

/// Browser WebSocket client configuration.
#[derive(Clone, Debug)]
pub struct BrowserWebSocketConfig {
    /// WebSocket endpoint URL.
    pub url: String,
    /// Framing and handshake identity supplied by the game.
    pub identity: WebSocketIdentity,
    /// Poll-driven bounded reconnect policy.
    pub reconnect: ReconnectPolicy,
    /// Browser buffered-byte watermark that paces reliable and unreliable
    /// traffic: a released frame waits while it would take `bufferedAmount`
    /// past this.
    /// At least [`MAX_WEBSOCKET_FRAME_BYTES`] so any frame can go out.
    pub reliable_buffered_bytes: usize,
    /// Browser buffered-byte watermark above which latest state stays coalesced.
    pub latest_buffered_bytes: usize,
    /// The connection's reliable message cap, lane weights and per-lane
    /// bounds. The browser cannot stop reading a socket, so a lane's
    /// `inbound_messages` and `inbound_bytes` must hold what can arrive
    /// between two polls; a peer that sends more is disconnected with
    /// `InboundOverflow`.
    pub reliable: ReliableConfig,
}

impl BrowserWebSocketConfig {
    /// Creates a browser client configuration with bounded defaults.
    #[must_use]
    pub fn new(url: impl Into<String>, identity: WebSocketIdentity) -> Self {
        Self {
            url: url.into(),
            identity,
            reconnect: ReconnectPolicy::default(),
            reliable_buffered_bytes: MAX_BROWSER_BUFFERED_BYTES,
            latest_buffered_bytes: 64 * 1024,
            reliable: ReliableConfig::DEFAULT,
        }
    }

    fn validate(&self) -> Result<(), wasm_bindgen::JsValue> {
        let reconnect = self.reconnect;
        if self.reliable_buffered_bytes < MAX_WEBSOCKET_FRAME_BYTES
            || self.reliable_buffered_bytes > MAX_BROWSER_BUFFERED_BYTES
            || self.latest_buffered_bytes == 0
            || self.latest_buffered_bytes > MAX_BROWSER_BUFFERED_BYTES
            || reconnect.max_attempts == 0
            || reconnect.max_attempts > MAX_BROWSER_RECONNECT_ATTEMPTS
            || reconnect.initial_delay_ms == 0
            || reconnect.initial_delay_ms > MAX_BROWSER_RECONNECT_DELAY_MS
            || reconnect.max_delay_ms < reconnect.initial_delay_ms
            || reconnect.max_delay_ms > MAX_BROWSER_RECONNECT_DELAY_MS
            || self.reliable.validate().is_err()
        {
            return Err(
                js_sys::Error::new("invalid bounded browser WebSocket configuration").into(),
            );
        }
        Ok(())
    }
}

struct BrowserState {
    peer: PeerState,
    opened: bool,
    connected_pending: bool,
    needs_reconnect: bool,
    local_close: bool,
}

impl BrowserState {
    fn new(reliable: &ReliableConfig) -> Self {
        Self {
            peer: PeerState::new(reliable),
            opened: false,
            connected_pending: false,
            needs_reconnect: false,
            local_close: false,
        }
    }

    fn fail(&mut self, reason: DisconnectReason) {
        self.peer.close(reason);
        if !self.local_close {
            self.needs_reconnect = true;
        }
    }

    fn reset_for_reconnect(&mut self, reliable: &ReliableConfig) {
        self.peer = PeerState::new(reliable);
        self.opened = false;
        self.connected_pending = false;
        self.needs_reconnect = false;
        self.local_close = false;
    }
}

struct BrowserSocket {
    socket: WebSocket,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
}

impl BrowserSocket {
    fn connect(
        url: &str,
        identity: &WebSocketIdentity,
        state: &Rc<RefCell<BrowserState>>,
    ) -> Result<Self, wasm_bindgen::JsValue> {
        let socket = WebSocket::new_with_str(url, &identity.subprotocol)?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let magic = identity.magic;

        let open_state = Rc::clone(state);
        let on_open = Closure::wrap(Box::new(move |_event: Event| {
            let mut state = open_state.borrow_mut();
            if state.peer.terminal().is_none() {
                state.opened = true;
                state.connected_pending = true;
            }
        }) as Box<dyn FnMut(_)>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let message_state = Rc::clone(state);
        let message_socket = socket.clone();
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
                message_state
                    .borrow_mut()
                    .fail(DisconnectReason::ProtocolViolation);
                let _ = message_socket.close();
                return;
            };
            let Some(bytes) = copy_bounded_binary(&buffer) else {
                message_state
                    .borrow_mut()
                    .fail(DisconnectReason::ProtocolViolation);
                let _ = message_socket.close();
                return;
            };
            let Ok(envelope) = decode_envelope(magic, &bytes) else {
                message_state
                    .borrow_mut()
                    .fail(DisconnectReason::ProtocolViolation);
                let _ = message_socket.close();
                return;
            };
            let result = message_state.borrow_mut().peer.receive(envelope);
            if result.is_err() {
                let _ = message_socket.close();
            }
        }) as Box<dyn FnMut(_)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let close_state = Rc::clone(state);
        let on_close = Closure::wrap(Box::new(move |_event: CloseEvent| {
            let mut state = close_state.borrow_mut();
            if state.peer.terminal().is_none() {
                state.fail(DisconnectReason::Peer);
            }
        }) as Box<dyn FnMut(_)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        let error_state = Rc::clone(state);
        let error_socket = socket.clone();
        let on_error = Closure::wrap(Box::new(move |_event: Event| {
            let mut state = error_state.borrow_mut();
            if state.peer.terminal().is_none() {
                state.fail(DisconnectReason::Transport);
            }
            let _ = error_socket.close();
        }) as Box<dyn FnMut(_)>);
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        Ok(Self {
            socket,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
            _on_error: on_error,
        })
    }

    fn detach_and_close(&self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        if matches!(
            self.socket.ready_state(),
            WebSocket::CONNECTING | WebSocket::OPEN
        ) {
            let _ = self.socket.close();
        }
    }
}

impl Drop for BrowserSocket {
    fn drop(&mut self) {
        self.detach_and_close();
    }
}

/// Poll-driven browser WebSocket client with bounded reconnects.
pub struct BrowserWebSocketClient {
    config: BrowserWebSocketConfig,
    state: Rc<RefCell<BrowserState>>,
    socket: BrowserSocket,
    reconnect: ReconnectState,
    terminal_announced: bool,
    ever_connected: bool,
}

impl BrowserWebSocketClient {
    /// Starts the initial browser WebSocket connection.
    pub fn connect(config: BrowserWebSocketConfig) -> Result<Self, wasm_bindgen::JsValue> {
        config.validate()?;
        let state = Rc::new(RefCell::new(BrowserState::new(&config.reliable)));
        let socket = BrowserSocket::connect(&config.url, &config.identity, &state)?;
        Ok(Self {
            config,
            state,
            socket,
            reconnect: ReconnectState::new(),
            terminal_announced: false,
            ever_connected: false,
        })
    }

    fn attempt_reconnect(&mut self) -> bool {
        self.socket.detach_and_close();
        self.state
            .borrow_mut()
            .reset_for_reconnect(&self.config.reliable);
        match BrowserSocket::connect(&self.config.url, &self.config.identity, &self.state) {
            Ok(socket) => {
                self.socket = socket;
                self.terminal_announced = false;
                true
            }
            Err(_) => {
                self.state.borrow_mut().fail(DisconnectReason::Transport);
                false
            }
        }
    }

    fn close_protocol(&mut self) {
        self.state
            .borrow_mut()
            .fail(DisconnectReason::ProtocolViolation);
        self.socket.detach_and_close();
    }

    fn flush_released(&mut self) {
        if self.socket.socket.ready_state() != WebSocket::OPEN {
            return;
        }
        loop {
            let buffered = self.socket.socket.buffered_amount() as usize;
            let next = self.state.borrow().peer.next_released_frame_len();
            let Some((delivery, frame_bytes)) = next else {
                break;
            };
            // Unreliable frames wait for the reliable watermark like reliable
            // ones: an accepted message is never dropped.
            let limit = match delivery {
                Delivery::Reliable(_) | Delivery::Unreliable(_) => {
                    self.config.reliable_buffered_bytes
                }
                Delivery::LatestState => self.config.latest_buffered_bytes,
            };
            // Paced, never fatal: the frame waits for the browser to drain.
            if buffered.saturating_add(frame_bytes) > limit {
                break;
            }
            let next_frame = self
                .state
                .borrow_mut()
                .peer
                .pop_released_frame()
                .expect("released outbound frame was observed above");
            let Ok(encoded) = encode_envelope(self.config.identity.magic, &next_frame.envelope())
            else {
                self.close_protocol();
                break;
            };
            if self.socket.socket.send_with_u8_array(&encoded).is_err() {
                self.state.borrow_mut().fail(DisconnectReason::Transport);
                self.socket.detach_and_close();
                break;
            }
        }
        let graceful_complete = {
            let mut state = self.state.borrow_mut();
            state.peer.finish_graceful_close();
            state.peer.terminal() == Some(DisconnectReason::Local)
        };
        if graceful_complete {
            self.socket.detach_and_close();
        }
    }
}

impl ClientIo for BrowserWebSocketClient {
    fn poll(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        let graceful_expired = self.state.borrow_mut().peer.expire_graceful_close(now_ms);
        if graceful_expired {
            self.socket.detach_and_close();
        }
        {
            let mut state = self.state.borrow_mut();
            if std::mem::take(&mut state.connected_pending) {
                self.ever_connected = true;
                self.terminal_announced = false;
                self.reconnect.reset();
                events.push(ClientEvent::Connected);
            }
            while let Some((delivery, payload)) = state.peer.pop_inbound() {
                events.push(ClientEvent::Message { delivery, payload });
            }
            if let Some(reason) = state.peer.terminal()
                && !self.terminal_announced
            {
                self.terminal_announced = true;
                events.push(ClientEvent::Disconnected { reason });
            }
            if state.needs_reconnect {
                self.reconnect.schedule(now_ms, self.config.reconnect);
            }
        }

        if let Some(attempt) = self.reconnect.take_due(now_ms, self.config.reconnect) {
            events.push(ClientEvent::Reconnecting { attempt });
            if !self.attempt_reconnect() {
                self.reconnect.schedule(now_ms, self.config.reconnect);
            }
        }
        // Released frames paced by `bufferedAmount` go out as it drains.
        self.flush_released();
        events
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        let (result, graceful_closing) = {
            let mut state = self.state.borrow_mut();
            let result = state.peer.send(delivery, payload);
            (result, state.peer.graceful_closing())
        };
        // A send that ended the peer closes the socket; any other refusal
        // changed nothing.
        if result == Err(SendError::Disconnected) && !graceful_closing {
            self.socket.detach_and_close();
        }
        result
    }

    fn capacity(&self, lane: Lane) -> ReliableCapacity {
        self.state.borrow().peer.capacity(lane)
    }

    fn flush(&mut self, now_ms: u64) {
        if self.state.borrow_mut().peer.expire_graceful_close(now_ms) {
            self.socket.detach_and_close();
            return;
        }
        let _ = self.state.borrow_mut().peer.release_outbound();
        self.flush_released();
    }

    fn disconnect(&mut self, _now_ms: u64) {
        if self.socket.socket.ready_state() != WebSocket::OPEN {
            {
                let mut state = self.state.borrow_mut();
                state.local_close = true;
                state.needs_reconnect = false;
                state.peer.close(DisconnectReason::Local);
            }
            self.socket.detach_and_close();
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.local_close = true;
            let _ = state.peer.release_outbound();
            state.peer.begin_graceful_close(_now_ms);
            state.needs_reconnect = false;
        }
        self.flush_released();
    }

    // Browser WebSockets expose no ping API, so round trips cannot be
    // measured; the all-zero estimate means "unmeasured" per the trait.
    fn rtt(&self) -> RttEstimate {
        RttEstimate::default()
    }
}
