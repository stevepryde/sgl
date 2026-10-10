//! Logical-canvas GPU rendering. This module may use winit's `Window` handle
//! to create a surface, but the game owns the window and event loop.
//!
//! It provides device/surface bring-up ([`gpu::Context`]), an offscreen scene
//! target at the logical resolution, and an aspect-preserving letterbox blit
//! to the swapchain ([`Renderer`]).
//!
//! The instanced sprite pipeline ([`sprite`]) consumes the
//! [`DrawList`] seam ([`draw`]), shelf-packed texture atlasing ([`atlas`]),
//! the world/screen [`Camera`] ([`camera`]), and PNG scene capture for the
//! visual-parity harness ([`Renderer::capture_scene`]).
//!
//! Flat-quad overlay primitives — lines, rect outlines, fills and circles on
//! the shared 1×1 white texture ([`crate::assets::white_texture`], uploaded by
//! [`Renderer::white_texture`]) — live in [`overlay`], for debug draws, editor
//! gizmos and screen fades on either channel.
//!
//! The 2D lighting pipeline ([`light`]) uses an
//! MRT scene pass (albedo + screen-space normals + light masks), per-light
//! additive accumulation with occluder shadows, and the
//! `albedo × (canvas_modulate + lights)` composite — fed by the
//! [`LightFrame`] seam next to the `DrawList`.

// Pixel dimensions, atlas coordinates, glyph metrics, and GPU indices cross
// integer/float widths at API boundaries. Inputs are bounded by texture/device
// limits before conversion. Pipeline construction is intentionally kept in
// cohesive builders so resource layouts can be audited together.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::default_trait_access,
    clippy::float_cmp,
    clippy::items_after_statements,
    clippy::many_single_char_names,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::too_many_lines
)]

pub mod atlas;
pub mod blit;
pub mod camera;
pub mod draw;
pub mod gpu;
pub mod letterbox;
pub mod light;
pub mod overlay;
pub mod sprite;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod test_gpu;
pub mod text;

pub use blit::{CaptureError, Renderer};
pub use camera::{Camera, WorldUnits};
pub use draw::{DrawList, Rect, SpriteInstance};
pub use gpu::{Context, Frame, Gpu, GpuError, RendererInitError};
pub use letterbox::{Letterbox, fit_fractional};
pub use light::{LightFrame, PointLight};
pub use overlay::Overlay;
pub use sprite::TextureError;

/// The color space the world channel is lit and composited in
/// (`Renderer::with_lighting`). Only the world channel, the light math and
/// the composite differ; the screen channel (UI), the letterbox blit and
/// scene capture are identical in both modes because the composited target
/// always holds sRGB-encoded 8-bit values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LightingSpace {
    /// Godot parity (D-8), the default: sprite pages are sampled raw
    /// (sRGB-encoded values), the albedo target is 8-bit gamma, modulates,
    /// light colors and `energy` are used as given (sRGB), and the composite
    /// `albedo × (modulate + accum)` is a gamma-space multiply-add stored
    /// unchanged. Bit-identical to the renderer before this option existed.
    #[default]
    Gamma,
    /// Linear lighting: world-channel sprite texels are decoded sRGB→linear
    /// when sampled, the albedo target is half-float linear, the clear color
    /// is converted to linear, and `SpriteInstance::color`, the
    /// `canvas_modulate`, light colors and `energy` are used **as given** —
    /// i.e. they must be authored in linear (shadow-sp's `#8A9BB0 × 0.003`
    /// ambient, its `0.045` glow intensity). The composite multiplies in
    /// linear, clamps, and sRGB-encodes exactly once into the 8-bit
    /// composited target. Cookies stay raw `Rgba8Unorm` data in both modes
    /// (the shipped cookies carry the falloff in alpha over a constant
    /// color, so their bytes are a shape, not a color).
    Linear,
}

/// Convert one sRGB-encoded channel to linear (the IEC 61966-2-1 transfer
/// function). Colors are authored in sRGB (what Godot displays); offscreen
/// render math happens in linear; the sRGB swapchain encodes back on store.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert one linear channel back to sRGB encoding — the inverse of
/// [`srgb_to_linear`], used when encoding the linear scene target to a PNG
/// (scene capture) so the file shows what the swapchain would.
#[must_use]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::{linear_to_srgb, srgb_to_linear};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn srgb_endpoints_and_midpoint() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // The PR-1 clear color channel: sRGB 0.3 is linear ~0.0732.
        assert!((srgb_to_linear(0.3) - 0.0732).abs() < 1e-3);
    }

    /// `linear_to_srgb` inverts `srgb_to_linear` across the range.
    #[wasm_bindgen_test(unsupported = test)]
    fn srgb_roundtrip() {
        for i in 0..=100 {
            let c = i as f32 / 100.0;
            let back = linear_to_srgb(srgb_to_linear(c));
            assert!((back - c).abs() < 1e-5, "roundtrip failed at {c}");
        }
    }
}
