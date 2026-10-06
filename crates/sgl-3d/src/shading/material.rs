//! Rust mirror of material.wgsl's `Material`: group 2's material values,
//! which the scene's ray source also holds, packed from the typed
//! [`SurfaceMaterial`] and the maps the material was added with; and the
//! period of material animation, by which the frame's time is reduced.
use crate::content::material::{AlphaMode, NormalLayer, SurfaceMaterial};

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
pub(crate) const MATERIAL_NORMAL_MAP: u32 = 4;
pub(crate) const MATERIAL_BUMP_MAP: u32 = 8;
pub(crate) const MATERIAL_ANISOTROPY_MAP: u32 = 16;
pub(crate) const MATERIAL_ALPHA_MASK: u32 = 32;
pub(crate) const MATERIAL_ALPHA_BLEND: u32 = 64;
pub(crate) const MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS: u32 = 128;
pub(crate) const MATERIAL_NORMAL_LAYERS: u32 = 256;
pub(crate) const MATERIAL_EMITS_INTO_GI: u32 = 512;

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
    pub padding: u32,
    /// With `MATERIAL_NORMAL_LAYERS`; zero otherwise.
    pub normal_layers: [NormalLayerUniform; 2],
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
                | bit(values.normal_layers.is_some(), MATERIAL_NORMAL_LAYERS)
                | bit(values.emits_into_gi, MATERIAL_EMITS_INTO_GI)
                | maps.0
                | alpha,
            padding: 0,
            normal_layers: values
                .normal_layers
                .map_or([bytemuck::Zeroable::zeroed(); 2], |layers| {
                    layers.each_ref().map(NormalLayerUniform::new)
                }),
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
