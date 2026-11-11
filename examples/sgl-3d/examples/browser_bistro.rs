//! Real embedded-scene WebGPU benchmark; fixture policy remains example-owned.
#![cfg(target_arch = "wasm32")]
#[path = "support/bistro_scene.rs"]
#[allow(dead_code)]
mod bistro_scene;

use serde_json::{Value, json};
use sgl_3d::{
    InstanceState, Mobility, RayData, Renderer, Scene, SceneOptions, asset,
    environment::{EnvironmentMap, PmremAtlas},
    glam::{Mat4, Vec3, camera},
    settings::{DynamicGiQuality, Settings},
    timing::{FrameTime, GpuTiming},
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::JsFuture;

thread_local! {
    static CAPTURE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static SEQUENCE: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}
#[wasm_bindgen]
pub fn take_capture() -> Vec<u8> {
    CAPTURE.with(|capture| std::mem::take(&mut *capture.borrow_mut()))
}
#[wasm_bindgen]
pub fn take_sequence_capture(index: u32) -> Vec<u8> {
    SEQUENCE.with(|frames| {
        frames
            .borrow_mut()
            .get_mut(index as usize)
            .map(std::mem::take)
            .unwrap_or_default()
    })
}
#[wasm_bindgen]
pub fn fixture() -> String {
    bistro_scene::fixture().to_string()
}

fn now() -> f64 {
    let performance = js_sys::Reflect::get(&js_sys::global(), &"performance".into()).unwrap();
    let function: js_sys::Function = js_sys::Reflect::get(&performance, &"now".into())
        .unwrap()
        .unchecked_into();
    function.call0(&performance).unwrap().as_f64().unwrap()
}
fn phase(name: &str) {
    let _ = js_sys::Reflect::set(&js_sys::global(), &"__sglPhase".into(), &name.into());
}
fn js_error(error: JsValue) -> String {
    format!("{error:?}")
}
async fn bytes(path: &str) -> Result<Vec<u8>, String> {
    // Only basenames, optionally in the fixed Compressed/ companion folder.
    let basename = path.strip_prefix("Compressed/").unwrap_or(path);
    if basename.is_empty()
        || !basename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(format!("asset path must be a basename: {path}"));
    }
    let global = js_sys::global();
    let fetch: js_sys::Function = js_sys::Reflect::get(&global, &"fetch".into())
        .map_err(js_error)?
        .unchecked_into();
    let response = JsFuture::from(js_sys::Promise::from(
        fetch
            .call1(&global, &format!("/assets/{path}").into())
            .map_err(js_error)?,
    ))
    .await
    .map_err(js_error)?;
    if js_sys::Reflect::get(&response, &"ok".into())
        .map_err(js_error)?
        .as_bool()
        != Some(true)
    {
        return Err(format!(
            "fetch {path} failed: {:?}",
            js_sys::Reflect::get(&response, &"status".into())
        ));
    }
    let read: js_sys::Function = js_sys::Reflect::get(&response, &"arrayBuffer".into())
        .map_err(js_error)?
        .unchecked_into();
    let buffer = JsFuture::from(js_sys::Promise::from(
        read.call0(&response).map_err(js_error)?,
    ))
    .await
    .map_err(js_error)?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
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
    json!({"samples":values.len(),"median_ms":values[(values.len()-1)/2],
        "p95_ms":values[(values.len()*95).div_ceil(100)-1],"max_ms":values[values.len()-1]})
}
fn collect(
    timing: &mut Option<GpuTiming>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    warmup: usize,
    frames: usize,
    reports: &mut Vec<FrameTime>,
) {
    if let Some(timing) = timing {
        reports.extend(timing.begin_frame(device, queue).filter(|frame| {
            frame.frame > warmup as u64 && frame.frame <= (warmup + frames) as u64
        }));
    }
}
fn vector(value: &Value) -> Result<Vec3, String> {
    let a = value
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("expected a three-component vector")?;
    let v = Vec3::new(
        a[0].as_f64().ok_or("invalid vector")? as f32,
        a[1].as_f64().ok_or("invalid vector")? as f32,
        a[2].as_f64().ok_or("invalid vector")? as f32,
    );
    if !v.is_finite() {
        return Err("nonfinite vector".into());
    }
    Ok(v)
}
fn number(value: &Value, fallback: f32) -> Result<f32, String> {
    let v = value.as_f64().map_or(fallback, |v| v as f32);
    if !v.is_finite() {
        return Err("nonfinite value".into());
    }
    Ok(v)
}
fn file<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("fixture missing {key}"))
}

async fn supplied_images(
    manifest: &Value,
    budget: asset::LoadBudget,
    ray_data: RayData,
) -> Result<(BTreeMap<usize, asset::Image>, Value), String> {
    let mut allowance = bistro_scene::SuppliedImages::new(budget);
    let entries = manifest["images"]
        .as_array()
        .ok_or("compressed manifest images missing")?;
    let start = now();
    let mut images = BTreeMap::new();
    let mut ktx_bytes = 0;
    let mut companion_bytes = 0;
    for entry in entries {
        let index = usize::try_from(
            entry["index"]
                .as_u64()
                .ok_or("compressed image index missing")?,
        )
        .map_err(|_| "compressed image index exceeds this target")?;
        let encoded = bytes(file(entry, "ktx2")?).await?;
        ktx_bytes += encoded.len();
        let companion = entry["decoded_level_0"]
            .as_str()
            .filter(|_| ray_data == RayData::Enabled);
        let encoded_companion = if let Some(path) = companion {
            let encoded_companion = bytes(path).await?;
            companion_bytes += encoded_companion.len();
            Some(encoded_companion)
        } else {
            None
        };
        let image = allowance
            .decode(manifest, entry, &encoded, encoded_companion.as_deref())
            .map_err(|e| format!("compressed image {index}: {e}"))?;
        if images
            .insert(index, asset::Image::Compressed(image))
            .is_some()
        {
            return Err(format!("duplicate compressed image {index}"));
        }
    }
    let observation = json!({"format":manifest["format"],"images":images.len(),"ktx2_source_bytes":ktx_bytes,
        "decoded_companion_source_bytes":companion_bytes,"companions_omitted":ray_data == RayData::Disabled,
        "owned_supplied_image_bytes":allowance.used,"fetch_parse_ms":now()-start,"manifest":manifest});
    Ok((images, observation))
}
fn limits(limits: &wgpu::Limits) -> Value {
    json!({"max_storage_buffer_binding_size":limits.max_storage_buffer_binding_size,"max_buffer_size":limits.max_buffer_size,
        "max_sampled_textures_per_shader_stage":limits.max_sampled_textures_per_shader_stage,
        "max_texture_dimension_2d":limits.max_texture_dimension_2d,"max_texture_array_layers":limits.max_texture_array_layers,
        "max_storage_buffers_per_shader_stage":limits.max_storage_buffers_per_shader_stage})
}
fn gi_report(report: &sgl_3d::diagnostics::DynamicGiReport) -> Value {
    json!({"frame":report.frame,"skipped":report.skipped,"probes":report.probes,"traced_probes":report.traced_probes,
        "unblended_probes":report.unblended_probes,"blended_requests":report.blended_requests,"stride":report.stride,"probes_by_rays":report.probes_by_rays,
        "rays":report.rays,"fixed_rays":report.fixed_rays,"hits":report.hits,"visibility_rays":report.visibility_rays,
        "ray_visits":report.ray_visits,"most_ray_visits":report.most_ray_visits,"visibility_visits":report.visibility_visits,
        "most_visibility_visits":report.most_visibility_visits,"exhausted_queries":report.exhausted_queries,
        "paused":report.paused,"converged":report.converged,"changes":format!("{:?}",report.changes),"scrolled":report.scrolled,"moving_bounds":report.moving_bounds})
}

async fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    output: &wgpu::Texture,
    size: [u32; 2],
) -> Result<(), String> {
    let row = (size[0] * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene final capture"),
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

#[wasm_bindgen]
pub async fn measure(descriptor: String, options: String) -> String {
    console_error_panic_hook::set_once();
    match measured(&descriptor, &options).await {
        Ok(report) => report.to_string(),
        Err(error) => json!({"schema":1,"error":error,
            "fixture":serde_json::from_str::<Value>(&descriptor).ok(),
            "requested_options":serde_json::from_str::<Value>(&options).ok()})
        .to_string(),
    }
}
async fn measured(descriptor: &str, options: &str) -> Result<Value, String> {
    let fixture: Value = serde_json::from_str(descriptor).map_err(|e| e.to_string())?;
    let options: Value = serde_json::from_str(options).map_err(|e| e.to_string())?;
    if fixture["schema"] != 1 {
        return Err("unknown fixture schema".into());
    }
    let frames = options["frames"].as_u64().unwrap_or(60) as usize;
    let warmup = options["warmup"]
        .as_u64()
        .or_else(|| fixture["warmup_frames"].as_u64())
        .unwrap_or(60) as usize;
    if !(60..=3600).contains(&frames) || warmup > 3600 {
        return Err("frames must be 60..=3600 and warmup <=3600".into());
    }
    let size = [
        options["width"]
            .as_u64()
            .or_else(|| fixture["size"][0].as_u64())
            .unwrap_or(1280) as u32,
        options["height"]
            .as_u64()
            .or_else(|| fixture["size"][1].as_u64())
            .unwrap_or(720) as u32,
    ];
    if size.iter().any(|&n| n == 0 || n > 4096) {
        return Err("size must be within 1..=4096".into());
    }
    let camera_id = options["view"]
        .as_str()
        .or_else(|| fixture["default_camera"].as_str())
        .unwrap_or("street");
    let camera_spec = fixture["cameras"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["id"] == camera_id))
        .ok_or("fixture camera not found")?;
    let exposure = number(
        &options["exposure"],
        number(&fixture["exposure_stops"], 0.)?,
    )?;
    let mut settings: Settings = serde_json::from_value(if options["settings"].is_object() {
        options["settings"].clone()
    } else {
        fixture["settings"].clone()
    })
    .map_err(|e| e.to_string())?;
    settings.diagnostics.dynamic_gi = options["observe_gi"].as_bool() == Some(true);
    settings.diagnostics.disable.normal_maps = options["normal_maps"].as_bool() == Some(false);
    settings.diagnostics.disable.fused_opaque = options["split"].as_bool() == Some(true);
    let ray_data = match options["ray_data"]
        .as_str()
        .or_else(|| fixture["ray_data"].as_str())
        .unwrap_or("enabled")
    {
        "enabled" => RayData::Enabled,
        "disabled" => RayData::Disabled,
        _ => return Err("ray_data must be enabled or disabled".into()),
    };
    let default_budget = asset::LoadBudget::default();
    let budget = if let Some(spec) = fixture.get("load_budget") {
        asset::LoadBudget::new(
            spec["source_bytes"]
                .as_u64()
                .ok_or("load_budget.source_bytes must be an unsigned integer")?,
            spec["decoded_bytes"]
                .as_u64()
                .ok_or("load_budget.decoded_bytes must be an unsigned integer")?,
        )
        .map_err(|e| e.to_string())?
    } else {
        default_budget
    };
    let started = now();
    let compressed_manifest = if let Some(path) = options["compressed_manifest"]
        .as_str()
        .or_else(|| fixture["compressed_images"].as_str())
    {
        Some(serde_json::from_slice::<Value>(&bytes(path).await?).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let compressed_manifest_ms = now() - started;
    // Browser choices remain explicit. Requested unsupported FSR is reported as its actual fallback.
    if let Some(aa) = options["aa"].as_str() {
        settings.antialiasing = match aa {
            "taa" => sgl_3d::settings::Antialiasing::Taa,
            "smaa" => sgl_3d::settings::Antialiasing::Smaa,
            "off" => sgl_3d::settings::Antialiasing::Off,
            "fsr2" => sgl_3d::settings::Antialiasing::Fsr2,
            "fsr3" => sgl_3d::settings::Antialiasing::Fsr3,
            _ => return Err("unknown AA".into()),
        };
    }
    phase("device");
    let device_start = now();
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await
        .map_err(|e| e.to_string())?;
    let info = adapter.get_info();
    let adapter_limits = limits(&adapter.limits());
    let adapter_features = format!("{:?}", adapter.features());
    let compression_feature = compressed_manifest
        .as_ref()
        .map(bistro_scene::compressed_format)
        .transpose()?
        .map_or(wgpu::Features::empty(), |format| match format {
            asset::CompressedFormat::Bc7 => wgpu::Features::TEXTURE_COMPRESSION_BC,
            asset::CompressedFormat::Etc2Rgba8 => wgpu::Features::TEXTURE_COMPRESSION_ETC2,
            asset::CompressedFormat::Astc4x4 => wgpu::Features::TEXTURE_COMPRESSION_ASTC,
        });
    if !adapter.features().contains(compression_feature) {
        return Err(format!(
            "adapter does not support requested compression: {compression_feature:?}"
        ));
    }
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("Bistro WebGPU benchmark"),
            required_features: sgl_3d::graphics_device::features(&adapter)
                | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
                | compression_feature,
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
    let device_ms = now() - device_start;
    phase("fetch compressed images");
    let start = now();
    let (supplied, compression) = if let Some(manifest) = &compressed_manifest {
        supplied_images(manifest, budget, ray_data).await?
    } else {
        (BTreeMap::new(), Value::Null)
    };
    let compressed_image_fetch_parse_ms = now() - start;
    let supplied = Mutex::new(supplied);
    phase("fetch GLB");
    let start = now();
    let glb = bytes(file(&fixture, "asset")?).await?;
    let glb_bytes = glb.len();
    let fetch_glb_ms = now() - start;
    phase("decode GLB");
    let start = now();
    let image_source = |image: asset::GltfImage<'_>| {
        Ok(supplied
            .lock()
            .unwrap()
            .remove(&image.index)
            .map_or(asset::ImageSource::Decode, asset::ImageSource::Supplied))
    };
    let mut loaded = asset::load_slice_with_options(
        &glb,
        asset::LoadOptions {
            budget,
            images: Some(&image_source),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    if !supplied.lock().unwrap().is_empty() {
        return Err("compressed manifest supplies image indices absent from the asset".into());
    }
    drop(glb);
    let decode_ms = now() - start;
    let triangles: usize = loaded.meshes.iter().map(|m| m.indices.len() / 3).sum();
    let vertices: usize = loaded.meshes.iter().map(|m| m.vertices.len()).sum();
    let mesh_count = loaded.meshes.len();
    let material_authoring =
        bistro_scene::apply_transmissive_materials(&mut loaded.materials, &fixture)?;
    let materials = loaded.materials.len();
    let images = loaded.images.len();
    let image_bytes: usize = loaded
        .images
        .iter()
        .map(|image| match image {
            asset::Image::Rgba8(image) => image.as_raw().len(),
            asset::Image::Compressed(image) => {
                image.levels.iter().map(Vec::len).sum::<usize>()
                    + image
                        .decoded_level_0
                        .as_ref()
                        .map_or(0, |i| i.as_raw().len())
            }
            asset::Image::Rgba8Mips(image) => [false, true]
                .into_iter()
                .flat_map(|srgb| image.levels(srgb))
                .map(|l| l.as_raw().len())
                .sum(),
        })
        .sum();
    let ignored: Vec<_> = loaded
        .ignored
        .iter()
        .map(|item| format!("{item:?}"))
        .collect();
    phase("prepare and upload scene");
    let start = now();
    let mut scene = Box::new(Scene::with_options(
        &device,
        &queue,
        SceneOptions { ray_data },
    ));
    let ids = scene
        .add_asset(&device, &queue, loaded)
        .map_err(|e| e.to_string())?;
    let model_scale = bistro_scene::model_scale(&fixture)?;
    scene
        .add_instance(
            &device,
            &queue,
            InstanceState {
                pose: Mat4::from_scale(Vec3::splat(model_scale)),
                ..InstanceState::new(ids.model)
            },
            Mobility::Static,
        )
        .map_err(|e| e.to_string())?;
    let mut local_light_ids = Vec::new();
    for light in bistro_scene::point_lights_from_json(&fixture)? {
        local_light_ids.push(
            scene
                .add_light(&device, &queue, light)
                .map_err(|e| e.to_string())?,
        );
    }
    if options["rect"].as_bool() == Some(true) {
        local_light_ids.push(
            scene
                .add_light(&device, &queue, bistro_scene::rect_light())
                .map_err(|e| e.to_string())?,
        );
    }
    let gi_volume = fixture
        .get("dynamic_gi_volume")
        .filter(|value| !value.is_null())
        .map(bistro_scene::volume_from_json)
        .transpose()?;
    scene
        .set_dynamic_gi_volume(&device, gi_volume)
        .map_err(|e| e.to_string())?;
    let upload_cpu_ms = now() - start;
    queue.submit([]);
    completed(&queue).await?;
    let upload_complete_ms = now() - start;
    // Do not retain GLB/download/decoded image allocations alongside the 96 MiB HDR atlas.
    phase("fetch environment");
    let start = now();
    let env = &fixture["environment"];
    let metadata: Value =
        serde_json::from_slice(&bytes(file(env, "metadata")?).await?).map_err(|e| e.to_string())?;
    let panorama = bytes(file(env, "panorama")?).await?;
    let panorama_bytes = panorama.len();
    let panorama = image::load_from_memory(&panorama)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let pmrem = bytes(file(env, "pmrem")?).await?;
    let pmrem_bytes = pmrem.len();
    let environment_fetch_decode_ms = now() - start;
    let start = now();
    let environment = scene
        .add_environment(
            &device,
            &queue,
            &EnvironmentMap {
                panorama,
                filtered: PmremAtlas {
                    width: metadata["width"]
                        .as_u64()
                        .ok_or("environment width missing")? as u32,
                    height: metadata["height"]
                        .as_u64()
                        .ok_or("environment height missing")? as u32,
                    rgba16: pmrem,
                },
            },
        )
        .map_err(|e| e.to_string())?;
    queue.submit([]);
    completed(&queue).await?;
    let environment_upload_complete_ms = now() - start;
    phase("install authored specular probe");
    let start = now();
    let mut probe_bytes = 0;
    if let Some(spec) = fixture
        .get("specular_probe")
        .filter(|value| !value.is_null())
    {
        bistro_scene::probe_shape(spec)?;
        let payload = bytes(file(spec, "rgba16")?).await?;
        probe_bytes = payload.len();
        let probe = bistro_scene::probe_from_json(spec, &payload)?;
        scene
            .set_baked_specular_probes(&device, &queue, &[probe])
            .map_err(|e| e.to_string())?;
        queue.submit([]);
        completed(&queue).await?;
    }
    let specular_probe_fetch_upload_ms = now() - start;
    phase("renderer initialization");
    let start = now();
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Bistro display output"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = output.create_view(&Default::default());
    let mut renderer = Box::new(
        Renderer::new(&device, &queue, output.format(), size, 1., &settings)
            .map_err(|e| e.to_string())?,
    );
    let renderer_cpu_ms = now() - start;
    let eye = vector(&camera_spec["eye"])?;
    let look_at = vector(&camera_spec["look_at"])?;
    let mut frame = bistro_scene::frame_from_json(
        camera_spec,
        size,
        exposure,
        options["cascades"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok()),
    )?;
    frame.environment = Some(environment);
    let sun = &camera_spec["sun"];
    let shadow = frame.directional_lights[0].unwrap().shadow;
    let mut effective_sun = sun.clone();
    effective_sun["shadow_cascades"] = shadow.map_or(Value::Null, |s| json!(s.cascades));
    effective_sun["shadow"] = json!(shadow.is_some());
    let mut timing = GpuTiming::new(&device, &queue);
    let mut gpu_frames = Vec::new();
    let mut cpu = Vec::new();
    let mut wall = Vec::new();
    let mut draws = Vec::new();
    let mut first_frame_ms = 0.;
    let mut gi_reports = Vec::new();
    let mut local_shadows = bistro_scene::LocalShadowMeasurements::default();
    let motion = options["motion"].as_bool() == Some(true);
    for index in 0..warmup + frames {
        phase(if index < warmup {
            "warmup frames"
        } else {
            "measured frames"
        });
        let start = now();
        let seconds = index as f64 / 60.;
        frame.elapsed_seconds = seconds;
        frame.frame_time_ms = 1000. / 60.;
        frame.camera_cut = index == 0;
        if motion {
            let offset = Vec3::new((seconds as f32 * 0.7).sin() * 0.35, 0., 0.);
            frame.camera.eye = eye + offset;
            frame.camera.view =
                camera::rh::view::look_at_mat4(eye + offset, look_at + offset, Vec3::Y);
        }
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            if (frame.camera.view.transform_vector3(axis).length() - 1.).abs() > 0.0001 {
                return Err("camera view must preserve unit basis".into());
            }
        }
        collect(
            &mut timing,
            &device,
            &queue,
            warmup,
            frames,
            &mut gpu_frames,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let encode = now();
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &frame,
            &settings,
            &view,
            timing.as_ref(),
        );
        local_shadows.observe(renderer.local_shadow_stats(), index >= warmup);
        let commands = encoder.finish();
        let encode_ms = now() - encode;
        queue.submit([commands]);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
        }
        completed(&queue).await?;
        renderer.finish_frame(&mut scene);
        gi_reports.extend(
            renderer
                .take_dynamic_gi_reports(&device)
                .iter()
                .map(gi_report),
        );
        if index == 0 {
            first_frame_ms = now() - started;
        }
        if index >= warmup {
            cpu.push(encode_ms);
            wall.push(now() - start);
            draws.push(renderer.diagnostic_draws().camera as f64);
        }
        if !errors.lock().unwrap().is_empty() {
            return Err(format!("WebGPU errors: {:?}", *errors.lock().unwrap()));
        }
    }
    phase("drain timestamp maps");
    for _ in 0..12 {
        collect(
            &mut timing,
            &device,
            &queue,
            warmup,
            frames,
            &mut gpu_frames,
        );
        completed(&queue).await?;
        gi_reports.extend(
            renderer
                .take_dynamic_gi_reports(&device)
                .iter()
                .map(gi_report),
        );
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
    phase("capture readback");
    let start = now();
    capture(&device, &queue, &output, size).await?;
    let capture_ms = now() - start;
    let sequence_frames = if options["sequence"].as_bool() == Some(true) {
        32
    } else {
        0
    };
    let sequence_step = 6;
    let mut temporal_frames = Vec::new();
    if sequence_frames > 0 {
        phase("temporal readbacks after measurement");
        let final_capture = CAPTURE.with(|pixels| std::mem::take(&mut *pixels.borrow_mut()));
        let mut captures = Vec::new();
        for offset in 1..=sequence_frames * sequence_step {
            let index = warmup + frames - 1 + offset;
            let seconds = index as f64 / 60.;
            frame.elapsed_seconds = seconds;
            frame.camera_cut = false;
            if motion {
                let offset = Vec3::new((seconds as f32 * 0.7).sin() * 0.35, 0., 0.);
                frame.camera.eye = eye + offset;
                frame.camera.view =
                    camera::rh::view::look_at_mat4(eye + offset, look_at + offset, Vec3::Y);
            }
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.render(
                &device,
                &queue,
                &mut encoder,
                &mut scene,
                &frame,
                &settings,
                &view,
                None,
            );
            queue.submit([encoder.finish()]);
            completed(&queue).await?;
            renderer.finish_frame(&mut scene);
            if offset % sequence_step == 0 {
                capture(&device, &queue, &output, size).await?;
                temporal_frames.push(json!({"frame":index,"elapsed_seconds":seconds,"eye":frame.camera.eye.to_array()}));
                captures.push(CAPTURE.with(|pixels| std::mem::take(&mut *pixels.borrow_mut())));
            }
        }
        CAPTURE.with(|pixels| *pixels.borrow_mut() = final_capture);
        SEQUENCE.with(|frames| *frames.borrow_mut() = captures);
    }
    let resources = scene.diagnostic_resources();
    let linear_memory = wasm_bindgen::memory()
        .unchecked_into::<js_sys::WebAssembly::Memory>()
        .buffer()
        .unchecked_into::<js_sys::ArrayBuffer>()
        .byte_length();
    phase("complete");
    Ok(
        json!({"schema":1,"benchmark":"embedded-scene-v1","scene":fixture["scene"],"fixture":fixture,
        "view":camera_id,"camera":camera_spec,"final_eye":if motion {(eye+Vec3::new((((warmup+frames-1) as f32/60.)*0.7).sin()*0.35,0.,0.)).to_array()}else{eye.to_array()},"size":size,"frames":frames,"warmup_frames":warmup,
        "motion":{"enabled":motion,"route":"shared eye/target x += sin(t*0.7)*0.35 metres","fixed_step_seconds":1./60.,"capture_frame":warmup+frames-1},"exposure_stops":exposure,
        "settings":settings,"diagnostics":{"normal_maps_disabled":settings.diagnostics.disable.normal_maps,"split_opaque":settings.diagnostics.disable.fused_opaque},"lighting":{"local_lights":bistro_scene::local_light_report(&scene, &local_light_ids)?,"local_shadows":local_shadows.report(),"sun":effective_sun,"environment_intensity":frame.diffuse_environment.intensity,"ambient_occlusion_radius_m":frame.ambient_occlusion_radius,"rect":options["rect"].as_bool().unwrap_or(false)},
        "adapter":{"name":info.name,"backend":format!("{:?}",info.backend),"device_type":format!("{:?}",info.device_type),"available_features":adapter_features,"requested_features":format!("{:?}",device.features()),"adapter_limits":adapter_limits,"device_limits":limits(&device.limits())},
        "content":{"glb_bytes":glb_bytes,"triangles":triangles,"vertices":vertices,"material_batches":mesh_count,"model_scale":model_scale,"materials":materials,"material_authoring":material_authoring,"images":images,"decoded_image_bytes":image_bytes,"compression":compression,"specular_probe_bytes":probe_bytes,"panorama_source_bytes":panorama_bytes,"pmrem_bytes":pmrem_bytes,"ignored":ignored},
        "load_budget":{"source_bytes":budget.source_bytes(),"decoded_bytes":budget.decoded_bytes(),"target_maximum_bytes":asset::LoadBudget::maximum_bytes(),"explicit":fixture.get("load_budget").is_some(),"scope":"Same caps passed to KTX2 parsers and glTF import; not a total heap, network-transfer or GPU-memory limit"},
        "startup":{"device_ms":device_ms,"compressed_manifest_ms":compressed_manifest_ms,"compressed_image_fetch_parse_ms":compressed_image_fetch_parse_ms,"fetch_glb_ms":fetch_glb_ms,"decode_glb_ms":decode_ms,"prepare_upload_cpu_ms":upload_cpu_ms,"prepare_upload_complete_ms":upload_complete_ms,"environment_fetch_decode_ms":environment_fetch_decode_ms,"environment_upload_complete_ms":environment_upload_complete_ms,"specular_probe_fetch_upload_ms":specular_probe_fetch_upload_ms,"renderer_cpu_ms":renderer_cpu_ms,"first_frame_complete_ms":first_frame_ms},
        "effective":{"antialiasing":format!("{:?}",renderer.antialiasing_in_effect(&settings)),"render_size":renderer.render_size(),"scene_size":renderer.scene_size(),"binding_tier":format!("{:?}",renderer.binding_tier()),"fsr_error":renderer.fsr_error(),"ray_data":format!("{ray_data:?}"),"hardware_ray_tracing":renderer.ray_tracing_in_effect(&settings,&scene),"ray_tracing_error":renderer.ray_tracing_error(),"ray_tracing_stats":format!("{:?}",renderer.ray_tracing_stats()),"ray_traced_shadows":renderer.ray_traced_shadows_in_effect(&settings,&scene),"world_space_reflections":format!("{:?}",renderer.world_space_reflections_in_effect(&settings,&scene)),"dynamic_gi_quality":format!("{:?}",renderer.dynamic_gi_in_effect(&settings,&scene)),"dynamic_gi_volume":gi_volume.is_some(),"dynamic_gi_enabled":gi_volume.is_some() && renderer.dynamic_gi_in_effect(&settings,&scene)!=DynamicGiQuality::Off,"occlusion_culling":renderer.occlusion_culling_in_effect(&settings)},
        "gi_observation":{"enabled":settings.diagnostics.dynamic_gi,"frames":gi_reports.len(),"last":gi_reports.last(),"reports":gi_reports,"timing_caveat":"GI observation writes per-ray costs and performs reduction/readback; instrumented runs are separate from clean performance runs"},
        "cpu_encode":summary(&cpu),"frame_complete_wall":summary(&wall),"camera_draws":{"min":draws.iter().copied().fold(f64::INFINITY,f64::min),"max":draws.iter().copied().fold(0.,f64::max)},
        "gpu":{"supported":timing.is_some(),"measured_frames":gpu_frames.len(),"missing_frames":frames.saturating_sub(gpu_frames.len()),"total":summary(&gpu_frames.iter().map(|f|f.total_ms).collect::<Vec<_>>()),"passes":passes,"missing_reason":if timing.is_none(){Some("TIMESTAMP_QUERY unavailable")}else if gpu_frames.is_empty(){Some("no timestamp readbacks completed")}else{None}},
        "capture":{"width":size[0],"height":size[1],"frame":warmup+frames-1,"readback_ms":capture_ms,"excluded_from_timing":true,"color_format":"Rgba8UnormSrgb"},
        "temporal_capture":{"frames":sequence_frames,"step_frames":sequence_step,"first_frame":warmup+frames-1+sequence_step,"frame_metadata":temporal_frames,"excluded_from_timing":true,"purpose":"deterministic motion inspection, not performance or an image oracle"},
        "resources":{"geometry_allocated_bytes":resources.geometry,"geometry_live_bytes":resources.geometry_live,"ray_source_allocated_bytes":resources.ray_source,"ray_source_used_bytes":resources.ray_source_used,"ray_source_live_bytes":resources.ray_source_live,"object_record_bytes":resources.object_records,"wasm_linear_memory_capacity_bytes":linear_memory,"texture_gpu_bytes":null,"render_target_gpu_bytes":null},
        "limitations":["Offscreen target excludes canvas presentation, display pacing and input latency","Serialized completion includes browser IPC and scheduling; not pure GPU time","WASM linear-memory capacity is not peak or total CPU/GPU memory","GPU resource observations are partial, not total VRAM","No hardware RT requested; dynamic GI needs explicit authored volume; requested FSR may fall back","Startup is process/session timing, not a guaranteed cold-cache download"],"errors":*errors.lock().unwrap()}),
    )
}
