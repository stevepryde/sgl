//! SGL's side of the startup comparison against three.js
//! (../../BROWSER_BASELINE.md, "Startup against three.js"). The shared page
//! harness (`browser/startup-harness.js`) calls these steps in the same order
//! as three.js's, and times them; this renders to the page's canvas through
//! `View::new_surface`, as a browser game does.

use serde_json::{Value, json};
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceId,
    InstanceState, Light, LightShape, Mobility, ModelId, ModelMesh, PreparedModel,
    PreparedModelOptions, RayData, Renderer, Scene, SceneOptions, View, ViewFrame,
    asset::{self, Asset},
    glam::{Mat4, Vec3, camera},
    settings::{
        AmbientOcclusionQuality, AnisotropicFiltering, Antialiasing, Bloom, DynamicGiQuality,
        Settings, Sharpening, SoftShadows,
    },
};
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

fn error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

fn vec3(value: &Value) -> Result<Vec3, JsError> {
    let values = value
        .as_array()
        .filter(|v| v.len() == 3)
        .ok_or_else(|| error("expected three numbers"))?;
    let at = |i: usize| {
        values[i]
            .as_f64()
            .map(|v| v as f32)
            .ok_or_else(|| error("not a number"))
    };
    Ok(Vec3::new(at(0)?, at(1)?, at(2)?))
}

fn matrix(value: &Value) -> Result<Mat4, JsError> {
    let values: Vec<f32> = value
        .as_array()
        .ok_or_else(|| error("expected a matrix"))?
        .iter()
        .map(|v| {
            v.as_f64()
                .map(|v| v as f32)
                .ok_or_else(|| error("not a number"))
        })
        .collect::<Result<_, _>>()?;
    let values: [f32; 16] = values
        .try_into()
        .map_err(|_| error("expected 16 numbers"))?;
    Ok(Mat4::from_cols_array(&values))
}

/// The page's canvas, its device and SGL's scene, renderer and view.
#[wasm_bindgen]
pub struct Startup {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    capabilities: wgpu::SurfaceCapabilities,
    info: wgpu::AdapterInfo,
    errors: Arc<Mutex<Vec<String>>>,
    asset: Option<Asset>,
    layout: Value,
    scene: Option<Box<Scene>>,
    renderer: Option<Box<Renderer>>,
    view: Option<Box<View>>,
    models: Vec<ModelId>,
    moving: Vec<(InstanceId, ModelId, Mat4)>,
    mask: Option<InstanceId>,
    settings: Settings,
    frames: u64,
    /// CPU milliseconds of each part of `upload`.
    upload_ms: Value,
}

#[wasm_bindgen]
impl Startup {
    /// Adapter, device and the canvas configured at its size.
    pub async fn create(canvas: web_sys::HtmlCanvasElement) -> Result<Startup, JsError> {
        console_error_panic_hook::set_once();
        let size = [canvas.width(), canvas.height()];
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(error)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(error)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("startup"),
                required_features: sgl_3d::graphics_device::features(&adapter),
                required_limits: sgl_3d::graphics_device::limits(&adapter),
                ..Default::default()
            })
            .await
            .map_err(error)?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        device.on_uncaptured_error(Arc::new({
            let errors = errors.clone();
            move |e| errors.lock().unwrap().push(e.to_string())
        }));
        let capabilities = surface.get_capabilities(&adapter);
        let config = surface
            .get_default_config(&adapter, size[0], size[1])
            .ok_or_else(|| error("the canvas is not compatible with the adapter"))?;
        surface.configure(&device, &config);
        Ok(Startup {
            device,
            queue,
            surface,
            config,
            capabilities,
            info: adapter.get_info(),
            errors,
            asset: None,
            layout: Value::Null,
            scene: None,
            renderer: None,
            view: None,
            models: Vec::new(),
            moving: Vec::new(),
            mask: None,
            settings: Settings::default(),
            frames: 0,
            upload_ms: Value::Null,
        })
    }

    /// The `GPUDevice` the canvas renders with, for its adapter's identity.
    #[wasm_bindgen(js_name = gpuDevice)]
    pub fn gpu_device(&self) -> JsValue {
        self.device
            .as_webgpu()
            .map_or(JsValue::NULL, |device| device.clone().into())
    }

    /// The GLB, its PNGs decoded as `asset::load_slice` does.
    pub fn decode(&mut self, glb: &[u8]) -> Result<(), JsError> {
        self.asset = Some(asset::load_slice(glb).map_err(error)?);
        Ok(())
    }

    /// The scene: materials once, each prototype mesh as a model, then the
    /// layout's instances and lights. The page awaits `completion()` after.
    pub fn upload(&mut self, layout: &str) -> Result<(), JsError> {
        self.layout = serde_json::from_str(layout).map_err(error)?;
        let asset = self.asset.take().ok_or_else(|| error("decode first"))?;
        let (device, queue) = (&self.device, &self.queue);
        let mut at = crate::now();
        let mut lap = || {
            let start = std::mem::replace(&mut at, crate::now());
            at - start
        };
        // No world-space ray effect is used, so the scene keeps no ray data,
        // as the package guide recommends.
        let mut scene = Box::new(Scene::with_options(
            device,
            queue,
            SceneOptions {
                ray_data: RayData::Disabled,
            },
        ));
        let scene_ms = lap();
        let materials = scene
            .add_materials(device, queue, &asset.materials, &asset.images)
            .map_err(error)?;
        let materials_ms = lap();
        // The GLB's mesh i uses material i: prototypes, then the mask.
        let mut models = vec![None; asset.materials.len()];
        for mesh in asset.meshes {
            let index = mesh.material;
            let model = PreparedModel::with_options(
                vec![ModelMesh {
                    vertices: mesh.vertices,
                    indices: mesh.indices,
                    material: materials[index],
                    deformation: mesh.deformation,
                }],
                PreparedModelOptions {
                    ray_data: RayData::Disabled,
                },
            )
            .map_err(error)?;
            models[index] = Some(scene.add_model(device, queue, model).map_err(error)?);
        }
        // The loader appends a default material no mesh of this GLB uses.
        let prototypes = self.layout["mask_prototype"].as_u64().unwrap_or(0) as usize + 1;
        self.models = models
            .into_iter()
            .take(prototypes)
            .collect::<Option<_>>()
            .ok_or_else(|| error("a prototype has no mesh"))?;
        let models_ms = lap();
        for instance in self.layout["instances"].as_array().into_iter().flatten() {
            let model = self.models[instance["prototype"].as_u64().unwrap_or(0) as usize];
            let pose = matrix(&instance["matrix"])?;
            let moving = instance["moving"].as_bool() == Some(true);
            let mobility = if moving {
                Mobility::Moving
            } else {
                Mobility::Static
            };
            let id = scene
                .add_instance(
                    device,
                    queue,
                    InstanceState {
                        pose,
                        ..InstanceState::new(model)
                    },
                    mobility,
                )
                .map_err(error)?;
            if moving {
                self.moving.push((id, model, pose));
            }
        }
        let instances_ms = lap();
        for light in self.layout["lights"].as_array().into_iter().flatten() {
            let color = vec3(&light["color"])?;
            scene
                .add_light(
                    device,
                    queue,
                    Light {
                        position: vec3(&light["position"])?,
                        shape: LightShape::Point { radius: 0.1 },
                        color: color.to_array(),
                        intensity: light["intensity"].as_f64().unwrap_or(0.) as f32,
                        range: light["range"].as_f64().unwrap_or(0.) as f32,
                        ..Default::default()
                    },
                )
                .map_err(error)?;
        }
        queue.submit([]);
        self.upload_ms = json!({"scene": scene_ms, "materials": materials_ms, "models": models_ms,
            "instances": instances_ms, "lights_and_submit": lap()});
        self.scene = Some(scene);
        Ok(())
    }

    /// Resolves when the queue's submitted work has completed.
    pub fn completion(&self) -> js_sys::Promise {
        let queue = self.queue.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let (send, receive) = futures::channel::oneshot::channel();
            queue.on_submitted_work_done(move || {
                let _ = send.send(());
            });
            receive
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })
    }

    /// The renderer and the canvas's view at the scene's profile.
    pub fn view(&mut self) -> Result<(), JsError> {
        self.settings = match self.layout["profile"].as_str() {
            // Everything that changes the image beyond lighting off.
            Some("minimal") => Settings {
                antialiasing: Antialiasing::Off,
                bloom: Bloom::Off,
                sharpening: Sharpening::Off,
                soft_shadows: SoftShadows::Off,
                anisotropic_filtering: AnisotropicFiltering::Off,
                dynamic_gi: DynamicGiQuality::Off,
                ..Default::default()
            },
            // High's defaults: TAA, bloom, sharpening, soft shadows, 8x aniso.
            Some("opening") => Settings {
                dynamic_gi: DynamicGiQuality::Off,
                ..Default::default()
            },
            profile => return Err(error(format!("unknown profile {profile:?}"))),
        };
        let renderer = Box::new(Renderer::new(&self.device));
        self.view = Some(Box::new(
            View::new_surface(
                &renderer,
                &self.device,
                &self.queue,
                &self.config,
                &self.capabilities,
                1.,
                &self.settings,
            )
            .map_err(error)?,
        ));
        self.renderer = Some(renderer);
        Ok(())
    }

    /// One frame: animate, encode, submit and present.
    pub fn frame(
        &mut self,
        seconds: f64,
        yaw_degrees: f32,
        mask: bool,
        ambient_occlusion: bool,
    ) -> Result<(), JsError> {
        let (device, queue) = (&self.device, &self.queue);
        let scene = self.scene.as_mut().ok_or_else(|| error("upload first"))?;
        let renderer = self.renderer.as_mut().ok_or_else(|| error("view first"))?;
        let view = self.view.as_mut().ok_or_else(|| error("view first"))?;
        for &(instance, model, pose) in &self.moving {
            scene
                .set_instance(
                    queue,
                    instance,
                    InstanceState {
                        pose: pose * Mat4::from_rotation_y(seconds as f32),
                        ..InstanceState::new(model)
                    },
                )
                .map_err(error)?;
        }
        match (mask, self.mask) {
            (true, None) => {
                let model =
                    self.models[self.layout["mask_prototype"].as_u64().unwrap_or(0) as usize];
                let state = InstanceState {
                    pose: matrix(&self.layout["mask_matrix"])?,
                    ..InstanceState::new(model)
                };
                self.mask = Some(
                    scene
                        .add_instance(device, queue, state, Mobility::Static)
                        .map_err(error)?,
                );
            }
            (false, Some(id)) => {
                scene.remove_instance(id).map_err(error)?;
                self.mask = None;
            }
            _ => {}
        }
        self.settings.ambient_occlusion = if ambient_occlusion {
            AmbientOcclusionQuality::High
        } else {
            AmbientOcclusionQuality::Off
        };
        let camera_layout = &self.layout["camera"];
        let target = vec3(&camera_layout["target"])?;
        let eye = target
            + Mat4::from_rotation_y(yaw_degrees.to_radians())
                .transform_vector3(vec3(&camera_layout["eye"])? - target);
        let fov = camera_layout["fov_y_degrees"].as_f64().unwrap_or(65.) as f32;
        let near = camera_layout["near"].as_f64().unwrap_or(0.1) as f32;
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_to_mat4(eye, (target - eye).normalize(), Vec3::Y),
            projection: sgl_3d::perspective(
                fov.to_radians(),
                self.config.width as f32 / self.config.height as f32,
                near,
            ),
        });
        input.elapsed_seconds = seconds;
        input.frame_time_ms = 1000. / 60.;
        input.camera_cut = self.frames == 0;
        let sun = &self.layout["sun"];
        input.directional_lights[0] = Some(DirectionalLight {
            direction: vec3(&sun["direction"])?,
            color: vec3(&sun["color"])?.to_array(),
            illuminance: sun["illuminance"].as_f64().unwrap_or(0.) as f32,
            shadow: sun["shadow"].as_object().map(|shadow| DirectionalShadow {
                distance: shadow["distance"].as_f64().unwrap_or(65.) as f32,
                cascades: shadow["cascades"].as_u64().unwrap_or(4) as u32,
            }),
            ..Default::default()
        });
        let hemisphere = &self.layout["hemisphere"];
        input.hemisphere_light = HemisphereLight {
            sky_color: vec3(&hemisphere["sky"])?.to_array(),
            ground_color: vec3(&hemisphere["ground"])?.to_array(),
            intensity: hemisphere["intensity"].as_f64().unwrap_or(0.) as f32,
        };
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            other => return Err(error(format!("no canvas texture: {other:?}"))),
        };
        let output = texture.texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &mut [ViewFrame {
                view,
                input: &input,
                settings: &self.settings,
                output: sgl_3d::ViewOutput::Texture(&output),
                timing: None,
            }],
        );
        queue.submit([encoder.finish()]);
        queue.present(texture);
        renderer.finish_frame(scene);
        self.frames += 1;
        Ok(())
    }

    /// What ran: the adapter, canvas, settings and any WebGPU errors.
    pub fn report(&self) -> String {
        json!({
            "adapter": {"name": self.info.name, "backend": format!("{:?}", self.info.backend),
                "device_type": format!("{:?}", self.info.device_type)},
            "canvas": {"format": format!("{:?}", self.config.format), "alpha_mode": format!("{:?}", self.config.alpha_mode),
                "present_mode": format!("{:?}", self.config.present_mode), "size": [self.config.width, self.config.height]},
            "antialiasing_in_effect": self.view.as_ref().map(|view| format!("{:?}", view.antialiasing_in_effect(&self.settings))),
            "settings": format!("{:?}", self.settings),
            "upload_cpu_ms": self.upload_ms,
            "errors": *self.errors.lock().unwrap(),
        })
        .to_string()
    }
}
