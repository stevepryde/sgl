//! The two contracts a game's module is validated against
//! (`validate`): a material shader's (the shader contract) and an HDR
//! effect's, each what the module defines, the functions it may call, its
//! parameter limit, what it is parsed against and composed into, and the
//! names it may not take.
use super::super::bind::BindingTier;
use super::super::hdr_effect;
use super::super::programs::ProgramShader;
use super::validate::{CONTRACT_FUNCTIONS, effect_reserved_names, programs, reserved_names};
use super::{CONTRACT_NAMES, SHADER_CONTRACT, SHADER_SCENE_DEPTH_NONE};
use crate::content::shader::{HDR_EFFECT_PARAMS_MAX_BYTES, SHADER_PARAMS_MAX_BYTES};
use std::collections::HashSet;

/// A function a contract has a game's module define: its name, its
/// arguments' and result's types, and how the contract writes it.
pub(super) struct Function {
    pub name: &'static str,
    pub arguments: [Type; 3],
    pub result: Type,
    pub expected: &'static str,
}

/// A type a contract's function takes or returns.
#[derive(Clone, Copy)]
pub(super) enum Type {
    /// A struct the contract declares.
    Named(&'static str),
    /// The module's parameter block.
    Params,
    /// `vec3<f32>`.
    Vec3F32,
}

/// What a contract asks of a game's module and composes it into.
pub(super) struct Contract {
    /// What the module defines, its parameter block first.
    pub names: &'static [&'static str],
    /// How the contract writes the parameter block.
    pub params_expected: &'static str,
    /// The contract's functions, which the module calls and does not define.
    pub provided: &'static [&'static str],
    pub functions: &'static [Function],
    /// The most bytes its parameter block may take.
    pub max_params: u32,
    /// The modules the game's module is parsed against alone.
    pub context: &'static [&'static super::super::Module],
    /// Every program a device of a tier composes the module into, by label.
    pub programs: fn(BindingTier, &str) -> Vec<(String, String)>,
    /// The names SGL3D's programs declare that the module may not.
    pub reserved: fn() -> &'static HashSet<String>,
}

/// A material shader's contract (the shader contract).
pub(super) static MATERIAL: Contract = Contract {
    names: &CONTRACT_NAMES,
    params_expected: "struct ShaderParams { ... }",
    provided: &CONTRACT_FUNCTIONS,
    functions: &[
        Function {
            name: "material_vertex",
            arguments: [
                Type::Named("MaterialVertex"),
                Type::Named("VertexContext"),
                Type::Params,
            ],
            result: Type::Named("MaterialVertex"),
            expected: VERTEX_EXPECTED,
        },
        Function {
            name: "material_surface",
            arguments: [
                Type::Named("MaterialSurface"),
                Type::Named("SurfaceContext"),
                Type::Params,
            ],
            result: Type::Named("MaterialSurface"),
            expected: SURFACE_EXPECTED,
        },
    ],
    max_params: SHADER_PARAMS_MAX_BYTES,
    context: &[&SHADER_CONTRACT, &SHADER_SCENE_DEPTH_NONE],
    programs: |tier, source| programs(tier, ProgramShader::Game(source)),
    reserved: reserved_names,
};

/// An HDR effect's contract.
pub(super) static HDR_EFFECT: Contract = Contract {
    names: &hdr_effect::CONTRACT_NAMES,
    params_expected: "struct EffectParams { ... }",
    provided: &hdr_effect::CONTRACT_FUNCTIONS,
    functions: &[Function {
        name: "hdr_effect",
        arguments: [Type::Vec3F32, Type::Named("EffectContext"), Type::Params],
        result: Type::Vec3F32,
        expected: EFFECT_EXPECTED,
    }],
    max_params: HDR_EFFECT_PARAMS_MAX_BYTES,
    context: &[&hdr_effect::HDR_EFFECT_INPUTS_NONE],
    programs: |_, source| vec![("HDR effect".into(), hdr_effect::program(source))],
    reserved: effect_reserved_names,
};

const VERTEX_EXPECTED: &str = "fn material_vertex(v: MaterialVertex, ctx: VertexContext, params: ShaderParams) -> MaterialVertex";
const SURFACE_EXPECTED: &str = "fn material_surface(s: MaterialSurface, ctx: SurfaceContext, params: ShaderParams) -> MaterialSurface";
const EFFECT_EXPECTED: &str =
    "fn hdr_effect(color: vec3<f32>, ctx: EffectContext, params: EffectParams) -> vec3<f32>";
