// The scene source's bindings and its readers, for ray queries and pulled
// vertices; its layout is scene_source.wgsl's.
// An instance's entry, at its index, as its object record is
// (scene::rays::instances): what the record lacks for a ray, the inverse of
// its pose, which takes a world ray into its model's space, and its model's
// first mesh record and BVH root (zero when it has no triangles). A hit
// reads its pose, flags and ambient cube from its object record.
struct SceneRayInstance {
 inverse_world:mat4x4<f32>,
 mesh_word:u32,
 bvh_root:u32,
}
@group(1) @binding(1) var<storage,read> scene_source:array<u32>;
@group(1) @binding(2) var<storage,read> scene_instances:array<SceneRayInstance>;

struct SceneHit {
 hit:bool, distance:f32, position:vec3<f32>, normal:vec3<f32>,
 geometric_normal:vec3<f32>, front_face:bool, uv:vec2<f32>, color:vec4<f32>,
 instance_id:u32, instance_flags:u32, mesh_id:u32, primitive_id:u32,
 material_word:u32, barycentrics:vec2<f32>,
 tangent:vec3<f32>, bitangent:vec3<f32>, lightmap_uv:vec2<f32>, lightmap_bounds:vec4<f32>, authored_tangent:vec4<f32>,
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
// The normal layer whose record starts at word `at`.
fn scene_normal_layer(at:u32)->NormalLayer {
 return NormalLayer(scene_v2(at+SCENE_NORMAL_LAYER_CYCLES),scene_f32(at+SCENE_NORMAL_LAYER_SCALE),scene_f32(at+SCENE_NORMAL_LAYER_STRENGTH));
}
fn scene_material_values(at:u32)->Material {
 let layers=at+SCENE_MATERIAL_NORMAL_LAYERS;
 return Material(scene_v4(at+SCENE_MATERIAL_BASE),scene_v3(at+SCENE_MATERIAL_EMISSION),scene_f32(at+SCENE_MATERIAL_ENVIRONMENT_SCALE),
 scene_f32(at+SCENE_MATERIAL_METALLIC),scene_f32(at+SCENE_MATERIAL_ROUGHNESS),scene_f32(at+SCENE_MATERIAL_COAT),scene_f32(at+SCENE_MATERIAL_COAT_ROUGHNESS),
 scene_f32(at+SCENE_MATERIAL_NORMAL_SCALE),scene_f32(at+SCENE_MATERIAL_BUMP_SCALE),scene_f32(at+SCENE_MATERIAL_ANISOTROPY_STRENGTH),scene_f32(at+SCENE_MATERIAL_ANISOTROPY_ROTATION),
 scene_f32(at+SCENE_MATERIAL_ALPHA_CUTOFF),scene_source[at+SCENE_MATERIAL_VISIBILITY_GROUP],scene_source[at+SCENE_MATERIAL_FLAGS],
 array<NormalLayer,2>(scene_normal_layer(layers),scene_normal_layer(layers+SCENE_NORMAL_LAYER_WORDS)));
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
  return vec4(srgb_to_linear(rgba.rgb),rgba.a);
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
// The rectangle the packed UVs of the mesh whose record is at `mesh` span
// (min in xy, extent in zw).
fn scene_mesh_uv_rect(mesh:u32)->vec4<f32> {
 return scene_v4(mesh+SCENE_MESH_UV_RECT);
}
// The packed vertex (packed_vertex.wgsl) whose record starts at word `at`,
// field by field; its UV across its mesh's rectangle `rect`.
fn scene_vertex_position(at:u32)->vec3<f32> {
 return scene_v3(at+PACKED_VERTEX_POSITION);
}
fn scene_vertex_frame(at:u32)->PackedFrame {
 return packed_vertex_frame(scene_source[at+PACKED_VERTEX_AXIS],scene_source[at+PACKED_VERTEX_ANGLE_CHART]);
}
fn scene_vertex_uv(at:u32,rect:vec4<f32>)->vec2<f32> {
 return packed_vertex_uv(scene_source[at+PACKED_VERTEX_UV],rect);
}
fn scene_vertex_color(at:u32)->vec4<f32> {
 return packed_vertex_color(scene_source[at+PACKED_VERTEX_COLOR]);
}
fn scene_vertex_lightmap_uv(at:u32)->vec2<f32> {
 return packed_vertex_lightmap_uv(scene_source[at+PACKED_VERTEX_LIGHTMAP_UV]);
}
// Its lightmap chart's bounds, from the chart table of the mesh whose record
// is at `mesh`.
fn scene_vertex_lightmap_bounds(mesh:u32,at:u32)->vec4<f32> {
 let chart=packed_vertex_chart(scene_source[at+PACKED_VERTEX_ANGLE_CHART]);
 return scene_v4(scene_source[mesh+SCENE_MESH_CHARTS]+chart*SCENE_CHART_WORDS);
}
// The record words of vertex `vertex` of the mesh whose record is at `mesh`.
fn scene_vertex_word(mesh:u32,vertex:u32)->u32 {
 return scene_source[mesh+SCENE_MESH_VERTICES]+vertex*PACKED_VERTEX_WORDS;
}
fn scene_vertex_words(mesh:u32,primitive:u32)->vec3<u32> {
 let indices=scene_source[mesh+SCENE_MESH_INDICES]+primitive*3u;
 return vec3(scene_vertex_word(mesh,scene_source[indices]),scene_vertex_word(mesh,scene_source[indices+1u]),scene_vertex_word(mesh,scene_source[indices+2u]));
}
// The texture coordinates of triangle `vertices` of a mesh whose UVs span
// `rect`, and its colour, at barycentrics `b`, as a hit's shading and the
// masked any-hit test read them.
fn scene_interpolated_uv(rect:vec4<f32>,vertices:vec3<u32>,b:vec3<f32>)->vec2<f32> {
 return scene_vertex_uv(vertices.x,rect)*b.x+scene_vertex_uv(vertices.y,rect)*b.y+scene_vertex_uv(vertices.z,rect)*b.z;
}
fn scene_interpolated_color(vertices:vec3<u32>,b:vec3<f32>)->vec4<f32> {
 return scene_vertex_color(vertices.x)*b.x+scene_vertex_color(vertices.y)*b.y+scene_vertex_color(vertices.z)*b.z;
}
fn scene_world_geometric(normal_matrix:mat4x4<f32>,v:vec3<u32>)->vec3<f32> {
 let e1=scene_vertex_position(v.y)-scene_vertex_position(v.x);
 let e2=scene_vertex_position(v.z)-scene_vertex_position(v.x);
 // Preserve the authored front side under mirrored instances, as raster does.
 return normalize((normal_matrix*vec4(cross(e1,e2),0.)).xyz);
}
struct SceneRay {
 origin:vec4<f32>,
 direction:vec4<f32>,
}
// A traversal's hit: whether it hit, the instance's index, the mesh and the
// triangle; its distance and barycentrics.
struct RawSceneHit {
 intersection:vec4<u32>,
 coords:vec4<f32>,
}
fn scene_decode_hit(raw:RawSceneHit,origin:vec3<f32>,direction:vec3<f32>)->SceneHit {
 var result:SceneHit;
 if raw.intersection.x==0u {
  return result;
 }
 let index=raw.intersection.y;
 let instance=scene_instances[index];
 let world=objects[index].model;
 let normal_matrix=transpose(instance.inverse_world);
 let mesh=instance.mesh_word+raw.intersection.z*SCENE_MESH_WORDS;
 let vertices=scene_vertex_words(mesh,raw.intersection.w);
 let b=vec3(1.-raw.coords.yz.x-raw.coords.yz.y,raw.coords.yz);
 let rect=scene_mesh_uv_rect(mesh);
 let uv0=scene_vertex_uv(vertices.x,rect);
 let uv1=scene_vertex_uv(vertices.y,rect);
 let uv2=scene_vertex_uv(vertices.z,rect);
 var frames=array<PackedFrame,3>(scene_vertex_frame(vertices.x),scene_vertex_frame(vertices.y),scene_vertex_frame(vertices.z));
 let n=frames[0].normal*b.x+frames[1].normal*b.y+frames[2].normal*b.z;
 result.hit=true;
 result.distance=raw.coords.x;
 // Reconstruct from the ACTUAL offset query origin, never the receiver position.
 result.position=origin+direction*raw.coords.x;
 result.geometric_normal=scene_world_geometric(normal_matrix,vertices);
 result.front_face=dot(result.geometric_normal,direction)<0.;
 result.normal=normalize((normal_matrix*vec4(n,0.)).xyz);
 // Match raster: transform and orthogonalize each authored vertex frame before
 // interpolation, then reorthogonalize the interpolated tangent at shading.
 var tangent=vec4(0.);
 for(var k=0u;k<3u;k++) {
  let authored=frames[k].tangent;
  let vertex_n=normalize((normal_matrix*vec4(frames[k].normal,0.)).xyz);
  let transformed=(world*vec4(authored.xyz,0.)).xyz;
  let projected=transformed-vertex_n*dot(vertex_n,transformed);
  if dot(projected,projected)>0. {
   tangent+=vec4(normalize(projected),authored.w)*b[k];
  }
 }
 tangent.w*=select(1.,-1.,dot(world[0].xyz,cross(world[1].xyz,world[2].xyz))<0.);
 result.authored_tangent=tangent;
 if !result.front_face {
  result.normal=-result.normal;
 }
 result.uv=uv0*b.x+uv1*b.y+uv2*b.z;
 result.color=scene_interpolated_color(vertices,b);
 result.lightmap_uv=scene_vertex_lightmap_uv(vertices.x)*b.x+scene_vertex_lightmap_uv(vertices.y)*b.y+scene_vertex_lightmap_uv(vertices.z)*b.z;
 result.lightmap_bounds=scene_vertex_lightmap_bounds(mesh,vertices.x);
 result.instance_id=index;
 result.instance_flags=objects[index].flags;
 result.mesh_id=raw.intersection.z;
 result.primitive_id=raw.intersection.w;
 result.material_word=scene_source[mesh+SCENE_MESH_MATERIAL_WORD];
 result.barycentrics=raw.coords.yz;
 let dp1=(world*vec4(scene_vertex_position(vertices.y)-scene_vertex_position(vertices.x),0.)).xyz;
 let dp2=(world*vec4(scene_vertex_position(vertices.z)-scene_vertex_position(vertices.x),0.)).xyz;
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
