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
        let mut ao = super::AmbientOcclusion::new(&device);
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
                analytic_geometry(&device, &queue, size, projection, wall, rotated);
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
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as f64 / 255.0 * 1.5)
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

fn analytic_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    projection: glam::Mat4,
    wall: bool,
    rotated: bool,
) -> (wgpu::TextureView, wgpu::TextureView) {
    // Rasterize analytical primary-ray/plane intersections. This fixture does
    // not use any AO reconstruction, horizon search, noise, or filtering logic.
    let source = format!(
        r#"
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {{
 let v=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); return vec4(v[i],0.,1.);
}}
struct Output {{ @location(0) normal:vec4<f32>, @builtin(frag_depth) depth:f32 }}
@fragment fn fs(@builtin(position) pixel:vec4<f32>)->Output {{
 let uv=pixel.xy/vec2({width}.0,{height}.0);
 let ray=vec3((uv.x*2.-1.)/{px},(1.-uv.y*2.)/{py},-1.);
 var t=3.; var n=vec3(0.,0.,1.);
 if {wall} && ray.x>0. {{ let wt=0.6/ray.x; if wt<t {{ t=wt; n=vec3(-1.,0.,0.); }} }}
 let z=ray.z*t;
 if {rotated} {{ n=vec3(-n.x,n.y,-n.z); }}
 var o:Output; o.normal=gbuffer_encode_normals(n,n); o.depth=({pz}*z+{pw})/(-z); return o;
}}
"#,
        width = size[0],
        height = size[1],
        px = projection.x_axis.x,
        py = projection.y_axis.y,
        pz = projection.z_axis.z,
        pw = projection.w_axis.z,
        rotated = rotated,
        wall = wall
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
