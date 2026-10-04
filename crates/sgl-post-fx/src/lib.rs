//! SGL's wgpu post-processing effects: screen-space reflections, temporal
//! anti-aliasing and their shared context. Derived from DiligentFX revision
//! f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b, with fixes and enhancements for
//! SGL3D. See README.md and PROVENANCE.md for origins and implementation notes.
//! This library evolves independently of upstream; scene and frame integration
//! belong to its caller.
//!
//! | Upstream | Here |
//! | --- | --- |
//! | `PostProcess/ScreenSpaceReflection/{interface,src}/ScreenSpaceReflection.*` | [`screen_space_reflection`] |
//! | `PostProcess/TemporalAntiAliasing/{interface,src}/TemporalAntiAliasing.*` | [`temporal_anti_aliasing`] |
//! | `PostProcess/Common/{interface,src}/PostFXContext.*` | [`post_fx_context`] |
//! | `PostProcess/Common/{interface,src}/PostFXRenderTechnique.*` | [`render_technique`] |
//! | `Shaders/Common/public/BasicStructures.fxh`, `ScreenSpaceReflectionStructures.fxh`, `TemporalAntiAliasingStructures.fxh` | [`structures`] |
//! | `Shaders/**`, DiligentCore `HLSLDefinitions.fxh` | `shaders/wgsl/**`, assembled by [`shaders`] |
pub mod post_fx_context;
pub mod render_technique;
pub mod screen_space_reflection;
pub mod shaders;
pub mod structures;
pub mod temporal_anti_aliasing;

pub use post_fx_context::PostFXContext;
pub use screen_space_reflection::ScreenSpaceReflection;
pub use structures::{CameraAttribs, ScreenSpaceReflectionAttribs, TemporalAntiAliasingAttribs};
pub use temporal_anti_aliasing::TemporalAntiAliasing;
