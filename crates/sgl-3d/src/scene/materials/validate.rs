//! What a material's values may be: the checks `Scene::add_materials` and
//! `Scene::set_material` make before they change anything.
use crate::asset;
use crate::content::material::{AlphaMode, NormalLayer, SurfaceMaterial};
use crate::scene::SceneError;
use crate::shading::bind::group2::MaterialMap;
use crate::shading::material::{MAX_LAYER_CYCLES, MaterialMaps, layer_cycles};
use gltf::texture::WrappingMode;

/// Anisotropy `values` may take on meshes of which `untangented` lack
/// authored tangent frames.
pub(super) fn validate_anisotropy(
    values: &SurfaceMaterial,
    untangented: u32,
) -> Result<(), SceneError> {
    if !asset::valid_anisotropy(values.anisotropy_strength, values.anisotropy_rotation) {
        return Err(SceneError::InvalidAnisotropy);
    }
    if values.anisotropy_strength > 0. && untangented > 0 {
        return Err(SceneError::MissingAnisotropyTangents);
    }
    Ok(())
}

/// A masked material's cutoff, as glTF bounds `alphaCutoff`: finite and
/// nonnegative. A NaN cutoff would cut out nothing.
pub(super) fn validate_alpha(values: &SurfaceMaterial) -> Result<(), SceneError> {
    match values.alpha {
        AlphaMode::Mask { cutoff } if !(cutoff.is_finite() && cutoff >= 0.) => {
            Err(SceneError::InvalidAlphaCutoff)
        }
        _ => Ok(()),
    }
}

/// The dielectric reflectance and occlusion `values` may take: an IOR of at
/// least 1, infinity included (KHR_materials_ior; F0 is defined for none
/// below), a specular strength and an occlusion strength in `0..=1`, as glTF
/// bounds them, and a finite nonnegative specular colour.
pub(super) fn validate_reflectance(values: &SurfaceMaterial) -> Result<(), SceneError> {
    if !(values.ior >= 1.
        && (0.0..=1.0).contains(&values.specular)
        && values
            .specular_color
            .iter()
            .all(|channel| channel.is_finite() && *channel >= 0.))
    {
        return Err(SceneError::InvalidReflectance);
    }
    if !(0.0..=1.0).contains(&values.occlusion_strength) {
        return Err(SceneError::InvalidOcclusion);
    }
    Ok(())
}

/// The thin film `values` may take (KHR_materials_iridescence): a strength
/// in `0..=1`, a finite IOR of at least 1, and finite nonnegative
/// thicknesses, the thinnest above the thickest allowed as glTF allows it.
pub(super) fn validate_iridescence(values: &SurfaceMaterial) -> Result<(), SceneError> {
    if (0.0..=1.0).contains(&values.iridescence)
        && values.iridescence_ior.is_finite()
        && values.iridescence_ior >= 1.
        && values
            .iridescence_thickness
            .iter()
            .all(|thickness| thickness.is_finite() && *thickness >= 0.)
    {
        Ok(())
    } else {
        Err(SceneError::InvalidIridescence)
    }
}

/// The transmission and volume `values` may take, as KHR_materials_
/// transmission, KHR_materials_volume and KHR_materials_dispersion bound
/// them: a transmission in `0..=1`, and none on an unlit material, which
/// takes no light; a finite nonnegative thickness and dispersion; a positive
/// attenuation distance, infinity included; and an attenuation colour in
/// `0..=1`.
pub(super) fn validate_transmission(values: &SurfaceMaterial) -> Result<(), SceneError> {
    let unit = |value: f32| (0.0..=1.0).contains(&value);
    let nonnegative = |value: f32| value.is_finite() && value >= 0.;
    if unit(values.transmission)
        && !(values.unlit && values.transmission > 0.)
        && nonnegative(values.thickness)
        && nonnegative(values.dispersion)
        && values.attenuation_distance > 0.
        && values
            .attenuation_color
            .iter()
            .all(|&channel| unit(channel))
    {
        Ok(())
    } else {
        Err(SceneError::InvalidTransmission)
    }
}

/// The sheen `values` may take (KHR_materials_sheen): a colour and a
/// roughness in `0..=1`, as glTF bounds them.
pub(super) fn validate_sheen(values: &SurfaceMaterial) -> Result<(), SceneError> {
    let unit = |value: &f32| (0.0..=1.0).contains(value);
    if values.sheen_color.iter().all(unit) && unit(&values.sheen_roughness) {
        Ok(())
    } else {
        Err(SceneError::InvalidSheen)
    }
}

/// The diffuse transmission `values` may take
/// (KHR_materials_diffuse_transmission): a share in `0..=1` and a finite
/// nonnegative colour, as its schema bounds them.
pub(super) fn validate_diffuse_transmission(values: &SurfaceMaterial) -> Result<(), SceneError> {
    if (0.0..=1.0).contains(&values.diffuse_transmission)
        && values
            .diffuse_transmission_color
            .iter()
            .all(|channel| channel.is_finite() && *channel >= 0.)
    {
        Ok(())
    } else {
        Err(SceneError::InvalidDiffuseTransmission)
    }
}

/// Normal layers `values` may take on a material added with `maps` and
/// `wrap`: they scroll its normal map, which must be there and repeat on both
/// axes, at finite velocities and strengths, positive finite scales, and
/// speeds whose whole repeats per period the record holds exactly.
pub(super) fn validate_normal_layers(
    values: &SurfaceMaterial,
    maps: MaterialMaps,
    wrap: [WrappingMode; 2],
) -> Result<(), SceneError> {
    let Some(layers) = values.normal_layers else {
        return Ok(());
    };
    let valid = |layer: &NormalLayer| {
        layer.velocity.iter().all(|speed| speed.is_finite())
            && layer.scale.is_finite()
            && layer.scale > 0.
            && layer.strength.is_finite()
            && layer_cycles(layer)
                .iter()
                .all(|cycles| cycles.abs() <= MAX_LAYER_CYCLES)
    };
    if maps.contains(MaterialMap::Normal)
        && wrap == [WrappingMode::Repeat; 2]
        && layers.iter().all(valid)
    {
        Ok(())
    } else {
        Err(SceneError::InvalidNormalLayers)
    }
}
