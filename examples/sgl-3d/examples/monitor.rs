//! An in-world monitor: a screen in the main view shows what a second
//! camera sees behind the player, live. The scene owns a render texture
//! (`Scene::add_render_texture`); the feed view draws into it
//! (`ViewOutput::RenderTexture`) and the screen's material names it as its
//! emission map (`SurfaceMaterial::emission_render_texture`). Both views
//! are listed in one `Renderer::render`, the main view first, its primary:
//! the renderer draws the feed before the main view, so the screen shows
//! this frame, and leaves the feed out of the frames in which no view
//! shows the screen.
//!
//! Run with `cargo run -p sgl-3d-examples --example monitor -- target/monitor.png`.
//! Optional `--frames N` sets how many frames run. `--measure` (use
//! `--release`) prints, where the device has timestamp queries, the timed
//! passes and GPU time a frame takes with the screen showing the feed,
//! with the same two views drawing into separate textures of the game's,
//! with the screen out of the main view, and with the main view alone;
//! `--passes` adds each arrangement's GPU time by pass group.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::settings::{RenderPreset, Settings};
use sgl_3d::timing::{FrameTime, GpuTiming};
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceId,
    InstanceState, MaterialId, Mobility, RenderTexture, RenderTextureId, Renderer, Scene,
    SurfaceMaterial, View, ViewFrame, ViewOutput,
    asset::{Asset, CpuMesh, Image, Material, MaterialTexture, Sampler, TextureWrap, Vertex},
};
use std::{error::Error, path::PathBuf};

const SIZE: [u32; 2] = [1280, 720];
/// The render texture's size, and so the feed view's.
const FEED: [u32; 2] = [512, 288];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The screen's centre and its size in metres; it faces -Z, the player.
const SCREEN: Vec3 = Vec3::new(0., 1.5, -2.);
const SCREEN_SIZE: [f32; 2] = [2.4, 1.35];
/// The frames `--measure` renders before those it counts, and those it
/// counts, for each arrangement.
const WARM_UP: u32 = 30;
const MEASURED: u32 = 240;

fn vertex(position: Vec3, normal: Vec3, uv: [f32; 2], tangent: Vec3) -> Vertex {
    Vertex {
        position: position.to_array(),
        normal: normal.to_array(),
        uv,
        color: [1.; 4],
        tangent: [tangent.x, tangent.y, tangent.z, 1.],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
    }
}

/// An axis-aligned box at `center` of `size` in material `material`.
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
            mesh.vertices.push(vertex(
                center + (normal + u * s + v * t) * half,
                normal,
                [(s + 1.) * 0.5, (t + 1.) * 0.5],
                u,
            ));
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

/// The screen: a quad facing -Z whose UV runs from its top left corner, as
/// the player sees it, to its bottom right, as a picture's does.
fn screen(material: usize) -> CpuMesh {
    let [half_width, half_height] = SCREEN_SIZE.map(|side| side * 0.5);
    // Looking along +Z, the player's right is -X.
    let corner = |right: f32, up: f32| {
        vertex(
            SCREEN + Vec3::new(-right * half_width, up * half_height, 0.),
            -Vec3::Z,
            [(right + 1.) * 0.5, (1. - up) * 0.5],
            -Vec3::X,
        )
    };
    CpuMesh {
        vertices: vec![
            corner(-1., -1.),
            corner(1., -1.),
            corner(1., 1.),
            corner(-1., 1.),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

fn material(base: [f32; 3], roughness: f32) -> Material {
    Material {
        name: "monitor example surface".into(),
        base: [base[0], base[1], base[2], 1.],
        roughness,
        metallic: 0.,
        ..Default::default()
    }
}

/// A camera at `eye` looking at `target`, for an output of `size`.
fn camera(eye: Vec3, target: Vec3, size: [u32; 2]) -> Camera {
    Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: sgl_3d::perspective(55f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
    }
}

fn texture(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("monitor example output"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let stride = (texture.width() * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("monitor example readback"),
        size: u64::from(stride * texture.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let mapped = slice.get_mapped_range().unwrap();
    Ok(mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| &row[..texture.width() as usize * 4])
        .copied()
        .collect())
}

/// The example's scene: a ground, pillars and a cart behind the player, and
/// before the player a screen on a stand. Returns the cart and the
/// screen's material.
fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
) -> Result<(InstanceId, InstanceState, MaterialId), Box<dyn Error>> {
    let mut add = |meshes: Vec<CpuMesh>, materials: Vec<Material>, mobility| {
        let asset = Asset {
            meshes,
            materials,
            images: vec![Image::Rgba8(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([255; 4]),
            ))],
            rig: Default::default(),
            ignored: Vec::new(),
            lights: Default::default(),
        };
        let ids = scene.add_asset(device, queue, asset)?;
        let state = InstanceState::new(ids.model);
        let instance = scene.add_instance(device, queue, state, mobility)?;
        Ok::<_, Box<dyn Error>>((instance, state, ids.materials))
    };
    add(
        vec![cuboid(
            Vec3::new(0., -0.1, -8.),
            Vec3::new(30., 0.2, 40.),
            0,
        )],
        vec![material([0.35, 0.37, 0.4], 0.8)],
        Mobility::Static,
    )?;
    add(
        (0..6)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / 6.;
                cuboid(
                    Vec3::new(4. * angle.cos(), 1., -14. + 4. * angle.sin()),
                    Vec3::new(0.6, 2., 0.6),
                    i % 2,
                )
            })
            .collect(),
        vec![
            material([0.8, 0.3, 0.2], 0.5),
            material([0.2, 0.5, 0.8], 0.4),
        ],
        Mobility::Static,
    )?;
    let (cart, cart_state, _) = add(
        vec![cuboid(Vec3::new(0., 0.4, 0.), Vec3::new(0.8, 0.8, 1.4), 0)],
        vec![material([0.9, 0.8, 0.2], 0.3)],
        Mobility::Moving,
    )?;
    // The screen emits the picture at this brightness; its black, glossy
    // base reflects the room as a real screen's glass does. The picture
    // takes its emission map's place (a white texel, which rays see) and is
    // sampled with that map's sampler: clamped, so its opposite edges do not
    // blend along the screen's border.
    let glass = Material {
        emissive: [1.5; 3],
        emissive_texture: Some(MaterialTexture {
            image: 0,
            sampler: Sampler {
                wrap: [TextureWrap::ClampToEdge; 2],
                ..Sampler::default()
            },
        }),
        ..material([0., 0., 0.], 0.15)
    };
    let (_, _, materials) = add(
        vec![
            screen(0),
            cuboid(
                SCREEN + Vec3::new(0., 0., 0.06),
                Vec3::new(SCREEN_SIZE[0] + 0.1, SCREEN_SIZE[1] + 0.1, 0.1),
                1,
            ),
            cuboid(Vec3::new(0., 0.4, -1.9), Vec3::new(0.2, 0.8, 0.2), 1),
        ],
        vec![glass, material([0.05, 0.05, 0.06], 0.6)],
        Mobility::Static,
    )?;
    Ok((cart, cart_state, materials[0]))
}

/// What a frame lists and shows.
#[derive(Clone, Copy, PartialEq)]
enum Arrangement {
    /// The feed draws the render texture the screen shows.
    RenderTexture,
    /// The same two views, the feed into a texture of the game's that no
    /// surface shows.
    SeparateViews,
    /// Both listed, the main view turned away from the screen.
    ScreenOutOfView,
    /// The main view alone, looking at the screen.
    MainAlone,
    /// The main view alone, turned away.
    MainAloneTurned,
}

impl Arrangement {
    fn name(self) -> &'static str {
        match self {
            Self::RenderTexture => "render texture",
            Self::SeparateViews => "separate views",
            Self::ScreenOutOfView => "screen out of view",
            Self::MainAlone => "main view alone",
            Self::MainAloneTurned => "main view alone, turned",
        }
    }

    fn lists_feed(self) -> bool {
        !matches!(self, Self::MainAlone | Self::MainAloneTurned)
    }

    fn turned(self) -> bool {
        matches!(self, Self::ScreenOutOfView | Self::MainAloneTurned)
    }
}

/// Everything a frame needs.
struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    main_view: View,
    feed_view: View,
    main_settings: Settings,
    feed_settings: Settings,
    main_output: wgpu::Texture,
    feed_output: wgpu::Texture,
    picture: RenderTextureId,
    screen: MaterialId,
    cart: (InstanceId, InstanceState),
    timing: Option<GpuTiming>,
    frame: u32,
}

impl Stage {
    /// The screen shows the render texture, or nothing.
    fn show(&mut self, shown: bool) -> Result<(), Box<dyn Error>> {
        let values = self.scene.material(self.screen)?;
        self.scene.set_material(
            &self.queue,
            self.screen,
            SurfaceMaterial {
                emission_render_texture: shown.then_some(self.picture),
                ..values
            },
        )?;
        Ok(())
    }

    /// Renders, submits and finishes one frame as `arrangement` lists it,
    /// and returns the GPU timings that completed meanwhile.
    fn frame(&mut self, arrangement: Arrangement) -> Result<Vec<FrameTime>, Box<dyn Error>> {
        let frame = self.frame;
        self.frame += 1;
        // The cart drives a slow circle behind the player.
        let angle = frame as f32 * 0.02;
        let position = Vec3::new(2.5 * angle.cos(), 0., -14. + 2.5 * angle.sin());
        self.cart.1.pose = Mat4::from_translation(position) * Mat4::from_rotation_y(-angle);
        self.scene
            .set_instance(&self.queue, self.cart.0, self.cart.1)?;
        let eye = Vec3::new(0., 1.6, -6.);
        let ahead = if arrangement.turned() {
            eye + Vec3::X
        } else {
            SCREEN
        };
        let mut main = FrameInput::new(camera(eye, ahead, SIZE));
        // The feed looks at the cart from above the player's shoulder.
        let mut feed = FrameInput::new(camera(
            Vec3::new(0., 3.5, -7.),
            position + Vec3::Y * 0.4,
            FEED,
        ));
        for input in [&mut main, &mut feed] {
            input.directional_lights[0] = Some(DirectionalLight {
                direction: Vec3::new(0.4, -1., 0.3),
                color: [1., 0.92, 0.8],
                illuminance: 3.,
                shadow: Some(DirectionalShadow {
                    distance: 30.,
                    cascades: 2,
                }),
                ..Default::default()
            });
            input.hemisphere_light = HemisphereLight {
                sky_color: [0.3, 0.4, 0.6],
                ground_color: [0.1, 0.08, 0.06],
                intensity: 0.4,
            };
            input.elapsed_seconds = f64::from(frame) / 60.;
            input.frame_time_ms = 1000. / 60.;
        }
        let timings = match &mut self.timing {
            Some(timing) => timing.begin_frame(&self.device, &self.queue).collect(),
            None => Vec::new(),
        };
        // Cheap when nothing changed. The feed view has the render
        // texture's size.
        self.main_view
            .resize(&self.device, SIZE, 1., &self.main_settings);
        self.feed_view
            .resize(&self.device, FEED, 1., &self.feed_settings);
        let [main_target, feed_target] =
            [&self.main_output, &self.feed_output].map(|t| t.create_view(&Default::default()));
        let mut views = vec![ViewFrame {
            view: &mut self.main_view,
            input: &main,
            settings: &self.main_settings,
            output: ViewOutput::Texture(&main_target),
            timing: self.timing.as_ref(),
        }];
        if arrangement.lists_feed() {
            views.push(ViewFrame {
                view: &mut self.feed_view,
                input: &feed,
                settings: &self.feed_settings,
                output: match arrangement {
                    Arrangement::SeparateViews => ViewOutput::Texture(&feed_target),
                    _ => ViewOutput::RenderTexture(self.picture),
                },
                timing: self.timing.as_ref(),
            });
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &mut views,
        );
        let submission = self.queue.submit([encoder.finish()]);
        if let Some(timing) = &mut self.timing {
            timing.submitted(&self.queue);
        }
        self.renderer.finish_frame(&mut self.scene);
        if self.timing.is_some() {
            // One frame on the GPU at a time, so a frame's time is its own.
            self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })?;
        }
        Ok(timings)
    }
}

/// The value `share` of the way through `sorted`.
fn quantile(sorted: &[f64], share: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * share).round() as usize]
}

/// Renders each arrangement and prints its frames' timed passes and GPU
/// time.
fn measure(stage: &mut Stage, by_pass: bool) -> Result<(), Box<dyn Error>> {
    if stage.timing.is_none() {
        println!("no GPU timestamps: the device has no timestamp queries");
        return Ok(());
    }
    println!(
        "main view {}x{}, feed and render texture {}x{}, {MEASURED} frames each after {WARM_UP}",
        SIZE[0], SIZE[1], FEED[0], FEED[1]
    );
    println!(
        "{:<26}{:>7}{:>8}{:>12}{:>9}",
        "arrangement", "listed", "passes", "median ms", "p95 ms"
    );
    for arrangement in [
        Arrangement::MainAlone,
        Arrangement::RenderTexture,
        Arrangement::SeparateViews,
        Arrangement::MainAloneTurned,
        Arrangement::ScreenOutOfView,
    ] {
        stage.show(arrangement != Arrangement::SeparateViews)?;
        let mut times = Vec::new();
        let mut passes = Vec::new();
        // Each pass group's passes a frame and its times, in first-use order.
        let mut groups: Vec<(&'static str, u32, Vec<f64>)> = Vec::new();
        // The timings arrive a few frames late: keep those of the frames
        // this arrangement rendered after its warm-up.
        let first = stage.frame + WARM_UP;
        let last = first + MEASURED;
        for _ in 0..WARM_UP + MEASURED + 8 {
            let arrangement_frame = stage.frame;
            for timed in stage.frame(arrangement)? {
                // `GpuTiming` counts frames from 1, one per `begin_frame`.
                let frame = timed.frame as u32 - 1;
                if (first..last).contains(&frame) && frame < arrangement_frame {
                    times.push(timed.total_ms);
                    passes.push(timed.passes.iter().map(|group| group.passes).sum::<u32>());
                    for group in &timed.passes {
                        match groups.iter_mut().find(|(name, ..)| *name == group.name) {
                            Some((_, _, times)) => times.push(group.ms),
                            None => groups.push((group.name, group.passes, vec![group.ms])),
                        }
                    }
                }
            }
        }
        times.sort_by(f64::total_cmp);
        passes.sort_unstable();
        if times.is_empty() {
            println!("{:<26} no completed timings", arrangement.name());
            continue;
        }
        println!(
            "{:<26}{:>7}{:>8}{:>12.3}{:>9.3}",
            arrangement.name(),
            if arrangement.lists_feed() { 2 } else { 1 },
            // Every frame of an arrangement makes the same passes.
            format!("{}", passes[passes.len() / 2]),
            quantile(&times, 0.5),
            quantile(&times, 0.95),
        );
        for (name, passes, times) in groups.iter_mut().filter(|_| by_pass) {
            times.sort_by(f64::total_cmp);
            println!("    {name:<30}{passes:>3}{:>12.3}", quantile(times, 0.5));
        }
    }
    Ok(())
}

async fn run(output: PathBuf, frames: u32, measuring: Option<bool>) -> Result<(), Box<dyn Error>> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await?;
    let timestamps = if measuring.is_some() {
        adapter.features() & wgpu::Features::TIMESTAMP_QUERY
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: sgl_3d::graphics_device::features(&adapter) | timestamps,
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await?;
    let mut scene = Scene::new(&device, &queue);
    let (cart, cart_state, screen) = build(&device, &queue, &mut scene)?;
    // The picture the feed draws and the screen shows.
    let description = RenderTexture::new(FEED);
    let picture = scene.add_render_texture(&device, description)?;
    let renderer = Renderer::new(&device);
    let main_settings = Settings::default();
    // A security camera's picture is small and cheap: the Low preset.
    let feed_settings = Settings {
        preset: RenderPreset::Low,
        ..Settings::default()
    };
    let main_view = View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &main_settings)?;
    // A view that draws a render texture has its format and size.
    let feed_view = View::new(
        &renderer,
        &device,
        &queue,
        description.format(),
        FEED,
        1.,
        &feed_settings,
    )?;
    let mut stage = Stage {
        timing: GpuTiming::new(&device, &queue),
        main_output: texture(&device, SIZE),
        feed_output: texture(&device, FEED),
        device,
        queue,
        scene,
        renderer,
        main_view,
        feed_view,
        main_settings,
        feed_settings,
        picture,
        screen,
        cart: (cart, cart_state),
        frame: 0,
    };
    if let Some(by_pass) = measuring {
        return measure(&mut stage, by_pass);
    }
    stage.show(true)?;
    for _ in 0..frames {
        stage.frame(Arrangement::RenderTexture)?;
    }
    let pixels = read_pixels(&stage.device, &stage.queue, &stage.main_output)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(&output, &pixels, SIZE[0], SIZE[1], image::ColorType::Rgba8)?;
    println!(
        "Rendered {frames} frames of a main view and the feed its screen shows; {}",
        output.display()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output: PathBuf = "target/sgl-3d-monitor.png".into();
    let mut frames = 60;
    // Whether to measure, and then whether by pass group too.
    let mut measuring = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--measure" => measuring = measuring.or(Some(false)),
            "--passes" => measuring = Some(true),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}").into()),
            _ => output = arg.into(),
        }
    }
    pollster::block_on(run(output, frames.max(1), measuring))
}
