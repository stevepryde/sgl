//! Retained native triangles: independent f64 ray oracle places the nearer
//! triangle before the one behind it.
//! Catches vertex-transform depth swaps and Equal-pass material overwrites.
use super::{GeometryPass, depth};
use glam::Mat4;
use wgpu::util::DeviceExt;

#[test]
fn primary_depth_precision_native_planes() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let data: serde_json::Value =
            serde_json::from_str(include_str!("primary_depth_precision_inputs.json")).unwrap();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native primary depth precision diagnosis"),
            source: wgpu::ShaderSource::Wgsl(format!("{}\n{}", crate::shading::compose(&[&crate::shading::BIND_SHADOW, &crate::shading::BIND_SCENE, &crate::shading::VERTEX]), r#"
struct Camera { combined:mat4x4<f32>, view:mat4x4<f32>, projection:mat4x4<f32>, mode:vec4<u32> }
@group(0) @binding(0) var<uniform> camera:Camera;
struct V { @invariant @builtin(position) position:vec4<f32>, @location(0) @interpolate(flat) identity:u32 }
@vertex fn vs(@location(0) position:vec3<f32>, @builtin(vertex_index) index:u32)->V {
 let world=vec4(position,1.);
 var clip=camera.combined*world;
 if camera.mode.x==1u {clip=scene_clip_position(world,camera.view,camera.projection);}
 return V(clip,index/3u+1u);
}
@fragment fn fs(v:V)->@location(0) vec4<f32> {return vec4(f32(v.identity),v.position.z,0.,1.);}
"#).into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = |compare: wgpu::CompareFunction| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 12,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0=>Float32x3],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(compare != wgpu::CompareFunction::Equal),
                    depth_compare: Some(compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipelines = [
            pipeline(wgpu::CompareFunction::Greater),
            pipeline(depth(GeometryPass::GBuffer).1),
            pipeline(depth(GeometryPass::Lighting { shadow_mask: false }).1),
        ];
        let target = |format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 960,
                    height: 540,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let outputs = [
            target(wgpu::TextureFormat::Rgba32Float),
            target(wgpu::TextureFormat::Rgba32Float),
        ];
        let views = outputs
            .each_ref()
            .map(|v| v.create_view(&Default::default()));
        let depth = target(wgpu::TextureFormat::Depth32Float).create_view(&Default::default());
        let mut legacy_wrong = 0;
        let mut separate_wrong = 0;
        for row in data["frames"].as_array().unwrap() {
            let parse = |name: &str| {
                Mat4::from_cols_array_2d(
                    &serde_json::from_value::<[[f32; 4]; 4]>(row[name].clone()).unwrap(),
                )
            };
            let view = parse("view");
            let projection = parse("projection");
            let vertices: Vec<f32> = row["hits"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|h| h["vertices"].as_array().unwrap())
                .flat_map(|v| v.as_array().unwrap())
                .map(|v| v.as_f64().unwrap() as f32)
                .collect();
            for (coplanar, reverse) in [(false, false), (false, true), (true, false), (true, true)]
            {
                let mut case_vertices = vertices.clone();
                if coplanar {
                    let plane = vertices[..9].to_vec();
                    case_vertices[9..].copy_from_slice(&plane);
                }
                let vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&case_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                for mode in 0..2u32 {
                    let words: Vec<u32> = (projection * view)
                        .to_cols_array()
                        .into_iter()
                        .chain(view.to_cols_array())
                        .chain(projection.to_cols_array())
                        .map(f32::to_bits)
                        .chain([mode, 0, 0, 0])
                        .collect();
                    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: bytemuck::cast_slice(&words),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &bind_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        }],
                    });
                    let mut encoder = device.create_command_encoder(&Default::default());
                    for pass_index in 0..2 {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: None,
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &views[pass_index],
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: Some(
                                wgpu::RenderPassDepthStencilAttachment {
                                    view: &depth,
                                    depth_ops: Some(wgpu::Operations {
                                        load: if pass_index == 0 {
                                            wgpu::LoadOp::Clear(0.)
                                        } else {
                                            wgpu::LoadOp::Load
                                        },
                                        store: wgpu::StoreOp::Store,
                                    }),
                                    stencil_ops: None,
                                },
                            ),
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                        pass.set_pipeline(
                            &pipelines[if pass_index == 1 { 2 } else { mode as usize }],
                        );
                        pass.set_bind_group(0, &group, &[]);
                        pass.set_vertex_buffer(0, vertex.slice(..));
                        pass.set_scissor_rect(415, 260, 1, 1);
                        if reverse {
                            pass.draw(3..6, 0..1);
                            pass.draw(0..3, 0..1);
                        } else {
                            pass.draw(0..3, 0..1);
                            pass.draw(3..6, 0..1);
                        }
                    }
                    queue.submit([encoder.finish()]);
                    let mut results = Vec::new();
                    for output in &outputs {
                        let bytes = crate::test_support::read(&device, &queue, output, 16);
                        let floats = bytemuck::cast_slice::<u8, f32>(&bytes);
                        results.push([
                            floats[(260 * 960 + 415) * 4],
                            floats[(260 * 960 + 415) * 4 + 1],
                        ]);
                    }
                    eprintln!(
                        "P23 coplanar={coplanar} reverse={reverse} frame={} mode={} prepass={:?} equal={:?} oracle front=317 rear=98496 separation_mm={:.6}",
                        row["frame"],
                        mode,
                        results[0],
                        results[1],
                        1000.
                            * (row["hits"][1]["distance"].as_f64().unwrap()
                                - row["hits"][0]["distance"].as_f64().unwrap())
                    );
                    // A distinct front surface wins regardless of order. Exact
                    // coplanar coverage has no physical front: both passes must
                    // consistently choose the last submitted material.
                    let expected = if coplanar && !reverse { 2. } else { 1. };
                    let wrong = results.iter().any(|v| v[0] != expected);
                    if mode == 0 {
                        legacy_wrong += usize::from(wrong);
                    } else {
                        separate_wrong += usize::from(wrong);
                    }
                }
            }
        }
        eprintln!("P23 legacy_wrong={legacy_wrong} separate_wrong={separate_wrong}");
        assert_eq!(
            separate_wrong, 0,
            "separate projection must preserve physical nearest membership in both passes"
        );
    });
}
