//! Shared GPU readback and caller-generated geometry for renderer fixtures.
pub(crate) fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    bpp: u32,
) -> Vec<u8> {
    let size = texture.size();
    let row = (size.width * bpp).div_ceil(256) * 256;
    // A 3D texture's slices, and a 2D array's layers, follow one another.
    let slices = match texture.dimension() {
        wgpu::TextureDimension::D1 => 1,
        _ => size.depth_or_array_layers,
    };
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("frame evidence"),
        size: u64::from(row * size.height * slices),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        tx.send(r).unwrap();
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.get_mapped_range(..).unwrap();
    let mut pixels = Vec::new();
    for line in mapped.chunks(row as usize) {
        pixels.extend_from_slice(&line[..(size.width * bpp) as usize]);
    }
    pixels
}
/// The words of `buffer`, which can be copied from.
pub(crate) fn read_words(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
) -> Vec<u32> {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("buffer readback"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &readback, 0, buffer.size());
    queue.submit([encoder.finish()]);
    readback.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    bytemuck::cast_slice(&readback.get_mapped_range(..).unwrap()).to_vec()
}

/// A unit vector's octahedral coordinates (Cigolle et al. 2014, "A Survey of
/// Efficient Representations for Independent Unit Vectors", section 3.1),
/// signed, as the G-buffer stores normals.
pub(crate) fn octahedral(v: glam::Vec3) -> [f32; 2] {
    let n = v / v.abs().element_sum();
    if n.z >= 0. {
        [n.x, n.y]
    } else {
        [
            (1. - n.y.abs()) * n.x.signum(),
            (1. - n.x.abs()) * n.y.signum(),
        ]
    }
}

/// Runs `observation` in compute under the frame's ray-hit lit group 0 and
/// the scene's group 1, and reads back its `words` vec4 outputs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn observe_ray_hits(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut crate::Renderer,
    scene: &mut crate::Scene,
    input: &crate::FrameInput,
    settings: &crate::settings::Settings,
    observation: &str,
    outputs: usize,
) -> Vec<[f32; 4]> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ray hit observation"),
        source: wgpu::ShaderSource::Wgsl(
            format!("{}\n{}", crate::shading::lit_compute_library(), observation).into(),
        ),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[
                    Some(renderer.test_lit_layout()),
                    Some(scene.scene_layout()),
                    None,
                    Some(&layout),
                ],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bytes = (outputs * 16) as u64;
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    renderer.prepare_test_frame(device, queue, scene, input, settings);
    scene.update_rays(device, queue, input.visibility_mask, false);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, renderer.test_ray_hit_lit(), &[]);
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    queue.submit([encoder.finish()]);
    scene.finish_frame();
    let words = read_words(device, queue, &output);
    bytemuck::cast_slice::<u32, [f32; 4]>(&words).to_vec()
}

/// Runs `observation`, WGSL after the surface library that reads its cases
/// at group 0 binding 3 and writes its `outputs` vectors at binding 4, from
/// its entry point `observe` over `workgroups` workgroups, under lit group
/// 0's `scene` lights, lookup tables and environment sampler, a zeroed frame
/// and no shadows, and reads the vectors back.
pub(crate) fn observe_surface(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &crate::Scene,
    cases: &[u8],
    observation: &str,
    workgroups: u32,
    outputs: usize,
) -> Vec<[f32; 4]> {
    use crate::shading::bind::group0;
    let source = format!(
        "{}\n{}",
        crate::shading::compose(&[
            &crate::shading::BIND_LIT,
            &crate::shading::SURFACE,
            &crate::shading::SHADOW_MASK_NONE,
            &crate::shading::tiers::LIT_BASIC,
        ]),
        observation
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("surface observations"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let storage = |read_only| wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    };
    let array = |sample_type| wgpu::BindingType::Texture {
        sample_type,
        view_dimension: wgpu::TextureViewDimension::D2Array,
        multisampled: false,
    };
    // Every binding an observation may read, whether or not it does.
    let entries = [
        (3, storage(true)),
        (4, storage(false)),
        (group0::LIGHTS, storage(true)),
        (group0::LOCAL_SHADOWS, storage(true)),
        (
            group0::FRAME,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
        ),
        (
            group0::LOOKUP_TABLES,
            array(wgpu::TextureSampleType::Float { filterable: true }),
        ),
        (
            group0::ENVIRONMENT_SAMPLER,
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        ),
        (
            group0::LOCAL_SHADOW_ATLAS,
            array(wgpu::TextureSampleType::Depth),
        ),
        (
            group0::SHADOW_SAMPLER,
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
        ),
    ];
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("surface observations"),
        entries: &entries.map(|(binding, ty)| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        }),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let input = crate::scene::buffer(device, "observed cases", cases, wgpu::BufferUsages::STORAGE);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (outputs * 16) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let shadows = crate::scene::buffer(
        device,
        "no local shadows",
        bytemuck::bytes_of(&crate::shading::lights::LocalShadowRecord::NONE),
        wgpu::BufferUsages::STORAGE,
    );
    let frame = crate::scene::buffer(
        device,
        "zero frame",
        bytemuck::bytes_of(
            &<crate::shading::uniforms::FrameUniform as bytemuck::Zeroable>::zeroed(),
        ),
        wgpu::BufferUsages::UNIFORM,
    );
    let atlas = device
        .create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
    let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        compare: Some(wgpu::CompareFunction::GreaterEqual),
        ..Default::default()
    });
    let resources = [
        input.as_entire_binding(),
        output.as_entire_binding(),
        scene.lights.buffer().as_entire_binding(),
        shadows.as_entire_binding(),
        frame.as_entire_binding(),
        wgpu::BindingResource::TextureView(&scene.lookup_tables),
        wgpu::BindingResource::Sampler(&scene.environments.sampler),
        wgpu::BindingResource::TextureView(&atlas),
        wgpu::BindingResource::Sampler(&shadow_sampler),
    ];
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &entries
            .iter()
            .zip(resources)
            .map(|(&(binding, _), resource)| wgpu::BindGroupEntry { binding, resource })
            .collect::<Vec<_>>(),
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
    queue.submit([encoder.finish()]);
    bytemuck::cast_slice::<u32, [f32; 4]>(&read_words(device, queue, &output)).to_vec()
}

/// The directional albedo seen at cosine `nv` of GGX at perceptual roughness
/// `rough` with height-correlated Smith visibility and Schlick's Fresnel,
/// `f0` at normal and `f90` at grazing incidence: the single scattering the
/// split sum stands for, integrated in f64 by importance sampling the GGX
/// distribution of normals over a 2^16-point Hammersley set (within 1e-5 of
/// a 2^20-point one).
pub(crate) fn ggx_albedo(nv: f64, rough: f64, f0: f64, f90: f64) -> f64 {
    use std::f64::consts::PI;
    const SAMPLES: u32 = 1 << 16;
    let a2 = rough.powi(4);
    let view = glam::DVec3::new((1. - nv * nv).max(0.).sqrt(), 0., nv);
    let mut sum = 0.;
    for i in 0..SAMPLES {
        let u = (i as f64 + 0.5) / SAMPLES as f64;
        let v = i.reverse_bits() as f64 / 2f64.powi(32);
        let cos_theta = ((1. - v) / (1. + (a2 - 1.) * v)).sqrt();
        let sin_theta = (1. - cos_theta * cos_theta).sqrt();
        let phi = 2. * PI * u;
        let h = glam::DVec3::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta);
        let vh = view.dot(h);
        let l = h * 2. * vh - view;
        if l.z <= 0. || vh <= 0. {
            continue;
        }
        let nl = l.z;
        let visibility =
            0.5 / (nl * (nv * nv * (1. - a2) + a2).sqrt() + nv * (nl * nl * (1. - a2) + a2).sqrt());
        let fresnel = f0 + (f90 - f0) * (1. - vh).powi(5);
        // f N.L / pdf, the pdf of l being D (n.h) / (4 v.h).
        sum += fresnel * visibility * 4. * vh * nl / h.z;
    }
    sum / SAMPLES as f64
}

/// The words of `buffer`, a storage buffer that cannot be copied from, as
/// a compute pass reads them.
pub(crate) fn storage_words(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
) -> Vec<u32> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("storage readback"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@group(0) @binding(0) var<storage,read> source:array<u32>;
@group(0) @binding(1) var<storage,read_write> copy:array<u32>;
@compute @workgroup_size(64) fn copy_words(@builtin(global_invocation_id) id:vec3<u32>) {
 let index=id.x+id.y*65535u*64u;
 if index<arrayLength(&source) {
  copy[index]=source[index];
 }
}
"#
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("storage readback"),
        layout: None,
        module: &module,
        entry_point: Some("copy_words"),
        compilation_options: Default::default(),
        cache: None,
    });
    let copy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("storage readback"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: copy.as_entire_binding(),
            },
        ],
    });
    let words = (buffer.size() / 4) as u32;
    let groups = words.div_ceil(64);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(groups.min(65535), groups.div_ceil(65535), 1);
    }
    queue.submit([encoder.finish()]);
    read_words(device, queue, &copy)
}

#[path = "../examples/support/ktx2_writer.rs"]
mod ktx2_writer;
pub(crate) use ktx2_writer::{bc7_mode_6, ktx2};

/// Binary16 of a nonnegative finite `value` below 65536, truncated; values
/// below binary16's normal range become zero.
pub(crate) fn to_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    if exponent <= 0 {
        return 0;
    }
    ((exponent as u16) << 10) | ((bits >> 13) & 0x3ff) as u16
}

/// A sampled RGBA16F texture of `size` holding `texels`, rows top to bottom.
pub(crate) fn hdr_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    texels: &[[f32; 4]],
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fixture HDR input"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: crate::shading::gbuffer::COLOR,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let halves: Vec<u16> = texels.iter().flatten().map(|&v| to_half(v)).collect();
    queue.write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&halves),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * 8),
            rows_per_image: Some(size[1]),
        },
        texture.size(),
    );
    texture.create_view(&Default::default())
}

pub(crate) fn half(bytes: &[u8]) -> f32 {
    let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = ((bits >> 10) & 31) as i32;
    let fraction = (bits & 1023) as f32;
    sign * if exponent == 0 {
        fraction * 2f32.powi(-24)
    } else if exponent == 31 {
        if fraction == 0. {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1. + fraction / 1024.) * 2f32.powi(exponent - 15)
    }
}

/// Unit cube geometry owned by tests, independent of any consuming game's asset.
pub(crate) fn cube() -> crate::asset::Asset {
    use crate::asset::{Asset, CpuMesh, Material, Vertex};
    use glam::Vec3;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (axis, u, v) in [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::Z, Vec3::X, Vec3::Y),
    ] {
        for sign in [-1., 1.] {
            let start = vertices.len() as u32;
            for (x, y) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)] {
                vertices.push(Vertex {
                    tangent: [0.0; 4],
                    lightmap_bounds: [0., 0., 1., 1.],
                    lightmap_uv: [0.; 2],
                    position: (axis * sign * 0.5 + u * x * 0.5 + v * y * sign * 0.5).to_array(),
                    normal: (axis * sign).to_array(),
                    uv: [(x + 1.) * 0.5, (y + 1.) * 0.5],
                    color: [1.; 4],
                });
            }
            indices.extend([0, 1, 2, 0, 2, 3].map(|index| start + index));
        }
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
            name: "fixture material".into(),
            visibility_group: 0,
            casts_directional_shadow: true,
            base: [0.78, 0.82, 0.85, 1.],
            emissive: [0.; 3],
            metallic: 0.55,
            roughness: 0.35,
            ior: 1.5,
            specular: 1.,
            specular_color: [1.; 3],
            clearcoat: 0.,
            coat_roughness: 0.,
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
            base_texture: None,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.,
            emissive_texture: None,
            normal_texture: None,
            normal_scale: 1.,
            normal_layers: None,
            bump_texture: None,
            bump_scale: 0.,
            transmission: 0.,
            transmission_texture: None,
            thickness: 0.,
            thickness_texture: None,
            attenuation_distance: f32::INFINITY,
            attenuation_color: [1.; 3],
            dispersion: 0.,
            wrap: [gltf::texture::WrappingMode::Repeat; 2],
            double_sided: true,
            unlit: false,
            emits_into_gi: true,
            alpha: crate::AlphaMode::Opaque,
            shader: None,
        }],
        images: vec![],
        rig: Default::default(),
        ignored: Vec::new(),
    }
}

/// A mesh of one leaf of camera culling's range hierarchy at each of
/// `centres`, in order (`scene::mesh_ranges`): a leaf's worth of small
/// triangles about each centre, so culling keeps or drops each leaf whole.
pub(crate) fn leaf_clusters(centres: &[glam::Vec3]) -> crate::asset::CpuMesh {
    use crate::asset::{CpuMesh, Vertex};
    let mut mesh = CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material: 0,
        deformation: Default::default(),
    };
    for centre in centres {
        for _ in 0..crate::scene::mesh_ranges::INDICES_PER_LEAF / 3 {
            for offset in [[-0.1, -0.1], [0.1, -0.1], [0., 0.1]] {
                mesh.indices.push(mesh.vertices.len() as u32);
                mesh.vertices.push(Vertex {
                    tangent: [1., 0., 0., 1.],
                    lightmap_bounds: [0., 0., 1., 1.],
                    lightmap_uv: [0.; 2],
                    position: (*centre + glam::Vec3::new(offset[0], offset[1], 0.)).to_array(),
                    normal: [0., 0., 1.],
                    uv: [0.; 2],
                    color: [1.; 4],
                });
            }
        }
    }
    mesh
}

/// A white 16x16 base map cut out (alpha 0) over its left half, u < 0.5,
/// and opaque over its right half.
pub(crate) fn half_cut_out() -> image::RgbaImage {
    image::RgbaImage::from_fn(16, 16, |x, _| {
        image::Rgba([255, 255, 255, if x < 8 { 0 } else { 255 }])
    })
}

/// `asset` with its first material masked at `cutoff` over the base map
/// `half_cut_out`, which its meshes' UVs map.
pub(crate) fn masked(mut asset: crate::asset::Asset, cutoff: f32) -> crate::asset::Asset {
    asset.images = vec![crate::asset::Image::Rgba8(half_cut_out())];
    asset.materials[0].base_texture = Some(0);
    asset.materials[0].alpha = crate::AlphaMode::Mask { cutoff };
    asset
}

/// A uniform environment: a 4x2 panorama of `panorama` and a 336x64 PMREM
/// atlas (16-texel cube faces) whose RGBA16F texels repeat `rgba16`'s bytes.
pub(crate) fn environment(panorama: [u8; 4], rgba16: &[u8]) -> crate::environment::EnvironmentMap {
    crate::environment::EnvironmentMap {
        panorama: image::RgbaImage::from_pixel(4, 2, image::Rgba(panorama)),
        filtered: crate::environment::PmremAtlas {
            width: 336,
            height: 64,
            rgba16: rgba16.iter().copied().cycle().take(336 * 64 * 8).collect(),
        },
    }
}

/// `asset` added whole and placed at the identity pose as a static instance
/// every view shows.
pub(crate) fn add_static(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut crate::Scene,
    asset: crate::asset::Asset,
) -> (crate::AssetIds, crate::InstanceId) {
    let ids = scene.add_asset(device, queue, asset).unwrap();
    let instance = scene
        .add_instance(
            device,
            queue,
            crate::InstanceState {
                model: ids.model,
                pose: glam::Mat4::IDENTITY,
                visible: true,
                capture_visible: true,
            },
            crate::Mobility::Static,
        )
        .unwrap();
    (ids, instance)
}

/// A device with the renderer's limits and optional features, or `None`
/// after printing why. `SGL_REQUIRE_GPU` turns the skip into a failure.
pub(crate) fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    device_without(wgpu::Features::empty())
}

/// `device`, without `features` of the renderer's optional ones.
pub(crate) fn device_without(features: wgpu::Features) -> Option<(wgpu::Device, wgpu::Queue)> {
    device_choosing(|adapter| crate::graphics_device::features(adapter) - features)
}

/// `device`, with FSR2's features where the adapter has them.
pub(crate) fn fsr2_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    device_choosing(|adapter| {
        crate::graphics_device::features(adapter) | crate::graphics_device::fsr2_features(adapter)
    })
}

/// A device that traces rays in hardware, with the renderer's limits as
/// `limits` changes them and its optional features, or `None` after printing
/// that the adapter has no ray queries: a test then reports itself
/// unsupported, never passed.
#[allow(unsafe_code)]
pub(crate) fn ray_tracing_device(
    limits: impl FnOnce(wgpu::Limits) -> wgpu::Limits,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = adapter()?;
    let ray_tracing = crate::graphics_device::ray_tracing_features(&adapter);
    if ray_tracing.is_empty() {
        eprintln!("unsupported: the adapter has no hardware ray queries");
        return None;
    }
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: (adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC)
                | crate::graphics_device::features(&adapter)
                | ray_tracing,
            required_limits: limits(crate::graphics_device::limits(&adapter)),
            // SAFETY: the tests accept wgpu's experimental ray queries.
            experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() },
            ..Default::default()
        }))
        .unwrap(),
    )
}

/// A device with the renderer's limits and the optional features `features`
/// chooses for the adapter, or `None` after printing why.
pub(crate) fn device_choosing(
    features: impl FnOnce(&wgpu::Adapter) -> wgpu::Features,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = adapter()?;
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: (adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC)
                | features(&adapter),
            required_limits: crate::graphics_device::limits(&adapter),
            ..Default::default()
        }))
        .unwrap(),
    )
}

/// The default adapter, for a test that requests its own devices, or `None`
/// after printing why. `SGL_REQUIRE_GPU` turns the skip into a failure.
pub(crate) fn adapter() -> Option<wgpu::Adapter> {
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    match pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())) {
        Ok(adapter) => Some(adapter),
        Err(error) => {
            assert!(
                !required,
                "GPU test cannot run but SGL_REQUIRE_GPU is set: {error}"
            );
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

/// A game's shader for fixtures (`Scene::add_shader`): it moves every vertex
/// along its parameters' `direction` by their `lift`, plus `amplitude` times
/// the sine of `frequency` times the frame's time, plus the vertex's shader
/// data's x and the instance's; its surface function cuts out the fragments
/// at negative world x where `cutting` is positive, and adds the scene
/// depth behind the fragment and whether it is available to its emission's
/// red and green.
pub(crate) const TEST_SHADER: &str = r#"
struct ShaderParams {
 direction:vec3<f32>,
 lift:f32,
 amplitude:f32,
 frequency:f32,
 cutting:f32,
 unused:f32,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 let along=params.lift+params.amplitude*sin(params.frequency*ctx.time)+v.shader_data.x+ctx.instance.x;
 out.position+=params.direction*along;
 return out;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 out.base_color.a=select(s.base_color.a,0.,params.cutting>0. && ctx.position.x<0.);
 out.emission=s.emission+vec3(scene_depth_behind(ctx),select(0.,1.,scene_depth_available()),0.);
 return out;
}
"#;

/// `TEST_SHADER`'s `ShaderParams`, mirrored.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct TestShaderParams {
    pub direction: [f32; 3],
    pub lift: f32,
    pub amplitude: f32,
    pub frequency: f32,
    pub cutting: f32,
    pub unused: f32,
}

/// `TEST_SHADER` added to `scene`.
pub(crate) fn add_test_shader(scene: &mut crate::Scene) -> crate::ShaderId {
    scene
        .add_shader(crate::ShaderSource {
            wgsl: TEST_SHADER.into(),
            label: "test shader".into(),
        })
        .unwrap_or_else(|error| panic!("{error}"))
}

/// `material` drawn through `shader`, whose vertices move at most `bound`.
pub(crate) fn shaded(
    material: crate::asset::Material,
    shader: crate::ShaderId,
    bound: f32,
) -> crate::asset::Material {
    crate::asset::Material {
        shader: Some(crate::MaterialShader {
            shader,
            displacement_bound: bound,
        }),
        ..material
    }
}

/// Sets `material`'s `TEST_SHADER` parameters.
pub(crate) fn set_test_params(
    scene: &mut crate::Scene,
    queue: &wgpu::Queue,
    material: crate::MaterialId,
    params: TestShaderParams,
) {
    scene
        .set_shader_parameters(queue, material, bytemuck::bytes_of(&params))
        .unwrap();
}
