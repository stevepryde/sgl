use std::net::SocketAddr;

use super::latest::Latest;
use super::packet::{ACK_LEN, Acks, MAX_RELIABLE_ITEM_PAYLOAD, Nonces};
use super::reliable::Reliable;
use super::unreliable::Unreliable;
use crate::lanes::{InboundUsage, LaneScheduler};
use crate::{RELIABLE_LANES, ReliableConfig, RttEstimate, RttEstimator};

/// What one lane sends in one datagram.
pub enum Outgoing {
    /// The in-flight reliable fragment with this sequence.
    Reliable(u16),
    /// An unreliable message, sent once, with its unreliable sequence.
    Unreliable(u16, Vec<u8>),
}

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
    /// Each lane's unreliable queue and duplicate filter.
    pub unreliable: [Unreliable; RELIABLE_LANES],
    /// Per lane: whether its next fresh datagram goes to an unreliable
    /// message rather than a new reliable fragment, when it has both.
    unreliable_turn: [bool; RELIABLE_LANES],
    /// Shares this peer's datagrams between its lanes.
    pub scheduler: LaneScheduler,
    /// Completed reliable messages per lane that the caller still holds
    /// (those of the current poll, and any it reported holding from earlier
    /// ones), against the lane's inbound bounds.
    pub delivered: [InboundUsage; RELIABLE_LANES],
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
            reliable: std::array::from_fn(|_| {
                Reliable::new(reliable.max_message_bytes, MAX_RELIABLE_ITEM_PAYLOAD)
            }),
            unreliable: Default::default(),
            unreliable_turn: [false; RELIABLE_LANES],
            scheduler: LaneScheduler::new(reliable),
            delivered: [InboundUsage::default(); RELIABLE_LANES],
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
            && self.unreliable.iter().all(Unreliable::is_empty)
    }

    /// Whether `lane` has a datagram to send now.
    pub fn sendable(&self, lane: usize, now_ms: u64, rto_ms: u64, max_transmissions: u8) -> bool {
        let reliable = &self.reliable[lane];
        !self.unreliable[lane].is_empty()
            || reliable.window_open()
            || reliable
                .first_due(now_ms, rto_ms, max_transmissions)
                .is_some()
    }

    /// The datagram `lane` sends next: a due retransmission first; else a
    /// new reliable fragment and an unreliable message take turns, so
    /// neither waits behind more than one of the other.
    pub fn next_outgoing(
        &mut self,
        lane: usize,
        now_ms: u64,
        rto_ms: u64,
        max_transmissions: u8,
    ) -> Option<Outgoing> {
        let reliable = &mut self.reliable[lane];
        if let Some(sequence) = reliable.first_due(now_ms, rto_ms, max_transmissions) {
            return Some(Outgoing::Reliable(sequence));
        }
        let unreliable = &mut self.unreliable[lane];
        let turn = &mut self.unreliable_turn[lane];
        if (*turn || !reliable.window_open())
            && let Some((sequence, payload)) = unreliable.pop()
        {
            *turn = false;
            return Some(Outgoing::Unreliable(sequence, payload));
        }
        let sequence = reliable.admit()?;
        *turn = !unreliable.is_empty();
        Some(Outgoing::Reliable(sequence))
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
