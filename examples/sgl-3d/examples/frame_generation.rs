//! Native FSR3 presentation with a moving scene and an sgl-2d HUD.
//! Run: cargo run -p sgl-3d-examples --example frame_generation -- --frames 120
//! Omit --frames to run until closed. The finite run toggles generation, resizes
//! and cuts the camera; success requires actual surface presentation calls.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use sgl_2d::{
        assets::{Assets, Texture},
        canvas::{DrawList, LightingSpace, SpriteInstance, WorldUnits, sprite::SpritePass},
    };
    use sgl_3d::{
        Camera, FrameInput, InstanceId, InstanceState, Mobility, PresentationError, Presenter,
        PresenterError, Scene,
        asset::{Asset, CpuMesh, Material, Vertex},
        glam::{Mat4, Vec2, Vec3, camera},
        graphics_device,
        settings::{Antialiasing, FrameGeneration, RenderPreset, Settings},
        timing::GpuTiming,
    };
    use std::{error::Error, sync::Arc, time::Instant};
    use winit::{
        application::ApplicationHandler,
        dpi::PhysicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        window::{Window, WindowId},
    };
    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    struct Graphics {
        presenter: Presenter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        scene: Scene,
        moving: InstanceId,
        state: InstanceState,
        hud: SpritePass,
        draw: DrawList,
        timing: Option<GpuTiming>,
        settings: Settings,
        start: Instant,
        previous: Instant,
        frames: u32,
        presents: usize,
    }
    impl Graphics {
        async fn new(window: Arc<Window>) -> Result<Self> {
            let instance = wgpu::Instance::default();
            let surface = Arc::new(instance.create_surface(window.clone())?);
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: Some(&surface),
                    ..Default::default()
                })
                .await?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    required_features: graphics_device::features(&adapter)
                        | graphics_device::fsr_features(&adapter)
                        | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
                    required_limits: graphics_device::limits(&adapter),
                    ..Default::default()
                })
                .await?;
            let size = window.inner_size();
            let caps = surface.get_capabilities(&adapter);
            let config = surface
                .get_default_config(&adapter, size.width, size.height)
                .ok_or("surface has no default configuration")?;
            let settings = Settings {
                preset: RenderPreset::Low,
                antialiasing: Antialiasing::Fsr3,
                frame_generation: FrameGeneration::Fsr3,
                atmosphere: false,
                ..Default::default()
            };
            let presenter = Presenter::new(
                &device,
                &queue,
                surface,
                &config,
                &caps,
                window.scale_factor() as f32,
                &settings,
            )?;
            println!(
                "adapter {:?}; generation {:?}",
                adapter.get_info(),
                presenter.frame_generation_status()
            );
            let mut scene = Scene::new(&device, &queue);
            let model = scene
                .add_asset(
                    &device,
                    &queue,
                    Asset {
                        meshes: vec![cuboid(Vec3::ZERO, Vec3::ONE, 0)],
                        materials: vec![Material {
                            base: [0.2, 0.5, 0.8, 1.],
                            emissive: [0.2, 0.5, 0.8],
                            ..Default::default()
                        }],
                        images: vec![],
                        rig: Default::default(),
                        ignored: vec![],
                    },
                )?
                .model;
            let state = InstanceState::new(model);
            let moving = scene.add_instance(&device, &queue, state, Mobility::Moving)?;
            let mut hud = SpritePass::new(
                &device,
                &queue,
                presenter.ui_format(),
                presenter.ui_format(),
                if presenter.ui_encodes_srgb() {
                    LightingSpace::Gamma
                } else {
                    LightingSpace::Linear
                },
            );
            let mut textures = Assets::<Texture>::new();
            let white = sgl_2d::assets::white_texture(&mut textures);
            hud.upload(&device, &queue, white, textures.get(white).unwrap())?;
            let mut draw = DrawList::new();
            draw.push_screen(SpriteInstance {
                scale: Vec2::new(140., 16.),
                color: [0.9, 0.65, 0.2, 0.8],
                ..SpriteInstance::new(white, Vec2::new(90., 28.))
            });
            let timing = GpuTiming::new(&device, &queue);
            Ok(Self {
                presenter,
                device,
                queue,
                scene,
                moving,
                state,
                hud,
                draw,
                timing,
                settings,
                start: Instant::now(),
                previous: Instant::now(),
                frames: 0,
                presents: 0,
            })
        }
        fn resize(&mut self, window: &Window) -> Result<()> {
            let size = window.inner_size();
            let mut config = self.presenter.configuration().clone();
            config.width = size.width;
            config.height = size.height;
            self.presenter
                .resize(&config, window.scale_factor() as f32, &self.settings)?;
            Ok(())
        }
        fn redraw(&mut self, window: &Window, finite: bool) -> Result<()> {
            if window.inner_size().width == 0 || window.inner_size().height == 0 {
                return Ok(());
            }
            if finite {
                if self.frames == 40 {
                    self.settings.frame_generation = FrameGeneration::Off;
                }
                if self.frames == 50 {
                    self.settings.frame_generation = FrameGeneration::Fsr3;
                }
                if self.frames == 60 {
                    let _ = window.request_inner_size(PhysicalSize::new(720, 405));
                }
            }
            self.resize(window)?;
            let now = Instant::now();
            let seconds = now.duration_since(self.start).as_secs_f32();
            self.state.pose = Mat4::from_translation(Vec3::new(seconds.sin() * 0.7, 0., 0.))
                * Mat4::from_rotation_y(seconds);
            self.scene
                .set_instance(&self.queue, self.moving, self.state)?;
            let config = self.presenter.configuration();
            let size = (config.width, config.height);
            let eye = Vec3::new(2.4, 2., 3.3);
            let mut input = FrameInput::new(Camera {
                eye,
                view: camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
                projection: sgl_3d::perspective(
                    55f32.to_radians(),
                    size.0 as f32 / size.1 as f32,
                    0.1,
                ),
            });
            input.elapsed_seconds = f64::from(seconds);
            input.frame_time_ms = now.duration_since(self.previous).as_secs_f32() * 1000.;
            input.camera_cut = self.frames == 0 || (finite && self.frames == 80);
            self.previous = now;
            if let Some(timing) = &mut self.timing {
                for measured in timing.begin_frame(&self.device, &self.queue) {
                    if self.frames.is_multiple_of(30) {
                        println!(
                            "GPU frame {}: {:.3} ms, {:?}",
                            measured.frame, measured.total_ms, measured.passes
                        );
                    }
                }
            }
            self.hud.prepare(
                &self.device,
                &self.queue,
                &self.draw,
                Mat4::IDENTITY,
                WorldUnits::default(),
                sgl_2d::canvas::Camera::screen_view_proj(size.0, size.1),
                size,
                1.,
            );
            let mut frame = self.presenter.render(
                &mut self.scene,
                &input,
                &self.settings,
                self.timing.as_mut(),
            )?;
            let ui = frame.ui_target().clone();
            {
                let mut pass = frame
                    .encoder()
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("game HUD"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &ui,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                self.hud.draw_screen(&mut pass);
            }
            frame.present()?; // Also commits real-frame histories and GPU timing.
            self.frames += 1;
            self.presents += self
                .presenter
                .take_reports()
                .iter()
                .map(|r| r.present_calls.len())
                .sum::<usize>();
            Ok(())
        }
    }

    #[derive(Default)]
    struct App {
        window: Option<Arc<Window>>,
        graphics: Option<Graphics>,
        limit: Option<u32>,
        error: Option<String>,
    }
    impl App {
        fn fail(&mut self, event_loop: &ActiveEventLoop, error: Box<dyn Error>) {
            // Surface availability is temporary. Presenter resets history and
            // reconfigures lost/outdated surfaces on the next frame.
            if matches!(
                error.downcast_ref::<PresenterError>(),
                Some(PresenterError::Presentation(PresentationError::Surface(_)))
            ) {
                eprintln!("presentation deferred: {error}");
            } else {
                self.error = Some(error.to_string());
                event_loop.exit();
            }
        }
    }
    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let result = (|| -> Result<()> {
                let window = Arc::new(
                    event_loop.create_window(
                        Window::default_attributes()
                            .with_title("SGL FSR3 frame generation")
                            .with_inner_size(PhysicalSize::new(640, 360)),
                    )?,
                );
                self.graphics = Some(pollster::block_on(Graphics::new(window.clone()))?);
                self.window = Some(window);
                Ok(())
            })();
            if let Err(error) = result {
                self.fail(event_loop, error);
            }
        }
        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            let (Some(graphics), Some(window)) = (&mut self.graphics, &self.window) else {
                return;
            };
            let result = match event {
                WindowEvent::CloseRequested => {
                    event_loop.exit();
                    Ok(())
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    graphics.resize(window)
                }
                WindowEvent::RedrawRequested => graphics.redraw(window, self.limit.is_some()),
                _ => Ok(()),
            };
            if self.limit.is_some_and(|limit| graphics.frames >= limit) {
                event_loop.exit();
            }
            if let Err(error) = result {
                self.fail(event_loop, error);
            }
        }
        fn about_to_wait(&mut self, _: &ActiveEventLoop) {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
    pub fn run() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let limit = match args.next().as_deref() {
            None => None,
            Some("--frames") => Some(
                args.next()
                    .ok_or("--frames requires N")?
                    .parse::<u32>()?
                    .max(1),
            ),
            _ => return Err("usage: frame_generation [--frames N]".into()),
        };
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Poll);
        let mut app = App {
            limit,
            ..Default::default()
        };
        event_loop.run_app(&mut app)?;
        if let Some(mut graphics) = app.graphics {
            graphics.presenter.wait_for_presents()?;
            graphics.presents += graphics
                .presenter
                .take_reports()
                .iter()
                .map(|r| r.present_calls.len())
                .sum::<usize>();
            graphics.presenter.shutdown()?;
            println!(
                "{} real frames submitted, {} observed surface present calls",
                graphics.frames, graphics.presents
            );
            if limit.is_some() && graphics.presents == 0 {
                return Err("no successful surface presents (window may be occluded)".into());
            }
        }
        if let Some(error) = app.error {
            return Err(error.into());
        }
        Ok(())
    }
    fn cuboid(center: Vec3, size: Vec3, material: usize) -> CpuMesh {
        let half = size * 0.5;
        let mut mesh = CpuMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            material,
            deformation: Default::default(),
        };
        for (normal, u, v) in [
            (Vec3::X, -Vec3::Z, Vec3::Y),
            (-Vec3::X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::X, -Vec3::Z),
            (-Vec3::Y, Vec3::X, Vec3::Z),
            (Vec3::Z, Vec3::X, Vec3::Y),
            (-Vec3::Z, -Vec3::X, Vec3::Y),
        ] {
            let start = mesh.vertices.len() as u32;
            for [s, t] in [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]] {
                mesh.vertices.push(Vertex {
                    tangent: [u.x, u.y, u.z, 1.],
                    lightmap_bounds: [0., 0., 1., 1.],
                    lightmap_uv: [0.; 2],
                    position: (center + (normal + u * s + v * t) * half).to_array(),
                    normal: normal.to_array(),
                    uv: [(s + 1.) * 0.5, (t + 1.) * 0.5],
                    color: [1.; 4],
                });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
        }
        mesh
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}
#[cfg(target_arch = "wasm32")]
fn main() {}
