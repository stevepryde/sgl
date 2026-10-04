//! Native nonblocking UDP sockets polled on the caller's thread.

use std::io;
use std::net::SocketAddr;

use super::{DatagramTransport, Endpoint, EndpointConfig, EndpointEvent};
use crate::{
    ClientEvent, ClientIo, ConnectionId, Delivery, DisconnectReason, RttEstimate, SendError,
    ServerEvent, ServerIo,
};

/// `std` UDP socket used by the clockless endpoint.
#[derive(Debug)]
pub struct UdpSocketTransport {
    socket: std::net::UdpSocket,
}

impl UdpSocketTransport {
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = std::net::UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Self { socket })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

impl DatagramTransport for UdpSocketTransport {
    fn send(&mut self, destination: SocketAddr, payload: &[u8], _now_ms: u64) {
        let _ = self.socket.send_to(payload, destination);
    }

    fn receive(&mut self, output: &mut [u8], _now_ms: u64) -> Option<(usize, SocketAddr)> {
        self.socket.recv_from(output).ok()
    }
}

/// Native server wrapper around the generic endpoint.
pub struct UdpServer {
    endpoint: Endpoint<UdpSocketTransport>,
}

impl UdpServer {
    pub fn bind(address: SocketAddr, endpoint_config: EndpointConfig) -> io::Result<Self> {
        let mut key = [0_u8; 32];
        getrandom::fill(&mut key).map_err(|error| io::Error::other(error.to_string()))?;
        Self::bind_with_key(address, endpoint_config, key)
    }

    pub fn bind_with_key(
        address: SocketAddr,
        endpoint_config: EndpointConfig,
        cookie_key: [u8; 32],
    ) -> io::Result<Self> {
        let transport = UdpSocketTransport::bind(address)?;
        Ok(Self {
            endpoint: Endpoint::server(transport, endpoint_config, cookie_key)
                .map_err(io::Error::other)?,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.transport().local_addr()
    }
}

impl ServerIo for UdpServer {
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

/// Native single-server client wrapper.
pub struct UdpClient {
    endpoint: Endpoint<UdpSocketTransport>,
    peer: u64,
}

impl UdpClient {
    pub fn connect(
        server: SocketAddr,
        endpoint_config: EndpointConfig,
        now_ms: u64,
    ) -> io::Result<Self> {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let nonce = u64::from_le_bytes(nonce).max(1);
        Self::connect_with_nonce(server, endpoint_config, now_ms, nonce)
    }

    pub fn connect_with_nonce(
        server: SocketAddr,
        endpoint_config: EndpointConfig,
        now_ms: u64,
        nonce: u64,
    ) -> io::Result<Self> {
        let bind = if server.is_ipv4() {
            SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            SocketAddr::from(([0_u16; 8], 0))
        };
        let transport = UdpSocketTransport::bind(bind)?;
        let mut endpoint =
            Endpoint::client(transport, endpoint_config).map_err(io::Error::other)?;
        let peer = endpoint
            .start_connect(server, now_ms, nonce)
            .map_err(io::Error::other)?;
        Ok(Self { endpoint, peer })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.transport().local_addr()
    }
}

impl ClientIo for UdpClient {
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

const _: DisconnectReason = DisconnectReason::Transport;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_round_trip_delivers_a_reliable_payload() {
        let mut server = UdpServer::bind_with_key(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            EndpointConfig::default(),
            [31; 32],
        )
        .unwrap();
        let mut client = UdpClient::connect_with_nonce(
            server.local_addr().unwrap(),
            EndpointConfig::default(),
            0,
            41,
        )
        .unwrap();

        let mut connection = None;
        for now in 0..200 {
            for event in server.poll(now) {
                if let ServerEvent::Connected { conn } = event {
                    connection = Some(conn);
                }
            }
            client.poll(now);
            server.flush(now);
            client.flush(now);
            if connection.is_some() {
                break;
            }
        }
        let connection = connection.expect("loopback handshake completes");

        client.send(Delivery::ReliableOrdered, b"loopback").unwrap();
        let mut delivered = false;
        for now in 200..400 {
            client.flush(now);
            delivered |= server.poll(now).iter().any(|event| {
                matches!(
                    event,
                    ServerEvent::Message {
                        conn,
                        delivery: Delivery::ReliableOrdered,
                        payload,
                    } if *conn == connection && payload == b"loopback"
                )
            });
            server.flush(now);
            client.poll(now);
            if delivered {
                break;
            }
        }
        assert!(delivered);
    }

    #[test]
    fn initial_timeout_disconnects_the_client() {
        let config = EndpointConfig {
            timeout_ms: 5,
            keepalive_ms: 1,
            handshake_retry_ms: 1,
            ..EndpointConfig::default()
        };
        let mut client =
            UdpClient::connect_with_nonce(SocketAddr::from(([127, 0, 0, 1], 9)), config, 0, 42)
                .unwrap();

        assert_eq!(
            client.poll(5),
            vec![ClientEvent::Disconnected {
                reason: DisconnectReason::TimedOut,
            }]
        );
        assert_eq!(
            client.send(Delivery::ReliableOrdered, b"after-timeout"),
            Err(SendError::Disconnected)
        );
    }
}
