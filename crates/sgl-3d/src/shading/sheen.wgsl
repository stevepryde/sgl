// KHR_materials_sheen's layer (Khronos glTF acfcbe65): a sheen of colour
// `sheen` and perceptual roughness over the base, the base dimmed by the
// layer's albedo, beneath any clearcoat (README 71, 145–170). Filament
// ef1a133's sheen, Apache-2.0 (src/LICENSE-filament.txt): D_Charlie and
// V_Neubelt (shaders/src/surface_brdf.fs 94–99, 139–143), the lobe the two
// make (surface_shading_model_standard.fs 1–7, 146–149), and the base's
// scaling by 1 − max3(sheen) E at the view, E the lobe's directional albedo
// from the DFG table's blue channel (scene/lookup_tables.rs), for lights and
// the environment alike (surface_shading_lit.fs 274–276;
// surface_light_indirect.fs 388–401). Changed (D-32): V_Neubelt takes
// Filament's mobile guard against a zero denominator; the scaling is
// clamped to [0, 1], since E exceeds 1 at grazing views of the smoothest
// sheens; and surface.wgsl lights the layer's indirect lobe by irradiance
// (shade_lit), where Filament takes prefiltered radiance.
fn D_Charlie(roughness:f32,nh:f32)->f32 {
 // Estevez and Kulla 2017, "Production Friendly Microfacet Sheen BRDF"
 let inv_alpha=1./roughness;
 let cos2h=nh*nh;
 let sin2h=max(1.-cos2h,.0078125); // 2^(-14/2), so sin2h^2 > 0 in fp16
 return (2.+inv_alpha)*pow(sin2h,inv_alpha*.5)/(2.*3.14159265359);
}
fn V_Neubelt(nv:f32,nl:f32)->f32 {
 // Neubelt and Pettineo 2013, "Crafting a Next-gen Material Pipeline for The Order: 1886"
 // 0.00001532 = nextafter(1.0 / MEDIUMP_FLT_MAX, 1.0) in fp16, so we don't overflow
 return 1./max(4.*(nl+nv-nl*nv),.00001532);
}
// The sheen lobe of colour `sheen` and perceptual roughness `roughness` for
// a light toward `light` over normal `n` seen along `view`, before its
// cosine: Filament's sheenLobe.
fn sheen_lobe(sheen:vec3<f32>,roughness:f32,n:vec3<f32>,view:vec3<f32>,light:vec3<f32>)->vec3<f32> {
 let h=normalize(view+light);
 let nv=clamp(dot(n,view),0.,1.);
 let nl=clamp(dot(n,light),0.,1.);
 let nh=clamp(dot(n,h),0.,1.);
 return sheen*(D_Charlie(roughness*roughness,nh)*V_Neubelt(nv,nl));
}
// What a sheen of colour `sheen` and directional albedo `albedo` at the view
// leaves of the base beneath it: 1 − max3(sheen) E, at least 0. A sheen of
// colour 0 leaves all of it (KHR README 71).
fn sheen_scaling(sheen:vec3<f32>,albedo:f32)->f32 {
 return saturate(1.-max(sheen.r,max(sheen.g,sheen.b))*albedo);
}
