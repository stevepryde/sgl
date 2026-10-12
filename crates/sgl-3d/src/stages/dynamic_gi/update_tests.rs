//! Sparse probe updates at the production blend boundary. Four rays toward
//! +Z leave the opposite octahedral hemisphere unobserved; its existing
//! light and visibility must remain, rather than treating no sample as zero.
use super::buffers::{Convergence, VolumeUniform};
use super::pipelines::UPDATE;
use super::volume::{Layouts, texture};
use crate::{shading, test_support};
use wgpu::util::DeviceExt;

#[test]
fn four_ray_updates_keep_unobserved_depth_and_irradiance_history() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let layouts = Layouts::new(&device);
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&layouts.update)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(shading::compose(&[&UPDATE]).into()),
    });
    let pipeline = |entry| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(&layout),
            module: &shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let irradiance = pipeline("update_irradiance");
    let depth = pipeline("update_depth");
    let storage = |bytes: &[u8]| {
        crate::scene::buffer(
            &device,
            "sparse probe history",
            bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        )
    };
    let half2 =
        |a, b| u32::from(test_support::to_half(a)) | (u32::from(test_support::to_half(b)) << 16);
    // Four rays hit a bright surface half a spacing away along +Z. Exact
    // axis directions give an independently known unsampled -Z hemisphere.
    let rays = [0, half2(1., 0.5), half2(2., 2.), half2(2., 0.)];
    let rays: Vec<u32> = rays.into_iter().cycle().take(4 * 4).collect();
    let ray_texture = device
        .create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&rays),
        )
        .create_view(&Default::default());
    let uniform = VolumeUniform {
        max_distance: 1.5,
        spacing: [1.; 3],
        max_rays: 4,
        probes: [1; 3],
        probe_count: 1,
        traced: 1,
        ..bytemuck::Zeroable::zeroed()
    };
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let ray_counts = storage(bytemuck::cast_slice(&[4u32]));
    let traced = storage(bytemuck::cast_slice(&[0u32]));
    let convergence = storage(&[0; std::mem::size_of::<Convergence>()]);
    for blended in [true, false] {
        // Both cases start with deliberately nonzero old data, so a fresh
        // probe must also replace any previous occupant's unsampled texels.
        let old_depth = half2(1., 1.);
        let depths = storage(bytemuck::cast_slice(&[old_depth; 16 * 16]));
        let old_variance = [
            half2(1., 1.),
            half2(1., 1.),
            half2(1., 1.),
            half2(1., 1.),
            half2(1., 1.),
            half2(1., 0.),
        ];
        let variance_words: Vec<u32> = old_variance.into_iter().cycle().take(6 * 6 * 6).collect();
        let variance = storage(bytemuck::cast_slice(&variance_words));
        let state = [0, half2(0., if blended { 1. } else { 0. }), 0, 1 << 25];
        let states = storage(bytemuck::cast_slice(&state));
        let output = texture(
            &device,
            "sparse probe output",
            [18, 27],
            wgpu::TextureFormat::Rgba16Float,
        );
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layouts.update,
            entries: &[
                entry(0, uniform.as_entire_binding()),
                entry(1, ray_counts.as_entire_binding()),
                entry(2, wgpu::BindingResource::TextureView(&ray_texture)),
                entry(3, variance.as_entire_binding()),
                entry(4, depths.as_entire_binding()),
                entry(5, states.as_entire_binding()),
                entry(6, wgpu::BindingResource::TextureView(&output)),
                entry(7, traced.as_entire_binding()),
                entry(8, convergence.as_entire_binding()),
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for pipeline in [&irradiance, &depth] {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);
        let depths = test_support::read_words(&device, &queue, &depths);
        let variance = test_support::read_words(&device, &queue, &variance);
        // Octahedral corner texels point toward -Z (|x|+|y|>1).
        // They receive none of the four +Z rays in either blend's kernel.
        for index in [0, 15, 240, 255] {
            assert_eq!(
                depths[index],
                if blended { old_depth } else { half2(1.5, 2.25) },
                "blended={blended}, depth corner {index}"
            );
        }
        for index in [0, 5, 30, 35] {
            let words = &variance[index * 6..(index + 1) * 6];
            if blended {
                assert_eq!(
                    words, old_variance,
                    "irradiance corner {index} changed without a sample"
                );
            } else {
                assert_eq!(
                    words[..5],
                    [0; 5],
                    "fresh irradiance corner {index} retained old light"
                );
            }
        }
        // The texture and its copied borders are what receivers sample;
        // preserving only the CPU-readable history buffer is insufficient.
        let pixels = test_support::read(&device, &queue, output.texture(), 8);
        let texel = |x: usize, y: usize| -> [u16; 4] {
            let at = (y * 18 + x) * 8;
            std::array::from_fn(|channel| {
                u16::from_le_bytes([pixels[at + channel * 2], pixels[at + channel * 2 + 1]])
            })
        };
        let expected_depth =
            if blended { [1., 1.] } else { [1.5, 2.25] }.map(test_support::to_half);
        for [x, y] in [[1, 1], [16, 16], [0, 17], [17, 0]] {
            assert_eq!(
                texel(x, y)[..2],
                expected_depth,
                "unobserved depth texture ({x},{y})"
            );
        }
        let expected_color = test_support::to_half(if blended { 1. } else { 0. });
        for [x, y] in [[1, 19], [6, 24], [0, 25], [7, 18]] {
            assert_eq!(
                texel(x, y)[..3],
                [expected_color; 3],
                "unobserved irradiance texture ({x},{y})"
            );
        }
        // The central +Z texels do receive rays and still update.
        assert_ne!(depths[7 * 16 + 7], old_depth);
        assert_ne!(variance[2 * 6 * 6 + 2 * 6], old_variance[0]);
    }
}
