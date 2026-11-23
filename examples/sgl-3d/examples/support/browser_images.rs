//! The browser lane's check of the browser's image decoding
//! (`asset::load_slice_in_browser`): a GLB built here holds PNG and JPEG
//! fixtures, each a map of a material, and is loaded into a scene that
//! keeps ray data. The oracle is the `image` crate's decode of the same
//! bytes. Each PNG's texels, as the texture the map samples and as its
//! image's record in the ray source hold them, must equal it byte for byte:
//! alpha from 0 to 255 under colours alpha does not scale, one with gamma
//! and chromaticity chunks a colour-managed decode would apply, widths
//! whose rows the ray copy takes alone and together, and one sampled as
//! linear data, whose texture and ray copy are its own, and one sampled on
//! the second UV set through a `KHR_texture_transform`, which moves where a
//! map samples, never what its texture and ray record hold. A JPEG's must lie
//! within `JPEG_TOLERANCE` of it, with its chroma whole and subsampled. A
//! JPEG whose EXIF orientation turns it 180° must load unturned, as the
//! `image` crate decodes it; decals must refuse a browser image; and a
//! PNG whose data fails its checksum must fail with the native loader's
//! error.

use sgl_3d::{
    Scene, SceneError,
    asset::{self, Image, LoadOptions},
    diagnostics::DiagnosticMap,
};

/// The largest difference allowed between two JPEG decoders' texels.
/// ITU-T T.83 lets a compliant decoder's samples differ from the reference
/// decoder's by 1 in each of Y, Cb and Cr, so two compliant decoders by 2;
/// JFIF's YCbCr to RGB (`B = Y + 1.772 Cb` is the widest) turns that into
/// at most 2 + 2 × 1.772 = 5.5 in a channel, and the two decoders' rounding
/// to 8 bits adds up to 1: 6 in whole levels. Chroma upsampling, which T.81
/// leaves to the decoder, is outside that; the 4:2:0 fixture is held to it
/// because both decoders upsample with libjpeg's triangle filter
/// (libjpeg-turbo's `h2v2_fancy_upsample`, zune-jpeg 0.5.15's
/// `upsampler/scalar.rs`), so a decoder upsampling otherwise could fail it.
const JPEG_TOLERANCE: u8 = 6;

/// A fixed sequence of bytes.
fn noise(seed: &mut u32) -> u8 {
    *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    (*seed >> 24) as u8
}

/// RGBA texels whose alpha runs through 0, 1, 2, 128, 254 and 255 under
/// colours unrelated to it, so a premultiplied decode changes them.
fn translucent(width: u32, height: u32) -> image::RgbaImage {
    let mut seed = 7;
    image::RgbaImage::from_fn(width, height, |x, y| {
        let alpha = [0, 1, 2, 128, 254, 255][((x + y * width) % 6) as usize];
        image::Rgba([noise(&mut seed), noise(&mut seed), noise(&mut seed), alpha])
    })
}

fn png(image: &image::DynamicImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// PNG's chunk CRC: CRC-32 (ISO 3309) over the chunk's type and data.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `png` with `chunks` (type, data) after its IHDR, which ends 33 bytes in.
fn with_chunks(png: &[u8], chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut out = png[..33].to_vec();
    for (kind, data) in chunks {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(*kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out.extend_from_slice(&png[33..]);
    out
}

/// A gamma of 1.0 and Rec. 2020-like primaries (PNG 11.3.3.2 and 11.3.3.3,
/// values times 100000), which a colour-managed decode would convert from.
fn colour_chunks() -> Vec<(&'static [u8; 4], Vec<u8>)> {
    let primaries: [u32; 8] = [31270, 32900, 70800, 29200, 17000, 79700, 13100, 4600];
    vec![
        (b"gAMA", 100_000u32.to_be_bytes().to_vec()),
        (
            b"cHRM",
            primaries.iter().flat_map(|v| v.to_be_bytes()).collect(),
        ),
    ]
}

/// A JPEG of slowly varying colour under fine luma detail, its chroma
/// whole: the `image` crate's encoder samples every component 1×1.
fn jpeg() -> Vec<u8> {
    let mut seed = 11;
    let texels = image::RgbImage::from_fn(48, 16, |x, y| {
        let detail = noise(&mut seed) / 4;
        image::Rgb([
            (x * 5) as u8 / 2 + detail,
            (y * 12) as u8 / 2 + detail,
            120 + detail,
        ])
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
        .encode_image(&texels)
        .unwrap();
    bytes
}

/// A 64×32 JPEG with 4:2:0 chroma (sampling factors 2×2, 1×1, 1×1):
/// libjpeg-turbo 3.2.0's `cjpeg -quality 90 -sample 2x2` of a pattern of
/// smooth colour under fine luma detail with a hard colour edge at x = 32.
const SUBSAMPLED_JPEG: &[u8] = include_bytes!("subsampled.jpg");

/// `jpeg` with an EXIF APP1 segment after its SOI marker saying its
/// texels are turned 180° (orientation 3; EXIF 2.32 4.6.4, TIFF 6.0
/// big-endian, one IFD entry), which keeps its size, so only reading the
/// orientation tells the browser's decode from the `image` crate's.
fn oriented(jpeg: &[u8]) -> Vec<u8> {
    let tiff = [
        b'M', b'M', 0, 42, 0, 0, 0, 8, // header: first IFD at 8
        0, 1, // one entry
        0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 3, 0, 0, // Orientation, SHORT, 1: 3
        0, 0, 0, 0, // no next IFD
    ];
    let mut segment = b"Exif\0\0".to_vec();
    segment.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&(segment.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(&segment);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// How a fixture's material samples it.
#[derive(Clone, Copy)]
enum Slot {
    /// As its base colour map.
    Colour,
    /// As its metallic-roughness map.
    Data,
    /// As its base colour map, on `TEXCOORD_1` through [`TRANSFORM`].
    SecondSetTransformed,
}

impl Slot {
    fn map(self) -> DiagnosticMap {
        match self {
            Self::Colour | Self::SecondSetTransformed => DiagnosticMap::BaseColor,
            Self::Data => DiagnosticMap::MetallicRoughness,
        }
    }
}

/// `Slot::SecondSetTransformed`'s `KHR_texture_transform`.
const TRANSFORM: asset::UvTransform = asset::UvTransform {
    offset: [0.25, 0.5],
    rotation: 0.5,
    scale: [2., 3.],
};

/// A GLB of one triangle per image, each drawn with a material whose map
/// in `slot` is that image, embedded with its MIME type. Every triangle has
/// both UV sets.
fn glb(images: &[(&[u8], &str, Slot)]) -> Vec<u8> {
    // Positions, normals and UVs of one triangle.
    let mut bin: Vec<u8> = [
        [0f32, 0., 0.],
        [1., 0., 0.],
        [0., 1., 0.],
        [0., 0., 1.],
        [0., 0., 1.],
        [0., 0., 1.],
    ]
    .iter()
    .flatten()
    .chain([0f32, 1., 1., 1., 0., 0.].iter())
    .flat_map(|v| v.to_le_bytes())
    .collect();
    let mut views = vec![
        serde_json::json!({"buffer": 0, "byteOffset": 0, "byteLength": 36}),
        serde_json::json!({"buffer": 0, "byteOffset": 36, "byteLength": 36}),
        serde_json::json!({"buffer": 0, "byteOffset": 72, "byteLength": 24}),
    ];
    for (bytes, _, _) in images {
        views.push(
            serde_json::json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len()}),
        );
        bin.extend_from_slice(bytes);
        bin.resize(bin.len().next_multiple_of(4), 0);
    }
    let count = images.len();
    let json = serde_json::json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": (0..count).map(|i| serde_json::json!({
            "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2, "TEXCOORD_1": 2},
            "material": i})).collect::<Vec<_>>()}],
        "extensionsUsed": ["KHR_texture_transform"],
        "materials": images.iter().enumerate().map(|(i, (_, _, slot))| match slot {
            Slot::Colour => serde_json::json!({
                "pbrMetallicRoughness": {"baseColorTexture": {"index": i}}}),
            Slot::Data => serde_json::json!({
                "pbrMetallicRoughness": {"metallicRoughnessTexture": {"index": i}}}),
            Slot::SecondSetTransformed => serde_json::json!({
                "pbrMetallicRoughness": {"baseColorTexture": {"index": i, "texCoord": 1,
                    "extensions": {"KHR_texture_transform": {"offset": TRANSFORM.offset,
                        "rotation": TRANSFORM.rotation, "scale": TRANSFORM.scale}}}}}),
        }).collect::<Vec<_>>(),
        "textures": (0..count).map(|i| serde_json::json!({"source": i})).collect::<Vec<_>>(),
        "images": images.iter().enumerate().map(|(i, (_, mime, _))| serde_json::json!({
            "bufferView": 3 + i, "mimeType": mime})).collect::<Vec<_>>(),
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0, 0, 0], "max": [1, 1, 0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
        ],
        "bufferViews": views,
        "buffers": [{"byteLength": bin.len()}],
    });
    let mut json = serde_json::to_vec(&json).unwrap();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut out = b"glTF".to_vec();
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

/// The largest difference between two texel lists of one length.
fn largest_difference(a: &[u8], b: &[u8]) -> Result<u8, String> {
    if a.len() != b.len() {
        return Err(format!("{} bytes against {}", a.len(), b.len()));
    }
    Ok(a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0))
}

pub async fn check(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<String, String> {
    let translucent = image::DynamicImage::ImageRgba8(translucent(37, 5));
    let mut seed = 3;
    let opaque = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 8, |_, _| {
        image::Rgb([noise(&mut seed), noise(&mut seed), noise(&mut seed)])
    }));
    let jpeg = jpeg();
    let oriented = oriented(&jpeg);
    let (colour, data) = (Slot::Colour, Slot::Data);
    // Name, bytes, MIME type, the map sampling it, and whether the browser
    // decodes it.
    let fixtures: [(&str, Vec<u8>, &str, Slot, bool); 8] = [
        // 37 texels: rows of 148 bytes, copied into the ray source alone.
        (
            "translucent PNG",
            png(&translucent),
            "image/png",
            colour,
            true,
        ),
        // 64 texels: rows of 256 bytes, copied together.
        ("opaque PNG", png(&opaque), "image/png", colour, true),
        (
            "PNG with gAMA and cHRM",
            with_chunks(&png(&translucent), &colour_chunks()),
            "image/png",
            colour,
            true,
        ),
        // A texture of its own, linear, and the ray copy taken from it.
        (
            "translucent PNG as data",
            png(&translucent),
            "image/png",
            data,
            true,
        ),
        ("4:4:4 JPEG", jpeg, "image/jpeg", colour, true),
        (
            "4:2:0 JPEG",
            SUBSAMPLED_JPEG.to_vec(),
            "image/jpeg",
            colour,
            true,
        ),
        ("EXIF-turned JPEG", oriented, "image/jpeg", colour, false),
        (
            "translucent PNG on UV1 through a transform",
            png(&translucent),
            "image/png",
            Slot::SecondSetTransformed,
            true,
        ),
    ];
    let file = glb(&fixtures
        .iter()
        .map(|(_, bytes, mime, map, _)| (bytes.as_slice(), *mime, *map))
        .collect::<Vec<_>>());
    let loaded = asset::load_slice_in_browser(&file, LoadOptions::default())
        .await
        .map_err(|e| format!("load_slice_in_browser: {e}"))?;
    let mut scene = Scene::new(device, queue);
    let materials = scene
        .add_materials(device, queue, &loaded.materials, &loaded.images)
        .map_err(|e| format!("add_materials: {e}"))?;
    let mut jpeg_differences = Vec::new();
    for (index, (name, bytes, _, slot, browser)) in fixtures.iter().enumerate() {
        let map = slot.map();
        if let Slot::SecondSetTransformed = slot {
            let texture = loaded.materials[index].base_texture;
            if texture.map(|t| (t.uv_set, t.transform)) != Some((asset::UvSet::Uv1, TRANSFORM)) {
                return Err(format!("{name}: loaded as {texture:?}"));
            }
        }
        let expected = image::load_from_memory(bytes)
            .map_err(|e| format!("{name}: the image crate: {e}"))?
            .into_rgba8();
        match &loaded.images[index] {
            Image::Browser(_) if *browser => {}
            Image::Rgba8(texels) if !*browser && texels == &expected => {}
            other => {
                return Err(format!(
                    "{name}: loaded as {:?}",
                    std::mem::discriminant(other)
                ));
            }
        }
        let held =
            sgl_3d::diagnostics::read_map_async(device, queue, &scene, materials[index], map)
                .await
                .map_err(|e| format!("{name}: {e}"))?
                .ok_or_else(|| format!("{name}: no {map:?} map"))?;
        let rays = held
            .rays
            .ok_or_else(|| format!("{name}: no ray source record"))?;
        let mut largest = 0;
        for (place, texels) in [("texture", &held.texture), ("ray source", &rays)] {
            let difference = largest_difference(texels, expected.as_raw())
                .map_err(|e| format!("{name} in its {place}: {e}"))?;
            largest = largest.max(difference);
            let allowed = if name.contains("JPEG") {
                JPEG_TOLERANCE
            } else {
                0
            };
            if difference > allowed {
                return Err(format!(
                    "{name} in its {place} differs from the image crate's decode by up to {difference}, more than {allowed}"
                ));
            }
        }
        if name.contains("JPEG") && *browser {
            jpeg_differences.push(format!("{name} within {largest}"));
        }
    }
    // Decals project CPU texels, which a browser image does not have.
    match scene.add_decal_image(loaded.images[0].clone()) {
        Err(SceneError::MissingDecodedImage) => {}
        other => return Err(format!("a decal took a browser image: {other:?}")),
    }
    // A PNG whose image data fails its CRC: the browser refuses it, and the
    // load reports the error the native loader does.
    let mut corrupt = png(&opaque);
    let at = corrupt.len() - 16;
    corrupt[at] ^= 0xff;
    let file = glb(&[(&corrupt, "image/png", Slot::Colour)]);
    let native = asset::load_slice(&file)
        .err()
        .ok_or("the native loader accepted a corrupt PNG")?
        .to_string();
    let browser = asset::load_slice_in_browser(&file, LoadOptions::default())
        .await
        .err()
        .ok_or("load_slice_in_browser accepted a corrupt PNG")?
        .to_string();
    if browser != native {
        return Err(format!(
            "a corrupt PNG failed with {browser:?}, natively with {native:?}"
        ));
    }
    Ok(format!(
        "5 PNGs equal the image crate's decode in texture and ray source, as colour, as data and on UV1 through a transform; {} (allowed {JPEG_TOLERANCE}); EXIF-turned JPEG decoded unturned; decals refuse a browser image; corrupt PNG refused as natively",
        jpeg_differences.join(", ")
    ))
}
