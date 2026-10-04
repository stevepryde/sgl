//! Caller-driven 3D rendering for SGL, native and in the browser (WASM +
//! WebGPU). Games own composition, simulation and content.
#![deny(unsafe_code)]
mod content;
#[cfg(any(test, feature = "diagnostics"))]
pub mod diagnostics;
mod frame_input;
pub mod graphics_device;
mod renderer;
mod scene;
pub mod settings;
mod shading;
mod stages;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod test_support;
pub mod timing;
mod view;

pub use content::baked_specular_probe::{
    BakedSpecularProbe, ProbeError, SpecularProbeBox, SpecularProbeRadiance, SpecularProbeTexels,
};
pub use content::decal::Decal;
pub use content::identity::{
    DecalId, DecalImageId, EnvironmentId, InstanceId, LightId, MaterialId, ModelId,
};
pub use content::instance::{InstanceState, Mobility};
pub use content::light::{Light, LightShape};
pub use content::lighting::{
    Backdrop, DirectionalLight, DirectionalShadow, EnvironmentLight, HemisphereLight, Mist,
};
pub use content::material::{AlphaMode, SurfaceMaterial};
pub use content::model::{AssetIds, ModelMesh};
pub use content::transient::FogVolume;
pub use content::{
    asset, baked_specular_probe, deformation, environment, geometry, lod, static_lighting,
};
pub use frame_input::{
    AutoExposure, BloomParameters, Camera, ColorGrading, ColorGradingGlobal, ColorGradingSection,
    CompensationCurve, CompensationCurveError, Exposure, Fog, FrameInput, MeteringMask,
    MotionBlurParameters, perspective,
};
pub use glam;
pub use renderer::{Renderer, RendererError};
pub use scene::{Scene, SceneError};
pub use stages::shadows::local::LocalShadowStats;
pub use view::draw_list::GeometryStats;

/// Caller-authored additive geometry.
pub mod effects {
    pub use crate::content::transient::{Glow, GlowKind, GlowProfile};
}
/// Caller-authored heat shimmer geometry.
pub mod heat_distortion {
    pub use crate::content::transient::{HeatDistortion, MAX_DISPLACEMENT_PIXELS, MAX_VERTICES};
}
