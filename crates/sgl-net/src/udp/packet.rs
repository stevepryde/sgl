//! Bounds-checked UDP datagram codec. The three-byte magic is caller-supplied.

pub const DATAGRAM_BYTES: usize = 1200;
pub const VERSION: u8 = 1;
pub const BASE_HEADER_LEN: usize = 3 + 1 + 1 + 8 + 8;
pub const PAYLOAD_HEADER_LEN: usize = BASE_HEADER_LEN + 2 + 4;
pub const ITEM_HEADER_LEN: usize = 5;
pub const MAX_ITEM_PAYLOAD: usize = DATAGRAM_BYTES - PAYLOAD_HEADER_LEN - ITEM_HEADER_LEN;

const RELIABLE_FINAL: u8 = 0;
const RELIABLE_MORE: u8 = 1;
const LATEST: u8 = 2;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    pub next: u16,
    pub bits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<'a> {
    Reliable {
        sequence: u16,
        more: bool,
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
        ack: Ack,
        items: ItemIter<'a>,
    },
}

fn header(output: &mut Vec<u8>, magic: [u8; 3], kind: Kind, nonces: Nonces) {
    output.clear();
    output.extend_from_slice(&magic);
    output.push(VERSION);
    output.push(kind as u8);
    output.extend_from_slice(&nonces.client.to_le_bytes());
    output.extend_from_slice(&nonces.server.to_le_bytes());
}

pub fn control(output: &mut Vec<u8>, magic: [u8; 3], kind: Kind, nonces: Nonces) {
    header(output, magic, kind, nonces);
}

pub fn begin_payload(output: &mut Vec<u8>, magic: [u8; 3], nonces: Nonces, ack: Ack) {
    header(output, magic, Kind::Payload, nonces);
    output.extend_from_slice(&ack.next.to_le_bytes());
    output.extend_from_slice(&ack.bits.to_le_bytes());
}

pub fn push_reliable(output: &mut Vec<u8>, sequence: u16, more: bool, payload: &[u8]) {
    output.push(if more { RELIABLE_MORE } else { RELIABLE_FINAL });
    push_item(output, sequence, payload);
}

pub fn push_latest(output: &mut Vec<u8>, sequence: u16, payload: &[u8]) {
    output.push(LATEST);
    push_item(output, sequence, payload);
}

fn push_item(output: &mut Vec<u8>, sequence: u16, payload: &[u8]) {
    let length = u16::try_from(payload.len()).expect("UDP item length is pre-bounded");
    output.extend_from_slice(&sequence.to_le_bytes());
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(payload);
}

pub fn parse(bytes: &[u8], magic: [u8; 3]) -> Option<Parsed<'_>> {
    if bytes.len() < BASE_HEADER_LEN || bytes[..3] != magic || bytes[3] != VERSION {
        return None;
    }
    let kind = Kind::decode(bytes[4])?;
    let nonces = Nonces {
        client: u64::from_le_bytes(bytes[5..13].try_into().ok()?),
        server: u64::from_le_bytes(bytes[13..21].try_into().ok()?),
    };
    if kind != Kind::Payload {
        return (bytes.len() == BASE_HEADER_LEN).then_some(Parsed::Control { kind, nonces });
    }
    if bytes.len() < PAYLOAD_HEADER_LEN {
        return None;
    }
    let mut rest = &bytes[PAYLOAD_HEADER_LEN..];
    let mut decoded = Vec::new();
    while !rest.is_empty() {
        if rest.len() < ITEM_HEADER_LEN {
            return None;
        }
        let tag = rest[0];
        let sequence = u16::from_le_bytes(rest[1..3].try_into().ok()?);
        let length = usize::from(u16::from_le_bytes(rest[3..5].try_into().ok()?));
        let payload = rest.get(ITEM_HEADER_LEN..ITEM_HEADER_LEN.checked_add(length)?)?;
        rest = &rest[ITEM_HEADER_LEN + length..];
        decoded.push(match tag {
            RELIABLE_FINAL => Item::Reliable {
                sequence,
                more: false,
                payload,
            },
            RELIABLE_MORE => Item::Reliable {
                sequence,
                more: true,
                payload,
            },
            LATEST => Item::Latest { sequence, payload },
            _ => return None,
        });
    }
    Some(Parsed::Payload {
        nonces,
        ack: Ack {
            next: u16::from_le_bytes(bytes[21..23].try_into().ok()?),
            bits: u32::from_le_bytes(bytes[23..27].try_into().ok()?),
        },
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

    #[wasm_bindgen_test(unsupported = test)]
    fn payload_round_trip_and_truncation_never_panics() {
        let nonces = Nonces {
            client: 11,
            server: 22,
        };
        let mut bytes = Vec::new();
        begin_payload(&mut bytes, MAGIC, nonces, Ack { next: 7, bits: 9 });
        push_reliable(&mut bytes, 3, false, b"event");
        push_latest(&mut bytes, 4, b"state");
        let Parsed::Payload { ack, items, .. } = parse(&bytes, MAGIC).unwrap() else {
            panic!("expected payload");
        };
        assert_eq!(ack, Ack { next: 7, bits: 9 });
        assert_eq!(items.count(), 2);
        for length in 0..bytes.len() {
            let _ = parse(&bytes[..length], MAGIC);
        }
        assert!(parse(&bytes[..PAYLOAD_HEADER_LEN + ITEM_HEADER_LEN + 4], MAGIC).is_none());
        assert!(parse(&bytes[..bytes.len() - 1], MAGIC).is_none());

        let mut unknown = bytes.clone();
        unknown[PAYLOAD_HEADER_LEN] = 99;
        assert!(parse(&unknown, MAGIC).is_none());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(parse(&trailing, MAGIC).is_none());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;

    const MAGIC: [u8; 3] = *b"TST";

    /// An owned item, comparable with what `parse` yields.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Owned {
        Reliable {
            sequence: u16,
            more: bool,
            payload: Vec<u8>,
        },
        Latest {
            sequence: u16,
            payload: Vec<u8>,
        },
    }

    impl From<Item<'_>> for Owned {
        fn from(item: Item<'_>) -> Self {
            match item {
                Item::Reliable {
                    sequence,
                    more,
                    payload,
                } => Self::Reliable {
                    sequence,
                    more,
                    payload: payload.to_vec(),
                },
                Item::Latest { sequence, payload } => Self::Latest {
                    sequence,
                    payload: payload.to_vec(),
                },
            }
        }
    }

    fn item() -> impl Strategy<Value = Owned> {
        prop_oneof![
            (any::<u16>(), any::<bool>(), bytes(300)).prop_map(|(sequence, more, payload)| {
                Owned::Reliable {
                    sequence,
                    more,
                    payload,
                }
            }),
            (any::<u16>(), bytes(300))
                .prop_map(|(sequence, payload)| Owned::Latest { sequence, payload }),
        ]
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

    fn encode(nonces: Nonces, ack: Ack, items: &[Owned]) -> Vec<u8> {
        let mut out = Vec::new();
        begin_payload(&mut out, MAGIC, nonces, ack);
        for item in items {
            match item {
                Owned::Reliable {
                    sequence,
                    more,
                    payload,
                } => push_reliable(&mut out, *sequence, *more, payload),
                Owned::Latest { sequence, payload } => push_latest(&mut out, *sequence, payload),
            }
        }
        out
    }

    fn parsed_items(bytes: &[u8]) -> Option<(Nonces, Ack, Vec<Owned>)> {
        match parse(bytes, MAGIC)? {
            Parsed::Payload { nonces, ack, items } => {
                Some((nonces, ack, items.map(Owned::from).collect()))
            }
            Parsed::Control { .. } => None,
        }
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
    /// parser, a tag confusion between reliable and latest items, or an
    /// item boundary computed off by one. Oracle: what was written is what
    /// is read, and `nonces` agrees with `parse`.
    #[test]
    fn payload_datagrams_round_trip_every_item() {
        let ack = (any::<u16>(), any::<u32>()).prop_map(|(next, bits)| Ack { next, bits });
        let strategy = (nonces(), ack, prop::collection::vec(item(), 0..4));
        check(strategy, |(nonces, ack, items)| {
            let encoded = encode(nonces, ack, &items);
            let (got_nonces, got_ack, got_items) = parsed_items(&encoded).expect("valid datagram");
            prop_assert_eq!(got_nonces, nonces);
            prop_assert_eq!(got_ack, ack);
            prop_assert_eq!(got_items, items);
            prop_assert_eq!(super::nonces(&encoded, MAGIC), Some(nonces));
            Ok(())
        });
    }

    /// Defect: a control datagram parsed as a payload, an unknown kind
    /// accepted, or trailing bytes tolerated. Oracle: every control kind
    /// round-trips exactly, and only at its exact length.
    #[test]
    fn control_datagrams_round_trip_and_reject_trailing_bytes() {
        check((kind(), nonces(), 1usize..8), |(kind, nonces, extra)| {
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
            out.extend(std::iter::repeat_n(0u8, extra));
            prop_assert!(parse(&out, MAGIC).is_none(), "trailing bytes accepted");
            Ok(())
        });
    }

    /// Defect: a truncated datagram yielding a partial or corrupted item.
    /// Oracle: any prefix of a valid datagram either fails to parse or
    /// parses to a prefix of the original items (a cut on an item boundary
    /// is itself a valid, shorter datagram); a flipped byte inside an item
    /// header never lets a payload escape its declared length.
    #[test]
    fn truncations_and_header_flips_never_yield_items_that_were_not_sent() {
        let ack = (any::<u16>(), any::<u32>()).prop_map(|(next, bits)| Ack { next, bits });
        let strategy = (
            nonces(),
            ack,
            prop::collection::vec(item(), 1..4),
            any::<usize>(),
            any::<usize>(),
            any::<u8>(),
        );
        check(strategy, |(nonces, ack, items, cut, flip_at, flip)| {
            let encoded = encode(nonces, ack, &items);
            let cut = cut % encoded.len();
            if let Some((_, _, prefix)) = parsed_items(&encoded[..cut]) {
                prop_assert!(
                    items.starts_with(&prefix),
                    "truncation produced {prefix:?} from {items:?}"
                );
            }
            let mut flipped = encoded.clone();
            let at = PAYLOAD_HEADER_LEN + flip_at % (encoded.len() - PAYLOAD_HEADER_LEN);
            flipped[at] ^= flip;
            if let Some((_, _, got)) = parsed_items(&flipped) {
                let total: usize = got
                    .iter()
                    .map(|i| match i {
                        Owned::Reliable { payload, .. } | Owned::Latest { payload, .. } => {
                            payload.len() + ITEM_HEADER_LEN
                        }
                    })
                    .sum();
                prop_assert_eq!(total, encoded.len() - PAYLOAD_HEADER_LEN);
            }
            Ok(())
        });
    }
}
