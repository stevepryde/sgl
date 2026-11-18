//! PROVENANCE.md DFX-63: Crystal's production temporal pass over a plane the
//! camera follows along its view axis, as a chase camera follows a craft.
//! The plane keeps its view depth while the camera travels, so its motion
//! carries no screen motion and no depth motion, and the previous frame's
//! depth buffer holds the plane at the same depth. Reprojecting its current
//! point through the previous camera alone places it the camera's travel
//! further away.
use super::temporal_neighborhood_tests::{
    SIZE, camera, depth, device, half, read_rgba16f, read_scalar, standing_behind, texels, texture,
};
use super::*;
use crate::{CameraAttribs, post_fx_context::FrameDesc};

/// The device depth of camera Z `z` under `camera`'s projection, either
/// convention (column-major: the z row of columns 2 and 3, over w = z).
fn device_depth(camera: &CameraAttribs, z: f32) -> f32 {
    camera.m_proj[10] + camera.m_proj[14] / z
}

/// A glossy plane `z` m ahead of the camera, and the previous frame.
struct Followed {
    reversed: bool,
    z: f32,
    /// How far the camera travelled along its axis since the previous frame.
    travel: f32,
    /// The motion texel everywhere: NDC motion, depth motion, validity.
    motion: [f32; 4],
    /// The history radiance (grey) at pixel `(x, y)`, which frame 0 stores.
    history: fn(u32, u32) -> f32,
    /// This frame's resolved radiance (grey) at pixel `(x, y)`.
    current: fn(u32, u32) -> f32,
}

/// Two frames of the production temporal pass, every denoiser tile active:
/// frame 0 resets and stores `history`; at frame 1 the camera had stood
/// `travel` m further back, the plane `z` m ahead of it then too. The
/// reflection's virtual point is the plane's own point, so the hit
/// reprojection is the camera's. Returns frame 1's radiance and variance
/// histories.
fn followed(device: &wgpu::Device, queue: &wgpu::Queue, f: &Followed) -> (Vec<[f32; 4]>, Vec<f32>) {
    let mut ssr = ScreenSpaceReflection::new(device);
    let mut context = PostFXContext::new(device, queue, Default::default());
    let context_flags = if f.reversed {
        post_fx_context::FeatureFlags::REVERSED_DEPTH
    } else {
        post_fx_context::FeatureFlags::NONE
    };
    let current = camera(f.reversed, 0);
    let plane = device_depth(&current, f.z);
    let mut histories = None;
    for index in 0..2 {
        let previous = if index == 0 {
            current
        } else {
            standing_behind(current, f.travel)
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        let scene_depth = depth(device, &mut encoder, plane);
        let previous_depth = depth(device, &mut encoder, plane);
        let normal = texture(
            device,
            queue,
            wgpu::TextureFormat::Rgba16Float,
            &half(&[0., 0., -1., 0.]),
        );
        let material = texture(device, queue, wgpu::TextureFormat::R8Unorm, &[0]);
        let motion = texture(
            device,
            queue,
            wgpu::TextureFormat::Rgba16Float,
            &half(&f.motion),
        );
        context.prepare_resources(
            device,
            &FrameDesc {
                index,
                width: SIZE[0],
                height: SIZE[1],
                output_width: SIZE[0],
                output_height: SIZE[1],
            },
            context_flags,
        );
        ssr.prepare_resources(device, &mut encoder, &mut context, FeatureFlags::NONE);
        ssr.prepare_shaders_and_pso(device);
        context.execute(&mut post_fx_context::RenderAttributes {
            device,
            queue,
            device_context: &mut encoder,
            curr_depth_buffer_srv: &scene_depth,
            prev_depth_buffer_srv: &previous_depth,
            curr_camera: Some(&current),
            prev_camera: Some(&previous),
            camera_attribs_cb: None,
            pass_timestamps: None,
        });
        let radiance = if index == 0 { f.history } else { f.current };
        let input: Vec<u8> = (0..SIZE[1])
            .flat_map(|y| {
                (0..SIZE[0]).flat_map(move |x| {
                    let value = radiance(x, y);
                    half(&[value, value, value, 1.])
                })
            })
            .collect();
        let resources = ssr.resources.as_mut().unwrap();
        resources.resolved_radiance =
            texels(device, queue, wgpu::TextureFormat::Rgba16Float, &input);
        resources.resolved_variance =
            texture(device, queue, wgpu::TextureFormat::R16Float, &half(&[0.25]));
        resources.resolved_depth = texture(
            device,
            queue,
            wgpu::TextureFormat::R32Float,
            &plane.to_le_bytes(),
        );
        resources.denoiser_tiles = texture(device, queue, wgpu::TextureFormat::R8Unorm, &[255]);
        let attribs = ScreenSpaceReflectionAttribs::default();
        let mut render = RenderAttributes {
            device,
            queue,
            device_context: &mut encoder,
            post_fx_context: &mut context,
            color_buffer_srv: &normal,
            depth_buffer_srv: &scene_depth,
            normal_buffer_srv: &normal,
            material_buffer_srv: &material,
            motion_vectors_srv: &motion,
            ssr_attribs: &attribs,
            pass_timestamps: None,
            reset_accumulation: false,
            frame_time: 1. / 60.,
        };
        ssr.update_constant_buffer(&render, false);
        ssr.reset_history(&mut render);
        ssr.compute_stencil_mask_and_extract_roughness(&mut render);
        ssr.compute_temporal_accumulation(&mut render);
        queue.submit([encoder.finish()]);
        let r = ssr.resources();
        let slot = (index & 1) as usize;
        histories = Some((
            read_rgba16f(device, queue, &r.radiance_history[slot]),
            read_scalar(device, queue, &r.variance_history[slot]),
        ));
    }
    histories.unwrap()
}

fn checkerboard(x: u32, y: u32) -> f32 {
    ((x + y) % 2) as f32
}

/// A grey that rises by `RAMP` a pixel to the right.
const RAMP: f32 = 0.01;
fn ramp(x: u32, _: u32) -> f32 {
    0.25 + RAMP * x as f32
}

// Plausible defect: the disocclusion test compares the history's depth with
// the surface's current point reprojected through the previous camera, which
// holds only for a surface at rest. The plane is 2 m ahead now and was 2 m
// ahead of the previous camera, which stood 10 m further back; its current
// point lies 12 m from that camera, a relative difference of 10/12 from the
// 2 m history, beyond what the fallback search accepts (79.9 %), so no
// position supplies history. A uniform 0.75 history under a checkerboard of
// 0 and 1, whose 3x3 box (mean ± 2σ) holds 0.75 at every pixel, must carry
// over: radiance 0.05 × current + 0.95 × 0.75 and variance
// 0.1 × 0.25 + 0.9 × 1 = 0.925 (the reset frame's variance is 1), under
// both depth conventions. A pixel that keeps no history stores the current
// radiance and variance 1.
#[test]
fn a_surface_the_camera_follows_keeps_its_reflection_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let (radiance, variance) = followed(
            &device,
            &queue,
            &Followed {
                reversed,
                z: 2.,
                travel: 10.,
                motion: [0., 0., 0., 1.],
                history: |_, _| 0.75,
                current: |x, y| checkerboard(x, y),
            },
        );
        let mut lost = 0;
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                let pixel = (y * SIZE[0] + x) as usize;
                let expected = 0.05 * checkerboard(x, y) + 0.95 * 0.75;
                // Binary16 steps at these values are under 0.001.
                if (radiance[pixel][0] - expected).abs() > 0.002
                    || (variance[pixel] - 0.925).abs() > 0.002
                {
                    lost += 1;
                }
            }
        }
        assert_eq!(
            lost, 0,
            "reversed={reversed}: {lost} pixels of the followed plane lost their history"
        );
    }
}

// Plausible defect: when the depth test fails at the reprojected position,
// the fallback search takes the first neighbour that passes, rows from -1
// and columns from -1, so on a uniform surface it reads history one pixel
// up and left, and a surface whose test fails every frame drifts. The plane
// is 8 m ahead now and was 8 m ahead of the previous camera, 2 m further
// back, and its motion carries no valid depth motion (W = 0), so the test
// takes the camera's reprojection: 10 m against the 8 m history, which fails
// at every position (exp(-0.2) < 0.9) and passes the search's halved
// threshold at every one. History and current radiance are the same ramp in
// x, so the incident point (this pixel) agrees with its neighbourhood and
// the camera-reprojected hit, nearer the centre, does not. Read at its own
// position the history gives the ramp at that pixel back; read one pixel to
// the left it is 0.95 × RAMP lower, which the 3x3 box (± 1.63 × RAMP)
// admits. Checked in the interior, at least 4 pixels from the centre column
// where the hit reprojection lands within 0.8 pixel of the pixel itself,
// within 0.4 × RAMP.
#[test]
fn the_fallback_search_keeps_a_followed_surfaces_history_in_place() {
    let Some((device, queue)) = device() else {
        return;
    };
    let (radiance, variance) = followed(
        &device,
        &queue,
        &Followed {
            reversed: false,
            z: 8.,
            travel: 2.,
            motion: [0., 0., 0., 0.],
            history: ramp,
            current: ramp,
        },
    );
    let mut drifted = Vec::new();
    for y in 1..SIZE[1] - 1 {
        for x in 1..SIZE[0] - 1 {
            if (x as f32 + 0.5 - SIZE[0] as f32 / 2.).abs() < 4. {
                continue;
            }
            let pixel = (y * SIZE[0] + x) as usize;
            assert!(
                variance[pixel] < 1.,
                "({x},{y}) kept no history: the search should accept the plane"
            );
            if (radiance[pixel][0] - ramp(x, y)).abs() > 0.4 * RAMP {
                drifted.push((x, y, ramp(x, y), radiance[pixel][0]));
            }
        }
    }
    assert!(
        drifted.is_empty(),
        "{} pixels read history from another position, first (x, y, expected, got): {:?}",
        drifted.len(),
        drifted.first()
    );
}
