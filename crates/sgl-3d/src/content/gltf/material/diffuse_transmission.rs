//! A material's `KHR_materials_diffuse_transmission`: the diffuse light it
//! passes to its other side.
use super::super::super::asset::Result;
use super::{GLTF_PROPERTY_KEYS, color_factor, extension_texture, unit_factor};

/// A material's diffuse transmission (KHR_materials_diffuse_transmission).
pub(super) struct DiffuseTransmission<'a> {
    pub factor: f32,
    pub color: [f32; 3],
    pub texture: Option<gltf::Texture<'a>>,
    pub color_texture: Option<gltf::Texture<'a>>,
}

// Authority: Khronos glTF acfcbe65e40c53d6d3aa55a7299982bf2c01c75d,
// KHR_materials_diffuse_transmission/README.md and schema:
// diffuseTransmissionFactor in 0..1, default 0;
// diffuseTransmissionColorFactor three nonnegative numbers, default white;
// diffuseTransmissionTexture and diffuseTransmissionColorTexture.
pub(super) fn read_diffuse_transmission<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<DiffuseTransmission<'a>> {
    let mut transmission = DiffuseTransmission {
        factor: 0.,
        color: [1.; 3],
        texture: None,
        color_texture: None,
    };
    let Some(value) = material.extension_value("KHR_materials_diffuse_transmission") else {
        return Ok(transmission);
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: diffuse transmission {message}");
    let object = value
        .as_object()
        .ok_or_else(|| error("extension must be an object"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "diffuseTransmissionFactor"
                | "diffuseTransmissionTexture"
                | "diffuseTransmissionColorFactor"
                | "diffuseTransmissionColorTexture"
        ) && !GLTF_PROPERTY_KEYS.contains(&key.as_str())
        {
            return Err(error(&format!("unsupported property {key}")).into());
        }
    }
    if material.unlit() {
        return Err(error("cannot be combined with KHR_materials_unlit").into());
    }
    transmission.factor = unit_factor(value, "diffuseTransmissionFactor", 0.)
        .ok_or_else(|| error("diffuseTransmissionFactor must be a number in 0..1"))?;
    if let Some(color) = value.get("diffuseTransmissionColorFactor") {
        transmission.color = color_factor(color, f32::INFINITY).ok_or_else(|| {
            error("diffuseTransmissionColorFactor must be three finite nonnegative numbers")
        })?;
    }
    transmission.texture =
        extension_texture(value, "diffuseTransmissionTexture", document, &error)?;
    transmission.color_texture =
        extension_texture(value, "diffuseTransmissionColorTexture", document, &error)?;
    Ok(transmission)
}
