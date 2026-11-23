//! Reproducible, bounded WebGPU workload, and SGL's side of the startup
//! comparison against three.js. See ../BROWSER_BASELINE.md.
#![cfg(target_arch = "wasm32")]

#[path = "support/startup.rs"]
mod startup;

use serde_json::{Value, json};
use sgl_3d::{
    AlphaMode, Camera, DepthOfFieldParameters, DirectionalLight, DirectionalShadow, FrameInput,
    HemisphereLight, InstanceState, Light, LightShape, Mobility, Renderer, Scene, View, ViewFrame,
    ViewOutput,
    asset::{Asset, CpuMesh, Image, Material, MaterialTexture, Rgba8MipChain, Vertex},
    glam::{Mat4, Vec3, camera},
    settings::{
        AmbientOcclusionQuality, Antialiasing, DepthOfField, DynamicGiQuality, SceneResolution,
        ScreenSpaceReflections, Settings, Upscaling,
    },
    timing::{FrameTime, GpuTiming},
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wasm_bindgen::prelude::*;

/// The output size unless the page asks for another.
const SIZE: [u32; 2] = [1280, 720];
/// The `hdr_effect` example's underwater effect: absorption along the view
/// ray, the water's colour scattered in.
const UNDERWATER: &str =
    "struct EffectParams { absorption:vec3<f32>, unused:f32, scatter:vec3<f32>, unused_too:f32, }
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 let kept=exp(-params.absorption*ctx.distance);
 return color*kept+params.scatter*(1.-kept);
}";
const WARMUP: usize = 60;
const MATERIALS: usize = 16;
const TEXTURE: u32 = 256;
thread_local! { static CAPTURE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) }; }

#[wasm_bindgen]
pub fn take_capture() -> Vec<u8> {
    CAPTURE.with(|capture| std::mem::take(&mut *capture.borrow_mut()))
}

async fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    output: &wgpu::Texture,
) -> Result<(), String> {
    let size = [output.width(), output.height()];
    let row = (size[0] * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("baseline final capture"),
        size: u64::from(row * size[1]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        output.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size[1]),
            },
        },
        output.size(),
    );
    queue.submit([encoder.finish()]);
    let (send, receive) = futures::channel::oneshot::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    receive
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .map_err(|e| e.to_string())?;
    let pixels = mapped
        .chunks_exact(row as usize)
        .flat_map(|row| row[..size[0] as usize * 4].iter().copied())
        .collect();
    drop(mapped);
    buffer.unmap();
    CAPTURE.with(|capture| *capture.borrow_mut() = pixels);
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
        // Eight subdivisions per edge give 768 triangles per object, enough
        // vertex/cluster work to complement the 720p material/post-effect load.
        for y in 0..8 {
            for x in 0..8 {
                let start = mesh.vertices.len() as u32;
                for [dx, dy] in [[0., 0.], [1., 0.], [1., 1.], [0., 1.]] {
                    let s = (x as f32 + dx) / 8. * 2. - 1.;
                    let t = (y as f32 + dy) / 8. * 2. - 1.;
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
        }
    }
    mesh
}

fn asset(number: usize) -> Asset {
    let texture = image::RgbaImage::from_fn(TEXTURE, TEXTURE, |x, y| {
        let checker = ((x / 16 + y / 16) % 2) as u8;
        image::Rgba([
            80 + number as u8 * 7,
            90 + checker * 70,
            150 - number as u8 * 4,
            255,
        ])
    });
    Asset {
        meshes: vec![cuboid(Vec3::new(0., 0.5, 0.), Vec3::ONE, 0)],
        materials: vec![Material {
            name: format!("baseline {number}"),
            base_texture: Some(MaterialTexture::new(0)),
            base: if number % MATERIALS >= 14 {
                [1., 1., 1., 0.45]
            } else {
                [1.; 4]
            },
            alpha: if number % MATERIALS >= 14 {
                AlphaMode::Blend {
                    receives_screen_space_reflections: true,
                    keeps_specular: false,
                }
            } else {
                AlphaMode::Opaque
            },
            roughness: 0.15 + (number % MATERIALS) as f32 * 0.045,
            metallic: if number.is_multiple_of(3) { 0.8 } else { 0. },
            ..Default::default()
        }],
        images: vec![Image::Rgba8(texture)],
        rig: Default::default(),
        ignored: vec![],
        lights: Default::default(),
        instanced: Vec::new(),
    }
}

fn now() -> f64 {
    let performance = js_sys::Reflect::get(&js_sys::global(), &"performance".into()).unwrap();
    let function: js_sys::Function = js_sys::Reflect::get(&performance, &"now".into())
        .unwrap()
        .unchecked_into();
    function.call0(&performance).unwrap().as_f64().unwrap()
}
async fn completed(queue: &wgpu::Queue) -> Result<(), String> {
    let (send, receive) = futures::channel::oneshot::channel();
    queue.on_submitted_work_done(move || {
        let _ = send.send(());
    });
    receive.await.map_err(|e| e.to_string())
}
fn summary(values: &[f64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    json!({"samples": values.len(), "median_ms": values[(values.len()-1)/2],
        "p95_ms": values[(values.len() * 95).div_ceil(100)-1],
        "max_ms": values[values.len()-1]})
}
fn collect(
    timing: &mut Option<GpuTiming>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frames: usize,
    reports: &mut Vec<FrameTime>,
) {
    if let Some(timing) = timing {
        reports.extend(timing.begin_frame(device, queue).filter(|frame| {
            frame.frame > WARMUP as u64 && frame.frame <= (WARMUP + frames) as u64
        }));
    }
}

/// `depth_of_field` is `off`, `low`, `medium` or `high`; `size` is the
/// output's width and height, or `SIZE` unless both are given and above 0;
/// `hdr_effect` applies an underwater HDR effect (`UNDERWATER`); `outline`
/// is `off`, `sparse` (one block outlined) or `dense` (every block of the
/// grid outlined).
#[wasm_bindgen]
#[allow(
    clippy::too_many_arguments,
    reason = "the page passes each of its query's options positionally"
)]
pub async fn measure(
    frames: u32,
    scaled: bool,
    prepared_upload: bool,
    capture_frame: bool,
    local_shadows: bool,
    depth_of_field: String,
    size: Vec<u32>,
    hdr_effect: bool,
    outline: String,
) -> String {
    console_error_panic_hook::set_once();
    let depth_of_field = match depth_of_field.as_str() {
        "off" => DepthOfField::Off,
        "low" => DepthOfField::Low,
        "medium" => DepthOfField::Medium,
        "high" => DepthOfField::High,
        other => return json!({"error": format!("unknown depth of field {other}")}).to_string(),
    };
    let outline = match outline.as_str() {
        "off" => Selected::Off,
        "sparse" => Selected::Sparse,
        "dense" => Selected::Dense,
        other => return json!({"error": format!("unknown outline {other}")}).to_string(),
    };
    let size = match size[..] {
        [width, height] if width > 0 && height > 0 => [width, height],
        _ => SIZE,
    };
    match measured(
        frames as usize,
        scaled,
        prepared_upload,
        capture_frame,
        (local_shadows, depth_of_field, size, hdr_effect, outline),
    )
    .await
    {
        Ok(report) => report.to_string(),
        Err(error) => json!({"error": error}).to_string(),
    }
}
/// The selection the measured frames outline.
#[derive(Clone, Copy, Debug)]
enum Selected {
    Off,
    Sparse,
    Dense,
}

async fn measured(
    frames: usize,
    scaled: bool,
    prepared_upload: bool,
    capture_frame: bool,
    (local_shadows, depth_of_field, size, hdr_effect, outline): (
        bool,
        DepthOfField,
        [u32; 2],
        bool,
        Selected,
    ),
) -> Result<Value, String> {
    if !(60..=3600).contains(&frames) {
        return Err("frames must be in 60..=3600".into());
    }
    let started = now();
    let adapter = sgl_webgpu::instance(&wgpu::InstanceDescriptor::new_without_display_handle())
        .request_adapter(&Default::default())
        .await
        .map_err(|e| e.to_string())?;
    let info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("browser baseline"),
            required_features: sgl_3d::graphics_device::features(&adapter)
                | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    let errors = Arc::new(Mutex::new(Vec::new()));
    device.on_uncaptured_error(Arc::new({
        let errors = errors.clone();
        move |error| errors.lock().unwrap().push(error.to_string())
    }));
    let device_ms = now() - started;
    let mut scene = Box::new(Scene::new(&device, &queue));
    let underwater = if hdr_effect {
        let effect = scene
            .add_hdr_effect(sgl_3d::ShaderSource {
                wgsl: UNDERWATER.into(),
                label: "underwater".into(),
            })
            .map_err(|e| e.to_string())?;
        let block: [f32; 8] = [0.12, 0.045, 0.03, 0., 0.02, 0.09, 0.12, 0.];
        let bytes: Vec<u8> = block.iter().flat_map(|v| v.to_le_bytes()).collect();
        Some(
            scene
                .hdr_effect(effect, &bytes)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    // Caller-owned preparation is deliberately outside every measured upload
    // and frame. This example runs it on this thread, not a hidden worker.
    let prepare_start = now();
    let mut prepared_assets = Vec::new();
    if prepared_upload {
        for number in 0..MATERIALS + 8 {
            let mut asset = asset(number);
            asset.images = asset
                .images
                .into_iter()
                .map(|image| match image {
                    Image::Rgba8(image) => Rgba8MipChain::new(image)
                        .map(Image::Rgba8Mips)
                        .map_err(|e| e.to_string()),
                    image => Ok(image),
                })
                .collect::<Result<_, _>>()?;
            prepared_assets.push(Some(asset));
        }
    }
    let prepare_cpu_ms = if prepared_upload {
        Some(now() - prepare_start)
    } else {
        None
    };
    let upload_start = now();
    let mut models = Vec::new();
    for number in 0..MATERIALS {
        let asset = if prepared_upload {
            prepared_assets
                .get_mut(number)
                .and_then(Option::take)
                .ok_or("prepared asset missing")?
        } else {
            asset(number)
        };
        models.push(
            scene
                .add_asset(&device, &queue, asset)
                .map_err(|e| e.to_string())?
                .model,
        );
    }
    let mut moving = Vec::new();
    let mut grid = Vec::new();
    for number in 0..256 {
        let model = models[number % MATERIALS];
        let position = Vec3::new(
            (number % 16) as f32 * 1.8 - 13.5,
            0.,
            (number / 16) as f32 * 1.8 - 13.5,
        );
        let pose = Mat4::from_translation(position)
            * Mat4::from_scale(Vec3::new(1., 0.6 + (number % 7) as f32 * 0.35, 1.));
        let state = InstanceState {
            pose,
            ..InstanceState::new(model)
        };
        let mobility = if number % 8 == 0 {
            Mobility::Moving
        } else {
            Mobility::Static
        };
        let instance = scene
            .add_instance(&device, &queue, state, mobility)
            .map_err(|e| e.to_string())?;
        grid.push(instance);
        if mobility == Mobility::Moving {
            moving.push((instance, model, pose));
        }
    }
    // The outline's selection: one static block near the grid's middle, or
    // every block.
    let selected = match outline {
        Selected::Off => None,
        Selected::Sparse => Some(&grid[8 * 16 + 9..8 * 16 + 10]),
        Selected::Dense => Some(&grid[..]),
    };
    let outlined = match selected {
        Some(instances) => Some(sgl_3d::Outline::new(
            scene.add_selection(instances).map_err(|e| e.to_string())?,
        )),
        None => None,
    };
    let ground = InstanceState {
        pose: Mat4::from_scale(Vec3::new(34., 0.1, 34.))
            * Mat4::from_translation(Vec3::new(0., -1., 0.)),
        ..InstanceState::new(models[0])
    };
    scene
        .add_instance(&device, &queue, ground, Mobility::Static)
        .map_err(|e| e.to_string())?;
    let mut lights = Vec::new();
    for number in 0..8 {
        let light = Light {
            position: Vec3::new(
                (number % 4) as f32 * 7. - 10.5,
                4.,
                (number / 4) as f32 * 14. - 7.,
            ),
            shape: LightShape::Point { radius: 0.1 },
            color: [1., 0.7, 0.4],
            intensity: 40.,
            range: 12.,
            ..Default::default()
        };
        let id = scene
            .add_light(&device, &queue, light)
            .map_err(|e| e.to_string())?;
        lights.push((id, light));
    }
    let upload_cpu_ms = now() - upload_start;
    // Submit even when the last upload has no render commands; includes real GPU texture/geometry uploads.
    queue.submit([]);
    completed(&queue).await?;
    let upload_wall_ms = now() - upload_start;
    let settings = Settings {
        scene_resolution: if scaled {
            SceneResolution::ThreeQuarter
        } else {
            SceneResolution::Full
        },
        upscaling: if scaled {
            Upscaling::On
        } else {
            Upscaling::Off
        },
        ambient_occlusion: AmbientOcclusionQuality::High,
        screen_space_reflections: ScreenSpaceReflections::Half,
        atmosphere: false,
        antialiasing: Antialiasing::Taa,
        dynamic_gi: DynamicGiQuality::Off,
        depth_of_field,
        ..Default::default()
    };
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("baseline offscreen output"),
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
    });
    let view = output.create_view(&Default::default());
    let renderer_start = now();
    let mut renderer = Box::new(Renderer::new(&device));
    let mut scene_view = Box::new(
        View::new(
            &renderer,
            &device,
            &queue,
            output.format(),
            size,
            1.,
            &settings,
        )
        .map_err(|e| e.to_string())?,
    );
    let renderer_cpu_ms = now() - renderer_start;
    let mut timing = GpuTiming::new(&device, &queue);
    let mut gpu_frames = Vec::new();
    let mut cpu = Vec::new();
    let mut wall = Vec::new();
    let mut draws = Vec::new();
    let mut blended_draws = Vec::new();
    let mut first_frame_ms = 0.;
    // With local shadows, the frame whose lights start to cast them: the
    // first measured one.
    let mut activation = None;
    let mut streamed_upload_cpu = Vec::new();
    let mut streamed_generate_cpu = Vec::new();
    let mut streamed_frame_wall = Vec::new();
    for index in 0..WARMUP + frames + 8 {
        let frame_start = now();
        let seconds = index as f32 / 60.;
        if local_shadows && index == WARMUP {
            for &(id, light) in &lights {
                scene
                    .set_light(
                        &queue,
                        id,
                        Light {
                            casts_shadow: true,
                            ..light
                        },
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        if index >= WARMUP + frames {
            let number = MATERIALS + index - WARMUP - frames;
            let generated = if prepared_upload {
                prepared_assets
                    .get_mut(number)
                    .and_then(Option::take)
                    .ok_or("prepared asset missing")?
            } else {
                asset(number)
            };
            streamed_generate_cpu.push(now() - frame_start);
            let upload = now();
            let model = scene
                .add_asset(&device, &queue, generated)
                .map_err(|e| e.to_string())?
                .model;
            scene
                .add_instance(
                    &device,
                    &queue,
                    InstanceState {
                        pose: Mat4::from_translation(Vec3::new(
                            0.,
                            3.,
                            (index - WARMUP - frames) as f32 - 4.,
                        )),
                        ..InstanceState::new(model)
                    },
                    Mobility::Static,
                )
                .map_err(|e| e.to_string())?;
            streamed_upload_cpu.push(now() - upload);
        }
        for &(instance, model, pose) in &moving {
            scene
                .set_instance(
                    &queue,
                    instance,
                    InstanceState {
                        pose: pose * Mat4::from_rotation_y(seconds),
                        ..InstanceState::new(model)
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        let eye = Vec3::new(
            (seconds * 0.25).cos() * 24.,
            13.,
            (seconds * 0.25).sin() * 24.,
        );
        let mut frame = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_to_mat4(
                eye,
                (Vec3::new(0., 1., 0.) - eye).normalize(),
                Vec3::Y,
            ),
            projection: sgl_3d::perspective(
                65f32.to_radians(),
                size[0] as f32 / size[1] as f32,
                0.1,
            ),
        });
        frame.hdr_effect = underwater;
        frame.outline = outlined;
        // The camera module takes a unit direction. Independently enforce
        // a rigid view basis so an accidentally scaled look vector fails
        // the fixture instead of producing misleading timing evidence.
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            let length = frame.camera.view.transform_vector3(axis).length();
            if !length.is_finite() || (length - 1.).abs() > 0.0001 {
                return Err("baseline camera view must preserve unit vector length".into());
            }
        }
        frame.elapsed_seconds = f64::from(seconds);
        frame.camera_cut = index == 0;
        frame.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.4, -1., -0.3),
            color: [1.; 3],
            illuminance: 3.,
            shadow: Some(DirectionalShadow {
                distance: 65.,
                cascades: 4,
            }),
            ..Default::default()
        });
        // While depth of field runs: a large-format lens (10 cm sensor) wide
        // open, focused 3 m ahead, so nearly every tile blurs, the distant
        // grid by about 11 pixels at 1080p.
        frame.depth_of_field = DepthOfFieldParameters {
            focus_distance: 3.,
            f_number: 1.,
            sensor_height: 0.1,
        };
        frame.hemisphere_light = HemisphereLight {
            sky_color: [0.3, 0.4, 0.6],
            ground_color: [0.1; 3],
            intensity: 0.5,
        };
        collect(&mut timing, &device, &queue, frames, &mut gpu_frames);
        let mut encoder = device.create_command_encoder(&Default::default());
        let encode_start = now();
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &mut [ViewFrame {
                view: &mut scene_view,
                input: &frame,
                settings: &settings,
                output: ViewOutput::Texture(&view),
                timing: timing.as_ref(),
            }],
        );
        let commands = encoder.finish();
        let encode_ms = now() - encode_start;
        queue.submit([commands]);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
        }
        completed(&queue).await?;
        renderer.finish_frame(&mut scene);
        if index == 0 {
            first_frame_ms = now() - started;
        }
        if local_shadows && index == WARMUP {
            activation = Some((encode_ms, now() - frame_start));
        }
        if (WARMUP..WARMUP + frames).contains(&index) {
            cpu.push(encode_ms);
            wall.push(now() - frame_start);
            draws.push(scene_view.diagnostic_draws().camera as f64);
            blended_draws.push(scene_view.diagnostic_draws().blended as f64);
        } else if index >= WARMUP + frames {
            streamed_frame_wall.push(now() - frame_start);
        }
    }
    // Drain delayed timestamp maps without adding more measured rendering.
    for _ in 0..12 {
        collect(&mut timing, &device, &queue, frames, &mut gpu_frames);
        completed(&queue).await?;
    }
    let mut passes: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for frame in &gpu_frames {
        for pass in &frame.passes {
            passes.entry(pass.name).or_default().push(pass.ms);
        }
    }
    let passes: BTreeMap<_, _> = passes
        .into_iter()
        .map(|(name, values)| (name, summary(&values)))
        .collect();
    if capture_frame {
        capture(&device, &queue, &output).await?;
    }
    let resources = scene.diagnostic_resources();
    // `GpuTiming` counts frames from 1.
    let activation = activation.map(|(encode_ms, wall_ms)| {
        json!({"frame":WARMUP,"cpu_encode_ms":encode_ms,"frame_complete_wall_ms":wall_ms,
            "gpu_total_ms":gpu_frames.iter().find(|f| f.frame == WARMUP as u64 + 1).map(|f| f.total_ms)})
    });
    Ok(
        json!({"schema":1,"scene":"textured-grid-v1","scaled":scaled,"prepared_upload":prepared_upload,"depth_of_field":format!("{depth_of_field:?}"),"hdr_effect":hdr_effect,"outline":format!("{outline:?}"),"size":size,"frames":frames,"warmup_frames":WARMUP,
        "adapter":{"name":info.name,"backend":format!("{:?}",info.backend),"device_type":format!("{:?}",info.device_type),
            "features":format!("{:?}",device.features()),"sampled_textures_per_stage":device.limits().max_sampled_textures_per_shader_stage},
        "settings":format!("{:?}",settings),
        "content":{"instances":257,"opaque_instances":225,"blended_instances":32,"moving_instances":moving.len(),"materials":MATERIALS,"textures":MATERIALS,
            "texture_size":TEXTURE,"source_texture_bytes":MATERIALS*TEXTURE as usize*TEXTURE as usize*4,"triangles":257*768,"local_lights":8,"shadow_cascades":4},
        "local_shadows":{"cast_from_frame":local_shadows.then_some(WARMUP),"activation":activation},
        "startup":{"prepare_cpu_ms":prepare_cpu_ms,"device_ms":device_ms,"content_cpu_ms":upload_cpu_ms,"content_complete_ms":upload_wall_ms,
            "renderer_cpu_ms":renderer_cpu_ms,"first_frame_complete_ms":first_frame_ms},
        "cpu_encode":summary(&cpu),"frame_complete_wall":summary(&wall),
        "streamed_upload":{"batches":8,"texture_source_bytes_per_batch":TEXTURE*TEXTURE*4,
            "instances_per_batch":1,"generate_cpu":summary(&streamed_generate_cpu),
            "add_asset_instance_cpu":summary(&streamed_upload_cpu),"frame_complete_wall":summary(&streamed_frame_wall)},
        "camera_draws":{"min":draws.iter().copied().fold(f64::INFINITY,f64::min),"max":draws.iter().copied().fold(0.,f64::max)},
        "blended_draws":{"min":blended_draws.iter().copied().fold(f64::INFINITY,f64::min),"max":blended_draws.iter().copied().fold(0.,f64::max)},
        "gpu":{"supported":timing.is_some(),"measured_frames":gpu_frames.len(),"missing_frames":frames-gpu_frames.len(),
            "total":summary(&gpu_frames.iter().map(|f|f.total_ms).collect::<Vec<_>>()),"passes":passes,
            "missing_reason":if timing.is_none(){Some("TIMESTAMP_QUERY unavailable")}else if gpu_frames.is_empty(){Some("no timestamp readbacks completed")}else{None}},
        "resources":{"geometry_allocated_bytes":resources.geometry,"geometry_live_bytes":resources.geometry_live,
            "ray_source_allocated_bytes":resources.ray_source,"object_record_bytes":resources.object_records,
            "texture_gpu_bytes":null,"render_target_gpu_bytes":null},
        "limitations":["Offscreen target: excludes canvas presentation, display frame pacing and input latency",
            "Serialized completion includes browser IPC, scheduling and GPU work, not pure GPU execution",
            "Generated assets: content phase includes generation/upload but no network transfer or file decoding",
            "Resource observations are partial, not total VRAM; GPU texture/render-target totals unavailable"],
        "errors":*errors.lock().unwrap()}),
    )
}
