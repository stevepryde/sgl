//! Block-compressed material images at real GPU boundaries: a KTX2 chain's
//! levels as the device samples them, and level 0 as rays decode it.
use crate::asset::{CompressedImage, CpuMesh, Image, Vertex};
use crate::scene::rays::Query;
use crate::test_support;
use crate::{AlphaMode, Scene, SceneError};
use wgpu::util::DeviceExt;

/// A device that samples BC formats, or `None` when the adapter has none.
fn bc_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let (device, queue) = test_support::device()?;
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        eprintln!("skipping: the adapter has no BC texture compression");
        return None;
    }
    Some((device, queue))
}

/// `count` BC7 blocks from a fixed seed, each of a mode 0..=7 drawn at
/// random with the rest of its bits random (every such block is valid).
fn random_bc7_blocks(count: usize, seed: &mut u64) -> Vec<u8> {
    let mut next = || {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *seed >> 11
    };
    (0..count)
        .flat_map(|_| {
            let mode = next() % 8;
            let bits = u128::from(next()) | u128::from(next()) << 53 | u128::from(next()) << 106;
            let block = (bits >> (mode + 1) << (mode + 1)) | 1 << mode;
            block.to_le_bytes()
        })
        .collect()
}

/// Level `level` of `image`, decoded on the CPU by bcdec and cropped to the
/// level's texels.
fn cpu_decode(image: &CompressedImage, level: usize) -> ([u32; 2], Vec<u8>) {
    let [width, height] = [image.width, image.height].map(|side| (side >> level).max(1));
    let blocks_wide = width.div_ceil(4) as usize;
    let padded_pitch = blocks_wide * 16;
    let mut padded = vec![0; padded_pitch * height.div_ceil(4) as usize * 4];
    for (index, block) in image.levels[level].chunks_exact(16).enumerate() {
        let at = (index / blocks_wide) * 4 * padded_pitch + (index % blocks_wide) * 16;
        bcdec_rs::bc7(block, &mut padded[at..], padded_pitch);
    }
    let texels = padded
        .chunks_exact(padded_pitch)
        .take(height as usize)
        .flat_map(|row| &row[..width as usize * 4])
        .copied()
        .collect();
    ([width, height], texels)
}

/// Every texel of `view`'s level `level` of `size` texels, as `textureLoad`
/// returns it.
fn gpu_texels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    level: u32,
    [width, height]: [u32; 2],
) -> Vec<[f32; 4]> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("compressed level readback"),
        source: wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var map:texture_2d<f32>;
@group(0) @binding(1) var<storage,read_write> texels:array<vec4<f32>>;
@group(0) @binding(2) var<uniform> pick:vec4<u32>;
@compute @workgroup_size(8,8) fn load(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=pick.y || id.y>=pick.z {return;}
 texels[id.y*pick.y+id.x]=textureLoad(map,id.xy,i32(pick.x));
}"
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("load"),
        compilation_options: Default::default(),
        cache: None,
    });
    let texels = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(width * height) * 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let pick = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&[level, width, height, 0]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: texels.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: pick.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }
    queue.submit([encoder.finish()]);
    test_support::read_words(device, queue, &texels)
        .chunks_exact(4)
        .map(|texel| std::array::from_fn(|channel| f32::from_bits(texel[channel])))
        .collect()
}

fn srgb_to_linear(byte: u8) -> f32 {
    let x = f32::from(byte) / 255.;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

// A KTX2 file of random BC7 blocks of every mode, Zstandard-supercompressed,
// with a full chain whose lower levels hold partial blocks, added to a scene
// as one material's sRGB base map, linear data map, or both. At every level
// the device samples, through each view a material uses, what bcdec decodes
// from that level's blocks.
#[test]
fn compressed_chain_samples_its_stored_levels() {
    let Some((device, queue)) = bc_device() else {
        return;
    };
    let [width, height] = [20, 12];
    let mut seed = 7;
    let levels: Vec<_> = (0..5)
        .map(|level| {
            let blocks = |side: u32| (side >> level).max(1).div_ceil(4) as usize;
            random_bc7_blocks(blocks(width) * blocks(height), &mut seed)
        })
        .collect();
    let file = test_support::ktx2(ktx2::Format::BC7_SRGB_BLOCK, [width, height], &levels, true);
    let image = CompressedImage::from_ktx2(&file).unwrap();
    assert_eq!(image.levels, levels, "the stored levels, level 0 first");
    for [color, data] in [[true, false], [false, true], [true, true]] {
        let mut material = test_support::cube().materials[0].clone();
        material.base_texture = color.then_some(0);
        material.mr_texture = data.then_some(0);
        let mut scene = Scene::new(&device, &queue);
        scene
            .add_materials(
                &device,
                &queue,
                &[material],
                &[Image::Compressed(image.clone())],
            )
            .unwrap();
        let texture = scene.materials.texture(0);
        assert_eq!(
            [texture.color.is_some(), texture.data.is_some()],
            [color, data]
        );
        for level in 0..levels.len() {
            let (size, expected) = cpu_decode(&image, level);
            if let Some(view) = &texture.data {
                let sampled: Vec<u8> = gpu_texels(&device, &queue, view, level as u32, size)
                    .iter()
                    .flatten()
                    .map(|&value| (value * 255.).round() as u8)
                    .collect();
                assert_eq!(sampled, expected, "level {level} as linear data");
            }
            let Some(view) = &texture.color else {
                continue;
            };
            let sampled = gpu_texels(&device, &queue, view, level as u32, size);
            for (texel, (sampled, bytes)) in
                sampled.iter().zip(expected.chunks_exact(4)).enumerate()
            {
                for channel in 0..4 {
                    let expected = if channel < 3 {
                        srgb_to_linear(bytes[channel])
                    } else {
                        f32::from(bytes[3]) / 255.
                    };
                    assert!(
                        (sampled[channel] - expected).abs() < 1e-3,
                        "level {level} texel {texel} channel {channel} as sRGB colour: {} vs {expected}",
                        sampled[channel]
                    );
                }
            }
        }
    }
}

// A BC7 image of a block for each mode and each value of the six bits after
// its mode bit (a partition of every mode that has them, and every rotation
// and index selection), the rest random, as one material's data map. A ray
// reads each texel of level 0 as the device samples it, from no more than
// the stored blocks: no decoded copy.
#[test]
fn rays_decode_every_bc7_mode_and_partition_as_level_0_samples() {
    let Some((device, queue)) = bc_device() else {
        return;
    };
    let [width, height] = [128, 64];
    let blocks: Vec<u8> = random_bc7_blocks((width / 4 * height / 4) as usize, &mut 11)
        .chunks_exact(16)
        .zip(0u128..)
        .flat_map(|(random, block)| {
            let mode = block % 8;
            let six_bits = block / 8 % 64;
            let random = u128::from_le_bytes(random.try_into().unwrap());
            let bits = (random >> (mode + 7) << (mode + 7)) | six_bits << (mode + 1) | 1 << mode;
            bits.to_le_bytes()
        })
        .collect();
    let image = CompressedImage {
        format: crate::asset::CompressedFormat::Bc7,
        width,
        height,
        levels: vec![blocks],
    };
    let mut material = test_support::cube().materials[0].clone();
    material.mr_texture = Some(0);
    let mut scene = Scene::new(&device, &queue);
    scene
        .add_materials(
            &device,
            &queue,
            &[material],
            &[Image::Compressed(image.clone())],
        )
        .unwrap();
    let texture = scene.materials.texture(0);
    let ray_words = (texture.ray.end - texture.ray.start) as usize;
    assert!(
        ray_words * 4 <= image.levels[0].len() + 16,
        "the ray image takes {ray_words} words for {} bytes of blocks",
        image.levels[0].len()
    );
    let sampled = gpu_texels(
        &device,
        &queue,
        texture.data.as_ref().unwrap(),
        0,
        [width, height],
    );
    let read = ray_texels(&device, &queue, &scene, texture.ray.start, [width, height]);
    let bytes = |texels: &[[f32; 4]]| -> Vec<[u8; 4]> {
        texels
            .iter()
            .map(|texel| texel.map(|value| (value * 255.).round() as u8))
            .collect()
    };
    for (texel, (read, sampled)) in bytes(&read).iter().zip(bytes(&sampled)).enumerate() {
        let [x, y] = [texel as u32 % width, texel as u32 / width];
        assert_eq!(
            *read,
            sampled,
            "texel ({x}, {y}): mode {}",
            (y / 4 * width / 4 + x / 4) % 8
        );
    }
}

/// Every texel of the image at `image_word` in `scene`'s ray source, of
/// `size` texels, as a ray reads it as data.
fn ray_texels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &Scene,
    image_word: u32,
    [width, height]: [u32; 2],
) -> Vec<[f32; 4]> {
    let library = crate::shading::compose(&[&crate::shading::SCENE_RAYS]);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ray image readback"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{library}
@group(0) @binding(0) var<storage,read_write> texels:array<vec4<f32>>;
@group(0) @binding(1) var<uniform> pick:vec4<u32>;
@compute @workgroup_size(8,8) fn read_texels(@builtin(global_invocation_id) id:vec3<u32>) {{
 if id.x>=pick.y || id.y>=pick.z {{return;}}
 texels[id.y*pick.y+id.x]=scene_texel(pick.x,vec2<i32>(id.xy),vec2(0u),false);
}}"
            )
            .into(),
        ),
    });
    let io = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&io), Some(&crate::shading::bind::scene(device))],
                immediate_size: 0,
            }),
        ),
        module: &module,
        entry_point: Some("read_texels"),
        compilation_options: Default::default(),
        cache: None,
    });
    let texels = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(width * height) * 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let pick = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&[image_word, width, height, 0]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &io,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: texels.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: pick.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }
    queue.submit([encoder.finish()]);
    test_support::read_words(device, queue, &texels)
        .chunks_exact(4)
        .map(|texel| std::array::from_fn(|channel| f32::from_bits(texel[channel])))
        .collect()
}

// The KTX2 reader refuses what it does not read, each a valid file of one
// 2D BC7 image (which it reads) changed in one header field: an array, a
// cube, generated mips, another format, another supercompression.
#[test]
fn ktx2_files_other_than_one_bc7_chain_are_refused() {
    let levels = [random_bc7_blocks(1, &mut 5)];
    let bc7 = |zstd| test_support::ktx2(ktx2::Format::BC7_UNORM_BLOCK, [4; 2], &levels, zstd);
    assert!(CompressedImage::from_ktx2(&bc7(false)).is_ok());
    assert!(CompressedImage::from_ktx2(&bc7(true)).is_ok());
    let with = |field: usize, value: u32| {
        let mut file = bc7(false);
        file[field..field + 4].copy_from_slice(&value.to_le_bytes());
        file
    };
    // Header words: layerCount at byte 32, faceCount 36, levelCount 40 and
    // supercompressionScheme 44 (KTX 2.0 section 3); ZLIB is scheme 3.
    for (case, file) in [
        ("an array", with(32, 2)),
        ("a cube", with(36, 6)),
        ("generated mips", with(40, 0)),
        ("ZLIB supercompression", with(44, 3)),
        (
            "BC6H",
            test_support::ktx2(ktx2::Format::BC6H_UFLOAT_BLOCK, [4; 2], &levels, false),
        ),
    ] {
        assert!(CompressedImage::from_ktx2(&file).is_err(), "{case}");
    }
}

// The scene refuses a chain it cannot upload: sides that are not whole
// blocks, more levels than a full chain, none, or a level of the wrong size.
#[test]
fn a_compressed_chain_the_device_cannot_upload_is_refused() {
    let Some((device, queue)) = bc_device() else {
        return;
    };
    let mut seed = 3;
    let mut chain = |[width, height]: [u32; 2], levels: usize| CompressedImage {
        format: crate::asset::CompressedFormat::Bc7,
        width,
        height,
        levels: (0..levels)
            .map(|level| {
                let blocks = |side: u32| (side >> level).max(1).div_ceil(4) as usize;
                random_bc7_blocks(blocks(width) * blocks(height), &mut seed)
            })
            .collect(),
    };
    let mut short_level = chain([8, 8], 2);
    short_level.levels[1].pop();
    let mut material = test_support::cube().materials[0].clone();
    material.base_texture = Some(0);
    let mut scene = Scene::new(&device, &queue);
    for image in [
        chain([6, 8], 1),
        chain([8, 8], 5),
        chain([8, 8], 0),
        short_level,
    ] {
        assert!(matches!(
            scene.add_materials(
                &device,
                &queue,
                &[material.clone()],
                &[Image::Compressed(image)]
            ),
            Err(SceneError::InvalidCompressedImage)
        ));
    }
    assert!(
        scene
            .add_materials(
                &device,
                &queue,
                &[material],
                &[Image::Compressed(chain([8, 4], 4))]
            )
            .is_ok(),
        "a full chain of a non-square image"
    );
}

// A masked material whose base map is a KTX2 BC7 image cut out texel by
// texel: one mode 6 block per 4x4 texels, its endpoints opaque and clear,
// each texel's index picking one. A ray at each texel's centre hits the
// quad exactly where that texel is opaque, as the portable traversal's
// acceptance predicate decodes the ray source's blocks.
#[test]
fn rays_pass_the_cut_out_texels_of_a_compressed_masked_material() {
    let Some((device, queue)) = bc_device() else {
        return;
    };
    const SIDE: u32 = 8;
    let opaque = |x: u32, y: u32| (x * 3 + y * 5) % 7 < 4;
    let blocks: Vec<u8> = (0..SIDE / 4)
        .flat_map(|block_y| (0..SIDE / 4).map(move |block_x| (block_x, block_y)))
        .flat_map(|(block_x, block_y)| {
            let texel = |i: u32| opaque(block_x * 4 + i % 4, block_y * 4 + i / 4);
            // The anchor texel takes endpoint 0.
            let first = texel(0);
            let alpha = |opaque: bool| if opaque { 127 } else { 0 };
            let indices = std::array::from_fn(|i| if texel(i as u32) == first { 0 } else { 15 });
            test_support::bc7_mode_6(
                [[100, 60, 20, alpha(first)], [100, 60, 20, alpha(!first)]],
                [u8::from(first), u8::from(!first)],
                indices,
            )
        })
        .collect();
    let file = test_support::ktx2(ktx2::Format::BC7_UNORM_BLOCK, [SIDE; 2], &[blocks], false);
    let corners = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]];
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: corners
            .map(|[x, y]| Vertex {
                position: [x, y, 0.],
                normal: [0., 0., 1.],
                uv: [x, y],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0.; 4],
                tangent: [0.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }];
    asset.images = vec![Image::Compressed(
        CompressedImage::from_ktx2(&file).unwrap(),
    )];
    asset.materials[0].base_texture = Some(0);
    asset.materials[0].alpha = AlphaMode::Mask { cutoff: 0.5 };
    asset.materials[0].double_sided = true;
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, asset);
    scene.update_rays(&device, &queue, !0);
    let rays: Vec<[f32; 8]> = (0..SIDE * SIDE)
        .map(|i| {
            let centre = |t: u32| (t as f32 + 0.5) / SIDE as f32;
            [centre(i % SIDE), centre(i / SIDE), 1., 0., 0., 0., -1., 2.]
        })
        .collect();
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&rays),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let hits = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: input.size(),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let query = Query::new(&device);
    let bindings = query.ray_bind_group(&device, &input, &hits);
    let mut encoder = device.create_command_encoder(&Default::default());
    query.trace(
        &mut encoder,
        &scene.scene_group,
        &bindings,
        rays.len() as u32,
    );
    queue.submit([encoder.finish()]);
    let words = test_support::read_words(&device, &queue, &hits);
    let hit: Vec<bool> = words.chunks_exact(8).map(|hit| hit[0] != 0).collect();
    let expected: Vec<bool> = (0..SIDE * SIDE)
        .map(|i| opaque(i % SIDE, i / SIDE))
        .collect();
    assert_eq!(hit, expected);
}
