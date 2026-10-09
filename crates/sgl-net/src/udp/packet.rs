//! Bounds-checked UDP datagram codec, version 3. The three-byte magic is
//! caller-supplied.
//!
//! ```text
//! 0 magic[3]  3 version  4 kind (low nibble) | lane ack mask (high nibble,
//! Payload only)  5 client nonce u64 LE  13 server nonce u64 LE
//! Payload: one { next u16 LE, bits u32 LE } per mask bit, in lane order,
//! bits 0..30: fragment next + 1 + bit arrived; bit 31 HELD: fragment next
//! arrived and waits for the receiver's caller to make room,
//! then any number of items, to the datagram's end:
//! tag u8 | sequence u16 LE | length u16 LE | [total u32 LE iff FIRST and
//! MORE] | payload
//! tag: bits 0..1 kind (0 reliable, 1 latest, 2 unreliable), bit 2 FIRST,
//! bit 3 MORE, bits 4..7 lane
//! ```
//!
//! Latest and unreliable items carry neither flag; latest state has no lane.
//! An unreliable item's sequence is its lane's unreliable sequence, which
//! only lets the receiver drop network duplicates.

use crate::lanes::Fragment;
use crate::{Lane, RELIABLE_LANES};

pub const DATAGRAM_BYTES: usize = 1200;
pub const VERSION: u8 = 3;
pub const BASE_HEADER_LEN: usize = 3 + 1 + 1 + 8 + 8;
/// One lane's acknowledgement entry.
pub const ACK_LEN: usize = 2 + 4;
/// Every lane's acknowledgement entry.
pub const ALL_ACKS_LEN: usize = RELIABLE_LANES * ACK_LEN;
pub const ITEM_HEADER_LEN: usize = 5;
/// The declared total a multi-fragment message's first fragment carries.
pub const TOTAL_LEN: usize = 4;
/// Largest reliable fragment: a reliable item always fits beside every
/// lane's acknowledgement.
pub const MAX_RELIABLE_ITEM_PAYLOAD: usize =
    DATAGRAM_BYTES - BASE_HEADER_LEN - ALL_ACKS_LEN - ITEM_HEADER_LEN;
/// Largest latest-state item: room beside one lane's acknowledgement.
pub const MAX_LATEST_ITEM_PAYLOAD: usize =
    DATAGRAM_BYTES - BASE_HEADER_LEN - ACK_LEN - ITEM_HEADER_LEN;

const RELIABLE: u8 = 0;
const LATEST: u8 = 1;
const UNRELIABLE: u8 = 2;
const KIND_BITS: u8 = 0b11;
const FIRST: u8 = 1 << 2;
const MORE: u8 = 1 << 3;
const LANE_SHIFT: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    ConnectRequest = 1,
    ConnectChallenge = 2,
    ConnectConfirm = 3,
    ConnectAccept = 4,
    ConnectDeny = 5,
    Disconnect = 6,
    Payload = 7,
}

impl Kind {
    fn decode(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::ConnectRequest,
            2 => Self::ConnectChallenge,
            3 => Self::ConnectConfirm,
            4 => Self::ConnectAccept,
            5 => Self::ConnectDeny,
            6 => Self::Disconnect,
            7 => Self::Payload,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonces {
    pub client: u64,
    pub server: u64,
}

/// A lane's receive state: every fragment before `next` consumed, the
/// later ones marked in `bits` buffered, and whether `next` is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    pub next: u16,
    /// Bit `i` (below 31): fragment `next + 1 + i` arrived.
    pub bits: u32,
    /// Fragment `next` arrived, and the receiver holds it until its caller
    /// makes room for the message it completes.
    pub held: bool,
}

const HELD: u32 = 1 << 31;

/// The acknowledgements a payload datagram carries, by lane.
pub type Acks = [Option<Ack>; RELIABLE_LANES];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<'a> {
    Reliable {
        lane: Lane,
        sequence: u16,
        fragment: Fragment,
        payload: &'a [u8],
    },
    Unreliable {
        lane: Lane,
        sequence: u16,
        payload: &'a [u8],
    },
    Latest {
        sequence: u16,
        payload: &'a [u8],
    },
}

#[derive(Debug)]
pub enum Parsed<'a> {
    Control {
        kind: Kind,
        nonces: Nonces,
    },
    Payload {
        nonces: Nonces,
        acks: Acks,
        items: ItemIter<'a>,
    },
}

fn header(output: &mut Vec<u8>, magic: [u8; 3], kind: u8, nonces: Nonces) {
    output.clear();
    output.extend_from_slice(&magic);
    output.push(VERSION);
    output.push(kind);
    output.extend_from_slice(&nonces.client.to_le_bytes());
    output.extend_from_slice(&nonces.server.to_le_bytes());
}

pub fn control(output: &mut Vec<u8>, magic: [u8; 3], kind: Kind, nonces: Nonces) {
    header(output, magic, kind as u8, nonces);
}

/// Starts a payload datagram carrying `acks`, in lane order.
pub fn begin_payload(output: &mut Vec<u8>, magic: [u8; 3], nonces: Nonces, acks: &Acks) {
    let mask = acks
        .iter()
        .enumerate()
        .filter(|(_, ack)| ack.is_some())
        .fold(0_u8, |mask, (lane, _)| mask | 1 << lane);
    header(output, magic, Kind::Payload as u8 | mask << 4, nonces);
    for ack in acks.iter().flatten() {
        output.extend_from_slice(&ack.next.to_le_bytes());
        let bits = ack.bits | if ack.held { HELD } else { 0 };
        output.extend_from_slice(&bits.to_le_bytes());
    }
}

pub fn push_reliable(
    output: &mut Vec<u8>,
    lane: Lane,
    sequence: u16,
    fragment: Fragment,
    payload: &[u8],
) {
    let (first, more) = fragment.flags();
    let lane = u8::try_from(lane.index()).expect("lanes fit four bits");
    output.push(
        RELIABLE | if first { FIRST } else { 0 } | if more { MORE } else { 0 } | lane << LANE_SHIFT,
    );
    push_item(output, sequence, payload, fragment.total());
}

pub fn push_unreliable(output: &mut Vec<u8>, lane: Lane, sequence: u16, payload: &[u8]) {
    let lane = u8::try_from(lane.index()).expect("lanes fit four bits");
    output.push(UNRELIABLE | lane << LANE_SHIFT);
    push_item(output, sequence, payload, None);
}

pub fn push_latest(output: &mut Vec<u8>, sequence: u16, payload: &[u8]) {
    output.push(LATEST);
    push_item(output, sequence, payload, None);
}

/// Bytes the acknowledgements in `acks` take.
pub fn acks_len(acks: &Acks) -> usize {
    acks.iter().flatten().count() * ACK_LEN
}

/// Bytes an unreliable or latest-state item carrying `payload` bytes takes.
pub const fn item_len(payload: usize) -> usize {
    ITEM_HEADER_LEN + payload
}

/// Bytes a reliable item takes: a first fragment also declares its total.
pub const fn reliable_item_len(fragment: Fragment, payload: usize) -> usize {
    item_len(payload)
        + if fragment.total().is_some() {
            TOTAL_LEN
        } else {
            0
        }
}

fn push_item(output: &mut Vec<u8>, sequence: u16, payload: &[u8], total: Option<u32>) {
    let length = u16::try_from(payload.len()).expect("UDP item length is pre-bounded");
    output.extend_from_slice(&sequence.to_le_bytes());
    output.extend_from_slice(&length.to_le_bytes());
    if let Some(total) = total {
        output.extend_from_slice(&total.to_le_bytes());
    }
    output.extend_from_slice(payload);
}

/// Takes `count` bytes off the front of `rest`.
fn take<'a>(rest: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    let (taken, remaining) = rest.split_at_checked(count)?;
    *rest = remaining;
    Some(taken)
}

fn item<'a>(rest: &mut &'a [u8]) -> Option<Item<'a>> {
    let header = take(rest, ITEM_HEADER_LEN)?;
    let tag = header[0];
    let sequence = u16::from_le_bytes([header[1], header[2]]);
    let length = usize::from(u16::from_le_bytes([header[3], header[4]]));
    let (first, more, lane) = (tag & FIRST != 0, tag & MORE != 0, tag >> LANE_SHIFT);
    match tag & KIND_BITS {
        RELIABLE => {
            let lane = Lane::new(lane)?;
            let total = if first && more {
                let total = u32::from_le_bytes(take(rest, TOTAL_LEN)?.try_into().ok()?);
                // A first fragment is shorter than the message it declares.
                (total as usize > length).then_some(total)?
            } else {
                0
            };
            Some(Item::Reliable {
                lane,
                sequence,
                fragment: Fragment::from_flags(first, more, total),
                payload: take(rest, length)?,
            })
        }
        UNRELIABLE if !first && !more => Some(Item::Unreliable {
            lane: Lane::new(lane)?,
            sequence,
            payload: take(rest, length)?,
        }),
        LATEST if !first && !more && lane == 0 => Some(Item::Latest {
            sequence,
            payload: take(rest, length)?,
        }),
        _ => None,
    }
}

pub fn parse(bytes: &[u8], magic: [u8; 3]) -> Option<Parsed<'_>> {
    let nonces = nonces(bytes, magic)?;
    let (kind, mask) = (bytes[4] & 0x0f, bytes[4] >> 4);
    let kind = Kind::decode(kind)?;
    if kind != Kind::Payload {
        return (mask == 0 && bytes.len() == BASE_HEADER_LEN)
            .then_some(Parsed::Control { kind, nonces });
    }
    if usize::from(mask) >> RELIABLE_LANES != 0 {
        return None;
    }
    let mut rest = &bytes[BASE_HEADER_LEN..];
    let mut acks = [None; RELIABLE_LANES];
    for (lane, ack) in acks.iter_mut().enumerate() {
        if mask & 1 << lane != 0 {
            let entry = take(&mut rest, ACK_LEN)?;
            let bits = u32::from_le_bytes([entry[2], entry[3], entry[4], entry[5]]);
            *ack = Some(Ack {
                next: u16::from_le_bytes([entry[0], entry[1]]),
                bits: bits & !HELD,
                held: bits & HELD != 0,
            });
        }
    }
    let mut decoded = Vec::new();
    while !rest.is_empty() {
        decoded.push(item(&mut rest)?);
    }
    Some(Parsed::Payload {
        nonces,
        acks,
        items: ItemIter {
            items: decoded.into_iter(),
        },
    })
}

pub fn nonces(bytes: &[u8], magic: [u8; 3]) -> Option<Nonces> {
    if bytes.len() < BASE_HEADER_LEN || bytes[..3] != magic || bytes[3] != VERSION {
        return None;
    }
    Some(Nonces {
        client: u64::from_le_bytes(bytes[5..13].try_into().ok()?),
        server: u64::from_le_bytes(bytes[13..21].try_into().ok()?),
    })
}

#[derive(Debug)]
pub struct ItemIter<'a> {
    items: std::vec::IntoIter<Item<'a>>,
}

impl<'a> Iterator for ItemIter<'a> {
    type Item = Item<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.items.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const MAGIC: [u8; 3] = *b"TST";

    /// The room arithmetic the transport contract depends on: a reliable
    /// datagram fits a whole fragment with every lane's acknowledgement, a
    /// first fragment with its total, and a latest item at the shared cap
    /// with one lane's acknowledgement — each exactly at the datagram limit.
    #[wasm_bindgen_test(unsupported = test)]
    fn maximal_items_fill_the_datagram_exactly() {
        let nonces = Nonces {
            client: 1,
            server: 2,
        };
        let all = [Some(Ack {
            next: 1,
            bits: 2,
            held: true,
        }); RELIABLE_LANES];
        let lane = Lane::new(3).unwrap();
        let mut bytes = Vec::new();
        begin_payload(&mut bytes, MAGIC, nonces, &all);
        push_reliable(
            &mut bytes,
            lane,
            9,
            Fragment::Whole,
            &[1; MAX_RELIABLE_ITEM_PAYLOAD],
        );
        assert_eq!(bytes.len(), DATAGRAM_BYTES);
        begin_payload(&mut bytes, MAGIC, nonces, &all);
        push_reliable(
            &mut bytes,
            lane,
            9,
            Fragment::First { total: 70_000 },
            &[1; MAX_RELIABLE_ITEM_PAYLOAD - TOTAL_LEN],
        );
        assert_eq!(bytes.len(), DATAGRAM_BYTES);
        let mut one = [None; RELIABLE_LANES];
        one[2] = Some(Ack {
            next: 5,
            bits: 6,
            held: false,
        });
        begin_payload(&mut bytes, MAGIC, nonces, &one);
        push_latest(&mut bytes, 4, &[2; crate::MAX_LATEST_STATE_BYTES]);
        assert_eq!(bytes.len(), DATAGRAM_BYTES);
        let Parsed::Payload {
            acks, mut items, ..
        } = parse(&bytes, MAGIC).unwrap()
        else {
            panic!("expected payload");
        };
        assert_eq!(acks, one);
        assert!(
            matches!(items.next(), Some(Item::Latest { sequence: 4, payload }) if payload.len() == crate::MAX_LATEST_STATE_BYTES)
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::cast_possible_truncation)]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;

    const MAGIC: [u8; 3] = *b"TST";

    /// An owned item, comparable with what `parse` yields.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Owned {
        Reliable {
            lane: Lane,
            sequence: u16,
            fragment: Fragment,
            payload: Vec<u8>,
        },
        Unreliable {
            lane: Lane,
            sequence: u16,
            payload: Vec<u8>,
        },
        Latest {
            sequence: u16,
            payload: Vec<u8>,
        },
    }

    impl Owned {
        fn encoded_len(&self) -> usize {
            match self {
                Self::Reliable {
                    fragment, payload, ..
                } => {
                    ITEM_HEADER_LEN
                        + payload.len()
                        + if fragment.total().is_some() {
                            TOTAL_LEN
                        } else {
                            0
                        }
                }
                Self::Unreliable { payload, .. } | Self::Latest { payload, .. } => {
                    ITEM_HEADER_LEN + payload.len()
                }
            }
        }
    }

    impl From<Item<'_>> for Owned {
        fn from(item: Item<'_>) -> Self {
            match item {
                Item::Reliable {
                    lane,
                    sequence,
                    fragment,
                    payload,
                } => Self::Reliable {
                    lane,
                    sequence,
                    fragment,
                    payload: payload.to_vec(),
                },
                Item::Unreliable {
                    lane,
                    sequence,
                    payload,
                } => Self::Unreliable {
                    lane,
                    sequence,
                    payload: payload.to_vec(),
                },
                Item::Latest { sequence, payload } => Self::Latest {
                    sequence,
                    payload: payload.to_vec(),
                },
            }
        }
    }

    fn lane() -> impl Strategy<Value = Lane> {
        (0..RELIABLE_LANES).prop_map(|index| Lane::new(index as u8).unwrap())
    }

    fn item() -> impl Strategy<Value = Owned> {
        let reliable = (lane(), any::<u16>(), bytes(300), 0u8..4, any::<u32>()).prop_map(
            |(lane, sequence, payload, kind, extra)| {
                let total = u32::try_from(payload.len()).unwrap() + 1 + extra % 100_000;
                let fragment = match kind {
                    0 => Fragment::Whole,
                    1 => Fragment::First { total },
                    2 => Fragment::Middle,
                    _ => Fragment::Last,
                };
                Owned::Reliable {
                    lane,
                    sequence,
                    fragment,
                    payload,
                }
            },
        );
        prop_oneof![
            reliable,
            (lane(), any::<u16>(), bytes(300)).prop_map(|(lane, sequence, payload)| {
                Owned::Unreliable {
                    lane,
                    sequence,
                    payload,
                }
            }),
            (any::<u16>(), bytes(300))
                .prop_map(|(sequence, payload)| Owned::Latest { sequence, payload }),
        ]
    }

    fn acks() -> impl Strategy<Value = Acks> {
        prop::array::uniform4(prop::option::of(
            (any::<u16>(), 0..HELD, any::<bool>()).prop_map(|(next, bits, held)| Ack {
                next,
                bits: bits & !HELD,
                held,
            }),
        ))
    }

    fn nonces() -> impl Strategy<Value = Nonces> {
        (any::<u64>(), any::<u64>()).prop_map(|(client, server)| Nonces { client, server })
    }

    fn kind() -> impl Strategy<Value = Kind> {
        prop_oneof![
            Just(Kind::ConnectRequest),
            Just(Kind::ConnectChallenge),
            Just(Kind::ConnectConfirm),
            Just(Kind::ConnectAccept),
            Just(Kind::ConnectDeny),
            Just(Kind::Disconnect),
        ]
    }

    fn encode(nonces: Nonces, acks: &Acks, items: &[Owned]) -> Vec<u8> {
        let mut out = Vec::new();
        begin_payload(&mut out, MAGIC, nonces, acks);
        for item in items {
            match item {
                Owned::Reliable {
                    lane,
                    sequence,
                    fragment,
                    payload,
                } => push_reliable(&mut out, *lane, *sequence, *fragment, payload),
                Owned::Unreliable {
                    lane,
                    sequence,
                    payload,
                } => push_unreliable(&mut out, *lane, *sequence, payload),
                Owned::Latest { sequence, payload } => push_latest(&mut out, *sequence, payload),
            }
        }
        out
    }

    fn parsed_items(bytes: &[u8]) -> Option<(Nonces, Acks, Vec<Owned>)> {
        match parse(bytes, MAGIC)? {
            Parsed::Payload {
                nonces,
                acks,
                items,
            } => Some((nonces, acks, items.map(Owned::from).collect())),
            Parsed::Control { .. } => None,
        }
    }

    fn acks_len(acks: &Acks) -> usize {
        acks.iter().flatten().count() * ACK_LEN
    }

    /// Defect: an index or length computation that panics on a hostile
    /// datagram (overflow checks are on), or a decoder that accepts the
    /// wrong magic or version. Oracle: the requirement itself — parsing
    /// untrusted bytes must return, and only this identity is accepted.
    #[test]
    fn parse_never_panics_and_rejects_foreign_identities() {
        check(bytes(1_400), |input| {
            let _ = parse(&input, MAGIC);
            let _ = super::nonces(&input, MAGIC);
            if input.len() >= BASE_HEADER_LEN && (input[..3] != MAGIC || input[3] != VERSION) {
                prop_assert!(parse(&input, MAGIC).is_none());
                prop_assert!(super::nonces(&input, MAGIC).is_none());
            }
            Ok(())
        });
    }

    /// Defect: a header field added or reordered in the writer but not the
    /// parser, an ack entry attributed to the wrong lane, a lane, flag or
    /// total dropped, items of one kind read as another, or an
    /// item boundary computed off by one. Oracle: what was written is what
    /// is read, and `nonces` agrees with `parse`.
    #[test]
    fn payload_datagrams_round_trip_every_item() {
        let strategy = (nonces(), acks(), prop::collection::vec(item(), 0..4));
        check(strategy, |(nonces, acks, items)| {
            let encoded = encode(nonces, &acks, &items);
            let (got_nonces, got_acks, got_items) = parsed_items(&encoded).expect("valid datagram");
            prop_assert_eq!(got_nonces, nonces);
            prop_assert_eq!(got_acks, acks);
            prop_assert_eq!(got_items, items);
            prop_assert_eq!(super::nonces(&encoded, MAGIC), Some(nonces));
            Ok(())
        });
    }

    /// Defect: a control datagram parsed as a payload, an unknown kind
    /// accepted, an ack mask honoured on a control kind, or trailing bytes
    /// tolerated. Oracle: every control kind round-trips exactly, only at
    /// its exact length and with no mask bits.
    #[test]
    fn control_datagrams_round_trip_and_reject_trailing_bytes_and_masks() {
        check(
            (kind(), nonces(), 1usize..8, 1u8..16),
            |(kind, nonces, extra, mask)| {
                let mut out = Vec::new();
                control(&mut out, MAGIC, kind, nonces);
                match parse(&out, MAGIC) {
                    Some(Parsed::Control {
                        kind: got_kind,
                        nonces: got_nonces,
                    }) => {
                        prop_assert_eq!(got_kind, kind);
                        prop_assert_eq!(got_nonces, nonces);
                    }
                    other => prop_assert!(false, "control parsed as {other:?}"),
                }
                let mut masked = out.clone();
                masked[4] |= mask << 4;
                prop_assert!(parse(&masked, MAGIC).is_none(), "mask on {kind:?}");
                out.extend(std::iter::repeat_n(0u8, extra));
                prop_assert!(parse(&out, MAGIC).is_none(), "trailing bytes accepted");
                Ok(())
            },
        );
    }

    /// Defect: an item the wire rules forbid reaching a lane — a reliable or
    /// unreliable lane past `RELIABLE_LANES`, a latest or unreliable item
    /// with fragment flags, a latest item with a lane, the reserved item
    /// kind, or a first fragment whose declared total is missing or not above
    /// its own length. Oracle: the explicit
    /// rejection rules of netcode.md 12, applied to one corrupted item of an
    /// otherwise valid datagram.
    #[test]
    fn items_the_wire_rules_forbid_reject_the_datagram() {
        let strategy = (nonces(), acks(), bytes(40), any::<u16>(), 0u8..10, 0u32..2);
        check(
            strategy,
            |(nonces, acks, payload, sequence, case, slack)| {
                let lane0 = Lane::DEFAULT;
                let mut bytes = Vec::new();
                begin_payload(&mut bytes, MAGIC, nonces, &acks);
                let at = bytes.len();
                let len = u32::try_from(payload.len()).unwrap();
                match case {
                    // Reliable lanes 4..=15 do not exist.
                    0 => {
                        push_reliable(&mut bytes, lane0, sequence, Fragment::Whole, &payload);
                        bytes[at] |= (4 + (slack as u8) * 7) << LANE_SHIFT;
                    }
                    // Latest state carries no fragment flags and no lane.
                    1 => {
                        push_latest(&mut bytes, sequence, &payload);
                        bytes[at] |= FIRST;
                    }
                    2 => {
                        push_latest(&mut bytes, sequence, &payload);
                        bytes[at] |= MORE;
                    }
                    3 => {
                        push_latest(&mut bytes, sequence, &payload);
                        bytes[at] |= (1 + slack as u8) << LANE_SHIFT;
                    }
                    // Item kind 3 is reserved, on its own or over another kind.
                    4 => {
                        push_reliable(&mut bytes, lane0, sequence, Fragment::Whole, &payload);
                        bytes[at] |= KIND_BITS;
                    }
                    5 => {
                        push_unreliable(&mut bytes, lane0, sequence, &payload);
                        bytes[at] |= KIND_BITS;
                    }
                    // Unreliable items carry no fragment flags and a lane
                    // that exists.
                    8 => {
                        push_unreliable(&mut bytes, lane0, sequence, &payload);
                        bytes[at] |= if slack == 0 { FIRST } else { MORE };
                    }
                    9 => {
                        push_unreliable(&mut bytes, lane0, sequence, &payload);
                        bytes[at] |= (4 + (slack as u8) * 7) << LANE_SHIFT;
                    }
                    // A first fragment declares more than it carries.
                    6 => {
                        push_reliable(
                            &mut bytes,
                            lane0,
                            sequence,
                            Fragment::First { total: len + 1 },
                            &payload,
                        );
                        let total = at + ITEM_HEADER_LEN;
                        bytes[total..total + TOTAL_LEN]
                            .copy_from_slice(&(len - slack.min(len)).to_le_bytes());
                    }
                    // FIRST and MORE set on an item written without a total:
                    // whatever its first payload bytes declare, it is short.
                    _ => {
                        push_reliable(&mut bytes, lane0, sequence, Fragment::Middle, &payload);
                        bytes[at] |= FIRST;
                    }
                }
                prop_assert!(parse(&bytes, MAGIC).is_none(), "case {} accepted", case);
                Ok(())
            },
        );
    }

    /// Defect: a truncated datagram yielding a partial or corrupted item.
    /// Oracle: any prefix of a valid datagram either fails to parse or
    /// parses to a prefix of the original items (a cut on an item boundary
    /// is itself a valid, shorter datagram); a flipped byte inside an item
    /// never lets a payload escape its declared length.
    #[test]
    fn truncations_and_header_flips_never_yield_items_that_were_not_sent() {
        let strategy = (
            nonces(),
            acks(),
            prop::collection::vec(item(), 1..4),
            any::<usize>(),
            any::<usize>(),
            any::<u8>(),
        );
        check(strategy, |(nonces, acks, items, cut, flip_at, flip)| {
            let encoded = encode(nonces, &acks, &items);
            let cut = cut % encoded.len();
            if let Some((_, _, prefix)) = parsed_items(&encoded[..cut]) {
                prop_assert!(
                    items.starts_with(&prefix),
                    "truncation produced {prefix:?} from {items:?}"
                );
            }
            let header = BASE_HEADER_LEN + acks_len(&acks);
            let mut flipped = encoded.clone();
            let at = header + flip_at % (encoded.len() - header);
            flipped[at] ^= flip;
            if let Some((_, _, got)) = parsed_items(&flipped) {
                let total: usize = got.iter().map(Owned::encoded_len).sum();
                prop_assert_eq!(total, encoded.len() - header);
            }
            Ok(())
        });
    }
}
