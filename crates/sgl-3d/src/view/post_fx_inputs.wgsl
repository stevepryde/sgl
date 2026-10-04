// SGL3D's surface (the Surface contract, specs/sgl3d-architecture.md: the
// receiver layer where a receiver is nearer than the opaque surface, else the
// G-buffer) as the inputs of DiligentFX's effects (post_fx.rs).
@group(0) @binding(0) var stable_normal:texture_2d<f32>;
@group(0) @binding(1) var stable_material:texture_2d<f32>;
@group(0) @binding(2) var stable_f0:texture_2d<f32>;
@group(0) @binding(3) var stable_motion:texture_2d<f32>;
@group(0) @binding(4) var surface_depth:texture_depth_2d;
// x near plane, y the effect's far plane (metres).
@group(0) @binding(5) var<uniform> planes:vec4<f32>;
@group(0) @binding(6) var opaque_depth:texture_depth_2d;
@group(0) @binding(7) var surface_receivers:texture_2d<f32>;
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
 let d=textureLoad(surface_depth,p,0);
 // The traced lobe: the coat of a coated surface, else the base. Its
 // perceptual roughness on an unlit surface, 1, is never traced
 // (IsReflectionSample: roughness <= RoughnessThreshold < 1).
 let lobe=gbuffer_surface_lobe(d,textureLoad(opaque_depth,p,0),textureLoad(surface_receivers,p,0),textureLoad(stable_normal,p,0),textureLoad(stable_material,p,0),textureLoad(stable_f0,p,0));
 // SGL3D motion is current - previous UV (+y down); Diligent's is NDC
 // (+y up) and the shaders scale it by F3NDC_XYZ_TO_UVD_SCALE (0.5, -0.5).
 let motion=textureLoad(stable_motion,p,0).xy;
 // SGL3D's infinite reversed-Z depth is near / z; the same z under a far
 // plane F is (F d - near) / (F - near). Beyond F (the sky included) is the
 // far plane.
 let far=(planes.y*d-planes.x)/(planes.y-planes.x);
 return Inputs(max(far,0.),vec4(lobe.normal,0.),vec4(lobe.roughness),vec4(motion*vec2(2.,-2.),0.,0.));
}
