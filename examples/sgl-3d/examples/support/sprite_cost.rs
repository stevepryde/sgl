//! What sprites cost against game-built quads through SGL3D's other path,
//! one model whose geometry the game rebuilds for each camera
//! (`Scene::set_model`), on native and in the browser alike: per frame the
//! uploads (`diagnostics::counters`), the CPU time of the game's edit and of
//! recording the frame, the camera's geometry draws (`View::geometry_stats`;
//! a sprite batch is one instanced draw a pass besides) and the frame's GPU
//! time (`GpuTiming`), as medians over the measured frames.
//!
//! A baseline row draws the ground alone. Two cases, each a ground plane
//! under an orbiting camera, which every
//! frame moves, so a camera-facing quad must be rebuilt every frame:
//! particles (blended sprites the game moves every frame, its quads sorted
//! back to front per camera) and foliage (masked cards upright along +Y that
//! never move: a sprite batch uploads nothing after its first frame).
use sgl_3d::asset::{Asset, CpuMesh, Image, Material, MaterialTexture, Vertex};
use sgl_3d::glam::{Mat4, Vec2, Vec3, camera};
use sgl_3d::settings::{RenderPreset, Settings};
use sgl_3d::sprite::{Sprite, SpriteBatch, SpriteBlend, SpriteFacing};
use sgl_3d::timing::GpuTiming;
use sgl_3d::{
    AlphaMode, Camera, FrameInput, InstanceState, Mobility, ModelMesh, PreparedModel,
    PreparedModelOptions, RayData, Renderer, Scene, SceneOptions, View, ViewFrame, ViewOutput,
};
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;

pub const SIZE: [u32; 2] = [1280, 720];

/// What the platform measures with: its clock in milliseconds, and a wait
/// for the queue's submitted work.
pub trait Platform {
    fn now(&self) -> f64;
    fn completed<'a>(
        &'a self,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>>;
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Case {
    Particles,
    Foliage,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Path {
    /// The ground alone, what both paths add to.
    Baseline,
    Sprites,
    Quads,
}

/// The case's sprites at frame `frame`.
fn sprites(case: Case, count: usize, frame: u32) -> Vec<Sprite> {
    let time = frame as f32 / 60.;
    (0..count)
        .map(|i| {
            let f = i as f32;
            match case {
                Case::Particles => {
                    let angle = f * 2.399 + time * (0.5 + (f * 0.37).fract());
                    let radius = 0.5 + 3.5 * (f * 0.618).fract();
                    let height = 0.2 + ((f * 0.13).fract() * 3. + time * 0.4) % 3.;
                    Sprite {
                        tint: [2., 0.8 + 0.4 * (f * 0.71).fract(), 0.3, 0.6],
                        ..Sprite::new(
                            Vec3::new(angle.cos() * radius, height, angle.sin() * radius),
                            Vec2::splat(0.25),
                        )
                    }
                }
                Case::Foliage => {
                    let side = (count as f32).sqrt().ceil() as usize;
                    let (x, z) = ((i % side) as f32, (i / side) as f32);
                    let spacing = 12. / side as f32;
                    Sprite {
                        pivot: Vec2::new(0.5, 0.),
                        tint: [0.3, 0.6 + 0.3 * (f * 0.7).fract(), 0.2, 1.],
                        ..Sprite::new(
                            Vec3::new(x * spacing - 6., 0., z * spacing - 6.),
                            Vec2::new(0.5, 0.8),
                        )
                    }
                }
            }
        })
        .collect()
}

/// A soft dot for particles, a leaf cut out of a square for foliage.
fn image(case: Case) -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(32, 32, |x, y| {
        let (u, v) = ((x as f32 + 0.5) / 16. - 1., (y as f32 + 0.5) / 16. - 1.);
        let alpha = match case {
            Case::Particles => (1. - (u * u + v * v).sqrt()).max(0.),
            Case::Foliage => f32::from(u.abs() < 0.6 * (1. - v * v).max(0.).sqrt()),
        };
        image::Rgba([255, 255, 255, (alpha * 255.) as u8])
    }))
}

/// The case's sprites as quads a game builds for `view`: each corner placed
/// as the sprite faces the camera (its right and up from the view's rows,
/// or for an upright card +Y and the right across it and the camera's
/// backward axis), back to front for blended ones.
fn quads(case: Case, sprites: &[Sprite], view: &Mat4, material: sgl_3d::MaterialId) -> ModelMesh {
    let back = view.row(2).truncate();
    let (right, up) = match case {
        Case::Particles => (view.row(0).truncate(), view.row(1).truncate()),
        Case::Foliage => (Vec3::Y.cross(back).normalize(), Vec3::Y),
    };
    let mut order: Vec<usize> = (0..sprites.len()).collect();
    if case == Case::Particles {
        let depth = |i: &usize| sprites[*i].position.dot(back);
        order.sort_by(|a, b| depth(a).total_cmp(&depth(b)));
    }
    let mut vertices = Vec::with_capacity(sprites.len() * 4);
    let mut indices = Vec::with_capacity(sprites.len() * 6);
    for i in order {
        let sprite = &sprites[i];
        let first = vertices.len() as u32;
        for (x, y, u, v) in [
            (-0.5, -0.5, 0., 1.),
            (0.5, -0.5, 1., 1.),
            (0.5, 0.5, 1., 0.),
            (-0.5, 0.5, 0., 0.),
        ] {
            let offset = (Vec2::new(x, y) - (sprite.pivot - 0.5)) * sprite.size;
            vertices.push(Vertex {
                uv1: [0.0; 2],
                position: (sprite.position + right * offset.x + up * offset.y).to_array(),
                normal: back.to_array(),
                uv: [u, v],
                color: sprite.tint,
                tangent: [0.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
            });
        }
        indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    ModelMesh {
        vertices,
        indices,
        material,
        deformation: Default::default(),
    }
}

fn ground() -> Asset {
    let corner = |x: f32, z: f32| Vertex {
        uv1: [0.0; 2],
        position: [x, 0., z],
        normal: [0., 1., 0.],
        uv: [0.; 2],
        color: [1.; 4],
        tangent: [0.; 4],
        lightmap_uv: [0.; 2],
        lightmap_bounds: [0., 0., 1., 1.],
    };
    Asset {
        meshes: vec![CpuMesh {
            vertices: vec![
                corner(-8., 8.),
                corner(8., 8.),
                corner(8., -8.),
                corner(-8., -8.),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            base: [0.05, 0.05, 0.06, 1.],
            unlit: true,
            ..Default::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Default::default(),
    }
}

/// Medians of one run.
struct Row {
    upload_kb: f64,
    writes: f64,
    edit_ms: f64,
    record_ms: f64,
    draws: f64,
    gpu_ms: f64,
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    values[(values.len() - 1) / 2]
}

async fn measure(
    platform: &dyn Platform,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (case, path, count, frames): (Case, Path, usize, u32),
) -> Result<Row, String> {
    let error = |e: sgl_3d::SceneError| e.to_string();
    let mut scene = Scene::with_options(
        device,
        queue,
        SceneOptions {
            ray_data: RayData::Disabled,
        },
    );
    let ground_model = scene
        .add_asset(device, queue, ground())
        .map_err(error)?
        .model;
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                model: ground_model,
                pose: Mat4::IDENTITY,
                visible: true,
                capture_visible: true,
            },
            Mobility::Static,
        )
        .map_err(error)?;
    // The content: a sprite batch, or a model with the image's material
    // that the game rebuilds.
    let mut batch = None;
    let mut model = None;
    match path {
        Path::Sprites => {
            let image = scene
                .add_sprite_image(device, queue, &image(case))
                .map_err(error)?;
            let id = scene
                .add_sprite_batch(&SpriteBatch {
                    facing: match case {
                        Case::Particles => SpriteFacing::Camera,
                        Case::Foliage => SpriteFacing::Axis(Vec3::Y),
                    },
                    blend: match case {
                        Case::Particles => SpriteBlend::Blended,
                        Case::Foliage => SpriteBlend::Masked { cutoff: 0.5 },
                    },
                    ..SpriteBatch::new(image)
                })
                .map_err(error)?;
            batch = Some(id);
        }
        Path::Baseline => {}
        Path::Quads => {
            let mut asset = ground();
            asset.images = vec![image(case)];
            asset.materials[0] = Material {
                base: [1.; 4],
                unlit: true,
                double_sided: true,
                base_texture: Some(MaterialTexture::new(0)),
                alpha: match case {
                    Case::Particles => AlphaMode::Blend {
                        receives_screen_space_reflections: false,
                        keeps_specular: false,
                    },
                    Case::Foliage => AlphaMode::Mask { cutoff: 0.5 },
                },
                ..Default::default()
            };
            let ids = scene.add_asset(device, queue, asset).map_err(error)?;
            scene
                .add_instance(
                    device,
                    queue,
                    InstanceState {
                        model: ids.model,
                        pose: Mat4::IDENTITY,
                        visible: true,
                        capture_visible: true,
                    },
                    Mobility::Moving,
                )
                .map_err(error)?;
            model = Some((ids.model, ids.materials[0]));
        }
    }
    let settings = Settings {
        preset: RenderPreset::High,
        atmosphere: false,
        ..Settings::default()
    };
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sprite cost output"),
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
    let output = output.create_view(&Default::default());
    let mut renderer = Renderer::new(device);
    let mut view = View::new(
        &renderer,
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        1.,
        &settings,
    )
    .map_err(|e| format!("{e:?}"))?;
    let mut timing = GpuTiming::new(device, queue);
    let warmup = 10;
    let (mut upload_kb, mut writes, mut edit_ms, mut record_ms, mut draws, mut gpu_ms) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for frame in 0..warmup + frames + 4 {
        let angle = frame as f32 * 0.01;
        let eye = Vec3::new(angle.cos() * 9., 3., angle.sin() * 9.);
        let view_matrix = camera::rh::view::look_at_mat4(eye, Vec3::new(0., 1., 0.), Vec3::Y);
        let mut input = FrameInput::new(Camera {
            view: view_matrix,
            projection: sgl_3d::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1),
            eye,
        });
        input.camera_cut = frame == 0;
        input.elapsed_seconds = f64::from(frame) / 60.;
        if let Some(timing) = &mut timing {
            for done in timing.begin_frame(device, queue) {
                if done.frame > u64::from(warmup) && gpu_ms.len() < frames as usize {
                    gpu_ms.push(done.total_ms);
                }
            }
        }
        let measured = frame >= warmup && frame < warmup + frames;
        let counted = sgl_3d::diagnostics::counters();
        let started = platform.now();
        // The game's edit: the case's sprites this frame, as a batch or as
        // quads built for this camera.
        match (path, batch, model) {
            (Path::Sprites, Some(id), _) => {
                // Foliage never moves: set once.
                if case == Case::Particles || frame == 0 {
                    let placed = sprites(case, count, frame);
                    scene
                        .set_sprites(device, queue, id, &placed)
                        .map_err(error)?;
                }
            }
            (Path::Quads, _, Some((id, material))) => {
                let placed = sprites(case, count, frame);
                let mesh = quads(case, &placed, &view_matrix, material);
                let prepared = PreparedModel::with_options(
                    vec![mesh],
                    PreparedModelOptions {
                        ray_data: RayData::Disabled,
                    },
                )
                .map_err(error)?;
                scene
                    .set_model(device, queue, id, prepared)
                    .map_err(error)?;
            }
            _ => {}
        }
        let edited = platform.now();
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &mut [ViewFrame {
                view: &mut view,
                input: &input,
                settings: &settings,
                output: ViewOutput::Texture(&output),
                timing: timing.as_ref(),
            }],
        );
        let commands = encoder.finish();
        let recorded = platform.now();
        let cost = sgl_3d::diagnostics::counters().since(&counted);
        queue.submit([commands]);
        if let Some(timing) = &mut timing {
            timing.submitted(queue);
        }
        renderer.finish_frame(&mut scene);
        platform.completed(device, queue).await;
        if measured {
            upload_kb.push(cost.uploaded_bytes() as f64 / 1024.);
            writes.push(cost.uploads.iter().map(|site| site.writes).sum::<u64>() as f64);
            edit_ms.push(edited - started);
            record_ms.push(recorded - edited);
            if let Some(stats) = view.geometry_stats(device) {
                draws.push(stats.total().0 as f64);
            }
        }
    }
    Ok(Row {
        upload_kb: median(&mut upload_kb),
        writes: median(&mut writes),
        edit_ms: median(&mut edit_ms),
        record_ms: median(&mut record_ms),
        draws: median(&mut draws),
        gpu_ms: median(&mut gpu_ms),
    })
}

/// The comparison of `count` sprites a case over `frames` frames, as a
/// table.
pub async fn report(
    platform: &dyn Platform,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    count: usize,
    frames: u32,
) -> Result<String, String> {
    let mut out = format!(
        "{count} sprites, {}x{}, High preset, medians over {frames} frames; a sprite batch \
         adds one instanced draw a pass (masked: depth and colour) beside the geometry draws\n\
         case       path      upload KiB  writes  edit ms  record ms  geometry draws  GPU frame ms\n",
        SIZE[0], SIZE[1]
    );
    for case in [Case::Particles, Case::Foliage] {
        for path in [Path::Baseline, Path::Sprites, Path::Quads] {
            let row = measure(platform, device, queue, (case, path, count, frames)).await?;
            let _ = writeln!(
                out,
                "{:<10} {:<9} {:>10.1} {:>7.0} {:>8.3} {:>10.3} {:>15.0} {:>13.3}",
                format!("{case:?}"),
                format!("{path:?}"),
                row.upload_kb,
                row.writes,
                row.edit_ms,
                row.record_ms,
                row.draws,
                row.gpu_ms
            );
        }
    }
    Ok(out)
}
