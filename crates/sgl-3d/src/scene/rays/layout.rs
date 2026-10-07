//! The WGSL twins of the ray source's layouts, which the layout test
//! compares with `scene_source.wgsl` and `scene_rays.wgsl`.
use super::charts::Chart;
use super::instances::{InstanceEntry, InstanceLeaf};
use super::model::{MeshRecord, SectionRecord};
use super::{
    IMAGE_BC7, IMAGE_RGBA8, ImageHeader, MaterialRecord, MaterialTextures, SourceHeader, bvh,
};
use crate::shading::material::{MaterialUniform, NormalLayerUniform};

/// The WGSL twins of the source's record layouts, in words: `SourceHeader`,
/// `ImageHeader` and its formats, `MeshRecord` and a chart table entry,
/// `MaterialRecord` with its normal layers, the BVH's node and leaf
/// primitive and an instance BVH's leaf. The packed vertex's are
/// `shading::packed_vertex`'s.
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
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
        ("SCENE_MESH_CHARTS", offset_of!(MeshRecord, charts)),
        ("SCENE_MESH_UV_RECT", offset_of!(MeshRecord, uv_rect)),
        ("SCENE_MESH_SECTIONS", offset_of!(MeshRecord, sections)),
        (
            "SCENE_MESH_SECTION_COUNT",
            offset_of!(MeshRecord, section_count),
        ),
        ("SCENE_SECTION_WORDS", size_of::<SectionRecord>()),
        ("SCENE_SECTION_MIN", offset_of!(SectionRecord, bounds_min)),
        ("SCENE_SECTION_MAX", offset_of!(SectionRecord, bounds_max)),
        (
            "SCENE_SECTION_FIRST_INDEX",
            offset_of!(SectionRecord, first_index),
        ),
        (
            "SCENE_SECTION_TRIANGLES",
            offset_of!(SectionRecord, triangles),
        ),
        ("SCENE_CHART_WORDS", size_of::<Chart>()),
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
            "SCENE_MATERIAL_OCCLUSION_STRENGTH",
            material(offset_of!(MaterialUniform, occlusion_strength)),
        ),
        (
            "SCENE_MATERIAL_SPECULAR_F0",
            material(offset_of!(MaterialUniform, specular_f0)),
        ),
        (
            "SCENE_MATERIAL_SPECULAR",
            material(offset_of!(MaterialUniform, specular)),
        ),
        (
            "SCENE_MATERIAL_NORMAL_LAYERS",
            material(offset_of!(MaterialUniform, normal_layers)),
        ),
        (
            "SCENE_MATERIAL_MAPS",
            material(offset_of!(MaterialUniform, maps)),
        ),
        (
            "SCENE_MATERIAL_COAT_NORMAL_SCALE",
            material(offset_of!(MaterialUniform, coat_normal_scale)),
        ),
        (
            "SCENE_MATERIAL_IRIDESCENCE",
            material(offset_of!(MaterialUniform, iridescence)),
        ),
        (
            "SCENE_MATERIAL_IRIDESCENCE_IOR",
            material(offset_of!(MaterialUniform, iridescence_ior)),
        ),
        (
            "SCENE_MATERIAL_IRIDESCENCE_THICKNESS",
            material(offset_of!(MaterialUniform, iridescence_thickness)),
        ),
        (
            "SCENE_MATERIAL_ATTENUATION",
            material(offset_of!(MaterialUniform, attenuation)),
        ),
        (
            "SCENE_MATERIAL_TRANSMISSION",
            material(offset_of!(MaterialUniform, transmission)),
        ),
        (
            "SCENE_MATERIAL_THICKNESS",
            material(offset_of!(MaterialUniform, thickness)),
        ),
        (
            "SCENE_MATERIAL_IOR",
            material(offset_of!(MaterialUniform, ior)),
        ),
        (
            "SCENE_MATERIAL_DISPERSION",
            material(offset_of!(MaterialUniform, dispersion)),
        ),
        (
            "SCENE_MATERIAL_SHEEN",
            material(offset_of!(MaterialUniform, sheen)),
        ),
        (
            "SCENE_MATERIAL_SHEEN_ROUGHNESS",
            material(offset_of!(MaterialUniform, sheen_roughness)),
        ),
        (
            "SCENE_MATERIAL_DIFFUSE_TRANSMISSION_COLOR",
            material(offset_of!(MaterialUniform, diffuse_transmission_color)),
        ),
        (
            "SCENE_MATERIAL_DIFFUSE_TRANSMISSION",
            material(offset_of!(MaterialUniform, diffuse_transmission)),
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
        (
            "SCENE_TEXTURE_CLEARCOAT",
            offset_of!(MaterialTextures, clearcoat),
        ),
        (
            "SCENE_TEXTURE_COAT_ROUGHNESS",
            offset_of!(MaterialTextures, coat_roughness),
        ),
        (
            "SCENE_TEXTURE_COAT_NORMAL",
            offset_of!(MaterialTextures, coat_normal),
        ),
        (
            "SCENE_TEXTURE_IRIDESCENCE",
            offset_of!(MaterialTextures, iridescence),
        ),
        (
            "SCENE_TEXTURE_IRIDESCENCE_THICKNESS",
            offset_of!(MaterialTextures, iridescence_thickness),
        ),
        (
            "SCENE_TEXTURE_SHEEN_COLOR",
            offset_of!(MaterialTextures, sheen_color),
        ),
        (
            "SCENE_TEXTURE_SHEEN_ROUGHNESS",
            offset_of!(MaterialTextures, sheen_roughness),
        ),
        (
            "SCENE_TEXTURE_DIFFUSE_TRANSMISSION",
            offset_of!(MaterialTextures, diffuse_transmission),
        ),
        (
            "SCENE_TEXTURE_DIFFUSE_TRANSMISSION_COLOR",
            offset_of!(MaterialTextures, diffuse_transmission_color),
        ),
        (
            "SCENE_TEXTURE_THICKNESS",
            offset_of!(MaterialTextures, thickness),
        ),
        ("SCENE_TEXTURES", size_of::<MaterialTextures>()),
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
                ("geometry", "SCENE_IMAGE_RGBA8", IMAGE_RGBA8),
                ("geometry", "SCENE_IMAGE_BC7", IMAGE_BC7),
                ("cull", "SCENE_SECTION_PAIRED", super::model::SECTION_PAIRED),
                (
                    "world_reflections",
                    "SCENE_BVH_LEAF_PRIMITIVES",
                    bvh::LEAF_PRIMITIVES as u32,
                ),
                // The kinds' bits, the TLAS instance masks the scene builds
                // with and a ray's cull mask selects.
                (
                    "world_reflections",
                    "SCENE_KIND_STATIC",
                    u32::from(super::acceleration::MASK_STATIC),
                ),
                (
                    "world_reflections",
                    "SCENE_KIND_MOVING",
                    u32::from(super::acceleration::MASK_MOVING),
                ),
            ]
            .map(|(program, name, value)| {
                crate::shading::layout_tests::Constant::new(
                    program,
                    name,
                    naga::Literal::U32(value),
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
