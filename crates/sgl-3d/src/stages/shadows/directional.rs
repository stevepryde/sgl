//! The directional light's shadow cascades: one layer of a depth array per
//! cascade, each drawn from its view's GPU-built draw list. A probe capture
//! draws its own cascades, CPU-built, into the same layers.
use crate::Scene;
use crate::view::cascades::MAX_SHADOW_CASCADES;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::frame::FrameContext;
use crate::view::pipelines::{GeometryPass, GeometryPipelines};

/// Each cascade's timing group, nearest first.
const TIMING: [&str; MAX_SHADOW_CASCADES] = [
    "directional shadow cascade 0",
    "directional shadow cascade 1",
    "directional shadow cascade 2",
    "directional shadow cascade 3",
];

pub(crate) struct Directional {
    /// Every cascade's layer, as lit group 0 samples them.
    pub array: wgpu::TextureView,
    /// Each cascade's layer, nearest first.
    pub layers: [wgpu::TextureView; MAX_SHADOW_CASCADES],
}

impl Directional {
    /// Cascades of `size` texels.
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("directional shadow cascades"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: MAX_SHADOW_CASCADES as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            array: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
            layers: std::array::from_fn(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer as u32,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            }),
        }
    }

    /// Each cascade's size in texels.
    pub fn size(&self) -> u32 {
        self.array.texture().width()
    }

    /// Each of the frame's cascades from its view and draw list.
    pub fn encode(&self, ctx: &mut FrameContext<'_>) {
        let count = ctx.views.cascade_count;
        for (index, slot) in ctx.views.cascades[..count].iter().enumerate() {
            let started = crate::counters::Moment::now();
            let mut pass = begin(
                ctx.encoder,
                &self.layers[index],
                ctx.timing.and_then(|t| t.render_pass(TIMING[index])),
            );
            pass.set_bind_group(0, ctx.bindings.cascade(index), &[]);
            slot.list.draw(
                ctx.scene,
                ctx.pipelines,
                &mut pass,
                GeometryPass::DirectionalShadow,
            );
            drop(pass);
            slot.recorded_since(started);
        }
    }

    /// A probe capture's cascades: `casters` drawn under each of `groups`,
    /// one per cascade, nearest first.
    pub fn encode_capture(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        pipelines: &GeometryPipelines,
        (casters, drawn): (&DrawList, &DrawInstances),
        groups: &[wgpu::BindGroup],
    ) {
        for (layer, group) in self.layers.iter().zip(groups) {
            let mut pass = begin(encoder, layer, None);
            pass.set_bind_group(0, group, &[]);
            casters.draw(
                scene,
                pipelines,
                drawn,
                &mut pass,
                GeometryPass::CaptureShadow,
            );
        }
    }
}

/// A depth-only pass that clears `layer`.
fn begin<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    layer: &wgpu::TextureView,
    timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'_>>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("directional shadow cascade"),
        color_attachments: &[],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: layer,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod casters_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
