//! What a draw batch's material and instance require of the pipeline that
//! draws it: the faces it culls, the alpha mode and deformed vertices.
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
    /// Only `GeometryPass::Blended` draws it: a blended or transmissive
    /// material (`SurfaceMaterial::blended`).
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
/// alpha mode, and whether the instance deforms, whose pulled vertices are
/// its deformed ones (`deformed_vertices` in vertex_pull.wgsl). Casters read
/// whichever positions they are given and ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Variant {
    pub cull: Cull,
    pub alpha: Alpha,
    pub deformed: bool,
}

impl Variant {
    /// How many variants there are.
    pub const COUNT: usize = 18;

    /// Its index among the variants, for a draw's lookup table.
    pub fn index(self) -> usize {
        (usize::from(self.deformed) * 3 + self.cull as usize) * 3 + self.alpha as usize
    }
}
