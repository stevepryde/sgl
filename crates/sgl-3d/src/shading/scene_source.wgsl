// The scene source's layout: a header, then the ranges content owns
// (scene::rays): each image's level 0, each material's record, each model's
// consecutive mesh records, each mesh's chart table, each mesh's section
// table, each mesh's packed vertices (packed_vertex.wgsl) and indices, and
// its BVH (`scene_rays_portable.wgsl`), a deforming model's influences and morph
// targets, a deforming instance's joint matrices, morph weights and
// deformed vertices (deformation.wgsl), and the static and moving instance
// BVHs (scene::rays::instances). Every address is a word of the source.
// Word 0 starts no record, so a zero image or BVH root word means none.
// The header's words: the enabled material visibility groups and the
// instance BVHs' roots.
const SCENE_HEADER_VISIBILITY_MASK:u32=0u;
const SCENE_HEADER_STATIC_ROOT:u32=1u;
const SCENE_HEADER_MOVING_ROOT:u32=2u;
// An image's: its size and format, then its level 0 row by row: RGBA8
// texels, or BC7's 4x4 blocks as stored, four words each.
const SCENE_IMAGE_WIDTH:u32=0u;
const SCENE_IMAGE_HEIGHT:u32=1u;
const SCENE_IMAGE_FORMAT:u32=2u;
const SCENE_IMAGE_TEXELS:u32=3u;
// An image's formats.
const SCENE_IMAGE_RGBA8:u32=0u;
const SCENE_IMAGE_BC7:u32=1u;
// A mesh record's: its packed vertices', indices' and material record's
// words; its first vertex among its model's, where a deforming instance's
// vertices of it start (deformation.wgsl); the word where its own chart
// table starts; the rectangle its packed UVs span (min in xy, extent in
// zw); and the word where its section table starts, with its sections.
const SCENE_MESH_WORDS:u32=11u;
const SCENE_MESH_VERTICES:u32=0u;
const SCENE_MESH_INDICES:u32=1u;
const SCENE_MESH_MATERIAL_WORD:u32=2u;
const SCENE_MESH_FIRST_VERTEX:u32=3u;
const SCENE_MESH_CHARTS:u32=4u;
const SCENE_MESH_UV_RECT:u32=5u;
const SCENE_MESH_SECTIONS:u32=9u;
const SCENE_MESH_SECTION_COUNT:u32=10u;
// A section table entry's: a leaf of the mesh's range hierarchy, at most
// SECTION_VERTICES / 3 triangles in the mesh's own order: its bounds in the
// model's space, min then max, its first index, relative to its mesh's
// indices, and its triangles, with SCENE_SECTION_PAIRED where they pair
// (scene::rays::model::SECTION_PAIRED).
const SCENE_SECTION_WORDS:u32=8u;
const SCENE_SECTION_MIN:u32=0u;
const SCENE_SECTION_MAX:u32=3u;
const SCENE_SECTION_FIRST_INDEX:u32=6u;
const SCENE_SECTION_TRIANGLES:u32=7u;
const SCENE_SECTION_PAIRED:u32=2147483648u;
// A chart table entry's: a lightmap chart's normalized atlas bounds, min
// then max.
const SCENE_CHART_WORDS:u32=4u;
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
const SCENE_MATERIAL_OCCLUSION_STRENGTH:u32=19u;
const SCENE_MATERIAL_SPECULAR_F0:u32=20u;
const SCENE_MATERIAL_SPECULAR:u32=23u;
const SCENE_MATERIAL_NORMAL_LAYERS:u32=24u;
const SCENE_MATERIAL_MAPS:u32=32u;
const SCENE_MATERIAL_COAT_NORMAL_SCALE:u32=33u;
const SCENE_MATERIAL_IRIDESCENCE:u32=34u;
const SCENE_MATERIAL_IRIDESCENCE_IOR:u32=35u;
const SCENE_MATERIAL_IRIDESCENCE_THICKNESS:u32=36u;
const SCENE_MATERIAL_SHEEN:u32=40u;
const SCENE_MATERIAL_SHEEN_ROUGHNESS:u32=43u;
const SCENE_MATERIAL_DIFFUSE_TRANSMISSION_COLOR:u32=44u;
const SCENE_MATERIAL_DIFFUSE_TRANSMISSION:u32=47u;
const SCENE_MATERIAL_TEXTURES:u32=48u;
const SCENE_MATERIAL_WRAP:u32=63u;
const SCENE_MATERIAL_BAKED:u32=65u;
// A normal layer's (material.wgsl's NormalLayer): its words, each member's
// word.
const SCENE_NORMAL_LAYER_WORDS:u32=4u;
const SCENE_NORMAL_LAYER_CYCLES:u32=0u;
const SCENE_NORMAL_LAYER_SCALE:u32=2u;
const SCENE_NORMAL_LAYER_STRENGTH:u32=3u;
// Each texture's index in `SceneMaterial::textures`, and their count.
const SCENE_TEXTURES:u32=15u;
const SCENE_TEXTURE_BASE:u32=0u;
const SCENE_TEXTURE_METALLIC_ROUGHNESS:u32=1u;
const SCENE_TEXTURE_EMISSION:u32=2u;
const SCENE_TEXTURE_NORMAL:u32=3u;
const SCENE_TEXTURE_BUMP:u32=4u;
const SCENE_TEXTURE_ANISOTROPY:u32=5u;
const SCENE_TEXTURE_CLEARCOAT:u32=6u;
const SCENE_TEXTURE_COAT_ROUGHNESS:u32=7u;
const SCENE_TEXTURE_COAT_NORMAL:u32=8u;
const SCENE_TEXTURE_IRIDESCENCE:u32=9u;
const SCENE_TEXTURE_IRIDESCENCE_THICKNESS:u32=10u;
const SCENE_TEXTURE_SHEEN_COLOR:u32=11u;
const SCENE_TEXTURE_SHEEN_ROUGHNESS:u32=12u;
const SCENE_TEXTURE_DIFFUSE_TRANSMISSION:u32=13u;
const SCENE_TEXTURE_DIFFUSE_TRANSMISSION_COLOR:u32=14u;
