// The shader contract (the package README's "Programmable surfaces";
// specs/sgl3d-architecture.md, Shader contract): what a game's WGSL module
// (Scene::add_shader) receives and returns. A game's module defines
// `struct ShaderParams`, `material_vertex` and `material_surface` over these
// structs, and from material_surface may call the scene depth functions its
// program's provider declares (shader_scene_depth_none.wgsl,
// shader_scene_depth.wgsl): scene_depth_available, scene_depth,
// scene_depth_behind and scene_volume_path; nothing else SGL3D declares is
// the contract. SGL3D's own programs compose shader_default.wgsl in its
// place, whose functions return their argument.
// The pattern is Filament ef1a133's materialVertex() and material()
// (shaders/src/surface_main.vs, surface_main.fs,
// surface_material_inputs.vs and .fs) and Godot b130438's vertex() and
// fragment() (scene_forward_clustered.glsl), which every pass of a material
// compiles.

// A vertex in and out of material_vertex, in the mesh's own units and axes,
// after SGL3D's skinning and morphing and before the instance's pose:
// `normal` unit; `tangent` xyz unit, in the normal's plane, and w its
// handedness (+1 or -1); a vertex without a usable tangent (none, along the
// normal, or handedness not +1 or -1) gets an arbitrary unit tangent in the
// normal's plane of handedness +1, never zero, so a shader that needs a
// consistent frame there builds its own; `color` linear RGB and alpha, as
// asset::Vertex::color;
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
// What scene_volume_path returns: the `length` in metres along the
// fragment's view ray inside the volume its material bounds, at most
// SCENE_DEPTH_FAR and never negative, and its `bound`, one of the VOLUME_*
// constants, which says what ends the path away from the fragment. SGL3D
// measures it in the camera's blended draws on the Extended binding tier
// while Settings::volume_paths is on, from depth layers of the meshes of
// the blended materials whose shader calls scene_volume_path, each a closed
// mesh around its volume: the nearest front faces, the nearest back faces
// and the nearest back faces behind those, at each pixel in front of the
// opaque surface.
struct VolumePath {
 length:f32,
 bound:u32,
}
// No path is measured (every other pass, the Basic binding tier, the
// setting off): length 0. The shader takes its own fallback, such as
// scene_depth_behind or an authored thickness.
const VOLUME_NONE:u32=0u;
// At a front fragment, where the view ray enters: the path ends at the
// nearest back face behind the fragment, where the ray leaves.
const VOLUME_EXIT:u32=1u;
// At a front fragment: the path ends at the opaque surface, with no back
// face between (the volume meets the opaque surface, or is open).
const VOLUME_OPAQUE:u32=2u;
// Where another volume's faces hide the fragment's own segment: at a front
// fragment behind two back faces, which leave its own exit unmeasured, the
// length is to the opaque surface; at a back fragment behind another back
// face, which hides where its segment starts, the length is from the eye.
// Either is an upper bound.
const VOLUME_HIDDEN:u32=3u;
// At a back fragment, seen from inside where the ray leaves: the path
// starts at the nearest front face in front of it, where the ray entered,
// where no other back face lies between.
const VOLUME_ENTRY:u32=4u;
// At a back fragment with no front face in front of it (the camera is
// inside, or the near plane cut the entry): the path starts at the eye.
const VOLUME_EYE:u32=5u;
