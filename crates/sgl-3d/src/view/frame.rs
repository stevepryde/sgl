//! What the renderer lends every stage of one frame, and which view holds
//! the completed scene as it passes from stage to stage.
use super::FrameViews;
use super::bindings::FrameBindings;
use super::effective::Effective;
use super::history::HistoryFrame;
use super::pipelines::GeometryPipelines;
use super::targets::{SharedTargets, Sizes, Surface};
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
