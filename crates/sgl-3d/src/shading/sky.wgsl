// The procedural sky's model (FrameEnvironment::Sky): Hillaire 2020, "A
// Scalable and Production Ready Sky and Atmosphere Rendering Technique"
// (EGSR), as Bevy 9d120361303727a66b62f31f0d053793af62417a ports it in
// crates/bevy_pbr/src/atmosphere/functions.wesl and transmittance_lut.wesl
// (MIT OR Apache-2.0, src/LICENSE-bevy.txt), with fast_acos, fast_acos_4,
// fast_atan2 and ray_sphere_intersect from crates/bevy_render/src/maths.wesl,
// and Bruneton's transmittance mapping (sky_bruneton.wgsl). Bevy's function
// names and order. One owner each (AR-2): the medium, the tables' sizes and
// mappings, and the transmittance integral, whose Rust twin is
// shading/sky.rs. The stage that computes the tables is stages/sky; the
// opaque stage's backdrop draw reads the sky-view table. Metres; the up axis
// is +Y and the planet's centre lies straight below the camera.
//
// Departures from Bevy, each for the reason given:
// 1. The medium is analytic, and its numbers are constants here, not an
//    asset's: Hillaire's Earth as Wicked Engine
//    4323a33c94d021d45404adaf863e9b01673ab365 holds it
//    (WickedEngine/shaders/ShaderInterop_Weather.h 62-94, MIT,
//    src/LICENSE-wicked.txt), in metres, which Bevy samples from two medium
//    textures built from its `ScatteringMedium`. Bevy 9d12036's
//    `ScatteringMedium::earth` is not followed: it swaps the Mie pair and
//    its falloffs of 8/60 and 1.2/60 of a 100 km shell are 13.3 km and 2 km.
//    `SkyAtmosphere` holds the game's scales of the three densities
//    (Sky::rayleigh, mie, ozone) and the ground's albedo; the planet's radii
//    are constants, so `Atmosphere` and its transform go.
// 2. The transmittance integral (ray_optical_depth) samples uniform steps at
//    their midpoints. Bevy weights sample i by t_i - t_(i-1) with samples
//    at 0.3 of each step, a right-endpoint sum, which at 40 samples
//    overstates Earth's transmittance by 4 % in blue at the zenith and 21 %
//    at 10° elevation; the midpoint form is within 0.2 % and 0.8 % (both
//    against a 400,000-step integral). The other marches keep Bevy's form.
// 3. Sample counts are compile-time caps (AR-12), not uniform fields:
//    SKY_TRANSMITTANCE_SAMPLES and the stage's.
// 4. A table sampler is passed to each table read with its table, as
//    callers bind them in different groups.

// Bevy's maths constants, named apart from the programs this module joins.
// π
const SKY_PI:f32=3.141592653589793;
// 2π
const SKY_PI_2:f32=6.283185307179586;
// π/2
const SKY_HALF_PI:f32=1.57079632679;
// 1 / (2π)
const SKY_FRAC_2_PI:f32=0.15915494309;
// 3 / (16π)
const SKY_FRAC_3_16_PI:f32=0.0596831036594607509;
// 1 / (4π)
const SKY_FRAC_4_PI:f32=0.07957747154594767;
const SKY_MIN_EXTINCTION:vec3<f32>=vec3(1e-12);
// Where in each segment the stage's marches sample it (0 its start, 0.5 its
// middle, 1 its end): Bevy's MIDPOINT_RATIO, nearer the start for the
// density's exponential falloff. The transmittance integral samples
// midpoints (departure 2).
const SKY_MIDPOINT_RATIO:f32=0.3;

// The planet (Wicked Engine's Earth, in metres): the radius of its surface and
// of the top of its atmosphere. Rust twins: shading::sky.
const SKY_GROUND_RADIUS:f32=6360000.;
const SKY_TOP_RADIUS:f32=6460000.;
// Rayleigh scattering per metre at the ground, which does not absorb; its
// density falls off as exp(-h / SKY_RAYLEIGH_SCALE_HEIGHT).
const SKY_RAYLEIGH_SCATTERING:vec3<f32>=vec3(5.802e-6,13.558e-6,33.1e-6);
const SKY_RAYLEIGH_SCALE_HEIGHT:f32=8000.;
// Mie scattering and extinction per metre at the ground, grey; its density
// falls off as exp(-h / SKY_MIE_SCALE_HEIGHT); its phase is Henyey-Greenstein
// of asymmetry SKY_MIE_ASYMMETRY (Bevy's PhaseFunction::Mie).
const SKY_MIE_SCATTERING:f32=3.996e-6;
const SKY_MIE_EXTINCTION:f32=4.440e-6;
const SKY_MIE_SCALE_HEIGHT:f32=1200.;
const SKY_MIE_ASYMMETRY:f32=0.8;
// Ozone absorption per metre where it is densest, which does not scatter; its
// density is a tent, 1 at SKY_OZONE_PEAK and 0 SKY_OZONE_HALF_WIDTH below and
// above it (10 and 40 km).
const SKY_OZONE_ABSORPTION:vec3<f32>=vec3(0.650e-6,1.881e-6,0.085e-6);
const SKY_OZONE_PEAK:f32=25000.;
const SKY_OZONE_HALF_WIDTH:f32=15000.;

// The tables (Bevy's AtmosphereSettings defaults), RGBA16F. Rust twins:
// stages::sky.
const SKY_TRANSMITTANCE_WIDTH:u32=256u;
const SKY_TRANSMITTANCE_HEIGHT:u32=128u;
const SKY_MULTISCATTERING_SIZE:u32=32u;
const SKY_VIEW_WIDTH:u32=400u;
const SKY_VIEW_HEIGHT:u32=200u;
// The transmittance integral's steps (Bevy's transmittance_lut_samples): the
// loop's cap (AR-12). Rust twin: shading::sky.
const SKY_TRANSMITTANCE_SAMPLES:u32=40u;

// The aerial volume over the camera's frustum (stages/sky/aerial_view_lut.wgsl):
// its columns across and down (Bevy's aerial_view_lut_size.xy), its depth
// slices (Bevy's aerial_view_lut_size.z) and how far along the view ray the
// last ends, metres (Bevy's aerial_view_lut_max_distance). Rust twins:
// shading::sky.
const AERIAL_SIDE:u32=32u;
const AERIAL_SLICES:u32=32u;
const AERIAL_REACH:f32=32000.;

// The game's sky as the stage takes it (Sky::resolved): the planet ground's
// linear albedo and the three media's densities as scales of Earth's.
struct SkyAtmosphere {
 ground_albedo:vec3<f32>,
 rayleigh:f32,
 mie:f32,
 ozone:f32,
}

// MATHS (Bevy's maths.wesl)

// Returns the distances along the ray to its intersections with a sphere
// centred at the origin: r is the distance from the sphere's centre to the
// ray's origin, mu the cosine of the ray's zenith angle. vec2(-1) for none.
fn ray_sphere_intersect(r:f32,mu:f32,sphere_radius:f32)->vec2<f32> {
 let discriminant=r*r*(mu*mu-1.)+sphere_radius*sphere_radius;
 // No intersection
 if discriminant<0. {
  return vec2(-1.);
 }
 let q=-r*mu;
 let sqrt_discriminant=sqrt(discriminant);
 // Return both intersection distances
 return vec2(q-sqrt_discriminant,q+sqrt_discriminant);
}

// Slightly less accurate than fast_acos_4, but much simpler.
fn fast_acos(in_x:f32)->f32 {
 let x=abs(in_x);
 var res=-0.156583*x+SKY_HALF_PI;
 res*=sqrt(1.-x);
 return select(SKY_PI-res,res,in_x>=0.);
}

// 4th order polynomial approximation, 7e-5 radians precision (Handbook of
// Mathematical Functions, M. Abramowitz and I.A. Stegun).
fn fast_acos_4(x:f32)->f32 {
 let x1=abs(x);
 let x2=x1*x1;
 let x3=x2*x1;
 var s:f32;
 s=-0.2121144*x1+1.5707288;
 s=0.0742610*x2+s;
 s=-0.0187293*x3+s;
 s=sqrt(1.-x1)*s;
 // acos function mirroring
 return select(SKY_PI-s,s,x>=0.);
}

fn fast_atan2(y:f32,x:f32)->f32 {
 var t0=max(abs(x),abs(y));
 var t1=min(abs(x),abs(y));
 var t3=t1/t0;
 var t4=t3*t3;
 t0=0.0872929;
 t0=t0*t4-0.301895;
 t0=t0*t4+1.;
 t3=t0*t3;
 t3=select(t3,(0.5*SKY_PI)-t3,abs(y)>abs(x));
 t3=select(t3,SKY_PI-t3,x<0.);
 t3=select(-t3,t3,y>0.);
 return t3;
}

// LUT UV PARAMETERIZATIONS

fn unit_to_sub_uvs(val:vec2<f32>,resolution:vec2<f32>)->vec2<f32> {
 return (val+0.5/resolution)*(resolution/(resolution+1.));
}

fn sub_uvs_to_unit(val:vec2<f32>,resolution:vec2<f32>)->vec2<f32> {
 return (val-0.5/resolution)*(resolution/(resolution-1.));
}

fn multiscattering_lut_r_mu_to_uv(r:f32,mu:f32)->vec2<f32> {
 let u=0.5+0.5*mu;
 let v=saturate((r-SKY_GROUND_RADIUS)/(SKY_TOP_RADIUS-SKY_GROUND_RADIUS));
 return unit_to_sub_uvs(vec2(u,v),vec2(f32(SKY_MULTISCATTERING_SIZE)));
}

fn multiscattering_lut_uv_to_r_mu(uv:vec2<f32>)->vec2<f32> {
 let adj_uv=sub_uvs_to_unit(uv,vec2(f32(SKY_MULTISCATTERING_SIZE)));
 let r=mix(SKY_GROUND_RADIUS,SKY_TOP_RADIUS,adj_uv.y);
 let mu=adj_uv.x*2.-1.;
 return vec2(r,mu);
}

fn sky_view_lut_r_mu_azimuth_to_uv(r:f32,mu:f32,azimuth:f32)->vec2<f32> {
 let u=(azimuth*SKY_FRAC_2_PI)+0.5;
 let v_horizon=sqrt(r*r-SKY_GROUND_RADIUS*SKY_GROUND_RADIUS);
 let cos_beta=v_horizon/r;
 // fast_acos_4 for better precision at small angles, against artifacts at
 // the horizon.
 let beta=fast_acos_4(cos_beta);
 let horizon_zenith=SKY_PI-beta;
 let view_zenith=fast_acos_4(mu);
 // A non-linear transformation compresses more texels near the horizon,
 // where high-frequency details matter most: l is latitude in [-π/2, π/2]
 // and v is texture coordinate in [0,1].
 let l=view_zenith-horizon_zenith;
 let abs_l=abs(l);
 let v=0.5+0.5*sign(l)*sqrt(abs_l/SKY_HALF_PI);
 return unit_to_sub_uvs(vec2(u,v),vec2(f32(SKY_VIEW_WIDTH),f32(SKY_VIEW_HEIGHT)));
}

fn sky_view_lut_uv_to_zenith_azimuth(r:f32,uv:vec2<f32>)->vec2<f32> {
 let adj_uv=sub_uvs_to_unit(vec2(uv.x,1.-uv.y),vec2(f32(SKY_VIEW_WIDTH),f32(SKY_VIEW_HEIGHT)));
 let azimuth=(adj_uv.x-0.5)*SKY_PI_2;
 // Horizon parameters
 let v_horizon=sqrt(r*r-SKY_GROUND_RADIUS*SKY_GROUND_RADIUS);
 let cos_beta=v_horizon/r;
 let beta=fast_acos_4(cos_beta);
 let horizon_zenith=SKY_PI-beta;
 // Inverse of the horizon-detail mapping, recovering the latitude from the
 // texture coordinate.
 let t=abs(2.*(adj_uv.y-0.5));
 let l=sign(adj_uv.y-0.5)*SKY_HALF_PI*t*t;
 return vec2(horizon_zenith-l,azimuth);
}

// LUT SAMPLING

fn sample_transmittance_lut(lut:texture_2d<f32>,lut_sampler:sampler,r:f32,mu:f32)->vec3<f32> {
 let uv=transmittance_lut_r_mu_to_uv(r,mu);
 return textureSampleLevel(lut,lut_sampler,uv,0.).rgb;
}

fn sample_multiscattering_lut(lut:texture_2d<f32>,lut_sampler:sampler,r:f32,mu:f32)->vec3<f32> {
 let uv=multiscattering_lut_r_mu_to_uv(r,mu);
 return textureSampleLevel(lut,lut_sampler,uv,0.).rgb;
}

// The sky-view table's in-scattered radiance toward ray_dir (world axes) from
// radius r: its azimuth is about +Y from -Z toward +X, as Bevy's atmosphere
// space measures it, in world axes.
fn sample_sky_view_lut(lut:texture_2d<f32>,lut_sampler:sampler,r:f32,ray_dir:vec3<f32>)->vec3<f32> {
 let mu=ray_dir.y;
 let azimuth=fast_atan2(ray_dir.x,-ray_dir.z);
 let uv=sky_view_lut_r_mu_azimuth_to_uv(r,mu,azimuth);
 return textureSampleLevel(lut,lut_sampler,uv,0.).rgb;
}

// ATMOSPHERE SAMPLING (analytic, departure 1)

// Bevy's components of sample_density_lut: the absorption density and the
// scattering density.
const SKY_ABSORPTION_DENSITY:f32=0.;
const SKY_SCATTERING_DENSITY:f32=1.;

// The three media's densities at radius r, each relative to its own peak and
// scaled by the game's: Rayleigh's, Mie's and ozone's. Heights outside the
// atmosphere take its nearest boundary's, as Bevy's density texture clamps.
fn sky_densities(atm:SkyAtmosphere,r:f32)->vec3<f32> {
 let h=clamp(r-SKY_GROUND_RADIUS,0.,SKY_TOP_RADIUS-SKY_GROUND_RADIUS);
 let rayleigh=exp(-h/SKY_RAYLEIGH_SCALE_HEIGHT);
 let mie=exp(-h/SKY_MIE_SCALE_HEIGHT);
 let ozone=max(1.-abs(h-SKY_OZONE_PEAK)/SKY_OZONE_HALF_WIDTH,0.);
 return vec3(rayleigh*atm.rayleigh,mie*atm.mie,ozone*atm.ozone);
}

// The medium's absorption (component SKY_ABSORPTION_DENSITY) or scattering
// (SKY_SCATTERING_DENSITY) per metre at radius r: Bevy's sample_density_lut.
fn sample_density(atm:SkyAtmosphere,r:f32,component:f32)->vec3<f32> {
 let density=sky_densities(atm,r);
 let absorption=vec3((SKY_MIE_EXTINCTION-SKY_MIE_SCATTERING)*density.y)+SKY_OZONE_ABSORPTION*density.z;
 let scattering=SKY_RAYLEIGH_SCATTERING*density.x+vec3(SKY_MIE_SCATTERING*density.y);
 return select(absorption,scattering,component==SKY_SCATTERING_DENSITY);
}

// The medium's scattering per metre at radius r times each medium's phase
// function, for a light whose direction's dot product with the view ray (which
// points away from the viewer) is neg_LdotV: Bevy's sample_scattering_lut,
// with its PhaseFunction::sample's Rayleigh and Mie (Henyey-Greenstein).
fn sample_scattering(atm:SkyAtmosphere,r:f32,neg_LdotV:f32)->vec3<f32> {
 let density=sky_densities(atm,r);
 let rayleigh_phase=SKY_FRAC_3_16_PI*(1.+neg_LdotV*neg_LdotV);
 let g=SKY_MIE_ASYMMETRY;
 let denom=1.+g*g-2.*g*neg_LdotV;
 let mie_phase=SKY_FRAC_4_PI*(1.-g*g)/(denom*sqrt(denom));
 return SKY_RAYLEIGH_SCATTERING*density.x*rayleigh_phase+vec3(SKY_MIE_SCATTERING*density.y*mie_phase);
}

// TRANSFORM UTILITIES

fn max_atmosphere_distance(r:f32,mu:f32)->f32 {
 let t_top=distance_to_top_atmosphere_boundary(r,mu);
 let t_bottom=distance_to_bottom_atmosphere_boundary(r,mu);
 let hits=ray_intersects_ground(r,mu);
 return mix(t_top,t_bottom,f32(hits));
}

// The up vector at distance t along ray_dir from a point at radius r straight
// above the planet's centre (+Y: the world is locally flat).
fn get_local_up(r:f32,t:f32,ray_dir:vec3<f32>)->vec3<f32> {
 return normalize(vec3(0.,r,0.)+t*ray_dir);
}

// Given a ray starting at radius r with mu = cos(zenith angle), and a distance
// t along it, the radius at t.
fn get_local_r(r:f32,mu:f32,t:f32)->f32 {
 return sqrt(t*t+2.*r*mu*t+r*r);
}

fn zenith_azimuth_to_ray_dir(zenith:f32,azimuth:f32)->vec3<f32> {
 let sin_zenith=sin(zenith);
 let mu=cos(zenith);
 let sin_azimuth=sin(azimuth);
 let cos_azimuth=cos(azimuth);
 return vec3(sin_azimuth*sin_zenith,mu,-cos_azimuth*sin_zenith);
}

struct RaymarchSegment {
 start:f32,
 end:f32,
}

fn get_raymarch_segment(r:f32,mu:f32)->RaymarchSegment {
 // Both intersection points with the atmosphere and the ground.
 let atmosphere_intersections=ray_sphere_intersect(r,mu,SKY_TOP_RADIUS);
 let ground_intersections=ray_sphere_intersect(r,mu,SKY_GROUND_RADIUS);
 var segment:RaymarchSegment;
 if r<SKY_GROUND_RADIUS {
  // Inside the planet: from the bottom of the atmosphere.
  segment.start=ground_intersections.y;
  segment.end=atmosphere_intersections.y;
 } else if r<SKY_TOP_RADIUS {
  // Inside the atmosphere.
  segment.start=0.;
  segment.end=select(atmosphere_intersections.y,ground_intersections.x,ray_intersects_ground(r,mu));
 } else {
  // Outside the atmosphere.
  if atmosphere_intersections.x<0. {
   // No intersection with the atmosphere.
   return segment;
  }
  // From the atmosphere's entry to its exit or the ground.
  segment.start=atmosphere_intersections.x;
  segment.end=select(atmosphere_intersections.y,ground_intersections.x,ray_intersects_ground(r,mu));
 }
 return segment;
}

// THE TRANSMITTANCE INTEGRAL (transmittance_lut.wesl, departure 2)

// The optical depth of the medium from radius r along zenith cosine mu to the
// top atmosphere boundary (or the ground): SKY_TRANSMITTANCE_SAMPLES uniform
// steps, each sampled at its midpoint. Rust twin: shading::sky::optical_depth.
fn ray_optical_depth(atm:SkyAtmosphere,r:f32,mu:f32)->vec3<f32> {
 let t_max=max_atmosphere_distance(r,mu);
 let dt=t_max/f32(SKY_TRANSMITTANCE_SAMPLES);
 var optical_depth=vec3<f32>(0.);
 for (var i=0u;i<SKY_TRANSMITTANCE_SAMPLES;i++) {
  let t_i=dt*(f32(i)+0.5);
  let r_i=get_local_r(r,mu,t_i);
  let absorption=sample_density(atm,r_i,SKY_ABSORPTION_DENSITY);
  let scattering=sample_density(atm,r_i,SKY_SCATTERING_DENSITY);
  let extinction=absorption+scattering;
  optical_depth+=extinction*dt;
 }
 return optical_depth;
}

// THE AERIAL VOLUME'S MAPPING (the writer, stages/sky/aerial_view_lut.wgsl,
// and the reader, the fog's integration, both take it)

// How far along the view ray slice `slice` of the aerial volume ends, metres:
// AERIAL_REACH × ((slice + 1) / AERIAL_SLICES)², slices spaced by the square
// as Wicked Engine 4323a33 spaces its camera volume
// (WickedEngine/shaders/skyAtmosphere.hlsli GetAerialPerspectiveTransmittance,
// w = sqrt(slice / AP_SLICE_COUNT), MIT, src/LICENSE-wicked.txt). Slice 0
// ends 31.25 m out; Bevy's linear slices end every kilometre.
fn aerial_slice_distance(slice:f32)->f32 {
 let end=(slice+1.)/f32(AERIAL_SLICES);
 return AERIAL_REACH*end*end;
}
// The volume's depth coordinate of `distance` metres along the view ray: each
// texel holds what lies between the camera and its slice's end, so a slice's
// end falls at its texel's centre (Bevy's half-slice offset in
// sample_aerial_view_lut). Beyond the reach it is past the last texel's centre,
// which a clamped sampler holds.
fn aerial_coordinate(distance:f32)->f32 {
 return sqrt(max(distance,0.)/AERIAL_REACH)-.5/f32(AERIAL_SLICES);
}
// What lies between the camera and `distance` metres along the ray through
// `uv` on the frame: the in-scattered radiance (rgb) and mean transmittance
// (a) the aerial volume `volume` holds, faded linearly to nothing over the
// first slice, as Bevy and Wicked Engine fade theirs (their first slice holds
// its whole length).
fn sample_aerial_volume(volume:texture_3d<f32>,volume_sampler:sampler,uv:vec2<f32>,distance:f32)->vec4<f32> {
 let air=textureSampleLevel(volume,volume_sampler,vec3(uv,aerial_coordinate(distance)),0.);
 let fade=saturate(distance/aerial_slice_distance(0.));
 return vec4(air.rgb*fade,mix(1.,air.a,fade));
}
