// Scene geometry's vertex stage: the authored vertex, the fragment it
// interpolates, and the drawn instance's transform of positions, normals and
// tangents by its object record. Reads `view` and `objects`.
struct Vertex {
 @location(0) position:vec3<f32>,
 @location(1) normal:vec3<f32>,
 @location(2) uv:vec2<f32>,
 @location(3) color:vec4<f32>,
 @location(4) lightmap_uv:vec2<f32>,
 @location(5) lightmap_bounds:vec4<f32>,
 @location(6) tangent:vec4<f32>,
}
struct Fragment {
 @invariant @builtin(position) clip:vec4<f32>,
 @location(0) world:vec3<f32>,
 @location(1) normal:vec3<f32>,
 @location(2) uv:vec2<f32>,
 @location(3) color:vec4<f32>,
 @location(4) current_clip:vec4<f32>,
 @location(5) previous_clip:vec4<f32>,
 @location(6) @interpolate(flat) source_id:vec2<u32>,
 @location(7) lightmap_uv:vec2<f32>,
 @location(8) @interpolate(flat) lightmap_bounds:vec4<f32>,
 @location(9) tangent:vec4<f32>,
}
// Keep projection separate from the view transform. Precomposing P*V lets
// translation cancellation corrupt tiny depth gaps before rasterization.
// Upchurch & Desbrun, Tightening the Precision of Perspective Rendering (2012).
fn scene_clip_position(world:vec4<f32>,view:mat4x4<f32>,projection:mat4x4<f32>)->vec4<f32> {
 let view_position=view*world;
 return projection*view_position;
}
// Cofactor(model) is inverse-transpose(model) times its determinant. Only
// normal direction survives interpolation; remove the determinant sign here.
fn object_normal(model:mat4x4<f32>,normal:vec3<f32>)->vec3<f32> {
 let x=model[0].xyz;
 let y=model[1].xyz;
 let z=model[2].xyz;
 let cofactor=mat3x3(cross(y,z),cross(z,x),cross(x,y));
 return (cofactor*normal)*select(1.,-1.,dot(x,cofactor[0])<0.);
}
fn object_tangent(model:mat4x4<f32>,t:vec4<f32>,normal:vec3<f32>)->vec4<f32> {
 let n=normalize(object_normal(model,normal));
 let transformed=(model*vec4(t.xyz,0.)).xyz;
 let projected=transformed-n*dot(n,transformed);
 let handed=t.w*select(1.,-1.,dot(model[0].xyz,cross(model[1].xyz,model[2].xyz))<0.);
 if dot(projected,projected)==0. {
  return vec4(0.);
 }
 return vec4(normalize(projected),handed);
}
// The index of the object record of the instance a fragment shows: its
// source identity is that index plus one. Its fields are read where they are
// used, so a stage loads only those.
fn fragment_object(i:Fragment)->u32 {
 return i.source_id.x-1u;
}
// Raster winding reverses under mirrored poses; material sides stay authored.
fn object_front_face(i:Fragment,front:bool)->bool {
 let model=objects[fragment_object(i)].model;
 let mirrored=dot(model[0].xyz,cross(model[1].xyz,model[2].xyz))<0.;
 return front!=mirrored;
}
// Where raster draws the world point `world`: its clip position, which
// the view's jitter offsets in xy by 2 * jitter * w. The transmitted light's
// exit point projects through it too (transmission.wgsl).
fn scene_raster_clip(world:vec4<f32>)->vec4<f32> {
 let clip=scene_clip_position(world,view.view,view.projection);
 return vec4(clip.xy+2.*view.jitter*clip.w,clip.zw);
}
// Motion is measured from `previous_position` at the instance's previous
// pose, both from the object record at index `object`.
fn vertex(object:u32,v:Vertex,previous_position:vec3<f32>)->Fragment {
 var o:Fragment;
 let model=objects[object].model;
 let p=model*vec4(v.position,1);
 o.clip=scene_raster_clip(p);
 o.world=p.xyz;
 o.normal=object_normal(model,v.normal);
 o.tangent=object_tangent(model,v.tangent,v.normal);
 o.uv=v.uv;
 o.color=v.color;
 o.lightmap_uv=v.lightmap_uv;
 o.lightmap_bounds=v.lightmap_bounds;
 o.current_clip=view.stable_view_projection*p;
 o.previous_clip=view.previous_view_projection*objects[object].previous_model*vec4(previous_position,1);
 return o;
}
