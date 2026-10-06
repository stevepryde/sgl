//! The scene ray validity test (`scene_ray_valid`, the architecture's Ray
//! source) on the default adapter's shader compiler: the one guard that
//! keeps every ray a hardware query or the portable walk takes finite, over
//! a nonnegative interval, with a direction.
use super::{Module, SCENE_RAYS_PREDICATE, compose};
use crate::test_support;
use wgpu::util::DeviceExt;

/// Each ray's `scene_ray_valid`, one word each.
static VALIDITY: Module = Module {
    name: "scene_ray_validity_test",
    source: "@group(0) @binding(0) var<storage,read> validity_rays:array<SceneRay>;\n@group(0) @binding(1) var<storage,read_write> validity:array<u32>;\n@compute @workgroup_size(64) fn ray_validity(@builtin(global_invocation_id) id:vec3<u32>) {\n if id.x<arrayLength(&validity_rays) {\n  validity[id.x]=select(0u,1u,scene_ray_valid(validity_rays[id.x]));\n }\n}\n",
    deps: &[&SCENE_RAYS_PREDICATE],
};

/// `scene_ray_valid` of each of `rays` (origin, t_min, direction, t_max).
fn validity((device, queue): (&wgpu::Device, &wgpu::Queue), rays: &[[f32; 8]]) -> Vec<bool> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scene ray validity"),
        source: wgpu::ShaderSource::Wgsl(compose(&[&VALIDITY]).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("scene ray validity"),
        layout: None,
        module: &module,
        entry_point: Some("ray_validity"),
        compilation_options: Default::default(),
        cache: None,
    });
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("scene ray validity rays"),
        contents: bytemuck::cast_slice(rays),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene ray validity"),
        size: 4 * rays.len() as u64,
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
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups((rays.len() as u32).div_ceil(64), 1, 1);
    }
    queue.submit([encoder.finish()]);
    test_support::read_words(device, queue, &output)
        .into_iter()
        .map(|word| word != 0)
        .collect()
}

// Plausible defects: a finiteness test that the shader compiler folds away
// under fast math (Metal's default, which wgpu-hal leaves), as a comparison
// such as abs(v)<=f32::MAX may be, letting a NaN or an infinity reach a ray
// query, whose behaviour for one is undefined
// (`wgpu::ShaderRuntimeChecks::ray_query_initialization_tracking`), or the
// walk; a component, sign or NaN payload the test misses; or the interval or
// direction rules wrong. The oracle is how each ray was built: a valid ray
// with one component replaced by a NaN (quiet and signalling, either sign)
// or an infinity of either sign is invalid, as are a negative start, an end
// before the start and a zero direction; extreme finite values stay valid.
#[test]
fn rays_with_a_non_finite_component_or_an_empty_interval_are_invalid() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let valid = [0.5, -2., 3., 0.01, 0., 0.6, -0.8, 100.];
    let mut cases: Vec<([f32; 8], bool)> = vec![
        (valid, true),
        ([0., 0., 0., 0., 0., 0., -1., 0.], true),
        ([0., 0., 0., 0., 0., 0., -1., f32::MAX], true),
        (
            [f32::MAX, -f32::MAX, 0., 0., f32::MIN_POSITIVE, 0., 0., 1.],
            true,
        ),
        ([0., 0., 0., -0., 0., 0., 1., 1.], true),
        ([0., 0., 0., -0.5, 0., 0., -1., 1.], false),
        ([0., 0., 0., 2., 0., 0., -1., 1.], false),
        ([0., 0., 0., 0., 0., 0., 0., 1.], false),
        ([0., 0., 0., 0., -0., 0., -0., 1.], false),
    ];
    let non_finite = [
        f32::NAN,
        -f32::NAN,
        f32::from_bits(0x7f80_0001),
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    for component in 0..8 {
        for value in non_finite {
            let mut ray = valid;
            ray[component] = value;
            cases.push((ray, false));
        }
    }
    let rays: Vec<_> = cases.iter().map(|(ray, _)| *ray).collect();
    let validity = validity((&device, &queue), &rays);
    for ((ray, expected), valid) in cases.iter().zip(validity) {
        let bits = ray.map(f32::to_bits);
        assert_eq!(valid, *expected, "{ray:?} ({bits:x?})");
    }
}
