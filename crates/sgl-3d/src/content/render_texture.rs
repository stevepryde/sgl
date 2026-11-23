//! A render texture's description (the architecture's Scene content,
//! Render texture).

/// A picture a view of the scene draws on the GPU for materials to sample:
/// an in-world monitor's. The scene owns its texture
/// (`Scene::add_render_texture`), of [`RenderTexture::format`] with one
/// level, black until a view draws into it. A view listed in
/// `Renderer::render` with `ViewOutput::RenderTexture` draws its whole
/// frame into it, as it would for a display, and a material names it as its
/// emission map (`SurfaceMaterial::emission_render_texture`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTexture {
    /// Its size in texels, each side at least 1 and at most the device's
    /// largest 2D texture side.
    pub size: [u32; 2],
}

impl RenderTexture {
    /// A render texture of `size` texels.
    pub fn new(size: [u32; 2]) -> Self {
        Self { size }
    }
}
