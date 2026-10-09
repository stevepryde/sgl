//! A game's material shader (`Scene::add_shader`; specs/sgl3d-architecture.md,
//! Shader contract): the contract's WGSL module, the providers a program
//! composes in place of a game's module or beside it, and the validation
//! that refuses a module before any program is created from it
//! (`validate`).
use super::Module;

mod loops;
mod validate;
pub(crate) use validate::validate;

/// The structs a game's module takes and returns.
pub(crate) static SHADER_CONTRACT: Module = Module {
    name: "shader_contract",
    source: include_str!("shader_contract.wgsl"),
    deps: &[],
};
/// The default provider: `ShaderParams` and functions that return their
/// argument, composed by every program of a material without a shader.
pub(crate) static SHADER_DEFAULT: Module = Module {
    name: "shader_default",
    source: include_str!("shader_default.wgsl"),
    deps: &[&SHADER_CONTRACT],
};
/// The parameters of the default provider's programs: a zero block, no
/// binding.
pub(crate) static SHADER_PARAMS_NONE: Module = Module {
    name: "shader_params_none",
    source: include_str!("shader_params_none.wgsl"),
    deps: &[],
};
/// The parameters of a game's shader's programs: group 2's two blocks.
pub(crate) static SHADER_PARAMS_BOUND: Module = Module {
    name: "shader_params_bound",
    source: include_str!("shader_params_bound.wgsl"),
    deps: &[],
};
/// The scene depth provider of every pass but a game's shader's blended
/// draws on the Extended binding tier: none available.
pub(crate) static SHADER_SCENE_DEPTH_NONE: Module = Module {
    name: "shader_scene_depth_none",
    source: include_str!("shader_scene_depth_none.wgsl"),
    deps: &[&SHADER_CONTRACT],
};
/// The scene depth provider of a game's shader's blended draws on the
/// Extended binding tier: the opaque depth.
pub(crate) static SHADER_SCENE_DEPTH: Module = Module {
    name: "shader_scene_depth",
    source: include_str!("shader_scene_depth.wgsl"),
    deps: &[&SHADER_CONTRACT, &super::tiers::BIND_BLENDED_EXTENDED],
};

/// The names a game's module defines in place of the default provider's.
pub(crate) const CONTRACT_NAMES: [&str; 3] =
    ["ShaderParams", "material_vertex", "material_surface"];

#[cfg(test)]
pub(crate) mod tests;
