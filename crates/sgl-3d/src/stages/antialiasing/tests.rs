//! TAA's parameters (`FrameInput::taa`) observed in real frames: an unlit
//! white cube on a black backdrop, its front face square to a camera that
//! stays still or pans across it.
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::{Backdrop, Camera, FrameInput, Scene, TaaParameters, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [100, 70];

/// What the last of six frames left: TAA's input (the composite) and its
/// accumulated output, whose alpha is the history weight the next frame
/// blends with.
struct Frames {
    composite: Vec<[f32; 4]>,
    taa: Vec<[f32; 4]>,
}

impl Frames {
    /// The history weight at the view's centre, on the cube's face.
    fn centre_weight(&self) -> f32 {
        self.taa[(SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) as usize][3]
    }

    /// The largest difference of TAA's colour from its input, relative to
    /// the input above 1.
    fn difference(&self) -> f32 {
        self.composite
            .iter()
            .zip(&self.taa)
            .flat_map(|(input, output)| {
                (0..3).map(|c| (output[c] - input[c]).abs() / input[c].max(1.))
            })
            .fold(0., f32::max)
    }
}

/// Six frames with TAA and `taa`, the camera 2 m in front of the cube's
/// face and moving `pan` metres along +x each frame.
fn frames(device: &wgpu::Device, queue: &wgpu::Queue, taa: TaaParameters, pan: f32) -> Frames {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Taa,
        scene_resolution: settings::SceneResolution::Full,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    let mut cube = test_support::cube();
    cube.materials[0].unlit = true;
    cube.materials[0].base = [1.; 4];
    test_support::add_static(device, queue, &mut scene, cube);
    let output = crate::view::targets::target(device, "TAA frames", SIZE, gbuffer::COLOR);
    for index in 0..6 {
        let eye = Vec3::new(pan * index as f32, 0., 2.);
        let mut input = FrameInput::new(Camera {
            view: Mat4::from_translation(-eye),
            projection: crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1),
            eye,
        });
        input.backdrop = Backdrop::Color([0.; 3]);
        input.taa = taa;
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
    let read = |view: &wgpu::TextureView| {
        test_support::read(device, queue, view.texture(), 8)
            .chunks_exact(8)
            .map(|texel| std::array::from_fn(|c| test_support::half(&texel[c * 2..])))
            .collect()
    };
    Frames {
        composite: read(&renderer.targets().composite),
        taa: read(renderer.taa_output().expect("TAA ran")),
    }
}

// Plausible defects: a history weight in `FrameInput::taa` that does not
// reach TAA's shader (a field left at its DiligentFX default, or read from
// the wrong place in the attributes), or the still-pixel weight applied to
// moving pixels or the reverse. The oracle is the weights' meaning: at 0
// TAA keeps no history, so its output weight is 0 and, after the frame
// that held the last history, its colour is its input's, which the jitter
// changes at the cube's edges. At the defaults history accumulates past the
// weight of a pixel without history (0.5, DiligentFX's), and the edges blend
// earlier frames.
#[test]
fn taa_history_weights_follow_frame_input() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let defaults = TaaParameters::default();

    let still = frames(&device, &queue, defaults, 0.);
    assert!(
        still.centre_weight() > 0.5,
        "a still pixel kept no history (weight {})",
        still.centre_weight()
    );
    assert!(
        still.difference() > 0.01,
        "a still camera's edges blended no history"
    );
    let still_without_history = frames(
        &device,
        &queue,
        TaaParameters {
            still_history_factor: 0.,
            ..defaults
        },
        0.,
    );
    assert_eq!(still_without_history.centre_weight(), 0.);
    assert!(
        still_without_history.difference() < 1e-3,
        "TAA without history differs from its input by {}",
        still_without_history.difference()
    );

    // 0.02 m per frame is about 0.85 pixels: moving, not still.
    let panning = frames(&device, &queue, defaults, 0.02);
    assert!(
        panning.centre_weight() > 0.5,
        "a consistently moving pixel kept no history (weight {})",
        panning.centre_weight()
    );
    let panning_without_history = frames(
        &device,
        &queue,
        TaaParameters {
            temporal_stability_factor: 0.,
            ..defaults
        },
        0.02,
    );
    assert_eq!(panning_without_history.centre_weight(), 0.);
}
