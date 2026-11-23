//! A ray hit's sample of a material map under its sampler state (specs/
//! sgl3d-architecture.md, Material samplers): level 0, the only level rays
//! hold, at an explicit level of detail of 0, which Vulkan's texel
//! filtering magnifies.
use super::tests::{Fixture, triangle};
use crate::asset::{Asset, Material, MaterialTexture, Sampler, TextureFilter, TextureWrap};
use crate::test_support::{labelled, magnified};
use TextureFilter::{Linear, Nearest};
use TextureWrap::{ClampToEdge, MirroredRepeat, Repeat};
use wgpu::util::DeviceExt;

/// Each map a ray hit reads, by its index here: its texture word
/// (`SCENE_TEXTURE_*`) and its map binding (`MATERIAL_SLOT_*`), the pair
/// surface_ray.wgsl samples it by.
const MAPS: &str = r#"
fn test_map(m:SceneMaterial,map:u32)->vec2<u32> {
 switch map {
  case 0u { return vec2(m.textures[SCENE_TEXTURE_BASE],MATERIAL_SLOT_BASE); }
  case 1u { return vec2(m.textures[SCENE_TEXTURE_METALLIC_ROUGHNESS],MATERIAL_SLOT_METALLIC_ROUGHNESS); }
  case 2u { return vec2(m.textures[SCENE_TEXTURE_EMISSION],MATERIAL_SLOT_EMISSION); }
  case 3u { return vec2(m.textures[SCENE_TEXTURE_NORMAL],MATERIAL_SLOT_RELIEF); }
  case 4u { return vec2(m.textures[SCENE_TEXTURE_ANISOTROPY],MATERIAL_SLOT_ANISOTROPY); }
  case 5u { return vec2(m.textures[SCENE_TEXTURE_CLEARCOAT],MATERIAL_SLOT_CLEARCOAT); }
  case 6u { return vec2(m.textures[SCENE_TEXTURE_COAT_ROUGHNESS],MATERIAL_SLOT_COAT_ROUGHNESS); }
  case 7u { return vec2(m.textures[SCENE_TEXTURE_COAT_NORMAL],MATERIAL_SLOT_COAT_NORMAL); }
  case 8u { return vec2(m.textures[SCENE_TEXTURE_IRIDESCENCE],MATERIAL_SLOT_IRIDESCENCE); }
  case 9u { return vec2(m.textures[SCENE_TEXTURE_IRIDESCENCE_THICKNESS],MATERIAL_SLOT_IRIDESCENCE_THICKNESS); }
  case 10u { return vec2(m.textures[SCENE_TEXTURE_THICKNESS],MATERIAL_SLOT_THICKNESS); }
  case 11u { return vec2(m.textures[SCENE_TEXTURE_SHEEN_COLOR],MATERIAL_SLOT_SHEEN_COLOR); }
  case 12u { return vec2(m.textures[SCENE_TEXTURE_SHEEN_ROUGHNESS],MATERIAL_SLOT_SHEEN_ROUGHNESS); }
  case 13u { return vec2(m.textures[SCENE_TEXTURE_DIFFUSE_TRANSMISSION],MATERIAL_SLOT_DIFFUSE_TRANSMISSION); }
  default { return vec2(m.textures[SCENE_TEXTURE_DIFFUSE_TRANSMISSION_COLOR],MATERIAL_SLOT_DIFFUSE_TRANSMISSION_COLOR); }
 }
}
struct Probe { material:u32, map:u32, uv:vec2<f32> }
@group(0) @binding(0) var<storage,read> probes:array<Probe>;
@group(0) @binding(1) var<storage,read_write> colors:array<vec4<f32>>;
@compute @workgroup_size(1) fn sample(@builtin(global_invocation_id) id:vec3<u32>) {
 let probe=probes[id.x];
 let m=scene_material(probe.material);
 let map=test_map(m,probe.map);
 colors[id.x]=scene_sample_map(m.values.map_samplers,map.x,map.y,probe.uv,true);
}
"#;

/// The map with `MAPS`' index `map` of `material`.
fn map_field(material: &mut Material, map: usize) -> &mut Option<MaterialTexture> {
    match map {
        0 => &mut material.base_texture,
        1 => &mut material.mr_texture,
        2 => &mut material.emissive_texture,
        3 => &mut material.normal_texture,
        4 => &mut material.anisotropy_texture,
        5 => &mut material.clearcoat_texture,
        6 => &mut material.coat_roughness_texture,
        7 => &mut material.coat_normal_texture,
        8 => &mut material.iridescence_texture,
        9 => &mut material.iridescence_thickness_texture,
        10 => &mut material.thickness_texture,
        11 => &mut material.sheen_color_texture,
        12 => &mut material.sheen_roughness_texture,
        13 => &mut material.diffuse_transmission_texture,
        _ => &mut material.diffuse_transmission_color_texture,
    }
}

/// How many maps `MAPS` names.
const MAP_COUNT: usize = 15;

/// The ray source of one triangle and `materials`, which sample the
/// labelled image (`labelled`), and each probe's sample, as a ray hit
/// takes it: material `material`'s map `map` (`MAPS`) at `uv`.
fn sample(materials: Vec<Material>, probes: &[(usize, usize, [f32; 2])]) -> Option<Vec<[f32; 4]>> {
    let (device, queue) = crate::test_support::device()?;
    let asset = Asset {
        meshes: vec![triangle(0., 0., false, 0)],
        materials,
        images: vec![labelled()],
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
    };
    let fixture = Fixture::new(&device, &queue, &[&asset]);
    let words: Vec<u32> = probes
        .iter()
        .flat_map(|&(material, map, [u, v])| {
            [
                fixture.materials[0][material],
                map as u32,
                u.to_bits(),
                v.to_bits(),
            ]
        })
        .collect();
    let probe_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sampler probes"),
        contents: bytemuck::cast_slice(&words),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sampled colours"),
        size: (probes.len() * 16) as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let source = crate::shading::compose(&[&crate::shading::SCENE_RAYS]);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ray hit samples"),
        source: wgpu::ShaderSource::Wgsl(format!("{source}\n{MAPS}").into()),
    });
    let io_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage {
                    read_only: binding == 0,
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }),
    });
    let io = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &io_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: probe_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[
                    Some(&io_layout),
                    Some(&crate::shading::bind::scene(&device)),
                ],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("sample"),
        compilation_options: Default::default(),
        cache: None,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &io, &[]);
        pass.set_bind_group(1, &fixture.scene_group(&device), &[]);
        pass.dispatch_workgroups(probes.len() as u32, 1, 1);
    }
    queue.submit([encoder.finish()]);
    let words = crate::test_support::storage_words(&device, &queue, &output);
    Some(bytemuck::cast_slice(&words).to_vec())
}

/// UV coordinates inside texels, between them and past both edges of the
/// labelled image's 4 texels, none on a texel's edge.
const COORDINATES: [f32; 8] = [-0.4, -0.1, 0.1, 0.3, 0.55, 0.875, 1.2, 1.45];

// Plausible defects: a ray hit sampling a map bilinearly or repeating
// whatever its sampler says, a wrap mode applied to the other axis or
// mapped to another, or the minification filter taken at level of detail 0
// where Vulkan magnifies. UVs (`COORDINATES`) fall inside texels, between
// them and past both edges of the labelled image (`labelled`) on both axes;
// the oracle is `magnified` over its known texels, in linear light, as a
// sample of the image's sRGB colour.
#[test]
fn ray_hits_sample_level_0_through_each_maps_sampler() {
    let states = [
        Sampler {
            mag_filter: Nearest,
            wrap: [Repeat, ClampToEdge],
            ..Sampler::default()
        },
        Sampler {
            wrap: [MirroredRepeat, Repeat],
            ..Sampler::default()
        },
        Sampler {
            wrap: [ClampToEdge, MirroredRepeat],
            ..Sampler::default()
        },
        Sampler {
            mag_filter: Nearest,
            min_filter: Linear,
            mipmap_filter: Some(Linear),
            wrap: [MirroredRepeat; 2],
        },
        // Minified, this would take level 0's nearest texel.
        Sampler {
            mag_filter: Linear,
            min_filter: Nearest,
            mipmap_filter: None,
            wrap: [ClampToEdge, Repeat],
        },
    ];
    let materials = states
        .map(|sampler| Material {
            base_texture: Some(MaterialTexture { image: 0, sampler }),
            ..Default::default()
        })
        .into();
    let probes: Vec<_> = (0..states.len())
        .flat_map(|material| {
            COORDINATES
                .iter()
                .flat_map(move |&u| COORDINATES.map(move |v| (material, 0, [u, v])))
        })
        .collect();
    let Some(colors) = sample(materials, &probes) else {
        return;
    };
    for (&(material, _, uv), color) in probes.iter().zip(&colors) {
        let expected = magnified(uv, states[material]);
        assert!(
            (color[0] - expected).abs() < 1e-4,
            "{:?} at UV {uv:?}: {} for {expected}",
            states[material],
            color[0]
        );
    }
}

// Plausible defects: a map binding's state written to another binding's
// byte of the record's `map_samplers`, or read from one, past the first
// word as much as in it. Every map a ray hit reads samples the one
// labelled image in a state of its own, no two neighbours alike; the
// oracle is `magnified` under that map's state.
#[test]
fn every_map_binding_carries_its_own_state_to_ray_hits() {
    let wraps = [
        [Repeat, ClampToEdge],
        [MirroredRepeat, Repeat],
        [ClampToEdge, MirroredRepeat],
    ];
    let state = |map: usize| Sampler {
        mag_filter: if map.is_multiple_of(2) {
            Nearest
        } else {
            Linear
        },
        wrap: wraps[map % 3],
        ..Sampler::default()
    };
    let mut material = Material::default();
    for map in 0..MAP_COUNT {
        *map_field(&mut material, map) = Some(MaterialTexture {
            image: 0,
            sampler: state(map),
        });
    }
    let probes: Vec<_> = (0..MAP_COUNT)
        .flat_map(|map| {
            COORDINATES
                .iter()
                .flat_map(move |&u| [-0.4, 1.2].map(move |v| (0, map, [u, v])))
        })
        .collect();
    let Some(colors) = sample(vec![material], &probes) else {
        return;
    };
    for (&(_, map, uv), color) in probes.iter().zip(&colors) {
        let expected = magnified(uv, state(map));
        assert!(
            (color[0] - expected).abs() < 1e-4,
            "map {map} ({:?}) at UV {uv:?}: {} for {expected}",
            state(map),
            color[0]
        );
    }
}
