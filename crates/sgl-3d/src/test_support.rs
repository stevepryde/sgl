//! Shared GPU readback and caller-generated geometry for renderer fixtures.
pub(crate) fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    bpp: u32,
) -> Vec<u8> {
    let size = texture.size();
    let row = (size.width * bpp).div_ceil(256) * 256;
    // A 3D texture's slices follow one another.
    let slices = match texture.dimension() {
        wgpu::TextureDimension::D3 => size.depth_or_array_layers,
        _ => 1,
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
    let mapped = buffer.get_mapped_range(..);
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
    bytemuck::cast_slice(&readback.get_mapped_range(..)).to_vec()
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
            clearcoat: 0.,
            coat_roughness: 0.,
            base_texture: None,
            mr_texture: None,
            emissive_texture: None,
            normal_texture: None,
            normal_scale: 1.,
            bump_texture: None,
            bump_scale: 0.,
            wrap: [gltf::texture::WrappingMode::Repeat; 2],
            double_sided: true,
            unlit: false,
            alpha: crate::AlphaMode::Opaque,
        }],
        images: vec![],
        rig: Default::default(),
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

/// A device with the renderer's limits and the optional features `features`
/// chooses for the adapter, or `None` after printing why.
fn device_choosing(
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
