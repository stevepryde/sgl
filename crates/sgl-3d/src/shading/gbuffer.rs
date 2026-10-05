//! The G-buffer's target formats; `gbuffer.wgsl` owns their encodings.
use wgpu::TextureFormat;

/// Lit colour, linear HDR.
pub(crate) const COLOR: TextureFormat = TextureFormat::Rgba16Float;
/// Unoccluded ambient diffuse radiance, linear HDR, and the irradiance
/// volume's sky visibility.
pub(crate) const AMBIENT: TextureFormat = TextureFormat::Rgba16Float;
/// Signed octahedral base and coat normals.
pub(crate) const NORMAL: TextureFormat = TextureFormat::Rgba16Float;
/// Coat and base roughness, coat strength and environment scale.
pub(crate) const MATERIAL: TextureFormat = TextureFormat::Rgba16Float;
/// F0 and the lit flag.
pub(crate) const F0: TextureFormat = TextureFormat::Rgba8Unorm;
/// World anisotropy tangent and strength.
pub(crate) const ANISOTROPY: TextureFormat = TextureFormat::Rgba16Float;
/// Current minus previous unjittered UV.
pub(crate) const MOTION: TextureFormat = TextureFormat::Rg16Float;
/// The receiver layer: a receiver's traced lobe's normal and roughness.
pub(crate) const RECEIVER: TextureFormat = TextureFormat::Rgba16Float;
/// Reversed-Z depth.
pub(crate) const DEPTH: TextureFormat = TextureFormat::Depth32Float;
/// Each pixel's raster source and primitive.
pub(crate) const SOURCE_ID: TextureFormat = TextureFormat::Rg32Uint;
