// Ports AMD FidelityFX Denoiser d7dfecbabe7b9523b14e7b067216e06b86e8d189,
// ffx-shadows-dnsr/ffx_denoiser_shadows_tileclassification.h, to WGSL,
// with upstream's names and order (src/LICENSE-amd-fidelityfx-denoiser.txt):
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
// Changed: translated to WGSL, out parameters as returned structs. The
// thread group's all-true takes the workgroup fallback alone (28–45), its
// count read through workgroupUniformLoad, which WGSL needs before the
// branches whose barriers follow; subgroups are a measured specialisation
// later. QuadReadAcrossX and QuadReadAcrossY read the 2×2 quad's
// neighbours through workgroup memory, WGSL having no quad operations
// without subgroups, so GetClosestVelocity takes the thread's place in the
// group. The velocity is the surface's motion in UV already, the current
// position less the previous, so GetClosestVelocity returns it unscaled
// where upstream converts from NDC. IsDisoccluded compares linear depths in
// the previous view directly, as the stage keeps its previous depth linear
// (FFX_DNSR_Shadows_GetPreviousLinearDepth and
// FFX_DNSR_Shadows_ReadPreviousLinearDepth stand in for the reprojection
// matrix and the previous depth buffer). The loops' literal bounds are
// named (AR-12). The caller supplies the FFX_DNSR_Shadows_* callbacks
// upstream's host shader does.

var<workgroup> g_FFX_DNSR_Shadows_false_count:i32;
fn FFX_DNSR_Shadows_ThreadGroupAllTrue(val:bool)->bool {
 workgroupBarrier();
 g_FFX_DNSR_Shadows_false_count=0;
 workgroupBarrier();
 if !val {
  g_FFX_DNSR_Shadows_false_count=1;
 }
 workgroupBarrier();
 return workgroupUniformLoad(&g_FFX_DNSR_Shadows_false_count)==0;
}

struct FFX_DNSR_Shadows_SpatialRegion {
 all_in_light:bool,
 all_in_shadow:bool,
}

fn FFX_DNSR_Shadows_SearchSpatialRegion(gid:vec2<u32>)->FFX_DNSR_Shadows_SpatialRegion {
 // The spatial passes can reach a total region of 1+2+4 = 7x7 around each block.
 // The masks are 8x4, so we need a larger vertical stride

 // Visualization - each x represents a 4x4 block, xx is one entire 8x4 mask as read from the raytracer result
 // Same for yy, these are the ones we are working on right now

 // xx xx xx
 // xx xx xx
 // xx yy xx <-- yy here is the base_tile below
 // xx yy xx
 // xx xx xx
 // xx xx xx

 // All of this should result in scalar ops
 let base_tile=vec2<i32>(FFX_DNSR_Shadows_GetTileIndexFromPixelPosition(gid*vec2(8u,8u)));

 // Load the entire region of masks in a scalar fashion
 var combined_or_mask=0u;
 var combined_and_mask=0xFFFFFFFFu;
 let dimensions=FFX_DNSR_Shadows_GetBufferDimensions();
 let tiles=vec2<i32>(vec2(FFX_DNSR_Shadows_RoundedDivide(dimensions.x,8u),FFX_DNSR_Shadows_RoundedDivide(dimensions.y,4u)));
 for (var j=-FFX_DNSR_SHADOWS_REGION_ROWS_ABOVE;j<=FFX_DNSR_SHADOWS_REGION_ROWS_BELOW;j++) {
  for (var i=-FFX_DNSR_SHADOWS_REGION_COLUMNS;i<=FFX_DNSR_SHADOWS_REGION_COLUMNS;i++) {
   let tile_index=clamp(base_tile+vec2(i,j),vec2(0),tiles-1);
   let linear_tile_index=FFX_DNSR_Shadows_LinearTileIndex(vec2<u32>(tile_index),dimensions.x);
   let shadow_mask=FFX_DNSR_Shadows_ReadRaytracedShadowMask(linear_tile_index);

   combined_or_mask=combined_or_mask|shadow_mask;
   combined_and_mask=combined_and_mask&shadow_mask;
  }
 }

 return FFX_DNSR_Shadows_SpatialRegion(combined_and_mask==0xFFFFFFFFu,combined_or_mask==0u);
}

fn FFX_DNSR_Shadows_GetLinearDepth(did:vec2<u32>,depth:f32)->f32 {
 let uv=(vec2<f32>(did)+.5)*FFX_DNSR_Shadows_GetInvBufferDimensions();
 let ndc=2.*vec2(uv.x,1.-uv.y)-1.;

 let projected=FFX_DNSR_Shadows_GetProjectionInverse()*vec4(ndc,depth,1.);
 return abs(projected.z/projected.w);
}

fn FFX_DNSR_Shadows_IsDisoccluded(did:vec2<u32>,depth:f32,velocity:vec2<f32>)->bool {
 let dims=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions());
 let texel_size=FFX_DNSR_Shadows_GetInvBufferDimensions();
 let uv=(vec2<f32>(did)+.5)*texel_size;
 let ndc=(2.*uv-1.)*vec2(1.,-1.);
 let previous_uv=uv-velocity;

 var is_disoccluded=true;
 if all(previous_uv>vec2(0.)) && all(previous_uv<vec2(1.)) {
  // Read the center values
  let normal=FFX_DNSR_Shadows_ReadNormals(did);

  // How aligned with the view vector? (the more Z aligned, the higher the depth errors)
  let homogeneous=FFX_DNSR_Shadows_GetViewProjectionInverse()*vec4(ndc,depth,1.);
  let world_position=homogeneous.xyz/homogeneous.w; // perspective divide
  let view_direction=normalize(FFX_DNSR_Shadows_GetEye()-world_position);
  var z_alignment=1.-dot(view_direction,normal);
  z_alignment=pow(z_alignment,8.);

  // Calculate the depth difference
  let linear_depth=FFX_DNSR_Shadows_GetPreviousLinearDepth(world_position); // get linear depth

  let idx=vec2<i32>(previous_uv*vec2<f32>(dims));
  let previous_depth=FFX_DNSR_Shadows_ReadPreviousLinearDepth(idx);
  let depth_difference=abs(previous_depth-linear_depth)/linear_depth;

  // Resolve into the disocclusion mask
  let depth_tolerance=mix(1e-2,1e-1,z_alignment);
  is_disoccluded=depth_difference>=depth_tolerance;
 }

 return is_disoccluded;
}

// The 2×2 quads' depths and velocities, exchanged across x and then y, as
// QuadReadAcrossX and QuadReadAcrossY read them.
var<workgroup> g_FFX_DNSR_Shadows_quad_depth:array<f32,64>;
var<workgroup> g_FFX_DNSR_Shadows_quad_velocity:array<vec2<f32>,64>;

fn FFX_DNSR_Shadows_GetClosestVelocity(did:vec2<i32>,gtid:vec2<u32>,depth:f32)->vec2<f32> {
 var closest_velocity=FFX_DNSR_Shadows_ReadVelocity(vec2<u32>(did));
 var closest_depth=depth;

 let lane=gtid.y*8u+gtid.x;
 g_FFX_DNSR_Shadows_quad_depth[lane]=closest_depth;
 g_FFX_DNSR_Shadows_quad_velocity[lane]=closest_velocity;
 workgroupBarrier();
 var new_depth=g_FFX_DNSR_Shadows_quad_depth[lane^1u];
 var new_velocity=g_FFX_DNSR_Shadows_quad_velocity[lane^1u];
 // INVERTED_DEPTH_RANGE
 if new_depth>closest_depth {
  closest_depth=new_depth;
  closest_velocity=new_velocity;
 }

 workgroupBarrier();
 g_FFX_DNSR_Shadows_quad_depth[lane]=closest_depth;
 g_FFX_DNSR_Shadows_quad_velocity[lane]=closest_velocity;
 workgroupBarrier();
 new_depth=g_FFX_DNSR_Shadows_quad_depth[lane^8u];
 new_velocity=g_FFX_DNSR_Shadows_quad_velocity[lane^8u];
 // INVERTED_DEPTH_RANGE
 if new_depth>closest_depth {
  closest_depth=new_depth;
  closest_velocity=new_velocity;
 }

 return closest_velocity;
}

const KERNEL_RADIUS:i32=8;
// The loops' bounds upstream writes as literals (AR-12): the tiles about a
// group's that SearchSpatialRegion reads, from two rows above to three
// below and a column each side; and the pixels of a tile's row, whose
// bits HorizontalNeighborhood weighs either side of the centre.
const FFX_DNSR_SHADOWS_REGION_ROWS_ABOVE:i32=2;
const FFX_DNSR_SHADOWS_REGION_ROWS_BELOW:i32=3;
const FFX_DNSR_SHADOWS_REGION_COLUMNS:i32=1;
const FFX_DNSR_SHADOWS_TILE_ROW:i32=8;
fn FFX_DNSR_Shadows_KERNEL_WEIGHT(i:f32)->f32 {
 return exp(-3.*i*i/((f32(KERNEL_RADIUS)+1.)*(f32(KERNEL_RADIUS)+1.)));
}
fn FFX_DNSR_Shadows_KernelWeight(i:f32)->f32 {
 // Statically initialize kernel_weights_sum
 var kernel_weights_sum=0.;
 kernel_weights_sum+=FFX_DNSR_Shadows_KERNEL_WEIGHT(0.);
 for (var c=1;c<=KERNEL_RADIUS;c++) {
  kernel_weights_sum+=2.*FFX_DNSR_Shadows_KERNEL_WEIGHT(f32(c)); // Add other half of the kernel to the sum
 }
 let inv_kernel_weights_sum=1./kernel_weights_sum;

 // The only runtime code in this function
 return FFX_DNSR_Shadows_KERNEL_WEIGHT(i)*inv_kernel_weights_sum;
}

fn FFX_DNSR_Shadows_AccumulateMoments(value:f32,weight:f32,moments:ptr<function,f32>) {
 // We get value from the horizontal neighborhood calculations. Thus, it's both mean and variance due to using one sample per pixel
 *moments+=value*weight;
}

// The horizontal part of a 17x17 local neighborhood kernel
fn FFX_DNSR_Shadows_HorizontalNeighborhood(did:vec2<i32>)->f32 {
 let base_did=did;

 // Prevent vertical out of bounds access
 if (base_did.y<0) || (base_did.y>=i32(FFX_DNSR_Shadows_GetBufferDimensions().y)) {
  return 0.;
 }

 let tile_index=FFX_DNSR_Shadows_GetTileIndexFromPixelPosition(vec2<u32>(base_did));
 let linear_tile_index=FFX_DNSR_Shadows_LinearTileIndex(tile_index,FFX_DNSR_Shadows_GetBufferDimensions().x);

 let left_tile_index=i32(linear_tile_index)-1;
 let center_tile_index=i32(linear_tile_index);
 let right_tile_index=i32(linear_tile_index)+1;

 let is_first_tile_in_row=tile_index.x==0u;
 let is_last_tile_in_row=tile_index.x==(FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u)-1u);

 var left_tile=0u;
 if !is_first_tile_in_row {
  left_tile=FFX_DNSR_Shadows_ReadRaytracedShadowMask(u32(left_tile_index));
 }
 let center_tile=FFX_DNSR_Shadows_ReadRaytracedShadowMask(u32(center_tile_index));
 var right_tile=0u;
 if !is_last_tile_in_row {
  right_tile=FFX_DNSR_Shadows_ReadRaytracedShadowMask(u32(right_tile_index));
 }

 // Construct a single uint with the lowest 17bits containing the horizontal part of the local neighborhood.

 // First extract the 8 bits of our row in each of the neighboring tiles
 let row_base_index=(u32(did.y)%4u)*8u;
 let left=(left_tile>>row_base_index)&0xFFu;
 let center=(center_tile>>row_base_index)&0xFFu;
 let right=(right_tile>>row_base_index)&0xFFu;

 // Combine them into a single mask containting [left, center, right] from least significant to most significant bit
 var neighborhood=left|(center<<8u)|(right<<16u);

 // Make sure our pixel is at bit position 9 to get the highest contribution from the filter kernel
 let bit_index_in_row=u32(did.x)%8u;
 neighborhood=neighborhood>>bit_index_in_row; // Shift out bits to the right, so the center bit ends up at bit 9.

 var moment=0.; // For one sample per pixel this is both, mean and variance

 // First 8 bits up to the center pixel
 var mask:u32;
 for (var i=0;i<FFX_DNSR_SHADOWS_TILE_ROW;i++) {
  mask=1u<<u32(i);
  moment+=select(0.,FFX_DNSR_Shadows_KernelWeight(f32(FFX_DNSR_SHADOWS_TILE_ROW-i)),(mask&neighborhood)!=0u);
 }

 // Center pixel
 mask=1u<<u32(FFX_DNSR_SHADOWS_TILE_ROW);
 moment+=select(0.,FFX_DNSR_Shadows_KernelWeight(0.),(mask&neighborhood)!=0u);

 // Last 8 bits
 for (var i=1;i<=FFX_DNSR_SHADOWS_TILE_ROW;i++) {
  mask=1u<<u32(FFX_DNSR_SHADOWS_TILE_ROW+i);
  moment+=select(0.,FFX_DNSR_Shadows_KernelWeight(f32(i)),(mask&neighborhood)!=0u);
 }

 return moment;
}

var<workgroup> g_FFX_DNSR_Shadows_neighborhood:array<array<f32,24>,8>;

fn FFX_DNSR_Shadows_ComputeLocalNeighborhood(did:vec2<i32>,gtid:vec2<i32>)->f32 {
 var local_neighborhood=0.;

 let upper=FFX_DNSR_Shadows_HorizontalNeighborhood(vec2(did.x,did.y-8));
 let center=FFX_DNSR_Shadows_HorizontalNeighborhood(vec2(did.x,did.y));
 let lower=FFX_DNSR_Shadows_HorizontalNeighborhood(vec2(did.x,did.y+8));

 g_FFX_DNSR_Shadows_neighborhood[gtid.x][gtid.y]=upper;
 g_FFX_DNSR_Shadows_neighborhood[gtid.x][gtid.y+8]=center;
 g_FFX_DNSR_Shadows_neighborhood[gtid.x][gtid.y+16]=lower;

 workgroupBarrier();

 // First combine the own values.
 // KERNEL_RADIUS pixels up is own upper and KERNEL_RADIUS pixels down is own lower value
 FFX_DNSR_Shadows_AccumulateMoments(center,FFX_DNSR_Shadows_KernelWeight(0.),&local_neighborhood);
 FFX_DNSR_Shadows_AccumulateMoments(upper,FFX_DNSR_Shadows_KernelWeight(f32(KERNEL_RADIUS)),&local_neighborhood);
 FFX_DNSR_Shadows_AccumulateMoments(lower,FFX_DNSR_Shadows_KernelWeight(f32(KERNEL_RADIUS)),&local_neighborhood);

 // Then read the neighboring values.
 for (var i=1;i<KERNEL_RADIUS;i++) {
  let upper_value=g_FFX_DNSR_Shadows_neighborhood[gtid.x][8+gtid.y-i];
  let lower_value=g_FFX_DNSR_Shadows_neighborhood[gtid.x][8+gtid.y+i];
  let weight=FFX_DNSR_Shadows_KernelWeight(f32(i));
  FFX_DNSR_Shadows_AccumulateMoments(upper_value,weight,&local_neighborhood);
  FFX_DNSR_Shadows_AccumulateMoments(lower_value,weight,&local_neighborhood);
 }

 return local_neighborhood;
}

fn FFX_DNSR_Shadows_WriteTileMetaData(gid:vec2<u32>,gtid:vec2<u32>,is_cleared:bool,all_in_light:bool) {
 if all(gtid==vec2(0u)) {
  let light_mask=select(0u,TILE_META_DATA_LIGHT_MASK,all_in_light);
  let clear_mask=select(0u,TILE_META_DATA_CLEAR_MASK,is_cleared);
  let mask=light_mask|clear_mask;
  FFX_DNSR_Shadows_WriteMetadata(gid.y*FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u)+gid.x,mask);
 }
}

fn FFX_DNSR_Shadows_ClearTargets(did:vec2<u32>,gtid:vec2<u32>,gid:vec2<u32>,shadow_value:f32,is_shadow_receiver:bool,all_in_light:bool) {
 FFX_DNSR_Shadows_WriteTileMetaData(gid,gtid,true,all_in_light);
 FFX_DNSR_Shadows_WriteReprojectionResults(did,vec2(shadow_value,0.)); // mean, variance

 let temporal_sample_count=select(0.,1.,is_shadow_receiver);
 FFX_DNSR_Shadows_WriteMoments(did,vec3(shadow_value,0.,temporal_sample_count)); // mean, variance, temporal sample count
}

fn FFX_DNSR_Shadows_TileClassification(group_index:u32,gid:vec2<u32>) {
 let gtid=FFX_DNSR_Shadows_RemapLane8x8(group_index); // Make sure we can use the QuadReadAcross intrinsics to access a 2x2 region.
 let did=gid*8u+gtid;

 let is_shadow_receiver=FFX_DNSR_Shadows_IsShadowReciever(did);

 let skip_sky=FFX_DNSR_Shadows_ThreadGroupAllTrue(!is_shadow_receiver);
 if skip_sky {
  // We have to set all resources of the tile we skipped to sensible values as neighboring active denoiser tiles might want to read them.
  FFX_DNSR_Shadows_ClearTargets(did,gtid,gid,0.,is_shadow_receiver,false);
  return;
 }

 let region=FFX_DNSR_Shadows_SearchSpatialRegion(gid);
 let all_in_light=region.all_in_light;
 let all_in_shadow=region.all_in_shadow;
 let shadow_value=select(0.,1.,all_in_light); // Either all_in_light or all_in_shadow must be true, otherwise we would not skip the tile.

 let can_skip=all_in_light || all_in_shadow;
 // We have to append the entire tile if there is a single lane that we can't skip
 let skip_tile=FFX_DNSR_Shadows_ThreadGroupAllTrue(can_skip);
 if skip_tile {
  // We have to set all resources of the tile we skipped to sensible values as neighboring active denoiser tiles might want to read them.
  FFX_DNSR_Shadows_ClearTargets(did,gtid,gid,shadow_value,is_shadow_receiver,all_in_light);
  return;
 }

 FFX_DNSR_Shadows_WriteTileMetaData(gid,gtid,false,false);

 let depth=FFX_DNSR_Shadows_ReadDepth(did);
 let velocity=FFX_DNSR_Shadows_GetClosestVelocity(vec2<i32>(did),gtid,depth); // Must happen before we deactivate lanes
 let local_neighborhood=FFX_DNSR_Shadows_ComputeLocalNeighborhood(vec2<i32>(did),vec2<i32>(gtid));

 let texel_size=FFX_DNSR_Shadows_GetInvBufferDimensions();
 let uv=(vec2<f32>(did)+.5)*texel_size;
 let history_uv=uv-velocity;
 let history_pos=vec2<i32>(history_uv*vec2<f32>(FFX_DNSR_Shadows_GetBufferDimensions()));

 let tile_index=FFX_DNSR_Shadows_GetTileIndexFromPixelPosition(did);
 let linear_tile_index=FFX_DNSR_Shadows_LinearTileIndex(tile_index,FFX_DNSR_Shadows_GetBufferDimensions().x);

 let shadow_tile=FFX_DNSR_Shadows_ReadRaytracedShadowMask(linear_tile_index);

 var moments_current=vec3(0.);
 var variance=0.;
 var shadow_clamped=0.;
 if is_shadow_receiver { // do not process sky pixels
  let hit_light=(shadow_tile&FFX_DNSR_Shadows_GetBitMaskFromPixelPosition(did))!=0u;
  let shadow_current=select(0.,1.,hit_light);

  // Perform moments and variance calculations
  {
   let is_disoccluded=FFX_DNSR_Shadows_IsDisoccluded(did,depth,velocity);
   var previous_moments=vec3(0.); // Can't trust previous moments on disocclusion
   if !is_disoccluded {
    previous_moments=FFX_DNSR_Shadows_ReadPreviousMomentsBuffer(history_pos);
   }

   let old_m=previous_moments.x;
   let old_s=previous_moments.y;
   let sample_count=previous_moments.z+1.;
   let new_m=old_m+(shadow_current-old_m)/sample_count;
   let new_s=old_s+(shadow_current-old_m)*(shadow_current-new_m);

   variance=select(1.,new_s/(sample_count-1.),sample_count>1.);
   moments_current=vec3(new_m,new_s,sample_count);
  }

  // Retrieve local neighborhood and reproject
  {
   let mean=local_neighborhood;
   var spatial_variance=local_neighborhood;

   spatial_variance=max(spatial_variance-mean*mean,0.);

   // Compute the clamping bounding box
   let std_deviation=sqrt(spatial_variance);
   let nmin=mean-.5*std_deviation;
   let nmax=mean+.5*std_deviation;

   // Clamp reprojected sample to local neighborhood
   var shadow_previous=shadow_current;
   if FFX_DNSR_Shadows_IsFirstFrame()==0 {
    shadow_previous=FFX_DNSR_Shadows_ReadHistory(history_uv);
   }

   shadow_clamped=clamp(shadow_previous,nmin,nmax);

   // Reduce history weighting
   let sigma=20.;
   let temporal_discontinuity=(shadow_previous-mean)/max(.5*std_deviation,.001);
   let sample_counter_damper=exp(-temporal_discontinuity*temporal_discontinuity/sigma);
   moments_current.z*=sample_counter_damper;

   // Boost variance on first frames
   if moments_current.z<16. {
    let variance_boost=max(16.-moments_current.z,1.);
    variance=max(variance,spatial_variance);
    variance*=variance_boost;
   }
  }

  // Perform the temporal blend
  let history_weight=sqrt(max(8.-moments_current.z,0.)/8.);
  shadow_clamped=mix(shadow_clamped,shadow_current,mix(.05,1.,history_weight));
 }

 // Output the results of the temporal pass
 FFX_DNSR_Shadows_WriteReprojectionResults(did,vec2(shadow_clamped,variance));
 FFX_DNSR_Shadows_WriteMoments(did,moments_current);
}
