//! Clockless, opaque-payload networking primitives.
//!
//! Transports carry only bytes and a [`Delivery`] class. Every operation that
//! needs time receives `now_ms` from its caller; implementations do not read a
//! system or browser clock and never interpret game messages.
//!
//! A full reliable lane refuses a send with [`SendError::WouldBlock`]: nothing
//! was queued, the connection is unaffected, and the caller keeps the message
//! to retry after a later `flush` and `poll`. Ignoring the error loses the
//! message.
//!
//! ```
//! use std::collections::VecDeque;
//! use sgl_net::{ClientIo, Delivery, SendError, ServerEvent, ServerIo, memory_duplex};
//!
//! let (mut client, mut server) = memory_duplex();
//! let sent: Vec<Vec<u8>> = (0u32..300).map(|i| i.to_le_bytes().to_vec()).collect();
//! let mut pending: VecDeque<Vec<u8>> = sent.iter().cloned().collect();
//! let mut received = Vec::new();
//! for now_ms in 0..10 {
//!     while let Some(message) = pending.front() {
//!         match client.send(Delivery::RELIABLE_ORDERED, message) {
//!             Ok(()) => drop(pending.pop_front()),
//!             // Keep the message and retry on a later tick.
//!             Err(SendError::WouldBlock) => break,
//!             Err(error) => panic!("connection lost: {error}"),
//!         }
//!     }
//!     client.flush(now_ms);
//!     for event in server.poll(now_ms) {
//!         if let ServerEvent::Message { payload, .. } = event {
//!             received.push(payload);
//!         }
//!     }
//! }
//! assert_eq!(received, sent);
//! ```
//!
//! There are three delivery classes: [`Delivery::Reliable`] and
//! [`Delivery::Unreliable`] on one of [`RELIABLE_LANES`] lanes, and
//! [`Delivery::LatestState`]. Reliable messages are exact and in order
//! within their lane; unreliable ones are never retransmitted and unordered,
//! delivered at most once. The sender never drops an accepted message of any
//! class while the connection lives; the network may lose an unreliable one,
//! and a receiver that is not polled drops its oldest unpolled unreliable
//! messages, as a full UDP socket buffer does (reliable overflow closes the
//! peer only on a browser WebSocket receiver or past
//! [`udp::EndpointConfig::global_reliable_inbound_bytes`]).
//! Each lane has its own bounds and a scheduling weight in
//! [`ReliableConfig`], which also sets the largest reliable message
//! (`max_message_bytes`, default 64 KiB, up to 16 MiB). Transports fragment
//! and reassemble long messages, and a lane holding nothing admits one
//! message of any size up to the cap, however small its byte allowance.
//! Busy lanes share the connection by weight, so a bulk transfer on one
//! lane neither refuses nor starves messages on another:
//!
//! ```
//! use sgl_net::{ClientIo, Delivery, Lane, ReliableConfig, memory_duplex_with};
//!
//! let bulk = Lane::new(1).expect("lane 1 exists");
//! let mut reliable = ReliableConfig::default();
//! reliable.max_message_bytes = 4 << 20;
//! reliable.lanes[Lane::DEFAULT.index()].weight = 8;
//! reliable.lanes[bulk.index()].weight = 1;
//! // Both ends of a connection use the same configuration.
//! let (mut client, _server) = memory_duplex_with(&reliable)?;
//! client.send(Delivery::Reliable(bulk), &vec![0; 4 << 20])?;
//! client.send(Delivery::RELIABLE_ORDERED, b"input")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod lanes;
pub mod memory;
pub mod mux;
pub mod udp;
pub mod websocket;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod proptest_support;

pub use lanes::{
    DEFAULT_LANE_INBOUND_BYTES, DEFAULT_LANE_INBOUND_MESSAGES, DEFAULT_LANE_OUTBOUND_BYTES,
    DEFAULT_LANE_OUTBOUND_MESSAGES, DEFAULT_LANE_UNRELIABLE_BYTES,
    DEFAULT_LANE_UNRELIABLE_MESSAGES, DEFAULT_RELIABLE_MESSAGE_BYTES, LANE_QUEUE_BYTES_LIMIT,
    LANE_QUEUE_MESSAGES_LIMIT, LaneConfig, MAX_LANE_WEIGHT, RELIABLE_MESSAGE_BYTES_LIMIT,
    ReliableConfig, ReliableConfigError,
};
pub use memory::{
    MemoryClientIo, MemoryServerIo, SOLO_CONNECTION, memory_duplex, memory_duplex_with,
};
pub use mux::{MAX_MUX_ORPHAN_BYTES, MAX_MUX_ORPHAN_MESSAGES, ServerIoMux};

use std::num::NonZeroU64;

/// Maximum bytes in one latest-state payload across every transport.
///
/// This is exactly the payload space left inside a 1,200-byte UDP datagram
/// after its nonce-bearing envelope, one lane's acknowledgement state, and
/// the item header. WebSocket and memory transports use the same cap so a
/// payload accepted by one transport remains portable to every other
/// transport.
pub const MAX_LATEST_STATE_BYTES: usize = udp::MAX_LATEST_PAYLOAD_BYTES;
/// Maximum bytes in one unreliable payload across every transport: one UDP
/// datagram item beside one lane's acknowledgement, the same room as latest
/// state. Unreliable messages are never fragmented.
pub const MAX_UNRELIABLE_BYTES: usize = udp::MAX_LATEST_PAYLOAD_BYTES;

/// A process-local connection identity.
///
/// A transport mux allocates these values. They identify a connection only
/// inside one server process and must never be encoded into a wire frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(NonZeroU64);

impl ConnectionId {
    /// The first allocatable process-local connection identity.
    pub const MIN: Self = Self(NonZeroU64::MIN);

    /// Constructs a process-local connection identity, rejecting reserved zero.
    #[must_use]
    pub const fn from_raw(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the process-local numeric value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0.get()
    }

    /// Returns the next process-local identity, or `None` at exhaustion.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.raw().checked_add(1) {
            Some(value) => Self::from_raw(value),
            None => None,
        }
    }
}

/// Number of independent reliable ordered lanes every transport provides.
pub const RELIABLE_LANES: usize = 4;

/// One of [`RELIABLE_LANES`] reliable ordered lanes. Lanes are numbered; what
/// each lane carries is the game's choice. Each lane is exact and in order on
/// its own; there is no order across lanes. Each has its own bounds and
/// scheduling weight in [`ReliableConfig`], so a saturated lane never
/// refuses or delays another lane's messages beyond its weighted share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lane(u8);

impl Lane {
    /// Lane 0, the lane of [`Delivery::RELIABLE_ORDERED`].
    pub const DEFAULT: Self = Self(0);

    /// Returns lane `index`, or `None` when `index >= RELIABLE_LANES`.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if (index as usize) < RELIABLE_LANES {
            Some(Self(index))
        } else {
            None
        }
    }

    /// Returns this lane's zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The delivery classes supported by every transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Delivery {
    /// Exact, in order and unduplicated within its [`Lane`]; unordered
    /// across lanes. A full lane refuses the send with
    /// [`SendError::WouldBlock`].
    Reliable(Lane),
    /// Independent best-effort messages on a [`Lane`]: each is delivered at
    /// most once or not at all, never retransmitted or fragmented (at most
    /// [`MAX_UNRELIABLE_BYTES`]), in no promised order. It shares its lane's
    /// scheduling share with that lane's reliable messages; a full queue
    /// refuses the send with [`SendError::WouldBlock`].
    Unreliable(Lane),
    /// A replaceable best-effort slot where only the newest state is useful.
    LatestState,
}

impl Delivery {
    /// Reliable delivery on [`Lane::DEFAULT`]. Usable in patterns.
    pub const RELIABLE_ORDERED: Self = Self::Reliable(Lane::DEFAULT);
}

/// Why a transport could not queue a payload.
///
/// Every error leaves the transport unchanged: nothing was queued and the
/// caller keeps the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// The server-side process-local connection is unknown or disconnected.
    UnknownConnection,
    /// The client-side peer is disconnected.
    Disconnected,
    /// The payload exceeds its delivery class's cap: the transport's
    /// [`ReliableConfig::max_message_bytes`], [`MAX_UNRELIABLE_BYTES`] or
    /// [`MAX_LATEST_STATE_BYTES`].
    PayloadTooLarge,
    /// The lane, or a shared ceiling, cannot take this message now. The
    /// connection is unaffected; retry after a later `flush` and `poll` have
    /// drained capacity (`capacity` reports it).
    WouldBlock,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnknownConnection => "unknown or disconnected connection",
            Self::Disconnected => "peer is disconnected",
            Self::PayloadTooLarge => "payload exceeds the delivery-class limit",
            Self::WouldBlock => "reliable lane is full; retry later",
        })
    }
}

impl std::error::Error for SendError {}

/// Why a transport refused admission before protocol traffic was available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// The transport's connection capacity is exhausted.
    ServerFull,
    /// The server has stopped admitting new peers.
    NotAdmitting,
    /// A native server rejected the browser Origin.
    OriginRejected,
    /// The transport refused malformed or unsupported setup data.
    Unsupported,
}

/// Why a connection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
    /// This side ended the connection.
    Local,
    /// The peer ended or dropped the connection.
    Peer,
    /// The peer stopped responding within its transport timeout.
    TimedOut,
    /// Incoming transport data violated its framing contract.
    ProtocolViolation,
    /// The peer exceeded an inbound reliable bound this side cannot slow it
    /// for: a browser receiver's lane bounds, or the UDP endpoint's
    /// `global_reliable_inbound_bytes`.
    InboundOverflow,
    /// The underlying socket or browser transport failed.
    Transport,
}

/// What one reliable lane admits right now.
///
/// Advisory: it holds until the next operation on the transport, and `send`
/// remains the only atomic admission. A payload of `len` bytes is admitted
/// when `messages >= 1 && bytes >= len`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReliableCapacity {
    /// Messages `send` would admit on this lane.
    pub messages: usize,
    /// Largest payload `send` would admit now: up to
    /// [`ReliableConfig::max_message_bytes`] while the lane holds nothing;
    /// zero when `messages` is zero.
    pub bytes: usize,
}

impl ReliableCapacity {
    /// A lane's capacity from its remaining message and byte allowances.
    pub(crate) fn remaining(messages: usize, bytes: usize) -> Self {
        if messages == 0 {
            return Self::default();
        }
        Self { messages, bytes }
    }
}

/// One event observed by a client transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientEvent {
    /// The transport became connected.
    Connected,
    /// The transport lost its connection and is attempting to restore it.
    Reconnecting {
        /// One-based bounded reconnect attempt number.
        attempt: u16,
    },
    /// An opaque payload arrived.
    Message {
        /// The payload's delivery class.
        delivery: Delivery,
        /// The opaque payload bytes.
        payload: Vec<u8>,
    },
    /// The remote transport refused admission.
    Denied {
        /// Stable transport-level refusal reason.
        reason: DenyReason,
    },
    /// The transport disconnected.
    Disconnected {
        /// The transport-level reason.
        reason: DisconnectReason,
    },
}

/// One event observed by a server transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerEvent {
    /// A connection became available.
    Connected {
        /// The process-local connection identity.
        conn: ConnectionId,
    },
    /// An opaque payload arrived.
    Message {
        /// The process-local connection identity.
        conn: ConnectionId,
        /// The payload's delivery class.
        delivery: Delivery,
        /// The opaque payload bytes.
        payload: Vec<u8>,
    },
    /// A connection ended.
    Disconnected {
        /// The process-local connection identity.
        conn: ConnectionId,
        /// The transport-level reason.
        reason: DisconnectReason,
    },
}

/// Object-safe client transport boundary.
pub trait ClientIo {
    /// Returns events available at the caller-provided virtual time.
    fn poll(&mut self, now_ms: u64) -> Vec<ClientEvent>;

    /// Queues an opaque payload for the next [`Self::flush`], or refuses it
    /// whole without affecting the connection.
    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError>;

    /// Reports what `lane` admits now; all zeros once disconnected.
    fn capacity(&self, lane: Lane) -> ReliableCapacity;

    /// Advances outbound transport work at the caller-provided virtual time.
    fn flush(&mut self, now_ms: u64);

    /// Flushes pending work and ends the connection at `now_ms`.
    fn disconnect(&mut self, now_ms: u64);

    /// Returns this transport's shared RTT estimate.
    ///
    /// A transport that cannot measure round trips (the browser WebSocket has
    /// no ping API) returns the all-zero [`RttEstimate::default`], which is
    /// indistinguishable from an unmeasured connection.
    fn rtt(&self) -> RttEstimate;
}

impl<T: ClientIo + ?Sized> ClientIo for Box<T> {
    fn poll(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        (**self).poll(now_ms)
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        (**self).send(delivery, payload)
    }

    fn capacity(&self, lane: Lane) -> ReliableCapacity {
        (**self).capacity(lane)
    }

    fn flush(&mut self, now_ms: u64) {
        (**self).flush(now_ms);
    }

    fn disconnect(&mut self, now_ms: u64) {
        (**self).disconnect(now_ms);
    }

    fn rtt(&self) -> RttEstimate {
        (**self).rtt()
    }
}

/// Object-safe server transport boundary.
pub trait ServerIo {
    /// Returns events available at the caller-provided virtual time.
    fn poll(&mut self, now_ms: u64) -> Vec<ServerEvent>;

    /// Queues an opaque payload for one process-local connection, or refuses
    /// it whole without affecting the connection.
    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError>;

    /// Reports what `lane` of `conn` admits now; all zeros for an unknown or
    /// disconnected connection.
    fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity;

    /// Advances outbound transport work at the caller-provided virtual time.
    fn flush(&mut self, now_ms: u64);

    /// Flushes and ends one process-local connection at `now_ms`.
    fn disconnect(&mut self, conn: ConnectionId, now_ms: u64);

    /// Stops accepting new connections without affecting existing ones.
    fn stop_admission(&mut self);
}

impl<T: ServerIo + ?Sized> ServerIo for Box<T> {
    fn poll(&mut self, now_ms: u64) -> Vec<ServerEvent> {
        (**self).poll(now_ms)
    }

    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError> {
        (**self).send(conn, delivery, payload)
    }

    fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
        (**self).capacity(conn, lane)
    }

    fn flush(&mut self, now_ms: u64) {
        (**self).flush(now_ms);
    }

    fn disconnect(&mut self, conn: ConnectionId, now_ms: u64) {
        (**self).disconnect(conn, now_ms);
    }

    fn stop_admission(&mut self) {
        (**self).stop_admission();
    }
}

/// RFC 6298 round-trip estimate in integer milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RttEstimate {
    /// Smoothed round-trip time.
    pub srtt_ms: u32,
    /// Smoothed mean deviation from `srtt_ms`.
    pub rttvar_ms: u32,
    /// Smallest measured sample.
    pub min_ms: u32,
}

/// Shared integer RFC 6298 estimator used by every transport.
///
/// The initial sample seeds `srtt = R`, `rttvar = R / 2`. Later samples use
/// alpha 1/8 and beta 1/4. Intermediate arithmetic is widened so even a
/// `u32::MAX` sample is defined identically in debug and release builds.
#[derive(Clone, Copy, Debug, Default)]
pub struct RttEstimator {
    estimate: RttEstimate,
    seeded: bool,
}

impl RttEstimator {
    /// Constructs an estimator with no samples.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            estimate: RttEstimate {
                srtt_ms: 0,
                rttvar_ms: 0,
                min_ms: 0,
            },
            seeded: false,
        }
    }

    /// Folds in one measured round trip.
    pub fn sample(&mut self, rtt_ms: u32) {
        if !self.seeded {
            self.seeded = true;
            self.estimate = RttEstimate {
                srtt_ms: rtt_ms,
                rttvar_ms: rtt_ms / 2,
                min_ms: rtt_ms,
            };
            return;
        }

        let srtt = u64::from(self.estimate.srtt_ms);
        let deviation = u64::from(self.estimate.srtt_ms.abs_diff(rtt_ms));
        let variation = (3 * u64::from(self.estimate.rttvar_ms) + deviation) / 4;
        let smoothed = (7 * srtt + u64::from(rtt_ms)) / 8;
        self.estimate.rttvar_ms = u32::try_from(variation).unwrap_or(u32::MAX);
        self.estimate.srtt_ms = u32::try_from(smoothed).unwrap_or(u32::MAX);
        self.estimate.min_ms = self.estimate.min_ms.min(rtt_ms);
    }

    /// Returns the current estimate, or all zeroes before the first sample.
    #[must_use]
    pub const fn estimate(&self) -> RttEstimate {
        self.estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn accepts_client_object(_: &mut dyn ClientIo) {}
    fn accepts_server_object(_: &mut dyn ServerIo) {}

    #[wasm_bindgen_test(unsupported = test)]
    fn transport_traits_are_object_safe() {
        let (mut client, mut server) = memory_duplex();
        accepts_client_object(&mut client);
        accepts_server_object(&mut server);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn connection_ids_reserve_zero_and_exhaustion_is_checked() {
        assert_eq!(ConnectionId::from_raw(0), None);
        assert_eq!(ConnectionId::MIN.raw(), 1);
        assert_eq!(
            ConnectionId::MIN.checked_next().map(ConnectionId::raw),
            Some(2)
        );
        let maximum = ConnectionId::from_raw(u64::MAX).expect("nonzero maximum");
        assert_eq!(maximum.checked_next(), None);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn rtt_estimator_measures_zero_and_converges() {
        let mut zero = RttEstimator::new();
        zero.sample(0);
        assert_eq!(zero.estimate(), RttEstimate::default());

        let mut steady = RttEstimator::new();
        for _ in 0..200 {
            steady.sample(150);
        }
        assert_eq!(
            steady.estimate(),
            RttEstimate {
                srtt_ms: 150,
                rttvar_ms: 0,
                min_ms: 150,
            }
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn rtt_estimator_uses_the_rfc_6298_integer_coefficients() {
        let mut estimator = RttEstimator::new();
        estimator.sample(100);
        assert_eq!(
            estimator.estimate(),
            RttEstimate {
                srtt_ms: 100,
                rttvar_ms: 50,
                min_ms: 100,
            }
        );
        estimator.sample(120);
        assert_eq!(
            estimator.estimate(),
            RttEstimate {
                srtt_ms: 102,
                rttvar_ms: 42,
                min_ms: 100,
            }
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn rtt_estimator_tracks_jitter_and_extreme_samples_without_overflow() {
        let mut jittery = RttEstimator::new();
        for index in 0..200 {
            jittery.sample(if index % 2 == 0 { 120 } else { 180 });
        }
        assert!(jittery.estimate().rttvar_ms > 0);
        assert_eq!(jittery.estimate().min_ms, 120);

        let mut extreme = RttEstimator::new();
        extreme.sample(u32::MAX);
        extreme.sample(0);
        extreme.sample(u32::MAX);
        assert_eq!(extreme.estimate().min_ms, 0);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::RttEstimator;
    use crate::proptest_support::check;
    use proptest::prelude::*;

    /// Defect: a coefficient regression in the integer estimator (alpha,
    /// beta, or the seed rule) that skews every transport's RTT. Oracle:
    /// RFC 6298 in floating point — the integer path may floor once per
    /// sample, so it must track the float path within one unit per sample
    /// and never panic, even on `u32::MAX` samples with overflow checks on.
    #[test]
    fn rtt_estimator_tracks_rfc_6298_within_integer_rounding() {
        let strategy =
            prop::collection::vec(prop_oneof![3 => 0u32..5_000, 1 => any::<u32>()], 1..64);
        check(strategy, |samples| {
            let mut estimator = RttEstimator::new();
            let (mut srtt, mut rttvar) = (f64::from(samples[0]), f64::from(samples[0]) / 2.0);
            for (i, &sample) in samples.iter().enumerate() {
                estimator.sample(sample);
                if i > 0 {
                    let r = f64::from(sample);
                    rttvar = 0.75 * rttvar + 0.25 * (srtt - r).abs();
                    srtt = 0.875 * srtt + 0.125 * r;
                }
                let estimate = estimator.estimate();
                let slack = f64::from(u8::try_from(i + 1).expect("fewer than 64 samples"));
                prop_assert!(
                    (f64::from(estimate.srtt_ms) - srtt).abs() <= slack,
                    "srtt {} vs rfc {srtt} after {} samples",
                    estimate.srtt_ms,
                    i + 1
                );
                prop_assert!(
                    (f64::from(estimate.rttvar_ms) - rttvar).abs() <= slack,
                    "rttvar {} vs rfc {rttvar} after {} samples",
                    estimate.rttvar_ms,
                    i + 1
                );
                prop_assert_eq!(estimate.min_ms, *samples[..=i].iter().min().unwrap());
            }
            Ok(())
        });
    }
}
