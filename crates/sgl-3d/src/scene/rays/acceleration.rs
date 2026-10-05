//! The hardware path's acceleration structures (the architecture's Hardware
//! ray tracing): beside the portable BVHs, where the device has ray queries
//! (`supported`), a BLAS for each model that does not deform and whose
//! hits a committed hit can judge (`RayClass::Opaque`), one for each
//! deforming instance over its deformed positions, and a TLAS over the
//! instance entries. A frame's builds are one `build_acceleration_structures`
//! call in its encoder, after the deform pass (`encode`); the scene commits
//! what they built at `finish_frame`, so an abandoned frame leaves its work
//! to the next. A model's BLAS waits, pending, for a frame that builds the
//! structures, and is built under a budget of vertices a frame nearest the
//! camera first (`blas`); the TLAS is rebuilt whole every such frame, as
//! Bevy b56fc29 (`crates/bevy_solari/src/scene/binder.rs` 74–81, 265–267)
//! and Wicked Engine (`wiRenderer.cpp`, `UpdateRaytracingAccelerationStructures`)
//! rebuild theirs. What the device cannot hold (its limits, or memory) is
//! left out and counted (`RayTracingStats`), never reaching wgpu's
//! validation.
use crate::content::identity::Identity;
use crate::content::instance::Mobility;
use crate::scene::instances::Instances;
use crate::scene::models::Models;
use crate::scene::ray_class::RayClass;
use crate::scene::static_edits::posed_bounds;
use glam::{Mat4, Vec3};
use std::sync::atomic::{AtomicBool, Ordering};

mod blas;

use blas::{BlasBuild, Blases};

/// The TLAS instance mask of a static instance, which a ray's cull mask
/// selects as the portable path selects the static instance BVH.
pub(crate) const MASK_STATIC: u8 = 1;
/// The TLAS instance mask of a moving instance, deforming ones among them.
pub(crate) const MASK_MOVING: u8 = 2;

/// Instances a TLAS instance's 24-bit custom index can name: an entry at a
/// higher index is left out (wgpu 29 refuses a larger custom index,
/// `wgpu-core` `command/ray_tracing.rs` 223–227).
const CUSTOM_INDICES: usize = 1 << 24;

/// Whether `device` traces rays in hardware: it has wgpu's experimental ray
/// queries (`graphics_device::ray_tracing_features`) and the
/// acceleration-structure limits `graphics_device::limits` requests.
pub(crate) fn supported(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    device
        .features()
        .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY)
        && limits.max_blas_primitive_count > 0
        && limits.max_blas_geometry_count > 0
        && limits.max_tlas_instance_count > 0
        && limits.max_acceleration_structures_per_shader_stage > 0
}

/// What the last rendered frame's hardware ray tracing held
/// (`Renderer::ray_tracing_stats`), counted over the capture-visible
/// instances rays stop at (those whose model has a mesh that is not
/// blended); zero for a frame that built no acceleration structures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RayTracingStats {
    /// Instances the scene's TLAS held.
    pub hardware: u32,
    /// Instances that do not deform and that the TLAS did not hold, which
    /// the portable BVHs cover: those whose model has a masked mesh, whose
    /// model's BLAS is pending, or that were left out.
    pub portable: u32,
    /// Instances left out of the TLAS because the device could not hold
    /// them: a model or instance beyond its acceleration-structure limits,
    /// or a structure its memory could not hold.
    pub left_out: u32,
}

/// An instance the TLAS may hold this frame: its entry, its BLAS, its pose,
/// its kind and whether it deforms.
struct TlasEntry {
    index: usize,
    blas: wgpu::Blas,
    pose: Mat4,
    mask: u8,
    deforms: bool,
}

/// Orders `entries` so that those the TLAS keeps come first, and returns
/// how many it keeps: none whose entry `index` is past the 24-bit custom
/// index, and, where more remain than `capacity`, the nearest the camera by
/// `distance`, so the farthest are left out.
fn keep<T>(
    entries: &mut [T],
    capacity: usize,
    index: impl Fn(&T) -> usize,
    distance: impl Fn(&T) -> f32,
) -> usize {
    entries.sort_by_key(|entry| index(entry) >= CUSTOM_INDICES);
    let named = entries.partition_point(|entry| index(entry) < CUSTOM_INDICES);
    if named <= capacity {
        return named;
    }
    // Distances are nonnegative, so their bits order as they do.
    entries[..named].sort_by_cached_key(|entry| distance(entry).max(0.).to_bits());
    capacity
}

/// The work of the frame being rendered, which `encode` records and
/// `finish_frame` commits once recorded.
struct FrameWork {
    builds: Vec<BlasBuild>,
    encoded: AtomicBool,
}

/// A scene's acceleration structures, which it holds while hardware ray
/// tracing is in effect on a device that has it.
pub(crate) struct AccelerationStructures {
    blases: Blases,
    tlas: wgpu::Tlas,
    /// Changes whenever `tlas` is replaced, so that a tracing pass's cached
    /// group binds the TLAS it holds (`CachedGroup::get_with_structures`).
    tlas_generation: u64,
    /// The instances `tlas` can hold, and how many the last frame set.
    capacity: usize,
    held: usize,
    /// Whether the last prepared frame's TLAS holds each entry, by index:
    /// the portable walk covers the capture-visible instances it does not.
    holds: Vec<bool>,
    frame: Option<FrameWork>,
}

/// The distance from `eye` to the box `bounds`, zero inside it.
fn distance(eye: Vec3, [min, max]: [Vec3; 2]) -> f32 {
    eye.distance(eye.clamp(min, max))
}

/// `create`'s result, unless the device ran out of memory making it: an
/// out-of-memory error scope around it, which native wgpu reports when it
/// is popped. Only a device that traces rays in hardware creates
/// acceleration structures, and no browser does, so the scope's future is
/// ready at once.
fn allocated<T>(device: &wgpu::Device, create: impl FnOnce() -> T) -> Option<T> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let created = create();
    let mut error = std::pin::pin!(scope.pop());
    match error
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(None) => Some(created),
        std::task::Poll::Ready(Some(_)) | std::task::Poll::Pending => None,
    }
}

/// A TLAS of `capacity` instances, at least one, so wgpu builds it even
/// when it holds none (wgpu-core 29 sizes its scratch from the capacity,
/// `device/ray_tracing.rs` 205–218, and skips only a build with nothing at
/// all to build, `command/ray_tracing.rs` 286–294). Bevy's flags
/// (`binder.rs` 74–81).
fn tlas(device: &wgpu::Device, capacity: usize) -> Option<wgpu::Tlas> {
    allocated(device, || {
        device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: Some("scene TLAS"),
            max_instances: capacity as u32,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        })
    })
}

/// A 3×4 row-major affine transform of `pose`, as a TLAS instance takes it
/// (Bevy's `tlas_transform`, `binder.rs` 449–453).
fn tlas_transform(pose: Mat4) -> [f32; 12] {
    pose.transpose().to_cols_array()[..12]
        .try_into()
        .expect("a 3×4 transform")
}

impl AccelerationStructures {
    /// Empty structures, with a TLAS of one instance; none when the device
    /// cannot hold it.
    pub fn new(device: &wgpu::Device) -> Option<Self> {
        Some(Self {
            blases: Blases::default(),
            tlas: tlas(device, 1)?,
            tlas_generation: crate::scene::next_generation(),
            capacity: 1,
            held: 0,
            holds: Vec::new(),
            frame: None,
        })
    }

    /// Before a frame that builds the structures, seen from `eye`: commits
    /// ready compactions, chooses the BLASes the frame builds (pending
    /// models under the budget, replaced ones and deforming instances
    /// outside it) and sets the TLAS's instances, as many as the device
    /// holds, its capacity grown with the instance entries (`entries`). A
    /// frame rendered before it and never finished is abandoned: its work
    /// is chosen again.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (models, instances): (&Models, &Instances),
        entries: usize,
        eye: Vec3,
    ) -> RayTracingStats {
        let limits = device.limits();
        self.frame = None;
        self.blases.forget_removed(models, instances);
        self.blases.compact(device, queue);
        let mut stats = RayTracingStats::default();
        // The capture-visible instances rays stop at, by what holds them.
        let mut opaque = Vec::new();
        let mut deforming = Vec::new();
        for (id, instance) in instances.slots.iter() {
            let model = models
                .slots
                .get(instance.state.model)
                .expect("an instance's model lives");
            if !instance.state.capture_visible || model.ray_class == RayClass::None {
                continue;
            }
            if instance.deformation.is_some() {
                deforming.push(id);
            } else if model.ray_class == RayClass::Masked {
                // A predicate instance (the baseline form).
                stats.portable += 1;
            } else {
                opaque.push(id);
            }
        }
        let mut builds = self
            .blases
            .choose_models(device, &limits, models, instances, &opaque, eye);
        let mut held = Vec::with_capacity(opaque.len() + deforming.len());
        for &id in &opaque {
            let instance = instances.slots.get(id).expect("a live instance");
            let model = instance.state.model;
            let Some(blas) = self.blases.model(models, model) else {
                stats.portable += 1;
                stats.left_out += u32::from(self.blases.left_out(models, model));
                continue;
            };
            held.push(TlasEntry {
                index: id.index(),
                blas: blas.clone(),
                pose: instance.state.pose,
                mask: match instance.mobility {
                    Mobility::Static => MASK_STATIC,
                    Mobility::Moving => MASK_MOVING,
                },
                deforms: false,
            });
        }
        for &id in &deforming {
            let instance = instances.slots.get(id).expect("a live instance");
            let Some(blas) =
                self.blases
                    .deformed(device, &limits, models, (id, instance), &mut builds)
            else {
                stats.left_out += 1;
                continue;
            };
            held.push(TlasEntry {
                index: id.index(),
                blas,
                pose: instance.state.pose,
                mask: MASK_MOVING,
                deforms: true,
            });
        }
        self.reserve(device, &limits, entries);
        let kept = keep(
            &mut held,
            self.capacity,
            |entry| entry.index,
            |entry| {
                let instance = instances.slots.at(entry.index).expect("a live instance");
                let model = models
                    .slots
                    .get(instance.state.model)
                    .expect("an instance's model lives");
                distance(eye, posed_bounds(instance.bounds(model), entry.pose))
            },
        );
        for entry in &held[kept..] {
            stats.left_out += 1;
            stats.portable += u32::from(!entry.deforms);
        }
        self.hold(&held[..kept]);
        stats.hardware = kept as u32;
        self.frame = Some(FrameWork {
            builds,
            encoded: AtomicBool::new(false),
        });
        stats
    }

    /// Grows the TLAS's capacity with the instance entries (`entries`), up
    /// to the device's limit. A TLAS the device's memory cannot hold leaves
    /// the capacity as it was.
    fn reserve(&mut self, device: &wgpu::Device, limits: &wgpu::Limits, entries: usize) {
        let capacity = entries.min(limits.max_tlas_instance_count as usize);
        if capacity > self.capacity
            && let Some(grown) = tlas(device, capacity)
        {
            self.tlas = grown;
            self.tlas_generation = crate::scene::next_generation();
            self.capacity = capacity;
            self.held = 0;
        }
    }

    /// A frame that does not build the structures: the work an abandoned
    /// frame chose before it is left for the next frame that builds them,
    /// which chooses it again, so nothing records it against the ray source
    /// as it is now.
    pub fn skip_frame(&mut self) {
        self.frame = None;
    }

    /// Sets the TLAS's instances to `held`, each named by its entry's
    /// index, and clears the slots the last frame set beyond them.
    fn hold(&mut self, held: &[TlasEntry]) {
        self.holds.clear();
        for entry in held {
            if self.holds.len() <= entry.index {
                self.holds.resize(entry.index + 1, false);
            }
            self.holds[entry.index] = true;
        }
        for (slot, entry) in held.iter().enumerate() {
            self.tlas[slot] = Some(wgpu::TlasInstance::new(
                &entry.blas,
                tlas_transform(entry.pose),
                entry.index as u32,
                entry.mask,
            ));
        }
        for slot in held.len()..self.held {
            self.tlas[slot] = None;
        }
        self.held = held.len();
    }

    /// Records the frame's builds, after its deform pass: its BLASes over
    /// `source`, the ray source, then the TLAS, in one call.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, source: &wgpu::Buffer) {
        let Some(frame) = &self.frame else {
            return;
        };
        let entries: Vec<_> = frame
            .builds
            .iter()
            .map(|build| build.entry(source))
            .collect();
        for build in &frame.builds {
            build.count();
        }
        crate::counters::tlas_build();
        encoder.build_acceleration_structures(&entries, [&self.tlas]);
        frame.encoded.store(true, Ordering::Relaxed);
    }

    /// Commits a submitted frame's builds.
    pub fn finish_frame(&mut self) {
        if let Some(frame) = self.frame.take()
            && frame.encoded.into_inner()
        {
            self.blases.commit(frame.builds);
        }
    }

    /// The TLAS, which a tracing pass binds, and its generation, which
    /// changes whenever it is replaced.
    pub fn tlas(&self) -> (&wgpu::Tlas, u64) {
        (&self.tlas, self.tlas_generation)
    }

    /// Whether the last prepared frame's TLAS holds the instance whose
    /// entry is at `index`.
    pub fn holds(&self, index: usize) -> bool {
        self.holds.get(index).copied().unwrap_or(false)
    }

    /// The BLASes it holds and their triangles.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn held(&self) -> (u64, u64) {
        self.blases.held()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(test)]
mod pure_tests {
    use super::{CUSTOM_INDICES, keep, tlas_transform};
    use glam::{EulerRot, Mat4, Quat, Vec3, Vec4};
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defect: the pose given to the TLAS transposed or in column
    // order, which places every instance's geometry wrongly for every ray.
    // The oracle is glam's transform of points by the pose: the 3×4
    // row-major rows wgpu documents for `TlasInstance::transform`, applied
    // as an affine transform, must place them alike.
    #[wasm_bindgen_test(unsupported = test)]
    fn tlas_transforms_place_points_as_their_poses_do() {
        let pose = Mat4::from_scale_rotation_translation(
            Vec3::new(2., -1., 0.5),
            Quat::from_euler(EulerRot::YXZ, 0.7, -0.3, 1.9),
            Vec3::new(3., -7., 11.),
        );
        let rows = tlas_transform(pose);
        let row = |r: usize| Vec4::from_slice(&rows[r * 4..r * 4 + 4]);
        for point in [Vec3::ZERO, Vec3::X, Vec3::new(-2., 5., 0.25)] {
            let p = point.extend(1.);
            let placed = Vec3::new(row(0).dot(p), row(1).dot(p), row(2).dot(p));
            let expected = pose.transform_point3(point);
            assert!(
                placed.distance(expected) < 1e-5,
                "{point}: {placed} against {expected}"
            );
        }
    }

    // Plausible defects: an instance past the 24-bit custom index kept,
    // which makes wgpu refuse the whole TLAS build; the nearest instances
    // left out instead of the farthest where the device holds fewer; or
    // more kept than the TLAS holds. The oracle is the architecture's rule
    // over entries whose index and distance the test chose.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_tlas_keeps_the_nearest_named_instances() {
        let entries = [
            (5, 9.),
            (CUSTOM_INDICES, 0.),
            (1, 3.),
            (CUSTOM_INDICES + 7, 1.),
            (2, 1.),
            (9, 4.),
        ];
        let kept = |capacity: usize| {
            let mut entries = entries;
            let count = keep(&mut entries, capacity, |e| e.0, |e| e.1);
            let mut kept: Vec<usize> = entries[..count].iter().map(|e| e.0).collect();
            kept.sort_unstable();
            kept
        };
        assert_eq!(kept(10), [1, 2, 5, 9]);
        assert_eq!(kept(4), [1, 2, 5, 9]);
        assert_eq!(kept(2), [1, 2]);
        assert_eq!(kept(3), [1, 2, 9]);
    }
}
