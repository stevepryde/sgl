//! The WGSL twins of the ray source's layouts, which the layout test
//! compares with `scene_source.wgsl` and `scene_rays.wgsl`.
use super::instances::{InstanceEntry, InstanceLeaf};
use super::{
    IMAGE_BC7, IMAGE_RGBA8, ImageHeader, MaterialRecord, MaterialTextures, MeshRecord,
    SourceHeader, bvh,
};
use crate::shading::material::{MaterialUniform, NormalLayerUniform};

/// The WGSL twins of the source's record layouts, in words: `SourceHeader`,
/// `ImageHeader` and its formats, `asset::Vertex` (which the source holds
/// verbatim), `MeshRecord`, `MaterialRecord` with its normal layers, the
/// BVH's node and leaf primitive and an instance BVH's leaf.
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::asset::Vertex;
    use std::mem::{offset_of, size_of};
    let material = |field: usize| offset_of!(MaterialRecord, material) + field;
    let source = [
        (
            "SCENE_HEADER_VISIBILITY_MASK",
            offset_of!(SourceHeader, visibility_mask),
        ),
        (
            "SCENE_HEADER_STATIC_ROOT",
            offset_of!(SourceHeader, static_root),
        ),
        (
            "SCENE_HEADER_MOVING_ROOT",
            offset_of!(SourceHeader, moving_root),
        ),
        ("SCENE_IMAGE_WIDTH", offset_of!(ImageHeader, width)),
        ("SCENE_IMAGE_HEIGHT", offset_of!(ImageHeader, height)),
        ("SCENE_IMAGE_FORMAT", offset_of!(ImageHeader, format)),
        ("SCENE_IMAGE_TEXELS", size_of::<ImageHeader>()),
        ("SCENE_VERTEX_WORDS", size_of::<Vertex>()),
        ("SCENE_VERTEX_POSITION", offset_of!(Vertex, position)),
        ("SCENE_VERTEX_NORMAL", offset_of!(Vertex, normal)),
        ("SCENE_VERTEX_UV", offset_of!(Vertex, uv)),
        ("SCENE_VERTEX_COLOR", offset_of!(Vertex, color)),
        ("SCENE_VERTEX_LIGHTMAP_UV", offset_of!(Vertex, lightmap_uv)),
        (
            "SCENE_VERTEX_LIGHTMAP_BOUNDS",
            offset_of!(Vertex, lightmap_bounds),
        ),
        ("SCENE_VERTEX_TANGENT", offset_of!(Vertex, tangent)),
        ("SCENE_MESH_WORDS", size_of::<MeshRecord>()),
        ("SCENE_MESH_VERTICES", offset_of!(MeshRecord, vertices)),
        ("SCENE_MESH_INDICES", offset_of!(MeshRecord, indices)),
        (
            "SCENE_MESH_MATERIAL_WORD",
            offset_of!(MeshRecord, material_word),
        ),
        (
            "SCENE_MESH_FIRST_VERTEX",
            offset_of!(MeshRecord, first_vertex),
        ),
        (
            "SCENE_MATERIAL_BASE",
            material(offset_of!(MaterialUniform, base)),
        ),
        (
            "SCENE_MATERIAL_EMISSION",
            material(offset_of!(MaterialUniform, emission)),
        ),
        (
            "SCENE_MATERIAL_ENVIRONMENT_SCALE",
            material(offset_of!(MaterialUniform, environment_scale)),
        ),
        (
            "SCENE_MATERIAL_METALLIC",
            material(offset_of!(MaterialUniform, metallic)),
        ),
        (
            "SCENE_MATERIAL_ROUGHNESS",
            material(offset_of!(MaterialUniform, roughness)),
        ),
        (
            "SCENE_MATERIAL_COAT",
            material(offset_of!(MaterialUniform, coat)),
        ),
        (
            "SCENE_MATERIAL_COAT_ROUGHNESS",
            material(offset_of!(MaterialUniform, coat_roughness)),
        ),
        (
            "SCENE_MATERIAL_NORMAL_SCALE",
            material(offset_of!(MaterialUniform, normal_scale)),
        ),
        (
            "SCENE_MATERIAL_BUMP_SCALE",
            material(offset_of!(MaterialUniform, bump_scale)),
        ),
        (
            "SCENE_MATERIAL_ANISOTROPY_STRENGTH",
            material(offset_of!(MaterialUniform, anisotropy_strength)),
        ),
        (
            "SCENE_MATERIAL_ANISOTROPY_ROTATION",
            material(offset_of!(MaterialUniform, anisotropy_rotation)),
        ),
        (
            "SCENE_MATERIAL_ALPHA_CUTOFF",
            material(offset_of!(MaterialUniform, alpha_cutoff)),
        ),
        (
            "SCENE_MATERIAL_VISIBILITY_GROUP",
            material(offset_of!(MaterialUniform, visibility_group)),
        ),
        (
            "SCENE_MATERIAL_FLAGS",
            material(offset_of!(MaterialUniform, flags)),
        ),
        (
            "SCENE_MATERIAL_NORMAL_LAYERS",
            material(offset_of!(MaterialUniform, normal_layers)),
        ),
        ("SCENE_NORMAL_LAYER_WORDS", size_of::<NormalLayerUniform>()),
        (
            "SCENE_NORMAL_LAYER_CYCLES",
            offset_of!(NormalLayerUniform, cycles),
        ),
        (
            "SCENE_NORMAL_LAYER_SCALE",
            offset_of!(NormalLayerUniform, scale),
        ),
        (
            "SCENE_NORMAL_LAYER_STRENGTH",
            offset_of!(NormalLayerUniform, strength),
        ),
        (
            "SCENE_MATERIAL_TEXTURES",
            offset_of!(MaterialRecord, textures),
        ),
        ("SCENE_MATERIAL_WRAP", offset_of!(MaterialRecord, wrap)),
        ("SCENE_MATERIAL_BAKED", offset_of!(MaterialRecord, baked)),
        ("SCENE_TEXTURE_BASE", offset_of!(MaterialTextures, base)),
        (
            "SCENE_TEXTURE_METALLIC_ROUGHNESS",
            offset_of!(MaterialTextures, metallic_roughness),
        ),
        (
            "SCENE_TEXTURE_EMISSION",
            offset_of!(MaterialTextures, emission),
        ),
        ("SCENE_TEXTURE_NORMAL", offset_of!(MaterialTextures, normal)),
        ("SCENE_TEXTURE_BUMP", offset_of!(MaterialTextures, bump)),
        (
            "SCENE_TEXTURE_ANISOTROPY",
            offset_of!(MaterialTextures, anisotropy),
        ),
    ];
    // The BVH is declared beside the portable traversal that reads it.
    source
        .into_iter()
        .map(|constant| ("geometry", constant))
        .chain(
            bvh::layout()
                .into_iter()
                .chain([("SCENE_BVH_INSTANCE_WORDS", size_of::<InstanceLeaf>())])
                .map(|constant| ("world_reflections", constant)),
        )
        .map(|(program, (name, bytes))| {
            crate::shading::layout_tests::Constant::new(
                program,
                name,
                naga::Literal::U32(u32::try_from(bytes / 4).unwrap()),
            )
        })
        .chain(
            [
                ("SCENE_IMAGE_RGBA8", IMAGE_RGBA8),
                ("SCENE_IMAGE_BC7", IMAGE_BC7),
            ]
            .map(|(name, format)| {
                crate::shading::layout_tests::Constant::new(
                    "geometry",
                    name,
                    naga::Literal::U32(format),
                )
            }),
        )
        .collect()
}

/// An instance's entry, `InstanceEntry`.
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "world_reflections",
        "SceneRayInstance",
        InstanceEntry,
        [inverse_world, mesh_word, bvh_root]
    )]
}
