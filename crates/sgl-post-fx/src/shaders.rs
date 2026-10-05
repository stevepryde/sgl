//! The WGSL port of the DiligentFX shaders.
//!
//! `shaders/wgsl/` mirrors the upstream shader files one WGSL module per HLSL
//! file (`Shaders/...` of DiligentFX, `Graphics/ShaderTools/include/` of
//! DiligentCore, and the HLSL strings in `PostFXContext.cpp`). WGSL has no
//! preprocessor, so a shader is assembled here the way Diligent's shader
//! factory compiles one: `HLSLDefinitions.fxh` first, then the shader
//! macros, then the file, resolving the same `#include`, `#define`,
//! `#if`/`#ifdef`/`#ifndef`/`#elif`, `#else` and `#endif` lines. Includes name
//! the upstream file (`"SSR_Common.fxh"`); `#define NAME VALUE` becomes
//! `const NAME = VALUE;`. The preprocessor is the one written for the
//! FidelityFX port (`sp-fidelity`).
use std::collections::HashMap;

macro_rules! modules {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../shaders/wgsl/", $path)))),*]
    };
}

/// WGSL modules, by path relative to `shaders/wgsl/`.
const MODULES: &[(&str, &str)] = modules![
    "DiligentCore/HLSLDefinitions.wgsl",
    "Common/public/ShaderDefinitions.wgsl",
    "Common/public/BasicStructures.wgsl",
    "Common/public/ShaderUtilities.wgsl",
    "Common/public/PostFX_Common.wgsl",
    "Common/public/PBR_Common.wgsl",
    "Common/private/FullScreenTriangleVSOutput.wgsl",
    "Common/private/FullScreenTriangleVS.wgsl",
    "Common/private/ComputeBlueNoiseTexture.wgsl",
    "Common/private/ComputeReprojectedDepth.wgsl",
    "PostProcess/Common/src/PostFXContext_ScreenTriangleVS.wgsl",
    "PostProcess/Common/src/PostFXContext_CopyTexturePS.wgsl",
    "PostProcess/ScreenSpaceReflection/public/ScreenSpaceReflectionStructures.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_Common.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeHierarchicalDepthBuffer.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeStencilMaskAndExtractRoughness.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeDownsampledStencilMask.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeIntersection.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_DenoiserTiles.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeDenoiserTiles.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeSpatialReconstruction.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeTemporalAccumulation.wgsl",
    "PostProcess/ScreenSpaceReflection/private/SSR_ComputeBilateralCleanup.wgsl",
    "PostProcess/TemporalAntiAliasing/public/TemporalAntiAliasingStructures.wgsl",
    "PostProcess/TemporalAntiAliasing/private/TAA_ComputeTemporalAccumulation.wgsl",
];

/// The module of an upstream file name (`"SSR_Common.fxh"`,
/// `"FullScreenTriangleVS.fx"`, `"HLSLDefinitions.fxh"`) or a module path.
fn module(name: &str) -> &'static str {
    let stem = name.rsplit('/').next().unwrap_or(name);
    let stem = stem.split_once('.').map_or(stem, |(stem, _)| stem);
    MODULES
        .iter()
        .find(|(path, _)| {
            let file = path.rsplit('/').next().unwrap();
            file.strip_suffix(".wgsl") == Some(stem)
        })
        .unwrap_or_else(|| panic!("unknown WGSL module {name}"))
        .1
}

/// WGSL for one upstream shader file compiled with `macros`, as
/// `PostFXRenderTechnique::CreateShader` compiles the HLSL file with a
/// `ShaderMacroHelper`. Macro values are Diligent's (`"1"`, `"0"`, `"0.5"`).
pub fn shader_source(file_name: &str, macros: &[(&str, &str)]) -> String {
    let mut preprocessor = Preprocessor {
        defines: HashMap::new(),
        output: String::new(),
    };
    preprocessor.include("HLSLDefinitions.fxh");
    let prelude: String = macros
        .iter()
        .map(|(name, value)| format!("#define {name} {value}\n"))
        .collect();
    preprocessor.process("<shader macros>", &prelude);
    preprocessor.include(file_name);
    preprocessor.output
}

/// The subset of the C preprocessor the retained modules use.
struct Preprocessor {
    defines: HashMap<String, String>,
    output: String,
}

struct Conditional {
    parent_active: bool,
    active: bool,
    taken: bool,
}

impl Preprocessor {
    fn include(&mut self, path: &str) {
        let source = module(path);
        self.process(path, source);
    }

    fn process(&mut self, path: &str, source: &str) {
        let mut stack: Vec<Conditional> = Vec::new();
        for line in source.lines() {
            let active = stack.last().is_none_or(|c| c.active);
            let Some(directive) = line.trim_start().strip_prefix('#') else {
                if active {
                    self.output.push_str(line);
                    self.output.push('\n');
                }
                continue;
            };
            let directive = strip_comment(directive).trim();
            let (name, rest) = directive
                .split_once(char::is_whitespace)
                .map_or((directive, ""), |(n, r)| (n, r.trim()));
            match name {
                "if" | "ifdef" | "ifndef" => {
                    let value = active
                        && match name {
                            "if" => self.evaluate(rest) != 0,
                            "ifdef" => self.defines.contains_key(rest),
                            _ => !self.defines.contains_key(rest),
                        };
                    stack.push(Conditional {
                        parent_active: active,
                        active: value,
                        taken: value,
                    });
                }
                "elif" => {
                    let top = stack.last().expect("#elif without #if");
                    let value = top.parent_active && !top.taken && self.evaluate(rest) != 0;
                    let top = stack.last_mut().unwrap();
                    top.active = value;
                    top.taken |= value;
                }
                "else" => {
                    let top = stack.last_mut().expect("#else without #if");
                    top.active = top.parent_active && !top.taken;
                    top.taken = true;
                }
                "endif" => {
                    stack.pop().expect("#endif without #if");
                }
                _ if !active => {}
                "define" => {
                    let (name, value) = rest
                        .split_once(char::is_whitespace)
                        .map_or((rest, ""), |(n, v)| (n, v.trim()));
                    // C allows redefining a macro with an identical value.
                    match self.defines.get(name) {
                        Some(existing) if existing == value => continue,
                        Some(existing) => {
                            panic!("#define {name} {value} redefines {existing} in {path}")
                        }
                        None => {}
                    }
                    if !value.is_empty() {
                        self.output.push_str(&format!("const {name} = {value};\n"));
                    }
                    self.defines.insert(name.to_owned(), value.to_owned());
                }
                "include" => self.include(rest.trim_matches('"')),
                _ => panic!("unsupported directive #{name} in {path}"),
            }
        }
        assert!(stack.is_empty(), "unterminated #if in {path}");
    }

    /// Integer `#if` expression: `defined(NAME)`, defines, literals and the C
    /// operators; an undefined name is 0.
    fn evaluate(&self, condition: &str) -> i64 {
        let tokens = tokenize(condition);
        let mut expanded = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            if tokens[i] == "defined" {
                let parenthesized = tokens.get(i + 1).is_some_and(|t| t == "(");
                let name = &tokens[i + if parenthesized { 2 } else { 1 }];
                expanded.push(u8::from(self.defines.contains_key(name)).to_string());
                i += if parenthesized { 4 } else { 2 };
            } else {
                let value = self.defines.get(&tokens[i]).filter(|v| !v.is_empty());
                expanded.extend(value.map_or_else(|| vec![tokens[i].clone()], |v| tokenize(v)));
                i += 1;
            }
        }
        let mut position = 0;
        let value = parse_expression(&expanded, &mut position, 0);
        assert_eq!(
            position,
            expanded.len(),
            "trailing tokens in #if {condition}"
        );
        value
    }
}

fn strip_comment(line: &str) -> &str {
    line.find("//").map_or(line, |at| &line[..at])
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        } else {
            let pair: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            if ["&&", "||", "==", "!=", "<=", ">=", "<<", ">>"].contains(&pair.as_str()) {
                tokens.push(pair);
                i += 2;
            } else {
                tokens.push(c.to_string());
                i += 1;
            }
        }
    }
    tokens
}

fn binary_precedence(operator: &str) -> Option<u8> {
    Some(match operator {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | "<=" | ">" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

fn parse_expression(tokens: &[String], position: &mut usize, min_precedence: u8) -> i64 {
    let mut left = parse_unary(tokens, position);
    while let Some(operator) = tokens.get(*position) {
        let Some(precedence) = binary_precedence(operator).filter(|p| *p > min_precedence) else {
            break;
        };
        let operator = operator.clone();
        *position += 1;
        let right = parse_expression(tokens, position, precedence);
        left = match operator.as_str() {
            "||" => i64::from(left != 0 || right != 0),
            "&&" => i64::from(left != 0 && right != 0),
            "|" => left | right,
            "^" => left ^ right,
            "&" => left & right,
            "==" => i64::from(left == right),
            "!=" => i64::from(left != right),
            "<" => i64::from(left < right),
            "<=" => i64::from(left <= right),
            ">" => i64::from(left > right),
            ">=" => i64::from(left >= right),
            "<<" => left << right,
            ">>" => left >> right,
            "+" => left + right,
            "-" => left - right,
            "*" => left * right,
            "/" => left / right,
            _ => left % right,
        };
    }
    left
}

fn parse_unary(tokens: &[String], position: &mut usize) -> i64 {
    let token = tokens
        .get(*position)
        .expect("incomplete #if expression")
        .clone();
    *position += 1;
    match token.as_str() {
        "!" => i64::from(parse_unary(tokens, position) == 0),
        "-" => -parse_unary(tokens, position),
        "~" => !parse_unary(tokens, position),
        "(" => {
            let value = parse_expression(tokens, position, 0);
            assert_eq!(tokens.get(*position).map(String::as_str), Some(")"));
            *position += 1;
            value
        }
        _ if token.starts_with(|c: char| c.is_ascii_digit()) => token
            .trim_end_matches(['u', 'U', 'l', 'L'])
            .parse()
            .expect("integer in #if"),
        // Identifiers that are not macros evaluate to zero.
        _ => 0,
    }
}
