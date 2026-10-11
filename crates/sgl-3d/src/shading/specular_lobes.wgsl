// A surface's specular lobes as its environment, probes and reflections
// light them: the base and, on a coated surface, the coat. The one owner of
// each lobe's response and direction, of the lobe a screen-space method
// traces and of the formula that composes the method's result into it
// (occlusion.wgsl occludes a lobe's environment), which
// source completion and composition (stages/reflections/source.wgsl) and lit
// shading (surface.wgsl: probe captures, ray hits and blended surfaces) call.
// The response is the split sum's single scattering (pbr_split_sum), the base's
// toward its F90 (surface_f90) and the coat's toward 1, the base beneath the
// coat's Fresnel and the coat weighted by its strength, as three.js
// 0.185.1's PhysicalLightingModel.finish layers them; the base reflects along the KHR anisotropy bent normal
// (anisotropy.wgsl), the coat along its normal's mirror direction, each bent
// toward its normal with roughness.
struct SpecularLobe {
 response:vec3<f32>,
 // The direction the lobe takes its environment from.
 direction:vec3<f32>,
 // Perceptual roughness.
 roughness:f32,
 // The cosine between the lobe's normal and the view.
 nv:f32,
}
const SPECULAR_BASE:u32=0u;
const SPECULAR_COAT:u32=1u;
// The cosine between a lobe's `normal` and the `view`, at which its DFG
// table is read.
fn specular_nv(normal:vec3<f32>,view:vec3<f32>)->f32 {
 return max(dot(normal,view),0.);
}
// The base lobe and the coat's (no response without a coat) of a surface
// seen along `view`, its base reflecting `f0` at normal and `f90` at
// grazing incidence. `base_dfg` is the DFG table at the base's
// specular_nv and `roughness`, which the caller has read for its own terms;
// the coat's is read from `tables`, filtered by `filtering`
// (lookup_tables.wgsl), only on a coated surface.
fn specular_lobes(normal:vec3<f32>,coat_normal:vec3<f32>,view:vec3<f32>,f0:vec3<f32>,f90:f32,roughness:f32,base_dfg:vec2<f32>,coat:f32,coat_roughness:f32,anisotropy:vec4<f32>,tables:texture_2d_array<f32>,filtering:sampler)->array<SpecularLobe,2> {
 let nv=specular_nv(normal,view);
 let coat_nv=specular_nv(coat_normal,view);
 let coat_fresnel=pbr_coat_fresnel(coat_normal,view,coat);
 let base_response=pbr_split_sum(f0,f90,base_dfg)*(1.-coat_fresnel);
 let base_direction=pbr_anisotropy_reflection(normal,view,anisotropy,roughness);
 var coat_dfg=vec2(0.);
 if coat>0. {
  coat_dfg=lookup_dfg(tables,filtering,coat_nv,coat_roughness);
 }
 let coat_response=pbr_split_sum(vec3(.04),1.,coat_dfg)*coat;
 let coat_mirror=reflect(-view,coat_normal);
 let coat_direction=normalize(mix(coat_mirror,coat_normal,pow(coat_roughness,4.)));
 return array(SpecularLobe(base_response,base_direction,roughness,nv),SpecularLobe(coat_response,coat_direction,coat_roughness,coat_nv));
}
// The lobe a screen-space method traces: the coat of a coated surface, else
// the base. The G-buffer's traced normal and roughness
// (gbuffer_reflection_normal, gbuffer_traced_roughness) select by it.
fn specular_traced_lobe(coat:f32)->u32 {
 return select(SPECULAR_BASE,SPECULAR_COAT,coat>0.);
}
// Whether a method traces a lobe of perceptual `roughness`: below its
// cutoff, given as the alpha roughness `traced` (the cutoff squared; 0 traces
// nothing).
fn specular_traces(roughness:f32,traced:f32)->bool {
 return roughness*roughness<traced;
}
// What a screen-space method returned for a surface's traced lobe at its
// pixel: radiance premultiplied by the confidence in it (rgb) and that
// confidence (a), with the method's cutoff and fade in perceptual roughness
// (specular_trace_fade). A cutoff of 0 is no result: the lobe takes its
// environment specular alone.
struct TracedReflection {
 reflected:vec4<f32>,
 cutoff:f32,
 fade:f32,
}
fn untraced_reflection()->TracedReflection {
 return TracedReflection(vec4(0.),0.,0.);
}
// The share of a screen-space method's result a lobe of perceptual
// `roughness` takes: none at the method's `cutoff`, rising smoothly over the
// `fade` of perceptual roughness below it to all, so no seam shows where a
// roughness crosses the cutoff.
fn specular_trace_fade(roughness:f32,cutoff:f32,fade:f32)->f32 {
 return 1.-smoothstep(cutoff-fade,cutoff,roughness);
}
// The traced lobe's specular: its response times the method's radiance
// (`reflected.rgb`, premultiplied by its confidence `reflected.a`) where the
// lobe takes it (`fade`, specular_trace_fade), and its `fallback` radiance
// for the rest, so the lobe counts once whatever share of it the method
// resolved. Bevy composites its SSR this way, and AMD's SSSR blends the
// environment into each missed ray.
fn specular_traced(lobe:SpecularLobe,reflected:vec4<f32>,fade:f32,fallback:vec3<f32>)->vec3<f32> {
 return lobe.response*(reflected.rgb*fade+fallback*specular_fallback_share(reflected.a,fade));
}
// The share of the traced lobe specular_traced gives its fallback where the
// method's result holds `confidence` and the lobe takes `fade` of it.
fn specular_fallback_share(confidence:f32,fade:f32)->f32 {
 return 1.-confidence*fade;
}
