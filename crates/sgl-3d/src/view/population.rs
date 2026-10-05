//! Populations: what a view draws of the scene, a filter over its
//! instances and materials, and the static casters a local light's range
//! reaches.
use super::culling::clip_intersects;
use super::hidden::HiddenInstances;
use super::lod::LodSelector;
use super::pipelines::{Alpha, Cull, Variant};
use crate::content::identity::{Identity, InstanceId};
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
    /// The main camera's opaque and masked surfaces: `visible` instances,
    /// each mesh whose material's groups the mask enables, culled against
    /// the view per instance unless `cull` is false, raster-culled by
    /// material side and pose, with the selected LOD; without the instances
    /// in `hidden`, the diagnostics oracle's (`InstanceVisibility`).
    Camera {
        lod: Option<LodSelector>,
        cull: bool,
        hidden: Option<&'a HiddenInstances>,
    },
    /// The main camera's blended surfaces, as `Camera` selects them, sorted
    /// back to front by the view depth of each mesh's bounds centre, as
    /// Bevy 9d12036's `Transparent3d` phase sorts
    /// (crates/bevy_core_pipeline/src/core_3d/mod.rs).
    Blended {
        lod: Option<LodSelector>,
        cull: bool,
    },
    /// A static specular probe face: static `capture_visible` instances'
    /// meshes whose material's groups the mask enables, whole and
    /// double-sided.
    ProbeFace,
    /// A directional shadow cascade: `capture_visible` instances, static
    /// ones and, with `moving`, moving ones, each mesh whose material casts
    /// directional shadows in the mask's groups, culled per instance against
    /// the view without its near plane with `cull`, since a caster between
    /// the light and the cascade casts into it.
    DirectionalShadow { cull: bool, moving: bool },
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

impl Population<'_> {
    /// Whether this population shows `instance`, identified as `id`.
    pub(super) fn shows(&self, id: InstanceId, instance: &Instance) -> bool {
        let state = &instance.state;
        match self {
            Self::Camera { hidden, .. } => {
                state.visible && !hidden.is_some_and(|hidden| hidden.holds(id))
            }
            Self::Blended { .. } => state.visible,
            Self::ProbeFace => state.capture_visible && instance.mobility == Mobility::Static,
            Self::DirectionalShadow { moving, .. } => {
                state.capture_visible && (*moving || instance.mobility == Mobility::Static)
            }
            Self::LocalShadow { casters, .. } => {
                state.capture_visible && casters.holds(instance.mobility)
            }
        }
    }

    /// The faces this population's raster culls of a mesh with `material`,
    /// `mirrored` when its pose reverses winding: none of a double-sided
    /// material; of a single-sided one, the side the population never
    /// draws, swapped under a mirroring pose. Pipelines keep CCW front faces
    /// because object_front_face and normal mapping account for mirroring.
    fn cull(&self, material: &SurfaceMaterial, mirrored: bool) -> Cull {
        let single_sided = match self {
            // Lit shaders do not discard back faces, so camera raster keeps
            // hidden-surface removal. Every shadow kind culls as the camera
            // does, so a single-sided material casts from its front faces:
            // Bevy's shadow pipelines specialize the material's own cull
            // mode, and its shadow bias assumes those casters.
            Self::Camera { .. }
            | Self::Blended { .. }
            | Self::DirectionalShadow { .. }
            | Self::LocalShadow { .. } => Cull::Back,
            Self::ProbeFace => Cull::None,
        };
        if material.double_sided {
            Cull::None
        } else if mirrored {
            single_sided.mirrored()
        } else {
            single_sided
        }
    }

    /// The pipeline variant of a mesh with `material` at a pose that is
    /// `mirrored`, of an instance that is `deformed`.
    pub(super) fn variant(
        &self,
        material: &SurfaceMaterial,
        mirrored: bool,
        deformed: bool,
    ) -> Variant {
        Variant {
            cull: self.cull(material, mirrored),
            alpha: Alpha::of(material.alpha),
            deformed,
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
            Self::DirectionalShadow { .. } => material.casts_directional_shadow(mask.unwrap_or(0)),
            _ => material.enabled(mask),
        }
    }
}
