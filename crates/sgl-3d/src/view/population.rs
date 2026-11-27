//! Populations: what a CPU-built view draws of the scene, a filter over its
//! instances (or a selection of them) and materials, and the static casters
//! a local light's range reaches. The camera's opaque and masked surfaces and the frame's
//! directional cascades are GPU-built (`draw_list::gpu`), and filter the
//! same way on the GPU (`stages::cull`).
use super::culling::clip_intersects;
use super::lod::LodSelector;
use super::pipelines::{Alpha, Cull, Variant};
use crate::content::identity::Identity;
use crate::scene::instances::Instance;
use crate::scene::materials::Material;
use crate::scene::static_edits::{posed_bounds, posed_grown};
use crate::{Mobility, Scene};
use glam::{Mat4, Vec3};

/// What a view draws of the scene, and how: a filter over the instances,
/// walked in index order (or a selection's instances, in its order), and
/// over materials by the visibility mask and alpha mode. Blended materials
/// are the `Blended` population's and an outline mask's alone.
pub(crate) enum Population<'a> {
    /// The main camera's blended surfaces: `visible` instances among those
    /// the scene indexes as holding a blended mesh, each blended mesh whose
    /// material's groups the mask enables, culled against the view per
    /// instance unless `cull` is false, raster-culled by material side and
    /// pose, with the selected LOD, sorted back to front by the view depth
    /// of each mesh's bounds centre, as Bevy 9d12036's `Transparent3d` phase
    /// sorts (crates/bevy_core_pipeline/src/core_3d/mod.rs).
    /// Two draws on either side of one of the `barriers`, the view depths
    /// of the blended sprite batches (`view::sprites`), are never merged,
    /// so each batch is drawn between them.
    Blended {
        lod: Option<LodSelector>,
        cull: bool,
        barriers: &'a [f32],
    },
    /// A specular probe face: the `capture_visible` instances a capture
    /// shows (`content`: a bake's static ones, a live refresh's moving ones
    /// too), each mesh whose material's groups the mask enables,
    /// double-sided: whole, or, when `culled`, the parts the face's view
    /// reaches, as the camera's blended surfaces are culled.
    ProbeFace { content: Casters, culled: bool },
    /// A probe capture's directional shadow cascades: the `capture_visible`
    /// instances the capture shows, unculled, each mesh whose material
    /// casts directional shadows in the mask's groups.
    CaptureShadow(Casters),
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
    /// A view's outline mask (`stages::outline`): the `visible` instances
    /// among `instances` alone, walked in their order, each mesh whose
    /// material's groups the mask enables, whatever its alpha mode (a
    /// blended one's triangles whole, a masked one's cut-out texels
    /// discarded), culled against the view per instance and drawn at the
    /// selected LOD, as the camera's blended surfaces are.
    Selection {
        instances: &'a [crate::InstanceId],
        lod: Option<LodSelector>,
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
            let model = scene.drawn_model(instance.state.model);
            for (mesh, posed) in instance.casters.iter().enumerate() {
                // Grown by what the mesh's material's shader may move it.
                let grow = scene
                    .drawn_material(instance.mesh_material(mesh, model.meshes[mesh].material))
                    .displacement_bound();
                for (group, &bounds) in posed.groups.iter().enumerate() {
                    if within(posed_grown(bounds, instance.state.pose, grow), light) {
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

/// Which instances a local-light shadow face's draw list or a probe
/// capture's holds, by whether they move: a face's static layer's or a
/// bake's, a face's moving ones, or all of them, as a live refresh shows.
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
/// A transmissive volume draws its front faces alone, whatever its side, as
/// KHR_materials_volume says `doubleSided` does not apply to a volume
/// boundary (Khronos glTF acfcbe65, README 112).
pub(crate) fn camera_variant(material: &Material, mirrored: bool, deformed: bool) -> Variant {
    Variant {
        cull: Cull::of(Cull::Back, material.values.two_sided(), mirrored),
        alpha: Alpha::of(&material.values),
        deformed,
        shader_sampled: material.shader_sampled(),
        uv1: material.map_uvs().0,
        transformed: material.map_uvs().1,
        occlusion_specular: material.occlusion_specular(),
    }
}

impl Population<'_> {
    /// Whether this population shows `instance`.
    pub(super) fn shows(&self, instance: &Instance) -> bool {
        let state = &instance.state;
        match self {
            Self::Blended { .. } | Self::Selection { .. } => state.visible,
            Self::ProbeFace {
                content: casters, ..
            }
            | Self::CaptureShadow(casters)
            | Self::LocalShadow { casters, .. } => {
                state.capture_visible && casters.holds(instance.mobility)
            }
        }
    }

    /// The pipeline variant of a mesh with `material` at a pose that is
    /// `mirrored`, of an instance that is `deformed`: the camera's
    /// (`camera_variant`), but a probe face, which draws both sides.
    pub(super) fn variant(&self, material: &Material, mirrored: bool, deformed: bool) -> Variant {
        match self {
            Self::ProbeFace { .. } => Variant {
                cull: Cull::None,
                ..camera_variant(material, mirrored, deformed)
            },
            Self::Blended { .. } | Self::CaptureShadow(_) | Self::LocalShadow { .. } => {
                camera_variant(material, mirrored, deformed)
            }
            // Depth-only through the casters, which discard a masked
            // material's cut-out texels whatever else it is: a masked
            // transmissive one included, which the camera draws blended
            // and cut.
            Self::Selection { .. } => Variant {
                alpha: if matches!(material.values.alpha, crate::AlphaMode::Mask { .. }) {
                    Alpha::Mask
                } else {
                    Alpha::of(&material.values)
                },
                ..camera_variant(material, mirrored, deformed)
            },
        }
    }

    /// Whether this population draws meshes with `material` under the
    /// frame's `mask`: blended ones only `Blended`, which draws nothing
    /// else, and `Selection`, which draws every alpha mode, and shadows only
    /// casters.
    pub(super) fn draws(&self, material: &Material, mask: Option<u32>) -> bool {
        match self {
            Self::LocalShadow { .. } => return material.casts_local_shadow(mask),
            Self::Selection { .. } => return material.enabled(mask),
            _ => {}
        }
        if material.values.blended() != matches!(self, Self::Blended { .. }) {
            return false;
        }
        match self {
            Self::CaptureShadow(_) => material.casts_directional_shadow(mask.unwrap_or(0)),
            _ => material.enabled(mask),
        }
    }
}
