//! Block-compressed KTX2 files and BC7 mode 6 blocks, as the KTX 2.0 and BC7
//! specifications lay them out. The `export_grate` example and sgl-3d's
//! tests include this file by path; it needs only the `ktx2` and `ruzstd`
//! crates.

/// A KTX2 file of one 2D image of `size` texels in block format `format`
/// holding `levels`, level 0 first, as the KTX 2.0 specification lays it
/// out: the header, the level index, the data format descriptor, then the
/// levels from the smallest to the largest, each aligned to 16 bytes unless
/// `zstd` supercompresses them with Zstandard.
pub(crate) fn ktx2(
    format: ktx2::Format,
    [width, height]: [u32; 2],
    levels: &[Vec<u8>],
    zstd: bool,
) -> Vec<u8> {
    use ktx2::{Header, Index, LevelIndex, SupercompressionScheme, dfd};
    let (basic, type_size) = dfd::Basic::from_format(format).unwrap();
    let block = dfd::Block::Basic(basic).to_vec();
    let index_at = Header::LENGTH;
    let dfd_at = index_at + levels.len() * LevelIndex::LENGTH;
    let dfd_length = 4 + block.len();
    let mut file = vec![0; dfd_at];
    file.extend_from_slice(&(dfd_length as u32).to_le_bytes());
    file.extend_from_slice(&block);
    let mut index = vec![None; levels.len()];
    for (level, texels) in levels.iter().enumerate().rev() {
        let stored = if zstd {
            ruzstd::encoding::compress_to_vec(
                &texels[..],
                ruzstd::encoding::CompressionLevel::Fastest,
            )
        } else {
            file.resize(file.len().next_multiple_of(16), 0);
            texels.clone()
        };
        index[level] = Some(LevelIndex {
            byte_offset: file.len() as u64,
            byte_length: stored.len() as u64,
            uncompressed_byte_length: texels.len() as u64,
        });
        file.extend_from_slice(&stored);
    }
    let header = Header {
        format: Some(format),
        type_size,
        pixel_width: width,
        pixel_height: height,
        pixel_depth: 0,
        layer_count: 0,
        face_count: 1,
        level_count: levels.len() as u32,
        supercompression_scheme: zstd.then_some(SupercompressionScheme::Zstandard),
        index: Index {
            dfd_byte_offset: dfd_at as u32,
            dfd_byte_length: dfd_length as u32,
            kvd_byte_offset: 0,
            kvd_byte_length: 0,
            sgd_byte_offset: 0,
            sgd_byte_length: 0,
        },
    };
    file[..Header::LENGTH].copy_from_slice(&header.as_bytes());
    for (level, entry) in index.into_iter().enumerate() {
        file[index_at + level * LevelIndex::LENGTH..][..LevelIndex::LENGTH]
            .copy_from_slice(&entry.unwrap().as_bytes());
    }
    file
}

/// A BC7 mode 6 block, laid out as the BC7 format specifies: the mode bit,
/// each endpoint's 7-bit R, G, B and A, the two p-bits, then each texel's
/// 4-bit index row by row, the first (the anchor) with its top bit implied
/// zero. It decodes to endpoint `e` = `endpoints[e] << 1 | p_bits[e]` per
/// channel, interpolated by the index's weight (0, 4, 9, …, 64) / 64.
pub(crate) fn bc7_mode_6(endpoints: [[u8; 4]; 2], p_bits: [u8; 2], indices: [u8; 16]) -> [u8; 16] {
    assert!(indices[0] < 8, "the anchor index has three bits");
    let mut bits = 1u128 << 6;
    let mut at = 7;
    let mut put = |value: u128, width: u32| {
        bits |= value << at;
        at += width;
    };
    for channel in 0..4 {
        for endpoint in endpoints {
            put(u128::from(endpoint[channel]), 7);
        }
    }
    for p_bit in p_bits {
        put(u128::from(p_bit), 1);
    }
    for (texel, &index) in indices.iter().enumerate() {
        put(u128::from(index), if texel == 0 { 3 } else { 4 });
    }
    assert_eq!(at, 128);
    bits.to_le_bytes()
}
