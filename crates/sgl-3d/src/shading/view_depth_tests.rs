//! The linear view depth of a device depth under each projection a view may
//! have (view_depth.wgsl), evaluated on the GPU and judged by projecting
//! known view-space points on the CPU: a point's device depth and image
//! position must give back its view depth.
use crate::view::reflected_camera;
use glam::{DVec2, DVec3, Mat4, Vec2, Vec3};

/// One point seen through one projection.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Case {
    projection: [[f32; 4]; 4],
    /// The point's unjittered position in the image, then its device
    /// depth.
    ndc_depth: [f32; 4],
}

/// `projected_view_depth` at each case.
fn evaluate(cases: &[Case]) -> Option<Vec<f32>> {
    let (device, queue) = crate::test_support::device()?;
    let observation = r#"
struct Case { projection:mat4x4<f32>,ndc_depth:vec4<f32> }
@group(0) @binding(0) var<storage,read> cases:array<Case>;
@group(0) @binding(1) var<storage,read_write> result:array<f32>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];
 let oblique=view_oblique_depth(c.projection,c.ndc_depth.xy);
 result[id.x]=projected_view_depth(c.projection,c.ndc_depth.z,oblique);
}
"#;
    let source = format!("{}{observation}", super::compose(&[&super::VIEW_DEPTH]));
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("view depth observation"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("view depth observation"),
        layout: None,
        module: &module,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let input = wgpu::util::DeviceExt::create_buffer_init(
        &device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("view depth cases"),
            contents: bytemuck::cast_slice(cases),
            usage: wgpu::BufferUsages::STORAGE,
        },
    );
    let result = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("view depths"),
        size: (cases.len() * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: result.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
    }
    queue.submit([encoder.finish()]);
    Some(
        crate::test_support::read_words(&device, &queue, &result)
            .into_iter()
            .map(f32::from_bits)
            .collect(),
    )
}

/// The case of the view-space point `point` (looking along -Z) under
/// `projection`: its image position and device depth by the projection's
/// own arithmetic in double precision.
fn case(projection: Mat4, point: DVec3) -> Case {
    let clip = projection.as_dmat4() * point.extend(1.);
    let ndc = DVec2::new(clip.x, clip.y) / clip.w;
    Case {
        projection: projection.to_cols_array_2d(),
        ndc_depth: [ndc.x as f32, ndc.y as f32, (clip.z / clip.w) as f32, 0.],
    }
}

// Plausible defects: the depth row's x and y terms, which a planar
// reflector's reflected view has, are left out, so every depth off the
// view's axis is wrong there; an orthographic or an off-centre projection
// takes the perspective formula. The fixture: points through a
// `perspective` projection, an off-centre one, a reversed-Z orthographic
// one, and a reflected view's cropped projection clipped at a tilted
// plane. (A finite far plane and the sky are the HDR effect suite's,
// `scenario_hdr_effect`, which reads its depth through the same function.) The oracle is the
// projection itself: the device depth and pixel a point projects to must
// give back the point's view depth, to 1 part in 2000 (f32 through one
// division of differences of products).
#[test]
fn a_device_depth_gives_back_its_view_depth_under_every_projection() {
    let perspective = crate::perspective(1., 16. / 9., 0.1);
    // A reflected view's projection: a viewer above a floor, looking down
    // and aside, cropped to part of its image and clipped at the floor.
    let eye = Vec3::new(0.3, 1.6, 2.);
    let consumer = crate::Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, Vec3::new(-1., 0., -4.), Vec3::Y),
        projection: perspective,
        eye,
    };
    let floor = Vec3::Y.extend(0.);
    let rect = reflected_camera::Rect {
        min: Vec2::new(-0.6, -1.),
        max: Vec2::new(0.4, 0.2),
    };
    let reflected = reflected_camera::camera(&consumer, floor, rect);
    let clipped = reflected_camera::oblique(reflected.projection, reflected.view, floor).unwrap();
    let off_centre = {
        let mut projection = perspective;
        projection.z_axis.x = 0.2;
        projection.z_axis.y = -0.1;
        projection
    };
    // Reversed Z over 0.1..100 m: depth 1 near, 0 far.
    let orthographic =
        glam::camera::rh::proj::directx::orthographic(-4., 4., -2.25, 2.25, 100., 0.1);
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    for (name, projection) in [
        ("perspective", perspective),
        ("off-centre", off_centre),
        ("orthographic", orthographic),
        ("clipped", clipped),
    ] {
        for point in [
            DVec3::new(0., 0., -5.),
            DVec3::new(1.5, -0.8, -3.),
            DVec3::new(-2., 1., -12.),
            DVec3::new(0.4, 0.3, -40.),
        ] {
            // The clipped projection's near plane is the floor: only points
            // beyond it have a depth to give back.
            let clip = projection.as_dmat4() * point.extend(1.);
            let depth = clip.z / clip.w;
            if !(0. ..1.).contains(&depth) {
                continue;
            }
            cases.push(case(projection, point));
            expected.push((name, point, -point.z));
        }
    }
    // Every projection contributed, the clipped one among them.
    for name in ["perspective", "orthographic", "clipped"] {
        assert!(expected.iter().any(|case| case.0 == name), "{name}");
    }
    assert!(
        clipped.x_axis.z != 0. || clipped.y_axis.z != 0.,
        "the fixture's clipped projection has depth terms in x or y"
    );
    let Some(depths) = evaluate(&cases) else {
        return;
    };
    for (depth, (name, point, expected)) in depths.iter().zip(&expected) {
        assert!(
            (f64::from(*depth) - expected).abs() <= expected / 2000.,
            "{name}, {point}: a view depth of {depth}, not {expected}"
        );
    }
}
