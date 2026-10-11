//! What `Scene::add_shader` refuses and accepts of a game's module, through
//! the validation it runs (`validate`), on both binding tiers: the typed
//! refusal the contract gives each kind of module, and the layout naga gives
//! `ShaderParams`. CPU only: naga parses and validates every program.
use super::validate;
use crate::content::shader::{
    ForbiddenItem, SHADER_PARAMS_MAX_BYTES, ShaderError, ShaderParamField,
};
use crate::shading::bind::BindingTier;
use wasm_bindgen_test::wasm_bindgen_test;

/// A game's module that reads every part of the contract: its parameters
/// in both functions, the vertex's data and the instance's, the time,
/// phase and evaluation it is, `custom` through to the surface, the scene
/// depth functions and the volume path with its bounds, a derivative in the
/// surface function and a counted loop in a helper.
pub(crate) const FIXTURE: &str = r#"
struct ShaderParams {
 amplitude:f32,
 frequency:f32,
 tint:vec2<f32>,
 waves:array<vec4<f32>,2>,
}
const COMPONENTS:u32=2u;
fn fixture_height(x:f32,params:ShaderParams,time:f32)->f32 {
 var height=0.;
 for (var i=0u;i<COMPONENTS;i++) {
  height+=params.waves[i].x*sin(x*params.frequency+time*params.waves[i].y);
 }
 return height;
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 let anchor=v.position.x+ctx.instance.x+v.shader_data.x;
 out.position.y+=params.amplitude*fixture_height(anchor,params,ctx.time+ctx.phase);
 out.custom=vec4(anchor,select(0.,1.,ctx.previous),ctx.model[3].x,1.);
 return out;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 let scale=(ctx.model_scale.x+ctx.model_scale.y+ctx.model_scale.z)/3.;
 let path=scene_volume_path(ctx);
 let measured=path.bound==VOLUME_EXIT || path.bound==VOLUME_ENTRY || path.bound==VOLUME_EYE;
 out.thickness=min(select(scene_depth_behind(ctx),path.length,measured)/scale,8.);
 out.base_color=vec4(s.base_color.rgb*vec3(params.tint,1.),s.base_color.a*select(0.5,1.,scene_depth_available()));
 out.roughness=clamp(s.roughness+abs(dpdx(ctx.position.x)),0.,1.);
 out.emission=s.emission*ctx.custom.w+vec3(0.,min(scene_depth(ctx.pixel),1.)*0.,ctx.time*0.);
 return out;
}
"#;

/// The fixture's functions, to which each case adds or in which it replaces
/// a declaration.
const FUNCTIONS: &str = r#"
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 return v;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
"#;
const PARAMS: &str = "struct ShaderParams { value:vec4<f32>, }\n";

/// A module of the identity functions, `PARAMS` and `extra`.
fn with(extra: &str) -> String {
    format!("{PARAMS}{FUNCTIONS}{extra}")
}

/// `source` validated on both tiers, which must agree.
fn validated(source: &str) -> Result<super::validate::ValidatedShader, ShaderError> {
    let extended = validate(source, BindingTier::Extended);
    let basic = validate(source, BindingTier::Basic);
    assert_eq!(
        extended.as_ref().err(),
        basic.as_ref().err(),
        "the tiers disagree on {source}"
    );
    extended
}

fn refused(source: &str) -> ShaderError {
    validated(source).expect_err("the module is refused")
}

// Plausible defect: a module that reads the whole contract refused, or one
// the programs it is composed into reject accepted. The oracle is the
// contract: the fixture uses only what it lists.
#[wasm_bindgen_test(unsupported = test)]
fn the_contract_is_accepted() {
    validated(FIXTURE).unwrap_or_else(|error| panic!("{error}"));
    validated(&with("")).unwrap_or_else(|error| panic!("{error}"));
}

// Plausible defects: a loop whose bound data can raise reaching a device,
// which can hang it (AR-12); a counted loop miscounted. The oracle is the
// counted-loop rule and SHADER_LOOP_BUDGET: a loop bounded by a parameter
// or a `while` is unbounded, as is a counter whose limit plus step, or
// start plus step, overflows its type, which wraps before it passes the
// limit (`i <= 4294967295u` holds for every u32); a 16 by 16 nest makes
// the budget's 256 iterations, a 17 by 17 one 289 and two loops of 200 one
// after the other 400, each over it.
#[wasm_bindgen_test(unsupported = test)]
fn loops_are_counted_within_the_budget() {
    let unbounded = |body: &str| {
        let error = refused(&with(&format!(
            "fn looped(params:ShaderParams)->f32 {{\n{body}\n}}"
        )));
        assert_eq!(
            error,
            ShaderError::UnboundedLoop {
                function: "looped".into()
            },
            "{body}"
        );
    };
    unbounded("var s=0.; for (var i=0u;i<u32(params.value.x);i++) { s+=1.; } return s;");
    unbounded("var s=0.; var i=0; while (i<4) { s+=1.; i++; } return s;");
    unbounded("var s=0.; for (var i=0;i<4;i++) { s+=1.; i--; } return s;");
    unbounded("var s=0.; loop { s+=1.; if s>4. { break; } } return s;");
    unbounded("var s=0.; for (var i=4294967290u;i<=4294967295u;i++) { s+=1.; } return s;");
    // A `break if` loop runs once whatever its counter holds: from a start
    // past its limit, its first step wraps below the limit (#446).
    unbounded(
        "var s=0.; var i=4294967295u; loop { s+=1.; continuing { i+=1u; break if i>4294967000u; } } return s;",
    );
    unbounded(
        "var s=0.; var i=2147483647; loop { s+=1.; continuing { i+=1; break if i>=2147483000; } } return s;",
    );
    // A `let` evaluates once where it stands (#287): a test or a step
    // computed before the loop reads the counter's first value for ever.
    unbounded(
        "var i=0u; let keep_going=i<4u; loop { if keep_going {} else { break; } continuing { i+=1u; } } return f32(i);",
    );
    unbounded(
        "var i=0u; let next=i+1u; loop { if i<4u {} else { break; } continuing { i=next; } } return f32(i);",
    );
    unbounded(
        "var i=0u; let done=i>=4u; loop { continuing { i+=1u; break if done; } } return f32(i);",
    );
    // A `break if` loop an outer loop enters again keeps its counter's last
    // value and runs once more each time, until the counter wraps.
    unbounded(
        "var s=0.; var i=4294967000u; for (var o=0u;o<4u;o++) { loop { s+=1.; continuing { i+=100u; break if i>=4294967100u; } } } return s;",
    );
    let nest = |n: u32| {
        format!(
            "fn nested()->f32 {{ var s=0.; for (var i=0u;i<{n}u;i++) {{ for (var j=0u;j<{n}u;j++) {{ s+=1.; }} }} return s; }}"
        )
    };
    validated(&with(&nest(16))).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        refused(&with(&nest(17))),
        ShaderError::LoopBudget {
            function: "nested".into(),
            iterations: 289
        }
    );
    assert_eq!(
        refused(&with(
            "fn twice()->f32 { var s=0.; for (var i=0;i<200;i++) { s+=1.; } for (var j=0;j<200;j++) { s+=1.; } return s; }"
        )),
        ShaderError::LoopBudget {
            function: "twice".into(),
            iterations: 400
        }
    );
    // A loop within a counted loop through a call counts its callee's.
    assert_eq!(
        refused(&with(
            "fn inner()->f32 { var s=0.; for (var i=0;i<=16;i+=1) { s+=1.; } return s; }
             fn outer()->f32 { var s=0.; for (var i=0;i<16;i++) { s+=inner(); } return s; }"
        )),
        ShaderError::LoopBudget {
            function: "outer".into(),
            iterations: 272
        }
    );
    // `break if` after the step: 4 iterations from 0 by 1 to 4; within an
    // outer loop, restarted by a store before it each time.
    validated(&with(
        "fn after()->f32 { var s=0.; var k=0; loop { s+=1.; continuing { k+=1; break if k>=4; } } return s; }",
    ))
    .unwrap_or_else(|error| panic!("{error}"));
    validated(&with(
        "fn again()->f32 { var s=0.; var k=0; for (var o=0;o<4;o++) { k=0; loop { s+=1.; continuing { k+=1; break if k>=4; } } } return s; }",
    ))
    .unwrap_or_else(|error| panic!("{error}"));
    validated(&with(
        "fn declared()->f32 { var s=0.; for (var o=0;o<4;o++) { var k=0; loop { s+=1.; continuing { k+=1; break if k>=4; } } } return s; }",
    ))
    .unwrap_or_else(|error| panic!("{error}"));
}

// Plausible defect: a module that would steal a binding, add a pass or
// override, end a fragment's coverage outside its base colour's alpha, or
// redefine a name SGL3D's programs use (its own, or a WGSL built-in they
// call) accepted, so a pipeline later fails or silently shades otherwise.
// The oracle is the contract's typed refusal for each.
#[wasm_bindgen_test(unsupported = test)]
fn forbidden_declarations_are_refused() {
    let forbidden = |item, name: &str| ShaderError::Forbidden {
        item,
        name: name.into(),
    };
    assert_eq!(
        refused(&with(
            "@group(1) @binding(0) var<uniform> stolen:vec4<f32>;"
        )),
        forbidden(ForbiddenItem::Binding, "stolen")
    );
    assert_eq!(
        refused(&with("var<private> counter:u32;")),
        forbidden(ForbiddenItem::Variable, "counter")
    );
    assert_eq!(
        refused(&with("override scale:f32=1.;")),
        forbidden(ForbiddenItem::Override, "scale")
    );
    assert_eq!(
        refused(&with(
            "@fragment fn extra()->@location(0) vec4<f32> { return vec4(1.); }"
        )),
        forbidden(ForbiddenItem::EntryPoint, "extra")
    );
    assert_eq!(
        refused(&format!("// a comment first\nenable f16;\n{}", with(""))),
        forbidden(ForbiddenItem::Directive, "enable f16")
    );
    assert_eq!(
        refused(&format!(
            "{PARAMS}fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{ return v; }}
             fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {{ if s.base_color.a<0.5 {{ discard; }} return s; }}"
        )),
        ShaderError::Discard {
            function: "material_surface".into()
        }
    );
    // `vertex` is SGL3D's (material_shader.wgsl), as is a contract struct.
    assert_eq!(
        refused(&with("fn vertex()->f32 { return 1.; }")),
        ShaderError::NameTaken {
            name: "vertex".into()
        }
    );
    assert_eq!(
        refused(&with("struct MaterialVertex { x:f32, }")),
        ShaderError::NameTaken {
            name: "MaterialVertex".into()
        }
    );
    // A WGSL built-in function, which a module-scope declaration would
    // replace in every SGL3D call to it (`smoothstep` in the film fade, the
    // decal normal fade and specular_trace_fade; `saturate` in
    // pbr_filtered_roughness): WGSL's own name, not one SGL3D declares.
    for builtin in [
        "fn smoothstep(a:f32,b:f32,x:f32)->f32 { return x; }",
        "fn saturate(x:f32)->f32 { return x; }",
        // A predeclared type alias, which naga's built-in list leaves out.
        "fn vec3f(x:f32)->vec3<f32> { return vec3(x); }",
    ] {
        let name = builtin[3..].split('(').next().unwrap();
        assert_eq!(
            refused(&with(builtin)),
            ShaderError::NameTaken { name: name.into() }
        );
    }
    // Hidden by WGSL's lexical rules as naga applies them: a line comment
    // ends at any line break, not only `\n`, and U+200E is blankspace.
    for hidden in [
        "// note\rfn smoothstep(a:f32,b:f32,x:f32)->f32 { return x; }",
        "// note\u{2028}fn smoothstep(a:f32,b:f32,x:f32)->f32 { return x; }",
        "fn\u{200e}smoothstep(a:f32,b:f32,x:f32)->f32 { return x; }",
    ] {
        assert_eq!(
            refused(&with(hidden)),
            ShaderError::NameTaken {
                name: "smoothstep".into()
            }
        );
    }
    // A name only the Extended tier's programs declare (its blended draws'
    // opaque depth, bind_blended_extended.wgsl), refused on Basic too: a
    // game's module runs on whichever tier a player's device has.
    let extended_only = with("const blended_scene_depth:f32=1.;");
    assert_eq!(
        validate(&extended_only, BindingTier::Basic).err(),
        Some(ShaderError::NameTaken {
            name: "blended_scene_depth".into()
        })
    );
}

// Plausible defect: a module missing a function, or with the wrong
// signature, reaching composition, where SGL3D's calls fail; a parse error
// placed in SGL3D's library rather than the game's line; SGL3D's globals
// readable; a derivative in the vertex function accepted. The oracle is
// the contract's signatures and the game's own source lines.
#[wasm_bindgen_test(unsupported = test)]
fn the_contract_is_held_to() {
    let surface = "fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface { return s; }";
    assert_eq!(
        refused(&format!("{PARAMS}{surface}")),
        ShaderError::MissingFunction {
            name: "material_vertex"
        }
    );
    assert_eq!(
        refused(FUNCTIONS),
        ShaderError::MissingFunction {
            name: "ShaderParams"
        }
    );
    assert!(matches!(
        refused(&format!(
            "{PARAMS}{surface}\nfn material_vertex(v:MaterialVertex,params:ShaderParams)->MaterialVertex {{ return v; }}"
        )),
        ShaderError::Signature {
            name: "material_vertex",
            ..
        }
    ));
    // Where `needle` is in `source`: its line and column, from 1.
    let place = |source: &str, needle: &str| {
        source
            .lines()
            .enumerate()
            .find_map(|(line, text)| Some((line as u32 + 1, text.find(needle)? as u32 + 1)))
            .unwrap()
    };
    let source = with("\nfn broken( {");
    match refused(&source) {
        ShaderError::Parse { line, .. } => assert_eq!(line, place(&source, "broken").0),
        error => panic!("{error:?}"),
    }
    let source = format!(
        "{PARAMS}{surface}\nfn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{\n var out=v;\n out.position.y+=frame.elapsed_seconds;\n return out;\n}}"
    );
    match refused(&source) {
        ShaderError::Parse { line, column, .. } => {
            assert_eq!((line, column), place(&source, "frame."))
        }
        error => panic!("{error:?}"),
    }
    assert!(matches!(
        refused(&format!(
            "{PARAMS}{surface}\nfn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{ var out=v; out.position.y+=dpdx(v.position.x); return out; }}"
        )),
        ShaderError::Validate { .. }
    ));
}

// Plausible defect: a vertex function that reads the scene depth, itself
// or through a function it calls, accepted, so the blended draws' pipeline,
// whose vertex stage would then read a binding only their fragment stage
// sees, fails when the material is first drawn. The oracle is the contract:
// scene depth is the surface function's, which may call the same helper.
#[wasm_bindgen_test(unsupported = test)]
fn scene_depth_is_the_surface_functions() {
    let helper = "fn depth_at(pixel:vec2<f32>)->f32 { return scene_depth(pixel); }\nfn path_of(ctx:SurfaceContext)->f32 { return scene_volume_path(ctx).length; }";
    let module = |vertex: &str, surface: &str| {
        format!(
            "{PARAMS}{helper}\nfn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{\n var out=v;\n {vertex}\n return out;\n}}\nfn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {{\n var out=s;\n {surface}\n return out;\n}}"
        )
    };
    validated(&module(
        "",
        "out.thickness=depth_at(ctx.pixel)+path_of(ctx);",
    ))
    .unwrap_or_else(|error| panic!("{error}"));
    for (vertex, function) in [
        ("out.position.y+=depth_at(vec2(0.));", "depth_at"),
        (
            "var surface:SurfaceContext; out.position.y+=path_of(surface);",
            "path_of",
        ),
        (
            "if scene_depth_available() { out.position.y+=1.; }",
            "material_vertex",
        ),
    ] {
        assert_eq!(
            refused(&module(vertex, "")),
            ShaderError::SceneDepthInVertex {
                function: function.into()
            },
            "{vertex}"
        );
    }
}

// Plausible defects: a shader that reads its volume path through a helper
// not recorded as reading it, so its materials' meshes are never drawn into
// the volume layers and it measures nothing; or one that only declares a
// helper that would read it, or reads none, recorded as reading it, so its
// meshes cost three depth passes for nothing. The oracle is the contract:
// reading it means `material_surface` reaching `scene_volume_path`, itself
// or through a function it calls.
#[wasm_bindgen_test(unsupported = test)]
fn reading_the_volume_path_is_recorded() {
    let module = |helpers: &str, surface: &str| {
        format!(
            "{PARAMS}{helpers}\nfn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{ return v; }}\nfn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {{\n var out=s;\n {surface}\n return out;\n}}"
        )
    };
    let helpers = "fn measured(ctx:SurfaceContext)->f32 { return scene_volume_path(ctx).length; }\nfn through(ctx:SurfaceContext)->f32 { return measured(ctx)*2.; }";
    for (surface, reads) in [
        ("out.thickness=through(ctx);", true),
        (
            "if scene_volume_path(ctx).bound==VOLUME_EXIT { out.thickness=1.; }",
            true,
        ),
        ("out.thickness=scene_depth_behind(ctx);", false),
        ("", false),
    ] {
        let shader = validated(&module(helpers, surface)).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(shader.reads_volume_path, reads, "{surface}");
    }
}

// Plausible defect: a module that takes a derivative where a browser's
// compiler refuses one accepted, so that its pipelines fail in Chrome after
// `add_shader` returned it, as naga 30 accepts it. The oracle is WGSL's
// uniformity rule, which Chrome applies: a derivative under a branch on a
// fragment's value, after a return under one, on the right of `&&` after a
// fragment's value, or in a function called under such a branch, may be
// taken by some invocations of a quad and not others, and is refused; one
// in a function's top-level statements, directly or through a function
// called there, is uniform and accepted.
#[wasm_bindgen_test(unsupported = test)]
fn derivatives_are_taken_in_uniform_control_flow() {
    let module = |body: &str, helpers: &str| {
        format!(
            "{PARAMS}{helpers}\nfn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{ return v; }}\nfn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {{\n var out=s;\n {body}\n return out;\n}}"
        )
    };
    let slope = "fn slope(x:f32)->f32 { return abs(dpdx(x)); }";
    for (body, helpers) in [
        ("out.roughness=abs(dpdx(ctx.position.x));", ""),
        (
            "let d=slope(ctx.position.x);\n out.roughness=select(s.roughness,d,s.base_color.a>0.5);",
            slope,
        ),
    ] {
        validated(&module(body, helpers)).unwrap_or_else(|error| panic!("{body}: {error}"));
    }
    for (body, helpers) in [
        (
            "if s.base_color.a>0.5 { out.roughness=abs(dpdx(ctx.position.x)); }",
            "",
        ),
        (
            "if s.base_color.a>0.5 { return out; }\n out.roughness=abs(dpdy(ctx.position.y));",
            "",
        ),
        (
            "let rough=s.base_color.a>0.5 && fwidth(ctx.position.x)>0.;\n out.roughness=select(0.,1.,rough);",
            "",
        ),
        (
            "if s.base_color.a>0.5 { out.roughness=slope(ctx.position.x); }",
            slope,
        ),
    ] {
        assert_eq!(
            refused(&module(body, helpers)),
            ShaderError::NonUniformDerivative {
                function: "material_surface".into()
            },
            "{body}"
        );
    }
}

// Plausible defects: naga's uniform layout misreported to the game, whose
// Rust mirror then writes members at the wrong offsets; a block past the
// limit accepted. The oracle is WGSL's alignment rules, independent of
// SGL3D: a vec3 aligns to 16 bytes and takes 12, an array of vec4 strides
// 16, a struct's size rounds up to its alignment.
#[wasm_bindgen_test(unsupported = test)]
fn the_parameter_layout_is_wgsl_s() {
    let layout = validated(&format!(
        "struct ShaderParams {{ a:f32, b:vec3<f32>, c:array<vec4<f32>,2>, }}{FUNCTIONS}"
    ))
    .unwrap_or_else(|error| panic!("{error}"))
    .layout;
    let field = |name: &str, offset, size| ShaderParamField {
        name: name.into(),
        offset,
        size,
    };
    assert_eq!(layout.size, 64);
    assert_eq!(
        layout.fields,
        [field("a", 0, 4), field("b", 16, 12), field("c", 32, 32)]
    );
    assert_eq!(
        refused(&format!(
            "struct ShaderParams {{ big:array<vec4<f32>,313>, }}{FUNCTIONS}"
        )),
        ShaderError::ParamsTooLarge {
            size: 5008,
            max: SHADER_PARAMS_MAX_BYTES
        }
    );
}

// Plausible defects: a shader that reads the frame's time through a helper,
// a local copy of its context or the phase, recorded as not reading it, so
// a local light's cached shadow of its casters never follows the time; or
// one that reads only other context members recorded as reading it, so its
// casters' shadows redraw every frame for nothing. The oracle is the
// contract: the time is the `time` and `phase` members of `VertexContext`
// and `SurfaceContext`.
#[wasm_bindgen_test(unsupported = test)]
fn reading_the_time_is_recorded() {
    let module = |helpers: &str, vertex: &str, surface: &str| {
        format!(
            "{PARAMS}{helpers}\nfn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {{\n var out=v;\n {vertex}\n return out;\n}}\nfn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {{\n var out=s;\n {surface}\n return out;\n}}"
        )
    };
    let wave = "fn wave(ctx:VertexContext)->f32 { return sin(ctx.time); }";
    for (helpers, vertex, surface, reads) in [
        ("", "out.position.y+=ctx.time;", "", true),
        ("", "", "out.roughness=fract(ctx.phase);", true),
        (wave, "out.position.y+=wave(ctx);", "", true),
        ("", "var held=ctx; out.position.y+=held.time;", "", true),
        (
            "",
            "out.position.y+=ctx.instance.x;",
            "out.roughness=ctx.uv.x;",
            false,
        ),
    ] {
        let source = module(helpers, vertex, surface);
        let validated = validated(&source).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(validated.reads_time, reads, "{source}");
    }
}
