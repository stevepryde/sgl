//! Native Bistro rendering through the public Scene/Renderer API.
//! Explicitly prepare content first: see ../BISTRO.md.
#[path = "support/bistro_scene.rs"]
#[allow(dead_code)]
mod bistro_scene;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{collections::BTreeMap, error::Error, path::PathBuf, sync::Mutex, time::Instant};

    use super::bistro_scene;
    use sgl_3d::{
        InstanceState, Mobility, RayData, Renderer, Scene, SceneOptions, asset,
        environment::{EnvironmentMap, PmremAtlas},
        settings::{Antialiasing, DynamicGiQuality},
        timing::GpuTiming,
    };

    const WARM_UP: u64 = 8;
    struct Options {
        asset: PathBuf,
        output: PathBuf,
        size: [u32; 2],
        frames: u32,
        view: String,
        exposure: Option<f32>,
        rect: bool,
        normal_maps: bool,
        max_quality: bool,
        aa: Option<Antialiasing>,
        no_gi: bool,
        no_ray_shadows: bool,
        settings_file: Option<PathBuf>,
        gi_volume_file: Option<PathBuf>,
        fixture_file: Option<PathBuf>,
        warmup: Option<u64>,
        cascades: Option<u32>,
        capture_probe: Option<PathBuf>,
    }
    impl Options {
        fn read() -> Result<Self, Box<dyn Error>> {
            let mut options = Self {
                asset: "target/bistro/prepared/BistroExterior.gltf".into(),
                output: "target/bistro/captures".into(),
                size: [1280, 720],
                frames: 48,
                view: "all".into(),
                exposure: None,
                rect: false,
                normal_maps: true,
                max_quality: false,
                aa: None,
                no_gi: false,
                no_ray_shadows: false,
                settings_file: None,
                gi_volume_file: None,
                fixture_file: None,
                warmup: None,
                cascades: None,
                capture_probe: None,
            };
            let mut args = std::env::args().skip(1);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--asset" => options.asset = args.next().ok_or("--asset needs a path")?.into(),
                    "--output" => {
                        options.output = args.next().ok_or("--output needs a path")?.into()
                    }
                    "--frames" => {
                        options.frames = args.next().ok_or("--frames needs a count")?.parse()?
                    }
                    "--view" => {
                        options.view = args
                            .next()
                            .ok_or("--view needs all or a fixture camera id")?
                    }
                    "--exposure" => {
                        options.exposure =
                            Some(args.next().ok_or("--exposure needs stops")?.parse()?)
                    }
                    "--size" => {
                        let text = args.next().ok_or("--size needs WxH")?;
                        let (x, y) = text.split_once('x').ok_or("--size needs WxH")?;
                        options.size = [x.parse()?, y.parse()?];
                    }
                    "--gi-volume" => {
                        options.gi_volume_file =
                            Some(args.next().ok_or("--gi-volume needs a JSON path")?.into())
                    }
                    "--fixture" => {
                        options.fixture_file =
                            Some(args.next().ok_or("--fixture needs a JSON path")?.into())
                    }
                    "--capture-probe" => {
                        options.capture_probe = Some(
                            args.next()
                                .ok_or("--capture-probe needs a descriptor JSON path")?
                                .into(),
                        )
                    }
                    "--warmup" => {
                        options.warmup = Some(args.next().ok_or("--warmup needs a count")?.parse()?)
                    }
                    "--cascades" => {
                        options.cascades =
                            Some(args.next().ok_or("--cascades needs 1..4")?.parse()?)
                    }
                    "--settings" => {
                        options.settings_file =
                            Some(args.next().ok_or("--settings needs a JSON path")?.into())
                    }
                    "--max-quality" => options.max_quality = true,
                    "--aa" => {
                        options.aa = Some(match args.next().as_deref() {
                            Some("taa") => Antialiasing::Taa,
                            Some("fsr2") => Antialiasing::Fsr2,
                            Some("smaa") => Antialiasing::Smaa,
                            Some("off") => Antialiasing::Off,
                            _ => return Err("--aa needs taa|fsr2|smaa|off".into()),
                        })
                    }
                    "--no-gi" => options.no_gi = true,
                    "--no-ray-shadows" => options.no_ray_shadows = true,
                    "--rect-light" => options.rect = true,
                    "--no-normal-maps" => options.normal_maps = false,
                    "--help" => {
                        println!(
                            "bistro [--asset FILE] [--output DIR] [--view all|CAMERA_ID] [--size WxH] [--frames N] [--warmup N] [--exposure STOPS] [--rect-light] [--no-normal-maps] [--max-quality] [--aa taa|fsr2|smaa|off] [--no-gi] [--no-ray-shadows] [--settings FILE] [--gi-volume FILE] [--fixture FILE] [--cascades 1..4] [--capture-probe FILE]"
                        );
                        std::process::exit(0);
                    }
                    _ => return Err(format!("unknown option {arg}").into()),
                }
            }
            if u64::from(options.frames) <= options.warmup.unwrap_or(WARM_UP)
                || options.size.contains(&0)
                || options.exposure.is_some_and(|n| !n.is_finite())
                || options.cascades.is_some_and(|n| !(1..=4).contains(&n))
            {
                return Err("invalid frames (must exceed 8), size or exposure".into());
            }
            if options.capture_probe.is_some() && options.view == "all" {
                return Err("--capture-probe needs one explicit --view".into());
            }
            Ok(options)
        }
    }
    fn environment(
        directory: &std::path::Path,
        spec: Option<&serde_json::Value>,
    ) -> Result<EnvironmentMap, Box<dyn Error>> {
        let path = |key: &str, fallback: &str| {
            bistro_scene::asset_path(
                directory,
                spec.and_then(|s| s[key].as_str()).unwrap_or(fallback),
            )
        };
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path("metadata", "environment.json")?)?)?;
        Ok(EnvironmentMap {
            panorama: image::open(path("panorama", "panorama.png")?)?.into_rgba8(),
            filtered: PmremAtlas {
                width: metadata["width"]
                    .as_u64()
                    .ok_or("environment width missing")? as u32,
                height: metadata["height"]
                    .as_u64()
                    .ok_or("environment height missing")? as u32,
                rgba16: std::fs::read(path("pmrem", "pmrem.rgba16")?)?,
            },
        })
    }
    fn read(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(sgl_3d::diagnostics::read(device, queue, texture, 4))
    }
    pub fn run() -> Result<(), Box<dyn Error>> {
        let options = Options::read()?;
        let fixture: Option<serde_json::Value> = options
            .fixture_file
            .as_ref()
            .map(|path| -> Result<_, Box<dyn Error>> {
                Ok(serde_json::from_slice(&std::fs::read(path)?)?)
            })
            .transpose()?;
        if fixture.as_ref().is_some_and(|f| f["schema"] != 1) {
            return Err("unknown fixture schema".into());
        }
        let exposure = options.exposure.unwrap_or_else(|| {
            fixture
                .as_ref()
                .and_then(|f| f["exposure_stops"].as_f64())
                .unwrap_or(0.) as f32
        });
        if !exposure.is_finite() {
            return Err("fixture exposure must be finite".into());
        }
        let settings_override: Option<serde_json::Value> = options
            .settings_file
            .as_ref()
            .map(|path| -> Result<_, Box<dyn Error>> {
                Ok(serde_json::from_slice(&std::fs::read(path)?)?)
            })
            .transpose()?;
        let mut settings = bistro_scene::resolve_settings(
            bistro_scene::settings(options.max_quality, options.no_ray_shadows),
            fixture.as_ref(),
            settings_override.as_ref(),
            options.aa,
        )?;
        settings.diagnostics.disable.normal_maps = !options.normal_maps;
        let cameras = bistro_scene::selected_cameras(fixture.as_ref(), &options.view)?;
        let model_scale =
            bistro_scene::model_scale(fixture.as_ref().unwrap_or(&serde_json::Value::Null))?;
        std::fs::create_dir_all(&options.output)?;
        let start = Instant::now();
        let ray_data = match fixture
            .as_ref()
            .and_then(|f| f["ray_data"].as_str())
            .unwrap_or("enabled")
        {
            "enabled" => RayData::Enabled,
            "disabled" => RayData::Disabled,
            _ => return Err("ray_data must be enabled or disabled".into()),
        };
        let budget = if let Some(spec) = fixture.as_ref().and_then(|f| f.get("load_budget")) {
            asset::LoadBudget::new(
                spec["source_bytes"]
                    .as_u64()
                    .ok_or("load_budget.source_bytes missing")?,
                spec["decoded_bytes"]
                    .as_u64()
                    .ok_or("load_budget.decoded_bytes missing")?,
            )?
        } else {
            asset::LoadBudget::default()
        };
        let directory = options
            .asset
            .parent()
            .ok_or("asset needs parent directory")?;
        let manifest: Option<serde_json::Value> = fixture
            .as_ref()
            .and_then(|f| f["compressed_images"].as_str())
            .map(|name| -> Result<_, Box<dyn Error>> {
                Ok(serde_json::from_slice(&std::fs::read(
                    bistro_scene::asset_path(directory, name)?,
                )?)?)
            })
            .transpose()?;
        let compression_feature = if let Some(manifest) = &manifest {
            if manifest["schema"] != 1 {
                return Err("unknown compressed-image manifest schema".into());
            }
            match manifest["format"].as_str() {
                Some("bc7") => wgpu::Features::TEXTURE_COMPRESSION_BC,
                Some("etc2") => wgpu::Features::TEXTURE_COMPRESSION_ETC2,
                Some("astc4x4") => wgpu::Features::TEXTURE_COMPRESSION_ASTC,
                _ => return Err("compressed manifest format must be bc7, etc2 or astc4x4".into()),
            }
        } else {
            wgpu::Features::empty()
        };
        let mut supplied = BTreeMap::new();
        let mut allowance = bistro_scene::SuppliedImages::new(budget);
        if let Some(manifest) = &manifest {
            for entry in manifest["images"]
                .as_array()
                .ok_or("compressed manifest images missing")?
            {
                let index = usize::try_from(
                    entry["index"]
                        .as_u64()
                        .ok_or("compressed image index missing")?,
                )?;
                let data = std::fs::read(bistro_scene::asset_path(
                    directory,
                    entry["ktx2"]
                        .as_str()
                        .ok_or("compressed image KTX2 missing")?,
                )?)?;
                let companion = if let Some(path) = entry["decoded_level_0"]
                    .as_str()
                    .filter(|_| ray_data == RayData::Enabled)
                {
                    Some(std::fs::read(bistro_scene::asset_path(directory, path)?)?)
                } else {
                    None
                };
                let image = allowance.decode(manifest, entry, &data, companion.as_deref())?;
                if supplied
                    .insert(index, asset::Image::Compressed(image))
                    .is_some()
                {
                    return Err("duplicate compressed image index".into());
                }
            }
        }
        let supplied = Mutex::new(supplied);
        let images = |image: asset::GltfImage<'_>| -> asset::Result<asset::ImageSource> {
            Ok(supplied
                .lock()
                .unwrap()
                .remove(&image.index)
                .map_or(asset::ImageSource::Decode, asset::ImageSource::Supplied))
        };
        let loaded = asset::load_with_options(
            &options.asset,
            asset::LoadOptions {
                budget,
                images: Some(&images),
                ..Default::default()
            },
        )?;
        if !supplied.lock().unwrap().is_empty() {
            return Err("compressed manifest has indices absent from asset".into());
        }
        let material_names: Vec<_> = loaded.materials.iter().map(|m| m.name.clone()).collect();
        let triangles: usize = loaded.meshes.iter().map(|m| m.indices.len() / 3).sum();
        let vertices: usize = loaded.meshes.iter().map(|m| m.vertices.len()).sum();
        let mesh_count = loaded.meshes.len();
        let material_count = loaded.materials.len();
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
                            .map_or(0, |level| level.as_raw().len())
                }
                asset::Image::Rgba8Mips(image) => [false, true]
                    .into_iter()
                    .flat_map(|srgb| image.levels(srgb))
                    .map(|level| level.as_raw().len())
                    .sum(),
            })
            .sum();
        let ignored: Vec<String> = loaded
            .ignored
            .iter()
            .map(|item| format!("{item:?}"))
            .collect();
        let load_ms = start.elapsed().as_secs_f64() * 1000.;
        println!(
            "Bistro: {triangles} triangles, {vertices} vertices, {mesh_count} batches, {material_count} materials; decoded images {:.1} MiB; loaded in {load_ms:.0} ms",
            image_bytes as f64 / 1048576.
        );
        for item in &ignored {
            println!("Asset ignored: {item}");
        }
        let adapter =
            pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
        let info = adapter.get_info();
        if !adapter.features().contains(compression_feature) {
            return Err("adapter lacks requested compressed format".into());
        }
        let warm_up = options.warmup.unwrap_or_else(|| {
            if options.max_quality {
                (options.frames / 2).min(120) as u64
            } else {
                WARM_UP
            }
        });
        let hardware_features = if settings.hardware_ray_tracing && ray_data == RayData::Enabled {
            sgl_3d::graphics_device::ray_tracing_features(&adapter)
        } else {
            wgpu::Features::empty()
        };
        let fsr2_features = if settings.antialiasing == Antialiasing::Fsr2 {
            sgl_3d::graphics_device::fsr2_features(&adapter)
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_features: sgl_3d::graphics_device::features(&adapter)
                    | hardware_features
                    | fsr2_features
                    | compression_feature
                    | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
                required_limits: sgl_3d::graphics_device::limits(&adapter),
                experimental_features: if hardware_features.is_empty() {
                    wgpu::ExperimentalFeatures::disabled()
                } else {
                    // SAFETY: this opt-in benchmark accepts wgpu's experimental ray queries.
                    unsafe { wgpu::ExperimentalFeatures::enabled() }
                },
                ..Default::default()
            }))?;
        let upload = Instant::now();
        let mut scene = Scene::with_options(&device, &queue, SceneOptions { ray_data });
        let ids = scene.add_asset(&device, &queue, loaded)?;
        scene.add_instance(
            &device,
            &queue,
            InstanceState {
                pose: sgl_3d::glam::Mat4::from_scale(sgl_3d::glam::Vec3::splat(model_scale)),
                ..InstanceState::new(ids.model)
            },
            Mobility::Static,
        )?;
        let environment = scene.add_environment(
            &device,
            &queue,
            &environment(
                options
                    .asset
                    .parent()
                    .ok_or("asset needs parent directory")?,
                fixture.as_ref().map(|f| &f["environment"]),
            )?,
        )?;
        if let Some(spec) = fixture
            .as_ref()
            .and_then(|f| f.get("specular_probe"))
            .filter(|v| !v.is_null())
        {
            bistro_scene::probe_shape(spec)?;
            let payload = std::fs::read(bistro_scene::asset_path(
                directory,
                spec["rgba16"]
                    .as_str()
                    .ok_or("probe rgba16 basename missing")?,
            )?)?;
            scene.set_baked_specular_probes(
                &device,
                &queue,
                &[bistro_scene::probe_from_json(spec, &payload)?],
            )?;
        }
        let mut local_light_ids = Vec::new();
        if let Some(descriptor) = fixture.as_ref() {
            for light in bistro_scene::point_lights_from_json(descriptor)? {
                local_light_ids.push(scene.add_light(&device, &queue, light)?);
            }
        }
        if options.rect {
            // Authored fill for exploring SGL's existing LTC rectangle shading on
            // the scooter, not a claim that the source asset contains this light.
            local_light_ids.push(scene.add_light(&device, &queue, bistro_scene::rect_light())?);
        }
        // Authored content covering the street/courtyard cameras, not an automatic bake.
        let gi_volume = if options.no_gi {
            None
        } else if let Some(path) = &options.gi_volume_file {
            Some(bistro_scene::volume_from_json(&serde_json::from_slice(
                &std::fs::read(path)?,
            )?)?)
        } else if let Some(volume) = fixture
            .as_ref()
            .and_then(|f| f.get("dynamic_gi_volume"))
            .filter(|v| !v.is_null())
        {
            Some(bistro_scene::volume_from_json(volume)?)
        } else {
            options.max_quality.then(bistro_scene::gi_volume)
        };
        scene.set_dynamic_gi_volume(&device, gi_volume)?;
        let upload_ms = upload.elapsed().as_secs_f64() * 1000.;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Bistro output"),
            size: wgpu::Extent3d {
                width: options.size[0],
                height: options.size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut renderer = Renderer::new(
            &device,
            &queue,
            texture.format(),
            options.size,
            1.,
            &settings,
        )?;
        let mut reports = Vec::new();
        for camera_recipe in &cameras {
            let name = camera_recipe["id"].as_str().ok_or("camera id missing")?;
            let eye = sgl_3d::glam::Vec3::from_array(serde_json::from_value(
                camera_recipe["eye"].clone(),
            )?);
            let look_at = sgl_3d::glam::Vec3::from_array(serde_json::from_value(
                camera_recipe["look_at"].clone(),
            )?);
            let camera_spec = fixture.as_ref().map(|_| camera_recipe);
            let mut frame = if fixture.is_some() {
                let spec = camera_spec.ok_or("fixture camera not found")?;
                bistro_scene::frame_from_json(spec, options.size, exposure, options.cascades)?
            } else {
                bistro_scene::frame(
                    name,
                    eye,
                    look_at,
                    options.size,
                    exposure,
                    options.max_quality,
                )
            };
            if let Some(cascades) = options.cascades
                && let Some(shadow) = frame.directional_lights[0]
                    .as_mut()
                    .and_then(|sun| sun.shadow.as_mut())
            {
                shadow.cascades = cascades;
            }
            frame.environment = Some(environment);
            let mut timing = GpuTiming::new(&device, &queue);
            let mut times: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
            let mut local_shadows = bistro_scene::LocalShadowMeasurements::default();
            for index in 0..options.frames {
                frame.camera_cut = index == 0;
                frame.elapsed_seconds = index as f64 / 60.;
                frame.frame_time_ms = 1000. / 60.;
                if let Some(timing) = &mut timing {
                    for done in timing.begin_frame(&device, &queue) {
                        if done.frame > warm_up {
                            for pass in done.passes {
                                times.entry(pass.name).or_default().push(pass.ms);
                            }
                        }
                    }
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
                    timing.as_ref(),
                );
                local_shadows.observe(renderer.local_shadow_stats(), u64::from(index) >= warm_up);
                queue.submit([encoder.finish()]);
                if let Some(timing) = &mut timing {
                    timing.submitted(&queue);
                }
                renderer.finish_frame(&mut scene);
                device.poll(wgpu::PollType::wait_indefinitely())?;
            }
            for _ in 0..4 {
                if let Some(timing) = &mut timing {
                    for done in timing.begin_frame(&device, &queue) {
                        if done.frame > warm_up {
                            for pass in done.passes {
                                times.entry(pass.name).or_default().push(pass.ms);
                            }
                        }
                    }
                }
                device.poll(wgpu::PollType::wait_indefinitely())?;
            }
            image::save_buffer(
                options.output.join(format!("{name}.png")),
                &read(&device, &queue, &texture)?,
                options.size[0],
                options.size[1],
                image::ColorType::Rgba8,
            )?;
            // Explicit native authoring after the measured frames. Capture-only
            // material exclusions are restored on both success and failure.
            if let Some(path) = &options.capture_probe {
                let mut spec: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
                let (face_size, expected_bytes) = bistro_scene::probe_shape(&spec)?;
                let center =
                    sgl_3d::glam::Vec3::from_array(serde_json::from_value(spec["center"].clone())?);
                let wanted: Vec<String> =
                    serde_json::from_value(spec["exclude_materials"].clone())?;
                const RESERVED: u32 = 1 << 23;
                let all: Vec<_> = ids
                    .materials
                    .iter()
                    .map(|&id| scene.material(id).map(|m| (id, m)))
                    .collect::<Result<_, _>>()?;
                if all.iter().any(|(_, m)| m.visibility_group & RESERVED != 0) {
                    return Err("probe exclusion group is already used".into());
                }
                let saved: Vec<_> = all
                    .iter()
                    .zip(&material_names)
                    .filter(|(_, name)| wanted.contains(name))
                    .map(|(&(id, m), _)| (id, m))
                    .collect();
                if saved.len() != wanted.len() {
                    return Err("probe exclusion material names must resolve exactly once".into());
                }
                let previous_mask = frame.visibility_mask;
                frame.visibility_mask &= !RESERVED;
                let applied_capture_frame = bistro_scene::frame_metadata(&frame);
                let captured = (|| -> Result<_, Box<dyn Error>> {
                    for &(id, mut material) in &saved {
                        material.visibility_group = RESERVED;
                        scene.set_material(&queue, id, material)?;
                    }
                    Ok(renderer.capture_specular_probe(
                        &device, &queue, &mut scene, &frame, &settings, center, face_size,
                    )?)
                })();
                frame.visibility_mask = previous_mask;
                let mut restoration_error = None;
                for (id, material) in saved {
                    if let Err(e) = scene.set_material(&queue, id, material) {
                        restoration_error.get_or_insert(e);
                    }
                }
                if let Some(e) = restoration_error {
                    return Err(e.into());
                }
                let radiance = captured?;
                let sgl_3d::SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
                    return Err("native capture returned unexpected encoding".into());
                };
                let payload: Vec<u8> = texels.iter().flat_map(|n| n.to_le_bytes()).collect();
                if payload.len() != expected_bytes {
                    return Err("native probe capture payload length mismatch".into());
                }
                let basename = spec["rgba16"]
                    .as_str()
                    .filter(|n| {
                        !n.is_empty()
                            && n.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    })
                    .ok_or("probe rgba16 must be a basename")?;
                std::fs::write(options.output.join(basename), &payload)?;
                spec["capture"] = serde_json::json!({"view":name,"after_frames":options.frames,"warmup_frames":warm_up,
                    "settings":settings,"normal_maps":options.normal_maps,"rect_light":options.rect,"ray_data":format!("{ray_data:?}"),
                    "applied_frame":applied_capture_frame,"camera_unused_for_capture":true,
                    "dynamic_gi_volume":gi_volume.map(|v|serde_json::json!({"origin":v.origin.to_array(),"spacing":v.spacing.to_array(),"probes":v.probes})),
                    "dynamic_gi_in_effect":format!("{:?}",renderer.dynamic_gi_in_effect(&settings,&scene)),
                    "model_scale":model_scale,"run_arguments":std::env::args().skip(1).collect::<Vec<_>>(),
                    "fixture":fixture,"excluded_from_frame_timings":true,"payload_bytes":payload.len(),"static_content_only":true});
                std::fs::write(
                    options.output.join("specular-probe.json"),
                    serde_json::to_vec_pretty(&spec)?,
                )?;
            }
            let mut groups = serde_json::Map::new();
            for (label, values) in &mut times {
                values.sort_by(f64::total_cmp);
                let at =
                    |q: f64| values[((values.len() as f64 * q).ceil() as usize).saturating_sub(1)];
                println!(
                    "{name}: {label}: median {:.3} / p95 {:.3} ms",
                    at(0.5),
                    at(0.95)
                );
                groups.insert(
                (*label).into(),
                serde_json::json!({"median_ms":at(0.5),"p95_ms":at(0.95),"frames":values.len()}),
            );
            }
            reports.push(serde_json::json!({"view":name,"eye":frame.camera.eye.to_array(),"look_at":camera_spec.map_or(serde_json::json!(look_at.to_array()),|c| c["look_at"].clone()),"fov_y_degrees":camera_spec.and_then(|c|c["fov_y_degrees"].as_f64()).unwrap_or(55.),"camera_view":frame.camera.view.to_cols_array(),"camera_projection":frame.camera.projection.to_cols_array(),"gpu_timings":groups,"local_shadows":local_shadows.report(),
                "directional_light": {"direction":frame.directional_lights[0].unwrap().direction.to_array(),"color":frame.directional_lights[0].unwrap().color,"illuminance":frame.directional_lights[0].unwrap().illuminance,"shadow_cascades":frame.directional_lights[0].unwrap().shadow.map(|s|s.cascades),"shadow_distance":frame.directional_lights[0].unwrap().shadow.map(|s|s.distance)},
                "environment_intensity": frame.diffuse_environment.intensity,
                "environment_yaw":frame.diffuse_environment.yaw,
                "ambient_occlusion_radius_m":frame.ambient_occlusion_radius,
                "effective": {
                    "antialiasing": format!("{:?}",renderer.antialiasing_in_effect(&settings)),
                    "render_size": renderer.render_size(),
                    "scene_size": renderer.scene_size(),
                    "fsr2_error": renderer.fsr2_error(),
                    "hardware_ray_tracing": renderer.ray_tracing_in_effect(&settings, &scene),
                    "ray_tracing_error": renderer.ray_tracing_error(),
                    "ray_tracing_stats": format!("{:?}",renderer.ray_tracing_stats()),
                    "ray_traced_shadows": renderer.ray_traced_shadows_in_effect(&settings, &scene),
                    "dynamic_gi_quality": format!("{:?}",renderer.dynamic_gi_in_effect(&settings, &scene)),
                    "dynamic_gi_enabled": gi_volume.is_some() && renderer.dynamic_gi_in_effect(&settings, &scene) != DynamicGiQuality::Off,
                }}));
        }
        std::fs::write(
            options.output.join("measurements.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "adapter":format!("{} / {:?}",info.name,info.backend),"device_features":format!("{:?}",device.features()),"asset":options.asset,"model_scale":model_scale,"triangles":triangles,"vertices":vertices,"material_batches":mesh_count,"materials":material_count,"decoded_image_bytes":image_bytes,"load_ms":load_ms,"prepare_upload_ms":upload_ms,"ignored":ignored,"ray_data":format!("{ray_data:?}"),"load_budget":{"source_bytes":budget.source_bytes(),"decoded_bytes":budget.decoded_bytes()},"owned_supplied_image_bytes":allowance.used,"compressed_manifest":manifest,
                "size":options.size,"frames_per_view":options.frames,"local_lights":bistro_scene::local_light_report(&scene, &local_light_ids)?,"warm_up_frames":warm_up,"exposure_stops":exposure,"rect_light":options.rect,"normal_maps":options.normal_maps,"views":reports,"fixture":fixture,
                "settings": settings, "max_quality": options.max_quality,
                "gi_volume": gi_volume.map(|volume| serde_json::json!({"origin":volume.origin.to_array(),"spacing":volume.spacing.to_array(),"probes":volume.probes})),
                "lighting":"Explicit supplied HDRI/PMREM and authored lights, recorded per view and in the optional fixture; optional LTC rectangle, dynamic GI and authored local specular probe; no reference-renderer parity claim"
            }))?,
        )?;
        let asset_directory = options
            .asset
            .parent()
            .ok_or("asset needs parent directory")?;
        if let Some(notices) = fixture.as_ref().and_then(|f| f["notices"].as_array()) {
            for notice in notices {
                let name = notice.as_str().ok_or("notice needs a basename")?;
                let source = bistro_scene::asset_path(asset_directory, name)?;
                let basename = source.file_name().ok_or("notice needs a basename")?;
                std::fs::copy(&source, options.output.join(basename))?;
            }
        } else {
            for (source, destination) in [
                ("LICENSE.txt", "BISTRO-LICENSE.txt"),
                ("SGL-ATTRIBUTION.txt", "BISTRO-ATTRIBUTION.txt"),
            ] {
                std::fs::copy(
                    bistro_scene::asset_path(asset_directory, source)?,
                    options.output.join(destination),
                )?;
            }
        }
        println!(
            "Bistro captures: {}",
            options.output.canonicalize()?.display()
        );
        Ok(())
    }
}
