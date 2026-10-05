//! Clusters: which scene lights and decals can reach each part of a view,
//! as shading's accessor reads them (`shading/clusters.wgsl`). The main
//! camera's are Bevy's clustered forward grid, assigned on the CPU every
//! frame, with decals as Bevy assigns its clustered decals. Probe captures
//! and world-space ray hits take one cluster holding culled lists, as Wicked
//! Engine's ray hits loop over the frame's culled lights
//! (`rtreflectionCS.hlsl`). Every list holds its live lights before its
//! baked ones, so a receiver with baked lighting stops after the live ones,
//! and its decals after both.
//!
//! The grid and its assignment, a port of Bevy's clustered forward
//! assignment, are in `assign`.
mod assign;

use crate::content::decal::Decal;
use crate::content::identity::Identity;
use crate::content::light::Light;
use crate::scene::decals::Decals;
use crate::scene::lights::Lights;
use crate::shading::clusters::{CLUSTER_HEADER_WORDS, ClusterGrid};
pub(crate) use assign::{BoxVolume, CAMERA_CLUSTERS, ClusterConfig, ViewVolume};
use assign::{Clusterable, Scratch};
use glam::{Mat4, UVec2};

/// Marks a baked light's index in a (cluster, item) pair.
const BAKED: u32 = 1 << 31;
/// Marks a decal's index in a (cluster, item) pair.
const DECAL: u32 = 1 << 30;
const KIND: u32 = BAKED | DECAL;

/// The lights and decals a view may list: its lights that are on, when
/// `lights_enabled`, live ones first, then every decal; each light's or
/// decal's index marked with its kind.
fn clusterables(
    lights: &Lights,
    lights_enabled: bool,
    decals: &Decals,
    out: &mut Vec<Clusterable>,
) {
    out.clear();
    if lights_enabled {
        for baked in [false, true] {
            out.extend(
                lights
                    .on()
                    .filter(|(_, light)| light.baked == baked)
                    .map(|(id, light)| {
                        let kind = if baked { BAKED } else { 0 };
                        Clusterable::light(id.index() as u32 | kind, light)
                    }),
            );
        }
    }
    out.extend(
        decals
            .iter()
            .map(|(id, decal)| Clusterable::decal(id.index() as u32 | DECAL, decal)),
    );
}

/// Writes clusters.wgsl's `Clusters::data` for `cluster_count` clusters
/// from (cluster, item) `pairs`, which hold every live light's pair before
/// any baked light's (marked `BAKED`), and those before any decal's (marked
/// `DECAL`): each cluster's header (its first item, then its live lights',
/// baked lights' and decals' counts), then the clusters' items, live lights
/// then baked lights then decals. A stable counting sort; `cursor` is its
/// scratch.
fn pack(cluster_count: usize, pairs: &[(u32, u32)], cursor: &mut Vec<u32>, data: &mut Vec<u32>) {
    data.clear();
    data.resize(cluster_count * CLUSTER_HEADER_WORDS, 0);
    for &(cluster, item) in pairs {
        let count = match item & KIND {
            0 => 1,
            BAKED => 2,
            _ => 3,
        };
        data[cluster as usize * CLUSTER_HEADER_WORDS + count] += 1;
    }
    cursor.clear();
    let mut offset = data.len() as u32;
    for cluster in data.chunks_exact_mut(CLUSTER_HEADER_WORDS) {
        cluster[0] = offset;
        cursor.push(offset);
        offset += cluster[1] + cluster[2] + cluster[3];
    }
    data.resize(offset as usize, 0);
    for &(cluster, item) in pairs {
        let at = &mut cursor[cluster as usize];
        data[*at as usize] = item & !KIND;
        *at += 1;
    }
}

/// One view's clusters on the GPU: its grid and `data`, as clusters.wgsl's
/// `Clusters` holds them. The buffer grows as the lists do.
pub(crate) struct Clusters {
    label: &'static str,
    buffer: wgpu::Buffer,
    data: Vec<u32>,
    scratch: Scratch,
}

impl Clusters {
    pub fn new(device: &wgpu::Device, label: &'static str) -> Self {
        Self {
            label,
            buffer: clusters_buffer(device, label, GRID_BYTES + 16),
            data: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    /// The buffer group 0 binds, which `cluster` and `list` may replace.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Assigns the scene's lights, when `lights_enabled`, and its decals to
    /// the clusters of the camera `view` and unjittered `projection` at
    /// `screen` pixels, with `config`, and uploads them.
    #[allow(clippy::too_many_arguments)]
    pub fn cluster(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        lights: &Lights,
        lights_enabled: bool,
        decals: &Decals,
        view: Mat4,
        projection: Mat4,
        screen: [u32; 2],
        config: ClusterConfig,
    ) {
        clusterables(lights, lights_enabled, decals, &mut self.scratch.objects);
        let grid = assign::assign(
            view,
            projection,
            UVec2::from_array(screen).max(UVec2::ONE),
            config,
            &mut self.scratch,
        );
        let [x, y, z] = grid.dimensions;
        let scratch = &mut self.scratch;
        pack(
            (x * y * z) as usize,
            &scratch.pairs,
            &mut scratch.cursor,
            &mut self.data,
        );
        self.upload(device, queue, grid);
    }

    /// One cluster listing the scene's lights, when `lights_enabled`, that
    /// `keep_light` keeps and its decals that `keep_decal` keeps, and
    /// uploads it.
    #[allow(clippy::too_many_arguments)]
    pub fn list(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        lights: &Lights,
        lights_enabled: bool,
        keep_light: impl Fn(&Light) -> bool,
        decals: &Decals,
        keep_decal: impl Fn(&Decal) -> bool,
    ) {
        let scratch = &mut self.scratch;
        clusterables(lights, lights_enabled, decals, &mut scratch.objects);
        scratch.pairs.clear();
        for object in &scratch.objects {
            let index = (object.index & !KIND) as usize;
            let keep = if object.index & DECAL == 0 {
                keep_light(lights.slots.at(index).unwrap())
            } else {
                keep_decal(decals.slots.at(index).unwrap())
            };
            if keep {
                scratch.pairs.push((0, object.index));
            }
        }
        pack(1, &scratch.pairs, &mut scratch.cursor, &mut self.data);
        self.upload(device, queue, ClusterGrid::LIST);
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, grid: ClusterGrid) {
        let bytes = GRID_BYTES + 4 * self.data.len() as u64;
        if bytes > self.buffer.size() {
            let limits = device.limits();
            let limit = limits
                .max_storage_buffer_binding_size
                .min(limits.max_buffer_size);
            // Lists beyond the device's binding are cut short, losing the
            // farthest clusters' last items; a scene that fills one is far
            // past any lighting budget.
            let size = bytes.next_power_of_two().min(limit & !3);
            self.buffer = clusters_buffer(device, self.label, size);
        }
        let room = ((self.buffer.size() - GRID_BYTES) / 4) as usize;
        self.data.truncate(room);
        crate::counters::write_buffer(queue, &self.buffer, 0, bytemuck::bytes_of(&grid));
        if !self.data.is_empty() {
            crate::counters::write_buffer(
                queue,
                &self.buffer,
                GRID_BYTES,
                bytemuck::cast_slice(&self.data),
            );
        }
    }
}

const GRID_BYTES: u64 = std::mem::size_of::<ClusterGrid>() as u64;

fn clusters_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
