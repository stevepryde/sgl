// The transmission copy (stages/transparent/transmission.rs): the composed
// frame copied into its first level, a texel a texel, then each further
// level from the level above it (`source`), bilinearly at its texel
// centres, a 2x2 box where the level above is twice its size, as three.js
// r185's WebGPU backend builds a texture's mipmaps
// (src/renderers/webgpu/utils/WebGPUTexturePassUtils.js 49-120, 317-333;
// commit 2431a09f, MIT, see stages/post/smaa/LICENSE-three.txt). Both draw
// the full-screen triangle (fullscreen_vs).
@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var source_sampler:sampler;
@fragment fn transmission_copy_fs(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 return textureLoad(source,vec2<i32>(position.xy),0);
}
@fragment fn transmission_downsample_fs(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let size=max(textureDimensions(source)/2u,vec2(1u));
 return textureSampleLevel(source,source_sampler,position.xy/vec2<f32>(size),0.);
}
