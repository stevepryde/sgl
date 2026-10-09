//! Reliable lanes: their configuration and admission rules, the scheduler
//! that shares one connection between them, and the fragment reassembly
//! every transport uses (netcode.md 11, 14 and 15).

use crate::{MAX_UNRELIABLE_BYTES, RELIABLE_LANES, ReliableCapacity};

/// Default [`ReliableConfig::max_message_bytes`].
pub const DEFAULT_RELIABLE_MESSAGE_BYTES: usize = 64 * 1024;
/// Ceiling for [`ReliableConfig::max_message_bytes`].
pub const RELIABLE_MESSAGE_BYTES_LIMIT: usize = 16 * 1024 * 1024;
/// Largest [`LaneConfig::weight`] a lane may be given.
pub const MAX_LANE_WEIGHT: u16 = 256;
/// Default [`LaneConfig::outbound_messages`].
pub const DEFAULT_LANE_OUTBOUND_MESSAGES: usize = 128;
/// Default [`LaneConfig::outbound_bytes`].
pub const DEFAULT_LANE_OUTBOUND_BYTES: usize = 256 * 1024;
/// Default [`LaneConfig::inbound_messages`].
pub const DEFAULT_LANE_INBOUND_MESSAGES: usize = 128;
/// Default [`LaneConfig::inbound_bytes`].
pub const DEFAULT_LANE_INBOUND_BYTES: usize = 256 * 1024;
/// Default [`LaneConfig::unreliable_messages`].
pub const DEFAULT_LANE_UNRELIABLE_MESSAGES: usize = 128;
/// Default [`LaneConfig::unreliable_bytes`].
pub const DEFAULT_LANE_UNRELIABLE_BYTES: usize = 64 * 1024;
/// Ceiling for a lane's message bounds.
pub const LANE_QUEUE_MESSAGES_LIMIT: usize = 65_536;
/// Ceiling for a lane's byte bounds.
pub const LANE_QUEUE_BYTES_LIMIT: usize = 256 * 1024 * 1024;

/// One lane's scheduling share and queue bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneConfig {
    /// Scheduling share, `1..=MAX_LANE_WEIGHT`: a backlogged lane sends
    /// `weight` fragments per round. Default 1 (equal shares).
    pub weight: u16,
    /// Messages this side holds for the lane until the peer acknowledges
    /// them, `1..=LANE_QUEUE_MESSAGES_LIMIT`. A full lane refuses `send`
    /// with [`SendError::WouldBlock`](crate::SendError::WouldBlock).
    pub outbound_messages: usize,
    /// Bytes this side holds for the lane until the peer acknowledges them,
    /// `1..=LANE_QUEUE_BYTES_LIMIT`. A lane holding no bytes admits one
    /// message of any size up to [`ReliableConfig::max_message_bytes`], so
    /// a bound smaller than a message never blocks it for good and the lane
    /// holds at most the larger of the two.
    pub outbound_bytes: usize,
    /// Completed messages from the peer not yet returned by `poll`,
    /// `1..=LANE_QUEUE_MESSAGES_LIMIT`. Past it the receiver slows the peer
    /// until `poll` makes room: UDP holds the lane's next message,
    /// unacknowledged, and native WebSocket stops reading. The browser
    /// cannot, so there a peer that exceeds it is disconnected with
    /// [`DisconnectReason::InboundOverflow`](crate::DisconnectReason::InboundOverflow).
    pub inbound_messages: usize,
    /// Bytes of completed messages not yet returned by `poll`,
    /// `1..=LANE_QUEUE_BYTES_LIMIT`. Beside them the lane holds at most one
    /// message larger than this bound, so a large message followed by
    /// smaller ones before the next poll fits, and the lane holds at most
    /// this plus [`ReliableConfig::max_message_bytes`].
    pub inbound_bytes: usize,
    /// Unreliable messages queued for the lane, `1..=LANE_QUEUE_MESSAGES_LIMIT`:
    /// on the sending side until sent, where a full queue refuses `send`
    /// with `WouldBlock`; on the receiving side until `poll` returns them,
    /// where a receiver that is not polled drops its oldest unpolled
    /// unreliable messages, as a full UDP socket buffer does.
    pub unreliable_messages: usize,
    /// Unreliable bytes queued for the lane, with the same two meanings,
    /// from [`MAX_UNRELIABLE_BYTES`] to `LANE_QUEUE_BYTES_LIMIT`.
    pub unreliable_bytes: usize,
}

impl LaneConfig {
    /// The default lane: weight 1 and the `DEFAULT_LANE_*` bounds.
    pub const DEFAULT: Self = Self {
        weight: 1,
        outbound_messages: DEFAULT_LANE_OUTBOUND_MESSAGES,
        outbound_bytes: DEFAULT_LANE_OUTBOUND_BYTES,
        inbound_messages: DEFAULT_LANE_INBOUND_MESSAGES,
        inbound_bytes: DEFAULT_LANE_INBOUND_BYTES,
        unreliable_messages: DEFAULT_LANE_UNRELIABLE_MESSAGES,
        unreliable_bytes: DEFAULT_LANE_UNRELIABLE_BYTES,
    };

    fn validate(&self) -> Result<(), ReliableConfigError> {
        if !(1..=MAX_LANE_WEIGHT).contains(&self.weight) {
            return Err(ReliableConfigError::InvalidWeight);
        }
        let messages = 1..=LANE_QUEUE_MESSAGES_LIMIT;
        // The one-message rules keep a bound below the largest message from
        // refusing it forever.
        let bytes = 1..=LANE_QUEUE_BYTES_LIMIT;
        if !messages.contains(&self.outbound_messages)
            || !messages.contains(&self.inbound_messages)
            || !messages.contains(&self.unreliable_messages)
            || !bytes.contains(&self.outbound_bytes)
            || !bytes.contains(&self.inbound_bytes)
            || !(MAX_UNRELIABLE_BYTES..=LANE_QUEUE_BYTES_LIMIT).contains(&self.unreliable_bytes)
        {
            return Err(ReliableConfigError::InvalidBound);
        }
        Ok(())
    }

    /// Whether a lane holding `held` (messages, bytes) for the peer admits
    /// a `len`-byte reliable message: within its message bound, and either
    /// holding no bytes (the one-message rule) or keeping its bytes within
    /// `outbound_bytes`. The message cap is checked before this.
    pub(crate) const fn outbound_admits(&self, held: (usize, usize), len: usize) -> bool {
        held.0 < self.outbound_messages && (held.1 == 0 || held.1 + len <= self.outbound_bytes)
    }

    /// What a lane holding `held` (messages, bytes) admits now: up to
    /// `max_message_bytes` while it holds no bytes, else what is left of
    /// `outbound_bytes` — nothing while a large message keeps it past them.
    pub(crate) fn outbound_capacity(
        &self,
        held: (usize, usize),
        max_message_bytes: usize,
    ) -> ReliableCapacity {
        let room = if held.1 == 0 {
            Some(max_message_bytes)
        } else {
            self.outbound_bytes.checked_sub(held.1)
        };
        room.map_or_else(ReliableCapacity::default, |room| {
            ReliableCapacity::remaining(
                self.outbound_messages.saturating_sub(held.0),
                room.min(max_message_bytes),
            )
        })
    }
}

impl Default for LaneConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Per-connection reliable configuration shared by every transport: the
/// largest reliable message, and one [`LaneConfig`] per lane, indexed by
/// [`Lane::index`](crate::Lane::index), bounding its reliable and unreliable
/// queues. Both ends of a connection use the same configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReliableConfig {
    /// Largest reliable payload, `1..=RELIABLE_MESSAGE_BYTES_LIMIT`
    /// (default [`DEFAULT_RELIABLE_MESSAGE_BYTES`]): a larger `send` returns
    /// [`SendError::PayloadTooLarge`](crate::SendError::PayloadTooLarge).
    /// Transports fragment and reassemble longer messages, holding at most
    /// one partial message of this size per lane. The receiver's value
    /// governs: a peer that declares a longer message is closed with
    /// [`DisconnectReason::ProtocolViolation`](crate::DisconnectReason::ProtocolViolation).
    pub max_message_bytes: usize,
    /// Each lane's weight and bounds.
    pub lanes: [LaneConfig; RELIABLE_LANES],
}

impl ReliableConfig {
    /// A [`DEFAULT_RELIABLE_MESSAGE_BYTES`] cap and every lane at
    /// [`LaneConfig::DEFAULT`].
    pub const DEFAULT: Self = Self {
        max_message_bytes: DEFAULT_RELIABLE_MESSAGE_BYTES,
        lanes: [LaneConfig::DEFAULT; RELIABLE_LANES],
    };

    /// Checks the message cap and every lane's weight and bounds against
    /// their ranges.
    pub fn validate(&self) -> Result<(), ReliableConfigError> {
        if !(1..=RELIABLE_MESSAGE_BYTES_LIMIT).contains(&self.max_message_bytes) {
            return Err(ReliableConfigError::InvalidMessageBytes);
        }
        self.lanes.iter().try_for_each(LaneConfig::validate)
    }
}

impl Default for ReliableConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why a [`ReliableConfig`] was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReliableConfigError {
    /// A weight is outside `1..=MAX_LANE_WEIGHT`.
    InvalidWeight,
    /// A message or byte bound is outside its range.
    InvalidBound,
    /// `max_message_bytes` is outside `1..=RELIABLE_MESSAGE_BYTES_LIMIT`.
    InvalidMessageBytes,
}

impl std::fmt::Display for ReliableConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidWeight => "reliable lane weight is outside 1..=MAX_LANE_WEIGHT",
            Self::InvalidBound => "reliable lane bound is outside its range",
            Self::InvalidMessageBytes => {
                "max_message_bytes is outside 1..=RELIABLE_MESSAGE_BYTES_LIMIT"
            }
        })
    }
}

impl std::error::Error for ReliableConfigError {}

/// One lane's completed reliable messages that `poll` has not returned yet,
/// against the lane's inbound bounds: at most `inbound_messages` messages,
/// and `inbound_bytes` bytes beside at most one message larger than that
/// (netcode.md 11). A message of any admitted size followed by smaller ones
/// before the next poll therefore fits, and the lane holds at most
/// `inbound_bytes` plus one message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InboundUsage {
    messages: usize,
    /// Bytes of the messages within `inbound_bytes` each.
    bytes: usize,
    /// Whether a message larger than `inbound_bytes` is held.
    oversized: bool,
}

impl InboundUsage {
    /// Whether the lane can take a completed `len`-byte message now.
    pub(crate) const fn admits(&self, len: usize, lane: &LaneConfig) -> bool {
        self.messages < lane.inbound_messages
            && if len > lane.inbound_bytes {
                !self.oversized
            } else {
                self.bytes + len <= lane.inbound_bytes
            }
    }

    /// Counts an admitted `len`-byte message.
    pub(crate) const fn add(&mut self, len: usize, lane: &LaneConfig) {
        self.messages += 1;
        if len > lane.inbound_bytes {
            self.oversized = true;
        } else {
            self.bytes += len;
        }
    }

    /// Releases a counted `len`-byte message that `poll` returned.
    pub(crate) const fn remove(&mut self, len: usize, lane: &LaneConfig) {
        self.messages -= 1;
        if len > lane.inbound_bytes {
            self.oversized = false;
        } else {
            self.bytes -= len;
        }
    }
}

/// Where a frame's payload sits in its reliable message.
///
/// A message that fits one frame is [`Whole`](Self::Whole); a longer one is
/// a [`First`](Self::First) that declares the message length, any number of
/// [`Middle`](Self::Middle) fragments, and a [`Last`](Self::Last).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fragment {
    /// The whole message.
    Whole,
    /// The first of several fragments, declaring the message's total length.
    First {
        /// Length of the whole message, larger than this fragment.
        total: u32,
    },
    /// A fragment after the first and before the last.
    Middle,
    /// The final fragment.
    Last,
}

impl Fragment {
    /// The wire's FIRST and MORE flags.
    pub(crate) const fn flags(self) -> (bool, bool) {
        match self {
            Self::Whole => (true, false),
            Self::First { .. } => (true, true),
            Self::Middle => (false, true),
            Self::Last => (false, false),
        }
    }

    /// The fragment for wire flags, with the total a FIRST-and-MORE frame
    /// carries.
    pub(crate) const fn from_flags(first: bool, more: bool, total: u32) -> Self {
        match (first, more) {
            (true, false) => Self::Whole,
            (true, true) => Self::First { total },
            (false, true) => Self::Middle,
            (false, false) => Self::Last,
        }
    }

    /// The total a FIRST fragment declares.
    pub(crate) const fn total(self) -> Option<u32> {
        match self {
            Self::First { total } => Some(total),
            Self::Whole | Self::Middle | Self::Last => None,
        }
    }

    /// The fragment of a `len`-byte message that starts at `start`, for
    /// frames carrying at most `fragment_bytes`, less `total_bytes` on a
    /// first fragment that declares the total: the fragment and its end.
    pub(crate) fn at(
        len: usize,
        start: usize,
        fragment_bytes: usize,
        total_bytes: usize,
    ) -> (Self, usize) {
        if start == 0 && len <= fragment_bytes {
            return (Self::Whole, len);
        }
        if start == 0 {
            let total = u32::try_from(len).expect("reliable messages are pre-bounded");
            return (Self::First { total }, fragment_bytes - total_bytes);
        }
        let end = len.min(start + fragment_bytes);
        (if end < len { Self::Middle } else { Self::Last }, end)
    }
}

/// Deficit round robin over the lanes with sendable work (Shreedhar and
/// Varghese, 1996). Every fragment is charged one whole fragment, since each
/// occupies one datagram or frame, so a lane's quantum is `weight`
/// fragments: a backlogged lane sends `weight` fragments per round, every
/// backlogged lane sends at least one, and between two fragments of lane `i`
/// the other lanes send at most the sum of their weights.
#[derive(Clone, Debug)]
pub(crate) struct LaneScheduler {
    weights: [u16; RELIABLE_LANES],
    /// The lane being visited.
    current: usize,
    /// Fragments `current` may still send in this visit; zero starts a new
    /// visit with a fresh quantum.
    remaining: u16,
}

impl LaneScheduler {
    pub(crate) fn new(config: &ReliableConfig) -> Self {
        Self {
            weights: config.lanes.map(|lane| lane.weight.max(1)),
            current: 0,
            remaining: 0,
        }
    }

    /// Picks and charges the lane that sends the next fragment among those
    /// `ready` says have one; `None` when none has.
    pub(crate) fn next(&mut self, mut ready: impl FnMut(usize) -> bool) -> Option<usize> {
        for _ in 0..RELIABLE_LANES {
            if self.remaining == 0 {
                self.remaining = self.weights[self.current];
            }
            if ready(self.current) {
                let lane = self.current;
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.advance();
                }
                return Some(lane);
            }
            // A lane with nothing to send forfeits the rest of its quantum,
            // as an emptied flow's deficit resets.
            self.advance();
        }
        None
    }

    /// The lane [`Self::next`] would pick, without charging it.
    pub(crate) fn peek(&self, ready: impl FnMut(usize) -> bool) -> Option<usize> {
        self.clone().next(ready)
    }

    fn advance(&mut self) {
        self.current = (self.current + 1) % RELIABLE_LANES;
        self.remaining = 0;
    }
}

/// A peer broke the fragment rules (netcode.md 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FramingViolation;

/// One lane's inbound reassembly: idle, or assembling one message against
/// the total its first fragment declared. Fragments arrive in lane order.
/// The buffer grows geometrically with what arrives, reserving at most twice
/// what has arrived and never past the declared total.
#[derive(Debug, Default)]
pub(crate) struct Reassembly {
    declared: Option<usize>,
    buffer: Vec<u8>,
}

impl Reassembly {
    /// The length of the message `fragment`, carrying `len` bytes, would
    /// complete; `None` when it completes nothing or breaks the rules.
    pub(crate) fn completes(&self, fragment: Fragment, len: usize) -> Option<usize> {
        match (fragment, self.declared) {
            (Fragment::Whole, None) => Some(len),
            (Fragment::Last, Some(declared)) if self.buffer.len() + len == declared => {
                Some(declared)
            }
            _ => None,
        }
    }

    /// Appends one fragment of a message declared `declared` bytes long,
    /// growing the buffer geometrically but never past the declaration.
    fn append(&mut self, payload: &[u8], declared: usize) {
        let needed = self.buffer.len() + payload.len();
        if needed > self.buffer.capacity() {
            let target = needed.max(2 * self.buffer.capacity()).min(declared);
            self.buffer.reserve_exact(target - self.buffer.len());
        }
        self.buffer.extend_from_slice(payload);
    }

    /// Applies one in-order fragment; returns the message it completes.
    /// Nothing is buffered beyond what arrived, and after a violation the
    /// caller closes the peer.
    pub(crate) fn push(
        &mut self,
        fragment: Fragment,
        payload: &[u8],
        max_message_bytes: usize,
    ) -> Result<Option<Vec<u8>>, FramingViolation> {
        let received = self.buffer.len().saturating_add(payload.len());
        match (fragment, self.declared) {
            (Fragment::Whole, None) if payload.len() <= max_message_bytes => {
                Ok(Some(payload.to_vec()))
            }
            (Fragment::First { total }, None)
                if (payload.len() + 1..=max_message_bytes).contains(&(total as usize)) =>
            {
                self.declared = Some(total as usize);
                self.append(payload, total as usize);
                Ok(None)
            }
            (Fragment::Middle, Some(declared)) if received < declared => {
                self.append(payload, declared);
                Ok(None)
            }
            (Fragment::Last, Some(declared)) if received == declared => {
                self.declared = None;
                self.append(payload, declared);
                Ok(Some(std::mem::take(&mut self.buffer)))
            }
            _ => Err(FramingViolation),
        }
    }

    /// Bytes of the message being assembled.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Defect: a reassembly that accepts a fragment the netcode.md 14 table
    /// rejects (a continuation or last while idle, a first or whole while
    /// assembling, a total over the cap or not above the first fragment, an
    /// overrun or a short final), or delivers before the declared total.
    /// Oracle: that table, row by row.
    #[wasm_bindgen_test(unsupported = test)]
    fn reassembly_follows_the_fragment_table() {
        const CAP: usize = 10;
        let first = |total: u32| Fragment::First { total };
        let violations: [&[(Fragment, &[u8])]; 10] = [
            &[(Fragment::Middle, b"a")],
            &[(Fragment::Last, b"a")],
            &[(first(11), b"a")],
            &[(first(2), b"ab")],
            &[(Fragment::Whole, &[0; CAP + 1])],
            &[(first(4), b"ab"), (Fragment::Whole, b"c")],
            &[(first(4), b"ab"), (first(4), b"cd")],
            &[(first(4), b"ab"), (Fragment::Middle, b"cd")],
            &[(first(4), b"ab"), (Fragment::Last, b"c")],
            &[(first(4), b"ab"), (Fragment::Last, b"cde")],
        ];
        for sequence in violations {
            let mut lane = Reassembly::default();
            let (last, rest) = sequence.split_last().unwrap();
            for (fragment, payload) in rest {
                assert_eq!(lane.push(*fragment, payload, CAP), Ok(None));
            }
            assert_eq!(
                lane.push(last.0, last.1, CAP),
                Err(FramingViolation),
                "{sequence:?}"
            );
        }

        let mut lane = Reassembly::default();
        assert_eq!(
            lane.push(Fragment::Whole, &[7; CAP], CAP),
            Ok(Some(vec![7; CAP]))
        );
        assert_eq!(lane.push(first(CAP as u32), b"ab", CAP), Ok(None));
        assert_eq!(lane.push(Fragment::Middle, b"cdefg", CAP), Ok(None));
        assert_eq!(lane.retained_bytes(), 7);
        assert_eq!(
            lane.push(Fragment::Last, b"hij", CAP),
            Ok(Some(b"abcdefghij".to_vec()))
        );
        assert_eq!(lane.retained_bytes(), 0);
        assert_eq!(lane.push(Fragment::Whole, b"", CAP), Ok(Some(Vec::new())));
    }

    /// Defect: fragment boundaries that lose, repeat or reorder bytes,
    /// overfill a frame, or label fragments so the reassembly table refuses
    /// them. Oracle: walking `Fragment::at` from the start and reassembling
    /// yields the message, each fragment within its frame's room.
    #[wasm_bindgen_test(unsupported = test)]
    fn fragment_boundaries_reassemble_into_the_message() {
        for len in [0usize, 1, 9, 10, 11, 15, 16, 17, 30, 31, 64] {
            let message: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut lane = Reassembly::default();
            let (mut start, mut delivered) = (0, None);
            while delivered.is_none() {
                let (fragment, end) = Fragment::at(len, start, 10, 4);
                let room = if fragment.total().is_some() { 6 } else { 10 };
                assert!(end - start <= room, "{len}: {fragment:?} {start}..{end}");
                delivered = lane.push(fragment, &message[start..end], 64).unwrap();
                assert!(
                    delivered.is_some() == (end == len),
                    "{len}: {fragment:?} ends at {end}"
                );
                start = end;
            }
            assert_eq!(delivered, Some(message), "{len}");
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use crate::proptest_support::check;
    use proptest::prelude::*;

    /// Defect: a scheduler that starves a backlogged lane, lets other lanes
    /// send past their shares between a lane's fragments, sends from an idle
    /// lane, or idles while a lane is ready. Oracle: deficit round robin's
    /// bounds for a uniform packet size (Shreedhar and Varghese 1996) —
    /// while lane `i` stays ready, the other lanes send at most the sum of
    /// their weights before its next fragment; with every lane ready, `N`
    /// picks give lane `i` at least `floor(N w_i / W) - W`.
    #[test]
    fn deficit_round_robin_keeps_its_published_bounds() {
        let weights = prop::array::uniform4(1..=MAX_LANE_WEIGHT);
        // Each step: which lanes are ready (a mask), and how many picks.
        let steps = prop::collection::vec((0u8..16, 1usize..600), 1..40);
        check((weights, steps), |(weights, steps)| {
            let mut config = ReliableConfig::DEFAULT;
            for (lane, weight) in config.lanes.iter_mut().zip(weights) {
                lane.weight = weight;
            }
            let mut scheduler = LaneScheduler::new(&config);
            let total: usize = weights.iter().map(|&w| usize::from(w)).sum();
            // Picks of other lanes since lane `i` last sent or became ready.
            let mut waited = [0usize; RELIABLE_LANES];
            let mut ready = [false; RELIABLE_LANES];
            for (mask, picks) in steps {
                for (lane, ready) in ready.iter_mut().enumerate() {
                    let now_ready = mask & (1 << lane) != 0;
                    if now_ready && !*ready {
                        waited[lane] = 0;
                    }
                    *ready = now_ready;
                }
                for _ in 0..picks {
                    let picked = scheduler.next(|lane| ready[lane]);
                    prop_assert_eq!(picked.is_some(), ready.contains(&true));
                    let Some(picked) = picked else { break };
                    prop_assert!(ready[picked], "picked idle lane {}", picked);
                    for lane in 0..RELIABLE_LANES {
                        if lane == picked {
                            waited[lane] = 0;
                        } else if ready[lane] {
                            waited[lane] += 1;
                            let others = total - usize::from(weights[lane]);
                            prop_assert!(
                                waited[lane] <= others,
                                "lane {} waited {} picks; bound {}",
                                lane,
                                waited[lane],
                                others
                            );
                        }
                    }
                }
            }

            // Every lane ready from a fresh scheduler: long-run shares.
            let mut scheduler = LaneScheduler::new(&config);
            let picks = 4 * total + 7;
            let mut counts = [0usize; RELIABLE_LANES];
            for _ in 0..picks {
                counts[scheduler.next(|_| true).expect("all ready")] += 1;
            }
            for lane in 0..RELIABLE_LANES {
                let share = picks * usize::from(weights[lane]) / total;
                prop_assert!(
                    counts[lane] + total >= share,
                    "lane {} sent {} of {}; fair share {}",
                    lane,
                    counts[lane],
                    picks,
                    share
                );
            }
            Ok(())
        });
    }
}
