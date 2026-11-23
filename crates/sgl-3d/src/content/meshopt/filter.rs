//! The attribute filters, applied in place to decoded elements:
//! meshoptimizer v1.3 `src/vertexfilter.cpp`, the scalar `decodeFilterOct`,
//! `decodeFilterQuat`, `decodeFilterExp` and `decodeFilterColor`. Each
//! converts as upstream does, in `f32`, rounding by adding a half and
//! truncating. Values outside a filter's valid range give unspecified
//! results, as both specifications allow, never a panic: Rust's float to
//! integer casts saturate where C++'s are undefined. See README.md.

/// Unit vectors from octahedral coordinates: four signed components of
/// `stride / 4` bytes each (`decodeFilterOct`).
pub(in crate::content) fn octahedral(data: &mut [u8], stride: usize) {
    if stride == 4 {
        for element in data.chunks_exact_mut(4) {
            let input = [0, 1, 2].map(|c| f32::from(element[c] as i8));
            let output = octahedral_element(input, 127.);
            for (c, value) in output.into_iter().enumerate() {
                element[c] = value as i8 as u8;
            }
        }
    } else {
        for element in data.chunks_exact_mut(8) {
            let input =
                [0, 2, 4].map(|c| f32::from(i16::from_le_bytes([element[c], element[c + 1]])));
            let output = octahedral_element(input, 32767.);
            for (c, value) in output.into_iter().enumerate() {
                element[2 * c..2 * c + 2].copy_from_slice(&(value as i16).to_le_bytes());
            }
        }
    }
}

fn octahedral_element([x, y, one]: [f32; 3], max: f32) -> [i32; 3] {
    // z from x and y, assuming the third component encodes 1.0 at the
    // same bit count.
    let (mut x, mut y) = (x, y);
    let z = one - x.abs() - y.abs();
    // Fold the lower hemisphere.
    let t = if z >= 0. { 0. } else { z };
    x += if x >= 0. { t } else { -t };
    y += if y >= 0. { t } else { -t };
    let s = max / (x * x + y * y + z * z).sqrt();
    [x, y, z].map(|v| (v * s + if v >= 0. { 0.5 } else { -0.5 }) as i32)
}

/// Unit quaternions from their three smallest components and the index of
/// the largest: four 16-bit components (`decodeFilterQuat`).
pub(in crate::content) fn quaternion(data: &mut [u8]) {
    let scale = 32767. / 2f32.sqrt();
    for element in data.chunks_exact_mut(8) {
        let component = |c: usize| i16::from_le_bytes([element[2 * c], element[2 * c + 1]]);
        let last = component(3);
        // The scale from the high bits of the last component.
        let s = f32::from(last | 3);
        let [x, y, z] = [0, 1, 2].map(|c| f32::from(component(c)));
        // w, unscaled; clamped to avoid a NaN from precision error.
        let ww = s * s * 2. - x * x - y * y - z * z;
        let w = (if ww >= 0. { ww } else { 0. }).sqrt();
        let ss = scale / s;
        let round = |v: f32| (v * ss + if v >= 0. { 0.5 } else { -0.5 }) as i32 as i16;
        let largest = (last & 3) as usize;
        let mut output = [0i16; 4];
        output[(largest + 1) & 3] = round(x);
        output[(largest + 2) & 3] = round(y);
        output[(largest + 3) & 3] = round(z);
        output[largest] = (w * ss + 0.5) as i32 as i16;
        for (c, value) in output.into_iter().enumerate() {
            element[2 * c..2 * c + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
}

/// Floats from an 8-bit signed exponent over a 24-bit signed mantissa
/// (`decodeFilterExp`), each 32-bit word on its own.
pub(in crate::content) fn exponential(data: &mut [u8]) {
    for word in data.chunks_exact_mut(4) {
        let v = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        let mantissa = ((v << 8) as i32) >> 8;
        let exponent = (v as i32) >> 24;
        // 2^exponent built directly, as upstream's `ldexp` shortcut does.
        let scale = f32::from_bits((exponent.wrapping_add(127) as u32) << 23);
        word.copy_from_slice(&(scale * mantissa as f32).to_bits().to_le_bytes());
    }
}

/// RGBA from YCoCg with an alpha whose top set bit gives the precision:
/// four unsigned components of `stride / 4` bytes each
/// (`decodeFilterColor`).
pub(in crate::content) fn color(data: &mut [u8], stride: usize) {
    if stride == 4 {
        for element in data.chunks_exact_mut(4) {
            let input = [
                i32::from(element[0]),
                i32::from(element[1] as i8),
                i32::from(element[2] as i8),
                i32::from(element[3]),
            ];
            for (c, value) in color_element(input, 255.).into_iter().enumerate() {
                element[c] = value as u8;
            }
        }
    } else {
        for element in data.chunks_exact_mut(8) {
            let word = |c: usize| u16::from_le_bytes([element[2 * c], element[2 * c + 1]]);
            let input = [
                i32::from(word(0)),
                i32::from(word(1) as i16),
                i32::from(word(2) as i16),
                i32::from(word(3)),
            ];
            for (c, value) in color_element(input, 65535.).into_iter().enumerate() {
                element[2 * c..2 * c + 2].copy_from_slice(&(value as u16).to_le_bytes());
            }
        }
    }
}

fn color_element([y, co, cg, alpha]: [i32; 4], max: f32) -> [i32; 4] {
    // All ones up to the alpha's top set bit.
    let mut scale = alpha;
    scale |= scale >> 1;
    scale |= scale >> 2;
    scale |= scale >> 4;
    scale |= scale >> 8;
    let r = y + co - cg;
    let g = y + cg;
    let b = y - co - cg;
    // Alpha widened by a bit to match the others, repeating its lowest.
    let a = ((alpha << 1) & scale) | (alpha & 1);
    let ss = max / scale as f32;
    [r, g, b, a].map(|v| (v as f32 * ss + 0.5) as i32)
}
