//! Cull, the GPU draw lists' builder (the architecture's "GPU draw lists
//! and occlusion culling"): prepare's last GPU step, after the deform pass
//! and the acceleration-structure builds, runs the early phase for every
//! GPU-built view, the camera's opaque and masked surfaces and each
//! directional cascade's casters: the instance cull over the scene's draw
//! candidates, its finalize, then the section cull over the candidates it
//! passed, appending their sections to their sets' draws (cull.wgsl).
//! While occlusion culling runs, the renderer encodes its other two parts
//! between the opaque stage's G-buffer passes: the late phase
//! (`encode_late`: the depth pyramid from the early set's depth, then the
//! late instance cull, its finalize and the late section cull) and the
//! pyramid again from the complete depth (`encode_pyramid`), which the next
//! frame's early phase tests against. It owns its pipelines, their layouts,
//! the bind groups over what the renderer lends it and the camera's depth
//! pyramid, its history, and nothing a pass draws from: each view's lists
//! are the view's (`view::draw_list::gpu`).
//!
//! Reads: the scene's draw candidates, sets, level chains, object records
//! (their poses and previous poses) and ray source (its section tables),
//! each GPU-built view's cull, the camera history's matrices and jitter,
//! and the opaque depth. Writes: each GPU-built view's lists, dispatch,
//! cluster lists and draws; its depth pyramid. Honours: the culling layer
//! and the visibility mask, through each view's cull; occlusion culling,
//! through whether the camera's list culls a late phase. History: the
//! pyramid, the last submitted frame's build; a frame whose camera history
//! restarted, or whose last submitted frame built none, tests no occlusion
//! early. Timing groups: `cull`, `depth pyramid`, `cull late`.
pub(crate) mod pyramid;

use crate::shading::Module;
use crate::shading::culling::{
    CullOcclusion, DISPATCH_EARLY_SECTIONS, DISPATCH_LATE_INSTANCES, DISPATCH_LATE_SECTIONS,
    OCCLUSION_EARLY,
};
use crate::view::FrameViews;
use crate::view::draw_list::gpu::CullBuffers;
use crate::view::frame::FrameContext;
use pyramid::{Builder, Pyramid, SUPPORTED_STORAGE_TEXTURES};

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
/// The entry points the cull's pipelines are created with: each phase's
/// instance test, finalisation and section test.
pub(crate) const CULL_INSTANCES_ENTRY: &str = "cull_instances";
pub(crate) const CULL_FINALIZE_ENTRY: &str = "cull_finalize";
pub(crate) const CULL_SECTIONS_ENTRY: &str = "cull_sections";
pub(crate) const CULL_INSTANCES_LATE_ENTRY: &str = "cull_instances_late";
pub(crate) const CULL_FINALIZE_LATE_ENTRY: &str = "cull_finalize_late";
pub(crate) const CULL_SECTIONS_LATE_ENTRY: &str = "cull_sections_late";

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
    pub const PYRAMID: u32 = 10;
    pub const OCCLUSION: u32 = 11;
}

/// Each layout's bindings: what it reads, then what it writes. The
/// instance culls and the finalizes share theirs, each phase's pipeline
/// binding what it reads; the section culls never hold the dispatch they
/// are driven by, nor do the instance culls, since wgpu tracks a buffer
/// whole and refuses an indirect dispatch's source as its writable storage.
const INSTANCE_CULL: &[u32] = &[
    binding::VIEW,
    binding::CANDIDATES,
    binding::CHAINS,
    binding::OBJECTS,
    binding::SETS,
    binding::LISTS,
    binding::PYRAMID,
    binding::OCCLUSION,
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
    binding::PYRAMID,
    binding::OCCLUSION,
];

/// The pipelines, by the entry point each runs.
mod pipeline {
    pub const INSTANCES: usize = 0;
    pub const FINALIZE: usize = 1;
    pub const SECTIONS: usize = 2;
    pub const INSTANCES_LATE: usize = 3;
    pub const FINALIZE_LATE: usize = 4;
    pub const SECTIONS_LATE: usize = 5;
}

/// A view's bind groups (the instance culls', the finalizes', the early
/// section cull's and the late section cull's, which binds the late
/// cluster list) with what they bind.
struct ViewGroups {
    bound: [wgpu::Buffer; 11],
    pyramid: wgpu::TextureView,
    groups: [wgpu::BindGroup; 4],
}

/// What occlusion culling needs: the pyramid's builder and the pyramid.
struct Occlusion {
    builder: Builder,
    /// The camera's pyramid, made for the render size on the first frame
    /// that culls occlusion.
    pyramid: Option<Pyramid>,
}

pub(crate) struct Cull {
    /// By `pipeline`'s indices.
    pipelines: [wgpu::ComputePipeline; 6],
    layouts: [wgpu::BindGroupLayout; 3],
    /// The camera's groups, then each cascade's.
    groups: Vec<Option<ViewGroups>>,
    /// The camera's `CullOcclusion`, and one that tests nothing, which the
    /// cascades bind.
    occlusion_uniform: wgpu::Buffer,
    no_occlusion: wgpu::Buffer,
    /// A 1×1 pyramid that a view without one binds and never reads.
    no_pyramid: wgpu::TextureView,
    /// None on a device without the pyramid's storage textures.
    occlusion: Option<Occlusion>,
    /// Whether this frame built the pyramid for the next, and whether the
    /// last submitted frame did, which `finish_frame` commits.
    frame_builds: bool,
    submitted_build: bool,
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
                        binding::VIEW | binding::OCCLUSION => wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        binding::PYRAMID => wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
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
        let occlusion_buffer = |label| {
            crate::counters::buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytemuck::bytes_of(&<CullOcclusion as bytemuck::Zeroable>::zeroed()),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                },
            )
        };
        let no_pyramid = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("no depth pyramid"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let supported =
            device.limits().max_storage_textures_per_shader_stage >= SUPPORTED_STORAGE_TEXTURES;
        Self {
            pipelines: [
                pipeline(&layouts[0], CULL_INSTANCES_ENTRY),
                pipeline(&layouts[1], CULL_FINALIZE_ENTRY),
                pipeline(&layouts[2], CULL_SECTIONS_ENTRY),
                pipeline(&layouts[0], CULL_INSTANCES_LATE_ENTRY),
                pipeline(&layouts[1], CULL_FINALIZE_LATE_ENTRY),
                pipeline(&layouts[2], CULL_SECTIONS_LATE_ENTRY),
            ],
            layouts,
            groups: Vec::new(),
            occlusion_uniform: occlusion_buffer("cull occlusion"),
            no_occlusion: occlusion_buffer("no cull occlusion"),
            no_pyramid,
            occlusion: supported.then(|| Occlusion {
                builder: Builder::new(device),
                pyramid: None,
            }),
            frame_builds: false,
            submitted_build: false,
        }
    }

    /// Whether the device binds the pyramid's storage textures, without
    /// which occlusion culling does not run.
    pub fn occlusion_supported(&self) -> bool {
        self.occlusion.is_some()
    }

    /// The early phase for every GPU-built view of the frame: each view's
    /// lists reset in the frame's encoder, then, in one compute pass, its
    /// instance cull, finalize and section cull. The camera's tests
    /// occlusion against the pyramid where its list culls a late phase,
    /// the camera history continues and the last submitted frame built the
    /// pyramid.
    pub fn encode_early(&mut self, ctx: &mut FrameContext<'_>) {
        self.frame_builds = false;
        let late = ctx.views.camera.list.late();
        let mut occlusion = <CullOcclusion as bytemuck::Zeroable>::zeroed();
        if let Some(state) = self.occlusion.as_mut().filter(|_| late) {
            let size = ctx.sizes.render;
            if state
                .pyramid
                .as_ref()
                .is_none_or(|pyramid| !Builder::fits(pyramid, size))
            {
                state.pyramid = Some(state.builder.pyramid(ctx.device, size));
                self.submitted_build = false;
            }
            let pyramid = state.pyramid.as_ref().unwrap();
            occlusion.current = ctx
                .history
                .camera
                .jittered_view_projection()
                .to_cols_array_2d();
            occlusion.levels = pyramid.complete();
            if let Some(previous) = ctx.history.previous_camera
                && self.submitted_build
            {
                occlusion.previous = previous.jittered_view_projection().to_cols_array_2d();
                occlusion.flags = OCCLUSION_EARLY;
            }
        }
        crate::counters::write_buffer(
            ctx.queue,
            &self.occlusion_uniform,
            0,
            bytemuck::bytes_of(&occlusion),
        );
        self.encode(ctx.device, ctx.encoder, ctx.scene, ctx.views, ctx.timing);
    }

    /// The late phase, after the G-buffer pass over the early set: the
    /// pyramid from its depth, then, in one compute pass, the camera's late
    /// instance cull over the early phase's late list, its finalize and the
    /// late section cull over the candidates it passed and the late section
    /// queue, appending to the late set. Nothing where the camera's list
    /// culls no late phase.
    pub fn encode_late(&mut self, ctx: &mut FrameContext<'_>) {
        let list = &ctx.views.camera.list;
        if !list.late() || !list.culls() {
            return;
        }
        self.encode_pyramid_build(ctx);
        let Some(groups) = self.groups.first().and_then(Option::as_ref) else {
            return;
        };
        let started = crate::counters::Moment::now();
        let dispatch = list.buffers().dispatch;
        let mut pass = ctx
            .encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cull late"),
                timestamp_writes: ctx
                    .timing
                    .and_then(|timing| timing.compute_pass("cull late")),
            });
        pass.set_pipeline(&self.pipelines[pipeline::INSTANCES_LATE]);
        pass.set_bind_group(0, &groups.groups[0], &[]);
        pass.dispatch_workgroups_indirect(dispatch, u64::from(DISPATCH_LATE_INSTANCES) * 4);
        pass.set_pipeline(&self.pipelines[pipeline::FINALIZE_LATE]);
        pass.set_bind_group(0, &groups.groups[1], &[]);
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(&self.pipelines[pipeline::SECTIONS_LATE]);
        pass.set_bind_group(0, &groups.groups[3], &[]);
        pass.dispatch_workgroups_indirect(dispatch, u64::from(DISPATCH_LATE_SECTIONS) * 4);
        drop(pass);
        ctx.views.camera.culled_since(started);
    }

    /// The pyramid again, from the complete opaque depth after the G-buffer
    /// pass over the late set, for the next frame's early phase. Nothing
    /// where the camera's list culls no late phase.
    pub fn encode_pyramid(&mut self, ctx: &mut FrameContext<'_>) {
        if !ctx.views.camera.list.late() {
            return;
        }
        self.frame_builds |= self.encode_pyramid_build(ctx);
    }

    /// Builds the pyramid from the opaque depth; whether there is one.
    fn encode_pyramid_build(&mut self, ctx: &mut FrameContext<'_>) -> bool {
        let Some(Occlusion {
            builder,
            pyramid: Some(pyramid),
        }) = self.occlusion.as_mut()
        else {
            return false;
        };
        builder.encode(
            ctx.device,
            ctx.encoder,
            pyramid,
            &ctx.targets.depth,
            ctx.timing,
        );
        true
    }

    /// After the caller submitted the last rendered frame: its pyramid, if
    /// it built one, is what the next frame's early phase reads.
    pub fn finish_frame(&mut self) {
        self.submitted_build = std::mem::take(&mut self.frame_builds);
    }

    /// The camera's pyramid, which tests read back.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn pyramid(&self) -> Option<&wgpu::Texture> {
        self.occlusion
            .as_ref()
            .and_then(|occlusion| occlusion.pyramid.as_ref())
            .map(Pyramid::texture)
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
            pass.set_pipeline(&self.pipelines[pipeline::INSTANCES]);
            pass.set_bind_group(0, &groups[0], &[]);
            pass.dispatch_workgroups(x, y, 1);
            pass.set_pipeline(&self.pipelines[pipeline::FINALIZE]);
            pass.set_bind_group(0, &groups[1], &[]);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.pipelines[pipeline::SECTIONS]);
            pass.set_bind_group(0, &groups[2], &[]);
            pass.dispatch_workgroups_indirect(
                slot.list.buffers().dispatch,
                u64::from(DISPATCH_EARLY_SECTIONS) * 4,
            );
            slot.culled_since(started);
        }
    }

    /// Creates view `at`'s groups over `scene`'s buffers and `view`'s
    /// unless they bind them already. The camera (view 0) binds its
    /// occlusion test and the pyramid, where there is one; a cascade binds
    /// an occlusion test that tests nothing and a stand-in pyramid.
    fn prepare_groups(
        &mut self,
        device: &wgpu::Device,
        scene: &crate::Scene,
        at: usize,
        view: CullBuffers<'_>,
    ) {
        let [candidates, sets, chains] = scene.candidates.buffers();
        let camera = at == 0;
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
            view.late_regions,
        ];
        let occlusion = if camera {
            &self.occlusion_uniform
        } else {
            &self.no_occlusion
        };
        let pyramid = self
            .occlusion
            .as_ref()
            .and_then(|occlusion| occlusion.pyramid.as_ref())
            .filter(|_| camera)
            .map_or(&self.no_pyramid, Pyramid::view);
        if self.groups[at]
            .as_ref()
            .is_some_and(|groups| groups.bound.iter().eq(buffers) && groups.pyramid == *pyramid)
        {
            return;
        }
        let group = |label, layout: &wgpu::BindGroupLayout, bindings: &[u32], late: bool| {
            let entries: Vec<_> = bindings
                .iter()
                .map(|&binding| wgpu::BindGroupEntry {
                    binding,
                    resource: match binding {
                        binding::PYRAMID => wgpu::BindingResource::TextureView(pyramid),
                        binding::OCCLUSION => occlusion.as_entire_binding(),
                        binding::REGIONS if late => view.late_regions.as_entire_binding(),
                        _ => buffers[binding as usize].as_entire_binding(),
                    },
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
            pyramid: pyramid.clone(),
            groups: [
                group("instance cull", &self.layouts[0], INSTANCE_CULL, false),
                group("cull finalize", &self.layouts[1], FINALIZE, false),
                group("section cull", &self.layouts[2], SECTION_CULL, false),
                group("late section cull", &self.layouts[2], SECTION_CULL, true),
            ],
        });
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod occlusion_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
