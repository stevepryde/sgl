//! Motion blur observed in real frames: an unlit square that moves, or does
//! not, across a static camera's view of a black backdrop.
use crate::renderer::Renderer;
use crate::settings::{self, MotionBlur, Settings};
use crate::shading::gbuffer;
use crate::{Backdrop, Camera, FrameInput, InstanceState, Mobility, Scene, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [100, 70];
/// The square's depth and half width in metres.
const DEPTH: f32 = 5.;
const HALF: f32 = 1.2;

fn projection() -> Mat4 {
    crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1)
}

/// An unlit white square facing the camera.
fn square() -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| crate::asset::Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * HALF, y * HALF, -DEPTH],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset.materials[0].unlit = true;
    asset.materials[0].base = [1.; 4];
    asset
}

/// What the last of two frames left: the frame before motion blur, the
/// blurred frame when motion blur ran, and the output.
struct Frames {
    antialiased: Vec<[f32; 4]>,
    blurred: Option<Vec<[f32; 4]>>,
    output: Vec<u8>,
}

fn texels(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|channel| test_support::half(&texel[channel * 2..])))
        .collect()
}

/// How the square moves between the two frames.
#[derive(Clone, Copy, Debug)]
struct Motion {
    /// The world axis it moves along: X (across rows) or Y (down columns).
    axis: Vec3,
    /// Its positions along `axis` in the two frames, in metres.
    positions: [f32; 2],
    /// The second frame is a camera cut.
    cut: bool,
}

/// Two frames of the square, a moving instance, as `motion` moves it, with
/// motion blur at `blur` and an authored `shutter`.
fn frames(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    blur: MotionBlur,
    shutter: f32,
    motion: Motion,
) -> Frames {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        motion_blur: blur,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    let ids = scene.add_asset(device, queue, square()).unwrap();
    let mut state = InstanceState {
        model: ids.model,
        pose: Mat4::IDENTITY,
        visible: true,
        capture_visible: true,
    };
    let instance = scene
        .add_instance(device, queue, state, Mobility::Moving)
        .unwrap();
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: projection(),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.motion_blur.shutter_angle = shutter;
    let output = crate::view::targets::target(device, "motion blur frames", SIZE, gbuffer::COLOR);
    for (index, position) in motion.positions.into_iter().enumerate() {
        state.pose = Mat4::from_translation(motion.axis * position);
        scene.set_instance(queue, instance, state).unwrap();
        input.camera_cut = index == 1 && motion.cut;
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let read =
        |view: &wgpu::TextureView| texels(&test_support::read(device, queue, view.texture(), 8));
    Frames {
        antialiased: read(&renderer.targets().composite),
        blurred: renderer.motion_blurred().map(read),
        output: test_support::read(device, queue, output.texture(), 8),
    }
}

/// Where a point `along` metres down `axis` at the square's depth lands on
/// that axis of the frame, in pixels (columns for X, rows for Y).
fn pixel(axis: Vec3, along: f32) -> f32 {
    let clip = projection() * (axis * along + Vec3::new(0., 0., -DEPTH)).extend(1.);
    if axis.x != 0. {
        (clip.x / clip.w + 1.) / 2. * SIZE[0] as f32
    } else {
        (1. - clip.y / clip.w) / 2. * SIZE[1] as f32
    }
}

// Plausible defects: blur along the wrong axis, with the wrong sign in y or
// the wrong length (pixels against UV, the full motion against the
// shutter's share, the setting's scale lost), blur that stops at a moving
// object's silhouette or at the edge of its tile instead of spreading over
// what is behind it (per-pixel blur's known artifact, or a lost
// neighbourhood maximum), or blur that reaches pixels its motion cannot. The
// oracle is the geometry: the square's motion projected through the camera
// gives its motion in pixels, and a shutter blurs each line of the frame
// before motion blur along that motion by a box of the shutter's share of it
// (McGuire et al. 2012). Lines the square does not cross keep their pixels
// exactly, and so do pixels beyond half the blur from the square. Each
// square ends just short of a 32-pixel tile edge, so its blur on that side
// lies in tiles that hold none of it.
#[test]
fn a_moving_square_blurs_over_the_shutters_share_of_its_motion() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    // Its right edge ends at column 61.9, its blur reaching past 64.
    let across = Motion {
        axis: Vec3::X,
        positions: [-1.77, -0.27],
        cut: false,
    };
    // Moving down, its top edge starts at row 31.7, its blur reaching above 32.
    let down = Motion {
        axis: Vec3::Y,
        positions: [0.555, -0.945],
        cut: false,
    };
    for (motion, blur, shutter, scale) in [
        (across, MotionBlur::Full, 1., 1.),
        (across, MotionBlur::Full, 0.6, 1.),
        (across, MotionBlur::Reduced, 1., 0.5),
        (down, MotionBlur::Full, 1., 1.),
    ] {
        let frames = frames(&device, &queue, blur, shutter, motion);
        let blurred = frames.blurred.expect("motion blur ran");
        let travel =
            pixel(motion.axis, motion.positions[1]) - pixel(motion.axis, motion.positions[0]);
        let length = travel.abs() * shutter * scale;
        let [width, height] = SIZE.map(|v| v as usize);
        // Lines along the motion: rows for X, columns for Y.
        let (lines, span) = if motion.axis.x != 0. {
            (height, width)
        } else {
            (width, height)
        };
        let at = |line: usize, k: usize| {
            if motion.axis.x != 0. {
                line * width + k
            } else {
                k * width + line
            }
        };
        let crossed: Vec<usize> = (0..lines)
            .filter(|&line| (0..span).any(|k| frames.antialiased[at(line, k)][0] > 0.))
            .collect();
        assert!(
            crossed.len() > 20,
            "the square crosses {} lines",
            crossed.len()
        );
        let ends = [-HALF, HALF].map(|side| pixel(motion.axis, motion.positions[1] + side));
        let (first, last) = (ends[0].min(ends[1]), ends[0].max(ends[1]));
        for line in 0..lines {
            let before: Vec<[f32; 4]> =
                (0..span).map(|k| frames.antialiased[at(line, k)]).collect();
            for k in 0..span {
                let after = blurred[at(line, k)];
                let centre = k as f32 + 0.5;
                let outside = centre < first - length / 2. - 1. || centre > last + length / 2. + 1.;
                let label = format!(
                    "{:?} {blur:?} shutter {shutter}: line {line}, pixel {k}",
                    motion.axis
                );
                if !crossed.contains(&line) || outside {
                    assert_eq!(after, before[k], "{label} beyond the blur changed");
                    continue;
                }
                // The line before blur averaged over the blur's box.
                let (start, end) = (centre - length / 2., centre + length / 2.);
                let expected = (0..span)
                    .map(|j| {
                        let overlap = (end.min(j as f32 + 1.) - start.max(j as f32)).max(0.);
                        overlap * before[j][0]
                    })
                    .sum::<f32>()
                    / length;
                assert!(
                    (after[0] - expected).abs() < 0.15,
                    "{label} is {} where a {length:.1} px box gives {expected}",
                    after[0]
                );
            }
        }
    }
}

// Plausible defects: the filter changes pixels that did not move (noise,
// rounding through its weights, tiles that blur without motion), or motion
// that should be zero is not (a static camera's or a resting instance's).
// The oracle is the same frames with motion blur off: a square at rest in
// front of a static camera presents exactly the same output with motion
// blur at full strength.
#[test]
fn a_static_frame_is_unchanged_by_motion_blur() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let resting = Motion {
        axis: Vec3::X,
        positions: [0.3, 0.3],
        cut: false,
    };
    let off = frames(&device, &queue, MotionBlur::Off, 1., resting);
    let full = frames(&device, &queue, MotionBlur::Full, 1., resting);
    assert!(full.blurred.is_some(), "motion blur ran");
    assert!(off.blurred.is_none(), "motion blur did not run while off");
    assert!(off.output == full.output, "a static frame changed");
}

// Plausible defect: a frame that restarts history blurs a moving instance by
// its motion from before the restart, smearing a respawned or teleported
// object across the screen, as TAA would ghost it if it kept history. The
// oracle is the same frames with motion blur off: after a camera cut, the
// moving square presents exactly as it does without motion blur.
#[test]
fn a_camera_cut_frame_is_not_blurred() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let teleported = Motion {
        axis: Vec3::X,
        positions: [-1.77, -0.27],
        cut: true,
    };
    let off = frames(&device, &queue, MotionBlur::Off, 1., teleported);
    let full = frames(&device, &queue, MotionBlur::Full, 1., teleported);
    assert!(off.output == full.output, "a camera-cut frame was blurred");
}
