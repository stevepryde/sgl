//! The decoders against meshoptimizer's own test vectors: v1.3
//! `demo/tests.cpp`, whose encoded streams and expected outputs came from
//! upstream's encoder and decoder, not from this port.
use super::{filter, index, vertex};
use wasm_bindgen_test::wasm_bindgen_test;

/// `kIndexBufferTricky` and its v1 encoding `kIndexDataV1`, which uses a
/// `next` restart and `last`.
const TRICKY: [u32; 15] = [0, 1, 2, 2, 1, 3, 0, 1, 2, 2, 1, 5, 2, 1, 4];
const TRICKY_V1: [u8; 24] = [
    0xe1, 0xf0, 0x10, 0xfe, 0x1f, 0x3d, 0x00, 0x0a, 0x00, 0x76, 0x87, 0x56, 0x67, 0x78, 0xa9, 0x86,
    0x65, 0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
];
/// `decodeIndexV1More`: explicit indices in the 0xff path.
const MORE: [u32; 12] = [0, 1, 2, 2, 1, 3, 4, 6, 5, 7, 8, 9];
const MORE_V1: [u8; 27] = [
    0xe1, 0xf0, 0x10, 0xfe, 0xff, 0xf0, 0x0c, 0xff, 0x02, 0x02, 0x02, 0x00, 0x76, 0x87, 0x56, 0x67,
    0x78, 0xa9, 0x86, 0x65, 0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
];
/// `decodeIndexV1ThreeEdges`: edges further back in the FIFO.
const THREE_EDGES: [u32; 12] = [0, 1, 2, 1, 0, 3, 2, 1, 4, 0, 2, 5];
const THREE_EDGES_V1: [u8; 21] = [
    0xe1, 0xf0, 0x20, 0x30, 0x40, 0x00, 0x76, 0x87, 0x56, 0x67, 0x78, 0xa9, 0x86, 0x65, 0x89, 0x68,
    0x98, 0x01, 0x69, 0x00, 0x00,
];
/// `kIndexSequence` and `kIndexSequenceV1`.
const SEQUENCE: [u32; 6] = [0, 1, 51, 2, 49, 1000];
const SEQUENCE_V1: [u8; 13] = [
    0xd1, 0x00, 0x04, 0xcd, 0x01, 0x04, 0x07, 0x98, 0x1f, 0x00, 0x00, 0x00, 0x00,
];

/// `kVertexBuffer`: four `PV` elements of 12 bytes (three u16 positions,
/// two u8 normal components, two u16 texture coordinates).
fn vertex_buffer() -> Vec<u8> {
    let elements: [([u16; 3], [u8; 2], [u16; 2]); 4] = [
        ([0, 0, 0], [0, 0], [0, 0]),
        ([300, 0, 0], [0, 0], [500, 0]),
        ([0, 300, 0], [0, 0], [0, 500]),
        ([300, 300, 0], [0, 0], [500, 500]),
    ];
    let mut bytes = Vec::new();
    for (position, normal, uv) in elements {
        position.iter().for_each(|v| bytes.extend(v.to_le_bytes()));
        bytes.extend(normal);
        uv.iter().for_each(|v| bytes.extend(v.to_le_bytes()));
    }
    bytes
}
const VERTEX_V0: [u8; 85] = [
    0xa0, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x58, 0x57, 0x58, 0x01, 0x26, 0x00, 0x00, 0x00, 0x01, 0x0c,
    0x00, 0x00, 0x00, 0x58, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x3f, 0x00,
    0x00, 0x00, 0x17, 0x18, 0x17, 0x01, 0x26, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x00, 0x00, 0x00, 0x17,
    0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00,
];
const VERTEX_V1: [u8; 44] = [
    0xa1, 0xee, 0xaa, 0xee, 0x00, 0x4b, 0x4b, 0x4b, 0x00, 0x00, 0x4b, 0x00, 0x00, 0x7d, 0x7d, 0x7d,
    0x00, 0x00, 0x7d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x62, 0x00, 0x62,
];
/// `kVertexDataV1Custom`: every control mode, from a custom encoder.
const VERTEX_V1_CUSTOM: [u8; 103] = [
    0xa1, 0xd4, 0x94, 0xd4, 0x01, 0x0e, 0x00, 0x58, 0x57, 0x58, 0x02, 0x02, 0x12, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x0e, 0x00, 0x7d, 0x7d, 0x7d, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7d, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x62,
];

fn triangles(src: &[u8], count: usize, size: usize) -> Result<Vec<u32>, super::Error> {
    let mut out = vec![0; count * size];
    index::decode_triangles(&mut out, count, size, src)?;
    Ok(words(&out, size))
}

fn sequence(src: &[u8], count: usize, size: usize) -> Result<Vec<u32>, super::Error> {
    let mut out = vec![0; count * size];
    index::decode_sequence(&mut out, count, size, src)?;
    Ok(words(&out, size))
}

fn attributes(
    src: &[u8],
    count: usize,
    stride: usize,
    version: u8,
) -> Result<Vec<u8>, super::Error> {
    let mut out = vec![0; count * stride];
    vertex::decode(&mut out, count, stride, src, version)?;
    Ok(out)
}

fn words(bytes: &[u8], size: usize) -> Vec<u32> {
    bytes
        .chunks_exact(size)
        .map(|word| match *word {
            [a] => u32::from(a),
            [a, b] => u32::from(u16::from_le_bytes([a, b])),
            [a, b, c, d] => u32::from_le_bytes([a, b, c, d]),
            _ => unreachable!(),
        })
        .collect()
}

// Defects: an edge or vertex FIFO read at the wrong slot, `next` taken in
// the wrong order or not restarted, a varint misread, 16-bit output not
// truncated. Oracle: upstream's encodings and their source indices.
#[wasm_bindgen_test(unsupported = test)]
fn triangle_streams_decode_to_upstream_indices_at_both_widths() {
    for (stream, expected) in [
        (&TRICKY_V1[..], &TRICKY[..]),
        (&MORE_V1, &MORE),
        (&THREE_EDGES_V1, &THREE_EDGES),
    ] {
        for size in [2, 4] {
            assert_eq!(triangles(stream, expected.len(), size).unwrap(), expected);
        }
    }
}

// Defects: the baseline bit or zigzag misread. Oracle: upstream's encoding.
#[wasm_bindgen_test(unsupported = test)]
fn index_sequence_decodes_to_upstream_indices_at_both_widths() {
    for size in [2, 4] {
        assert_eq!(sequence(&SEQUENCE_V1, 6, size).unwrap(), SEQUENCE);
    }
}

// Defects: header bits, sentinels or byte lanes misread, block or tail
// boundaries wrong, a v1 control or channel mode misapplied. Oracle:
// upstream's encodings of `kVertexBuffer` and its expected buffers.
#[wasm_bindgen_test(unsupported = test)]
fn attribute_streams_decode_to_upstream_buffers() {
    for stream in [&VERTEX_V0[..], &VERTEX_V1, &VERTEX_V1_CUSTOM] {
        assert_eq!(attributes(stream, 4, 12, 1).unwrap(), vertex_buffer());
    }
    // `decodeVertexV0More`: 2- and 4-bit sentinels and raw bytes.
    let expected: Vec<u8> = (0..16u8).flat_map(|i| [0, i, 2 * i, 8 * i]).collect();
    let input = [
        &[0xa0, 0x00, 0x01, 0x2a, 0xaa, 0xaa, 0xaa, 0x02, 0x04][..],
        &[0x44; 7],
        &[0x03, 0x00],
        &[0x10; 15],
        &[0x00; 32],
    ]
    .concat();
    assert_eq!(attributes(&input, 16, 4, 0).unwrap(), expected);
    // `decodeVertexV0Mode2`: 4-bit deltas in every lane.
    let expected: Vec<u8> = (0..16u8)
        .flat_map(|i| [4 * i, 5 * i, 6 * i, 7 * i])
        .collect();
    let mut input = vec![0xa0];
    for byte in [0x88, 0xaa, 0xcc, 0xee] {
        input.extend([0x02, byte & 0x0f]);
        input.extend([byte; 7]);
    }
    input.extend([0; 32]);
    assert_eq!(attributes(&input, 16, 4, 0).unwrap(), expected);
    // `decodeVertexV1Deltas`: 16-bit channel deltas.
    let expected: [u16; 64] = [
        248, 248, 240, 240, 249, 250, 243, 244, 250, 252, 246, 248, 251, 254, 249, 252, 252, 256,
        252, 256, 253, 258, 255, 260, 254, 260, 258, 264, 255, 262, 261, 268, 256, 264, 264, 272,
        257, 262, 267, 268, 258, 260, 270, 264, 259, 258, 273, 260, 260, 256, 276, 256, 261, 254,
        279, 252, 262, 252, 282, 248, 263, 250, 285, 244,
    ];
    let input = [
        0xa1, 0x99, 0x99, 0x01, 0x2a, 0xaa, 0xaa, 0xaa, 0x02, 0x04, 0x44, 0x44, 0x44, 0x43, 0x33,
        0x33, 0x33, 0x02, 0x06, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x02, 0x08, 0x88, 0x88,
        0x88, 0x87, 0x77, 0x77, 0x77, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xf8, 0x00, 0xf8, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0x01, 0x01,
    ];
    let decoded = attributes(&input, 16, 8, 1).unwrap();
    assert_eq!(words(&decoded, 2), expected.map(u32::from));
}

// Defect: an `EXT_meshopt_compression` stream accepted in the version only
// `KHR_meshopt_compression` defines. Oracle: both specifications' headers.
#[wasm_bindgen_test(unsupported = test)]
fn version_one_attributes_need_the_khr_extension() {
    assert!(attributes(&VERTEX_V1, 4, 12, 0).is_err());
    assert_eq!(attributes(&VERTEX_V0, 4, 12, 0).unwrap(), vertex_buffer());
}

// Defects: a truncated stream decoded from bytes past its end, or a
// trailing byte, wrong header or invalid version accepted (upstream's
// `MemorySafe`, `RejectExtraBytes`, `RejectMalformedHeaders` and
// `RejectInvalidVersion` checks, and `decodeIndexMalformedVByte`). A
// panic fails the test as surely as a wrong result.
#[wasm_bindgen_test(unsupported = test)]
fn malformed_streams_are_errors() {
    type Decode = fn(&[u8]) -> bool;
    let cases: [(&[u8], Decode); 5] = [
        (&TRICKY_V1, |s| triangles(s, 15, 4).is_ok()),
        (&MORE_V1, |s| triangles(s, 12, 4).is_ok()),
        (&SEQUENCE_V1, |s| sequence(s, 6, 4).is_ok()),
        (&VERTEX_V0, |s| attributes(s, 4, 12, 1).is_ok()),
        (&VERTEX_V1, |s| attributes(s, 4, 12, 1).is_ok()),
    ];
    for (stream, decodes) in cases {
        assert!(decodes(stream));
        for length in 0..stream.len() {
            assert!(!decodes(&stream[..length]), "{length} of {}", stream.len());
        }
        assert!(!decodes(&[stream, &[0]].concat()));
        let mut broken = stream.to_vec();
        broken[0] = 0;
        assert!(!decodes(&broken));
        broken[0] = stream[0] | 0x0f;
        assert!(!decodes(&broken));
    }
    let vbyte = [
        &[0xe1, 0x20, 0x20, 0x20, 0xff][..],
        &[0x20; 19],
        &[0xff; 4],
        &[0x20; 11],
    ]
    .concat();
    assert!(triangles(&vbyte, 66, 4).is_err());
}

// Defect: a corrupt byte anywhere makes a decoder read out of bounds or
// panic. Every single-byte change of each vector must return, as a result
// or an error.
#[wasm_bindgen_test(unsupported = test)]
fn corrupt_streams_never_panic() {
    for stream in [
        &TRICKY_V1[..],
        &MORE_V1,
        &THREE_EDGES_V1,
        &SEQUENCE_V1,
        &VERTEX_V0,
        &VERTEX_V1,
        &VERTEX_V1_CUSTOM,
    ] {
        for at in 0..stream.len() {
            for value in [0x00, 0x0f, 0x7f, 0x80, 0xfe, 0xff, stream[at] ^ 0x55] {
                let mut broken = stream.to_vec();
                broken[at] = value;
                let _ = triangles(&broken, 15, 2);
                let _ = sequence(&broken, 6, 4);
                let _ = attributes(&broken, 4, 12, 1);
                let _ = attributes(&broken, 16, 4, 1);
            }
        }
    }
}

/// `count` four-component elements of `stride / 4` bytes each.
fn elements(values: &[i32], stride: usize) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| {
            if stride == 4 {
                vec![v as u8]
            } else {
                (v as u16).to_le_bytes().to_vec()
            }
        })
        .collect()
}

// Defects: the octahedral fold, scale or rounding wrong, or the fourth
// component changed. Oracle: upstream's `decodeFilterOct8` and
// `decodeFilterOct12` expected bytes.
#[wasm_bindgen_test(unsupported = test)]
fn octahedral_filter_matches_upstream() {
    let mut data = elements(
        &[
            0, 1, 127, 0, 0, 187, 127, 1, 255, 1, 127, 0, 14, 130, 127, 1,
        ],
        4,
    );
    filter::octahedral(&mut data, 4);
    assert_eq!(
        data,
        elements(
            &[0, 1, 127, 0, 0, 159, 82, 1, 255, 1, 127, 0, 1, 130, 241, 1],
            4
        )
    );
    let mut data = elements(
        &[
            0, 1, 2047, 0, 0, 1870, 2047, 1, 2017, 1, 2047, 0, 14, 1300, 2047, 1,
        ],
        8,
    );
    filter::octahedral(&mut data, 8);
    let expected = [
        0, 16, 32767, 0, 0, 32621, 3088, 1, 32764, 16, 471, 0, 307, 28541, 16093, 1,
    ];
    assert_eq!(data, elements(&expected, 8));
}

// Defects: the largest component's slot, the scale or w's reconstruction
// wrong. Oracle: upstream's `decodeFilterQuat12` expected values.
#[wasm_bindgen_test(unsupported = test)]
fn quaternion_filter_matches_upstream() {
    let mut data = elements(
        &[
            0, 1, 0, 0x7fc, 0, 1870, 0, 0x7fd, 2017, 1, 0, 0x7fe, 14, 1300, 0, 0x7ff,
        ],
        8,
    );
    filter::quaternion(&mut data);
    let expected = [
        32767, 0, 11, 0, 0, 25013, 0, 21166, 11, 0, 23504, 22830, 158, 14715, 0, 29277,
    ];
    assert_eq!(data, elements(&expected, 8));
}

// Defects: exponent or mantissa sign extension wrong. Oracle: upstream's
// `decodeFilterExp` expected floats.
#[wasm_bindgen_test(unsupported = test)]
fn exponential_filter_matches_upstream() {
    let mut data: Vec<u8> = [0u32, 0xff000003, 0x02fffff7, 0xfe7fffff]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    filter::exponential(&mut data);
    assert_eq!(
        words(&data, 4),
        [0, 0x3fc00000, 0xc2100000, 0x49fffffe] // 0, 1.5, -36, 2097151
    );
}

// Defects: YCoCg reconstruction, the alpha-derived scale or the alpha
// widening wrong. Oracle: upstream's `encodeFilterColor8` and
// `encodeFilterColor12` encodings of four colours, which decode to those
// colours within its stated tolerance, greys staying grey.
#[wasm_bindgen_test(unsupported = test)]
fn color_filter_recovers_upstream_colours() {
    let colours = [
        [1.0f32, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 0.5],
        [0.0, 0.0, 1.0, 0.25],
        [0.4, 0.4, 0.4, 0.75],
    ];
    let encoded8 = [
        0x40, 0x7f, 0xc1, 0xff, 0x7f, 0x00, 0x7f, 0xc0, 0x40, 0x81, 0xc0, 0xa0, 0x66, 0x00, 0x00,
        0xdf,
    ];
    let encoded12 = [
        0x0400, 0x07ff, 0xfc01, 0x0fff, 0x07ff, 0x0000, 0x07ff, 0x0c00, 0x0400, 0xf801, 0xfc00,
        0x0a00, 0x0666, 0x0000, 0x0000, 0x0dff,
    ];
    for (stride, encoded, max, tolerance) in [
        (4, &encoded8[..], 255., 1e-2),
        (8, &encoded12[..], 65535., 1e-3),
    ] {
        let mut data = elements(encoded, stride);
        filter::color(&mut data, stride);
        let decoded = words(&data, stride / 4);
        for (value, expected) in decoded.iter().zip(colours.as_flattened()) {
            assert!(
                (*value as f32 / max - expected).abs() < tolerance,
                "{decoded:?}"
            );
        }
        assert!(decoded[12] == decoded[13] && decoded[13] == decoded[14]);
    }
}

/// The elements `tests/fixtures/meshopt/encode.mjs` authors: 4 bytes each.
fn stride4(count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|i| [i as u8, (i >> 2) as u8, ((i * 151 + 7) % 251) as u8, 9])
        .collect()
}

/// 64 bytes each: sixteen 4-byte channels, by channel `c % 4` a u32
/// counter, two u16 ramps, an f32 exact in binary and four bytes.
fn stride64(count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..count {
        for c in 0..16usize {
            match c % 4 {
                0 => out.extend((i as u32 * 1000 + c as u32).to_le_bytes()),
                1 => {
                    out.extend((i as u16 * 3 + c as u16).to_le_bytes());
                    out.extend((60000 - i as u16 * 5).to_le_bytes());
                }
                2 => out.extend((i as f32 * 0.25 + c as f32).to_le_bytes()),
                _ => out.extend([(i * c) as u8, (i % 7) as u8, 255 - i as u8, c as u8]),
            }
        }
    }
    out
}

// Defects: `last` reset or the read position lost between blocks, the
// block size wrong where it falls below 256 elements (stride 64: 128), a
// short final block misread, or the XOR channel's rotation misapplied
// (the version 1 stride-64 stream uses it). Oracle: the elements the
// fixture script authored, encoded by meshoptimizer's own encoder.
#[wasm_bindgen_test(unsupported = test)]
fn streams_of_several_blocks_decode_to_the_authored_elements() {
    let fixture = |name: &str| -> &'static [u8] {
        match name {
            "stride4-v0" => include_bytes!("../../../tests/fixtures/meshopt/stride4-v0.bin"),
            "stride4-v1" => include_bytes!("../../../tests/fixtures/meshopt/stride4-v1.bin"),
            "stride64-v0" => include_bytes!("../../../tests/fixtures/meshopt/stride64-v0.bin"),
            _ => include_bytes!("../../../tests/fixtures/meshopt/stride64-v1.bin"),
        }
    };
    for version in [0, 1] {
        let name = format!("stride4-v{version}");
        assert_eq!(
            attributes(fixture(&name), 1000, 4, 1).unwrap(),
            stride4(1000),
            "{name}"
        );
        let name = format!("stride64-v{version}");
        assert_eq!(
            attributes(fixture(&name), 300, 64, 1).unwrap(),
            stride64(300),
            "{name}"
        );
    }
}

// Defect: 2-bit sentinels miscounted, including in the last lane of a
// 13-element group. Oracle: upstream's `decodeVertexBitGroupSentinelCount`.
#[wasm_bindgen_test(unsupported = test)]
fn sentinel_count_matches_upstream() {
    let input = [
        &[0xa1, 0xa9, 0x01, 0xfc, 0x3c, 0xcc, 0xcc][..],
        &[0x01; 9],
        &[0x00; 24],
    ]
    .concat();
    let expected: Vec<u8> = [
        0xff, 0xfe, 0xfd, 0xfd, 0xfd, 0xfc, 0xfb, 0xfb, 0xfa, 0xfa, 0xf9, 0xf9, 0xf8,
    ]
    .iter()
    .flat_map(|&b| [b, 0, 0, 0])
    .collect();
    assert_eq!(attributes(&input, 13, 4, 1).unwrap(), expected);
}

// Defect: a triangle stream that reads a FIFO entry never written, or has
// a lookup table breaking its two limits, decodes into indices (upstream's
// all-ones placeholder) rather than failing. Oracle: KHR Appendix A, mode
// 1: such streams are invalid.
#[wasm_bindgen_test(unsupported = test)]
fn triangle_streams_reading_unwritten_fifo_entries_are_errors() {
    // The first triangle reads edge 0 before any edge was pushed.
    assert!(triangles(&[[0xe1, 0x01].as_slice(), &[0; 16]].concat(), 3, 4).is_err());
    // A recent vertex (Y = 12) read when only three have been pushed.
    let mut stream = TRICKY_V1.to_vec();
    stream[2] = 0x0c;
    assert!(triangles(&stream, 15, 4).is_err());
    // A table whose last bytes are not zero, or with a 0xf nibble.
    for (at, value) in [(TRICKY_V1.len() - 1, 1), (TRICKY_V1.len() - 16, 0x0f)] {
        let mut stream = TRICKY_V1.to_vec();
        stream[at] = value;
        assert!(triangles(&stream, 15, 4).is_err(), "{at}");
    }
}
