//! Which frames of a TAA view bind the temporal masks, run the temporal
//! composition pass and hold the mask targets, for a game's shader, normal
//! layers and a blended surface, observed in real frames (the register's
//! F-15). Screens are `renderer::render_texture_tests`', sprites
//! `sprites_tests`', instance footprints `footprints_tests`' and
//! `renderer::compile_tests`', and planar reflectors
//! `renderer::planar_reflection_tests`'.
use super::composition_tests::{
    CUTTING_SHADER, MOVING, SIZE, STILL, TIMED_SHADER, add_shader, glass, layered, place, shaded,
    square, temporal_renderer,
};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::timing::GpuTiming;
use crate::{Camera, FrameInput, Scene, test_support};
use glam::{Mat4, Quat, Vec3};

/// What a frame did with the temporal masks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Masks {
    /// The frame bound them: its draws wrote them and TAA's resolve read
    /// them.
    bound: bool,
    /// The GPU ran the temporal composition pass.
    composition: bool,
    /// The view holds the two mask targets after the frame.
    held: bool,
}

/// Nothing marked them yet.
const NONE: Masks = Masks {
    bound: false,
    composition: false,
    held: false,
};
/// Nothing marks them, and the targets made before are kept.
const KEPT: Masks = Masks {
    bound: false,
    composition: false,
    held: true,
};
/// A surface whose shading changes marks them in the temporal composition.
const COMPOSED: Masks = Masks {
    bound: true,
    composition: true,
    held: true,
};
/// A blended surface marks them where it draws.
const BLENDED: Masks = Masks {
    bound: true,
    composition: false,
    held: true,
};

/// Ahead, down -Z, where the fixtures stand.
const AHEAD: Quat = Quat::IDENTITY;

/// A device that times passes where its adapter can.
fn timed_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    test_support::device_choosing(|adapter| {
        crate::graphics_device::features(adapter)
            | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
    })
}

/// A TAA view's frames from the origin, a second apart, continuing its
/// history after the first, timed where the adapter has timestamp queries.
struct Frames {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: TestRenderer,
    settings: Settings,
    timing: Option<GpuTiming>,
    output: wgpu::TextureView,
    time: f64,
}

impl Frames {
    /// None where TAA does not run.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let (renderer, settings) = temporal_renderer(device, queue, settings::Antialiasing::Taa)?;
        let timing = GpuTiming::new(device, queue);
        if timing.is_none() {
            eprintln!("the adapter has no timestamp queries: the passes run go unchecked");
        }
        Some(Self {
            device: device.clone(),
            queue: queue.clone(),
            renderer,
            settings,
            timing,
            output: crate::view::targets::target(device, "writers frame", SIZE, gbuffer::COLOR),
            time: 0.,
        })
    }

    /// Renders a frame of `scene` facing `facing` and asserts that it did
    /// with the masks what `expected` says (`what`): whether it ran the
    /// temporal composition pass only where the frame is timed.
    fn expect(&mut self, scene: &mut Scene, facing: Quat, expected: Masks, what: &str) {
        let (bound, held, composition) = self.render(scene, facing);
        let actual = Masks {
            bound,
            held,
            composition: composition.unwrap_or(expected.composition),
        };
        assert_eq!(actual, expected, "{what}");
    }

    /// A frame of `scene` facing `facing`: whether it bound the masks,
    /// whether the view holds them after it, and, where it is timed,
    /// whether the GPU ran the temporal composition pass, which its pass
    /// timings, read back once it completes, a resolve and a map later,
    /// name.
    fn render(&mut self, scene: &mut Scene, facing: Quat) -> (bool, bool, Option<bool>) {
        self.time += 1.;
        let mut input = FrameInput::new(Camera {
            view: Mat4::from_quat(facing),
            projection: crate::perspective(1., 1., 0.1),
            eye: Vec3::ZERO,
        });
        input.elapsed_seconds = self.time;
        if let Some(timing) = &mut self.timing {
            let _ = timing.begin_frame(&self.device, &self.queue);
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            scene,
            &input,
            &self.settings,
            &self.output,
            self.timing.as_ref(),
        );
        self.queue.submit([encoder.finish()]);
        if let Some(timing) = &mut self.timing {
            timing.submitted(&self.queue);
        }
        self.renderer.finish_frame(scene);
        let targets = self.renderer.targets();
        let bound = targets.temporal_masks().is_some();
        let held = targets.held_temporal_masks().is_some();
        let Some(timing) = &mut self.timing else {
            return (bound, held, None);
        };
        for _ in 0..8 {
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            if let Some(frame) = timing.begin_frame(&self.device, &self.queue).next_back() {
                let composition = frame
                    .passes
                    .iter()
                    .any(|pass| pass.name == "temporal composition");
                return (bound, held, Some(composition));
            }
        }
        panic!("the frame's timings did not complete");
    }
}

// Plausible defects (#877): a TAA frame binds the masks and runs the
// temporal composition pass for a game's shader that reads no time and
// whose parameters did not change, as every frame did while the scene
// merely held one; or binds none in the frame its parameters were edited,
// or in a frame of a shader that reads the time; or the targets are
// dropped and made again as that flips. The oracle is the register's F-15
// with M-16: a frame binds the masks, and runs the pass, exactly when its
// view's list holds a material whose shading changes in it (its edit's
// frame; every frame of a shader that reads the time), and the targets,
// made at the first, are kept unbound after.
#[test]
fn a_game_shader_binds_the_masks_only_in_frames_its_shading_changes() {
    let Some((device, queue)) = timed_device() else {
        return;
    };
    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let still = add_shader(&mut scene, CUTTING_SHADER);
    let at = (square(-3., 0.4), Vec3::ZERO);
    let (material, _) = shaded((&device, &queue), &mut scene, at, still, [0.; 4]);
    // The first frame restarts the view's history; nothing it reads
    // changes after it.
    for frame in 0..2 {
        frames.expect(&mut scene, AHEAD, NONE, &format!("frame {frame}"));
    }
    let edit = [0.25_f32, 0., 0., 0.];
    scene
        .set_shader_parameters(&queue, material, bytemuck::bytes_of(&edit))
        .unwrap();
    frames.expect(&mut scene, AHEAD, COMPOSED, "its edit's frame");
    frames.expect(&mut scene, AHEAD, KEPT, "the frame after");

    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let timed = add_shader(&mut scene, TIMED_SHADER);
    let at = (square(-3., 0.4), Vec3::ZERO);
    shaded((&device, &queue), &mut scene, at, timed, [0.; 4]);
    for frame in 0..3 {
        frames.expect(
            &mut scene,
            AHEAD,
            COMPOSED,
            &format!("frame {frame} of a shader reading the time"),
        );
    }
}

// Plausible defects (#877): a TAA frame binds the masks and runs the
// temporal composition pass for normal layers that stand still, or binds
// none while they move; or the scene's count of moving materials lags an
// edit. The oracle is the register's F-15 with M-16: layers that move
// change the material's shading in every frame, still ones in none.
#[test]
fn moving_normal_layers_bind_the_masks_and_still_ones_do_not() {
    let Some((device, queue)) = timed_device() else {
        return;
    };
    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let (ids, _, _) = place(
        &device,
        &queue,
        &mut scene,
        layered(STILL, false),
        Vec3::ZERO,
    );
    let material = ids.materials[0];
    let set = |scene: &mut Scene, layers| {
        let values = scene.material(material).unwrap();
        scene
            .set_material(
                &queue,
                material,
                crate::SurfaceMaterial {
                    normal_layers: Some(layers),
                    ..values
                },
            )
            .unwrap();
    };
    for frame in 0..2 {
        frames.expect(&mut scene, AHEAD, NONE, &format!("still, frame {frame}"));
    }
    set(&mut scene, MOVING);
    for frame in 0..2 {
        frames.expect(
            &mut scene,
            AHEAD,
            COMPOSED,
            &format!("moving, frame {frame}"),
        );
    }
    set(&mut scene, STILL);
    frames.expect(&mut scene, AHEAD, KEPT, "still again");
}

// Plausible defects (#877): a TAA frame binds the masks for a blended
// surface the scene holds out of the view, as every frame did while the
// scene merely held one; or binds none while the view draws it; or runs
// the temporal composition pass for it. The oracle is the register's F-15
// with M-11: a blended surface marks the masks in the frames its view's
// blended list holds it, in the blended pass.
#[test]
fn a_blended_surface_binds_the_masks_only_while_the_view_draws_it() {
    let Some((device, queue)) = timed_device() else {
        return;
    };
    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    place(&device, &queue, &mut scene, glass(), Vec3::ZERO);
    let behind = Quat::from_rotation_y(std::f32::consts::PI);
    frames.expect(&mut scene, AHEAD, BLENDED, "in view");
    frames.expect(&mut scene, behind, KEPT, "behind the view");
    frames.expect(&mut scene, AHEAD, BLENDED, "in view again");
}
