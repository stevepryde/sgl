//! Depth of field's passes on fixture frames and depths, and its resources
//! in real frames. Its focus is observed frame by frame in the scenario
//! suite (`tests/sgl-3d/tests/scenario_depth_of_field.rs`).
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, DepthOfField, Settings};
use crate::shading::gbuffer;
use crate::test_support;
use crate::view::effective::DepthOfFieldFrame;
use crate::{Backdrop, Camera, FrameInput, Scene};
use glam::{Mat4, Vec3};

/// `size`'s depth target, every pixel left of column `split` at device
/// depth `depths[0]` and the rest at `depths[1]`.
fn split_depth(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    split: u32,
    depths: [f32; 2],
) -> wgpu::TextureView {
    let depth = crate::view::targets::target(
        device,
        "depth of field fixture depth",
        size,
        wgpu::TextureFormat::Depth32Float,
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("depth of field fixture depth"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {{
                  let uv=vec2<f32>(f32((i<<1u)&2u),f32(i&2u));
                  return vec4(uv*2.0-1.0,0.0,1.0);
                }}
                @fragment fn fs(@builtin(position) p:vec4<f32>)->@builtin(frag_depth) f32 {{
                  return select({:?},{:?},p.x>={split}.0);
                }}",
                depths[0], depths[1]
            )
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("depth of field fixture depth"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
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
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[],
        }),
        multiview_mask: None,
        cache: None,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
    depth
}

fn texels(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|channel| test_support::half(&texel[channel * 2..])))
        .collect()
}

// Plausible defect: depth of field turning a texel of +infinity, which an
// RGBA16F frame holds where radiance passed its range on a backend that
// rounds overflow to infinity (#464) and which reaches it unclamped when
// neither TAA nor FSR ran, into NaN or spreading it as infinity: its
// prefilter and gather sum weighted colours, a weight of 0 times infinity
// is NaN, and every pixel whose disc reaches the texel would carry it. The
// frame's left part lies 0.5 m away and its right 10 m, under a lens
// focused at 10 m, blurring the near part by about 13 pixels, so tiles of
// every form (early exit on the right, the
// cheap average on the left and the weighted gather at the split) read an
// infinite texel. Oracle: RD-4's finite-output invariant over the focused
// frame.
#[test]
fn infinite_texels_focus_into_a_finite_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = [160u32, 64];
    let inf = f32::INFINITY;
    let infinite = [
        ([10, 20], [inf, inf, inf]),
        ([30, 40], [inf, 0.5, 0.25]),
        ([78, 30], [0.25, inf, 0.5]),
        ([82, 33], [0.5, 0.25, inf]),
        ([150, 10], [inf, inf, inf]),
    ];
    let frame_texels: Vec<_> = (0..size[1])
        .flat_map(|y| (0..size[0]).map(move |x| [x, y]))
        .map(|texel| {
            let [r, g, b] = infinite
                .iter()
                .find(|(at, _)| *at == texel)
                .map_or([0.2, 0.1, 0.05], |(_, color)| *color);
            [r, g, b, 1.]
        })
        .collect();
    let frame = test_support::hdr_texture(&device, &queue, size, &frame_texels);
    let near = 0.1;
    // Device depth is near over distance.
    let depth = split_depth(&device, &queue, size, 80, [near / 0.5, near / 10.]);
    let lens = DepthOfFieldFrame {
        focus_distance: 10.,
        f_number: 1.,
        sensor_height: 0.012,
        focal_length: 0.05,
        rings: 4,
    };
    let uniform = super::DepthOfFieldUniform::new(&lens, near, size, size);
    let mut filter = super::Filter::new(&crate::view::compile::Creator::now(&device)).now();
    let mut encoder = device.create_command_encoder(&Default::default());
    filter.dispatch(
        (&device, &queue),
        &mut encoder,
        [&frame, &depth],
        uniform,
        None,
    );
    queue.submit([encoder.finish()]);
    let output = &filter.targets.as_ref().unwrap().output;
    let focused = texels(&test_support::read(&device, &queue, output.texture(), 8));
    let non_finite: Vec<_> = focused
        .iter()
        .enumerate()
        .filter(|(_, texel)| !texel.iter().all(|value| value.is_finite()))
        .map(|(index, texel)| (index as u32 % size[0], index as u32 / size[0], *texel))
        .collect();
    assert!(
        non_finite.is_empty(),
        "{} non-finite pixels, first {:?}",
        non_finite.len(),
        non_finite.first()
    );
}

// Plausible defects: a view that never turns depth of field on still
// creates its pipelines or targets, or one that turns it off keeps its
// full- and half-size targets (the spec: off, it holds no target). The
// oracle is the stage's own holdings after real frames: none before it
// runs, a focused frame while it runs, none after a frame with it off.
#[test]
fn depth_of_field_holds_targets_only_while_it_runs() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = [64u32, 48];
    let off = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let on = Settings {
        depth_of_field: DepthOfField::Medium,
        ..off
    };
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &off);
    assert!(
        renderer.view.depth_of_field.0.ready().is_none(),
        "a view created with depth of field off made its pipelines"
    );
    let mut scene = Scene::new(&device, &queue);
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.5; 3]);
    let output =
        crate::view::targets::target(&device, "depth of field frames", size, gbuffer::COLOR);
    let mut frame = |settings: &Settings, renderer: &mut TestRenderer| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    };
    frame(&off, &mut renderer);
    assert!(renderer.view.depth_of_field.output().is_none());
    frame(&on, &mut renderer);
    let focused = renderer.view.depth_of_field.output().expect("it ran");
    assert_eq!(
        [focused.texture().width(), focused.texture().height()],
        size
    );
    frame(&off, &mut renderer);
    assert!(
        renderer.view.depth_of_field.output().is_none(),
        "the targets outlived depth of field"
    );
}

// Plausible defect: motion blur keeps a bind group for each frame view it
// blurred, and depth of field makes a new focused frame each time it turns
// on and lets it go each time it turns off, so a game toggling it (a photo
// mode entered and left) grows motion blur's groups without end and keeps
// every focused frame they bind alive. The oracle is the count the cache
// is for: one frame view blurred at a time, so after toggling depth of
// field on and off three times under motion blur the cache holds what the
// last frame bound, at most one group.
#[test]
fn toggling_depth_of_field_keeps_motion_blurs_groups_bounded() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = [64u32, 48];
    let off = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        motion_blur: settings::MotionBlur::Full,
        ..Settings::default()
    };
    let on = Settings {
        depth_of_field: DepthOfField::Medium,
        ..off
    };
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &off);
    let mut scene = Scene::new(&device, &queue);
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.5; 3]);
    let output =
        crate::view::targets::target(&device, "depth of field frames", size, gbuffer::COLOR);
    for settings in [&off, &off, &on, &off, &on, &off, &on, &off] {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let groups = renderer.view.motion_blur.cached_groups();
    assert!(groups <= 1, "motion blur keeps {groups} groups");
}

/// A square facing +Z at depth `z` from x = `left` to `right` metres and
/// `half` metres above and below the view's centre, drawn with `material`.
fn square(
    left: f32,
    right: f32,
    half: f32,
    z: f32,
    material: crate::asset::Material,
) -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(left, -half), (right, -half), (right, half), (left, half)]
            .map(|(x, y)| crate::asset::Vertex {
                uv1: [0.0; 2],
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, y, z],
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset.materials = vec![material];
    asset
}

// Plausible defect: depth of field reads the surface depth, which is the
// opaque depth unless the receiver pass draws the scene's receivers over
// it, and that pass runs only for the stages the effective configuration
// names; without depth of field among them a receiver (glass, water) in
// front of an in-focus floor is focused at the floor's depth, sharp, when
// no screen-space method, TAA, FSR or motion blur runs. The oracle is the
// thin lens: a dark receiver 1 m ahead of a lens focused at 4 m, over a
// white floor at 4 m, blurs by about 7 pixels, so the floor's pixels beside
// its edge darken; focused at the floor's depth it would leave them as they
// are.
#[test]
fn a_receiver_is_focused_at_its_own_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = [96u32, 64];
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        depth_of_field: DepthOfField::Medium,
        ..Settings::default()
    };
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &settings);
    let mut scene = Scene::new(&device, &queue);
    let unlit = |base| crate::asset::Material {
        base,
        unlit: true,
        ..Default::default()
    };
    test_support::add_static(
        &device,
        &queue,
        &mut scene,
        square(-8., 8., 8., -4., unlit([1.; 4])),
    );
    let receiver = crate::asset::Material {
        alpha: crate::AlphaMode::Blend {
            receives_screen_space_reflections: true,
            keeps_specular: false,
        },
        ..unlit([0., 0., 0., 0.95])
    };
    // The left half of the view, 1 m ahead.
    test_support::add_static(
        &device,
        &queue,
        &mut scene,
        square(-2., 0., 2., -1., receiver),
    );
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.5; 3]);
    // At cot(0.5) = 1.83 the focal length is 0.183 m; the receiver's circle
    // of confusion is f² |1 − 4| / (N (4 − f) h) × 64 px ≈ 8.4 px across.
    input.depth_of_field = crate::DepthOfFieldParameters {
        focus_distance: 4.,
        f_number: 1.,
        sensor_height: 0.2,
    };
    let output =
        crate::view::targets::target(&device, "depth of field frames", size, gbuffer::COLOR);
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
    let read =
        |view: &wgpu::TextureView| texels(&test_support::read(&device, &queue, view.texture(), 8));
    let composite = read(&renderer.targets().composite);
    let focused = read(renderer.view.depth_of_field.output().expect("it ran"));
    let row = size[1] / 2;
    // The receiver's edge is at the view's centre column; the floor's
    // first pixel beside it.
    let beside = (row * size[0] + size[0] / 2) as usize;
    assert!(
        composite[beside][0] > 0.9,
        "the floor beside the receiver is {:?} before focusing",
        composite[beside]
    );
    assert!(
        focused[beside][0] < 0.9 * composite[beside][0],
        "the floor beside the 1 m receiver is {:?} focused, {:?} before",
        focused[beside],
        composite[beside]
    );
}
