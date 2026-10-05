//! BLASes (the architecture's Hardware ray tracing, *Structures*): one for
//! each model of `RayClass::Opaque` that does not deform, one geometry per
//! mesh over the packed vertices' positions and the mesh's indices where
//! they lie in the ray source, as Bevy b56fc29 builds one per mesh from its
//! mesh allocator's slices (`crates/bevy_solari/src/scene/blas.rs` 55–102,
//! 144–171) and Wicked Engine one per mesh LOD
//! (`wiScene_Components.cpp`, `CreateRaytracingRenderData`); and one for each
//! deforming instance over its deformed positions, built whole again after
//! each deform. A model's BLAS is pending until a frame that builds the
//! structures, which builds the pending ones nearest the camera first under
//! Bevy's budget of vertices (`MOST_VERTICES_PER_FRAME`), the first always,
//! and a replaced model's outside it; a built one is then compacted through
//! Bevy's queue under the same budget (`blas.rs` 104–142).
use super::{allocated, distance};
use crate::content::identity::{Identity, InstanceId, ModelId};
use crate::scene::instances::{Instance, Instances};
use crate::scene::models::{Model, Models};
use crate::scene::ray_class::RayClass;
use crate::scene::rays::{RayMeshWords, VERTEX_WORDS};
use crate::scene::static_edits::posed_bounds;
use crate::shading::deformation::DEFORMED_POSITION_WORDS;
use glam::Vec3;
use std::collections::{HashMap, VecDeque};

/// Bevy's `MAX_COMPACTION_VERTICES_PER_FRAME` (`blas.rs` 19–21), which
/// serves the builds too: once a frame has built, or compacted, this many
/// vertices' worth of models, it builds, or compacts, no further one, so
/// the first is always taken whatever its size. A game that turns hardware
/// ray tracing on, or installs a world, ramps its structures in over a few
/// frames, an RD-2 improvement on Bevy, which builds every mesh in the frame
/// it arrives.
pub(super) const MOST_VERTICES_PER_FRAME: u32 = 400_000;

/// A model's BLAS, for its geometry `geometry` (`Model::geometry`).
enum ModelBlas {
    Built {
        blas: wgpu::Blas,
        geometry: u64,
        #[cfg(any(test, feature = "diagnostics"))]
        triangles: u64,
    },
    /// The device could not hold it: its limits or its memory.
    LeftOut { geometry: u64 },
}

impl ModelBlas {
    fn geometry(&self) -> u64 {
        match self {
            Self::Built { geometry, .. } | Self::LeftOut { geometry } => *geometry,
        }
    }
}

/// A deforming instance's BLAS, for its model's geometry `geometry`, as of
/// its deformation `revision` (`InstanceDeformation::revision`).
enum DeformedBlas {
    Built {
        blas: wgpu::Blas,
        geometry: u64,
        revision: u64,
        #[cfg(any(test, feature = "diagnostics"))]
        triangles: u64,
    },
    LeftOut {
        geometry: u64,
    },
}

/// A built model BLAS waiting to be compacted (Bevy's queue entry).
struct Compaction {
    model: ModelId,
    geometry: u64,
    vertices: u32,
    started: bool,
}

/// What a frame's BLAS build makes.
enum Built {
    Model {
        id: ModelId,
        geometry: u64,
        vertices: u32,
    },
    Deformed {
        id: InstanceId,
        geometry: u64,
        revision: u64,
    },
}

/// Where one geometry's vertices and indices start in the ray source: its
/// first vertex in whole strides and its first index.
struct GeometryStart {
    first_vertex: u32,
    first_index: u32,
}

/// One BLAS a frame builds: its geometries' sizes and starts, read with
/// `stride` bytes between vertices.
pub(super) struct BlasBuild {
    built: Built,
    blas: wgpu::Blas,
    sizes: Vec<wgpu::BlasTriangleGeometrySizeDescriptor>,
    starts: Vec<GeometryStart>,
    stride: u64,
}

impl BlasBuild {
    /// Its build entry over `source`, the ray source.
    pub fn entry<'a>(&'a self, source: &'a wgpu::Buffer) -> wgpu::BlasBuildEntry<'a> {
        wgpu::BlasBuildEntry {
            blas: &self.blas,
            geometry: wgpu::BlasGeometries::TriangleGeometries(
                self.sizes
                    .iter()
                    .zip(&self.starts)
                    .map(|(size, start)| wgpu::BlasTriangleGeometry {
                        size,
                        vertex_buffer: source,
                        first_vertex: start.first_vertex,
                        vertex_stride: self.stride,
                        index_buffer: Some(source),
                        first_index: Some(start.first_index),
                        transform_buffer: None,
                        transform_buffer_offset: None,
                    })
                    .collect(),
            ),
        }
    }

    /// Counts the build (`counters`).
    pub fn count(&self) {
        match self.built {
            Built::Model { vertices, .. } => crate::counters::blas_build(vertices),
            Built::Deformed { .. } => crate::counters::deformed_blas_build(),
        }
    }
}

/// A model whose BLAS is pending: its distance from the camera (its
/// instances' nearest), its vertices, and whether it was replaced, so that
/// it has a BLAS of its old geometry.
#[derive(Clone, Copy)]
struct Pending {
    id: ModelId,
    nearest: f32,
    vertices: u32,
    replaced: bool,
}

/// The pending models a frame builds: every replaced one, outside the
/// budget, so an edited chunk never leaves the TLAS; then the others nearest
/// the camera first while the frame has built fewer than
/// `MOST_VERTICES_PER_FRAME` vertices of them, so the first is always taken.
/// Equally near models go by index, so every run chooses alike.
fn admit(mut pending: Vec<Pending>) -> Vec<ModelId> {
    pending.sort_by(|a, b| {
        b.replaced
            .cmp(&a.replaced)
            .then(a.nearest.total_cmp(&b.nearest))
            .then(a.id.index().cmp(&b.id.index()))
    });
    let mut vertices = 0u32;
    let mut admitted = Vec::new();
    for pending in pending {
        if !pending.replaced {
            if vertices >= MOST_VERTICES_PER_FRAME {
                break;
            }
            vertices = vertices.saturating_add(pending.vertices);
        }
        admitted.push(pending.id);
    }
    admitted
}

/// Whether the device holds a BLAS over `meshes`: no more geometries than
/// `max_blas_geometry_count`, no more triangles than
/// `max_blas_primitive_count`, and, as wgpu 29 also checks each geometry's
/// vertex count against that limit (`wgpu-core` `device/ray_tracing.rs`
/// 93–98), no mesh of more vertices than it.
fn fits(meshes: &[RayMeshWords], limits: &wgpu::Limits) -> bool {
    let triangles: u64 = meshes
        .iter()
        .map(|mesh| u64::from(mesh.index_count / 3))
        .sum();
    meshes.len() as u64 <= u64::from(limits.max_blas_geometry_count)
        && triangles <= u64::from(limits.max_blas_primitive_count)
        && meshes
            .iter()
            .all(|mesh| mesh.vertex_count <= limits.max_blas_primitive_count)
}

/// Each geometry's sizes over `meshes`: `Float32x3` positions, which need no
/// extended vertex format, and `Uint32` indices, every geometry `OPAQUE`
/// (the baseline form). A mesh's indices count its whole triangles only, as
/// its BVH and culling ranges do: wgpu refuses a count that is not a
/// multiple of three (`wgpu-core` `command/ray_tracing.rs` 755–760).
fn sizes(meshes: &[RayMeshWords]) -> Vec<wgpu::BlasTriangleGeometrySizeDescriptor> {
    meshes
        .iter()
        .map(|mesh| wgpu::BlasTriangleGeometrySizeDescriptor {
            vertex_format: wgpu::VertexFormat::Float32x3,
            vertex_count: mesh.vertex_count,
            index_format: Some(wgpu::IndexFormat::Uint32),
            index_count: Some(mesh.index_count / 3 * 3),
            flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
        })
        .collect()
}

/// A BLAS for `sizes` with `flags`, unless the device's memory cannot hold
/// it.
fn create(
    device: &wgpu::Device,
    label: &str,
    sizes: &[wgpu::BlasTriangleGeometrySizeDescriptor],
    flags: wgpu::AccelerationStructureFlags,
) -> Option<wgpu::Blas> {
    allocated(device, || {
        device.create_blas(
            &wgpu::CreateBlasDescriptor {
                label: Some(label),
                flags,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            },
            wgpu::BlasGeometrySizeDescriptors::Triangles {
                descriptors: sizes.to_vec(),
            },
        )
    })
}

/// The build of `model`'s BLAS (`id`), unless the device cannot hold it.
/// Bevy's flags (`blas.rs` 158–164): a game changes a rigid model's
/// geometry by replacing it, never by refitting it.
fn model_build(device: &wgpu::Device, id: ModelId, model: &Model) -> Option<BlasBuild> {
    let sizes = sizes(&model.ray_meshes);
    let blas = create(
        device,
        "scene model BLAS",
        &sizes,
        wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE
            | wgpu::AccelerationStructureFlags::ALLOW_COMPACTION,
    )?;
    let starts = model
        .ray_meshes
        .iter()
        .map(|mesh| {
            debug_assert_eq!(mesh.vertices % VERTEX_WORDS, 0, "a vertex block is aligned");
            GeometryStart {
                first_vertex: mesh.vertices / VERTEX_WORDS,
                first_index: mesh.indices,
            }
        })
        .collect();
    Some(BlasBuild {
        built: Built::Model {
            id,
            geometry: model.geometry,
            vertices: model.ray_meshes.iter().map(|mesh| mesh.vertex_count).sum(),
        },
        blas,
        sizes,
        starts,
        stride: u64::from(VERTEX_WORDS) * 4,
    })
}

/// The BLASes of a scene's models and deforming instances.
#[derive(Default)]
pub(super) struct Blases {
    models: HashMap<ModelId, ModelBlas>,
    deformed: HashMap<InstanceId, DeformedBlas>,
    compaction: VecDeque<Compaction>,
    /// The model BLASes the frame being prepared builds.
    building: HashMap<ModelId, wgpu::Blas>,
}

impl Blases {
    /// Drops the BLASes of removed models and instances, of models no
    /// longer `RayClass::Opaque` and of instances whose model rays pass
    /// through.
    pub fn forget_removed(&mut self, models: &Models, instances: &Instances) {
        self.models.retain(|&id, _| {
            models.slots.get(id).is_some_and(|model| {
                model.ray_class == RayClass::Opaque && model.deformation.is_none()
            })
        });
        self.deformed.retain(|&id, _| {
            instances.slots.get(id).is_some_and(|instance| {
                instance.deformation.is_some()
                    && models
                        .slots
                        .get(instance.state.model)
                        .is_some_and(|model| model.ray_class != RayClass::None)
            })
        });
        self.building.clear();
    }

    /// Bevy's compaction (`blas.rs` 104–142): each built model BLAS in the
    /// queue is prepared for compaction once, and compacted through `queue`
    /// once ready, until the frame has compacted the budget's vertices or
    /// looked at every queued BLAS once; the TLAS takes a compacted BLAS at
    /// its next build. A BLAS the device's memory cannot hold compacted stays
    /// as it was built.
    pub fn compact(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let queued = self.compaction.len();
        let mut looked = 0;
        let mut vertices = 0;
        while !self.compaction.is_empty() && vertices < MOST_VERTICES_PER_FRAME && looked < queued {
            looked += 1;
            let mut next = self.compaction.pop_front().expect("a queued BLAS");
            let Some(ModelBlas::Built { blas, geometry, .. }) = self.models.get_mut(&next.model)
            else {
                continue;
            };
            if *geometry != next.geometry {
                continue;
            }
            if !next.started {
                blas.prepare_compaction_async(|_| {});
            }
            if blas.ready_for_compaction() {
                if let Some(compacted) = allocated(device, || queue.compact_blas(blas)) {
                    *blas = compacted;
                    crate::counters::blas_compaction(next.vertices);
                    vertices += next.vertices;
                }
                continue;
            }
            next.started = true;
            self.compaction.push_back(next);
        }
    }

    /// The model BLASes the frame builds, of the models of the instances
    /// `opaque` whose BLAS is pending (`admit`). A model the device cannot
    /// hold is left out.
    pub fn choose_models(
        &mut self,
        device: &wgpu::Device,
        limits: &wgpu::Limits,
        models: &Models,
        instances: &Instances,
        opaque: &[InstanceId],
        eye: Vec3,
    ) -> Vec<BlasBuild> {
        let mut pending: HashMap<ModelId, Pending> = HashMap::new();
        for &id in opaque {
            let instance = instances.slots.get(id).expect("a live instance");
            let model_id = instance.state.model;
            let model = models
                .slots
                .get(model_id)
                .expect("an instance's model lives");
            let built = self.models.get(&model_id);
            if built.is_some_and(|blas| blas.geometry() == model.geometry) {
                continue;
            }
            let nearest = distance(eye, posed_bounds(model.bounds, instance.state.pose));
            pending
                .entry(model_id)
                .and_modify(|pending| pending.nearest = pending.nearest.min(nearest))
                .or_insert(Pending {
                    id: model_id,
                    nearest,
                    vertices: model.ray_meshes.iter().map(|mesh| mesh.vertex_count).sum(),
                    // A model left out goes back under the budget.
                    replaced: matches!(built, Some(ModelBlas::Built { .. })),
                });
        }
        let mut builds = Vec::new();
        let mut held = Vec::with_capacity(pending.len());
        for pending in pending.into_values() {
            let model = models.slots.get(pending.id).expect("a pending model lives");
            if fits(&model.ray_meshes, limits) {
                held.push(pending);
            } else {
                self.leave_out(pending.id, model);
            }
        }
        for id in admit(held) {
            let model = models.slots.get(id).expect("a pending model lives");
            match model_build(device, id, model) {
                Some(build) => {
                    self.building.insert(id, build.blas.clone());
                    builds.push(build);
                }
                None => self.leave_out(id, model),
            }
        }
        builds
    }

    /// Leaves model `id`'s current geometry out of the TLAS.
    fn leave_out(&mut self, id: ModelId, model: &Model) {
        self.models.insert(
            id,
            ModelBlas::LeftOut {
                geometry: model.geometry,
            },
        );
    }

    /// The BLAS of model `id`'s current geometry, built or building this
    /// frame.
    pub fn model(&self, models: &Models, id: ModelId) -> Option<&wgpu::Blas> {
        if let Some(blas) = self.building.get(&id) {
            return Some(blas);
        }
        let geometry = models.slots.get(id).expect("a live model").geometry;
        match self.models.get(&id) {
            Some(ModelBlas::Built {
                blas, geometry: g, ..
            }) if *g == geometry => Some(blas),
            _ => None,
        }
    }

    /// Whether the device could not hold model `id`'s current geometry.
    pub fn left_out(&self, models: &Models, id: ModelId) -> bool {
        let geometry = models.slots.get(id).expect("a live model").geometry;
        matches!(self.models.get(&id), Some(ModelBlas::LeftOut { geometry: g }) if *g == geometry)
    }

    /// The BLAS of deforming instance `id` as the frame deforms it, adding
    /// its build to `builds` when its deformation changed since it was
    /// built: whole, with `PREFER_FAST_BUILD`, outside the budget, as Wicked
    /// Engine rebuilds its skinned meshes' BLASes every frame
    /// (`wiScene.cpp` 4246–4252). None when the device cannot hold it.
    pub fn deformed(
        &mut self,
        device: &wgpu::Device,
        limits: &wgpu::Limits,
        models: &Models,
        (id, instance): (InstanceId, &Instance),
        builds: &mut Vec<BlasBuild>,
    ) -> Option<wgpu::Blas> {
        let model = models
            .slots
            .get(instance.state.model)
            .expect("an instance's model lives");
        let deformation = instance.deformation.as_ref().expect("a deforming instance");
        let blas = match self.deformed.get(&id) {
            Some(DeformedBlas::LeftOut { geometry }) if *geometry == model.geometry => {
                return None;
            }
            Some(DeformedBlas::Built {
                blas,
                geometry,
                revision,
                ..
            }) if *geometry == model.geometry => {
                if *revision == deformation.revision {
                    return Some(blas.clone());
                }
                blas.clone()
            }
            _ => {
                let sizes = sizes(&model.ray_meshes);
                let created = fits(&model.ray_meshes, limits)
                    .then(|| {
                        create(
                            device,
                            "scene deformed BLAS",
                            &sizes,
                            wgpu::AccelerationStructureFlags::PREFER_FAST_BUILD,
                        )
                    })
                    .flatten();
                let Some(blas) = created else {
                    self.deformed.insert(
                        id,
                        DeformedBlas::LeftOut {
                            geometry: model.geometry,
                        },
                    );
                    return None;
                };
                blas
            }
        };
        let rig = model.deformation.as_ref().expect("a deforming model");
        let starts = model
            .ray_meshes
            .iter()
            .enumerate()
            .map(|(mesh, words)| {
                let positions = deformation.positions(rig, mesh);
                debug_assert_eq!(
                    positions % DEFORMED_POSITION_WORDS,
                    0,
                    "positions are aligned"
                );
                GeometryStart {
                    first_vertex: positions / DEFORMED_POSITION_WORDS,
                    first_index: words.indices,
                }
            })
            .collect();
        builds.push(BlasBuild {
            built: Built::Deformed {
                id,
                geometry: model.geometry,
                revision: deformation.revision,
            },
            blas: blas.clone(),
            sizes: sizes(&model.ray_meshes),
            starts,
            stride: u64::from(DEFORMED_POSITION_WORDS) * 4,
        });
        Some(blas)
    }

    /// Commits a submitted frame's `builds`: each model's BLAS is built and
    /// queued for compaction, each deforming instance's built as of its
    /// deformation.
    pub fn commit(&mut self, builds: Vec<BlasBuild>) {
        for build in builds {
            #[cfg(any(test, feature = "diagnostics"))]
            let triangles = build
                .sizes
                .iter()
                .map(|size| u64::from(size.index_count.unwrap_or(0) / 3))
                .sum();
            match build.built {
                Built::Model {
                    id,
                    geometry,
                    vertices,
                } => {
                    self.models.insert(
                        id,
                        ModelBlas::Built {
                            blas: build.blas,
                            geometry,
                            #[cfg(any(test, feature = "diagnostics"))]
                            triangles,
                        },
                    );
                    self.compaction.push_back(Compaction {
                        model: id,
                        geometry,
                        vertices,
                        started: false,
                    });
                }
                Built::Deformed {
                    id,
                    geometry,
                    revision,
                } => {
                    self.deformed.insert(
                        id,
                        DeformedBlas::Built {
                            blas: build.blas,
                            geometry,
                            revision,
                            #[cfg(any(test, feature = "diagnostics"))]
                            triangles,
                        },
                    );
                }
            }
        }
        self.building.clear();
    }

    /// The BLASes it holds and their triangles.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn held(&self) -> (u64, u64) {
        let models = self.models.values().filter_map(|blas| match blas {
            ModelBlas::Built { triangles, .. } => Some(*triangles),
            ModelBlas::LeftOut { .. } => None,
        });
        let deformed = self.deformed.values().filter_map(|blas| match blas {
            DeformedBlas::Built { triangles, .. } => Some(*triangles),
            DeformedBlas::LeftOut { .. } => None,
        });
        models
            .chain(deformed)
            .fold((0, 0), |(count, sum), triangles| {
                (count + 1, sum + triangles)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{MOST_VERTICES_PER_FRAME, Pending, admit, sizes};
    use crate::content::identity::{Identity, ModelId};
    use crate::scene::rays::RayMeshWords;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defect: a mesh's raw index count given to its BLAS when it
    // is not a multiple of three, as a mesh the scene accepts may have (its
    // BVH and culling ranges drop the indices past its last whole
    // triangle): wgpu then refuses the frame's build (`InvalidIndexCount`),
    // an uncaptured error that panics the game. The oracle is wgpu's rule,
    // each geometry's count a multiple of three, holding each mesh's whole
    // triangles.
    #[wasm_bindgen_test(unsupported = test)]
    fn blas_geometries_hold_whole_triangles() {
        let mesh = |index_count| RayMeshWords {
            vertices: 0,
            vertex_count: 4,
            indices: 0,
            index_count,
        };
        let counts: Vec<_> = sizes(&[mesh(7), mesh(6), mesh(2), mesh(0)])
            .iter()
            .map(|size| size.index_count)
            .collect();
        assert_eq!(counts, [Some(6), Some(6), Some(0), Some(0)]);
    }

    fn pending(index: usize, nearest: f32, vertices: u32, replaced: bool) -> Pending {
        Pending {
            id: ModelId::issue(index, 1),
            nearest,
            vertices,
            replaced,
        }
    }

    // Plausible defects: pending models built farthest first; a first
    // model larger than the budget never taken, so its instances stay off
    // the TLAS for good; the budget ignored, so a world installed at once
    // builds in one frame; or a replaced model held back by the budget, so
    // an edited chunk drops out of a frame's rays. The oracle is the
    // architecture's rule over Bevy's budget loop (`blas.rs` 112–141),
    // which takes a model while the frame has built fewer vertices than the
    // budget.
    #[wasm_bindgen_test(unsupported = test)]
    fn pending_models_are_built_nearest_first_under_the_budget() {
        let budget = MOST_VERTICES_PER_FRAME;
        let ids = |admitted: Vec<ModelId>| -> Vec<usize> {
            admitted.into_iter().map(Identity::index).collect()
        };
        // The first is taken whatever its size.
        assert_eq!(ids(admit(vec![pending(0, 10., budget * 5, false)])), [0]);
        let fresh = [
            pending(1, 30., budget / 4, false),
            pending(2, 10., budget / 4 * 3, false),
            pending(3, 20., budget / 8, false),
            pending(4, 40., budget / 40, false),
        ];
        // Three quarters of the budget, then seven eighths, then past it,
        // after which none is taken.
        assert_eq!(ids(admit(fresh.to_vec())), [2, 3, 1]);
        // A replaced model is taken whatever the budget, and spends none.
        let mut edited = fresh.to_vec();
        edited.push(pending(5, 90., budget * 2, true));
        assert_eq!(ids(admit(edited)), [5, 2, 3, 1]);
    }
}
