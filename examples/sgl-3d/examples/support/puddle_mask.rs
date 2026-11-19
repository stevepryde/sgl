//! Standing water, for a decal (`sgl_3d::Decal`): generated in code, so the
//! examples carry no image file and native and browser builds share it.

/// The image's side in texels.
const SIZE: u32 = 256;
/// Noise cells across the image at the coarsest octave.
const CELLS: f32 = 2.5;
/// The field's level at the puddle's shore, and the width of the shore's
/// soft edge, over which the surface returns to the stone's own.
const SHORE: f32 = 0.4;
const EDGE: f32 = 0.12;

/// A square RGBA8 image of one puddle: black, with alpha its coverage. One
/// coherent, irregular body of water, full in its core, fading over a soft
/// shore and gone well inside the image's edges. A decal that takes it
/// flattens what it covers toward its normal map, sets roughness from its
/// metallic-roughness map and darkens toward black by its `base_color_mix`,
/// each in proportion to this coverage.
pub fn puddle_mask() -> image::RgbaImage {
    image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let [u, v] = [x, y].map(|at| (at as f32 + 0.5) / SIZE as f32 * 2. - 1.);
        // Zero beyond 0.95 of the half side, so no box edge shows.
        let radial = 1. - smoothstep(0.3, 0.95, (u * u + v * v).sqrt());
        let field = radial * (0.45 + 0.55 * fbm(u * CELLS, v * CELLS));
        let coverage = smoothstep(SHORE - EDGE, SHORE + EDGE, field);
        image::Rgba([0, 0, 0, (coverage * 255.).round() as u8])
    })
}

/// A metallic-roughness image that sets `roughness` (green) and no metal
/// (blue), in glTF's layout, uniform: the decal's coverage shapes it.
pub fn wet_roughness(roughness: f32) -> image::RgbaImage {
    let green = (roughness.clamp(0., 1.) * 255.).round() as u8;
    image::RgbaImage::from_pixel(4, 4, image::Rgba([0, green, 0, 255]))
}

/// A tangent-space normal map of a level surface, uniform: water's surface
/// is flat where it fills the joints between stones.
pub fn flat_normal() -> image::RgbaImage {
    image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 128, 255, 255]))
}

fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = ((x - low) / (high - low)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// A lattice point's value in [0, 1], from an integer hash.
fn lattice(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841);
    h = (h ^ (h >> 13)).wrapping_mul(0xcb1a_b31f);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

/// Smoothly interpolated lattice values.
fn value_noise(x: f32, y: f32) -> f32 {
    let (cx, cy) = (x.floor(), y.floor());
    let (fx, fy) = (x - cx, y - cy);
    let (sx, sy) = (fx * fx * (3. - 2. * fx), fy * fy * (3. - 2. * fy));
    let (ix, iy) = (cx as i32, cy as i32);
    let top = lattice(ix, iy) + (lattice(ix + 1, iy) - lattice(ix, iy)) * sx;
    let bottom = lattice(ix, iy + 1) + (lattice(ix + 1, iy + 1) - lattice(ix, iy + 1)) * sx;
    top + (bottom - top) * sy
}

/// Three octaves of value noise, in [0, 1].
fn fbm(x: f32, y: f32) -> f32 {
    let (mut sum, mut amplitude, mut scale) = (0., 0.5, 1.);
    for _ in 0..3 {
        sum += amplitude * value_noise(x * scale + 7.3, y * scale - 2.1);
        amplitude *= 0.5;
        scale *= 2.;
    }
    sum / 0.875
}
