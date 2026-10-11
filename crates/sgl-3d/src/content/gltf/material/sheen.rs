//! A material's `KHR_materials_sheen`: the sheen layer cloth takes.
use super::super::super::asset::Result;
use super::{GLTF_PROPERTY_KEYS, color_factor, extension_texture, unit_factor};

/// A material's sheen (KHR_materials_sheen).
pub(super) struct Sheen<'a> {
    pub color: [f32; 3],
    pub roughness: f32,
    pub color_texture: Option<gltf::Texture<'a>>,
    pub roughness_texture: Option<gltf::Texture<'a>>,
}

// Authority: Khronos glTF acfcbe65e40c53d6d3aa55a7299982bf2c01c75d,
// KHR_materials_sheen/README.md and schema: sheenColorFactor three numbers
// in 0..1, default black; sheenRoughnessFactor in 0..1, default 0;
// sheenColorTexture and sheenRoughnessTexture.
pub(super) fn read_sheen<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<Sheen<'a>> {
    let mut sheen = Sheen {
        color: [0.; 3],
        roughness: 0.,
        color_texture: None,
        roughness_texture: None,
    };
    let Some(value) = material.extension_value("KHR_materials_sheen") else {
        return Ok(sheen);
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: sheen {message}");
    let object = value
        .as_object()
        .ok_or_else(|| error("extension must be an object"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "sheenColorFactor"
                | "sheenColorTexture"
                | "sheenRoughnessFactor"
                | "sheenRoughnessTexture"
        ) && !GLTF_PROPERTY_KEYS.contains(&key.as_str())
        {
            return Err(error(&format!("unsupported property {key}")).into());
        }
    }
    if material.unlit() {
        return Err(error("cannot be combined with KHR_materials_unlit").into());
    }
    if let Some(color) = value.get("sheenColorFactor") {
        sheen.color = color_factor(color, 1.)
            .ok_or_else(|| error("sheenColorFactor must be three numbers in 0..1"))?;
    }
    sheen.roughness = unit_factor(value, "sheenRoughnessFactor", 0.)
        .ok_or_else(|| error("sheenRoughnessFactor must be a number in 0..1"))?;
    sheen.color_texture = extension_texture(value, "sheenColorTexture", document, &error)?;
    sheen.roughness_texture = extension_texture(value, "sheenRoughnessTexture", document, &error)?;
    Ok(sheen)
}
