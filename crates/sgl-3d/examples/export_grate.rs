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

#[path = "support/ktx2_writer.rs"]
mod ktx2_writer;

/// BC7's 4-bit index weights, out of 64.
const WEIGHTS: [f32; 16] = [
    0., 4., 9., 13., 17., 21., 26., 30., 34., 38., 43., 47., 51., 55., 60., 64.,
];

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
                        ktx2_writer::bc7_mode_6([opaque, clear], [1, 0], indices)
                    } else {
                        ktx2_writer::bc7_mode_6([clear, opaque], [0, 1], indices)
                    }
                })
                .collect()
        })
        .collect()
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/grate.ktx2");
    let file = ktx2_writer::ktx2(ktx2::Format::BC7_SRGB_BLOCK, [64, 64], &levels(), true);
    std::fs::write(path, file)?;
    println!("wrote {path}");
    Ok(())
}
