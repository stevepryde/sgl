//! The executor's CPU form: a CPU-built list's draws, direct, each batch's
//! instanced over its index ranges; `gpu` holds its indirect form for the
//! GPU-built lists. Every geometry pass draws through one of the two, which
//! bind through one `Binder`.
use super::{Binder, DrawBatch, DrawInstances, DrawList, Geometry};
use crate::Scene;
use crate::scene::geometry::GeometryRange;
use crate::shading::vertex::{CASTER_SLOT, DRAW_INSTANCE_SLOT};
use crate::view::pipelines::{GeometryPass, GeometryPipelines};

impl DrawList {
    /// Issues this list's draws in `pass`, whose group 0 the caller bound,
    /// with the uploaded draw instances it was built into, and returns how
    /// many it issued: of a blended list, only its receivers' for the
    /// `Receivers` pass, and only those whose material's shader reads its
    /// volume path for the volume layers' passes. The only place scene geometry is drawn: it binds
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
        let mut binder = Binder::new(pipelines, kind);
        let mut geometry = None;
        // The bound positions (a slab, or a deformed instance's in the ray
        // source) and index slab, and where the batch's geometry starts.
        let mut positions = None;
        let mut index_slab = None;
        let mut first_index = 0;
        let receivers = kind == GeometryPass::Receivers;
        let volumes = kind.volume();
        for (batch, range) in self.calls() {
            if (receivers && !receives(scene, batch)) || (volumes && !measures_volume(scene, batch))
            {
                continue;
            }
            let key = batch.key;
            binder.bind(pass, scene, key.variant, key.material);
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
pub(super) fn receives(scene: &Scene, batch: &DrawBatch) -> bool {
    scene
        .drawn_material(batch.key.material)
        .values
        .receives_screen_space_reflections()
}

/// Whether `batch` draws a material whose shader reads its volume path
/// (`scene_volume_path`), whose meshes the volume layers draw.
pub(super) fn measures_volume(scene: &Scene, batch: &DrawBatch) -> bool {
    scene
        .drawn_material(batch.key.material)
        .values
        .shader
        .is_some_and(|shader| {
            scene
                .shaders
                .get(shader.shader)
                .is_ok_and(|shader| shader.reads_volume_path)
        })
}
