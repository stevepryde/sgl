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
    zero_normal: fn(u32, u32) -> bool,
) -> ScreenSpaceReflection {
    let camera = infinite_reversed_camera();
    let mut encoder = device.create_command_encoder(&Default::default());
    let scene_depth = depth_values(device, queue, &mut encoder, &floor_depths(floor_normal));
    let previous_depth = depth(device, &mut encoder, 0.0);
    let floor_normal = floor_normal.map(|v| v as f32);
    let normals: Vec<u8> = (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let [nx, ny, nz] = if zero_normal(x, y) {
                [0.0; 3]
            } else {
                floor_normal
            };
            half(&[nx, ny, nz, 0.0])
        })
        .collect();
    let normal = texels(device, queue, wgpu::TextureFormat::Rgba16Float, &normals);
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
    let ssr = trace_mirror_floor(
        &device,
        &queue,
        [0.0, c, -s],
        FeatureFlags::HALF_RESOLUTION,
        |_, _| false,
    );
    for (name, view) in [
        ("radiance", &ssr.resources().radiance),
        ("direction and PDF", &ssr.resources().ray_direction_pdf),
    ] {
        let rays = read_rgba16f(&device, &queue, view);
        let nonfinite = nonfinite(&rays);
        assert_eq!(nonfinite, 0, "{nonfinite} of {} rays' {name}", rays.len());
    }
}

// PROVENANCE.md DFX-48: a mirror floor reflects every ray into the sky, so
// under an infinite reversed-Z projection each ray leaves through the far
// plane, depth 0, where its camera-space distance is infinite. Rays stepping
// one cell at a time end at or just past depth 0, where the distance is
// infinite or past RGBA16F's range. The camera is pitched up so the horizon
// lies 0.1 pixel above the centre of row 24, the floor's first row. That
// row's rays reach depth 0 0.2 pixel from their origin, so the initial
// advance to the cell boundary half a pixel away overshoots to a clearly
// negative depth, where the distance is finite but meaningless. Each floor
// pixel's stored ray must still point along the mirror direction of its view
// ray about the floor and be as long as RGBA16F holds, and the reconstructed
// output must stay finite.
#[test]
fn rays_past_the_far_plane_keep_their_direction() {
    let Some((device, queue)) = device() else {
        return;
    };
    let (a, b) = projection_scales();
    // Row 24's centre, NDC y -1/48, and 0.1 pixel (0.2/48 in NDC) above it.
    let horizon = -1.0 / 48.0 + 0.2 / 48.0;
    let (s, c) = (horizon / f64::from(b)).atan().sin_cos();
    let floor_normal = [0.0, c, -s];
    let depths = floor_depths(floor_normal);
    let ssr = trace_mirror_floor(&device, &queue, floor_normal, FeatureFlags::NONE, |_, _| {
        false
    });
    let rays = read_rgba16f(&device, &queue, &ssr.resources().ray_direction_pdf);
    let mut wrong = Vec::new();
    let mut row_24_past_far_plane = 0;
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let at = (y * SIZE[0] + x) as usize;
            if depths[at] == 0.0 {
                continue;
            }
            assert!(y >= 24, "({x}, {y}) is floor above row 24");
            let ray = rays[at];
            let length = ray[..3].iter().map(|v| v * v).sum::<f32>().sqrt();
            let view = pixel_ray(x, y);
            let view_length = view.iter().map(|v| v * v).sum::<f64>().sqrt();
            let view = view.map(|v| v / view_length);
            let v_dot_n: f64 = (0..3).map(|i| view[i] * floor_normal[i]).sum();
            let mirror: [f32; 3] =
                std::array::from_fn(|i| (view[i] - 2.0 * v_dot_n * floor_normal[i]) as f32);
            // A ray whose vanishing point, where its depth reaches 0, lies on
            // the screen a pixel or more inside its edges leaves through the
            // far plane before the screen's edge. Its length is binary16's
            // largest finite value, 65504, within the rounding of each stored
            // component near it (a spacing of at most 32).
            let vanishing = [
                f64::from(mirror[0] / mirror[2] * a),
                f64::from(mirror[1] / mirror[2] * b),
            ];
            let past_far_plane = mirror[2] > 0.0
                && vanishing[0].abs() < 1.0 - 2.0 / f64::from(SIZE[0])
                && vanishing[1].abs() < 1.0 - 2.0 / f64::from(SIZE[1]);
            if past_far_plane && y == 24 {
                row_24_past_far_plane += 1;
            }
            // Each component holds about 11 significant bits (a relative
            // 4.9e-4), and the GPU rebuilds the ray's origin from its depth
            // in f32, so the unit direction may differ by a few 1e-4.
            if !((!past_far_plane || (65504.0 - 64.0..=65504.0 + 64.0).contains(&length))
                && (0..3).all(|i| (ray[i] / length - mirror[i]).abs() < 4e-3))
            {
                wrong.push(format!("({x}, {y}): {ray:?}, mirror {mirror:?}"));
            }
        }
    }
    assert!(
        row_24_past_far_plane > 0,
        "no ray of row 24 overshoots the far plane"
    );
    assert!(
        wrong.is_empty(),
        "{} of the floor's rays are not along their mirror direction, as long as RGBA16F holds past the far plane:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    let output = read_rgba16f(&device, &queue, ssr.get_ssr_radiance_srv());
    let nonfinite = nonfinite(&output);
    assert_eq!(nonfinite, 0, "{nonfinite} non-finite output pixels");
}

// PROVENANCE.md DFX-52: a zero world normal, outside the input contract, has
// no tangent frame to sample a reflection from, which made its ray's
// direction NaN. Every fifth pixel of a mirror floor has a zero normal. Each
// ray's radiance, direction and PDF stay finite at full and half resolution,
// and at full resolution each zero-normal floor pixel reports a miss, no ray
// and no radiance, while each unit-normal floor pixel still stores its ray.
#[test]
fn zero_normals_report_a_miss() {
    let Some((device, queue)) = device() else {
        return;
    };
    let (_, b) = projection_scales();
    let (s, c) = (0.13 / f64::from(b)).atan().sin_cos();
    let floor_normal = [0.0, c, -s];
    let zero_normal = |x: u32, y: u32| (x + y).is_multiple_of(5);
    let depths = floor_depths(floor_normal);
    for feature_flags in [FeatureFlags::NONE, FeatureFlags::HALF_RESOLUTION] {
        let ssr = trace_mirror_floor(&device, &queue, floor_normal, feature_flags, zero_normal);
        let radiance = read_rgba16f(&device, &queue, &ssr.resources().radiance);
        let rays = read_rgba16f(&device, &queue, &ssr.resources().ray_direction_pdf);
        for (name, texels) in [("radiance", &radiance), ("direction and PDF", &rays)] {
            let nonfinite = nonfinite(texels);
            assert_eq!(
                nonfinite,
                0,
                "{feature_flags:?}: {nonfinite} of {} rays' {name}",
                texels.len()
            );
        }
        if feature_flags != FeatureFlags::NONE {
            continue;
        }
        let mut wrong = Vec::new();
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                let at = (y * SIZE[0] + x) as usize;
                if depths[at] == 0.0 {
                    continue;
                }
                let traced = rays[at][..3].iter().any(|&v| v != 0.0);
                let missed = rays[at] == [0.0; 4] && radiance[at] == [0.0; 4];
                if zero_normal(x, y) && !missed || !zero_normal(x, y) && !traced {
                    wrong.push(format!(
                        "({x}, {y}): ray {:?}, radiance {:?}",
                        rays[at], radiance[at]
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} floor pixels traced a zero normal or missed a unit one:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }
}
