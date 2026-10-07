//! The G-buffer's target formats; `gbuffer.wgsl` owns their encodings.
use wgpu::TextureFormat;

/// Lit colour, linear HDR; at a lit pixel its alpha holds the multiple
/// scattering's share of the ambient light, which source completion reads.
pub(crate) const COLOR: TextureFormat = TextureFormat::Rgba16Float;
/// Unoccluded ambient light (its diffuse share and the specular multiple
/// scattering it carries), linear HDR, and the irradiance volume's sky
/// visibility.
pub(crate) const AMBIENT: TextureFormat = TextureFormat::Rgba16Float;
/// Signed octahedral base and coat normals.
pub(crate) const NORMAL: TextureFormat = TextureFormat::Rgba16Float;
/// Coat and base roughness, coat strength and the base's F90.
pub(crate) const MATERIAL: TextureFormat = TextureFormat::Rgba16Float;
/// F0, with whether the surface is lit and takes baked scene lights and its
/// material occlusion, one 8-bit code (`gbuffer_encode_f0`).
pub(crate) const F0: TextureFormat = TextureFormat::Rgba8Unorm;
/// World anisotropy tangent, signed octahedral, its strength and the
/// environment scale.
pub(crate) const ANISOTROPY: TextureFormat = TextureFormat::Rgba16Float;
/// Current minus previous unjittered UV.
pub(crate) const MOTION: TextureFormat = TextureFormat::Rg16Float;
/// The receiver layer: a receiver's traced lobe's normal and roughness.
pub(crate) const RECEIVER: TextureFormat = TextureFormat::Rgba16Float;
/// Reversed-Z depth.
pub(crate) const DEPTH: TextureFormat = TextureFormat::Depth32Float;
/// Each pixel's raster source and primitive.
pub(crate) const SOURCE_ID: TextureFormat = TextureFormat::Rg32Uint;
