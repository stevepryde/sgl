//! Per-pass GPU time from render/compute pass `timestamp_writes`.
//!
//! Metal writes pass counter samples asynchronously and does not order a query
//! resolve after them, even later in the same command buffer: such resolves
//! return zero, stale or reversed values. Each frame's queries are therefore
//! resolved only after that frame's work has completed, through a small ring
//! of query sets, so timing never waits on the GPU.
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const SLOTS: usize = 4;
/// Timed passes per frame; later passes in the same frame are left untimed.
pub const MAX_PASSES: usize = 128;
const PENDING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// GPU time one named group of passes added to its frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PassTime {
    pub name: &'static str,
    /// Sum over the group's passes of the time from the later of the pass's
    /// start and the end of every earlier timed pass, to the pass's end.
    /// Tile-based GPUs start a pass long before earlier fragments finish, so
    /// raw start-to-end spans overlap and would count shared time repeatedly.
    pub ms: f64,
}

/// Completed GPU timings for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameTime {
    /// Frame number counted by `GpuTiming::begin_frame`.
    pub frame: u64,
    /// From the first timed pass's start, or the previous frame's last timed
    /// end when that is later, to the last timed end. Includes untimed passes
    /// and idle gaps, so group times sum to at most this.
    pub total_ms: f64,
    /// Groups in first-use order.
    pub passes: Vec<PassTime>,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Free,
    /// Submitted; waiting for the frame's work to complete.
    Running,
    /// Resolve encoded; mapping not yet requested.
    Resolved,
    /// Waiting for the readback mapping.
    Reading,
}

struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    names: Vec<&'static str>,
    frame: u64,
    /// Newest timestamp read from this slot. Metal leaves an unwritten sample
    /// holding its previous value, so anything not newer is stale.
    newest: u64,
    state: State,
    signal: Arc<AtomicU8>,
}

/// Caller-owned GPU pass timing on one device. Per frame: `begin_frame`, pass
/// descriptors from `render_pass`/`compute_pass`, submit, then `submitted`.
pub struct GpuTiming {
    slots: Vec<Slot>,
    recording: Option<usize>,
    names: RefCell<Vec<&'static str>>,
    frame: u64,
    period_ns: f64,
    /// Last timed end of the most recently read frame.
    previous: Option<(u64, u64)>,
    completed: Vec<FrameTime>,
    latest: Option<FrameTime>,
}

impl GpuTiming {
    /// `None` when the device was created without `Features::TIMESTAMP_QUERY`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let size = (MAX_PASSES * 2 * size_of::<u64>()) as u64;
        let slots = (0..SLOTS)
            .map(|_| Slot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("pass timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: MAX_PASSES as u32 * 2,
                }),
                resolve: crate::counters::buffer(
                    device,
                    &wgpu::BufferDescriptor {
                        label: Some("pass timestamp resolve"),
                        size,
                        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: false,
                    },
                ),
                readback: crate::counters::buffer(
                    device,
                    &wgpu::BufferDescriptor {
                        label: Some("pass timestamp readback"),
                        size,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    },
                ),
                names: Vec::new(),
                frame: 0,
                newest: 0,
                state: State::Free,
                signal: Arc::new(AtomicU8::new(PENDING)),
            })
            .collect();
        Some(Self {
            slots,
            recording: None,
            names: RefCell::new(Vec::with_capacity(MAX_PASSES)),
            frame: 0,
            period_ns: f64::from(queue.get_timestamp_period()),
            previous: None,
            completed: Vec::new(),
            latest: None,
        })
    }

    /// Collect finished timings without blocking and start recording a frame.
    /// Returns frames completed since the previous call, oldest first. A frame
    /// is left untimed when every query set is still in flight.
    pub fn begin_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> std::vec::Drain<'_, FrameTime> {
        let _ = device.poll(wgpu::PollType::Poll);
        // An encoder abandoned before submission leaves its queries unwritten.
        self.recording = None;
        let mut order: Vec<usize> = (0..SLOTS).collect();
        order.sort_by_key(|&index| self.slots[index].frame);
        let mut encoder = None;
        for index in order {
            let slot = &mut self.slots[index];
            let signal = slot.signal.load(Ordering::Acquire);
            match (slot.state, signal) {
                (State::Reading, READY) => {
                    let previous = self
                        .previous
                        .filter(|(frame, _)| frame + 1 == slot.frame)
                        .map(|(_, end)| end);
                    if let Some((frame, end)) = read(slot, self.period_ns, previous) {
                        self.previous = Some((frame.frame, end));
                        self.latest = Some(frame.clone());
                        self.completed.push(frame);
                    }
                    slot.state = State::Free;
                }
                (State::Reading, FAILED) => slot.state = State::Free,
                (State::Running, READY) => {
                    let encoder = encoder.get_or_insert_with(|| {
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("resolve completed pass timestamps"),
                        })
                    });
                    let count = slot.names.len() as u32 * 2;
                    encoder.resolve_query_set(&slot.queries, 0..count, &slot.resolve, 0);
                    encoder.copy_buffer_to_buffer(
                        &slot.resolve,
                        0,
                        &slot.readback,
                        0,
                        u64::from(count) * size_of::<u64>() as u64,
                    );
                    slot.state = State::Resolved;
                }
                _ => {}
            }
        }
        if let Some(encoder) = encoder {
            queue.submit([encoder.finish()]);
            for slot in self.slots.iter_mut().filter(|s| s.state == State::Resolved) {
                slot.state = State::Reading;
                slot.signal.store(PENDING, Ordering::Release);
                let signal = slot.signal.clone();
                slot.readback
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        let value = if result.is_ok() { READY } else { FAILED };
                        signal.store(value, Ordering::Release);
                    });
            }
        }
        self.frame += 1;
        self.names.get_mut().clear();
        self.recording = self.slots.iter().position(|s| s.state == State::Free);
        self.completed.drain(..)
    }

    /// Timestamp writes for one render pass in the named group, or `None` when
    /// this frame is untimed or full. Passes in a group should be consecutive.
    pub fn render_pass(&self, name: &'static str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.next(name)
            .map(|(query_set, index)| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(index),
                end_of_pass_write_index: Some(index + 1),
            })
    }

    /// Timestamp writes for one compute pass in the named group.
    pub fn compute_pass(&self, name: &'static str) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        self.next(name)
            .map(|(query_set, index)| wgpu::ComputePassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(index),
                end_of_pass_write_index: Some(index + 1),
            })
    }

    /// Query pairs for up to `count` compute passes in the named group that
    /// another recorder begins in order: the query set, the first pair's
    /// beginning index and the pairs reserved. Give back the pairs it did not
    /// use with `release_unused` before timing any other pass.
    pub fn reserve_compute_passes(
        &self,
        name: &'static str,
        count: u32,
    ) -> Option<(wgpu::QuerySet, u32, u32)> {
        let slot = &self.slots[self.recording?];
        let mut names = self.names.borrow_mut();
        let first = names.len();
        let count = count.min((MAX_PASSES - first) as u32);
        if count == 0 {
            return None;
        }
        names.extend(std::iter::repeat_n(name, count as usize));
        Some((slot.queries.clone(), first as u32 * 2, count))
    }

    /// Return the last `unused` pairs of `reserve_compute_passes`.
    pub fn release_unused(&self, unused: u32) {
        let mut names = self.names.borrow_mut();
        let kept = names.len() - unused as usize;
        names.truncate(kept);
    }

    fn next(&self, name: &'static str) -> Option<(&wgpu::QuerySet, u32)> {
        let slot = &self.slots[self.recording?];
        let mut names = self.names.borrow_mut();
        if names.len() == MAX_PASSES {
            return None;
        }
        names.push(name);
        Some((&slot.queries, (names.len() as u32 - 1) * 2))
    }

    /// Call after submitting every command buffer that used this frame's writes.
    pub fn submitted(&mut self, queue: &wgpu::Queue) {
        let Some(index) = self.recording.take() else {
            return;
        };
        let names = std::mem::take(self.names.get_mut());
        if names.is_empty() {
            return;
        }
        let slot = &mut self.slots[index];
        slot.names = names;
        slot.frame = self.frame;
        slot.state = State::Running;
        slot.signal.store(PENDING, Ordering::Release);
        let signal = slot.signal.clone();
        queue.on_submitted_work_done(move || signal.store(READY, Ordering::Release));
    }

    /// The most recently completed frame.
    pub fn latest(&self) -> Option<&FrameTime> {
        self.latest.as_ref()
    }
}

/// The frame and its last timed end, given the previous frame's last end.
fn read(slot: &mut Slot, period_ns: f64, previous: Option<u64>) -> Option<(FrameTime, u64)> {
    let count = slot.names.len() * 2;
    let values: Vec<u64> = {
        let view = slot
            .readback
            .slice(..(count * size_of::<u64>()) as u64)
            .get_mapped_range();
        bytemuck::cast_slice(&view).to_vec()
    };
    slot.readback.unmap();
    let ms = |ticks: u64| ticks as f64 * period_ns / 1e6;
    let mut first = None;
    let mut frontier = previous;
    let mut groups: Vec<(&'static str, u64)> = Vec::new();
    for (name, pair) in slot.names.iter().zip(values.chunks_exact(2)) {
        let (begin, end) = (pair[0], pair[1]);
        // Unwritten, stale or reversed samples, and implausibly long passes.
        if begin <= slot.newest || end < begin || ms(end - begin) > 1000. {
            continue;
        }
        let start = frontier.map_or(begin, |frontier| frontier.max(begin));
        first.get_or_insert(start);
        frontier = Some(frontier.map_or(end, |frontier| frontier.max(end)));
        let added = end.saturating_sub(start);
        match groups.iter_mut().find(|group| group.0 == *name) {
            Some(group) => group.1 += added,
            None => groups.push((name, added)),
        }
    }
    let (first, last) = (first?, frontier?);
    slot.newest = last;
    let frame = FrameTime {
        frame: slot.frame,
        total_ms: ms(last.saturating_sub(first)),
        passes: groups
            .into_iter()
            .map(|(name, ticks)| PassTime {
                name,
                ms: ms(ticks),
            })
            .collect(),
    };
    Some((frame, last))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "timing_tests.rs"]
mod tests;
