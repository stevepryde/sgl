//! Content: the CPU data a game authors or loads and hands to a `Scene`:
//! meshes, materials and images (glTF), rigs and the sampling and blending
//! of their animation clips, the identities a scene issues for
//! them, instance states, lights and their measured profiles, decals, environments, baked lighting,
//! specular probes, the dynamic GI volume's placement, the irradiance
//! volume's placement and cells, mesh alternatives, material shaders,
//! render textures' sizes and transient geometry;
//! and the frame's lights and authored look it describes in `FrameInput`.
//! Plain data and its validation; nothing here touches the GPU.
pub mod animation;
pub mod asset;
pub mod baked_specular_probe;
pub(crate) mod decal;
pub mod deformation;
pub(crate) mod dynamic_gi;
pub mod environment;
pub mod geometry;
mod gltf;
pub(crate) mod hdr_effect;
pub(crate) mod identity;
pub(crate) mod images;
pub(crate) mod instance;
pub(crate) mod irradiance_volume;
pub(crate) mod light;
pub(crate) mod light_profile;
pub(crate) mod lighting;
pub(crate) mod live_specular_probe;
mod load_budget;
pub mod lod;
pub(crate) mod material;
mod material_texture;
pub(crate) mod model;
pub(crate) mod render_texture;
pub mod shader;
pub(crate) mod srgb;
pub mod static_lighting;
pub(crate) mod transient;

mod meshopt;
mod zstandard;
