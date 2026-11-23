//! Render textures: each one's GPU texture, which a view of the scene
//! draws its frame into and materials sample as their emission map, and
//! whether a frame changed its picture, with the `Scene` operations that
//! add, read, resize and remove them (the architecture's Scene content,
//! Render texture, and its Frame, "Render textures").
use super::slots::Slots;
use super::{Scene, SceneError, textures};
use crate::content::identity::RenderTextureId;
use crate::content::render_texture::RenderTexture;

impl RenderTexture {
    /// Its texture's format, which the view that draws into it is created
    /// with (`View::new`): `Rgba8UnormSrgb`, sRGB-encoded, so the
    /// attachment encodes the view's displayed picture and sampling decodes
    /// it, once each.
    pub fn format(&self) -> wgpu::TextureFormat {
        wgpu::TextureFormat::Rgba8UnormSrgb
    }
}

pub(crate) struct Held {
    pub description: RenderTexture,
    /// The whole texture, which its producer's post stage writes and its
    /// materials' group 2 samples.
    pub view: wgpu::TextureView,
    /// The materials that name it.
    users: u32,
    /// A render since the last finished frame drew into it, or it was
    /// replaced.
    pending: bool,
    /// The last finished frame followed such a change.
    changed: bool,
}

impl Held {
    fn new(device: &wgpu::Device, description: RenderTexture) -> Self {
        let [width, height] = description.size;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("render texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: description.format(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            description,
            view: texture.create_view(&Default::default()),
            users: 0,
            pending: true,
            changed: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct RenderTextures {
    slots: Slots<RenderTextureId, Held>,
}

impl RenderTextures {
    pub fn get(&self, id: RenderTextureId) -> Result<&Held, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownRenderTexture)
    }

    /// Counts `by` more materials naming `id`, a live render texture.
    pub fn count(&mut self, id: Option<RenderTextureId>, by: i32) {
        if let Some(id) = id {
            let held = self
                .slots
                .get_mut(id)
                .expect("a named render texture lives");
            held.users = held.users.checked_add_signed(by).unwrap();
        }
    }

    /// The frame being rendered draws into `id`.
    pub fn drawn(&mut self, id: RenderTextureId) {
        if let Some(held) = self.slots.get_mut(id) {
            held.pending = true;
        }
    }

    /// Whether a surface that samples `id` may show another picture in the
    /// frame being rendered than in the last finished frame: a render since
    /// that frame drew into it or it was replaced, or the same held for
    /// that frame, whose consumers may have sampled it before its producer
    /// drew (the register's M-16).
    pub fn changed(&self, id: RenderTextureId) -> bool {
        self.slots
            .get(id)
            .is_some_and(|held| held.pending || held.changed)
    }

    /// Commits a submitted frame.
    pub fn finish_frame(&mut self) {
        for (_, held) in self.slots.iter_mut() {
            held.changed = std::mem::take(&mut held.pending);
        }
    }
}

/// A surface that samples a render texture: a mesh of a visible instance
/// drawn with a material that names it.
pub(crate) type Screen = super::named_surfaces::Named<RenderTextureId>;

impl Scene {
    /// The surfaces that sample a render texture, which a frame tests
    /// against its views (`renderer::order`): none without a material that
    /// names one.
    pub(crate) fn screens(&self) -> Vec<Screen> {
        self.named_surfaces(|material| material.emission_render_texture)
    }

    /// Adds a render texture: a GPU texture of `texture.size` texels and
    /// [`RenderTexture::format`], black until a view draws into it. List a
    /// view of that size and format in `Renderer::render` with
    /// `ViewOutput::RenderTexture` to draw into it, and name it in a
    /// material (`SurfaceMaterial::emission_render_texture`) to show it.
    /// `SceneError::EmptyImage` for a side of 0 and `DeviceLimit` for one
    /// beyond the device's largest 2D texture.
    pub fn add_render_texture(
        &mut self,
        device: &wgpu::Device,
        texture: RenderTexture,
    ) -> Result<RenderTextureId, SceneError> {
        textures::validate_size(device, texture.size)?;
        Ok(self
            .render_textures
            .slots
            .insert(Held::new(device, texture)))
    }

    /// A render texture's description.
    pub fn render_texture(&self, id: RenderTextureId) -> Result<RenderTexture, SceneError> {
        Ok(self.render_textures.get(id)?.description)
    }

    /// Replaces a render texture with one of `texture.size`, which does
    /// nothing unless the size differs: a new GPU texture, black until a
    /// view draws into it, which the materials that name it sample from now
    /// on. Resize the view that draws into it to the same size
    /// (`View::resize`). Errors as `add_render_texture`, leaving it as it
    /// was.
    pub fn set_render_texture(
        &mut self,
        device: &wgpu::Device,
        id: RenderTextureId,
        texture: RenderTexture,
    ) -> Result<(), SceneError> {
        let held = self
            .render_textures
            .slots
            .get_mut(id)
            .ok_or(SceneError::UnknownRenderTexture)?;
        if held.description == texture {
            return Ok(());
        }
        textures::validate_size(device, texture.size)?;
        *held = Held {
            users: held.users,
            ..Held::new(device, texture)
        };
        self.materials
            .rebind_render_texture(device, &self.render_textures, id);
        Ok(())
    }

    /// Removes a render texture no material names.
    pub fn remove_render_texture(&mut self, id: RenderTextureId) -> Result<(), SceneError> {
        if self.render_textures.get(id)?.users > 0 {
            return Err(SceneError::RenderTextureInUse);
        }
        self.render_textures.slots.remove(id);
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
