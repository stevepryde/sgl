//! What a draw batch's material and instance require of the pipeline that
//! draws it: the faces it culls, the alpha mode, deformed vertices and maps
//! sampled in the shader.
use crate::content::material::{AlphaMode, SurfaceMaterial};

/// Hardware face culling, which each draw batch chooses from its
/// population, material side and pose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Cull {
    None,
    Back,
    Front,
}

impl Cull {
    /// The same faces of a mesh under a mirroring pose, which reverses its
    /// winding.
    pub fn mirrored(self) -> Self {
        match self {
            Self::None => Self::None,
            Self::Back => Self::Front,
            Self::Front => Self::Back,
        }
    }

    /// The faces a view culls of a mesh whose material is `double_sided`
    /// at a pose that is `mirrored`, where it culls `single_sided` of a
    /// single-sided material: none of a double-sided one, and the other
    /// side under a mirroring pose, which reverses winding. Pipelines keep
    /// CCW front faces because object_front_face and normal mapping
    /// account for mirroring.
    pub fn of(single_sided: Self, double_sided: bool, mirrored: bool) -> Self {
        if double_sided {
            Self::None
        } else if mirrored {
            single_sided.mirrored()
        } else {
            single_sided
        }
    }

    pub(super) fn face(self) -> Option<wgpu::Face> {
        match self {
            Self::None => None,
            Self::Back => Some(wgpu::Face::Back),
            Self::Front => Some(wgpu::Face::Front),
        }
    }
}

/// What a material's alpha mode, or its transmission, requires of the
/// pipelines that draw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Alpha {
    Opaque,
    /// Fragments discard the texels the material cuts out (`alpha_mask`),
    /// in every opaque pass and caster.
    Mask,
    /// A blended or transmissive material (`SurfaceMaterial::blended`),
    /// which `GeometryPass::Blended` draws, and an outline mask draws
    /// depth-only through `GeometryPass::LocalShadow`'s casters, whole.
    Blend,
}

impl Alpha {
    pub fn of(material: &SurfaceMaterial) -> Self {
        if material.blended() {
            return Self::Blend;
        }
        match material.alpha {
            AlphaMode::Opaque => Self::Opaque,
            AlphaMode::Mask { .. } => Self::Mask,
            AlphaMode::Blend { .. } => Self::Blend,
        }
    }
}

/// What a draw batch's material and instance require of its pipeline: the
/// faces it culls, from the material's side and the pose, the material's
/// alpha mode, whether the instance deforms, whose pulled vertices are its
/// deformed ones (`deformed_vertices` in vertex_pull.wgsl), and whether the
/// material samples a map in the shader, in a state other than the one its
/// group 2 sampler binds: a bit, by its map binding's index, for each such
/// binding (`material_shader_sampled_maps` in material_sampling.wgsl;
/// `scene::materials::Material::shader_sampled`), none for most materials;
/// and likewise a bit for each map binding that samples the second UV set
/// (`uv1`) and for each that samples through a transform (`transformed`)
/// (`scene::materials::Material::map_uvs`), none for a material on its
/// first UV set untransformed, which a pipeline's key reduces to whether a
/// map its programs sample does (`material_uv1_enabled` and
/// `material_transforms_enabled` in map_uv.wgsl, `PipelineKey::new`).
/// Casters read whichever positions they are given and ignore `deformed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Variant {
    pub cull: Cull,
    pub alpha: Alpha,
    pub deformed: bool,
    pub shader_sampled: u16,
    pub uv1: u16,
    pub transformed: u16,
}

impl Variant {
    /// How many variants there are whose maps all sample the first UV set
    /// untransformed through group 2's sampler.
    pub const COUNT: usize = 18;

    /// Its index among those variants, for a draw's lookup table; none
    /// where a map of it samples in the shader, the second UV set or
    /// through a transform.
    pub fn index(self) -> Option<usize> {
        (self.shader_sampled == 0 && self.uv1 == 0 && self.transformed == 0).then(|| {
            (usize::from(self.deformed) * 3 + self.cull as usize) * 3 + self.alpha as usize
        })
    }
}
