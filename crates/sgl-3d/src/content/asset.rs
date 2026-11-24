//! glTF 2.0 mesh loading for the SGL renderer.
//!
//! Rigid nodes' transforms are baked into vertices, mirrored winding is
//! corrected, and primitives are batched by material, skin and morphed node;
//! skins, morph targets, the node hierarchy and animation clips are imported
//! as plain data (`deformation`). The default scene (or first scene)
//! is loaded. An extension the file requires that SGL3D does not support,
//! and other unsupported visible features, return an error containing the
//! asset path instead of producing a partial model; an unsupported extension
//! the file only uses is left out and listed in [`Asset::ignored`]. Opaque,
//! masked and blended alpha modes are supported. Supported material
//! extensions are scalar clearcoat, anisotropy (on authored tangents, or
//! ones the loader generates: `LoadOptions::tangents`), emissive strength,
//! unlit, bump mapping, IOR, and specular with its maps.
//! Authored punctual lights (`KHR_lights_punctual`) load as data in
//! [`Asset::lights`], and `EXT_mesh_gpu_instancing` nodes as shared
//! geometry and placements in [`Asset::instanced`], for the game to install.
use std::error::Error;

use glam::Vec3;

use super::deformation::{MeshDeformation, Rig};
pub use super::gltf::{
    AssetLights, GltfImage, Ignored, ImageSource, InstancedMesh, InstancedNode, LightPlacement,
    LoadOptions, PlacedLight, PunctualKind, PunctualLight, TangentFrames, load, load_slice,
    load_slice_with_options, load_with_options,
};
#[cfg(target_arch = "wasm32")]
pub use super::gltf::{decode_image_in_browser, load_slice_in_browser};
#[cfg(target_arch = "wasm32")]
pub use super::images::BrowserImage;
pub use super::images::{CompressedFormat, CompressedImage, Image, ImageError, Rgba8MipChain};
pub use super::load_budget::LoadBudget;
use super::material::{AlphaMode, NormalLayer};
pub use super::material_texture::{
    MaterialTexture, Sampler, TextureFilter, TextureWrap, UvSet, UvTransform,
};

/// A loader failure, including the source asset path for file loads.
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Interleaved vertex input used by the renderer, in the asset's coordinate system.
/// The scene packs each into 32 bytes when its model is prepared, and its
/// second UV set into 4 more where its mesh has one
/// (`PreparedModel::new`; the README's asset limits give the precision).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Position after authored node transforms have been applied, kept exact.
    pub position: [f32; 3],
    /// Unit surface normal, transformed by the inverse transpose of the node matrix.
    /// Finite and not zero, or the mesh is refused; kept within 0.01°.
    pub normal: [f32; 3],
    /// First texture coordinate set (`TEXCOORD_0`), kept as 16-bit fractions
    /// of the rectangle its mesh's UVs span. Finite, or the mesh is refused,
    /// as is a mesh whose UVs span more than `f32` holds.
    pub uv: [f32; 2],
    /// Second texture coordinate set (`TEXCOORD_1`), which a map on
    /// [`UvSet::Uv1`] samples; zero for a vertex without one. Kept as
    /// 16-bit fractions of the rectangle its mesh's span, 4 bytes a
    /// vertex, only for a mesh with a vertex whose `uv1` is not zero.
    /// Finite, or the mesh is refused. The glTF loader fills it only for
    /// meshes whose material has a map on that set.
    pub uv1: [f32; 2],
    /// Linear vertex color multiplier, including alpha. Clamped to 0..=1 and
    /// kept as 8-bit sRGB with 8-bit linear alpha.
    pub color: [f32; 4],
    /// Normalized UV into the caller-baked static irradiance atlas, which
    /// the glTF loader reads from `TEXCOORD_1`; it and `lightmap_bounds`
    /// alone select the lightmap chart, whatever `uv1` holds.
    /// (0, 0) or a negative UV marks a surface without an atlas chart: it has no
    /// baked lighting, so baked scene lights light it. Kept as 16-bit fractions.
    /// Finite, or the mesh is refused.
    pub lightmap_uv: [f32; 2],
    /// Normalized atlas min/max for a conservatively cropped triangle chart.
    /// Supply the same bounds on all triangle vertices; outside is black.
    /// A mesh's vertices may name at most 65,536 distinct bounds.
    pub lightmap_bounds: [f32; 4],
    /// Unit tangent XYZ and bitangent handedness W after node transforms:
    /// glTF's frame of the normal map's UV set (T along increasing U,
    /// B = (N × T)·W toward decreasing V). Zero means absent, as does a
    /// tangent within 0.02° of its normal. A material takes its meshes'
    /// tangents for its normal maps while every mesh drawing it has them,
    /// so a tangent that is not that frame (a placeholder) shades them
    /// wrong: leave it zero. Anisotropy requires a valid frame.
    /// Made perpendicular to the normal and unit, then kept within 0.01°.
    pub tangent: [f32; 4],
}

/// An indexed triangle mesh retained on the CPU.
#[derive(Clone)]
pub struct CpuMesh {
    /// Vertex attributes shared by this mesh's triangles.
    pub vertices: Vec<Vertex>,
    /// Triangle-list indices into [`Self::vertices`], three per face.
    pub indices: Vec<u32>,
    /// Index into the containing [`Asset::materials`].
    pub material: usize,
    /// Its skin and morph targets, which name [`Asset::rig`]'s joints and
    /// morph weights; rigid by default.
    pub deformation: MeshDeformation,
}

/// Metallic/roughness material inputs understood by the renderer.
#[derive(Clone)]
pub struct Material {
    /// Authored material label, retained for diagnostics.
    pub name: String,
    /// Caller-selected visibility group. Zero is always visible; nonzero groups
    /// are visible when selected by the scene. Imported materials start at zero.
    pub visibility_group: u32,
    /// Whether this material casts directional shadows when its visibility group is enabled.
    /// Imported materials cast by default; games may opt out decorative inlays explicitly.
    pub casts_directional_shadow: bool,
    /// Linear base color multiplier, each channel finite and nonnegative,
    /// and alpha in `0..=1`.
    pub base: [f32; 4],
    /// Linear emissive color with emissive strength already applied, each
    /// channel finite and nonnegative.
    pub emissive: [f32; 3],
    /// Metallic factor in `0..=1`.
    pub metallic: f32,
    /// Perceptual roughness factor in `0..=1`.
    pub roughness: f32,
    /// Index of refraction (`KHR_materials_ior`): at least 1, or
    /// `f32::INFINITY` (glTF's `ior` 0). It sets the dielectric F0,
    /// ((ior − 1) / (ior + 1))²: 0.04 at glTF's default 1.5, 0.02 for water
    /// at 1.33.
    pub ior: f32,
    /// Strength of the dielectric specular reflection in `0..=1`
    /// (`KHR_materials_specular`'s `specularFactor`): 1 as the IOR gives
    /// it, 0 none. It scales the dielectric F0 and is its reflectance at
    /// grazing incidence (F90), which metallic mixes toward 1.
    pub specular: f32,
    /// Linear tint of the dielectric F0, each channel finite and
    /// nonnegative (`KHR_materials_specular`'s `specularColorFactor`): the
    /// F0 is the IOR's times this, at most 1, times `specular`.
    pub specular_color: [f32; 3],
    /// Specular map (`KHR_materials_specular`'s `specularTexture`), sampled
    /// as linear data: its alpha channel multiplies `specular`, the F0's
    /// scale and the F90 alike. A device of the `Basic` binding tier binds
    /// none and takes the factor alone.
    pub specular_texture: Option<MaterialTexture>,
    /// Specular colour map (`specularColorTexture`), sampled as sRGB: its
    /// RGB multiplies `specular_color` within the F0's clamp to 1. `Basic`
    /// takes the factor alone.
    pub specular_color_texture: Option<MaterialTexture>,
    /// Clearcoat intensity in `0..=1` (`KHR_materials_clearcoat`'s
    /// `clearcoatFactor`).
    pub clearcoat: f32,
    /// Clearcoat perceptual roughness in `0..=1`
    /// (`clearcoatRoughnessFactor`).
    pub coat_roughness: f32,
    /// Clearcoat map, sampled as linear data: its red channel
    /// multiplies `clearcoat` (`clearcoatTexture`).
    pub clearcoat_texture: Option<MaterialTexture>,
    /// Clearcoat roughness map, sampled as linear data: its green
    /// channel multiplies `coat_roughness` (`clearcoatRoughnessTexture`).
    pub coat_roughness_texture: Option<MaterialTexture>,
    /// Tangent-space clearcoat normal map, sampled as linear data
    /// (`clearcoatNormalTexture`): the coat's normal, on the base normal
    /// map's tangent frame. Without it the coat follows the geometry
    /// normal, whatever the base's normal map does.
    pub coat_normal_texture: Option<MaterialTexture>,
    /// Finite scale applied to the clearcoat normal map's tangent-space X and Y
    /// components (`clearcoatNormalTexture.scale`).
    pub coat_normal_scale: f32,
    /// Strength of a thin film over the surface in `0..=1`
    /// (`KHR_materials_iridescence`'s `iridescenceFactor`): 0 none.
    pub iridescence: f32,
    /// The film's index of refraction, at least 1 (`iridescenceIor`).
    pub iridescence_ior: f32,
    /// The film's thinnest and thickest thickness in nanometres
    /// (`iridescenceThicknessMinimum`, `iridescenceThicknessMaximum`), each
    /// finite and nonnegative: the thickness map's green channel mixes from
    /// the first to the second, and without the map the film is the second.
    pub iridescence_thickness: [f32; 2],
    /// Iridescence map, sampled as linear data: its red channel
    /// multiplies `iridescence` (`iridescenceTexture`).
    pub iridescence_texture: Option<MaterialTexture>,
    /// Iridescence thickness map, sampled as linear data: its green
    /// channel mixes `iridescence_thickness` (`iridescenceThicknessTexture`).
    pub iridescence_thickness_texture: Option<MaterialTexture>,
    /// Linear colour of a sheen over the surface, each channel in `0..=1`
    /// (`KHR_materials_sheen`'s `sheenColorFactor`): black none. Fabric and
    /// cloth.
    pub sheen_color: [f32; 3],
    /// The sheen's perceptual roughness in `0..=1` (`sheenRoughnessFactor`).
    pub sheen_roughness: f32,
    /// Sheen colour map, sampled as sRGB: its RGB multiplies
    /// `sheen_color` (`sheenColorTexture`).
    pub sheen_color_texture: Option<MaterialTexture>,
    /// Sheen roughness map, sampled as linear data: its alpha
    /// channel multiplies `sheen_roughness` (`sheenRoughnessTexture`).
    pub sheen_roughness_texture: Option<MaterialTexture>,
    /// The share in `0..=1` of the light the base diffuses that passes
    /// through to the surface's other side
    /// (`KHR_materials_diffuse_transmission`'s `diffuseTransmissionFactor`):
    /// 0 none. Leaves, paper, flags and lampshades.
    pub diffuse_transmission: f32,
    /// Linear colour of the light it passes through, each channel finite
    /// and nonnegative (`diffuseTransmissionColorFactor`).
    pub diffuse_transmission_color: [f32; 3],
    /// Diffuse transmission map, sampled as linear data: its alpha
    /// channel multiplies `diffuse_transmission`
    /// (`diffuseTransmissionTexture`).
    pub diffuse_transmission_texture: Option<MaterialTexture>,
    /// Diffuse transmission colour map, sampled as sRGB: its RGB
    /// multiplies `diffuse_transmission_color`
    /// (`diffuseTransmissionColorTexture`).
    pub diffuse_transmission_color_texture: Option<MaterialTexture>,
    /// KHR_materials_anisotropy strength in `0..=1`; zero preserves isotropic shading.
    pub anisotropy_strength: f32,
    /// Counter-clockwise direction rotation in tangent/bitangent space, in radians.
    pub anisotropy_rotation: f32,
    /// Anisotropy map, sampled as linear data: normalized remapped RG direction and B strength
    /// multiplier. A device of the `Basic` binding tier binds no anisotropy
    /// map and shades the factors alone (`Renderer::binding_tier`).
    pub anisotropy_texture: Option<MaterialTexture>,
    /// Base color map, sampled as sRGB.
    pub base_texture: Option<MaterialTexture>,
    /// Metallic (blue) / roughness (green) map, sampled as linear data.
    pub mr_texture: Option<MaterialTexture>,
    /// Occlusion map (glTF's `occlusionTexture`), sampled as linear data:
    /// its red channel is the share of ambient light that reaches the
    /// surface, which occludes its ambient light, its baked light
    /// (lightmap, atlas chart or ambient cube) and its environment
    /// specular, and shadows its direct light's cavities (micro-shadowing).
    /// Where it is `mr_texture`, image, sampler, UV set and transform (ORM
    /// packing: occlusion, roughness and metallic in red, green and blue),
    /// it is read from that map's sample on both binding tiers; any other
    /// is sampled on its own on the `Extended` tier, and a `Basic` device
    /// takes none.
    pub occlusion_texture: Option<MaterialTexture>,
    /// How much of the occlusion map applies, in `0..=1` (glTF's
    /// `occlusionTexture.strength`): 1 as authored, 0 none.
    pub occlusion_strength: f32,
    /// Emissive map, sampled as sRGB.
    pub emissive_texture: Option<MaterialTexture>,
    /// Tangent-space normal map, sampled as linear data.
    pub normal_texture: Option<MaterialTexture>,
    /// Finite scale applied to the normal map's tangent-space X and Y components.
    pub normal_scale: f32,
    /// The normal map drawn as two scrolling layers
    /// ([`SurfaceMaterial::normal_layers`](crate::SurfaceMaterial::normal_layers));
    /// `None` draws it once. glTF has no such extension: the loader leaves
    /// it `None`.
    pub normal_layers: Option<[NormalLayer; 2]>,
    /// Bump map, sampled as linear height data.
    pub bump_texture: Option<MaterialTexture>,
    /// Finite bump height multiplier
    /// ([`SurfaceMaterial::bump_scale`](crate::SurfaceMaterial::bump_scale)).
    pub bump_scale: f32,
    /// The share of the light behind the surface that passes through it,
    /// refracted, in `0..=1` (`KHR_materials_transmission`'s
    /// `transmissionFactor`); see
    /// [`SurfaceMaterial::transmission`](crate::SurfaceMaterial::transmission).
    pub transmission: f32,
    /// Transmission map: its red channel multiplies `transmission`,
    /// sampled as linear data. A device of the `Basic` binding tier binds
    /// none and takes the factor alone.
    pub transmission_texture: Option<MaterialTexture>,
    /// The thickness of the volume beneath the surface in the mesh's units,
    /// 0 for a thin wall (`KHR_materials_volume`'s `thicknessFactor`).
    pub thickness: f32,
    /// Thickness map: its green channel multiplies `thickness`,
    /// sampled as linear data; `Basic` takes the factor alone.
    pub thickness_texture: Option<MaterialTexture>,
    /// The distance in metres light travels in the volume before it takes
    /// `attenuation_color`, positive or `f32::INFINITY` for none
    /// (`KHR_materials_volume`).
    pub attenuation_distance: f32,
    /// The linear colour white light turns into at `attenuation_distance`,
    /// each channel in `0..=1`.
    pub attenuation_color: [f32; 3],
    /// How far transmitted light's colours spread, 20 over the Abbe number,
    /// nonnegative (`KHR_materials_dispersion`): 0 none.
    pub dispersion: f32,
    /// Whether both sides of each triangle are rendered.
    pub double_sided: bool,
    /// Whether the material bypasses lighting (`KHR_materials_unlit`).
    pub unlit: bool,
    /// Whether global illumination gathers the light the material gives off
    /// itself
    /// ([`SurfaceMaterial::emits_into_gi`](crate::SurfaceMaterial::emits_into_gi)).
    /// Imported materials do; set it false on a fixture that a scene light
    /// stands for.
    pub emits_into_gi: bool,
    /// How the base alpha is used: opaque, masked or blended.
    pub alpha: AlphaMode,
}

impl Material {
    /// Whether its occlusion map is the red channel of its metallic-roughness
    /// map, the same image sampled the same way at the same coordinates (ORM
    /// packing), which shading reads from that map's sample.
    pub(crate) fn packed_occlusion(&self) -> bool {
        self.occlusion_texture.is_some() && self.occlusion_texture == self.mr_texture
    }

    /// Whether a vertex tangent frame serves it: it has a normal map or
    /// anisotropy, or a clearcoat normal map on the frame's set, the normal
    /// map's (the first without one), as specs/sgl3d-architecture.md's
    /// Material map coordinates take the vertex frame. A glTF primitive it
    /// draws that carries no TANGENT is given MikkTSpace's
    /// (`LoadOptions::tangents`).
    pub(crate) fn takes_vertex_frame(&self) -> bool {
        self.normal_texture.is_some()
            || self.anisotropy_strength > 0.0
            || self
                .coat_normal_texture
                .as_ref()
                .is_some_and(|texture| texture.uv_set == self.frame_set())
    }

    /// The UV set a vertex tangent frame is the frame of: the normal map's,
    /// the first without one (specs/sgl3d-architecture.md, Material map
    /// coordinates).
    pub(crate) fn frame_set(&self) -> UvSet {
        self.normal_texture
            .as_ref()
            .map_or(UvSet::Uv0, |texture| texture.uv_set)
    }

    /// Every map SGL3D samples: each it names but a bump map beside a
    /// normal map, which shading never takes.
    pub(crate) fn sampled_textures(&self) -> impl Iterator<Item = MaterialTexture> {
        let [base, mr, occlusion, emissive, normal, bump, rest @ ..] = self.textures();
        let bump = bump.filter(|_| normal.is_none());
        [base, mr, occlusion, emissive, normal, bump]
            .into_iter()
            .chain(rest)
            .flatten()
    }

    /// Every map it names, each field once, the occlusion map third: a new
    /// map's field fails to compile here until it is listed.
    pub(crate) fn textures(&self) -> [Option<MaterialTexture>; 20] {
        let Self {
            name: _,
            visibility_group: _,
            casts_directional_shadow: _,
            base: _,
            emissive: _,
            metallic: _,
            roughness: _,
            ior: _,
            specular: _,
            specular_color: _,
            specular_texture,
            specular_color_texture,
            clearcoat: _,
            coat_roughness: _,
            clearcoat_texture,
            coat_roughness_texture,
            coat_normal_texture,
            coat_normal_scale: _,
            iridescence: _,
            iridescence_ior: _,
            iridescence_thickness: _,
            iridescence_texture,
            iridescence_thickness_texture,
            sheen_color: _,
            sheen_roughness: _,
            sheen_color_texture,
            sheen_roughness_texture,
            diffuse_transmission: _,
            diffuse_transmission_color: _,
            diffuse_transmission_texture,
            diffuse_transmission_color_texture,
            anisotropy_strength: _,
            anisotropy_rotation: _,
            anisotropy_texture,
            base_texture,
            mr_texture,
            occlusion_texture,
            occlusion_strength: _,
            emissive_texture,
            normal_texture,
            normal_scale: _,
            normal_layers: _,
            bump_texture,
            bump_scale: _,
            transmission: _,
            transmission_texture,
            thickness: _,
            thickness_texture,
            attenuation_distance: _,
            attenuation_color: _,
            dispersion: _,
            double_sided: _,
            unlit: _,
            emits_into_gi: _,
            alpha: _,
        } = self;
        [
            *base_texture,
            *mr_texture,
            *occlusion_texture,
            *emissive_texture,
            *normal_texture,
            *bump_texture,
            *anisotropy_texture,
            *clearcoat_texture,
            *coat_roughness_texture,
            *coat_normal_texture,
            *iridescence_texture,
            *iridescence_thickness_texture,
            *transmission_texture,
            *thickness_texture,
            *sheen_color_texture,
            *sheen_roughness_texture,
            *diffuse_transmission_texture,
            *diffuse_transmission_color_texture,
            *specular_texture,
            *specular_color_texture,
        ]
    }
}

impl Default for Material {
    /// glTF 2.0's default material, which the loader gives a primitive
    /// without one: unnamed, a white base, metallic and roughness 1, IOR
    /// 1.5, specular 1 untinted, no emission, clearcoat, iridescence (its
    /// film's IOR 1.3 and thickness 100 to 400 nm as glTF's defaults),
    /// sheen, diffuse transmission (its colour white, as glTF's default),
    /// anisotropy, bump, transmission, thickness, attenuation or
    /// dispersion, no textures, normal, coat normal and occlusion scales 1,
    /// no normal layers, single-sided, lit and opaque, in visibility
    /// group 0, casting directional shadows and emitting into global
    /// illumination. Set what differs and take the rest with
    /// `..Default::default()`.
    fn default() -> Self {
        Self {
            name: String::new(),
            visibility_group: 0,
            casts_directional_shadow: true,
            base: [1.0; 4],
            emissive: [0.0; 3],
            metallic: 1.0,
            roughness: 1.0,
            ior: 1.5,
            specular: 1.0,
            specular_color: [1.0; 3],
            specular_texture: None,
            specular_color_texture: None,
            clearcoat: 0.0,
            coat_roughness: 0.0,
            clearcoat_texture: None,
            coat_roughness_texture: None,
            coat_normal_texture: None,
            coat_normal_scale: 1.0,
            iridescence: 0.0,
            iridescence_ior: 1.3,
            iridescence_thickness: [100.0, 400.0],
            iridescence_texture: None,
            iridescence_thickness_texture: None,
            sheen_color: [0.0; 3],
            sheen_roughness: 0.0,
            sheen_color_texture: None,
            sheen_roughness_texture: None,
            diffuse_transmission: 0.0,
            diffuse_transmission_color: [1.0; 3],
            diffuse_transmission_texture: None,
            diffuse_transmission_color_texture: None,
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            base_texture: None,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            normal_layers: None,
            bump_texture: None,
            bump_scale: 0.0,
            transmission: 0.0,
            transmission_texture: None,
            thickness: 0.0,
            thickness_texture: None,
            attenuation_distance: f32::INFINITY,
            attenuation_color: [1.0; 3],
            dispersion: 0.0,
            double_sided: false,
            unlit: false,
            emits_into_gi: true,
            alpha: AlphaMode::Opaque,
        }
    }
}

/// CPU-side meshes, material inputs, and images ready for GPU upload.
///
/// The caller owns this data and decides how to compose it into a scene.
#[derive(Clone)]
pub struct Asset {
    /// Triangle meshes with baked transforms, batched by material when
    /// loaded; an instanced node's mesh is in [`Self::instanced`] instead.
    pub meshes: Vec<CpuMesh>,
    /// Materials addressed by [`CpuMesh::material`].
    pub materials: Vec<Material>,
    /// Images addressed by the materials' texture indices; colour-space
    /// interpretation belongs to each channel. A glTF image no sampled map
    /// uses is a placeholder ([`Ignored::Image`]).
    pub images: Vec<Image>,
    /// What poses its deforming meshes, and its animation clips; empty when
    /// nothing deforms.
    pub rig: Rig,
    /// What a glTF load left unrendered, which the file did not require
    /// (glTF lets a loader leave it out); empty for content built in code.
    pub ignored: Vec<Ignored>,
    /// Its authored lights (`KHR_lights_punctual`), which loading does not
    /// install; empty without them and for content built in code.
    pub lights: AssetLights,
    /// The meshes its `EXT_mesh_gpu_instancing` nodes instance, each once
    /// in its own space, with every placement of it, which loading does not
    /// install: `Scene::add_asset` prepares each as its own model
    /// (`AssetIds::instanced`) and the game adds an instance per transform
    /// it wants. These meshes are not in [`Self::meshes`]. Empty without
    /// the extension and for content built in code.
    pub instanced: Vec<InstancedMesh>,
}

/// Whether every vertex carries a finite, nonzero authored tangent frame with
/// handedness +1 or -1, which anisotropy requires of each mesh drawn with an
/// anisotropic material.
pub(crate) fn tangent_frames(vertices: &[Vertex]) -> bool {
    vertices.iter().all(tangent_frame)
}

/// Whether `vertex` carries a finite, nonzero authored tangent frame: a
/// tangent off its normal (`tangent_off_normal`), with handedness +1 or -1.
pub(crate) fn tangent_frame(vertex: &Vertex) -> bool {
    tangent_off_normal(
        Vec3::from_array(vertex.normal),
        Vec3::from_slice(&vertex.tangent[..3]),
    )
    .is_some()
        && matches!(vertex.tangent[3], -1.0 | 1.0)
}

/// The squared sine of the angle within which a tangent lies along its
/// normal: f32::EPSILON, so within 0.0198°. Filament's `randomPerp`
/// (libs/geometry/src/SurfaceOrientation.cpp) treats a unit vector as along
/// an axis by the same test, `dot(perp, perp) <= epsilon`, and Godot's glTF
/// importer counts a tangent as off perpendicular past `|t·n| > 0.0001`
/// (modules/gltf/gltf_document.cpp). An exact-zero test, as MikkTSpace's
/// `VNotZero` (FLT_MIN), would let a tangent a few f32 ulps (6e-8 each) off
/// its normal normalise that rounding into an arbitrary direction (#573);
/// past this threshold a few ulps turn the tangent by under about 0.1°.
/// Authored tangents (MikkTSpace makes them perpendicular to the normal)
/// sit near 90°, far from it.
const ALONG_NORMAL_SIN_SQUARED: f32 = f32::EPSILON;

/// `tangent` made unit and perpendicular to `normal`, or `None` when either
/// cannot be normalised or the tangent lies along the normal (within
/// `ALONG_NORMAL_SIN_SQUARED`), where it spans no frame. The one rule for
/// both glTF import and procedural vertices.
pub(crate) fn tangent_off_normal(normal: Vec3, tangent: Vec3) -> Option<Vec3> {
    let normal = normal.try_normalize()?;
    let tangent = tangent.try_normalize()?;
    let projected = tangent - normal * normal.dot(tangent);
    (projected.length_squared() > ALONG_NORMAL_SIN_SQUARED).then(|| projected.normalize())
}

pub(crate) fn valid_anisotropy(strength: f32, rotation: f32) -> bool {
    (0.0..=1.0).contains(&strength) && rotation.is_finite()
}

/// Create an asset with no geometry, materials, or images.
pub fn empty() -> Asset {
    Asset {
        meshes: Vec::new(),
        materials: Vec::new(),
        images: Vec::new(),
        rig: Rig::default(),
        ignored: Vec::new(),
        lights: AssetLights::default(),
        instanced: Vec::new(),
    }
}
