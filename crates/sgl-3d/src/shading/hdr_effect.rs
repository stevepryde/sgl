//! A game's HDR effect (`Scene::add_hdr_effect`; specs/sgl3d-architecture.md,
//! HDR effect contract): the contract's WGSL module, the pass's provider
//! the game's module is composed after, the frame's values it binds, and
//! the program `add_hdr_effect` validates and the stage creates its
//! pipeline from (`program`), so both see one text.
use super::programs::GAME_MODULE_HEADER;
use super::{Module, compose};

/// The struct a game's effect evaluates at, and its far depth.
pub(crate) static HDR_EFFECT_CONTRACT: Module = Module {
    name: "hdr_effect_contract",
    source: sgl_post_fx::embed_wgsl!("hdr_effect_contract.wgsl"),
    deps: &[&super::VIEW_DEPTH],
};
/// The contract's functions without bindings, which a game's module is
/// parsed against alone.
pub(crate) static HDR_EFFECT_INPUTS_NONE: Module = Module {
    name: "hdr_effect_inputs_none",
    source: sgl_post_fx::embed_wgsl!("hdr_effect_inputs_none.wgsl"),
    deps: &[&HDR_EFFECT_CONTRACT],
};
/// The pass: its bindings, the contract's functions over them and its
/// fragment entry point, which calls the game's `hdr_effect`.
pub(crate) static HDR_EFFECT_PASS: Module = Module {
    name: "hdr_effect_pass",
    source: sgl_post_fx::embed_wgsl!("hdr_effect_pass.wgsl"),
    deps: &[
        &HDR_EFFECT_CONTRACT,
        &super::FULLSCREEN_VS,
        &super::SCENE_COLOR,
        &super::FINITE,
    ],
};
/// The pass's fragment entry point.
pub(crate) const FRAGMENT_ENTRY: &str = "hdr_effect_fs";

/// The names a game's module defines.
pub(crate) const CONTRACT_NAMES: [&str; 2] = ["EffectParams", "hdr_effect"];
/// The contract's functions, which a game's module calls and does not
/// define.
pub(crate) const CONTRACT_FUNCTIONS: [&str; 3] =
    ["effect_color", "effect_view_depth", "effect_distance"];

/// An effect that returns the colour it is given, which SGL3D's own
/// program is composed with where it needs one with no game's module: the
/// names it reserves.
pub(crate) const IDENTITY: &str = "struct EffectParams { unused: f32 }
fn hdr_effect(color: vec3<f32>, ctx: EffectContext, params: EffectParams) -> vec3<f32> {
    return color;
}";

/// The pass's program with the game's module `source` composed last.
pub(crate) fn program(source: &str) -> String {
    let mut program = compose(&[&HDR_EFFECT_PASS]);
    program.push_str(GAME_MODULE_HEADER);
    program.push_str(source);
    program.push('\n');
    program
}

/// `HdrEffectFrame` in `hdr_effect_pass.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct HdrEffectFrameUniform {
    pub projection: [[f32; 4]; 4],
    pub view_from_clip: [[f32; 4]; 4],
    pub world_from_view: [[f32; 4]; 4],
    pub eye: [f32; 3],
    pub time: f32,
    pub size: [f32; 2],
    pub phase: f32,
    pub _padding: f32,
}

/// The layouts this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<super::layout_tests::Mirror> {
    use super::layout_tests::mirror;
    vec![mirror!(
        "hdr_effect",
        "HdrEffectFrame",
        HdrEffectFrameUniform,
        [
            projection,
            view_from_clip,
            world_from_view,
            eye,
            time,
            size,
            phase
        ]
    )]
}
