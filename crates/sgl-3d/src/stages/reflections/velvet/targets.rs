//! Velvet's targets, as `SSEffects::ssr_allocate_buffers` allocates them,
//! with SGL3D's full-resolution inputs and the temporal pass's history.
use super::FilterParams;

/// One view of each of `texture`'s first `mips` levels.
pub(super) fn mip_views(texture: &wgpu::Texture, mips: u32) -> Vec<wgpu::TextureView> {
    (0..mips)
        .map(|mip| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: mip,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect()
}

/// `SSEffects::ssr_allocate_buffers`, with SGL3D's full-resolution inputs.
pub(super) struct Targets {
    pub(super) full: [u32; 2],
    pub(super) half: bool,
    /// The traced size: full, or half with `half`.
    pub(super) size: [u32; 2],
    pub(super) mipmaps: u32,
    /// Godot's normal-roughness buffer and depth at full resolution. At full
    /// size the depth is the hierarchy's first mip.
    pub(super) normal_roughness: wgpu::TextureView,
    pub(super) depth: Option<wgpu::TextureView>,
    /// `RB_NORMAL_ROUGHNESS` at half size.
    pub(super) normal_roughness_half: Option<wgpu::TextureView>,
    /// `RB_HIZ`: all mips, and one view per mip.
    pub(super) hiz: wgpu::TextureView,
    pub(super) hiz_mips: Vec<wgpu::TextureView>,
    /// `RB_SSR`.
    pub(super) ssr: wgpu::TextureView,
    pub(super) ssr_mips: Vec<wgpu::TextureView>,
    pub(super) filter_params: Vec<wgpu::Buffer>,
    /// `RB_MIP_LEVEL`.
    pub(super) mip_level: wgpu::TextureView,
    /// The resolved reflections at full resolution (`RB_FINAL`).
    pub(super) output: wgpu::TextureView,
    /// This frame's traced hits, before temporal accumulation into `ssr`.
    pub(super) traced: wgpu::TextureView,
    /// The depth of each receiver's reflected point, for hit reprojection.
    pub(super) reprojection: wgpu::TextureView,
    /// Accumulated hits and receiver depth, alternating between frames.
    pub(super) history: [wgpu::TextureView; 2],
    pub(super) depth_history: [wgpu::TextureView; 2],
}

impl Targets {
    pub(super) fn new(device: &wgpu::Device, full: [u32; 2], half: bool) -> Self {
        let size = if half {
            full.map(|v| (v / 2).max(1))
        } else {
            full
        };
        let mut mipmaps = 1;
        let [mut width, mut height] = size;
        while width > 1 && height > 1 {
            width /= 2;
            height /= 2;
            mipmaps += 1;
        }
        let texture = |label, size: [u32; 2], format, mips| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: mips,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
        };
        let view = |texture: wgpu::Texture| texture.create_view(&Default::default());
        let normal_roughness_format = wgpu::TextureFormat::Rgba16Float;
        let hiz = texture(
            "Godot SSR hi-z",
            size,
            wgpu::TextureFormat::R32Float,
            mipmaps,
        );
        let ssr = texture("Godot SSR", size, crate::shading::gbuffer::COLOR, mipmaps);
        Self {
            full,
            half,
            size,
            mipmaps,
            normal_roughness: view(texture(
                "Godot SSR normal roughness",
                full,
                normal_roughness_format,
                1,
            )),
            depth: half.then(|| {
                view(texture(
                    "Godot SSR depth",
                    full,
                    wgpu::TextureFormat::R32Float,
                    1,
                ))
            }),
            normal_roughness_half: half.then(|| {
                view(texture(
                    "Godot SSR half normal roughness",
                    size,
                    normal_roughness_format,
                    1,
                ))
            }),
            hiz_mips: mip_views(&hiz, mipmaps),
            hiz: view(hiz),
            ssr_mips: mip_views(&ssr, mipmaps),
            ssr: view(ssr),
            filter_params: (0..mipmaps)
                .map(|m| {
                    crate::counters::buffer_init(
                        device,
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("Godot SSR filter parameters"),
                            contents: bytemuck::bytes_of(&FilterParams {
                                screen_size: size.map(|v| (v >> m).max(1) as i32),
                                mip_level: m,
                                pad: 0,
                            }),
                            usage: wgpu::BufferUsages::UNIFORM,
                        },
                    )
                })
                .collect(),
            mip_level: view(texture(
                "Godot SSR mip level",
                size,
                wgpu::TextureFormat::R32Float,
                1,
            )),
            output: view(texture(
                "Godot SSR resolved",
                full,
                crate::shading::gbuffer::COLOR,
                1,
            )),
            traced: view(texture(
                "Godot SSR traced",
                size,
                crate::shading::gbuffer::COLOR,
                1,
            )),
            reprojection: view(texture(
                "Godot SSR reprojection",
                size,
                wgpu::TextureFormat::R32Float,
                1,
            )),
            history: [0, 1].map(|_| {
                view(texture(
                    "Godot SSR history",
                    size,
                    crate::shading::gbuffer::COLOR,
                    1,
                ))
            }),
            depth_history: [0, 1].map(|_| {
                view(texture(
                    "Godot SSR depth history",
                    size,
                    wgpu::TextureFormat::R32Float,
                    1,
                ))
            }),
        }
    }
}
