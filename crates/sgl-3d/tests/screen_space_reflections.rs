//! DiligentFX SSR through SGL3D's public Scene and Renderer API.
#![cfg(not(target_arch = "wasm32"))]
use sgl_3d::glam::camera;
use sgl_3d::{
    Backdrop, Camera, FrameInput, InstanceState, Mobility, ModelId, Renderer, Scene,
    SpecularProbeTexels,
    asset::{Asset, CpuMesh, Material, Vertex},
    diagnostics::DiagnosticTarget,
    environment::{EnvironmentMap, PmremAtlas},
    glam::{Mat4, Vec2, Vec3},
    perspective,
    settings::{self, Settings},
    static_lighting::IrradianceAtlas,
};

// DiligentFX's hit validation (ValidateHit, SSR_ComputeIntersection.fx)
// accepts a hit within 2.5 % of its view depth of the surface it lands on, so
// it needs a realistic pixel footprint.
const SIZE: [u32; 2] = [480, 320];

/// A device, or `None` after printing why. `SGL_REQUIRE_GPU` turns the skip
/// into a failure.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let result = pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))
        .map_err(|e| e.to_string())
        .and_then(|adapter| {
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits: sgl_3d::graphics_device::limits(&adapter),
                ..Default::default()
            }))
            .map_err(|e| e.to_string())
        });
    match result {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                !required,
                "GPU test cannot run but SGL_REQUIRE_GPU is set: {error}"
            );
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

/// The caller's output texture.
fn target(device: &wgpu::Device, size: [u32; 2]) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("frame output"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn material(base: [f32; 4], metallic: f32, roughness: f32) -> Material {
    Material {
        anisotropy_strength: 0.,
        anisotropy_rotation: 0.,
        anisotropy_texture: None,
        name: "fixture".into(),
        visibility_group: 0,
        casts_directional_shadow: true,
        base,
        emissive: [0.; 3],
        metallic,
        roughness,
        clearcoat: 0.,
        coat_roughness: 0.,
        base_texture: None,
        mr_texture: None,
        emissive_texture: None,
        normal_texture: None,
        normal_scale: 1.,
        normal_layers: None,
        bump_texture: None,
        bump_scale: 0.,
        wrap: [gltf::texture::WrappingMode::Repeat; 2],
        double_sided: true,
        unlit: false,
        emits_into_gi: true,
        alpha: sgl_3d::AlphaMode::Opaque,
    }
}

fn quad(corners: [Vec3; 4], normal: Vec3, material: usize) -> CpuMesh {
    CpuMesh {
        vertices: corners
            .map(|position| Vertex {
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: position.to_array(),
                normal: normal.to_array(),
                uv: [(position.x + 6.) / 12., (position.z + 20.) / 24.],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

/// A floor at y=-1 and, unless `panel` is zero, an unlit panel of that
/// radiance standing on it.
fn world(floor: Material, panel: f32) -> Asset {
    let [a, b, c, d] =
        [(-6., 4.), (6., 4.), (6., -20.), (-6., -20.)].map(|(x, z)| Vec3::new(x, -1., z));
    let mut asset = Asset {
        meshes: vec![quad([a, b, c, d], Vec3::Y, 0)],
        materials: vec![floor],
        images: Vec::new(),
        rig: Default::default(),
    };
    if panel > 0. {
        let mut glow = material([panel, panel, panel, 1.], 0., 1.);
        glow.unlit = true;
        asset.materials.push(glow);
        let [e, f, g, h] =
            [(-1., -1.), (1., -1.), (1., 0.2), (-1., 0.2)].map(|(x, y)| Vec3::new(x, y, -6.));
        asset.meshes.push(quad([e, f, g, h], Vec3::Z, 1));
    }
    asset
}

fn smooth_metal() -> Material {
    material([0.9, 0.9, 0.9, 1.], 1., 0.05)
}

/// A uniform environment of `radiance` in every direction and at every roughness.
fn environment(radiance: f32) -> EnvironmentMap {
    // Exact for the fixture's positive powers of two.
    let half = |v: f32| ((v.log2() as i32 + 15) << 10) as u16;
    let texel = if radiance > 0. {
        [half(radiance), half(radiance), half(radiance), half(1.)]
    } else {
        [0, 0, 0, half(1.)]
    };
    let byte = (radiance.min(1.) * 255.) as u8;
    EnvironmentMap {
        panorama: image::RgbaImage::from_pixel(4, 2, image::Rgba([byte, byte, byte, 255])),
        filtered: PmremAtlas {
            width: 336,
            height: 64,
            rgba16: texel
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .cycle()
                .take(336 * 64 * 8)
                .collect(),
        },
    }
}

struct Frames {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    /// The world, a static instance's model.
    world: ModelId,
    /// The models the caller places.
    models: Vec<ModelId>,
    renderer: Renderer,
    settings: Settings,
    input: FrameInput,
    output: wgpu::TextureView,
    size: [u32; 2],
}

/// `model` at the identity pose, shown everywhere.
fn shown(model: ModelId) -> InstanceState {
    InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible: true,
        capture_visible: true,
    }
}

impl Frames {
    fn new(world: Asset, environment: EnvironmentMap, ssr: bool, size: [u32; 2]) -> Option<Self> {
        Self::with_models(world, vec![], environment, ssr, size)
    }

    /// With `models` added, for the caller to place.
    fn with_models(
        world: Asset,
        models: Vec<Asset>,
        environment: EnvironmentMap,
        ssr: bool,
        size: [u32; 2],
    ) -> Option<Self> {
        let (device, queue) = device()?;
        let mut scene = Scene::new(&device, &queue);
        let world = scene.add_asset(&device, &queue, world).unwrap().model;
        scene
            .add_instance(&device, &queue, shown(world), Mobility::Static)
            .unwrap();
        let models = models
            .into_iter()
            .map(|model| scene.add_asset(&device, &queue, model).unwrap().model)
            .collect();
        let eye = Vec3::new(0., 0.2, 2.);
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., -0.6, -6.), Vec3::Y),
            projection: perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        });
        input.backdrop = Backdrop::Color([0.; 3]);
        input.environment = Some(
            scene
                .add_environment(&device, &queue, &environment)
                .unwrap(),
        );
        let settings = Settings {
            scene_resolution: settings::SceneResolution::Full,
            atmosphere: false,
            bloom: settings::Bloom::Off,
            screen_space_reflections: if ssr {
                settings::ScreenSpaceReflections::Full
            } else {
                settings::ScreenSpaceReflections::Off
            },
            ..Settings::default()
        };
        let renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            size,
            1.,
            &settings,
        )
        .unwrap();
        let output = target(&device, size);
        Some(Self {
            device,
            queue,
            scene,
            world,
            models,
            renderer,
            settings,
            input,
            output,
            size,
        })
    }

    fn resize(&mut self, size: [u32; 2]) {
        self.size = size;
        self.input.camera.projection = perspective(1., size[0] as f32 / size[1] as f32, 0.1);
        self.renderer.resize(&self.device, size, 1., &self.settings);
        self.output = target(&self.device, size);
    }

    /// Move the camera sideways to `x`; floor depth per pixel is unchanged.
    fn look_from(&mut self, x: f32) {
        let eye = Vec3::new(x, 0.2, 2.);
        self.input.camera.eye = eye;
        self.input.camera.view =
            camera::rh::view::look_at_mat4(eye, Vec3::new(x, -0.6, -6.), Vec3::Y);
    }

    fn render(&mut self, frames: usize, reset: bool) {
        for index in 0..frames {
            self.input.camera_cut = reset && index == 0;
            let mut encoder = self.device.create_command_encoder(&Default::default());
            self.renderer.render(
                &self.device,
                &self.queue,
                &mut encoder,
                &mut self.scene,
                &self.input,
                &self.settings,
                &self.output,
                None,
            );
            self.queue.submit([encoder.finish()]);
            self.renderer.finish_frame(&mut self.scene);
        }
    }

    /// `target` of the last frame.
    fn target(&self, target: DiagnosticTarget) -> &wgpu::TextureView {
        self.renderer.diagnostic_target(target).unwrap()
    }

    /// RGB of the HDR composite, row-major.
    fn composite(&self) -> Vec<[f32; 3]> {
        let pixels: Vec<[f32; 3]> = self
            .texels::<4>(self.target(DiagnosticTarget::Composite))
            .into_iter()
            .map(|[r, g, b, _]| [r, g, b])
            .collect();
        assert!(
            pixels.iter().flatten().all(|v| v.is_finite()),
            "composition must stay finite"
        );
        pixels
    }

    /// Texels of an `N`-channel half-float target, row-major.
    fn texels<const N: usize>(&self, view: &wgpu::TextureView) -> Vec<[f32; N]> {
        let bytes = N as u32 * 2;
        let texture = view.texture();
        let [width, height] = [texture.width(), texture.height()];
        let row = (width * bytes).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("target readback"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let mapped = buffer.get_mapped_range(..).unwrap();
        mapped
            .chunks(row as usize)
            .flat_map(|line| line[..(width * bytes) as usize].chunks_exact(bytes as usize))
            .map(|texel| std::array::from_fn(|c| half(&texel[c * 2..c * 2 + 2])))
            .collect()
    }

    /// Composite luminance where `point` projects.
    fn luminance_at(&self, composite: &[[f32; 3]], point: Vec3) -> f32 {
        let camera = self.input.camera;
        let ndc = (camera.projection * camera.view).project_point3(point);
        let size = Vec2::new(self.size[0] as f32, self.size[1] as f32);
        let pixel = (Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * size).as_uvec2();
        luminance(composite[(pixel.y * self.size[0] + pixel.x) as usize])
    }
}

fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn half(bytes: &[u8]) -> f32 {
    let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
    let sign = if bits & 0x8000 == 0 { 1. } else { -1. };
    let exponent = i32::from((bits >> 10) & 31);
    let fraction = f32::from(bits & 1023);
    sign * match exponent {
        0 => fraction * 2f32.powi(-24),
        31 if fraction == 0. => f32::INFINITY,
        31 => f32::NAN,
        _ => (1. + fraction / 1024.) * 2f32.powi(exponent - 15),
    }
}

/// Mirror images across the floor at y=-1: the panel centre and the black sky above it.
const PANEL_MIRROR: Vec3 = Vec3::new(0., -1.6, -6.);
const SKY_MIRROR: Vec3 = Vec3::new(0., -3.5, -6.);

/// Enough frames for the temporal denoiser to settle.
const FRAMES: usize = 6;

const METHODS: [settings::ReflectionMethod; 2] = [
    settings::ReflectionMethod::Crystal,
    settings::ReflectionMethod::Velvet,
];

// Defect: a method is not traced or composited, or its frame matrices are in
// the wrong convention (the reflection lands elsewhere or nowhere).
#[test]
fn reflects_an_emitter_where_the_mirror_puts_it() {
    for method in METHODS {
        let Some(mut frames) = Frames::new(world(smooth_metal(), 4.), environment(0.), true, SIZE)
        else {
            return;
        };
        frames.settings.reflection_method = method;
        frames.render(FRAMES, true);
        let composite = frames.composite();
        let panel = frames.luminance_at(&composite, PANEL_MIRROR);
        let sky = frames.luminance_at(&composite, SKY_MIRROR);
        assert!(
            panel > 2.,
            "{method:?}: floor reflecting the panel: luminance {panel}"
        );
        assert!(
            sky < 0.2,
            "{method:?}: floor reflecting black sky: luminance {sky}"
        );
    }
}

// Defect: the trace ends at the first surface a ray passes behind, so a thin
// foreground object blanks every reflection seen past it (DFX-18, and Godot's
// depth tolerance). A bar floats near the camera, across the screen between
// the floor's reflection of the panel and the panel: the floor's rays pass
// metres behind it and never touch it.
#[test]
fn reflects_past_a_thin_occluder_the_ray_passes_behind() {
    for method in METHODS {
        let mut world = world(smooth_metal(), 4.);
        world.materials.push(material([0.1, 0.1, 0.1, 1.], 0., 1.));
        let [a, b, c, d] =
            [(-4., -0.24), (4., -0.24), (4., -0.2), (-4., -0.2)].map(|(x, y)| Vec3::new(x, y, -1.));
        world.meshes.push(quad([a, b, c, d], Vec3::Z, 2));
        let Some(mut frames) = Frames::new(world, environment(0.), true, SIZE) else {
            return;
        };
        frames.settings.reflection_method = method;
        frames.render(FRAMES, true);
        let composite = frames.composite();
        let panel = frames.luminance_at(&composite, PANEL_MIRROR);
        assert!(
            panel > 2.,
            "{method:?}: floor reflecting the panel past the bar: luminance {panel}"
        );
    }
}

// Defect: the static capture draws every material whatever the frame's
// visibility mask selects, so a game cannot leave geometry out of a probe (a
// reflection probe's culling mask). The capture looks straight at the unlit
// panel, which is in visibility group 2; with the group unselected it sees the
// black sky behind it.
#[test]
fn static_probe_capture_leaves_out_unselected_visibility_groups() {
    let mut world = world(smooth_metal(), 4.);
    world.materials[1].visibility_group = 2;
    let Some(mut frames) = Frames::new(world, environment(0.), false, SIZE) else {
        return;
    };
    let mut panel = |mask| {
        frames.input.visibility_mask = mask;
        frames.render(1, true);
        let face_size = 64;
        let probe = frames
            .renderer
            .capture_specular_probe(
                &frames.device,
                &frames.queue,
                &mut frames.scene,
                &frames.input,
                &frames.settings,
                Vec3::new(0., -0.4, 2.),
                face_size,
            )
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(texels) = probe.texels else {
            panic!("captures are RGBA16F");
        };
        // Level 0, the -Z face (4), its centre texel.
        let size = face_size as usize;
        let texel = (4 * size * size + size / 2 * size + size / 2) * 4;
        let rgb = std::array::from_fn(|c| half(&texels[texel + c].to_le_bytes()));
        luminance(rgb)
    };
    let (selected, unselected) = (panel(2), panel(0));
    assert!(selected > 2., "selected panel: luminance {selected}");
    assert!(unselected < 0.1, "unselected panel: luminance {unselected}");
}

// Defect: environment specular ignores ambient occlusion, so a glossy floor
// keeps reflecting the open environment right against a wall that hides most
// of it. SSR is off, so the floor's specular is the environment's alone, and
// the metal floor has no diffuse for ambient occlusion to darken instead. Its
// F0 is a dielectric's, which multi-bounce barely brightens back.
#[test]
fn ambient_occlusion_occludes_environment_specular_against_a_wall() {
    let mut world = world(material([0.04, 0.04, 0.04, 1.], 1., 0.05), 0.);
    let [a, b, c, d] =
        [(-6., -1.), (6., -1.), (6., 3.), (-6., 3.)].map(|(x, y)| Vec3::new(x, y, -6.));
    world.meshes.push(quad([a, b, c, d], Vec3::Z, 0));
    let Some(mut frames) = Frames::new(world, environment(4.), false, SIZE) else {
        return;
    };
    frames.render(FRAMES, true);
    let open = frames.composite();
    frames.settings.ambient_occlusion = settings::AmbientOcclusionQuality::Ultra;
    frames.render(FRAMES, true);
    let occluded = frames.composite();
    let [near, far] = [Vec3::new(0., -1., -5.9), Vec3::new(0., -1., -2.)]
        .map(|point| [&open, &occluded].map(|composite| frames.luminance_at(composite, point)));
    assert!(
        near[1] < 0.8 * near[0],
        "floor against the wall: luminance {} without ambient occlusion, {} with",
        near[0],
        near[1]
    );
    assert!(
        (far[1] - far[0]).abs() <= 0.02 * far[0],
        "open floor: luminance {} without ambient occlusion, {} with",
        far[0],
        far[1]
    );
}

// Defect: screen-space reflections cannot see a moving object's underside, so
// the floor beneath a hovering box reflects the bright environment instead of
// the box. This floor point's mirror ray passes under the box's front edge and
// meets its underside, which the camera never sees.
#[test]
fn world_space_reflections_find_a_moving_objects_hidden_underside() {
    let (lo, hi) = (Vec3::new(-1., -0.7, -4.), Vec3::new(1., -0.3, -3.));
    let corner = |x: bool, y: bool, z: bool| {
        Vec3::new(
            if x { hi.x } else { lo.x },
            if y { hi.y } else { lo.y },
            if z { hi.z } else { lo.z },
        )
    };
    let faces = [
        ([(0, 0, 1), (1, 0, 1), (1, 0, 0), (0, 0, 0)], Vec3::NEG_Y),
        ([(0, 1, 1), (1, 1, 1), (1, 1, 0), (0, 1, 0)], Vec3::Y),
        ([(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], Vec3::Z),
        ([(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)], Vec3::NEG_Z),
        ([(0, 0, 0), (0, 0, 1), (0, 1, 1), (0, 1, 0)], Vec3::NEG_X),
        ([(1, 0, 0), (1, 0, 1), (1, 1, 1), (1, 1, 0)], Vec3::X),
    ]
    .map(|(corners, normal)| {
        quad(
            corners.map(|(x, y, z)| corner(x == 1, y == 1, z == 1)),
            normal,
            0,
        )
    });
    let mut black = material([0., 0., 0., 1.], 0., 1.);
    black.unlit = true;
    let model = Asset {
        meshes: faces.to_vec(),
        materials: vec![black],
        images: Vec::new(),
        rig: Default::default(),
    };
    let Some(mut frames) = Frames::with_models(
        world(smooth_metal(), 0.),
        vec![model],
        environment(4.),
        true,
        SIZE,
    ) else {
        return;
    };
    frames
        .scene
        .add_instance(
            &frames.device,
            &frames.queue,
            shown(frames.models[0]),
            Mobility::Moving,
        )
        .unwrap();
    frames.render(FRAMES, true);
    let screen_only = frames.composite();
    frames.settings.world_space_reflections = settings::WorldSpaceReflections::Moving;
    frames.render(FRAMES, true);
    let traced = frames.composite();
    let floor = Vec3::new(0., -1., -2.4);
    let [before, after] = [&screen_only, &traced].map(|c| frames.luminance_at(c, floor));
    assert!(
        after < 0.5 * before,
        "floor under the box: luminance {before} with screen-space reflections, {after} with world-space rays"
    );
}

// A clear coat is not normal-mapped, so relief in the base's normal map must
// not bend the coat's reflection: the panel still appears where the flat
// floor's mirror puts it. The rough metal base makes the coat the only glossy
// lobe: tracing it with the base's roughness loses the panel, composing it
// with the base's F0 exceeds what a coat can reflect.
#[test]
fn coated_floor_reflects_with_its_geometry_normal_despite_its_normal_map() {
    for method in METHODS {
        for resolution in [
            settings::ScreenSpaceReflections::Full,
            settings::ScreenSpaceReflections::Half,
        ] {
            let floor = || {
                let mut floor = material([0.9, 0.9, 0.9, 1.], 1., 0.9);
                floor.clearcoat = 1.;
                floor.coat_roughness = 0.05;
                floor
            };
            let world = |panel| {
                let mut world = world(floor(), panel);
                // A constant 45 degree tangent-space tilt; off the mapped normal, the
                // mirror ray leaves the panel and reflects the black sky instead.
                world
                    .images
                    .push(sgl_3d::asset::Image::Rgba8(image::RgbaImage::from_pixel(
                        1,
                        1,
                        image::Rgba([218, 128, 218, 255]),
                    )));
                for vertex in &mut world.meshes[0].vertices {
                    vertex.uv = [vertex.position[0], vertex.position[2]];
                }
                world.materials[0].normal_texture = Some(world.images.len() - 1);
                world
            };
            let Some(mut frames) = Frames::new(world(40.), environment(0.), true, SIZE) else {
                return;
            };
            frames.settings.reflection_method = method;
            frames.settings.screen_space_reflections = resolution;
            frames.render(FRAMES, true);
            let composite = frames.composite();
            let panel = frames.luminance_at(&composite, PANEL_MIRROR);
            let sky = frames.luminance_at(&composite, SKY_MIRROR);
            // The same floor without the panel: the base's own shading there.
            let mut unlit = Frames::new(world(0.), environment(0.), true, SIZE).unwrap();
            unlit.settings.reflection_method = method;
            unlit.settings.screen_space_reflections = resolution;
            unlit.render(FRAMES, true);
            let base = unlit.luminance_at(&unlit.composite(), PANEL_MIRROR);
            // The coat reflects the panel with the coat's Fresnel (F0 0.04) at the
            // receiver's view angle, not the metal base's (F0 0.9): the reflection is
            // nearer the coat's Schlick value than the base's.
            let eye = frames.input.camera.eye;
            let toward = PANEL_MIRROR - eye;
            let receiver = eye + toward * ((-1. - eye.y) / toward.y);
            let n_dot_v = (eye - receiver).normalize().y;
            let schlick = |f0: f32| 40. * (f0 + (1. - f0) * (1. - n_dot_v).powi(5));
            let reflected = panel - base;
            eprintln!(
                "coat SSR {method:?} {resolution:?}: panel={reflected}, sky={sky}, base={base}"
            );
            assert!(
                reflected > 2.
                    && (reflected - schlick(0.04)).abs() < (reflected - schlick(0.9)).abs(),
                "coated floor reflecting the panel: {reflected} over the base's {base}; coat {}, base {}",
                schlick(0.04),
                schlick(0.9)
            );
            assert!(
                sky < 0.2,
                "coated floor reflecting black sky: luminance {sky}"
            );
        }
    }
}

// With nothing on screen to reflect, confidence is zero and composition adds
// the traced lobe's environment specular, which source completion leaves out:
// the floor equals the environment-only floor. Double counting doubles it;
// counting neither leaves a black metal floor. A smooth receiver is traced; a
// rough one is not, and completion keeps its environment specular. Both
// paths use SGL3D's split-sum table; they differ only by the composite's
// half-float roundings.
#[test]
fn environment_is_reflected_once_on_smooth_and_rough_receivers() {
    for roughness in [0.05, 0.6] {
        let floor = material([0.9, 0.9, 0.9, 1.], 1., roughness);
        let Some(mut ssr) = Frames::new(world(floor.clone(), 0.), environment(1.), true, SIZE)
        else {
            return;
        };
        let mut reference = Frames::new(world(floor, 0.), environment(1.), false, SIZE).unwrap();
        reference.render(1, true);
        let expected = reference.composite();
        for frame in 0..FRAMES {
            ssr.render(1, frame == 0);
            let composite = ssr.composite();
            for point in [(0., -3.), (0., -8.), (-2., -5.)].map(|(x, z)| Vec3::new(x, -1., z)) {
                let ratio =
                    ssr.luminance_at(&composite, point) / reference.luminance_at(&expected, point);
                assert!(
                    (ratio - 1.).abs() < 5e-3,
                    "roughness {roughness}, frame {frame}, floor at {point}: {ratio} of the environment-only floor"
                );
            }
        }
    }
}

// A panel that emits exactly the environment's radiance leaves nothing for a
// reflection to tell apart: the floor must be the environment-only floor
// whether a ray hits the panel or misses into the environment, including
// where only some of a pixel's rays hit (the panel reflection's edges, which
// a glossy receiver blurs). Counting partly covered pixels' hits by the hit
// share twice (`lerp(environment, reflected, confidence)` of an average that
// counts misses as black) darkens those edges by up to a quarter.
#[test]
fn partly_covered_reflections_keep_the_environment_energy() {
    let floor = || material([0.9, 0.9, 0.9, 1.], 1., 0.1);
    let Some(mut ssr) = Frames::new(world(floor(), 2.), environment(2.), true, SIZE) else {
        return;
    };
    let mut reference = Frames::new(world(floor(), 2.), environment(2.), false, SIZE).unwrap();
    for frame in 0..FRAMES {
        ssr.render(1, frame == 0);
        reference.render(1, frame == 0);
        let (composite, expected) = (ssr.composite(), reference.composite());
        let worst = composite
            .iter()
            .zip(&expected)
            .filter(|(_, e)| luminance(**e) > 0.1)
            .map(|(c, e)| (luminance(*c) / luminance(*e) - 1.).abs())
            .fold(0., f32::max);
        assert!(
            worst < 5e-3,
            "frame {frame}: a pixel differs from the environment-only frame by {worst}"
        );
    }
}

// SSR fades out towards its roughness cutoff, so a receiver's reflection is
// continuous in roughness: just below the cutoff it matches the untraced
// receiver just above it. A hard cutoff switches from the SSR reflection of
// the panel to the black environment there.
#[test]
fn reflections_are_continuous_across_the_roughness_cutoff() {
    let at = |roughness: f32| {
        let floor = material([0.9, 0.9, 0.9, 1.], 1., roughness);
        let mut frames = Frames::new(world(floor, 40.), environment(0.), true, SIZE)?;
        frames.render(FRAMES, true);
        Some(frames.luminance_at(&frames.composite(), PANEL_MIRROR))
    };
    let cutoff = sgl_3d::diagnostics::crystal_roughness_threshold();
    let Some(traced) = at(cutoff - 0.06) else {
        return;
    };
    assert!(
        traced > 2.,
        "a floor well below the cutoff reflects the panel: luminance {traced}"
    );
    let (below, above) = (at(cutoff - 0.002).unwrap(), at(cutoff + 0.002).unwrap());
    assert!(
        (below - above).abs() < 0.02 * traced,
        "across the cutoff the reflection jumps from {below} to {above} (traced floor {traced})"
    );
}

fn max_difference(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (luminance(*a) - luminance(*b)).abs())
        .fold(0., f32::max)
}

// After a resize, which restarts history without a camera cut, frames equal
// those of a renderer created at the new size. Over several frames, so a
// group still binding a target from before the resize, on either side of a
// history pair, shows.
#[test]
fn resize_restarts_at_the_new_size() {
    let (size, resized_size) = ([96, 64], [80, 48]);
    for method in METHODS {
        let start = |size| {
            let mut frames = Frames::new(world(smooth_metal(), 4.), environment(0.25), true, size)?;
            frames.settings.reflection_method = method;
            Some(frames)
        };
        let Some(mut fresh) = start(resized_size) else {
            return;
        };
        let mut resized = start(size).unwrap();
        resized.render(FRAMES, true);
        resized.resize(resized_size);
        for frame in 0..FRAMES {
            fresh.render(1, frame == 0);
            resized.render(1, false);
            let difference = max_difference(&fresh.composite(), &resized.composite());
            assert!(
                difference < 1e-3,
                "{method:?}: frame {frame} after the resize differs by {difference}"
            );
        }
    }
}

// A resize that keeps the render size (scene resolution Half rounds both
// output sizes down to it) replaces the shared targets but not the methods'
// own, which must then bind the new ones: while the camera moves, frames
// after it equal those of a renderer created at the new output size.
#[test]
fn resize_to_the_same_render_size_binds_the_new_shared_targets() {
    let (size, resized_size) = ([96, 64], [97, 65]);
    for method in METHODS {
        let start = |size| {
            let mut frames = Frames::new(world(smooth_metal(), 4.), environment(0.25), true, size)?;
            frames.settings.reflection_method = method;
            frames.settings.scene_resolution = settings::SceneResolution::Half;
            frames.resize(size);
            Some(frames)
        };
        let Some(mut fresh) = start(resized_size) else {
            return;
        };
        let mut resized = start(size).unwrap();
        resized.render(FRAMES, true);
        resized.resize(resized_size);
        for frame in 0..FRAMES {
            for frames in [&mut fresh, &mut resized] {
                frames.look_from(0.05 * (frame + 1) as f32);
            }
            fresh.render(1, frame == 0);
            resized.render(1, false);
            let difference = max_difference(&fresh.composite(), &resized.composite());
            assert!(
                difference < 1e-3,
                "{method:?}: frame {frame} after the resize differs by {difference}"
            );
        }
    }
}

// A camera cut (a history reset without a resize) discards the effect's
// history: frames after it equal those of a pipeline started at the new view.
#[test]
fn camera_cut_restarts_history() {
    let fixture = || world(smooth_metal(), 4.);
    let Some(mut fresh) = Frames::new(fixture(), environment(0.25), true, SIZE) else {
        return;
    };
    let mut cut = Frames::new(fixture(), environment(0.25), true, SIZE).unwrap();
    cut.look_from(1.5);
    cut.render(FRAMES, true);
    cut.look_from(0.);
    for frame in 0..FRAMES {
        fresh.render(1, frame == 0);
        cut.render(1, frame == 0);
        let difference = max_difference(&fresh.composite(), &cut.composite());
        assert!(
            difference < 1e-3,
            "frame {frame} after the cut differs by {difference}"
        );
    }
}

// SSR switched off for some frames discards its history: its next frame
// equals that of SSR first switched on at the same frame. SSR that ran
// throughout differs, so history does shape the frame.
#[test]
fn switching_off_discards_history() {
    let fixture = || world(smooth_metal(), 4.);
    let Some(mut missed) = Frames::new(fixture(), environment(0.25), true, SIZE) else {
        return;
    };
    let mut late = Frames::new(fixture(), environment(0.25), false, SIZE).unwrap();
    let mut steady = Frames::new(fixture(), environment(0.25), true, SIZE).unwrap();
    missed.render(FRAMES, true);
    missed.settings.screen_space_reflections = settings::ScreenSpaceReflections::Off;
    missed.render(2, false);
    missed.settings.screen_space_reflections = settings::ScreenSpaceReflections::Full;
    late.render(FRAMES + 2, true);
    late.settings.screen_space_reflections = settings::ScreenSpaceReflections::Full;
    steady.render(FRAMES + 2, true);
    for frames in [&mut missed, &mut late, &mut steady] {
        frames.render(1, false);
    }
    let restarted = missed.composite();
    let difference = max_difference(&restarted, &late.composite());
    assert!(
        difference < 1e-3,
        "after missed frames the method differs from a new one by {difference}"
    );
    let history = max_difference(&restarted, &steady.composite());
    assert!(
        history > 1e-2,
        "history made no difference ({history}); the comparison above proves nothing"
    );
}

/// A black baked irradiance atlas.
fn black_atlas() -> IrradianceAtlas {
    IrradianceAtlas {
        size: [4, 4],
        irradiance: vec![[0.; 3]; 16],
        back_irradiance: vec![[0.; 3]; 16],
        directionality: Vec::new(),
        back_directionality: Vec::new(),
    }
}

/// A named edit of a running frame sequence.
type Edit = (&'static str, fn(&mut Frames));

// Defect: a content edit or a lighting switch restarts every history (#356).
// History restarts only for a camera cut, a resize or another scene, as in
// Hydrogent, FSR2 and Wicked; each history rejects changed content itself.
// After an edit that leaves the frame's content as it was, the frame equals
// one without the edit, while a camera cut at the same point differs, so
// history does shape these frames.
#[test]
fn content_edits_keep_history() {
    for method in METHODS {
        let start = || {
            let mut frames = Frames::new(world(smooth_metal(), 4.), environment(0.25), true, SIZE)?;
            frames.settings.reflection_method = method;
            frames
                .scene
                .set_static_irradiance_atlas(&frames.device, &frames.queue, &black_atlas())
                .unwrap();
            frames.render(FRAMES, true);
            Some(frames)
        };
        let Some(mut steady) = start() else {
            return;
        };
        let mut edited = start().unwrap();
        let edits: [Edit; 3] = [
            ("registering no LODs", |frames| {
                frames
                    .scene
                    .set_mesh_lods(frames.world, 0, Vec::new())
                    .unwrap()
            }),
            ("installing the same baked atlas", |frames| {
                frames
                    .scene
                    .set_static_irradiance_atlas(&frames.device, &frames.queue, &black_atlas())
                    .unwrap()
            }),
            ("adding a light that is off", |frames| {
                frames
                    .scene
                    .add_light(
                        &frames.device,
                        &frames.queue,
                        sgl_3d::Light {
                            position: Vec3::new(0., 2., -4.),
                            shape: sgl_3d::LightShape::Point {
                                radius: sgl_3d::LightShape::DEFAULT_RADIUS,
                            },
                            color: [1.; 3],
                            intensity: 0.,
                            range: 10.,
                            baked: false,
                            specular: 1.,
                            casts_shadow: true,
                            ..Default::default()
                        },
                    )
                    .map(drop)
                    .unwrap()
            }),
        ];
        for (edit, apply) in edits {
            apply(&mut edited);
            steady.render(1, false);
            edited.render(1, false);
            let difference = max_difference(&steady.composite(), &edited.composite());
            assert!(
                difference < 1e-3,
                "{method:?}: {edit} changed the next frame by {difference}"
            );
        }
        steady.render(1, false);
        edited.render(1, true);
        let history = max_difference(&steady.composite(), &edited.composite());
        assert!(
            history > 1e-2,
            "{method:?}: a camera cut made no difference ({history}); the comparison above proves nothing"
        );
    }
}

// The sky is at infinity, so it moves with the camera's rotation only
// (Diligent's EnvMap.psh, Bevy's skybox motion vectors); SSR and TAA
// reproject it with that motion. After a turn and a step forward, a sky
// pixel's motion is where its view direction was in the previous frame.
#[test]
fn sky_motion_follows_camera_rotation() {
    let Some(mut frames) = Frames::new(world(smooth_metal(), 0.), environment(0.25), false, SIZE)
    else {
        return;
    };
    // Unjittered pixel centres.
    frames.settings.antialiasing = settings::Antialiasing::Off;
    // Looking 31 degrees up with a 57 degree field of view: only sky.
    let camera = |eye: Vec3, yaw: f32| {
        let view = camera::rh::view::look_at_mat4(
            eye,
            eye + Vec3::new(yaw.sin(), 0.6, -yaw.cos()),
            Vec3::Y,
        );
        (eye, view)
    };
    let (eye, view) = camera(Vec3::new(0., 0.2, 2.), 0.);
    frames.input.camera.eye = eye;
    frames.input.camera.view = view;
    frames.render(2, true);
    let previous = frames.input.camera.projection * view;
    let (eye, view) = camera(Vec3::new(0., 0.2, 1.), 0.05);
    frames.input.camera.eye = eye;
    frames.input.camera.view = view;
    frames.render(1, false);
    let current = frames.input.camera.projection * view;

    let motion = frames.texels::<2>(frames.target(DiagnosticTarget::Motion));
    let size = Vec2::new(SIZE[0] as f32, SIZE[1] as f32);
    for pixel in [Vec2::new(360., 40.), Vec2::new(60., 280.)] {
        let centre = (pixel + 0.5) / size;
        let ndc = Vec2::new(centre.x * 2. - 1., 1. - centre.y * 2.);
        let direction = current.inverse().project_point3(ndc.extend(1.)) - eye;
        let was = previous * direction.extend(0.);
        let was = Vec2::new(was.x / was.w, was.y / was.w);
        // Current minus previous UV (+y down).
        let expected = (ndc - was) * Vec2::new(0.5, -0.5);
        let texel = motion[(pixel.y as u32 * SIZE[0] + pixel.x as u32) as usize];
        let actual = Vec2::new(texel[0], texel[1]);
        assert!(expected.length() > 0.01, "the turn moved {pixel}");
        assert!(
            (actual - expected).length() < 1e-3,
            "sky motion at {pixel} is {actual}, expected {expected}"
        );
    }
}

#[path = "support/coated_normals.rs"]
mod coated_normals;
