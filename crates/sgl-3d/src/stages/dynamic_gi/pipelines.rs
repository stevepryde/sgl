//! The stage's shaders and their compute pipelines: the allocation's and
//! the blends', and the trace's for each path its rays take.
use super::volume::Layouts;
use crate::shading::{self, RayQueryForm};
use crate::view::frame::HardwareRays;
use crate::view::pipelines::LitConstants;
use crate::view::trace_paths::{TracePath, TracePaths};
use std::collections::HashMap;

/// What the stage's passes share.
pub(crate) static COMMON: shading::Module = shading::Module {
    name: "dynamic_gi_common",
    source: include_str!("common.wgsl"),
    deps: &[&shading::DYNAMIC_GI],
};
/// The ray allocation and the trace's indirect dispatch, at the stage's own
/// group 0.
pub(crate) static ALLOCATE: shading::Module = shading::Module {
    name: "dynamic_gi_allocate",
    source: include_str!("allocate.wgsl"),
    deps: &[&COMMON],
};
/// The entry points the allocation's pipelines are created with.
pub(crate) const RANK_ENTRY: &str = "rank";
pub(crate) const THRESHOLD_ENTRY: &str = "threshold";
pub(crate) const ALLOCATE_ENTRY: &str = "allocate";
pub(crate) const PREPARE_TRACE_ENTRY: &str = "prepare_trace";
/// The trace: the volume's lit group 0 of the Extended binding tier, whose
/// probe texture it samples for the bounce, the scene's group 1 and the
/// stage's own group 3.
pub(crate) static TRACE: shading::Module = shading::Module {
    name: "dynamic_gi_trace",
    source: include_str!("trace.wgsl"),
    deps: &[
        &shading::tiers::BIND_LIT_EXTENDED,
        &shading::SURFACE_RAY,
        &shading::SHADOW_MASK_NONE,
        &COMMON,
    ],
};
/// The entry points the trace's pipelines are created with: the trace's,
/// and the observed trace's from the portable program (feature
/// `diagnostics`), which every path's program holds.
pub(crate) const TRACE_ENTRY: &str = "trace";
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) const TRACE_OBSERVED_ENTRY: &str = "trace_observed";
/// The irradiance and depth blends, at the stage's own group 0.
pub(crate) static UPDATE: shading::Module = shading::Module {
    name: "dynamic_gi_update",
    source: include_str!("update.wgsl"),
    deps: &[&COMMON],
};
/// The entry points the blends' pipelines are created with.
pub(crate) const UPDATE_IRRADIANCE_ENTRY: &str = "update_irradiance";
pub(crate) const UPDATE_DEPTH_ENTRY: &str = "update_depth";
pub(crate) const SETTLE_ENTRY: &str = "settle";
pub(crate) const SCROLL_ENTRY: &str = "scroll";

/// The stage's pipelines.
pub(super) struct Pipelines {
    pub rank: wgpu::ComputePipeline,
    pub threshold: wgpu::ComputePipeline,
    pub allocate: wgpu::ComputePipeline,
    pub prepare_trace: wgpu::ComputePipeline,
    pub update_irradiance: wgpu::ComputePipeline,
    pub update_depth: wgpu::ComputePipeline,
    pub settle: wgpu::ComputePipeline,
    pub scroll: wgpu::ComputePipeline,
    /// The trace's programs for each path its rays take.
    pub paths: TracePaths,
    /// The trace's pipelines for each set of lit constants and path, each
    /// created when a frame first needs it, as the geometry pipelines
    /// specialise on the scene's rectangle lights and decals.
    pub trace: HashMap<(LitConstants, Option<RayQueryForm>), wgpu::ComputePipeline>,
}

impl Pipelines {
    /// `layouts` are the stage's own groups', and `lit` and `scene` group
    /// 0's lit layout and group 1's.
    pub fn new(
        device: &wgpu::Device,
        layouts: &Layouts,
        lit: &shading::bind::LitLayout,
        scene: &wgpu::BindGroupLayout,
    ) -> Self {
        let module = |label, root| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(shading::compose(&[root]).into()),
            })
        };
        let allocate_shader = module("dynamic GI allocation", &ALLOCATE);
        let update_shader = module("dynamic GI blend", &UPDATE);
        let pipeline = |label, layout: &wgpu::BindGroupLayout, shader, entry_point| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                module: shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let rank = pipeline(
            "dynamic GI ramp rank",
            &layouts.allocate,
            &allocate_shader,
            RANK_ENTRY,
        );
        let threshold = pipeline(
            "dynamic GI ramp threshold",
            &layouts.allocate,
            &allocate_shader,
            THRESHOLD_ENTRY,
        );
        let allocate = pipeline(
            "dynamic GI allocation",
            &layouts.allocate,
            &allocate_shader,
            ALLOCATE_ENTRY,
        );
        let prepare_trace = pipeline(
            "dynamic GI dispatch",
            &layouts.allocate,
            &allocate_shader,
            PREPARE_TRACE_ENTRY,
        );
        let update_irradiance = pipeline(
            "dynamic GI irradiance blend",
            &layouts.update,
            &update_shader,
            UPDATE_IRRADIANCE_ENTRY,
        );
        let update_depth = pipeline(
            "dynamic GI depth blend",
            &layouts.update,
            &update_shader,
            UPDATE_DEPTH_ENTRY,
        );
        let settle = pipeline(
            "dynamic GI convergence",
            &layouts.update,
            &update_shader,
            SETTLE_ENTRY,
        );
        let scroll = pipeline(
            "dynamic GI scroll",
            &layouts.update,
            &update_shader,
            SCROLL_ENTRY,
        );
        Self {
            paths: TracePaths::new("dynamic GI rays", &TRACE, &layouts.trace, lit, scene),
            trace: HashMap::new(),
            rank,
            threshold,
            allocate,
            prepare_trace,
            update_irradiance,
            update_depth,
            settle,
            scroll,
        }
    }

    /// Makes the trace's pipeline compiled with `lit` for the path
    /// `hardware` takes; returns the path it took (`TracePaths::pipeline`).
    pub fn trace_pipeline(
        &mut self,
        device: &wgpu::Device,
        lit: LitConstants,
        hardware: Option<HardwareRays<'_>>,
    ) -> Option<RayQueryForm> {
        self.paths.pipeline(
            device,
            hardware,
            (&mut self.trace, lit),
            |TracePath { shader, layout, .. }| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("dynamic GI rays"),
                    layout: Some(layout),
                    module: shader,
                    entry_point: Some(TRACE_ENTRY),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &lit.constants(),
                        ..Default::default()
                    },
                    cache: None,
                })
            },
        )
    }
}
