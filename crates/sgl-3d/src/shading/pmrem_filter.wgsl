// Writing Three.js r185's PMREM atlas (CubeUV), whose reader is
// probe_sampling.wgsl's pmrem_bilinear: the direction through each texel of a
// level's faces, and the GGX filter from one level to the next. Ported from
// three.js 2431a09f46f34c560bc8e44b33be0e567723d5b9 src/extras/PMREMGenerator.js
// (MIT, src/stages/post/smaa/LICENSE-three.txt): _createPlanes' faces, each
// face's quad reaching one texel past its edges (the face border
// pmrem_bilinear reads), _getCommonVertexShader's getDirection, and
// _getGGXShader's Heitz 2018 VNDF importance sampling over a Hammersley set,
// weighted by N.L. Departures: a compute pass writes each texel where Three
// rasterises a quad (the quad's uv at the texel's centre, which Three's
// interpolation gives exactly, getDirection being linear in uv); the face is
// an integer; the radical inverse is WGSL's reverseBits, which Three's
// radicalInverse_VdC computes by hand; and the sample count is the named cap
// SKY_ENVIRONMENT_GGX_SAMPLES (AR-12), Three's GGX_SAMPLES.
//
// Directions here are the atlas's own (pmrem_direction's output): a world
// direction is pmrem_direction(atlas direction, 0).

// Three's GGX_SAMPLES: the filter's samples a texel, its loop's cap.
const SKY_ENVIRONMENT_GGX_SAMPLES:u32=256u;

// Three's getDirection: the atlas direction through face `face` at plane uv
// `uv` (RH coordinate system; PMREM face-indexing convention).
fn pmrem_get_direction(uv_in:vec2<f32>,face:u32)->vec3<f32> {
 let uv=2.*uv_in-1.;
 var direction=vec3(uv,1.);
 if face==0u {
  direction=direction.zyx; // ( 1, v, u ) pos x
 } else if face==1u {
  direction=direction.xzy;
  direction=vec3(-direction.x,direction.y,-direction.z); // ( -u, 1, -v ) pos y
 } else if face==2u {
  direction.x*=-1.; // ( -u, v, 1 ) pos z
 } else if face==3u {
  direction=direction.zyx;
  direction=vec3(-direction.x,direction.y,-direction.z); // ( -1, v, -u ) neg x
 } else if face==4u {
  direction=direction.xzy;
  direction=vec3(-direction.x,-direction.y,direction.z); // ( -u, -1, v ) neg y
 } else if face==5u {
  direction.z*=-1.; // ( u, v, -1 ) neg z
 }
 return direction;
}

// The atlas direction through texel `texel` of face `face` of a level whose
// faces are `size` texels a side: _createPlanes' quad spans the face's
// texels with uv from -1/(size-2) to 1+1/(size-2), so a texel's centre takes
// the uv pmrem_bilinear's lookup maps back to it.
fn pmrem_texel_direction(texel:vec2<u32>,face:u32,size:u32)->vec3<f32> {
 let border=1./f32(size-2u);
 let uv=-border+(vec2<f32>(texel)+.5)/f32(size)*(1.+2.*border);
 return normalize(pmrem_get_direction(uv,face));
}

// Van der Corput radical inverse (Three's radicalInverse_VdC).
fn pmrem_radical_inverse(bits:u32)->f32 {
 return f32(reverseBits(bits))*2.3283064365386963e-10;
}

// Hammersley sequence.
fn pmrem_hammersley(i:u32,n:u32)->vec2<f32> {
 return vec2(f32(i)/f32(n),pmrem_radical_inverse(i));
}

// GGX VNDF importance sampling (Eric Heitz 2018, "Sampling the GGX
// Distribution of Visible Normals", https://jcgt.org/published/0007/04/01/).
fn pmrem_importance_sample_ggx_vndf(xi:vec2<f32>,v:vec3<f32>,roughness:f32)->vec3<f32> {
 let alpha=roughness*roughness;
 // Section 4.1: orthonormal basis.
 let t1_axis=vec3(1.,0.,0.);
 let t2_axis=cross(v,t1_axis);
 // Section 4.2: parameterization of the projected area.
 let r=sqrt(xi.x);
 let phi=2.*3.14159265359*xi.y;
 let t1=r*cos(phi);
 var t2=r*sin(phi);
 let s=.5*(1.+v.z);
 t2=(1.-s)*sqrt(1.-t1*t1)+s*t2;
 // Section 4.3: reprojection onto the hemisphere.
 let nh=t1*t1_axis+t2*t2_axis+sqrt(max(0.,1.-t1*t1-t2*t2))*v;
 // Section 3.4: transform back to the ellipsoid configuration.
 return normalize(vec3(alpha*nh.x,alpha*nh.y,max(0.,nh.z)));
}

// _getGGXShader's main: the radiance level `mip_in` of `map` holds about
// atlas direction `direction`, filtered by GGX at `roughness` (the
// incremental roughness _applyGGXFilter passes), with the view along the
// normal.
fn pmrem_ggx_filter(map:texture_2d_array<f32>,filtering:sampler,direction:vec3<f32>,roughness:f32,mip_in:f32)->vec3<f32> {
 let n=normalize(direction);
 // Assume the view direction equals the normal for prefiltering.
 let v=n;
 // For very low roughness, just sample the environment directly.
 if roughness<0.001 {
  return pmrem_bilinear(map,filtering,n,mip_in);
 }
 // Tangent space basis for VNDF sampling.
 let up=select(vec3(1.,0.,0.),vec3(0.,0.,1.),abs(n.z)<0.999);
 let tangent=normalize(cross(up,n));
 let bitangent=cross(n,tangent);
 var prefiltered=vec3(0.);
 var total_weight=0.;
 for (var i=0u;i<SKY_ENVIRONMENT_GGX_SAMPLES;i++) {
  let xi=pmrem_hammersley(i,SKY_ENVIRONMENT_GGX_SAMPLES);
  // For a PMREM, V = N, so in tangent space V is always (0, 0, 1).
  let h_tangent=pmrem_importance_sample_ggx_vndf(xi,vec3(0.,0.,1.),roughness);
  // Transform H back to the atlas's space.
  let h=normalize(tangent*h_tangent.x+bitangent*h_tangent.y+n*h_tangent.z);
  let l=normalize(2.*dot(v,h)*h-v);
  let n_dot_l=max(dot(n,l),0.);
  if n_dot_l>0. {
   // VNDF importance sampling handles the distribution's filtering; the
   // weight is N.L, for the split-sum approximation.
   prefiltered+=pmrem_bilinear(map,filtering,l,mip_in)*n_dot_l;
   total_weight+=n_dot_l;
  }
 }
 if total_weight>0. {
  prefiltered/=total_weight;
 }
 return prefiltered;
}
