//! A material's values, which `Scene::material` reads and
//! `Scene::set_material` replaces, and how its alpha is used.
use super::asset::Material;
use super::shader::MaterialShader;

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
    /// bounds centre (glTF `BLEND`, which loads with both fields false). It
    /// casts no shadow, and rays pass through it. Unmarked, it writes no
    /// depth or motion. Marked `receives_screen_space_reflections`, it is the
    /// surface the frame's screen-space reflections trace and its temporal
    /// effects reproject at the pixels where it is the nearest receiver:
    /// water or glass that reflects the scene in front of it.
    Blend {
        receives_screen_space_reflections: bool,
        /// Whether alpha fades only its diffuse and emitted light, its
        /// reflections and highlights keeping their full strength, as glass
        /// does (Filament's `transparent` blending). False fades all of its
        /// light, glTF's alpha as coverage (Filament's `fade`).
        keeps_specular: bool,
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
    /// Linear RGB base multiplier, each channel finite and nonnegative, and
    /// alpha in `0..=1`.
    pub base: [f32; 4],
    /// Linear RGB emission, each channel finite and nonnegative.
    pub emission: [f32; 3],
    /// Metallic factor in `0..=1`.
    pub metallic: f32,
    /// Perceptual roughness in `0..=1`.
    pub roughness: f32,
    /// Index of refraction, at least 1 or `f32::INFINITY`, which sets the
    /// dielectric F0, ((ior − 1) / (ior + 1))²: 1.5 gives 0.04, water's 1.33
    /// 0.02 ([`asset::Material::ior`](crate::asset::Material::ior)).
    pub ior: f32,
    /// Strength of the dielectric specular reflection in `0..=1`: 1 as the
    /// IOR gives it, 0 none. It scales the dielectric F0 and is its
    /// reflectance at grazing incidence (F90), which metallic mixes toward 1.
    pub specular: f32,
    /// Linear tint of the dielectric F0, finite and nonnegative: the F0 is
    /// the IOR's times this, at most 1, times `specular`.
    pub specular_color: [f32; 3],
    /// How much of the material's occlusion map applies, in `0..=1`; nothing
    /// without one
    /// ([`asset::Material::occlusion_texture`](crate::asset::Material::occlusion_texture)).
    pub occlusion_strength: f32,
    /// Clearcoat intensity in `0..=1`, which a clearcoat map's red channel
    /// multiplies.
    pub clearcoat: f32,
    /// Clearcoat perceptual roughness in `0..=1`, which a clearcoat
    /// roughness map's green channel multiplies.
    pub coat_roughness: f32,
    /// Scale applied to the clearcoat normal map's tangent-space X and Y
    /// components ([`Material::coat_normal_scale`]).
    pub coat_normal_scale: f32,
    /// A thin film's strength in `0..=1`, which an iridescence map's red
    /// channel multiplies ([`Material::iridescence`]).
    pub iridescence: f32,
    /// The film's index of refraction, at least 1.
    pub iridescence_ior: f32,
    /// The film's thinnest and thickest thickness in nanometres, each finite
    /// and nonnegative ([`Material::iridescence_thickness`]).
    pub iridescence_thickness: [f32; 2],
    /// A sheen's linear colour, each channel in `0..=1`, which a sheen
    /// colour map's RGB multiplies ([`Material::sheen_color`]): black none.
    pub sheen_color: [f32; 3],
    /// The sheen's perceptual roughness in `0..=1`, which a sheen roughness
    /// map's alpha multiplies ([`Material::sheen_roughness`]).
    pub sheen_roughness: f32,
    /// The share in `0..=1` of the light the base diffuses that passes to
    /// the surface's other side, which a diffuse transmission map's alpha
    /// multiplies ([`Material::diffuse_transmission`]): 0 none.
    pub diffuse_transmission: f32,
    /// The linear colour of the light it passes through, each channel finite
    /// and nonnegative, which a diffuse transmission colour map's RGB multiplies
    /// ([`Material::diffuse_transmission_color`]).
    pub diffuse_transmission_color: [f32; 3],
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
    /// The share of the light behind the surface that passes through it,
    /// refracted, in `0..=1` (`KHR_materials_transmission`), in place of
    /// that share of its diffuse light: glass, water, clear plastic. A
    /// material with any transmission is drawn with the blended surfaces
    /// whatever its alpha mode (its alpha still covers as the mode says): it
    /// writes no depth, G-buffer or motion unless it is a `Blend` receiver
    /// of screen-space reflections, takes no ambient occlusion and casts no
    /// shadow, rays pass through it and probe captures leave it out, and
    /// what it shows through it is the opaque frame behind it, blurred by
    /// its roughness, not other blended or transmissive surfaces. Give glass
    /// its own material: a transmission map's zeros do not make the rest of
    /// the material opaque. On a device of the `Basic` binding tier, and
    /// in what screen-space reflections see of it, the light behind it
    /// passes unrefracted and unblurred, dimmed as the refracted light is.
    pub transmission: f32,
    /// The thickness of the volume beneath the surface, in the mesh's units
    /// (the instance's scale applies), which the transmitted light crosses
    /// (`KHR_materials_volume`): 0 is a thin wall, which bends no light; a
    /// thicker one is a closed volume, whose back faces are not drawn.
    pub thickness: f32,
    /// The distance in metres light travels through the volume before white
    /// light takes `attenuation_color` (Beer-Lambert), positive, or
    /// `f32::INFINITY` for no attenuation.
    pub attenuation_distance: f32,
    /// The linear colour white light turns into at `attenuation_distance`,
    /// each channel in `0..=1`.
    pub attenuation_color: [f32; 3],
    /// How far transmitted light's colours spread, as 20 over the Abbe
    /// number, nonnegative (`KHR_materials_dispersion`): 0 none, 0.33 crown
    /// glass, 2 an exaggerated prism.
    pub dispersion: f32,
    /// Anisotropy strength in `0..=1`; zero is isotropic.
    pub anisotropy_strength: f32,
    /// Counter-clockwise tangent-space rotation of the anisotropy direction,
    /// in radians.
    pub anisotropy_rotation: f32,
    /// Multiplier of the environment's diffuse and specular light, finite
    /// and nonnegative.
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
    /// The game's shader (`Scene::add_shader`) its surfaces are evaluated
    /// through, which a game names with `Scene::set_material` after adding
    /// the material (an authored material has none): its vertex function
    /// places every vertex in every view that rasterises it and its surface
    /// function finishes every fragment from these values and the maps,
    /// with what culling allows for its displacement. Its parameters start as zeros
    /// (`Scene::set_shader_parameters`). A shader material's surface counts
    /// as moving for FSR2's composition mask, and its `double_sided` holds
    /// even for a volume (`thickness` above 0). Rays, bakes and the static
    /// layers of local-light shadows see its rest geometry and these values.
    /// None by default.
    pub shader: Option<MaterialShader>,
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
            ior: m.ior,
            specular: m.specular,
            specular_color: m.specular_color,
            occlusion_strength: m.occlusion_strength,
            clearcoat: m.clearcoat,
            coat_roughness: m.coat_roughness,
            coat_normal_scale: m.coat_normal_scale,
            iridescence: m.iridescence,
            iridescence_ior: m.iridescence_ior,
            iridescence_thickness: m.iridescence_thickness,
            sheen_color: m.sheen_color,
            sheen_roughness: m.sheen_roughness,
            diffuse_transmission: m.diffuse_transmission,
            diffuse_transmission_color: m.diffuse_transmission_color,
            normal_scale: m.normal_scale,
            normal_layers: m.normal_layers,
            bump_scale: m.bump_scale,
            transmission: m.transmission,
            thickness: m.thickness,
            attenuation_distance: m.attenuation_distance,
            attenuation_color: m.attenuation_color,
            dispersion: m.dispersion,
            anisotropy_strength: m.anisotropy_strength,
            anisotropy_rotation: m.anisotropy_rotation,
            environment_scale: 1.,
            visibility_group: m.visibility_group,
            unlit: m.unlit,
            emits_into_gi: m.emits_into_gi,
            double_sided: m.double_sided,
            alpha: m.alpha,
            shader: None,
        }
    }

    /// Whether the material is drawn by the transparent stage rather than
    /// with opaque surfaces: blended, or transmissive whatever its alpha
    /// mode. This is the population every view, cache and ray takes it by.
    pub(crate) fn blended(&self) -> bool {
        matches!(self.alpha, AlphaMode::Blend { .. }) || self.transmissive()
    }

    /// Whether light behind it passes through it, refracted.
    pub(crate) fn transmissive(&self) -> bool {
        self.transmission > 0.
    }

    /// Whether it bounds a volume, whose back faces are not drawn
    /// (`KHR_materials_volume`: `doubleSided` does not apply to a volume).
    /// A material with a shader is the game's to shade on either side
    /// (its surface context's `front`), so its `double_sided` holds.
    pub(crate) fn volume(&self) -> bool {
        self.transmissive() && self.thickness > 0. && self.shader.is_none()
    }

    /// Whether the material is a blended receiver of screen-space
    /// reflections.
    pub(crate) fn receives_screen_space_reflections(&self) -> bool {
        matches!(
            self.alpha,
            AlphaMode::Blend {
                receives_screen_space_reflections: true,
                ..
            }
        )
    }

    /// What its shadow casters depend on: its side, its visibility group,
    /// whether it is drawn blended (and casts nothing), for a masked
    /// material, its cutoff and base alpha, and its shader, which places
    /// their vertices.
    pub(crate) fn caster_values(
        &self,
    ) -> (bool, u32, AlphaMode, bool, f32, Option<MaterialShader>) {
        let alpha = match self.alpha {
            AlphaMode::Mask { .. } => self.base[3],
            _ => 1.,
        };
        (
            self.double_sided,
            self.visibility_group,
            self.alpha,
            self.blended(),
            alpha,
            self.shader,
        )
    }
}
