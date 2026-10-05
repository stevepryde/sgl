//! What a material's values may be: the checks `Scene::add_materials` and
//! `Scene::set_material` make before they change anything.
use crate::asset;
use crate::content::material::{AlphaMode, NormalLayer, SurfaceMaterial};
use crate::scene::SceneError;
use crate::shading::material::{MATERIAL_NORMAL_MAP, MAX_LAYER_CYCLES, MaterialMaps, layer_cycles};
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
    if maps.0 & MATERIAL_NORMAL_MAP != 0
        && wrap == [WrappingMode::Repeat; 2]
        && layers.iter().all(valid)
    {
        Ok(())
    } else {
        Err(SceneError::InvalidNormalLayers)
    }
}
