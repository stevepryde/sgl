//! The presentation boundary: textures hold linear float colour; a surface
//! additionally declares how the compositor interprets its stored values.
//! Follows wgpu 30 `SurfaceColorSpace` and its backend Auto resolution.
#[derive(Clone, Copy)]
pub(crate) struct Output {
    pub format: wgpu::TextureFormat,
    pub encodes_srgb: bool,
}

impl Output {
    pub fn texture(format: wgpu::TextureFormat) -> Self {
        let float = matches!(
            format,
            wgpu::TextureFormat::Rgba16Float
                | wgpu::TextureFormat::Rgba32Float
                | wgpu::TextureFormat::Rg11b10Ufloat
        );
        Self {
            format,
            encodes_srgb: !format.is_srgb() && !float,
        }
    }

    pub fn surface(
        config: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
        backend: wgpu::Backend,
    ) -> Option<Self> {
        let linear = match config.color_space {
            wgpu::SurfaceColorSpace::Auto => {
                backend != wgpu::Backend::BrowserWebGpu
                    && config.format == wgpu::TextureFormat::Rgba16Float
                    && capabilities.format_capabilities.iter().any(|format| {
                        format.format == config.format
                            && format
                                .color_spaces
                                .contains(wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR)
                    })
            }
            wgpu::SurfaceColorSpace::ExtendedSrgbLinear => true,
            wgpu::SurfaceColorSpace::Srgb | wgpu::SurfaceColorSpace::ExtendedSrgb => false,
            _ => return None,
        };
        // An sRGB attachment always encodes; it cannot deliver linear stored
        // values to a linear compositor even if the driver advertises the pair.
        if linear && config.format.is_srgb() {
            return None;
        }
        Some(Self {
            format: config.format,
            encodes_srgb: !config.format.is_srgb() && !linear,
        })
    }
}
