//! Numerical acceptance is declared in ambient_occlusion/README.md before execution.
/// Independent cosine-weighted hemisphere ray integral. The receiver is on
/// z=-3 with normal +Z; the only occluder is the wall x=0.6, z>=-3.
fn wall_visibility(distance: f64, radius: f64) -> f64 {
    let mut visible = 0usize;
    for radial in 0..256 {
        let r = ((radial as f64 + 0.5) / 256.0).sqrt();
        for angular in 0..256 {
            let phi = (angular as f64 + 0.5) / 256.0 * std::f64::consts::TAU;
            let ray_x = r * phi.cos();
            let hit_distance = distance / ray_x;
            if ray_x <= 0.0 || hit_distance > radius {
                visible += 1;
            }
        }
    }
    visible as f64 / (256 * 256) as f64
}

#[test]
fn ambient_occlusion_matches_independent_hemisphere_integral() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut ao = super::AmbientOcclusion::new(&device, &queue);
        for (size, wall, rotated) in [
            ([129, 113], false, false),
            ([129, 113], true, false),
            ([129, 113], true, true),
            ([17, 19], false, false),
        ] {
            let projection =
                crate::perspective(60f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1);
            let mut view: crate::shading::uniforms::ViewUniform = bytemuck::Zeroable::zeroed();
            // Rotate the camera and physical scene together by 180 degrees about Y.
            // The projected geometry is identical, but the supplied world normals change.
            view.view = if rotated {
                glam::Mat4::from_diagonal(glam::Vec4::new(-1.0, 1.0, -1.0, 1.0)).to_cols_array_2d()
            } else {
                glam::Mat4::IDENTITY.to_cols_array_2d()
            };
            view.projection = projection.to_cols_array_2d();
            let (depth, normals) =
                analytic_geometry(&device, &queue, size, projection, wall, rotated, None);
            let mut encoder = device.create_command_encoder(&Default::default());
            let result = ao.encode(
                &device,
                &queue,
                &mut encoder,
                &depth,
                &normals,
                &view,
                crate::settings::AmbientOcclusionQuality::Ultra,
                1.0,
                None,
            );
            queue.submit([encoder.finish()]);
            let data = crate::test_support::read(&device, &queue, result.texture(), 4);
            let values: Vec<_> = data
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            assert!(
                values.iter().all(|&v| v <= 255),
                "visibility escaped 8-bit range"
            );
            if !wall {
                // The whole open surface, including final row/column of odd extents,
                // is physically unoccluded; missing writes produce black, not a pass.
                let error = values.iter().map(|&v| 1.0 - v as f64 / 255.0).sum::<f64>()
                    / values.len() as f64;
                println!("XeGTAO open plane {size:?}: mean absolute error {error:.6}");
                assert!(error <= 0.025, "open plane error {error}");
                assert!(
                    values.iter().all(|&v| v >= 240),
                    "unoccluded pixels darkened or unwritten"
                );
            } else {
                let raw_bytes =
                    crate::test_support::read(&device, &queue, ao.raw().unwrap().texture(), 4);
                let raw: Vec<_> = raw_bytes
                    .chunks_exact(4)
                    .map(|b| (u32::from_le_bytes(b.try_into().unwrap()) & 255) as f64 / 255.0 * 1.5)
                    .collect();
                let mut errors = Vec::new();
                let mut raw_errors = Vec::new();
                let mut noop_errors = Vec::new();
                for y in [40, 56, 72] {
                    for x in (35..size[0] - 20).step_by(3) {
                        let px = ((x as f64 + 0.5) / size[0] as f64 * 2.0 - 1.0) * 3.0
                            / projection.x_axis.x as f64;
                        let distance = 0.6 - px;
                        if !(0.08..=0.95).contains(&distance) {
                            continue;
                        }
                        let expected = wall_visibility(distance, 1.0);
                        let measured = values[(y * size[0] + x) as usize] as f64 / 255.0;
                        errors.push((measured - expected).abs());
                        raw_errors
                            .push((raw[(y * size[0] + x) as usize].min(1.0) - expected).abs());
                        noop_errors.push(1.0 - expected);
                    }
                }
                assert!(!errors.is_empty());
                let mean = errors.iter().sum::<f64>() / errors.len() as f64;
                let maximum = errors.iter().copied().fold(0f64, f64::max);
                println!(
                    "XeGTAO wall hemisphere integral (rotated={rotated}): samples={} MAE={mean:.6} max={maximum:.6}",
                    errors.len()
                );
                let raw_mean = raw_errors.iter().sum::<f64>() / raw_errors.len() as f64;
                let raw_max = raw_errors.iter().copied().fold(0f64, f64::max);
                let noop_mean = noop_errors.iter().sum::<f64>() / noop_errors.len() as f64;
                let noop_max = noop_errors.iter().copied().fold(0f64, f64::max);
                println!(
                    "XeGTAO raw MAE={raw_mean:.6} max={raw_max:.6}; unoccluded negative control MAE={noop_mean:.6} max={noop_max:.6}"
                );
                assert!(
                    noop_mean > 0.16 && noop_max > 0.30,
                    "fixture must reject no-op against both declared budgets"
                );
                assert!(mean <= 0.16, "wall mean error {mean}");
                assert!(maximum <= 0.30, "wall maximum error {maximum}");
            }
        }
    });
}

// Defect: the denoiser loses the edges the main pass found (the working
// word drops them), so it blurs across a depth discontinuity. Oracle: a plate facing the camera 1 m away is
// unoccluded, as the open plane is (visibility at least 240/255), where
// nothing lies within the 1 m radius (1.457 m reach) of it: up to its
// silhouette over the wall junction, which lies at least 1.6 m behind it
// there and is measurably darker, so blurring it into the plate darkens
// the plate's silhouette.
#[test]
fn an_unoccluded_plate_stays_unoccluded_up_to_its_silhouette() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [129, 113];
    let plate = 50..65;
    let projection = crate::perspective(60f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1);
    let mut view: crate::shading::uniforms::ViewUniform = bytemuck::Zeroable::zeroed();
    view.view = glam::Mat4::IDENTITY.to_cols_array_2d();
    view.projection = projection.to_cols_array_2d();
    let (depth, normals) = analytic_geometry(
        &device,
        &queue,
        size,
        projection,
        true,
        false,
        Some(plate.clone()),
    );
    let mut ao = super::AmbientOcclusion::new(&device, &queue);
    let mut encoder = device.create_command_encoder(&Default::default());
    let result = ao.encode(
        &device,
        &queue,
        &mut encoder,
        &depth,
        &normals,
        &view,
        crate::settings::AmbientOcclusionQuality::Ultra,
        1.0,
        None,
    );
    queue.submit([encoder.finish()]);
    let values: Vec<u32> = crate::test_support::read(&device, &queue, result.texture(), 4)
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let at = |x: u32, y: u32| values[(y * size[0] + x) as usize];
    // The columns where the floor (3 m away) or the wall (x = 0.6 m) lies at
    // least 1.6 m behind the plate.
    let columns: Vec<u32> = (0..size[0])
        .filter(|&x| {
            let ray_x = ((x as f32 + 0.5) / size[0] as f32 * 2.0 - 1.0) / projection.x_axis.x;
            let behind = if ray_x > 0.0 {
                (0.6 / ray_x).min(3.0)
            } else {
                3.0
            };
            behind - 1.0 >= 1.6
        })
        .collect();
    // The junction beside the plate, two rows above and below it.
    let beside = columns
        .iter()
        .flat_map(|&x| [at(x, plate.start - 2), at(x, plate.end + 1)])
        .min()
        .unwrap();
    assert!(
        beside < 200,
        "the junction beside the plate is not dark: {beside}"
    );
    for y in plate {
        for &x in &columns {
            assert!(
                at(x, y) >= 240,
                "plate pixel ({x}, {y}) darkened to {}",
                at(x, y)
            );
        }
    }
}

// Defect: the denoiser, two pixels an invocation, gives the pair's second
// pixel another pixel's edges or visibility, so odd and even columns filter
// differently. Oracle: the filter is the same at every pixel, so moving its
// input one column right moves its output one column right, away from the
// image's clamped left and right borders.
#[test]
fn denoising_an_image_moved_one_column_moves_the_result() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [37, 11];
    let mut ao = super::AmbientOcclusion::new(&device, &queue);
    // A frame at this size writes the parameters the denoiser reads.
    let (depth, normals) = analytic_geometry(
        &device,
        &queue,
        size,
        crate::perspective(1.0, 1.0, 0.1),
        false,
        false,
        None,
    );
    let mut view: crate::shading::uniforms::ViewUniform = bytemuck::Zeroable::zeroed();
    view.view = glam::Mat4::IDENTITY.to_cols_array_2d();
    view.projection = crate::perspective(1.0, 1.0, 0.1).to_cols_array_2d();
    let mut encoder = device.create_command_encoder(&Default::default());
    ao.encode(
        &device,
        &queue,
        &mut encoder,
        &depth,
        &normals,
        &view,
        crate::settings::AmbientOcclusionQuality::Low,
        1.0,
        None,
    );
    queue.submit([encoder.finish()]);
    // Arbitrary working words: visibility and edges in their low 16 bits.
    let mut state = 0x2545_f491_u32;
    let words: Vec<u32> = (0..size[0] * size[1])
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state & 0xffff
        })
        .collect();
    let moved: Vec<u32> = (0..size[1])
        .flat_map(|y| (0..size[0]).map(move |x| (y, x.saturating_sub(1))))
        .map(|(y, x)| words[(y * size[0] + x) as usize])
        .collect();
    let denoise = |words: &[u32]| -> Vec<u32> {
        let texture = |usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("AO denoise fixture"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Uint,
                usage,
                view_formats: &[],
            })
        };
        let working = texture(wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        queue.write_texture(
            working.as_image_copy(),
            bytemuck::cast_slice(words),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * 4),
                rows_per_image: None,
            },
            working.size(),
        );
        let output = texture(wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC);
        let mut encoder = device.create_command_encoder(&Default::default());
        ao.encode_denoise(
            &device,
            &mut encoder,
            &working.create_view(&Default::default()),
            &output.create_view(&Default::default()),
            size,
            None,
        );
        queue.submit([encoder.finish()]);
        crate::test_support::read(&device, &queue, &output, 4)
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    };
    let (original, result) = (denoise(&words), denoise(&moved));
    for y in 0..size[1] {
        for x in 1..size[0] - 2 {
            let at = |x: u32| (y * size[0] + x) as usize;
            assert_eq!(
                result[at(x + 1)],
                original[at(x)],
                "pixel ({x}, {y}) moved to ({}, {y}) denoised otherwise",
                x + 1
            );
        }
    }
}

// Defect: the Hilbert index table the main pass's noise reads (XeGTAO.h
// HilbertIndex) misses or repeats a cell, or jumps between cells, so the
// slices' and steps' noise loses its low-discrepancy order across the
// 64x64 tile. Oracle: a Hilbert curve visits every cell of its tile once,
// each cell beside the one before.
#[test]
fn hilbert_indices_walk_the_tile_one_neighbouring_cell_at_a_time() {
    let width = super::HILBERT_WIDTH;
    let mut cells = vec![None; (width * width) as usize];
    for y in 0..width {
        for x in 0..width {
            let index = super::hilbert_index(x, y) as usize;
            assert!(
                cells.get(index).is_some_and(Option::is_none),
                "({x}, {y}) has index {index}, outside the tile or taken"
            );
            cells[index] = Some((x, y));
        }
    }
    for (index, pair) in cells.windows(2).enumerate() {
        let ((ax, ay), (bx, by)) = (pair[0].unwrap(), pair[1].unwrap());
        assert_eq!(
            ax.abs_diff(bx) + ay.abs_diff(by),
            1,
            "index {index} at ({ax}, {ay}) is not beside the next at ({bx}, {by})"
        );
    }
}

/// View depth at the image's origin in metres, and its rise per pixel
/// across and down: unequal, so a mip that swaps axes or blocks is off.
const LINEAR_DEPTH: f64 = 2.0;
const LINEAR_GRADIENT: [f64; 2] = [0.011, 0.017];

// Defect: the depth prefilter (XeGTAO_PrefilterDepths16x16, one group to a
// 16x16 tile) filters a texel from a block other than its own children (a
// wrong tile origin, stride or axis), leaves texels unwritten where the
// target is not a multiple of 16, or clamps a one-texel-wide mip's children otherwise than texture
// loads clamp. Oracle: at a 10000 m radius every child lies in the
// filter's full-weight band, so each mip texel is its children's mean; over
// a view depth linear in the pixel position, that mean is the depth at the
// mean of their pixel centres. A mip is the one before's extent halved,
// rounded down, and at least one texel, as texture mip extents are, and a
// child beyond it clamps to its last texel, as loads do.
#[test]
fn depth_mips_hold_the_linear_depth_at_their_childrens_centre() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut ao = super::AmbientOcclusion::new(&device, &queue);
    for size in [[129, 113], [37, 5], [3, 1]] {
        let projection =
            crate::perspective(60f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1);
        let mut view: crate::shading::uniforms::ViewUniform = bytemuck::Zeroable::zeroed();
        view.view = glam::Mat4::IDENTITY.to_cols_array_2d();
        view.projection = projection.to_cols_array_2d();
        // Reversed-Z infinite depth: device depth is near / view depth.
        let fragment = format!(
            "let z={LINEAR_DEPTH}+{gx}*pixel.x+{gy}*pixel.y; var o:Output; o.normal=gbuffer_encode_normals(vec3(0.,0.,1.),vec3(0.,0.,1.)); o.depth={near}/z; return o;",
            gx = LINEAR_GRADIENT[0],
            gy = LINEAR_GRADIENT[1],
            near = projection.w_axis.z,
        );
        let (depth, normals) = rasterize(&device, &queue, size, &fragment);
        let mut encoder = device.create_command_encoder(&Default::default());
        ao.encode(
            &device,
            &queue,
            &mut encoder,
            &depth,
            &normals,
            &view,
            crate::settings::AmbientOcclusionQuality::Low,
            10000.0,
            None,
        );
        queue.submit([encoder.finish()]);
        let pyramid = ao.depth_pyramid().unwrap();
        for mip in 0..pyramid.mip_level_count() {
            let stride = (pyramid.width() >> mip).max(1);
            let texels = read_mip(&device, &queue, pyramid, mip);
            let extent = size.map(|extent| (extent >> mip).max(1));
            for y in 0..extent[1] {
                for x in 0..extent[0] {
                    let expected = LINEAR_DEPTH
                        + LINEAR_GRADIENT[0] * centre(x, mip, size[0])
                        + LINEAR_GRADIENT[1] * centre(y, mip, size[1]);
                    let measured = f64::from(texels[(y * stride + x) as usize]);
                    assert!(
                        (measured - expected).abs() <= 1e-4,
                        "{size:?} mip {mip} texel ({x}, {y}): {measured} m, expected {expected} m"
                    );
                }
            }
        }
    }
}

/// The mean pixel centre, along one axis of `extent` pixels, of the
/// full-resolution pixels a texel of `mip` filters.
fn centre(texel: u32, mip: u32, extent: u32) -> f64 {
    if mip == 0 {
        return f64::from(texel) + 0.5;
    }
    let children = (extent >> (mip - 1)).max(1);
    let second = (2 * texel + 1).min(children - 1);
    (centre(2 * texel, mip - 1, extent) + centre(second, mip - 1, extent)) / 2.0
}

/// One mip level of an R32Float texture, row-major at the level's extent.
fn read_mip(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip: u32,
) -> Vec<f32> {
    let extent = texture.size().mip_level_size(mip, texture.dimension());
    let row = (extent.width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("AO depth mip"),
        size: u64::from(row * extent.height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(extent.height),
            },
        },
        extent,
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = buffer.get_mapped_range(..).unwrap();
    mapped
        .chunks(row as usize)
        .flat_map(|line| {
            bytemuck::cast_slice::<u8, f32>(&line[..(extent.width * 4) as usize]).to_vec()
        })
        .collect()
}

fn analytic_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    projection: glam::Mat4,
    wall: bool,
    rotated: bool,
    plate: Option<std::ops::Range<u32>>,
) -> (wgpu::TextureView, wgpu::TextureView) {
    // Rasterize analytical primary-ray/plane intersections. This fixture does
    // not use any AO reconstruction, horizon search, noise, or filtering logic.
    let fragment = format!(
        r#"
 let uv=pixel.xy/vec2({width}.0,{height}.0);
 let ray=vec3((uv.x*2.-1.)/{px},(1.-uv.y*2.)/{py},-1.);
 var t=3.; var n=vec3(0.,0.,1.);
 if {wall} && ray.x>0. {{ let wt=0.6/ray.x; if wt<t {{ t=wt; n=vec3(-1.,0.,0.); }} }}
 if pixel.y>={plate_top}. && pixel.y<{plate_bottom}. {{ t=1.; n=vec3(0.,0.,1.); }}
 let z=ray.z*t;
 if {rotated} {{ n=vec3(-n.x,n.y,-n.z); }}
 var o:Output; o.normal=gbuffer_encode_normals(n,n); o.depth=({pz}*z+{pw})/(-z); return o;
"#,
        width = size[0],
        height = size[1],
        px = projection.x_axis.x,
        py = projection.y_axis.y,
        pz = projection.z_axis.z,
        pw = projection.w_axis.z,
        rotated = rotated,
        wall = wall,
        plate_top = plate.as_ref().map_or(0, |rows| rows.start),
        plate_bottom = plate.as_ref().map_or(0, |rows| rows.end),
    );
    rasterize(device, queue, size, &fragment)
}

/// The depth and normal targets a full-screen triangle writes, `fragment`
/// being the body of a fragment shader that takes the pixel's position and
/// returns an `Output` of its encoded normals and device depth.
fn rasterize(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    fragment: &str,
) -> (wgpu::TextureView, wgpu::TextureView) {
    let source = format!(
        r#"
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {{
 let v=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); return vec4(v[i],0.,1.);
}}
struct Output {{ @location(0) normal:vec4<f32>, @builtin(frag_depth) depth:f32 }}
@fragment fn fs(@builtin(position) pixel:vec4<f32>)->Output {{{fragment}}}
"#
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("analytic AO geometry"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}\n{source}",
                crate::shading::compose(&[&crate::shading::GBUFFER])
            )
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("analytic AO geometry"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::TextureFormat::Rgba16Float.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let texture = |format| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("analytic AO input"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let depth = texture(wgpu::TextureFormat::Depth32Float);
    let normals = texture(wgpu::TextureFormat::Rgba16Float);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("analytic AO input"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &normals,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
    (depth, normals)
}
