// Planar reflections (stages/reflections/planar.rs; the architecture's
// Reflections): where the surface at a pixel is a planar reflector's, its
// traced lobe's reflected radiance is the reflector's picture, the scene as
// the view's mirrored camera rendered it this frame, with confidence 1, in
// the form every method returns (premultiplied radiance, confidence).
// Composition takes it by the one formula (specular_lobes.wgsl). Vertex
// entry fullscreen_vs.
struct PlanarParameters {
 // The viewer's raster clip space, with its device depth, to world.
 world_from_clip:mat4x4<f32>,
 // World to the picture's clip space: the reflected view's unclipped
 // projection times its view, whose x, y and w place a point in the picture.
 picture_from_world:mat4x4<f32>,
 // The picture's clip space, with its device depth, to world.
 world_from_picture:mat4x4<f32>,
 // The reflector's plane (unit normal, d), positive on the reflecting side.
 plane:vec4<f32>,
 // The viewer's eye.
 eye:vec3<f32>,
}
@group(0) @binding(0) var<uniform> planar:PlanarParameters;
// 1 where the surface at the opaque depth is the reflector's.
@group(0) @binding(1) var planar_coverage:texture_2d<f32>;
@group(0) @binding(2) var planar_depth:texture_depth_2d;
// The surface depth (the Surface contract): nearer than the opaque depth
// under a blended receiver, whose result the method's is.
@group(0) @binding(3) var planar_surface_depth:texture_depth_2d;
@group(0) @binding(4) var planar_normal:texture_2d<f32>;
@group(0) @binding(5) var planar_material:texture_2d<f32>;
@group(0) @binding(6) var planar_picture:texture_2d<f32>;
@group(0) @binding(7) var planar_picture_depth:texture_depth_2d;
@group(0) @binding(8) var planar_sampler:sampler;
// The screen-space method's result, which the frame's result holds wherever
// no reflector's surface is.
@group(0) @binding(9) var planar_method:texture_2d<f32>;

// Where `clip`, in the picture's clip space, lies in the picture, with its
// clip w, positive in front of the reflected camera.
fn planar_uv(clip:vec4<f32>)->vec3<f32> {
 return vec3(clip.xy/clip.w*vec2(.5,-.5)+vec2(.5),clip.w);
}
// The picture's radiance for a surface point `world` of the reflector whose
// traced lobe's normal is `normal`, seen from the viewer's eye. The picture
// holds, at the point's own texel, what the mirror direction reaches and,
// in its depth, how far that is: the texel sampled is where the surface's
// own reflected ray reaches that distance, so a surface in the plane with
// the plane's normal reads its own texel and a mapped normal bends the
// picture by the angle it turns the ray, less for what is near (D-77). A
// ray the normal turns into the plane is mirrored back out of it.
fn planar_radiance(world:vec3<f32>,normal:vec3<f32>)->vec3<f32> {
 let own=planar_uv(planar.picture_from_world*vec4(world,1.));
 var direction=reflect(normalize(world-planar.eye),normal);
 let away=dot(direction,planar.plane.xyz);
 if away<0. {
  direction-=2.*away*planar.plane.xyz;
 }
 let size=vec2<f32>(textureDimensions(planar_picture_depth));
 let texel=clamp(vec2<i32>(own.xy*size),vec2(0),vec2<i32>(size)-vec2(1));
 let depth=textureLoad(planar_picture_depth,texel,0);
 // Nothing drawn (the sky): the ray's direction alone places it.
 var aim=planar.picture_from_world*vec4(direction,0.);
 if depth>0. {
  let hit=planar.world_from_picture*vec4(own.x*2.-1.,1.-own.y*2.,depth,1.);
  aim=planar.picture_from_world*vec4(world+direction*distance(hit.xyz/hit.w,world),1.);
 }
 var uv=own.xy;
 if aim.w>0. {
  uv=planar_uv(aim).xy;
 }
 return max(textureSampleLevel(planar_picture,planar_sampler,uv,0.).rgb,vec3(0.));
}
@fragment fn planar_fs(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let p=vec2<i32>(position.xy);
 let z=textureLoad(planar_depth,p,0);
 if textureLoad(planar_coverage,p,0).r<.5 || gbuffer_under_receiver(textureLoad(planar_surface_depth,p,0),z) {
  discard;
 }
 let size=vec2<f32>(textureDimensions(planar_depth));
 let ndc=vec2(position.x/size.x*2.-1.,1.-position.y/size.y*2.);
 let world=planar.world_from_clip*vec4(ndc,z,1.);
 let material=gbuffer_material(textureLoad(planar_material,p,0));
 let normal=gbuffer_reflection_normal(textureLoad(planar_normal,p,0),material.coat);
 return vec4(planar_radiance(world.xyz/world.w,normal),1.);
}
// The screen-space method's result, copied where the frame's result starts
// from it.
@fragment fn planar_method_fs(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 return textureLoad(planar_method,vec2<i32>(position.xy),0);
}
