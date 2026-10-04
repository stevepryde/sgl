//! Writes `grate.ktx2` beside this file, the masked grate's base map in the
//! `offscreen` and `browser_smoke` examples, as a game's export step writes a
//! block-compressed image: bars around square holes, 64 texels square with
//! its full chain of levels, each texel's alpha the bars' coverage of its
//! footprint. Every block is BC7 mode 6 between a clear and an opaque
//! endpoint of the bars' colour, each texel taking the index whose weight is
//! nearest its alpha; the levels are Zstandard-supercompressed.
//!
//! Run with `cargo run -p sgl-3d --example export_grate`.
use std::error::Error;

/// BC7's 4-bit index weights, out of 64.
const WEIGHTS: [f32; 16] = [
    0., 4., 9., 13., 17., 21., 26., 30., 34., 38., 43., 47., 51., 55., 60., 64.,
];

/// A BC7 mode 6 block, laid out as the BC7 format specifies: the mode bit,
/// each endpoint's 7-bit R, G, B and A, the two p-bits, then each texel's
/// 4-bit index row by row, the first (the anchor) with its top bit implied
/// zero. It decodes to endpoint `e` = `endpoints[e] << 1 | p_bits[e]` per
/// channel, interpolated by the index's weight / 64.
fn bc7_mode_6(endpoints: [[u8; 4]; 2], p_bits: [u8; 2], indices: [u8; 16]) -> [u8; 16] {
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
    bits.to_le_bytes()
}

/// The grate's levels of BC7 blocks, level 0 first.
fn levels() -> Vec<Vec<u8>> {
    let bar = |x: u32, y: u32| !((3..13).contains(&(x % 16)) && (3..13).contains(&(y % 16)));
    (0..7)
        .map(|level| {
            let side: u32 = 64 >> level;
            let footprint = 1 << level;
            let coverage = |x: u32, y: u32| {
                let bars = (0..footprint * footprint)
                    .filter(|i| bar(x * footprint + i % footprint, y * footprint + i / footprint))
                    .count();
                bars as f32 / (footprint * footprint) as f32
            };
            let blocks = side.div_ceil(4);
            (0..blocks * blocks)
                .flat_map(|block| {
                    let [x, y] = [block % blocks * 4, block / blocks * 4];
                    let mut indices: [u8; 16] = std::array::from_fn(|i| {
                        let [x, y] = [x + i as u32 % 4, y + i as u32 / 4];
                        let alpha = if x < side && y < side {
                            coverage(x, y) * 64.
                        } else {
                            0.
                        };
                        (0..16)
                            .min_by(|&a, &b| {
                                (WEIGHTS[a] - alpha)
                                    .abs()
                                    .total_cmp(&(WEIGHTS[b] - alpha).abs())
                            })
                            .unwrap() as u8
                    });
                    let clear = [100, 102, 105, 0];
                    let opaque = [100, 102, 105, 127];
                    // The anchor index has three bits: swap the endpoints when
                    // it would need four, as the weights are symmetric.
                    if indices[0] > 7 {
                        indices = indices.map(|index| 15 - index);
                        bc7_mode_6([opaque, clear], [1, 0], indices)
                    } else {
                        bc7_mode_6([clear, opaque], [0, 1], indices)
                    }
                })
                .collect()
        })
        .collect()
}

/// A KTX2 file of one 64x64 sRGB BC7 image holding `levels`, level 0 first,
/// as the KTX 2.0 specification lays it out: the header, the level index,
/// the data format descriptor, then the Zstandard-supercompressed levels
/// from the smallest to the largest.
fn ktx2(levels: &[Vec<u8>]) -> Result<Vec<u8>, Box<dyn Error>> {
    use ktx2::{Format, Header, Index, LevelIndex, SupercompressionScheme, dfd};
    let format = Format::BC7_SRGB_BLOCK;
    let (basic, type_size) = dfd::Basic::from_format(format)?;
    let block = dfd::Block::Basic(basic).to_vec();
    let index_at = Header::LENGTH;
    let dfd_at = index_at + levels.len() * LevelIndex::LENGTH;
    let dfd_length = 4 + block.len();
    let mut file = vec![0; dfd_at];
    file.extend_from_slice(&(dfd_length as u32).to_le_bytes());
    file.extend_from_slice(&block);
    let mut index = vec![None; levels.len()];
    for (level, texels) in levels.iter().enumerate().rev() {
        let stored = ruzstd::encoding::compress_to_vec(
            &texels[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
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
        pixel_width: 64,
        pixel_height: 64,
        pixel_depth: 0,
        layer_count: 0,
        face_count: 1,
        level_count: levels.len() as u32,
        supercompression_scheme: Some(SupercompressionScheme::Zstandard),
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
        let entry = entry.ok_or("a level is missing from the index")?;
        file[index_at + level * LevelIndex::LENGTH..][..LevelIndex::LENGTH]
            .copy_from_slice(&entry.as_bytes());
    }
    Ok(file)
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/grate.ktx2");
    std::fs::write(path, ktx2(&levels())?)?;
    println!("wrote {path}");
    Ok(())
}
