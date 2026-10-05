//! The scene's baked diffuse lighting on the GPU: the lightmap and the fixed
//! irradiance atlas static instances take (`static_lighting`'s content),
//! with the `Scene` operations that install them. Moving instances' ambient
//! cubes are in their object records (`instances`).
use super::{Scene, SceneError};
use crate::content::identity::MaterialId;
use crate::static_lighting::{
    CompressedIrradianceAtlas, IrradianceAtlas, Lightmap, irradiance_half, lobe_rgba8,
    valid_directionality,
};

/// A baked map's textures: irradiance / PI and its directionality, in layers
/// of one size (the atlas's front and back, the lightmap's one).
pub(crate) struct BakedMap {
    pub irradiance: wgpu::TextureView,
    pub direction: wgpu::TextureView,
}

/// A texture array of `size` and `layer_count` layers holding `bytes` in
/// `format`.
fn layers(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    [width, height]: [u32; 2],
    layer_count: u32,
    bytes: &[u8],
) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: layer_count,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (block_width, block_height) = format.block_dimensions();
    crate::counters::write_texture(
        queue,
        texture.as_image_copy(),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width / block_width * format.block_copy_size(None).unwrap_or(0)),
            rows_per_image: Some(height / block_height),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

/// All-zero RGBA8 layers, the shader's sentinel for absent directionality.
fn absent_directionality(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer_count: u32,
) -> wgpu::TextureView {
    layers(
        device,
        queue,
        "baked directionality",
        wgpu::TextureFormat::Rgba8Unorm,
        [1, 1],
        layer_count,
        &vec![0; 4 * layer_count as usize],
    )
}

/// One layer of a baked map in linear values: its irradiance and
/// directionality.
type LinearLayer<'a> = (&'a [[f32; 3]], &'a [[f32; 4]]);

impl BakedMap {
    /// RGBA16F irradiance and RGBA8Unorm directionality layers of `size`
    /// from linear values; each layer's irradiance already holds one texel
    /// per pixel of `size`.
    fn linear(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        maps: &[LinearLayer],
    ) -> Result<Self, SceneError> {
        let count = size[0] as usize * size[1] as usize;
        if maps
            .iter()
            .any(|(_, directionality)| !valid_directionality(directionality, count))
        {
            return Err(SceneError::InvalidDirectionality);
        }
        if size[0].max(size[1]) > device.limits().max_texture_dimension_2d {
            return Err(SceneError::DeviceLimit);
        }
        let mut bytes = Vec::with_capacity(count * maps.len() * 8);
        for pixel in maps.iter().flat_map(|(irradiance, _)| irradiance.iter()) {
            for value in pixel.iter().copied().chain(std::iter::once(0.)) {
                if !value.is_finite() || !(0. ..=65504.).contains(&value) {
                    return Err(SceneError::InvalidIrradiance);
                }
                bytes.extend(irradiance_half(value).to_le_bytes());
            }
        }
        let layer_count = maps.len() as u32;
        // All-zero is the internal absent-layer sentinel. Authored neutral lobes
        // use [.5; 4]; never manufacture a full map when every layer is absent.
        let direction = if maps.iter().any(|(_, layer)| !layer.is_empty()) {
            let mut directions = Vec::with_capacity(count * maps.len() * 4);
            for (_, layer) in maps {
                if layer.is_empty() {
                    directions.resize(directions.len() + count * 4, 0);
                } else {
                    directions.extend(layer.iter().flat_map(|&lobe| lobe_rgba8(lobe)));
                }
            }
            layers(
                device,
                queue,
                "baked directionality",
                wgpu::TextureFormat::Rgba8Unorm,
                size,
                layer_count,
                &directions,
            )
        } else {
            absent_directionality(device, queue, layer_count)
        };
        Ok(Self {
            irradiance: layers(
                device,
                queue,
                "baked irradiance",
                wgpu::TextureFormat::Rgba16Float,
                size,
                layer_count,
                &bytes,
            ),
            direction,
        })
    }

    fn atlas(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &IrradianceAtlas,
    ) -> Result<Self, SceneError> {
        let [width, height] = atlas.size;
        let count = width as usize * height as usize;
        if width == 0
            || height == 0
            || atlas.irradiance.len() != count
            || atlas.back_irradiance.len() != count
        {
            return Err(SceneError::InvalidIrradianceAtlas);
        }
        Self::linear(
            device,
            queue,
            atlas.size,
            &[
                (&atlas.irradiance, &atlas.directionality),
                (&atlas.back_irradiance, &atlas.back_directionality),
            ],
        )
    }

    fn compressed_atlas(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &CompressedIrradianceAtlas,
    ) -> Result<Self, SceneError> {
        let [width, height] = atlas.size;
        if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
            return Err(SceneError::InvalidIrradianceAtlas);
        }
        if !device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            return Err(SceneError::CompressionUnsupported);
        }
        if !atlas.scale.is_finite() || atlas.scale < 0. {
            return Err(SceneError::InvalidIrradiance);
        }
        let layer = (width / 4) as usize * (height / 4) as usize * 16;
        if atlas.irradiance.len() != layer * 2
            || !(atlas.directionality.is_empty() || atlas.directionality.len() == layer * 2)
        {
            return Err(SceneError::InvalidIrradianceAtlas);
        }
        if width.max(height) > device.limits().max_texture_dimension_2d {
            return Err(SceneError::DeviceLimit);
        }
        Ok(Self {
            irradiance: layers(
                device,
                queue,
                "baked irradiance",
                wgpu::TextureFormat::Bc6hRgbUfloat,
                atlas.size,
                2,
                &atlas.irradiance,
            ),
            direction: if atlas.directionality.is_empty() {
                absent_directionality(device, queue, 2)
            } else {
                layers(
                    device,
                    queue,
                    "baked directionality",
                    wgpu::TextureFormat::Bc7RgbaUnorm,
                    atlas.size,
                    2,
                    &atlas.directionality,
                )
            },
        })
    }

    fn lightmap(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        map: &Lightmap,
    ) -> Result<Self, SceneError> {
        let [width, height] = map.size;
        if width == 0 || height == 0 || map.irradiance.len() != width as usize * height as usize {
            return Err(SceneError::InvalidLightmap);
        }
        Self::linear(
            device,
            queue,
            map.size,
            &[(&map.irradiance, &map.directionality)],
        )
    }
}

/// The scene's baked diffuse lighting: what lit group 0 binds of it and what
/// the frame data carries.
pub(crate) struct StaticLighting {
    /// Front and back layers.
    pub atlas: BakedMap,
    /// Multiplies the atlas's irradiance (`CompressedIrradianceAtlas::scale`).
    pub atlas_scale: f32,
    /// The game installed an atlas; until then `atlas` is a black stand-in
    /// that covers no receiver.
    pub atlas_installed: bool,
    pub lightmap: BakedMap,
    /// `Lightmap::uv_scale_offset`.
    pub lightmap_chart: [f32; 4],
    /// Samples both maps bilinearly. X clamps and Y repeats, the lightmap's
    /// addressing; the shader keeps atlas UVs inside the atlas.
    pub sampler: wgpu::Sampler,
}

impl StaticLighting {
    pub fn empty(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let black = [[0.; 3]];
        Self {
            atlas: BakedMap::linear(device, queue, [1, 1], &[(&black, &[]), (&black, &[])])
                .unwrap(),
            atlas_scale: 1.,
            atlas_installed: false,
            lightmap: BakedMap::linear(device, queue, [1, 1], &[(&black, &[])]).unwrap(),
            lightmap_chart: [0.; 4],
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("baked diffuse: X clamps, Y repeats"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }
}

impl Scene {
    /// Install the caller-authored irradiance atlas static instances take
    /// through `Vertex::lightmap_uv`; lightmapped materials take precedence.
    pub fn set_static_irradiance_atlas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &IrradianceAtlas,
    ) -> Result<(), SceneError> {
        self.static_lighting.atlas = BakedMap::atlas(device, queue, atlas)?;
        self.static_lighting.atlas_scale = 1.;
        self.static_lighting.atlas_installed = true;
        self.resources = super::next_generation();
        Ok(())
    }

    /// Install a block-compressed irradiance atlas in place of
    /// `set_static_irradiance_atlas`; see [`CompressedIrradianceAtlas`].
    pub fn set_compressed_static_irradiance_atlas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &CompressedIrradianceAtlas,
    ) -> Result<(), SceneError> {
        self.static_lighting.atlas = BakedMap::compressed_atlas(device, queue, atlas)?;
        self.static_lighting.atlas_scale = atlas.scale;
        self.static_lighting.atlas_installed = true;
        self.resources = super::next_generation();
        Ok(())
    }

    /// Install a bake that lights `materials` where static instances use
    /// them, replacing any previous bake and its materials. The caller owns
    /// geometry, chart resolution and source values.
    pub fn set_lightmap(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        map: &Lightmap,
        materials: &[MaterialId],
    ) -> Result<(), SceneError> {
        for &material in materials {
            self.materials.get(material)?;
        }
        let lightmap = BakedMap::lightmap(device, queue, map)?;
        let ids: Vec<_> = self.materials.slots.iter().map(|(id, _)| id).collect();
        for id in ids {
            self.materials
                .set_baked(queue, &self.rays, id, materials.contains(&id));
        }
        self.static_lighting.lightmap = lightmap;
        self.static_lighting.lightmap_chart = map.uv_scale_offset;
        self.resources = super::next_generation();
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "static_lighting_tests.rs"]
mod tests;
