// A surface's specular lobes as its environment, probes and reflections
// light them: the base and, on a coated surface, the coat. The one owner of
// each lobe's response and direction, of the lobe a screen-space method
// traces and of the formula that composes the method's result into it, which
// source completion and composition (stages/reflections/source.wgsl) and lit
// shading (surface.wgsl: probe captures, ray hits and blended surfaces) call.
// The response is Three.js 0.185.1's split-sum single scattering
// (PhysicalLightingModel, BRDF_GGX_Multiscatter's single term), the base
// beneath the coat's Fresnel and the coat weighted by its strength, as its
// finish layers them; the base reflects along the KHR anisotropy bent normal
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
// The base lobe and the coat's (no response without a coat) of a surface
// seen along `view`, with the DFG table of `tables` filtered by `filtering`
// (lookup_tables.wgsl).
fn specular_lobes(normal:vec3<f32>,coat_normal:vec3<f32>,view:vec3<f32>,f0:vec3<f32>,roughness:f32,coat:f32,coat_roughness:f32,anisotropy:vec4<f32>,tables:texture_2d_array<f32>,filtering:sampler)->array<SpecularLobe,2> {
 let nv=max(dot(normal,view),0.);
 let coat_nv=max(dot(coat_normal,view),0.);
 let coat_fresnel=pbr_coat_fresnel(coat_normal,view,coat);
 let base_response=pbr_three_single_scatter(f0,lookup_dfg(tables,filtering,nv,roughness))*(1.-coat_fresnel);
 let base_direction=pbr_anisotropy_reflection(normal,view,anisotropy,roughness);
 let coat_response=pbr_three_single_scatter(vec3(.04),lookup_dfg(tables,filtering,coat_nv,coat_roughness))*coat;
 let coat_mirror=reflect(-view,coat_normal);
 let coat_direction=normalize(mix(coat_mirror,coat_normal,pow(coat_roughness,4.)));
 return array(SpecularLobe(base_response,base_direction,roughness,nv),SpecularLobe(coat_response,coat_direction,coat_roughness,coat_nv));
}
// The lobe a screen-space method traces: the coat of a coated surface, else
// the base.
fn specular_traced_lobe(coat:f32)->u32 {
 return select(SPECULAR_BASE,SPECULAR_COAT,coat>0.);
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
 return lobe.response*(reflected.rgb*fade+fallback*(1.-reflected.a*fade));
}
