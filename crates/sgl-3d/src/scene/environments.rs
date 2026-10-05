//! Environments: each one's panorama (the sky's backdrop) and PMREM atlas
//! (surface and reflection lighting), a black stand-in for frames that name
//! none, and the DFG lookup table they are lit with, with the `Scene`
//! operations that add and remove them.
use super::slots::Slots;
use super::textures;
use super::{Scene, SceneError};
use crate::content::identity::EnvironmentId;
use crate::environment::{EnvironmentMap, PmremAtlas};

/// One environment's textures as group 0 binds them.
pub(crate) struct Environment {
    /// The panorama, sampled as sRGB.
    pub backdrop: wgpu::TextureView,
    /// The PMREM atlas, as a one-layer array.
    pub pmrem: wgpu::TextureView,
}

impl Environment {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panorama: &image::RgbaImage,
        atlas: &PmremAtlas,
    ) -> Self {
        let size = wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Three r185 PMREM atlas"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        crate::counters::write_texture(
            queue,
            texture.as_image_copy(),
            &atlas.rgba16,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width * 8),
                rows_per_image: Some(atlas.height),
            },
            size,
        );
        Self {
            backdrop: textures::upload(device, queue, panorama, true),
            pmrem: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
        }
    }
}

pub(crate) struct Environments {
    pub slots: Slots<EnvironmentId, Environment>,
    /// Black, for frames without an environment.
    neutral: Environment,
    pub sampler: wgpu::Sampler,
}

impl Environments {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let black = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let neutral = Environment::new(
            device,
            queue,
            &black,
            &PmremAtlas {
                width: 1,
                height: 1,
                rgba16: vec![0; 8],
            },
        );
        Self {
            slots: Slots::default(),
            neutral,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("equirectangular wrap longitude clamp latitude"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// `id` while it is in the scene.
    pub fn live(&self, id: Option<EnvironmentId>) -> Option<EnvironmentId> {
        id.filter(|&id| self.slots.get(id).is_some())
    }

    /// The environment a frame naming `id` binds: the black stand-in for
    /// none or an ended identity.
    pub fn frame(&self, id: Option<EnvironmentId>) -> &Environment {
        id.and_then(|id| self.slots.get(id))
            .unwrap_or(&self.neutral)
    }
}

impl Scene {
    /// Adds an environment. `FrameInput::environment` names the one that
    /// lights a frame.
    pub fn add_environment(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        map: &EnvironmentMap,
    ) -> Result<EnvironmentId, SceneError> {
        textures::validate_size(device, map.panorama.dimensions().into())?;
        let atlas = &map.filtered;
        if atlas.width == 0
            || atlas.height == 0
            || atlas.rgba16.len() != atlas.width as usize * atlas.height as usize * 8
        {
            return Err(SceneError::InvalidEnvironmentMap);
        }
        if atlas.width.max(atlas.height) > device.limits().max_texture_dimension_2d {
            return Err(SceneError::DeviceLimit);
        }
        let environment = Environment::new(device, queue, &map.panorama, atlas);
        Ok(self.environments.slots.insert(environment))
    }

    /// Removes an environment. A frame that names it has none.
    pub fn remove_environment(&mut self, id: EnvironmentId) -> Result<(), SceneError> {
        self.environments
            .slots
            .remove(id)
            .map(drop)
            .ok_or(SceneError::UnknownEnvironment)
    }
}
