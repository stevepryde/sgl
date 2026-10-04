//! Bounded camera-path heat shimmer, following Sousa, GPU Gems 2 chapter 19.
//! The caller supplies the vector field (the scene's transient heat
//! geometry); this is not physical refractive transport.
use crate::content::transient::HeatDistortion;
use crate::scene::transient::Transient;
use crate::shading::gbuffer::COLOR as HDR;
use crate::shading::vertex::{VertexLayout, vertex_layout};
use crate::view::targets::{attachment, target};
use wgpu::util::DeviceExt;

pub(crate) static HEAT: crate::shading::Module = crate::shading::Module {
    name: "heat_distortion",
    source: include_str!("heat.wgsl"),
    deps: &[],
};
/// The heat shimmer's vertex buffer, read by `vs`.
pub(crate) const HEAT_LAYOUT: VertexLayout =
    vertex_layout!(HeatDistortion, [position, displacement, weight]);

pub(crate) struct Heat {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    matrix: wgpu::Buffer,
    source: Option<wgpu::TextureView>,
}
impl Heat {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("heat snapshot layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded heat shimmer"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&HEAT]).into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bounded heat shimmer"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[HEAT_LAYOUT.buffer],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR,
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
        Self {
            pipeline,
            layout,
            matrix: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("heat stable camera"),
                contents: bytemuck::cast_slice(&glam::Mat4::IDENTITY.to_cols_array()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            }),
            source: None,
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        transient: &Transient,
        matrix: &[[f32; 4]; 4],
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        if transient.heat_count == 0 {
            return;
        }
        let size = [color.texture().width(), color.texture().height()];
        if self
            .source
            .as_ref()
            .is_none_or(|s| s.texture().size() != color.texture().size())
        {
            self.source = Some(target(device, "immutable heat source", size, HDR));
        }
        let source = self.source.as_ref().unwrap();
        queue.write_buffer(&self.matrix, 0, bytemuck::bytes_of(matrix));
        encoder.copy_texture_to_texture(
            color.texture().as_image_copy(),
            source.texture().as_image_copy(),
            color.texture().size(),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("heat current source and depth"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.matrix.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
            ],
        });
        let mut attachment = attachment(color).unwrap();
        attachment.ops.load = wgpu::LoadOp::Load;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("camera heat before bloom and HUD"),
            color_attachments: &[Some(attachment)],
            timestamp_writes: timing.and_then(|t| t.render_pass("heat distortion")),
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_vertex_buffer(0, transient.heat.slice(..));
        pass.draw(0..transient.heat_count, 0..1);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::content::transient::MAX_VERTICES;
    // Defects: wrong displacement scale/orientation, feedback, foreground bleed,
    // viewport clamping, stale resize resources and changes outside coverage.
    // Oracle: authored linear ramp and independently rasterized foreground stripe.
    #[test]
    #[ignore = "real GPU: bounded heat source sampling and depth footprint"]
    fn bounded_sampling() {
        pollster::block_on(async {
            let adapter = wgpu::Instance::default()
                .request_adapter(&Default::default())
                .await
                .unwrap();
            let (device, queue) = adapter
                .request_device(&crate::test_support::diagnostic_device_descriptor(&adapter))
                .await
                .unwrap();
            let mut heat = Heat::new(&device);
            let mut transient = Transient::new(&device);
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl("@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {let p=vec2(f32((i<<1u)&2u),f32(i&2u));return vec4(p*2.-1.,.75,1.);}".into()) });
            let depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: None,
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            for size in [[32, 16], [48, 24]] {
                let color = target(&device, "ramp", size, HDR);
                let depth = target(
                    &device,
                    "foreground stripe",
                    size,
                    wgpu::TextureFormat::Depth32Float,
                );
                let pixels: Vec<u16> = (0..size[1])
                    .flat_map(|y| {
                        (0..size[0]).flat_map(move |x| {
                            [0x3c00 + x as u16 * 16, 0x3c00 + y as u16 * 16, 0, 0x3c00]
                        })
                    })
                    .collect();
                for (offset, weight, clear) in [
                    ([2.5_f32, 1.5_f32], 1., false),
                    ([-2.5, -1.5], 1., false),
                    ([0., 0.], 1., false),
                    ([2.5, 1.5], 0., false),
                    ([32., 32.], 1., false),
                    ([2.5, 1.5], 1., true),
                ] {
                    queue.write_texture(
                        color.texture().as_image_copy(),
                        bytemuck::cast_slice(&pixels),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(size[0] * 8),
                            rows_per_image: Some(size[1]),
                        },
                        color.texture().size(),
                    );
                    // Left three quarters, middle half vertically, at device depth .5.
                    let vertices = [
                        [-1., -0.5, 0.5],
                        [0.5, -0.5, 0.5],
                        [-1., 0.5, 0.5],
                        [-1., 0.5, 0.5],
                        [0.5, -0.5, 0.5],
                        [0.5, 0.5, 0.5],
                    ]
                    .map(|position| HeatDistortion {
                        position,
                        displacement: offset,
                        weight,
                    });
                    transient.update_heat(&queue, &vertices).unwrap();
                    // Failed replacement must not clear or partially overwrite a live list.
                    assert!(
                        transient
                            .update_heat(&queue, &vec![vertices[0]; MAX_VERTICES + 3])
                            .is_err()
                    );
                    if clear {
                        transient.update_heat(&queue, &[]).unwrap();
                    }
                    let mut encoder = device.create_command_encoder(&Default::default());
                    {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            color_attachments: &[],
                            depth_stencil_attachment: Some(
                                wgpu::RenderPassDepthStencilAttachment {
                                    view: &depth,
                                    depth_ops: Some(wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(0.2),
                                        store: wgpu::StoreOp::Store,
                                    }),
                                    stencil_ops: None,
                                },
                            ),
                            ..Default::default()
                        });
                        pass.set_pipeline(&depth_pipeline);
                        pass.set_scissor_rect(10, 0, 1, size[1]);
                        pass.draw(0..3, 0..1);
                    }
                    heat.encode(
                        &device,
                        &queue,
                        &mut encoder,
                        &transient,
                        &glam::Mat4::IDENTITY.to_cols_array_2d(),
                        &color,
                        &depth,
                        None,
                    );
                    queue.submit([encoder.finish()]);
                    let actual = crate::test_support::read(&device, &queue, color.texture(), 8);
                    for y in 0..size[1] {
                        for x in 0..size[0] {
                            let i = ((y * size[0] + x) * 8) as usize;
                            let changed = weight > 0.
                                && !clear
                                && offset[0].abs() == 2.5
                                && x < size[0] * 3 / 4
                                && y >= size[1] / 4
                                && y < size[1] * 3 / 4
                                && x != 10
                                && if offset[0] > 0. {
                                    x != 7 && x != 8
                                } else {
                                    x >= 3 && x != 12 && x != 13
                                };
                            if changed {
                                let red = crate::test_support::half(&actual[i..]);
                                let green = crate::test_support::half(&actual[i + 2..]);
                                assert!(
                                    (red - (1. + (x as f32 + offset[0]) / 64.)).abs() < 0.001,
                                    "red ({x},{y}): {red}"
                                );
                                assert!(
                                    (green - (1. + (y as f32 + offset[1]) / 64.)).abs() < 0.001,
                                    "green ({x},{y}): {green}"
                                );
                            } else {
                                assert_eq!(
                                    &actual[i..i + 8],
                                    &bytemuck::cast_slice::<_, u8>(&pixels)[i..i + 8],
                                    "preserve ({x},{y}) offset {offset:?} weight {weight}"
                                );
                            }
                        }
                    }
                }
                transient.update_heat(&queue, &[]).unwrap();
            }
        });
    }

    // Defect: heat distorting anything but the complete composed frame: run
    // before a transparent draw (glow or mist would land on the composite
    // unwarped) or on another target than the composite that exposure and
    // antialiasing read (the frame would not warp). Oracle: the same frame
    // without heat, its composite shifted by the authored two pixels wherever
    // the heat covers it and its footprint stays on screen.
    #[test]
    fn heat_warps_the_composite_after_its_transparent_draws() {
        use crate::effects::Glow;
        use crate::settings::{Antialiasing, Bloom, SceneResolution, Settings};
        use crate::{Camera, FrameInput, Mist, Renderer, Scene};
        use glam::{Mat4, Vec3};
        let Some((device, queue)) = crate::test_support::device() else {
            return;
        };
        let size = [32, 24];
        let mut scene = Scene::new(&device, &queue);
        // Glow over the whole frame, its red growing to the right.
        let glow = [
            ([-20., -20., -2.], 0.),
            ([60., -20., -2.], 4.),
            ([-20., 60., -2.], 0.),
        ]
        .map(|(position, red)| Glow {
            position,
            color: [red, 0., 0., 1.],
            ..Glow::default()
        });
        scene.update_effects(&device, &queue, &glow);
        // One mist billboard across the middle of the frame.
        scene.update_mist(&device, &queue, &[[0., 0., -2.]]);
        // Heat over the whole frame, shifting it two pixels.
        let heat = [[-10., -10., -1.], [30., -10., -1.], [-10., 30., -1.]].map(|position| {
            HeatDistortion {
                position,
                displacement: [2., 0.],
                weight: 1.,
            }
        });
        scene.update_heat_distortion(&queue, &heat).unwrap();
        let mut input = FrameInput::new(Camera {
            view: Mat4::IDENTITY,
            projection: crate::perspective(
                std::f32::consts::FRAC_PI_2,
                size[0] as f32 / size[1] as f32,
                0.1,
            ),
            eye: Vec3::ZERO,
        });
        input.camera_cut = true;
        input.atmosphere = true;
        input.mist = Mist {
            thin_color: [0., 0., 1.],
            dense_color: [0., 4., 0.],
            opacity: 1.,
            width: 3.,
            height: 3.,
            ..Mist::default()
        };
        let settings = Settings {
            scene_resolution: SceneResolution::Full,
            antialiasing: Antialiasing::Off,
            bloom: Bloom::Off,
            ..Settings::default()
        };
        let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
        let output = crate::view::targets::target(
            &device,
            "caller output",
            size,
            crate::shading::gbuffer::COLOR,
        );
        let mut composite = |heat_distortion| {
            let settings = Settings {
                heat_distortion,
                ..settings
            };
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.render(
                &device,
                &queue,
                &mut encoder,
                &mut scene,
                &input,
                &settings,
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            renderer.finish_frame(&mut scene);
            crate::test_support::read(&device, &queue, renderer.targets().composite.texture(), 8)
        };
        let unwarped = composite(false);
        let warped = composite(true);
        let texel = |frame: &[u8], x: u32, y: u32| {
            let i = ((y * size[0] + x) * 8) as usize;
            frame[i..i + 8].to_vec()
        };
        // A misplaced heat shows only where the frame varies across it.
        let channel = |x, channel: usize| {
            crate::test_support::half(&texel(&unwarped, x, size[1] / 2)[channel * 2..])
        };
        assert!(
            channel(4, 0) < channel(28, 0),
            "the glow must brighten to the right"
        );
        assert!(channel(16, 1) > 0., "the mist must show");
        for y in 0..size[1] {
            for x in 0..size[0] {
                let source = if x + 2 < size[0] { x + 2 } else { x };
                assert_eq!(
                    texel(&warped, x, y),
                    texel(&unwarped, source, y),
                    "({x},{y}) is not the unwarped frame's ({source},{y})"
                );
            }
        }
    }
}
