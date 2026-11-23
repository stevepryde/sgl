// The mip chain stage (stages/mip_chains.rs): one level of a decoded
// image's texture, drawn from the level above it (`above`) by the
// full-screen triangle (fullscreen_vs). A texel is the mean of the texels
// of the level above that it covers, weighted by the area it covers of each
// (the architecture's Material mip chains): a 2x2 box where that level is
// twice this one's size, the filter three.js r187
// (WebGPUTexturePassUtils.js, generateMipmaps) and Filament ef1a133
// (WebGPURenderPassMipmapGenerator.cpp) take with one bilinear tap, and
// along an axis of odd size the three texels its footprint spans, which
// their tap leaves part of out. The texels are loaded and weighed here, so
// the result depends on no sampler's filtering precision. A colour
// texture's view decodes sRGB on load and its target encodes on store, so
// colour is averaged in linear light; a data texture's is averaged as
// stored. content/images/mips.rs (`footprint`) is the same rule for the CPU.
// One pass draws a level of up to four textures of one size and format,
// each from its own level above, into four colour targets (WebGPU's
// default 32 bytes a sample, at 8 an RGBA8 target): a render pass is the
// costly unit on a tiled GPU's driver.
@group(0) @binding(0) var above_0:texture_2d<f32>;
@group(0) @binding(1) var above_1:texture_2d<f32>;
@group(0) @binding(2) var above_2:texture_2d<f32>;
@group(0) @binding(3) var above_3:texture_2d<f32>;

// Along one axis: the first texel of the level above that texel `at` of
// this level covers, and the shares of its footprint that texel and the
// next two hold. The level above is `size` texels long and this one
// max(size/2, 1): its one texel whole where size is 1; two texels equally
// where it is even; and where it is odd, 2m+1, three, weighted m-at, m and
// at+1 over 2m+1.
struct MipFootprint {
 first:u32,
 shares:vec3<f32>,
}
fn mip_footprint(at:u32,size:u32)->MipFootprint {
 if size==1u {
  return MipFootprint(0u,vec3(1.,0.,0.));
 }
 if (size&1u)==0u {
  return MipFootprint(2u*at,vec3(.5,.5,0.));
 }
 let below=size/2u;
 return MipFootprint(2u*at,vec3(f32(below-at),f32(below),f32(at+1u))/f32(size));
}

// Row `row` of the level `above` under `columns`. A texel past the level's
// edge holds no share: its load is clamped onto the edge and weighs 0.
fn mip_row(above:texture_2d<f32>,columns:MipFootprint,row:u32,size:vec2<u32>)->vec4<f32> {
 let last=size-vec2(1u);
 let y=min(row,last.y);
 return columns.shares.x*textureLoad(above,vec2(min(columns.first,last.x),y),0)
  +columns.shares.y*textureLoad(above,vec2(min(columns.first+1u,last.x),y),0)
  +columns.shares.z*textureLoad(above,vec2(min(columns.first+2u,last.x),y),0);
}

// Texel `at` of the level below `above`.
fn mip_texel(above:texture_2d<f32>,at:vec2<u32>)->vec4<f32> {
 let size=textureDimensions(above);
 let columns=mip_footprint(at.x,size.x);
 let rows=mip_footprint(at.y,size.y);
 return rows.shares.x*mip_row(above,columns,rows.first,size)
  +rows.shares.y*mip_row(above,columns,rows.first+1u,size)
  +rows.shares.z*mip_row(above,columns,rows.first+2u,size);
}

// One texture's level, from above_0.
@fragment fn mip_chain_fs(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 return mip_texel(above_0,vec2<u32>(position.xy));
}

// Four textures' levels, each from its own level above.
struct MipChainBatch {
 @location(0) level_0:vec4<f32>,
 @location(1) level_1:vec4<f32>,
 @location(2) level_2:vec4<f32>,
 @location(3) level_3:vec4<f32>,
}
@fragment fn mip_chain_batch_fs(@builtin(position) position:vec4<f32>)->MipChainBatch {
 let at=vec2<u32>(position.xy);
 return MipChainBatch(
  mip_texel(above_0,at),mip_texel(above_1,at),mip_texel(above_2,at),mip_texel(above_3,at));
}
