//! Independent analytic planes exercise the portable BVH's ray and candidate
//! queries on the default adapter.
use super::instances::{RayInstances, bounded};
use super::*;
use crate::asset::{Asset, CpuMesh, Material, Vertex};
use crate::content::instance::Mobility;
use crate::scene::objects::Objects;
use crate::shading::uniforms::ObjectUniform;
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

/// A moving instance of fixture model `model` (an asset's index) at `world`,
/// at index `id`.
#[derive(Clone, Copy)]
pub(super) struct Pose {
    pub model: usize,
    pub world: Mat4,
    pub id: u32,
}

/// The ray source of `assets`, each one model, added as a scene adds a
/// material's images and record and a model's meshes, with the instances'
/// object records and entries, group 1 over them and the standalone
/// dispatch that traces it.
pub(super) struct Fixture {
    pub rays: SceneRays,
    instances: RayInstances,
    /// Each asset's model and its bounds.
    models: Vec<(RayModel, [Vec3; 2])>,
    /// Each asset's material records.
    pub materials: Vec<Vec<u32>>,
    objects: Objects,
    layout: wgpu::BindGroupLayout,
    pub query: Query,
}

impl Fixture {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, assets: &[&Asset]) -> Self {
        let mut rays = SceneRays::new(device);
        let mut models = Vec::new();
        let mut materials = Vec::new();
        for asset in assets {
            let images: Vec<_> = asset
                .images
                .iter()
                .map(|image| rays.add_image(device, queue, image).unwrap().start)
                .collect();
            let words: Vec<_> = asset
                .materials
                .iter()
                .map(|m| {
                    // Every map in effect, as on a device of the Extended
                    // binding tier.
                    let maps = crate::scene::materials::maps::InEffect::of(
                        m,
                        crate::shading::bind::BindingTier::Extended,
                    );
                    rays.add_material(
                        device,
                        queue,
                        &MaterialUniform::new(&crate::SurfaceMaterial::authored(m), maps.maps()),
                        maps.ray_textures(|index| images[index]),
                        m.wrap,
                    )
                    .unwrap()
                    .start
                })
                .collect();
            let ranges: Vec<_> = asset
                .meshes
                .iter()
                .map(|mesh| {
                    crate::scene::mesh_ranges::MeshRanges::new(&mesh.vertices, &mesh.indices)
                })
                .collect();
            let meshes: Vec<_> = asset
                .meshes
                .iter()
                .zip(&ranges)
                .map(|(mesh, ranges)| RayMesh {
                    vertices: &mesh.vertices,
                    indices: &mesh.indices,
                    ranges,
                })
                .collect();
            let material_words: Vec<_> = asset
                .meshes
                .iter()
                .map(|mesh| words[mesh.material])
                .collect();
            let bounds = asset.meshes.iter().flat_map(|mesh| &mesh.vertices).fold(
                [Vec3::INFINITY, Vec3::NEG_INFINITY],
                |[min, max], vertex| {
                    let p = Vec3::from_array(vertex.position);
                    [min.min(p), max.max(p)]
                },
            );
            let mut prepared = super::prepare_model(&meshes).unwrap();
            let placed = rays
                .place_model(device, queue, &mut prepared, &material_words)
                .unwrap();
            rays.write(queue, placed.range.start, prepared.words());
            models.push((placed.ray, bounds));
            materials.push(words);
        }
        Self {
            rays,
            instances: RayInstances::new(device),
            models,
            materials,
            objects: Objects::new(device),
            layout: crate::shading::bind::scene(device),
            query: Query::new(device),
        }
    }

    /// Sets `poses` as the moving instances: each one's object record and
    /// entry at its index, and the moving BVH over them.
    pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, poses: &[Pose]) {
        let count = poses
            .iter()
            .map(|pose| pose.id as usize + 1)
            .max()
            .unwrap_or(0);
        self.instances
            .reserve(
                device,
                queue,
                &mut self.rays,
                count,
                Mobility::Moving,
                poses.len(),
            )
            .unwrap();
        self.objects.reserve(device, count).unwrap();
        let mut moving = Vec::new();
        for pose in poses {
            let (model, bounds) = self.models[pose.model];
            let world = pose.world.to_cols_array_2d();
            let record = ObjectUniform {
                model: world,
                previous_model: world,
                ..bytemuck::Zeroable::zeroed()
            };
            self.objects.write(queue, pose.id as usize, &record);
            self.instances.set(pose.id as usize, model, pose.world);
            moving.push(bounded(pose.id as usize, bounds, pose.world));
        }
        self.instances
            .update(device, queue, &self.rays, None, &mut moving);
    }

    /// Group 1 over the object records, the source and the entries.
    pub fn scene_group(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        super::super::scene_group(
            device,
            &self.layout,
            &[
                self.objects.buffer().clone(),
                self.rays.source().clone(),
                self.instances.buffer().clone(),
            ],
        )
    }
}

pub(super) fn triangle(x: f32, z: f32, back: bool, material: usize) -> CpuMesh {
    let vertices = [
        ([-1., -1., 0.], [0., 0.]),
        ([1., -1., 0.], [1., 0.]),
        ([0., 1., 0.], [0.5, 1.]),
    ]
    .map(|(p, uv)| Vertex {
        tangent: [0.0; 4],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
        position: [p[0] + x, p[1], p[2] + z],
        normal: [0., 0., 1.],
        uv,
        color: [0.2, 0.4, 0.8, 1.],
    })
    .to_vec();
    CpuMesh {
        vertices,
        indices: if back { vec![0, 2, 1] } else { vec![0, 1, 2] },
        material,
        deformation: Default::default(),
    }
}
fn material(double_sided: bool) -> Material {
    Material {
        anisotropy_strength: 0.0,
        anisotropy_rotation: 0.0,
        anisotropy_texture: None,
        visibility_group: 0,
        casts_directional_shadow: true,
        name: String::new(),
        base: [1.; 4],
        emissive: [2., 3., 4.],
        metallic: 0.3,
        roughness: 0.4,
        ior: 1.5,
        specular: 1.,
        specular_color: [1.; 3],
        clearcoat: 0.5,
        coat_roughness: 0.6,
        clearcoat_texture: None,
        coat_roughness_texture: None,
        coat_normal_texture: None,
        coat_normal_scale: 1.,
        iridescence: 0.,
        iridescence_ior: 1.3,
        iridescence_thickness: [100., 400.],
        iridescence_texture: None,
        iridescence_thickness_texture: None,
        sheen_color: [0.; 3],
        sheen_roughness: 0.,
        sheen_color_texture: None,
        sheen_roughness_texture: None,
        diffuse_transmission: 0.,
        diffuse_transmission_color: [1.; 3],
        diffuse_transmission_texture: None,
        diffuse_transmission_color_texture: None,
        base_texture: Some(0),
        mr_texture: None,
        occlusion_texture: None,
        occlusion_strength: 1.,
        emissive_texture: None,
        normal_texture: None,
        normal_scale: 1.,
        normal_layers: None,
        bump_texture: None,
        bump_scale: 0.,
        wrap: [gltf::texture::WrappingMode::Repeat; 2],
        double_sided,
        unlit: false,
        emits_into_gi: true,
        alpha: crate::AlphaMode::Opaque,
    }
}
pub(super) fn asset(meshes: Vec<CpuMesh>, two_sided: bool) -> Asset {
    Asset {
        meshes,
        materials: vec![material(two_sided)],
        images: vec![crate::asset::Image::Rgba8(
            image::RgbaImage::from_raw(
                2,
                2,
                vec![
                    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
                ],
            )
            .unwrap(),
        )],
        rig: Default::default(),
        ignored: Vec::new(),
    }
}

#[test]
fn portable_scene_faces_occlusion_coincident_offscreen_and_current_pose() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut optional_occluder = asset(
            vec![triangle(-3., -4., false, 0), triangle(-3., -1., false, 1)],
            false,
        );
        let mut optional_material = material(false);
        optional_material.visibility_group = 1;
        optional_occluder.materials.push(optional_material);
        let mut sloped = triangle(9., -5., false, 0);
        for vertex in &mut sloped.vertices {
            vertex.position[2] += 0.2 * (vertex.position[0] - 9.);
            vertex.normal = Vec3::new(-0.2, 0., 1.).normalize().to_array();
        }
        let assets = [
            asset(
                vec![triangle(0., -4., false, 0), triangle(0., -2., true, 0)],
                false,
            ),
            asset(vec![triangle(3., -2., true, 0)], true),
            optional_occluder,
            asset(
                vec![triangle(6., -3., true, 0), triangle(6., -3., false, 0)],
                false,
            ),
            asset(vec![triangle(30., -7., false, 0)], false),
            asset(vec![sloped], false),
        ];
        let mut scene = Fixture::new(&device, &queue, &assets.iter().collect::<Vec<_>>());
        let mut poses: Vec<_> = (0..assets.len())
            .map(|model| Pose {
                model,
                world: Mat4::IDENTITY,
                id: 100 + model as u32,
            })
            .collect();
        // Receiver at z=1 verifies reconstruction from actual offset origin.
        let ray_values: Vec<[f32; 8]> = [
            (0., 1., 10.),
            (3., 0., 10.),
            (-3., 0., 10.),
            (6., 0., 10.),
            (30., 0., 10.),
            (9., 0., 10.),
            (20., 0., 10.),
            (0., 0., 3.),
        ]
        .into_iter()
        .map(|(x, z, max)| [x, 0., z, 0.001, 0., 0., -1., max])
        .collect();
        let rays = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("analytic scene rays"),
            contents: bytemuck::cast_slice(&ray_values),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let hits = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("analytic scene hits"),
            size: rays.size(),
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let bindings = scene.query.ray_bind_group(&device, &rays, &hits);
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("decoded scene results"),
            size: 8 * 80,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene behavior readback"),
            size: output.size(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader_source = crate::shading::compose(&[&crate::shading::SCENE_RAYS_PORTABLE]);
        let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor {label:Some("decode independently inspected hit"),source:wgpu::ShaderSource::Wgsl(format!("{shader_source}\n{}",r#"
struct TestResult { position_t:vec4<f32>, normal_front:vec4<f32>, ids:vec4<f32>, uv_sample:vec4<f32>, tex_color:vec4<f32> }
@group(0) @binding(0) var<storage,read_write> test_results:array<TestResult>;
@group(0) @binding(1) var<storage,read> test_rays:array<SceneRay>;
@group(0) @binding(2) var<storage,read> test_hits:array<RawSceneHit>;
@compute @workgroup_size(8) fn decode(@builtin(global_invocation_id) id:vec3<u32>) {
 let r=test_rays[id.x];let h=scene_decode_hit(test_hits[id.x],r.origin.xyz,r.direction.xyz);
 var out:TestResult;
 if h.hit {
  let m=scene_material(h.material_word);
  out.position_t=vec4(h.position,h.distance);out.normal_front=vec4(h.normal,select(0.,1.,h.front_face));
  out.ids=vec4(f32(h.instance_id),f32(h.mesh_id),0.,1.);
  out.uv_sample=vec4(h.uv,h.color.xy);out.tex_color=scene_sample_texture(m.textures[0],h.uv,m.wrap,true);
 }
 test_results[id.x]=out;
}
"#).into())});
        let io_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[0, 1, 2].map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: binding != 0,
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
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: rays.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: hits.as_entire_binding(),
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
            entry_point: Some("decode"),
            compilation_options: Default::default(),
            cache: None,
        });
        for (mirrored, moved, enabled) in [false, true].into_iter().flat_map(|mirrored| {
            [(false, false), (false, true), (true, true), (true, false)]
                .map(move |(moved, enabled)| (mirrored, moved, enabled))
        }) {
            scene.rays.set_visibility_mask(&queue, u32::from(enabled));
            // Mirror each authored plane around its center: its front remains +Z.
            for (model, x) in [0., 3., -3., 6., 30., 9.].into_iter().enumerate() {
                let scale = if model == 5 { 2. } else { 1. } * if mirrored { -1. } else { 1. };
                poses[model].world = Mat4::from_translation(Vec3::new(
                    x,
                    0.,
                    if model == 5 && moved { 2. } else { 0. },
                )) * Mat4::from_scale(Vec3::new(scale, 1., 1.))
                    * Mat4::from_translation(Vec3::new(-x, 0., 0.));
            }
            let mut encoder = device.create_command_encoder(&Default::default());
            scene.update(&device, &queue, &poses);
            let group = scene.scene_group(&device);
            scene.query.trace(&mut encoder, &group, &bindings, 8);
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &io, &[]);
                pass.set_bind_group(1, &group, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output.size());
            queue.submit([encoder.finish()]);
            let (send, recv) = std::sync::mpsc::channel();
            readback.map_async(wgpu::MapMode::Read, .., move |result| {
                send.send(result).unwrap()
            });
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            recv.recv().unwrap().unwrap();
            let mapped = readback.get_mapped_range(..).unwrap();
            let result: &[[f32; 20]] = bytemuck::cast_slice(&mapped);
            eprintln!(
                "mirrored={mirrored}, moved={moved}, enabled={enabled}: hit distances {:?}",
                result.iter().map(|hit| hit[3]).collect::<Vec<_>>()
            );
            let close =
                |a: f32, b: f32| assert!((a - b).abs() < 0.0001, "expected {b}, actual {a}");
            // A rejected near backface must preserve the valid farther surface.
            close(result[0][3], 5.);
            close(result[0][2], -4.);
            close(result[0][9], 0.);
            close(result[0][7], 1.);
            // A back-facing double-sided material is accepted and its normal flips.
            close(result[1][3], 2.);
            close(result[1][6], -1.);
            close(result[1][7], 0.);
            // An optional near surface occludes when enabled; otherwise it is absent,
            // matching raster discard, so the always-visible farther face wins.
            close(result[2][3], if enabled { 1. } else { 4. });
            close(result[2][9], if enabled { 1. } else { 0. });
            // Coincident opposing faces cannot be handled by biased tmin restarts.
            close(result[3][3], 3.);
            close(result[3][9], 1.);
            close(result[3][7], 1.);
            close(result[4][3], 7.);
            close(result[4][0], 30.);
            close(result[5][3], if moved { 3. } else { 5. });
            let normal = Vec3::from_slice(&result[5][4..7]);
            let tangent = Vec3::new(if mirrored { -2. } else { 2. }, 0., 0.2).normalize();
            assert!(
                normal.dot(tangent).abs() < 0.0001,
                "ray normal {normal:?} is not perpendicular to {tangent:?}"
            );
            assert!(
                normal.z > 0.99,
                "mirrored authored front normal must face +Z"
            );
            close(result[5][7], 1.);
            close(result[6][11], 0.);
            close(result[7][11], 0.);
            close(result[0][12], 0.5);
            close(result[0][13], 0.5);
            // Vertex colours are packed sRGB8 (the Vertex encoding): within
            // half an 8-bit step of their sRGB encoding (IEC 61966-2-1).
            let srgb = |linear: f32| {
                if linear <= 0.0031308 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1. / 2.4) - 0.055
                }
            };
            for (actual, expected) in [(result[0][14], 0.2), (result[0][15], 0.4)] {
                assert!(
                    (srgb(actual) - srgb(expected)).abs() <= 0.5 / 255. + 1e-5,
                    "expected colour {expected}, actual {actual}"
                );
            }
            for channel in &result[0][16..19] {
                close(*channel, 0.5);
            }
            drop(mapped);
            readback.unmap();
        }
    });
}
