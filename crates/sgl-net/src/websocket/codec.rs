use crate::{Delivery, MAX_LATEST_STATE_BYTES, MAX_RELIABLE_MESSAGE_BYTES};

/// Frozen WebSocket envelope version.
pub const ENVELOPE_VERSION: u8 = 1;
/// Number of bytes before the opaque payload.
pub const ENVELOPE_HEADER_LEN: usize = 17;

const RELIABLE_CLASS: u8 = 0;
const LATEST_CLASS: u8 = 1;

/// A decoded WebSocket application envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// Delivery semantics carried by this frame.
    pub delivery: Delivery,
    /// Zero for reliable frames and nonzero for latest-state frames.
    pub sequence: u64,
    /// Opaque transport payload.
    pub payload: Vec<u8>,
}

/// Why a binary WebSocket application frame was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The frame is shorter than the fixed header.
    Truncated,
    /// The three-byte magic did not match the caller-supplied identity.
    BadMagic,
    /// The single supported version does not match.
    UnsupportedVersion,
    /// The delivery-class byte is unknown.
    UnknownDelivery,
    /// Reliable used a nonzero sequence or latest-state used zero.
    InvalidSequence,
    /// The declared payload length does not equal the frame remainder.
    LengthMismatch,
    /// The payload exceeds the selected delivery class's shared cap.
    TooLarge,
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid WebSocket envelope: {self:?}")
    }
}

impl std::error::Error for EnvelopeError {}

/// Encodes one envelope. Callers must supply a nonzero sequence for latest state.
pub fn encode_envelope(
    magic: [u8; 3],
    delivery: Delivery,
    sequence: u64,
    payload: &[u8],
) -> Result<Vec<u8>, EnvelopeError> {
    let (class, sequence, cap) = match delivery {
        Delivery::ReliableOrdered if sequence == 0 => {
            (RELIABLE_CLASS, 0, MAX_RELIABLE_MESSAGE_BYTES)
        }
        Delivery::LatestState if sequence != 0 => (LATEST_CLASS, sequence, MAX_LATEST_STATE_BYTES),
        Delivery::ReliableOrdered | Delivery::LatestState => {
            return Err(EnvelopeError::InvalidSequence);
        }
    };
    if payload.len() > cap || payload.len() > u32::MAX as usize {
        return Err(EnvelopeError::TooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| EnvelopeError::TooLarge)?;
    let mut output = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
    output.extend_from_slice(&magic);
    output.push(ENVELOPE_VERSION);
    output.push(class);
    output.extend_from_slice(&sequence.to_be_bytes());
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(payload);
    Ok(output)
}

/// Decodes one complete binary message and applies the delivery-class cap.
pub fn decode_envelope(magic: [u8; 3], bytes: &[u8]) -> Result<Envelope, EnvelopeError> {
    if bytes.len() < ENVELOPE_HEADER_LEN {
        return Err(EnvelopeError::Truncated);
    }
    if bytes[..3] != magic {
        return Err(EnvelopeError::BadMagic);
    }
    if bytes[3] != ENVELOPE_VERSION {
        return Err(EnvelopeError::UnsupportedVersion);
    }
    let class = bytes[4];
    let sequence = u64::from_be_bytes(bytes[5..13].try_into().expect("fixed-width sequence"));
    let length = u32::from_be_bytes(bytes[13..17].try_into().expect("fixed-width length")) as usize;
    let (delivery, cap) = match class {
        RELIABLE_CLASS if sequence == 0 => (Delivery::ReliableOrdered, MAX_RELIABLE_MESSAGE_BYTES),
        LATEST_CLASS if sequence != 0 => (Delivery::LatestState, MAX_LATEST_STATE_BYTES),
        RELIABLE_CLASS | LATEST_CLASS => return Err(EnvelopeError::InvalidSequence),
        _ => return Err(EnvelopeError::UnknownDelivery),
    };
    if length > cap {
        return Err(EnvelopeError::TooLarge);
    }
    if bytes.len() != ENVELOPE_HEADER_LEN.saturating_add(length) {
        return Err(EnvelopeError::LengthMismatch);
    }
    Ok(Envelope {
        delivery,
        sequence,
        payload: bytes[ENVELOPE_HEADER_LEN..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const MAGIC: [u8; 3] = *b"TST";

    #[wasm_bindgen_test(unsupported = test)]
    fn frozen_header_round_trips_both_delivery_classes() {
        let reliable = encode_envelope(MAGIC, Delivery::ReliableOrdered, 0, b"event").unwrap();
        assert_eq!(&reliable[..5], b"TST\x01\x00");
        assert_eq!(reliable.len(), ENVELOPE_HEADER_LEN + 5);
        assert_eq!(decode_envelope(MAGIC, &reliable).unwrap().sequence, 0);

        let latest = encode_envelope(MAGIC, Delivery::LatestState, 7, b"state").unwrap();
        assert_eq!(&latest[5..13], &7_u64.to_be_bytes());
        assert_eq!(
            decode_envelope(MAGIC, &latest).unwrap().delivery,
            Delivery::LatestState
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn malformed_wrong_class_and_oversized_frames_are_rejected() {
        let valid = encode_envelope(MAGIC, Delivery::LatestState, 1, b"state").unwrap();
        assert_eq!(
            decode_envelope(MAGIC, &valid[..16]),
            Err(EnvelopeError::Truncated)
        );

        let mut bad_magic = valid.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            decode_envelope(MAGIC, &bad_magic),
            Err(EnvelopeError::BadMagic)
        );
        let mut bad_version = valid.clone();
        bad_version[3] = 2;
        assert_eq!(
            decode_envelope(MAGIC, &bad_version),
            Err(EnvelopeError::UnsupportedVersion)
        );
        let mut bad_class = valid.clone();
        bad_class[4] = 9;
        assert_eq!(
            decode_envelope(MAGIC, &bad_class),
            Err(EnvelopeError::UnknownDelivery)
        );
        let mut zero_latest = valid.clone();
        zero_latest[5..13].copy_from_slice(&0_u64.to_be_bytes());
        assert_eq!(
            decode_envelope(MAGIC, &zero_latest),
            Err(EnvelopeError::InvalidSequence)
        );
        let mut wrong_length = valid;
        wrong_length[13..17].copy_from_slice(&999_u32.to_be_bytes());
        assert_eq!(
            decode_envelope(MAGIC, &wrong_length),
            Err(EnvelopeError::LengthMismatch)
        );

        let oversized = vec![0; MAX_LATEST_STATE_BYTES + 1];
        assert_eq!(
            encode_envelope(MAGIC, Delivery::LatestState, 1, &oversized),
            Err(EnvelopeError::TooLarge)
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::*;
    use crate::proptest_support::{bytes, check};
    use proptest::prelude::*;

    const MAGIC: [u8; 3] = *b"TST";

    fn delivery() -> impl Strategy<Value = Delivery> {
        prop_oneof![Just(Delivery::ReliableOrdered), Just(Delivery::LatestState)]
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

    /// Defect: the encoder and decoder disagreeing on a field's width or
    /// byte order, or the class/sequence rule (reliable = 0, latest ≠ 0)
    /// enforced on one side only. Oracle: encode then decode is the
    /// identity for every valid combination and both sides refuse the
    /// invalid ones the same way.
    #[test]
    fn envelopes_round_trip_and_both_sides_enforce_the_sequence_rule() {
        check(
            (delivery(), any::<u64>(), bytes(300)),
            |(delivery, sequence, payload)| {
                let valid = match delivery {
                    Delivery::ReliableOrdered => sequence == 0,
                    Delivery::LatestState => sequence != 0,
                };
                match encode_envelope(MAGIC, delivery, sequence, &payload) {
                    Ok(frame) => {
                        prop_assert!(valid);
                        prop_assert_eq!(frame.len(), ENVELOPE_HEADER_LEN + payload.len());
                        let decoded = decode_envelope(MAGIC, &frame).expect("own frame decodes");
                        prop_assert_eq!(decoded.delivery, delivery);
                        prop_assert_eq!(decoded.sequence, sequence);
                        prop_assert_eq!(decoded.payload, payload);
                    }
                    Err(error) => {
                        prop_assert!(!valid);
                        prop_assert_eq!(error, EnvelopeError::InvalidSequence);
                    }
                }
                Ok(())
            },
        );
    }

    /// Defect: a decoder that trusts the declared length over the frame
    /// size, accepts an unknown class, or lets a latest-state frame past
    /// the reliable cap. Oracle: each single corruption of a valid frame
    /// is refused with its own reason.
    #[test]
    fn single_corruptions_of_a_valid_frame_are_refused_with_the_right_reason() {
        let strategy = (delivery(), 1u64.., bytes(300), 1usize..4, 2u8..);
        check(strategy, |(delivery, sequence, payload, extra, class)| {
            let sequence = if delivery == Delivery::ReliableOrdered {
                0
            } else {
                sequence
            };
            let frame = encode_envelope(MAGIC, delivery, sequence, &payload).unwrap();

            let mut longer = frame.clone();
            longer.extend(std::iter::repeat_n(0, extra));
            prop_assert_eq!(
                decode_envelope(MAGIC, &longer),
                Err(EnvelopeError::LengthMismatch)
            );
            if !payload.is_empty() {
                let shorter = &frame[..frame.len() - 1];
                prop_assert_eq!(
                    decode_envelope(MAGIC, shorter),
                    Err(EnvelopeError::LengthMismatch)
                );
            }

            let mut unknown = frame.clone();
            unknown[4] = class;
            prop_assert_eq!(
                decode_envelope(MAGIC, &unknown),
                Err(EnvelopeError::UnknownDelivery)
            );

            let mut swapped = frame.clone();
            swapped[4] ^= 1;
            prop_assert_eq!(
                decode_envelope(MAGIC, &swapped),
                Err(EnvelopeError::InvalidSequence)
            );

            let mut huge = frame;
            let cap = match delivery {
                Delivery::ReliableOrdered => MAX_RELIABLE_MESSAGE_BYTES,
                Delivery::LatestState => MAX_LATEST_STATE_BYTES,
            };
            huge[13..17].copy_from_slice(&u32::try_from(cap + 1).unwrap().to_be_bytes());
            prop_assert_eq!(decode_envelope(MAGIC, &huge), Err(EnvelopeError::TooLarge));
            Ok(())
        });
    }

    /// Defect: the payload cap enforced after allocation or off by one.
    /// Oracle: exactly the cap encodes, one more byte is `TooLarge`.
    #[test]
    fn payload_caps_are_exact() {
        check(delivery(), |delivery| {
            let (sequence, cap) = match delivery {
                Delivery::ReliableOrdered => (0, MAX_RELIABLE_MESSAGE_BYTES),
                Delivery::LatestState => (1, MAX_LATEST_STATE_BYTES),
            };
            prop_assert!(encode_envelope(MAGIC, delivery, sequence, &vec![0; cap]).is_ok());
            prop_assert_eq!(
                encode_envelope(MAGIC, delivery, sequence, &vec![0; cap + 1]),
                Err(EnvelopeError::TooLarge)
            );
            Ok(())
        });
    }
}
