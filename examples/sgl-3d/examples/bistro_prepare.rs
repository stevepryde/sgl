//! Explicit Bistro content conversion; no runtime loader exceptions.
//! See ../BISTRO.md. KTX-Software performs Basis decoding; SGL's image
//! dependency resizes and corrects channels before exporting core glTF.
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
    process::Command,
};

use image::{ImageBuffer, Rgba, Rgba32FImage, imageops::FilterType};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Color,
    Normal,
    Data,
}

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
    for material in document["materials"].as_array().unwrap() {
        for (texture, role) in [
            (
                &material["pbrMetallicRoughness"]["baseColorTexture"],
                Role::Color,
            ),
            (&material["emissiveTexture"], Role::Color),
            (&material["normalTexture"], Role::Normal),
            (
                &material["pbrMetallicRoughness"]["metallicRoughnessTexture"],
                Role::Data,
            ),
            (&material["occlusionTexture"], Role::Data),
        ] {
            if let Some(image) = image_index(document, texture)
                && let Some(previous) = roles.insert(image, role)
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
    let normal_rg = role == Role::Normal && input.pixels().all(|p| p[2] == 0);
    let mut linear: Rgba32FImage = ImageBuffer::from_fn(input.width(), input.height(), |x, y| {
        let mut pixel = input.get_pixel(x, y).0.map(|v| f32::from(v) / 255.);
        if role == Role::Color {
            for channel in 0..3 {
                pixel[channel] = srgb_to_linear(pixel[channel]) * pixel[3];
            }
        } else if role == Role::Normal {
            let nx = pixel[0] * 2. - 1.;
            let ny = 1. - pixel[1] * 2.; // DirectX Y-down to glTF Y-up.
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
        } else if role == Role::Normal {
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
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--source" => source = value.into(),
            "--output" => output = value.into(),
            "--ktx" => ktx = value.into(),
            "--max-texture" => maximum = value.parse()?,
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
    for name in ["BistroExterior.bin", "LICENSE.txt", "README.md"] {
        std::fs::copy(source.join(name), output.join(name))?;
    }
    document["asset"]["copyright"] = json!(
        "Amazon Lumberyard Bistro (2017), ORCA, CC BY 4.0. glTF conversion: qian-o/GLTF-Assets. SGL adaptations listed in SGL-ATTRIBUTION.txt."
    );
    std::fs::write(
        output.join("SGL-ATTRIBUTION.txt"),
        format!(
            "Amazon Lumberyard Bistro, Open Research Content Archive (ORCA)\nAmazon Lumberyard, July 2017\nhttps://developer.nvidia.com/orca/amazon-lumberyard-bistro\nCC BY 4.0: https://creativecommons.org/licenses/by/4.0/\nSee LICENSE.txt for the complete licence.\n\nglTF conversion: https://github.com/qian-o/GLTF-Assets\nSource revision: 816a1d6533c55787b3fabc9b75660c0d0c57ebc6\n\nSGL adaptations: Basis/KTX2 decoded with Khronos KTX-Software to PNG;\nTwo-channel normal positive Z reconstructed before filtering; DirectX normal\nY inverted and resized normals renormalized; packed G/B\nrestored to metallicRoughnessTexture with unit factors; no occlusion binding\nis inferred (source glTF binds none; inspected packed red channels are zero);\ntextures limited to {maximum} pixels (linear-light, premultiplied-alpha colour resize);\ngeometry, authored transforms, alpha and transmission preserved.\nNo geometry simplification or automatic cache-staleness rejection.\n\nEnvironment: San Giuseppe Bridge by Greg Zaal, HDRIHaven / Poly Haven, CC0.\nhttps://polyhaven.com/a/san_giuseppe_bridge\nhttps://polyhaven.com/license\nPMREM atlas generated with Three.js 0.185.1 (MIT; THREE-LICENSE.txt).\n"
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
        let result = convert(&image, Role::Normal, 1024);
        let [x, y, z, _] = result.get_pixel(0, 0).0;
        let vector = glam::Vec3::from_array([x, y, z].map(|v| f32::from(v) / 127.5 - 1.));
        assert!(vector.y > 0.69 && vector.z > 0.69 && vector.x.abs() < 0.01);
        assert!((vector.length() - 1.).abs() < 0.01);
    }
    #[test]
    fn two_channel_normals_reconstruct_positive_z_before_resizing() {
        let flat = image::RgbaImage::from_pixel(1, 1, Rgba([128, 128, 0, 255]));
        let result = convert(&flat, Role::Normal, 1024);
        let pixel = result.get_pixel(0, 0);
        assert!(pixel[0].abs_diff(128) <= 1 && pixel[1].abs_diff(128) <= 1);
        assert_eq!(pixel[2], 255);
        let tilted = image::RgbaImage::from_pixel(1, 1, Rgba([204, 51, 0, 255]));
        let result = convert(&tilted, Role::Normal, 1024);
        let pixel = result.get_pixel(0, 0);
        assert!(pixel[0].abs_diff(204) <= 1 && pixel[1].abs_diff(204) <= 1);
        assert!((194..=196).contains(&pixel[2])); // sqrt(1 - 0.6² - 0.6²)
        let pair = image::RgbaImage::from_fn(2, 1, |x, _| {
            Rgba([if x == 0 { 204 } else { 51 }, 128, 0, 255])
        });
        let filtered = convert(&pair, Role::Normal, 1);
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
        let filtered = convert(&asymmetric, Role::Normal, 1);
        let pixel = filtered.get_pixel(0, 0);
        assert!((186..=188).contains(&pixel[0]));
        assert!((171..=173).contains(&pixel[1]));
        assert!((230..=232).contains(&pixel[2]));
        // Lossy XY values can stray beyond the unit disk: keep a finite
        // tangent vector rather than taking a square root of a negative.
        let outside = image::RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 255]));
        let result = convert(&outside, Role::Normal, 1);
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
}
