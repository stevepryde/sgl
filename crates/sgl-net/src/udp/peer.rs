use std::net::SocketAddr;

use super::latest::Latest;
use super::packet::Nonces;
use super::reliable::Reliable;
use crate::{
    MAX_RELIABLE_MESSAGE_BYTES, RELIABLE_INBOUND_BYTES, RELIABLE_INBOUND_MESSAGES,
    RELIABLE_OUTBOUND_BYTES, RELIABLE_OUTBOUND_MESSAGES, RttEstimate, RttEstimator,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handshake {
    ClientRequest,
    ClientConfirm,
    Connected,
}

#[derive(Debug, Clone, Copy)]
pub struct CloseGrace {
    pub deadline_ms: u64,
}

#[derive(Debug)]
pub struct Peer {
    pub addr: SocketAddr,
    pub nonces: Nonces,
    pub handshake: Handshake,
    pub reliable: Reliable,
    pub latest: Latest,
    pub last_receive_ms: u64,
    pub last_send_ms: u64,
    pub last_handshake_send_ms: u64,
    pub rtt: RttEstimator,
    pub verified_cookie_epoch: Option<u64>,
    pub close_grace: Option<CloseGrace>,
}

impl Peer {
    pub fn new(addr: SocketAddr, nonces: Nonces, handshake: Handshake, now_ms: u64) -> Self {
        Self {
            addr,
            nonces,
            handshake,
            reliable: Reliable::new(
                MAX_RELIABLE_MESSAGE_BYTES,
                RELIABLE_OUTBOUND_MESSAGES,
                RELIABLE_OUTBOUND_BYTES,
                RELIABLE_INBOUND_MESSAGES,
                RELIABLE_INBOUND_BYTES,
            ),
            latest: Latest::default(),
            last_receive_ms: now_ms,
            last_send_ms: now_ms,
            last_handshake_send_ms: now_ms,
            rtt: RttEstimator::new(),
            verified_cookie_epoch: None,
            close_grace: None,
        }
    }

    pub fn update_rtt(&mut self, samples: &[u64]) {
        for &sample in samples {
            self.rtt.sample(u32::try_from(sample).unwrap_or(u32::MAX));
        }
    }

    pub fn rto_ms(&self) -> u64 {
        let estimate = self.rtt.estimate();
        if estimate == RttEstimate::default() {
            return 200;
        }
        u64::from(estimate.srtt_ms)
            .saturating_add(4 * u64::from(estimate.rttvar_ms))
            .clamp(50, 1_000)
    }

    pub const fn rtt(&self) -> RttEstimate {
        self.rtt.estimate()
    }

    #[must_use]
    pub const fn is_closing(&self) -> bool {
        self.close_grace.is_some()
    }
}
