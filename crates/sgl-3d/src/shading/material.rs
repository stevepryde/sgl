//! Rust mirror of material.wgsl's `Material`: group 2's material values,
//! which the scene's ray source also holds, packed from the typed
//! [`SurfaceMaterial`] and the maps the material was added with.
use crate::content::material::{AlphaMode, SurfaceMaterial};

pub(crate) const MATERIAL_UNLIT: u32 = 1;
pub(crate) const MATERIAL_DOUBLE_SIDED: u32 = 2;
pub(crate) const MATERIAL_NORMAL_MAP: u32 = 4;
pub(crate) const MATERIAL_BUMP_MAP: u32 = 8;
pub(crate) const MATERIAL_ANISOTROPY_MAP: u32 = 16;
pub(crate) const MATERIAL_ALPHA_MASK: u32 = 32;
pub(crate) const MATERIAL_ALPHA_BLEND: u32 = 64;
pub(crate) const MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS: u32 = 128;

/// Which maps a material was added with, as `MATERIAL_*_MAP` bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MaterialMaps(pub u32);

impl MaterialMaps {
    pub fn new(normal: bool, bump: bool, anisotropy: bool) -> Self {
        let bit = |on: bool, bit: u32| if on { bit } else { 0 };
        Self(
            bit(normal, MATERIAL_NORMAL_MAP)
                | bit(bump, MATERIAL_BUMP_MAP)
                | bit(anisotropy, MATERIAL_ANISOTROPY_MAP),
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MaterialUniform {
    pub base: [f32; 4],
    pub emission: [f32; 3],
    pub environment_scale: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub coat: f32,
    pub coat_roughness: f32,
    pub normal_scale: f32,
    pub bump_scale: f32,
    pub anisotropy_strength: f32,
    pub anisotropy_rotation: f32,
    /// A masked material's cutoff; zero otherwise.
    pub alpha_cutoff: f32,
    pub visibility_group: u32,
    pub flags: u32,
    pub padding: u32,
}

impl MaterialUniform {
    /// `values` with `maps`, as the shaders read them.
    pub fn new(values: &SurfaceMaterial, maps: MaterialMaps) -> Self {
        let bit = |on: bool, bit: u32| if on { bit } else { 0 };
        let (alpha_cutoff, alpha) = match values.alpha {
            AlphaMode::Opaque => (0., 0),
            AlphaMode::Mask { cutoff } => (cutoff, MATERIAL_ALPHA_MASK),
            AlphaMode::Blend {
                receives_screen_space_reflections,
            } => (
                0.,
                MATERIAL_ALPHA_BLEND
                    | bit(
                        receives_screen_space_reflections,
                        MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS,
                    ),
            ),
        };
        Self {
            base: values.base,
            emission: values.emission,
            environment_scale: values.environment_scale,
            metallic: values.metallic,
            roughness: values.roughness,
            coat: values.clearcoat,
            coat_roughness: values.coat_roughness,
            normal_scale: values.normal_scale,
            bump_scale: values.bump_scale,
            anisotropy_strength: values.anisotropy_strength,
            anisotropy_rotation: values.anisotropy_rotation,
            alpha_cutoff,
            visibility_group: values.visibility_group,
            flags: bit(values.unlit, MATERIAL_UNLIT)
                | bit(values.double_sided, MATERIAL_DOUBLE_SIDED)
                | maps.0
                | alpha,
            padding: 0,
        }
    }
}
