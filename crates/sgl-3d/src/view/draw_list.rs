//! Draw lists: the two builders that turn the scene and a view into draws,
//! and the one executor that issues scene geometry draws (the
//! architecture's "Draw lists").
//!
//! The GPU builder (`gpu`) builds the camera's opaque and masked list and
//! each directional cascade's from the scene's draw candidates, culled on
//! the GPU (`stages::cull`). The CPU builder here walks the instances for
//! the rest (`view::population`): the camera's blended surfaces, the
//! local-light shadow faces and a probe capture's faces and cascades. It
//! finds each shown instance's draws (culled and LOD-selected per
//! instance), then merges the draws of equal geometry, material, pipeline
//! variant and mobility that draw the same index ranges into one instanced
//! draw, as Bevy 9d12036 batches its render phases
//! (crates/bevy_render/src/batching/no_gpu_preprocessing.rs, MIT OR
//! Apache-2.0): capture and caster populations into bins, as its binned
//! phases do, and blended ones only where they are adjacent in their
//! back-to-front order, as its sorted phases do. A batch's instances each
//! name their object record in the frame's draw instances
//! (`DrawInstances`), which the vertex stage steps through; no instance is
//! renumbered, so source identities and motion are those of the instance.
use super::View;
use super::culling::clip_intersects;
use super::population::{LightReach, Population, moving_caster_reaches, within};
use crate::content::identity::{Identity, ModelId};
use crate::scene::instances::Instance;
use crate::scene::models::{Mesh, Model};
use crate::shading::vertex::DrawInstance;
use crate::{Mobility, Scene};
use batching::{BatchKey, Batcher, InstanceDraw};
pub(crate) use binder::Binder;
use glam::{Mat4, Vec3};
pub(crate) use instances::DrawInstances;
pub use stats::GeometryStats;
use std::ops::Range;

/// The vertices and indices a batch draws: mesh `mesh` of `model`, by its
/// own indices or its caster clusters', or as deforming instance
/// `instance` deforms it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Geometry {
    Mesh {
        model: ModelId,
        mesh: usize,
    },
    Clusters {
        model: ModelId,
        mesh: usize,
    },
    Deformed {
        instance: usize,
        model: ModelId,
        mesh: usize,
    },
}

impl Geometry {
    /// Mesh `mesh` of `model` as `instance` (at `index`) shows it.
    fn of(index: usize, instance: &Instance, model: ModelId, mesh: usize) -> Self {
        if instance.deformation.is_some() {
            Self::Deformed {
                instance: index,
                model,
                mesh,
            }
        } else {
            Self::Mesh { model, mesh }
        }
    }
}

/// One instanced draw per index range: equal geometry, material, pipeline
/// variant and mobility for each of its instances.
pub(crate) struct DrawBatch {
    key: BatchKey,
    /// Its index ranges in `DrawList::ranges`, in draw order.
    ranges: Range<usize>,
    /// Its instances in `DrawList::instances`, in draw order.
    instances: Range<u32>,
}

/// A view's draws, reused from frame to frame.
#[derive(Default)]
pub(crate) struct DrawList {
    batches: Vec<DrawBatch>,
    ranges: Vec<Range<u32>>,
    /// Each batch's instances, in batch order.
    instances: Vec<DrawInstance>,
    /// Where `instances` start in the draw instances it was built into.
    first: u32,
    /// The blended population's submitted draws.
    pub stats: GeometryStats,
    /// Merges the walk's draws, kept between builds for its capacity.
    batcher: Batcher,
}

impl DrawList {
    /// Replaces this list with `population`'s draws of `scene` from `view`
    /// and appends its instances to `drawn`, which it draws from. A mask of
    /// None (no frame yet) enables every group.
    pub fn build(
        &mut self,
        drawn: &mut DrawInstances,
        scene: &Scene,
        view: &View,
        visibility_mask: Option<u32>,
        population: Population<'_>,
    ) {
        self.batches.clear();
        self.ranges.clear();
        self.instances.clear();
        self.batcher.clear();
        self.stats = GeometryStats::default();
        // The blended population walks the instances the scene indexes as
        // holding a blended mesh; the others walk every instance.
        let walked: Box<dyn Iterator<Item = (usize, &Instance)>> = match population {
            Population::Blended { .. } => Box::new(scene.candidates.blended().map(|index| {
                let instance = scene.instances.slots.at(index);
                (index, instance.expect("an indexed instance lives"))
            })),
            _ => Box::new(
                scene
                    .instances
                    .slots
                    .iter()
                    .map(|(id, instance)| (id.index(), instance)),
            ),
        };
        for (index, instance) in walked {
            if !population.shows(instance) {
                continue;
            }
            let rank = self.batcher.rank(instance.state.model);
            let model = scene.drawn_model(instance.state.model);
            let shown = (index, instance, model, rank);
            match population {
                Population::LocalShadow {
                    position,
                    range,
                    reach,
                    ..
                } => match instance.mobility {
                    Mobility::Static => self.clusters(
                        scene,
                        view,
                        visibility_mask,
                        &population,
                        shown,
                        (position, range),
                        reach,
                    ),
                    Mobility::Moving => self.whole(
                        scene,
                        view,
                        visibility_mask,
                        &population,
                        shown,
                        (position, range),
                    ),
                },
                _ => self.object(scene, view, visibility_mask, &population, shown),
            }
        }
        let merged = (&self.ranges[..], &mut self.batches, &mut self.instances);
        if matches!(population, Population::Blended { .. }) {
            self.batcher.sort_by_depth(depth_in(scene, view));
            self.batcher.merge_adjacent(merged);
        } else {
            self.batcher.bin(merged);
        }
        if matches!(population, Population::Blended { .. }) {
            for batch in &self.batches {
                let instances = u64::from(batch.instances.end - batch.instances.start);
                for range in &self.ranges[batch.ranges.clone()] {
                    self.stats.add(batch.key.mobility, range, instances);
                }
            }
        }
        self.first = drawn.append(&self.instances);
    }

    /// The camera's blended draws of one instance, and a probe capture's
    /// faces' and cascades'.
    fn object(
        &mut self,
        scene: &Scene,
        view: &View,
        mask: Option<u32>,
        population: &Population,
        (index, instance, model, rank): (usize, &Instance, &Model, u32),
    ) {
        let pose = instance.state.pose;
        let (camera, lod, culled) = match population {
            Population::Blended { lod, cull } => (true, lod.as_ref(), *cull),
            _ => (false, None, false),
        };
        // The clip volume, built at the instance's first drawn mesh: a
        // population draws few of most instances' meshes, or none.
        let mut frustum = None;
        let mirrored = pose.determinant() < 0.;
        for (mesh_index, mesh) in model.meshes.iter().enumerate() {
            let material = scene.drawn_material(mesh.material);
            if !population.draws(material, mask) {
                continue;
            }
            let variant =
                population.variant(&material.values, mirrored, instance.deformation.is_some());
            // A deforming instance draws its model's own meshes, deformed.
            let lod = lod.filter(|_| instance.deformation.is_none());
            let (drawn_model, drawn_index) =
                match lod.and_then(|lod| lod.select(mesh, &scene.models, pose)) {
                    Some(alternative) => (alternative.model, alternative.mesh),
                    None => (instance.state.model, mesh_index),
                };
            let drawn_owner = scene.drawn_model(drawn_model);
            let drawn = &drawn_owner.meshes[drawn_index];
            let start = self.ranges.len();
            if camera {
                let frustum = culled.then(|| &*frustum.get_or_insert_with(|| view.frustum(pose)));
                let push = |range: Range<u32>| self.ranges.push(range);
                match &instance.deformation {
                    // Its triangles' bounds at bind do not hold it: it is
                    // culled whole, by its deformed bounds.
                    Some(deformation) => {
                        let bounds = deformation.mesh_bounds[mesh_index];
                        if frustum.is_none_or(|frustum| frustum.reaches(bounds)) {
                            drawn.ranges.visible(None, push);
                        }
                    }
                    None => drawn.ranges.visible(frustum, push),
                }
            } else if drawn.count > 0 {
                self.ranges.push(0..drawn.count);
            }
            if self.ranges.len() == start {
                continue;
            }
            self.batcher.push(InstanceDraw {
                key: BatchKey {
                    variant,
                    material: mesh.material,
                    geometry: Geometry::of(index, instance, drawn_model, drawn_index),
                    mobility: instance.mobility,
                },
                ranges: start..self.ranges.len(),
                instance: draw_instance(
                    index,
                    drawn_owner.ray.mesh_word(drawn_index),
                    instance,
                    drawn,
                ),
                order: (rank, mesh_index as u32),
            });
        }
    }

    /// A local-light shadow face's draws of a static instance: its meshes'
    /// caster clusters within `range` of `position` in the face's clip
    /// volume, among the groups `reach` found within the range.
    #[allow(clippy::too_many_arguments)]
    fn clusters(
        &mut self,
        scene: &Scene,
        view: &View,
        mask: Option<u32>,
        population: &Population,
        (index, instance, model, rank): (usize, &Instance, &Model, u32),
        light: (Vec3, f32),
        reach: &LightReach,
    ) {
        let view_projection = view.view_projection();
        let mirrored = instance.state.pose.determinant() < 0.;
        let mut groups = reach.of(index).peekable();
        while let Some(&(mesh_index, _)) = groups.peek() {
            let mesh = &model.meshes[mesh_index];
            let clustered = mesh
                .clusters
                .as_ref()
                .expect("a reached group's mesh has clusters");
            let bounds = &instance.casters[mesh_index];
            let material = scene.drawn_material(mesh.material);
            let enabled = population.draws(material, mask);
            let start = self.ranges.len();
            while let Some((_, group)) = groups.next_if(|&(mesh, _)| mesh == mesh_index) {
                if !enabled || !clip_intersects(bounds.groups[group], view_projection) {
                    continue;
                }
                let group = clustered.groups[group].clone();
                for (cluster, _) in clustered.clusters[group.clone()]
                    .iter()
                    .zip(&bounds.clusters[group])
                    .filter(|(_, bounds)| {
                        within(**bounds, light) && clip_intersects(**bounds, view_projection)
                    })
                {
                    // Clusters adjacent in the index buffer draw as one range.
                    match self.ranges[start..].last_mut() {
                        Some(last) if last.end == cluster.indices.start => {
                            last.end = cluster.indices.end;
                        }
                        _ => self.ranges.push(cluster.indices.clone()),
                    }
                }
            }
            if self.ranges.len() == start {
                continue;
            }
            self.batcher.push(InstanceDraw {
                key: BatchKey {
                    variant: population.variant(
                        &material.values,
                        mirrored,
                        instance.deformation.is_some(),
                    ),
                    material: mesh.material,
                    geometry: Geometry::Clusters {
                        model: instance.state.model,
                        mesh: mesh_index,
                    },
                    mobility: instance.mobility,
                },
                ranges: start..self.ranges.len(),
                instance: draw_instance(index, model.ray.mesh_word(mesh_index), instance, mesh),
                order: (rank, mesh_index as u32),
            });
        }
    }

    /// A local-light shadow face's draws of a moving instance that reaches
    /// it: its meshes whole.
    fn whole(
        &mut self,
        scene: &Scene,
        view: &View,
        mask: Option<u32>,
        population: &Population,
        (index, instance, model, rank): (usize, &Instance, &Model, u32),
        light: (Vec3, f32),
    ) {
        let pose = instance.state.pose;
        let bounds = instance.bounds(model);
        if !moving_caster_reaches(bounds, pose, view.view_projection(), light) {
            return;
        }
        let mirrored = pose.determinant() < 0.;
        for (mesh_index, mesh) in model.meshes.iter().enumerate() {
            let material = scene.drawn_material(mesh.material);
            if !population.draws(material, mask) {
                continue;
            }
            if mesh.count == 0 {
                continue;
            }
            let start = self.ranges.len();
            self.ranges.push(0..mesh.count);
            self.batcher.push(InstanceDraw {
                key: BatchKey {
                    variant: population.variant(
                        &material.values,
                        mirrored,
                        instance.deformation.is_some(),
                    ),
                    material: mesh.material,
                    geometry: Geometry::of(index, instance, instance.state.model, mesh_index),
                    mobility: instance.mobility,
                },
                ranges: start..self.ranges.len(),
                instance: draw_instance(index, model.ray.mesh_word(mesh_index), instance, mesh),
                order: (rank, mesh_index as u32),
            });
        }
    }

    /// Whether it draws nothing.
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Whether it draws a blended receiver of screen-space reflections,
    /// which the `Receivers` pass draws.
    pub fn holds_receivers(&self, scene: &Scene) -> bool {
        self.batches
            .iter()
            .any(|batch| execute::receives(scene, batch))
    }

    /// Whether it draws a transmissive material, which samples the
    /// transparent stage's copy of the composed frame.
    pub fn holds_transmissive(&self, scene: &Scene) -> bool {
        self.batches.iter().any(|batch| {
            scene
                .drawn_material(batch.key.material)
                .values
                .transmissive()
        })
    }

    /// The instances `batch` draws, in draw order.
    #[cfg(any(test, feature = "diagnostics"))]
    fn instances_of(&self, batch: &DrawBatch) -> &[DrawInstance] {
        &self.instances[batch.instances.start as usize..batch.instances.end as usize]
    }

    /// The draw calls `draw` issues, in order: each batch's index ranges in
    /// turn, each drawn for all the batch's instances.
    fn calls(&self) -> impl Iterator<Item = (&DrawBatch, &Range<u32>)> {
        self.batches.iter().flat_map(|batch| {
            self.ranges[batch.ranges.clone()]
                .iter()
                .map(move |range| (batch, range))
        })
    }

    /// The draw calls `draw` issues.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn draws(&self) -> usize {
        self.calls().count()
    }

    /// Its submitted draws of the instances of each model it draws: the
    /// draws that hold one, and the triangles they submit for them.
    #[cfg(feature = "diagnostics")]
    pub fn stats_by_model(&self, scene: &Scene) -> rustc_hash::FxHashMap<ModelId, (usize, u64)> {
        let mut by_model = rustc_hash::FxHashMap::default();
        let mut held = rustc_hash::FxHashMap::default();
        for batch in &self.batches {
            held.clear();
            for drawn in self.instances_of(batch) {
                let owner = scene.instances.slots.at(drawn.object as usize);
                let model = owner.expect("a drawn instance lives").state.model;
                *held.entry(model).or_insert(0u64) += 1;
            }
            for (&model, &instances) in &held {
                let total: &mut (usize, u64) = by_model.entry(model).or_default();
                for range in &self.ranges[batch.ranges.clone()] {
                    total.0 += 1;
                    total.1 += u64::from((range.end - range.start) / 3) * instances;
                }
            }
        }
        by_model
    }
}

/// The draw instance of mesh `mesh`, whose record is at word `word`, as
/// `instance` (at `index`) shows it: every index range of the mesh from
/// first index zero, with the base vertex an indexed draw of it adds to the
/// mesh's indices, its first vertex in its positions slab, or zero for a
/// deforming instance, whose own positions the draw binds.
fn draw_instance(index: usize, word: u32, instance: &Instance, mesh: &Mesh) -> DrawInstance {
    DrawInstance {
        object: index as u32,
        mesh: word,
        first_index: 0,
        triangles: mesh.count / 3,
        first_vertex: match instance.deformation {
            Some(_) => 0,
            None => mesh.positions.first,
        },
    }
}

/// The view depth of the bounds centre of the mesh a blended draw draws,
/// at its instance's pose, in `view` of `scene`: Bevy 9d12036's
/// `Transparent3d` sort key, the view-space Z of the mesh's world bounds
/// centre (`TransparentSortingInfo3d::Sorted`), which sorts back to front
/// ascending.
fn depth_in<'a>(scene: &'a Scene, view: &View) -> impl Fn(&InstanceDraw) -> f32 + 'a {
    let view_from_world = Mat4::from_cols_array_2d(&view.uniform.view);
    move |draw| {
        let (Geometry::Mesh { model, mesh } | Geometry::Deformed { model, mesh, .. }) =
            draw.key.geometry
        else {
            unreachable!("blended draws draw whole meshes");
        };
        let instance = scene
            .instances
            .slots
            .at(draw.instance.object as usize)
            .unwrap();
        let pose = instance.state.pose;
        let bounds = instance.mesh_bounds(scene.drawn_model(model), mesh);
        let centre = bounds.map_or(Vec3::ZERO, |[min, max]| (min + max) * 0.5);
        view_from_world
            .transform_point3(pose.transform_point3(centre))
            .z
    }
}

mod batching;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod batching_tests;
mod binder;
mod execute;
pub(crate) mod gpu;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod instance_transform_tests;
mod instances;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod instancing_tests;
pub(crate) mod readback;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod sort_tests;
mod stats;
