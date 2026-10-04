// SGL3D's G-buffer as the inputs of Godot's screen-space reflections
// (velvet.rs), at full resolution: Godot's normal-roughness buffer
// (view-space normal packed x0.5+0.5, perceptual roughness) and its reversed-Z
// depth buffer.
@group(0) @binding(0) var stable_normal:texture_2d<f32>;
@group(0) @binding(1) var stable_material:texture_2d<f32>;
@group(0) @binding(2) var stable_f0:texture_2d<f32>;
@group(0) @binding(3) var stable_depth:texture_depth_2d;
// SGL3D's view matrix.
@group(0) @binding(4) var<uniform> view:mat4x4<f32>;
@group(0) @binding(5) var normal_roughness:texture_storage_2d<rgba16float,write>;
@group(0) @binding(6) var depth:texture_storage_2d<r32float,write>;

@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=textureDimensions(depth)) {
  return;
 }
 let p=vec2<i32>(id.xy);
 // The reflecting layer's normal and perceptual roughness; an unlit
 // receiver's 1 is never traced.
 let normal=textureLoad(stable_normal,p,0);
 let material=gbuffer_material(textureLoad(stable_material,p,0));
 let roughness=gbuffer_traced_roughness(material,gbuffer_lit(textureLoad(stable_f0,p,0)));
 let normal_view=normalize((view*vec4(gbuffer_reflection_normal(normal,material.coat),0.)).xyz);
 textureStore(normal_roughness,p,vec4(normal_view*.5+.5,roughness));
 // Infinite reversed-Z: the sky is 0, as Godot's reversed-Z background.
 textureStore(depth,p,vec4(textureLoad(stable_depth,p,0),0.,0.,0.));
}
