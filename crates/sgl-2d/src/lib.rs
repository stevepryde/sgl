//! Reusable native/browser client building blocks.
//!
//! Games own composition, simulation, content, and UI layout. This crate owns
//! reusable asset, rendering, text, lighting, and immediate UI
//! mechanisms extracted from working games.

pub mod aseprite;
pub mod assets;
pub mod canvas;
mod fps;
pub mod render;
mod surface;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod test_fs;
pub mod ui;
