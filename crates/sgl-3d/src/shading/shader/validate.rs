//! What `Scene::add_shader` and `Scene::add_hdr_effect` check of a game's
//! module before they accept it, so that no program created from an
//! accepted module later fails WGSL validation: no directive (it cannot
//! follow SGL3D's library) and no name SGL3D's programs declare; that it
//! parses against its contract alone, which leaves SGL3D's other
//! declarations unknown to it; that it declares only constants, structs,
//! aliases and functions, among them its parameter block and functions as
//! the contract gives them, without `discard`, with counted loops within
//! the budget (`loops`), derivatives only in uniform control flow
//! (`derivatives`) and parameters within the contract's limit; and that
//! every program the device's tier composes it into (`programs`) parses
//! and validates with the naga wgpu creates its modules with. One validator
//! for both contracts (`Contract`): a material shader's (the shader
//! contract) and an HDR effect's (the HDR effect contract), the first with
//! the rules of its own a material's passes need. Bevy 9d12036 validates a
//! material's shader only when wgpu creates its module and logs the error
//! (crates/bevy_render/src/render_resource/pipeline_cache.rs 133–180);
//! validating at the add returns it to the game instead.
use super::super::bind::BindingTier;
use super::super::compose;
use super::super::hdr_effect;
use super::super::programs::{
    GAME_MODULE_HEADER, GeometryForm, ProgramShader, caster_program, geometry_program,
};
use super::contracts::{Contract, HDR_EFFECT, MATERIAL, Type};
use super::tokens::{declared_names, leading_directive, tokens};
use super::{CONTRACT_NAMES, SHADER_DEFAULT, derivatives, loops};
use crate::content::shader::{ForbiddenItem, ShaderError, ShaderParamField, ShaderParamsLayout};
use std::collections::HashSet;
use std::sync::OnceLock;

/// What validation learns of an accepted module.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedShader {
    pub layout: ShaderParamsLayout,
    /// `material_surface` reaches `scene_volume_path`, itself or through a
    /// function it calls: the shader reads its volume path, so its blended
    /// materials' meshes are drawn into the volume layers.
    pub reads_volume_path: bool,
    /// One of its functions reads the frame's time from a context
    /// (`reads_time`), so what it does may change from frame to frame.
    pub reads_time: bool,
}

/// The contract's scene depth functions, which a game's module calls and
/// does not define.
pub(super) const CONTRACT_FUNCTIONS: [&str; 4] = [
    "scene_depth_available",
    "scene_depth",
    "scene_depth_behind",
    VOLUME_PATH,
];

/// The predeclared types naga 30 resolves (`front::wgsl::parse::conv`,
/// `map_predeclared_type`) that its `keywords::wgsl::BUILTIN_IDENTIFIERS`
/// leaves out: WGSL's vector and matrix aliases, and naga's 16-bit integers
/// and ray query types.
const PREDECLARED_TYPES: [&str; 34] = [
    "i16",
    "u16",
    "vec2i",
    "vec3i",
    "vec4i",
    "vec2u",
    "vec3u",
    "vec4u",
    "vec2f",
    "vec3f",
    "vec4f",
    "vec2h",
    "vec3h",
    "vec4h",
    "mat2x2f",
    "mat2x3f",
    "mat2x4f",
    "mat3x2f",
    "mat3x3f",
    "mat3x4f",
    "mat4x2f",
    "mat4x3f",
    "mat4x4f",
    "mat2x2h",
    "mat2x3h",
    "mat2x4h",
    "mat3x2h",
    "mat3x3h",
    "mat3x4h",
    "mat4x2h",
    "mat4x3h",
    "mat4x4h",
    "RayDesc",
    "RayIntersection",
];

/// The scene depth function that measures a volume's path.
const VOLUME_PATH: &str = "scene_volume_path";

/// The validator wgpu creates modules with (`ValidationFlags::all()`), with
/// the capabilities SGL3D's programs need and nothing more, so a game's
/// module reaches no feature a device may lack: cube array textures, and
/// WGSL's core `pack2x16float` and `unpack2x16float`, which screen-space
/// indirect light's kept result packs with and which wgpu grants every
/// WebGPU-compliant adapter (`DownlevelFlags::SHADER_F16_IN_F32`).
pub(crate) fn validator() -> naga::valid::Validator {
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::CUBE_ARRAY_TEXTURES
            | naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32,
    )
}

/// What the checks common to both contracts learn of a module that passed
/// those before its parameter block's layout and its programs.
struct Parsed {
    module: naga::Module,
    info: naga::valid::ModuleInfo,
    params: naga::Handle<naga::Type>,
}

/// `source`, a game's material shader module, validated for a device of
/// `tier`.
pub(crate) fn validate(source: &str, tier: BindingTier) -> Result<ValidatedShader, ShaderError> {
    let Parsed {
        module,
        info,
        params,
    } = parse_checked(&MATERIAL, source)?;
    if let Some(function) = reaches(&module, "material_vertex", &CONTRACT_FUNCTIONS) {
        return Err(ShaderError::SceneDepthInVertex { function });
    }
    let reads_volume_path = reaches(&module, "material_surface", &[VOLUME_PATH]).is_some();
    let reads_time = reads_time(&module, &info);
    let layout = finish(&MATERIAL, source, tier, &module, params)?;
    Ok(ValidatedShader {
        layout,
        reads_volume_path,
        reads_time,
    })
}

/// `source`, a game's HDR effect module, validated: its parameter block's
/// layout. Its one program is the same on either binding tier.
pub(crate) fn validate_hdr_effect(source: &str) -> Result<ShaderParamsLayout, ShaderError> {
    let Parsed { module, params, .. } = parse_checked(&HDR_EFFECT, source)?;
    finish(&HDR_EFFECT, source, BindingTier::Basic, &module, params)
}

/// The checks of `contract` on `source` up to its parameter block's layout
/// and its programs.
fn parse_checked(contract: &Contract, source: &str) -> Result<Parsed, ShaderError> {
    let tokens = tokens(source);
    if let Some(directive) = leading_directive(&tokens) {
        return Err(ShaderError::Forbidden {
            item: ForbiddenItem::Directive,
            name: directive,
        });
    }
    let reserved = (contract.reserved)();
    let declared = declared_names(&tokens);
    // WGSL lets a module-scope declaration shadow a predeclared type,
    // enumerant or built-in function, and naga resolves the module's own
    // first, so a game's `smoothstep` would replace the one SGL3D's
    // programs call: those names are SGL3D's too.
    let builtin = |name: &String| {
        naga::keywords::wgsl::BUILTIN_IDENTIFIERS.contains(&name.as_str())
            || PREDECLARED_TYPES.contains(&name.as_str())
    };
    if let Some(name) = declared
        .iter()
        .find(|name| reserved.contains(*name) || builtin(name))
    {
        return Err(ShaderError::NameTaken { name: name.clone() });
    }
    // Missing before parsing, where an undeclared parameter block the
    // functions name would read as an unknown name.
    if let Some(&name) = contract
        .names
        .iter()
        .find(|&&name| !declared.iter().any(|declared| declared == name))
    {
        return Err(ShaderError::MissingFunction { name });
    }
    let module = parse_against_contract(contract, source)?;
    let info = validator()
        .validate(&module)
        .map_err(|error| validation(&error))?;
    forbidden(&module)?;
    let params = signatures(contract, &module)?;
    let game = |name: &str| !contract.provided.contains(&name);
    for (_, function) in module.functions.iter() {
        let name = function.name.clone().unwrap_or_default();
        if !game(&name) {
            continue;
        }
        if discards(&function.body) {
            return Err(ShaderError::Discard { function: name });
        }
    }
    loops::check(&module, game)?;
    derivatives::check(&module, game)?;
    Ok(Parsed {
        module,
        info,
        params,
    })
}

/// The parameter block's layout of `module`, within `contract`'s limit,
/// once every program a device of `tier` composes `source` into parses and
/// validates.
fn finish(
    contract: &Contract,
    source: &str,
    tier: BindingTier,
    module: &naga::Module,
    params: naga::Handle<naga::Type>,
) -> Result<ShaderParamsLayout, ShaderError> {
    let layout = params_layout(module, params, contract.max_params)?;
    for (label, program) in (contract.programs)(tier, source) {
        let module =
            naga::front::wgsl::parse_str(&program).map_err(|error| ShaderError::Validate {
                message: format!("{label}: {}", error.message()),
            })?;
        validator()
            .validate(&module)
            .map_err(|error| validation(&error))?;
    }
    Ok(layout)
}

/// Whether a function of `module`, the game's or the contract's, reads the
/// `time` or `phase` member of a `VertexContext` or `SurfaceContext`, by
/// value or through a pointer, whether or not its material functions reach
/// it: a conservative test, as Godot b130438 marks a material animated where
/// its code reads TIME (`is_animated()`, scene_shader_forward_clustered.cpp
/// 250–252). Every read of a struct member is an access by constant index.
fn reads_time(module: &naga::Module, info: &naga::valid::ModuleInfo) -> bool {
    let timed = |ty: naga::Handle<naga::Type>, index: u32| {
        let ty = match module.types[ty].inner {
            naga::TypeInner::Pointer { base, .. } => base,
            _ => ty,
        };
        let naga::TypeInner::Struct { members, .. } = &module.types[ty].inner else {
            return false;
        };
        matches!(
            module.types[ty].name.as_deref(),
            Some("VertexContext" | "SurfaceContext")
        ) && members
            .get(index as usize)
            .is_some_and(|member| matches!(member.name.as_deref(), Some("time" | "phase")))
    };
    module.functions.iter().any(|(handle, function)| {
        let info = &info[handle];
        function.expressions.iter().any(|(_, expression)| {
            let naga::Expression::AccessIndex { base, index } = *expression else {
                return false;
            };
            match &info[base].ty {
                naga::proc::TypeResolution::Handle(ty) => timed(*ty, index),
                naga::proc::TypeResolution::Value(naga::TypeInner::Pointer { base, .. }) => {
                    timed(*base, index)
                }
                naga::proc::TypeResolution::Value(_) => false,
            }
        })
    })
}

/// Every program a device of `tier` composes `shader` into, by label: the
/// geometry program of each form that differs (`GeometryForm::distinct`)
/// and the caster program, with the occlusion and specular maps' fallback
/// provider. Their sampling provider (`Variant::occlusion_specular`), which
/// a game's module may also be composed with, differs only in that
/// provider's function bodies, which name SGL3D's declarations alone, all
/// of them reserved from a game's module (`reserved_names`), so a module
/// these programs accept is accepted there too; the layout tests validate
/// those programs with a game's module.
pub(crate) fn programs(tier: BindingTier, shader: ProgramShader<'_>) -> Vec<(String, String)> {
    use crate::shading::programs::CameraShadows::{ContactMask, Maps, ShadowMask};
    let mut programs = vec![(
        "geometry".to_owned(),
        geometry_program(GeometryForm::Plain, tier, shader, false),
    )];
    for (label, shadows, screen_indirect) in [
        ("geometry with the shadow mask", ShadowMask, false),
        ("geometry with the contact mask", ContactMask, false),
        ("geometry with screen-space indirect light", Maps, true),
        (
            "geometry with the shadow mask and screen-space indirect light",
            ShadowMask,
            true,
        ),
        (
            "geometry with the contact mask and screen-space indirect light",
            ContactMask,
            true,
        ),
    ] {
        let form = GeometryForm::Camera {
            shadows,
            screen_indirect,
        };
        programs.push((
            label.to_owned(),
            geometry_program(form, tier, shader, false),
        ));
    }
    if GeometryForm::Blended.distinct(tier, shader) {
        programs.push((
            "blended geometry".to_owned(),
            geometry_program(GeometryForm::Blended, tier, shader, false),
        ));
    }
    programs.push(("casters".to_owned(), caster_program(tier, shader, false)));
    programs
}

/// A validation error with what it holds, outermost first.
fn validation(error: &dyn std::error::Error) -> ShaderError {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        message.push_str(": ");
        message.push_str(&inner.to_string());
        source = inner.source();
    }
    ShaderError::Validate { message }
}

/// `source` composed after `contract`'s modules and the functions it may
/// call, parsed: a parse error placed in the game's source.
fn parse_against_contract(contract: &Contract, source: &str) -> Result<naga::Module, ShaderError> {
    let mut program = compose(contract.context);
    program.push_str(GAME_MODULE_HEADER);
    let before = program.matches('\n').count() as u32;
    program.push_str(source);
    program.push('\n');
    naga::front::wgsl::parse_str(&program).map_err(|error| {
        let (line, column) = error
            .location(&program)
            .filter(|location| location.line_number > before)
            .map_or((0, 0), |location| {
                (location.line_number - before, location.line_position)
            });
        ShaderError::Parse {
            message: error.message().to_owned(),
            line,
            column,
        }
    })
}

/// Refuses a module-scope variable, binding, override or entry point.
fn forbidden(module: &naga::Module) -> Result<(), ShaderError> {
    let named = |name: &Option<String>| name.clone().unwrap_or_default();
    if let Some(entry) = module.entry_points.first() {
        return Err(ShaderError::Forbidden {
            item: ForbiddenItem::EntryPoint,
            name: entry.name.clone(),
        });
    }
    if let Some((_, variable)) = module.global_variables.iter().next() {
        let item = if variable.binding.is_some() {
            ForbiddenItem::Binding
        } else {
            ForbiddenItem::Variable
        };
        return Err(ShaderError::Forbidden {
            item,
            name: named(&variable.name),
        });
    }
    if let Some((_, constant)) = module.overrides.iter().next() {
        return Err(ShaderError::Forbidden {
            item: ForbiddenItem::Override,
            name: named(&constant.name),
        });
    }
    Ok(())
}

/// The named type `name`.
fn named_type(module: &naga::Module, name: &str) -> Option<naga::Handle<naga::Type>> {
    module
        .types
        .iter()
        .find(|(_, ty)| ty.name.as_deref() == Some(name))
        .map(|(handle, _)| handle)
}

/// `contract`'s parameter block a struct, and its functions with the
/// contract's signatures. Returns the parameter block.
fn signatures(
    contract: &Contract,
    module: &naga::Module,
) -> Result<naga::Handle<naga::Type>, ShaderError> {
    let name = contract.names[0];
    let params = named_type(module, name).ok_or(ShaderError::MissingFunction { name })?;
    if !matches!(module.types[params].inner, naga::TypeInner::Struct { .. }) {
        return Err(ShaderError::Signature {
            name,
            expected: contract.params_expected,
        });
    }
    let of = |ty: Type| match ty {
        Type::Named(name) => named_type(module, name),
        Type::Params => Some(params),
        Type::Vec3F32 => module
            .types
            .iter()
            .find(|(_, ty)| {
                ty.name.is_none()
                    && ty.inner
                        == naga::TypeInner::Vector {
                            size: naga::VectorSize::Tri,
                            scalar: naga::Scalar::F32,
                        }
            })
            .map(|(handle, _)| handle),
    };
    for function in contract.functions {
        let name = function.name;
        let (_, declared) = module
            .functions
            .iter()
            .find(|(_, declared)| declared.name.as_deref() == Some(name))
            .ok_or(ShaderError::MissingFunction { name })?;
        let arguments: Vec<_> = declared
            .arguments
            .iter()
            .map(|argument| Some(argument.ty))
            .collect();
        let expected: Vec<_> = function.arguments.iter().map(|&ty| of(ty)).collect();
        let result = declared.result.as_ref().map(|result| result.ty);
        if expected.iter().any(Option::is_none)
            || arguments != expected
            || result.is_none()
            || result != of(function.result)
        {
            return Err(ShaderError::Signature {
                name,
                expected: function.expected,
            });
        }
    }
    Ok(params)
}

/// Whether `block` discards.
fn discards(block: &naga::Block) -> bool {
    block.iter().any(|statement| match statement {
        naga::Statement::Kill => true,
        naga::Statement::Block(inner) => discards(inner),
        naga::Statement::If { accept, reject, .. } => discards(accept) || discards(reject),
        naga::Statement::Switch { cases, .. } => cases.iter().any(|case| discards(&case.body)),
        naga::Statement::Loop {
            body, continuing, ..
        } => discards(body) || discards(continuing),
        _ => false,
    })
}

/// `params`' layout as naga lays out WGSL's host-shareable structs, which a
/// uniform block takes when it meets the uniform rules the programs'
/// validation checks; within `max` bytes.
fn params_layout(
    module: &naga::Module,
    params: naga::Handle<naga::Type>,
    max: u32,
) -> Result<ShaderParamsLayout, ShaderError> {
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|error| validation(&error))?;
    let naga::TypeInner::Struct { members, span } = &module.types[params].inner else {
        unreachable!("the parameter block was found a struct");
    };
    if *span > max {
        return Err(ShaderError::ParamsTooLarge { size: *span, max });
    }
    Ok(ShaderParamsLayout {
        size: *span,
        fields: members
            .iter()
            .map(|member| ShaderParamField {
                name: member.name.clone().unwrap_or_default(),
                offset: member.offset,
                size: layouter[member.ty].size,
            })
            .collect(),
    })
}

/// The function that calls one of `targets` where `root` reaches one,
/// itself or through a function it calls. `material_vertex` may reach no
/// scene depth function: scene depth is the surface function's, and the
/// binding it reads is the blended draws' fragment stage's alone, so a
/// vertex stage that reads it would fail when its pipeline is created.
fn reaches(module: &naga::Module, root: &str, targets: &[&str]) -> Option<String> {
    let named = |handle: naga::Handle<naga::Function>| {
        module.functions[handle].name.clone().unwrap_or_default()
    };
    let mut pending: Vec<_> = module
        .functions
        .iter()
        .filter(|&(handle, _)| named(handle) == root)
        .map(|(handle, _)| handle)
        .collect();
    let mut seen = HashSet::new();
    while let Some(handle) = pending.pop() {
        if !seen.insert(handle) {
            continue;
        }
        let mut callees = Vec::new();
        calls(&module.functions[handle].body, &mut callees);
        for callee in callees {
            if targets.contains(&named(callee).as_str()) {
                return Some(named(handle));
            }
            pending.push(callee);
        }
    }
    None
}

/// The functions `block` calls, at any depth, into `callees`.
fn calls(block: &naga::Block, callees: &mut Vec<naga::Handle<naga::Function>>) {
    for statement in block.iter() {
        match statement {
            naga::Statement::Call { function, .. } => callees.push(*function),
            naga::Statement::Block(inner) => calls(inner, callees),
            naga::Statement::If { accept, reject, .. } => {
                calls(accept, callees);
                calls(reject, callees);
            }
            naga::Statement::Switch { cases, .. } => {
                for case in cases {
                    calls(&case.body, callees);
                }
            }
            naga::Statement::Loop {
                body, continuing, ..
            } => {
                calls(body, callees);
                calls(continuing, callees);
            }
            _ => {}
        }
    }
}

/// The names SGL3D's programs on a device of either binding tier declare
/// at module scope, but those a game's module defines: each program parsed
/// with the default provider's text where a game's module goes. Both
/// tiers', so a module one tier accepts never takes a name the other's
/// programs declare. Found once.
pub(super) fn reserved_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = HashSet::new();
        let tiers = [BindingTier::Basic, BindingTier::Extended];
        let shader = ProgramShader::Game(SHADER_DEFAULT.source);
        for (label, program) in tiers.into_iter().flat_map(|tier| programs(tier, shader)) {
            let module = naga::front::wgsl::parse_str(&program)
                .unwrap_or_else(|error| panic!("{label}: {}", error.message()));
            names.extend(module_names(&module));
        }
        for name in CONTRACT_NAMES {
            names.remove(name);
        }
        names
    })
}

/// The names an HDR effect's program declares at module scope, but those
/// a game's module defines: the program parsed with an effect that returns
/// its colour. Found once.
pub(super) fn effect_reserved_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let program = hdr_effect::program(hdr_effect::IDENTITY);
        let module = naga::front::wgsl::parse_str(&program)
            .unwrap_or_else(|error| panic!("HDR effect: {}", error.message()));
        let mut names = module_names(&module);
        for name in hdr_effect::CONTRACT_NAMES {
            names.remove(name);
        }
        names
    })
}

/// The names `module` declares at module scope.
fn module_names(module: &naga::Module) -> HashSet<String> {
    let mut names = HashSet::new();
    let name = |name: &Option<String>| name.clone();
    names.extend(module.types.iter().filter_map(|(_, ty)| name(&ty.name)));
    names.extend(module.constants.iter().filter_map(|(_, c)| name(&c.name)));
    names.extend(module.overrides.iter().filter_map(|(_, o)| name(&o.name)));
    names.extend(
        module
            .global_variables
            .iter()
            .filter_map(|(_, variable)| name(&variable.name)),
    );
    names.extend(module.functions.iter().filter_map(|(_, f)| name(&f.name)));
    names.extend(module.entry_points.iter().map(|entry| entry.name.clone()));
    names
}
