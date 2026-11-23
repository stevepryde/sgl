//! Whether a froxel lies wholly behind a reflected view's plane
//! (fog.wgsl `fog_froxel_behind`), evaluated on the GPU and judged against
//! a brute-force model: the froxel's eight corners in view space, each
//! placed by the column's ray, and the plane's signed distance at each.
use glam::{DVec3, DVec4};

/// One froxel and one plane.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Case {
    /// The plane in view space (unit normal, d).
    plane: [f32; 4],
    /// The projection's x and y scales, then the froxel's near and far
    /// view depths.
    scale_depths: [f32; 4],
    /// The froxel's column corners (frame position plus the projection's
    /// offset): one corner, then the opposite.
    columns: [f32; 4],
}

/// `fog_froxel_behind` at each case, 1 for behind.
fn evaluate(cases: &[Case]) -> Option<Vec<u32>> {
    let (device, queue) = crate::test_support::device()?;
    let observation = r#"
struct Case { plane:vec4<f32>,scale_depths:vec4<f32>,columns:vec4<f32> }
@group(0) @binding(0) var<storage,read> cases:array<Case>;
@group(0) @binding(1) var<storage,read_write> result:array<u32>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];
 result[id.x]=select(0u,1u,fog_froxel_behind(c.plane,c.scale_depths.xy,c.columns.xy,c.columns.zw,c.scale_depths.z,c.scale_depths.w));
}
"#;
    let source = format!("{}{observation}", super::compose(&[&super::FOG]));
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("froxel plane observation"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("froxel plane observation"),
        layout: None,
        module: &module,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let input = wgpu::util::DeviceExt::create_buffer_init(
        &device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("froxel plane cases"),
            contents: bytemuck::cast_slice(cases),
            usage: wgpu::BufferUsages::STORAGE,
        },
    );
    let result = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("froxel plane results"),
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
    Some(crate::test_support::read_words(&device, &queue, &result))
}

/// The greatest signed distance from `plane` of the froxel's eight corners,
/// each the column's view point (position × depth / scale, -depth).
fn greatest_distance(case: &Case) -> f64 {
    let plane = DVec4::from(case.plane.map(f64::from));
    let [sx, sy, near, far] = case.scale_depths.map(f64::from);
    let [x0, y0, x1, y1] = case.columns.map(f64::from);
    let mut greatest = f64::NEG_INFINITY;
    for x in [x0, x1] {
        for y in [y0, y1] {
            for depth in [near, far] {
                let point = DVec3::new(x * depth / sx, y * depth / sy, -depth);
                greatest = greatest.max(plane.truncate().dot(point) + plane.w);
            }
        }
    }
    greatest
}

// Plausible defects: the bound picks the wrong corner across the frame (a
// swapped sign, or y taken up the frame where it runs down), so a froxel
// the plane crosses is taken as wholly behind it and its medium dropped;
// or it tests one depth only. The fixture: 4096 froxels and planes drawn
// at random (fixed seed), planes at every tilt, froxels across the frame
// and from the eye to 200 m deep, of either column order. The oracle is
// the brute-force model: a froxel is behind exactly where all eight of its
// corners are, cases within f32 rounding of the plane left out.
#[test]
fn a_froxel_is_behind_the_plane_exactly_where_all_its_corners_are() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    let mut cases = Vec::new();
    while cases.len() < 4096 {
        let normal = DVec3::new(
            f64::from(next()) * 2. - 1.,
            f64::from(next()) * 2. - 1.,
            f64::from(next()) * 2. - 1.,
        );
        let Some(normal) = normal.try_normalize() else {
            continue;
        };
        let d = f64::from(next()) * 40. - 20.;
        let near = next() * 150.;
        let far = near + 0.01 + next() * 50.;
        let x = next() * 2.4 - 1.2;
        let y = next() * 2.4 - 1.2;
        let case = Case {
            plane: normal.extend(d).as_vec4().to_array(),
            scale_depths: [0.5 + next() * 2., 0.5 + next() * 2., near, far],
            columns: [x, y, x + next() * 0.2 - 0.1, y + next() * 0.2 - 0.1],
        };
        // Within f32 rounding of the plane the answer is either.
        let greatest = greatest_distance(&case);
        if greatest.abs() > 1e-3 * (1. + f64::from(far)) {
            cases.push(case);
        }
    }
    let behind = cases
        .iter()
        .filter(|case| greatest_distance(case) < 0.)
        .count();
    assert!(
        behind > 500 && behind < 3500,
        "the fixture holds froxels on both sides: {behind} behind"
    );
    let Some(results) = evaluate(&cases) else {
        return;
    };
    for (case, result) in cases.iter().zip(results) {
        let expected = greatest_distance(case) < 0.;
        assert_eq!(
            result == 1,
            expected,
            "plane {:?}, scale and depths {:?}, columns {:?}: greatest distance {}",
            case.plane,
            case.scale_depths,
            case.columns,
            greatest_distance(case)
        );
    }
}
