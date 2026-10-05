//! The camera's geometry statistics, read back without blocking: a frame's
//! render copies the GPU-built list's statistics words (and, with
//! diagnostics, its candidates' appended sections) for readback; once the
//! game submits the frame, `finish_frame` requests the map, which is never
//! waited on; a frame whose readback has arrived becomes what
//! `Renderer::geometry_stats` describes, with its blended list's CPU
//! counts. An abandoned frame's copy never ran, so every frame starts with
//! nothing copied. A frame whose map fails reports nothing.
use super::GeometryStats;
use super::gpu::GpuList;
#[cfg(feature = "diagnostics")]
use crate::content::identity::ModelId;
use crate::shading::culling::CullStatistics;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Readbacks in flight at most; a frame past them is not read back, so a
/// game that never asks keeps bounded memory.
const MOST_PENDING: usize = 8;
const PENDING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// One frame's readback and what its CPU knew of it.
struct Pending {
    buffer: wgpu::Buffer,
    ready: Arc<AtomicU8>,
    /// The blended list's counts.
    blended: GeometryStats,
    /// Each candidate slot's model, and the blended list's counts by model.
    #[cfg(feature = "diagnostics")]
    models: ModelStatistics,
}

/// What attributes a frame's candidates' statistics to models.
#[cfg(feature = "diagnostics")]
pub(crate) struct ModelStatistics {
    pub slots: Arc<Vec<Option<ModelId>>>,
    pub blended: rustc_hash::FxHashMap<ModelId, (usize, u64)>,
}

/// The most recent completed frame's statistics.
#[derive(Default)]
struct Completed {
    stats: GeometryStats,
    #[cfg(feature = "diagnostics")]
    by_model: rustc_hash::FxHashMap<ModelId, (usize, u64)>,
}

#[derive(Default)]
pub(crate) struct StatisticsReadback {
    /// The frame rendered, until it is submitted.
    next: Option<Pending>,
    pending: VecDeque<Pending>,
    /// Readback buffers to reuse.
    spare: Vec<wgpu::Buffer>,
    latest: Option<Completed>,
}

impl StatisticsReadback {
    /// Starts a frame: nothing copied yet.
    pub fn begin(&mut self) {
        if let Some(next) = self.next.take() {
            self.spare.push(next.buffer);
        }
    }

    /// Copies the frame's statistics of the camera's `list` for readback,
    /// with its blended list's counts `blended`.
    pub fn copy(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        list: &GpuList,
        blended: GeometryStats,
        #[cfg(feature = "diagnostics")] models: ModelStatistics,
    ) {
        if self.pending.len() >= MOST_PENDING {
            return;
        }
        let (draws, candidates) = list.statistics();
        let head = std::mem::size_of::<CullStatistics>() as u64;
        let size = head + candidates.map_or(0, |(_, count)| u64::from(count) * 8);
        let buffer = match self.spare.iter().position(|spare| spare.size() >= size) {
            Some(at) => self.spare.swap_remove(at),
            None => crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("geometry statistics readback"),
                    size: size.next_power_of_two(),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                },
            ),
        };
        encoder.copy_buffer_to_buffer(draws, 0, &buffer, 0, head);
        if let Some((at, count)) = candidates.filter(|&(_, count)| count > 0) {
            encoder.copy_buffer_to_buffer(draws, at, &buffer, head, u64::from(count) * 8);
        }
        self.next = Some(Pending {
            buffer,
            ready: Arc::new(AtomicU8::new(PENDING)),
            blended,
            #[cfg(feature = "diagnostics")]
            models,
        });
    }

    /// After the caller submitted the frame: requests its readback's map.
    pub fn submitted(&mut self) {
        if let Some(next) = self.next.take() {
            let ready = next.ready.clone();
            next.buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let state = if result.is_ok() { READY } else { FAILED };
                    ready.store(state, Ordering::Release);
                });
            self.pending.push_back(next);
        }
    }

    /// Takes, without waiting, the frames the device has completed, in
    /// order: the newest becomes the latest.
    pub fn poll(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        while let Some(front) = self.pending.front() {
            match front.ready.load(Ordering::Acquire) {
                PENDING => break,
                READY => {}
                // A map that failed (a lost device) reports nothing of its
                // frame, as `timing` drops a failed frame's timestamps; its
                // buffer is not reused.
                _ => {
                    self.pending.pop_front();
                    continue;
                }
            }
            let pending = self.pending.pop_front().unwrap();
            let mut completed = Completed::default();
            {
                let mapped = pending.buffer.slice(..).get_mapped_range();
                let words: &[u32] = bytemuck::cast_slice(&mapped);
                let statistics: CullStatistics =
                    bytemuck::pod_read_unaligned(bytemuck::cast_slice(&words[..4]));
                let opaque = GeometryStats {
                    static_instances: (
                        statistics.static_sections as usize,
                        u64::from(statistics.static_triangles),
                    ),
                    moving_instances: (
                        statistics.moving_sections as usize,
                        u64::from(statistics.moving_triangles),
                    ),
                };
                completed.stats = opaque.with(&pending.blended);
                #[cfg(feature = "diagnostics")]
                {
                    let mut by_model = pending.models.blended.clone();
                    let candidates = words[4..].chunks_exact(2);
                    for (model, counts) in pending.models.slots.iter().zip(candidates) {
                        if let Some(model) = model {
                            let total = by_model.entry(*model).or_default();
                            total.0 += counts[0] as usize;
                            total.1 += u64::from(counts[1]);
                        }
                    }
                    completed.by_model = by_model;
                }
            }
            pending.buffer.unmap();
            self.spare.push(pending.buffer);
            self.latest = Some(completed);
        }
    }

    /// The most recent completed frame's statistics; none before one
    /// completes.
    pub fn latest(&self) -> Option<GeometryStats> {
        self.latest.as_ref().map(|completed| completed.stats)
    }

    /// The most recent completed frame's draws of `model`'s instances.
    #[cfg(feature = "diagnostics")]
    pub fn latest_for(&self, model: ModelId) -> Option<(usize, u64)> {
        self.latest
            .as_ref()
            .map(|completed| completed.by_model.get(&model).copied().unwrap_or_default())
    }
}
