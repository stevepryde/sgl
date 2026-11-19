// The transmittance table: Bevy 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/transmittance_lut.wesl (MIT OR Apache-2.0,
// src/LICENSE-bevy.txt), whose integral (ray_optical_depth) is
// shading/sky.wgsl's: each texel is the air's transmittance from the radius
// and zenith cosine Bruneton's mapping gives it to the top of the atmosphere.
@group(0) @binding(4) var transmittance_lut_out:texture_storage_2d<rgba16float,write>;

@compute @workgroup_size(16,16,1)
fn transmittance_lut_main(@builtin(global_invocation_id) idx:vec3<u32>) {
 let size=vec2(SKY_TRANSMITTANCE_WIDTH,SKY_TRANSMITTANCE_HEIGHT);
 if any(idx.xy>=size) {
  return;
 }
 let uv=(vec2<f32>(idx.xy)+0.5)/vec2<f32>(size);
 // Map UV coordinates to view height (r) and zenith cos angle (mu).
 let r_mu=transmittance_lut_uv_to_r_mu(uv);
 // The optical depth from view height r to the top atmosphere boundary.
 let optical_depth=ray_optical_depth(sky.atmosphere,r_mu.x,r_mu.y);
 let transmittance=exp(-optical_depth);
 textureStore(transmittance_lut_out,idx.xy,vec4(transmittance,1.));
}
