use crate::lanes::Fragment;
use crate::{Delivery, Lane, MAX_LATEST_STATE_BYTES};

/// Frozen WebSocket envelope version.
pub const ENVELOPE_VERSION: u8 = 2;
/// Number of bytes before the opaque payload, or before the declared total
/// of a first fragment.
pub const ENVELOPE_HEADER_LEN: usize = 18;
/// The declared total a multi-fragment message's first frame carries.
pub const ENVELOPE_TOTAL_LEN: usize = 4;
/// Most reliable payload bytes one frame carries; longer messages are
/// fragmented so lanes interleave on the stream.
pub const WEBSOCKET_FRAGMENT_BYTES: usize = 16 * 1024;

const RELIABLE_KIND: u8 = 0;
const LATEST_KIND: u8 = 1;
const KIND_BITS: u8 = 0b11;
const FIRST: u8 = 1 << 2;
const MORE: u8 = 1 << 3;
const FLAG_BITS: u8 = KIND_BITS | FIRST | MORE;

/// One WebSocket application frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Envelope<'a> {
    /// Delivery semantics carried by this frame, including its lane.
    pub delivery: Delivery,
    /// Zero for reliable frames and nonzero for latest-state frames.
    pub sequence: u64,
    /// Where the payload sits in its reliable message; always
    /// [`Fragment::Whole`] for latest state.
    pub fragment: Fragment,
    /// Opaque transport payload.
    pub payload: &'a [u8],
}

/// Why a binary WebSocket application frame was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The frame is shorter than its header.
    Truncated,
    /// The three-byte magic did not match the caller-supplied identity.
    BadMagic,
    /// The single supported version does not match.
    UnsupportedVersion,
    /// The delivery kind is unknown.
    UnknownDelivery,
    /// Reliable used a nonzero sequence or latest-state used zero.
    InvalidSequence,
    /// A reliable lane past [`RELIABLE_LANES`](crate::RELIABLE_LANES), or
    /// latest state on a lane other than zero.
    InvalidLane,
    /// Reserved flag bits are set, or latest state carries fragment flags.
    InvalidFlags,
    /// A first fragment's declared total is not larger than the fragment.
    InvalidTotal,
    /// The declared payload length does not equal the frame remainder.
    LengthMismatch,
    /// The payload exceeds its frame's cap: [`WEBSOCKET_FRAGMENT_BYTES`]
    /// for reliable, [`MAX_LATEST_STATE_BYTES`] for latest state.
    TooLarge,
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid WebSocket envelope: {self:?}")
    }
}

impl std::error::Error for EnvelopeError {}

/// Encodes one frame: magic, version, flags, lane, big-endian sequence and
/// length, the declared total on a first fragment, then the payload.
pub fn encode_envelope(magic: [u8; 3], envelope: &Envelope<'_>) -> Result<Vec<u8>, EnvelopeError> {
    let Envelope {
        delivery,
        sequence,
        fragment,
        payload,
    } = *envelope;
    let (kind, lane, cap) = match delivery {
        Delivery::Reliable(lane) if sequence == 0 => {
            (RELIABLE_KIND, lane, WEBSOCKET_FRAGMENT_BYTES)
        }
        Delivery::LatestState if sequence != 0 => {
            (LATEST_KIND, Lane::DEFAULT, MAX_LATEST_STATE_BYTES)
        }
        Delivery::Reliable(_) | Delivery::LatestState => {
            return Err(EnvelopeError::InvalidSequence);
        }
    };
    let (first, more) = match delivery {
        Delivery::Reliable(_) => fragment.flags(),
        Delivery::LatestState if fragment == Fragment::Whole => (false, false),
        Delivery::LatestState => return Err(EnvelopeError::InvalidFlags),
    };
    if payload.len() > cap {
        return Err(EnvelopeError::TooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| EnvelopeError::TooLarge)?;
    if fragment.total().is_some_and(|total| total <= length) {
        return Err(EnvelopeError::InvalidTotal);
    }
    let mut output = Vec::with_capacity(ENVELOPE_HEADER_LEN + ENVELOPE_TOTAL_LEN + payload.len());
    output.extend_from_slice(&magic);
    output.push(ENVELOPE_VERSION);
    output.push(kind | if first { FIRST } else { 0 } | if more { MORE } else { 0 });
    output.push(u8::try_from(lane.index()).expect("lanes fit a byte"));
    output.extend_from_slice(&sequence.to_be_bytes());
    output.extend_from_slice(&length.to_be_bytes());
    if let Some(total) = fragment.total() {
        output.extend_from_slice(&total.to_be_bytes());
    }
    output.extend_from_slice(payload);
    Ok(output)
}

/// Decodes one complete binary frame and applies its rules: known kind and
/// lane, the sequence rule, fragment flags only on reliable frames, the
/// per-frame payload cap, and a declared total larger than the first
/// fragment.
pub fn decode_envelope(magic: [u8; 3], bytes: &[u8]) -> Result<Envelope<'_>, EnvelopeError> {
    if bytes.len() < ENVELOPE_HEADER_LEN {
        return Err(EnvelopeError::Truncated);
    }
    if bytes[..3] != magic {
        return Err(EnvelopeError::BadMagic);
    }
    if bytes[3] != ENVELOPE_VERSION {
        return Err(EnvelopeError::UnsupportedVersion);
    }
    let flags = bytes[4];
    if flags & !FLAG_BITS != 0 {
        return Err(EnvelopeError::InvalidFlags);
    }
    let (first, more) = (flags & FIRST != 0, flags & MORE != 0);
    let sequence = u64::from_be_bytes(bytes[6..14].try_into().expect("fixed-width sequence"));
    let length = u32::from_be_bytes(bytes[14..18].try_into().expect("fixed-width length"));
    let (delivery, cap) = match flags & KIND_BITS {
        RELIABLE_KIND => {
            let lane = Lane::new(bytes[5]).ok_or(EnvelopeError::InvalidLane)?;
            (Delivery::Reliable(lane), WEBSOCKET_FRAGMENT_BYTES)
        }
        LATEST_KIND if bytes[5] != 0 => return Err(EnvelopeError::InvalidLane),
        LATEST_KIND if first || more => return Err(EnvelopeError::InvalidFlags),
        LATEST_KIND => (Delivery::LatestState, MAX_LATEST_STATE_BYTES),
        _ => return Err(EnvelopeError::UnknownDelivery),
    };
    if (delivery == Delivery::LatestState) == (sequence == 0) {
        return Err(EnvelopeError::InvalidSequence);
    }
    if length as usize > cap {
        return Err(EnvelopeError::TooLarge);
    }
    let (header, total) = if first && more {
        let end = ENVELOPE_HEADER_LEN + ENVELOPE_TOTAL_LEN;
        let total = bytes
            .get(ENVELOPE_HEADER_LEN..end)
            .ok_or(EnvelopeError::Truncated)?;
        let total = u32::from_be_bytes(total.try_into().expect("fixed-width total"));
        if total <= length {
            return Err(EnvelopeError::InvalidTotal);
        }
        (end, total)
    } else {
        (ENVELOPE_HEADER_LEN, 0)
    };
    if bytes.len() != header + length as usize {
        return Err(EnvelopeError::LengthMismatch);
    }
    Ok(Envelope {
        delivery,
        sequence,
        fragment: match delivery {
            Delivery::Reliable(_) => Fragment::from_flags(first, more, total),
            Delivery::LatestState => Fragment::Whole,
        },
        payload: &bytes[header..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const MAGIC: [u8; 3] = *b"TST";

    fn reliable(lane: u8, fragment: Fragment, payload: &[u8]) -> Envelope<'_> {
        Envelope {
            delivery: Delivery::Reliable(Lane::new(lane).unwrap()),
            sequence: 0,
            fragment,
            payload,
        }
    }

    /// The frozen version-2 layout of netcode.md 13: flags then lane after
    /// the version, big-endian sequence and length, and the declared total
    /// only on a first fragment.
    #[wasm_bindgen_test(unsupported = test)]
    fn frozen_header_round_trips_every_frame_shape() {
        let whole = encode_envelope(MAGIC, &reliable(2, Fragment::Whole, b"event")).unwrap();
        assert_eq!(whole[..18], *b"TST\x02\x04\x02\0\0\0\0\0\0\0\0\0\0\0\x05");
        assert_eq!(whole.len(), ENVELOPE_HEADER_LEN + 5);

        let first = encode_envelope(
            MAGIC,
            &reliable(1, Fragment::First { total: 70_000 }, b"ab"),
        )
        .unwrap();
        assert_eq!(first[4..6], [0x0c, 1]);
        assert_eq!(first[18..22], 70_000_u32.to_be_bytes());
        assert_eq!(first.len(), ENVELOPE_HEADER_LEN + ENVELOPE_TOTAL_LEN + 2);
        assert_eq!(
            decode_envelope(MAGIC, &first).unwrap(),
            reliable(1, Fragment::First { total: 70_000 }, b"ab")
        );

        let latest = Envelope {
            delivery: Delivery::LatestState,
            sequence: 7,
            fragment: Fragment::Whole,
            payload: b"state",
        };
        let encoded = encode_envelope(MAGIC, &latest).unwrap();
        assert_eq!(encoded[4..14], [1, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
        assert_eq!(decode_envelope(MAGIC, &encoded).unwrap(), latest);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::cast_possible_truncation)]
mod properties {
    use super::*;
    use crate::RELIABLE_LANES;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;

    const MAGIC: [u8; 3] = *b"TST";

    fn delivery() -> impl Strategy<Value = Delivery> {
        prop_oneof![
            (0..RELIABLE_LANES).prop_map(|lane| Delivery::Reliable(Lane::new(lane as u8).unwrap())),
            Just(Delivery::LatestState)
        ]
    }

    fn fragment() -> impl Strategy<Value = Fragment> {
        prop_oneof![
            Just(Fragment::Whole),
            any::<u32>().prop_map(|total| Fragment::First { total }),
            Just(Fragment::Middle),
            Just(Fragment::Last),
        ]
    }

    /// A valid frame for `delivery` with `payload`.
    fn valid(
        delivery: Delivery,
        sequence: u64,
        fragment: Fragment,
        payload: &[u8],
    ) -> Envelope<'_> {
        let fragment = match (delivery, fragment) {
            (Delivery::LatestState, _) => Fragment::Whole,
            (_, Fragment::First { total }) => Fragment::First {
                total: (total % 100_000).max(payload.len() as u32 + 1),
            },
            (_, fragment) => fragment,
        };
        Envelope {
            delivery,
            sequence: if delivery == Delivery::LatestState {
                sequence.max(1)
            } else {
                0
            },
            fragment,
            payload,
        }
    }

    /// Defect: an out-of-bounds slice or an overflowing length sum on a
    /// hostile frame (overflow checks are on), or a frame from another
    /// identity or version accepted. Oracle: decoding untrusted bytes must
    /// return, and the first header checks are magic then version.
    #[test]
    fn decode_never_panics_and_rejects_foreign_identities() {
        check(bytes(600), |input| {
            let result = decode_envelope(MAGIC, &input);
            if input.len() < ENVELOPE_HEADER_LEN {
                prop_assert_eq!(result, Err(EnvelopeError::Truncated));
            } else if input[..3] != MAGIC {
                prop_assert_eq!(result, Err(EnvelopeError::BadMagic));
            } else if input[3] != ENVELOPE_VERSION {
                prop_assert_eq!(result, Err(EnvelopeError::UnsupportedVersion));
            }
            Ok(())
        });
    }

    /// Defect: the encoder and decoder disagreeing on a field's width, byte
    /// order or position (lane, flags, total), or a rule (reliable sequence
    /// zero, latest nonzero and unfragmented, a total above the fragment)
    /// enforced on one side only. Oracle: encode then decode is the identity
    /// for every valid frame, and the encoder refuses exactly the invalid
    /// ones.
    #[test]
    fn envelopes_round_trip_and_the_encoder_enforces_every_rule() {
        check(
            (delivery(), any::<u64>(), fragment(), bytes(300)),
            |(delivery, sequence, fragment, payload)| {
                let envelope = Envelope {
                    delivery,
                    sequence,
                    fragment,
                    payload: &payload,
                };
                let expected = match delivery {
                    _ if (delivery == Delivery::LatestState) == (sequence == 0) => {
                        Err(EnvelopeError::InvalidSequence)
                    }
                    Delivery::LatestState if fragment != Fragment::Whole => {
                        Err(EnvelopeError::InvalidFlags)
                    }
                    _ if fragment
                        .total()
                        .is_some_and(|total| total as usize <= payload.len()) =>
                    {
                        Err(EnvelopeError::InvalidTotal)
                    }
                    _ => Ok(()),
                };
                match encode_envelope(MAGIC, &envelope) {
                    Ok(frame) => {
                        prop_assert_eq!(expected, Ok(()));
                        prop_assert_eq!(decode_envelope(MAGIC, &frame), Ok(envelope));
                    }
                    Err(error) => prop_assert_eq!(expected, Err(error)),
                }
                Ok(())
            },
        );
    }

    /// Defect: a decoder that trusts the declared length over the frame
    /// size, accepts an unknown kind, reserved flags, a lane that does not
    /// exist, fragment flags or a lane on latest state, a total not above
    /// its fragment, or a payload past its frame cap. Oracle: each single
    /// corruption of a valid frame is refused with its own reason
    /// (netcode.md 13).
    #[test]
    fn single_corruptions_of_a_valid_frame_are_refused_with_the_right_reason() {
        let strategy = (
            delivery(),
            1u64..,
            fragment(),
            bytes(300),
            1usize..4,
            2u8..4,
            4u8..=255,
            1u8..16,
        );
        check(
            strategy,
            |(delivery, sequence, fragment, payload, extra, kind, lane, reserved)| {
                let envelope = valid(delivery, sequence, fragment, &payload);
                let frame = encode_envelope(MAGIC, &envelope).unwrap();
                let decode = |bytes: &[u8]| decode_envelope(MAGIC, bytes).map(|_| ());

                let mut longer = frame.clone();
                longer.extend(std::iter::repeat_n(0, extra));
                prop_assert_eq!(decode(&longer), Err(EnvelopeError::LengthMismatch));
                if !payload.is_empty() {
                    prop_assert_eq!(
                        decode(&frame[..frame.len() - 1]),
                        Err(EnvelopeError::LengthMismatch)
                    );
                }

                let mut unknown = frame.clone();
                unknown[4] = (unknown[4] & !KIND_BITS) | kind;
                prop_assert_eq!(decode(&unknown), Err(EnvelopeError::UnknownDelivery));

                let mut flagged = frame.clone();
                flagged[4] |= reserved << 4;
                prop_assert_eq!(decode(&flagged), Err(EnvelopeError::InvalidFlags));

                let mut laned = frame.clone();
                laned[5] = match delivery {
                    Delivery::Reliable(_) => lane,
                    Delivery::LatestState => lane - 3,
                };
                prop_assert_eq!(decode(&laned), Err(EnvelopeError::InvalidLane));

                let mut huge = frame.clone();
                let cap = match delivery {
                    Delivery::Reliable(_) => WEBSOCKET_FRAGMENT_BYTES,
                    Delivery::LatestState => MAX_LATEST_STATE_BYTES,
                };
                huge[14..18].copy_from_slice(&u32::try_from(cap + 1).unwrap().to_be_bytes());
                prop_assert_eq!(decode(&huge), Err(EnvelopeError::TooLarge));

                match envelope.fragment {
                    Fragment::First { .. } => {
                        let mut short_total = frame.clone();
                        short_total[18..22]
                            .copy_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
                        prop_assert_eq!(decode(&short_total), Err(EnvelopeError::InvalidTotal));
                    }
                    // Latest with fragment flags; reliable as the other kind
                    // keeps its sequence, which the other kind forbids.
                    _ if delivery == Delivery::LatestState => {
                        let mut fragmented = frame.clone();
                        fragmented[4] |= FIRST;
                        prop_assert_eq!(decode(&fragmented), Err(EnvelopeError::InvalidFlags));
                        let mut swapped = frame.clone();
                        swapped[4] ^= LATEST_KIND;
                        prop_assert_eq!(decode(&swapped), Err(EnvelopeError::InvalidSequence));
                    }
                    Fragment::Whole | Fragment::Middle | Fragment::Last => {
                        let mut swapped = frame.clone();
                        swapped[4] = LATEST_KIND;
                        swapped[5] = 0;
                        prop_assert_eq!(decode(&swapped), Err(EnvelopeError::InvalidSequence));
                    }
                }
                Ok(())
            },
        );
    }

    /// Defect: the payload cap enforced after allocation or off by one.
    /// Oracle: exactly the cap encodes, one more byte is `TooLarge`.
    #[test]
    fn payload_caps_are_exact() {
        check(delivery(), |delivery| {
            let cap = match delivery {
                Delivery::Reliable(_) => WEBSOCKET_FRAGMENT_BYTES,
                Delivery::LatestState => MAX_LATEST_STATE_BYTES,
            };
            let full = vec![0; cap];
            prop_assert!(
                encode_envelope(MAGIC, &valid(delivery, 1, Fragment::Whole, &full)).is_ok()
            );
            let over = vec![0; cap + 1];
            prop_assert_eq!(
                encode_envelope(MAGIC, &valid(delivery, 1, Fragment::Whole, &over)),
                Err(EnvelopeError::TooLarge)
            );
            Ok(())
        });
    }
}
