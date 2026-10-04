// SGL3D's G-buffer as the inputs of DiligentFX's effects (post_fx.rs).
@group(0) @binding(0) var stable_normal:texture_2d<f32>;
@group(0) @binding(1) var stable_material:texture_2d<f32>;
@group(0) @binding(2) var stable_f0:texture_2d<f32>;
@group(0) @binding(3) var stable_motion:texture_2d<f32>;
@group(0) @binding(4) var stable_depth:texture_depth_2d;
// x near plane, y the effect's far plane (metres).
@group(0) @binding(5) var<uniform> planes:vec4<f32>;
struct Inputs {
 // Depth under the effect's finite-far reversed projection (pDepthBufferSRV).
 @builtin(frag_depth) depth:f32,
 // World normal in [-1, 1] (pNormalBufferSRV, read as .xyz).
 @location(0) normal:vec4<f32>,
 // Perceptual roughness in channel 0 (pMaterialBufferSRV, IsRoughnessPerceptual).
 @location(1) material:vec4<f32>,
 // NDC current - previous position (pMotionVectorsSRV).
 @location(2) motion:vec4<f32>,
}
// Drawn with the vertex entry fullscreen_vs.
@fragment fn fs_main(@builtin(position) position:vec4<f32>)->Inputs {
 let p=vec2<i32>(position.xy);
 // The stable target preserves both normals; trace the outermost layer.
 let normal=textureLoad(stable_normal,p,0);
 // The traced lobe's perceptual roughness; an unlit receiver's 1 is never
 // traced (IsReflectionSample: roughness <= RoughnessThreshold < 1).
 let material=gbuffer_material(textureLoad(stable_material,p,0));
 let roughness=gbuffer_traced_roughness(material,gbuffer_lit(textureLoad(stable_f0,p,0)));
 // SGL3D motion is current - previous UV (+y down); Diligent's is NDC
 // (+y up) and the shaders scale it by F3NDC_XYZ_TO_UVD_SCALE (0.5, -0.5).
 let motion=textureLoad(stable_motion,p,0).xy;
 // SGL3D's infinite reversed-Z depth is near / z; the same z under a far
 // plane F is (F d - near) / (F - near). Beyond F (the sky included) is the
 // far plane.
 let d=textureLoad(stable_depth,p,0);
 let far=(planes.y*d-planes.x)/(planes.y-planes.x);
 return Inputs(max(far,0.),vec4(gbuffer_reflection_normal(normal,material.coat),0.),vec4(roughness),vec4(motion*vec2(2.,-2.),0.,0.));
}
