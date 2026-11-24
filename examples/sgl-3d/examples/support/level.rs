//! The level the `loading` examples load: a block world
//! (`voxel_world.rs`: terrain, water, torches, creatures, a sun with four
//! cascades, TAA and screen- and world-space reflections) about a camera,
//! with a live specular probe at the camera's start, and what a game does
//! with it: compile a frame's programs ahead (`Renderer::compile`, which
//! readies the probe's first refresh too), render one, and refresh the
//! probe.
use super::voxel_world::{self, CHUNK, Mesher, World};
use sgl_3d::glam::{DVec3, IVec3, Mat4, Vec3};
use sgl_3d::settings::{ContactShadows, MotionBlur, Settings};
use sgl_3d::{
    EnvironmentId, FrameInput, InstanceState, LiveSpecularProbe, LiveSpecularProbeId, Mobility,
    ModelMesh, PreparedModel, ProbeRefreshPace, Renderer, Scene, SpecularProbeBox, View, ViewFrame,
    ViewOutput,
};

/// The output's size, and the chunks the level holds about the camera:
/// `ACROSS` each way along x and z, `UP` along y.
pub const SIZE: [u32; 2] = [1280, 720];
const ACROSS: i32 = 3;
const UP: i32 = 1;
const SHADOW_DISTANCE: f32 = 150.;
const CREATURES: usize = 20;
/// Each face of the live probe's cube, in texels.
const PROBE_FACE_SIZE: u32 = 128;

/// The level: what a game holds once its content is loaded.
pub struct Level {
    pub scene: Scene,
    pub renderer: Renderer,
    pub view: View,
    pub output: wgpu::TextureView,
    pub world: World,
    pub origin: DVec3,
    pub sky: EnvironmentId,
    pub settings: Settings,
    /// A live probe at the camera's start, as a game refreshes one that
    /// follows a moving object.
    pub probe: LiveSpecularProbeId,
}

impl Level {
    pub fn load(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, String> {
        let error = |error: sgl_3d::SceneError| error.to_string();
        let gpu = (device, queue);
        let mut scene = Scene::new(device, queue);
        let materials = voxel_world::add_materials(&mut scene, gpu, false).map_err(error)?;
        let sky = scene
            .add_environment(device, queue, &voxel_world::sky())
            .map_err(error)?;
        let world = World::default();
        let eye = voxel_world::eye(&world, 0.5, 4.3);
        let centre = voxel_world::chunk_of(eye);
        let corner = centre * CHUNK;
        let origin = IVec3::new(corner.x, 0, corner.z).as_dvec3();
        let mesher = Mesher {
            world: &world,
            seconds: 0.,
            per_block: false,
            terrain: &materials.terrain,
            water: materials.water,
        };
        for dy in -UP..=UP {
            for dz in -ACROSS..=ACROSS {
                for dx in -ACROSS..=ACROSS {
                    let chunk = centre + IVec3::new(dx, dy, dz);
                    let prepared = mesher.prepare(chunk, [true, true]).map_err(error)?;
                    let pose =
                        Mat4::from_translation(((chunk * CHUNK).as_dvec3() - origin).as_vec3());
                    let mut terrain = false;
                    for (model, mobility) in [
                        (prepared.terrain, Mobility::Static),
                        (prepared.water, Mobility::Moving),
                    ] {
                        let Some((model, _)) = model.filter(|(_, triangles)| *triangles > 0) else {
                            continue;
                        };
                        let model = scene.add_model(device, queue, model).map_err(error)?;
                        let state = InstanceState {
                            pose,
                            ..InstanceState::new(model)
                        };
                        scene
                            .add_instance(device, queue, state, mobility)
                            .map_err(error)?;
                        terrain |= mobility == Mobility::Static;
                    }
                    if terrain {
                        for (at, shadowed) in voxel_world::torches(&world, chunk, 1) {
                            scene
                                .add_light(device, queue, voxel_world::torch(at, shadowed, origin))
                                .map_err(error)?;
                        }
                    }
                }
            }
        }
        let creature = voxel_world::creature();
        let creature = PreparedModel::new(vec![ModelMesh {
            vertices: creature.vertices,
            indices: creature.indices,
            material: materials.creature,
            deformation: Default::default(),
        }])
        .map_err(error)?;
        let creature = scene.add_model(device, queue, creature).map_err(error)?;
        for index in 0..CREATURES {
            let place = voxel_world::creature_place(&world, eye, 0., index);
            let state = InstanceState {
                pose: voxel_world::creature_pose(place, origin),
                ..InstanceState::new(creature)
            };
            scene
                .add_instance(device, queue, state, Mobility::Moving)
                .map_err(error)?;
        }
        let center = (eye - origin).as_vec3();
        let probe = scene
            .add_live_specular_probe(
                device,
                queue,
                LiveSpecularProbe {
                    center,
                    world_to_local: Mat4::from_translation(-center),
                    influence: SpecularProbeBox {
                        min: Vec3::splat(-8.),
                        max: Vec3::splat(8.),
                    },
                    blend: Vec3::splat(2.),
                    proxy: None,
                    face_size: PROBE_FACE_SIZE,
                    hidden_groups: 0,
                },
            )
            .map_err(error)?;
        let settings = voxel_world::settings();
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("level output"),
            size: wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let renderer = Renderer::new(device);
        let view = View::new(
            &renderer,
            device,
            queue,
            output.format(),
            SIZE,
            1.,
            &settings,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            scene,
            renderer,
            view,
            output: output.create_view(&Default::default()),
            world,
            origin,
            sky,
            settings,
            probe,
        })
    }

    /// The frame `metres` along the camera's walk.
    pub fn input(&self, metres: f64, cut: bool) -> FrameInput {
        let eye = voxel_world::eye(&self.world, 0.5 + metres, 4.3);
        let camera = voxel_world::camera((eye - self.origin).as_vec3());
        let mut input = voxel_world::input(camera, metres / 4.3, self.sky, SHADOW_DISTANCE);
        input.camera_cut = cut;
        input.atmosphere = true;
        input
    }

    /// What the frame `input` still lacks, and the jobs that compile it.
    /// The view is resized first, as before `render`: its settings take
    /// effect there.
    pub fn compile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &FrameInput,
    ) -> sgl_3d::Compilation {
        self.view.resize(device, SIZE, 1., &self.settings);
        self.renderer.compile(
            device,
            queue,
            &mut self.scene,
            &mut [ViewFrame {
                view: &mut self.view,
                input,
                settings: &self.settings,
                output: ViewOutput::Texture(&self.output),
                timing: None,
            }],
        )
    }

    /// Encodes and submits the frame `input`.
    pub fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, input: &FrameInput) {
        self.view.resize(device, SIZE, 1., &self.settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        self.renderer.render(
            device,
            queue,
            &mut encoder,
            &mut self.scene,
            &mut [ViewFrame {
                view: &mut self.view,
                input,
                settings: &self.settings,
                output: ViewOutput::Texture(&self.output),
                timing: None,
            }],
        );
        queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
    }

    /// Refreshes the live probe whole, between frames, with the frame
    /// `input`'s lights and the view's last frame's shadows and sky.
    pub fn refresh(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &FrameInput,
    ) -> Result<(), String> {
        self.renderer
            .refresh_specular_probe(
                device,
                queue,
                &mut self.scene,
                &mut self.view,
                input,
                &self.settings,
                None,
                self.probe,
                ProbeRefreshPace::Whole,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// The settings change the run measures: contact shadows, which give
    /// the camera's surfaces other pipelines, and motion blur.
    pub fn change_settings(&mut self) {
        self.settings.contact_shadows = ContactShadows::Short;
        self.settings.motion_blur = MotionBlur::Full;
    }
}
