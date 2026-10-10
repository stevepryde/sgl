//! What `Scene::add_shader` checks of a game's module before it accepts it,
//! so that no program created from an accepted module later fails WGSL
//! validation: no directive (it cannot follow SGL3D's library) and no name
//! SGL3D's programs declare; that it parses against the contract alone,
//! which leaves SGL3D's other declarations unknown to it; that it declares
//! only constants, structs, aliases and functions, among them
//! `ShaderParams`, `material_vertex` and `material_surface` as the contract
//! gives them, without `discard`, with counted loops within the budget
//! (`loops`), derivatives only in uniform control flow (`derivatives`) and
//! parameters within `SHADER_PARAMS_MAX_BYTES`; and that every
//! program the device's tier composes it into (`programs`) parses and
//! validates with the naga wgpu creates its modules with. Bevy 9d12036
//! validates a material's shader only when wgpu creates its module and logs
//! the error (crates/bevy_render/src/render_resource/pipeline_cache.rs
//! 133–180); validating at the add returns it to the game instead.
use super::super::bind::BindingTier;
use super::super::compose;
use super::super::programs::{
    GAME_MODULE_HEADER, GeometryForm, ProgramShader, caster_program, geometry_program,
};
use super::{
    CONTRACT_NAMES, SHADER_CONTRACT, SHADER_DEFAULT, SHADER_SCENE_DEPTH_NONE, derivatives, loops,
};
use crate::content::shader::{
    ForbiddenItem, SHADER_PARAMS_MAX_BYTES, ShaderError, ShaderParamField, ShaderParamsLayout,
};
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
}

/// The contract's scene depth functions, which a game's module calls and
/// does not define.
const CONTRACT_FUNCTIONS: [&str; 4] = [
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
/// module reaches no feature a device may lack.
pub(crate) fn validator() -> naga::valid::Validator {
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::CUBE_ARRAY_TEXTURES,
    )
}

/// `source`, a game's module, validated for a device of `tier`.
pub(crate) fn validate(source: &str, tier: BindingTier) -> Result<ValidatedShader, ShaderError> {
    let tokens = tokens(source);
    if let Some(directive) = leading_directive(&tokens) {
        return Err(ShaderError::Forbidden {
            item: ForbiddenItem::Directive,
            name: directive,
        });
    }
    let reserved = reserved_names();
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
    // Missing before parsing, where an undeclared ShaderParams the
    // functions name would read as an unknown name.
    if let Some(&name) = CONTRACT_NAMES
        .iter()
        .find(|&&name| !declared.iter().any(|declared| declared == name))
    {
        return Err(ShaderError::MissingFunction { name });
    }
    let module = parse_against_contract(source)?;
    validator()
        .validate(&module)
        .map_err(|error| validation(&error))?;
    forbidden(&module)?;
    let params = signatures(&module)?;
    for (_, function) in module.functions.iter() {
        let name = function.name.clone().unwrap_or_default();
        if CONTRACT_FUNCTIONS.contains(&name.as_str()) {
            continue;
        }
        if discards(&function.body) {
            return Err(ShaderError::Discard { function: name });
        }
    }
    loops::check(&module, |name| !CONTRACT_FUNCTIONS.contains(&name))?;
    derivatives::check(&module, |name| !CONTRACT_FUNCTIONS.contains(&name))?;
    if let Some(function) = reaches(&module, "material_vertex", &CONTRACT_FUNCTIONS) {
        return Err(ShaderError::SceneDepthInVertex { function });
    }
    let reads_volume_path = reaches(&module, "material_surface", &[VOLUME_PATH]).is_some();
    let layout = params_layout(&module, params)?;
    for (label, program) in programs(tier, ProgramShader::Game(source)) {
        let module =
            naga::front::wgsl::parse_str(&program).map_err(|error| ShaderError::Validate {
                message: format!("{label}: {}", error.message()),
            })?;
        validator()
            .validate(&module)
            .map_err(|error| validation(&error))?;
    }
    Ok(ValidatedShader {
        layout,
        reads_volume_path,
    })
}

/// Every program a device of `tier` composes `shader` into, by label: the
/// geometry program of each form that differs (`GeometryForm::distinct`)
/// and the caster program.
pub(crate) fn programs(
    tier: BindingTier,
    shader: ProgramShader<'_>,
) -> Vec<(&'static str, String)> {
    let mut programs = vec![
        (
            "geometry",
            geometry_program(GeometryForm::Plain, tier, shader),
        ),
        (
            "geometry with the shadow mask",
            geometry_program(GeometryForm::ShadowMask, tier, shader),
        ),
    ];
    if GeometryForm::Blended.distinct(tier, shader) {
        programs.push((
            "blended geometry",
            geometry_program(GeometryForm::Blended, tier, shader),
        ));
    }
    programs.push(("casters", caster_program(shader)));
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

/// `source` composed after the contract and the scene depth functions it
/// may call, parsed: a parse error placed in the game's source.
fn parse_against_contract(source: &str) -> Result<naga::Module, ShaderError> {
    let mut program = compose(&[&SHADER_CONTRACT, &SHADER_SCENE_DEPTH_NONE]);
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

const PARAMS_EXPECTED: &str = "struct ShaderParams { ... }";
const VERTEX_EXPECTED: &str = "fn material_vertex(v: MaterialVertex, ctx: VertexContext, params: ShaderParams) -> MaterialVertex";
const SURFACE_EXPECTED: &str = "fn material_surface(s: MaterialSurface, ctx: SurfaceContext, params: ShaderParams) -> MaterialSurface";

/// `ShaderParams` a struct, and the two functions with the contract's
/// signatures. Returns `ShaderParams`.
fn signatures(module: &naga::Module) -> Result<naga::Handle<naga::Type>, ShaderError> {
    let params = named_type(module, "ShaderParams").ok_or(ShaderError::MissingFunction {
        name: "ShaderParams",
    })?;
    if !matches!(module.types[params].inner, naga::TypeInner::Struct { .. }) {
        return Err(ShaderError::Signature {
            name: "ShaderParams",
            expected: PARAMS_EXPECTED,
        });
    }
    let contract = |name| named_type(module, name).expect("the contract declares its structs");
    for (name, value, context, expected) in [
        (
            "material_vertex",
            contract("MaterialVertex"),
            contract("VertexContext"),
            VERTEX_EXPECTED,
        ),
        (
            "material_surface",
            contract("MaterialSurface"),
            contract("SurfaceContext"),
            SURFACE_EXPECTED,
        ),
    ] {
        let (_, function) = module
            .functions
            .iter()
            .find(|(_, function)| function.name.as_deref() == Some(name))
            .ok_or(ShaderError::MissingFunction { name })?;
        let arguments: Vec<_> = function
            .arguments
            .iter()
            .map(|argument| argument.ty)
            .collect();
        let result = function.result.as_ref().map(|result| result.ty);
        if arguments != [value, context, params] || result != Some(value) {
            return Err(ShaderError::Signature { name, expected });
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
/// validation checks; within `SHADER_PARAMS_MAX_BYTES`.
fn params_layout(
    module: &naga::Module,
    params: naga::Handle<naga::Type>,
) -> Result<ShaderParamsLayout, ShaderError> {
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|error| validation(&error))?;
    let naga::TypeInner::Struct { members, span } = &module.types[params].inner else {
        unreachable!("ShaderParams was found a struct");
    };
    if *span > SHADER_PARAMS_MAX_BYTES {
        return Err(ShaderError::ParamsTooLarge {
            size: *span,
            max: SHADER_PARAMS_MAX_BYTES,
        });
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
fn reserved_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = HashSet::new();
        let tiers = [BindingTier::Basic, BindingTier::Extended];
        let shader = ProgramShader::Game(SHADER_DEFAULT.source);
        for (label, program) in tiers.into_iter().flat_map(|tier| programs(tier, shader)) {
            let module = naga::front::wgsl::parse_str(&program)
                .unwrap_or_else(|error| panic!("{label}: {}", error.message()));
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
        }
        for name in CONTRACT_NAMES {
            names.remove(name);
        }
        names
    })
}

/// A token of WGSL source with its comments removed: an identifier or
/// keyword, or one other character.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Mark(char),
}

/// `source`'s tokens, line and nested block comments skipped.
fn tokens(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut depth = 1;
                let mut previous = ' ';
                for next in chars.by_ref() {
                    match (previous, next) {
                        ('/', '*') => {
                            depth += 1;
                            previous = ' ';
                            continue;
                        }
                        ('*', '/') => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            previous = ' ';
                            continue;
                        }
                        _ => {}
                    }
                    previous = next;
                }
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if !(next.is_alphanumeric() || next == '_') {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
            c if c.is_whitespace() => {}
            c => tokens.push(Token::Mark(c)),
        }
    }
    tokens
}

/// The directive the module starts with, if any, up to its `;`.
fn leading_directive(tokens: &[Token]) -> Option<String> {
    let Some(Token::Word(word)) = tokens.first() else {
        return None;
    };
    if !matches!(word.as_str(), "enable" | "requires" | "diagnostic") {
        return None;
    }
    let mut directive = String::new();
    for token in tokens
        .iter()
        .take_while(|token| **token != Token::Mark(';'))
    {
        if !directive.is_empty() {
            directive.push(' ');
        }
        match token {
            Token::Word(word) => directive.push_str(word),
            Token::Mark(mark) => directive.push(*mark),
        }
    }
    Some(directive)
}

/// The names the module declares at module scope: each `fn`, `struct`,
/// `const`, `override`, `alias` and `var` outside any brace.
fn declared_names(tokens: &[Token]) -> Vec<String> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut at = 0;
    while at < tokens.len() {
        match &tokens[at] {
            Token::Mark('{') => depth += 1,
            Token::Mark('}') => depth = depth.saturating_sub(1),
            Token::Word(word)
                if depth == 0
                    && matches!(
                        word.as_str(),
                        "fn" | "struct" | "const" | "override" | "alias" | "var"
                    ) =>
            {
                let mut next = at + 1;
                // A variable's address space and access: `var<...>`.
                if word == "var" && tokens.get(next) == Some(&Token::Mark('<')) {
                    let mut angles = 0usize;
                    while let Some(token) = tokens.get(next) {
                        match token {
                            Token::Mark('<') => angles += 1,
                            Token::Mark('>') => {
                                angles -= 1;
                                if angles == 0 {
                                    next += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        next += 1;
                    }
                }
                if let Some(Token::Word(name)) = tokens.get(next) {
                    names.push(name.clone());
                }
                at = next;
            }
            _ => {}
        }
        at += 1;
    }
    names
}
