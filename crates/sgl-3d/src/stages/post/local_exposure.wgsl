// Local exposure (stages/post/local_exposure.rs): the constants its passes
// and the tone map share, the log luminance they grid, and the tone map's
// lookup. Built from Patry, "Real-Time Samurai Cinema" (SIGGRAPH 2021
// Advances, slides 130-146), on Chen, Paris and Durand's bilateral grid
// (SIGGRAPH 2007); no code is ported. The Rust owner of each constant is
// local_exposure.rs, which the layout test ties to these twins.

// Grid cells along the frame's longer side.
const LOCAL_EXPOSURE_CELLS:u32=64u;
// Slices spanning -RANGE..=RANGE stops, 4/3 stop apart.
const LOCAL_EXPOSURE_SLICES:u32=13u;
// The stops about middle grey the grid spans, auto exposure's range.
const LOCAL_EXPOSURE_RANGE:f32=8.;
// The pivot: middle grey after the frame's exposure.
const LOCAL_EXPOSURE_MIDDLE_GREY:f32=.18;
// Patry's blend: 40% bilateral, 60% wide Gaussian.
const LOCAL_EXPOSURE_BILATERAL_SHARE:f32=.4;
// Below this sampled grid weight the neighbourhood is empty and the base
// is the Gaussian alone. A texel's own contribution keeps its own lookup
// above it.
const LOCAL_EXPOSURE_MIN_WEIGHT:f32=1e-4;

// V: the exposed colour's log2 luminance in stops about middle grey, within
// the grid's range.
fn local_exposure_v(exposed:vec3<f32>)->f32 {
 let stops=log2(max(luminance(exposed),exp2(-24.))/LOCAL_EXPOSURE_MIDDLE_GREY);
 return clamp(stops,-LOCAL_EXPOSURE_RANGE,LOCAL_EXPOSURE_RANGE);
}

// V's position along the slices, 0 at -RANGE and SLICES - 1 at +RANGE.
fn local_exposure_slice(v:f32)->f32 {
 return (v+LOCAL_EXPOSURE_RANGE)*f32(LOCAL_EXPOSURE_SLICES-1u)/(2.*LOCAL_EXPOSURE_RANGE);
}

// `LocalExposureUniform`: the authored contrasts while the lookup runs.
struct LocalExposure {
 highlight_contrast:f32,
 shadow_contrast:f32,
 // 0 while local exposure is off: the tone map takes no sample.
 enabled:u32,
 _padding:u32,
}

// The multiplier local exposure applies to the exposed colour at `uv`:
// 2^((c - 1) B), Patry's Io = c (B - M) + d (Ii - B) + M with d = 1 and
// M = 0 in V. B is 40% the grid sliced at the pixel's own V (trilinear,
// normalised by its weight; the grid holds V / RANGE) and 60% the
// upsampled Gaussian base, or the Gaussian alone where the grid holds no
// weight. c is the highlight contrast above middle grey, the shadow
// contrast below.
fn local_exposure_multiplier(
 grid:texture_3d<f32>,
 base:texture_2d<f32>,
 linear:sampler,
 uv:vec2<f32>,
 exposed:vec3<f32>,
 contrasts:LocalExposure,
)->f32 {
 let z=(local_exposure_slice(local_exposure_v(exposed))+.5)/f32(LOCAL_EXPOSURE_SLICES);
 let cell=textureSampleLevel(grid,linear,vec3(uv,z),0.).rg;
 let gaussian=textureSampleLevel(base,linear,uv,0.).r;
 var b=gaussian;
 if cell.y>=LOCAL_EXPOSURE_MIN_WEIGHT {
  b=mix(gaussian,LOCAL_EXPOSURE_RANGE*cell.x/cell.y,LOCAL_EXPOSURE_BILATERAL_SHARE);
 }
 let c=select(contrasts.shadow_contrast,contrasts.highlight_contrast,b>0.);
 return exp2((c-1.)*b);
}
