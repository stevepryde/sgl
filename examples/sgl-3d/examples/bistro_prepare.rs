//! Explicit Bistro content conversion; no runtime loader exceptions.
//! See ../BISTRO.md. KTX-Software performs Basis decoding; SGL's image
//! dependency resizes and corrects channels before exporting core glTF.
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    f64::consts::TAU,
    path::{Path, PathBuf},
    process::Command,
};

use glam::DVec3;
use image::{ImageBuffer, Luma, Rgba, Rgba32FImage, imageops::FilterType};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Color,
    /// A normal map, by which way its source image's green points.
    Normal(Green),
    Data,
}

/// Which way a source normal map's green (+Y) points up its image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Green {
    /// DirectX: +Y toward the image's bottom. Inverted for glTF.
    Down,
    /// glTF: +Y toward the image's top. Kept.
    Up,
}

/// The source normal maps authored green up, though the conversion's README
/// calls every Bistro normal map DirectX (#910). A height field's normal map
/// is integrable (near-zero curl) only read the right way up; Khronos's
/// NormalTangentTest, green up by glTF's rule, has a curl to divergence ratio
/// of 0.20 read green up against 1.23 read green down (#907). Decoded at
/// BISTRO_REVISION and averaged to 64-2048 px, each map here reads lower
/// green up at every size but at most one (Plantpots at 64 px, the main door
/// at 2048) and under 0.75 of its green-down ratio at two to six sizes: the
/// curtains 0.03-0.32 against 0.20-0.44, the ivy leaf 0.08-0.48 against
/// 1.25-1.48; the weakest, the doors, 0.32-0.53 against 0.44-0.61. The two
/// antenna plastics are one fine, near-flat image reading 0.68-0.78 against
/// 1.17-1.36 at 512-2048 px. The other maps read green down wherever the
/// measure decides. Those it cannot keep the README's DirectX: flat,
/// constant (Paris_LiquorBottle_01_Labels), axis-aligned hard-surface relief
/// it is blind to, and atlases reading one way coarse and the other fine
/// (Paris_TrafficSign_A, Paris_StreetSign_01, Foliage_Flowers).
const GREEN_UP_SOURCES: [&str; 12] = [
    "Textures/Antenna_Plastic_Blue_Normal.ktx2",
    "Textures/Antenna_Plastic_Normal.ktx2",
    "Textures/Foliage_Ivy_branches_Normal.ktx2",
    "Textures/Foliage_Ivy_leaf_a_Normal.ktx2",
    "Textures/MASTER_Bistro_Main_Door_Normal.ktx2",
    "Textures/MASTER_Curtains_Normal.ktx2",
    "Textures/MASTER_Doors_Normal.ktx2",
    "Textures/MASTER_Frosted_Glass_Normal.ktx2",
    "Textures/Pavement_Curbstones_Normal.ktx2",
    "Textures/Plantpots_Normal.ktx2",
    "Textures/Shopsign_Bakery_Normal.ktx2",
    "Textures/Spotlight_Glass_Emissive_Normal.ktx2",
];

fn image_index(document: &Value, texture: &Value) -> Option<usize> {
    let texture = &document["textures"][texture["index"].as_u64()? as usize];
    texture["extensions"]["KHR_texture_basisu"]["source"]
        .as_u64()
        .or_else(|| texture["source"].as_u64())
        .map(|v| v as usize)
}

fn adapt(document: &mut Value) -> Result<BTreeMap<usize, Role>, Box<dyn Error>> {
    // The conversion README specifies R=occlusion, G=roughness, B=metalness.
    // Its glTF mistakenly puts that texture in KHR_materials_specular.
    // Restore G/B roughness/metalness. Do not infer an occlusionTexture:
    // the source glTF binds none, and inspected Vespa, concrete and cobble
    // images have R=0 everywhere. Treating that as glTF visibility would
    // extinguish all indirect light. Retain R unused, as the source glTF does.
    for material in document["materials"]
        .as_array_mut()
        .ok_or("missing materials")?
    {
        let packed = material["extensions"]["KHR_materials_specular"]["specularTexture"].clone();
        if !packed.is_null() {
            material["pbrMetallicRoughness"]["metallicRoughnessTexture"] = packed.clone();
            material["pbrMetallicRoughness"]["metallicFactor"] = json!(1.);
            material["pbrMetallicRoughness"]["roughnessFactor"] = json!(1.);
            let extension = material["extensions"]["KHR_materials_specular"]
                .as_object_mut()
                .unwrap();
            extension.remove("specularTexture");
            if extension.is_empty() {
                material["extensions"]
                    .as_object_mut()
                    .unwrap()
                    .remove("KHR_materials_specular");
            }
        }
    }
    let mut roles = BTreeMap::new();
    let images = document["images"].as_array().ok_or("missing images")?;
    let green = |image: usize| match images[image]["uri"].as_str() {
        Some(uri) if GREEN_UP_SOURCES.contains(&uri) => Green::Up,
        _ => Green::Down,
    };
    for material in document["materials"].as_array().unwrap() {
        for (texture, role) in [
            (
                &material["pbrMetallicRoughness"]["baseColorTexture"],
                Role::Color,
            ),
            (&material["emissiveTexture"], Role::Color),
            (&material["normalTexture"], Role::Normal(Green::Down)),
            (
                &material["pbrMetallicRoughness"]["metallicRoughnessTexture"],
                Role::Data,
            ),
            (&material["occlusionTexture"], Role::Data),
        ] {
            let Some(image) = image_index(document, texture) else {
                continue;
            };
            let role = match role {
                Role::Normal(_) => Role::Normal(green(image)),
                role => role,
            };
            if let Some(previous) = roles.insert(image, role)
                && previous != role
            {
                return Err(
                    format!("image {image} is shared by incompatible colour/data roles").into(),
                );
            }
        }
    }
    for texture in document["textures"]
        .as_array_mut()
        .ok_or("missing textures")?
    {
        if let Some(source) = texture["extensions"]["KHR_texture_basisu"]["source"].as_u64() {
            texture["source"] = json!(source);
            texture["extensions"]
                .as_object_mut()
                .unwrap()
                .remove("KHR_texture_basisu");
            if texture["extensions"]
                .as_object()
                .is_some_and(|object| object.is_empty())
            {
                texture.as_object_mut().unwrap().remove("extensions");
            }
        }
    }
    let has_specular = document["materials"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| !m["extensions"]["KHR_materials_specular"].is_null());
    for key in ["extensionsRequired", "extensionsUsed"] {
        if let Some(extensions) = document[key].as_array_mut() {
            extensions.retain(|extension| {
                extension != "KHR_texture_basisu"
                    && (has_specular || extension != "KHR_materials_specular")
            });
        }
    }
    Ok(roles)
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}

fn convert(input: &image::RgbaImage, role: Role, maximum: u32) -> image::RgbaImage {
    // This Bistro conversion's two-channel normals retain XY in RG and a
    // constant zero B (e.g. Vespa_Normal). Decode their positive Z before
    // filtering; treating the absent B as signed -1 reverses the normals.
    // Full RGB normal maps, including the small flat defaults, keep their Z.
    let normal_rg = matches!(role, Role::Normal(_)) && input.pixels().all(|p| p[2] == 0);
    let mut linear: Rgba32FImage = ImageBuffer::from_fn(input.width(), input.height(), |x, y| {
        let mut pixel = input.get_pixel(x, y).0.map(|v| f32::from(v) / 255.);
        if role == Role::Color {
            for channel in 0..3 {
                pixel[channel] = srgb_to_linear(pixel[channel]) * pixel[3];
            }
        } else if let Role::Normal(green) = role {
            let nx = pixel[0] * 2. - 1.;
            let ny = match green {
                Green::Down => 1. - pixel[1] * 2., // DirectX Y-down to glTF Y-up.
                Green::Up => pixel[1] * 2. - 1.,
            };
            let nz = if normal_rg {
                (1. - nx * nx - ny * ny).max(0.).sqrt()
            } else {
                pixel[2] * 2. - 1.
            };
            let normal = glam::Vec3::new(nx, ny, nz)
                .try_normalize()
                .unwrap_or(glam::Vec3::Z);
            pixel[..3].copy_from_slice(&(normal * 0.5 + glam::Vec3::splat(0.5)).to_array());
        }
        Rgba(pixel)
    });
    let scale = (maximum as f32 / input.width().max(input.height()) as f32).min(1.);
    if scale < 1. {
        linear = image::imageops::resize(
            &linear,
            (input.width() as f32 * scale).round().max(1.) as u32,
            (input.height() as f32 * scale).round().max(1.) as u32,
            FilterType::Lanczos3,
        );
    }
    ImageBuffer::from_fn(linear.width(), linear.height(), |x, y| {
        let mut pixel = linear.get_pixel(x, y).0;
        if role == Role::Color {
            for channel in 0..3 {
                pixel[channel] = linear_to_srgb(pixel[channel].max(0.) / pixel[3].max(1e-8));
            }
        } else if matches!(role, Role::Normal(_)) {
            // Re-normalize the already decoded, Y-corrected vectors after
            // linear filtering, never gamma-correct normal data.
            let mut normal =
                glam::Vec3::new(pixel[0] * 2. - 1., pixel[1] * 2. - 1., pixel[2] * 2. - 1.);
            normal = normal.try_normalize().unwrap_or(glam::Vec3::Z);
            pixel[..3].copy_from_slice(&(normal * 0.5 + glam::Vec3::splat(0.5)).to_array());
        }
        Rgba(pixel.map(|v| (v.clamp(0., 1.) * 255.).round() as u8))
    })
}

/// What the export binds as materials' occlusion (`--occlusion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OcclusionExport {
    /// None, as the source binds none: the default.
    None,
    /// Occlusion derived from tiled materials' normal maps
    /// (derive_occlusion): content this export makes, not source data.
    Derived,
}

/// The azimuths derived visibility averages over.
const OCCLUSION_AZIMUTHS: usize = 16;
/// Derived visibility looks for the horizon within the image's width over
/// this: the example's authored reach (BISTRO.md).
const OCCLUSION_REACH_DIVISOR: usize = 64;
/// A normal map is integrable where the normals of its integrated height
/// differ from its own by at most this RMS angle, in degrees: an integrable
/// map's differ by 8-bit quantisation, under 1°.
const INTEGRABLE_DEGREES: f64 = 5.;
/// One 8-bit step of a normal component in -1..1, the least Z a slope is
/// taken over.
const NORMAL_STEP: f64 = 2. / 255.;
/// How far a texture coordinate may stray from [0, 1] before it tiles.
const UV_TOLERANCE: f32 = 1e-3;
/// glTF's REPEAT wrapping.
const REPEAT: u64 = 10497;

/// A height field's slopes from an exported normal image (glTF's +Y, unit
/// tangent-space normals) under normalTexture `scale`: dh/dx along U and
/// dh/dy with y up the image (V up), in texels, each normal's Z floored at
/// one 8-bit step. Indexed `y * width + x`, y counted from the bottom row.
fn normal_slopes(image: &image::RgbaImage, scale: f64) -> (Vec<f64>, Vec<f64>) {
    let (width, height) = image.dimensions();
    let texels = (width * height) as usize;
    let (mut p, mut q) = (Vec::with_capacity(texels), Vec::with_capacity(texels));
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, _] = image.get_pixel(x, height - 1 - y).0;
            let [nx, ny, nz] = [r, g, b].map(|v| f64::from(v) / 127.5 - 1.);
            let nz = nz.max(NORMAL_STEP);
            p.push(-nx * scale / nz);
            q.push(-ny * scale / nz);
        }
    }
    (p, q)
}

/// In-place radix-2 FFT of `re` + i`im`, whose length is a power of two;
/// the inverse unscaled where `inverse`.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1. } else { -1. };
    let twiddles: Vec<(f64, f64)> = (0..n / 2)
        .map(|k| (sign * TAU * k as f64 / n as f64).sin_cos())
        .collect();
    let mut length = 2;
    while length <= n {
        let stride = n / length;
        for start in (0..n).step_by(length) {
            for k in 0..length / 2 {
                let (sin, cos) = twiddles[k * stride];
                let (a, b) = (start + k, start + k + length / 2);
                let tr = re[b] * cos - im[b] * sin;
                let ti = re[b] * sin + im[b] * cos;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        length <<= 1;
    }
}

/// fft over each row of a `width`-wide field, then each column.
fn fft2(re: &mut [f64], im: &mut [f64], width: usize, inverse: bool) {
    let height = re.len() / width;
    for (row_re, row_im) in re.chunks_exact_mut(width).zip(im.chunks_exact_mut(width)) {
        fft(row_re, row_im, inverse);
    }
    let (mut column_re, mut column_im) = (vec![0.; height], vec![0.; height]);
    for x in 0..width {
        for y in 0..height {
            column_re[y] = re[y * width + x];
            column_im[y] = im[y * width + x];
        }
        fft(&mut column_re, &mut column_im, inverse);
        for y in 0..height {
            re[y * width + x] = column_re[y];
            im[y * width + x] = column_im[y];
        }
    }
}

/// The angular frequency of DFT bin `k` of `n`, in radians a texel: 0 at
/// Nyquist, where a real field's derivative has no sign.
fn frequency(k: usize, n: usize) -> f64 {
    if 2 * k == n {
        0.
    } else if 2 * k < n {
        TAU * k as f64 / n as f64
    } else {
        TAU * (k as f64 - n as f64) / n as f64
    }
}

/// Frankot and Chellappa's (1988) integration: the periodic height of mean
/// 0 whose gradient is nearest slopes `p` and `q` (normal_slopes) in least
/// squares, H = −i(ωx P + ωy Q) / (ωx² + ωy²) in the frequency domain.
fn integrate(p: &[f64], q: &[f64], width: usize) -> Vec<f64> {
    let height = p.len() / width;
    let zero = vec![0.; p.len()];
    let (mut pr, mut pi, mut qr, mut qi) = (p.to_vec(), zero.clone(), q.to_vec(), zero.clone());
    fft2(&mut pr, &mut pi, width, false);
    fft2(&mut qr, &mut qi, width, false);
    let (mut hr, mut hi) = (zero.clone(), zero);
    for v in 0..height {
        let wy = frequency(v, height);
        for u in 0..width {
            let wx = frequency(u, width);
            let d = wx * wx + wy * wy;
            if d == 0. {
                continue;
            }
            let i = v * width + u;
            let (a, b) = (wx * pr[i] + wy * qr[i], wx * pi[i] + wy * qi[i]);
            hr[i] = b / d;
            hi[i] = -a / d;
        }
    }
    fft2(&mut hr, &mut hi, width, true);
    let n = p.len() as f64;
    hr.iter().map(|h| h / n).collect()
}

/// The slopes of periodic `height`: its spectral derivative, iωH per axis,
/// the derivative integrate inverts.
fn gradient(height: &[f64], width: usize) -> (Vec<f64>, Vec<f64>) {
    let rows = height.len() / width;
    let (mut hr, mut hi) = (height.to_vec(), vec![0.; height.len()]);
    fft2(&mut hr, &mut hi, width, false);
    let (mut pr, mut pi, mut qr, mut qi) = (hr.clone(), hi.clone(), hr.clone(), hi.clone());
    for v in 0..rows {
        let wy = frequency(v, rows);
        for u in 0..width {
            let wx = frequency(u, width);
            let i = v * width + u;
            (pr[i], pi[i]) = (-wx * hi[i], wx * hr[i]);
            (qr[i], qi[i]) = (-wy * hi[i], wy * hr[i]);
        }
    }
    fft2(&mut pr, &mut pi, width, true);
    fft2(&mut qr, &mut qi, width, true);
    let n = height.len() as f64;
    (
        pr.iter().map(|v| v / n).collect(),
        qr.iter().map(|v| v / n).collect(),
    )
}

/// The RMS angle in degrees between the normals two sets of slopes stand
/// for, (−p, −q, 1) normalised.
fn rms_angle((pa, qa): (&[f64], &[f64]), (pb, qb): (&[f64], &[f64])) -> f64 {
    let normal = |p: f64, q: f64| DVec3::new(-p, -q, 1.).normalize();
    let sum: f64 = (0..pa.len())
        .map(|i| {
            normal(pa[i], qa[i])
                .dot(normal(pb[i], qb[i]))
                .clamp(-1., 1.)
                .acos()
                .powi(2)
        })
        .sum();
    (sum / pa.len() as f64).sqrt().to_degrees()
}

/// The cosine-weighted visibility of periodic `height` (in texels,
/// `width` wide) about its base plane at each texel: the mean over
/// OCCLUSION_AZIMUTHS evenly spaced azimuths of cos² of the greatest
/// elevation to the field within `reach` texels, sampled bilinearly a texel
/// apart and wrapping. A slice of the cosine-weighted hemisphere whose
/// horizon stands at elevation β keeps cos² β of its share.
fn visibility(height: &[f64], width: usize, reach: usize) -> Vec<f32> {
    let rows = height.len() / width;
    let wrap = |v: f64, n: usize| (v as i64).rem_euclid(n as i64) as usize;
    let at = |x: f64, y: f64| {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (xa, xb) = (wrap(x0, width), wrap(x0 + 1., width));
        let (ya, yb) = (wrap(y0, rows), wrap(y0 + 1., rows));
        let h = |x: usize, y: usize| height[y * width + x];
        let below = h(xa, ya) + (h(xb, ya) - h(xa, ya)) * fx;
        let above = h(xa, yb) + (h(xb, yb) - h(xa, yb)) * fx;
        below + (above - below) * fy
    };
    let directions: Vec<(f64, f64)> = (0..OCCLUSION_AZIMUTHS)
        .map(|k| {
            let (sin, cos) = (TAU * k as f64 / OCCLUSION_AZIMUTHS as f64).sin_cos();
            (cos, sin)
        })
        .collect();
    let mut result = Vec::with_capacity(height.len());
    for y in 0..rows {
        for x in 0..width {
            let base = height[y * width + x];
            let mut sum = 0.;
            for &(dx, dy) in &directions {
                let mut rise = 0f64;
                for step in 1..=reach {
                    let d = step as f64;
                    rise = rise.max((at(x as f64 + d * dx, y as f64 + d * dy) - base) / d);
                }
                sum += 1. / (1. + rise * rise);
            }
            result.push((sum / OCCLUSION_AZIMUTHS as f64) as f32);
        }
    }
    result
}

/// What deriving occlusion from one normal image gives.
#[derive(Debug)]
enum Derived {
    /// Its visibility, rows top first as the image's, and the RMS angle
    /// between its normals and those of its integrated height.
    Visibility(Vec<f32>, f64),
    /// A normal map no height field reproduces, by that RMS angle.
    NotIntegrable(f64),
}

/// Occlusion from a tiled material's exported normal image under
/// normalTexture `scale`: its slopes integrated to a height, which must
/// reproduce its normals (INTEGRABLE_DEGREES), and that height's
/// visibility within the image's width over OCCLUSION_REACH_DIVISOR.
fn derive_visibility(normal: &image::RgbaImage, scale: f64) -> Derived {
    let (width, rows) = (normal.width() as usize, normal.height() as usize);
    let (p, q) = normal_slopes(normal, scale);
    let height = integrate(&p, &q, width);
    let (rp, rq) = gradient(&height, width);
    let rms = rms_angle((&p, &q), (&rp, &rq));
    if rms > INTEGRABLE_DEGREES {
        return Derived::NotIntegrable(rms);
    }
    let up = visibility(&height, width, (width / OCCLUSION_REACH_DIVISOR).max(1));
    let top_first = (0..rows)
        .rev()
        .flat_map(|y| up[y * width..][..width].iter().copied())
        .collect();
    Derived::Visibility(top_first, rms)
}

/// Whether the texture `info` refers to repeats on both axes.
fn texture_repeats(document: &Value, info: &Value) -> bool {
    let texture = &document["textures"][info["index"].as_u64().unwrap_or(u64::MAX) as usize];
    let Some(sampler) = texture["sampler"].as_u64() else {
        return true;
    };
    let sampler = &document["samplers"][sampler as usize];
    ["wrapS", "wrapT"]
        .iter()
        .all(|key| sampler[key].as_u64().is_none_or(|wrap| wrap == REPEAT))
}

/// Whether some coordinate of TEXCOORD_0 accessor `index` leaves [0, 1].
fn texcoords_leave_unit(
    document: &Value,
    buffers: &[Vec<u8>],
    index: usize,
) -> Result<bool, String> {
    let accessor = &document["accessors"][index];
    if !accessor["sparse"].is_null() {
        return Err("its TEXCOORD_0 is sparse".into());
    }
    // Normalized integer coordinates lie in [0, 1].
    if accessor["componentType"].as_u64() != Some(5126) {
        return Ok(false);
    }
    let view = accessor["bufferView"]
        .as_u64()
        .map(|view| &document["bufferViews"][view as usize])
        .ok_or("its TEXCOORD_0 has no buffer view")?;
    let buffer = view["buffer"]
        .as_u64()
        .and_then(|buffer| buffers.get(buffer as usize))
        .ok_or("its TEXCOORD_0's buffer is missing")?;
    let stride = view["byteStride"].as_u64().unwrap_or(8) as usize;
    let start = (view["byteOffset"].as_u64().unwrap_or(0)
        + accessor["byteOffset"].as_u64().unwrap_or(0)) as usize;
    for vertex in 0..accessor["count"].as_u64().unwrap_or(0) as usize {
        for component in 0..2 {
            let at = start + vertex * stride + component * 4;
            let bytes = buffer
                .get(at..at + 4)
                .ok_or("its TEXCOORD_0 runs past its buffer")?;
            let value = f32::from_le_bytes(bytes.try_into().unwrap());
            if !(-UV_TOLERANCE..=1. + UV_TOLERANCE).contains(&value) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// A material that qualifies for derived occlusion: its normal and
/// metallic-roughness images, the metallic-roughness textureInfo, and its
/// normal map's scale.
struct Tiled {
    normal: usize,
    mr: usize,
    mr_texture: Value,
    scale: f64,
}

/// Material `index` where it qualifies for derived occlusion, else why
/// not: a normal map and a metallic-roughness map on TEXCOORD_0, whose
/// images are paired with each other in every material that uses either
/// (`normal_partners`, `mr_partners`: the images each is paired with, None
/// for a material without the other), whose samplers repeat, and some
/// primitive of which has TEXCOORD_0 outside [0, 1], so that its maps tile.
/// The derivation assumes square texels: an image stretched over its tile
/// gives a distorted height.
fn qualify(
    document: &Value,
    buffers: &[Vec<u8>],
    index: usize,
    normal_partners: &BTreeMap<usize, BTreeSet<Option<usize>>>,
    mr_partners: &BTreeMap<usize, BTreeSet<Option<usize>>>,
) -> Result<Tiled, String> {
    let material = &document["materials"][index];
    let normal_info = &material["normalTexture"];
    let mr_info = &material["pbrMetallicRoughness"]["metallicRoughnessTexture"];
    let (Some(normal), Some(mr)) = (
        image_index(document, normal_info),
        image_index(document, mr_info),
    ) else {
        return Err("it lacks a normal map or a metallic-roughness map".into());
    };
    if [normal_info, mr_info]
        .iter()
        .any(|info| info["texCoord"].as_u64().unwrap_or(0) != 0)
    {
        return Err("a map is not on TEXCOORD_0".into());
    }
    if normal_partners[&normal] != BTreeSet::from([Some(mr)])
        || mr_partners[&mr] != BTreeSet::from([Some(normal)])
    {
        return Err("another material pairs its images otherwise".into());
    }
    if !texture_repeats(document, normal_info) || !texture_repeats(document, mr_info) {
        return Err("its sampler does not repeat".into());
    }
    let mut tiles = false;
    for primitive in document["meshes"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|mesh| mesh["primitives"].as_array().into_iter().flatten())
        .filter(|primitive| primitive["material"].as_u64() == Some(index as u64))
    {
        if let Some(accessor) = primitive["attributes"]["TEXCOORD_0"].as_u64() {
            tiles |= texcoords_leave_unit(document, buffers, accessor as usize)?;
        }
    }
    if !tiles {
        return Err("its TEXCOORD_0 stays within [0, 1], so its maps do not tile".into());
    }
    Ok(Tiled {
        normal,
        mr,
        mr_texture: mr_info.clone(),
        scale: normal_info["scale"].as_f64().unwrap_or(1.),
    })
}

/// `--occlusion derived`: for each tiled material (qualify), occlusion
/// derived from its exported normal image (derive_visibility, under
/// `images`, the export's directory) is written to its exported
/// metallic-roughness image's red channel, resampled to that image's size,
/// and bound as its occlusionTexture at strength 1 (ORM packing). Every
/// other material is listed with why it was skipped. `buffers` are the
/// document's buffers, in order.
fn derive_occlusion(
    document: &mut Value,
    buffers: &[Vec<u8>],
    images: &Path,
) -> Result<(), Box<dyn Error>> {
    let materials = document["materials"]
        .as_array()
        .ok_or("missing materials")?
        .len();
    let (mut normal_partners, mut mr_partners) = (BTreeMap::new(), BTreeMap::new());
    for material in document["materials"].as_array().unwrap() {
        let normal = image_index(document, &material["normalTexture"]);
        let mr = image_index(
            document,
            &material["pbrMetallicRoughness"]["metallicRoughnessTexture"],
        );
        if let Some(normal) = normal {
            normal_partners
                .entry(normal)
                .or_insert_with(BTreeSet::new)
                .insert(mr);
        }
        if let Some(mr) = mr {
            mr_partners
                .entry(mr)
                .or_insert_with(BTreeSet::new)
                .insert(normal);
        }
    }
    let name = |document: &Value, index: usize| {
        document["materials"][index]["name"]
            .as_str()
            .map_or_else(|| format!("material {index}"), str::to_owned)
    };
    let mut skipped = 0;
    let mut pairs: BTreeMap<(usize, usize), Vec<(usize, Tiled)>> = BTreeMap::new();
    for index in 0..materials {
        match qualify(document, buffers, index, &normal_partners, &mr_partners) {
            Ok(tiled) => pairs
                .entry((tiled.normal, tiled.mr))
                .or_default()
                .push((index, tiled)),
            Err(reason) => {
                println!("Occlusion: skipped {}: {reason}", name(document, index));
                skipped += 1;
            }
        }
    }
    let mut derived = 0;
    let image_path = |document: &Value, image: usize| -> Result<PathBuf, Box<dyn Error>> {
        Ok(images.join(
            document["images"][image]["uri"]
                .as_str()
                .ok_or("image without URI")?,
        ))
    };
    for ((normal, mr), tiled) in pairs {
        let skip = |document: &Value, reason: &str| {
            for (index, _) in &tiled {
                println!("Occlusion: skipped {}: {reason}", name(document, *index));
            }
            tiled.len()
        };
        let scale = tiled[0].1.scale;
        if tiled.iter().any(|(_, t)| t.scale != scale) {
            skipped += skip(
                document,
                "materials sharing its images scale their normals differently",
            );
            continue;
        }
        let normal_image = image::open(image_path(document, normal)?)?.into_rgba8();
        let mr_path = image_path(document, mr)?;
        let mut mr_image = image::open(&mr_path)?.into_rgba8();
        if ![normal_image.dimensions(), mr_image.dimensions()]
            .iter()
            .all(|(w, h)| w.is_power_of_two() && h.is_power_of_two())
        {
            skipped += skip(document, "an exported image's sides are not powers of two");
            continue;
        }
        let (visibility, rms) = match derive_visibility(&normal_image, scale) {
            Derived::Visibility(visibility, rms) => (visibility, rms),
            Derived::NotIntegrable(rms) => {
                skipped += skip(
                    document,
                    &format!(
                        "its normals are not integrable ({rms:.2}° RMS from its height's, over {INTEGRABLE_DEGREES}°)"
                    ),
                );
                continue;
            }
        };
        let mut occlusion = ImageBuffer::<Luma<f32>, _>::from_raw(
            normal_image.width(),
            normal_image.height(),
            visibility,
        )
        .unwrap();
        if occlusion.dimensions() != mr_image.dimensions() {
            occlusion = image::imageops::resize(
                &occlusion,
                mr_image.width(),
                mr_image.height(),
                FilterType::Triangle,
            );
        }
        for (pixel, value) in mr_image.pixels_mut().zip(occlusion.pixels()) {
            pixel[0] = (value[0].clamp(0., 1.) * 255.).round() as u8;
        }
        mr_image.save(&mr_path)?;
        for (index, tiled) in &tiled {
            document["materials"][*index]["occlusionTexture"] =
                json!({"index": tiled.mr_texture["index"], "strength": 1.});
            println!(
                "Occlusion: derived {}: its height's normals within {rms:.2}° RMS of its own",
                name(document, *index)
            );
            derived += 1;
        }
    }
    println!("Occlusion: derived for {derived} materials, {skipped} skipped");
    Ok(())
}

fn resolved_destination(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = std::path::absolute(path)?;
            let name = path.file_name().ok_or_else(|| {
                std::io::Error::other("output path must have a resolvable directory name")
            })?;
            Ok(resolved_destination(path.parent().unwrap())?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn same_file(left: &Path, right: &Path) -> std::io::Result<bool> {
    let left = std::fs::metadata(left)?;
    let right = std::fs::metadata(right)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    {
        // Canonical paths below cover directory/file symlinks on all targets;
        // Unix additionally exposes stable identities for hardlink aliases.
        let _ = (left, right);
        Ok(false)
    }
}

fn validate_export_paths(
    source: &Path,
    output: &Path,
    document: &Value,
) -> Result<(), Box<dyn Error>> {
    let source = std::fs::canonicalize(source)?;
    let output = resolved_destination(output)?;
    if output.starts_with(&source) || source.starts_with(&output) {
        return Err("--output must be separate from --source, including symlink aliases".into());
    }
    let mut inputs = vec![source.join("BistroExterior.gltf")];
    let mut outputs = vec![
        output.join("BistroExterior.gltf"),
        output.join("SGL-ATTRIBUTION.txt"),
        output.join("decoded.png"),
    ];
    for name in ["BistroExterior.bin", "LICENSE.txt", "README.md"] {
        inputs.push(source.join(name));
        outputs.push(output.join(name));
    }
    for image in document["images"].as_array().ok_or("missing images")? {
        let uri = Path::new(image["uri"].as_str().ok_or("image without URI")?);
        if uri.is_absolute()
            || uri.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            return Err("image URI must stay inside source/output directories".into());
        }
        inputs.push(source.join(uri));
        outputs.push(output.join(uri.with_extension("png")));
    }
    for target in outputs {
        let actual = resolved_destination(&target)?;
        if actual.starts_with(&source) {
            return Err("output file resolves inside source directory".into());
        }
        if target.exists() {
            for input in &inputs {
                if std::fs::canonicalize(input)? == actual || same_file(input, &target)? {
                    return Err("output file aliases an input file".into());
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut source = PathBuf::from("target/bistro/source");
    let mut output = PathBuf::from("target/bistro/prepared");
    let mut ktx = PathBuf::from("ktx");
    let mut maximum = 1024;
    let mut occlusion = OcclusionExport::None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--source" => source = value.into(),
            "--output" => output = value.into(),
            "--ktx" => ktx = value.into(),
            "--max-texture" => maximum = value.parse()?,
            "--occlusion" => {
                occlusion = match value.as_str() {
                    "none" => OcclusionExport::None,
                    "derived" => OcclusionExport::Derived,
                    _ => return Err("--occlusion must be none or derived".into()),
                }
            }
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    if !(1..=16384).contains(&maximum) {
        return Err("--max-texture must be 1..16384".into());
    }
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(source.join("BistroExterior.gltf"))?)?;
    validate_export_paths(&source, &output, &document)?;
    let roles = adapt(&mut document)?;
    std::fs::create_dir_all(output.join("Textures"))?;
    let scratch = output.join("decoded.png");
    let count = document["images"].as_array().unwrap().len();
    for (index, image) in document["images"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let uri = image["uri"].as_str().ok_or("image without URI")?;
        let input = source.join(uri);
        let png = PathBuf::from(uri).with_extension("png");
        let decoded = Command::new(&ktx)
            .args(["extract", "--transcode", "rgba8"])
            .arg(&input)
            .arg(&scratch)
            .status()?;
        if !decoded.success() {
            return Err(format!("KTX extraction failed: {}", input.display()).into());
        }
        let bitmap = image::open(&scratch)?.into_rgba8();
        convert(
            &bitmap,
            roles.get(&index).copied().unwrap_or(Role::Data),
            maximum,
        )
        .save(output.join(&png))?;
        image["uri"] = json!(png.to_str().ok_or("non-UTF8 image URI")?.replace('\\', "/"));
        image["mimeType"] = json!("image/png");
        if index % 25 == 0 || index + 1 == count {
            println!("Bistro textures: {}/{count}", index + 1);
        }
    }
    std::fs::remove_file(scratch)?;
    if occlusion == OcclusionExport::Derived {
        let mut buffers = Vec::new();
        for buffer in document["buffers"].as_array().ok_or("missing buffers")? {
            buffers.push(std::fs::read(
                source.join(buffer["uri"].as_str().ok_or("buffer without URI")?),
            )?);
        }
        derive_occlusion(&mut document, &buffers, &output)?;
    }
    let occlusion_note = match occlusion {
        OcclusionExport::None => {
            "no occlusion binding\nis inferred (source glTF binds none; inspected packed red channels are zero);"
        }
        OcclusionExport::Derived => {
            "--occlusion derived: tiled materials' occlusion\nderived from their normal maps into the packed red channel and bound (not\nsource data); every other material binds none;"
        }
    };
    for name in ["BistroExterior.bin", "LICENSE.txt", "README.md"] {
        std::fs::copy(source.join(name), output.join(name))?;
    }
    document["asset"]["copyright"] = json!(
        "Amazon Lumberyard Bistro (2017), ORCA, CC BY 4.0. glTF conversion: qian-o/GLTF-Assets. SGL adaptations listed in SGL-ATTRIBUTION.txt."
    );
    std::fs::write(
        output.join("SGL-ATTRIBUTION.txt"),
        format!(
            "Amazon Lumberyard Bistro, Open Research Content Archive (ORCA)\nAmazon Lumberyard, July 2017\nhttps://developer.nvidia.com/orca/amazon-lumberyard-bistro\nCC BY 4.0: https://creativecommons.org/licenses/by/4.0/\nSee LICENSE.txt for the complete licence.\n\nglTF conversion: https://github.com/qian-o/GLTF-Assets\nSource revision: 816a1d6533c55787b3fabc9b75660c0d0c57ebc6\n\nSGL adaptations: Basis/KTX2 decoded with Khronos KTX-Software to PNG;\nTwo-channel normal positive Z reconstructed before filtering; DirectX normal\nY inverted (twelve maps authored green up kept) and resized normals renormalized; packed G/B\nrestored to metallicRoughnessTexture with unit factors; {occlusion_note}\ntextures limited to {maximum} pixels (linear-light, premultiplied-alpha colour resize);\ngeometry, authored transforms, alpha and transmission preserved.\nNo geometry simplification or automatic cache-staleness rejection.\n\nEnvironment: San Giuseppe Bridge by Greg Zaal, HDRIHaven / Poly Haven, CC0.\nhttps://polyhaven.com/a/san_giuseppe_bridge\nhttps://polyhaven.com/license\nPMREM atlas generated with Three.js 0.185.1 (MIT; THREE-LICENSE.txt).\n"
        ),
    )?;
    std::fs::write(
        output.join("BistroExterior.gltf"),
        serde_json::to_vec(&document)?,
    )?;
    println!("Prepared {}", output.join("BistroExterior.gltf").display());
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn source_and_output_aliases_fail_before_creating_or_truncating_files() {
        let root = std::env::temp_dir().join(format!("sgl-bistro-paths-{}", std::process::id()));
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let document = json!({"images":[]});
        for name in [
            "BistroExterior.gltf",
            "BistroExterior.bin",
            "LICENSE.txt",
            "README.md",
        ] {
            std::fs::write(source.join(name), format!("source {name}")).unwrap();
        }
        for output in [source.clone(), root.clone(), source.join("nested")] {
            assert!(
                validate_export_paths(&source, &output, &document)
                    .unwrap_err()
                    .to_string()
                    .contains("separate")
            );
        }
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&source, &alias).unwrap();
            assert!(validate_export_paths(&source, &alias, &document).is_err());
            let output = root.join("output");
            std::fs::create_dir_all(&output).unwrap();
            std::fs::hard_link(source.join("LICENSE.txt"), output.join("LICENSE.txt")).unwrap();
            assert!(
                validate_export_paths(&source, &output, &document)
                    .unwrap_err()
                    .to_string()
                    .contains("aliases")
            );
            std::fs::remove_file(output.join("LICENSE.txt")).unwrap();
            std::os::unix::fs::symlink(source.join("README.md"), output.join("README.md")).unwrap();
            assert!(
                validate_export_paths(&source, &output, &document)
                    .unwrap_err()
                    .to_string()
                    .contains("source directory")
            );
            std::fs::remove_file(output.join("README.md")).unwrap();
            validate_export_paths(&source, &output, &document).unwrap();
        }
        for name in [
            "BistroExterior.gltf",
            "BistroExterior.bin",
            "LICENSE.txt",
            "README.md",
        ] {
            assert_eq!(
                std::fs::read_to_string(source.join(name)).unwrap(),
                format!("source {name}")
            );
        }
        assert!(!source.join("Textures").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    // A wrong extension removal or texture slot survives JSON serialization
    // but is refused or ignored by the real SGL loader. Exercise that boundary
    // with one triangle and the conversion's original material structure.
    #[test]
    fn converted_material_loads_as_metal_rough_without_an_inferred_ao_map() {
        let directory =
            std::env::temp_dir().join(format!("sgl-bistro-material-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let values: [f32; 24] = [
            0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0.,
            0., 1.,
        ];
        std::fs::write(
            directory.join("triangle.bin"),
            bytemuck::cast_slice(&values),
        )
        .unwrap();
        image::RgbaImage::from_pixel(1, 1, Rgba([0, 160, 32, 255]))
            .save(directory.join("packed.png"))
            .unwrap();
        let mut document = json!({
            "asset":{"version":"2.0"},"extensionsRequired":["KHR_texture_basisu"],"extensionsUsed":["KHR_texture_basisu","KHR_materials_specular"],
            "buffers":[{"uri":"triangle.bin","byteLength":96}],
            "bufferViews":[{"buffer":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":36},{"buffer":0,"byteOffset":72,"byteLength":24}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},{"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"}],
            "images":[{"uri":"packed.png"}],"textures":[{"extensions":{"KHR_texture_basisu":{"source":0}}}],
            "materials":[{"name":"converted","pbrMetallicRoughness":{"roughnessFactor":0.55},"extensions":{"KHR_materials_specular":{"specularTexture":{"index":0}}}}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"material":0}]}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0
        });
        adapt(&mut document).unwrap();
        std::fs::write(
            directory.join("scene.gltf"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let loaded = sgl_3d::asset::load(&directory.join("scene.gltf")).unwrap();
        assert!(loaded.ignored.is_empty(), "{:?}", loaded.ignored);
        let material = loaded
            .materials
            .iter()
            .find(|m| m.name == "converted")
            .unwrap();
        assert!(material.mr_texture.is_some());
        assert!(material.occlusion_texture.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn a_directx_normal_toward_negative_y_becomes_positive_y() {
        let image = image::RgbaImage::from_pixel(1, 1, Rgba([128, 37, 218, 255]));
        let result = convert(&image, Role::Normal(Green::Down), 1024);
        let [x, y, z, _] = result.get_pixel(0, 0).0;
        let vector = glam::Vec3::from_array([x, y, z].map(|v| f32::from(v) / 127.5 - 1.));
        assert!(vector.y > 0.69 && vector.z > 0.69 && vector.x.abs() < 0.01);
        assert!((vector.length() - 1.).abs() < 0.01);
    }
    #[test]
    fn green_up_sources_keep_their_green_and_the_rest_are_inverted() {
        // MASTER_Curtains_Normal is authored green up (#910), Vespa_Normal
        // green down, as the conversion's README says of all of them. A
        // two-channel texel leaning up its image in each one's encoding
        // leans up (glTF's +Y) after export.
        let mut document = json!({
            "images":[
                {"uri":"Textures/MASTER_Curtains_Normal.ktx2"},
                {"uri":"Textures/Vespa_Normal.ktx2"}
            ],
            "textures":[
                {"extensions":{"KHR_texture_basisu":{"source":0}}},
                {"extensions":{"KHR_texture_basisu":{"source":1}}}
            ],
            "materials":[
                {"normalTexture":{"index":0}},
                {"normalTexture":{"index":1}}
            ]
        });
        let roles = adapt(&mut document).unwrap();
        for (image, green) in [(0, 218), (1, 37)] {
            let texel = image::RgbaImage::from_pixel(1, 1, Rgba([128, green, 0, 255]));
            let [x, y, z, _] = convert(&texel, roles[&image], 1024).get_pixel(0, 0).0;
            let vector = glam::Vec3::from_array([x, y, z].map(|v| f32::from(v) / 127.5 - 1.));
            assert!(
                vector.y > 0.69 && vector.z > 0.69,
                "image {image}: {vector}"
            );
        }
    }
    #[test]
    fn two_channel_normals_reconstruct_positive_z_before_resizing() {
        let flat = image::RgbaImage::from_pixel(1, 1, Rgba([128, 128, 0, 255]));
        let result = convert(&flat, Role::Normal(Green::Down), 1024);
        let pixel = result.get_pixel(0, 0);
        assert!(pixel[0].abs_diff(128) <= 1 && pixel[1].abs_diff(128) <= 1);
        assert_eq!(pixel[2], 255);
        let tilted = image::RgbaImage::from_pixel(1, 1, Rgba([204, 51, 0, 255]));
        let result = convert(&tilted, Role::Normal(Green::Down), 1024);
        let pixel = result.get_pixel(0, 0);
        assert!(pixel[0].abs_diff(204) <= 1 && pixel[1].abs_diff(204) <= 1);
        assert!((194..=196).contains(&pixel[2])); // sqrt(1 - 0.6² - 0.6²)
        let pair = image::RgbaImage::from_fn(2, 1, |x, _| {
            Rgba([if x == 0 { 204 } else { 51 }, 128, 0, 255])
        });
        let filtered = convert(&pair, Role::Normal(Green::Down), 1);
        assert!(filtered.get_pixel(0, 0)[2] >= 254);
        // The average of (0.8, 0, 0.6) and (0, 0.6, 0.8), then
        // normalized, differs from reconstructing Z from their mean XY.
        let asymmetric = image::RgbaImage::from_fn(2, 1, |x, _| {
            Rgba(if x == 0 {
                [230, 128, 0, 255]
            } else {
                [128, 51, 0, 255]
            })
        });
        let filtered = convert(&asymmetric, Role::Normal(Green::Down), 1);
        let pixel = filtered.get_pixel(0, 0);
        assert!((186..=188).contains(&pixel[0]));
        assert!((171..=173).contains(&pixel[1]));
        assert!((230..=232).contains(&pixel[2]));
        // Lossy XY values can stray beyond the unit disk: keep a finite
        // tangent vector rather than taking a square root of a negative.
        let outside = image::RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 255]));
        let result = convert(&outside, Role::Normal(Green::Down), 1);
        let pixel = result.get_pixel(0, 0);
        assert!((217..=218).contains(&pixel[0]) && (217..=218).contains(&pixel[1]));
        assert_eq!(pixel[2], 128);
    }
    #[test]
    fn colour_resize_filters_light_while_data_resize_filters_values() {
        let image = image::RgbaImage::from_fn(2, 1, |x, _| Rgba([if x == 0 { 0 } else { 255 }; 4]));
        // Opaque black and white average to 0.5 linear light, about 188 sRGB.
        let mut image = image;
        for pixel in image.pixels_mut() {
            pixel[3] = 255;
        }
        let colour = convert(&image, Role::Color, 1);
        let data = convert(&image, Role::Data, 1);
        assert!((187..=189).contains(&colour.get_pixel(0, 0)[0]));
        assert!((127..=129).contains(&data.get_pixel(0, 0)[0]));
    }

    /// A `width` × `height` glTF normal image (+Y up the image) of the
    /// height field whose slopes (dh/dx, dh/dy with y up, in texels) at a
    /// texel `slopes` gives, in 8 bits.
    fn normal_image(
        width: u32,
        height: u32,
        slopes: impl Fn(f64, f64) -> (f64, f64),
    ) -> image::RgbaImage {
        image::RgbaImage::from_fn(width, height, |x, row| {
            let (p, q) = slopes(f64::from(x), f64::from(height - 1 - row));
            let n = DVec3::new(-p, -q, 1.).normalize() * 0.5 + DVec3::splat(0.5);
            Rgba([n.x, n.y, n.z, 1.].map(|v| (v * 255.).round() as u8))
        })
    }

    /// Two cosines over a 64 × 32 texel tile, in texels: one along U, one
    /// across U and V, so a swapped or flipped axis shows.
    fn two_cosines(x: f64, y: f64) -> f64 {
        1.5 * (TAU * 2. * x / 64.).cos() + (TAU * (x / 64. + 3. * y / 32.)).cos()
    }

    /// two_cosines' analytic gradient.
    fn two_cosines_slopes(x: f64, y: f64) -> (f64, f64) {
        let across = (TAU * (x / 64. + 3. * y / 32.)).sin();
        (
            -1.5 * TAU * 2. / 64. * (TAU * 2. * x / 64.).sin() - TAU / 64. * across,
            -TAU * 3. / 32. * across,
        )
    }

    // Plausible defects: a slope's sign flipped, U and V swapped, image
    // rows taken as V up unflipped, or the frequency of an axis scaled by
    // the other's size. The oracle is round-trip identity: a periodic
    // height of two cosines (mean 0 over its tile), written as an 8-bit
    // glTF normal image from its analytic gradient, integrates back to
    // itself within what 8-bit normals resolve. Each defect errs by about
    // the field's amplitude, 2.5 texels; the bound is 0.05 texels. A flat
    // image integrates to a flat height.
    #[test]
    fn a_periodic_height_integrates_back_from_its_normals() {
        let image = normal_image(64, 32, two_cosines_slopes);
        let (p, q) = normal_slopes(&image, 1.);
        let height = integrate(&p, &q, 64);
        let error = (0..64 * 32)
            .map(|i| (height[i] - two_cosines((i % 64) as f64, (i / 64) as f64)).abs())
            .fold(0., f64::max);
        assert!(error <= 0.05, "integrated to within {error} texels");
        let flat = image::RgbaImage::from_pixel(64, 32, Rgba([128, 128, 255, 255]));
        let (p, q) = normal_slopes(&flat, 1.);
        let height = integrate(&p, &q, 64);
        assert!(height.iter().all(|h| h.abs() < 1e-9), "{height:?}");
    }

    // Plausible defects: the horizon's cosine left unsquared, or the share
    // a slice keeps taken through the horizon's sine. The oracle is the closed form: at the floor of a long V-groove
    // whose walls rise at angle α, within the reach, the horizon at azimuth
    // φ from the groove's normal stands at atan(tan α |cos φ|), and the mean
    // of 1 / (1 + tan² α cos² φ) over φ is cos α. Sixteen even azimuths
    // reach it within 3e-4 at 60°; the bound is 1e-3. A flat field's
    // visibility is 1.
    #[test]
    fn a_groove_floor_has_the_analytic_visibility() {
        let width = 256;
        for degrees in [30f64, 45., 60.] {
            let slope = degrees.to_radians().tan();
            // A periodic triangle wave in x: the floor at x 64, the ridge
            // half a tile away.
            let height: Vec<f64> = (0..width * width)
                .map(|i| {
                    let distance = ((i % width) as f64 - 64.).abs();
                    slope * distance.min(width as f64 - distance)
                })
                .collect();
            let floor =
                visibility(&height, width, width / OCCLUSION_REACH_DIVISOR)[10 * width + 64];
            let expected = degrees.to_radians().cos();
            assert!(
                (f64::from(floor) - expected).abs() <= 1e-3,
                "a {degrees}° groove's floor sees {floor}, cos α {expected}"
            );
        }
        let flat = visibility(&vec![0.; 64 * 64], 64, 4);
        assert!(flat.iter().all(|&v| v == 1.), "{flat:?}");
    }

    // Plausible defects: a map with curl integrated and bound regardless,
    // the integrability bound never reached or always reached, a material
    // whose UVs stay in [0, 1] taken as tiled, the binding left off, on the
    // wrong texture or at another strength, or the metallic-roughness image's
    // green and blue overwritten. The oracles: a periodic normal image whose
    // slope along x varies only with y has curl, so no height field
    // reproduces it, and its material binds no occlusion; a tiled material
    // with an integrable map binds its metallic-roughness image as its
    // occlusion at strength 1, keeping that image's roughness and metalness;
    // and a material whose UVs stay in [0, 1] binds none. Observed through
    // SGL's loader.
    #[test]
    fn derived_occlusion_binds_integrable_tiled_maps_only() {
        let directory =
            std::env::temp_dir().join(format!("sgl-bistro-occlusion-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let positions = [0f32, 0., 0., 1., 0., 0., 0., 1., 0.];
        let normals = [0f32, 0., 1., 0., 0., 1., 0., 0., 1.];
        let tiled_uvs = [0f32, 0., 4., 0., 0., 4.];
        let unit_uvs = [0f32, 0., 1., 0., 0., 1.];
        let bin: Vec<u8> = [&positions[..], &normals, &tiled_uvs, &unit_uvs]
            .concat()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        std::fs::write(directory.join("scene.bin"), &bin).unwrap();
        let curl = |_: f64, y: f64| (0.5 * (TAU * y / 64.).sin(), 0.);
        // two_cosines stretched over a 64-texel square: h(x, y / 2).
        let scaled = |x: f64, y: f64| {
            let (p, q) = two_cosines_slopes(x, y * 0.5);
            (p, q * 0.5)
        };
        for (index, image) in [
            normal_image(64, 64, scaled),
            normal_image(64, 64, curl),
            normal_image(64, 64, scaled),
        ]
        .into_iter()
        .enumerate()
        {
            image
                .save(directory.join(format!("normal{index}.png")))
                .unwrap();
            image::RgbaImage::from_pixel(64, 64, Rgba([0, 160, 32, 255]))
                .save(directory.join(format!("mr{index}.png")))
                .unwrap();
        }
        let material = |name: &str, index: usize| {
            json!({"name": name, "normalTexture": {"index": 2 * index},
                "pbrMetallicRoughness": {"metallicRoughnessTexture": {"index": 2 * index + 1}}})
        };
        let primitive = |material: usize, uvs: usize| json!({"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":uvs},"material":material}]});
        let mut document = json!({
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"scene.bin","byteLength":bin.len()}],
            "bufferViews":[{"buffer":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":36},{"buffer":0,"byteOffset":72,"byteLength":24},{"buffer":0,"byteOffset":96,"byteLength":24}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},{"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"},{"bufferView":3,"componentType":5126,"count":3,"type":"VEC2"}],
            "images":(0..3).flat_map(|i| [json!({"uri": format!("normal{i}.png")}), json!({"uri": format!("mr{i}.png")})]).collect::<Vec<_>>(),
            "textures":(0..6).map(|i| json!({"source": i})).collect::<Vec<_>>(),
            "materials":[material("tiled", 0), material("curl", 1), material("prop", 2)],
            "meshes":[primitive(0, 2), primitive(1, 2), primitive(2, 3)],
            "nodes":[{"mesh":0},{"mesh":1},{"mesh":2}],"scenes":[{"nodes":[0,1,2]}],"scene":0
        });
        derive_occlusion(&mut document, &[bin], &directory).unwrap();
        std::fs::write(
            directory.join("scene.gltf"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let loaded = sgl_3d::asset::load(&directory.join("scene.gltf")).unwrap();
        assert!(loaded.ignored.is_empty(), "{:?}", loaded.ignored);
        let material = |name| loaded.materials.iter().find(|m| m.name == name).unwrap();
        let tiled = material("tiled");
        assert!(tiled.mr_texture.is_some());
        assert_eq!(tiled.occlusion_texture, tiled.mr_texture);
        assert_eq!(tiled.occlusion_strength, 1.);
        assert!(material("curl").occlusion_texture.is_none());
        assert!(material("prop").occlusion_texture.is_none());
        let packed = image::open(directory.join("mr0.png")).unwrap().into_rgba8();
        assert!(
            packed
                .pixels()
                .all(|p| p[0] > 0 && p[1] == 160 && p[2] == 32),
            "{:?}",
            packed.get_pixel(0, 0)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
