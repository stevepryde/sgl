//! Programmable surfaces (`Scene::add_shader`) observed at real boundaries:
//! frames rendered, submitted and finished through `Renderer::render`, and
//! their depth, surface depth, motion and composite read back. Each oracle
//! is the test's own analytic surface, projected through its own camera:
//! the fixture shader (`test_support::TEST_SHADER`) moves a flat quad along
//! a direction by its parameters, its time, its vertices' data and its
//! instance's, so the displaced quad is a plane the CPU intersects each
//! pixel's ray with.
use crate::asset::{self, Vertex};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::test_support::{self, TestShaderParams, add_test_shader, read, set_test_params, shaded};
use crate::{
    AlphaMode, Camera, FrameInput, InstanceId, InstanceState, MaterialId, MaterialShader, Mobility,
    ModelMesh, PreparedModel, Scene, SceneError, ShaderSource,
};
use glam::{DVec3, DVec4, Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];

/// Settings that leave the frame's targets as the geometry wrote them: no
/// antialiasing (no jitter), bloom, ambient occlusion or atmosphere.
fn settings() -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        ..Settings::default()
    }
}

/// `settings` with screen-space reflections, whose receivers write the
/// surface depth.
fn receiving() -> Settings {
    Settings {
        screen_space_reflections: settings::ScreenSpaceReflections::Full,
        reflection_method: settings::ReflectionMethod::Velvet,
        ..settings()
    }
}

/// A renderer whose frames are rendered, submitted and finished as a
/// game's are, and its scene.
struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    settings: Settings,
    output: wgpu::TextureView,
    scene: Scene,
}

impl Harness {
    fn new(settings: Settings) -> Option<Self> {
        Some(Self::on(test_support::device()?, settings))
    }

    fn on((device, queue): (wgpu::Device, wgpu::Queue), settings: Settings) -> Self {
        let renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        let output = crate::view::targets::target(
            &device,
            "shader frames",
            SIZE,
            crate::shading::gbuffer::COLOR,
        );
        let scene = Scene::new(&device, &queue);
        Self {
            device,
            queue,
            renderer,
            settings,
            output,
            scene,
        }
    }

    /// Renders, submits and finishes a frame of `input`.
    fn frame(&mut self, input: &FrameInput) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            input,
            &self.settings,
            &self.output,
            None,
        );
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
    }

    /// A depth texture's texels.
    fn depths(&self, texture: &wgpu::Texture) -> Vec<f32> {
        bytemuck::cast_slice(&read(&self.device, &self.queue, texture, 4)).to_vec()
    }

    /// The opaque depth.
    fn depth(&self) -> Vec<f32> {
        self.depths(self.renderer.targets().depth.texture())
    }

    /// The surface depth: the opaque depth with the receivers over it.
    fn surface_depth(&self) -> Vec<f32> {
        let surface = self.renderer.targets().surface.as_ref().expect("receivers");
        self.depths(surface.depth.texture())
    }

    /// The motion target's texels.
    fn motion(&self) -> Vec<[f32; 2]> {
        let texels = read(
            &self.device,
            &self.queue,
            self.renderer.targets().motion.texture(),
            4,
        );
        texels
            .chunks_exact(4)
            .map(|texel| [test_support::half(texel), test_support::half(&texel[2..])])
            .collect()
    }

    /// The composite's texels, HDR before exposure and tone mapping.
    fn composite(&self) -> Vec<[f32; 4]> {
        let texels = read(
            &self.device,
            &self.queue,
            self.renderer.targets().composite.texture(),
            8,
        );
        texels
            .chunks_exact(8)
            .map(|texel| std::array::from_fn(|channel| test_support::half(&texel[channel * 2..])))
            .collect()
    }

    /// A material of `values`, with its identity.
    fn material(&mut self, values: asset::Material) -> MaterialId {
        let (device, queue) = (&self.device, &self.queue);
        self.scene
            .add_materials(device, queue, &[values], &[])
            .unwrap_or_else(|error| panic!("{error}"))[0]
    }

    /// A static instance of `mesh`'s quad drawn with `material`, its
    /// vertices carrying `data` (none where empty), with `instance` its
    /// shader data.
    fn place(
        &mut self,
        (vertices, data): (Vec<Vertex>, Vec<[f32; 4]>),
        material: MaterialId,
        instance: [f32; 4],
    ) -> InstanceId {
        let (device, queue) = (&self.device, &self.queue);
        let mesh = ModelMesh {
            vertices,
            indices: vec![0, 1, 2, 0, 2, 3],
            material,
            deformation: Default::default(),
        };
        let prepared = PreparedModel::with_shader_data(vec![mesh], vec![data]).unwrap();
        let model = self.scene.add_model(device, queue, prepared).unwrap();
        let id = self
            .scene
            .add_instance(device, queue, InstanceState::new(model), Mobility::Static)
            .unwrap();
        self.scene
            .set_instance_shader_data(queue, id, instance)
            .unwrap();
        id
    }
}

/// A quad in the plane y = 0 over `x` and `z`, facing +Y, its vertices'
/// shader data `slope` times their x.
fn ground(x: [f32; 2], z: [f32; 2], slope: f32) -> (Vec<Vertex>, Vec<[f32; 4]>) {
    let corners = [(x[0], z[1]), (x[1], z[1]), (x[1], z[0]), (x[0], z[0])];
    let vertices = corners
        .map(|(x, z)| Vertex {
            position: [x, 0., z],
            normal: [0., 1., 0.],
            color: [1.; 4],
            lightmap_bounds: [0., 0., 1., 1.],
            ..bytemuck::Zeroable::zeroed()
        })
        .to_vec();
    let data = corners.map(|(x, _)| [slope * x, 0., 0., 0.]).to_vec();
    (vertices, data)
}

/// A double-sided lit white material through `shader`'s vertices moved at
/// most `bound`.
fn opaque(shader: crate::ShaderId, bound: f32) -> asset::Material {
    shaded(
        asset::Material {
            base: [0.8, 0.8, 0.8, 1.],
            metallic: 0.,
            double_sided: true,
            ..Default::default()
        },
        shader,
        bound,
    )
}

/// A blended receiver of screen-space reflections through `shader`.
fn receiver(shader: crate::ShaderId, bound: f32) -> asset::Material {
    asset::Material {
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: true,
            keeps_specular: true,
        },
        ..opaque(shader, bound)
    }
}

/// A camera above and in front of the origin, looking at it.
fn looking_down() -> Camera {
    let eye = Vec3::new(0., 4., 6.);
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    }
}

/// A frame of `camera` at `time`, with a black backdrop.
fn frame(camera: Camera, time: f64) -> FrameInput {
    let mut input = FrameInput::new(camera);
    input.elapsed_seconds = time;
    input.backdrop = crate::Backdrop::Color([0.; 3]);
    input.baked_lighting = false;
    input
}

/// The ray of `camera` through the centre of pixel `pixel`: its origin on
/// the near plane and its direction, in f64.
fn ray(camera: &Camera, [x, y]: [u32; 2]) -> (DVec3, DVec3) {
    let clip_from_world = (camera.projection * camera.view).as_dmat4();
    let world_from_clip = clip_from_world.inverse();
    let ndc_x = (f64::from(x) + 0.5) / f64::from(SIZE[0]) * 2. - 1.;
    let ndc_y = 1. - (f64::from(y) + 0.5) / f64::from(SIZE[1]) * 2.;
    let at = |depth: f64| {
        let p = world_from_clip * DVec4::new(ndc_x, ndc_y, depth, 1.);
        p.truncate() / p.w
    };
    let (near, beyond) = (at(1.), at(0.5));
    (near, (beyond - near).normalize())
}

/// Where `ray` meets the plane y = `a` + `b` x.
fn meets((origin, direction): (DVec3, DVec3), (a, b): (f64, f64)) -> DVec3 {
    let normal = DVec3::new(-b, 1., 0.);
    let t = (a - normal.dot(origin)) / normal.dot(direction);
    origin + direction * t
}

/// `point`'s device depth through `camera`, and its position on screen in
/// motion's units (UV, +Y down).
fn project(camera: &Camera, point: DVec3) -> (f64, [f64; 2]) {
    let clip = (camera.projection * camera.view).as_dmat4() * point.extend(1.);
    (
        clip.z / clip.w,
        [clip.x / clip.w * 0.5, -clip.y / clip.w * 0.5],
    )
}

fn texel<T: Copy>(texels: &[T], [x, y]: [u32; 2]) -> T {
    texels[(y * SIZE[0] + x) as usize]
}

fn assert_depth(observed: f32, expected: f64, label: &str) {
    assert!(
        (f64::from(observed) - expected).abs() <= 2e-4 * expected,
        "{label}: depth {observed}, expected {expected}"
    );
}

/// Pixels spread over the target.
const PIXELS: [[u32; 2]; 5] = [[4, 12], [27, 12], [16, 16], [6, 26], [25, 27]];

// Plausible defects: the shader's vertex function not applied, applied in
// one pass only, or given another vertex's or instance's data (the
// per-vertex stream or the object record wired to the wrong vertex,
// instance or word). The oracle is the test's own surface: two quads, each
// lifted along +Y by the parameters' lift, its vertices' data (0.1 times
// their x, which tilts it) and its instance's (0.5 on the left, 1 on the
// right), so each is the plane y = 0.25 + c + 0.1 x, which the CPU
// intersects each pixel's ray with through the frame's camera. The opaque
// depth holds the nearer plane at every pixel; the receiver pass's surface
// depth holds a blended receiver's plane alike.
#[test]
fn the_vertex_function_places_the_surface_in_every_camera_pass() {
    let Some(mut harness) = Harness::new(receiving()) else {
        return;
    };
    let params = TestShaderParams {
        direction: [0., 1., 0.],
        lift: 0.25,
        ..Default::default()
    };
    let camera = looking_down();
    let halves: [([f32; 2], f32); 2] = [([-8., 0.], 0.5), ([0., 8.], 1.)];
    for (kind, receives) in [("opaque", false), ("receiver", true)] {
        harness.scene = Scene::new(&harness.device, &harness.queue);
        let shader = add_test_shader(&mut harness.scene);
        let values = if receives {
            receiver(shader, 2.)
        } else {
            opaque(shader, 2.)
        };
        let material = harness.material(values);
        set_test_params(&mut harness.scene, &harness.queue, material, params);
        for (x, instance) in halves {
            harness.place(ground(x, [-8., 8.], 0.1), material, [instance, 0., 0., 0.]);
        }
        harness.frame(&frame(camera, 0.));
        let depths = if receives {
            harness.surface_depth()
        } else {
            harness.depth()
        };
        let mut checked = 0;
        for pixel in PIXELS {
            // The plane whose quad the pixel's ray meets on its own half.
            let expected = halves.iter().find_map(|&(x, instance)| {
                let hit = meets(ray(&camera, pixel), (0.25 + f64::from(instance), 0.1));
                (hit.x > f64::from(x[0]) + 0.3 && hit.x < f64::from(x[1]) - 0.3)
                    .then(|| project(&camera, hit).0)
            });
            if let Some(expected) = expected {
                assert_depth(
                    texel(&depths, pixel),
                    expected,
                    &format!("{kind} {pixel:?}"),
                );
                checked += 1;
            }
        }
        assert!(checked >= 4, "{kind}: the pixels lie on the quads");
    }
}

// Plausible defects: the evaluation motion is measured from takes this
// frame's time or parameters, or the last submitted frame's are not kept
// as scene and renderer history, so a surface the shader moves writes no
// motion, or one it stopped moving writes some. The oracle is the test's
// surface through its own camera: a ground lifted by 0.5 sin(2 t), and
// then by a lift its parameters change, at frames whose time and
// parameters the test chooses; each pixel's motion is where its point of
// this frame's plane lay on the last frame's plane (straight below it),
// projected.
#[test]
fn motion_follows_the_shaders_time_and_parameters() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    let shader = add_test_shader(&mut harness.scene);
    let material = harness.material(opaque(shader, 2.));
    harness.place(ground([-8., 8.], [-8., 8.], 0.), material, [0.; 4]);
    let camera = looking_down();
    let waving = TestShaderParams {
        direction: [0., 1., 0.],
        amplitude: 0.5,
        frequency: 2.,
        ..Default::default()
    };
    let height = |params: TestShaderParams, time: f64| {
        f64::from(params.lift)
            + f64::from(params.amplitude) * (f64::from(params.frequency) * time).sin()
    };
    let lifted = TestShaderParams {
        direction: [0., 1., 0.],
        lift: 0.2,
        ..Default::default()
    };
    // Each step: the parameters set before it, its time, and the plane's
    // height then and in the step before.
    let steps = [
        (waving, 0.3, None),
        (waving, 0.55, Some(height(waving, 0.3))),
        (lifted, 0.55, Some(height(waving, 0.55))),
        (lifted, 0.55, Some(height(lifted, 0.55))),
    ];
    for (step, (params, time, previous)) in steps.into_iter().enumerate() {
        set_test_params(&mut harness.scene, &harness.queue, material, params);
        harness.frame(&frame(camera, time));
        let Some(previous) = previous else {
            continue;
        };
        let motion = harness.motion();
        let now = height(params, time);
        for pixel in PIXELS {
            let point = meets(ray(&camera, pixel), (now, 0.));
            let before = point - DVec3::Y * (now - previous);
            let (_, [x, y]) = project(&camera, point);
            let (_, [x0, y0]) = project(&camera, before);
            let expected = [x - x0, y - y0];
            let observed = texel(&motion, pixel);
            for axis in 0..2 {
                assert!(
                    (f64::from(observed[axis]) - expected[axis]).abs()
                        <= 2e-3 * expected[axis].abs() + 2e-4,
                    "step {step} {pixel:?}: motion {observed:?}, expected {expected:?}"
                );
            }
        }
    }
}

// Plausible defect: an effect anchored through the render frame, its
// instance's data translated by a move of the render origin, or the last
// submitted frame's data or pose left in the old frame, so a move the
// camera follows moves the surface or writes motion. The oracle is the
// world: after `move_origin` by Δ, with the camera moved by Δ, the frame is
// the same frame: the depth unchanged and no motion.
#[test]
fn a_render_origin_move_moves_no_surface() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    let shader = add_test_shader(&mut harness.scene);
    let material = harness.material(opaque(shader, 4.));
    set_test_params(
        &mut harness.scene,
        &harness.queue,
        material,
        TestShaderParams {
            direction: [0., 1., 0.],
            ..Default::default()
        },
    );
    harness.place(
        ground([-8., 8.], [-8., 8.], 0.1),
        material,
        [0.75, 0., 0., 0.],
    );
    let camera = looking_down();
    harness.frame(&frame(camera, 1.));
    let before = harness.depth();
    let by = Vec3::new(10., 0., -20.);
    harness
        .scene
        .move_origin(&harness.device, &harness.queue, by)
        .unwrap();
    let moved = Camera {
        view: camera.view * Mat4::from_translation(by),
        eye: camera.eye - by,
        ..camera
    };
    harness.frame(&frame(moved, 1.));
    let after = harness.depth();
    let motion = harness.motion();
    for pixel in PIXELS {
        let (was, is) = (texel(&before, pixel), texel(&after, pixel));
        assert!(
            (was - is).abs() <= 1e-4 * was,
            "{pixel:?}: depth {was} before the move, {is} after"
        );
        let [x, y] = texel(&motion, pixel);
        assert!(
            x.abs() < 1e-4 && y.abs() < 1e-4,
            "{pixel:?}: motion {x}, {y}"
        );
    }
}

// Plausible defects: the scene depth a blended surface's shader reads in
// another space (device depth, or view depth along the axis rather than
// the ray), from another target (the surface depth, the blended surface's
// own), or bound where the tier's sampled textures are spent. The oracle
// is the test's own geometry through its camera: an unlit blended quad 1 m
// above an opaque ground writes the distance along each pixel's ray to the
// ground behind it, and whether scene depth is available, into its
// emission, which the composite holds as they are (no exposure or tone
// mapping yet): within 1 % of the CPU's distance on the Extended tier, and
// none on the Basic tier, which binds no scene depth.
#[test]
fn blended_shaders_read_the_opaque_depth_behind_them_on_the_extended_tier() {
    let Some(adapter) = test_support::adapter() else {
        return;
    };
    let basic = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .unwrap();
    for (tier, device) in [("Extended", test_support::device()), ("Basic", Some(basic))] {
        let Some(device) = device else {
            return;
        };
        let mut harness = Harness::on(device, settings());
        let extended =
            harness.renderer.binding_tier() == crate::graphics_device::BindingTier::Extended;
        assert_eq!(extended, tier == "Extended", "the {tier} device's tier");
        let shader = add_test_shader(&mut harness.scene);
        let black = asset::Material {
            base: [0., 0., 0., 1.],
            unlit: true,
            double_sided: true,
            ..Default::default()
        };
        let ground_material = harness.material(black.clone());
        harness.place(ground([-8., 8.], [-8., 8.], 0.), ground_material, [0.; 4]);
        let glass = harness.material(shaded(
            asset::Material {
                alpha: AlphaMode::Blend {
                    receives_screen_space_reflections: false,
                    keeps_specular: false,
                },
                ..black
            },
            shader,
            1.,
        ));
        set_test_params(
            &mut harness.scene,
            &harness.queue,
            glass,
            TestShaderParams {
                direction: [0., 1., 0.],
                lift: 1.,
                ..Default::default()
            },
        );
        harness.place(ground([-8., 8.], [-8., 8.], 0.), glass, [0.; 4]);
        let camera = looking_down();
        harness.frame(&frame(camera, 0.));
        let composite = harness.composite();
        for pixel in PIXELS {
            let [behind, available, ..] = texel(&composite, pixel);
            let ray = ray(&camera, pixel);
            let distance = meets(ray, (0., 0.)).distance(meets(ray, (1., 0.)));
            if extended {
                assert!(
                    (f64::from(behind) - distance).abs() <= 0.01 * distance && available == 1.,
                    "{tier} {pixel:?}: {behind} behind and available {available}, expected {distance}"
                );
            } else {
                assert!(
                    behind == 0. && available == 0.,
                    "{tier} {pixel:?}: {behind} behind and available {available}"
                );
            }
        }
    }
}

// Plausible defect: a removed shader's programs and pipelines kept, so a
// game that replaces its shaders leaks a set of modules and pipelines each
// time. The oracle is the renderer's own count of the modules and
// pipelines it holds for shaders: some while a drawn material names one,
// none once none does and it is removed, however often that repeats.
#[test]
fn a_removed_shader_frees_its_programs() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    let material = harness.material(asset::Material {
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        },
        ..Default::default()
    });
    harness.place(ground([-8., 8.], [-8., 8.], 0.), material, [0.; 4]);
    let camera = looking_down();
    for _ in 0..4 {
        let shader = add_test_shader(&mut harness.scene);
        let mut values = harness.scene.material(material).unwrap();
        values.shader = Some(MaterialShader {
            shader,
            displacement_bound: 0.,
        });
        harness
            .scene
            .set_material(&harness.queue, material, values)
            .unwrap();
        harness.frame(&frame(camera, 0.));
        let (modules, pipelines) = harness.renderer.test_shader_programs();
        assert!(modules > 0 && pipelines > 0, "a drawn shader's programs");
        values.shader = None;
        harness
            .scene
            .set_material(&harness.queue, material, values)
            .unwrap();
        harness.scene.remove_shader(shader).unwrap();
        harness.frame(&frame(camera, 0.));
        assert_eq!(harness.renderer.test_shader_programs(), (0, 0));
    }
}

// Plausible defects: a shader's or its parameter block's lifetime rules
// not held, so a material names an ended shader or another scene's, a
// shader is removed under a material that names it, or a block of the
// wrong size is written. The oracle is the contract's typed refusal.
#[test]
fn shader_lifetimes_and_parameter_blocks_are_held_to() {
    let Some(mut harness) = Harness::new(settings()) else {
        return;
    };
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    let shader = add_test_shader(&mut harness.scene);
    let named = harness.material(opaque(shader, 1.));
    let plain = harness.material(asset::Material::default());
    let scene = &mut harness.scene;
    assert!(matches!(
        scene.set_shader_parameters(&queue, named, &[0; 12]),
        Err(SceneError::ShaderParameters {
            expected: 32,
            given: 12
        })
    ));
    assert!(matches!(
        scene.set_shader_parameters(&queue, plain, &[0; 32]),
        Err(SceneError::ShaderParameters {
            expected: 0,
            given: 32
        })
    ));
    assert!(matches!(
        scene.remove_shader(shader),
        Err(SceneError::ShaderInUse)
    ));
    // Another scene's identity, and an ended one.
    let mut other = Scene::new(&device, &queue);
    let foreign = add_test_shader(&mut other);
    assert!(matches!(
        scene.add_materials(&device, &queue, &[opaque(foreign, 1.)], &[]),
        Err(SceneError::UnknownShader)
    ));
    let mut values = scene.material(named).unwrap();
    values.shader = None;
    scene.set_material(&queue, named, values).unwrap();
    scene.remove_shader(shader).unwrap();
    assert!(matches!(
        scene.remove_shader(shader),
        Err(SceneError::UnknownShader)
    ));
    assert!(matches!(
        scene.shader_parameters_layout(shader),
        Err(SceneError::UnknownShader)
    ));
    values.shader = Some(MaterialShader {
        shader,
        displacement_bound: 1.,
    });
    assert!(matches!(
        scene.set_material(&queue, named, values),
        Err(SceneError::UnknownShader)
    ));
    let live = add_test_shader(scene);
    values.shader = Some(MaterialShader {
        shader: live,
        displacement_bound: f32::NAN,
    });
    assert!(matches!(
        scene.set_material(&queue, named, values),
        Err(SceneError::InvalidDisplacementBound)
    ));
    // A refused module adds nothing.
    assert!(matches!(
        scene.add_shader(ShaderSource {
            wgsl: "fn material_vertex() {}".into(),
            label: "broken".into(),
        }),
        Err(SceneError::Shader(_))
    ));
    // A mesh's data, one entry per vertex or none.
    let (vertices, mut data) = ground([0., 1.], [0., 1.], 0.);
    data.pop();
    let mesh = ModelMesh {
        vertices,
        indices: vec![0, 1, 2],
        material: plain,
        deformation: Default::default(),
    };
    assert!(matches!(
        PreparedModel::with_shader_data(vec![mesh.clone(), mesh.clone()], vec![Vec::new(), data]),
        Err(SceneError::ShaderDataLength { mesh: 1 })
    ));
    assert!(matches!(
        PreparedModel::with_shader_data(vec![mesh], Vec::new()),
        Err(SceneError::ShaderDataLength { mesh: 0 })
    ));
}
