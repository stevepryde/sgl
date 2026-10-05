//! Populations: what a CPU-built view draws of the scene, a filter over its
//! instances and materials, and the static casters a local light's range
//! reaches. The camera's opaque and masked surfaces and the frame's
//! directional cascades are GPU-built (`draw_list::gpu`), and filter the
//! same way on the GPU (`stages::cull`).
use super::culling::clip_intersects;
use super::lod::LodSelector;
use super::pipelines::{Alpha, Cull, Variant};
use crate::content::identity::Identity;
use crate::content::material::SurfaceMaterial;
use crate::scene::instances::Instance;
use crate::scene::materials::Material;
use crate::scene::static_edits::posed_bounds;
use crate::{Mobility, Scene};
use glam::{Mat4, Vec3};

/// What a view draws of the scene, and how: a filter over the instances,
/// walked in index order, and over materials by the visibility mask and
/// alpha mode. Blended materials are the `Blended` population's alone.
pub(crate) enum Population<'a> {
    /// The main camera's blended surfaces: `visible` instances among those
    /// the scene indexes as holding a blended mesh, each blended mesh whose
    /// material's groups the mask enables, culled against the view per
    /// instance unless `cull` is false, raster-culled by material side and
    /// pose, with the selected LOD, sorted back to front by the view depth
    /// of each mesh's bounds centre, as Bevy 9d12036's `Transparent3d` phase
    /// sorts (crates/bevy_core_pipeline/src/core_3d/mod.rs).
    Blended {
        lod: Option<LodSelector>,
        cull: bool,
    },
    /// A static specular probe face: static `capture_visible` instances'
    /// meshes whose material's groups the mask enables, whole and
    /// double-sided.
    ProbeFace,
    /// A probe capture's directional shadow cascades: static
    /// `capture_visible` instances, unculled, each mesh whose material casts
    /// directional shadows in the mask's groups.
    CaptureShadow,
    /// A local-light shadow face: `capture_visible` instances of
    /// `casters`, each mesh whose material's groups the mask enables. A
    /// static instance draws its meshes' caster clusters within `range` of
    /// `position` in the view's clip volume, among the groups `reach` found
    /// within the range; a moving one that reaches the face
    /// (`moving_caster_reaches`) draws its meshes whole.
    LocalShadow {
        position: Vec3,
        range: f32,
        casters: Casters,
        reach: &'a LightReach,
    },
}

/// The static casters' cluster groups a local light's range reaches, in
/// instance, mesh and group order: found once per light, so each of its
/// faces tests only these.
#[derive(Default)]
pub(crate) struct LightReach {
    /// Instance index, mesh and group of each.
    groups: Vec<(u32, u32, u32)>,
}

/// Whether `bounds` reach within `range` of `position`.
pub(crate) fn within(bounds: [Vec3; 2], (position, range): (Vec3, f32)) -> bool {
    position
        .clamp(bounds[0], bounds[1])
        .distance_squared(position)
        <= range * range
}

impl LightReach {
    /// Finds the groups of `scene`'s static capture-visible instances
    /// within `range` of `position`.
    pub fn find(&mut self, scene: &Scene, light: (Vec3, f32)) {
        self.groups.clear();
        for (id, instance) in scene.instances.slots.iter() {
            if instance.mobility != Mobility::Static || !instance.state.capture_visible {
                continue;
            }
            for (mesh, posed) in instance.casters.iter().enumerate() {
                for (group, &bounds) in posed.groups.iter().enumerate() {
                    if within(bounds, light) {
                        self.groups
                            .push((id.index() as u32, mesh as u32, group as u32));
                    }
                }
            }
        }
    }

    /// The groups of instance `index`, as (mesh, group).
    pub(super) fn of(&self, index: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let index = index as u32;
        let start = self
            .groups
            .partition_point(|&(instance, _, _)| instance < index);
        self.groups[start..]
            .iter()
            .take_while(move |&&(instance, _, _)| instance == index)
            .map(|&(_, mesh, group)| (mesh as usize, group as usize))
    }
}

/// Which casters a local-light shadow face's draw list holds: its static
/// layer's, its moving ones', or all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Casters {
    Static,
    Moving,
    All,
}

impl Casters {
    fn holds(self, mobility: Mobility) -> bool {
        match self {
            Self::Static => mobility == Mobility::Static,
            Self::Moving => mobility == Mobility::Moving,
            Self::All => true,
        }
    }
}

/// Whether a moving instance with `bounds` in its model's space
/// (`Instance::bounds`) at `pose` casts into the local-light shadow face of
/// `view_projection` whose light at `position` reaches `range`: its world
/// bounds reach the range and the face's clip volume.
pub(crate) fn moving_caster_reaches(
    bounds: [Vec3; 2],
    pose: Mat4,
    view_projection: Mat4,
    (position, range): (Vec3, f32),
) -> bool {
    within(posed_bounds(bounds, pose), (position, range))
        && clip_intersects(bounds, view_projection * pose)
}

/// The pipeline variant the camera and every shadow kind, CPU-built or
/// GPU-built (`draw_list::gpu`), draw a mesh with `material` with at a pose
/// that is `mirrored`, of an instance that is `deformed`: Back culling of a
/// single-sided material, swapped under a mirroring pose (`Cull::of`). Lit
/// shaders do not discard back faces, so camera raster keeps hidden-surface
/// removal. Every shadow kind culls as the camera does, so a single-sided
/// material casts from its front faces: Bevy's shadow pipelines specialize
/// the material's own cull mode, and its shadow bias assumes those casters.
pub(crate) fn camera_variant(
    material: &SurfaceMaterial,
    mirrored: bool,
    deformed: bool,
) -> Variant {
    Variant {
        cull: Cull::of(Cull::Back, material.double_sided, mirrored),
        alpha: Alpha::of(material.alpha),
        deformed,
    }
}

impl Population<'_> {
    /// Whether this population shows `instance`.
    pub(super) fn shows(&self, instance: &Instance) -> bool {
        let state = &instance.state;
        match self {
            Self::Blended { .. } => state.visible,
            Self::ProbeFace | Self::CaptureShadow => {
                state.capture_visible && instance.mobility == Mobility::Static
            }
            Self::LocalShadow { casters, .. } => {
                state.capture_visible && casters.holds(instance.mobility)
            }
        }
    }

    /// The pipeline variant of a mesh with `material` at a pose that is
    /// `mirrored`, of an instance that is `deformed`: the camera's
    /// (`camera_variant`), but a probe face, which draws both sides.
    pub(super) fn variant(
        &self,
        material: &SurfaceMaterial,
        mirrored: bool,
        deformed: bool,
    ) -> Variant {
        match self {
            Self::ProbeFace => Variant {
                cull: Cull::None,
                alpha: Alpha::of(material.alpha),
                deformed,
            },
            Self::Blended { .. } | Self::CaptureShadow | Self::LocalShadow { .. } => {
                camera_variant(material, mirrored, deformed)
            }
        }
    }

    /// Whether this population draws meshes with `material` under the
    /// frame's `mask`: blended ones only `Blended`, which draws nothing
    /// else, and shadows only casters.
    pub(super) fn draws(&self, material: &Material, mask: Option<u32>) -> bool {
        if material.values.blended() != matches!(self, Self::Blended { .. }) {
            return false;
        }
        match self {
            Self::CaptureShadow => material.casts_directional_shadow(mask.unwrap_or(0)),
            _ => material.enabled(mask),
        }
    }
}
