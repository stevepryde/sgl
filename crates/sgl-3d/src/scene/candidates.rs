//! The scene's draw candidates (the architecture's "GPU draw lists and
//! occlusion culling"): beside its object record, each instance has a
//! candidate per mesh a GPU-built view may draw (every mesh with triangles
//! whose material is not blended), holding the mesh's bounds in its model's
//! space (a deforming instance's deformed ones, written with its
//! deformation), its object record's index, its mesh's record word, its set
//! (`sets`) and, for a mesh with registered alternatives, its level chain
//! (`chains`). An instance's candidates are consecutive slots, placed by
//! `scene::ranges`; a free slot names no set, and the cull skips it. The
//! scene keeps them up as its edits change what they describe, and also
//! indexes the instances whose models hold a blended mesh, which the CPU
//! builder's blended walk is over. Edits change the CPU records; the frame's
//! prepare uploads what changed (`mirror`).
mod chains;
mod mirror;
mod sets;

pub(crate) use sets::SetKey;

use super::SceneError;
use super::deformation::InstanceDeformation;
use super::materials::Materials;
use super::models::Models;
use super::ranges::Ranges;
use crate::content::identity::{Identity, MaterialId, ModelId};
use crate::content::instance::InstanceState;
use crate::shading::culling::{ChainLevel, CullListsHeader, DrawCandidate, NO_SET};
use crate::shading::vertex::DrawInstance;
use chains::Chains;
use glam::Vec3;
use mirror::Mirror;
use sets::Sets;
use std::collections::BTreeSet;
use std::ops::Range;

/// What a slot holds besides its record: the mesh of its instance's model
/// it draws, and the most sections among the mesh's levels, which its set's
/// region holds room for.
#[derive(Clone, Copy, Default)]
struct Slot {
    mesh: u32,
    need: u32,
}

pub(crate) struct Candidates {
    records: Mirror<DrawCandidate>,
    slots: Vec<Slot>,
    placed: Ranges,
    /// Each instance's slots, by its index.
    by_instance: Vec<Range<u32>>,
    /// The instances whose models hold a blended mesh, by index.
    blended: BTreeSet<u32>,
    pub(crate) sets: Sets,
    chains: Chains,
    /// The most candidate slots and draw instances in regions a device
    /// binds: a GPU-built view's lists and cluster list hold them.
    most_slots: u32,
    most_regions: u32,
    /// Each slot's instance's model, which a frame's statistics readback
    /// keeps to attribute its candidates' sections
    /// (`Renderer::geometry_stats_for_model`); copied on write while a
    /// readback holds it.
    #[cfg(feature = "diagnostics")]
    models: std::sync::Arc<Vec<Option<ModelId>>>,
}

/// One candidate of an instance being placed.
struct Placing {
    record: DrawCandidate,
    key: SetKey,
    slot: Slot,
}

impl Candidates {
    pub fn new(device: &wgpu::Device) -> Self {
        let limits = device.limits();
        let binding = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size);
        let most = |stride: usize, head: usize| {
            u32::try_from(binding.saturating_sub(head as u64) / stride as u64).unwrap_or(u32::MAX)
        };
        let candidate = std::mem::size_of::<DrawCandidate>();
        let entry = 2 * std::mem::size_of::<u32>();
        Self {
            records: Mirror::new("draw candidates"),
            slots: Vec::new(),
            placed: Ranges::new(0),
            by_instance: Vec::new(),
            blended: BTreeSet::new(),
            sets: Sets::new(),
            chains: Chains::new(),
            most_slots: most(candidate, 0).min(most(entry, std::mem::size_of::<CullListsHeader>())),
            most_regions: most(std::mem::size_of::<DrawInstance>(), 0),
            #[cfg(feature = "diagnostics")]
            models: Default::default(),
        }
    }

    /// Places the candidates of the instance at `index` in `state`, as
    /// `deformation` deforms it, replacing those it had. Fails with nothing
    /// changed when they would pass what the device binds.
    pub fn place(
        &mut self,
        index: usize,
        (state, deformation): (&InstanceState, Option<&InstanceDeformation>),
        models: &Models,
        materials: &Materials,
    ) -> Result<(), SceneError> {
        let model = models.get(state.model)?;
        let mirrored = state.pose.determinant() < 0.;
        let mut placing = Vec::new();
        let mut blended = false;
        for (mesh_index, mesh) in model.meshes.iter().enumerate() {
            let material = materials.get(mesh.material)?;
            if material.values.blended() {
                blended = true;
                continue;
            }
            let Some(bounds) = (match deformation {
                Some(deformation) => Some(deformation.mesh_bounds[mesh_index]),
                None => mesh.ranges.bounds(),
            }) else {
                continue;
            };
            let need = mesh
                .lods
                .iter()
                .fold(mesh.ranges.section_count(), |need, lod| {
                    let alternative = &models
                        .get(lod.model)
                        .expect("a level of detail's model lives")
                        .meshes[lod.mesh];
                    need.max(alternative.ranges.section_count())
                });
            // A deforming instance's empty mesh has bounds but no section.
            if need == 0 {
                continue;
            }
            placing.push(Placing {
                record: DrawCandidate {
                    bounds_min: bounds[0].to_array(),
                    object: index as u32,
                    bounds_max: bounds[1].to_array(),
                    mesh: model.ray.mesh_word(mesh_index),
                    draw_set: NO_SET,
                    chain: self.chains.of(state.model, mesh_index),
                    padding: [0; 2],
                },
                key: SetKey {
                    material: mesh.material,
                    mirrored,
                    deforms: deformation.is_some(),
                },
                slot: Slot {
                    mesh: mesh_index as u32,
                    need,
                },
            });
        }
        let range = self
            .placed
            .allocate(placing.len() as u32)
            .ok_or(SceneError::DeviceLimit)?;
        if self.placed.end() > self.most_slots {
            self.placed.free(range);
            return Err(SceneError::DeviceLimit);
        }
        for placing in &mut placing {
            placing.record.draw_set = self.sets.add(placing.key, materials, placing.slot.need);
        }
        if self.sets.region_end() > self.most_regions {
            for placing in &placing {
                self.sets
                    .remove(placing.record.draw_set, placing.slot.need, materials);
            }
            self.placed.free(range);
            return Err(SceneError::DeviceLimit);
        }
        self.remove(index, materials);
        for (slot, placing) in range.clone().zip(placing) {
            self.records.set(slot, placing.record, DrawCandidate::FREE);
            if self.slots.len() <= slot as usize {
                self.slots.resize(slot as usize + 1, Slot::default());
            }
            self.slots[slot as usize] = placing.slot;
            #[cfg(feature = "diagnostics")]
            {
                let models = std::sync::Arc::make_mut(&mut self.models);
                if models.len() <= slot as usize {
                    models.resize(slot as usize + 1, None);
                }
                models[slot as usize] = Some(state.model);
            }
        }
        if self.by_instance.len() <= index {
            self.by_instance.resize(index + 1, 0..0);
        }
        self.by_instance[index] = range;
        if blended {
            self.blended.insert(index as u32);
        }
        Ok(())
    }

    /// Removes the candidates of the instance at `index`.
    pub fn remove(&mut self, index: usize, materials: &Materials) {
        self.blended.remove(&(index as u32));
        let Some(range) = self.by_instance.get_mut(index).map(std::mem::take) else {
            return;
        };
        for slot in range.clone() {
            let record = *self.records.get(slot);
            self.sets
                .remove(record.draw_set, self.slots[slot as usize].need, materials);
            self.records
                .set(slot, DrawCandidate::FREE, DrawCandidate::FREE);
            self.slots[slot as usize] = Slot::default();
            #[cfg(feature = "diagnostics")]
            {
                std::sync::Arc::make_mut(&mut self.models)[slot as usize] = None;
            }
        }
        self.placed.free(range);
    }

    /// Writes the bounds of the instance at `index` as `deformation`
    /// deforms it now.
    pub fn deformed(&mut self, index: usize, deformation: &InstanceDeformation) {
        let range = self.by_instance.get(index).cloned().unwrap_or(0..0);
        for slot in range {
            let bounds = deformation.mesh_bounds[self.slots[slot as usize].mesh as usize];
            let record = DrawCandidate {
                bounds_min: bounds[0].to_array(),
                bounds_max: bounds[1].to_array(),
                ..*self.records.get(slot)
            };
            self.records.set(slot, record, DrawCandidate::FREE);
        }
    }

    /// Whether candidates of `slots` more instance meshes, drawing at most
    /// `sections` more sections, fit what the device binds, wherever
    /// re-placing their sets' regions puts them.
    pub fn can_hold(&self, slots: u64, sections: u64) -> bool {
        u64::from(self.placed.end()) + slots <= u64::from(self.most_slots)
            && u64::from(self.sets.region_end()) + 2 * sections < u64::from(self.most_regions)
    }

    /// Sets mesh `mesh` of `model`'s level chain from its registered
    /// alternatives in `models`; its instances' candidates are then placed
    /// again to name it.
    pub fn set_lods(&mut self, model: ModelId, mesh: usize, models: &Models) {
        let owner = models.get(model).expect("a model with alternatives lives");
        let base = &owner.meshes[mesh];
        let bounds = base.ranges.bounds();
        let levels: Vec<ChainLevel> = base
            .lods
            .iter()
            .map(|lod| {
                let alternative = models
                    .get(lod.model)
                    .expect("a level of detail's model lives");
                // An empty alternative has no sections: its bounds are the
                // base's, so the instance cull's tests take it as them.
                let [min, max] = alternative.meshes[lod.mesh]
                    .ranges
                    .bounds()
                    .or(bounds)
                    .unwrap_or([Vec3::ZERO; 2]);
                ChainLevel {
                    bounds_min: min.to_array(),
                    error: lod.max_error,
                    bounds_max: max.to_array(),
                    mesh: alternative.ray.mesh_word(lod.mesh),
                }
            })
            .collect();
        self.chains.set(model, mesh, &levels);
    }

    /// Removes `model`'s level chains: its geometry was replaced or it was
    /// removed.
    pub fn remove_chains(&mut self, model: ModelId) {
        self.chains.remove_model(model);
    }

    /// `material`'s values changed: its sets take its visibility group.
    pub fn material_changed(&mut self, id: MaterialId, materials: &Materials) {
        self.sets.material_changed(id, materials);
    }

    /// Uploads what the edits since the last frame changed.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.records.upload(device, queue);
        self.sets.upload(device, queue);
        self.chains.upload(device, queue);
    }

    /// The instances whose models hold a blended mesh, in index order.
    pub fn blended(&self) -> impl Iterator<Item = usize> + '_ {
        self.blended.iter().map(|&index| index as usize)
    }

    /// One past the last slot a candidate holds: the cull's candidates.
    pub fn end(&self) -> u32 {
        self.placed.end()
    }

    /// The candidates', sets' and chains' buffers, once uploaded.
    pub fn buffers(&self) -> [&wgpu::Buffer; 3] {
        [
            self.records.buffer(),
            self.sets.buffer(),
            self.chains.buffer(),
        ]
    }

    /// Each slot's instance's model.
    #[cfg(feature = "diagnostics")]
    pub fn models(&self) -> std::sync::Arc<Vec<Option<ModelId>>> {
        self.models.clone()
    }

    /// The candidates', sets' and chains' buffers' bytes.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn bytes(&self) -> [u64; 3] {
        [self.records.bytes(), self.sets.bytes(), self.chains.bytes()]
    }
}

impl super::Scene {
    /// Places again the candidates of `model`'s instances, after an edit
    /// changed what they name: its geometry or alternatives, or a material
    /// of its meshes' alpha mode. The edit checked first that they fit
    /// (`Candidates::can_hold`).
    pub(crate) fn place_candidates_of(&mut self, model: ModelId) {
        for (id, instance) in self.instances.slots.iter() {
            if instance.state.model == model {
                self.candidates
                    .place(
                        id.index(),
                        (&instance.state, instance.deformation.as_ref()),
                        &self.models,
                        &self.materials,
                    )
                    .expect("an edit checks its candidates fit before it commits");
            }
        }
    }

    /// Whether `model`'s instances' candidates fit the device when each of
    /// their meshes may draw at most the sections `sections` lists.
    pub(crate) fn candidates_fit(&self, model: ModelId, sections: &[u32]) -> bool {
        let users = self
            .instances
            .slots
            .iter()
            .filter(|(_, instance)| instance.state.model == model)
            .count() as u64;
        let total: u64 = sections.iter().map(|&count| u64::from(count)).sum();
        self.candidates
            .can_hold(users * sections.len() as u64, users * total)
    }
}
