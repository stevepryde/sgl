//! glTF materials and the material extensions SGL3D supports.
use gltf::texture::{MagFilter, MinFilter, WrappingMode};

use super::super::asset::{Material, Result};
use super::super::material::AlphaMode;
use super::Ignored;

mod diffuse_transmission;
mod sheen;
mod transmission;
use diffuse_transmission::read_diffuse_transmission;
use sheen::read_sheen;
use transmission::{read_dispersion, read_transmission, read_volume};

/// The properties every material extension object takes from glTF 2.0's
/// glTFProperty, beside its own (Khronos glTF
/// specification/2.0/schema/glTFProperty.schema.json).
const GLTF_PROPERTY_KEYS: [&str; 2] = ["extensions", "extras"];

/// glTF material `index`, adding what SGL3D leaves out of it to `ignored`.
pub(super) fn read_material(
    material: gltf::Material<'_>,
    document: &gltf::Document,
    index: usize,
    ignored: &mut Vec<Ignored>,
) -> Result<Material> {
    let name = material.name().unwrap_or("unnamed/default");
    // glTF 2.0 (3.9.5): alphaCutoff applies only in MASK mode, defaults to
    // 0.5 and must not be negative. BLEND's alpha is coverage, which fades
    // the specular too (Khronos glTF acfcbe65,
    // KHR_materials_transmission/README.md 58–61, 130), as Filament ef1a133's
    // gltfio loads it as `fade` (libs/gltfio/src/JitShaderProvider.cpp
    // 562–564).
    let alpha = match material.alpha_mode() {
        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
        gltf::material::AlphaMode::Blend => AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        },
        gltf::material::AlphaMode::Mask => {
            let cutoff = material.alpha_cutoff().unwrap_or(0.5);
            if !(cutoff.is_finite() && cutoff >= 0.) {
                return Err(format!(
                    "material {name}: alphaCutoff must be a finite nonnegative number"
                )
                .into());
            }
            AlphaMode::Mask { cutoff }
        }
    };
    let clearcoat = read_clearcoat(&material, document)?;
    let iridescence = read_iridescence(&material, document)?;
    let sheen = read_sheen(&material, document)?;
    let diffuse = read_diffuse_transmission(&material, document)?;
    let (anisotropy_strength, anisotropy_rotation, anisotropy_texture) =
        read_anisotropy(&material, document)?;
    let ior = read_ior(&material)?;
    let transmission = read_transmission(&material, document)?;
    let volume = read_volume(&material, document)?;
    let dispersion = read_dispersion(&material)?;
    let (specular, specular_color) = read_specular(&material, index, ignored)?;
    let pbr = material.pbr_metallic_roughness();
    let mr_image = pbr
        .metallic_roughness_texture()
        .map(|info| info.texture().source().index());
    let occlusion = read_occlusion(&material, mr_image, index, ignored)?;
    let texture_index = |info: gltf::texture::Info<'_>| -> Result<usize> {
        if info.tex_coord() != 0 {
            return Err(format!(
                "material {name}: TEXCOORD_{} is unsupported; export UV0",
                info.tex_coord()
            )
            .into());
        }
        Ok(info.texture().source().index())
    };
    let normal_texture = material
        .normal_texture()
        .map(|info| -> Result<usize> {
            if info.tex_coord() != 0 {
                return Err(format!("material {name}: normal map requires UV0").into());
            }
            Ok(info.texture().source().index())
        })
        .transpose()?;
    let bump = material.extension_value("EXT_materials_bump");
    let bump_info = bump.and_then(|b| b.get("bumpTexture"));
    let bump_texture = if let Some(info) = bump_info {
        if info.get("texCoord").and_then(|v| v.as_u64()).unwrap_or(0) != 0 {
            return Err(format!("material {name}: bump map requires UV0").into());
        }
        let index = info
            .get("index")
            .and_then(|v| v.as_u64())
            .ok_or("bump texture index missing")?;
        Some(
            document
                .textures()
                .nth(index as usize)
                .ok_or("bump texture index out of range")?,
        )
    } else {
        None
    };
    let textures = [
        pbr.base_color_texture().map(|t| t.texture()),
        pbr.metallic_roughness_texture().map(|t| t.texture()),
        material.emissive_texture().map(|t| t.texture()),
        material.normal_texture().map(|t| t.texture()),
        bump_texture.clone(),
        anisotropy_texture.clone(),
        clearcoat.texture.clone(),
        clearcoat.roughness_texture.clone(),
        clearcoat.normal_texture.clone(),
        iridescence.texture.clone(),
        iridescence.thickness_texture.clone(),
        transmission.texture.clone(),
        volume.thickness_texture.clone(),
        sheen.color_texture.clone(),
        sheen.roughness_texture.clone(),
        diffuse.texture.clone(),
        diffuse.color_texture.clone(),
        // An occlusion map SGL3D samples, the metallic-roughness image on
        // TEXCOORD_0, takes its sampler; an ignored one constrains nothing.
        material
            .occlusion_texture()
            .map(|t| t.texture())
            .filter(|_| occlusion.texture.is_some() && occlusion.texture == mr_image),
    ];
    // The textures SGL3D samples, and only those (an ignored map's sampler
    // constrains nothing), share one wrapping and its trilinear filtering.
    let mut wrap = None;
    for texture in textures.into_iter().flatten() {
        let sampler = texture.sampler();
        if !matches!(sampler.mag_filter(), None | Some(MagFilter::Linear))
            || !matches!(
                sampler.min_filter(),
                None | Some(MinFilter::LinearMipmapLinear)
            )
        {
            return Err(format!("material {name}: texture {} uses unsupported sampling; export linear magnification and trilinear minification", texture.index()).into());
        }
        let modes = [sampler.wrap_s(), sampler.wrap_t()];
        if wrap.is_some_and(|existing| existing != modes) {
            return Err(format!("material {name}: texture channels use different wrapping").into());
        }
        wrap = Some(modes);
    }
    let strength = material.emissive_strength().unwrap_or(1.0);
    Ok(Material {
        name: name.to_owned(),
        visibility_group: 0,
        casts_directional_shadow: true,
        base: pbr.base_color_factor(),
        emissive: material.emissive_factor().map(|v| v * strength),
        metallic: pbr.metallic_factor(),
        roughness: pbr.roughness_factor(),
        ior,
        specular,
        specular_color,
        clearcoat: clearcoat.factor,
        coat_roughness: clearcoat.roughness,
        clearcoat_texture: clearcoat.texture.map(|t| t.source().index()),
        coat_roughness_texture: clearcoat.roughness_texture.map(|t| t.source().index()),
        coat_normal_texture: clearcoat.normal_texture.map(|t| t.source().index()),
        coat_normal_scale: clearcoat.normal_scale,
        iridescence: iridescence.factor,
        iridescence_ior: iridescence.ior,
        iridescence_thickness: iridescence.thickness,
        iridescence_texture: iridescence.texture.map(|t| t.source().index()),
        iridescence_thickness_texture: iridescence.thickness_texture.map(|t| t.source().index()),
        sheen_color: sheen.color,
        sheen_roughness: sheen.roughness,
        sheen_color_texture: sheen.color_texture.map(|t| t.source().index()),
        sheen_roughness_texture: sheen.roughness_texture.map(|t| t.source().index()),
        diffuse_transmission: diffuse.factor,
        diffuse_transmission_color: diffuse.color,
        diffuse_transmission_texture: diffuse.texture.map(|t| t.source().index()),
        diffuse_transmission_color_texture: diffuse.color_texture.map(|t| t.source().index()),
        anisotropy_strength,
        anisotropy_rotation,
        anisotropy_texture: anisotropy_texture.map(|t| t.source().index()),
        base_texture: pbr.base_color_texture().map(texture_index).transpose()?,
        mr_texture: pbr
            .metallic_roughness_texture()
            .map(texture_index)
            .transpose()?,
        occlusion_texture: occlusion.texture,
        occlusion_strength: occlusion.strength,
        emissive_texture: material.emissive_texture().map(texture_index).transpose()?,
        normal_texture,
        normal_scale: material.normal_texture().map_or(1.0, |t| t.scale()),
        normal_layers: None,
        bump_texture: bump_texture.map(|t| t.source().index()),
        // Authority: EXT_materials_bump's schema (KhronosGroup/glTF#2339):
        // bumpFactor in 0..100, default 1.
        bump_scale: bump
            .map(|b| {
                scalar(b, "bumpFactor", 1.0, 0.0..=100.0, |message| {
                    format!("material {name}: bump {message}")
                })
            })
            .transpose()?
            .unwrap_or(1.0),
        transmission: transmission.factor,
        transmission_texture: transmission.texture.map(|t| t.source().index()),
        thickness: volume.thickness,
        thickness_texture: volume.thickness_texture.map(|t| t.source().index()),
        attenuation_distance: volume.attenuation_distance,
        attenuation_color: volume.attenuation_color,
        dispersion,
        wrap: wrap.unwrap_or([WrappingMode::Repeat; 2]),
        double_sided: material.double_sided(),
        unlit: material.unlit(),
        // glTF has no such property: whether a light stands for a surface
        // is the game's.
        emits_into_gi: true,
        alpha,
    })
}

// Authority: Khronos glTF acfcbe65e40c53d6d3aa55a7299982bf2c01c75d,
// extensions/2.0/Khronos/KHR_materials_anisotropy/README.md and schema.
fn read_anisotropy<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<(f32, f32, Option<gltf::Texture<'a>>)> {
    let Some(value) = material.extension_value("KHR_materials_anisotropy") else {
        return Ok((0.0, 0.0, None));
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: anisotropy {message}");
    let object = value
        .as_object()
        .ok_or_else(|| error("extension must be an object"))?;
    if material.unlit() {
        return Err(error("cannot be combined with KHR_materials_unlit").into());
    }
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "anisotropyStrength" | "anisotropyRotation" | "anisotropyTexture"
        ) && !GLTF_PROPERTY_KEYS.contains(&key.as_str())
        {
            return Err(error(&format!("unsupported property {key}")).into());
        }
    }
    let number = |key: &str, default: f64| -> Result<f64> {
        value.get(key).map_or(Ok(default), |number| {
            number
                .as_f64()
                .filter(|v| (*v as f32).is_finite())
                .ok_or_else(|| error(&format!("{key} must be a finite number")).into())
        })
    };
    let strength = number("anisotropyStrength", 0.0)?;
    if !(0.0..=1.0).contains(&strength) {
        return Err(error("anisotropyStrength must be in 0..1").into());
    }
    let rotation = number("anisotropyRotation", 0.0)?;
    let texture = extension_texture(value, "anisotropyTexture", document, &error)?;
    Ok((strength as f32, rotation as f32, texture))
}

/// Extension property `key` of `value`, a glTF textureInfo: its texture,
/// which must lie on TEXCOORD_0. `error` words a refusal.
fn extension_texture<'a>(
    value: &serde_json::Value,
    key: &str,
    document: &'a gltf::Document,
    error: &dyn Fn(&str) -> String,
) -> Result<Option<gltf::Texture<'a>>> {
    let Some(info) = value.get(key) else {
        return Ok(None);
    };
    let info = info
        .as_object()
        .ok_or_else(|| error(&format!("{key} must be an object")))?;
    if info.get("texCoord").is_some_and(|v| v.as_u64() != Some(0)) {
        return Err(error(&format!("{key} requires TEXCOORD_0; export UV0")).into());
    }
    let index = info
        .get("index")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| error(&format!("{key} index must be a nonnegative integer")))?;
    document
        .textures()
        .nth(index)
        .map(Some)
        .ok_or_else(|| error(&format!("{key} index out of range")).into())
}

/// A material's clearcoat layer (KHR_materials_clearcoat).
struct Clearcoat<'a> {
    factor: f32,
    roughness: f32,
    texture: Option<gltf::Texture<'a>>,
    roughness_texture: Option<gltf::Texture<'a>>,
    normal_texture: Option<gltf::Texture<'a>>,
    normal_scale: f32,
}

// Authority: Khronos glTF KHR_materials_clearcoat/README.md and schema:
// clearcoatFactor and clearcoatRoughnessFactor in 0..1, default 0; its
// three textureInfos, clearcoatNormalTexture's scale defaulting to 1.
fn read_clearcoat<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<Clearcoat<'a>> {
    let mut clearcoat = Clearcoat {
        factor: 0.,
        roughness: 0.,
        texture: None,
        roughness_texture: None,
        normal_texture: None,
        normal_scale: 1.,
    };
    let Some(value) = material.extension_value("KHR_materials_clearcoat") else {
        return Ok(clearcoat);
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: clearcoat {message}");
    let object = value
        .as_object()
        .ok_or_else(|| error("extension must be an object"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "clearcoatFactor"
                | "clearcoatRoughnessFactor"
                | "clearcoatTexture"
                | "clearcoatRoughnessTexture"
                | "clearcoatNormalTexture"
        ) && !GLTF_PROPERTY_KEYS.contains(&key.as_str())
        {
            return Err(error(&format!("unsupported property {key}")).into());
        }
    }
    clearcoat.factor = scalar(value, "clearcoatFactor", 0.0, 0.0..=1.0, error)?;
    clearcoat.roughness = scalar(value, "clearcoatRoughnessFactor", 0.0, 0.0..=1.0, error)?;
    clearcoat.texture = extension_texture(value, "clearcoatTexture", document, &error)?;
    clearcoat.roughness_texture =
        extension_texture(value, "clearcoatRoughnessTexture", document, &error)?;
    clearcoat.normal_texture =
        extension_texture(value, "clearcoatNormalTexture", document, &error)?;
    if let Some(scale) = value
        .get("clearcoatNormalTexture")
        .and_then(|info| info.get("scale"))
    {
        clearcoat.normal_scale = scale
            .as_f64()
            .map(|v| v as f32)
            .filter(|v| v.is_finite())
            .ok_or_else(|| error("clearcoatNormalTexture.scale must be a finite number"))?;
    }
    Ok(clearcoat)
}

/// A material's thin film (KHR_materials_iridescence).
struct Iridescence<'a> {
    factor: f32,
    ior: f32,
    /// Its thinnest and thickest, in nanometres.
    thickness: [f32; 2],
    texture: Option<gltf::Texture<'a>>,
    thickness_texture: Option<gltf::Texture<'a>>,
}

// Authority: Khronos glTF KHR_materials_iridescence/README.md and schema:
// iridescenceFactor in 0..1, default 0; iridescenceIor at least 1, default
// 1.3; iridescenceThicknessMinimum and Maximum nonnegative nanometres,
// default 100 and 400; iridescenceTexture and iridescenceThicknessTexture.
fn read_iridescence<'a>(
    material: &gltf::Material<'a>,
    document: &'a gltf::Document,
) -> Result<Iridescence<'a>> {
    let mut iridescence = Iridescence {
        factor: 0.,
        ior: 1.3,
        thickness: [100., 400.],
        texture: None,
        thickness_texture: None,
    };
    let Some(value) = material.extension_value("KHR_materials_iridescence") else {
        return Ok(iridescence);
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: iridescence {message}");
    let object = value
        .as_object()
        .ok_or_else(|| error("extension must be an object"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "iridescenceFactor"
                | "iridescenceTexture"
                | "iridescenceIor"
                | "iridescenceThicknessMinimum"
                | "iridescenceThicknessMaximum"
                | "iridescenceThicknessTexture"
        ) && !GLTF_PROPERTY_KEYS.contains(&key.as_str())
        {
            return Err(error(&format!("unsupported property {key}")).into());
        }
    }
    let number = |key: &str, default: f32, least: f32| -> Result<f32> {
        value.get(key).map_or(Ok(default), |number| {
            number
                .as_f64()
                .map(|v| v as f32)
                .filter(|v| v.is_finite() && *v >= least)
                .ok_or_else(|| {
                    error(&format!(
                        "{key} must be a finite number of at least {least}"
                    ))
                    .into()
                })
        })
    };
    iridescence.factor = number("iridescenceFactor", 0., 0.)?;
    if iridescence.factor > 1. {
        return Err(error("iridescenceFactor must be in 0..1").into());
    }
    iridescence.ior = number("iridescenceIor", 1.3, 1.)?;
    iridescence.thickness = [
        number("iridescenceThicknessMinimum", 100., 0.)?,
        number("iridescenceThicknessMaximum", 400., 0.)?,
    ];
    iridescence.texture = extension_texture(value, "iridescenceTexture", document, &error)?;
    iridescence.thickness_texture =
        extension_texture(value, "iridescenceThicknessTexture", document, &error)?;
    Ok(iridescence)
}

/// Property `key` of `value`, a number in `0..=1`; `default` where absent,
/// none where it is not such a number.
fn unit_factor(value: &serde_json::Value, key: &str, default: f32) -> Option<f32> {
    value.get(key).map_or(Some(default), |number| {
        number
            .as_f64()
            .map(|v| v as f32)
            .filter(|v| (0.0..=1.0).contains(v))
    })
}

/// `value` as a colour factor: three finite numbers from 0 to `most`.
fn color_factor(value: &serde_json::Value, most: f32) -> Option<[f32; 3]> {
    let channels = value.as_array().filter(|channels| channels.len() == 3)?;
    let mut color = [0.; 3];
    for (channel, value) in color.iter_mut().zip(channels) {
        *channel = value
            .as_f64()
            .map(|v| v as f32)
            .filter(|v| v.is_finite() && (0.0..=most).contains(v))?;
    }
    Some(color)
}

/// A material's occlusion map (glTF 2.0 `occlusionTexture`).
struct Occlusion {
    /// Its image, where it lies on `TEXCOORD_0`.
    texture: Option<usize>,
    /// glTF's `strength`, in `0..=1`; 1 without a map.
    strength: f32,
}

/// Material `index`'s occlusion map, where the image of its metallic-roughness
/// map is `mr_image`. SGL3D samples it where it is that image (ORM packing);
/// another image is kept but listed in `ignored`, and a map on another UV
/// set, which SGL3D has no material input for, is left out and listed.
fn read_occlusion(
    material: &gltf::Material<'_>,
    mr_image: Option<usize>,
    index: usize,
    ignored: &mut Vec<Ignored>,
) -> Result<Occlusion> {
    let Some(occlusion) = material.occlusion_texture() else {
        return Ok(Occlusion {
            texture: None,
            strength: 1.,
        });
    };
    let strength = occlusion.strength();
    if !(0.0..=1.0).contains(&strength) {
        let name = material.name().unwrap_or("unnamed/default");
        return Err(format!("material {name}: occlusionTexture.strength must be in 0..1").into());
    }
    let image = occlusion.texture().source().index();
    let texture = (occlusion.tex_coord() == 0).then_some(image);
    if texture.is_none() || texture != mr_image {
        ignored.push(Ignored::OcclusionMap { material: index });
    }
    Ok(Occlusion { texture, strength })
}

// Authority: Khronos glTF KHR_materials_ior/README.md: ior defaults to 1.5,
// and is at least 1 or 0, which stands for an infinite IOR.
fn read_ior(material: &gltf::Material<'_>) -> Result<f32> {
    let ior = material
        .extension_value("KHR_materials_ior")
        .and_then(|value| value.get("ior"))
        .map_or(Some(1.5), serde_json::Value::as_f64);
    match ior {
        Some(0.) => Ok(f32::INFINITY),
        Some(ior) if ior >= 1. && (ior as f32).is_finite() => Ok(ior as f32),
        _ => {
            let name = material.name().unwrap_or("unnamed/default");
            Err(format!("material {name}: ior must be 0 or a number of at least 1").into())
        }
    }
}

// Authority: Khronos glTF KHR_materials_specular/README.md: specularFactor
// in 0..1, default 1; specularColorFactor linear RGB, nonnegative, default
// white. Its textures are listed in `ignored`.
fn read_specular(
    material: &gltf::Material<'_>,
    index: usize,
    ignored: &mut Vec<Ignored>,
) -> Result<(f32, [f32; 3])> {
    let Some(value) = material.extension_value("KHR_materials_specular") else {
        return Ok((1., [1.; 3]));
    };
    let name = material.name().unwrap_or("unnamed/default");
    let error = |message: &str| format!("material {name}: specular {message}");
    let factor = value
        .get("specularFactor")
        .map_or(Some(1.), |v| v.as_f64().filter(|v| (0.0..=1.0).contains(v)));
    let factor = factor.ok_or_else(|| error("specularFactor must be a number in 0..1"))?;
    let color = match value.get("specularColorFactor") {
        None => [1.; 3],
        Some(color) => color
            .as_array()
            .filter(|channels| channels.len() == 3)
            .and_then(|channels| {
                let channels: Option<Vec<f32>> = channels
                    .iter()
                    .map(|v| {
                        v.as_f64()
                            .map(|v| v as f32)
                            .filter(|v| v.is_finite() && *v >= 0.)
                    })
                    .collect();
                channels.map(|c| [c[0], c[1], c[2]])
            })
            .ok_or_else(|| error("specularColorFactor must be three finite nonnegative numbers"))?,
    };
    if value.get("specularTexture").is_some() || value.get("specularColorTexture").is_some() {
        ignored.push(Ignored::SpecularMap { material: index });
    }
    Ok((factor as f32, color))
}

/// The number `key` in `range`, `default` when absent.
fn scalar(
    value: &serde_json::Value,
    key: &str,
    default: f32,
    range: std::ops::RangeInclusive<f64>,
    error: impl Fn(&str) -> String,
) -> Result<f32> {
    match value.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|v| range.contains(v))
            .map(|v| v as f32)
            .ok_or_else(|| {
                error(&format!(
                    "{key} must be a number in {}..{}",
                    range.start(),
                    range.end()
                ))
                .into()
            }),
    }
}
