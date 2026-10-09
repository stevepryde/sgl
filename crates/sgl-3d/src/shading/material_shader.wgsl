// The material's shader functions (shader_contract.wgsl) where SGL3D calls
// them. Its vertex function: in every pass that rasterises scene geometry,
// after SGL3D's
// deformation and before the instance's pose, as Filament ef1a133 calls
// materialVertex() after morphing and skinning (shaders/src/surface_main.vs)
// and Godot b130438 its vertex() over its already skinned vertices
// (scene_forward_clustered.glsl). The camera's passes that write motion
// evaluate it again at the last submitted frame (motion_vertices), with that
// frame's time, phase, parameters, pose and instance data, as Godot's
// motion-vector variant calls vertex_shader() twice
// (scene_forward_clustered.glsl 776–860). Its surface function: where
// surface_raster.wgsl's raster_material and a masked caster's coverage
// evaluate the material, at the context material_context gives it. Reads
// `view`, `frame` and `objects`.

// Whether the pipeline writes motion, so its vertices are evaluated at the
// last submitted frame too: a pipeline constant (view::pipelines), off for
// the blended draws and FSR2's composition mask, which write none. It
// changes no input of the evaluation this frame.
override motion_vertices:bool=true;
// What material_vertex evaluates the instance whose object record is at
// index `object` at: this frame, or the last submitted one (`previous`).
fn material_vertex_context(object:u32,previous:bool)->VertexContext {
 if previous {
  return VertexContext(objects[object].previous_model,objects[object].previous_shader_data,frame.previous_elapsed_seconds,frame.previous_animation_phase,true);
 }
 return VertexContext(objects[object].model,objects[object].shader_data,frame.elapsed_seconds,frame.animation_phase,false);
}
// `rest` as the material's vertex function places it for the instance at
// `object`, this frame or the last submitted one (`previous`): a normal the
// function changed is made unit again; the pose's transforms
// (object_normal, object_tangent) make the tangent perpendicular to it.
fn material_shaded_vertex(object:u32,rest:MaterialVertex,previous:bool)->MaterialVertex {
 var shaded=material_vertex(rest,material_vertex_context(object,previous),material_shader_params(previous));
 if any(shaded.normal!=rest.normal) {
  shaded.normal=normalize(shaded.normal);
 }
 return shaded;
}
// The fragment of vertex `rest` of the instance whose object record is at
// index `object`, with its lightmap chart's UV and bounds. Motion is
// measured from `previous_position` at the instance's previous pose, where
// the pipeline writes motion as the material's vertex function places it
// at the last submitted frame, with this frame's normal and tangent.
fn vertex(object:u32,rest:MaterialVertex,lightmap_uv:vec2<f32>,lightmap_bounds:vec4<f32>,previous_position:vec3<f32>)->Fragment {
 var o:Fragment;
 let model=objects[object].model;
 let v=material_shaded_vertex(object,rest,false);
 let p=model*vec4(v.position,1);
 o.clip=scene_raster_clip(p);
 o.world=p.xyz;
 o.normal=object_normal(model,v.normal);
 o.tangent=object_tangent(model,v.tangent,v.normal);
 o.uv=v.uv;
 o.color=v.color;
 o.custom=v.custom;
 o.lightmap_uv=lightmap_uv;
 o.lightmap_bounds=lightmap_bounds;
 o.current_clip=view.stable_view_projection*p;
 var previous=rest;
 previous.position=previous_position;
 if motion_vertices {
  previous=material_shaded_vertex(object,previous,true);
 }
 o.previous_clip=view.previous_view_projection*objects[object].previous_model*vec4(previous.position,1);
 return o;
}
// What the material's surface function evaluates a fragment of the instance
// whose object record is at index `object` at (shader_contract.wgsl's
// SurfaceContext): its render-frame position `world`, geometry normal on
// the shaded side, UV, colour and `custom`, whether it is the material's
// authored front, and its `pixel` in the pass's target. A shadow view's
// eye and depth are its light's view's.
fn material_context(object:u32,world:vec3<f32>,geometry_normal:vec3<f32>,uv:vec2<f32>,color:vec4<f32>,custom:vec4<f32>,front:bool,pixel:vec2<f32>)->SurfaceContext {
 let model=objects[object].model;
 let view_depth=-(view.view*vec4(world,1.)).z;
 return SurfaceContext(world,geometry_normal,normalize(view.eye-world),uv,color,custom,objects[object].shader_data,model,transmission_model_scale(model),frame.elapsed_seconds,frame.animation_phase,front,pixel,view_depth);
}
