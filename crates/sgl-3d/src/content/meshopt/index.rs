//! The triangle (mode 1) and index sequence (mode 2) streams: meshoptimizer
//! v1.3 `src/indexcodec.cpp` (`meshopt_decodeIndexBuffer`,
//! `meshopt_decodeIndexSequence`, `decodeVByte`, `decodeIndex`). See
//! README.md.
use super::{Error, Result};

/// Both extensions define version 1 of each stream only.
const TRIANGLE_HEADER: u8 = 0xe1;
const SEQUENCE_HEADER: u8 = 0xd1;
/// The triangle stream's lookup table, and the index sequence's tail.
const TRIANGLE_TAIL: usize = 16;
const SEQUENCE_TAIL: usize = 4;

/// Writes `indices` as `size`-byte little-endian integers (2 or 4),
/// truncating to 16 bits for 2, in order.
struct Output<'a> {
    bytes: std::slice::ChunksExactMut<'a, u8>,
}

impl Output<'_> {
    fn push(&mut self, index: u32) {
        if let Some(out) = self.bytes.next() {
            if let Ok(out) = <&mut [u8; 2]>::try_from(&mut *out) {
                *out = (index as u16).to_le_bytes();
            } else if let Ok(out) = <&mut [u8; 4]>::try_from(out) {
                *out = index.to_le_bytes();
            }
        }
    }
}

fn output(dst: &mut [u8], count: usize, size: usize) -> Result<Output<'_>> {
    if !matches!(size, 2 | 4) {
        return Err(Error("index stride must be 2 or 4"));
    }
    if count.checked_mul(size) != Some(dst.len()) {
        return Err(Error("index output is not count times stride bytes"));
    }
    Ok(Output {
        bytes: dst.chunks_exact_mut(size),
    })
}

/// A byte of `data` at `position`, advancing it.
fn byte(data: &[u8], position: &mut usize) -> Result<u8> {
    let byte = *data
        .get(*position)
        .ok_or(Error("index stream is truncated"))?;
    *position += 1;
    Ok(byte)
}

/// An unsigned LEB128 value of one to five bytes (`decodeVByte`). Upstream
/// stops after five bytes whatever the fifth holds; the specification
/// makes a fifth byte with its continuation bit set invalid.
fn varint(data: &[u8], position: &mut usize) -> Result<u32> {
    let mut value = 0u32;
    for group in 0..5 {
        let byte = byte(data, position)?;
        value |= u32::from(byte & 0x7f) << (7 * group);
        if byte < 0x80 {
            return Ok(value);
        }
    }
    Err(Error("index stream has a varint longer than five bytes"))
}

/// `last` moved by the zigzag delta `value` (`decodeIndex`).
fn delta(value: u32, last: u32) -> u32 {
    last.wrapping_add((value >> 1) ^ (value & 1).wrapping_neg())
}

/// A 16-entry FIFO (`EdgeFifo`, `VertexFifo`) that refuses to read an
/// entry never written, which KHR Appendix A makes a stream invalid;
/// upstream reads its all-ones fill instead.
struct Fifo<T> {
    entries: [T; 16],
    offset: usize,
    written: usize,
}

impl<T: Copy> Fifo<T> {
    fn new(fill: T) -> Self {
        Self {
            entries: [fill; 16],
            offset: 0,
            written: 0,
        }
    }

    /// Stores `value` at the next slot, which `advance` takes, as upstream's
    /// conditional push does.
    fn push(&mut self, value: T, advance: bool) {
        self.entries[self.offset] = value;
        if advance {
            self.offset = (self.offset + 1) & 15;
            self.written = (self.written + 1).min(16);
        }
    }

    /// The entry `distance` back from the latest (0).
    fn read(&self, distance: usize) -> Result<T> {
        if distance >= self.written {
            return Err(Error("triangle stream reads a FIFO entry never written"));
        }
        Ok(self.entries[self.offset.wrapping_sub(1 + distance) & 15])
    }
}

/// Decodes a triangle list of `count` indices (a multiple of 3) of `size`
/// bytes each from `src` into `dst` (`meshopt_decodeIndexBuffer`).
pub(in crate::content) fn decode_triangles(
    dst: &mut [u8],
    count: usize,
    size: usize,
    src: &[u8],
) -> Result<()> {
    let mut out = output(dst, count, size)?;
    if !count.is_multiple_of(3) {
        return Err(Error("triangle index count is not a multiple of 3"));
    }
    let triangles = count / 3;
    // The shortest stream: header, a code a triangle and the table.
    if src.len() < 1 + triangles + TRIANGLE_TAIL {
        return Err(Error("triangle stream is shorter than its codes and table"));
    }
    if src[0] != TRIANGLE_HEADER {
        return Err(Error("triangle stream header is not 0xe1"));
    }
    let codes = &src[1..=triangles];
    // Triangle data ends where the 16-byte table begins. The table's last
    // two bytes are zero and no nibble of it is 0xf (KHR Appendix A).
    let safe_end = src.len() - TRIANGLE_TAIL;
    let table = &src[safe_end..];
    if table[14..] != [0, 0] || table.iter().any(|&b| b >> 4 == 15 || b & 15 == 15) {
        return Err(Error("triangle stream's lookup table breaks its limits"));
    }
    let mut edges = Fifo::new([0u32; 2]);
    let mut vertices = Fifo::new(0u32);
    let (mut next, mut last) = (0u32, 0u32);
    let mut position = 1 + triangles;
    for &code in codes {
        let (high, low) = (usize::from(code >> 4), usize::from(code & 15));
        if code < 0xf0 {
            // A recent edge, and a third vertex that is `next`, recent, or
            // explicit.
            let [a, b] = edges.read(high)?;
            let c = if low < 13 {
                let c = if low == 0 { next } else { vertices.read(low)? };
                next = next.wrapping_add(u32::from(low == 0));
                vertices.push(c, low == 0);
                c
            } else {
                // Upstream checks the worst-case read once a triangle.
                if position > safe_end {
                    return Err(Error("triangle stream is truncated"));
                }
                // 13 and 14 are `last` -1 and +1; 15 an explicit index.
                last = match low {
                    13 => last.wrapping_sub(1),
                    14 => last.wrapping_add(1),
                    _ => delta(varint(src, &mut position)?, last),
                };
                vertices.push(last, true);
                last
            };
            edges.push([c, b], true);
            edges.push([a, c], true);
            out.push(a);
            out.push(b);
            out.push(c);
        } else {
            // Three vertices, each `next`, recent or explicit, described by
            // a table entry (0xf0..=0xfd) or a byte of data (0xfe, 0xff).
            let (aux, explicit) = if code < 0xfe {
                (table[low], false)
            } else {
                if position > safe_end {
                    return Err(Error("triangle stream is truncated"));
                }
                let aux = byte(src, &mut position)?;
                // A zero byte restarts the `next` sequence.
                if aux == 0 {
                    next = 0;
                }
                (aux, true)
            };
            let explicit_a = code == 0xff;
            let (fb, fc) = (usize::from(aux >> 4), usize::from(aux & 15));
            // Every `next` is taken before any explicit index is decoded,
            // as the encoder does. An explicit field (15) is decoded below.
            let mut take_next = || {
                next = next.wrapping_add(1);
                next.wrapping_sub(1)
            };
            let mut a = if explicit_a { 0 } else { take_next() };
            let mut pick = |field: usize| match field {
                0 => Ok(take_next()),
                15 if explicit => Ok(0),
                field => vertices.read(field - 1),
            };
            let mut b = pick(fb)?;
            let mut c = pick(fc)?;
            if explicit_a {
                last = delta(varint(src, &mut position)?, last);
                a = last;
            }
            if explicit && fb == 15 {
                last = delta(varint(src, &mut position)?, last);
                b = last;
            }
            if explicit && fc == 15 {
                last = delta(varint(src, &mut position)?, last);
                c = last;
            }
            out.push(a);
            out.push(b);
            out.push(c);
            vertices.push(a, true);
            vertices.push(b, fb == 0 || (explicit && fb == 15));
            vertices.push(c, fc == 0 || (explicit && fc == 15));
            edges.push([b, a], true);
            edges.push([c, b], true);
            edges.push([a, c], true);
        }
    }
    if position != safe_end {
        return Err(Error("triangle stream has bytes left before its table"));
    }
    Ok(())
}

/// Decodes `count` indices of `size` bytes each, deltas from two
/// baselines, from `src` into `dst` (`meshopt_decodeIndexSequence`).
pub(in crate::content) fn decode_sequence(
    dst: &mut [u8],
    count: usize,
    size: usize,
    src: &[u8],
) -> Result<()> {
    let mut out = output(dst, count, size)?;
    // The shortest stream: header, a byte an index and the tail.
    if count
        .checked_add(1 + SEQUENCE_TAIL)
        .is_none_or(|shortest| src.len() < shortest)
    {
        return Err(Error("index sequence is shorter than its indices and tail"));
    }
    if src[0] != SEQUENCE_HEADER {
        return Err(Error("index sequence header is not 0xd1"));
    }
    let safe_end = src.len() - SEQUENCE_TAIL;
    let mut last = [0u32; 2];
    let mut position = 1;
    for _ in 0..count {
        if position >= safe_end {
            return Err(Error("index sequence is truncated"));
        }
        let value = varint(src, &mut position)?;
        let baseline = &mut last[(value & 1) as usize];
        *baseline = delta(value >> 1, *baseline);
        out.push(*baseline);
    }
    if position != safe_end {
        return Err(Error("index sequence has bytes left before its tail"));
    }
    Ok(())
}
