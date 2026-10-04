//! Binary WebSocket transports for native servers, native clients, and browsers.
//!
//! Every application message uses one 17-byte envelope: caller-supplied magic
//! (3 bytes), version (1), delivery class (1), big-endian sequence (8),
//! big-endian payload length (4), then the opaque payload. Reliable frames use
//! sequence zero. Latest-state frames use a nonzero, strictly increasing
//! sequence per connection. There is no text mode or version negotiation.

#[cfg(target_arch = "wasm32")]
mod browser;
mod codec;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod native_write;
mod queue;

#[cfg(target_arch = "wasm32")]
pub use browser::{BrowserWebSocketClient, BrowserWebSocketConfig};
pub use codec::{
    ENVELOPE_HEADER_LEN, ENVELOPE_VERSION, Envelope, EnvelopeError, decode_envelope,
    encode_envelope,
};
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    MAX_NATIVE_WEBSOCKET_ACCEPTS_PER_SECOND, MAX_NATIVE_WEBSOCKET_CONNECTIONS,
    MAX_NATIVE_WEBSOCKET_EVENTS_PER_POLL, MAX_WEBSOCKET_HANDSHAKE_TIMEOUT,
    MAX_WEBSOCKET_LIVENESS_MS, NativeWebSocketClient, NativeWebSocketClientConfig,
    NativeWebSocketServer, NativeWebSocketServerConfig, OriginPolicy,
};

/// Shared default HTTP path used by the source games.
pub const GAME_PATH: &str = "/game/ws";

/// Caller-supplied WebSocket framing and handshake identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSocketIdentity {
    /// Three-byte envelope magic.
    pub magic: [u8; 3],
    /// HTTP path the native listener accepts.
    pub path: String,
    /// RFC 6455 subprotocol.
    pub subprotocol: String,
}

impl WebSocketIdentity {
    /// Constructs an identity. Path and subprotocol must be non-empty.
    #[must_use]
    pub fn new(magic: [u8; 3], path: impl Into<String>, subprotocol: impl Into<String>) -> Self {
        Self {
            magic,
            path: path.into(),
            subprotocol: subprotocol.into(),
        }
    }
}

/// Default maximum WebSocket message length, including its envelope.
pub const MAX_WEBSOCKET_FRAME_BYTES: usize =
    ENVELOPE_HEADER_LEN + crate::MAX_RELIABLE_MESSAGE_BYTES;
/// Hard ceiling for bytes retained by the browser's platform send buffer.
pub const MAX_BROWSER_BUFFERED_BYTES: usize = crate::RELIABLE_OUTBOUND_BYTES;
/// Hard ceiling for caller-configured browser reconnect attempts.
pub const MAX_BROWSER_RECONNECT_ATTEMPTS: u16 = 64;
/// Hard ceiling for one caller-clock browser reconnect delay.
pub const MAX_BROWSER_RECONNECT_DELAY_MS: u64 = 60_000;
/// Caller-clock deadline for draining accepted reliable frames during disconnect.
pub const GRACEFUL_CLOSE_TIMEOUT_MS: u64 = 1_000;

/// Poll-driven reconnect policy used by the browser transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Maximum reconnect attempts after a connection is lost.
    pub max_attempts: u16,
    /// Delay before the first reconnect attempt.
    pub initial_delay_ms: u64,
    /// Maximum delay between attempts.
    pub max_delay_ms: u64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            initial_delay_ms: 250,
            max_delay_ms: 4_000,
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReconnectState {
    attempt: u16,
    due_ms: Option<u64>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl ReconnectState {
    const fn new() -> Self {
        Self {
            attempt: 0,
            due_ms: None,
        }
    }

    fn schedule(&mut self, now_ms: u64, policy: ReconnectPolicy) {
        if self.attempt >= policy.max_attempts || self.due_ms.is_some() {
            return;
        }
        let shift = u32::from(self.attempt.min(63));
        let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
        let delay = policy
            .initial_delay_ms
            .saturating_mul(multiplier)
            .min(policy.max_delay_ms);
        self.due_ms = Some(now_ms.saturating_add(delay));
    }

    fn take_due(&mut self, now_ms: u64, policy: ReconnectPolicy) -> Option<u16> {
        if self.attempt >= policy.max_attempts || self.due_ms.is_none_or(|due| now_ms < due) {
            return None;
        }
        self.due_ms = None;
        self.attempt += 1;
        Some(self.attempt)
    }

    #[cfg(target_arch = "wasm32")]
    fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn reconnect_backoff_is_poll_driven_bounded_and_saturating() {
        let policy = ReconnectPolicy {
            max_attempts: 4,
            initial_delay_ms: 10,
            max_delay_ms: 25,
        };
        let mut state = ReconnectState::new();

        state.schedule(100, policy);
        assert_eq!(state.take_due(109, policy), None);
        assert_eq!(state.take_due(110, policy), Some(1));
        state.schedule(110, policy);
        assert_eq!(state.take_due(129, policy), None);
        assert_eq!(state.take_due(130, policy), Some(2));
        state.schedule(130, policy);
        assert_eq!(state.take_due(155, policy), Some(3));
        state.schedule(u64::MAX - 1, policy);
        assert_eq!(state.take_due(u64::MAX - 1, policy), None);
        assert_eq!(state.take_due(u64::MAX, policy), Some(4));
        state.schedule(u64::MAX, policy);
        assert_eq!(state.take_due(u64::MAX, policy), None);
    }
}
