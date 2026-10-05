//! What the renderer lends every stage of one frame, and which view holds
//! the completed scene as it passes from stage to stage.
use super::FrameViews;
use super::bindings::FrameBindings;
use super::effective::{Effective, HardwareRayTracing};
use super::history::HistoryFrame;
use super::pipelines::GeometryPipelines;
use super::targets::{SharedTargets, Sizes, Surface};
use super::trace_paths::DeviceRayForm;
use crate::shading::RayQueryForm;
use crate::shading::uniforms::FrameValues;
use crate::timing::GpuTiming;
use crate::{FrameInput, Scene};

/// What every stage of one frame reads, after the prepare stage.
pub(crate) struct FrameContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub timing: Option<&'a GpuTiming>,
    pub effective: &'a Effective,
    pub sizes: Sizes,
    pub targets: &'a SharedTargets,
    /// The surface reflections and the temporal consumers read: the opaque
    /// depth lent until the receiver pass draws receivers, then the
    /// surface's own targets (`SharedTargets::surface`).
    pub surface: Surface<'a>,
    pub scene: &'a Scene,
    /// The camera's view data and the frame's data as uploaded.
    pub values: &'a FrameValues,
    /// The frame's authored values.
    pub input: &'a FrameInput,
    pub views: &'a FrameViews,
    pub bindings: &'a mut FrameBindings,
    pub pipelines: &'a GeometryPipelines,
    /// The camera history every stage continues or restarts with.
    pub history: HistoryFrame,
    /// The hardware path the frame's rays trace, if any.
    pub hardware_rays: Option<HardwareRays<'a>>,
}

/// The hardware path a frame's rays trace (the architecture's Hardware ray
/// tracing): the device's form, whose query module a tracing pipeline
/// composes (`shading::ray_trace_root`), and the scene's TLAS, which each
/// tracing pass binds in its group 3 (`shading::bind::tlas_entry`), lent
/// from the scene as the ray-hit group is.
#[derive(Clone, Copy)]
pub(crate) struct HardwareRays<'a> {
    pub tlas: &'a wgpu::Tlas,
    /// Changes whenever the scene replaces its TLAS.
    pub tlas_generation: u64,
    /// The form the device's tracing programs take, which falls back from
    /// candidates to the baseline when a candidate program fails
    /// (`TracePaths::pipeline`).
    pub device_form: &'a DeviceRayForm,
}

impl<'a> HardwareRays<'a> {
    /// The hardware path of a frame of `scene` under `effective` on a device
    /// whose form is `device_form`, whose prepare built the scene's
    /// acceleration structures for it (`prepared`); none where the portable
    /// path traces.
    pub fn of(
        effective: &Effective,
        scene: &'a Scene,
        prepared: bool,
        device_form: Option<&'a DeviceRayForm>,
    ) -> Option<Self> {
        let HardwareRayTracing::On(_) = effective.hardware_ray_tracing else {
            return None;
        };
        let (tlas, tlas_generation) = scene.acceleration_structures().filter(|_| prepared)?.tlas();
        Some(Self {
            tlas,
            tlas_generation,
            device_form: device_form?,
        })
    }

    /// The form a tracing pipeline composes the query module of: the
    /// frame's prepare built the structures for it, or for the candidate
    /// form it fell back from, which the baseline traces as well.
    pub fn form(&self) -> RayQueryForm {
        self.device_form.form()
    }
}

/// Which view holds the completed scene, passed from the transparent
/// stage through antialiasing to post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Completed {
    /// TAA's output in the post-effect context.
    Taa,
    /// FSR2's output.
    Fsr2,
    /// The shared composite.
    Composite,
}
