//! `Renderer::compile` against ordinary rendering of the same scene, view,
//! settings and frame input: what each creates, counted by wgpu itself
//! (`wgpu::Instance::generate_report`), and what each draws.
use super::{Renderer, View, ViewFrame, ViewOutput};
use crate::settings::{ContactShadows, Settings};
use crate::{Camera, DirectionalLight, DirectionalShadow, FrameInput, Scene, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 48];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A game's level on a device of its own: an opaque cube, a masked one and
/// a blended one under a sun with two cascades and a point light that
/// casts shadows, seen by one view. The camera's opaque surfaces and the
/// cascades draw from GPU-built lists, the blended cube and the point
/// light's shadow faces from CPU-built ones.
struct Level {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    view: View,
    /// A second view that draws a render texture the opaque cube emits,
    /// where the level has one (`with_feed`).
    feed: Option<(View, crate::RenderTextureId)>,
    /// The opaque cube's material.
    opaque: crate::MaterialId,
    output: wgpu::Texture,
    settings: Settings,
}

impl Level {
    fn new() -> Option<Self> {
        Self::with_opaque(test_support::cube())
    }

    /// The level with `opaque` as its opaque cube.
    fn with_opaque(opaque: crate::asset::Asset) -> Option<Self> {
        let (instance, device, queue) = test_support::counted_device()?;
        let mut scene = Scene::new(&device, &queue);
        let (opaque, _) = test_support::add_static(&device, &queue, &mut scene, opaque);
        let (_, masked) = test_support::add_static(
            &device,
            &queue,
            &mut scene,
            test_support::masked(test_support::cube(), 0.5),
        );
        scene
            .set_instance(
                &queue,
                masked,
                crate::InstanceState {
                    pose: Mat4::from_translation(Vec3::new(1.5, 0., 0.)),
                    ..*scene.instance(masked).unwrap()
                },
            )
            .unwrap();
        let mut blended = test_support::cube();
        blended.materials[0].alpha = crate::AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        };
        blended.materials[0].base[3] = 0.5;
        let (_, blended) = test_support::add_static(&device, &queue, &mut scene, blended);
        scene
            .set_instance(
                &queue,
                blended,
                crate::InstanceState {
                    pose: Mat4::from_translation(Vec3::new(-1.5, 0., 0.)),
                    ..*scene.instance(blended).unwrap()
                },
            )
            .unwrap();
        scene
            .add_light(
                &device,
                &queue,
                crate::Light {
                    position: Vec3::new(0.75, 2., 2.),
                    shape: crate::LightShape::Point {
                        radius: crate::LightShape::DEFAULT_RADIUS,
                    },
                    intensity: 20.,
                    range: 8.,
                    casts_shadow: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let settings = Settings::default();
        let renderer = Renderer::new(&device);
        let view = View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &settings).unwrap();
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("compile test output"),
            size: wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Some(Self {
            instance,
            device,
            queue,
            scene,
            renderer,
            view,
            feed: None,
            opaque: opaque.materials[0],
            output,
            settings,
        })
    }

    /// The level with a render texture its first material emits, drawn by
    /// a second view, which a frame renders before the view that shows it.
    fn with_feed() -> Option<Self> {
        let mut level = Self::new()?;
        let description = crate::RenderTexture::new([16, 16]);
        let texture = level
            .scene
            .add_render_texture(&level.device, description)
            .unwrap();
        let material = level.opaque;
        let values = level.scene.material(material).unwrap();
        level
            .scene
            .set_material(
                &level.queue,
                material,
                crate::SurfaceMaterial {
                    emission_render_texture: Some(texture),
                    ..values
                },
            )
            .unwrap();
        let feed = View::new(
            &level.renderer,
            &level.device,
            &level.queue,
            description.format(),
            [16, 16],
            1.,
            &level.settings,
        )
        .unwrap();
        level.feed = Some((feed, texture));
        Some(level)
    }

    /// The frame's views for `render` or `compile`: the level's view into
    /// `output`, then the feed into its render texture.
    fn views<'a>(
        view: &'a mut View,
        feed: &'a mut Option<(View, crate::RenderTextureId)>,
        (input, settings): (&'a FrameInput, &'a Settings),
        output: &'a wgpu::TextureView,
    ) -> Vec<ViewFrame<'a>> {
        let mut views = vec![ViewFrame {
            view,
            input,
            settings,
            output: ViewOutput::Texture(output),
            timing: None,
        }];
        if let Some((feed, texture)) = feed {
            views.push(ViewFrame {
                view: feed,
                input,
                settings,
                output: ViewOutput::RenderTexture(*texture),
                timing: None,
            });
        }
        views
    }

    /// The frame with the camera `along` its track.
    fn input(along: f32) -> FrameInput {
        let eye = Vec3::new(0.75 + along, 1.5, 5.);
        let mut input = FrameInput::new(Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, Vec3::new(0.75, 0., 0.), Vec3::Y),
            projection: crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1),
            eye,
        });
        input.camera_cut = along == 0.;
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.4, -1., -0.3),
            color: [1.; 3],
            illuminance: 3.,
            shadow: Some(DirectionalShadow {
                distance: 20.,
                cascades: 2,
            }),
            ..Default::default()
        });
        input
    }

    /// One `Renderer::compile` of the frame `input`.
    fn compile(&mut self, input: &FrameInput) -> super::Compilation {
        let output = self.output.create_view(&Default::default());
        self.renderer.compile(
            &self.device,
            &self.queue,
            &mut self.scene,
            &mut Self::views(
                &mut self.view,
                &mut self.feed,
                (input, &self.settings),
                &output,
            ),
        )
    }

    /// Compiles the frame `input` until nothing is pending, running each
    /// job on a thread of its own. Returns how many jobs ran.
    fn compile_all(&mut self, input: &FrameInput) -> usize {
        let mut ran = 0;
        for _ in 0..8 {
            let compilation = self.compile(input);
            assert_eq!(compilation.errors, []);
            if compilation.ready() {
                return ran;
            }
            ran += compilation.jobs.len();
            let workers: Vec<_> = compilation
                .jobs
                .into_iter()
                .map(|job| std::thread::spawn(move || job.run()))
                .collect();
            workers
                .into_iter()
                .for_each(|worker| worker.join().unwrap());
        }
        panic!("the frame was never ready");
    }

    /// Renders, submits and finishes the frame `input`, and returns its
    /// output's bytes.
    fn render(&mut self, input: &FrameInput) -> Vec<u8> {
        self.view.resize(&self.device, SIZE, 1., &self.settings);
        let output = self.output.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &mut Self::views(
                &mut self.view,
                &mut self.feed,
                (input, &self.settings),
                &output,
            ),
        );
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
        test_support::read(&self.device, &self.queue, &self.output, 4)
    }

    /// The pipelines and shader modules wgpu holds for the device.
    fn programs(&self) -> (usize, usize) {
        let report = self.instance.generate_report().unwrap();
        let hub = report.hub_report();
        (
            hub.render_pipelines.num_allocated + hub.compute_pipelines.num_allocated,
            hub.shader_modules.num_allocated,
        )
    }
}

// Plausible defect: `compile` reports ready while the frame still lacks a
// pipeline or shader module (a variant the planning pass did not reach, a
// job's result that never reached its cache), so the first frame compiles
// on the window thread after all. The oracle is wgpu's own count of the
// pipelines and shader modules the device holds: unchanged across a
// compiled level's first frame, and raised by an ordinary one, which
// shows the count sees a frame's creation.
#[test]
fn a_compiled_levels_first_frame_creates_no_program() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let input = Level::input(0.);
    let jobs = compiled.compile_all(&input);
    assert!(
        jobs > 0,
        "the level draws geometry, whose pipelines are jobs"
    );
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    let before = ordinary.programs();
    ordinary.render(&input);
    let after = ordinary.programs();
    assert!(
        after.0 > before.0 && after.1 > before.1,
        "an ordinary first frame creates its programs: {before:?} to {after:?}"
    );
}

// Plausible defect: depth of field, a stage the view did not create its
// pipelines for (it was off when the view was made), left out of the
// planning pass or making them only when it draws, so a level that turns
// it on for a photo mode compiles on the window thread after all. The
// oracle is wgpu's count, as `a_compiled_levels_first_frame_creates_no_program`'s:
// unchanged across the compiled level's first frame with depth of field
// on, which the frame ran.
#[test]
fn a_compiled_level_with_depth_of_field_creates_no_program_at_its_first_frame() {
    let Some(mut compiled) = Level::new() else {
        return;
    };
    compiled.settings.depth_of_field = crate::settings::DepthOfField::High;
    let input = Level::input(0.);
    compiled.compile_all(&input);
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    assert!(
        compiled.view.depth_of_field.output().is_some(),
        "the frame ran depth of field"
    );
}

// Plausible defect: a spot light's measured profile, whose lit and fog
// programs are specialisations a scene without one never builds, left out
// of the planning pass (the fog's injection, which builds its profiled
// variant when a frame first needs it, created only when it draws), so a
// level with a profiled lamp compiles on the window thread after all. The
// oracle is wgpu's count, as `a_compiled_levels_first_frame_creates_no_program`'s:
// unchanged across the compiled level's first frame with a profiled spot
// shining through the fog.
#[test]
fn a_compiled_level_with_a_profiled_spot_in_fog_creates_no_program_at_its_first_frame() {
    let Some(mut compiled) = Level::new() else {
        return;
    };
    let profile = compiled.scene.add_light_profile(
        crate::LightProfile::from_ies(
            "IESNA:LM-63-2002\nTILT=NONE\n1 -1 1 2 2 1 2 0 0 0\n1 1 10\n0 90\n0 360\n1000 500 1000 500\n",
        )
        .unwrap(),
    );
    compiled
        .scene
        .add_light(
            &compiled.device,
            &compiled.queue,
            crate::Light {
                position: Vec3::new(0., 2.5, 0.),
                shape: crate::LightShape::Spot {
                    direction: Vec3::NEG_Y,
                    inner_angle: 0.4,
                    outer_angle: 0.6,
                    radius: 0.,
                },
                intensity: 20.,
                range: 6.,
                profile: Some(crate::SpotProfile {
                    profile,
                    zero_azimuth: Vec3::X,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    compiled.settings.atmosphere = true;
    let mut input = Level::input(0.);
    input.atmosphere = true;
    input.fog.density = 0.05;
    compiled.compile_all(&input);
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
}

// Plausible defect: the planning passes leave something behind that the
// first frames differ by (history advanced, a target written, a stage's
// state changed), a pipeline a job made differs from the one a frame
// makes, or a planning pass asks for a variant no frame draws, so
// compiling costs more than drawing. The oracle is equivalence with
// ordinary rendering on the same inputs: the same bytes in the first
// frame and in the one after the camera moved, and the same number of
// programs held, compiled ahead or not.
#[test]
fn compiled_and_ordinary_frames_are_the_same() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let (first, moved) = (Level::input(0.), Level::input(0.25));
    compiled.compile_all(&first);
    assert_eq!(compiled.render(&first), ordinary.render(&first));
    assert_eq!(compiled.programs(), ordinary.programs());
    assert_eq!(compiled.render(&moved), ordinary.render(&moved));
    assert_eq!(compiled.programs(), ordinary.programs());
}

// Plausible defect: a frame that draws while jobs are neither run nor
// dropped waits for them for ever, draws without their pipelines, or
// compiles them beside the jobs; or `pending` counts requests, not
// programs, so a loading bar jumps (the two cascades draw their casters
// with one pipeline). The oracle is the contract (one job a program,
// `pending` the jobs not finished) and equivalence with ordinary
// rendering: the same bytes and the same programs held, the late jobs
// compiling nothing more.
#[test]
fn a_frame_drawn_before_its_jobs_ran_is_an_ordinary_frame() {
    let (Some(mut early), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let input = Level::input(0.);
    let compilation = early.compile(&input);
    assert!(!compilation.ready());
    // Each program waited for is a job, however many passes and cascades
    // draw with it.
    assert_eq!(compilation.pending, compilation.jobs.len());
    assert_eq!(early.compile(&input).pending, compilation.jobs.len());
    assert_eq!(early.render(&input), ordinary.render(&input));
    compilation.jobs.into_iter().for_each(|job| job.run());
    assert_eq!(early.programs(), ordinary.programs());
    assert!(early.compile(&Level::input(0.25)).ready());
}

// Plausible defect: after a settings change or a resize `compile` reports
// ready from what it compiled before, or compiles everything again. The
// oracle is equivalence with ordinary rendering through the same change:
// contact shadows give the camera's surfaces other pipelines, so there
// are jobs, the changed frame then creates nothing, and both levels hold
// the same programs and draw the same bytes; and the architecture's
// Frame, by which a stage keeps its pipelines across a resize of its
// targets, so after one nothing is pending and none is created.
#[test]
fn a_settings_change_compiles_what_is_new_and_a_resize_nothing() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let (first, moved) = (Level::input(0.), Level::input(0.25));
    compiled.compile_all(&first);
    compiled.render(&first);
    ordinary.render(&first);
    for level in [&mut compiled, &mut ordinary] {
        level.settings.contact_shadows = ContactShadows::Short;
    }
    let before = ordinary.programs();
    let changed = ordinary.render(&moved);
    assert!(
        ordinary.programs().0 > before.0,
        "the change needs programs"
    );
    assert!(compiled.compile_all(&moved) > 0);
    let before = compiled.programs();
    assert_eq!(compiled.render(&moved), changed);
    assert_eq!(compiled.programs(), before);
    assert_eq!(compiled.programs(), ordinary.programs());

    let resized = [SIZE[0] * 2, SIZE[1] * 2];
    compiled
        .view
        .resize(&compiled.device, resized, 1., &compiled.settings);
    let before = compiled.programs();
    assert!(compiled.compile(&moved).ready());
    assert_eq!(compiled.programs(), before);
}

// Plausible defect: a planning pass renders the views as listed, not in
// the frame's order, or leaves out the view that draws a render texture,
// so the programs of the frame's second view (its own stages') are
// created by the first frame. The oracle is wgpu's count, unchanged
// across the compiled first frame, and equivalence with ordinary
// rendering of the same two views: the same bytes and the same programs
// held.
#[test]
fn a_view_that_draws_a_render_texture_is_compiled_with_the_view_that_shows_it() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::with_feed(), Level::with_feed()) else {
        return;
    };
    let input = Level::input(0.);
    compiled.compile_all(&input);
    let before = compiled.programs();
    let drawn = compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    assert_eq!(drawn, ordinary.render(&input));
    assert_eq!(compiled.programs(), ordinary.programs());
}

// Plausible defect: the planning pass reaches a material's pipelines
// without the maps it samples in the shader (`Variant::shader_sampled`),
// so the first frame creates them on the window thread. The opaque cube's
// base map repeats and its emission map, on the same image, clamps, so
// the shader samples the emission map. The oracle is wgpu's count of the
// pipelines and shader modules, unchanged across the compiled first frame.
#[test]
fn a_compiled_frame_with_a_map_sampled_in_the_shader_creates_no_program() {
    use crate::asset::{Image, MaterialTexture, Sampler, TextureWrap};
    let mut cube = test_support::cube();
    cube.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([200; 4]),
    ))];
    cube.materials[0].base_texture = Some(MaterialTexture::new(0));
    cube.materials[0].emissive_texture = Some(MaterialTexture {
        image: 0,
        sampler: Sampler {
            wrap: [TextureWrap::ClampToEdge; 2],
            ..Sampler::default()
        },
    });
    let Some(mut level) = Level::with_opaque(cube) else {
        return;
    };
    let input = Level::input(0.);
    level.compile_all(&input);
    let before = level.programs();
    level.render(&input);
    assert_eq!(level.programs(), before);
}
