//! CPU-only inspection of the actual embedded Bistro export through SGL's
//! ordinary loader. Optional diagnostic; never part of runtime or export.
use std::{collections::BTreeMap, error::Error, mem::size_of, path::PathBuf, sync::Mutex};

use serde_json::{Value, json};
use sgl_3d::asset::{self, CompressedImage, Image, ImageSource, LoadOptions, Vertex};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(
        args.next()
            .ok_or("usage: bistro_asset_stats PACKAGE_DIR [ASSET.glb] [--omit-companions]")?,
    );
    let mut asset_file = String::from("BistroExterior.glb");
    let mut omit_companions = false;
    for arg in args {
        if arg == "--omit-companions" {
            omit_companions = true;
        } else {
            asset_file = arg;
        }
    }
    let bytes = std::fs::read(directory.join(asset_file))?;
    let mut supplied = BTreeMap::new();
    let manifest = directory.join("compressed-images.json");
    if manifest.exists() {
        let manifest: Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
        for entry in manifest["images"]
            .as_array()
            .ok_or("missing compressed images")?
        {
            let data =
                std::fs::read(directory.join(entry["ktx2"].as_str().ok_or("missing KTX2")?))?;
            let image = if let Some(decoded) = entry["decoded_level_0"]
                .as_str()
                .filter(|_| !omit_companions)
            {
                CompressedImage::from_ktx2_with_decoded_level_0(
                    &data,
                    image::open(directory.join(decoded))?.into_rgba8(),
                )?
            } else {
                CompressedImage::from_ktx2(&data)?
            };
            supplied.insert(
                entry["index"].as_u64().ok_or("missing image index")? as usize,
                Image::Compressed(image),
            );
        }
    }
    let supplied = Mutex::new(supplied);
    let images = |image: asset::GltfImage<'_>| -> asset::Result<ImageSource> {
        Ok(supplied
            .lock()
            .unwrap()
            .remove(&image.index)
            .map_or(ImageSource::Decode, ImageSource::Supplied))
    };
    let asset = asset::load_slice_with_options(
        &bytes,
        LoadOptions {
            images: Some(&images),
            ..Default::default()
        },
    )?;
    drop(bytes);
    let vertices: usize = asset.meshes.iter().map(|mesh| mesh.vertices.len()).sum();
    let indices: usize = asset.meshes.iter().map(|mesh| mesh.indices.len()).sum();
    let geometry_bytes = vertices * size_of::<Vertex>() + indices * size_of::<u32>();
    let image_bytes: usize = asset
        .images
        .iter()
        .map(|image| match image {
            Image::Rgba8(image) => image.as_raw().len(),
            Image::Compressed(image) => {
                image.levels.iter().map(Vec::len).sum::<usize>()
                    + image
                        .decoded_level_0
                        .as_ref()
                        .map_or(0, |image| image.as_raw().len())
            }
            Image::Rgba8Mips(_) => {
                unreachable!("export inspection does not supply prepared RGBA mips")
            }
        })
        .sum();
    let path = directory.join("asset-stats.json");
    let mut stats: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let inspection = json!({
        "loader": "sgl_3d::asset::load_slice_with_options",
        "vertices": vertices,
        "indices": indices,
        "triangles": indices / 3,
        "vertexStrideBytes": size_of::<Vertex>(),
        "batchedMeshes": asset.meshes.len(),
        "materialsIncludingDefault": asset.materials.len(),
        "images": asset.images.len(),
        "ignored": format!("{:?}", asset.ignored),
        "note": "Geometry and retained image bytes only (RGBA pixels or compressed blocks plus companions), not process peak or animation/skin metadata. Browser has the same loader with lower default caps; this inspection itself is native."
    });
    if omit_companions {
        let mut no_companions = inspection.clone();
        no_companions["sceneRequirement"] = json!(
            "ETC2/ASTC without companions require a RayData::Disabled scene; this CPU-only inspection does not install a GPU scene"
        );
        stats["loaderInspectionWithoutCompanions"] = no_companions;
        stats["retainedImageBytesWithoutCompanions"] = json!(image_bytes);
        stats["decodedGeometryAndImageBytesWithoutCompanions"] =
            json!(geometry_bytes + image_bytes);
    } else {
        stats["decodedGeometryBytes"] = json!(geometry_bytes);
        stats["loadedDecodedImageBytes"] = json!(image_bytes);
        stats["decodedGeometryAndImageBytes"] = json!(geometry_bytes + image_bytes);
        stats["loaderInspection"] = inspection.clone();
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&stats)?)?;
    println!("{inspection}");
    println!("decoded geometry {geometry_bytes} bytes; images {image_bytes} bytes");
    Ok(())
}
