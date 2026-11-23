//! The attribute stream (mode 0): meshoptimizer v1.3 `src/vertexcodec.cpp`,
//! the scalar decoder (`decodeBytesGroup`, `decodeBytes`, `decodeDeltas1`,
//! `decodeVertexBlock`, `meshopt_decodeVertexBuffer`). See README.md.
use super::{Error, Result};

/// The bytes of attribute data one block covers at most, and its element
/// cap (`kVertexBlockSizeBytes`, `kVertexBlockMaxSize`): part of the format.
const BLOCK_BYTES: usize = 8192;
const BLOCK_MAX_ELEMENTS: usize = 256;
/// Elements per group, and the bytes a group may read
/// (`kByteGroupSize`, `kByteGroupDecodeLimit`).
const GROUP: usize = 16;
const GROUP_DECODE_LIMIT: usize = 24;
/// The tail's minimum size per version (`kTailMinSizeV0`, `kTailMinSizeV1`).
const TAIL_MIN: [usize; 2] = [32, 24];
/// Bits per delta for each header value (`kBitsV0`, and `kBitsV1` from
/// the byte's control mode).
const BITS_V0: [u8; 4] = [0, 2, 4, 8];
const BITS_V1: [u8; 5] = [0, 1, 2, 4, 8];

/// The elements a block holds for elements of `stride` bytes
/// (`getVertexBlockSize`).
fn block_elements(stride: usize) -> usize {
    ((BLOCK_BYTES / stride) & !(GROUP - 1)).min(BLOCK_MAX_ELEMENTS)
}

/// Decodes `count` elements of `stride` bytes from `src` into `dst`
/// (`count * stride` bytes), accepting stream versions up to
/// `max_version`: 0 for `EXT_meshopt_compression`, 1 for
/// `KHR_meshopt_compression`. The stream must end exactly at its tail.
pub(in crate::content) fn decode(
    dst: &mut [u8],
    count: usize,
    stride: usize,
    src: &[u8],
    max_version: u8,
) -> Result<()> {
    if stride == 0 || stride > 256 || !stride.is_multiple_of(4) {
        return Err(Error(
            "attribute stride must be a multiple of 4 from 4 to 256",
        ));
    }
    if count.checked_mul(stride) != Some(dst.len()) {
        return Err(Error("attribute output is not count times stride bytes"));
    }
    let (&header, data) = src.split_first().ok_or(Error("empty attribute stream"))?;
    if header & 0xf0 != 0xa0 {
        return Err(Error("attribute stream header is not 0xa0 or 0xa1"));
    }
    let version = header & 0x0f;
    if version > max_version {
        return Err(Error(
            "attribute stream version is not one this extension allows",
        ));
    }
    let v1 = version == 1;
    let tail_size = stride + if v1 { stride / 4 } else { 0 };
    let tail_padded = tail_size.max(TAIL_MIN[usize::from(version)]);
    if data.len() < tail_padded {
        return Err(Error("attribute stream is shorter than its tail"));
    }
    let tail = &data[data.len() - tail_size..];
    let mut last = [0u8; 256];
    last[..stride].copy_from_slice(&tail[..stride]);
    let channels = v1.then(|| &tail[stride..]);
    let block = block_elements(stride);
    let mut position = 0;
    for out in dst.chunks_mut(block * stride) {
        decode_block(data, &mut position, out, stride, &mut last, channels)?;
    }
    if data.len() - position != tail_padded {
        return Err(Error("attribute stream has bytes left before its tail"));
    }
    Ok(())
}

/// One block of `out.len() / stride` elements (`decodeVertexBlock`),
/// decoded straight into `out` (upstream's `MESHOPTIMIZER_VERTEXCODEC_ZEROCOPY`
/// layout). `channels` is the version 1 tail's channel modes.
fn decode_block(
    data: &[u8],
    position: &mut usize,
    out: &mut [u8],
    stride: usize,
    last: &mut [u8; 256],
    channels: Option<&[u8]>,
) -> Result<()> {
    let count = out.len() / stride;
    let aligned = count.next_multiple_of(GROUP);
    // Four byte lanes of at most 256 elements; each lane's decode writes
    // its aligned count, spilling into the next lane before that lane is
    // decoded, as upstream's `buffer[kVertexBlockMaxSize * 4]` does.
    let mut lanes = [0u8; BLOCK_MAX_ELEMENTS * 4];
    let control = match channels {
        Some(_) => take(data, position, stride / 4)?,
        None => &[],
    };
    for k in (0..stride).step_by(4) {
        let control = control.get(k / 4).copied().unwrap_or(0);
        for j in 0..4 {
            let lane = &mut lanes[j * count..j * count + aligned];
            match (control >> (j * 2)) & 3 {
                // Literal bytes.
                3 => lane[..count].copy_from_slice(take(data, position, count)?),
                // All deltas zero.
                2 => lane[..count].fill(0),
                mode => {
                    let bits = match channels {
                        Some(_) => &BITS_V1[usize::from(mode)..usize::from(mode) + 4],
                        None => &BITS_V0[..],
                    };
                    decode_bytes(data, position, lane, bits)?;
                }
            }
        }
        let channel = channels.map_or(0, |channels| channels[k / 4]);
        // The low nibble is the channel mode; only the XOR mode uses the
        // high nibble (its rotation). Upstream masks the mode with 3 and
        // ignores a stray high nibble, which the specification makes invalid.
        match (channel & 0x0f, channel >> 4) {
            (0, 0) => deltas_u8(&lanes, count, out, stride, k, last),
            (1, 0) => deltas_u16(&lanes, count, out, stride, k, last),
            (2, rotation) => deltas_xor(&lanes, count, out, stride, k, last, rotation),
            _ => return Err(Error("attribute stream has an invalid channel mode")),
        }
    }
    Ok(())
}

/// `length` bytes from `data` at `position`, advancing it.
fn take<'a>(data: &'a [u8], position: &mut usize, length: usize) -> Result<&'a [u8]> {
    let end = position
        .checked_add(length)
        .ok_or(Error("attribute stream is truncated"))?;
    let bytes = data
        .get(*position..end)
        .ok_or(Error("attribute stream is truncated"))?;
    *position = end;
    Ok(bytes)
}

/// One byte lane's groups (`decodeBytes`): two header bits a group, then
/// each group's deltas at the bits its header value selects.
fn decode_bytes(data: &[u8], position: &mut usize, lane: &mut [u8], bits: &[u8]) -> Result<()> {
    let groups = lane.len() / GROUP;
    let header = take(data, position, groups.div_ceil(4))?;
    for (group, out) in lane.as_chunks_mut::<GROUP>().0.iter_mut().enumerate() {
        // Upstream requires this many bytes before decoding a group, which
        // covers the most a group reads.
        let window: &[u8; GROUP_DECODE_LIMIT] = data
            .get(*position..*position + GROUP_DECODE_LIMIT)
            .and_then(|window| window.try_into().ok())
            .ok_or(Error("attribute stream is truncated"))?;
        let mode = (header[group / 4] >> ((group % 4) * 2)) & 3;
        *position += decode_group(window, out, bits[usize::from(mode)]);
    }
    Ok(())
}

/// One group of 16 deltas at `bits` each (`decodeBytesGroup`), returning
/// the bytes it read. A delta of all ones is a sentinel: its value is the
/// next byte after the packed ones. At most 16 sentinels follow 8 packed
/// bytes, so every read stays within the 24-byte window.
fn decode_group(window: &[u8; GROUP_DECODE_LIMIT], out: &mut [u8; GROUP], bits: u8) -> usize {
    match bits {
        0 => {
            *out = [0; GROUP];
            0
        }
        1 => packed::<1>(window, out),
        2 => packed::<2>(window, out),
        4 => packed::<4>(window, out),
        _ => {
            out.copy_from_slice(&window[..GROUP]);
            GROUP
        }
    }
}

/// Deltas packed `BITS` to a byte, with sentinel escapes after them. 1-bit
/// deltas are stored from the least significant bit, 2- and 4-bit ones
/// from the most significant.
#[inline]
fn packed<const BITS: usize>(window: &[u8; GROUP_DECODE_LIMIT], out: &mut [u8; GROUP]) -> usize {
    let per_byte = 8 / BITS;
    let sentinel = ((1u16 << BITS) - 1) as u8;
    // Before element m's escape, at most m sentinels have been read after
    // the 16 / per_byte packed bytes: at most 23.
    let mut extra = GROUP / per_byte;
    for (m, out) in out.iter_mut().enumerate() {
        let shift = if BITS == 1 {
            m % 8
        } else {
            8 - BITS * (m % per_byte + 1)
        };
        let delta = (window[m / per_byte] >> shift) & sentinel;
        let escaped = delta == sentinel;
        *out = if escaped { window[extra] } else { delta };
        extra += usize::from(escaped);
    }
    extra
}

/// Undoes byte zigzag deltas into lane positions `k..k + 4` of each element
/// (`decodeDeltas1<unsigned char, false>`).
fn deltas_u8(
    lanes: &[u8],
    count: usize,
    out: &mut [u8],
    stride: usize,
    k: usize,
    last: &mut [u8; 256],
) {
    let lane = |j: usize| &lanes[j * count..(j + 1) * count];
    let (l0, l1, l2, l3) = (lane(0), lane(1), lane(2), lane(3));
    let mut previous = [last[k], last[k + 1], last[k + 2], last[k + 3]];
    let unzigzag = |d: u8| (d >> 1) ^ (d & 1).wrapping_neg();
    for (i, element) in out.chunks_exact_mut(stride).enumerate() {
        previous = [
            previous[0].wrapping_add(unzigzag(l0[i])),
            previous[1].wrapping_add(unzigzag(l1[i])),
            previous[2].wrapping_add(unzigzag(l2[i])),
            previous[3].wrapping_add(unzigzag(l3[i])),
        ];
        element[k..k + 4].copy_from_slice(&previous);
    }
    last[k..k + 4].copy_from_slice(&previous);
}

/// Undoes 16-bit zigzag deltas (`decodeDeltas1<unsigned short, false>`).
fn deltas_u16(
    lanes: &[u8],
    count: usize,
    out: &mut [u8],
    stride: usize,
    k: usize,
    last: &mut [u8; 256],
) {
    let lane = |j: usize| &lanes[j * count..(j + 1) * count];
    let (l0, l1, l2, l3) = (lane(0), lane(1), lane(2), lane(3));
    let mut low = u16::from_le_bytes([last[k], last[k + 1]]);
    let mut high = u16::from_le_bytes([last[k + 2], last[k + 3]]);
    let unzigzag = |d: u16| (d >> 1) ^ (d & 1).wrapping_neg();
    for (i, element) in out.chunks_exact_mut(stride).enumerate() {
        low = low.wrapping_add(unzigzag(u16::from_le_bytes([l0[i], l1[i]])));
        high = high.wrapping_add(unzigzag(u16::from_le_bytes([l2[i], l3[i]])));
        let [a, b] = low.to_le_bytes();
        let [c, d] = high.to_le_bytes();
        element[k..k + 4].copy_from_slice(&[a, b, c, d]);
    }
    let [a, b] = low.to_le_bytes();
    let [c, d] = high.to_le_bytes();
    last[k..k + 4].copy_from_slice(&[a, b, c, d]);
}

/// Undoes 32-bit XOR deltas, each rotated left by `(32 - rotation) & 31`
/// first (`decodeDeltas1<unsigned int, true>`).
fn deltas_xor(
    lanes: &[u8],
    count: usize,
    out: &mut [u8],
    stride: usize,
    k: usize,
    last: &mut [u8; 256],
    rotation: u8,
) {
    let lane = |j: usize| &lanes[j * count..(j + 1) * count];
    let (l0, l1, l2, l3) = (lane(0), lane(1), lane(2), lane(3));
    let rotation = (32 - u32::from(rotation)) & 31;
    let mut previous = u32::from_le_bytes([last[k], last[k + 1], last[k + 2], last[k + 3]]);
    for (i, element) in out.chunks_exact_mut(stride).enumerate() {
        let delta = u32::from_le_bytes([l0[i], l1[i], l2[i], l3[i]]);
        previous ^= delta.rotate_left(rotation);
        element[k..k + 4].copy_from_slice(&previous.to_le_bytes());
    }
    last[k..k + 4].copy_from_slice(&previous.to_le_bytes());
}
