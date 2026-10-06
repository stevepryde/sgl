// Ports AMD FidelityFX Denoiser d7dfecbabe7b9523b14e7b067216e06b86e8d189,
// ffx-shadows-dnsr/ffx_denoiser_shadows_filter.h, to WGSL, with upstream's
// names and order (src/LICENSE-amd-fidelityfx-denoiser.txt):
/**********************************************************************
Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
********************************************************************/
// Changed: translated to WGSL, out parameters as returned structs, and
// float16_t values as f32, which pack2x16float and unpack2x16float pack in
// the group's memory as upstream's PackFloat16 and UnpackFloat16 do. The
// input arrives packed as the caller keeps it and stays so in the group's
// memory, where upstream packs it there; the caller's
// FFX_DNSR_Shadows_UnpackInput unpacks it. The tile's metadata reaches
// every thread of the group through workgroupUniformLoad, which WGSL needs
// before the branch whose barrier follows. The caller reads linear depth,
// 0 for the sky, so the depth is not linearised through the inverse
// projection, and a sky neighbour, and the centre, are skipped, where
// upstream weighs them by zero. Whether a pass writes a cleared tile is
// the caller's (FilterSoftShadowsPass's `write_cleared`), where upstream's
// second pass of three skips it, so that a chain of other lengths ends in
// a pass that writes it. The loops' literal bounds are named (AR-12).
// The caller's lanes (traced_denoise_lanes_*.wgsl: the FfxDnsr* types) are
// the slots filtered together, a lane each, where upstream filters one
// light a dispatch: the input, the shadow similarity, the sums and the
// results are per lane, and the depth, normals and their weights, which
// upstream loads and computes again for every light, once; a lane whose
// tile is cleared is computed with the rest and takes the cleared tile's
// values, and the group skips only when every lane's tile is cleared.
// With one lane this is upstream's filter of one light. The caller
// supplies the FFX_DNSR_Shadows_* callbacks upstream's host shader does,
// and FFX_DNSR_Shadows_UnpackInput.

var<workgroup> g_FFX_DNSR_Shadows_shared_input:array<array<FfxDnsrUint,16>,16>;
var<workgroup> g_FFX_DNSR_Shadows_shared_depth:array<array<f32,16>,16>;
var<workgroup> g_FFX_DNSR_Shadows_shared_normals_xy:array<array<u32,16>,16>;
var<workgroup> g_FFX_DNSR_Shadows_shared_normals_zw:array<array<u32,16>,16>;

fn FFX_DNSR_Shadows_PackFloat16(v:vec2<f32>)->u32 {
 return pack2x16float(v);
}

fn FFX_DNSR_Shadows_UnpackFloat16(a:u32)->vec2<f32> {
 return unpack2x16float(a);
}

// The lanes' filter inputs: mean and variance.
struct FFX_DNSR_Shadows_Input {
 mean:FfxDnsrFloat,
 variance:FfxDnsrFloat,
}

fn FFX_DNSR_Shadows_LoadInputFromGroupSharedMemory(idx:vec2<i32>)->FFX_DNSR_Shadows_Input {
 return FFX_DNSR_Shadows_UnpackInput(g_FFX_DNSR_Shadows_shared_input[idx.y][idx.x]);
}

fn FFX_DNSR_Shadows_LoadDepthFromGroupSharedMemory(idx:vec2<i32>)->f32 {
 return g_FFX_DNSR_Shadows_shared_depth[idx.y][idx.x];
}

fn FFX_DNSR_Shadows_LoadNormalsFromGroupSharedMemory(idx:vec2<i32>)->vec3<f32> {
 var normals:vec3<f32>;
 let xy=FFX_DNSR_Shadows_UnpackFloat16(g_FFX_DNSR_Shadows_shared_normals_xy[idx.y][idx.x]);
 normals.x=xy.x;
 normals.y=xy.y;
 normals.z=FFX_DNSR_Shadows_UnpackFloat16(g_FFX_DNSR_Shadows_shared_normals_zw[idx.y][idx.x]).x;
 return normals;
}

fn FFX_DNSR_Shadows_StoreInGroupSharedMemory(idx:vec2<i32>,normals:vec3<f32>,input:FfxDnsrUint,depth:f32) {
 g_FFX_DNSR_Shadows_shared_input[idx.y][idx.x]=input;
 g_FFX_DNSR_Shadows_shared_depth[idx.y][idx.x]=depth;
 g_FFX_DNSR_Shadows_shared_normals_xy[idx.y][idx.x]=FFX_DNSR_Shadows_PackFloat16(normals.xy);
 g_FFX_DNSR_Shadows_shared_normals_zw[idx.y][idx.x]=FFX_DNSR_Shadows_PackFloat16(vec2(normals.z,0.));
}

struct FFX_DNSR_Shadows_Loaded {
 normals:vec3<f32>,
 input:FfxDnsrUint,
 depth:f32,
}

fn FFX_DNSR_Shadows_LoadWithOffset(did_in:vec2<i32>,offset:vec2<i32>)->FFX_DNSR_Shadows_Loaded {
 let did=did_in+offset;

 let p=clamp(did,vec2(0),vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1);
 return FFX_DNSR_Shadows_Loaded(FFX_DNSR_Shadows_ReadNormals(p),FFX_DNSR_Shadows_ReadInput(p),FFX_DNSR_Shadows_ReadDepth(p));
}

fn FFX_DNSR_Shadows_StoreWithOffset(gtid_in:vec2<i32>,offset:vec2<i32>,loaded:FFX_DNSR_Shadows_Loaded) {
 let gtid=gtid_in+offset;
 FFX_DNSR_Shadows_StoreInGroupSharedMemory(gtid,loaded.normals,loaded.input,loaded.depth);
}

fn FFX_DNSR_Shadows_InitializeGroupSharedMemory(did_in:vec2<i32>,gtid:vec2<i32>) {
 let offset_0=vec2(0);
 let offset_1=vec2(8,0);
 let offset_2=vec2(0,8);
 let offset_3=vec2(8,8);

 /// XA
 /// BC

 let did=did_in-4;
 let loaded_0=FFX_DNSR_Shadows_LoadWithOffset(did,offset_0); // X
 let loaded_1=FFX_DNSR_Shadows_LoadWithOffset(did,offset_1); // A
 let loaded_2=FFX_DNSR_Shadows_LoadWithOffset(did,offset_2); // B
 let loaded_3=FFX_DNSR_Shadows_LoadWithOffset(did,offset_3); // C

 FFX_DNSR_Shadows_StoreWithOffset(gtid,offset_0,loaded_0); // X
 FFX_DNSR_Shadows_StoreWithOffset(gtid,offset_1,loaded_1); // A
 FFX_DNSR_Shadows_StoreWithOffset(gtid,offset_2,loaded_2); // B
 FFX_DNSR_Shadows_StoreWithOffset(gtid,offset_3,loaded_3); // C
}

fn FFX_DNSR_Shadows_GetShadowSimilarity(x1:FfxDnsrFloat,x2:FfxDnsrFloat,sigma:FfxDnsrFloat)->FfxDnsrFloat {
 return exp(-abs(x1-x2)/sigma);
}

fn FFX_DNSR_Shadows_GetDepthSimilarity(x1:f32,x2:f32,sigma:f32)->f32 {
 return exp(-abs(x1-x2)/sigma);
}

fn FFX_DNSR_Shadows_GetNormalSimilarity(x1:vec3<f32>,x2:vec3<f32>)->f32 {
 return pow(saturate(dot(x1,x2)),32.);
}

// The caller's depth is linear already (see the header), where upstream
// linearises a projected depth through the inverse projection.
fn FFX_DNSR_Shadows_GetLinearDepth(did:vec2<u32>,depth:f32)->f32 {
 return depth;
}

// The filters' radius in steps, upstream's literal k (AR-12).
const FFX_DNSR_SHADOWS_FILTER_RADIUS:i32=1;

fn FFX_DNSR_Shadows_FetchFilteredVarianceFromGroupSharedMemory(pos:vec2<i32>)->FfxDnsrFloat {
 let k=FFX_DNSR_SHADOWS_FILTER_RADIUS;
 var variance=FfxDnsrFloat(0.);
 var kernel=array<array<f32,2>,2>(
  array<f32,2>(1./4.,1./8.),
  array<f32,2>(1./8.,1./16.)
 );
 for (var y=-k;y<=k;y++) {
  for (var x=-k;x<=k;x++) {
   let w=kernel[abs(x)][abs(y)];
   variance+=w*FFX_DNSR_Shadows_LoadInputFromGroupSharedMemory(pos+vec2(x,y)).variance;
  }
 }
 return variance;
}

struct FFX_DNSR_Shadows_Sums {
 weight_sum:FfxDnsrFloat,
 mean_sum:FfxDnsrFloat,
 variance_sum:FfxDnsrFloat,
}

fn FFX_DNSR_Shadows_DenoiseFromGroupSharedMemory(did:vec2<u32>,gtid:vec2<u32>,depth:f32,stepsize:u32)->FFX_DNSR_Shadows_Sums {
 // Load our center sample
 let shadow_center=FFX_DNSR_Shadows_LoadInputFromGroupSharedMemory(vec2<i32>(gtid));
 let normal_center=FFX_DNSR_Shadows_LoadNormalsFromGroupSharedMemory(vec2<i32>(gtid));

 var weight_sum=FfxDnsrFloat(1.);
 var mean_sum=shadow_center.mean;
 var variance_sum=shadow_center.variance;

 let variance=FFX_DNSR_Shadows_FetchFilteredVarianceFromGroupSharedMemory(vec2<i32>(gtid));
 let std_deviation=sqrt(max(variance+1e-9,FfxDnsrFloat(0.)));
 let depth_center=FFX_DNSR_Shadows_GetLinearDepth(did,depth); // linearize the depth value

 // Iterate filter kernel
 let k=FFX_DNSR_SHADOWS_FILTER_RADIUS;
 var kernel=array<f32,3>(1.,2./3.,1./6.);

 for (var y=-k;y<=k;y++) {
  for (var x=-k;x<=k;x++) {
   // Should we process this sample?
   let step=vec2(x,y)*i32(stepsize);
   let gtid_idx=vec2<i32>(gtid)+step;
   let did_idx=vec2<i32>(did)+step;

   var depth_neigh=FFX_DNSR_Shadows_LoadDepthFromGroupSharedMemory(gtid_idx);

   // Zero weight for sky pixels, whose linear depth reads as 0, and the
   // centre, already summed.
   if (x==0 && y==0) || depth_neigh<=0. {
    continue;
   }

   let normal_neigh=FFX_DNSR_Shadows_LoadNormalsFromGroupSharedMemory(gtid_idx);
   let shadow_neigh=FFX_DNSR_Shadows_LoadInputFromGroupSharedMemory(gtid_idx);

   // Fetch our filtering values
   depth_neigh=FFX_DNSR_Shadows_GetLinearDepth(vec2<u32>(max(did_idx,vec2(0))),depth_neigh);

   // Evaluate the edge-stopping function, the depth and normals' part
   // once for the four lanes
   var w_shared=kernel[abs(x)]*kernel[abs(y)]; // kernel weight
   w_shared*=FFX_DNSR_Shadows_GetDepthSimilarity(depth_center,depth_neigh,FFX_DNSR_Shadows_GetDepthSimilaritySigma());
   w_shared*=FFX_DNSR_Shadows_GetNormalSimilarity(normal_center,normal_neigh);
   let w=w_shared*FFX_DNSR_Shadows_GetShadowSimilarity(shadow_center.mean,shadow_neigh.mean,std_deviation);

   // Accumulate the filtered sample
   mean_sum+=w*shadow_neigh.mean;
   variance_sum+=w*w*shadow_neigh.variance;
   weight_sum+=w;
  }
 }
 return FFX_DNSR_Shadows_Sums(weight_sum,mean_sum,variance_sum);
}

fn FFX_DNSR_Shadows_ApplyFilterWithPrecache(did:vec2<u32>,gtid_in:vec2<u32>,stepsize:u32)->FFX_DNSR_Shadows_Input {
 var weight_sum=FfxDnsrFloat(1.);
 var mean_sum=FfxDnsrFloat(0.);
 var variance_sum=FfxDnsrFloat(0.);

 FFX_DNSR_Shadows_InitializeGroupSharedMemory(vec2<i32>(did),vec2<i32>(gtid_in));
 let needs_denoiser=FFX_DNSR_Shadows_IsShadowReciever(did);
 workgroupBarrier();
 if needs_denoiser {
  let depth=FFX_DNSR_Shadows_ReadDepth(vec2<i32>(did));
  let gtid=gtid_in+4u; // Center threads in groupshared memory
  let sums=FFX_DNSR_Shadows_DenoiseFromGroupSharedMemory(did,gtid,depth,stepsize);
  weight_sum=sums.weight_sum;
  mean_sum=sums.mean_sum;
  variance_sum=sums.variance_sum;
 }

 let mean=mean_sum/weight_sum;
 let variance=variance_sum/(weight_sum*weight_sum);
 return FFX_DNSR_Shadows_Input(mean,variance);
}

struct FFX_DNSR_Shadows_TileMetaData {
 is_cleared:FfxDnsrBool,
 all_in_light:FfxDnsrBool,
}

fn FFX_DNSR_Shadows_ReadTileMetaDataOf(gid:vec2<u32>)->FFX_DNSR_Shadows_TileMetaData {
 let meta_data=FFX_DNSR_Shadows_ReadTileMetaData(gid.y*FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u)+gid.x);
 return FFX_DNSR_Shadows_TileMetaData((meta_data&FfxDnsrUint(TILE_META_DATA_CLEAR_MASK))!=FfxDnsrUint(0u),(meta_data&FfxDnsrUint(TILE_META_DATA_LIGHT_MASK))!=FfxDnsrUint(0u));
}

struct FFX_DNSR_Shadows_FilterResult {
 results:FFX_DNSR_Shadows_Input,
 write_results:bool,
}

// `write_cleared`: whether the pass writes a cleared tile, its value, as
// upstream's every pass but its second of three does; that pass's target
// holds the value already, as the tile classification wrote it.
fn FFX_DNSR_Shadows_FilterSoftShadowsPass(gid:vec2<u32>,gtid:vec2<u32>,did:vec2<u32>,write_cleared:bool,stepsize:u32)->FFX_DNSR_Shadows_FilterResult {
 let meta_data=FFX_DNSR_Shadows_ReadTileMetaDataOf(gid);

 let cleared=FFX_DNSR_Shadows_Input(select(FfxDnsrFloat(0.),FfxDnsrFloat(1.),meta_data.all_in_light),FfxDnsrFloat(0.));
 if ffx_dnsr_all(meta_data.is_cleared) {
  return FFX_DNSR_Shadows_FilterResult(cleared,write_cleared);
 }
 let filtered=FFX_DNSR_Shadows_ApplyFilterWithPrecache(did,gtid,stepsize);
 let results=FFX_DNSR_Shadows_Input(
  select(filtered.mean,cleared.mean,meta_data.is_cleared),
  select(filtered.variance,cleared.variance,meta_data.is_cleared),
 );
 return FFX_DNSR_Shadows_FilterResult(results,true);
}
