//! Datagram I/O abstraction used by the clockless endpoint.

use std::net::SocketAddr;

/// Minimal datagram boundary. Implementations must be bounded and nonblocking.
pub trait DatagramTransport {
    /// Attempts to send one already-bounded datagram.
    fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64);

    /// Returns one datagram available at virtual time `now_ms`.
    ///
    /// The endpoint supplies a 1,201-byte buffer so a 1,201-byte receive can be
    /// distinguished from a valid 1,200-byte datagram and rejected.
    fn receive(&mut self, output: &mut [u8], now_ms: u64) -> Option<(usize, SocketAddr)>;
}
