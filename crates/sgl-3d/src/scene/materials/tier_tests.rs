//! What of a material's maps a device of the Basic binding tier puts in
//! effect.
use crate::asset::Image;
use crate::{Scene, test_support};

/// The most sampled textures per shader stage of a device S3D-1 gives the
/// Basic binding tier.
const BASIC_SAMPLED_TEXTURES: u32 = 47;

// Plausible defects: a map the device does not bind keeps its ray-source
// texture word or its `maps` bit, so ray hits shade it where raster gives
// way to its factor (S3D-5); the Extended tier starts below 48; or a bump
// map beside a normal map stays in effect, so the relief binding may take
// the wrong one. The oracle is the same material added without that map on
// the same device: on a device of 47 sampled textures a stage, the record
// of a material with the Extended tier's maps (anisotropy, clearcoat,
// clearcoat roughness and normal, iridescence and its thickness) holds the
// words of the same material without them, and that of a material with a normal and a bump map
// the words of the material with the normal map alone.
#[test]
fn a_basic_device_records_a_material_as_without_the_maps_it_drops() {
    let Some(adapter) = test_support::adapter() else {
        return;
    };
    let mut limits = crate::graphics_device::limits(&adapter);
    if limits.max_sampled_textures_per_shader_stage < BASIC_SAMPLED_TEXTURES {
        eprintln!(
            "skipping: the adapter binds fewer than {BASIC_SAMPLED_TEXTURES} sampled textures"
        );
        return;
    }
    limits.max_sampled_textures_per_shader_stage = BASIC_SAMPLED_TEXTURES;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: crate::graphics_device::features(&adapter),
        required_limits: limits,
        ..Default::default()
    }))
    .unwrap();
    let mut scene = Scene::new(&device, &queue);
    let images: Vec<_> = [
        [200, 90, 40, 255],
        [128, 128, 255, 255],
        [60; 4],
        [255, 128, 0, 255],
    ]
    .map(|texel| Image::Rgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba(texel))))
    .into();
    let plain = crate::asset::Material {
        base_texture: Some(0),
        anisotropy_strength: 0.5,
        bump_scale: 1.,
        ..test_support::cube().materials[0].clone()
    };
    let materials = [
        crate::asset::Material {
            anisotropy_texture: Some(3),
            clearcoat_texture: Some(3),
            coat_roughness_texture: Some(3),
            coat_normal_texture: Some(1),
            iridescence_texture: Some(3),
            iridescence_thickness_texture: Some(3),
            ..plain.clone()
        },
        plain.clone(),
        crate::asset::Material {
            normal_texture: Some(1),
            bump_texture: Some(2),
            ..plain.clone()
        },
        crate::asset::Material {
            normal_texture: Some(1),
            ..plain
        },
    ];
    let ids = scene
        .add_materials(&device, &queue, &materials, &images)
        .unwrap();
    let words = test_support::storage_words(&device, &queue, scene.rays.source());
    let record = |index: usize| {
        let range = scene.materials.get(ids[index]).unwrap().record.clone();
        words[range.start as usize..range.end as usize].to_vec()
    };
    assert_eq!(record(0), record(1), "the Extended tier's maps on Basic");
    assert_eq!(record(2), record(3), "a bump map beside a normal map");
}
