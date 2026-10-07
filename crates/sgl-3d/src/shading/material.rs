//! Rust mirror of material.wgsl's `Material`: group 2's material values,
//! which the scene's ray source also holds, packed from the typed
//! [`SurfaceMaterial`] and its maps in effect; and the
//! period of material animation, by which the frame's time is reduced.
use crate::content::material::{AlphaMode, NormalLayer, SurfaceMaterial};
use crate::shading::bind::group2::MaterialMap;

/// The seconds after which material animation repeats exactly: an hour, the
/// period at which Godot b130438 rolls its shader `TIME` over
/// (`rendering/limits/time/time_rollover_secs`, `RendererCompositorRD::
/// begin_frame`) and Bevy 9d12036 wraps `globals.time`
/// (`Time::DEFAULT_WRAP_PERIOD`). Both jump at the wrap; SGL3D instead
/// rounds each normal layer's speed to whole repeats per period
/// (`NormalLayerUniform::cycles`), so the wrap moves nothing, and reduces
/// the frame's double-precision time modulo the period on the CPU
/// (`animation_phase`), so the GPU's `f32` keeps its precision however long
/// a session runs.
const ANIMATION_PERIOD_SECONDS: f64 = 3600.;

/// Where `seconds` (`FrameInput::elapsed_seconds`) falls within the
/// animation period, as a fraction of it in `0..=1`: `Frame::animation_phase`.
pub(crate) fn animation_phase(seconds: f64) -> f32 {
    if !seconds.is_finite() {
        return 0.;
    }
    (seconds.rem_euclid(ANIMATION_PERIOD_SECONDS) / ANIMATION_PERIOD_SECONDS) as f32
}

pub(crate) const MATERIAL_UNLIT: u32 = 1;
pub(crate) const MATERIAL_DOUBLE_SIDED: u32 = 2;
pub(crate) const MATERIAL_ALPHA_MASK: u32 = 32;
pub(crate) const MATERIAL_ALPHA_BLEND: u32 = 64;
pub(crate) const MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS: u32 = 128;
pub(crate) const MATERIAL_NORMAL_LAYERS: u32 = 256;
pub(crate) const MATERIAL_EMITS_INTO_GI: u32 = 512;
pub(crate) const MATERIAL_KEEPS_SPECULAR: u32 = 1024;
pub(crate) const MATERIAL_TRANSMISSIVE: u32 = 2048;

/// The attenuation coefficient the record holds for a channel that turns
/// white light black at any distance (`attenuation_coefficient`): large and
/// finite, so any positive path through the volume passes nothing, as
/// Beer-Lambert's law with an attenuation colour of 0 does, and none passes
/// all of it, with no infinity or NaN in the shader.
pub(crate) const OPAQUE_ATTENUATION: f32 = 1e30;

/// Beer-Lambert's attenuation coefficient per metre of a channel that white
/// light turns `color` in after `distance` metres, -ln(color) / distance
/// (KHR_materials_volume, Khronos glTF acfcbe65, README 148-168): 0 at an
/// infinite distance, at most `OPAQUE_ATTENUATION`.
pub(crate) fn attenuation_coefficient(color: f32, distance: f32) -> f32 {
    if distance.is_infinite() {
        return 0.;
    }
    if color <= 0. {
        return OPAQUE_ATTENUATION;
    }
    (-color.ln() / distance).min(OPAQUE_ATTENUATION)
}

/// A set of a material's maps, as the record's `maps` word holds them
/// (`MaterialMap::bit`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MaterialMaps(pub u32);

impl MaterialMaps {
    /// The set of `maps`.
    pub fn of(maps: impl IntoIterator<Item = MaterialMap>) -> Self {
        Self(maps.into_iter().fold(0, |bits, map| bits | map.bit()))
    }

    pub fn contains(self, map: MaterialMap) -> bool {
        self.0 & map.bit() != 0
    }
}

/// The reflectance at normal incidence of a dielectric of index of
/// refraction `ior` seen from air, ((ior − 1) / (ior + 1))², as
/// KHR_materials_ior and Fresnel's equations give it, written so that an
/// infinite IOR gives 1.
fn ior_f0(ior: f32) -> f32 {
    let r = 1. - 2. / (ior + 1.);
    r * r
}

/// One normal layer as the shaders read it (`NormalLayer` in
/// material.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct NormalLayerUniform {
    /// The whole repeats of the map the layer moves along U and V each
    /// animation period: its velocity times its scale, in repeats per
    /// second, times the period, rounded.
    pub cycles: [f32; 2],
    pub scale: f32,
    pub strength: f32,
}

/// The most whole repeats per period a layer may move along an axis: `f32`,
/// in which the record holds them, holds every whole number up to it, so
/// the period's end lands where it began.
pub(crate) const MAX_LAYER_CYCLES: f64 = 16_777_216.;

/// The whole repeats of its map `layer` moves along U and V each animation
/// period (`NormalLayerUniform::cycles`), before they are checked against
/// `MAX_LAYER_CYCLES`.
pub(crate) fn layer_cycles(layer: &NormalLayer) -> [f64; 2] {
    layer.velocity.map(|velocity| {
        (f64::from(velocity) * f64::from(layer.scale) * ANIMATION_PERIOD_SECONDS).round()
    })
}

impl NormalLayerUniform {
    fn new(layer: &NormalLayer) -> Self {
        Self {
            cycles: layer_cycles(layer).map(|cycles| cycles as f32),
            scale: layer.scale,
            strength: layer.strength,
        }
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
    /// glTF's `occlusionTexture.strength`, read with `MATERIAL_MAP_OCCLUSION`.
    pub occlusion_strength: f32,
    /// The IOR's F0 times the specular colour, which the shaders clamp to 1
    /// and scale by `specular` (`material_dielectric_f0`).
    pub specular_f0: [f32; 3],
    pub specular: f32,
    /// With `MATERIAL_NORMAL_LAYERS`; zero otherwise.
    pub normal_layers: [NormalLayerUniform; 2],
    /// Its maps in effect, `MATERIAL_MAP_*` bits (`MaterialMaps`).
    pub maps: u32,
    /// glTF's `clearcoatNormalTexture.scale`, read with
    /// `MATERIAL_MAP_COAT_NORMAL`.
    pub coat_normal_scale: f32,
    /// KHR_materials_iridescence's film: its strength (0 none), IOR and
    /// thinnest and thickest thickness in nanometres.
    pub iridescence: f32,
    pub iridescence_ior: f32,
    pub iridescence_thickness: [f32; 2],
    pub padding: [u32; 2],
    /// Beer-Lambert's attenuation coefficient per metre on each channel
    /// (`attenuation_coefficient`).
    pub attenuation: [f32; 3],
    /// KHR_materials_transmission's factor, with `MATERIAL_TRANSMISSIVE`.
    pub transmission: f32,
    /// The volume's thickness in the mesh's units, 0 for a thin wall.
    pub thickness: f32,
    /// The IOR as KHR_materials_ior writes it, 0 for an infinite one, which
    /// refraction alone reads: the shaders never derive F0 from it, which
    /// `specular_f0` holds.
    pub ior: f32,
    /// KHR_materials_dispersion's 20 over the Abbe number.
    pub dispersion: f32,
    pub volume_padding: u32,
    /// KHR_materials_sheen's linear colour (0 none) and perceptual
    /// roughness.
    pub sheen: [f32; 3],
    pub sheen_roughness: f32,
    /// KHR_materials_diffuse_transmission's colour, and the share of the
    /// light the base diffuses that it passes to its other side (0 none).
    pub diffuse_transmission_color: [f32; 3],
    pub diffuse_transmission: f32,
}

impl MaterialUniform {
    /// `values` with its maps in effect, `maps`, as the shaders read them.
    pub fn new(values: &SurfaceMaterial, maps: MaterialMaps) -> Self {
        let bit = |on: bool, bit: u32| if on { bit } else { 0 };
        let (alpha_cutoff, alpha) = match values.alpha {
            AlphaMode::Opaque => (0., 0),
            AlphaMode::Mask { cutoff } => (cutoff, MATERIAL_ALPHA_MASK),
            AlphaMode::Blend {
                receives_screen_space_reflections,
                keeps_specular,
            } => (
                0.,
                MATERIAL_ALPHA_BLEND
                    | bit(
                        receives_screen_space_reflections,
                        MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS,
                    )
                    | bit(keeps_specular, MATERIAL_KEEPS_SPECULAR),
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
                | bit(values.normal_layers.is_some(), MATERIAL_NORMAL_LAYERS)
                | bit(values.emits_into_gi, MATERIAL_EMITS_INTO_GI)
                | bit(values.transmissive(), MATERIAL_TRANSMISSIVE)
                | alpha,
            occlusion_strength: values.occlusion_strength,
            specular_f0: values.specular_color.map(|tint| ior_f0(values.ior) * tint),
            specular: values.specular,
            normal_layers: values
                .normal_layers
                .map_or([bytemuck::Zeroable::zeroed(); 2], |layers| {
                    layers.each_ref().map(NormalLayerUniform::new)
                }),
            maps: maps.0,
            coat_normal_scale: values.coat_normal_scale,
            iridescence: values.iridescence,
            iridescence_ior: values.iridescence_ior,
            iridescence_thickness: values.iridescence_thickness,
            padding: [0; 2],
            attenuation: values
                .attenuation_color
                .map(|color| attenuation_coefficient(color, values.attenuation_distance)),
            transmission: values.transmission,
            thickness: values.thickness,
            ior: if values.ior.is_finite() {
                values.ior
            } else {
                0.
            },
            dispersion: values.dispersion,
            volume_padding: 0,
            sheen: values.sheen_color,
            sheen_roughness: values.sheen_roughness,
            diffuse_transmission_color: values.diffuse_transmission_color,
            diffuse_transmission: values.diffuse_transmission,
        }
    }

    /// Whether its shading changes with the frame's time where its geometry
    /// stands still: it is lit and a normal layer moves, whole repeats of
    /// its map each animation period. An unlit surface's shading takes no
    /// normal, and a layer whose speed rounds to none stands still.
    pub fn surface_moves(&self) -> bool {
        self.flags & MATERIAL_UNLIT == 0
            && self
                .normal_layers
                .iter()
                .any(|layer| layer.cycles != [0.; 2])
    }
}
