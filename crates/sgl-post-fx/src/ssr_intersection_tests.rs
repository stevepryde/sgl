//! #344: half-resolution intersection reads its own outputs, which the
//! denoiser's clamps can hide.
use super::temporal_neighborhood_tests::{
    SIZE, depth, depth_values, device, half, read_rgba16f, texels, texture,
};
use super::*;
use crate::{CameraAttribs, post_fx_context::FrameDesc};

// PROVENANCE.md DFX-41: at half resolution a 2×2 block on a glossy floor's
// horizon passes the mask on its floor pixels' depth, while the pixel it
// traces may be the sky, whose depth 0 has no finite position under an
// infinite reversed-Z projection. Every ray's radiance, direction and PDF
// stay finite.
#[test]
fn half_resolution_rays_at_an_infinite_horizon_are_finite() {
    let Some((device, queue)) = device() else {
        return;
    };
    const NEAR: f32 = 0.1;
    let aspect = SIZE[0] as f32 / SIZE[1] as f32;
    let (a, b) = (1.0 / 0.5f32.tan() / aspect, 1.0 / 0.5f32.tan());
    // Infinite reversed-Z, left-handed, columns listed: depth = near / z.
    let proj = [
        a, 0., 0., 0., 0., b, 0., 0., 0., 0., 0., 1., 0., 0., NEAR, 0.,
    ];
    let proj_inv = [
        1. / a,
        0.,
        0.,
        0.,
        0.,
        1. / b,
        0.,
        0.,
        0.,
        0.,
        0.,
        1. / NEAR,
        0.,
        0.,
        1.,
        0.,
    ];
    let identity = [
        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
    ];
    let mut camera = CameraAttribs {
        f4_viewport_size: [
            SIZE[0] as f32,
            SIZE[1] as f32,
            1.0 / SIZE[0] as f32,
            1.0 / SIZE[1] as f32,
        ],
        m_view: identity,
        m_proj: proj,
        m_view_proj: proj,
        m_view_inv: identity,
        m_proj_inv: proj_inv,
        m_view_proj_inv: proj_inv,
        ..Default::default()
    };
    camera.set_clip_planes(f32::INFINITY, NEAR);
    // A floor 1 below a camera pitched down so the horizon lies at NDC y
    // 0.13, between the centres of rows 20 (sky) and 21 (floor), which share
    // 2×2 blocks.
    let (s, c) = (0.13 / f64::from(b)).atan().sin_cos();
    let floor_normal = [0.0, c, -s];
    let [width, height] = SIZE.map(f64::from);
    let depths: Vec<f32> = (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (x, y)))
        .map(|(x, y)| {
            let ray = [
                ((f64::from(x) + 0.5) / width * 2.0 - 1.0) / f64::from(a),
                (1.0 - (f64::from(y) + 0.5) / height * 2.0) / f64::from(b),
                1.0,
            ];
            let towards_floor: f64 = (0..3).map(|i| ray[i] * floor_normal[i]).sum();
            // The ray meets dot(p, n) = -1 at view z = -1 / dot(ray, n).
            if towards_floor < 0.0 {
                (f64::from(NEAR) * -towards_floor) as f32
            } else {
                0.0
            }
        })
        .collect();
    let mut encoder = device.create_command_encoder(&Default::default());
    let scene_depth = depth_values(&device, &queue, &mut encoder, &depths);
    let previous_depth = depth(&device, &mut encoder, 0.0);
    let normal = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[
            floor_normal[0] as f32,
            floor_normal[1] as f32,
            floor_normal[2] as f32,
            0.0,
        ]),
    );
    // A mirror everywhere, the sky included.
    let material = texture(&device, &queue, wgpu::TextureFormat::R8Unorm, &[0]);
    let color = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[1.0, 1.0, 1.0, 1.0]),
    );
    let motion = texels(
        &device,
        &queue,
        wgpu::TextureFormat::Rg16Float,
        &vec![0; 4 * (SIZE[0] * SIZE[1]) as usize],
    );
    let mut context = PostFXContext::new(
        &device,
        &queue,
        post_fx_context::CreateInfo {
            transition_duration: 0.0,
        },
    );
    let mut ssr = ScreenSpaceReflection::new(&device);
    context.prepare_resources(
        &device,
        &FrameDesc {
            index: 0,
            width: SIZE[0],
            height: SIZE[1],
            output_width: SIZE[0],
            output_height: SIZE[1],
        },
        post_fx_context::FeatureFlags::REVERSED_DEPTH,
    );
    ssr.prepare_resources(
        &device,
        &mut encoder,
        &mut context,
        FeatureFlags::HALF_RESOLUTION,
    );
    context.execute(&mut post_fx_context::RenderAttributes {
        device: &device,
        queue: &queue,
        device_context: &mut encoder,
        curr_depth_buffer_srv: &scene_depth,
        prev_depth_buffer_srv: &previous_depth,
        curr_camera: Some(&camera),
        prev_camera: Some(&camera),
        camera_attribs_cb: None,
        pass_timestamps: None,
    });
    ssr.execute(&mut RenderAttributes {
        device: &device,
        queue: &queue,
        device_context: &mut encoder,
        post_fx_context: &mut context,
        color_buffer_srv: &color,
        depth_buffer_srv: &scene_depth,
        normal_buffer_srv: &normal,
        material_buffer_srv: &material,
        motion_vectors_srv: &motion,
        ssr_attribs: &ScreenSpaceReflectionAttribs::default(),
        pass_timestamps: None,
        reset_accumulation: true,
        frame_time: 1.0 / 60.0,
    });
    queue.submit([encoder.finish()]);
    for (name, view) in [
        ("radiance", &ssr.resources().radiance),
        ("direction and PDF", &ssr.resources().ray_direction_pdf),
    ] {
        let rays = read_rgba16f(&device, &queue, view);
        let nonfinite = rays
            .iter()
            .filter(|r| !r.iter().all(|v| v.is_finite()))
            .count();
        assert_eq!(nonfinite, 0, "{nonfinite} of {} rays' {name}", rays.len());
    }
}
