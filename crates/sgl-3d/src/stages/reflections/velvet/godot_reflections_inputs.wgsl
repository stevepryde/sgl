// SGL3D's surface (the Surface contract, specs/sgl3d-architecture.md: the
// receiver layer where a receiver is nearer than the opaque surface, else the
// G-buffer) as the inputs of Godot's screen-space reflections (velvet.rs), at
// full resolution: Godot's normal-roughness buffer (view-space normal packed
// x0.5+0.5, perceptual roughness) and its reversed-Z depth buffer.
@group(0) @binding(0) var stable_normal:texture_2d<f32>;
@group(0) @binding(1) var stable_material:texture_2d<f32>;
@group(0) @binding(2) var stable_f0:texture_2d<f32>;
@group(0) @binding(3) var surface_depth:texture_depth_2d;
// SGL3D's view matrix.
@group(0) @binding(4) var<uniform> view:mat4x4<f32>;
@group(0) @binding(5) var normal_roughness:texture_storage_2d<rgba16float,write>;
@group(0) @binding(6) var depth:texture_storage_2d<r32float,write>;
@group(0) @binding(7) var opaque_depth:texture_depth_2d;
@group(0) @binding(8) var surface_receivers:texture_2d<f32>;

@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=textureDimensions(depth)) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let d=textureLoad(surface_depth,p,0);
 // The traced lobe's normal and perceptual roughness; an unlit surface's 1
 // is never traced.
 let lobe=gbuffer_surface_lobe(d,textureLoad(opaque_depth,p,0),textureLoad(surface_receivers,p,0),textureLoad(stable_normal,p,0),textureLoad(stable_material,p,0),textureLoad(stable_f0,p,0));
 let normal_view=normalize((view*vec4(lobe.normal,0.)).xyz);
 textureStore(normal_roughness,p,vec4(normal_view*.5+.5,lobe.roughness));
 // Infinite reversed-Z: the sky is 0, as Godot's reversed-Z background.
 textureStore(depth,p,vec4(d,0.,0.,0.));
}
