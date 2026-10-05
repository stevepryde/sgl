//! Cull, the GPU draw lists' builder (the architecture's "GPU draw lists
//! and occlusion culling"): prepare's last GPU step, after the deform pass
//! and the acceleration-structure builds, runs the early phase for every
//! GPU-built view, the camera's opaque and masked surfaces and each
//! directional cascade's casters: the instance cull over the scene's draw
//! candidates, its finalize, then the section cull over the candidates it
//! passed, appending their sections to their sets' draws (cull.wgsl). It
//! owns its pipelines, their layouts and the bind groups over what the
//! renderer lends it, and nothing a pass draws from: each view's lists are
//! the view's (`view::draw_list::gpu`).
//!
//! Reads: the scene's draw candidates, sets, level chains, object records
//! and ray source (its section tables), and each GPU-built view's cull.
//! Writes: each GPU-built view's lists, dispatch, cluster list and draws.
//! Honours: the culling layer and the visibility mask, through each view's
//! cull. Timing group: `cull`.
use crate::shading::Module;
use crate::view::FrameViews;
use crate::view::draw_list::gpu::CullBuffers;
use crate::view::frame::FrameContext;

pub(crate) static CULL: Module = Module {
    name: "cull",
    source: include_str!("cull.wgsl"),
    deps: &[
        &crate::shading::CULLING,
        &crate::shading::UNIFORMS,
        &crate::shading::SCENE_SOURCE,
        &crate::shading::DRAW_INSTANCE,
    ],
};

/// The bindings cull.wgsl declares.
mod binding {
    pub const VIEW: u32 = 0;
    pub const CANDIDATES: u32 = 1;
    pub const CHAINS: u32 = 2;
    pub const OBJECTS: u32 = 3;
    pub const SETS: u32 = 4;
    pub const LISTS: u32 = 5;
    pub const SOURCE: u32 = 6;
    pub const REGIONS: u32 = 7;
    pub const DRAWS: u32 = 8;
    pub const DISPATCH: u32 = 9;
}

/// Each pipeline's bindings, in its layout: what it reads, then what it
/// writes. The section cull's never holds the dispatch it is driven by,
/// since wgpu tracks a buffer whole and refuses an indirect dispatch's
/// source as its writable storage.
const INSTANCE_CULL: &[u32] = &[
    binding::VIEW,
    binding::CANDIDATES,
    binding::CHAINS,
    binding::OBJECTS,
    binding::SETS,
    binding::LISTS,
];
const FINALIZE: &[u32] = &[binding::VIEW, binding::LISTS, binding::DISPATCH];
const SECTION_CULL: &[u32] = &[
    binding::VIEW,
    binding::CANDIDATES,
    binding::OBJECTS,
    binding::SETS,
    binding::LISTS,
    binding::SOURCE,
    binding::REGIONS,
    binding::DRAWS,
];

/// A view's bind groups, one per pipeline, with the buffers they bind.
struct ViewGroups {
    bound: [wgpu::Buffer; 10],
    groups: [wgpu::BindGroup; 3],
}

pub(crate) struct Cull {
    /// The instance cull's, the finalize's and the section cull's.
    pipelines: [wgpu::ComputePipeline; 3],
    layouts: [wgpu::BindGroupLayout; 3],
    /// The camera's groups, then each cascade's.
    groups: Vec<Option<ViewGroups>>,
}

impl Cull {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cull"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&CULL]).into()),
        });
        let layout = |label, bindings: &[u32]| {
            let entries: Vec<_> = bindings
                .iter()
                .map(|&binding| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: match binding {
                        binding::VIEW => wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        _ => wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage {
                                read_only: matches!(
                                    binding,
                                    binding::CANDIDATES
                                        | binding::CHAINS
                                        | binding::OBJECTS
                                        | binding::SETS
                                        | binding::SOURCE
                                ),
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    },
                    count: None,
                })
                .collect();
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &entries,
            })
        };
        let layouts = [
            layout("instance cull", INSTANCE_CULL),
            layout("cull finalize", FINALIZE),
            layout("section cull", SECTION_CULL),
        ];
        let pipeline = |layout: &wgpu::BindGroupLayout, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(entry),
                        bind_group_layouts: &[Some(layout)],
                        immediate_size: 0,
                    }),
                ),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            pipelines: [
                pipeline(&layouts[0], "cull_instances"),
                pipeline(&layouts[1], "cull_finalize"),
                pipeline(&layouts[2], "cull_sections"),
            ],
            layouts,
            groups: Vec::new(),
        }
    }

    /// The early phase for every GPU-built view of the frame: each view's
    /// lists reset in the frame's encoder, then, in one compute pass, its
    /// instance cull, finalize and section cull.
    pub fn encode_early(&mut self, ctx: &mut FrameContext<'_>) {
        self.encode(ctx.device, ctx.encoder, ctx.scene, ctx.views, ctx.timing);
    }

    /// `encode_early` for `views` of `scene`.
    pub(crate) fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        scene: &crate::Scene,
        views: &FrameViews,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let slots: Vec<_> = std::iter::once(&views.camera)
            .chain(&views.cascades[..views.cascade_count])
            .collect();
        for slot in &slots {
            let started = crate::counters::Moment::now();
            slot.list.encode_reset(encoder);
            slot.culled_since(started);
        }
        if slots.iter().all(|slot| !slot.list.culls()) {
            return;
        }
        if self.groups.len() < slots.len() {
            self.groups.resize_with(slots.len(), || None);
        }
        for (at, slot) in slots.iter().enumerate() {
            self.prepare_groups(device, scene, at, slot.list.buffers());
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("cull"),
            timestamp_writes: timing.and_then(|timing| timing.compute_pass("cull")),
        });
        for (at, slot) in slots.iter().enumerate() {
            if !slot.list.culls() {
                continue;
            }
            let started = crate::counters::Moment::now();
            let groups = &self.groups[at].as_ref().unwrap().groups;
            let [x, y] = slot.list.instance_workgroups();
            pass.set_pipeline(&self.pipelines[0]);
            pass.set_bind_group(0, &groups[0], &[]);
            pass.dispatch_workgroups(x, y, 1);
            pass.set_pipeline(&self.pipelines[1]);
            pass.set_bind_group(0, &groups[1], &[]);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.pipelines[2]);
            pass.set_bind_group(0, &groups[2], &[]);
            pass.dispatch_workgroups_indirect(slot.list.buffers().dispatch, 0);
            slot.culled_since(started);
        }
    }

    /// Creates view `at`'s groups over `scene`'s buffers and `view`'s
    /// unless they bind them already.
    fn prepare_groups(
        &mut self,
        device: &wgpu::Device,
        scene: &crate::Scene,
        at: usize,
        view: CullBuffers<'_>,
    ) {
        let [candidates, sets, chains] = scene.candidates.buffers();
        let buffers = [
            view.view,
            candidates,
            chains,
            scene.instances.objects.buffer(),
            sets,
            view.lists,
            scene.rays.source(),
            view.regions,
            view.draws,
            view.dispatch,
        ];
        if self.groups[at]
            .as_ref()
            .is_some_and(|groups| groups.bound.iter().eq(buffers))
        {
            return;
        }
        let group = |label, layout: &wgpu::BindGroupLayout, bindings: &[u32]| {
            let entries: Vec<_> = bindings
                .iter()
                .map(|&binding| wgpu::BindGroupEntry {
                    binding,
                    resource: buffers[binding as usize].as_entire_binding(),
                })
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &entries,
            })
        };
        self.groups[at] = Some(ViewGroups {
            bound: buffers.map(Clone::clone),
            groups: [
                group("instance cull", &self.layouts[0], INSTANCE_CULL),
                group("cull finalize", &self.layouts[1], FINALIZE),
                group("section cull", &self.layouts[2], SECTION_CULL),
            ],
        });
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
