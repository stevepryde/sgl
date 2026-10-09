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

pub(crate) use sets::{SetKey, SetLook};

use super::SceneError;
use super::deformation::InstanceDeformation;
use super::geometry::GeometryRange;
use super::materials::Material;
use super::models::{Model, Models};
use super::ranges::Ranges;
use crate::content::identity::{Identity, MaterialId, ModelId};
use crate::lod::MeshLod;
use crate::shading::culling::{ChainLevel, CullListsHeader, DrawCandidate};
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

/// A model's mesh as its instances' candidates take it
/// (`Scene::candidate_meshes`): its material and how its set's record takes
/// it, whether it is blended (and has no candidate), the most sections among
/// its levels (none: no candidate), its bounds in the model's space, its
/// record word, its level chain, and its positions' slab and first vertex
/// there (`GeometryRange::EMPTY`'s slab and `NO_POSITIONS` for a deforming
/// model's mesh, which has none).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CandidateMesh {
    pub material: MaterialId,
    pub look: SetLook,
    pub blended: bool,
    pub need: u32,
    pub bounds: [Vec3; 2],
    pub word: u32,
    pub chain: u32,
    pub positions: (u32, u32),
}

/// One candidate a placement accounts for: its set's key, how its set's
/// record takes its material, and the sections it draws at most.
pub(crate) type CandidateKey = (SetKey, SetLook, u32);

/// The candidates of an instance whose pose is `mirrored` and that deforms
/// or not, of `meshes`: each that is not blended and has sections, with its
/// mesh's index.
fn keyed(
    meshes: &[CandidateMesh],
    mirrored: bool,
    deforms: bool,
) -> impl Iterator<Item = (usize, CandidateKey)> + '_ {
    meshes
        .iter()
        .enumerate()
        .filter(|(_, mesh)| !mesh.blended && mesh.need > 0)
        .map(move |(index, mesh)| {
            let key = SetKey {
                material: mesh.material,
                mirrored,
                deforms,
                positions: mesh.positions.0,
            };
            (index, (key, mesh.look, mesh.need))
        })
}

/// What a placement of one instance's candidates changes of the sets and the
/// slots: its held candidates' (set, need) leave their sets and its slots
/// are freed, then `keys` take new slots and join their sets. Returns the
/// new slots and each key's set. A dry run (`Candidates::fit`) and a
/// placement take the same steps, so they agree on what fits.
fn account(
    (sets, placed): (&mut Sets, &mut Ranges),
    (held, slots): (&[(u32, u32)], Range<u32>),
    keys: &[CandidateKey],
) -> Option<(Range<u32>, Vec<u32>)> {
    for &(set, need) in held {
        sets.remove(set, need);
    }
    placed.free(slots);
    let range = placed.allocate(keys.len() as u32)?;
    let joined = keys
        .iter()
        .map(|&(key, look, need)| sets.add(key, look, need))
        .collect();
    Some((range, joined))
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
    /// binds: a GPU-built view's lists and cluster lists hold them, a
    /// cascade's cluster list each region twice, its pulled and its paired
    /// draw instances. The camera's lists while it culls occlusion, an
    /// entry for each slot in each of three lists and one for each draw
    /// instance in its queue, fit within the same binding: at these caps
    /// they take at most 0.9 of it.
    most_slots: u32,
    most_regions: u32,
    /// Each slot's instance's model, which a frame's statistics readback
    /// keeps to attribute its candidates' sections
    /// (`Renderer::geometry_stats_for_model`); copied on write while a
    /// readback holds it.
    #[cfg(feature = "diagnostics")]
    models: std::sync::Arc<Vec<Option<ModelId>>>,
}

impl Candidates {
    /// Candidates for a device of `limits`.
    pub fn new(limits: &wgpu::Limits) -> Self {
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
            most_regions: most(2 * std::mem::size_of::<DrawInstance>(), 0),
            #[cfg(feature = "diagnostics")]
            models: Default::default(),
        }
    }

    /// The (set, need) of each candidate the instance at `index` holds, and
    /// its slots.
    fn held(&self, index: usize) -> (Vec<(u32, u32)>, Range<u32>) {
        let range = self.by_instance.get(index).cloned().unwrap_or(0..0);
        let held = range
            .clone()
            .map(|slot| {
                let set = self.records.get(slot).draw_set;
                (set, self.slots[slot as usize].need)
            })
            .collect();
        (held, range)
    }

    /// Whether the slots and sets fit what the device binds.
    fn fits(&self, sets: &Sets, placed: &Ranges) -> bool {
        placed.end() <= self.most_slots && sets.fit(self.most_regions)
    }

    /// Whether placing each listed instance's candidates in turn, as
    /// `place` would, replacing those it holds, keeps every slot and region
    /// within what the device binds after each placement: a dry run on
    /// copies of the sets and slots, which an edit that places many
    /// instances takes before it changes anything. Each instance is listed
    /// once.
    pub fn fit(&self, instances: impl IntoIterator<Item = (usize, Vec<CandidateKey>)>) -> bool {
        let (mut sets, mut placed) = (self.sets.clone(), self.placed.clone());
        for (index, keys) in instances {
            let (held, slots) = self.held(index);
            if account((&mut sets, &mut placed), (&held, slots), &keys).is_none()
                || !self.fits(&sets, &placed)
            {
                return false;
            }
        }
        true
    }

    /// The candidate keys of an instance of `meshes` at a pose that is
    /// `mirrored`, deforming or not, for `fit`.
    pub fn keys(meshes: &[CandidateMesh], mirrored: bool, deforms: bool) -> Vec<CandidateKey> {
        keyed(meshes, mirrored, deforms)
            .map(|(_, key)| key)
            .collect()
    }

    /// Places the candidates of the instance at `index`, of `model`'s
    /// `meshes`, at a pose that is `mirrored`, with each mesh's bounds
    /// `deformed` where it deforms, replacing those it held. Fails with
    /// nothing changed when they would pass what the device binds: the
    /// sets and slots are restored from copies taken before.
    pub fn place(
        &mut self,
        index: usize,
        (model, meshes): (ModelId, &[CandidateMesh]),
        mirrored: bool,
        deformed: Option<&[[Vec3; 2]]>,
    ) -> Result<(), SceneError> {
        #[cfg(not(feature = "diagnostics"))]
        let _ = model;
        let placing: Vec<(usize, CandidateKey)> =
            keyed(meshes, mirrored, deformed.is_some()).collect();
        let keys: Vec<CandidateKey> = placing.iter().map(|&(_, key)| key).collect();
        let (held, slots) = self.held(index);
        let restore = (self.sets.clone(), self.placed.clone());
        let placed = account(
            (&mut self.sets, &mut self.placed),
            (&held, slots.clone()),
            &keys,
        )
        .filter(|_| self.fits(&self.sets, &self.placed));
        let Some((range, joined)) = placed else {
            (self.sets, self.placed) = restore;
            return Err(SceneError::DeviceLimit);
        };
        // Its old slots end before its new ones, which may reuse them, are
        // written.
        for slot in slots {
            self.free_slot(slot);
        }
        for (slot, ((mesh, (_, _, need)), set)) in
            range.clone().zip(placing.into_iter().zip(joined))
        {
            let shape = &meshes[mesh];
            let bounds = deformed.map_or(shape.bounds, |deformed| deformed[mesh]);
            let record = DrawCandidate {
                bounds_min: bounds[0].to_array(),
                object: index as u32,
                bounds_max: bounds[1].to_array(),
                mesh: shape.word,
                draw_set: set,
                chain: shape.chain,
                positions: shape.positions.1,
                padding: 0,
            };
            self.records.set(slot, record, DrawCandidate::FREE);
            if self.slots.len() <= slot as usize {
                self.slots.resize(slot as usize + 1, Slot::default());
            }
            self.slots[slot as usize] = Slot {
                mesh: mesh as u32,
                need,
            };
            #[cfg(feature = "diagnostics")]
            {
                let models = std::sync::Arc::make_mut(&mut self.models);
                if models.len() <= slot as usize {
                    models.resize(slot as usize + 1, None);
                }
                models[slot as usize] = Some(model);
            }
        }
        if self.by_instance.len() <= index {
            self.by_instance.resize(index + 1, 0..0);
        }
        self.by_instance[index] = range;
        if meshes.iter().any(|mesh| mesh.blended) {
            self.blended.insert(index as u32);
        } else {
            self.blended.remove(&(index as u32));
        }
        Ok(())
    }

    /// Ends slot `slot`'s candidate, which its sets and slots no longer
    /// account for.
    fn free_slot(&mut self, slot: u32) {
        self.records
            .set(slot, DrawCandidate::FREE, DrawCandidate::FREE);
        self.slots[slot as usize] = Slot::default();
        #[cfg(feature = "diagnostics")]
        {
            std::sync::Arc::make_mut(&mut self.models)[slot as usize] = None;
        }
    }

    /// Removes the candidates of the instance at `index`.
    pub fn remove(&mut self, index: usize) {
        self.blended.remove(&(index as u32));
        let (held, slots) = self.held(index);
        account(
            (&mut self.sets, &mut self.placed),
            (&held, slots.clone()),
            &[],
        )
        .expect("an empty placement takes no slot");
        for slot in slots {
            self.free_slot(slot);
        }
        if let Some(range) = self.by_instance.get_mut(index) {
            *range = 0..0;
        }
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

    /// Material `id`'s values changed: its sets take how it now looks.
    pub fn material_changed(&mut self, id: MaterialId, look: SetLook) {
        self.sets.material_changed(id, look);
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

/// How a set's record takes `material`.
pub(crate) fn look(material: &Material) -> SetLook {
    SetLook {
        group: material.values.visibility_group,
        casts: material.casts_directional_shadows(),
        opaque: material.values.alpha == crate::AlphaMode::Opaque,
        displacement_bound: material.displacement_bound(),
    }
}

/// The most sections among the levels of a mesh of `base` sections whose
/// alternatives of `models` are `lods`, none where the mesh itself has none:
/// its candidates' share of their set's region, which a placement and an
/// edit's dry run both count by.
pub(crate) fn mesh_need(base: u32, lods: &[MeshLod], models: &Models) -> u32 {
    if base == 0 {
        return 0;
    }
    lods.iter().fold(base, |need, lod| {
        let alternative = &models
            .get(lod.model)
            .expect("a level of detail's model lives")
            .meshes[lod.mesh];
        need.max(alternative.ranges.section_count())
    })
}

impl super::Scene {
    /// `model`'s meshes (`id`'s, or its replacement's) as its instances'
    /// candidates take them.
    pub(crate) fn candidate_meshes(&self, id: ModelId, model: &Model) -> Vec<CandidateMesh> {
        model
            .meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| {
                let material = self.drawn_material(mesh.material);
                CandidateMesh {
                    material: mesh.material,
                    look: look(material),
                    blended: material.values.blended(),
                    need: mesh_need(mesh.ranges.section_count(), &mesh.lods, &self.models),
                    bounds: mesh.ranges.bounds().unwrap_or([Vec3::ZERO; 2]),
                    word: model.ray.mesh_word(index),
                    chain: self.candidates.chains.of(id, index),
                    positions: match mesh.positions {
                        GeometryRange { count: 0, .. } => (
                            GeometryRange::EMPTY.slab,
                            crate::shading::vertex::NO_POSITIONS,
                        ),
                        range => (range.slab, range.first),
                    },
                }
            })
            .collect()
    }

    /// Each of `models`' instances, model by model, in index order: one
    /// walk of the instances for an edit's placements and their dry run.
    fn instances_of(&self, models: &[ModelId]) -> Vec<Vec<crate::InstanceId>> {
        let at: rustc_hash::FxHashMap<ModelId, usize> = models
            .iter()
            .enumerate()
            .map(|(at, &model)| (model, at))
            .collect();
        let mut groups = vec![Vec::new(); models.len()];
        for (id, instance) in self.instances.slots.iter() {
            if let Some(&at) = at.get(&instance.state.model) {
                groups[at].push(id);
            }
        }
        groups
    }

    /// Whether the candidates of each listed model's instances fit the
    /// device when, model by model, each takes the model's listed meshes,
    /// deforming where the listed flag says or, for none, where it deforms
    /// now: the dry run of the `place_candidates_of` an edit makes with
    /// those models in that order, which it takes before it changes
    /// anything. Each model is listed once.
    pub(crate) fn candidates_fit(
        &self,
        plan: &[(ModelId, &[CandidateMesh], Option<bool>)],
    ) -> bool {
        let models: Vec<ModelId> = plan.iter().map(|&(model, ..)| model).collect();
        let groups = self.instances_of(&models);
        let instances = plan
            .iter()
            .zip(&groups)
            .flat_map(|(&(_, meshes, deforms), ids)| {
                ids.iter().map(move |&id| {
                    let instance = self.instances.get(id).expect("a grouped instance lives");
                    let mirrored = instance.state.pose.determinant() < 0.;
                    let deforms = deforms.unwrap_or(instance.deformation.is_some());
                    (id.index(), Candidates::keys(meshes, mirrored, deforms))
                })
            });
        self.candidates.fit(instances)
    }

    /// Places again the candidates of `models`' instances, model by model
    /// in index order, after an edit changed what they name: a model's
    /// geometry or alternatives, or a material of its meshes' alpha mode.
    /// The edit took the same placements' dry run first
    /// (`candidates_fit`), so they fit.
    pub(crate) fn place_candidates_of(&mut self, models: &[ModelId]) {
        let groups = self.instances_of(models);
        for (&model, ids) in models.iter().zip(groups) {
            let meshes = self.candidate_meshes(model, self.drawn_model(model));
            for id in ids {
                let instance = self.instances.get(id).expect("a grouped instance lives");
                let mirrored = instance.state.pose.determinant() < 0.;
                let deformed = instance
                    .deformation
                    .as_ref()
                    .map(|deformation| deformation.mesh_bounds.as_slice());
                self.candidates
                    .place(id.index(), (model, &meshes), mirrored, deformed)
                    .expect("an edit's dry run found its candidates fit");
            }
        }
    }
}

#[cfg(test)]
mod tests;
