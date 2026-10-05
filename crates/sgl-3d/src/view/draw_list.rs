//! Draw lists: the one builder that turns the scene and a view into
//! instanced draws for a population (`view::population`), and the one
//! executor that issues scene geometry draws.
//!
//! The builder finds each shown instance's draws (culled and LOD-selected
//! per instance), then merges the draws of equal geometry, material,
//! pipeline variant and mobility that draw the same index ranges into one
//! instanced draw, as Bevy 9d12036 batches its render phases
//! (crates/bevy_render/src/batching/no_gpu_preprocessing.rs, MIT OR
//! Apache-2.0): opaque, masked, capture and caster populations into bins,
//! as its binned phases do, and blended ones only where they are adjacent
//! in their back-to-front order, as its sorted phases do. A batch's
//! instances each name their object record in the frame's draw instances
//! (`DrawInstances`), which the vertex stage steps through; no instance is
//! renumbered, so source identities and motion are those of the instance.
use super::View;
use super::culling::clip_intersects;
use super::pipelines::{GeometryPass, GeometryPipelines, Variant};
use super::population::{LightReach, Population, moving_caster_reaches, within};
use crate::content::identity::{Identity, ModelId};
use crate::scene::geometry::GeometryRange;
use crate::scene::instances::Instance;
use crate::scene::models::{Mesh, Model};
use crate::shading::vertex::{CASTER_SLOT, DRAW_INSTANCE_SLOT, DrawInstance};
use crate::{Mobility, Scene};
use batching::{BatchKey, Batcher, InstanceDraw};
use glam::{Mat4, Vec3};
pub(crate) use instances::DrawInstances;
use std::ops::Range;

/// A camera draw list's (draw calls, submitted triangles) by instance
/// mobility, after visibility, culling and LOD selection and before GPU
/// backface culling. An instanced draw holds instances of one mobility and
/// submits its triangles once per instance.
#[derive(Clone, Copy, Debug, Default)]
pub struct GeometryStats {
    pub static_instances: (usize, u64),
    pub moving_instances: (usize, u64),
}

impl GeometryStats {
    /// These draws and `other`'s.
    pub(crate) fn with(self, other: &Self) -> Self {
        let sum = |a: (usize, u64), b: (usize, u64)| (a.0 + b.0, a.1 + b.1);
        Self {
            static_instances: sum(self.static_instances, other.static_instances),
            moving_instances: sum(self.moving_instances, other.moving_instances),
        }
    }

    /// Draw calls and triangles of all content.
    pub fn total(&self) -> (usize, u64) {
        (
            self.static_instances.0 + self.moving_instances.0,
            self.static_instances.1 + self.moving_instances.1,
        )
    }

    /// A draw of `range` for `instances` instances of `mobility`.
    fn add(&mut self, mobility: Mobility, range: &Range<u32>, instances: u64) {
        let total = match mobility {
            Mobility::Static => &mut self.static_instances,
            Mobility::Moving => &mut self.moving_instances,
        };
        total.0 += 1;
        total.1 += u64::from((range.end - range.start) / 3) * instances;
    }
}

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
    /// The camera population's submitted draws.
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
        for (id, instance) in scene.instances.slots.iter() {
            if !population.shows(instance) {
                continue;
            }
            let rank = self.batcher.rank(instance.state.model);
            let model = scene.drawn_model(instance.state.model);
            let shown = (id.index(), instance, model, rank);
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
        if matches!(
            population,
            Population::Camera { .. } | Population::Blended { .. }
        ) {
            for batch in &self.batches {
                let instances = u64::from(batch.instances.end - batch.instances.start);
                for range in &self.ranges[batch.ranges.clone()] {
                    self.stats.add(batch.key.mobility, range, instances);
                }
            }
        }
        self.first = drawn.append(&self.instances);
    }

    /// The camera's, directional cascades' and probe views' draws of one
    /// instance.
    fn object(
        &mut self,
        scene: &Scene,
        view: &View,
        mask: Option<u32>,
        population: &Population,
        (index, instance, model, rank): (usize, &Instance, &Model, u32),
    ) {
        let pose = instance.state.pose;
        let camera = matches!(
            population,
            Population::Camera { .. } | Population::Blended { .. }
        );
        let caster = matches!(population, Population::DirectionalShadow { .. });
        let (lod, culled) = match population {
            Population::Camera { lod, cull } | Population::Blended { lod, cull } => {
                (lod.as_ref(), *cull)
            }
            Population::DirectionalShadow { cull, .. } => (None, *cull),
            _ => (None, false),
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
            if camera || culled {
                let frustum = culled.then(|| {
                    &*frustum.get_or_insert_with(|| {
                        let frustum = view.frustum(pose);
                        if caster {
                            frustum.without_near()
                        } else {
                            frustum
                        }
                    })
                });
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
                instance: DrawInstance {
                    object: index as u32,
                    mesh: drawn_owner.ray.mesh_word(drawn_index),
                    first_vertex: base_vertex(instance, drawn),
                },
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
                instance: DrawInstance {
                    object: index as u32,
                    mesh: model.ray.mesh_word(mesh_index),
                    first_vertex: base_vertex(instance, mesh),
                },
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
                instance: DrawInstance {
                    object: index as u32,
                    mesh: model.ray.mesh_word(mesh_index),
                    first_vertex: base_vertex(instance, mesh),
                },
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
        self.batches.iter().any(|batch| receives(scene, batch))
    }

    /// The instances `batch` draws, in draw order.
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

    /// Submitted draws of the instances of `model` in this camera list: the
    /// draws that hold one, and the triangles they submit for them.
    pub fn stats_for_model(&self, scene: &Scene, model: ModelId) -> (usize, u64) {
        let mut total = (0, 0);
        for batch in &self.batches {
            let instances = self
                .instances_of(batch)
                .iter()
                .filter(|drawn| {
                    let owner = scene.instances.slots.at(drawn.object as usize);
                    owner.is_some_and(|instance| instance.state.model == model)
                })
                .count() as u64;
            if instances == 0 {
                continue;
            }
            for range in &self.ranges[batch.ranges.clone()] {
                total.0 += 1;
                total.1 += u64::from((range.end - range.start) / 3) * instances;
            }
        }
        total
    }

    /// Issues this list's draws in `pass`, whose group 0 the caller bound,
    /// with the uploaded draw instances it was built into, and returns how
    /// many it issued: of a blended list, only its receivers' for the
    /// `Receivers` pass. The only place scene geometry is drawn: it binds
    /// the scene's group 1 and the draw instances once, and each batch's
    /// pipeline, material and, for an indexed pass, the geometry buffers
    /// when they change. An indexed draw draws its mesh's own index range
    /// at the mesh's first index in its slab, with the mesh's first vertex
    /// as the base vertex (`scene::geometry`).
    pub fn draw(
        &self,
        scene: &Scene,
        pipelines: &GeometryPipelines,
        drawn: &DrawInstances,
        pass: &mut wgpu::RenderPass<'_>,
        kind: GeometryPass,
    ) -> usize {
        if self.batches.is_empty() {
            return 0;
        }
        let pulled = kind.pulled();
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.set_vertex_buffer(DRAW_INSTANCE_SLOT, drawn.buffer().slice(..));
        let mut draws = 0;
        // This pass's pipeline for each variant, looked up once.
        let mut by_variant = [None; Variant::COUNT];
        let mut variant = None;
        let mut material = None;
        let mut geometry = None;
        // The bound positions (a slab, or a deformed instance's in the ray
        // source) and index slab, and where the batch's geometry starts.
        let mut positions = None;
        let mut index_slab = None;
        let mut first_index = 0;
        let receivers = kind == GeometryPass::Receivers;
        for (batch, range) in self.calls() {
            if receivers && !receives(scene, batch) {
                continue;
            }
            let key = batch.key;
            if variant != Some(key.variant) {
                let pipeline = by_variant[key.variant.index()]
                    .get_or_insert_with(|| pipelines.get(kind, key.variant));
                pass.set_pipeline(pipeline);
                variant = Some(key.variant);
            }
            if material != Some(key.material) {
                pass.set_bind_group(2, &scene.drawn_material(key.material).group, &[]);
                material = Some(key.material);
            }
            if !pulled && geometry != Some(key.geometry) {
                let (vertices, indices) = match key.geometry {
                    Geometry::Mesh { model, mesh } => {
                        let mesh = &scene.drawn_model(model).meshes[mesh];
                        (Positions::Slab(mesh.positions), mesh.indices)
                    }
                    Geometry::Clusters { model, mesh } => {
                        let mesh = &scene.drawn_model(model).meshes[mesh];
                        let clustered = mesh
                            .clusters
                            .as_ref()
                            .expect("a cluster batch's mesh has clusters");
                        (Positions::Slab(mesh.positions), clustered.indices)
                    }
                    Geometry::Deformed {
                        instance,
                        model,
                        mesh,
                    } => {
                        let owner = scene.drawn_model(model);
                        let word = scene
                            .instances
                            .slots
                            .at(instance)
                            .and_then(|instance| instance.deformation.as_ref())
                            .expect("a deformed batch's instance deforms")
                            .positions(
                                owner.deformation.as_ref().expect("its model deforms"),
                                mesh,
                            );
                        (Positions::Deformed(word), owner.meshes[mesh].indices)
                    }
                };
                if positions != Some(vertices.binding()) {
                    let buffer = match vertices {
                        Positions::Slab(range) => scene.geometry.buffer(range.slab).slice(..),
                        Positions::Deformed(word) => {
                            scene.rays.source().slice(u64::from(word) * 4..)
                        }
                    };
                    pass.set_vertex_buffer(CASTER_SLOT, buffer);
                    positions = Some(vertices.binding());
                }
                if index_slab != Some(indices.slab) {
                    let buffer = scene.geometry.buffer(indices.slab);
                    pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
                    index_slab = Some(indices.slab);
                }
                first_index = indices.first;
                geometry = Some(key.geometry);
            }
            let instances = self.first + batch.instances.start..self.first + batch.instances.end;
            if pulled {
                pass.draw(range.clone(), instances);
            } else {
                let indices = first_index + range.start..first_index + range.end;
                // Every instance of a batch draws its geometry, so the first's
                // base vertex is all of theirs.
                let base_vertex = self.instances[batch.instances.start as usize].first_vertex;
                let base_vertex =
                    i32::try_from(base_vertex).expect("a slab's vertices fit a base vertex");
                pass.draw_indexed(indices, base_vertex, instances);
            }
            draws += 1;
        }
        draws
    }
}

/// The base vertex an indexed draw of `mesh` as `instance` shows it adds to
/// the mesh's indices: its first vertex in its positions slab, or zero for a
/// deforming instance, whose own positions the draw binds.
fn base_vertex(instance: &Instance, mesh: &Mesh) -> u32 {
    match instance.deformation {
        Some(_) => 0,
        None => mesh.positions.first,
    }
}

/// Where an indexed draw's positions come from: a mesh's range of a
/// positions slab, or a deforming instance's deformed positions at a word
/// of the ray source.
#[derive(Clone, Copy)]
enum Positions {
    Slab(GeometryRange),
    Deformed(u32),
}

impl Positions {
    /// What binding them sets: a slab, or a word of the ray source.
    fn binding(self) -> (bool, u32) {
        match self {
            Self::Slab(range) => (true, range.slab),
            Self::Deformed(word) => (false, word),
        }
    }
}

/// Whether `batch` draws a blended receiver of screen-space reflections.
fn receives(scene: &Scene, batch: &DrawBatch) -> bool {
    scene
        .drawn_material(batch.key.material)
        .values
        .receives_screen_space_reflections()
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
#[cfg(all(test, not(target_arch = "wasm32")))]
mod instance_transform_tests;
mod instances;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod instancing_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod sort_tests;
