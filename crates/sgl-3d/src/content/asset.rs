//! glTF 2.0 mesh loading for the SGL renderer.
//!
//! Rigid nodes' transforms are baked into vertices, mirrored winding is
//! corrected, and primitives are batched by material, skin and morphed node;
//! skins, morph targets, the node hierarchy and animation clips are imported
//! as plain data (`deformation`). The default scene (or first scene)
//! is loaded. Unsupported visible features return an error containing the asset
//! path instead of producing a partial model. Opaque, masked and blended alpha
//! modes are supported. Supported material extensions are
//! scalar clearcoat, anisotropy with authored tangents, emissive strength, unlit, and bump mapping.
use std::error::Error;

use glam::Vec3;
use gltf::texture::WrappingMode;

use super::deformation::{MeshDeformation, Rig};
pub use super::gltf::{
    GltfImage, ImageSource, LoadOptions, load, load_slice, load_slice_with_options,
    load_with_options,
};
pub use super::images::{CompressedFormat, CompressedImage, Image};
use super::material::{AlphaMode, NormalLayer};

/// A loader failure, including the source asset path for file loads.
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Interleaved vertex input used by the renderer, in the asset's coordinate system.
/// The scene packs each into 32 bytes when its model is prepared
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
    /// of the rectangle its mesh's UVs span.
    pub uv: [f32; 2],
    /// Linear vertex color multiplier, including alpha. Clamped to 0..=1 and
    /// kept as 8-bit sRGB with 8-bit linear alpha.
    pub color: [f32; 4],
    /// TEXCOORD_1: secondary normalized UV into the caller-baked static irradiance atlas.
    /// (0, 0) or a negative UV marks a surface without an atlas chart: it has no
    /// baked lighting, so baked scene lights light it. Kept as 16-bit fractions.
    pub lightmap_uv: [f32; 2],
    /// Normalized atlas min/max for a conservatively cropped triangle chart.
    /// Supply the same bounds on all triangle vertices; outside is black.
    /// A mesh's vertices may name at most 65,536 distinct bounds.
    pub lightmap_bounds: [f32; 4],
    /// Unit tangent XYZ and bitangent handedness W after node transforms.
    /// Zero means absent for legacy isotropic geometry; anisotropy requires a valid frame.
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
    /// Linear base color multiplier, including alpha.
    pub base: [f32; 4],
    /// Linear emissive color with emissive strength already applied.
    pub emissive: [f32; 3],
    /// Metallic factor in `0..=1`.
    pub metallic: f32,
    /// Perceptual roughness factor in `0..=1`.
    pub roughness: f32,
    /// Scalar clearcoat intensity.
    pub clearcoat: f32,
    /// Clearcoat perceptual roughness.
    pub coat_roughness: f32,
    /// KHR_materials_anisotropy strength in `0..=1`; zero preserves isotropic shading.
    pub anisotropy_strength: f32,
    /// Counter-clockwise direction rotation in tangent/bitangent space, in radians.
    pub anisotropy_rotation: f32,
    /// Linear image index: normalized remapped RG direction and B strength multiplier.
    pub anisotropy_texture: Option<usize>,
    /// Base color image index into [`Asset::images`], sampled as sRGB.
    pub base_texture: Option<usize>,
    /// Metallic (blue) / roughness (green) image index, sampled as linear data.
    pub mr_texture: Option<usize>,
    /// Emissive image index, sampled as sRGB.
    pub emissive_texture: Option<usize>,
    /// Tangent-space normal image index, sampled as linear data.
    pub normal_texture: Option<usize>,
    /// Scale applied to the normal map's tangent-space X and Y components.
    pub normal_scale: f32,
    /// The normal map drawn as two scrolling layers
    /// ([`SurfaceMaterial::normal_layers`](crate::SurfaceMaterial::normal_layers));
    /// `None` draws it once. glTF has no such extension: the loader leaves
    /// it `None`.
    pub normal_layers: Option<[NormalLayer; 2]>,
    /// Bump image index, sampled as linear height data.
    pub bump_texture: Option<usize>,
    /// Bump height multiplier.
    pub bump_scale: f32,
    /// U and V wrapping shared by all texture channels of this material.
    pub wrap: [WrappingMode; 2],
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

impl Default for Material {
    /// glTF 2.0's default material, which the loader gives a primitive
    /// without one: unnamed, a white base, metallic and roughness 1, no
    /// emission, clearcoat, anisotropy, bump or textures, normal scale 1, no
    /// normal layers, repeating, single-sided, lit and opaque, in visibility
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
            clearcoat: 0.0,
            coat_roughness: 0.0,
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            base_texture: None,
            mr_texture: None,
            emissive_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            normal_layers: None,
            bump_texture: None,
            bump_scale: 0.0,
            wrap: [WrappingMode::Repeat; 2],
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
    /// Triangle meshes with baked transforms, batched by material when loaded.
    pub meshes: Vec<CpuMesh>,
    /// Materials addressed by [`CpuMesh::material`].
    pub materials: Vec<Material>,
    /// Images addressed by the materials' texture indices; colour-space
    /// interpretation belongs to each channel.
    pub images: Vec<Image>,
    /// What poses its deforming meshes, and its animation clips; empty when
    /// nothing deforms.
    pub rig: Rig,
}

/// Whether every vertex carries a finite, nonzero authored tangent frame with
/// handedness +1 or -1, which anisotropy requires of each mesh drawn with an
/// anisotropic material.
pub(crate) fn tangent_frames(vertices: &[Vertex]) -> bool {
    vertices.iter().all(tangent_frame)
}

/// Whether `vertex` carries a finite, nonzero authored tangent frame: a
/// tangent that keeps a length off its normal, with handedness +1 or -1.
pub(crate) fn tangent_frame(vertex: &Vertex) -> bool {
    let n = Vec3::from_array(vertex.normal);
    let t = Vec3::from_slice(&vertex.tangent[..3]);
    n.try_normalize().is_some()
        && t.try_normalize().is_some()
        && n.cross(t).try_normalize().is_some()
        && matches!(vertex.tangent[3], -1.0 | 1.0)
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
    }
}
