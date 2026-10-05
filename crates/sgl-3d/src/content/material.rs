//! A material's values, which `Scene::material` reads and
//! `Scene::set_material` replaces, and how its alpha is used.
use super::asset::Material;

/// How a material uses its alpha: glTF's `alphaMode`, with Bevy's
/// `AlphaMode` names (Bevy 9d12036, its phases and pipeline keys in
/// `crates/bevy_pbr/src/material.rs`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AlphaMode {
    /// Alpha is ignored.
    #[default]
    Opaque,
    /// Opaque where the base alpha reaches `cutoff` and cut out below it, in
    /// every view, shadow and ray (glTF `MASK` and its `alphaCutoff`).
    Mask { cutoff: f32 },
    /// Blended over what lies behind it, sorted back to front by each mesh's
    /// bounds centre (glTF `BLEND`, which loads unmarked). It casts no
    /// shadow, and rays pass through it. Unmarked, it writes no depth or
    /// motion. Marked `receives_screen_space_reflections`, it is the surface
    /// the frame's screen-space reflections trace and its temporal effects
    /// reproject at the pixels where it is the nearest receiver: water or
    /// glass that reflects the scene in front of it.
    Blend {
        receives_screen_space_reflections: bool,
    },
}

/// One layer of a material's scrolling normals
/// ([`SurfaceMaterial::normal_layers`]): the material's normal map drawn at
/// its own scale, moving across the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalLayer {
    /// The direction and speed the layer moves across the surface, in the
    /// material's UV units per second along U and V. SGL3D rounds it to a
    /// whole number of repeats of the layer's map per hour, so the layer is
    /// where it was an hour earlier and long sessions keep their precision:
    /// in steps of 1/(3600 × `scale`) UV units per second, at most half a
    /// step off, and a layer slower than 1/7200 of a repeat per second
    /// stands still. At most 2^24 repeats per hour (about 4660 a second).
    pub velocity: [f32; 2],
    /// How many times the normal map repeats per unit of the material's UVs;
    /// positive.
    pub scale: f32,
    /// How much of the layer's slopes the surface takes, with the
    /// material's `normal_scale`: 1 as the map is authored, 0 none.
    pub strength: f32,
}

impl Default for NormalLayer {
    /// The map as authored, at the material's UVs, standing still.
    fn default() -> Self {
        Self {
            velocity: [0.; 2],
            scale: 1.,
            strength: 1.,
        }
    }
}

/// A material's values, read by [`Scene::material`](crate::Scene::material)
/// and replaced by [`Scene::set_material`](crate::Scene::set_material).
/// Author initial content with [`asset::Material`](crate::asset::Material);
/// the textures it was added with stay as they were.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceMaterial {
    /// Linear RGB base multiplier and alpha.
    pub base: [f32; 4],
    /// Linear RGB emission.
    pub emission: [f32; 3],
    /// Metallic factor in `0..=1`.
    pub metallic: f32,
    /// Perceptual roughness in `0..=1`.
    pub roughness: f32,
    /// Scalar clearcoat intensity.
    pub clearcoat: f32,
    /// Clearcoat perceptual roughness.
    pub coat_roughness: f32,
    /// Scale of the normal map's tangent-space X and Y.
    pub normal_scale: f32,
    /// Scrolling normals: the normal map drawn as two layers that move
    /// across the surface, their slopes added as superposed waves' are, at
    /// the frame's `FrameInput::elapsed_seconds`: water's moving waves, with
    /// no geometry uploaded per frame. Needs a normal map that repeats on
    /// both axes. `None` draws the map once at the material's UVs.
    pub normal_layers: Option<[NormalLayer; 2]>,
    /// Bump height multiplier.
    pub bump_scale: f32,
    /// Anisotropy strength in `0..=1`; zero is isotropic.
    pub anisotropy_strength: f32,
    /// Counter-clockwise tangent-space rotation of the anisotropy direction,
    /// in radians.
    pub anisotropy_rotation: f32,
    /// Multiplier of the environment's diffuse and specular light.
    pub environment_scale: f32,
    /// Visibility group: every bit must be enabled by the frame's mask.
    /// Zero is always visible.
    pub visibility_group: u32,
    /// Whether the material bypasses lighting.
    pub unlit: bool,
    /// Whether global illumination gathers the light the material gives off
    /// itself (its emission, and an unlit material's whole colour): today
    /// the dynamic GI volume's probe rays. Set it false on a fixture that a
    /// scene light stands for, such as a lamp's glowing panel beside its
    /// rectangle light, so its light reaches other surfaces once, through
    /// the light, as Unity's emission "Global Illumination: None" keeps a
    /// glowing material's light out of its GI. The surface still glows,
    /// shows in reflections, blocks the probes' rays and, when lit, bounces
    /// the light that reaches it.
    pub emits_into_gi: bool,
    /// Whether both sides of each triangle are drawn.
    pub double_sided: bool,
    pub alpha: AlphaMode,
}

impl Default for SurfaceMaterial {
    /// The values of the default authored material
    /// (`asset::Material::default()`, glTF 2.0's), lit by the whole
    /// environment (`environment_scale` 1). Set what
    /// differs and take the rest with `..Default::default()`, or from
    /// [`Scene::material`](crate::Scene::material) to change a material's
    /// current values.
    fn default() -> Self {
        Self::authored(&Material::default())
    }
}

impl SurfaceMaterial {
    /// The values of an authored material, as a scene adds them.
    pub(crate) fn authored(m: &Material) -> Self {
        Self {
            base: m.base,
            emission: m.emissive,
            metallic: m.metallic,
            roughness: m.roughness,
            clearcoat: m.clearcoat,
            coat_roughness: m.coat_roughness,
            normal_scale: m.normal_scale,
            normal_layers: m.normal_layers,
            bump_scale: m.bump_scale,
            anisotropy_strength: m.anisotropy_strength,
            anisotropy_rotation: m.anisotropy_rotation,
            environment_scale: 1.,
            visibility_group: m.visibility_group,
            unlit: m.unlit,
            emits_into_gi: m.emits_into_gi,
            double_sided: m.double_sided,
            alpha: m.alpha,
        }
    }

    /// Whether the material is drawn by the transparent stage rather than
    /// with opaque surfaces.
    pub(crate) fn blended(&self) -> bool {
        matches!(self.alpha, AlphaMode::Blend { .. })
    }

    /// Whether the material is a blended receiver of screen-space
    /// reflections.
    pub(crate) fn receives_screen_space_reflections(&self) -> bool {
        matches!(
            self.alpha,
            AlphaMode::Blend {
                receives_screen_space_reflections: true
            }
        )
    }

    /// What its shadow casters depend on: its side, its visibility group
    /// and, for a masked material, its cutoff and base alpha.
    pub(crate) fn caster_values(&self) -> (bool, u32, AlphaMode, f32) {
        let alpha = match self.alpha {
            AlphaMode::Mask { .. } => self.base[3],
            _ => 1.,
        };
        (self.double_sided, self.visibility_group, self.alpha, alpha)
    }
}
