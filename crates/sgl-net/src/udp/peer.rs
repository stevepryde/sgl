use std::net::SocketAddr;

use super::latest::Latest;
use super::packet::{self, ACK_LEN, Acks, MAX_RELIABLE_ITEM_PAYLOAD, Nonces};
use super::reliable::{MAX_RTO_MS, Reliable};
use super::unreliable::Unreliable;
use crate::lanes::{InboundUsage, LaneScheduler};
use crate::{RELIABLE_LANES, ReliableConfig, RttEstimate, RttEstimator};

/// The item a lane sends next, chosen before it is taken so the endpoint
/// can first check that it fits the datagram being packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// A due retransmission of the in-flight fragment with this sequence.
    Retransmit(u16),
    /// The lane's oldest queued unreliable message.
    Unreliable,
    /// The lane's next new reliable fragment.
    Fragment,
}

/// A lane item taken for the datagram being packed.
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
    /// Per lane: whether its next fresh item is an unreliable message rather
    /// than a new reliable fragment, when it has both.
    unreliable_turn: [bool; RELIABLE_LANES],
    /// Lanes whose acknowledgement went out in the current flush.
    acked: [bool; RELIABLE_LANES],
    /// Shares this peer's items between its lanes.
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
            acked: [false; RELIABLE_LANES],
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
        if !self.rtt.is_seeded() {
            return 200;
        }
        let estimate = self.rtt.estimate();
        u64::from(estimate.srtt_ms)
            .saturating_add(4 * u64::from(estimate.rttvar_ms))
            .clamp(50, MAX_RTO_MS)
    }

    pub const fn rtt(&self) -> RttEstimate {
        self.rtt.estimate()
    }

    /// Applies one datagram's acknowledgements, returning the round trips
    /// they sample. Anything newly acknowledged on any lane returns every
    /// lane's backoff to the round-trip estimate (RFC 9002 §6.2.1).
    pub fn acknowledge(&mut self, acks: &Acks, now_ms: u64) -> Vec<u64> {
        let mut samples = Vec::new();
        let mut progressed = false;
        for (lane, ack) in self.reliable.iter_mut().zip(acks) {
            if let Some(ack) = ack {
                progressed |= lane.acknowledge(*ack, now_ms, &mut samples);
            }
        }
        if progressed {
            self.reliable.iter_mut().for_each(Reliable::reset_backoff);
        }
        samples
    }

    /// Whether a lane has waited `bound_ms` for an acknowledgement with no
    /// progress ([`Reliable::stalled`]).
    pub fn stalled(&self, now_ms: u64, bound_ms: u64) -> bool {
        self.reliable
            .iter()
            .any(|lane| lane.stalled(now_ms, bound_ms))
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

    /// The item `lane` sends next and its encoded length, without taking
    /// it: a due retransmission first; else a new reliable fragment and an
    /// unreliable message take turns, so neither waits behind more than one
    /// of the other. `None` while the lane has nothing to send.
    pub fn peek(
        &self,
        lane: usize,
        now_ms: u64,
        rto_ms: u64,
        max_transmissions: Option<u8>,
    ) -> Option<(Next, usize)> {
        let reliable = &self.reliable[lane];
        if let Some(sequence) = reliable.first_due(now_ms, rto_ms, max_transmissions) {
            let (fragment, bytes) = reliable
                .fragment(sequence)
                .expect("a due fragment is in flight");
            let len = packet::reliable_item_len(fragment, bytes.len());
            return Some((Next::Retransmit(sequence), len));
        }
        let fresh = reliable.upcoming();
        if (self.unreliable_turn[lane] || fresh.is_none())
            && let Some(len) = self.unreliable[lane].front_len()
        {
            return Some((Next::Unreliable, packet::item_len(len)));
        }
        let (fragment, start, end) = fresh?;
        Some((
            Next::Fragment,
            packet::reliable_item_len(fragment, end - start),
        ))
    }

    /// Takes the item [`Self::peek`] chose for `lane`; a reliable fragment
    /// counts as transmitted at `now_ms`.
    pub fn take(&mut self, lane: usize, next: Next, now_ms: u64) -> Outgoing {
        match next {
            Next::Retransmit(sequence) => {
                self.reliable[lane].mark_sent(sequence, now_ms);
                Outgoing::Reliable(sequence)
            }
            Next::Unreliable => {
                self.unreliable_turn[lane] = false;
                let (sequence, payload) = self.unreliable[lane]
                    .pop()
                    .expect("peeked a queued message");
                Outgoing::Unreliable(sequence, payload)
            }
            Next::Fragment => {
                let sequence = self.reliable[lane]
                    .admit()
                    .expect("peeked a fragment the window admits");
                self.reliable[lane].mark_sent(sequence, now_ms);
                self.unreliable_turn[lane] = !self.unreliable[lane].is_empty();
                Outgoing::Reliable(sequence)
            }
        }
    }

    /// Whether `lane` owes the peer an acknowledgement in this flush.
    fn owes_ack(&self, lane: usize) -> bool {
        self.reliable[lane].acks_owed > 0 && !self.acked[lane]
    }

    /// Whether a lane owes the peer an acknowledgement in this flush.
    pub fn ack_dirty(&self) -> bool {
        (0..RELIABLE_LANES).any(|lane| self.owes_ack(lane))
    }

    /// The acknowledgements that fit in `room` bytes: every lane owing one
    /// first, then the other lanes that have received anything, repeated so
    /// a lost acknowledgement costs no retransmission.
    pub fn acks(&self, room: usize) -> Acks {
        let mut acks = [None; RELIABLE_LANES];
        let mut entries = room / ACK_LEN;
        for owed in [true, false] {
            for (index, ack) in acks.iter_mut().enumerate() {
                let lane = &self.reliable[index];
                if entries > 0
                    && ack.is_none()
                    && lane.has_received()
                    && self.owes_ack(index) == owed
                {
                    *ack = Some(lane.ack());
                    entries -= 1;
                }
            }
        }
        acks
    }

    /// Records that `acks` went out: those lanes owe nothing more in this
    /// flush.
    pub fn sent_acks(&mut self, acks: &Acks) {
        for (acked, ack) in self.acked.iter_mut().zip(acks) {
            *acked |= ack.is_some();
        }
    }

    /// Ends a flush: each lane whose acknowledgement went out owes one flush
    /// fewer.
    pub fn end_flush(&mut self) {
        for (lane, acked) in self.reliable.iter_mut().zip(&mut self.acked) {
            if std::mem::take(acked) {
                lane.acks_owed = lane.acks_owed.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn peer() -> Peer {
        Peer::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            Nonces {
                client: 1,
                server: 2,
            },
            Handshake::Connected,
            0,
            &ReliableConfig::default(),
        )
    }

    /// Defect: a 0 ms first sample (loopback, sub-millisecond LAN) yields the
    /// all-zero estimate and was read as "unmeasured", keeping the 200 ms
    /// unseeded timeout. Oracle: srtt + 4 * rttvar = 0 clamps to the 50 ms
    /// floor, as a 1 ms sample does.
    #[test]
    fn a_zero_ms_first_sample_seeds_the_retransmission_floor() {
        let mut unmeasured = peer();
        assert_eq!(unmeasured.rto_ms(), 200);
        unmeasured.update_rtt(&[0]);
        assert_eq!(unmeasured.rto_ms(), 50);
    }
}
