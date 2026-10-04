//! Bounded custom UDP transport.
//!
//! [`Endpoint`] is the clockless protocol core: all time, keys, and nonces are
//! injected by its caller. Native sockets are polled on the caller's thread.

mod cookie;
mod endpoint;
mod latest;
mod packet;
mod peer;
mod reliable;
mod sequence;
mod session;
pub mod simulated;
mod transport;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod threaded;

pub use endpoint::{Endpoint, EndpointConfig, EndpointError, EndpointEvent, EndpointRole};
pub use session::{ClientEndpoint, ServerEndpoint};
pub use transport::DatagramTransport;

#[cfg(not(target_arch = "wasm32"))]
pub use native::{UdpClient, UdpServer, UdpSocketTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use threaded::{ThreadedUdpConfig, ThreadedUdpServer};

/// Maximum bytes in one UDP datagram.
pub const MAX_DATAGRAM_BYTES: usize = packet::DATAGRAM_BYTES;
/// Maximum bytes in one reliable fragment. Larger messages are fragmented.
pub const MAX_RELIABLE_FRAGMENT_BYTES: usize = packet::MAX_ITEM_PAYLOAD;
/// Latest state is intentionally capped by the shared transport contract.
pub const MAX_LATEST_PAYLOAD_BYTES: usize = crate::MAX_LATEST_STATE_BYTES;
