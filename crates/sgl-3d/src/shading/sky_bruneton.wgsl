// Copyright (c) 2017 Eric Bruneton
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// 1. Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in the
//    documentation and/or other materials provided with the distribution.
// 3. Neither the name of the copyright holders nor the names of its
//    contributors may be used to endorse or promote products derived from
//    this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF
// THE POSSIBILITY OF SUCH DAMAGE.
//
// Precomputed Atmospheric Scattering
// Copyright (c) 2008 INRIA
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// 1. Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in the
//    documentation and/or other materials provided with the distribution.
// 3. Neither the name of the copyright holders nor the names of its
//    contributors may be used to endorse or promote products derived from
//    this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF
// THE POSSIBILITY OF SUCH DAMAGE.
//
// Bruneton and Neyret's transmittance mapping and ray-sphere distances
// (ebruneton/precomputed_atmospheric_scattering, BSD-3-Clause, notice above
// and in src/LICENSE-bruneton.txt), as Bevy 9d12036 carries them in
// crates/bevy_pbr/src/atmosphere/bruneton_functions.wesl (MIT OR
// Apache-2.0, src/LICENSE-bevy.txt). Bevy's names and order. Departures:
// the planet's radii are shading/sky.wgsl's constants, not an `Atmosphere`
// uniform's fields, so no function takes one; and
// sample_transmittance_lut_segment, which only aerial perspective reads, is
// not ported. Part of the sky module (shading/sky.wgsl).

// Bruneton's paper calls the surface and exosphere "bottom radius" and "top
// radius": SKY_GROUND_RADIUS and SKY_TOP_RADIUS.

// Mapping from view height (r) and zenith cos angle (mu) to UV coordinates in
// the transmittance LUT, assuming r between ground and top atmosphere boundary,
// and mu = cos(zenith_angle). Chosen to increase precision near the ground and
// to work around a discontinuity at the horizon. See Bruneton and Neyret 2008,
// "Precomputed Atmospheric Scattering" section 4.
fn transmittance_lut_r_mu_to_uv(r:f32,mu:f32)->vec2<f32> {
 // Distance along a horizontal ray from the ground to the top atmosphere boundary.
 let H=sqrt(SKY_TOP_RADIUS*SKY_TOP_RADIUS-SKY_GROUND_RADIUS*SKY_GROUND_RADIUS);
 // Distance from a point at height r to the horizon; ignores r below the ground.
 let rho=sqrt(max(r*r-SKY_GROUND_RADIUS*SKY_GROUND_RADIUS,0.));
 // Distance from a point at height r to the top atmosphere boundary at zenith angle mu.
 let d=distance_to_top_atmosphere_boundary(r,mu);
 // Minimum and maximum distance to the top atmosphere boundary from a point at
 // height r: straight up, and grazing the horizon.
 let d_min=SKY_TOP_RADIUS-r;
 let d_max=rho+H;
 let u=(d-d_min)/(d_max-d_min);
 let v=rho/H;
 return vec2<f32>(u,v);
}

// Inverse of the mapping above, from UV coordinates in the transmittance LUT to
// view height (r) and zenith cos angle (mu).
fn transmittance_lut_uv_to_r_mu(uv:vec2<f32>)->vec2<f32> {
 // Distance to top atmosphere boundary for a horizontal ray at ground level.
 let H=sqrt(SKY_TOP_RADIUS*SKY_TOP_RADIUS-SKY_GROUND_RADIUS*SKY_GROUND_RADIUS);
 // Distance to the horizon, from which we can compute r.
 let rho=H*uv.y;
 let r=sqrt(rho*rho+SKY_GROUND_RADIUS*SKY_GROUND_RADIUS);
 // Distance to the top atmosphere boundary for the ray (r,mu), and its minimum
 // and maximum values over all mu, obtained for (r,1) and (r,mu_horizon), from
 // which we can recover mu.
 let d_min=SKY_TOP_RADIUS-r;
 let d_max=rho+H;
 let d=d_min+uv.x*(d_max-d_min);
 var mu:f32;
 if d==0. {
  mu=1.;
 } else {
  mu=(H*H-rho*rho-d*d)/(2.*r*d);
 }
 mu=clamp(mu,-1.,1.);
 return vec2<f32>(r,mu);
}

// Simplified ray-sphere intersection, with the ray from radius r <= the top
// radius at zenith cosine mu and the sphere of the top radius centred on the
// planet: the quadratic of line-sphere intersection under those assumptions.
fn distance_to_top_atmosphere_boundary(r:f32,mu:f32)->f32 {
 // Ignores r above the top radius.
 let positive_discriminant=max(r*r*(mu*mu-1.)+SKY_TOP_RADIUS*SKY_TOP_RADIUS,0.);
 return max(-r*mu+sqrt(positive_discriminant),0.);
}

// Simplified ray-sphere intersection as above, with the ground.
fn distance_to_bottom_atmosphere_boundary(r:f32,mu:f32)->f32 {
 let positive_discriminant=max(r*r*(mu*mu-1.)+SKY_GROUND_RADIUS*SKY_GROUND_RADIUS,0.);
 return max(-r*mu-sqrt(positive_discriminant),0.);
}

fn ray_intersects_ground(r:f32,mu:f32)->bool {
 return mu<0.&&r*r*(mu*mu-1.)+SKY_GROUND_RADIUS*SKY_GROUND_RADIUS>=0.;
}
