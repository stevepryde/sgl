//! What production shading makes of a material's normal on real geometry:
//! raster's `surface_normal` (or the coat's, or the anisotropy axis) at
//! every pixel of a fixture draw, with actual raster derivatives, and
//! `ray_normal` (likewise) at actual BLAS hits. Tests supply the asset, the
//! draw's vertex stage and the rays, and judge the numbers by their own
//! oracles (`secondary_normal_tests`, `khronos_normal_tests`).
use super::tests::{Fixture, Pose};
use super::*;
use crate::asset::{Asset, Vertex};
use crate::scene::textures::upload as texture;
use crate::shading::{self, uniforms::ObjectUniform};
use glam::Mat4;
use wgpu::util::DeviceExt;

/// What a probe reads: the base normal, the anisotropy axis about it, or
/// the coat's normal. The axis is the material's for a direction texel
/// (0.5, 1, 1), which KHR_materials_anisotropy reads as the bitangent at
/// full strength (red along T, green along B).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Observe {
    Base = 0,
    Axis = 1,
    Coat = 2,
}

/// One probe: `asset`'s first mesh rasterized and every mesh traced, at
/// `world`.
pub(super) struct Probe<'a> {
    pub asset: &'a Asset,
    pub world: Mat4,
    /// Raster's material record; rays read the asset's material.
    pub material: MaterialUniform,
    /// The images raster binds as the relief and coat normal maps.
    pub relief: usize,
    pub coat: usize,
    pub normal_maps_enabled: bool,
    pub observe: Observe,
    /// WGSL defining `fixture_vs(v: FixtureVertex, @builtin(instance_index)
    /// view: u32) -> Fragment`, drawn `views` times into a target of `size`,
    /// and `fixture_front(raster: bool) -> bool`, the material's side of a
    /// face raster takes as `raster`: production's `object_front` where
    /// `fixture_vs` projects the posed positions, as every pass does, so a
    /// mirroring pose reverses raster's winding; `raster` itself where it
    /// projects the mesh's own.
    pub vertex: &'a str,
    pub views: u32,
    pub size: [u32; 2],
    /// Each ray: origin, minimum distance, direction, maximum distance.
    pub rays: &'a [[f32; 8]],
}

/// Each pixel's and each ray's observation: the vector, then +1 on a front
/// face and -1 on a back (the anisotropy axis: its strength).
pub(super) struct Observed {
    pub raster: Vec<[f32; 4]>,
    pub rays: Vec<[f32; 4]>,
}

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

pub(super) fn observe(device: &wgpu::Device, queue: &wgpu::Queue, probe: &Probe) -> Observed {
    let asset = probe.asset;
    let object = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("normal probe object"),
        contents: bytemuck::bytes_of(&ObjectUniform {
            model: probe.world.to_cols_array_2d(),
            ..bytemuck::Zeroable::zeroed()
        }),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("normal probe vertices"),
        contents: bytemuck::cast_slice(&asset.meshes[0].vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("normal probe indices"),
        contents: bytemuck::cast_slice(&asset.meshes[0].indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let material = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("normal probe material"),
        contents: bytemuck::bytes_of(&probe.material),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let relief_map = texture(
        device,
        queue,
        &asset.images[probe.relief].texels().unwrap(),
        false,
    );
    let coat_normal_map = texture(
        device,
        queue,
        &asset.images[probe.coat].texels().unwrap(),
        false,
    );
    let constants = [
        ("normal_maps_enabled", f64::from(probe.normal_maps_enabled)),
        ("fixture_observe", probe.observe as u32 as f64),
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
                "{}\n{}\n{}",
                shading::compose(&[
                    &shading::BIND_LIT,
                    &shading::BIND_SCENE,
                    &shading::BIND_MATERIAL,
                    &shading::SURFACE_RASTER,
                    &shading::SHADOW_MASK_NONE,
                    &shading::CONTACT_SHADOW_NONE,
                    &shading::tiers::MATERIAL_MAPS_EXTENDED,
                    &shading::tiers::MATERIAL_OCCLUSION_SPECULAR_NONE,
                    &shading::tiers::LIT_BASIC,
                    &shading::shader::SHADER_DEFAULT,
                    &shading::shader::SHADER_INPUTS_NONE,
                    &shading::shader::SHADER_SCENE_DEPTH_NONE,
                ]),
                r#"
override fixture_observe:u32=0u;
struct FixtureVertex {
 @location(0) position:vec3<f32>,
 @location(1) normal:vec3<f32>,
 @location(2) uv:vec2<f32>,
 @location(3) color:vec4<f32>,
 @location(4) lightmap_uv:vec2<f32>,
 @location(5) lightmap_bounds:vec4<f32>,
 @location(6) tangent:vec4<f32>,
}
@fragment fn fixture_fs(i:Fragment,@builtin(front_facing) raster_front:bool)->@location(0) vec4<f32> {
 let front=fixture_front(raster_front);
 if fixture_observe==2u {return vec4(surface_coat_normal(i,front),select(-1.,1.,front));}
 let n=surface_normal(i,front);
 if fixture_observe==1u {return pbr_resolve_anisotropy(n,surface_tangent_frame(i,front),material.anisotropy_strength,material.anisotropy_rotation,true,vec3(.5,1.,1.));}
 return vec4(n,select(-1.,1.,front));
}
"#,
                probe.vertex
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
        label: Some("normal probe raster"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &raster_shader,
            entry_point: Some("fixture_vs"),
            compilation_options: Default::default(),
            buffers: &[Some(FIXTURE_LAYOUT.buffer)],
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
    // Material sampling reads the view's texture mip bias (zero here),
    // and normal layers the frame's animation phase (none here).
    let view = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("zero view"),
        contents: bytemuck::bytes_of(
            &<shading::uniforms::ViewUniform as bytemuck::Zeroable>::zeroed(),
        ),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let frame = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("zero frame"),
        contents: bytemuck::bytes_of(
            &<shading::uniforms::FrameUniform as bytemuck::Zeroable>::zeroed(),
        ),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("zero view and frame"),
        layout: &raster.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: view.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: frame.as_entire_binding(),
            },
        ],
    });
    // Ray hits read the frame alone.
    let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("zero frame"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let ray_frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("zero frame"),
        layout: &frame_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 1,
            resource: frame.as_entire_binding(),
        }],
    });
    let object_group = group(device, &raster.get_bind_group_layout(1), &object);
    // Every map binding on the first UV set, untransformed; the vertex
    // frames where every mesh has one, as the scene keeps them.
    let vertex_frames = asset
        .meshes
        .iter()
        .all(|mesh| crate::asset::tangent_frames(&mesh.vertices));
    let bindings = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("material bindings"),
        contents: bytemuck::bytes_of(&shading::map_uvs::MaterialBindings {
            flags: if vertex_frames {
                shading::map_uvs::MATERIAL_BINDING_VERTEX_FRAMES
            } else {
                0
            },
            more_maps: 0,
            padding: [0; 2],
            map_uvs: *shading::map_uvs::MapUvs::default().rows(),
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });
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
                resource: wgpu::BindingResource::TextureView(&relief_map),
            },
            wgpu::BindGroupEntry {
                binding: shading::bind::group2::MATERIAL_BINDINGS,
                resource: bindings.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: shading::bind::group2::COAT_NORMAL_MAP,
                resource: wgpu::BindingResource::TextureView(&coat_normal_map),
            },
        ],
    });
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("world normals from actual raster derivatives"),
        size: wgpu::Extent3d {
            width: probe.size[0],
            height: probe.size[1],
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
    let mut scene = Fixture::new(device, queue, &[asset]);
    let ray_count = probe.rays.len() as u32;
    let rays = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("normal probe rays"),
        contents: bytemuck::cast_slice(probe.rays),
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
    let normals_size = 16 * u64::from(ray_count);
    let normals = buffer(
        "secondary world normals",
        normals_size,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let readback = buffer(
        "secondary normal readback",
        normals_size,
        wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    let bindings = scene.query.ray_bind_group(device, &rays, &hits);
    let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("exercise production ray_normal after actual query"),
        source: wgpu::ShaderSource::Wgsl(format!("{}\n{}", crate::shading::lit_compute_library(), r#"
@group(3) @binding(0) var<storage,read> fixture_rays:array<SceneRay>;
@group(3) @binding(1) var<storage,read> fixture_hits:array<RawSceneHit>;
@group(3) @binding(2) var<storage,read_write> fixture_normals:array<vec4<f32>>;
override fixture_observe:u32=0u;
@compute @workgroup_size(64) fn fixture_compute(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&fixture_rays) {return;}
 let ray=fixture_rays[id.x];let hit=scene_decode_hit(fixture_hits[id.x],ray.origin.xyz,ray.direction.xyz);
 let m=scene_material(hit.material_word);let n=ray_normal(hit,m);
 if fixture_observe==1u {fixture_normals[id.x]=pbr_resolve_anisotropy(n,ray_tangent_frame(hit),m.values.anisotropy_strength,m.values.anisotropy_rotation,true,vec3(.5,1.,1.));}
 else if fixture_observe==2u {fixture_normals[id.x]=vec4(ray_coat_normal(hit,m),select(-1.,1.,hit.front_face));}
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
                    Some(&frame_layout),
                    Some(&shading::bind::scene(device)),
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
        device,
        queue,
        &[Pose {
            model: 0,
            world: probe.world,
            id: 0,
        }],
    );
    let scene_group = scene.scene_group(device);
    scene
        .query
        .trace(&mut encoder, &scene_group, &bindings, ray_count);
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&compute);
        pass.set_bind_group(0, &ray_frame_group, &[]);
        pass.set_bind_group(1, &scene_group, &[]);
        pass.set_bind_group(3, &io, &[]);
        pass.dispatch_workgroups(ray_count.div_ceil(64), 1, 1);
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("normal probe views"),
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
        pass.draw_indexed(0..asset.meshes[0].indices.len() as u32, 0, 0..probe.views);
    }
    encoder.copy_buffer_to_buffer(&normals, 0, &readback, 0, normals_size);
    queue.submit([encoder.finish()]);
    let pixels = crate::test_support::read(device, queue, &output, 16);
    let (send, recv) = std::sync::mpsc::channel();
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        send.send(result).unwrap()
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    recv.recv().unwrap().unwrap();
    let rays = bytemuck::pod_collect_to_vec(&readback.get_mapped_range(..).unwrap());
    Observed {
        raster: bytemuck::pod_collect_to_vec(&pixels),
        rays,
    }
}
