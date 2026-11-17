//! DiligentFX's inputs converted from SGL3D's surface (`post_fx_inputs.wgsl`):
//! their targets at one render size and the pass's program, which
//! `PostFx::begin` draws.
use crate::shading;

/// SGL3D's G-buffer as DiligentFX's inputs.
pub(crate) static INPUTS: shading::Module = shading::Module {
    name: "post_fx_inputs",
    source: include_str!("../post_fx_inputs.wgsl"),
    deps: &[&shading::GBUFFER, &shading::FULLSCREEN_VS],
};
/// The entry point the inputs' pipeline is created with, beside
/// `shading::FULLSCREEN_VS_ENTRY`.
pub(crate) const FS_MAIN_ENTRY: &str = "fs_main";

/// DiligentFX's inputs converted from SGL3D's G-buffer, at one render size.
pub(super) struct Inputs {
    pub size: [u32; 2],
    pub normal: wgpu::TextureView,
    pub material: wgpu::TextureView,
    pub motion: wgpu::TextureView,
    /// Depth under the effect camera's finite-far projection, alternating by
    /// frame index as Hydrogent swaps its two depth buffers: the frame's is
    /// `pCurrDepthBufferSRV` and `pDepthBufferSRV`, the other the previous
    /// frame's, `pPrevDepthBufferSRV`.
    pub depth: [wgpu::TextureView; 2],
}

impl Inputs {
    pub fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | usage,
                view_formats: &[],
            })
        };
        let target = |label, format| {
            texture(label, format, wgpu::TextureUsages::RENDER_ATTACHMENT)
                .create_view(&Default::default())
        };
        Self {
            size,
            normal: target("DiligentFX input normal", NORMAL_FORMAT),
            material: target("DiligentFX input material parameters", MATERIAL_FORMAT),
            motion: target("DiligentFX input motion vectors", MOTION_FORMAT),
            depth: std::array::from_fn(|_| {
                target("DiligentFX input depth", wgpu::TextureFormat::Depth32Float)
            }),
        }
    }
}

pub(super) const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
// Full precision, so DiligentFX's traced test (roughness <= threshold, made
// once by its mask for every pass, DFX-62) sees the roughness SGL3D's
// composition tests.
pub(super) const MATERIAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// Screen motion in RG and the G-buffer's depth motion in BA, which TAA's
/// `MOTION_ADAPTIVE_HISTORY` reads; SSR reads RG.
pub(super) const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
