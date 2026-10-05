// The irradiance volume's light at a receiver, which the one determination
// of indirect diffuse light takes (surface.wgsl). Reads `frame`,
// `irradiance_volume` and `baked_sampler`. The texture's layout is
// scene::irradiance_volume's: Bevy's (Rx, 2Ry, 3Rz), the X, Y and Z faces
// in turn along z and each slab's negative face at Ry + y.
//
// Ports Bevy 9d120361303727a66b62f31f0d053793af62417a's
// irradiance_volume_light (crates/bevy_pbr/src/light_probe/
// irradiance_volume.wesl 51-73: the position clamped to the edge cells'
// centres, three hardware trilinear taps of the faces the normal points
// to, blended by its squared components as Valve's ambient cube), MIT OR
// Apache-2.0 (src/LICENSE-bevy.txt). Changed: one volume, an axis-aligned
// lattice in the render frame, in place of Bevy's transformed cubes iterated
// per fragment; before the clamp, the position moves half a cell along the
// geometry normal, so a voxel face reads the cell before it, never the
// solid one behind it (Godot ed1daf0's SDFGI normal_bias is the precedent,
// servers/rendering/renderer_rd/shaders/environment/gi.glsl 195); a texel's
// alpha is the sky's occlusion toward its face, blended as its rgb is; the
// volume's share fades over the one cell past its extent (volume_share.wgsl)
// in place of Bevy's falloff; the sampler is `baked_sampler`, which the
// clamp keeps within each face's slab.
//
// The volume's light at a receiver: its own light rgb(n), irradiance / PI;
// the share of the frame's ambient that reaches it, a(n); and the volume's
// share of the receiver's indirect diffuse light, 0 where the volume does
// not light the frame or reach the receiver.
struct IrradianceVolumeLight {
 light:vec3<f32>,
 sky_visibility:f32,
 share:f32,
}
fn irradiance_volume_light(position:vec3<f32>,geometry_normal:vec3<f32>,N:vec3<f32>)->IrradianceVolumeLight {
 var light=IrradianceVolumeLight(vec3(0.),1.,0.);
 if !baked_lighting_enabled || (frame.flags&FRAME_IRRADIANCE_VOLUME)==0u {
  return light;
 }
 let resolution=vec3<f32>(frame.irradiance_volume_cells);
 // The position in cells from the volume's least corner, half a cell along
 // the geometry normal.
 let cells=(position-frame.irradiance_volume_origin)/frame.irradiance_volume_cell_size+normalize(geometry_normal)*.5;
 light.share=volume_share(cells,resolution);
 if light.share<=0. {
  return light;
 }
 let atlas_resolution=resolution*vec3(1.,2.,3.);
 // Make sure to clamp to the edges to avoid texture bleed.
 let stp=clamp(cells,vec3(.5),resolution-vec3(.5));
 let uvw=stp/atlas_resolution;
 // The bottom half of each cube slice is the negative part, so choose it if
 // applicable on each slice.
 let neg_offset=select(vec3(0.),vec3(.5),N<vec3(0.));
 let uvw_x=uvw+vec3(0.,neg_offset.x,0.);
 let uvw_y=uvw+vec3(0.,neg_offset.y,1./3.);
 let uvw_z=uvw+vec3(0.,neg_offset.z,2./3.);
 let rgba_x=textureSampleLevel(irradiance_volume,baked_sampler,uvw_x,0.);
 let rgba_y=textureSampleLevel(irradiance_volume,baked_sampler,uvw_y,0.);
 let rgba_z=textureSampleLevel(irradiance_volume,baked_sampler,uvw_z,0.);
 // Use Valve's formula to sample.
 let blended=ambient_cube_blend(rgba_x,rgba_y,rgba_z,N);
 light.light=blended.rgb;
 light.sky_visibility=1.-blended.a;
 return light;
}
