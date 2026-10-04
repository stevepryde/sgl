// The scene source's layout: a header, then the ranges content owns
// (scene::rays): each image's level 0, each material's record, each model's
// consecutive mesh records, packed exact Vertex arrays and indices, and its
// BVH (`scene_rays_portable.wgsl`), a deforming model's influences and morph
// targets, and a deforming instance's joint matrices, morph weights and
// deformed vertices (deformation.wgsl). Every address is a word of the
// source. Word 0 starts no record, so a zero image or BVH root word means
// none.
// The header's words.
const SCENE_HEADER_VISIBILITY_MASK:u32=0u;
const SCENE_HEADER_INSTANCE_COUNT:u32=1u;
// An image's: its size and format, then its level 0 row by row: RGBA8
// texels, or BC7's 4x4 blocks as stored, four words each.
const SCENE_IMAGE_WIDTH:u32=0u;
const SCENE_IMAGE_HEIGHT:u32=1u;
const SCENE_IMAGE_FORMAT:u32=2u;
const SCENE_IMAGE_TEXELS:u32=3u;
// An image's formats.
const SCENE_IMAGE_RGBA8:u32=0u;
const SCENE_IMAGE_BC7:u32=1u;
// A vertex's stride and its fields' offsets, in words.
const SCENE_VERTEX_WORDS:u32=22u;
const SCENE_VERTEX_POSITION:u32=0u;
const SCENE_VERTEX_NORMAL:u32=3u;
const SCENE_VERTEX_UV:u32=6u;
const SCENE_VERTEX_COLOR:u32=8u;
const SCENE_VERTEX_LIGHTMAP_UV:u32=12u;
const SCENE_VERTEX_LIGHTMAP_BOUNDS:u32=14u;
const SCENE_VERTEX_TANGENT:u32=18u;
// A mesh record's: its vertices', indices' and material record's words, and
// its first vertex among its model's, where a deforming instance's vertices
// of it start (deformation.wgsl).
const SCENE_MESH_WORDS:u32=4u;
const SCENE_MESH_VERTICES:u32=0u;
const SCENE_MESH_INDICES:u32=1u;
const SCENE_MESH_MATERIAL_WORD:u32=2u;
const SCENE_MESH_FIRST_VERTEX:u32=3u;
// A material record's: its values (material.wgsl's Material, each member's
// word), then its textures' image words, wraps and whether lightmap charts
// light it.
const SCENE_MATERIAL_BASE:u32=0u;
const SCENE_MATERIAL_EMISSION:u32=4u;
const SCENE_MATERIAL_ENVIRONMENT_SCALE:u32=7u;
const SCENE_MATERIAL_METALLIC:u32=8u;
const SCENE_MATERIAL_ROUGHNESS:u32=9u;
const SCENE_MATERIAL_COAT:u32=10u;
const SCENE_MATERIAL_COAT_ROUGHNESS:u32=11u;
const SCENE_MATERIAL_NORMAL_SCALE:u32=12u;
const SCENE_MATERIAL_BUMP_SCALE:u32=13u;
const SCENE_MATERIAL_ANISOTROPY_STRENGTH:u32=14u;
const SCENE_MATERIAL_ANISOTROPY_ROTATION:u32=15u;
const SCENE_MATERIAL_ALPHA_CUTOFF:u32=16u;
const SCENE_MATERIAL_VISIBILITY_GROUP:u32=17u;
const SCENE_MATERIAL_FLAGS:u32=18u;
const SCENE_MATERIAL_TEXTURES:u32=20u;
const SCENE_MATERIAL_WRAP:u32=26u;
const SCENE_MATERIAL_BAKED:u32=28u;
// Each texture's index in `SceneMaterial::textures`.
const SCENE_TEXTURE_BASE:u32=0u;
const SCENE_TEXTURE_METALLIC_ROUGHNESS:u32=1u;
const SCENE_TEXTURE_EMISSION:u32=2u;
const SCENE_TEXTURE_NORMAL:u32=3u;
const SCENE_TEXTURE_BUMP:u32=4u;
const SCENE_TEXTURE_ANISOTROPY:u32=5u;
