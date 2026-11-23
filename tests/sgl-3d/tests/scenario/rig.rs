//! The native rig: a device, renderer, scene and views, each with its
//! output target, driven one frame at a time through the public API,
//! fixtures whose radiance is known by construction, typed readback of the
//! frame's targets, and opt-in dumps.
use super::geometry::{FRAME_RATE, Pose, View};
use super::image::Image;
use sgl_3d::asset::{Asset, CpuMesh, Material, MaterialTexture, Sampler, TextureWrap, Vertex};
use sgl_3d::deformation::{MorphDelta, MorphTarget};
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::{Mat4, Vec2, Vec3};
use sgl_3d::settings::{self, Settings};
use sgl_3d::{
    AlphaMode, Backdrop, Exposure, FrameInput, InstanceId, InstanceState, MaterialId,
    MaterialShader, Mobility, RenderTexture, RenderTextureId, Renderer, Scene, ShaderId,
    SurfaceMaterial, View as SceneView, ViewFrame, ViewOutput,
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

/// A device with the renderer's limits and optional features, and the
/// texture compression the adapter has, as a game requests them, or `None`
/// after printing why. `SGL_REQUIRE_GPU` (`specs/testing.md` requirement 5)
/// turns a missing adapter into a failure.
pub fn gpu() -> Option<Gpu> {
    device(features)
}

/// [`gpu`]'s device held to S3D-1's floor, WebGPU's default limits
/// (`wgpu::Limits::default()`), so it takes the Basic binding tier.
pub fn floor_gpu() -> Option<Gpu> {
    device_with(features, |_| wgpu::Limits::default())
}

/// The renderer's optional features and the adapter's texture compression.
fn features(adapter: &wgpu::Adapter) -> wgpu::Features {
    sgl_3d::graphics_device::features(adapter)
        | sgl_3d::graphics_device::texture_compression_features(adapter)
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
        Some(features(adapter) | fsr)
    })
}

/// A device with hardware ray tracing's feature too where the adapter has
/// it (`graphics_device::ray_tracing_features`), accepting wgpu's
/// experimental ray queries as a game that requests them does; otherwise
/// [`gpu`]'s.
pub fn ray_gpu() -> Option<Gpu> {
    device(|adapter| features(adapter) | sgl_3d::graphics_device::ray_tracing_features(adapter))
}

fn device<F, R>(features: F) -> Option<Gpu>
where
    F: FnOnce(&wgpu::Adapter) -> R,
    R: Into<Option<wgpu::Features>>,
{
    device_with(features, sgl_3d::graphics_device::limits)
}

fn device_with<F, R>(features: F, limits: fn(&wgpu::Adapter) -> wgpu::Limits) -> Option<Gpu>
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
    let experimental_features = if features.contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY) {
        // SAFETY: the scenarios that ask for ray queries accept wgpu's
        // experimental ray tracing, as a game that requests it does.
        unsafe { wgpu::ExperimentalFeatures::enabled() }
    } else {
        wgpu::ExperimentalFeatures::disabled()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: limits(&adapter),
        experimental_features,
        ..Default::default()
    }))
    .expect("a device the adapter offers");
    Some(Gpu { device, queue })
}

/// The settings a fixture's truth assumes, with `antialiasing` under test:
/// the scene at the output size with no upscaling, FSR at native quality,
/// no sharpening (RCAS deliberately moves edges away from their coverage,
/// which the reference oracle measures where it reads the displayed
/// image), and everything that would change an unlit card's
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
        sharpening: settings::Sharpening::Off,
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
    /// Drawn blended with this alpha (`AlphaMode::Blend`), which writes no
    /// depth or motion; `None` opaque.
    pub blend: Option<f32>,
}

impl Shape {
    /// A card `size` metres across facing +Z, turned `roll` radians about
    /// its normal, of `radiance`.
    pub fn card(size: Vec2, roll: f32, radiance: f32) -> Shape {
        Shape {
            quads: vec![quad(-size * 0.5, size * 0.5, 0., roll, radiance)],
            blend: None,
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
        Shape { quads, blend: None }
    }

    /// The same shape drawn blended with `alpha`: over what lies behind it
    /// a quad shows `alpha` of its radiance and `1 - alpha` of what is
    /// behind.
    pub fn blended(&self, alpha: f32) -> Shape {
        Shape {
            blend: Some(alpha),
            ..self.clone()
        }
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
            blend: self.blend,
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
                base: [q.radiance, q.radiance, q.radiance, self.blend.unwrap_or(1.)],
                metallic: 0.,
                double_sided: true,
                unlit: true,
                alpha: match self.blend {
                    Some(_) => AlphaMode::Blend {
                        receives_screen_space_reflections: false,
                        keeps_specular: false,
                    },
                    None => AlphaMode::Opaque,
                },
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
                        uv1: [0.0; 2],
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
            lights: Default::default(),
            instanced: Vec::new(),
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
    mobility: Mobility,
}

/// One of the rig's views: the renderer's view, the lens it sees through
/// at `lens.size`, its camera, input and settings, and its output.
pub struct RigView {
    pub view: SceneView,
    pub lens: View,
    pub input: FrameInput,
    pub settings: Settings,
    /// The camera's pose (world).
    camera: Pose,
    output: (wgpu::TextureView, [u32; 2]),
    /// The render texture it draws into in place of its output
    /// ([`Rig::add_producer`]).
    produces: Option<RenderTextureId>,
}

/// A renderer, its scene and its views, driven frame by frame. Scripts set
/// each view's camera with [`Rig::look_view`] ([`Rig::look`] for the first)
/// and edit the scene and each view's `input`; [`Rig::render`] derives the
/// clock from the frame index. The single-view methods act on view 0.
pub struct Rig {
    pub gpu: Gpu,
    pub scene: Scene,
    pub renderer: Renderer,
    /// The views, view 0 first; a frame lists them in this order unless
    /// [`Rig::render_order`] says otherwise.
    pub views: Vec<RigView>,
    placed: Vec<Placed>,
    /// The render origin's sum of moves, which scripts' world positions
    /// are translated by.
    origin: Vec3,
    /// The lowest and highest radiance the rig's fixtures had: the sky's and
    /// every shape's it held, departed ones included, since history may
    /// still hold them.
    range: [f32; 2],
    /// The antialiased frames' furthest excursion beyond the fixture's
    /// radiance range, relative to it, and where and when it was.
    excursion: RefCell<Option<(f32, String)>>,
    /// The index of the frame last rendered.
    frame: u32,
}

impl Rig {
    /// A rig of one view through `lens` whose backdrop is the grey `sky`,
    /// with `antialiasing` in effect: it fails if the renderer would replace
    /// it (FSR falling back to TAA).
    pub fn new(gpu: &Gpu, lens: View, antialiasing: settings::Antialiasing, sky: f32) -> Rig {
        let mut input = FrameInput::new(lens.camera(Pose::at(Vec3::ZERO)));
        input.backdrop = Backdrop::Color([sky; 3]);
        input.exposure = Exposure {
            stops: 0.,
            automatic: None,
        };
        input.visibility_mask = 0x00ff_ffff;
        let renderer = Renderer::new(&gpu.device);
        let view = rig_view(
            gpu,
            &renderer,
            lens,
            fixture_settings(antialiasing),
            input,
            None,
        );
        Rig {
            scene: Scene::new(&gpu.device, &gpu.queue),
            gpu: gpu.clone(),
            renderer,
            views: vec![view],
            placed: Vec::new(),
            origin: Vec3::ZERO,
            range: [sky; 2],
            excursion: RefCell::new(None),
            frame: 0,
        }
    }

    /// Adds a view through `lens` with `antialiasing` in effect, its input
    /// as view 0's was when the rig was made (backdrop, exposure and
    /// visibility mask), its camera at the origin until [`Rig::look_view`].
    /// Returns its index.
    pub fn add_view(&mut self, lens: View, antialiasing: settings::Antialiasing) -> usize {
        self.add_view_into(lens, antialiasing, None)
    }

    /// [`Rig::add_view`] of a view that draws into the scene's render
    /// texture `texture`, of `lens`'s size, in place of an output of its
    /// own.
    pub fn add_producer(
        &mut self,
        lens: View,
        antialiasing: settings::Antialiasing,
        texture: RenderTextureId,
    ) -> usize {
        self.add_view_into(lens, antialiasing, Some(texture))
    }

    fn add_view_into(
        &mut self,
        lens: View,
        antialiasing: settings::Antialiasing,
        produces: Option<RenderTextureId>,
    ) -> usize {
        let mut input = self.views[0].input;
        input.camera_cut = false;
        let view = rig_view(
            &self.gpu,
            &self.renderer,
            lens,
            fixture_settings(antialiasing),
            input,
            produces,
        );
        self.views.push(view);
        let index = self.views.len() - 1;
        self.look_view(index, Pose::at(Vec3::ZERO));
        index
    }

    /// Adds an unlit black card `size` metres across facing +Z that emits
    /// `emission`, times its emission map from corner to corner (UV 0..=1,
    /// V down) once a script gives its material one
    /// (`SurfaceMaterial::emission_render_texture`), at `pose` as a moving
    /// instance in visibility group 0: a screen, or a source of one flat
    /// colour. It is not one of the rig's shapes, as
    /// [`Rig::add_textured`]'s card is not.
    pub fn add_emissive(
        &mut self,
        size: Vec2,
        emission: [f32; 3],
        pose: Pose,
    ) -> (InstanceId, MaterialId) {
        let [low, high] = [-size * 0.5, size * 0.5];
        let vertex = |x: f32, y: f32, u: f32, v: f32| Vertex {
            uv1: [0.0; 2],
            position: [x, y, 0.],
            normal: [0., 0., 1.],
            uv: [u, v],
            color: [1.; 4],
            tangent: [1., 0., 0., 1.],
            lightmap_bounds: [0., 0., 1., 1.],
            lightmap_uv: [0.; 2],
        };
        let asset = Asset {
            meshes: vec![CpuMesh {
                vertices: vec![
                    vertex(low.x, low.y, 0., 1.),
                    vertex(high.x, low.y, 1., 1.),
                    vertex(high.x, high.y, 1., 0.),
                    vertex(low.x, high.y, 0., 0.),
                ],
                indices: vec![0, 1, 2, 0, 2, 3],
                material: 0,
                deformation: Default::default(),
            }],
            materials: vec![Material {
                name: "scenario emissive fixture".into(),
                base: [0., 0., 0., 1.],
                emissive: emission,
                // White, which rays see, until a render texture takes its
                // place, sampled with this map's clamping sampler.
                emissive_texture: Some(MaterialTexture {
                    sampler: Sampler {
                        wrap: [TextureWrap::ClampToEdge; 2],
                        ..Sampler::default()
                    },
                    ..MaterialTexture::new(0)
                }),
                metallic: 0.,
                double_sided: true,
                unlit: true,
                ..Material::default()
            }],
            images: vec![sgl_3d::asset::Image::Rgba8(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([255; 4]),
            ))],
            rig: Default::default(),
            ignored: Vec::new(),
            lights: Default::default(),
            instanced: Vec::new(),
        };
        let Gpu { device, queue } = &self.gpu;
        let ids = self
            .scene
            .add_asset(device, queue, asset)
            .expect("an emissive fixture model");
        let state = InstanceState {
            pose: self.render_pose(pose),
            ..InstanceState::new(ids.model)
        };
        let instance = self
            .scene
            .add_instance(device, queue, state, Mobility::Moving)
            .expect("an emissive fixture instance");
        (instance, ids.materials[0])
    }

    /// Drops view `index`; the views after it move down one.
    pub fn remove_view(&mut self, index: usize) {
        self.views.remove(index);
    }

    /// Adds `shape` at `pose` as a moving instance in visibility `group`.
    pub fn add(&mut self, shape: &Shape, pose: Pose, group: u32) -> InstanceId {
        self.add_placed(shape, pose, group, false).0
    }

    /// [`Rig::add`] as a static instance (`Mobility::Static`), which writes
    /// no motion when [`Rig::place`] moves it.
    pub fn add_static(&mut self, shape: &Shape, pose: Pose, group: u32) -> InstanceId {
        self.add_placed_as(shape, pose, group, false, Mobility::Static)
            .0
    }

    /// [`Rig::add`], with its quads' materials in order, which a script
    /// edits with `Scene::set_material` on `scene`.
    pub fn add_editable(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
    ) -> (InstanceId, Vec<MaterialId>) {
        self.add_placed(shape, pose, group, false)
    }

    /// Adds an unlit card `size` metres across facing +Z, whose base colour
    /// is `image` from corner to corner (UV 0..=1, V down), at `pose` as a
    /// moving instance in visibility group 0. It is not one of the rig's
    /// shapes: `instances`, `replace_scene` and the radiance range leave it
    /// out.
    pub fn add_textured(&mut self, size: Vec2, image: sgl_3d::asset::Image, pose: Pose) {
        let [low, high] = [-size * 0.5, size * 0.5];
        let vertex = |x: f32, y: f32, u: f32, v: f32| Vertex {
            uv1: [0.0; 2],
            position: [x, y, 0.],
            normal: [0., 0., 1.],
            uv: [u, v],
            color: [1.; 4],
            tangent: [1., 0., 0., 1.],
            lightmap_bounds: [0., 0., 1., 1.],
            lightmap_uv: [0.; 2],
        };
        let asset = Asset {
            meshes: vec![CpuMesh {
                vertices: vec![
                    vertex(low.x, low.y, 0., 1.),
                    vertex(high.x, low.y, 1., 1.),
                    vertex(high.x, high.y, 1., 0.),
                    vertex(low.x, high.y, 0., 0.),
                ],
                indices: vec![0, 1, 2, 0, 2, 3],
                material: 0,
                deformation: Default::default(),
            }],
            materials: vec![Material {
                name: "scenario textured fixture".into(),
                base: [1.; 4],
                base_texture: Some(MaterialTexture::new(0)),
                metallic: 0.,
                double_sided: true,
                unlit: true,
                ..Material::default()
            }],
            images: vec![image],
            rig: Default::default(),
            ignored: Vec::new(),
            lights: Default::default(),
            instanced: Vec::new(),
        };
        let Gpu { device, queue } = &self.gpu;
        let ids = self
            .scene
            .add_asset(device, queue, asset)
            .expect("a textured fixture model");
        let state = InstanceState {
            pose: self.render_pose(pose),
            ..InstanceState::new(ids.model)
        };
        self.scene
            .add_instance(device, queue, state, Mobility::Moving)
            .expect("a textured fixture instance");
    }

    /// Adds `shape` at `pose` as a moving instance in visibility `group`,
    /// each of its materials drawn through `shader` (a game's,
    /// `Scene::add_shader` on `scene`) with `params` as its `ShaderParams`
    /// and its vertices moving at most `bound` metres, with its quads'
    /// materials in order. The rig's poses do not include what the shader
    /// moves.
    pub fn add_shaded(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        (shader, bound): (ShaderId, f32),
        params: &[u8],
    ) -> (InstanceId, Vec<MaterialId>) {
        let (id, materials) = self.add_placed(shape, pose, group, true);
        let queue = &self.gpu.queue;
        for &material in &materials {
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
        (id, materials)
    }

    /// Adds `asset` whole at `pose` as a moving instance: a fixture whose
    /// radiance the rig does not know (a lit one), outside the shapes it
    /// places and the radiance range it bounds.
    pub fn add_model(&mut self, asset: Asset, pose: Pose) -> InstanceId {
        let Gpu { device, queue } = &self.gpu;
        let ids = self
            .scene
            .add_asset(device, queue, asset)
            .expect("a fixture model");
        let state = InstanceState {
            pose: self.render_pose(pose),
            ..InstanceState::new(ids.model)
        };
        self.scene
            .add_instance(device, queue, state, Mobility::Moving)
            .expect("a fixture instance")
    }

    /// Adds `shape` at `pose` as a moving instance in visibility group 0
    /// whose every vertex one morph target, of morph weight 0, displaces by
    /// `by` in its own space: `Scene::set_instance_deformation` with a
    /// weight moves it that share of the way with no pose change, no game
    /// shader and no mask, as a game's morph or skin animation does. It is
    /// not one of the rig's shapes: `instances` and `replace_scene` leave
    /// it out, while the radiance range takes it in.
    pub fn add_morphing(&mut self, shape: &Shape, pose: Pose, by: Vec3) -> InstanceId {
        for quad in &shape.quads {
            self.admit(quad.radiance);
        }
        let mut asset = shape.asset(0);
        for mesh in &mut asset.meshes {
            let delta = MorphDelta {
                position: by.to_array(),
                ..MorphDelta::default()
            };
            mesh.deformation.morph_targets = vec![MorphTarget {
                weight: 0,
                deltas: vec![delta; mesh.vertices.len()],
            }];
        }
        let Gpu { device, queue } = &self.gpu;
        let ids = self
            .scene
            .add_asset(device, queue, asset)
            .expect("a morphing fixture model");
        let state = InstanceState {
            pose: self.render_pose(pose),
            ..InstanceState::new(ids.model)
        };
        self.scene
            .add_instance(device, queue, state, Mobility::Moving)
            .expect("a morphing fixture instance")
    }

    /// Widens the radiance range the bound oracle holds the frames to by
    /// `radiance`: what a fixture outside the placed shapes adds (a glow).
    pub fn admit(&mut self, radiance: f32) {
        self.range = [self.range[0].min(radiance), self.range[1].max(radiance)];
    }

    fn add_placed(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        shaded: bool,
    ) -> (InstanceId, Vec<MaterialId>) {
        self.add_placed_as(shape, pose, group, shaded, Mobility::Moving)
    }

    fn add_placed_as(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        shaded: bool,
        mobility: Mobility,
    ) -> (InstanceId, Vec<MaterialId>) {
        for quad in &shape.quads {
            self.range = [
                self.range[0].min(quad.radiance),
                self.range[1].max(quad.radiance),
            ];
        }
        let (id, materials) = self.instance(shape, pose, group, true, mobility);
        self.placed.push(Placed {
            id,
            shape: shape.clone(),
            pose,
            group,
            visible: true,
            shaded,
            mobility,
        });
        (id, materials)
    }

    fn instance(
        &mut self,
        shape: &Shape,
        pose: Pose,
        group: u32,
        visible: bool,
        mobility: Mobility,
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
            .add_instance(device, queue, state, mobility)
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

    /// Gives `id` a model of `shape`, at its pose and visibility, as a game
    /// swaps a model (`Scene::set_instance` naming another model).
    pub fn swap(&mut self, id: InstanceId, shape: &Shape) {
        for quad in &shape.quads {
            self.range = [
                self.range[0].min(quad.radiance),
                self.range[1].max(quad.radiance),
            ];
        }
        let group = self.placed(id).group;
        let Gpu { device, queue } = &self.gpu;
        let ids = self
            .scene
            .add_asset(device, queue, shape.asset(group))
            .expect("a fixture model");
        let state = InstanceState {
            model: ids.model,
            ..*self.scene.instance(id).unwrap()
        };
        self.scene
            .set_instance(queue, id, state)
            .expect("a fixture model swap");
        self.placed(id).shape = shape.clone();
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
            let id = self
                .instance(&p.shape, p.pose, p.group, p.visible, p.mobility)
                .0;
            self.placed.push(Placed { id, ..p });
        }
    }

    /// The instances the rig placed, in order.
    pub fn instances(&self) -> Vec<InstanceId> {
        self.placed.iter().map(|p| p.id).collect()
    }

    /// View 0's camera at `pose` (world, before any origin move).
    pub fn look(&mut self, pose: Pose) {
        self.look_view(0, pose);
    }

    /// View `index`'s camera at `pose` (world, before any origin move).
    pub fn look_view(&mut self, index: usize, pose: Pose) {
        let origin = self.origin;
        let view = &mut self.views[index];
        view.camera = pose;
        view.input.camera = view.lens.camera(Pose {
            position: pose.position - origin,
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
        for index in 0..self.views.len() {
            self.look_view(index, self.views[index].camera);
        }
    }

    fn render_pose(&self, pose: Pose) -> Mat4 {
        Mat4::from_translation(-self.origin) * pose.matrix()
    }

    /// The lowest and highest radiance the rig's fixtures had: the sky's
    /// and every shape's it held.
    pub fn radiance_range(&self) -> [f32; 2] {
        self.range
    }

    /// Sizes view 0 for its lens and settings, as `render` does first, and
    /// returns the size its passes up to antialiasing render at
    /// (`View::render_size`): FSR's render size while it upscales.
    pub fn render_size(&mut self) -> [u32; 2] {
        let Gpu { device, .. } = &self.gpu;
        let view = &mut self.views[0];
        view.view.resize(device, view.lens.size, 1., &view.settings);
        view.view.render_size()
    }

    /// Renders and submits frame `frame` of every view, in order, each at
    /// its lens's size: `elapsed_seconds` and `frame_time_ms` come from the
    /// index, each view's `resize` runs first (a no-op unless its size or
    /// settings changed), and `camera_cut` holds for this frame only.
    pub fn render(&mut self, frame: u32) {
        self.render_then(frame, |_| {});
    }

    /// [`Rig::render`] of the views `order` lists, in that order, the first
    /// the frame's primary view: a view left out is not in the frame.
    pub fn render_order(&mut self, frame: u32, order: &[usize]) {
        let encoder = self.encode(frame, order);
        self.gpu.queue.submit([encoder.finish()]);
        self.end_cuts(order);
        self.renderer.finish_frame(&mut self.scene);
    }

    /// [`Rig::render`], with `edits` made after the frame is submitted and
    /// before `Renderer::finish_frame`: the next frame's edits, in a loop
    /// that updates the scene before it finishes the frame it submitted.
    pub fn render_then(&mut self, frame: u32, edits: impl FnOnce(&mut Rig)) {
        let order = self.every_view();
        let encoder = self.encode(frame, &order);
        self.gpu.queue.submit([encoder.finish()]);
        self.end_cuts(&order);
        edits(self);
        self.renderer.finish_frame(&mut self.scene);
    }

    /// Renders frame `frame` as [`Rig::render`] does and drops its encoder
    /// unsubmitted, without `finish_frame`: an abandoned frame.
    /// `camera_cut` holds for the next.
    pub fn render_unsubmitted(&mut self, frame: u32) {
        let order = self.every_view();
        drop(self.encode(frame, &order));
    }

    /// Renders and submits frame `frame` as [`Rig::render`] does, without
    /// `finish_frame`: a frame the game rendered and did not finish.
    /// `camera_cut` holds for the next.
    pub fn render_unfinished(&mut self, frame: u32) {
        let order = self.every_view();
        let encoder = self.encode(frame, &order);
        self.gpu.queue.submit([encoder.finish()]);
    }

    fn every_view(&self) -> Vec<usize> {
        (0..self.views.len()).collect()
    }

    fn end_cuts(&mut self, order: &[usize]) {
        for &index in order {
            self.views[index].input.camera_cut = false;
        }
    }

    /// Frame `frame` of the views `order` lists, encoded in that order,
    /// each at its lens's size after its `resize`.
    fn encode(&mut self, frame: u32, order: &[usize]) -> wgpu::CommandEncoder {
        self.frame = frame;
        for &index in order {
            let view = &mut self.views[index];
            view.input.elapsed_seconds = f64::from(frame) / FRAME_RATE;
            view.input.frame_time_ms = (1000. / FRAME_RATE) as f32;
            if view.output.1 != view.lens.size {
                view.output = (target(&self.gpu.device, view.lens.size), view.lens.size);
                let camera = view.camera;
                self.look_view(index, camera);
            }
            let view = &mut self.views[index];
            view.view
                .resize(&self.gpu.device, view.lens.size, 1., &view.settings);
        }
        let Gpu { device, queue } = &self.gpu;
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut views: Vec<Option<&mut RigView>> = self.views.iter_mut().map(Some).collect();
        let mut frames: Vec<ViewFrame<'_>> = order
            .iter()
            .map(|&index| {
                let view = views[index].take().expect("each view listed once");
                ViewFrame {
                    view: &mut view.view,
                    input: &view.input,
                    settings: &view.settings,
                    output: match view.produces {
                        Some(texture) => ViewOutput::RenderTexture(texture),
                        None => ViewOutput::Texture(&view.output.0),
                    },
                    timing: None,
                }
            })
            .collect();
        self.renderer
            .render(device, queue, &mut encoder, &mut self.scene, &mut frames);
        encoder
    }

    /// `target` of view 0's last frame, decoded.
    pub fn read(&self, target: DiagnosticTarget) -> Image {
        self.read_view(0, target)
    }

    /// `target` of view `index`'s last frame, decoded.
    pub fn read_view(&self, index: usize, target: DiagnosticTarget) -> Image {
        let view = self.views[index]
            .view
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
                        format!(
                            "{c} at {pixel:?}, frame {}, of the range {low}..={high}",
                            self.frame
                        ),
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

/// A view of the rig's renderer through `lens` with `settings` and `input`,
/// whose antialiasing must run, drawing into the render texture `produces`
/// where it names one.
fn rig_view(
    gpu: &Gpu,
    renderer: &Renderer,
    lens: View,
    settings: Settings,
    input: FrameInput,
    produces: Option<RenderTextureId>,
) -> RigView {
    let view = SceneView::new(
        renderer,
        &gpu.device,
        &gpu.queue,
        match produces {
            Some(_) => RenderTexture::new(lens.size).format(),
            None => wgpu::TextureFormat::Rgba8Unorm,
        },
        lens.size,
        1.,
        &settings,
    )
    .expect("a view");
    assert_eq!(
        view.antialiasing_in_effect(&settings),
        settings.antialiasing,
        "the antialiasing under test must run: {:?}",
        view.fsr_error()
    );
    RigView {
        view,
        output: (target(&gpu.device, lens.size), lens.size),
        lens,
        input,
        settings,
        camera: Pose::at(Vec3::ZERO),
        produces,
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
