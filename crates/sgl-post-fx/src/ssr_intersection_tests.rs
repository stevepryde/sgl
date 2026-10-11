//! #344, #537: intersection reads and writes its own outputs, which the
//! denoiser's clamps can hide.
use super::temporal_neighborhood_tests::{
    SIZE, depth, depth_values, device, half, read_rgba16f, texels, texture,
};
use super::*;
use crate::{CameraAttribs, post_fx_context::FrameDesc};

const NEAR: f32 = 0.1;

/// The projection's horizontal and vertical scales, for a vertical field of
/// view of one radian.
fn projection_scales() -> (f32, f32) {
    let aspect = SIZE[0] as f32 / SIZE[1] as f32;
    (1.0 / 0.5f32.tan() / aspect, 1.0 / 0.5f32.tan())
}

/// A camera at the origin looking along +z with an infinite reversed-Z,
/// left-handed projection: depth = NEAR / z.
fn infinite_reversed_camera() -> CameraAttribs {
    let (a, b) = projection_scales();
    // Columns listed.
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
    camera
}

/// The view-space ray through the centre of pixel (`x`, `y`), at view z 1.
fn pixel_ray(x: u32, y: u32) -> [f64; 3] {
    let (a, b) = projection_scales();
    let [width, height] = SIZE.map(f64::from);
    [
        ((f64::from(x) + 0.5) / width * 2.0 - 1.0) / f64::from(a),
        (1.0 - (f64::from(y) + 0.5) / height * 2.0) / f64::from(b),
        1.0,
    ]
}

/// The depths of a plane 1 below `infinite_reversed_camera` with normal
/// `floor_normal`, and of the sky (depth 0) above its horizon.
fn floor_depths(floor_normal: [f64; 3]) -> Vec<f32> {
    (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (x, y)))
        .map(|(x, y)| {
            let ray = pixel_ray(x, y);
            let towards_floor: f64 = (0..3).map(|i| ray[i] * floor_normal[i]).sum();
            // The ray meets dot(p, n) = -1 at view z = -1 / dot(ray, n).
            if towards_floor < 0.0 {
                (f64::from(NEAR) * -towards_floor) as f32
            } else {
                0.0
            }
        })
        .collect()
}

/// One SSR frame of `infinite_reversed_camera` over a mirror floor with
/// normal `floor_normal`, reflecting a white scene.
fn trace_mirror_floor(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    floor_normal: [f64; 3],
    feature_flags: FeatureFlags,
) -> ScreenSpaceReflection {
    let camera = infinite_reversed_camera();
    let mut encoder = device.create_command_encoder(&Default::default());
    let scene_depth = depth_values(device, queue, &mut encoder, &floor_depths(floor_normal));
    let previous_depth = depth(device, &mut encoder, 0.0);
    let normal = texture(
        device,
        queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[
            floor_normal[0] as f32,
            floor_normal[1] as f32,
            floor_normal[2] as f32,
            0.0,
        ]),
    );
    // A mirror everywhere, the sky included.
    let material = texture(device, queue, wgpu::TextureFormat::R8Unorm, &[0]);
    let color = texture(
        device,
        queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[1.0, 1.0, 1.0, 1.0]),
    );
    let motion = texels(
        device,
        queue,
        wgpu::TextureFormat::Rg16Float,
        &vec![0; 4 * (SIZE[0] * SIZE[1]) as usize],
    );
    let mut context = PostFXContext::new(
        device,
        queue,
        post_fx_context::CreateInfo {
            transition_duration: 0.0,
        },
    );
    let mut ssr = ScreenSpaceReflection::new(device);
    context.prepare_resources(
        device,
        &FrameDesc {
            index: 0,
            width: SIZE[0],
            height: SIZE[1],
            output_width: SIZE[0],
            output_height: SIZE[1],
        },
        post_fx_context::FeatureFlags::REVERSED_DEPTH,
    );
    ssr.prepare_resources(device, &mut encoder, &mut context, feature_flags);
    context.execute(&mut post_fx_context::RenderAttributes {
        device,
        queue,
        device_context: &mut encoder,
        curr_depth_buffer_srv: &scene_depth,
        prev_depth_buffer_srv: &previous_depth,
        curr_camera: Some(&camera),
        prev_camera: Some(&camera),
        camera_attribs_cb: None,
        pass_timestamps: None,
    });
    ssr.execute(&mut RenderAttributes {
        device,
        queue,
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
    ssr
}

/// The number of texels of `texels` with a non-finite channel.
fn nonfinite(texels: &[[f32; 4]]) -> usize {
    texels
        .iter()
        .filter(|t| !t.iter().all(|v| v.is_finite()))
        .count()
}

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
    let (_, b) = projection_scales();
    // A floor 1 below a camera pitched down so the horizon lies at NDC y
    // 0.13, between the centres of rows 20 (sky) and 21 (floor), which share
    // 2×2 blocks.
    let (s, c) = (0.13 / f64::from(b)).atan().sin_cos();
    let ssr = trace_mirror_floor(&device, &queue, [0.0, c, -s], FeatureFlags::HALF_RESOLUTION);
    for (name, view) in [
        ("radiance", &ssr.resources().radiance),
        ("direction and PDF", &ssr.resources().ray_direction_pdf),
    ] {
        let rays = read_rgba16f(&device, &queue, view);
        let nonfinite = nonfinite(&rays);
        assert_eq!(nonfinite, 0, "{nonfinite} of {} rays' {name}", rays.len());
    }
}

// PROVENANCE.md DFX-48: a level mirror floor reflects rays into the sky,
// which under an infinite reversed-Z projection end at or just past the far
// plane, depth 0, where the hit's camera-space distance is infinite or past
// RGBA16F's range. Each floor pixel's stored ray still points along the
// mirror direction of its view ray about the floor, and the reconstructed
// output stays finite.
#[test]
fn rays_past_the_far_plane_keep_their_direction() {
    let Some((device, queue)) = device() else {
        return;
    };
    let floor_normal = [0.0, 1.0, 0.0];
    let depths = floor_depths(floor_normal);
    let ssr = trace_mirror_floor(&device, &queue, floor_normal, FeatureFlags::NONE);
    let rays = read_rgba16f(&device, &queue, &ssr.resources().ray_direction_pdf);
    let mut wrong = Vec::new();
    let mut far = 0;
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let at = (y * SIZE[0] + x) as usize;
            if depths[at] == 0.0 {
                continue;
            }
            let ray = rays[at];
            let length = ray[..3].iter().map(|v| v * v).sum::<f32>().sqrt();
            // Farther than any floor point in front of the sky that this
            // camera sees: the ray left through the far plane.
            if length > 1000.0 {
                far += 1;
            }
            let view = pixel_ray(x, y);
            let view_length = view.iter().map(|v| v * v).sum::<f64>().sqrt();
            let mirror = [view[0], -view[1], view[2]].map(|v| (v / view_length) as f32);
            if !(length > 0.0 && (0..3).all(|i| (ray[i] / length - mirror[i]).abs() < 4e-3)) {
                wrong.push(format!("({x}, {y}): {ray:?}, mirror {mirror:?}"));
            }
        }
    }
    assert!(far > 0, "no ray left through the far plane");
    assert!(
        wrong.is_empty(),
        "{} of the floor's rays left their mirror direction:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    let output = read_rgba16f(&device, &queue, ssr.get_ssr_radiance_srv());
    let nonfinite = nonfinite(&output);
    assert_eq!(nonfinite, 0, "{nonfinite} non-finite output pixels");
}
