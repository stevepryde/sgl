//! Datagram I/O abstraction used by the clockless endpoint.

use std::io;
use std::net::SocketAddr;

/// Minimal datagram boundary. Implementations must be bounded and nonblocking.
pub trait DatagramTransport {
    /// Attempts to send one already-bounded datagram. `Ok(())` means the
    /// local transport accepted it, not that the remote received it.
    /// Return `WouldBlock` for temporary local buffer exhaustion (including
    /// native `ENOBUFS`), or `Interrupted` for a retryable interruption.
    /// The endpoint keeps that datagram's work for a later flush. Other
    /// errors concern one datagram and retain the existing network-loss
    /// behavior; they do not end the connection.
    fn send(&mut self, destination: SocketAddr, payload: &[u8], now_ms: u64) -> io::Result<()>;

    /// Returns one datagram available at virtual time `now_ms`, `Ok(None)`
    /// when none is waiting, or an error for a receive that failed (a
    /// datagram too large for `output`, a reported ICMP error). The
    /// endpoint skips a failed receive and reads on within its per-poll
    /// datagram cap; only `Ok(None)` ends its poll's reading.
    ///
    /// The endpoint supplies a 1,201-byte buffer so a 1,201-byte receive can be
    /// distinguished from a valid 1,200-byte datagram and rejected.
    fn receive(
        &mut self,
        output: &mut [u8],
        now_ms: u64,
    ) -> io::Result<Option<(usize, SocketAddr)>>;
}

/// A refusal before the datagram entered the transport is not network loss.
pub(super) fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
