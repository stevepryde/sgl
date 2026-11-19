// Local exposure's passes (stages/post/local_exposure.rs), one compute pass
// of dispatches: the bilateral grid and cell means built from the image the
// tone map is about to read, the grid blurred along x, y and slice, the
// means blurred by a wide Gaussian along x and y, and the blurred means
// upsampled four times each way by a cubic B-spline. Patry (SIGGRAPH 2021
// Advances, slides 130-146) and Chen, Paris and Durand (SIGGRAPH 2007,
// sections 2.3 and 3); see local_exposure.wgsl.

// The build reads the image and the frame's 1x1 exposure multiplier and
// writes the grid's sums and the cells' mean V.
@group(0) @binding(0) var local_exposure_scene:texture_2d<f32>;
@group(0) @binding(1) var local_exposure_exposure:texture_2d<f32>;
@group(0) @binding(2) var local_exposure_grid_out:texture_storage_3d<rgba16float,write>;
@group(0) @binding(3) var local_exposure_mean_out:texture_storage_2d<rgba16float,write>;
// The grid blur reads one grid and writes the other.
@group(0) @binding(4) var local_exposure_grid_in:texture_3d<f32>;
@group(0) @binding(5) var local_exposure_grid_blurred:texture_storage_3d<rgba16float,write>;
// The Gaussian and the upsample read one 2D image and write another.
@group(0) @binding(6) var local_exposure_image_in:texture_2d<f32>;
@group(0) @binding(7) var local_exposure_image_out:texture_storage_2d<rgba16float,write>;

// The axis a blur pipeline runs along: 0 x, 1 y, 2 slice.
override LOCAL_EXPOSURE_AXIS:u32=0u;

// The build walks at most TILE_SIDE x TILE_SIDE texels of a cell, one
// budget for both axes: a cell of a scene 8192 texels on its longer side
// is 128 across, beyond any size WebGPU's default limits allow. A larger
// cell is walked at a uniform stride, so the walk never grows. It also
// keeps every per-slice sum within binary16 (at most 16384) and a lone
// texel's weight far above binary16's smallest normal after the blur.
const LOCAL_EXPOSURE_TILE_SIDE:u32=128u;
// Chen's five-tap kernel an axis: the binomial 1 4 6 4 1 / 16, a variance
// of one cell (one slice).
const LOCAL_EXPOSURE_GRID_TAPS:i32=5;
// The Gaussian of the cells' mean: Patry's radius of 13 cells, sigma a
// third of it, 27 taps.
const LOCAL_EXPOSURE_GAUSSIAN_RADIUS:i32=13;
const LOCAL_EXPOSURE_GAUSSIAN_SIGMA:f32=13./3.;
// The upsample's factor each way and its cubic B-spline's 4 x 4 taps.
const LOCAL_EXPOSURE_UPSAMPLE:u32=4u;
const LOCAL_EXPOSURE_SPLINE_TAPS:i32=4;

// The build's workgroup: BUILD_THREADS invocations share a cell, each
// walking every BUILD_THREADS-th of its texels; their sums are added in a
// tree of BUILD_STEPS halvings (local_exposure.rs gives the measurements).
const LOCAL_EXPOSURE_BUILD_THREADS:u32=32u;
const LOCAL_EXPOSURE_BUILD_STEPS:u32=5u;
var<workgroup> local_exposure_partial_sums:array<vec2<f32>,LOCAL_EXPOSURE_BUILD_THREADS*LOCAL_EXPOSURE_SLICES>;
var<workgroup> local_exposure_partial_totals:array<f32,LOCAL_EXPOSURE_BUILD_THREADS>;

// One workgroup a cell: the cell's texels (every one, or a uniform stride
// of them past TILE_SIDE) each add V / RANGE and 1, weighted linearly
// between the two nearest slices and not at all in x or y (Patry, slide
// 136), to the cell's slice sums, which are stored undivided as (sum of
// w V / RANGE, sum of w). The cell's mean V is stored beside them. A cell
// is the texels from floor(i size / cells) to floor((i + 1) size / cells),
// at least one each way. The dispatch is one workgroup a cell exactly.
@compute @workgroup_size(LOCAL_EXPOSURE_BUILD_THREADS,1,1)
fn local_exposure_build(
 @builtin(workgroup_id) cell:vec3<u32>,
 @builtin(local_invocation_index) thread:u32,
) {
 let cells=textureDimensions(local_exposure_mean_out);
 let size=textureDimensions(local_exposure_scene);
 let start=cell.xy*size/cells;
 let extent=max((cell.xy+1u)*size/cells-start,vec2(1u));
 let stride=(extent+LOCAL_EXPOSURE_TILE_SIDE-1u)/LOCAL_EXPOSURE_TILE_SIDE;
 let steps=(extent+stride-1u)/stride;
 let count=steps.x*steps.y;
 let exposure=textureLoad(local_exposure_exposure,vec2(0),0).r;
 var sums:array<vec2<f32>,LOCAL_EXPOSURE_SLICES>;
 var total=0.;
 // At most TILE_SIDE x TILE_SIDE texels a cell, shared by the threads.
 for (var i=0u;i<LOCAL_EXPOSURE_TILE_SIDE*LOCAL_EXPOSURE_TILE_SIDE/LOCAL_EXPOSURE_BUILD_THREADS;i++) {
  let k=thread+i*LOCAL_EXPOSURE_BUILD_THREADS;
  if k>=count {
   break;
  }
  let p=start+vec2(k%steps.x,k/steps.x)*stride;
  let color=finite_scene_color(textureLoad(local_exposure_scene,p,0)).rgb;
  let v=local_exposure_v(max(color,vec3(0.))*exposure);
  let z=local_exposure_slice(v);
  let lower=min(u32(z),LOCAL_EXPOSURE_SLICES-2u);
  let upper_weight=z-f32(lower);
  sums[lower]+=(1.-upper_weight)*vec2(v/LOCAL_EXPOSURE_RANGE,1.);
  sums[lower+1u]+=upper_weight*vec2(v/LOCAL_EXPOSURE_RANGE,1.);
  total+=v;
 }
 for (var s=0u;s<LOCAL_EXPOSURE_SLICES;s++) {
  local_exposure_partial_sums[thread*LOCAL_EXPOSURE_SLICES+s]=sums[s];
 }
 local_exposure_partial_totals[thread]=total;
 workgroupBarrier();
 for (var level=0u;level<LOCAL_EXPOSURE_BUILD_STEPS;level++) {
  let partner=LOCAL_EXPOSURE_BUILD_THREADS>>(level+1u);
  if thread<partner {
   for (var s=0u;s<LOCAL_EXPOSURE_SLICES;s++) {
    local_exposure_partial_sums[thread*LOCAL_EXPOSURE_SLICES+s]+=
     local_exposure_partial_sums[(thread+partner)*LOCAL_EXPOSURE_SLICES+s];
   }
   local_exposure_partial_totals[thread]+=local_exposure_partial_totals[thread+partner];
  }
  workgroupBarrier();
 }
 if thread==0u {
  for (var s=0u;s<LOCAL_EXPOSURE_SLICES;s++) {
   textureStore(local_exposure_grid_out,vec3(cell.xy,s),vec4(local_exposure_partial_sums[s],0.,0.));
  }
  textureStore(local_exposure_mean_out,cell.xy,vec4(local_exposure_partial_totals[0]/f32(count),0.,0.,0.));
 }
}

// The unit step along LOCAL_EXPOSURE_AXIS.
fn local_exposure_axis()->vec3<i32> {
 return vec3(
  i32(LOCAL_EXPOSURE_AXIS==0u),
  i32(LOCAL_EXPOSURE_AXIS==1u),
  i32(LOCAL_EXPOSURE_AXIS==2u));
}

// One axis of the grid's blur: the binomial kernel, edges clamped.
@compute @workgroup_size(8,8,1)
fn local_exposure_blur_grid(@builtin(global_invocation_id) id:vec3<u32>) {
 let size=vec3<i32>(textureDimensions(local_exposure_grid_in));
 let p=vec3<i32>(id);
 if any(p>=size) {
  return;
 }
 let weights=array<f32,LOCAL_EXPOSURE_GRID_TAPS>(1.,4.,6.,4.,1.);
 var sum=vec4(0.);
 for (var t=0;t<LOCAL_EXPOSURE_GRID_TAPS;t++) {
  let q=clamp(p+(t-LOCAL_EXPOSURE_GRID_TAPS/2)*local_exposure_axis(),vec3(0),size-1);
  sum+=weights[t]*textureLoad(local_exposure_grid_in,q,0);
 }
 textureStore(local_exposure_grid_blurred,id,sum/16.);
}

// One axis of the cells' mean's Gaussian, normalised, edges clamped.
@compute @workgroup_size(8,8,1)
fn local_exposure_gaussian(@builtin(global_invocation_id) id:vec3<u32>) {
 let size=vec2<i32>(textureDimensions(local_exposure_image_in));
 let p=vec2<i32>(id.xy);
 if any(p>=size) {
  return;
 }
 let axis=local_exposure_axis().xy;
 var sum=0.;
 var weights=0.;
 for (var t=-LOCAL_EXPOSURE_GAUSSIAN_RADIUS;t<=LOCAL_EXPOSURE_GAUSSIAN_RADIUS;t++) {
  let w=exp(-f32(t*t)/(2.*LOCAL_EXPOSURE_GAUSSIAN_SIGMA*LOCAL_EXPOSURE_GAUSSIAN_SIGMA));
  let q=clamp(p+t*axis,vec2(0),size-1);
  sum+=w*textureLoad(local_exposure_image_in,q,0).r;
  weights+=w;
 }
 textureStore(local_exposure_image_out,p,vec4(sum/weights,0.,0.,0.));
}

// The uniform cubic B-spline's weights at fraction f for the taps at
// -1, 0, 1 and 2.
fn local_exposure_spline(f:f32)->vec4<f32> {
 let g=1.-f;
 return vec4(
  g*g*g,
  3.*f*f*f-6.*f*f+4.,
  -3.*f*f*f+3.*f*f+3.*f+1.,
  f*f*f)/6.;
}

// The blurred means upsampled UPSAMPLE times each way by the cubic
// B-spline (Patry, slide 140), edges clamped.
@compute @workgroup_size(8,8,1)
fn local_exposure_upsample(@builtin(global_invocation_id) id:vec3<u32>) {
 let out_size=textureDimensions(local_exposure_image_out);
 if any(id.xy>=out_size) {
  return;
 }
 let size=vec2<i32>(textureDimensions(local_exposure_image_in));
 let position=(vec2<f32>(id.xy)+.5)/f32(LOCAL_EXPOSURE_UPSAMPLE)-.5;
 let corner=floor(position);
 let wx=local_exposure_spline(position.x-corner.x);
 let wy=local_exposure_spline(position.y-corner.y);
 var sum=0.;
 for (var t=0;t<LOCAL_EXPOSURE_SPLINE_TAPS*LOCAL_EXPOSURE_SPLINE_TAPS;t++) {
  let tap=vec2(t%LOCAL_EXPOSURE_SPLINE_TAPS,t/LOCAL_EXPOSURE_SPLINE_TAPS);
  let q=clamp(vec2<i32>(corner)+tap-1,vec2(0),size-1);
  sum+=wx[tap.x]*wy[tap.y]*textureLoad(local_exposure_image_in,q,0).r;
 }
 textureStore(local_exposure_image_out,id.xy,vec4(sum,0.,0.,0.));
}
