//! Opaque blockers of supplemental moving-object rays through the Ultra frame path.
#![cfg(not(target_arch = "wasm32"))]
use sgl_3d::glam::camera;
use sgl_3d::{
    Camera, FrameInput, InstanceId, InstanceState, Mobility, Renderer, Scene,
    asset::{Asset, CpuMesh, Material, Vertex},
    diagnostics::DiagnosticTarget,
    environment::{EnvironmentMap, PmremAtlas},
    glam::{Mat4, Vec3},
    perspective,
    settings::{self, Settings},
};

const SIZE: [u32; 2] = [128, 128];

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
        ior: 1.5,
        specular: 1.,
        specular_color: [1.; 3],
        clearcoat: 0.,
        coat_roughness: 0.,
        clearcoat_texture: None,
        coat_roughness_texture: None,
        coat_normal_texture: None,
        coat_normal_scale: 1.,
        iridescence: 0.,
        iridescence_ior: 1.3,
        iridescence_thickness: [100., 400.],
        iridescence_texture: None,
        iridescence_thickness_texture: None,
        base_texture: None,
        mr_texture: None,
        occlusion_texture: None,
        occlusion_strength: 1.,
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

struct Frames {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    /// The offscreen model, which only reflections show.
    offscreen_model: InstanceId,
    renderer: Renderer,
    settings: Settings,
    input: FrameInput,
    output: wgpu::TextureView,
    size: [u32; 2],
}
impl Frames {
    fn new(wall_z: Option<f32>) -> Option<Self> {
        Self::with_roughness(wall_z, 0.045)
    }
    fn with_roughness(wall_z: Option<f32>, roughness: f32) -> Option<Self> {
        // An unlit offscreen model's radiance is independent of scene lighting
        // and the static wall.
        let mut offscreen_model = material([6., 6., 6., 1.], 0., 1.);
        offscreen_model.unlit = true;
        Self::with_offscreen_model(wall_z, roughness, offscreen_model)
    }
    /// A mirror at z = -4 under the camera at the origin, looking down -Z, and
    /// an `offscreen_model` behind it at z = 2 that only reflections show.
    fn with_offscreen_model(
        wall_z: Option<f32>,
        roughness: f32,
        offscreen_model: Material,
    ) -> Option<Self> {
        let (device, queue) = device()?;
        let panel = |z, material| {
            quad(
                [
                    Vec3::new(-20., -20., z),
                    Vec3::new(20., -20., z),
                    Vec3::new(20., 20., z),
                    Vec3::new(-20., 20., z),
                ],
                Vec3::Z,
                material,
            )
        };
        let mut mirror = material([0.9, 0.9, 0.9, 1.], 1., roughness);
        mirror.double_sided = true;
        let mut world = Asset {
            meshes: vec![panel(-4., 0)],
            materials: vec![mirror],
            images: vec![],
            rig: Default::default(),
            ignored: Vec::new(),
        };
        if let Some(z) = wall_z {
            world.materials.push(material([0.1, 0.1, 0.1, 1.], 0., 1.));
            world.meshes.push(panel(z, 1));
        }
        let mut scene = Scene::new(&device, &queue);
        let world = scene.add_asset(&device, &queue, world).unwrap().model;
        let shown = |model| InstanceState {
            model,
            pose: Mat4::IDENTITY,
            visible: true,
            capture_visible: true,
        };
        scene
            .add_instance(&device, &queue, shown(world), Mobility::Static)
            .unwrap();
        let offscreen_model = Asset {
            meshes: vec![panel(2., 0)],
            materials: vec![offscreen_model],
            images: vec![],
            rig: Default::default(),
            ignored: Vec::new(),
        };
        let offscreen_model = scene
            .add_asset(&device, &queue, offscreen_model)
            .unwrap()
            .model;
        let offscreen_model = scene
            .add_instance(&device, &queue, shown(offscreen_model), Mobility::Moving)
            .unwrap();
        let mut input = FrameInput::new(Camera {
            eye: Vec3::ZERO,
            view: Mat4::IDENTITY,
            projection: perspective(1., 1., 0.1),
        });
        input.environment = Some(
            scene
                .add_environment(&device, &queue, &environment(0.25))
                .unwrap(),
        );
        // A game's Ultra: High render resources plus full SSR and world rays.
        let settings = Settings {
            screen_space_reflections: settings::ScreenSpaceReflections::Full,
            world_space_reflections: settings::WorldSpaceReflections::Moving,
            scene_resolution: settings::SceneResolution::Full,
            atmosphere: false,
            bloom: settings::Bloom::Off,
            antialiasing: settings::Antialiasing::Off,
            ..Settings::default()
        };
        let renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            SIZE,
            1.,
            &settings,
        )
        .unwrap();
        let output = target(&device, SIZE);
        Some(Self {
            device,
            queue,
            scene,
            offscreen_model,
            renderer,
            settings,
            input,
            output,
            size: SIZE,
        })
    }
    fn render(&mut self) -> f32 {
        for index in 0..6 {
            self.frame(index == 0);
        }
        self.reflected()
    }
    /// The last frame's mean radiance over the central receivers.
    fn reflected(&self) -> f32 {
        let pixels = self.composite();
        // Central receivers see only the mirror in primary visibility. Their
        // secondary rays head behind the camera toward the offscreen model and
        // wall.
        let mut sum = 0.;
        for y in 56..72 {
            for x in 56..72 {
                let p = pixels[(y * SIZE[0] + x) as usize];
                assert!(
                    p.iter().all(|v| v.is_finite()),
                    "nonfinite reflected radiance: {p:?}"
                );
                sum += (p[0] + p[1] + p[2]) / 3.;
            }
        }
        sum / 256.
    }
    fn frame(&mut self, camera_cut: bool) {
        let encoder = self.encode_frame(camera_cut);
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
    }
    fn encode_frame(&mut self, camera_cut: bool) -> wgpu::CommandEncoder {
        self.input.camera_cut = camera_cut;
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
        encoder
    }
    /// The last frame's HDR composite.
    fn composite(&self) -> Vec<[f32; 4]> {
        self.rgba(
            self.renderer
                .diagnostic_target(DiagnosticTarget::Composite)
                .unwrap(),
        )
    }
    fn history() -> Option<Self> {
        let mut frames = Self::with_roughness(None, 0.12)?;
        frames.move_offscreen_model(0.);
        Some(frames)
    }
    fn move_offscreen_model(&mut self, x: f32) {
        self.pose_offscreen_model(
            Mat4::from_translation(Vec3::new(x, 0., 0.))
                * Mat4::from_scale(Vec3::new(0.04, 0.04, 1.)),
        );
    }
    fn pose_offscreen_model(&mut self, pose: Mat4) {
        let state = InstanceState {
            pose,
            ..*self.scene.instance(self.offscreen_model).unwrap()
        };
        self.scene
            .set_instance(&self.queue, self.offscreen_model, state)
            .unwrap();
    }
    fn move_camera(&mut self, x: f32) {
        let eye = Vec3::new(x, 0., 0.);
        self.input.camera.eye = eye;
        self.input.camera.view = camera::rh::view::look_at_mat4(eye, eye - Vec3::Z, Vec3::Y);
    }
    fn resize(&mut self, size: [u32; 2]) {
        self.size = size;
        self.input.camera.projection = perspective(1., size[0] as f32 / size[1] as f32, 0.1);
        self.renderer.resize(&self.device, size, 1., &self.settings);
        self.output = target(&self.device, size);
    }
    /// Texels of an RGBA16F target, row-major.
    fn rgba(&self, view: &wgpu::TextureView) -> Vec<[f32; 4]> {
        let texture = view.texture();
        let [width, height] = self.size;
        let row = (width * 8).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite readback"),
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
            .flat_map(|line| line[..(width * 8) as usize].chunks_exact(8))
            .map(|texel| std::array::from_fn(|c| half(&texel[c * 2..c * 2 + 2])))
            .collect()
    }
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

// Defect: world-space ray hits shade without the scene's lights (their list
// empty or unbound). Independent signal: a diffuse-only baked light adds
// nothing to the metal mirror itself, so it can reach the composite only
// through the mirror's ray hits on the moving offscreen model that only
// reflections show.
#[test]
fn ray_hits_on_a_moving_instance_take_scene_lights() {
    let Some(mut frames) =
        Frames::with_offscreen_model(None, 0.045, material([0.9, 0.9, 0.9, 1.], 0., 1.))
    else {
        return;
    };
    let dark = frames.render();
    frames
        .scene
        .add_light(
            &frames.device,
            &frames.queue,
            sgl_3d::Light {
                position: Vec3::new(0., 0., 1.),
                shape: sgl_3d::LightShape::Point {
                    radius: sgl_3d::LightShape::DEFAULT_RADIUS,
                },
                color: [1.; 3],
                intensity: 20.,
                range: 4.,
                baked: true,
                specular: 0.,
                casts_shadow: false,
                ..Default::default()
            },
        )
        .unwrap();
    let lit = frames.render();
    eprintln!("world ray hits: without the light {dark}, with it {lit}");
    assert!(
        lit > dark + 0.5,
        "the offscreen model's reflection took no light: {lit} vs {dark}"
    );
}

// Defect: dropping static instances before intersection reflects the
// offscreen model through an opaque wall. Independent signal: GPU HDR radiance
// must return to the same scene's normal fallback when a wall lies between
// mirror and offscreen model; a wall beyond the offscreen model must preserve
// it. Shader/type checks cannot detect this scene-visibility error. Executes
// production world trace and composition.
#[test]
fn opaque_static_geometry_blocks_offscreen_model_in_ultra() {
    let Some(mut clear) = Frames::new(None) else {
        return;
    };
    let clear_ray = clear.render();
    clear.settings.world_space_reflections = settings::WorldSpaceReflections::Off;
    let fallback = clear.render();
    let mut blocked = Frames::new(Some(1.)).unwrap();
    let blocked_ray = blocked.render();
    blocked.settings.world_space_reflections = settings::WorldSpaceReflections::Off;
    let blocked_fallback = blocked.render();
    let mut behind = Frames::new(Some(3.)).unwrap();
    let behind_ray = behind.render();
    eprintln!(
        "Ultra world rays: clear={clear_ray}, fallback={fallback}, blocked={blocked_ray}, blocked_fallback={blocked_fallback}, wall_behind_offscreen_model={behind_ray}"
    );
    assert!(
        clear_ray > fallback + 1.,
        "unobstructed moving object must supply reflected radiance"
    );
    assert!(
        (blocked_ray - blocked_fallback).abs() < 0.01,
        "static closest hit must retain normal fallback: {blocked_ray} versus {blocked_fallback}"
    );
    assert!(
        (behind_ray - clear_ray).abs() < 0.01,
        "a wall beyond the offscreen model must not suppress the nearer moving hit"
    );
}

// Defect: world-space ray hits shade with the sky alone, or the trace binds an
// empty probe collection, so a reflected moving object inside a probe shows sky
// specular where probe captures take the probe's; or the hit looks up the wrong
// cell of the probe grid (transposed axes, a wrong stride). Independent signal:
// a probe whose influence holds the lit metal offscreen model and no receiver
// leaves the composite unchanged with world rays off, and with them on the
// reflected offscreen model takes its radiance (16 times the sky's); a second
// probe far from the scene changes nothing the rays see. Executes the
// production world trace and composition.
#[test]
fn ray_hits_take_installed_probe_specular() {
    let Some(mut frames) = Frames::with_offscreen_model(None, 0.045, material([1.; 4], 1., 0.5))
    else {
        return;
    };
    let sky_ray = frames.render();
    frames.settings.world_space_reflections = settings::WorldSpaceReflections::Off;
    let sky_fallback = frames.render();
    let face = 64usize;
    // Radiance four (binary16 0x4400) in every direction and at every level.
    let texels = [0x4400u16, 0x4400, 0x4400, 0x3c00]
        .into_iter()
        .cycle()
        .take((0..7).map(|mip| 6 * (face >> mip).pow(2) * 4).sum())
        .collect();
    let probe = sgl_3d::BakedSpecularProbe {
        center: Vec3::new(0., 0., 2.),
        world_to_local: Mat4::IDENTITY,
        // The offscreen model's plane, behind the camera; the mirror is at z = -4.
        influence: sgl_3d::SpecularProbeBox {
            min: Vec3::new(-30., -30., 1.),
            max: Vec3::new(30., 30., 3.),
        },
        blend: Vec3::ZERO,
        proxy: None,
        radiance: sgl_3d::SpecularProbeRadiance {
            face_size: face as u32,
            texels: sgl_3d::SpecularProbeTexels::Rgba16Float(texels),
        },
    };
    frames
        .scene
        .set_baked_specular_probes(&frames.device, &frames.queue, std::slice::from_ref(&probe))
        .unwrap();
    let probe_fallback = frames.render();
    frames.settings.world_space_reflections = settings::WorldSpaceReflections::Moving;
    let probe_ray = frames.render();
    // Far off on all three axes, it makes the grid's axes unequal and puts the
    // offscreen model's cell at different coordinates on each, so a transposed
    // or mis-strided lookup reads a cell that does not list the first probe.
    let far = sgl_3d::BakedSpecularProbe {
        center: Vec3::new(-205., 105., -90.),
        influence: sgl_3d::SpecularProbeBox {
            min: Vec3::new(-210., 100., -95.),
            max: Vec3::new(-200., 110., -85.),
        },
        ..probe.clone()
    };
    frames
        .scene
        .set_baked_specular_probes(&frames.device, &frames.queue, &[probe, far])
        .unwrap();
    let two_probe_ray = frames.render();
    eprintln!(
        "Ray hit environment: sky={sky_ray}, probe={probe_ray}, with a far probe={two_probe_ray}, fallback without probe={sky_fallback}, with probe={probe_fallback}"
    );
    assert!(
        (probe_fallback - sky_fallback).abs() < 0.01,
        "the probe must hold no receiver: {probe_fallback} versus {sky_fallback}"
    );
    assert!(
        probe_ray > sky_ray + 1.,
        "a ray hit inside a probe must take its specular: {probe_ray} versus {sky_ray}"
    );
    assert_eq!(
        two_probe_ray, probe_ray,
        "a probe far from every hit must not change the hits' specular"
    );
}

fn max_difference(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(a, b)| a[..3].iter().zip(&b[..3]))
        .map(|(a, b)| {
            assert!(a.is_finite() && b.is_finite());
            (a - b).abs()
        })
        .fold(0., f32::max)
}

// Defect: old world histories survive discontinuities although motion vectors
// describe only the preceding frame. Independent oracle: a new effect rendering
// the same scene. The glossy reflected model's edges make stale accumulation
// observable in actual HDR output, beyond shader/type validation.
#[test]
fn world_history_restarts_after_discontinuities() {
    let mut failed = Vec::new();
    for transition in [
        "camera cut",
        "abandoned encode then retry",
        "SSR off",
        "world off",
        "resize",
    ] {
        let Some(mut dirty) = Frames::history() else {
            return;
        };
        for i in 0..8 {
            dirty.frame(i == 0);
        }
        let mut fresh = Frames::history().unwrap();
        dirty.move_offscreen_model(0.3);
        fresh.move_offscreen_model(0.3);
        dirty.move_camera(0.15);
        fresh.move_camera(0.15);
        match transition {
            "abandoned encode then retry" => {
                drop(dirty.encode_frame(false));
            }
            "SSR off" => {
                dirty.settings.screen_space_reflections = settings::ScreenSpaceReflections::Off;
                dirty.frame(false);
                dirty.frame(false);
                dirty.settings.screen_space_reflections = settings::ScreenSpaceReflections::Full;
            }
            "world off" => {
                dirty.settings.world_space_reflections = settings::WorldSpaceReflections::Off;
                dirty.frame(false);
                dirty.settings.world_space_reflections = settings::WorldSpaceReflections::Moving;
            }
            "resize" => {
                dirty.resize([144, 112]);
                fresh.resize([144, 112]);
            }
            _ => {}
        }
        dirty.frame(transition == "camera cut");
        fresh.frame(true);
        let difference = max_difference(&dirty.composite(), &fresh.composite());
        eprintln!("World history {transition}: max HDR difference from new effect={difference}");
        if difference >= 0.001 {
            failed.push((transition, difference));
        }
    }
    assert!(
        failed.is_empty(),
        "world history survived discontinuities: {failed:?}"
    );
}

// Defect: after a resize, a world reflection pass keeps a group that binds
// a target from before it, on either side of the history pair. Independent
// oracle: a new effect at the new size, frame by frame while the history
// pair alternates.
#[test]
fn world_frames_after_a_resize_equal_a_new_effect() {
    let Some(mut resized) = Frames::history() else {
        return;
    };
    for i in 0..8 {
        resized.frame(i == 0);
    }
    let mut fresh = Frames::history().unwrap();
    let size = [144, 112];
    resized.resize(size);
    fresh.resize(size);
    for frame in 0..3 {
        resized.frame(false);
        fresh.frame(frame == 0);
        let difference = max_difference(&resized.composite(), &fresh.composite());
        eprintln!(
            "World frame {frame} after a resize: max HDR difference from new effect={difference}"
        );
        assert!(
            difference < 0.001,
            "frame {frame} after the resize differs from a new effect by {difference}"
        );
    }
}

#[test]
fn consecutive_world_frames_keep_accumulation() {
    let Some(mut accumulated) = Frames::history() else {
        return;
    };
    for i in 0..8 {
        accumulated.frame(i == 0);
    }
    let mut fresh = Frames::history().unwrap();
    fresh.frame(true);
    let difference = max_difference(&accumulated.composite(), &fresh.composite());
    eprintln!("World consecutive history: max HDR difference from new effect={difference}");
    assert!(
        difference > 0.01,
        "consecutive frames must retain progressive sampling and accumulation"
    );
}

// Defect: a frame with world rays traces the ray instances an earlier frame
// uploaded, or none, so after a frame without world rays its rays find a
// moving object where it was, or nowhere. Independent signal: the unlit
// offscreen model (radiance 6 against a 0.25 sky) raises the central
// receivers' reflected radiance only where their rays hit it. Moved into their
// rays from beyond every one of them, during a frame without world rays, it
// must show in the next frame with them. Executes the production prepare
// upload, world trace and composition.
#[test]
fn world_rays_trace_poses_moved_while_they_were_off() {
    let Some(mut frames) = Frames::new(None) else {
        return;
    };
    frames.pose_offscreen_model(Mat4::from_translation(Vec3::X * 100.));
    let aside = frames.render();
    frames.settings.world_space_reflections = settings::WorldSpaceReflections::Off;
    frames.frame(false);
    frames.pose_offscreen_model(Mat4::IDENTITY);
    frames.settings.world_space_reflections = settings::WorldSpaceReflections::Moving;
    frames.frame(false);
    let moved = frames.reflected();
    eprintln!(
        "World rays after a frame without them: offscreen model aside={aside}, moved into view={moved}"
    );
    assert!(
        moved > aside + 1.,
        "the first world frame must trace the offscreen model where it now is: {moved} versus {aside}"
    );
}
