//! The dynamic GI stage's observation (`Diagnostics::dynamic_gi`, feature
//! `diagnostics`): an observed frame traces through the trace's observed
//! entry point, whose rays also write what their BVH walks cost; a pass sums
//! them, and the sums, the allocation's counts and the volume's convergence
//! are copied for a readback whose map is requested once the frame is
//! submitted and never waited on, as the instance visibility's are
//! (`stages::visible_instances`). Counts are deterministic for a given
//! scene, frame sequence and device.
use super::{ALLOCATION_TRACED, Allocation, Convergence, DynamicGiChanges, TRACE, volume};
use crate::diagnostics::DynamicGiReport;
use crate::view::frame::FrameContext;
use crate::view::pipelines::LitConstants;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) static OBSERVE: crate::shading::Module = crate::shading::Module {
    name: "dynamic_gi_observe",
    source: include_str!("observe.wgsl"),
    deps: &[&super::COMMON],
};

/// The probes' bins of rays (`DDGI_OBSERVED_RAY_BINS`).
const RAY_BINS: usize = 8;

/// `DdgiObservation` in observe.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Observation {
    rays: u32,
    fixed_rays: u32,
    hits: u32,
    visibility_rays: u32,
    ray_visits: u32,
    ray_visits_high: u32,
    visibility_visits: u32,
    visibility_visits_high: u32,
    most_ray_visits: u32,
    most_visibility_visits: u32,
    exhausted: u32,
    probes: [u32; RAY_BINS],
}

const OBSERVATION_BYTES: u64 = std::mem::size_of::<Observation>() as u64;
/// The allocation's counts the report takes: up to and including the
/// probes not yet blended.
const ALLOCATION_COUNTS: u64 = std::mem::offset_of!(Allocation, bins) as u64;
const CONVERGENCE_BYTES: u64 = std::mem::size_of::<Convergence>() as u64;
const READBACK_BYTES: u64 = OBSERVATION_BYTES + ALLOCATION_COUNTS + CONVERGENCE_BYTES;

/// Readbacks in flight at most; an observed frame past them is not
/// observed, so a game that never polls keeps bounded memory.
const MOST_PENDING: usize = 8;
const PENDING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// What the CPU knows of an observed frame.
#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub probes: u32,
    pub changes: DynamicGiChanges,
    pub scrolled: [i32; 3],
    pub moving_bounds: u32,
}

struct Pending {
    readback: wgpu::Buffer,
    ready: Arc<AtomicU8>,
    frame: Frame,
}

pub(super) struct Observer {
    layout: wgpu::BindGroupLayout,
    costs_layout: wgpu::BindGroupLayout,
    trace_layout: wgpu::PipelineLayout,
    pipeline: wgpu::ComputePipeline,
    /// The observed trace's pipelines for each set of lit constants.
    trace: HashMap<LitConstants, wgpu::ComputePipeline>,
    sums: wgpu::Buffer,
    /// Each ray slot's costs, sized to the rays' textures.
    costs: Option<wgpu::TextureView>,
    next: Option<Pending>,
    pending: VecDeque<Pending>,
    spare: Vec<wgpu::Buffer>,
    reports: Vec<DynamicGiReport>,
}

impl Observer {
    /// `lit`, `scene` and `trace` are the trace's groups 0, 1 and 3.
    pub fn new(
        device: &wgpu::Device,
        lit: &wgpu::BindGroupLayout,
        scene: &wgpu::BindGroupLayout,
        trace: &wgpu::BindGroupLayout,
    ) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let texture = |binding| {
            entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dynamic GI observation"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                texture(1),
                texture(2),
                entry(
                    3,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let costs_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dynamic GI ray costs"),
            entries: &[entry(
                0,
                wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rg32Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
            )],
        });
        let trace_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dynamic GI rays observed"),
            bind_group_layouts: &[Some(lit), Some(scene), Some(&costs_layout), Some(trace)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dynamic GI observation"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&OBSERVE]).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("dynamic GI observation"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("dynamic GI observation"),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                }),
            ),
            module: &shader,
            entry_point: Some("observe"),
            compilation_options: Default::default(),
            cache: None,
        });
        let sums = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("dynamic GI observation"),
                size: OBSERVATION_BYTES,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );
        Self {
            layout,
            costs_layout,
            trace_layout,
            pipeline,
            trace: HashMap::new(),
            sums,
            costs: None,
            next: None,
            pending: VecDeque::new(),
            spare: Vec::new(),
            reports: Vec::new(),
        }
    }

    /// The observer in `slot`, created on first use, while the frame is
    /// observed and can be, with its trace's pipeline for `lit` ready.
    /// `layouts` are the trace's groups 0 and 1, and its own group 3.
    pub fn for_frame<'a>(
        slot: &'a mut Option<Self>,
        (groups, trace): (&[wgpu::BindGroupLayout; 2], &wgpu::BindGroupLayout),
        ctx: &FrameContext<'_>,
        lit: LitConstants,
    ) -> Option<&'a mut Self> {
        if !ctx.effective.dynamic_gi_observation {
            return None;
        }
        let observer =
            slot.get_or_insert_with(|| Self::new(ctx.device, &groups[0], &groups[1], trace));
        if !observer.ready() {
            return None;
        }
        observer.trace_pipeline(ctx.device, lit);
        Some(observer)
    }

    /// Every frame starts with nothing observed: an observed frame
    /// abandoned before `finish_frame` leaves a readback whose copy never
    /// ran, which no later frame may map.
    pub fn begin(&mut self) {
        self.next = None;
    }

    /// Whether this frame can be observed: fewer readbacks than the most
    /// are in flight.
    pub fn ready(&self) -> bool {
        self.pending.len() < MOST_PENDING
    }

    /// The observed trace's pipeline for `lit`, created when first needed.
    pub fn trace_pipeline(&mut self, device: &wgpu::Device, lit: LitConstants) {
        let layout = &self.trace_layout;
        self.trace.entry(lit).or_insert_with(|| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("dynamic GI rays observed"),
                source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&TRACE]).into()),
            });
            let mut constants = lit.constants().to_vec();
            constants.push(("ray_observation_enabled", 1.));
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("dynamic GI rays observed"),
                layout: Some(layout),
                module: &shader,
                entry_point: Some("trace_observed"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                cache: None,
            })
        });
    }

    /// Binds the observed trace's pipeline for `lit` and its ray costs,
    /// sized for `rays`, at group 2.
    pub fn bind_trace(
        &mut self,
        device: &wgpu::Device,
        pass: &mut wgpu::ComputePass<'_>,
        lit: LitConstants,
        rays: &volume::Rays,
    ) {
        let size = rays.list.texture().size();
        if self
            .costs
            .as_ref()
            .is_none_or(|costs| costs.texture().size() != size)
        {
            self.costs = Some(volume::texture(
                device,
                "dynamic GI ray costs",
                [u64::from(size.width), u64::from(size.height)],
                wgpu::TextureFormat::Rg32Uint,
            ));
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dynamic GI ray costs"),
            layout: &self.costs_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(self.costs.as_ref().unwrap()),
            }],
        });
        pass.set_pipeline(&self.trace[&lit]);
        pass.set_bind_group(2, &group, &[]);
    }

    /// After the blends: sums the frame's ray costs and copies them, the
    /// allocation's counts and the convergence for readback.
    pub fn observe(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        (uniform, rays, volume): (&wgpu::Buffer, &volume::Rays, &volume::Volume),
        frame: Frame,
    ) {
        let Some(costs) = &self.costs else {
            return;
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dynamic GI observation"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&rays.list),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(costs),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.sums.as_entire_binding(),
                },
            ],
        });
        encoder.clear_buffer(&self.sums, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("dynamic GI observation"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups_indirect(&volume.allocation, 0);
        }
        let readback = self.spare.pop().unwrap_or_else(|| {
            crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("dynamic GI observation readback"),
                    size: READBACK_BYTES,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                },
            )
        });
        encoder.copy_buffer_to_buffer(&self.sums, 0, &readback, 0, OBSERVATION_BYTES);
        encoder.copy_buffer_to_buffer(
            &volume.allocation,
            0,
            &readback,
            OBSERVATION_BYTES,
            ALLOCATION_COUNTS,
        );
        encoder.copy_buffer_to_buffer(
            &volume.convergence,
            0,
            &readback,
            OBSERVATION_BYTES + ALLOCATION_COUNTS,
            CONVERGENCE_BYTES,
        );
        self.next = Some(Pending {
            readback,
            ready: Arc::new(AtomicU8::new(PENDING)),
            frame,
        });
    }

    /// After the caller submitted the frame: requests its readback's map.
    pub fn submitted(&mut self) {
        if let Some(next) = self.next.take() {
            let ready = next.ready.clone();
            next.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    ready.store(
                        if result.is_ok() { READY } else { FAILED },
                        Ordering::Release,
                    );
                });
            self.pending.push_back(next);
        }
    }

    /// The reports read back since the last call, oldest first.
    pub fn take_reports(&mut self, device: &wgpu::Device) -> Vec<DynamicGiReport> {
        let _ = device.poll(wgpu::PollType::Poll);
        while let Some(front) = self.pending.front() {
            match front.ready.load(Ordering::Acquire) {
                PENDING => break,
                READY => {}
                _ => panic!("dynamic GI observation readback failed"),
            }
            let pending = self.pending.pop_front().unwrap();
            let report = {
                let mapped = pending.readback.slice(..).get_mapped_range();
                report(&mapped, pending.frame)
            };
            pending.readback.unmap();
            self.spare.push(pending.readback);
            self.reports.push(report);
        }
        std::mem::take(&mut self.reports)
    }
}

/// The report of a frame `frame` describes from its readback `bytes`.
fn report(bytes: &[u8], frame: Frame) -> DynamicGiReport {
    let (observation, rest) = bytes.split_at(OBSERVATION_BYTES as usize);
    let (allocation, convergence) = rest.split_at(ALLOCATION_COUNTS as usize);
    let observation: Observation = bytemuck::pod_read_unaligned(observation);
    let allocation: &[u32] = bytemuck::cast_slice(allocation);
    let word = |offset: u64| allocation[(offset / 4) as usize];
    let converged = convergence[std::mem::offset_of!(Convergence, converged)..][..4]
        .try_into()
        .map(u32::from_le_bytes)
        .unwrap();
    let wide = |low: u32, high: u32| u64::from(high) << 32 | u64::from(low);
    DynamicGiReport {
        probes: frame.probes,
        traced_probes: word(ALLOCATION_TRACED),
        unblended_probes: word(std::mem::offset_of!(Allocation, unblended) as u64),
        blended_requests: word(std::mem::offset_of!(Allocation, demand) as u64),
        stride: 1 << word(std::mem::offset_of!(Allocation, stride) as u64),
        probes_by_rays: observation.probes,
        rays: observation.rays,
        fixed_rays: observation.fixed_rays,
        hits: observation.hits,
        visibility_rays: observation.visibility_rays,
        ray_visits: wide(observation.ray_visits, observation.ray_visits_high),
        most_ray_visits: observation.most_ray_visits,
        visibility_visits: wide(
            observation.visibility_visits,
            observation.visibility_visits_high,
        ),
        most_visibility_visits: observation.most_visibility_visits,
        exhausted_queries: observation.exhausted,
        paused: word(std::mem::offset_of!(Allocation, paused) as u64) != 0,
        converged: converged != 0,
        changes: frame.changes,
        scrolled: frame.scrolled,
        moving_bounds: frame.moving_bounds,
    }
}

/// The layout this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "dynamic_gi_observe",
        "DdgiObservation",
        Observation,
        [
            rays,
            fixed_rays,
            hits,
            visibility_rays,
            ray_visits,
            ray_visits_high,
            visibility_visits,
            visibility_visits_high,
            most_ray_visits,
            most_visibility_visits,
            exhausted,
            probes,
        ]
    )]
}

/// The constant this module shares with its shader.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    vec![crate::shading::layout_tests::Constant::new(
        "dynamic_gi_observe",
        "DDGI_OBSERVED_RAY_BINS",
        naga::Literal::U32(RAY_BINS as u32),
    )]
}
