// Light transmitted through a surface from behind it
// (KHR_materials_transmission), across its volume (KHR_materials_volume)
// and spread by dispersion (KHR_materials_dispersion): three.js r185's
// getIBLVolumeRefraction and its helpers (src/nodes/functions/
// PhysicalLightingModel.js 29-185; commit 2431a09f, MIT,
// stages/post/smaa/LICENSE-three.txt), after the Khronos glTF Sample Viewer,
// translated to WGSL with upstream's names and order. The frame behind the
// surface comes from the program's transmission provider
// (transmission_frame_held, transmission_frame_size,
// transmission_frame_sample): the transparent stage's mipmapped copy of the
// composed frame on the Extended binding tier (transmission_extended.wgsl),
// none on Basic (transmission_basic.wgsl).
// Changed: the exit point projects through raster's own clip transform,
// jitter included (scene_raster_clip), and to a texture's UV with +Y down,
// as three.js's WebGPU path flips it; Beer-Lambert's coefficient comes
// precomputed and finite in the record (shading::material::
// attenuation_coefficient), and the record's IOR is 0 for an infinite one,
// so no infinity reaches the shader; dispersion clamps each channel's IOR to
// at least 1, as KHR_materials_dispersion advises (Khronos glTF acfcbe65,
// README 135), so a refracted ray never meets total internal reflection,
// whose refract is 0, a NaN once normalised, nor a channel's IOR of 0 the
// record's infinite one; and the result carries the share of the light behind the
// surface that its transmission passes (TransmittedLight), which the blend
// takes where no copy holds the frame.

// Whether a blended pipeline compiles transmission in: while the scene holds
// a transmissive material (view::pipelines::PipelineKey), so that blended
// surfaces pay nothing for it otherwise, as films_enabled spares lit
// fragments a film.
override transmission_enabled:bool=true;
// The reciprocal of the record's IOR `ior` (at least 1, or 0), which
// refraction takes: 0 for an infinite one (KHR_materials_ior's 0), whose
// rays leave along -n. The max keeps the branch select does not take finite.
fn transmission_eta(ior:f32)->f32 {
 return select(1./max(ior,1.),0.,ior==0.);
}
fn getVolumeTransmissionRay(n:vec3<f32>,v:vec3<f32>,thickness:f32,ior:f32,modelMatrix:mat4x4<f32>)->vec3<f32> {
 // Direction of refracted light.
 let refractionVector=refract(-v,normalize(n),transmission_eta(ior));
 // Compute rotation-independent scaling of the model matrix
 // (transmission_model_scale, vertex.wgsl).
 let modelScale=transmission_model_scale(modelMatrix);
 // The thickness is specified in local space.
 return normalize(refractionVector)*thickness*modelScale;
}
// Scale roughness with IOR so that an IOR of 1.0 results in no microfacet
// refraction and an IOR of 1.5 results in the default amount of microfacet
// refraction. An infinite IOR takes the default.
fn applyIorToRoughness(roughness:f32,ior:f32)->f32 {
 return roughness*select(clamp(ior*2.-2.,0.,1.),1.,ior==0.);
}
// The frame behind the surface at `fragCoord` (UV), blurred by the level
// its roughness takes.
fn getTransmissionSample(fragCoord:vec2<f32>,roughness:f32,ior:f32)->vec4<f32> {
 let lod=log2(transmission_frame_size().x)*applyIorToRoughness(roughness,ior);
 return transmission_frame_sample(fragCoord,lod);
}
// Where the refracted ray leaves the volume at `refractedRayExit`, on the
// frame behind it: its UV.
fn transmission_coordinates(refractedRayExit:vec3<f32>)->vec2<f32> {
 // Project refracted vector on the framebuffer, while mapping to
 // normalized device coordinates.
 let ndcPos=scene_raster_clip(vec4(refractedRayExit,1.));
 let refractionCoords=ndcPos.xy/ndcPos.w;
 return refractionCoords*vec2(.5,-.5)+.5;
}
// What getIBLVolumeRefraction gives: the light transmitted through the
// surface, (1 - F) times the transmittance times the frame's light behind
// it, none where no copy holds the frame; and the share of the light
// behind it that it passes, the mean over channels of (1 - F) times the
// transmittance, which the blend passes unrefracted where none does.
// three.js's opacity takes the mean of the transmittance alone
// (transmittanceFactor); with (1 - F), the share blended through carries
// what the refracted light does.
struct TransmittedLight {
 light:vec3<f32>,
 share:f32,
}
fn getIBLVolumeRefraction(n:vec3<f32>,v:vec3<f32>,roughness:f32,diffuseColor:vec3<f32>,specularColor:vec3<f32>,specularF90:f32,position:vec3<f32>,modelMatrix:mat4x4<f32>,ior:f32,thickness:f32,attenuation:vec3<f32>,dispersion:f32)->TransmittedLight {
 let held=transmission_frame_held();
 var transmittedLight=vec3(0.);
 var transmittance=vec3(0.);
 if dispersion>0. && ior>0. {
  let halfSpread=(ior-1.)*.025*dispersion;
  let iors=max(vec3(ior-halfSpread,ior,ior+halfSpread),vec3(1.));
  // A ray, sample and transmittance for each channel: a constant three.
  for (var i=0;i<3;i++) {
   let transmissionRay=getVolumeTransmissionRay(n,v,thickness,iors[i],modelMatrix);
   let refractedRayExit=position+transmissionRay;
   if held {
    // Sample framebuffer to get pixel the refracted ray hits.
    let transmissionSample=getTransmissionSample(transmission_coordinates(refractedRayExit),roughness,iors[i]);
    transmittedLight[i]=transmissionSample[i];
   }
   transmittance[i]=diffuseColor[i]*volumeAttenuation(length(transmissionRay),attenuation)[i];
  }
 } else {
  let transmissionRay=getVolumeTransmissionRay(n,v,thickness,ior,modelMatrix);
  let refractedRayExit=position+transmissionRay;
  if held {
   // Sample framebuffer to get pixel the refracted ray hits.
   transmittedLight=getTransmissionSample(transmission_coordinates(refractedRayExit),roughness,ior).rgb;
  }
  transmittance=diffuseColor*volumeAttenuation(length(transmissionRay),attenuation);
 }
 let attenuatedColor=transmittance*transmittedLight;
 // Get the specular component.
 let F=pbr_split_sum(specularColor,specularF90,surface_dfg(specular_nv(n,v),roughness));
 let passed=(vec3(1.)-F)*transmittance;
 return TransmittedLight((vec3(1.)-F)*attenuatedColor,(passed.r+passed.g+passed.b)/3.);
}
