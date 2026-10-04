// The scene source's bindings and its readers, for ray queries and pulled
// vertices; its layout is scene_source.wgsl's.
// One capture-visible instance: its pose, its model's mesh records and BVH,
// and its object record's index (`id`), flags and ambient cube.
struct SceneRayInstanceData {
 world:mat4x4<f32>, normal_matrix:mat4x4<f32>, mesh_word:u32, id:u32, flags:u32, bvh_root:u32, baked_irradiance:array<vec4<f32>,6>,
}
@group(1) @binding(1) var<storage,read> scene_source:array<u32>;
@group(1) @binding(2) var<storage,read> scene_instances:array<SceneRayInstanceData>;

struct SceneHit {
 hit:bool, distance:f32, position:vec3<f32>, normal:vec3<f32>,
 geometric_normal:vec3<f32>, front_face:bool, uv:vec2<f32>, color:vec4<f32>,
 instance_id:u32, instance_flags:u32, mesh_id:u32, primitive_id:u32,
 material_word:u32, barycentrics:vec2<f32>,
 tangent:vec3<f32>, bitangent:vec3<f32>, lightmap_uv:vec2<f32>, lightmap_bounds:vec4<f32>, instance_slot:u32, authored_tangent:vec4<f32>,
}
struct SceneMaterial {
 values:Material,
 textures:array<u32,6>, wrap:vec2<u32>, baked:u32,
}
fn scene_f32(at:u32)->f32 {
 return bitcast<f32>(scene_source[at]);
}
fn scene_v2(at:u32)->vec2<f32> {
 return vec2(scene_f32(at),scene_f32(at+1u));
}
fn scene_v3(at:u32)->vec3<f32> {
 return vec3(scene_f32(at),scene_f32(at+1u),scene_f32(at+2u));
}
fn scene_v4(at:u32)->vec4<f32> {
 return vec4(scene_v3(at),scene_f32(at+3u));
}
fn scene_material_values(at:u32)->Material {
 return Material(scene_v4(at+SCENE_MATERIAL_BASE),scene_v3(at+SCENE_MATERIAL_EMISSION),scene_f32(at+SCENE_MATERIAL_ENVIRONMENT_SCALE),
 scene_f32(at+SCENE_MATERIAL_METALLIC),scene_f32(at+SCENE_MATERIAL_ROUGHNESS),scene_f32(at+SCENE_MATERIAL_COAT),scene_f32(at+SCENE_MATERIAL_COAT_ROUGHNESS),
 scene_f32(at+SCENE_MATERIAL_NORMAL_SCALE),scene_f32(at+SCENE_MATERIAL_BUMP_SCALE),scene_f32(at+SCENE_MATERIAL_ANISOTROPY_STRENGTH),scene_f32(at+SCENE_MATERIAL_ANISOTROPY_ROTATION),
 scene_f32(at+SCENE_MATERIAL_ALPHA_CUTOFF),scene_source[at+SCENE_MATERIAL_VISIBILITY_GROUP],scene_source[at+SCENE_MATERIAL_FLAGS]);
}
fn scene_material(at:u32)->SceneMaterial {
 let textures=at+SCENE_MATERIAL_TEXTURES;
 let wrap=at+SCENE_MATERIAL_WRAP;
 return SceneMaterial(scene_material_values(at),
 array<u32,6>(scene_source[textures],scene_source[textures+1u],scene_source[textures+2u],scene_source[textures+3u],scene_source[textures+4u],scene_source[textures+5u]),
 vec2(scene_source[wrap],scene_source[wrap+1u]),scene_source[at+SCENE_MATERIAL_BAKED]);
}
fn scene_wrap_texel(p:i32,size:i32,mode:u32)->i32 {
 if mode==2u {
  return clamp(p,0,size-1);
 }
 if mode==1u {
  let q=((p%(size*2))+size*2)%(size*2);
  return min(q,size*2-1-q);
 }
 return ((p%size)+size)%size;
}
fn scene_srgb_to_linear(c:vec3<f32>)->vec3<f32> {
 return select(pow((c+vec3(0.055))/1.055,vec3(2.4)),c/12.92,c<=vec3(0.04045));
}
fn scene_image_size(image_word:u32)->vec2<u32> {
 return vec2(scene_source[image_word+SCENE_IMAGE_WIDTH],scene_source[image_word+SCENE_IMAGE_HEIGHT]);
}
// Texel `p` of an image's level 0, decoded from its block when the image is
// BC7.
fn scene_texel(image_word:u32,p:vec2<i32>,wrap:vec2<u32>,srgb:bool)->vec4<f32> {
 let size=vec2<i32>(scene_image_size(image_word));
 let x=scene_wrap_texel(p.x,size.x,wrap.x);
 let y=scene_wrap_texel(p.y,size.y,wrap.y);
 let texels=image_word+SCENE_IMAGE_TEXELS;
 var rgba:vec4<f32>;
 if scene_source[image_word+SCENE_IMAGE_FORMAT]==SCENE_IMAGE_BC7 {
  let block=texels+u32((y/4)*(size.x/4)+x/4)*4u;
  let words=vec4(scene_source[block],scene_source[block+1u],scene_source[block+2u],scene_source[block+3u]);
  rgba=bc7_texel(words,u32((y%4)*4+x%4));
 } else {
  rgba=unpack4x8unorm(scene_source[texels+u32(y*size.x+x)]);
 }
 if srgb {
  return vec4(scene_srgb_to_linear(rgba.rgb),rgba.a);
 }
 return rgba;
}
// LOD 0 is explicit: no fragment derivatives exist at a secondary ray hit.
// Decode color texels before filtering, matching sRGB sampled textures.
fn scene_sample_texture(image_word:u32,uv:vec2<f32>,wrap:vec2<u32>,srgb:bool)->vec4<f32> {
 if image_word==0u {
  return vec4(1.);
 }
 let size=vec2<f32>(scene_image_size(image_word));
 let p=uv*size-vec2(0.5);
 let i=vec2<i32>(floor(p));
 let f=fract(p);
 return mix(mix(scene_texel(image_word,i,wrap,srgb),scene_texel(image_word,i+vec2(1,0),wrap,srgb),f.x),
 mix(scene_texel(image_word,i+vec2(0,1),wrap,srgb),scene_texel(image_word,i+vec2(1,1),wrap,srgb),f.x),f.y);
}
// A ray hit's base colour at `uv` with vertex colour `color`: the material's
// base times the vertex colour and its base map at LOD 0.
fn scene_base_color(material:SceneMaterial,uv:vec2<f32>,color:vec4<f32>)->vec4<f32> {
 return material.values.base*color*scene_sample_texture(material.textures[SCENE_TEXTURE_BASE],uv,material.wrap,true);
}
fn scene_vertex_words(mesh:u32,primitive:u32)->vec3<u32> {
 let vertices=scene_source[mesh+SCENE_MESH_VERTICES];
 let indices=scene_source[mesh+SCENE_MESH_INDICES]+primitive*3u;
 return vec3(vertices+scene_source[indices]*SCENE_VERTEX_WORDS,vertices+scene_source[indices+1u]*SCENE_VERTEX_WORDS,vertices+scene_source[indices+2u]*SCENE_VERTEX_WORDS);
}
// The texture coordinates and colour of triangle `vertices` at barycentrics
// `b`, as a hit's shading and the masked any-hit test read them.
fn scene_interpolated_uv(vertices:vec3<u32>,b:vec3<f32>)->vec2<f32> {
 return scene_v2(vertices.x+SCENE_VERTEX_UV)*b.x+scene_v2(vertices.y+SCENE_VERTEX_UV)*b.y+scene_v2(vertices.z+SCENE_VERTEX_UV)*b.z;
}
fn scene_interpolated_color(vertices:vec3<u32>,b:vec3<f32>)->vec4<f32> {
 return scene_v4(vertices.x+SCENE_VERTEX_COLOR)*b.x+scene_v4(vertices.y+SCENE_VERTEX_COLOR)*b.y+scene_v4(vertices.z+SCENE_VERTEX_COLOR)*b.z;
}
fn scene_world_geometric(instance:SceneRayInstanceData,v:vec3<u32>)->vec3<f32> {
 let e1=scene_v3(v.y+SCENE_VERTEX_POSITION)-scene_v3(v.x+SCENE_VERTEX_POSITION);
 let e2=scene_v3(v.z+SCENE_VERTEX_POSITION)-scene_v3(v.x+SCENE_VERTEX_POSITION);
 // Preserve the authored front side under mirrored instances, as raster does.
 return normalize((instance.normal_matrix*vec4(cross(e1,e2),0.)).xyz);
}
struct SceneRay {
 origin:vec4<f32>,
 direction:vec4<f32>,
}
struct RawSceneHit {
 intersection:vec4<u32>,
 coords:vec4<f32>,
}
fn scene_decode_hit(raw:RawSceneHit,origin:vec3<f32>,direction:vec3<f32>)->SceneHit {
 var result:SceneHit;
 if raw.intersection.x==0u {
  return result;
 }
 let instance=scene_instances[raw.intersection.y];
 let mesh=instance.mesh_word+raw.intersection.z*SCENE_MESH_WORDS;
 let vertices=scene_vertex_words(mesh,raw.intersection.w);
 let b=vec3(1.-raw.coords.yz.x-raw.coords.yz.y,raw.coords.yz);
 let uv0=scene_v2(vertices.x+SCENE_VERTEX_UV);
 let uv1=scene_v2(vertices.y+SCENE_VERTEX_UV);
 let uv2=scene_v2(vertices.z+SCENE_VERTEX_UV);
 let n=scene_v3(vertices.x+SCENE_VERTEX_NORMAL)*b.x+scene_v3(vertices.y+SCENE_VERTEX_NORMAL)*b.y+scene_v3(vertices.z+SCENE_VERTEX_NORMAL)*b.z;
 result.hit=true;
 result.distance=raw.coords.x;
 // Reconstruct from the ACTUAL offset query origin, never the receiver position.
 result.position=origin+direction*raw.coords.x;
 result.geometric_normal=scene_world_geometric(instance,vertices);
 result.front_face=dot(result.geometric_normal,direction)<0.;
 result.normal=normalize((instance.normal_matrix*vec4(n,0.)).xyz);
 // Match raster: transform and orthogonalize each authored vertex frame before
 // interpolation, then reorthogonalize the interpolated tangent at shading.
 var tangent=vec4(0.);
 for(var k=0u;k<3u;k++) {
  let authored=scene_v4(vertices[k]+SCENE_VERTEX_TANGENT);
  let vertex_n=normalize((instance.normal_matrix*vec4(scene_v3(vertices[k]+SCENE_VERTEX_NORMAL),0.)).xyz);
  let transformed=(instance.world*vec4(authored.xyz,0.)).xyz;
  let projected=transformed-vertex_n*dot(vertex_n,transformed);
  if dot(projected,projected)>0. {
   tangent+=vec4(normalize(projected),authored.w)*b[k];
  }
 }
 tangent.w*=select(1.,-1.,dot(instance.world[0].xyz,cross(instance.world[1].xyz,instance.world[2].xyz))<0.);
 result.authored_tangent=tangent;
 if !result.front_face {
  result.normal=-result.normal;
 }
 result.uv=scene_interpolated_uv(vertices,b);
 result.color=scene_interpolated_color(vertices,b);
 result.lightmap_uv=scene_v2(vertices.x+SCENE_VERTEX_LIGHTMAP_UV)*b.x+scene_v2(vertices.y+SCENE_VERTEX_LIGHTMAP_UV)*b.y+scene_v2(vertices.z+SCENE_VERTEX_LIGHTMAP_UV)*b.z;
 result.lightmap_bounds=scene_v4(vertices.x+SCENE_VERTEX_LIGHTMAP_BOUNDS);
 result.instance_slot=raw.intersection.y;
 result.instance_id=instance.id;
 result.instance_flags=instance.flags;
 result.mesh_id=raw.intersection.z;
 result.primitive_id=raw.intersection.w;
 result.material_word=scene_source[mesh+SCENE_MESH_MATERIAL_WORD];
 result.barycentrics=raw.coords.yz;
 let dp1=(instance.world*vec4(scene_v3(vertices.y+SCENE_VERTEX_POSITION)-scene_v3(vertices.x+SCENE_VERTEX_POSITION),0.)).xyz;
 let dp2=(instance.world*vec4(scene_v3(vertices.z+SCENE_VERTEX_POSITION)-scene_v3(vertices.x+SCENE_VERTEX_POSITION),0.)).xyz;
 let duv1=uv1-uv0;
 let duv2=uv2-uv0;
 let determinant=duv1.x*duv2.y-duv1.y*duv2.x;
 if abs(determinant)>1e-12 {
  result.tangent=(dp1*duv2.y-dp2*duv1.y)/determinant;
  result.bitangent=(dp2*duv1.x-dp1*duv2.x)/determinant;
 }
 return result;
}

fn scene_texture_size(image_word:u32)->vec2<u32> {
 if image_word==0u {
  return vec2(1u);
 }
 return scene_image_size(image_word);
}
