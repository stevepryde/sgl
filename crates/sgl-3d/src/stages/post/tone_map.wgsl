// Tone mapping: the frame's exposure, Bevy's colour grading and the selected
// display transform, from the HDR scene to display-referred linear colour, dithered as it
// is written to the output. Where SMAA runs, the scene is tone mapped at the
// scene size first, SMAA antialiases that, and the result is resampled to
// the output and dithered, as Godot and Bevy run SMAA after tone mapping
// and before upscaling.
//
// Colour grading ports Bevy 9d12036
// crates/bevy_core_pipeline/src/tonemapping.wesl (LEVEL_MARGIN,
// LEVEL_MARGIN_DIV, saturation, sectional_color_grading and tone_mapping's
// grading), crates/bevy_render/src/color_operations.wesl (rgb_to_hsv,
// hsv_to_rgb) and crates/bevy_render/src/maths.wesl (powsafe), MIT OR
// Apache-2.0 (src/LICENSE-bevy.txt). Changes: the frame's one exposure
// multiplies the scene first and grading has none of its own; the white
// balance always runs; rgb_to_hsv gives grey a hue of 0 instead of dividing
// by its zero chroma; a negative hue remainder is shifted into [0, 2π),
// hsv_to_rgb's input range, instead of retaining Bevy's signed remainder;
// the dark crossfade's residual weights shadows, fixing Bevy 9d12036's
// highlights residual (sectional_color_grading, line 306).
//
// AgX ports Filament ef1a133 filament/src/ToneMapper.cpp (AgxToneMapper with
// its looks, agxLook) as filament/src/details/ColorGrading.cpp applies a
// custom ToneMapper on its non-FILMIC path: from sRGB to Rec. 2020 primaries
// (selectColorGradingTransformIn, 743-755, with filament/src/ColorSpaceUtils.h),
// the tone mapper, back to sRGB and saturated (hdrColorAt, 1068-1088);
// Apache-2.0, see LICENSE-filament.txt. Modified: translated to WGSL; the colour space
// matrices and AgXOutsetMatrix (the inverse of AgXOutsetMatrixInv) are
// evaluated; the looks' zero CDL offset is left out.
//
// The deband dither ports Bevy 9d12036's DebandDither::Enabled
// (crates/bevy_core_pipeline/src/tonemapping.wesl screen_space_dither, after
// Vlachos 2015, "Advanced VR Rendering", slide 49, and its use in
// tonemapping/tonemapping_frag.wesl), MIT OR Apache-2.0
// (src/LICENSE-bevy.txt). Changes: it dithers the output, not the
// tone-mapped target, so diagnostics capture the undithered scene and the
// copy from it dithers as the direct path does.
struct ColorGrading {
 balance:mat3x3<f32>,
 saturation:vec3<f32>,
 contrast:vec3<f32>,
 gamma:vec3<f32>,
 gain:vec3<f32>,
 lift:vec3<f32>,
 midtone_range:vec2<f32>,
 hue:f32,
 post_saturation:f32,
 // TONE_KHRONOS_NEUTRAL, TONE_ACES_FILMIC or an AGX_LOOK value.
 tone_mapping:u32,
}
// The frame's exposure multiplier (`stages::exposure`).
@group(1) @binding(0) var exposure:texture_2d<f32>;
@group(1) @binding(1) var<uniform> grading:ColorGrading;
const FRAC_PI_3:f32=1.0471975512;
const PI_2:f32=6.283185307179586;
// Half the crossfade between shadows and midtones and between midtones and
// highlights.
const LEVEL_MARGIN:f32=.1;
const LEVEL_MARGIN_DIV:f32=.5/LEVEL_MARGIN;
fn powsafe(color:vec3<f32>,power:f32)->vec3<f32> {
 return pow(abs(color),vec3(power))*sign(color);
}
fn hsv_to_rgb(hsv:vec3<f32>)->vec3<f32> {
 let n=vec3(5.,3.,1.);
 let k=(n+hsv.x/FRAC_PI_3)%6.;
 return hsv.z-hsv.z*hsv.y*max(vec3(0.),min(k,min(4.-k,vec3(1.))));
}
fn rgb_to_hsv(rgb:vec3<f32>)->vec3<f32> {
 let x_max=max(rgb.r,max(rgb.g,rgb.b));
 let x_min=min(rgb.r,min(rgb.g,rgb.b));
 let c=x_max-x_min;
 var swizzle=vec3(0.);
 if x_max==rgb.r {
  swizzle=vec3(rgb.gb,0.);
 } else if x_max==rgb.g {
  swizzle=vec3(rgb.br,2.);
 } else {
  swizzle=vec3(rgb.rg,4.);
 }
 var h=0.;
 if c>0. {
  h=FRAC_PI_3*fract(((swizzle.x-swizzle.y)/c+swizzle.z)/6.)*6.;
 }
 var s=0.;
 if x_max>0. {
  s=c/x_max;
 }
 return vec3(h,s,x_max);
}
// Rec. 709 luminance mixed with the colour: 0 is grey, 1 unchanged.
fn saturation(color:vec3<f32>,saturation_amount:f32)->vec3<f32> {
 let luma=luminance(color);
 return mix(vec3(luma),color,vec3(saturation_amount));
}
// Bevy's grading of shadows, midtones and highlights, blended near the
// midtone range's ends. Bevy 9d12036 assigns the dark crossfade's residual
// to highlights; corrected here to blend the adjacent shadows and midtones.
// Reference consulted (no code copied): Blender c31386f6's
// source/blender/compositor/shaders/library/gpu_shader_compositor_color_correction.glsl,
// node_composite_color_correction, whose dark crossfade weights shadows.
fn sectional_color_grading(in:vec3<f32>)->vec3<f32> {
 var color=in;
 let level=(color.r+color.g+color.b)/3.;
 var levels=vec3(0.);
 let midtone_range=grading.midtone_range;
 if level<midtone_range.x-LEVEL_MARGIN {
  levels.x=1.;
 } else if level<midtone_range.x+LEVEL_MARGIN {
  levels.y=((level-midtone_range.x)*LEVEL_MARGIN_DIV)+.5;
  levels.x=1.-levels.y;
 } else if level<midtone_range.y-LEVEL_MARGIN {
  levels.y=1.;
 } else if level<midtone_range.y+LEVEL_MARGIN {
  levels.z=((level-midtone_range.y)*LEVEL_MARGIN_DIV)+.5;
  levels.y=1.-levels.z;
 } else {
  levels.z=1.;
 }
 let contrast=dot(levels,grading.contrast);
 let saturation=dot(levels,grading.saturation);
 let gamma=dot(levels,grading.gamma);
 let gain=dot(levels,grading.gain);
 let lift=dot(levels,grading.lift);
 let luma=luminance(color);
 color=luma+saturation*(color-luma);
 color=.5+(color-.5)*contrast;
 // ASC CDL: (i × gain + lift)^(1 / gamma).
 color=powsafe(color*gain+lift,1./gamma);
 return max(color,vec3(0.));
}
const SRGB_TO_REC2020=mat3x3<f32>(
 vec3(.627507389,.069107726,.016396480),
 vec3(.329277724,.919504464,.088024333),
 vec3(.043303847,.011359092,.895513475));
const REC2020_TO_SRGB=mat3x3<f32>(
 vec3(1.660213470,-.124552041,-.018155023),
 vec3(-.587564290,1.132946134,-.100604795),
 vec3(-.072827578,-.008348331,1.118831873));
// Blender's AgX matrices for Rec. 2020 primaries
// (EaryChow/AgX_LUT_Gen AgXBaseRec2020.py), column by column.
const AGX_INSET_MATRIX=mat3x3<f32>(
 vec3(.856627153315983,.137318972929847,.11189821299995),
 vec3(.0951212405381588,.761241990602591,.0767994186031903),
 vec3(.0482516061458583,.101439036467562,.811302368396859));
const AGX_OUTSET_MATRIX=mat3x3<f32>(
 vec3(1.127100587,-.141329765,-.141329765),
 vec3(-.110606641,1.157823682,-.110606641),
 vec3(-.016493939,-.016493937,1.251936436));
// log2(2^-10 × 0.18) and log2(2^6.5 × 0.18).
const AGX_MIN_EV:f32=-12.47393;
const AGX_MAX_EV:f32=4.026069;
// The iolite engine's polynomial fit of AgX's sigmoid.
fn agx_default_contrast_approx(x:vec3<f32>)->vec3<f32> {
 let x2=x*x;
 let x4=x2*x2;
 let x6=x4*x2;
 return -17.86*x6*x+78.01*x6-126.7*x4*x+92.06*x4-28.72*x2*x+4.361*x2-.1718*x+.002857;
}
// Filament's AgxLook values.
const TONE_KHRONOS_NEUTRAL:u32=0u;
const TONE_ACES_FILMIC:u32=1u;
const AGX_LOOK_NONE:u32=2u;
const AGX_LOOK_PUNCHY:u32=3u;
const AGX_LOOK_GOLDEN:u32=4u;
// Filament's agxLook of the sigmoid's output: an ASC CDL slope and power,
// then saturation about Rec. 709 luminance.
fn agx_look(in:vec3<f32>)->vec3<f32> {
 let look=grading.tone_mapping;
 if look==AGX_LOOK_NONE {
  return in;
 }
 let luma=luminance(in);
 var slope=vec3(1.);
 var power=vec3(1.);
 var sat=1.;
 if look==AGX_LOOK_GOLDEN {
  slope=vec3(1.,.9,.5);
  power=vec3(.8);
  sat=1.3;
 }
 if look==AGX_LOOK_PUNCHY {
  power=vec3(1.35);
  sat=1.4;
 }
 let v=pow(in*slope,power);
 return luma+sat*(v-luma);
}
// Filament's AgX of linear Rec. 2020 colour.
fn agx(color:vec3<f32>)->vec3<f32> {
 var v=max(vec3(0.),color);
 v=AGX_INSET_MATRIX*v;
 // Log2 encoding.
 v=max(v,vec3(1e-10));
 v=log2(v);
 v=(v-AGX_MIN_EV)/(AGX_MAX_EV-AGX_MIN_EV);
 v=clamp(v,vec3(0.),vec3(1.));
 v=agx_default_contrast_approx(v);
 v=agx_look(v);
 v=AGX_OUTSET_MATRIX*v;
 // Linearize.
 return pow(max(vec3(0.),v),vec3(2.2));
}
// KhronosGroup/ToneMapping 180b1a7, PBR_Neutral/pbrNeutral.glsl,
// PBRNeutralToneMapping; Apache-2.0, LICENSE-khronos-tone-mapping.txt.
// Translated to WGSL; exposure is applied once by tone_map before grading.
fn khronos_pbr_neutral(input:vec3<f32>)->vec3<f32> {
 const START_COMPRESSION:f32=.8-.04;
 const DESATURATION:f32=.15;
 let x=min(input.r,min(input.g,input.b));
 var offset=.04;
 if x<.08 { offset=x-6.25*x*x; }
 var color=input-vec3(offset);
 let peak=max(color.r,max(color.g,color.b));
 if peak<START_COMPRESSION { return color; }
 let d=1.-START_COMPRESSION;
 let new_peak=1.-d*d/(peak+d-START_COMPRESSION);
 color*=new_peak/peak;
 let g=1.-1./(DESATURATION*(peak-new_peak)+1.);
 return mix(color,vec3(new_peak),g);
}
// Three.js r185 (2431a09), ACESFilmicToneMapping / RRTAndODTFit,
// src/renderers/shaders/ShaderChunk/tonemapping_pars_fragment.glsl.js;
// MIT, smaa/LICENSE-three.txt. Stephen Hill's ACES fit follows
// selfshadow/ltc_code; LICENSE-ltc.txt. Translated to WGSL; exposure remains
// the frame's one multiplier. The intrinsic Three.js 1/0.6 scale is retained.
fn aces_filmic(color:vec3<f32>)->vec3<f32> {
 let input_matrix=mat3x3<f32>(
  vec3(.59719,.07600,.02840),
  vec3(.35458,.90834,.13383),
  vec3(.04823,.01566,.83777));
 let output_matrix=mat3x3<f32>(
  vec3(1.60475,-.10208,-.00327),
  vec3(-.53108,1.10813,-.07276),
  vec3(-.07367,-.00605,1.07602));
 let v=input_matrix*(color/.6);
 let a=v*(v+.0245786)-.000090537;
 let b=v*(.983729*v+.4329510)+.238081;
 return clamp(output_matrix*(a/b),vec3(0.),vec3(1.));
}
// The exposed, graded and tone-mapped colour of linear HDR `hdr`.
fn tone_map(hdr:vec3<f32>)->vec3<f32> {
 var color=max(hdr,vec3(0.))*textureLoad(exposure,vec2(0),0).r;
 if grading.hue!=0. {
  var hsv=rgb_to_hsv(color);
  hsv.r=(hsv.r+grading.hue)%PI_2;
  // WGSL's remainder keeps the dividend's sign; HSV needs a positive angle.
  if hsv.r<0. {
   hsv.r+=PI_2;
  }
  color=hsv_to_rgb(hsv);
 }
 color=max(grading.balance*color,vec3(0.));
 color=sectional_color_grading(color);
 if grading.tone_mapping==TONE_KHRONOS_NEUTRAL {
  color=khronos_pbr_neutral(color);
 } else if grading.tone_mapping==TONE_ACES_FILMIC {
  color=aces_filmic(color);
 } else {
  color=REC2020_TO_SRGB*agx(SRGB_TO_REC2020*color);
 }
 color=saturation(color,grading.post_saturation);
 return clamp(color,vec3(0.),vec3(1.));
}
// Bevy's screen_space_dither: up to half an 8-bit step either way per
// channel, from the output pixel's position.
fn screen_space_dither(frag_coord:vec2<f32>)->vec3<f32> {
 var dither=vec3(dot(vec2(171.,231.),frag_coord));
 dither=fract(dither/vec3(103.,71.,97.));
 return (dither-.5)/255.;
}
// Whether the output stores what is written, so its colour is written
// sRGB-encoded (post::Output): a pipeline constant of the
// pipelines that write the output.
override OUTPUT_ENCODES_SRGB:bool=false;
// Display-referred `color` as the output takes it, dithered at the output
// pixel `frag_coord` as Bevy's tonemapping_frag.wesl does, against banding
// where it quantizes to 8 bits: in the sRGB encoding written here where the
// output stores what is written (OUTPUT_ENCODES_SRGB), as sgl-2d's blit
// encodes for such a surface; else linear, dithered in a 2.2 gamma
// approximation of the encoding its attachment then performs.
fn dither(color:vec3<f32>,frag_coord:vec2<f32>)->vec3<f32> {
 if OUTPUT_ENCODES_SRGB {
  return linear_to_srgb(max(color,vec3(0.)))+screen_space_dither(frag_coord);
 }
 return powsafe(powsafe(color,1./2.2)+screen_space_dither(frag_coord),2.2);
}
// The tone-mapped target is already at output resolution: the copy dithers
// each texel as the direct path does, encoded for the output alike.
@fragment fn copy_pixel(i:Output)->@location(0) vec4<f32> {
 let color=textureLoad(scene,vec2<i32>(i.position.xy),0);
 return vec4(dither(color.rgb,i.position.xy),color.a);
}
// Display-referred `color` written to the output pixel `frag_coord` as the
// tone-mapped target's copy writes it: rounded through half floats as the
// RGBA16F target holds them (within one 8-bit code of it; WGSL leaves the
// conversion's rounding open), then dithered, without an otherwise
// redundant full-resolution image.
fn present_rounded(color:vec3<f32>,frag_coord:vec2<f32>)->vec4<f32> {
 let rg=unpack2x16float(pack2x16float(color.rg));
 let ba=unpack2x16float(pack2x16float(vec2(color.b,1.)));
 return vec4(dither(vec3(rg,ba.x),frag_coord),ba.y);
}
@fragment fn present(i:Output)->@location(0) vec4<f32> {
 return vec4(tone_map(textureSample(scene,linear_sampler,i.uv).rgb),1.);
}
@fragment fn present_direct(i:Output)->@location(0) vec4<f32> {
 return present_rounded(tone_map(textureSample(scene,linear_sampler,i.uv).rgb),i.position.xy);
}
// The antialiased tone-mapped scene, at the scene size, resampled to the
// tone-mapped target.
@fragment fn resample(i:Output)->@location(0) vec4<f32> {
 return vec4(textureSample(scene,linear_sampler,i.uv).rgb,1.);
}
// The antialiased tone-mapped scene resampled to the output, as `resample`
// and the copy write it.
@fragment fn resample_direct(i:Output)->@location(0) vec4<f32> {
 return present_rounded(textureSample(scene,linear_sampler,i.uv).rgb,i.position.xy);
}
