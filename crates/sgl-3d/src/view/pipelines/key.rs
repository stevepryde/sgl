//! What keys a geometry pipeline besides its pass and variant: the
//! diagnostics layers and the lit constants compiled into it, and the
//! constants each key sets.
use super::{Alpha, Cull, GeometryPass, Variant};
use crate::Scene;
use crate::content::identity::ShaderId;
use crate::settings::DisabledLayers;
use crate::shading::bind::group2;

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

    /// Only the diagnostic switches that change evaluated material maps.
    pub(crate) fn material_maps(self) -> Self {
        Self {
            normal_maps: self.normal_maps,
            bump_maps: self.bump_maps,
            ..Self::ALL
        }
    }

    fn map_constants(self) -> [(&'static str, f64); 2] {
        [
            ("normal_maps_enabled", f64::from(u8::from(self.normal_maps))),
            ("bump_maps_enabled", f64::from(u8::from(self.bump_maps))),
        ]
    }

    fn constants(self) -> Vec<(&'static str, f64)> {
        self.map_constants()
            .into_iter()
            .chain([
                (
                    "baked_lighting_enabled",
                    f64::from(u8::from(self.baked_lighting)),
                ),
                (
                    "instance_emission_enabled",
                    f64::from(u8::from(self.instance_emission)),
                ),
            ])
            .collect()
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
    /// Iridescent films (`films_enabled` in surface.wgsl), whose evaluation
    /// costs every lit fragment occupancy where it is compiled in, as
    /// Filament compiles it only into a material that has one
    /// (`MATERIAL_HAS_IRIDESCENCE`); the trade is a compile when the first
    /// filmed material is added or the last loses its film.
    pub films: bool,
    /// Sheens (`sheens_enabled` in surface.wgsl), compiled in only while a
    /// material has one, as Filament compiles its sheen only into a
    /// material that has one (`MATERIAL_HAS_SHEEN_COLOR`).
    pub sheens: bool,
    /// Diffuse transmission (`diffuse_transmission_enabled` in
    /// surface.wgsl): the transmitted lobe, its back-side shadows and
    /// indirect light, compiled in only while a material passes diffuse
    /// light through, as Bevy compiles it only into a material that does
    /// (`STANDARD_MATERIAL_DIFFUSE_TRANSMISSION`).
    pub diffuse_transmission: bool,
    /// Spot lights' cookies (`light_cookies_enabled` in light_cookie.wgsl),
    /// sampled only while the scene holds a cookied light, as Godot
    /// specialises its forward pipelines on `use_light_projector` (for each
    /// drawn element a projector reaches, where this follows the scene).
    pub light_cookies: bool,
    /// An occlusion map of its own and the specular and specular colour
    /// maps at ray hits (`occlusion_specular_maps_enabled`, declared in
    /// surface.wgsl and read in surface_ray.wgsl), sampled only while a
    /// material has one in effect,
    /// so a scene without them traces what it did before they existed.
    /// Raster takes them by material (`Variant::occlusion_specular`), so
    /// the geometry pipelines' keys leave this out.
    pub occlusion_specular: bool,
}

impl LitConstants {
    /// What `scene` holds.
    pub fn of(scene: &Scene) -> Self {
        Self {
            rect_lights: scene.lights.holds_rect(),
            decals: !scene.decals.is_empty(),
            films: scene.materials.holds_films(),
            sheens: scene.materials.holds_sheens(),
            diffuse_transmission: scene.materials.holds_diffuse_transmission(),
            light_cookies: scene.lights.holds_cookie(),
            occlusion_specular: scene.materials.holds_occlusion_specular(),
        }
    }

    pub fn constants(self) -> [(&'static str, f64); 7] {
        [
            ("rect_lights_enabled", f64::from(u8::from(self.rect_lights))),
            ("decals_enabled", f64::from(u8::from(self.decals))),
            ("films_enabled", f64::from(u8::from(self.films))),
            ("sheens_enabled", f64::from(u8::from(self.sheens))),
            (
                "diffuse_transmission_enabled",
                f64::from(u8::from(self.diffuse_transmission)),
            ),
            (
                "light_cookies_enabled",
                f64::from(u8::from(self.light_cookies)),
            ),
            (
                "occlusion_specular_maps_enabled",
                f64::from(u8::from(self.occlusion_specular)),
            ),
        ]
    }
}

/// What a geometry pipeline's constants specialise on: the diagnostics
/// layers, the lit constants and whether the blended passes compile
/// transmission in.
pub(super) type Specialisation = (LayerConstants, LitConstants, bool);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    pub(super) pass: GeometryPass,
    pub(super) variant: Variant,
    /// The material's shader, whose programs it is created from; none for
    /// SGL3D's default provider's.
    pub(super) shader: Option<ShaderId>,
    layers: LayerConstants,
    lit: LitConstants,
    /// A blended pass's transmission (`transmission_enabled` in
    /// transmission.wgsl), compiled in only while the scene holds a
    /// transmissive material, so blended surfaces pay nothing for it
    /// otherwise, as `films` does for films.
    transmission: bool,
}

impl PipelineKey {
    /// Shader-driven masked casters take material-map diagnostics; other
    /// casters and the volume layers take no layer or lit constants, and
    /// the volume layers cull no face, whatever the batch's side and pose;
    /// indexed casters read the positions they are given, deformed or not,
    /// and pulled ones pull a deforming instance's deformed positions. Only
    /// the blended passes take `transmission`.
    pub fn new(
        pass: GeometryPass,
        variant: Variant,
        shader: Option<ShaderId>,
        (layers, lit): (LayerConstants, LitConstants),
        transmission: bool,
    ) -> Self {
        let constant = pass.caster() || pass.volume();
        let shaded_mask = pass.caster() && variant.alpha == Alpha::Mask && shader.is_some();
        // The map bindings the pass's programs sample: none for a caster of
        // an opaque material, the base map alone for a masked material's
        // caster without a shader (material_base_color), else every one.
        let sampled: u32 = if !samples_maps(pass, variant.alpha) {
            0
        } else if pass.caster() && shader.is_none() {
            1 << group2::map_slot(group2::BASE_MAP)
        } else {
            u32::MAX
        };
        // Whether the pass's programs read the occlusion and specular maps:
        // every pass that samples every map but those whose outputs take
        // neither occlusion nor F0 (the receivers' traced lobe, which takes
        // only the G-buffer's lit bit of them, the temporal composition's
        // coverage and the anisotropy target), unless a game's shader, which
        // may read them for coverage, runs there; never the volume layers,
        // which evaluate no material. Where they are not read, their
        // bindings are no binding the pass samples, so they key none of its
        // variants.
        let reads_occlusion_specular = sampled == u32::MAX
            && !pass.volume()
            && (shader.is_some()
                || !matches!(
                    pass,
                    GeometryPass::Receivers
                        | GeometryPass::TemporalComposition { .. }
                        | GeometryPass::GBufferAnisotropy
                ));
        let sampled = if reads_occlusion_specular {
            sampled
        } else {
            sampled & !occlusion_specular_slots()
        };
        Self {
            pass,
            shader,
            variant: Variant {
                deformed: variant.deformed && pass.pulled(),
                shader_sampled: variant.shader_sampled & sampled,
                // Whether a sampled map takes the second set or a
                // transform, as the sampled bindings' mask or none: each
                // map's own bit is the record's (`map_uv_bits`), so
                // materials whose maps take them differently share
                // pipelines, and a key reduced again is unchanged.
                uv1: if variant.uv1 & sampled != 0 {
                    sampled
                } else {
                    0
                },
                transformed: if variant.transformed & sampled != 0 {
                    sampled
                } else {
                    0
                },
                occlusion_specular: variant.occlusion_specular && reads_occlusion_specular,
                cull: if pass.volume() {
                    Cull::None
                } else {
                    variant.cull
                },
                ..variant
            },
            layers: if shaded_mask {
                layers.material_maps()
            } else if constant {
                LayerConstants::ALL
            } else {
                layers
            },
            lit: if constant {
                LitConstants::default()
            } else {
                LitConstants {
                    occlusion_specular: false,
                    ..lit
                }
            },
            transmission: transmission && matches!(pass, GeometryPass::Blended { .. }),
        }
    }

    /// Whether the key belongs to this specialisation. Plain casters and
    /// volume layers belong to every specialisation; shader-driven masked
    /// casters follow the material-map diagnostics.
    pub(super) fn specialised(self, (layers, lit, transmission): Specialisation) -> bool {
        self == Self::new(
            self.pass,
            self.variant,
            self.shader,
            (layers, lit),
            transmission,
        )
    }

    /// The pipeline constants: a masked material's discard (`alpha_mask`,
    /// material_raster.wgsl), for the pulled passes deformed vertices, and
    /// for the camera and probe passes the layers and the lit constants and
    /// whether they write motion (`motion_vertices`, material_shader.wgsl:
    /// not the blended draws, the temporal composition mask or the volume
    /// layers), and for the
    /// blended passes whether transmission is compiled in.
    pub(super) fn constants(self) -> Vec<(&'static str, f64)> {
        // The temporal composition's footprint marks cut-out texels too.
        let discards = self.variant.alpha == Alpha::Mask
            && self.pass != (GeometryPass::TemporalComposition { footprint: true });
        let masked = ("alpha_mask", f64::from(u8::from(discards)));
        let deformed = (
            "deformed_vertices",
            f64::from(u8::from(self.variant.deformed)),
        );
        let mut constants = Vec::new();
        if self.pass.caster() && self.variant.alpha == Alpha::Mask && self.shader.is_some() {
            constants.extend(self.layers.map_constants());
        }
        if !self.pass.caster() {
            constants.extend(self.layers.constants());
            constants.extend(self.lit.constants());
            let motion = !matches!(
                self.pass,
                GeometryPass::Blended { .. } | GeometryPass::TemporalComposition { .. }
            ) && !self.pass.volume();
            constants.push(("motion_vertices", f64::from(u8::from(motion))));
        }
        if samples_maps(self.pass, self.variant.alpha) {
            constants.push(masked);
            constants.push((
                "material_shader_sampled_maps",
                f64::from(self.variant.shader_sampled),
            ));
            constants.push((
                "material_uv1_enabled",
                f64::from(u8::from(self.variant.uv1 != 0)),
            ));
            constants.push((
                "material_transforms_enabled",
                f64::from(u8::from(self.variant.transformed != 0)),
            ));
        }
        if self.pass.pulled() {
            constants.push(deformed);
        }
        if matches!(self.pass, GeometryPass::Blended { .. }) {
            constants.push((
                "transmission_enabled",
                f64::from(u8::from(self.transmission)),
            ));
        }
        constants
    }
}

/// The map bindings of an occlusion map of its own and the specular and
/// specular colour maps, a bit by `MAP_BINDINGS` index: those a program
/// samples only with their sampling provider (`Variant::occlusion_specular`).
fn occlusion_specular_slots() -> u32 {
    [
        group2::OCCLUSION_MAP,
        group2::SPECULAR_MAP,
        group2::SPECULAR_COLOR_MAP,
    ]
    .into_iter()
    .fold(0, |bits, binding| bits | 1 << group2::map_slot(binding))
}

/// Whether `pass`'s programs sample a material of `alpha`'s maps
/// (material_raster.wgsl), so take its `alpha_mask`,
/// `material_shader_sampled_maps`, `material_uv1_enabled` and
/// `material_transforms_enabled` constants: every pass but a caster of an
/// opaque material, which reads no texel.
fn samples_maps(pass: GeometryPass, alpha: Alpha) -> bool {
    !pass.caster() || alpha == Alpha::Mask
}
