//! Content: the CPU data a game authors or loads and hands to a `Scene`:
//! meshes, materials and images (glTF), the identities a scene issues for
//! them, instance states, lights, decals, environments, baked lighting,
//! specular probes, the dynamic GI volume's placement, the irradiance
//! volume's placement and cells, mesh alternatives, material shaders and
//! transient geometry;
//! and the frame's lights and authored look it describes in `FrameInput`.
//! Plain data and its validation; nothing here touches the GPU.
pub mod asset;
pub mod baked_specular_probe;
pub(crate) mod decal;
pub mod deformation;
pub(crate) mod dynamic_gi;
pub mod environment;
pub mod geometry;
mod gltf;
pub(crate) mod identity;
mod images;
pub(crate) mod instance;
pub(crate) mod irradiance_volume;
pub(crate) mod light;
pub(crate) mod lighting;
pub mod lod;
pub(crate) mod material;
pub(crate) mod model;
pub mod shader;
pub mod static_lighting;
pub(crate) mod transient;
