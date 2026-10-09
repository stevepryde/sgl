//! A game's material shaders (`Scene::add_shader`): one WGSL module that
//! defines `material_vertex`, `material_surface` and `struct ShaderParams`
//! (the package README's "Programmable surfaces"), which SGL3D composes
//! into its own programs. A shader changes what a surface is, never how
//! SGL3D renders it: it is content, not a setting.
use super::identity::ShaderId;

/// The most bytes a shader's `ShaderParams` may take.
pub const SHADER_PARAMS_MAX_BYTES: u32 = 4096;
/// The most loop iterations one call of a function of a shader may make:
/// nested loops' counts multiply, one loop after another's add, and a call
/// within a loop counts its callee's (AR-12). Six Gerstner components take
/// six.
pub const SHADER_LOOP_BUDGET: u64 = 256;

/// A game's WGSL module: `material_vertex`, `material_surface` and
/// `struct ShaderParams`, over the contract's structs, declaring only
/// `const`, `struct`, `alias` and `fn`. `Scene::add_shader` validates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderSource {
    pub wgsl: String,
    /// Names the shader in errors and GPU labels.
    pub label: String,
}

/// A material's shader and what culling must allow for it
/// (`SurfaceMaterial::shader`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialShader {
    pub shader: ShaderId,
    /// The farthest `material_vertex` moves any vertex from its rest
    /// position, in the mesh's units (the pose scales it as it scales the
    /// mesh); finite and nonnegative. Culling grows the bounds of the meshes
    /// drawn with the material, and of their sections, by it. 0 for a
    /// shader that moves nothing.
    pub displacement_bound: f32,
}

/// The uniform layout naga gives a shader's `ShaderParams`
/// (`Scene::shader_parameters_layout`): its size in bytes and each member's
/// name, offset and size, in declaration order. A game mirrors the block in
/// Rust and checks its mirror against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderParamsLayout {
    pub size: u32,
    pub fields: Vec<ShaderParamField>,
}

/// One member of `ShaderParams`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderParamField {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

/// What a game's module may not declare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForbiddenItem {
    /// A module-scope `var` (`var<private>`, `var<workgroup>`).
    Variable,
    /// A pipeline-overridable constant.
    Override,
    /// A resource binding (`@group`/`@binding`).
    Binding,
    /// An entry point (`@vertex`, `@fragment`, `@compute`).
    EntryPoint,
    /// An `enable`, `requires` or `diagnostic` directive.
    Directive,
}

/// Why `Scene::add_shader` refused a module. Nothing is added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShaderError {
    /// The module does not parse against the contract: a syntax error, an
    /// unknown name (SGL3D's declarations beyond the contract are not the
    /// game's to read) or a type error. `line` and `column` count from 1 in
    /// the game's source; 0 where the error has no place in it.
    Parse {
        message: String,
        line: u32,
        column: u32,
    },
    /// naga refused a program the module is composed into, such as a
    /// derivative reached from `material_vertex`, or `ShaderParams` breaking
    /// the uniform layout's rules.
    Validate { message: String },
    /// `material_vertex`, `material_surface` or `ShaderParams` is missing.
    MissingFunction { name: &'static str },
    /// `name` is declared, but not as `expected`.
    Signature {
        name: &'static str,
        expected: &'static str,
    },
    /// A declaration the module may not make.
    Forbidden { item: ForbiddenItem, name: String },
    /// `discard` in `function`: a fragment's coverage has one owner, its
    /// surface's base colour alpha.
    Discard { function: String },
    /// A loop in `function` that is not a counted loop: a counter of `i32`
    /// or `u32` that starts at a literal or `const`, is compared with `<` or
    /// `<=` against a literal or `const` limit (or tested with `>=` or `>`
    /// in `break if`), and is changed only in the loop's `continuing` block,
    /// by adding a positive literal or `const` step, as `for` writes it.
    UnboundedLoop { function: String },
    /// One call of `function` makes more than `SHADER_LOOP_BUDGET` loop
    /// iterations.
    LoopBudget { function: String, iterations: u64 },
    /// `function`, which `material_vertex` is or calls, calls a scene depth
    /// function (`scene_depth_available`, `scene_depth`,
    /// `scene_depth_behind`): scene depth is `material_surface`'s.
    SceneDepthInVertex { function: String },
    /// `function` takes a derivative (`dpdx`, `dpdy`, `fwidth` and their
    /// forms), or calls a function that takes one, within an `if`, a
    /// `switch` or a loop (the right of `&&` and `||` among them), or after
    /// a `return` within one: WGSL allows derivatives only in uniform
    /// control flow, and a browser refuses a program that may take one
    /// elsewhere. Take it in the function's top-level statements and
    /// `select` on its value.
    NonUniformDerivative { function: String },
    /// A declaration whose name SGL3D's programs on either binding tier
    /// already use.
    NameTaken { name: String },
    /// `ShaderParams` takes more than `SHADER_PARAMS_MAX_BYTES`.
    ParamsTooLarge { size: u32, max: u32 },
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse {
                message,
                line,
                column,
            } => write!(f, "{line}:{column}: {message}"),
            Self::Validate { message } => f.write_str(message),
            Self::MissingFunction { name } => write!(f, "the shader declares no {name}"),
            Self::Signature { name, expected } => {
                write!(f, "{name} must be declared as {expected}")
            }
            Self::Forbidden { item, name } => {
                write!(f, "a shader may not declare {item:?} {name}")
            }
            Self::Discard { function } => write!(f, "{function} discards"),
            Self::UnboundedLoop { function } => {
                write!(f, "{function} has a loop that is not a counted loop")
            }
            Self::LoopBudget {
                function,
                iterations,
            } => write!(
                f,
                "{function} makes {iterations} loop iterations, more than {SHADER_LOOP_BUDGET}"
            ),
            Self::SceneDepthInVertex { function } => write!(
                f,
                "{function} reads the scene depth from material_vertex, which only material_surface may"
            ),
            Self::NonUniformDerivative { function } => write!(
                f,
                "{function} takes a derivative within control flow, which WGSL allows only in uniform control flow"
            ),
            Self::NameTaken { name } => write!(f, "SGL3D already declares {name}"),
            Self::ParamsTooLarge { size, max } => {
                write!(f, "ShaderParams takes {size} bytes, more than {max}")
            }
        }
    }
}

impl std::error::Error for ShaderError {}
