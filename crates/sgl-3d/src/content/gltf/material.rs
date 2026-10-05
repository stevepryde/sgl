//! glTF materials and the material extensions SGL3D supports.
use gltf::texture::WrappingMode;

use super::super::asset::{Material, Result};
use super::super::material::AlphaMode;

pub(super) fn read_material(
    material: gltf::Material<'_>,
    document: &gltf::Document,
) -> Result<Material> {
    let name = material.name().unwrap_or("unnamed/default");
    // glTF 2.0 (3.9.5): alphaCutoff applies only in MASK mode, defaults to
    // 0.5 and must not be negative.
    let alpha = match material.alpha_mode() {
        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
        gltf::material::AlphaMode::Blend => AlphaMode::Blend {
            receives_screen_space_reflections: false,
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
    if material.occlusion_texture().is_some() {
        return Err(format!("material {name}: occlusion textures are not yet supported").into());
    }
    let mut clearcoat = 0.0;
    let mut coat_roughness = 0.0;
    if let Some(coat) = material.extension_value("KHR_materials_clearcoat") {
        let object = coat
            .as_object()
            .ok_or("clearcoat extension must be an object")?;
        for key in object.keys() {
            if !matches!(key.as_str(), "clearcoatFactor" | "clearcoatRoughnessFactor") {
                return Err(format!("material {name}: unsupported clearcoat property {key}; only scalar clearcoat is supported").into());
            }
        }
        clearcoat = scalar(coat, "clearcoatFactor")?;
        coat_roughness = scalar(coat, "clearcoatRoughnessFactor")?;
    }
    let (anisotropy_strength, anisotropy_rotation, anisotropy_texture) =
        read_anisotropy(&material, document)?;
    let pbr = material.pbr_metallic_roughness();
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
    ];
    let mut wrap = None;
    for texture in textures.into_iter().flatten() {
        let sampler = texture.sampler();
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
        clearcoat,
        coat_roughness,
        anisotropy_strength,
        anisotropy_rotation,
        anisotropy_texture: anisotropy_texture.map(|t| t.source().index()),
        base_texture: pbr.base_color_texture().map(texture_index).transpose()?,
        mr_texture: pbr
            .metallic_roughness_texture()
            .map(texture_index)
            .transpose()?,
        emissive_texture: material.emissive_texture().map(texture_index).transpose()?,
        normal_texture,
        normal_scale: material.normal_texture().map_or(1.0, |t| t.scale()),
        normal_layers: None,
        bump_texture: bump_texture.map(|t| t.source().index()),
        bump_scale: bump
            .map(|b| scalar(b, "bumpFactor"))
            .transpose()?
            .unwrap_or(0.0),
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
            "anisotropyStrength" | "anisotropyRotation" | "anisotropyTexture" | "extras"
        ) {
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
    let texture = value
        .get("anisotropyTexture")
        .map(|info| -> Result<_> {
            let info = info
                .as_object()
                .ok_or_else(|| error("texture must be an object"))?;
            if info.get("texCoord").is_some_and(|v| v.as_u64() != Some(0)) {
                return Err(error("texture requires TEXCOORD_0; export UV0").into());
            }
            if info.get("extensions").is_some() {
                return Err(error(
                    "texture extensions are unsupported; bake the texture transform into UV0",
                )
                .into());
            }
            let index = info
                .get("index")
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| error("texture index must be a nonnegative integer"))?;
            document
                .textures()
                .nth(index)
                .ok_or_else(|| error("texture index out of range").into())
        })
        .transpose()?;
    Ok((strength as f32, rotation as f32, texture))
}

fn scalar(value: &serde_json::Value, key: &str) -> Result<f32> {
    match value.get(key) {
        None => Ok(0.0),
        Some(value) => value
            .as_f64()
            .filter(|v| (0.0..=1.0).contains(v))
            .map(|v| v as f32)
            .ok_or_else(|| format!("clearcoat {key} must be a number in 0..1").into()),
    }
}
