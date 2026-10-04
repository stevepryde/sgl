//! Actual raster derivatives and actual BLAS hits, with a glTF tangent-space oracle.
//! Normal texture [204,153,230] decodes to direction [153,51,205]. On the
//! authored XY plane, +U is +X (+U is -X in the mirrored islands), +V is +Y,
//! and the front normal is +Z. Back views reverse the complete lighting normal.
//! This catches wrong derivative signs, mirrored UV handedness and back-side
//! reversal; merely making raster and compute agree cannot satisfy the oracle.
//! Authority: glTF2.0 sections 3.9.3/3.9.5 and material.normalTexture;
//! Three185.1 TangentUtils.js and WGSLNodeBuilder.js (dFdy -> -dpdy).
//! MaterialNode.js selects normalMap before bumpMap. A varying bump texture
//! deliberately competes with the normal map, but must never alter this oracle,
//! including when the diagnostic disables the selected normal-map stage.
use super::tests::{Fixture, Pose};
use super::*;
use crate::asset::{Asset, CpuMesh, Material, Vertex};
use crate::scene::textures::upload as texture;
use crate::shading::material::MaterialMaps;
use crate::shading::{self, uniforms::ObjectUniform};
use glam::Vec3;
use wgpu::util::DeviceExt;

const CENTERS: [f32; 4] = [-3., -1., 1., 3.];
const PIXEL: [u8; 4] = [204, 153, 230, 255];

/// A bind group of `buffer` at binding 0.
fn group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    })
}

fn plane_asset(has_normal_map: bool) -> Asset {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (case, center) in CENTERS.into_iter().enumerate() {
        let first = vertices.len() as u32;
        for [x, y] in [[-0.75, -0.75], [0.75, -0.75], [0.75, 0.75], [-0.75, 0.75]] {
            let u = x / 1.5 + 0.5;
            vertices.push(Vertex {
                tangent: [0.0; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [center + x, y, 0.],
                normal: [0., 0., 1.],
                uv: [if case % 2 == 0 { u } else { 1. - u }, y / 1.5 + 0.5],
                // Fixture orthographic camera: cases 2/3 view the same +Z face
                // from -Z (180 degrees around Y), reversing screen X only.
                color: [center, if case < 2 { 1. } else { -1. }, 0., 1.],
            });
        }
        indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    Asset {
        meshes: vec![CpuMesh {
            vertices,
            indices,
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            visibility_group: 0,
            casts_directional_shadow: true,
            name: "known tangent-space slope".into(),
            base: [1.; 4],
            emissive: [0.; 3],
            metallic: 0.,
            roughness: 0.5,
            clearcoat: 0.,
            coat_roughness: 0.,
            base_texture: None,
            mr_texture: None,
            emissive_texture: None,
            normal_texture: has_normal_map.then_some(0),
            normal_scale: 1.,
            bump_texture: Some(1),
            bump_scale: 1.,
            wrap: [gltf::texture::WrappingMode::Repeat; 2],
            double_sided: true,
            unlit: false,
            alpha: crate::AlphaMode::Opaque,
        }],
        images: vec![
            crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba(PIXEL))),
            // A genuine nonconstant slope in both UV axes; the old sequential
            // composition measurably rotates the known normal in every case.
            crate::asset::Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| {
                let height = ((x + 2 * y) * 5) as u8;
                image::Rgba([height, height, height, 255])
            })),
        ],
        rig: Default::default(),
    }
}

#[test]
#[ignore = "real Metal raster/secondary normal-map orientation regression"]
fn authored_normal_map_axes_mirrored_uv_and_back_faces() {
    material_normal_oracle(true, true, false, false);
}

#[test]
#[ignore = "real Metal diagnostic must not fall through to an unselected bump map"]
fn disabled_normal_stage_preserves_normal_map_precedence() {
    material_normal_oracle(true, false, false, false);
}

// Known pre-extraction failure, reproduced on Apple M5/Metal using raster
// shaders from Hyperdrive 8486a6248ec43a346dd1e1145b33cdb63cb4fda3:
// case 0 raster = (-0.013229594, -0.0262142, 0.9995688), while the affine-height
// oracle = (-0.18945529, -0.37891057, 0.90583307). Secondary matches the oracle.
// Extraction preserves that behavior and this unsatisfied physical requirement.
#[test]
#[ignore = "known pre-extraction affine raster bump mismatch; real Metal"]
fn affine_bump_height_axes_mirrored_uv_and_back_faces() {
    material_normal_oracle(false, true, false, false);
}

#[test]
#[ignore = "real GPU; authored tangent under mirrored shear, normal maps and actual secondary hits"]
fn anisotropic_authored_frames_mirrored_shear_and_back_faces() {
    material_normal_oracle(true, true, true, false);
    material_normal_oracle(true, true, true, true);
}

fn material_normal_oracle(
    has_normal_map: bool,
    normal_maps_enabled: bool,
    authored: bool,
    observe_axis: bool,
) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance.request_adapter(&Default::default()).await.unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: crate::graphics_device::limits(&adapter),
                ..Default::default()
            })
            .await
            .unwrap();
        eprintln!("normal-map oracle adapter: {:?}", adapter.get_info());
        let mut asset = plane_asset(has_normal_map);
        let world = if authored {
            Mat4::from_cols_array(&[
                -2., 1., 0.4, 0., 1., 3., 0.2, 0., 0., 0.5, 0.5, 0., 0., 0., 0., 1.,
            ])
        } else {
            Mat4::IDENTITY
        };
        if authored {
            asset.materials[0].anisotropy_strength = 0.6;
            asset.materials[0].anisotropy_rotation = std::f32::consts::FRAC_PI_4;
            for (i, vertex) in asset.meshes[0].vertices.iter_mut().enumerate() {
                let direction = if (i / 4) % 2 == 0 { 1. } else { -1. };
                vertex.tangent = [direction, 0., 0., direction];
            }
        }
        let object = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("independent affine normal fixture"),
            contents: bytemuck::bytes_of(&ObjectUniform {
                model: world.to_cols_array_2d(),
                ..bytemuck::Zeroable::zeroed()
            }),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("authored normal fixture plane vertices"),
            contents: bytemuck::cast_slice(&asset.meshes[0].vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("authored normal fixture plane indices"),
            contents: bytemuck::cast_slice(&asset.meshes[0].indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let material = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("normal-map material"),
            contents: bytemuck::bytes_of(&MaterialUniform::new(
                &crate::SurfaceMaterial {
                    base: [1.; 4],
                    emission: [0.; 3],
                    metallic: 0.,
                    roughness: 0.5,
                    clearcoat: 0.,
                    coat_roughness: 0.,
                    normal_scale: 1.,
                    bump_scale: 1.,
                    anisotropy_strength: if authored { 0.6 } else { 0. },
                    anisotropy_rotation: std::f32::consts::FRAC_PI_4,
                    environment_scale: 1.,
                    visibility_group: 0,
                    unlit: false,
                    double_sided: true,
                    alpha: crate::AlphaMode::Opaque,
                },
                MaterialMaps::new(has_normal_map, true, false),
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let normal_map = texture(&device, &queue, &asset.images[0].texels(), false);
        let bump_map = texture(&device, &queue, &asset.images[1].texels(), false);
        let constants = [
            ("normal_maps_enabled", f64::from(normal_maps_enabled)),
            ("fixture_axis", f64::from(observe_axis)),
        ];
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let raster_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("exercise production raster surface_normal"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    shading::compose(&[
                        &shading::BIND_LIT,
                        &shading::BIND_SCENE,
                        &shading::BIND_MATERIAL,
                        &shading::SURFACE_RASTER,
                    ]),
                    r#"
override fixture_axis:bool=false;
@vertex fn fixture_vs(v:Vertex)->Fragment {
 var o:Fragment;
 o.clip=vec4((v.color.x+(v.position.x-v.color.x)*v.color.y)/4.,v.position.y,0.,1.);
 let model=objects[0].model;
 o.world=(model*vec4(v.position,1.)).xyz;o.normal=object_normal(model,v.normal);o.tangent=object_tangent(model,v.tangent,v.normal);o.uv=v.uv;o.color=v.color;
 return o;
}
@fragment fn fixture_fs(i:Fragment,@builtin(front_facing) front:bool)->@location(0) vec4<f32> {
 let n=surface_normal(i,front);
 if fixture_axis {return pbr_resolve_anisotropy(n,surface_tangent_frame(i,front),material.anisotropy_strength,material.anisotropy_rotation,false,vec3(1.,.5,1.));}
 return vec4(n,select(-1.,1.,front));
}
"#
                )
                .into(),
            ),
        });
        // The fixture reads every field from a vertex buffer, unlike the
        // renderer's passes (`shading::vertex::VERTEX_LAYOUT`).
        const FIXTURE_LAYOUT: shading::vertex::VertexLayout = shading::vertex::vertex_layout!(
            Vertex,
            [
                position,
                normal,
                uv,
                color,
                lightmap_uv,
                lightmap_bounds,
                tangent,
            ]
        );
        let raster = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("known glTF normal plane raster"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &raster_shader,
                entry_point: Some("fixture_vs"),
                compilation_options: Default::default(),
                buffers: &[FIXTURE_LAYOUT.buffer],
            },
            fragment: Some(wgpu::FragmentState {
                module: &raster_shader,
                entry_point: Some("fixture_fs"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // Material sampling reads the view's texture mip bias (zero here).
        let frame = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("zero view"),
            contents: bytemuck::bytes_of(
                &<shading::uniforms::ViewUniform as bytemuck::Zeroable>::zeroed(),
            ),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("zero frame"),
            layout: &raster.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame.as_entire_binding(),
            }],
        });
        let object_group = group(&device, &raster.get_bind_group_layout(1), &object);
        let raster_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("real normal texture and material"),
            layout: &raster.get_bind_group_layout(2),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: material.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&normal_map),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&bump_map),
                },
            ],
        });
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("world normals from actual raster derivatives"),
            size: wgpu::Extent3d {
                width: 128,
                height: 32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&Default::default());
        let mut scene = Fixture::new(&device, &queue, &[&asset]);
        let ray_values: Vec<[f32; 8]> = CENTERS
            .into_iter()
            .enumerate()
            .map(|(case, x)| {
                let side = if case < 2 { 1. } else { -1. };
                let center = world.transform_point3(Vec3::new(x, 0., 0.));
                let normal = if authored {
                    Vec3::new(1., -0.8, 7.).normalize()
                } else {
                    Vec3::Z
                };
                let origin = center + normal * side;
                [
                    origin.x,
                    origin.y,
                    origin.z,
                    0.001,
                    -normal.x * side,
                    -normal.y * side,
                    -normal.z * side,
                    2.,
                ]
            })
            .collect();
        let rays = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("front/back rays through plane centers"),
            contents: bytemuck::cast_slice(&ray_values),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let hits = buffer(
            "real secondary intersections",
            rays.size(),
            wgpu::BufferUsages::STORAGE,
        );
        let normals = buffer(
            "secondary world normals",
            64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = buffer(
            "secondary normal readback",
            64,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let bindings = scene.query.ray_bind_group(&device, &rays, &hits);
        let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("exercise production ray_normal after actual query"),
            source: wgpu::ShaderSource::Wgsl(format!("{}\n{}", crate::shading::lit_compute_library(), r#"
@group(3) @binding(0) var<storage,read> fixture_rays:array<SceneRay>;
@group(3) @binding(1) var<storage,read> fixture_hits:array<RawSceneHit>;
@group(3) @binding(2) var<storage,read_write> fixture_normals:array<vec4<f32>>;
override fixture_axis:bool=false;
@compute @workgroup_size(4) fn fixture_compute(@builtin(global_invocation_id) id:vec3<u32>) {
 let ray=fixture_rays[id.x];let hit=scene_decode_hit(fixture_hits[id.x],ray.origin.xyz,ray.direction.xyz);
 let m=scene_material(hit.material_word);let n=ray_normal(hit,m);
 if fixture_axis {fixture_normals[id.x]=pbr_resolve_anisotropy(n,ray_tangent_frame(hit),m.values.anisotropy_strength,m.values.anisotropy_rotation,false,vec3(1.,.5,1.));}
 else {fixture_normals[id.x]=vec4(n,select(-1.,1.,hit.front_face));}
}
"#).into()),
        });
        let io_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("secondary normal observation"),
            entries: &[0, 1, 2].map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: binding != 2,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }),
        });
        let compute = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("real secondary normal observation"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[
                        None,
                        Some(&shading::bind::scene(&device)),
                        None,
                        Some(&io_layout),
                    ],
                    immediate_size: 0,
                }),
            ),
            module: &compute_shader,
            entry_point: Some("fixture_compute"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                ..Default::default()
            },
            cache: None,
        });
        let io = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &io_layout,
            entries: &[&rays, &hits, &normals]
                .into_iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: b.as_entire_binding(),
                })
                .collect::<Vec<_>>(),
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        scene.update(
            &device,
            &queue,
            &[Pose {
                model: 0,
                world,
                id: 0,
            }],
        );
        let scene_group = scene.scene_group(&device);
        scene.query.trace(&mut encoder, &scene_group, &bindings, 4);
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&compute);
            pass.set_bind_group(1, &scene_group, &[]);
            pass.set_bind_group(3, &io, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("known normal-map plane views"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&raster);
            pass.set_bind_group(0, &frame_group, &[]);
            pass.set_bind_group(1, &object_group, &[]);
            pass.set_bind_group(2, &raster_group, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..24, 0, 0..1);
        }
        encoder.copy_buffer_to_buffer(&normals, 0, &readback, 0, 64);
        queue.submit([encoder.finish()]);
        let pixels = crate::test_support::read(&device, &queue, &output, 16);
        let (send, recv) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |result| {
            send.send(result).unwrap()
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        recv.recv().unwrap().unwrap();
        let mapped = readback.get_mapped_range(..);
        let secondary: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
        let raster_pixels: &[[f32; 4]] = bytemuck::cast_slice(&pixels);
        let mut failures = Vec::new();
        for case in 0..4 {
            // Khronos texture channel mapping plus the independently authored
            // plane axes above. No ray/shader frame math is copied into this oracle.
            let front_normal = |texels_per_step: f32| {
                if !has_normal_map {
                    // Height rises 5/255 and 10/255 per texel along U and V, so
                    // the front normal is proportional to (-dh/du, -dh/dv, 1)
                    // over one step. Mirroring U reverses the authored X slope.
                    let x = 5. * texels_per_step;
                    Vec3::new(
                        if case % 2 == 0 { -x } else { x },
                        -10. * texels_per_step,
                        255.,
                    )
                    .normalize()
                } else if normal_maps_enabled {
                    Vec3::new(if case % 2 == 0 { 153. } else { -153. }, 51., 205.).normalize()
                } else {
                    Vec3::Z
                }
            };
            let side = if case < 2 { 1. } else { -1. };
            // Secondary rays take the physical height gradient per metre: 16
            // texels across each 1.5 m plane axis. Raster follows Three r185's
            // BumpMapNode, whose normalized positional derivatives take the
            // gradient per pixel: 16 texels across the 24 pixels each plane
            // covers (a quarter of 128 columns wide at 1.5/4 of clip X, and
            // 1.5/2 of 32 rows tall).
            for (path, texels_per_step) in [("raster", 16. / 24.), ("secondary", 16. / 1.5)] {
                let expected = if authored {
                    // Independent world-space surface plane: transformed authored
                    // edges (-2,1,.4) and (1,3,.2) give the authored normal below.
                    let n = Vec3::new(1., -0.8, 7.).normalize();
                    let t =
                        Vec3::new(-2., 1., 0.4).normalize() * if case % 2 == 0 { 1. } else { -1. };
                    let b = n.cross(t) * if case % 2 == 0 { -1. } else { 1. };
                    let mapped = (t * 153. + b * 51. + n * 205.).normalize() * side;
                    if observe_axis {
                        let direction = (t + b) * side;
                        (direction - mapped * direction.dot(mapped)).normalize()
                    } else {
                        mapped
                    }
                } else {
                    front_normal(texels_per_step) * side
                };
                let actual = if path == "raster" {
                    raster_pixels[16 * 128 + 16 + case * 32]
                } else {
                    secondary[case]
                };
                {
                    let normal = Vec3::from_array(actual[..3].try_into().unwrap());
                    eprintln!(
                        "case={case} {path}: {normal:?}, expected {expected:?}, facing={}",
                        actual[3]
                    );
                    // R8 linear-filter precision is amplified by the raster's
                    // finite differences. Keep the exact affine-height oracle;
                    // allow its observed quantization, far below a reversed slope.
                    let tolerance = if !has_normal_map && path == "raster" {
                        0.006
                    } else {
                        0.002
                    };
                    if normal.distance(expected) > tolerance
                        || (actual[3] - if observe_axis { 0.6 } else { side }).abs() > 1e-6
                    {
                        failures.push(format!("case {case} {path}: {normal:?} != {expected:?}"));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    });
}
