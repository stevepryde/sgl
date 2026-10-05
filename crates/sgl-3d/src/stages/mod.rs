//! The stages, one module each, in the order the renderer runs them
//! (`renderer/frame.rs`). Each is one struct that owns its pipelines, bind
//! groups, targets and history, and states what it reads, writes and
//! honours and its timing groups. Stages meet only through what the
//! renderer lends them (`view::frame::FrameContext`) and the values it
//! passes between them; no stage uses another. Two modules are not in the
//! frame's order: a probe capture's GGX prefilter (`probe_prefilter`) and
//! the diagnostics observations (`frame_probe`, `visible_instances`).
pub(crate) mod antialiasing;
pub(crate) mod deform;
pub(crate) mod dynamic_gi;
pub(crate) mod exposure;
pub(crate) mod fog;
#[cfg(feature = "diagnostics")]
pub(crate) mod frame_probe;
pub(crate) mod motion_blur;
pub(crate) mod opaque;
pub(crate) mod post;
pub(crate) mod prepare;
pub(crate) mod probe_prefilter;
pub(crate) mod reflections;
pub(crate) mod shadows;
pub(crate) mod transparent;
#[cfg(feature = "diagnostics")]
pub(crate) mod visible_instances;
