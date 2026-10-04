use super::GpuTiming;

const SHADER: &str = "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2. - 1., 0.5, 1.);
}
@fragment fn fs() -> @location(0) vec4f { return vec4f(0.001); }
@group(0) @binding(0) var<storage, read_write> values: array<u32>;
@compute @workgroup_size(64) fn cs(@builtin(global_invocation_id) id: vec3u) {
    values[id.x] = id.x * 3u;
}";

/// A timestamp-capable device, or `None` after printing why. `SGL_REQUIRE_GPU`
/// turns the skip into a failure.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()));
    let adapter = match adapter {
        Ok(adapter) if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) => adapter,
        other => {
            let reason = other.map_or_else(|e| e.to_string(), |_| "no timestamp queries".into());
            assert!(
                !required,
                "GPU test cannot run but SGL_REQUIRE_GPU is set: {reason}"
            );
            eprintln!("skipping GPU timing test: {reason}");
            return None;
        }
    };
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TIMESTAMP_QUERY,
            ..Default::default()
        }))
        .unwrap(),
    )
}

// Resolving before a frame completed returned zero, stale or reversed Metal
// samples. With two frames in flight, as with a swapchain, every frame must
// report each group with its real relative cost.
#[test]
fn pass_groups_report_completed_frames_in_flight() {
    let Some((device, queue)) = device() else {
        return;
    };
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let format = wgpu::TextureFormat::Rgba16Float;
    // One target orders every pass after the previous one, across frames too.
    let target = device
        .create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1920,
                height: 1080,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default());
    // Additive blending prevents hidden-surface removal from discarding layers.
    let draw = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::REPLACE,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let compute = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("cs"),
        compilation_options: Default::default(),
        cache: None,
    });
    let values = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 4 * 64 * 64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &compute.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: values.as_entire_binding(),
        }],
    });
    let mut timing = GpuTiming::new(&device, &queue).unwrap();
    let mut completed = Vec::new();
    let mut in_flight = std::collections::VecDeque::new();
    let frames = 40;
    // Two extra rounds drain the completion and readback stages.
    for frame in 0..frames + 2 {
        if frame >= frames {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        } else if in_flight.len() == 2 {
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: in_flight.pop_front(),
                timeout: None,
            });
        }
        completed.extend(timing.begin_frame(&device, &queue));
        if frame >= frames {
            continue;
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        for (name, layers) in [("light", 1), ("heavy", 200), ("heavy", 200)] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                timestamp_writes: timing.render_pass(name),
                ..Default::default()
            });
            pass.set_pipeline(&draw);
            pass.draw(0..3, 0..layers);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: timing.compute_pass("compute"),
            });
            pass.set_pipeline(&compute);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(64, 1, 1);
        }
        in_flight.push_back(queue.submit([encoder.finish()]));
        timing.submitted(&queue);
    }
    // The ring covers two frames in flight plus resolve and readback latency.
    assert_eq!(
        completed.iter().map(|f| f.frame).collect::<Vec<_>>(),
        (1..=frames).collect::<Vec<_>>()
    );
    for frame in &completed {
        let names: Vec<_> = frame.passes.iter().map(|p| p.name).collect();
        assert_eq!(names, ["light", "heavy", "compute"], "{frame:?}");
        let added: f64 = frame.passes.iter().map(|p| p.ms).sum();
        assert!(frame.passes.iter().all(|p| p.ms >= 0.), "{frame:?}");
        assert!(
            added <= frame.total_ms + 1e-6 && frame.total_ms < 1000.,
            "{frame:?}"
        );
        let [light, heavy] = [0, 1].map(|i| frame.passes[i].ms);
        assert!(light > 0., "{frame:?}");
        // Each group clears and stores a 1080p target, which costs the
        // one-layer group about 1.4 ms on Apple M5 against about 5.4 ms for
        // 400 layers; misattributed timestamps put heavy at or below light.
        assert!(
            heavy > 2. * light,
            "400 blended layers versus one: {frame:?}"
        );
    }
}
