//! The ray source above the model BVHs: one entry per instance at its
//! identity's index, holding what its object record lacks (its model's ray
//! words and its inverse pose), and two instance BVHs, static and moving,
//! over the posed model bounds of the capture-visible instances of each kind
//! that do not deform, whose leaves name entries. Two-level, as DXR and
//! Vulkan acceleration structures, Bevy's ray-traced scene and Wicked
//! Engine's hardware path are (Wald et al. 2003; Meister et al. 2021,
//! §5.3.3).
//!
//! Entries set since the last traced frame are uploaded together: one write
//! of them, packed, and a copy of each run of consecutive indices to its
//! place, submitted through the queue, so moving instances among static
//! ones cost one write rather than one each, as Bevy 9d12036 uploads the
//! instance slots that changed in one sparse update
//! (`bevy_solari/src/scene/binder/instances.rs`,
//! `bevy_render/src/render_resource/sparse_buffer_vec.rs`). The BVHs are
//! built on the CPU with the model BVHs' builder and node record (`bvh`),
//! each into its own range of the source, kept until it outgrows it: the
//! moving one on every traced frame and the static one on the first traced
//! frame after a static edit. They are rebuilt rather than refitted, as Bevy
//! (`scene/binder/tlas.rs`) and Wicked Engine 2ff1d9e
//! (`wiRenderer.cpp`, `UpdateRaytracingAccelerationStructures`) rebuild
//! their TLAS every frame and NVIDIA's and AMD's ray-tracing guides advise.
//! Nothing else is uploaded for unchanged content.
use super::bvh::{self, Primitive};
use super::{RayModel, SceneRays};
use crate::content::instance::Mobility;
use crate::counters::{BuildStep, step};
use crate::scene::SceneError;
use crate::scene::static_edits::posed_bounds;
use glam::{Mat4, Vec3};
use std::ops::Range;

/// An instance's entry, at its index in the entry buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct InstanceEntry {
    /// The inverse of its pose, which takes a world ray into its model's
    /// space.
    pub inverse_world: [[f32; 4]; 4],
    /// Its model's first mesh record.
    pub mesh_word: u32,
    /// Its model's BVH root, zero when it has no triangles.
    pub bvh_root: u32,
    pub padding: [u32; 2],
}

/// An instance BVH's leaf record: the index of the entry it names.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct InstanceLeaf {
    pub index: u32,
}

/// An instance an instance BVH bounds (`bounded`).
pub(crate) type Bounded = Primitive<InstanceLeaf>;

/// Instance `index`, whose model has triangles within `bounds`, at `pose`,
/// bounded by `posed_bounds`, which holds the posed model whatever the
/// rounding.
pub(crate) fn bounded(index: usize, bounds: [Vec3; 2], pose: Mat4) -> Bounded {
    let [min, max] = posed_bounds(bounds, pose);
    Primitive::new(
        min,
        max,
        InstanceLeaf {
            index: index as u32,
        },
    )
}

/// One instance BVH: its range of the source and its root, zero when it
/// bounds nothing.
#[derive(Default)]
struct InstanceBvh {
    range: Range<u32>,
    /// The instances the range holds a BVH over.
    capacity: usize,
    root: u32,
}

impl InstanceBvh {
    /// Room for a BVH over `instances`. True when its range moved, so it
    /// must be built again before a ray reads it.
    fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        instances: usize,
    ) -> Result<bool, SceneError> {
        if instances <= self.capacity {
            return Ok(false);
        }
        let capacity = instances.max(self.capacity * 2);
        let range = rays.allocate(device, queue, bvh::words::<InstanceLeaf>(capacity))?;
        rays.free(std::mem::replace(&mut self.range, range));
        self.capacity = capacity;
        self.root = 0;
        Ok(true)
    }

    /// Builds the BVH over `instances` in its range, through `words`.
    fn build(
        &mut self,
        queue: &wgpu::Queue,
        rays: &SceneRays,
        instances: &mut [Bounded],
        words: &mut Vec<u32>,
    ) {
        words.clear();
        self.root =
            bvh::append_primitives(instances, bvh::Split::EqualCounts, words, self.range.start);
        debug_assert!(words.len() <= self.range.len(), "a BVH fits its range");
        rays.write(queue, self.range.start, words);
    }
}

pub(crate) struct RayInstances {
    /// Group 1's entries, at their instances' indices.
    buffer: wgpu::Buffer,
    /// The entries as last set, which `buffer` mirrors once uploaded.
    entries: Vec<InstanceEntry>,
    /// Whether each entry was set since the last upload.
    dirty: Vec<bool>,
    /// The indices of the dirty entries, each once, so frames that trace
    /// nothing keep at most one per entry however often they set it.
    written: Vec<u32>,
    /// The entries an upload copies from, packed.
    staging: wgpu::Buffer,
    packed: Vec<InstanceEntry>,
    statics: InstanceBvh,
    moving: InstanceBvh,
    /// The static edits the static BVH bounds the scene after (`StaticEdits::
    /// edits`); none until it is built in its current range.
    statics_built: Option<u64>,
    /// The roots the source's header names.
    roots: [u32; 2],
    /// A build's words.
    words: Vec<u32>,
}

impl RayInstances {
    /// No entries, room for one, and empty BVHs.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: entry_buffer(device, 1),
            entries: Vec::new(),
            dirty: Vec::new(),
            written: Vec::new(),
            staging: staging_buffer(device, 1),
            packed: Vec::new(),
            statics: InstanceBvh::default(),
            moving: InstanceBvh::default(),
            statics_built: None,
            roots: [0; 2],
            words: Vec::new(),
        }
    }

    /// Group 1's entry buffer.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// The entries the entry buffer holds, which the hardware path's TLAS
    /// grows with.
    pub fn capacity(&self) -> usize {
        (self.buffer.size() / std::mem::size_of::<InstanceEntry>() as u64) as usize
    }

    /// Room for entries below index `count`, and for `kind`'s BVH over
    /// `instances` of it in `rays`' source. A grown entry buffer replaces
    /// the one group 1 binds, and every entry is uploaded to it again.
    pub fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        count: usize,
        kind: Mobility,
        instances: usize,
    ) -> Result<(), SceneError> {
        let stride = std::mem::size_of::<InstanceEntry>() as u64;
        let capacity = self.buffer.size() / stride;
        if count as u64 > capacity {
            let limits = device.limits();
            let limit = limits
                .max_storage_buffer_binding_size
                .min(limits.max_buffer_size)
                / stride;
            if count as u64 > limit {
                return Err(SceneError::DeviceLimit);
            }
            self.buffer = entry_buffer(device, (count as u64).max(capacity * 2).min(limit));
            for index in 0..self.entries.len() {
                self.mark(index);
            }
        }
        match kind {
            Mobility::Static => {
                if self.statics.reserve(device, queue, rays, instances)? {
                    self.statics_built = None;
                }
            }
            Mobility::Moving => {
                self.moving.reserve(device, queue, rays, instances)?;
            }
        }
        Ok(())
    }

    /// Sets instance `index`'s entry to `model` at `pose`, which the next
    /// update uploads. Its index is below the reserved count.
    pub fn set(&mut self, index: usize, model: RayModel, pose: Mat4) {
        if self.entries.len() <= index {
            self.entries.resize(index + 1, bytemuck::Zeroable::zeroed());
            self.dirty.resize(index + 1, false);
        }
        self.entries[index] = InstanceEntry {
            inverse_world: pose.inverse().to_cols_array_2d(),
            mesh_word: model.mesh_word,
            bvh_root: model.bvh_root,
            padding: [0; 2],
        };
        self.mark(index);
    }

    /// Marks entry `index` for the next upload.
    fn mark(&mut self, index: usize) {
        if !self.dirty[index] {
            self.dirty[index] = true;
            self.written.push(index as u32);
        }
    }

    /// The entries the next upload writes.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.written.len()
    }

    /// The static BVH must be built again before a ray reads it: what it
    /// bounds moved with the render origin.
    pub fn rebuild_statics(&mut self) {
        self.statics_built = None;
    }

    /// Whether the static BVH must be built again after `edits` static
    /// edits (`StaticEdits::edits`).
    pub fn statics_stale(&self, edits: u64) -> bool {
        self.statics_built != Some(edits)
    }

    /// Before a traced frame: uploads the entries set since the last update,
    /// builds the moving BVH over `moving` and, given them, the static BVH
    /// over the scene's static instances after `edits` static edits, and
    /// names their roots in the header. Through the queue, never a frame's
    /// encoder, so an abandoned frame loses none of it.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &SceneRays,
        statics: Option<(&mut [Bounded], u64)>,
        moving: &mut [Bounded],
    ) {
        self.upload_entries(device, queue);
        if let Some((statics, edits)) = statics {
            step(BuildStep::StaticInstanceBvh, || {
                self.statics.build(queue, rays, statics, &mut self.words)
            });
            self.statics_built = Some(edits);
        }
        step(BuildStep::MovingInstanceBvh, || {
            self.moving.build(queue, rays, moving, &mut self.words)
        });
        let roots = [self.statics.root, self.moving.root];
        if roots != self.roots {
            rays.set_instance_roots(queue, roots);
            self.roots = roots;
        }
    }

    /// Uploads the entries set since the last upload: one write of them,
    /// packed in index order, then a copy of each run of consecutive indices
    /// to its place. A queue write of the staging buffer lands after the
    /// copies an earlier submission made from it.
    fn upload_entries(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.written.is_empty() {
            return;
        }
        self.written.sort_unstable();
        self.packed.clear();
        for &index in &self.written {
            self.dirty[index as usize] = false;
            self.packed.push(self.entries[index as usize]);
        }
        let stride = std::mem::size_of::<InstanceEntry>() as u64;
        let capacity = self.staging.size() / stride;
        if self.packed.len() as u64 > capacity {
            self.staging = staging_buffer(device, (self.packed.len() as u64).max(capacity * 2));
        }
        crate::counters::write_buffer(queue, &self.staging, 0, bytemuck::cast_slice(&self.packed));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scene ray instance entries"),
        });
        let mut packed = 0;
        for run in self.written.chunk_by(|a, b| a + 1 == *b) {
            let len = run.len() as u64;
            encoder.copy_buffer_to_buffer(
                &self.staging,
                packed * stride,
                &self.buffer,
                u64::from(run[0]) * stride,
                len * stride,
            );
            packed += len;
        }
        queue.submit([encoder.finish()]);
        self.written.clear();
    }
}

fn entry_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene ray instance entries"),
            size: capacity * std::mem::size_of::<InstanceEntry>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}

fn staging_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene ray instance entry uploads"),
            size: capacity * std::mem::size_of::<InstanceEntry>() as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}
