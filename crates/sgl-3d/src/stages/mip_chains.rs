//! Mip chains, part of the frame's scene work: builds the levels below the
//! first of each decoded image's texture added since the last time it ran,
//! a render pass a level that draws the level from the one above in the
//! texture's own format (the architecture's "Material mip chains"), as
//! three.js r187's WebGPU backend builds a texture's mipmaps
//! (src/renderers/webgpu/utils/WebGPUTexturePassUtils.js, `generateMipmaps`;
//! MIT, see `stages/post/smaa/LICENSE-three.txt`) and Filament ef1a133's
//! (filament/backend/src/webgpu/WebGPURenderPassMipmapGenerator.cpp;
//! Apache-2.0, see `LICENSE-filament.txt`). The practice is theirs; the
//! shader and its footprint weights are SGL3D's own.
//!
//! It runs before any view of a frame draws and at the start of a probe
//! capture, so no draw samples a texture without its chain, and submits
//! its passes itself through the queue, never in a frame's encoder: an
//! abandoned frame or a planning pass loses no chain.
//!
//! Reads: the scene's waiting textures (`scene::textures::Chains`).
//! Writes: their levels below the first. Honours: nothing. Timing group:
//! "mip chains", in the frame's (or live probe refresh's) timing, one span
//! from its first pass's start to its last pass's end, since a scene's
//! chains can take more passes than a frame times.
use crate::scene::textures::Chains;
use crate::view::compile::{Compiled, Compiler};
use crate::view::targets::attachment;

pub(crate) static MIP_CHAIN: crate::shading::Module = crate::shading::Module {
    name: "mip_chain",
    source: sgl_post_fx::embed_wgsl!("mip_chains.wgsl"),
    deps: &[&crate::shading::FULLSCREEN_VS],
};
/// The entry points the stage's pipelines are created with, beside
/// `shading::FULLSCREEN_VS_ENTRY`: one texture's level, and the levels of
/// `BATCH` textures.
pub(crate) const MIP_CHAIN_FS_ENTRY: &str = "mip_chain_fs";
pub(crate) const MIP_CHAIN_BATCH_FS_ENTRY: &str = "mip_chain_batch_fs";

/// The textures of one size and format whose levels one pass draws, each
/// into a colour target of its own: as many as WebGPU's default 32
/// `maxColorAttachmentBytesPerSample` holds at 8 bytes an RGBA8 target
/// (the specification's render target pixel byte cost), so every device
/// at S3D-1's floor runs it. A render pass is the costly unit of this
/// stage: on a tiled GPU's driver each takes its own pooled buffers
/// (Metal: about 100 microseconds a pass the first time), and a level's
/// load brings hundreds.
const BATCH: usize = 4;

/// The formats a decoded image's texture has: sampled as linear data, and
/// as sRGB colour.
const FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Rgba8Unorm,
    wgpu::TextureFormat::Rgba8UnormSrgb,
];

#[derive(Default)]
pub(crate) struct MipChains {
    programs: Compiled<Programs>,
}

/// The stage's programs: for each of `FORMATS`, a pipeline that draws one
/// texture's level and one that draws `BATCH` textures' (`FORMATS`' order).
/// They differ by their targets and fragment entry alone.
struct Programs {
    /// The levels above, `BATCH` of them: a pass of one texture binds its
    /// level to each.
    layout: wgpu::BindGroupLayout,
    single: [wgpu::RenderPipeline; 2],
    batch: [wgpu::RenderPipeline; 2],
}

impl Programs {
    fn new(device: &wgpu::Device) -> Self {
        let entries: [_; BATCH] = std::array::from_fn(|binding| wgpu::BindGroupLayoutEntry {
            binding: binding as u32,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mip chain levels above"),
            entries: &entries,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mip chain"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&MIP_CHAIN]).into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mip chain"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |format: wgpu::TextureFormat, targets: usize, fragment| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mip chain"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(crate::shading::FULLSCREEN_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &vec![Some(format.into()); targets],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            single: FORMATS.map(|format| pipeline(format, 1, MIP_CHAIN_FS_ENTRY)),
            batch: FORMATS.map(|format| pipeline(format, BATCH, MIP_CHAIN_BATCH_FS_ENTRY)),
            layout,
        }
    }

    /// Draws every level below the first of `textures`, each from the
    /// level above it: one texture, or `BATCH` of one format and size.
    fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        textures: &[wgpu::Texture],
        span: &mut Span<'_>,
    ) {
        let pipelines = match textures.len() {
            1 => &self.single,
            _ => &self.batch,
        };
        let pipeline = &pipelines[format_index(&textures[0])];
        let levels = |level| -> Vec<wgpu::TextureView> {
            textures
                .iter()
                .map(|texture| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        base_mip_level: level,
                        mip_level_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect()
        };
        let mut above = levels(0);
        for level in 1..textures[0].mip_level_count() {
            let entries: [_; BATCH] = std::array::from_fn(|binding| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(&above[binding % above.len()]),
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mip chain levels above"),
                layout: &self.layout,
                entries: &entries,
            });
            let targets = levels(level);
            let attachments: Vec<_> = targets.iter().map(attachment).collect();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mip chain"),
                color_attachments: &attachments,
                timestamp_writes: span.next(),
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
            drop(pass);
            above = targets;
        }
    }
}

/// One timed span over the build's passes: its start written by the first
/// pass, its end by the last.
struct Span<'a> {
    writes: Option<wgpu::RenderPassTimestampWrites<'a>>,
    passes: usize,
    at: usize,
}

impl<'a> Span<'a> {
    /// The timestamp writes of the next of `passes`.
    fn next(&mut self) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
        let writes = self.writes.as_ref()?;
        let (first, last) = (self.at == 0, self.at + 1 == self.passes);
        self.at += 1;
        (first || last).then(|| wgpu::RenderPassTimestampWrites {
            query_set: writes.query_set,
            beginning_of_pass_write_index: writes.beginning_of_pass_write_index.filter(|_| first),
            end_of_pass_write_index: writes.end_of_pass_write_index.filter(|_| last),
        })
    }
}

/// `texture`'s format among `FORMATS`.
fn format_index(texture: &wgpu::Texture) -> usize {
    FORMATS
        .iter()
        .position(|format| *format == texture.format())
        .expect("a waiting texture is a decoded image's")
}

impl MipChains {
    /// Builds the chains of the textures waiting in `chains` and submits
    /// them, after which none wait, timed in `timing`. In a planning pass
    /// that finds the stage's programs missing it queues their job and
    /// leaves the textures waiting.
    pub fn build(
        &mut self,
        compiler: &Compiler,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        chains: &mut Chains,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        if chains.is_empty() {
            return;
        }
        let Some(programs) = self.programs.get(compiler, "material mip chains", || {
            let device = device.clone();
            move || Programs::new(&device)
        }) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("material mip chains"),
        });
        // Textures of one format and size draw each level together, `BATCH`
        // to a pass, and those left over a pass each.
        let key =
            |texture: &wgpu::Texture| (format_index(texture), texture.width(), texture.height());
        let mut waiting = chains.take();
        waiting.sort_by_key(key);
        let groups: Vec<&[wgpu::Texture]> = waiting
            .chunk_by(|a, b| key(a) == key(b))
            .flat_map(|alike| {
                let batches = alike.chunks_exact(BATCH);
                let left = batches.remainder().iter().map(std::slice::from_ref);
                left.chain(batches)
            })
            .collect();
        let mut span = Span {
            writes: timing.and_then(|timing| timing.render_pass("mip chains")),
            passes: groups
                .iter()
                .map(|group| group[0].mip_level_count() as usize - 1)
                .sum(),
            at: 0,
        };
        for group in groups {
            programs.encode(device, &mut encoder, group, &mut span);
        }
        queue.submit([encoder.finish()]);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
