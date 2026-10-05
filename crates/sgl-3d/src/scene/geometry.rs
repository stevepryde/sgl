//! Raster geometry (the architecture's "Raster geometry"): the shared
//! buffers that hold what shadow casters draw from vertex and index buffers,
//! each mesh's positions and its own and its caster clusters' indices,
//! suballocated in slabs as Bevy b56fc29 packs meshes
//! (`crates/bevy_render/src/mesh/allocator.rs` over
//! `crates/bevy_render/src/slab_allocator.rs`, MIT OR Apache-2.0): slabs of
//! one element layout each, `SlabAllocatorSettings`' defaults for their
//! sizes, the first slab with room taken, a new one when none has it, data
//! at Bevy's large threshold given a slab of its own, and an emptied slab
//! released. Ranges within a slab come from the scene's range allocator
//! (`ranges`) rather than Bevy's `offset_allocator`. Where Bevy commits a
//! frame's allocations together, a range is placed and written at the scene
//! operation, and a slab grows by copying it through the queue, as the ray
//! source grows: an abandoned frame loses no content.
use super::SceneError;
use super::ranges::Ranges;
use crate::shading::vertex::CasterVertex;

/// Bevy's `SlabAllocatorSettings::default()`: a slab's first size, its
/// largest size, the size of data given a slab of its own, and how a slab
/// grows.
const MIN_SLAB_BYTES: u64 = 1 << 20;
const MAX_SLAB_BYTES: u64 = 512 << 20;
const LARGE_BYTES: u64 = 256 << 20;
const GROWTH: f64 = 1.5;

/// What a slab holds: positions (`CasterVertex`), or `u32` indices, which
/// meshes and their caster clusters share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Elements {
    Positions,
    Indices,
}

impl Elements {
    /// An element's bytes, a multiple of the copy alignment, so any range
    /// of elements is written and copied whole.
    const fn size(self) -> u64 {
        match self {
            Self::Positions => std::mem::size_of::<CasterVertex>() as u64,
            Self::Indices => 4,
        }
    }

    fn usage(self) -> wgpu::BufferUsages {
        let copies = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        match self {
            Self::Positions => copies | wgpu::BufferUsages::VERTEX,
            Self::Indices => copies | wgpu::BufferUsages::INDEX,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Positions => "scene geometry positions",
            Self::Indices => "scene geometry indices",
        }
    }
}

/// A mesh's elements of one kind: their slab and where they start, in
/// elements. Empty ranges name no slab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GeometryRange {
    pub slab: u32,
    pub first: u32,
    pub count: u32,
}

impl GeometryRange {
    const EMPTY: Self = Self {
        slab: u32::MAX,
        first: 0,
        count: 0,
    };
}

struct Slab {
    elements: Elements,
    buffer: wgpu::Buffer,
    /// A general slab's ranges, in elements; None for a slab one large
    /// range owns.
    ranges: Option<Ranges>,
    /// The elements its buffer holds.
    capacity: u32,
}

pub(crate) struct GeometryBuffers {
    /// By identity; a released slab's identity is reused.
    slabs: Vec<Option<Slab>>,
    /// The device's largest buffer, in bytes.
    limit: u64,
}

impl GeometryBuffers {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            slabs: Vec::new(),
            limit: device.limits().max_buffer_size,
        }
    }

    /// Slab `slab`'s buffer.
    pub fn buffer(&self, slab: u32) -> &wgpu::Buffer {
        &self.slabs[slab as usize]
            .as_ref()
            .expect("a placed range's slab lives")
            .buffer
    }

    /// Places `data`, whole elements of kind `elements`, and writes it:
    /// in the first slab of that kind with room, growing it if it must, or
    /// in a new slab.
    pub fn place(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        elements: Elements,
        data: &[u8],
    ) -> Result<GeometryRange, SceneError> {
        let size = elements.size();
        debug_assert_eq!(data.len() as u64 % size, 0, "whole elements");
        let bytes = data.len() as u64;
        if bytes == 0 {
            return Ok(GeometryRange::EMPTY);
        }
        let count = u32::try_from(bytes / size).map_err(|_| SceneError::DeviceLimit)?;
        let largest = MAX_SLAB_BYTES.min(self.limit);
        let range = if bytes >= LARGE_BYTES.min(largest) {
            self.place_large(device, elements, count)?
        } else {
            self.place_general(device, queue, elements, count, (largest / size) as u32)
        };
        let slab = self.slabs[range.slab as usize].as_ref().unwrap();
        crate::counters::write_buffer(queue, &slab.buffer, u64::from(range.first) * size, data);
        Ok(range)
    }

    /// `count` elements in the first general slab of `elements` with room,
    /// growing it by half again until they fit, up to `most` elements, or
    /// in a new slab.
    fn place_general(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        elements: Elements,
        count: u32,
        most: u32,
    ) -> GeometryRange {
        for (index, slot) in self.slabs.iter_mut().enumerate() {
            let Some(slab) = slot.as_mut().filter(|slab| slab.elements == elements) else {
                continue;
            };
            let Some(ranges) = slab.ranges.as_mut() else {
                continue;
            };
            let Some(range) = ranges.allocate(count) else {
                continue;
            };
            if range.end > most {
                ranges.free(range);
                continue;
            }
            if range.end > slab.capacity {
                let mut capacity = slab.capacity;
                while capacity < range.end {
                    capacity = ((f64::from(capacity) * GROWTH).ceil() as u32).min(most);
                }
                slab.grow(device, queue, capacity);
            }
            return GeometryRange {
                slab: index as u32,
                first: range.start,
                count,
            };
        }
        let capacity = (MIN_SLAB_BYTES.div_ceil(elements.size()) as u32)
            .max(count)
            .min(most);
        let mut ranges = Ranges::new(0);
        let range = ranges.allocate(count).expect("a new slab has room");
        let slab = self.insert(Slab {
            elements,
            buffer: slab_buffer(device, elements, capacity),
            ranges: Some(ranges),
            capacity,
        });
        GeometryRange {
            slab,
            first: range.start,
            count,
        }
    }

    /// `count` elements in a slab of their own.
    fn place_large(
        &mut self,
        device: &wgpu::Device,
        elements: Elements,
        count: u32,
    ) -> Result<GeometryRange, SceneError> {
        if u64::from(count) * elements.size() > self.limit {
            return Err(SceneError::DeviceLimit);
        }
        let slab = self.insert(Slab {
            elements,
            buffer: slab_buffer(device, elements, count),
            ranges: None,
            capacity: count,
        });
        Ok(GeometryRange {
            slab,
            first: 0,
            count,
        })
    }

    fn insert(&mut self, slab: Slab) -> u32 {
        match self.slabs.iter().position(Option::is_none) {
            Some(free) => {
                self.slabs[free] = Some(slab);
                free as u32
            }
            None => {
                self.slabs.push(Some(slab));
                (self.slabs.len() - 1) as u32
            }
        }
    }

    /// Frees `range` for reuse, releasing its slab once it holds nothing.
    pub fn free(&mut self, range: GeometryRange) {
        if range.count == 0 {
            return;
        }
        let slot = &mut self.slabs[range.slab as usize];
        let slab = slot.as_mut().expect("a placed range's slab lives");
        let empty = match slab.ranges.as_mut() {
            Some(ranges) => {
                ranges.free(range.first..range.first + range.count);
                ranges.end() == 0
            }
            None => true,
        };
        if empty {
            *slot = None;
        }
    }

    /// The slabs' bytes, the bytes their ranges hold, and how many there
    /// are.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn sizes(&self) -> [u64; 3] {
        self.slabs
            .iter()
            .flatten()
            .fold([0; 3], |[bytes, live, count], slab| {
                let held = match &slab.ranges {
                    Some(ranges) => u64::from(ranges.end()) - ranges.free_units(),
                    None => u64::from(slab.capacity),
                };
                [
                    bytes + slab.buffer.size(),
                    live + held * slab.elements.size(),
                    count + 1,
                ]
            })
    }
}

impl Slab {
    /// Grows it to `capacity` elements, copying what it holds through the
    /// queue, never a frame's encoder.
    fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, capacity: u32) {
        crate::counters::geometry_growth();
        let grown = slab_buffer(device, self.elements, capacity);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("grow scene geometry slab"),
        });
        encoder.copy_buffer_to_buffer(&self.buffer, 0, &grown, 0, self.buffer.size());
        queue.submit([encoder.finish()]);
        self.buffer = grown;
        self.capacity = capacity;
    }
}

fn slab_buffer(device: &wgpu::Device, elements: Elements, capacity: u32) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some(elements.label()),
            size: u64::from(capacity) * elements.size(),
            usage: elements.usage(),
            mapped_at_creation: false,
        },
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{Elements, GeometryBuffers, MIN_SLAB_BYTES};
    use crate::shading::vertex::CasterVertex;
    use crate::test_support;

    // Plausible defect: a slab that grows loses what it held (a copy left
    // out, short or at the wrong offset), so every mesh placed before the
    // growth casts the wrong shadow. The oracle is the test's own data: a
    // mesh's positions, placed before a larger mesh outgrows the slab's
    // first megabyte, read back from the grown slab as they were given.
    #[test]
    fn a_grown_slab_keeps_what_it_held() {
        let Some((device, queue)) = test_support::device() else {
            return;
        };
        let mut geometry = GeometryBuffers::new(&device);
        let positions = |count: usize, from: f32| -> Vec<CasterVertex> {
            (0..count)
                .map(|index| CasterVertex {
                    position: [from + index as f32, -(index as f32), 0.5 * index as f32],
                })
                .collect()
        };
        let first = positions(100, 1.);
        let placed = geometry
            .place(
                &device,
                &queue,
                Elements::Positions,
                bytemuck::cast_slice(&first),
            )
            .unwrap();
        let larger = (MIN_SLAB_BYTES / Elements::Positions.size()) as usize;
        let grown = geometry
            .place(
                &device,
                &queue,
                Elements::Positions,
                bytemuck::cast_slice(&positions(larger, 1e4)),
            )
            .unwrap();
        assert_eq!(
            grown.slab, placed.slab,
            "the larger mesh grew the first slab"
        );
        let words = test_support::read_words(&device, &queue, geometry.buffer(placed.slab));
        let given: &[u32] = bytemuck::cast_slice(&first);
        let at = placed.first as usize * 3;
        assert_eq!(&words[at..at + given.len()], given);
    }
}
