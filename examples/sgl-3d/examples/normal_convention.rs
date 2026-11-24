//! Tells whether a tangent-space normal map is green up (glTF's convention,
//! which SGL3D reads) or green down (DirectX), from its pixels, so a map in
//! the wrong convention is caught when it is added. An agent runs it on
//! third-party textures and on a procedural generator's output; nothing runs
//! it automatically.
//!
//! The measure (#907, #909): a normal map made from a height field has slopes
//! with near-zero curl relative to their divergence only when its Y is read
//! the right way up. Read the wrong way up, the curl becomes as large as the
//! divergence. The map's unit vectors are box-averaged to a reduced size and
//! the RMS curl/divergence ratio is taken read green up and read green down;
//! the smaller wins when it is under three quarters of the other.
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
};

use image::RgbaImage;

const HELP: &str = "\
SGL3D reads tangent-space normal maps as glTF does: green up, +Y toward the image's top (decreasing V), Z in blue.
A DirectX (green-down) map must have its green channel inverted before SGL3D uses it.

Usage: normal_convention PATH...

PATH is an image (PNG, JPEG, KTX2 level 0 in RGBA8, BC7 or BC5), a directory
(every image in it, recursively, so one holding only normal maps), or a
glTF/GLB (the images its materials use as normal or clearcoat-normal textures).

Each map gets one line: its verdict, the curl/divergence ratio of its slopes
read green up and read green down at 128 px, or 256 px if it is flat at 128
(the smaller ratio is the convention when it is under 0.75 of the other), the
verdict at 64, 128, 256 and 512 px (a verdict that holds at several sizes is
firmer), and its name.
Verdicts: green-up, green-down, undetermined (the measure cannot tell: relief
along the image's axes, noise, or not a height field), flat (no relief, or a constant tilt).
For undetermined and flat maps, check how the map was made instead.

Exit status: 0 when no map reads green-down, 1 when one does, 2 on a usage
error or a file that could not be read (the others are still measured).";

/// The averaging sizes, in texels along the image's shorter side.
const SIZES: [u32; 4] = [64, 128, 256, 512];
/// `SIZES[VERDICT_SIZE]` gives the line's verdict and ratios (#909's size),
/// unless the map is flat there: then the next size does, since fine relief
/// can average away at 128 px (Hyperdrive's tunnel detail map, green down
/// at 256 and 512 px).
const VERDICT_SIZE: usize = 1;
/// One reading's ratio must be under this fraction of the other's to decide.
const DECISIVE: f64 = 0.75;
/// RMS slope (tangent of the tilt) below which a map carries no evidence.
const FLAT_SLOPE: f64 = 0.01;
/// Smallest Z a slope is taken from, so a grazing normal cannot blow it up.
const MIN_Z: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    GreenUp,
    GreenDown,
    Undetermined,
    Flat,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Self::GreenUp => "green-up",
            Self::GreenDown => "green-down",
            Self::Undetermined => "undetermined",
            Self::Flat => "flat",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::GreenUp => "up",
            Self::GreenDown => "down",
            Self::Undetermined => "undet",
            Self::Flat => "flat",
        }
    }

    /// Why the measure cannot decide, and what to do instead.
    fn reason(self) -> Option<&'static str> {
        match self {
            Self::Undetermined => Some(
                "reads alike both ways: relief along the image's axes, noise, or not a height field; check how the map was made",
            ),
            Self::Flat => Some("too little relief to measure; check how the map was made"),
            Self::GreenUp | Self::GreenDown => None,
        }
    }
}

/// One averaging size's result.
#[derive(Clone, Copy, Debug)]
struct Reading {
    /// The averaged size, in texels per side (the image's, if smaller).
    size: u32,
    /// RMS curl / RMS divergence of the slopes read green up.
    up: f64,
    /// The same read green down.
    down: f64,
    verdict: Verdict,
}

/// How Z was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlueChannel {
    /// Blue holds Z.
    Z,
    /// Blue is constant below 128: an RG map.
    TwoChannel,
    /// Blue is constant at or above 128.
    Constant,
}

/// The map's readings at every size in `SIZES`, and how Z was taken.
fn measure(texels: &RgbaImage) -> ([Reading; 4], BlueChannel) {
    let blues = texels.pixels().map(|pixel| pixel[2]);
    let (low, high) = blues.fold((u8::MAX, 0), |(low, high), blue| {
        (low.min(blue), high.max(blue))
    });
    // Within two codes is constant, allowing for block compression.
    let blue = if high - low > 2 {
        BlueChannel::Z
    } else if high < 128 {
        BlueChannel::TwoChannel
    } else {
        BlueChannel::Constant
    };
    (
        SIZES.map(|size| reading(texels, size, blue != BlueChannel::Z)),
        blue,
    )
}

/// The texels' slopes box-averaged to `size` (at most the image's shorter
/// side), and their curl/divergence ratios read both ways.
fn reading(texels: &RgbaImage, size: u32, rebuild_z: bool) -> Reading {
    let (width, height) = texels.dimensions();
    let side = size.min(width).min(height) as usize;
    let (across, down) = (width as usize / side, height as usize / side);
    let mut sums = vec![[0_f64; 3]; side * side];
    for row in 0..side * down {
        for column in 0..side * across {
            let pixel = texels.get_pixel(column as u32, row as u32);
            let [x, y, z] = [0, 1, 2].map(|channel| f64::from(pixel[channel]) / 127.5 - 1.);
            let z = if rebuild_z {
                (1. - x * x - y * y).max(0.).sqrt()
            } else {
                z
            };
            let sum = &mut sums[row / down * side + column / across];
            sum[0] += x;
            sum[1] += y;
            sum[2] += z;
        }
    }
    // Slopes of the height field the normals describe, with y up the image:
    // p = dh/dx, q = dh/dy.
    let (mut p, mut q) = (vec![0.; sums.len()], vec![0.; sums.len()]);
    for (index, [x, y, z]) in sums.into_iter().enumerate() {
        let length = (x * x + y * y + z * z).sqrt().max(1e-9);
        let z = (z / length).max(MIN_Z);
        p[index] = -x / length / z;
        q[index] = -y / length / z;
    }
    let slope = (p.iter().zip(&q).map(|(p, q)| p * p + q * q).sum::<f64>() / p.len() as f64).sqrt();
    // A map tilted the same way everywhere has slope but no relief: its
    // curl and divergence are both zero.
    let constant = p
        .iter()
        .zip(&q)
        .all(|(x, y)| (x - p[0]).abs() < 1e-9 && (y - q[0]).abs() < 1e-9);
    // Central differences, wrapping at the edges as a tiled map does.
    let at = |field: &[f64], row: usize, column: usize| field[row % side * side + column % side];
    let d_row = |field: &[f64], row: usize, column: usize| {
        (at(field, row + 1, column) - at(field, row + side - 1, column)) / 2.
    };
    let d_column = |field: &[f64], row: usize, column: usize| {
        (at(field, row, column + 1) - at(field, row, column + side - 1)) / 2.
    };
    // Rows run down the image: read green up, dh/drow = -q; green down, +q.
    let ratio = |sign: f64| {
        let along_rows: Vec<f64> = q.iter().map(|q| sign * q).collect();
        let (mut curl, mut divergence) = (0., 0.);
        for row in 0..side {
            for column in 0..side {
                let c = d_row(&p, row, column) - d_column(&along_rows, row, column);
                let d = d_column(&p, row, column) + d_row(&along_rows, row, column);
                curl += c * c;
                divergence += d * d;
            }
        }
        (curl / divergence.max(1e-24)).sqrt()
    };
    let (up, down) = (ratio(-1.), ratio(1.));
    let verdict = if slope < FLAT_SLOPE || constant {
        Verdict::Flat
    } else if up < DECISIVE * down {
        Verdict::GreenUp
    } else if down < DECISIVE * up {
        Verdict::GreenDown
    } else {
        Verdict::Undetermined
    };
    Reading {
        size: side as u32,
        up,
        down,
        verdict,
    }
}

/// The reading that gives a map's line its verdict and ratios.
fn line_reading(readings: &[Reading; 4]) -> Reading {
    let main = readings[VERDICT_SIZE];
    if main.verdict == Verdict::Flat {
        readings[VERDICT_SIZE + 1]
    } else {
        main
    }
}

/// A map to measure: its name on the line and its texels.
struct Map {
    name: String,
    texels: Result<RgbaImage, String>,
}

/// The maps `path` names: an image, every image under a directory, or the
/// normal and clearcoat-normal images a glTF's materials use.
fn maps(path: &Path) -> Vec<Map> {
    if path.is_dir() {
        let mut files = Vec::new();
        if let Err(error) = images_under(path, &mut files) {
            return vec![Map {
                name: path.display().to_string(),
                texels: Err(error),
            }];
        }
        return files
            .into_iter()
            .map(|file| Map {
                name: file.display().to_string(),
                texels: std::fs::read(&file)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| decode(&bytes)),
            })
            .collect();
    }
    if matches!(extension(path).as_deref(), Some("gltf" | "glb")) {
        return gltf_maps(path).unwrap_or_else(|error| {
            vec![Map {
                name: path.display().to_string(),
                texels: Err(error),
            }]
        });
    }
    vec![Map {
        name: path.display().to_string(),
        texels: std::fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| decode(&bytes)),
    }]
}

fn extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

/// Every image file under `directory`, recursively, in name order.
fn images_under(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect()
        })
        .map_err(|error: std::io::Error| format!("{}: {error}", directory.display()))?;
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            images_under(&entry, files)?;
        } else if extension(&entry).as_deref() == Some("ktx2")
            || image::ImageFormat::from_path(&entry).is_ok_and(|format| format.reading_enabled())
        {
            files.push(entry);
        }
    }
    Ok(())
}

/// A KTX2 file's level 0, or any image the `image` crate reads.
fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    if bytes.starts_with(b"\xabKTX 20\xbb") {
        return decode_ktx2(bytes);
    }
    Ok(image::load_from_memory(bytes)
        .map_err(|error| error.to_string())?
        .into_rgba8())
}

fn decode_ktx2(bytes: &[u8]) -> Result<RgbaImage, String> {
    use ktx2::{Format, SupercompressionScheme};
    let reader = ktx2::Reader::new(bytes).map_err(|error| format!("KTX2: {error}"))?;
    let header = reader.header();
    let (width, height) = (header.pixel_width, header.pixel_height.max(1));
    let level = reader.levels().next().ok_or("KTX2: no levels")?;
    let data = match header.supercompression_scheme {
        None => level.data.to_vec(),
        Some(SupercompressionScheme::Zstandard) => {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(level.data)
                .map_err(|error| format!("KTX2 Zstandard: {error}"))?;
            let mut data = Vec::new();
            (&mut decoder)
                .take(level.uncompressed_byte_length)
                .read_to_end(&mut data)
                .map_err(|error| format!("KTX2 Zstandard: {error}"))?;
            data
        }
        Some(other) => {
            return Err(format!(
                "KTX2 supercompression {other:?} is not read; transcode to RGBA8 first (ktx extract --transcode rgba8)"
            ));
        }
    };
    let short = || format!("KTX2 level 0 holds {} bytes, too few", data.len());
    match header.format {
        Some(Format::R8G8B8A8_UNORM | Format::R8G8B8A8_SRGB) => {
            let bytes = width as usize * height as usize * 4;
            RgbaImage::from_raw(width, height, data.get(..bytes).ok_or_else(short)?.to_vec())
                .ok_or_else(short)
        }
        Some(Format::BC7_UNORM_BLOCK | Format::BC7_SRGB_BLOCK) => {
            decode_blocks(&data, width, height, 4, |block, texels, pitch| {
                bcdec_rs::bc7(block, texels, pitch);
            })
            .ok_or_else(short)
        }
        Some(Format::BC5_UNORM_BLOCK) => {
            let mut red_green = [0; 32];
            decode_blocks(&data, width, height, 4, |block, texels, pitch| {
                bcdec_rs::bc5(block, &mut red_green, 8, false);
                for (row, pair) in red_green.chunks_exact(8).enumerate() {
                    for (column, rg) in pair.chunks_exact(2).enumerate() {
                        let at = row * pitch + column * 4;
                        texels[at..at + 4].copy_from_slice(&[rg[0], rg[1], 0, 255]);
                    }
                }
            })
            .ok_or_else(short)
        }
        None => Err(
            "KTX2 Basis Universal is not read; transcode to RGBA8 first (ktx extract --transcode rgba8)"
                .into(),
        ),
        Some(format) => Err(format!(
            "KTX2 {format:?} is not read; use RGBA8, BC7 or BC5"
        )),
    }
}

/// 4x4 blocks of 16 bytes decoded to RGBA8 by `block`, cropped to the image.
fn decode_blocks(
    data: &[u8],
    width: u32,
    height: u32,
    side: usize,
    mut block: impl FnMut(&[u8], &mut [u8], usize),
) -> Option<RgbaImage> {
    let (across, down) = (
        (width as usize).div_ceil(side),
        (height as usize).div_ceil(side),
    );
    let pitch = across * side * 4;
    let mut padded = vec![0; pitch * down * side];
    let blocks = data.get(..across * down * 16)?;
    for (index, bytes) in blocks.chunks_exact(16).enumerate() {
        let at = index / across * side * pitch + index % across * side * 4;
        block(bytes, &mut padded[at..], pitch);
    }
    Some(RgbaImage::from_fn(width, height, |x, y| {
        let at = y as usize * pitch + x as usize * 4;
        image::Rgba(padded[at..at + 4].try_into().unwrap())
    }))
}

/// The images a glTF's materials use as normal or clearcoat-normal
/// textures, one map per image, named by texture index and material.
fn gltf_maps(path: &Path) -> Result<Vec<Map>, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let gltf = gltf::Gltf::from_slice(&bytes).map_err(|error| format!("glTF: {error}"))?;
    let base = path.parent().unwrap_or(Path::new("."));
    let texture_count = gltf.textures().len();
    // (image, texture) -> the material slots that use it, in file order.
    let mut uses: Vec<(usize, usize, Vec<String>)> = Vec::new();
    for (number, material) in gltf.materials().enumerate() {
        let label = material.name().map_or_else(
            || format!("material {number}"),
            |name| format!("\"{name}\""),
        );
        let normal = material.normal_texture().map(|info| info.texture().index());
        let coat = material
            .extension_value("KHR_materials_clearcoat")
            .and_then(|coat| coat["clearcoatNormalTexture"]["index"].as_u64())
            .map(|index| index as usize);
        for (slot, texture) in [("normal", normal), ("clearcoat normal", coat)] {
            let Some(texture) = texture else { continue };
            let image = gltf
                .textures()
                .nth(texture)
                .ok_or_else(|| {
                    format!("glTF: material {number} names texture {texture} of {texture_count}")
                })?
                .source()
                .index();
            let entry = match uses.iter_mut().find(|(used, ..)| *used == image) {
                Some(entry) => entry,
                None => {
                    uses.push((image, texture, Vec::new()));
                    uses.last_mut().unwrap()
                }
            };
            entry.2.push(format!("{label} {slot}"));
        }
    }
    let file = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(uses
        .into_iter()
        .map(|(index, texture, slots)| {
            let image = gltf.images().nth(index).expect("validated image index");
            let (source, encoded) = match image.source() {
                gltf::image::Source::Uri { uri, .. } => (
                    format!(" ({})", uri.get(..uri.len().min(48)).unwrap_or("data URI")),
                    read_uri(base, uri),
                ),
                gltf::image::Source::View { view, .. } => {
                    let buffer = match view.buffer().source() {
                        gltf::buffer::Source::Bin => gltf
                            .blob
                            .clone()
                            .ok_or_else(|| "glTF: no binary chunk".to_string()),
                        gltf::buffer::Source::Uri(uri) => read_uri(base, uri),
                    };
                    let name = image
                        .name()
                        .map(|name| format!(" ({name})"))
                        .unwrap_or_default();
                    let encoded = buffer.and_then(|buffer| {
                        buffer
                            .get(view.offset()..view.offset() + view.length())
                            .map(<[u8]>::to_vec)
                            .ok_or_else(|| "glTF: buffer view outside its buffer".to_string())
                    });
                    (name, encoded)
                }
            };
            let shown = 3;
            let mut slot_list = slots[..slots.len().min(shown)].join(", ");
            if slots.len() > shown {
                slot_list += &format!(" and {} more", slots.len() - shown);
            }
            Map {
                name: format!("{file} texture {texture}{source}: {slot_list}"),
                texels: encoded.and_then(|bytes| decode(&bytes)),
            }
        })
        .collect())
}

/// A glTF URI's bytes: a base64 data URI, or a file beside the glTF.
fn read_uri(base: &Path, uri: &str) -> Result<Vec<u8>, String> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (_, encoded) = data
            .split_once(";base64,")
            .ok_or("glTF: data URI is not base64")?;
        return base64(encoded).ok_or_else(|| "glTF: invalid base64 data URI".into());
    }
    let file = base.join(percent_decoded(uri));
    std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let (mut bits, mut count) = (0_u32, 0);
    for byte in text.bytes().filter(|&byte| byte != b'=') {
        bits = bits << 6 | u32::from(value(byte)?);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    Some(bytes)
}

/// A relative URI's path with its %XX escapes decoded (RFC 3986).
fn percent_decoded(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = (bytes[index] == b'%')
            .then(|| std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = escape {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn line(name: &str, readings: &[Reading; 4], blue: BlueChannel) -> String {
    let main = line_reading(readings);
    let sizes = readings
        .iter()
        .map(|reading| format!("{}:{}", reading.size, reading.verdict.short()))
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = format!(
        "{:<12}  up {:.2} down {:.2}  {sizes:<34}  {name}",
        main.verdict.name(),
        main.up,
        main.down
    );
    match blue {
        BlueChannel::Z => {}
        BlueChannel::TwoChannel => text += "  [two-channel: Z rebuilt from X and Y]",
        BlueChannel::Constant => text += "  [blue constant: Z rebuilt from X and Y]",
    }
    if let Some(reason) = main.verdict.reason() {
        text += &format!("  [{reason}]");
    }
    text
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.is_empty() || args.iter().any(|arg| arg.starts_with('-')) {
        eprintln!("usage: normal_convention PATH...  (--help explains)");
        return ExitCode::from(2);
    }
    let mut counts = [0_usize; 4];
    let mut unreadable = 0;
    for path in &args {
        for map in maps(Path::new(path)) {
            match map.texels {
                Ok(texels) => {
                    let (readings, blue) = measure(&texels);
                    counts[line_reading(&readings).verdict as usize] += 1;
                    println!("{}", line(&map.name, &readings, blue));
                }
                Err(error) => {
                    unreadable += 1;
                    eprintln!("unreadable    {}: {error}", map.name);
                }
            }
        }
    }
    let [up, down, undetermined, flat] = counts;
    let mut summary = format!(
        "{} maps: {up} green-up, {down} green-down, {undetermined} undetermined, {flat} flat",
        counts.iter().sum::<usize>() + unreadable
    );
    if unreadable > 0 {
        summary += &format!(", {unreadable} unreadable");
    }
    println!("{summary}");
    ExitCode::from(if unreadable > 0 {
        2
    } else {
        u8::from(down > 0)
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    const SIDE: u32 = 256;

    /// A normal map of `height` (texel units, rows down the image) written
    /// by glTF's definition: n = normalize(-dh/dx, -dh/dy, 1) with y up the
    /// image, so green holds +dh/drow. Slopes come from the analytic height
    /// by a fine central difference, not from the measure's grid.
    fn normal_map(
        height: impl Fn(f64, f64) -> f64,
        green_sign: f64,
        blue: Option<u8>,
    ) -> RgbaImage {
        let encode = |value: f64| ((value * 0.5 + 0.5) * 255.).round() as u8;
        RgbaImage::from_fn(SIDE, SIDE, |column, row| {
            let (x, y) = (f64::from(column), f64::from(row));
            let step = 1e-3;
            let dx = (height(x + step, y) - height(x - step, y)) / (2. * step);
            let d_row = (height(x, y + step) - height(x, y - step)) / (2. * step);
            let (nx, ny, nz) = (-dx, green_sign * d_row, 1.);
            let length = (nx * nx + ny * ny + nz * nz).sqrt();
            image::Rgba([
                encode(nx / length),
                encode(ny / length),
                blue.unwrap_or(encode(nz / length)),
                255,
            ])
        })
    }

    /// Round bumps and a ridge slanted at 35 degrees, tapered along its
    /// length, all away from the edges.
    fn bumps_and_ridge(x: f64, y: f64) -> f64 {
        let bump = |cx: f64, cy: f64, radius: f64, peak: f64| {
            peak * (-((x - cx).powi(2) + (y - cy).powi(2)) / (2. * radius * radius)).exp()
        };
        let (cos, sin) = (35_f64.to_radians().cos(), 35_f64.to_radians().sin());
        let (along, across) = (
            (x - 128.) * cos + (y - 128.) * sin,
            -(x - 128.) * sin + (y - 128.) * cos,
        );
        let ridge = 8. * (-across * across / 72.).exp() * (-along * along / 5000.).exp();
        bump(60., 70., 14., 10.)
            + bump(190., 80., 10., -7.)
            + bump(90., 190., 18., 12.)
            + bump(200., 200., 8., 5.)
            + ridge
    }

    fn verdict(texels: &RgbaImage) -> Verdict {
        line_reading(&measure(texels).0).verdict
    }

    #[test]
    fn a_height_fields_map_reads_the_way_it_was_written() {
        assert_eq!(
            verdict(&normal_map(bumps_and_ridge, 1., None)),
            Verdict::GreenUp
        );
        assert_eq!(
            verdict(&normal_map(bumps_and_ridge, -1., None)),
            Verdict::GreenDown
        );
        // A two-channel map of the same relief: blue carries nothing.
        let (readings, blue) = measure(&normal_map(bumps_and_ridge, 1., Some(0)));
        assert_eq!(blue, BlueChannel::TwoChannel);
        assert_eq!(line_reading(&readings).verdict, Verdict::GreenUp);
    }

    fn khronos() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/sgl-3d/tests/fixtures/gltf/NormalTangentTest/glTF")
    }

    /// Khronos NormalTangentTest's map is green up by glTF's definition.
    #[test]
    fn khronos_normal_tangent_test_reads_green_up_as_an_image_and_through_its_gltf() {
        let image = maps(&khronos().join("NormalTangentTest_Normal.png"));
        let gltf = maps(&khronos().join("NormalTangentTest.gltf"));
        assert_eq!(gltf.len(), 1, "the glTF's one material uses one normal map");
        for map in image.into_iter().chain(gltf) {
            assert_eq!(
                verdict(&map.texels.unwrap()),
                Verdict::GreenUp,
                "{}",
                map.name
            );
        }
    }

    /// Constant maps, upright or tilted (Bistro's liquor label), have no relief.
    #[test]
    fn a_constant_map_is_flat() {
        for texel in [[128, 128, 255, 255], [53, 202, 199, 255]] {
            let texels = RgbaImage::from_pixel(SIDE, SIDE, image::Rgba(texel));
            let (readings, _) = measure(&texels);
            assert!(
                readings
                    .iter()
                    .all(|reading| reading.verdict == Verdict::Flat),
                "{texel:?}"
            );
        }
    }

    /// Relief that varies along X alone plus relief along Y alone has no
    /// curl read either way round.
    #[test]
    fn relief_along_the_image_axes_is_undetermined() {
        let grooves = |x: f64, y: f64| {
            4. * (x / 9.).sin().powi(3) + 3. * (-(y % 40. - 20.).powi(2) / 18.).exp()
        };
        assert_eq!(
            verdict(&normal_map(grooves, 1., None)),
            Verdict::Undetermined
        );
    }
}
