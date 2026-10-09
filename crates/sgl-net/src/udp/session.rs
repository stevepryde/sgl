//! Session-trait adapters for any datagram transport.

use std::net::SocketAddr;

use super::{DatagramTransport, Endpoint, EndpointConfig, EndpointError, EndpointEvent};
use crate::{
    ClientEvent, ClientIo, ConnectionId, Delivery, Lane, ReliableCapacity, RttEstimate, SendError,
    ServerEvent, ServerIo,
};

/// Server-side session adapter over a caller-supplied datagram transport.
pub struct ServerEndpoint<T: DatagramTransport> {
    endpoint: Endpoint<T>,
}

impl<T: DatagramTransport> ServerEndpoint<T> {
    pub fn new(
        transport: T,
        config: EndpointConfig,
        cookie_key: [u8; 32],
    ) -> Result<Self, EndpointError> {
        Ok(Self {
            endpoint: Endpoint::server(transport, config, cookie_key)?,
        })
    }

    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint<T> {
        &self.endpoint
    }

    pub const fn endpoint_mut(&mut self) -> &mut Endpoint<T> {
        &mut self.endpoint
    }
}

impl<T: DatagramTransport> ServerIo for ServerEndpoint<T> {
    fn poll(&mut self, now_ms: u64) -> Vec<ServerEvent> {
        self.endpoint
            .poll(now_ms)
            .into_iter()
            .filter_map(|event| match event {
                EndpointEvent::Connected { peer } => {
                    ConnectionId::from_raw(peer).map(|conn| ServerEvent::Connected { conn })
                }
                EndpointEvent::Message {
                    peer,
                    delivery,
                    payload,
                } => ConnectionId::from_raw(peer).map(|conn| ServerEvent::Message {
                    conn,
                    delivery,
                    payload,
                }),
                EndpointEvent::Disconnected { peer, reason } => ConnectionId::from_raw(peer)
                    .map(|conn| ServerEvent::Disconnected { conn, reason }),
                EndpointEvent::Denied { .. } => None,
            })
            .collect()
    }

    fn send(
        &mut self,
        conn: ConnectionId,
        delivery: Delivery,
        payload: &[u8],
    ) -> Result<(), SendError> {
        self.endpoint.send(conn.raw(), delivery, payload)
    }

    fn capacity(&self, conn: ConnectionId, lane: Lane) -> ReliableCapacity {
        self.endpoint.capacity(conn.raw(), lane)
    }

    fn flush(&mut self, now_ms: u64) {
        self.endpoint.flush(now_ms);
    }

    fn disconnect(&mut self, conn: ConnectionId, now_ms: u64) {
        self.endpoint.disconnect(conn.raw(), now_ms);
    }

    fn stop_admission(&mut self) {
        self.endpoint.stop_admission();
    }
}

/// Single-server client session adapter over a caller-supplied datagram transport.
pub struct ClientEndpoint<T: DatagramTransport> {
    endpoint: Endpoint<T>,
    peer: u64,
}

impl<T: DatagramTransport> ClientEndpoint<T> {
    pub fn connect(
        transport: T,
        server: SocketAddr,
        config: EndpointConfig,
        now_ms: u64,
        nonce: u64,
    ) -> Result<Self, EndpointError> {
        let mut endpoint = Endpoint::client(transport, config)?;
        let peer = endpoint.start_connect(server, now_ms, nonce)?;
        Ok(Self { endpoint, peer })
    }

    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint<T> {
        &self.endpoint
    }

    pub const fn endpoint_mut(&mut self) -> &mut Endpoint<T> {
        &mut self.endpoint
    }
}

impl<T: DatagramTransport> ClientIo for ClientEndpoint<T> {
    fn poll(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        self.endpoint
            .poll(now_ms)
            .into_iter()
            .filter_map(|event| match event {
                EndpointEvent::Connected { peer } if peer == self.peer => {
                    Some(ClientEvent::Connected)
                }
                EndpointEvent::Message {
                    peer,
                    delivery,
                    payload,
                } if peer == self.peer => Some(ClientEvent::Message { delivery, payload }),
                EndpointEvent::Denied { peer, reason } if peer == self.peer => {
                    Some(ClientEvent::Denied { reason })
                }
                EndpointEvent::Disconnected { peer, reason } if peer == self.peer => {
                    Some(ClientEvent::Disconnected { reason })
                }
                _ => None,
            })
            .collect()
    }

    fn send(&mut self, delivery: Delivery, payload: &[u8]) -> Result<(), SendError> {
        self.endpoint.send(self.peer, delivery, payload)
    }

    fn capacity(&self, lane: Lane) -> ReliableCapacity {
        self.endpoint.capacity(self.peer, lane)
    }

    fn flush(&mut self, now_ms: u64) {
        self.endpoint.flush(now_ms);
    }

    fn disconnect(&mut self, now_ms: u64) {
        self.endpoint.disconnect(self.peer, now_ms);
    }

    fn rtt(&self) -> RttEstimate {
        self.endpoint.rtt(self.peer)
    }
}
