use std::net::SocketAddr;

use super::latest::Latest;
use super::packet::{ACK_LEN, Acks, Nonces};
use super::reliable::Reliable;
use crate::lanes::LaneScheduler;
use crate::{
    MAX_RELIABLE_MESSAGE_BYTES, RELIABLE_LANES, ReliableConfig, RttEstimate, RttEstimator,
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
    /// One independent ARQ per reliable lane.
    pub reliable: [Reliable; RELIABLE_LANES],
    /// Shares this peer's datagrams between its lanes.
    pub scheduler: LaneScheduler,
    /// Completed reliable messages and bytes per lane in the current poll.
    pub delivered: [(usize, usize); RELIABLE_LANES],
    pub latest: Latest,
    pub last_receive_ms: u64,
    pub last_send_ms: u64,
    pub last_handshake_send_ms: u64,
    pub rtt: RttEstimator,
    pub verified_cookie_epoch: Option<u64>,
    pub close_grace: Option<CloseGrace>,
}

impl Peer {
    pub fn new(
        addr: SocketAddr,
        nonces: Nonces,
        handshake: Handshake,
        now_ms: u64,
        reliable: &ReliableConfig,
    ) -> Self {
        Self {
            addr,
            nonces,
            handshake,
            reliable: std::array::from_fn(|_| Reliable::new(MAX_RELIABLE_MESSAGE_BYTES)),
            scheduler: LaneScheduler::new(reliable),
            delivered: [(0, 0); RELIABLE_LANES],
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

    /// Reliable messages and bytes held for every lane until acknowledged.
    pub fn held(&self) -> (usize, usize) {
        self.reliable
            .iter()
            .fold((0, 0), |(messages, bytes), lane| {
                let (lane_messages, lane_bytes) = lane.held();
                (messages + lane_messages, bytes + lane_bytes)
            })
    }

    /// Inbound reliable bytes retained across lanes.
    pub fn retained_bytes(&self) -> usize {
        self.reliable.iter().map(Reliable::retained_bytes).sum()
    }

    pub fn outbound_is_idle(&self) -> bool {
        self.reliable.iter().all(Reliable::outbound_is_idle)
    }

    pub fn retry_exhausted(&self, now_ms: u64, maximum: u8) -> bool {
        let rto_ms = self.rto_ms();
        self.reliable
            .iter()
            .any(|lane| lane.retry_exhausted(now_ms, rto_ms, maximum))
    }

    /// Whether a lane owes the peer an acknowledgement.
    pub fn ack_dirty(&self) -> bool {
        self.reliable.iter().any(|lane| lane.ack_dirty)
    }

    /// The acknowledgements that fit in `room` bytes: every lane owing one
    /// first, then the other lanes that have received anything, repeated so
    /// a lost acknowledgement costs no retransmission.
    pub fn acks(&self, room: usize) -> Acks {
        let mut acks = [None; RELIABLE_LANES];
        let mut entries = room / ACK_LEN;
        for dirty in [true, false] {
            for (lane, ack) in self.reliable.iter().zip(&mut acks) {
                if entries > 0 && ack.is_none() && lane.has_received() && lane.ack_dirty == dirty {
                    *ack = Some(lane.ack());
                    entries -= 1;
                }
            }
        }
        acks
    }

    /// Records that `acks` went out: those lanes owe nothing more.
    pub fn sent_acks(&mut self, acks: &Acks) {
        for (lane, ack) in self.reliable.iter_mut().zip(acks) {
            if ack.is_some() {
                lane.ack_dirty = false;
            }
        }
    }
}
