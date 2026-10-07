//! A material's `KHR_materials_transmission`, `KHR_materials_volume` and
//! `KHR_materials_dispersion`: the light it transmits from behind it.
use super::super::super::asset::Result;
use super::extension_texture;

/// A material extension's number at `key` of `value`, `default` where it is
/// absent, which `valid` must hold for; `error` words a refusal.
fn number(
    value: &serde_json::Value,
    key: &str,
    default: f64,
    valid: impl Fn(f64) -> bool,
    error: &dyn Fn(&str) -> String,
) -> Result<f32> {
    let number = value
        .get(key)
        .map_or(Some(default), serde_json::Value::as_f64);
    match number {
        Some(number) if valid(number) => Ok(number as f32),
        _ => Err(error(&format!("{key} is out of range")).into()),
    }
}

/// Refuses a property of `value`, an extension's object, that `known` does
/// not list (or `extras`), as SGL3D's other material extensions do;
/// `error` words the refusal.
fn known_properties(
    value: &serde_json::Value,
    known: &[&str],
    error: &dyn Fn(&str) -> String,
) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| error("extension must be an object"))?;
    match object
        .keys()
        .find(|key| *key != "extras" && !known.contains(&key.as_str()))
    {
        Some(key) => Err(error(&format!("unsupported property {key}")).into()),
        None => Ok(()),
    }
}

/// A material's `KHR_materials_transmission`: its factor and map.
pub(super) struct Transmission<'a> {
    pub factor: f32,
    pub texture: Option<gltf::Texture<'a>>,
}

// Authority: Khronos glTF acfcbe65e40c53d6d3aa55a7299982bf2c01c75d,
// extensions/2.0/Khronos/KHR_materials_transmission/README.md and schema:
// transmissionFactor in 0..1, default 0; transmissionTexture's red channel.
// Read as three.js r185's GLTFLoader reads it
// (examples/jsm/loaders/GLTFLoader.js 1150-1196). An unlit material takes
// no light to transmit and is refused, as with anisotropy.
pub(super) fn read_transmission<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<Transmission<'a>> {
    let Some(value) = material.extension_value("KHR_materials_transmission") else {
        return Ok(Transmission {
            factor: 0.,
            texture: None,
        });
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: transmission {message}");
    known_properties(
        value,
        &["transmissionFactor", "transmissionTexture"],
        &error,
    )?;
    if material.unlit() {
        return Err(error("cannot be combined with KHR_materials_unlit").into());
    }
    Ok(Transmission {
        factor: number(
            value,
            "transmissionFactor",
            0.,
            |v| (0.0..=1.0).contains(&v),
            &error,
        )?,
        texture: extension_texture(value, "transmissionTexture", document, &error)?,
    })
}

/// A material's `KHR_materials_volume`.
pub(super) struct Volume<'a> {
    pub thickness: f32,
    pub thickness_texture: Option<gltf::Texture<'a>>,
    pub attenuation_distance: f32,
    pub attenuation_color: [f32; 3],
}

// Authority: Khronos glTF acfcbe65, KHR_materials_volume/README.md 110-115
// and schema: thicknessFactor nonnegative, default 0; thicknessTexture's
// green channel; attenuationDistance positive, default +infinity (absent);
// attenuationColor linear RGB in 0..1, default white. Read as three.js
// r185's GLTFLoader reads it (examples/jsm/loaders/GLTFLoader.js 1200-1247).
pub(super) fn read_volume<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<Volume<'a>> {
    let Some(value) = material.extension_value("KHR_materials_volume") else {
        return Ok(Volume {
            thickness: 0.,
            thickness_texture: None,
            attenuation_distance: f32::INFINITY,
            attenuation_color: [1.; 3],
        });
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: volume {message}");
    known_properties(
        value,
        &[
            "thicknessFactor",
            "thicknessTexture",
            "attenuationDistance",
            "attenuationColor",
        ],
        &error,
    )?;
    let attenuation_color = match value.get("attenuationColor") {
        None => [1.; 3],
        Some(color) => color
            .as_array()
            .filter(|channels| channels.len() == 3)
            .and_then(|channels| {
                let channels: Option<Vec<f32>> = channels
                    .iter()
                    .map(|v| {
                        v.as_f64()
                            .filter(|v| (0.0..=1.0).contains(v))
                            .map(|v| v as f32)
                    })
                    .collect();
                channels.map(|c| [c[0], c[1], c[2]])
            })
            .ok_or_else(|| error("attenuationColor must be three numbers in 0..1"))?,
    };
    Ok(Volume {
        thickness: number(
            value,
            "thicknessFactor",
            0.,
            |v| v >= 0. && (v as f32).is_finite(),
            &error,
        )?,
        thickness_texture: extension_texture(value, "thicknessTexture", document, &error)?,
        attenuation_distance: number(
            value,
            "attenuationDistance",
            f64::INFINITY,
            |v| v > 0. && !v.is_nan(),
            &error,
        )?,
        attenuation_color,
    })
}

// Authority: Khronos glTF acfcbe65, KHR_materials_dispersion/README.md
// 96-100 and schema: dispersion, 20 over the Abbe number, nonnegative,
// default 0. Read as three.js r185's GLTFLoader reads it
// (examples/jsm/loaders/GLTFLoader.js 964-988).
pub(super) fn read_dispersion(material: &gltf::Material<'_>) -> Result<f32> {
    let Some(value) = material.extension_value("KHR_materials_dispersion") else {
        return Ok(0.);
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: dispersion {message}");
    known_properties(value, &["dispersion"], &error)?;
    number(
        value,
        "dispersion",
        0.,
        |v| v >= 0. && (v as f32).is_finite(),
        &error,
    )
}
