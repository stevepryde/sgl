//! The transparent stage's sprite batches (`Scene::add_sprite_batch`): the
//! camera's masked batches' depth and motion over the opaque depth (Frame
//! item 7), their colour at that depth and the blended batches' among the
//! blended surfaces (item 9), each one instanced draw. Each view orders a
//! blended batch's sprites back to front along the batch's facing normal
//! and uploads that order only when the batch or the view's eye or facing
//! changed. Nothing runs, and nothing is created, in a frame without
//! sprites; the pipelines are one program set in a `Compiled` slot (AR-13).
use crate::content::identity::SpriteBatchId;
use crate::content::sprite::SpriteFacing;
use crate::shading::gbuffer::{COLOR as HDR, DEPTH, MOTION};
use crate::shading::sprite::{facing_normal, group, order_key};
use crate::view::compile::{Compiled, Creation, Creator};
use crate::view::frame::FrameContext;
use crate::view::sprites::{ListedBatch, Mark};
use crate::view::targets::{composition_targets, mask_targets};
use glam::Mat4;
use std::collections::HashMap;

/// The sprite programs, under the unlit group 0.
pub(crate) static SPRITES: crate::shading::Module = crate::shading::Module {
    name: "sprites",
    source: sgl_post_fx::embed_wgsl!("sprites.wgsl"),
    deps: &[
        &crate::shading::BIND_UNLIT,
        &crate::shading::FRAME_FOG,
        &crate::shading::TEMPORAL_MASKS,
        &crate::shading::GBUFFER,
        &crate::shading::sprite::SPRITE,
    ],
};
pub(crate) const VS_ENTRY: &str = "sprite_vs";
pub(crate) const ORDERED_VS_ENTRY: &str = "sprite_ordered_vs";
pub(crate) const DEPTH_FS_ENTRY: &str = "sprite_depth_fs";
pub(crate) const MASKED_FS_ENTRY: &str = "sprite_masked_fs";
pub(crate) const MASKED_MASKED_FS_ENTRY: &str = "sprite_masked_masked_fs";
pub(crate) const BLENDED_FS_ENTRY: &str = "sprite_blended_fs";
pub(crate) const BLENDED_MASKED_FS_ENTRY: &str = "sprite_blended_masked_fs";
pub(crate) const FOOTPRINT_FS_ENTRY: &str = "sprite_footprint_fs";

/// What a masked batch's colour writes to the temporal masks.
#[derive(Clone, Copy)]
enum Marks {
    /// The frame has none.
    None,
    /// Nothing: an unchanged batch.
    Unmarked,
    /// Transparency and composition 1: a changed batch.
    Marked,
}

/// Every sprite pipeline, compiled together.
struct Programs {
    /// A masked batch's depth and motion.
    depth: wgpu::RenderPipeline,
    /// A masked batch's colour, by `Marks`.
    masked: [wgpu::RenderPipeline; 3],
    /// A blended batch's colour, without and with the temporal masks.
    blended: [wgpu::RenderPipeline; 2],
    /// A changed masked batch's last submitted sprites' quads, marking
    /// transparency and composition 1 and nothing else.
    footprint: wgpu::RenderPipeline,
}

impl Programs {
    fn new(creator: &Creator<'_>, unlit: &wgpu::BindGroupLayout) -> Creation<Self> {
        let device = creator.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprites"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&SPRITES]).into()),
        });
        let batch = crate::shading::sprite::batch_layout(device);
        let order = crate::shading::sprite::order_layout(device);
        let layout = |label, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let masked_layout = layout("masked sprites", &[Some(unlit), Some(&batch)]);
        let blended_layout = layout(
            "blended sprites",
            &[Some(unlit), Some(&batch), Some(&order)],
        );
        let [reactive, composition] = mask_targets();
        let [kept, marked] = composition_targets();
        let color = |blend| {
            Some(wgpu::ColorTargetState {
                format: HDR,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = |label,
                        layout: &wgpu::PipelineLayout,
                        (vertex, entry),
                        constants: &[(&str, f64)],
                        targets: &[Option<wgpu::ColorTargetState>],
                        depth_write| {
            let compilation_options = wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            };
            creator.render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    compilation_options: compilation_options.clone(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options,
                    targets,
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let premultiplied = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let depth = pipeline(
            "masked sprite depth and motion",
            &masked_layout,
            (VS_ENTRY, DEPTH_FS_ENTRY),
            &[],
            &[Some(wgpu::ColorTargetState {
                format: MOTION,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            true,
        );
        let masked = Creation::array([
            pipeline(
                "masked sprites",
                &masked_layout,
                (VS_ENTRY, MASKED_FS_ENTRY),
                &[],
                &[color(None)],
                false,
            ),
            pipeline(
                "masked sprites with temporal masks",
                &masked_layout,
                (VS_ENTRY, MASKED_MASKED_FS_ENTRY),
                &[],
                &[color(None), reactive.clone(), composition.clone()],
                false,
            ),
            pipeline(
                "changed masked sprites with temporal masks",
                &masked_layout,
                (VS_ENTRY, MASKED_MASKED_FS_ENTRY),
                &[("sprite_marks", 1.)],
                &[color(None), reactive.clone(), composition.clone()],
                false,
            ),
        ]);
        let blended = Creation::array([
            pipeline(
                "blended sprites",
                &blended_layout,
                (ORDERED_VS_ENTRY, BLENDED_FS_ENTRY),
                &[],
                &[color(premultiplied)],
                false,
            ),
            pipeline(
                "blended sprites with temporal masks",
                &blended_layout,
                (ORDERED_VS_ENTRY, BLENDED_MASKED_FS_ENTRY),
                &[],
                &[color(premultiplied), reactive, composition],
                false,
            ),
        ]);
        let footprint = pipeline(
            "masked sprites' last footprint",
            &masked_layout,
            (VS_ENTRY, FOOTPRINT_FS_ENTRY),
            &[],
            &[
                Some(wgpu::ColorTargetState {
                    format: HDR,
                    blend: None,
                    write_mask: wgpu::ColorWrites::empty(),
                }),
                kept,
                marked,
            ],
            false,
        );
        depth.zip(masked).zip(blended).zip(footprint).map(
            move |(((depth, masked), blended), footprint)| Self {
                depth,
                masked,
                blended,
                footprint,
            },
        )
    }
}

/// A view's order of a blended batch and what it was sorted for.
struct Order {
    key: OrderKey,
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
}

/// What a batch's order follows: its revision, the view's facing normal
/// and, where the order depends on it (`order_key`), the view's eye, bit
/// for bit.
#[derive(Clone, Copy, PartialEq, Eq)]
struct OrderKey {
    revision: (u64, u64),
    eye: [u32; 3],
    normal: [u32; 3],
}

#[derive(Default)]
pub(crate) struct Sprites {
    programs: Compiled<Programs>,
    /// The view's order of each blended batch it drew in its last frame.
    orders: HashMap<SpriteBatchId, Order>,
    order_layout: Option<wgpu::BindGroupLayout>,
    /// Kept between frames for its capacity.
    indices: Vec<(f32, u32)>,
}

impl Sprites {
    /// The programs, where the frame has them: none in a planning pass
    /// whose job is still queued, which leaves the draws out.
    fn programs(&mut self, ctx: &FrameContext<'_>) -> Option<&Programs> {
        let unlit = ctx.bindings.unlit.clone();
        self.programs
            .get(ctx.compiler, "sprites", move || {
                move |creator: &Creator<'_>| Programs::new(creator, &unlit)
            })
            .map(|programs| &*programs)
    }

    /// Orders each of the camera's blended batches back to front from its
    /// eye, re-sorting and uploading only a batch whose sprites or look, or
    /// the view's eye or facing normal, changed since this view last
    /// ordered it, and forgets the orders of batches it no longer draws.
    pub fn order(&mut self, ctx: &FrameContext<'_>) {
        let listed = ctx.views.sprites.blended();
        self.orders
            .retain(|id, _| listed.iter().any(|batch| batch.id == *id));
        if listed.is_empty() {
            return;
        }
        let view = Mat4::from_cols_array_2d(&ctx.values.view.view);
        let eye = ctx.input.camera.eye;
        let perspective = ctx.input.camera.projection.z_axis.w != 0.;
        for listed in listed {
            let batch = ctx
                .scene
                .sprites
                .batches
                .get(listed.id)
                .expect("a listed batch lives");
            let normal = facing_normal(&view, batch.look.facing);
            let eye_matters = perspective && matches!(batch.look.facing, SpriteFacing::Axis(_));
            let key = OrderKey {
                revision: batch.revision(),
                eye: if eye_matters {
                    eye.to_array().map(f32::to_bits)
                } else {
                    [0; 3]
                },
                normal: normal.to_array().map(f32::to_bits),
            };
            if self
                .orders
                .get(&listed.id)
                .is_some_and(|order| order.key == key)
            {
                continue;
            }
            self.indices.clear();
            self.indices
                .extend(batch.sprites().iter().enumerate().map(|(index, sprite)| {
                    let key = order_key(sprite.position, normal, eye, eye_matters);
                    (key, index as u32)
                }));
            self.indices.sort_by(|a, b| a.0.total_cmp(&b.0));
            let indices: Vec<u32> = self.indices.iter().map(|&(_, index)| index).collect();
            let bytes: &[u8] = bytemuck::cast_slice(&indices);
            let held = self.orders.remove(&listed.id);
            let (buffer, group) = match held {
                Some(order) if order.buffer.size() >= bytes.len() as u64 => {
                    (order.buffer, order.group)
                }
                held => {
                    // Grown geometrically, as the batch's records are.
                    let size = held.map_or(0, |order| order.buffer.size().saturating_mul(2));
                    let buffer = crate::counters::buffer(
                        ctx.device,
                        &wgpu::BufferDescriptor {
                            label: Some("sprite order"),
                            size: size.max(bytes.len() as u64),
                            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        },
                    );
                    let layout = self
                        .order_layout
                        .get_or_insert_with(|| crate::shading::sprite::order_layout(ctx.device));
                    let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("sprite order"),
                        layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: group::ORDER,
                            resource: buffer.as_entire_binding(),
                        }],
                    });
                    (buffer, group)
                }
            };
            crate::counters::write_buffer(ctx.queue, &buffer, 0, bytes);
            self.orders.insert(listed.id, Order { key, buffer, group });
        }
    }

    /// Frame item 7: the camera's masked batches' depth over the opaque
    /// depth, tested nearer or equal and written, and their unjittered
    /// motion into the G-buffer's. Returns whether it drew.
    pub fn encode_depth(&mut self, ctx: &mut FrameContext<'_>) -> bool {
        if ctx.views.sprites.masked().is_empty() {
            return false;
        }
        let Some(programs) = self.programs(ctx) else {
            return false;
        };
        let targets = ctx.targets;
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("masked sprite depth and motion"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.motion,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: ctx.timing.and_then(|t| t.render_pass("sprites")),
            ..Default::default()
        });
        pass.set_pipeline(&programs.depth);
        pass.set_bind_group(0, ctx.bindings.camera_unlit(), &[]);
        for listed in ctx.views.sprites.masked() {
            draw(&mut pass, ctx.scene, listed, None);
        }
        true
    }

    /// Frame item 9's masked colour: the camera's masked batches onto
    /// `beauty` at the depth `encode_depth` wrote, tested and not written,
    /// and onto the `temporal_masks` where the frame has them, which this
    /// pass clears unless they were `cleared`: a changed batch's pixels, and
    /// the quads of the last submitted frame's sprites of each batch the
    /// view lists as a footprint. Returns whether it drew.
    pub fn encode_masked(
        &mut self,
        ctx: &mut FrameContext<'_>,
        beauty: &wgpu::TextureView,
        temporal_masks: Option<&[wgpu::TextureView; 2]>,
        cleared: bool,
    ) -> bool {
        let footprints = &ctx.views.sprites.footprints;
        if ctx.views.sprites.masked().is_empty()
            && (temporal_masks.is_none() || footprints.is_empty())
        {
            return false;
        }
        let Some(programs) = self.programs(ctx) else {
            return false;
        };
        let load = |view, load: bool| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if load {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    },
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let mut attachments = vec![load(beauty, true)];
        attachments.extend(
            temporal_masks
                .into_iter()
                .flatten()
                .map(|mask| load(mask, cleared)),
        );
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("masked sprites"),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &ctx.targets.depth,
                depth_ops: None,
                stencil_ops: None,
            }),
            timestamp_writes: ctx.timing.and_then(|t| t.render_pass("sprites")),
            ..Default::default()
        });
        pass.set_bind_group(0, ctx.bindings.camera_unlit(), &[]);
        for listed in ctx.views.sprites.masked() {
            let marks = match temporal_masks {
                None => Marks::None,
                Some(_) if batch_changed(ctx.scene, listed) => Marks::Marked,
                Some(_) => Marks::Unmarked,
            };
            pass.set_pipeline(&programs.masked[marks as usize]);
            draw(&mut pass, ctx.scene, listed, None);
        }
        // Where changed batches' sprites stood in the last submitted frame,
        // their cut-out texels included, whether or not the batch draws now
        // (cleared, or moved out of view), and what removed or restyled
        // batches drew: what they may have left.
        if temporal_masks.is_some() {
            pass.set_pipeline(&programs.footprint);
            for mark in footprints {
                let (group, count) = match *mark {
                    Mark::Previous(id) => {
                        let batch = ctx.scene.sprites.batches.get(id);
                        let Some(previous) = batch.and_then(|batch| batch.previous()) else {
                            continue;
                        };
                        (previous.group, previous.count)
                    }
                    Mark::Departed(index) => {
                        let (footprint, _) = &ctx.scene.sprites.departed[index];
                        (&footprint.group, footprint.count)
                    }
                };
                pass.set_bind_group(1, group, &[]);
                pass.draw(0..6, 0..count);
            }
        }
        true
    }

    /// Whether the blended batches can be drawn this frame: the camera has
    /// some and the programs are ready, compiled here where a frame needs
    /// them (none in a planning pass whose job is still queued).
    pub fn ready_blended(&mut self, ctx: &FrameContext<'_>) -> bool {
        !ctx.views.sprites.blended().is_empty() && self.programs(ctx).is_some()
    }

    /// Draws blended batch `listed` in the blended pass `pass`, with the
    /// temporal masks where `masked`, under `camera_unlit`, leaving the
    /// geometry draws' bindings for the caller to restore. Nothing unless
    /// `ready_blended` said so.
    pub fn draw_blended(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &crate::Scene,
        camera_unlit: &wgpu::BindGroup,
        listed: &ListedBatch,
        masked: bool,
    ) {
        let (Some(order), Some(programs)) = (self.orders.get(&listed.id), self.programs.ready())
        else {
            return;
        };
        pass.set_pipeline(&programs.blended[usize::from(masked)]);
        pass.set_bind_group(0, camera_unlit, &[]);
        draw(pass, scene, listed, Some(&order.group));
    }
}

/// Whether `listed`'s batch changed since the last submitted frame.
pub(super) fn batch_changed(scene: &crate::Scene, listed: &ListedBatch) -> bool {
    scene
        .sprites
        .batches
        .get(listed.id)
        .is_some_and(|batch| batch.changed())
}

/// One instanced draw of `listed`'s sprites, six vertices each, through
/// `order` for a blended batch.
fn draw(
    pass: &mut wgpu::RenderPass<'_>,
    scene: &crate::Scene,
    listed: &ListedBatch,
    order: Option<&wgpu::BindGroup>,
) {
    let batch = scene
        .sprites
        .batches
        .get(listed.id)
        .expect("a listed batch lives");
    let group = batch.group().expect("a listed batch has its group");
    pass.set_bind_group(1, group, &[]);
    if let Some(order) = order {
        pass.set_bind_group(2, order, &[]);
    }
    pass.draw(0..6, 0..batch.sprites().len() as u32);
}
