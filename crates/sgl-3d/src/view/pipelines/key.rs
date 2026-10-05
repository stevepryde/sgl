//! What keys a geometry pipeline besides its pass and variant: the
//! diagnostics layers and the lit constants compiled into it, and the
//! constants each key sets.
use super::{Alpha, GeometryPass, Variant};
use crate::Scene;
use crate::settings::DisabledLayers;

/// The geometry shader's diagnostics layers, compiled as pipeline constants.
/// Every layer is on outside diagnostics builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LayerConstants {
    pub normal_maps: bool,
    pub bump_maps: bool,
    pub baked_lighting: bool,
    pub instance_emission: bool,
}

impl LayerConstants {
    pub const ALL: Self = Self {
        normal_maps: true,
        bump_maps: true,
        baked_lighting: true,
        instance_emission: true,
    };

    /// Every layer `disable` keeps on.
    pub fn new(disable: &DisabledLayers) -> Self {
        Self {
            normal_maps: !disable.normal_maps,
            bump_maps: !disable.bump_maps,
            baked_lighting: !disable.baked_lighting,
            instance_emission: !disable.instance_emission,
        }
    }

    fn constants(self) -> [(&'static str, f64); 4] {
        [
            ("normal_maps_enabled", f64::from(u8::from(self.normal_maps))),
            ("bump_maps_enabled", f64::from(u8::from(self.bump_maps))),
            (
                "baked_lighting_enabled",
                f64::from(u8::from(self.baked_lighting)),
            ),
            (
                "instance_emission_enabled",
                f64::from(u8::from(self.instance_emission)),
            ),
        ]
    }
}

/// What lit shading compiles in only while the scene holds it, so a scene
/// without it pays nothing for it: the lit passes' and the world-space
/// reflection trace's constants that follow the scene's content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct LitConstants {
    /// Rectangle lights' shading (`rect_lights_enabled` in lights.wgsl), as
    /// Godot specialises its clustered pass on `cluster_has_area_light`.
    pub rect_lights: bool,
    /// Decals (`decals_enabled` in decals.wgsl). Neither Godot, whose
    /// clustered pass walks each cluster's decals whatever the scene holds,
    /// nor Bevy, whose `CLUSTERED_DECALS_ARE_USABLE` follows the device,
    /// specialises on them; the trade is a compile when the first decal is
    /// added or the last removed.
    pub decals: bool,
}

impl LitConstants {
    /// What `scene` holds.
    pub fn of(scene: &Scene) -> Self {
        Self {
            rect_lights: scene.lights.holds_rect(),
            decals: !scene.decals.is_empty(),
        }
    }

    pub fn constants(self) -> [(&'static str, f64); 2] {
        [
            ("rect_lights_enabled", f64::from(u8::from(self.rect_lights))),
            ("decals_enabled", f64::from(u8::from(self.decals))),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    pub(super) pass: GeometryPass,
    pub(super) variant: Variant,
    layers: LayerConstants,
    lit: LitConstants,
}

impl PipelineKey {
    /// Casters take no layer or lit constants; indexed casters read the
    /// positions they are given, deformed or not, and pulled ones pull a
    /// deforming instance's deformed positions.
    pub fn new(
        pass: GeometryPass,
        variant: Variant,
        layers: LayerConstants,
        lit: LitConstants,
    ) -> Self {
        let caster = pass.caster();
        Self {
            pass,
            variant: Variant {
                deformed: variant.deformed && pass.pulled(),
                ..variant
            },
            layers: if caster { LayerConstants::ALL } else { layers },
            lit: if caster { LitConstants::default() } else { lit },
        }
    }

    /// The pipeline constants: a masked material's discard (`alpha_mask`,
    /// material_raster.wgsl), for the pulled passes deformed vertices, and
    /// for the camera and probe passes the layers and the lit constants.
    pub(super) fn constants(self) -> Vec<(&'static str, f64)> {
        let masked = (
            "alpha_mask",
            f64::from(u8::from(self.variant.alpha == Alpha::Mask)),
        );
        let deformed = (
            "deformed_vertices",
            f64::from(u8::from(self.variant.deformed)),
        );
        let mut constants = Vec::new();
        if !self.pass.caster() {
            constants.extend(self.layers.constants());
            constants.extend(self.lit.constants());
        }
        if !self.pass.caster() || self.variant.alpha == Alpha::Mask {
            constants.push(masked);
        }
        if self.pass.pulled() {
            constants.push(deformed);
        }
        constants
    }
}
