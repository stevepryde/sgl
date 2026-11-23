//! Planar reflections: a wall mirror and a pond that show the scene
//! mirrored in them, a cart driving behind the player included. Each is a
//! planar reflector of the scene (`Scene::add_planar_reflector`) that its
//! surface's material names (`SurfaceMaterial::planar_reflector`): the
//! renderer does the rest. A view that shows a reflector renders the scene
//! again from the mirrored eye, over the reflector's bounds on screen, at
//! the level `Settings::planar_reflections` chooses, and only in the frames
//! the reflector is in that view. The pond's moving normal map bends its
//! picture.
//!
//! Run with `cargo run -p sgl-3d-examples --example planar_reflections -- target/planar.png`.
//! Optional `--frames N` sets how many frames run and `--level
//! off|quarter|half|full` the pictures' level (`half`, the default);
//! `--fog` fills the scene with a thin uniform medium, which the mirror and
//! the pond show as the fog along the whole reflected path.
//! `--measure` (use
//! `--release`) prints, where the device has timestamp queries, the timed
//! passes and GPU time a frame takes at each level and with the reflectors
//! out of view, and what each reflected view draws; `--passes` adds each
//! arrangement's GPU time by pass group.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::settings::{PlanarReflections, Settings};
use sgl_3d::timing::{FrameTime, GpuTiming};
use sgl_3d::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, Fog, FrameInput, HemisphereLight,
    InstanceId, InstanceState, MaterialId, Mobility, NormalLayer, PlanarReflector,
    PlanarReflectorId, Renderer, Scene, SurfaceMaterial, View, ViewFrame, ViewOutput,
    asset::{Asset, CpuMesh, Image, Material, MaterialTexture, Vertex},
};
use std::{error::Error, path::PathBuf};

const SIZE: [u32; 2] = [1280, 720];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The mirror's centre and its size in metres; it faces +Z, the player.
const MIRROR: Vec3 = Vec3::new(-2.5, 1.6, -6.);
const MIRROR_SIZE: [f32; 2] = [3., 2.2];
/// The pond's centre, its water's height and its size in metres.
const POND: Vec3 = Vec3::new(3.5, 0.02, -4.);
const POND_SIZE: [f32; 2] = [6., 8.];
/// The frames `--measure` renders before those it counts, and those it
/// counts, for each arrangement.
const WARM_UP: u32 = 30;
const MEASURED: u32 = 240;

fn vertex(position: Vec3, normal: Vec3, uv: [f32; 2], tangent: Vec3) -> Vertex {
    Vertex {
        uv1: [0.0; 2],
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

/// A one-sided rectangle at `center` facing `normal`, `size` metres along
/// `right` and its other side, its UVs in metres over `tile`.
fn rectangle(center: Vec3, normal: Vec3, right: Vec3, size: [f32; 2], tile: f32) -> CpuMesh {
    let up = normal.cross(right);
    let corner = |s: f32, t: f32| {
        vertex(
            center + right * s * size[0] * 0.5 + up * t * size[1] * 0.5,
            normal,
            [s * size[0] * 0.5 / tile, t * size[1] * 0.5 / tile],
            right,
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
        material: 0,
        deformation: Default::default(),
    }
}

fn material(base: [f32; 3], roughness: f32) -> Material {
    Material {
        name: "planar reflections example surface".into(),
        base: [base[0], base[1], base[2], 1.],
        roughness,
        metallic: 0.,
        ..Default::default()
    }
}

/// A tangent-space normal map of gentle ripples: two crossed sines, whose
/// slopes are their derivatives, repeating once across the image.
fn ripples() -> Image {
    const TEXELS: u32 = 64;
    let image = image::RgbaImage::from_fn(TEXELS, TEXELS, |x, y| {
        let phase =
            |texel: u32, waves: f32| texel as f32 / TEXELS as f32 * waves * std::f32::consts::TAU;
        let slope = Vec3::new(
            0.12 * phase(x, 2.).cos() + 0.06 * phase(y, 3.).sin(),
            0.12 * phase(y, 2.).cos() + 0.06 * phase(x, 3.).sin(),
            1.,
        )
        .normalize();
        let encode = |value: f32| ((value * 0.5 + 0.5) * 255.).round() as u8;
        image::Rgba([encode(slope.x), encode(slope.y), encode(slope.z), 255])
    });
    Image::Rgba8(image)
}

/// A camera at `eye` looking at `target`.
fn camera(eye: Vec3, target: Vec3) -> Camera {
    Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: sgl_3d::perspective(60f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
    }
}

fn texture(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("planar reflections example output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
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
        label: Some("planar reflections example readback"),
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

/// The reflectors the example's scene holds, each with its name.
struct Reflectors {
    mirror: PlanarReflectorId,
    pond: PlanarReflectorId,
}

/// The example's scene: a ground with a pond in it, pillars and a cart
/// behind the player, and before the player a framed mirror on a wall.
/// Returns the cart and the reflectors.
fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
) -> Result<(InstanceId, InstanceState, Reflectors), Box<dyn Error>> {
    let mut add = |meshes: Vec<CpuMesh>, materials: Vec<Material>, images: Vec<Image>, mobility| {
        let asset = Asset {
            meshes,
            materials,
            images,
            rig: Default::default(),
            ignored: Vec::new(),
            lights: Default::default(),
            instanced: Vec::new(),
        };
        let ids = scene.add_asset(device, queue, asset)?;
        let state = InstanceState::new(ids.model);
        let instance = scene.add_instance(device, queue, state, mobility)?;
        Ok::<_, Box<dyn Error>>((instance, state, ids.materials))
    };
    // The ground, with the pond's bed sunk into it.
    add(
        vec![
            cuboid(Vec3::new(-6., -0.1, -4.), Vec3::new(13., 0.2, 40.), 0),
            cuboid(Vec3::new(12.5, -0.1, -4.), Vec3::new(12., 0.2, 40.), 0),
            cuboid(Vec3::new(3.5, -0.1, 10.), Vec3::new(6., 0.2, 20.), 0),
            cuboid(Vec3::new(3.5, -0.1, -16.), Vec3::new(6., 0.2, 16.), 0),
            cuboid(Vec3::new(POND.x, -0.6, POND.z), Vec3::new(6., 0.2, 8.), 1),
        ],
        vec![
            material([0.35, 0.37, 0.4], 0.8),
            material([0.05, 0.08, 0.07], 0.9),
        ],
        Vec::new(),
        Mobility::Static,
    )?;
    add(
        (0..6)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / 6.;
                cuboid(
                    Vec3::new(4. * angle.cos(), 1., 6. + 4. * angle.sin()),
                    Vec3::new(0.6, 2., 0.6),
                    i % 2,
                )
            })
            .collect(),
        vec![
            material([0.8, 0.3, 0.2], 0.5),
            material([0.2, 0.5, 0.8], 0.4),
        ],
        Vec::new(),
        Mobility::Static,
    )?;
    let (cart, cart_state, _) = add(
        vec![cuboid(Vec3::new(0., 0.4, 0.), Vec3::new(0.8, 0.8, 1.4), 0)],
        vec![material([0.9, 0.8, 0.2], 0.3)],
        Vec::new(),
        Mobility::Moving,
    )?;
    // The wall and the mirror's frame, then the mirror: a one-sided metal
    // of roughness 0 in the reflector's plane.
    add(
        vec![
            cuboid(MIRROR + Vec3::new(0., 0., -0.3), Vec3::new(8., 3.4, 0.3), 0),
            cuboid(
                MIRROR + Vec3::new(0., 0., -0.1),
                Vec3::new(MIRROR_SIZE[0] + 0.2, MIRROR_SIZE[1] + 0.2, 0.16),
                1,
            ),
        ],
        vec![
            material([0.6, 0.58, 0.55], 0.9),
            material([0.25, 0.15, 0.08], 0.5),
        ],
        Vec::new(),
        Mobility::Static,
    )?;
    let (_, _, mirror) = add(
        vec![rectangle(MIRROR, Vec3::Z, Vec3::X, MIRROR_SIZE, 1.)],
        vec![Material {
            metallic: 1.,
            ..material([0.95, 0.95, 0.95], 0.)
        }],
        Vec::new(),
        Mobility::Static,
    )?;
    // The pond: an opaque dielectric, dark and smooth, whose normal map
    // moves in two layers.
    let (_, _, pond) = add(
        vec![rectangle(POND, Vec3::Y, Vec3::X, POND_SIZE, 2.)],
        vec![Material {
            ior: 1.33,
            normal_texture: Some(MaterialTexture::new(0)),
            normal_layers: Some([
                NormalLayer {
                    velocity: [0.03, 0.02],
                    scale: 1.,
                    strength: 0.6,
                },
                NormalLayer {
                    velocity: [-0.02, 0.035],
                    scale: 2.3,
                    strength: 0.4,
                },
            ]),
            ..material([0.01, 0.03, 0.04], 0.04)
        }],
        vec![ripples()],
        Mobility::Static,
    )?;
    // Each plane, and the material that lies in it.
    let mut reflect = |material: MaterialId, origin: Vec3, normal: Vec3| {
        let reflector = scene.add_planar_reflector(PlanarReflector::new(origin, normal))?;
        let values = scene.material(material)?;
        scene.set_material(
            queue,
            material,
            SurfaceMaterial {
                planar_reflector: Some(reflector),
                ..values
            },
        )?;
        Ok::<_, Box<dyn Error>>(reflector)
    };
    let reflectors = Reflectors {
        mirror: reflect(mirror[0], MIRROR, Vec3::Z)?,
        pond: reflect(pond[0], POND, Vec3::Y)?,
    };
    Ok((cart, cart_state, reflectors))
}

/// What a frame looks at and at which level.
#[derive(Clone, Copy, PartialEq)]
enum Arrangement {
    /// The mirror and the pond in view.
    Level(PlanarReflections),
    /// The player turned away from both.
    Turned,
}

impl Arrangement {
    fn name(self) -> &'static str {
        match self {
            Self::Level(PlanarReflections::Off) => "off",
            Self::Level(PlanarReflections::Quarter) => "quarter",
            Self::Level(PlanarReflections::Half) => "half",
            Self::Level(PlanarReflections::Full) => "full",
            Self::Turned => "reflectors out of view",
        }
    }
}

/// Everything a frame needs.
struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    view: View,
    settings: Settings,
    output: wgpu::Texture,
    reflectors: Reflectors,
    cart: (InstanceId, InstanceState),
    timing: Option<GpuTiming>,
    frame: u32,
    /// How long the last frame's `Renderer::render` took on the CPU.
    encoded: std::time::Duration,
    /// The scene holds a uniform medium (`--fog`).
    fog: bool,
}

impl Stage {
    /// Renders, submits and finishes one frame as `arrangement` says, and
    /// returns the GPU timings that completed meanwhile.
    fn frame(&mut self, arrangement: Arrangement) -> Result<Vec<FrameTime>, Box<dyn Error>> {
        let frame = self.frame;
        self.frame += 1;
        // The cart drives a slow circle behind the player.
        let angle = frame as f32 * 0.02;
        let position = Vec3::new(2.5 * angle.cos(), 0., 6. + 2.5 * angle.sin());
        self.cart.1.pose = Mat4::from_translation(position) * Mat4::from_rotation_y(-angle);
        self.scene
            .set_instance(&self.queue, self.cart.0, self.cart.1)?;
        let eye = Vec3::new(0.5, 1.7, 1.);
        let ahead = match arrangement {
            Arrangement::Turned => eye + Vec3::new(-1., 0., 0.4),
            Arrangement::Level(_) => Vec3::new(0.5, 0.9, -5.),
        };
        if let Arrangement::Level(level) = arrangement {
            self.settings.planar_reflections = level;
        }
        let mut input = FrameInput::new(camera(eye, ahead));
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(0.4, -1., -0.3),
            color: [1., 0.92, 0.8],
            illuminance: 3.,
            shadow: Some(DirectionalShadow {
                distance: 30.,
                cascades: 2,
            }),
            ..Default::default()
        });
        // A plain sky, which the pond and the mirror show beyond the scene.
        input.backdrop = Backdrop::Color([0.22, 0.4, 0.75]);
        if self.fog {
            input.atmosphere = true;
            input.fog = Fog {
                density: 0.03,
                ambient: 0.2,
                ..Fog::default()
            };
        }
        input.hemisphere_light = HemisphereLight {
            sky_color: [0.3, 0.4, 0.6],
            ground_color: [0.1, 0.08, 0.06],
            intensity: 0.4,
        };
        input.elapsed_seconds = f64::from(frame) / 60.;
        input.frame_time_ms = 1000. / 60.;
        let timings = match &mut self.timing {
            Some(timing) => timing.begin_frame(&self.device, &self.queue).collect(),
            None => Vec::new(),
        };
        self.view.resize(&self.device, SIZE, 1., &self.settings);
        let target = self.output.create_view(&Default::default());
        let mut views = [ViewFrame {
            view: &mut self.view,
            input: &input,
            settings: &self.settings,
            output: ViewOutput::Texture(&target),
            timing: self.timing.as_ref(),
        }];
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let started = std::time::Instant::now();
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &mut views,
        );
        self.encoded = started.elapsed();
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

    /// What the view's last frame showed: each reflector's name, its
    /// picture's size and what its reflected view drew.
    fn shown(&mut self) -> Vec<String> {
        let Reflectors { mirror, pond } = self.reflectors;
        self.view
            .planar_reflections(&self.device)
            .into_iter()
            .map(|stats| {
                let name = match stats.reflector {
                    id if id == mirror => "mirror",
                    id if id == pond => "pond",
                    _ => "reflector",
                };
                let drawn = stats.geometry.map_or_else(
                    || "no statistics yet".to_string(),
                    |geometry| {
                        let (statics, static_triangles) = geometry.static_instances;
                        let (moving, moving_triangles) = geometry.moving_instances;
                        format!(
                            "{} draws, {} triangles",
                            statics + moving,
                            static_triangles + moving_triangles
                        )
                    },
                );
                format!(
                    "{name}: {}x{} picture, {drawn}",
                    stats.size[0], stats.size[1]
                )
            })
            .collect()
    }
}

/// The value `share` of the way through `sorted`.
fn quantile(sorted: &[f64], share: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * share).round() as usize]
}

/// Renders each arrangement and prints its frames' timed passes and GPU
/// time, and what its reflected views drew.
fn measure(stage: &mut Stage, by_pass: bool) -> Result<(), Box<dyn Error>> {
    if stage.timing.is_none() {
        println!("no GPU timestamps: the device has no timestamp queries");
        return Ok(());
    }
    println!(
        "view {}x{}, {MEASURED} frames each after {WARM_UP}",
        SIZE[0], SIZE[1]
    );
    println!(
        "{:<26}{:>8}{:>12}{:>9}",
        "arrangement", "passes", "median ms", "p95 ms"
    );
    for arrangement in [
        Arrangement::Level(PlanarReflections::Off),
        Arrangement::Level(PlanarReflections::Quarter),
        Arrangement::Level(PlanarReflections::Half),
        Arrangement::Level(PlanarReflections::Full),
        Arrangement::Turned,
    ] {
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
            "{:<26}{:>8}{:>12.3}{:>9.3}",
            arrangement.name(),
            // Every frame of an arrangement makes the same passes.
            passes[passes.len() / 2],
            quantile(&times, 0.5),
            quantile(&times, 0.95),
        );
        for line in stage.shown() {
            println!("    {line}");
        }
        for (name, passes, times) in groups.iter_mut().filter(|_| by_pass) {
            times.sort_by(f64::total_cmp);
            println!("    {name:<30}{passes:>3}{:>12.3}", quantile(times, 0.5));
        }
    }
    // A reflected view is kept while its reflector is briefly out of view,
    // its picture's targets and shadow maps released: what
    // `Renderer::render` takes on the CPU in the frame both reflectors
    // return in, which makes those again, against the frames around it.
    let half = Arrangement::Level(PlanarReflections::Half);
    let mut steady = Vec::new();
    let mut returning = Vec::new();
    for _ in 0..8 {
        for _ in 0..4 {
            stage.frame(Arrangement::Turned)?;
        }
        stage.frame(half)?;
        returning.push(stage.encoded.as_secs_f64() * 1000.);
        for _ in 0..8 {
            stage.frame(half)?;
            steady.push(stage.encoded.as_secs_f64() * 1000.);
        }
    }
    steady.sort_by(f64::total_cmp);
    returning.sort_by(f64::total_cmp);
    println!(
        "render on the CPU, median ms: {:.3} with both reflectors in view, {:.3} in the frame they return to it",
        quantile(&steady, 0.5),
        quantile(&returning, 0.5),
    );
    Ok(())
}

async fn run(
    output: PathBuf,
    frames: u32,
    level: PlanarReflections,
    measuring: Option<bool>,
    fog: bool,
) -> Result<(), Box<dyn Error>> {
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
    let (cart, cart_state, reflectors) = build(&device, &queue, &mut scene)?;
    let renderer = Renderer::new(&device);
    // `Half` is the default level: a game sets nothing for a reflector to
    // show.
    let settings = Settings {
        planar_reflections: level,
        ..Settings::default()
    };
    let view = View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &settings)?;
    let mut stage = Stage {
        timing: GpuTiming::new(&device, &queue),
        output: texture(&device),
        device,
        queue,
        scene,
        renderer,
        view,
        settings,
        reflectors,
        cart: (cart, cart_state),
        frame: 0,
        encoded: Default::default(),
        fog,
    };
    if let Some(by_pass) = measuring {
        return measure(&mut stage, by_pass);
    }
    for _ in 0..frames {
        stage.frame(Arrangement::Level(level))?;
    }
    let pixels = read_pixels(&stage.device, &stage.queue, &stage.output)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(&output, &pixels, SIZE[0], SIZE[1], image::ColorType::Rgba8)?;
    println!(
        "Rendered {frames} frames of a view that shows a mirror and a pond; {}",
        output.display()
    );
    for line in stage.shown() {
        println!("{line}");
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output: PathBuf = "target/sgl-3d-planar-reflections.png".into();
    let mut frames = 60;
    let mut level = PlanarReflections::default();
    let mut fog = false;
    // Whether to measure, and then whether by pass group too.
    let mut measuring = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--level" => {
                level = match args.next().as_deref() {
                    Some("off") => PlanarReflections::Off,
                    Some("quarter") => PlanarReflections::Quarter,
                    Some("half") => PlanarReflections::Half,
                    Some("full") => PlanarReflections::Full,
                    _ => return Err("--level requires off, quarter, half or full".into()),
                }
            }
            "--measure" => measuring = measuring.or(Some(false)),
            "--fog" => fog = true,
            "--passes" => measuring = Some(true),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}").into()),
            _ => output = arg.into(),
        }
    }
    pollster::block_on(run(output, frames.max(1), level, measuring, fog))
}
