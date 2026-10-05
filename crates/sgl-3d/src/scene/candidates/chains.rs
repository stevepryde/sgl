//! The level chains: a record per model and mesh with registered
//! alternatives (`Scene::set_mesh_lods`), each level's bounds, error and
//! mesh record word, which the instance cull chooses a level of detail from.
use super::mirror::Mirror;
use crate::content::identity::ModelId;
use crate::shading::culling::{ChainLevel, LodChain, MAX_MESH_LODS, NO_CHAIN};
use rustc_hash::FxHashMap;

pub(crate) struct Chains {
    by_mesh: FxHashMap<(ModelId, usize), u32>,
    /// Indices of removed chains, for reuse.
    free: Vec<u32>,
    records: Mirror<LodChain>,
}

const EMPTY: LodChain = LodChain {
    levels: [ChainLevel {
        bounds_min: [0.; 3],
        error: 0.,
        bounds_max: [0.; 3],
        mesh: 0,
    }; MAX_MESH_LODS],
    count: 0,
    padding: [0; 3],
};

impl Chains {
    pub fn new() -> Self {
        Self {
            by_mesh: FxHashMap::default(),
            free: Vec::new(),
            records: Mirror::new("level of detail chains"),
        }
    }

    /// Mesh `mesh` of `model`'s chain, or `NO_CHAIN`.
    pub fn of(&self, model: ModelId, mesh: usize) -> u32 {
        self.by_mesh
            .get(&(model, mesh))
            .copied()
            .unwrap_or(NO_CHAIN)
    }

    /// Sets mesh `mesh` of `model`'s alternatives to `levels`, detailed to
    /// coarse, at most `MAX_MESH_LODS`; none removes its chain.
    pub fn set(&mut self, model: ModelId, mesh: usize, levels: &[ChainLevel]) {
        if levels.is_empty() {
            if let Some(index) = self.by_mesh.remove(&(model, mesh)) {
                self.free.push(index);
            }
            return;
        }
        let index = match self.by_mesh.get(&(model, mesh)) {
            Some(&index) => index,
            None => {
                let index = self.free.pop().unwrap_or(self.records.len() as u32);
                self.by_mesh.insert((model, mesh), index);
                index
            }
        };
        let mut chain = EMPTY;
        chain.levels[..levels.len()].copy_from_slice(levels);
        chain.count = levels.len() as u32;
        self.records.set(index, chain, EMPTY);
    }

    /// Removes `model`'s chains: its geometry was replaced, or it was
    /// removed.
    pub fn remove_model(&mut self, model: ModelId) {
        let free = &mut self.free;
        self.by_mesh.retain(|&(owner, _), &mut index| {
            let keep = owner != model;
            if !keep {
                free.push(index);
            }
            keep
        });
    }

    /// The chain records' buffer.
    pub fn buffer(&self) -> &wgpu::Buffer {
        self.records.buffer()
    }

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.records.upload(device, queue);
    }

    #[cfg(any(test, feature = "diagnostics"))]
    pub fn bytes(&self) -> u64 {
        self.records.bytes()
    }
}
