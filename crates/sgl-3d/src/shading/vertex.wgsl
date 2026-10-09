// Scene geometry's vertex stage: the fragment a vertex interpolates, and the
// drawn instance's transform of positions, normals and tangents by its
// object record (shaded_vertex.wgsl places the vertex). Reads `view` and
// `objects`.
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
 // What the material's vertex function passes its surface function
 // (shader_contract.wgsl's MaterialVertex.custom).
 @location(10) custom:vec4<f32>,
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
// The rotation-independent scale of `model` on each axis, which takes a
// volume's thickness, given in the mesh's units (KHR_materials_volume), into
// the world's: the one owner of that scale, which every volume lobe takes,
// the refracted one along its ray (transmission.wgsl) and the diffusely
// transmitted one as one length (transmission_world_thickness).
fn transmission_model_scale(model:mat4x4<f32>)->vec3<f32> {
 return vec3(length(model[0].xyz),length(model[1].xyz),length(model[2].xyz));
}
// A volume's thickness `thickness`, in the mesh's units, as one world length
// for an instance posed by `model`: times the mean of its axis scales
// (transmission_model_scale), as the Khronos glTF Sample Renderer 0686eb2
// scales it for diffuse transmission (source/Renderer/shaders/pbr.frag
// 174–177).
fn transmission_world_thickness(thickness:f32,model:mat4x4<f32>)->f32 {
 let scale=transmission_model_scale(model);
 return thickness*(scale.x+scale.y+scale.z)/3.;
}
// The index of the object record of the instance a fragment shows: its
// source identity is that index plus one. Its fields are read where they are
// used, so a stage loads only those.
fn fragment_object(i:Fragment)->u32 {
 return i.source_id.x-1u;
}
// Raster winding reverses under mirrored poses; material sides stay
// authored: whether a face raster takes as `front` of the instance whose
// object record is at index `object` is its material's front.
fn object_front(object:u32,front:bool)->bool {
 let model=objects[object].model;
 let mirrored=dot(model[0].xyz,cross(model[1].xyz,model[2].xyz))<0.;
 return front!=mirrored;
}
fn object_front_face(i:Fragment,front:bool)->bool {
 return object_front(fragment_object(i),front);
}
// The interpolated vertex normal `normal`, unit, on the side shaded: the
// material's `front`, or its back.
fn side_normal(normal:vec3<f32>,front:bool)->vec3<f32> {
 return normalize(normal)*select(-1.0,1.0,front);
}
// Where raster draws the world point `world`: its clip position, which
// the view's jitter offsets in xy by 2 * jitter * w. The transmitted light's
// exit point projects through it too (transmission.wgsl).
fn scene_raster_clip(world:vec4<f32>)->vec4<f32> {
 let clip=scene_clip_position(world,view.view,view.projection);
 return vec4(clip.xy+2.*view.jitter*clip.w,clip.zw);
}
