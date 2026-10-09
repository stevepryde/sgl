// The shader contract (the package README's "Programmable surfaces";
// specs/sgl3d-architecture.md, Shader contract): what a game's WGSL module
// (Scene::add_shader) receives and returns. A game's module defines
// `struct ShaderParams`, `material_vertex` and `material_surface` over these
// structs, and from material_surface may call the scene depth functions its
// program's provider declares (shader_scene_depth_none.wgsl,
// shader_scene_depth.wgsl); nothing else SGL3D declares is the contract. SGL3D's own programs compose
// shader_default.wgsl in its place, whose functions return their argument.
// The pattern is Filament ef1a133's materialVertex() and material()
// (shaders/src/surface_main.vs, surface_main.fs,
// surface_material_inputs.vs and .fs) and Godot b130438's vertex() and
// fragment() (scene_forward_clustered.glsl), which every pass of a material
// compiles.

// A vertex in and out of material_vertex, in the mesh's own units and axes,
// after SGL3D's skinning and morphing and before the instance's pose:
// `normal` unit; `tangent` xyz unit and w its handedness, all zero where the
// mesh has none; `color` linear RGB and alpha, as asset::Vertex::color;
// `shader_data` the mesh's per-vertex data (PreparedModel::with_shader_data),
// zero without any; `custom` what the shader passes to material_surface,
// interpolated, zero in.
struct MaterialVertex {
 position:vec3<f32>,
 normal:vec3<f32>,
 tangent:vec4<f32>,
 uv:vec2<f32>,
 color:vec4<f32>,
 shader_data:vec4<f32>,
 custom:vec4<f32>,
}
// What material_vertex evaluates at: the instance's pose `model`, its
// shader data `instance` (Scene::set_instance_shader_data), the frame's
// `time` (FrameInput::elapsed_seconds in f32, which loses precision over a
// long session) and `phase` (where that time falls in the hour over which
// material animation repeats exactly, 0..1), and whether this is the
// evaluation at the last submitted frame (`previous`), whose time, phase,
// parameters, pose and instance data it then holds, from which motion is
// measured.
struct VertexContext {
 model:mat4x4<f32>,
 instance:vec4<f32>,
 time:f32,
 phase:f32,
 previous:bool,
}
// A surface in and out of material_surface: the material's record and maps
// already evaluated at the fragment. `base_color` is linear RGB, the
// record's base times its base map and the vertex colour, its alpha the
// fragment's coverage; `roughness` perceptual; `normal` unit, in the render
// frame's world space on the shaded side, the mapped normal; `specular`
// KHR_materials_specular's strength; `transmission` KHR_materials_
// transmission's share; `thickness` KHR_materials_volume's, in the mesh's
// units; `attenuation` the Beer-Lambert coefficient per metre, finite and
// nonnegative; `ior` at least 1, 0 for an infinite one.
struct MaterialSurface {
 base_color:vec4<f32>,
 emission:vec3<f32>,
 metallic:f32,
 roughness:f32,
 normal:vec3<f32>,
 occlusion:f32,
 specular:f32,
 clearcoat:f32,
 coat_roughness:f32,
 transmission:f32,
 thickness:f32,
 attenuation:vec3<f32>,
 ior:f32,
 dispersion:f32,
}
// What material_surface evaluates at: the fragment's render-frame
// `position` (displaced), its interpolated `geometry_normal` on the shaded
// side, the unit direction toward the eye `view`, its `uv` and `color`,
// material_vertex's `custom`, the instance's data, pose and the pose's axis
// scales `model_scale`, the frame's `time` and `phase`, whether it is the
// material's authored `front`, its `pixel` in the pass's target in texels,
// and its linear `view_depth` in metres.
struct SurfaceContext {
 position:vec3<f32>,
 geometry_normal:vec3<f32>,
 view:vec3<f32>,
 uv:vec2<f32>,
 color:vec4<f32>,
 custom:vec4<f32>,
 instance:vec4<f32>,
 model:mat4x4<f32>,
 model_scale:vec3<f32>,
 time:f32,
 phase:f32,
 front:bool,
 pixel:vec2<f32>,
 view_depth:f32,
}
// What scene_depth returns at the sky.
const SCENE_DEPTH_FAR:f32=1e30;
