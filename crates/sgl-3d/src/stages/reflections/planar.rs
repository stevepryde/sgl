//! Planar reflections' composite (the architecture's Reflections): the
//! frame's method result with each planar reflector's picture written over
//! the reflector's surfaces, which composition then takes as it takes a
//! screen-space method's. The pictures are the view's reflected views'
//! composites, rendered earlier in the frame (`renderer::reflected`).
use crate::content::identity::PlanarReflectorId;
use crate::shading;
use crate::shading::gbuffer;
use crate::view::cached_group::CachedGroup;
use crate::view::compile::{Compiled, SharedModule};
use crate::view::frame::FrameContext;
use crate::view::pipelines::GeometryPass;
use crate::view::reflected_camera::{self, Rect};
use crate::view::targets::{MASK_FORMAT, target};
use glam::{Mat4, Vec4};

static PLANAR: shading::Module = shading::Module {
    name: "planar_reflections",
    source: sgl_post_fx::embed_wgsl!("planar.wgsl"),
    deps: &[&shading::GBUFFER, &shading::FULLSCREEN_VS],
};
const PICTURE_ENTRY: &str = "planar_fs";
const METHOD_ENTRY: &str = "planar_method_fs";

/// A planar reflector's picture for the view that shows it: the reflected
/// view's composite, scene-referred radiance before exposure, and its
/// depth, with where a world point lies in them.
pub(crate) struct Picture {
    pub reflector: PlanarReflectorId,
    pub color: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    /// The reflected view's unclipped projection times its view.
    pub picture_from_world: Mat4,
    /// From the picture's clip space, with its device depth, to world: the
    /// inverse of the reflected view's raster projection times its view.
    pub world_from_picture: Mat4,
    /// The reflector's plane (unit normal, d).
    pub plane: Vec4,
    /// The bounds of the reflector's surfaces in the viewer's image.
    pub bounds: Rect,
}

/// `PlanarParameters` in planar.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct PlanarParameters {
    world_from_clip: [[f32; 4]; 4],
    picture_from_world: [[f32; 4]; 4],
    world_from_picture: [[f32; 4]; 4],
    plane: [f32; 4],
    eye: [f32; 3],
    padding: f32,
}

/// One of the stage's fragment pipelines.
fn pipeline(
    device: &wgpu::Device,
    shader: &SharedModule,
    entry: &str,
    label: &str,
) -> wgpu::RenderPipeline {
    let shader = shader.get(device);
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: None,
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(shading::FULLSCREEN_VS_ENTRY),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(gbuffer::COLOR.into())],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// One picture a frame shows, in the order it shows them: its parameters
/// and its group, kept from frame to frame.
struct Slot {
    parameters: wgpu::Buffer,
    group: CachedGroup,
}

/// The planar method, held by a view's reflections while a frame of it
/// shows a planar reflector.
///
/// Reads: the opaque depth, the surface depth, the G-buffer's normal and
/// material, the camera's opaque sets of each reflector's materials, each
/// picture and its depth, the screen-space method's result.
/// Writes: its result, the frame's method result, and its coverage mask.
/// Timing group: `planar reflection`.
/// History: none.
pub(crate) struct Planar {
    picture: Compiled<wgpu::RenderPipeline>,
    /// Copies the screen-space method's result, from the first frame one
    /// runs beside a picture, and its group.
    method: Compiled<wgpu::RenderPipeline>,
    method_group: Option<CachedGroup>,
    shader: SharedModule,
    sampler: wgpu::Sampler,
    /// Its targets, in frames that show a picture: released while the
    /// view only keeps its reflected views, which take a fraction of a
    /// millisecond to make again where the pipelines take milliseconds.
    targets: Option<Targets>,
    slots: Vec<Slot>,
}

/// The planar method's targets, at the view's render size.
struct Targets {
    size: [u32; 2],
    /// The frame's method result: the screen-space method's, or nothing,
    /// with each picture over its reflector's surfaces.
    result: wgpu::TextureView,
    /// The coverage of one reflector's surfaces, and the attachment the
    /// pipeline that marks it keeps (`composition_targets`).
    coverage: [wgpu::TextureView; 2],
}

impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        Self {
            size,
            result: target(device, "planar reflection result", size, gbuffer::COLOR),
            coverage: ["planar reflection kept mask", "planar reflection coverage"]
                .map(|label| target(device, label, size, MASK_FORMAT)),
        }
    }
}

impl Planar {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            picture: Compiled::default(),
            method: Compiled::default(),
            method_group: None,
            shader: SharedModule::composed("planar reflections", &PLANAR),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("planar reflection picture"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            targets: None,
            slots: Vec::new(),
        }
    }

    /// Releases its targets, and the groups that bind them, keeping its
    /// pipelines: the view shows no picture in this frame.
    pub fn release_targets(&mut self) {
        self.targets = None;
        for slot in &mut self.slots {
            slot.group.forget();
        }
        if let Some(group) = &mut self.method_group {
            group.forget();
        }
    }

    /// The frame's method result: `method`, a screen-space method's result
    /// at the render size, or nothing, with each of `pictures` written over
    /// its reflector's surfaces. Per picture: the camera's opaque sets of
    /// the materials that name its reflector, drawn again at the opaque
    /// depth, tested equal, into the coverage mask (the temporal
    /// composition's pipeline, `GeometryPass::TemporalComposition`), then
    /// one pass over the reflector's bounds on screen. A planning pass
    /// (`Renderer::compile`) whose pipelines are compiling draws the
    /// coverage, which asks for its geometry pipelines, and no picture.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        pictures: &[Picture],
        method: Option<&wgpu::TextureView>,
    ) -> wgpu::TextureView {
        let device = ctx.device;
        let size = ctx.sizes.render;
        if self
            .targets
            .as_ref()
            .is_none_or(|targets| targets.size != size)
        {
            self.targets = Some(Targets::new(device, size));
        }
        let shader = &self.shader;
        let picture_pipeline = self
            .picture
            .get(ctx.compiler, "planar reflection", || {
                let shader = shader.clone();
                move |device: &wgpu::Device| {
                    pipeline(device, &shader, PICTURE_ENTRY, "planar reflection")
                }
            })
            .cloned();
        let method_pipeline = match method {
            Some(_) => self
                .method
                .get(ctx.compiler, "planar reflection method", || {
                    let shader = shader.clone();
                    move |device: &wgpu::Device| {
                        pipeline(device, &shader, METHOD_ENTRY, "planar reflection method")
                    }
                })
                .cloned(),
            None => None,
        };
        while let Some(picture) = picture_pipeline
            .as_ref()
            .filter(|_| self.slots.len() < pictures.len())
        {
            self.slots.push(Slot {
                parameters: crate::counters::buffer(
                    device,
                    &wgpu::BufferDescriptor {
                        label: Some("planar reflection parameters"),
                        size: size_of::<PlanarParameters>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                ),
                group: CachedGroup::new(picture.get_bind_group_layout(0)),
            });
        }
        // A picture the frame no longer shows lets go of its views.
        for slot in self.slots.iter_mut().skip(pictures.len()) {
            slot.group.forget();
        }
        let texture = wgpu::BindingResource::TextureView;
        let camera = ctx.views.reflection_camera;
        let scene = ctx.scene;
        let targets = self.targets.as_ref().expect("the targets were made");
        let [width, height] = targets.size;
        for (index, picture) in pictures.iter().enumerate() {
            let mut coverage = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("planar reflector coverage"),
                color_attachments: &targets.coverage.each_ref().map(|view| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                }),
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &ctx.targets.depth,
                    depth_ops: None,
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("planar reflection")),
                ..Default::default()
            });
            coverage.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
            ctx.views.camera.list.draw_materials(
                scene,
                ctx.pipelines,
                &mut coverage,
                GeometryPass::TemporalComposition { footprint: false },
                &|material| {
                    scene.drawn_material(material).values.planar_reflector
                        == Some(picture.reflector)
                },
            );
            drop(coverage);
            let Some(picture_pipeline) = &picture_pipeline else {
                continue;
            };
            crate::counters::write_buffer(
                ctx.queue,
                &self.slots[index].parameters,
                0,
                bytemuck::bytes_of(&PlanarParameters {
                    world_from_clip: camera.inverse_view_proj,
                    picture_from_world: picture.picture_from_world.to_cols_array_2d(),
                    world_from_picture: picture.world_from_picture.to_cols_array_2d(),
                    plane: picture.plane.to_array(),
                    eye: [
                        camera.camera_position[0],
                        camera.camera_position[1],
                        camera.camera_position[2],
                    ],
                    padding: 0.,
                }),
            );
            // The frame's result starts from the method's, or from nothing.
            let first = index == 0;
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("planar reflection"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.result,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if first {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("planar reflection")),
                ..Default::default()
            });
            if let Some((method, copy)) = method.zip(method_pipeline.as_ref()).filter(|_| first) {
                let group = self
                    .method_group
                    .get_or_insert_with(|| CachedGroup::new(copy.get_bind_group_layout(0)));
                let group = group.get(device, "planar reflection method", &[(9, texture(method))]);
                pass.set_pipeline(copy);
                pass.set_bind_group(0, group, &[]);
                pass.draw(0..3, 0..1);
            }
            let Slot { parameters, group } = &mut self.slots[index];
            let resources = [
                (0, parameters.as_entire_binding()),
                (1, texture(&targets.coverage[1])),
                (2, texture(&ctx.targets.depth)),
                (3, texture(ctx.surface.depth)),
                (4, texture(&ctx.targets.normal)),
                (5, texture(&ctx.targets.material)),
                (6, texture(&picture.color)),
                (7, texture(&picture.depth)),
                (8, wgpu::BindingResource::Sampler(&self.sampler)),
            ];
            let group = group.get(device, "planar reflection", &resources);
            // The reflector's bounds on screen, a texel wider.
            let [(x, columns), (y, rows)] =
                reflected_camera::texels(picture.bounds, [width, height]);
            pass.set_scissor_rect(x, y, columns, rows);
            pass.set_pipeline(picture_pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
        targets.result.clone()
    }
}
