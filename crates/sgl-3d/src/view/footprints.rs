//! The view's instance footprints (the register's M-23): one draw per mesh
//! of each footprint the scene holds (`scene::footprints`) that the view's
//! clip volume meets, in the visibility groups its mask enables, built in
//! prepare into the frame's draw instances; the transparent stage's
//! temporal composition pass draws them (`stages::transparent::footprints`).
//! Where instances stood, drawn from the scene's footprint records; where
//! they stand, from their object records.
use super::ViewData;
use super::culling::clip_intersects;
use super::draw_list::DrawInstances;
use crate::Scene;
use crate::content::identity::Identity;
use crate::scene::footprints::Need;
use crate::scene::materials::group_enabled;
use crate::shading::vertex::DrawInstance;

/// One draw: its draw instance in the frame's draw instances, and its
/// vertices, three for each of the mesh's triangles.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FootprintDraw {
    pub instance: u32,
    pub vertices: u32,
}

#[derive(Default)]
pub(crate) struct FootprintList {
    /// Where changed instances stood.
    pub stood: Vec<FootprintDraw>,
    /// Where changed instances stand.
    pub stands: Vec<FootprintDraw>,
    /// The draws' instances, kept for their capacity.
    entries: Vec<DrawInstance>,
}

impl FootprintList {
    /// Whether the view draws no footprint.
    pub fn is_empty(&self) -> bool {
        self.stood.is_empty() && self.stands.is_empty()
    }

    /// Draws nothing.
    pub fn clear(&mut self) {
        self.stood.clear();
        self.stands.clear();
    }

    /// Replaces the list with the footprints of `scene` that `view`'s clip
    /// volume meets, each mesh whose material's group `mask` enables, and
    /// appends their draw instances to `drawn`. A stand's meshes are its
    /// model's that need one as its materials are now: neither blended,
    /// transmissive nor displaced by a shader (`Scene::footprint_stands`).
    pub fn build(&mut self, drawn: &mut DrawInstances, scene: &Scene, view: &ViewData, mask: u32) {
        self.clear();
        let footprints = &scene.footprints;
        if footprints.is_empty() {
            return;
        }
        self.entries.clear();
        let clip_from_world = view.view_projection();
        for (index, (stood, _)) in footprints.stood.iter().enumerate() {
            if !clip_intersects(stood.bounds, clip_from_world) {
                continue;
            }
            for mesh in &stood.meshes {
                if group_enabled(mesh.group, Some(mask)) {
                    self.entries.push(DrawInstance {
                        object: index as u32,
                        mesh: mesh.word,
                        first_index: 0,
                        triangles: mesh.triangles,
                        first_vertex: 0,
                    });
                }
            }
        }
        let stood = self.entries.len();
        for (stand, _) in &footprints.stands {
            let Some(instance) = scene.instances.slots.get(stand.id) else {
                continue;
            };
            let model = scene.drawn_model(instance.state.model);
            let pose = instance.state.pose;
            if !instance.state.visible
                || !clip_intersects(instance.bounds(model), clip_from_world * pose)
            {
                continue;
            }
            for (index, mesh) in model.meshes.iter().enumerate() {
                let material = scene.drawn_material(mesh.material);
                if mesh.count > 0
                    && scene.need(material) == Need::Footprint
                    && material.enabled(Some(mask))
                {
                    self.entries.push(DrawInstance {
                        object: stand.id.index() as u32,
                        mesh: model.ray.mesh_word(index),
                        first_index: 0,
                        triangles: mesh.count / 3,
                        first_vertex: 0,
                    });
                }
            }
        }
        if self.entries.is_empty() {
            return;
        }
        let first = drawn.append(&self.entries);
        for (index, entry) in self.entries.iter().enumerate() {
            let draw = FootprintDraw {
                instance: first + index as u32,
                vertices: entry.triangles * 3,
            };
            if index < stood {
                self.stood.push(draw);
            } else {
                self.stands.push(draw);
            }
        }
    }
}
