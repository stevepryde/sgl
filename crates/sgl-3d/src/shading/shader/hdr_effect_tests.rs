//! What `Scene::add_hdr_effect` refuses and accepts of a game's module,
//! through the validation it runs (`validate_hdr_effect`): the typed
//! refusal the HDR effect contract gives each kind of module, and the
//! layout naga gives `EffectParams`. CPU only: naga parses and validates
//! the program.
use super::validate_hdr_effect;
use crate::content::shader::{
    ForbiddenItem, HDR_EFFECT_PARAMS_MAX_BYTES, ShaderError, ShaderParamField,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// An effect that reads every part of the contract: the colour, each
/// context member, the three functions, the far depth, a derivative in its
/// top-level statements and a counted loop in a helper.
pub(crate) const FIXTURE: &str = r#"
struct EffectParams {
 tint:vec3<f32>,
 density:f32,
 taps:u32,
}
const TAPS:u32=4u;
fn fixture_blur(uv:vec2<f32>,size:vec2<f32>)->vec3<f32> {
 var sum=vec3(0.);
 for (var i=0u;i<TAPS;i++) {
  sum+=effect_color(uv+vec2(f32(i)/size.x,0.));
 }
 return sum/f32(TAPS);
}
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 let edge=fwidth(ctx.view_depth);
 let far=select(1.,0.,effect_view_depth(ctx.uv)>=SCENE_DEPTH_FAR);
 let along=effect_distance(ctx.uv)*far+ctx.distance*0.;
 let lit=fixture_blur(ctx.uv,ctx.size)*ctx.exposure+ctx.ray*0.+ctx.eye*0.;
 return mix(color,lit*params.tint,exp(-params.density*along))+vec3(edge*0.,ctx.pixel.x*0.,ctx.time*0.);
}
"#;

const PARAMS: &str = "struct EffectParams { value:vec4<f32>, }\n";
const IDENTITY: &str = "fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {\n return color;\n}\n";

/// A module of `PARAMS`, the identity effect and `extra`.
fn with(extra: &str) -> String {
    format!("{PARAMS}{IDENTITY}{extra}")
}

fn refused(source: &str) -> ShaderError {
    validate_hdr_effect(source).expect_err("the module is refused")
}

// Plausible defect: an effect that reads the whole contract refused, or
// one its program rejects accepted. The oracle is the contract: the
// fixture uses only what it lists.
#[wasm_bindgen_test(unsupported = test)]
fn the_effect_contract_is_accepted() {
    validate_hdr_effect(FIXTURE).unwrap_or_else(|error| panic!("{error}"));
    validate_hdr_effect(&with("")).unwrap_or_else(|error| panic!("{error}"));
}

// Plausible defects: the effect's contract not held as a material shader's
// is (a missing or mis-signed function accepted, the material contract's
// names readable), or a module the program would reject accepted. The
// oracle is the HDR effect contract's list of refusals.
#[wasm_bindgen_test(unsupported = test)]
fn the_effect_contract_is_held_to() {
    assert_eq!(
        refused(PARAMS),
        ShaderError::MissingFunction { name: "hdr_effect" }
    );
    assert_eq!(
        refused(IDENTITY),
        ShaderError::MissingFunction {
            name: "EffectParams"
        }
    );
    let expected =
        "fn hdr_effect(color: vec3<f32>, ctx: EffectContext, params: EffectParams) -> vec3<f32>";
    for wrong in [
        "fn hdr_effect(color:vec4<f32>,ctx:EffectContext,params:EffectParams)->vec4<f32> { return color; }",
        "fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec4<f32> { return vec4(color,1.); }",
        "fn hdr_effect(color:vec3<f32>,params:EffectParams,ctx:EffectContext)->vec3<f32> { return color; }",
        "fn hdr_effect(color:vec3<f32>,ctx:EffectContext)->vec3<f32> { return color; }",
    ] {
        assert_eq!(
            refused(&format!("{PARAMS}{wrong}")),
            ShaderError::Signature {
                name: "hdr_effect",
                expected
            },
            "{wrong}"
        );
    }
    // The material contract's names are not the effect's.
    assert!(matches!(
        refused(&with(
            "fn reads(ctx:SurfaceContext)->f32 { return ctx.view_depth; }"
        )),
        ShaderError::Parse { .. }
    ));
    // Nor are the pass's own declarations, which the game's would replace.
    for taken in ["effect_frame", "effect_bound", "HdrEffectFrame", "finite"] {
        assert_eq!(
            refused(&with(&format!("const {taken}:f32=1.;"))),
            ShaderError::NameTaken { name: taken.into() },
            "{taken}"
        );
    }
    assert_eq!(
        refused(&with("@group(0) @binding(9) var<uniform> extra:vec4<f32>;")),
        ShaderError::Forbidden {
            item: ForbiddenItem::Binding,
            name: "extra".into()
        }
    );
    assert_eq!(
        refused(&format!(
            "{PARAMS}fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {{ if color.r<0. {{ discard; }} return color; }}"
        )),
        ShaderError::Discard {
            function: "hdr_effect".into()
        }
    );
    assert_eq!(
        refused(&format!(
            "{PARAMS}fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {{ if color.r>0. {{ return vec3(dpdx(color.r)); }} return color; }}"
        )),
        ShaderError::NonUniformDerivative {
            function: "hdr_effect".into()
        }
    );
}

// Plausible defect: a loop whose bound data can raise reaching a device
// in a full-screen pass, which runs it at every pixel (AR-12). The oracle
// is the counted-loop rule and SHADER_LOOP_BUDGET: a loop bounded by a
// parameter is unbounded, and a 17 by 17 nest makes 289 iterations, over
// the budget's 256.
#[wasm_bindgen_test(unsupported = test)]
fn effect_loops_are_counted_within_the_budget() {
    assert_eq!(
        refused(&format!(
            "{PARAMS}fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {{ var s=color; for (var i=0u;i<u32(params.value.x);i++) {{ s+=effect_color(ctx.uv); }} return s; }}"
        )),
        ShaderError::UnboundedLoop {
            function: "hdr_effect".into()
        }
    );
    assert_eq!(
        refused(&format!(
            "{PARAMS}fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {{ var s=color; for (var i=0;i<17;i++) {{ for (var j=0;j<17;j++) {{ s+=effect_color(ctx.uv); }} }} return s; }}"
        )),
        ShaderError::LoopBudget {
            function: "hdr_effect".into(),
            iterations: 289
        }
    );
}

// Plausible defects: a layout that is not WGSL's uniform layout, so the
// game's mirror of its parameters puts a field where the effect reads
// another; a block beyond what `FrameInput::hdr_effect` carries accepted.
// The oracle is WGSL's alignment rules: a vec3<f32> aligns to 16 and takes
// 12, a following f32 fills its last 4 bytes, a u32 then sits at 16, and a
// struct's size rounds up to its alignment of 16; 17 vec4s take 272 bytes,
// over HDR_EFFECT_PARAMS_MAX_BYTES.
#[wasm_bindgen_test(unsupported = test)]
fn the_effect_parameter_layout_is_wgsl_s() {
    let layout = validate_hdr_effect(FIXTURE).unwrap_or_else(|error| panic!("{error}"));
    let field = |name: &str, offset, size| ShaderParamField {
        name: name.into(),
        offset,
        size,
    };
    assert_eq!(layout.size, 32);
    assert_eq!(
        layout.fields,
        [
            field("tint", 0, 12),
            field("density", 12, 4),
            field("taps", 16, 4)
        ]
    );
    assert_eq!(
        refused(&format!(
            "struct EffectParams {{ big:array<vec4<f32>,17>, }}{IDENTITY}"
        )),
        ShaderError::ParamsTooLarge {
            size: 272,
            max: HDR_EFFECT_PARAMS_MAX_BYTES
        }
    );
}
