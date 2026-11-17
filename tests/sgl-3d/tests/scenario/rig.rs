//! The native rig: a device, renderer, scene and output target driven one
//! frame at a time through the public API, fixtures whose radiance is known
//! by construction, typed readback of the frame's targets, and opt-in dumps.
use super::geometry::{FRAME_RATE, Pose, View};
use super::image::Image;
use sgl_3d::asset::{Asset, CpuMesh, Material, Vertex};
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::{Mat4, Vec2, Vec3};
use sgl_3d::settings::{self, Settings};
use sgl_3d::{
    Backdrop, Exposure, FrameInput, InstanceId, InstanceState, MaterialId, MaterialShader,
    Mobility, Renderer, Scene, ShaderId, SurfaceMaterial,
};
use std::cell::RefCell;
use std::path::PathBuf;

/// One step of a half float relative to its value (10 fraction bits): the
/// most one store into an Rgba16Float target changes a value, whether the
/// GPU rounds to nearest or toward zero.
pub const HALF_STEP: f32 = 1. / 1024.;

/// A device and its queue.
#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// A device with the renderer's limits and optional features, or `None`
/// after printing why. `SGL_REQUIRE_GPU` (`specs/testing.md` requirement 5)
/// turns a missing adapter into a failure.
pub fn gpu() -> Option<Gpu> {
    device(sgl_3d::graphics_device::features)
}

/// A device with FSR's features too, or `None` after printing that the
/// adapter lacks them (FSR is optional; TAA would replace it) or why there
/// is no device.
pub fn fsr_gpu() -> Option<Gpu> {
    device(|adapter| {
        let fsr = sgl_3d::graphics_device::fsr_features(adapter);
        if fsr.is_empty() {
            return None;
        }
        Some(sgl_3d::graphics_device::features(adapter) | fsr)
    })
}

fn device<F, R>(features: F) -> Option<Gpu>
where
    F: FnOnce(&wgpu::Adapter) -> R,
    R: Into<Option<wgpu::Features>>,
{
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let adapter =
        match pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())) {
            Ok(adapter) => adapter,
            Err(error) => {
                assert!(
                    !required,
                    "GPU test cannot run but SGL_REQUIRE_GPU is set: {error}"
                );
                eprintln!("skipping GPU scenario: {error}");
                return None;
            }
        };
    let Some(features) = features(&adapter).into() else {
        eprintln!("skipping: the adapter lacks FSR's features");
        return None;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        ..Default::default()
    }))
    .expect("a device the adapter offers");
    Some(Gpu { device, queue })
}

/// The settings a fixture's truth assumes, with `antialiasing` under test:
/// the scene at the output size with no upscaling, FSR at native quality
/// without sharpening (RCAS deliberately moves edges away from their
/// coverage, which the reference oracle measures), and everything that would change an unlit card's
/// radiance or the frame before antialiasing off (bloom, ambient
/// occlusion, screen-space and world-space reflections, atmosphere, dynamic
/// GI, motion blur). The frame input adds a fixed exposure of 0 stops, no
/// fog, mist or lights, and a backdrop of one colour.
pub fn fixture_settings(antialiasing: settings::Antialiasing) -> Settings {
    Settings {
        scene_resolution: settings::SceneResolution::Full,
        upscaling: settings::Upscaling::Off,
        antialiasing,
        fsr_quality: settings::FsrQuality::NativeAa,
        fsr_sharpening: false,
        bloom: settings::Bloom::Off,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        world_space_reflections: settings::WorldSpaceReflections::Off,
        atmosphere: false,
        dynamic_gi: settings::DynamicGiQuality::Off,
        motion_blur: settings::MotionBlur::Off,
        ..Settings::default()
    }
}

/// One unlit quad of a shape, in the shape's own frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub corners: [Vec3; 4],
    pub radiance: f32,
}

/// A fixture's geometry: unlit quads of stated radiance, so a pixel wholly
/// inside one shows that radiance and an edge pixel its box-filtered
/// coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub quads: Vec<Quad>,
}

impl Shape {
    /// A card `size` metres across facing +Z, turned `roll` radians about
    /// its normal, of `radiance`.
    pub fn card(size: Vec2, roll: f32, radiance: f32) -> Shape {
        Shape {
            quads: vec![quad(-size * 0.5, size * 0.5, 0., roll, radiance)],
        }
    }

    /// A card of `radiance` carrying strips of `strip_radiance` along its
    /// height, `width` metres wide every `period` metres, 1 mm in front of
    /// it, all turned `roll` radians: the stationary sub-pixel geometry of
    /// #767 where `width` is below a pixel.
    pub fn grating(
        size: Vec2,
        roll: f32,
        radiance: f32,
        strip_radiance: f32,
        width: f32,
        period: f32,
    ) -> Shape {
        let mut quads = vec![quad(-size * 0.5, size * 0.5, 0., roll, radiance)];
        let mut x = -size.x * 0.5 + (period - width) * 0.5;
        while x + width <= size.x * 0.5 {
            quads.push(quad(
                Vec2::new(x, -size.y * 0.5),
                Vec2::new(x + width, size.y * 0.5),
                0.001,
                roll,
                strip_radiance,
            ));
            x += period;
        }
        Shape { quads }
    }

    /// The same shape `factor` times the size about its own origin. Placed
    /// `factor` times as far from a camera, it projects to the same pixels
    /// at `factor` times the depth: the same picture with only depth's
    /// magnitude changed.
    pub fn scaled(&self, factor: f32) -> Shape {
        Shape {
            quads: self
                .quads
                .iter()
                .map(|q| Quad {
                    corners: q.corners.map(|c| c * factor),
                    ..*q
                })
                .collect(),
        }
    }

    /// Every quad's corners at `pose`.
    pub fn quads_at(&self, pose: Pose) -> Vec<[Vec3; 4]> {
        let matrix = pose.matrix();
        self.quads
            .iter()
            .map(|q| q.corners.map(|c| matrix.transform_point3(c)))
            .collect()
    }

    fn asset(&self, group: u32) -> Asset {
        let materials = self
            .quads
            .iter()
            .map(|q| Material {
                name: "scenario fixture".into(),
                visibility_group: group,
                base: [q.radiance, q.radiance, q.radiance, 1.],
                metallic: 0.,
                double_sided: true,
                unlit: true,
                ..Material::default()
            })
            .collect();
        let meshes = self
            .quads
            .iter()
            .enumerate()
            .map(|(material, q)| CpuMesh {
                vertices: q
                    .corners
                    .map(|position| Vertex {
                        position: position.to_array(),
                        normal: [0., 0., 1.],
                        uv: [0.; 2],
                        color: [1.; 4],
                        tangent: [1., 0., 0., 1.],
                        lightmap_bounds: [0., 0., 1., 1.],
                        lightmap_uv: [0.; 2],
                    })
                    .to_vec(),
                indices: vec![0, 1, 2, 0, 2, 3],
                material,
                deformation: Default::default(),
            })
            .collect();
        Asset {
            meshes,
            materials,
            images: Vec::new(),
            rig: Default::default(),
            ignored: Vec::new(),
        }
    }
}

fn quad(low: Vec2, high: Vec2, z: f32, roll: f32, radiance: f32) -> Quad {
    let turn = Mat4::from_rotation_z(roll);
    Quad {
        corners: [
            (low.x, low.y),
            (high.x, low.y),
            (high.x, high.y),
            (low.x, high.y),
        ]
        .map(|(x, y)| turn.transform_point3(Vec3::new(x, y, z))),
        radiance,
    }
}

/// A shape the rig placed.
struct Placed {
    id: InstanceId,
    shape: Shape,
    pose: Pose,
    group: u32,
    visible: bool,
    /// Drawn through a game's shader ([`Rig::add_shaded`]).
    shaded: bool,
}

/// A renderer, its scene and its output at `view.size`, driven frame by
/// frame. Scripts set the camera with [`Rig::look`] and edit the scene and
/// `input`; [`Rig::render`] derives the clock from the frame index.
pub struct Rig {
    pub gpu: Gpu,
    pub view: View,
    pub scene: Scene,
    pub renderer: Renderer,
    pub settings: Settings,
    pub input: FrameInput,
    output: (wgpu::TextureView, [u32; 2]),
    placed: Vec<Placed>,
    /// The render origin's sum of moves, which scripts' world positions
    /// are translated by.
    origin: Vec3,
    /// The camera's pose (world).
    camera: Pose,
    /// The lowest and highest radiance the rig's fixtures had: the sky's and
    /// every shape's it held, departed ones included, since history may
    /// still hold them.
    range: [f32; 2],
    /// The antialiased frames' furthest excursion beyond the fixture's
    /// radiance range, relative to it, and where it was.
    excursion: RefCell<Option<(f32, String)>>,
}

impl Rig {
    /// A rig whose backdrop is the grey `sky`, with `antialiasing` in effect:
    /// it fails if the renderer would replace it (FSR falling back to TAA).
    pub fn new(gpu: &Gpu, view: View, antialiasing: settings::Antialiasing, sky: f32) -> Rig {
        let settings = fixture_settings(antialiasing);
        let renderer = Renderer::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            view.size,
            1.,
            &settings,
        )
        .expect("a renderer");
        assert_eq!(
            renderer.antialiasing_in_effect(&settings),
            antialiasing,
            "the antialiasing under test must run: {:?}",
            renderer.fsr_error()
        );
        let mut input = FrameInput::new(view.camera(Pose::at(Vec3::ZERO)));
        input.backdrop = Backdrop::Color([sky; 3]);
        input.exposure = Exposure {
            stops: 0.,
            automatic: None,
        };
        input.visibility_mask = 0x00ff_ffff;
        Rig {
            output: (target(&gpu.device, view.size), view.size),
            scene: Scene::new(&gpu.device, &gpu.queue),
            gpu: gpu.clone(),
            view,
            renderer,
            settings,
            input,
            placed: Vec::new(),
            origin: Vec3::ZERO,
            camera: Pose::at(Vec3::ZERO),
            range: [sky; 2],
            excursion: RefCell::new(None),
        }
    }

    /// Adds `shape` at `pose` as a moving instance in visibility `group`.
    pub fn add(&mut self, shape: &Shape, pose: Pose, group: u32) -> InstanceId {
        self.add_placed(shape, pose, group, false).0
    }

    /// Adds `shape` at `pose` as a moving instance in visibility `group`,
    /// each of its materials drawn through `shader` (a game's,
    /// `Scene::add_shader` on `scene`) with `params` as its `ShaderParams`
    /// and its vertices moving at most `bound` metres. The rig's poses do
    /// not include what the shader moves.
    pub fn add_shaded(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        (shader, bound): (ShaderId, f32),
        params: &[u8],
    ) -> InstanceId {
        let (id, materials) = self.add_placed(shape, pose, group, true);
        let queue = &self.gpu.queue;
        for material in materials {
            let values = self.scene.material(material).expect("a fixture material");
            let shader = Some(MaterialShader {
                shader,
                displacement_bound: bound,
            });
            self.scene
                .set_material(queue, material, SurfaceMaterial { shader, ..values })
                .expect("a shaded fixture material");
            self.scene
                .set_shader_parameters(queue, material, params)
                .expect("the shader's parameters");
        }
        id
    }

    fn add_placed(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        shaded: bool,
    ) -> (InstanceId, Vec<MaterialId>) {
        for quad in &shape.quads {
            self.range = [
                self.range[0].min(quad.radiance),
                self.range[1].max(quad.radiance),
            ];
        }
        let (id, materials) = self.instance(shape, pose, group, true);
        self.placed.push(Placed {
            id,
            shape: shape.clone(),
            pose,
            group,
            visible: true,
            shaded,
        });
        (id, materials)
    }

    fn instance(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        visible: bool,
    ) -> (InstanceId, Vec<MaterialId>) {
        let Gpu { device, queue } = &self.gpu;
        let ids = self
            .scene
            .add_asset(device, queue, shape.asset(group))
            .expect("a fixture model");
        let state = InstanceState {
            pose: self.render_pose(pose),
            visible,
            ..InstanceState::new(ids.model)
        };
        let id = self
            .scene
            .add_instance(device, queue, state, Mobility::Moving)
            .expect("a fixture instance");
        (id, ids.materials)
    }

    fn placed(&mut self, id: InstanceId) -> &mut Placed {
        self.placed
            .iter_mut()
            .find(|p| p.id == id)
            .expect("an instance the rig placed")
    }

    /// Poses `id` at `pose` (world, before any origin move).
    pub fn place(&mut self, id: InstanceId, pose: Pose) {
        self.placed(id).pose = pose;
        self.update(id);
    }

    pub fn set_visible(&mut self, id: InstanceId, visible: bool) {
        self.placed(id).visible = visible;
        self.update(id);
    }

    fn update(&mut self, id: InstanceId) {
        let placed = self.placed.iter().find(|p| p.id == id).unwrap();
        let state = InstanceState {
            pose: self.render_pose(placed.pose),
            visible: placed.visible,
            ..*self.scene.instance(id).unwrap()
        };
        self.scene
            .set_instance(&self.gpu.queue, id, state)
            .expect("a fixture pose");
    }

    pub fn remove(&mut self, id: InstanceId) {
        self.scene.remove_instance(id).expect("a placed instance");
        self.placed.retain(|p| p.id != id);
    }

    /// Replaces the scene with an empty one, which restarts history: a
    /// renderer costs its pipelines to create, so a suite reuses one across
    /// runs this way.
    pub fn new_scene(&mut self) {
        self.scene = Scene::new(&self.gpu.device, &self.gpu.queue);
        self.placed.clear();
    }

    /// Replaces the scene with a new one holding the same placed shapes:
    /// another scene, which restarts history. Shaded shapes' shaders belong
    /// to the old scene, so it takes plain shapes only.
    pub fn replace_scene(&mut self) {
        assert!(
            self.placed.iter().all(|p| !p.shaded),
            "replace_scene re-adds plain shapes only"
        );
        self.scene = Scene::new(&self.gpu.device, &self.gpu.queue);
        let placed = std::mem::take(&mut self.placed);
        for p in placed {
            let id = self.instance(&p.shape, p.pose, p.group, p.visible).0;
            self.placed.push(Placed { id, ..p });
        }
    }

    /// The instances the rig placed, in order.
    pub fn instances(&self) -> Vec<InstanceId> {
        self.placed.iter().map(|p| p.id).collect()
    }

    /// The camera at `pose` (world, before any origin move).
    pub fn look(&mut self, pose: Pose) {
        self.camera = pose;
        self.input.camera = self.view.camera(Pose {
            position: pose.position - self.origin,
            ..pose
        });
    }

    /// Moves the scene's render origin by `by`; the rig keeps taking world
    /// positions as before.
    pub fn move_origin(&mut self, by: Vec3) {
        let Gpu { device, queue } = &self.gpu;
        self.scene
            .move_origin(device, queue, by)
            .expect("a finite origin");
        self.origin += by;
        self.look(self.camera);
    }

    fn render_pose(&self, pose: Pose) -> Mat4 {
        Mat4::from_translation(-self.origin) * pose.matrix()
    }

    /// The lowest and highest radiance the rig's fixtures had: the sky's
    /// and every shape's it held.
    pub fn radiance_range(&self) -> [f32; 2] {
        self.range
    }

    /// Renders and submits frame `frame` at `view.size`: `elapsed_seconds`
    /// and `frame_time_ms` come from the index, `resize` runs first (a
    /// no-op unless the size or settings changed), and `camera_cut` holds
    /// for this frame only.
    pub fn render(&mut self, frame: u32) {
        self.input.elapsed_seconds = f64::from(frame) / FRAME_RATE;
        self.input.frame_time_ms = (1000. / FRAME_RATE) as f32;
        if self.output.1 != self.view.size {
            self.output = (target(&self.gpu.device, self.view.size), self.view.size);
            self.look(self.camera);
        }
        let Gpu { device, queue } = &self.gpu;
        self.renderer
            .resize(device, self.view.size, 1., &self.settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        self.renderer.render(
            device,
            queue,
            &mut encoder,
            &mut self.scene,
            &self.input,
            &self.settings,
            &self.output.0,
            None,
        );
        queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
        self.input.camera_cut = false;
    }

    /// `target` of the last frame, decoded.
    pub fn read(&self, target: DiagnosticTarget) -> Image {
        let view = self
            .renderer
            .diagnostic_target(target)
            .unwrap_or_else(|| panic!("{target:?} did not run"));
        read(&self.gpu, view.texture())
    }

    /// The last frame's `Antialiased` target, measured against the bound
    /// oracle (`excursion`).
    pub fn antialiased(&self) -> Image {
        let image = self.read(DiagnosticTarget::Antialiased);
        let [low, high] = self.radiance_range();
        let mut excursion = self.excursion.borrow_mut();
        for (i, texel) in image.texels.iter().enumerate() {
            for &c in &texel[..3] {
                let beyond = if !c.is_finite() || c < 0. {
                    f32::INFINITY
                } else {
                    ((low - c) / low).max((c - high) / high).max(0.)
                };
                if beyond > excursion.as_ref().map_or(0., |e| e.0) {
                    let pixel = [i as u32 % image.size[0], i as u32 / image.size[0]];
                    *excursion = Some((
                        beyond,
                        format!("{c} at {pixel:?} of the range {low}..={high}"),
                    ));
                }
            }
        }
        image
    }

    /// The bound oracle over every antialiased frame read: finite,
    /// non-negative and within the fixture's radiance range, since an unlit
    /// fixture's antialiased pixel blends its radiances and creates none.
    /// `None` when every frame held within `rounding` (relative: the
    /// half-float stores the radiance passed through, `HALF_STEP` each);
    /// otherwise how far the furthest value went beyond the range, relative
    /// to the bound it crossed, and where.
    pub fn excursion(&self, rounding: f32) -> Option<(f32, String)> {
        self.excursion
            .borrow()
            .clone()
            .filter(|(beyond, _)| *beyond > rounding)
    }
}

/// An output texture of `size`.
fn target(device: &wgpu::Device, size: [u32; 2]) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("scenario output"),
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

/// `texture`, read back and decoded by its format.
pub fn read(gpu: &Gpu, texture: &wgpu::Texture) -> Image {
    let format = texture.format();
    type Decode = fn(&[u8]) -> [f32; 4];
    let (bytes, decode): (u32, Decode) = match format {
        wgpu::TextureFormat::Rgba16Float => (8, |t| {
            std::array::from_fn(|c| diagnostics::half(&t[c * 2..c * 2 + 2]))
        }),
        wgpu::TextureFormat::Rgba32Float => (16, |t| {
            std::array::from_fn(|c| f32::from_le_bytes(t[c * 4..c * 4 + 4].try_into().unwrap()))
        }),
        wgpu::TextureFormat::Depth32Float | wgpu::TextureFormat::R32Float => (4, |t| {
            [f32::from_le_bytes(t.try_into().unwrap()), 0., 0., 0.]
        }),
        wgpu::TextureFormat::R32Uint => (4, |t| {
            [u32::from_le_bytes(t.try_into().unwrap()) as f32, 0., 0., 0.]
        }),
        wgpu::TextureFormat::Rg32Uint => (8, |t| {
            let word =
                |c: usize| u32::from_le_bytes(t[c * 4..c * 4 + 4].try_into().unwrap()) as f32;
            [word(0), word(1), 0., 0.]
        }),
        other => panic!("no decoding for {other:?}"),
    };
    let raw = diagnostics::read(&gpu.device, &gpu.queue, texture, bytes);
    Image {
        size: [texture.width(), texture.height()],
        texels: raw.chunks_exact(bytes as usize).map(decode).collect(),
    }
}

/// A scenario's opt-in dump (RD-5): with `SGL_SCENARIO_DUMP` naming a
/// directory (for example `SGL_SCENARIO_DUMP=.cache/scenarios`), frames as
/// PNG and metrics as JSON under `<dir>/<scenario>/`, for agents to look at.
/// Nothing asserts on it; without the variable it writes nothing.
pub struct Dump {
    directory: Option<PathBuf>,
    metrics: serde_json::Map<String, serde_json::Value>,
}

impl Dump {
    pub fn new(scenario: &str) -> Dump {
        let directory = std::env::var_os("SGL_SCENARIO_DUMP")
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join(scenario));
        if let Some(directory) = &directory {
            std::fs::create_dir_all(directory).expect("the dump directory");
        }
        Dump {
            directory,
            metrics: Default::default(),
        }
    }

    /// `image` as `<label>.png`, mapped `v / (1 + v)` and encoded with a 2.2
    /// gamma for viewing only.
    pub fn image(&self, label: &str, image: &Image) {
        let Some(directory) = &self.directory else {
            return;
        };
        let byte = |v: f32| ((v.max(0.) / (1. + v.max(0.))).powf(1. / 2.2) * 255.).round() as u8;
        let png = ::image::RgbaImage::from_fn(image.size[0], image.size[1], |x, y| {
            let [r, g, b, _] = image.at([x, y]);
            ::image::Rgba([byte(r), byte(g), byte(b), 255])
        });
        png.save(directory.join(format!("{label}.png")))
            .expect("a dumped frame");
    }

    /// Records `value` under `key` in the scenario's `metrics.json`.
    pub fn metric(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.metrics.insert(key.into(), value.into());
    }
}

impl Drop for Dump {
    fn drop(&mut self) {
        if let Some(directory) = &self.directory {
            let json = serde_json::to_string_pretty(&self.metrics).unwrap();
            std::fs::write(directory.join("metrics.json"), json).expect("the dumped metrics");
        }
    }
}
