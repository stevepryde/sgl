//! Browser lane smoke test (testing.md 5): SGL3D on the browser's WebGPU.
//! Built for wasm32 as a `cdylib`, bound with `wasm-bindgen --target web`,
//! and driven by Playwright from `browser/sgl3d.test.ts` via
//! `bun scripts/tasks.ts check-browser`.
//!
//! It creates a device with the adapter's limits, builds a scene and renders
//! a few frames of it under several settings into an offscreen target. The
//! scene holds a textured ground; a box that shades part of it from a
//! shadowed sun; a masked grate and a blended pane that receives
//! screen-space reflections; a skinned and morphed box
//! with an ambient cube; a box lit by a static irradiance atlas; point, spot
//! and rectangle lights; a decal; a baked specular probe; a dynamic GI volume
//! over the ground, part of it covered by an irradiance volume written by
//! region; glow, heat shimmer and mist; a fog volume; and an
//! environment. Where the device has BC, the
//! grate's image, the probe and the atlas are block-compressed, as a game
//! ships them. Each
//! configuration reports `ok` or `FAIL`: WebGPU validation, out-of-memory and
//! internal errors fail it, and so does a ground pixel in the box's shadow
//! that is not clearly darker than one the sun reaches. That expectation comes
//! from the geometry alone: the box stands between the sun and the shadowed
//! point, and nothing stands between it and the lit one.
#![cfg(target_arch = "wasm32")]
#![allow(clippy::future_not_send)]

use sgl_3d::glam::camera;
use sgl_3d::{
    AlphaMode, BakedSpecularProbe, Camera, Decal, DirectionalLight, DirectionalShadow,
    DynamicGiVolume, EnvironmentId, Fog, FogVolume, FrameInput, HemisphereLight, InstanceId,
    InstanceState, IrradianceCell, IrradianceVolume, Light, LightShape, Mist, Mobility,
    PreparedIrradianceRegion, Renderer, Scene, SpecularProbeBox, SpecularProbeRadiance,
    SpecularProbeTexels,
    asset::{Asset, CompressedImage, CpuMesh, Image, Material, Vertex},
    deformation::{
        Influence, Joint, MeshDeformation, MorphDelta, MorphTarget, MorphWeight, Node, Rig,
    },
    effects::Glow,
    environment::{EnvironmentMap, PmremAtlas},
    glam::{Mat4, Quat, Vec3, Vec4},
    heat_distortion::HeatDistortion,
    settings::{
        AmbientOcclusionQuality, Antialiasing, DynamicGiQuality, MotionBlur, ReflectionMethod,
        RenderPreset, ScreenSpaceReflections, Settings,
    },
    static_lighting::{AmbientCube, CompressedIrradianceAtlas, IrradianceAtlas},
    timing::GpuTiming,
};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

const SIZE: [u32; 2] = [256, 192];
const FRAMES: u32 = 4;
/// The sun shines down and along +X, so the box's shadow falls on +X of it.
const SUN: Vec3 = Vec3::new(1., -1., 0.);
/// On the ground's top face (y = 0): back towards the sun it passes through
/// the box (x and z within ±0.5, y within 0..2) at y = 1.5.
const SHADOWED: Vec3 = Vec3::new(1.5, 0., 0.);
/// Beside it, beyond the box's depth, so nothing stands towards the sun.
const LIT: Vec3 = Vec3::new(1.5, 0., 2.);

fn material(base: [f32; 4], metallic: f32, roughness: f32) -> Material {
    Material {
        name: "smoke surface".into(),
        base,
        metallic,
        roughness,
        coat_roughness: 0.3,
        ..Default::default()
    }
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

/// A double-sided quad at `center` spanning `u` and `v` (half extents).
fn panel(center: Vec3, u: Vec3, v: Vec3, material: usize) -> CpuMesh {
    let normal = u.cross(v).normalize();
    CpuMesh {
        vertices: [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]]
            .map(|[s, t]| Vertex {
                tangent: [u.x, u.y, u.z, 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (center + u * s + v * t).to_array(),
                normal: normal.to_array(),
                uv: [(s + 1.) * 0.5, (1. - t) * 0.5],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

/// The textured ground and the box that shadows it. The ground's base
/// texture is a uniform light grey, so its texels do not vary between the
/// measured points.
fn world() -> Asset {
    let mut ground = material([1.; 4], 0., 0.8);
    ground.base_texture = Some(0);
    Asset {
        meshes: vec![
            cuboid(Vec3::new(0., -0.2, 0.), Vec3::new(14., 0.4, 14.), 0),
            cuboid(Vec3::new(0., 1., 0.), Vec3::new(1., 2., 1.), 1),
        ],
        materials: vec![ground, material([0.6, 0.3, 0.2, 1.], 0., 0.5)],
        images: vec![Image::Rgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([200, 200, 200, 255]),
        ))],
        rig: Default::default(),
    }
}

/// A masked grate (the BC7 `grate.ktx2` where the device has BC, else an
/// RGBA8 lattice) and a blended pane that receives screen-space reflections,
/// both behind the box from the camera.
fn alpha_content(bc: bool) -> Result<Asset, String> {
    let image = if bc {
        Image::Compressed(
            CompressedImage::from_ktx2(include_bytes!("grate.ktx2"))
                .map_err(|e| format!("grate.ktx2: {e}"))?,
        )
    } else {
        Image::Rgba8(image::RgbaImage::from_fn(32, 32, |x, y| {
            let bar = x % 8 < 2 || y % 8 < 2;
            image::Rgba([150, 150, 160, if bar { 255 } else { 0 }])
        }))
    };
    let mut bars = material([0.6, 0.6, 0.62, 1.], 0.8, 0.35);
    bars.base_texture = Some(0);
    bars.double_sided = true;
    bars.alpha = AlphaMode::Mask { cutoff: 0.5 };
    let mut glass = material([0.2, 0.5, 1., 0.4], 0., 0.05);
    glass.double_sided = true;
    glass.alpha = AlphaMode::Blend {
        receives_screen_space_reflections: true,
    };
    Ok(Asset {
        meshes: vec![
            panel(Vec3::new(-3., 1.5, -3.), Vec3::X, Vec3::Y, 0),
            panel(Vec3::new(-1., 1., -3.5), Vec3::X * 0.8, Vec3::Y * 0.8, 1),
        ],
        materials: vec![bars, glass],
        images: vec![image],
        rig: Default::default(),
    })
}

/// A BC6H block in mode 11 (one region, 10-bit endpoints) with both
/// endpoints `endpoint` and every index 0: per the D3D11 format each texel
/// decodes to ((endpoint << 16) + 0x8000) >> 10, finished as (x * 31) >> 6,
/// a binary16 value (400 gives 0x307F, about 0.14).
fn bc6h_block(endpoint: u16) -> [u8; 16] {
    let mut block = 0b00011u128;
    for channel in 0..3 {
        block |= u128::from(endpoint) << (5 + 10 * channel);
        block |= u128::from(endpoint) << (35 + 10 * channel);
    }
    block.to_le_bytes()
}

/// A dim, uniform probe whose influence covers the scene, with a proxy box
/// for parallax: BC6H blocks where the device has BC, else RGBA16F.
fn probe(bc: bool) -> BakedSpecularProbe {
    const FACE: u32 = 64;
    let texels = if bc {
        let blocks: usize = (0..7)
            .map(|level| (FACE >> level).div_ceil(4).pow(2) as usize * 6)
            .sum();
        SpecularProbeTexels::Bc6hUfloat(bc6h_block(400).repeat(blocks))
    } else {
        let texels: usize = (0..7)
            .map(|level| (FACE >> level).pow(2) as usize * 6)
            .sum();
        SpecularProbeTexels::Rgba16Float([0x307f, 0x307f, 0x307f, 0x3c00].repeat(texels))
    };
    BakedSpecularProbe {
        center: Vec3::new(0., 1., 0.),
        world_to_local: Mat4::IDENTITY,
        influence: SpecularProbeBox {
            min: Vec3::new(-7., -1., -7.),
            max: Vec3::new(7., 4., 7.),
        },
        blend: Vec3::ONE,
        proxy: Some(SpecularProbeBox {
            min: Vec3::new(-7., 0., -7.),
            max: Vec3::new(7., 5., 7.),
        }),
        radiance: SpecularProbeRadiance {
            face_size: FACE,
            texels,
        },
    }
}

/// A small static box whose vertices sample a static irradiance atlas (UV
/// 0.5, 0.5); the other surfaces' UV (0, 0) takes no baked lighting.
fn baked_box() -> Asset {
    let mut mesh = cuboid(Vec3::new(3.5, 0.25, -2.5), Vec3::splat(0.5), 0);
    for vertex in &mut mesh.vertices {
        vertex.lightmap_uv = [0.5; 2];
    }
    Asset {
        meshes: vec![mesh],
        materials: vec![material([0.5, 0.5, 0.6, 1.], 0., 0.6)],
        images: Vec::new(),
        rig: Default::default(),
    }
}

/// Installs the static irradiance atlas: one 4×4 BC6H block per layer with a
/// BC7 upward lobe where the device has BC, else an RGBA16F texel.
fn install_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    bc: bool,
) -> Result<(), String> {
    if bc {
        // BC7 mode 6 with endpoints (128, 160, 128, 0): w along +Y.
        let lobe = [128u128, 128, 160, 160, 128, 128, 0, 0]
            .iter()
            .enumerate()
            .fold(1u128 << 6, |block, (i, &c)| block | (c >> 1) << (7 + 7 * i))
            .to_le_bytes();
        scene.set_compressed_static_irradiance_atlas(
            device,
            queue,
            &CompressedIrradianceAtlas {
                size: [4, 4],
                irradiance: bc6h_block(400).repeat(2),
                directionality: lobe.repeat(2),
                scale: 0.5,
            },
        )
    } else {
        scene.set_static_irradiance_atlas(
            device,
            queue,
            &IrradianceAtlas {
                size: [1, 1],
                irradiance: vec![[0.07; 3]],
                back_irradiance: vec![[0.07; 3]],
                directionality: Vec::new(),
                back_directionality: Vec::new(),
            },
        )
    }
    .map_err(|e| format!("static irradiance atlas: {e}"))
}

/// A box skinned to one joint, with one morph target that lifts its top
/// face, away from the measured points.
fn deforming() -> Asset {
    let mut mesh = cuboid(Vec3::new(0., 0.4, 0.), Vec3::splat(0.8), 0);
    let deltas = mesh
        .vertices
        .iter()
        .map(|vertex| MorphDelta {
            position: if vertex.position[1] > 0.5 {
                [0., 0.3, 0.]
            } else {
                [0.; 3]
            },
            normal: [0.; 3],
            tangent: [0.; 3],
        })
        .collect();
    mesh.deformation = MeshDeformation {
        influences: vec![
            Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            mesh.vertices.len()
        ],
        morph_targets: vec![MorphTarget { weight: 0, deltas }],
    };
    Asset {
        meshes: vec![mesh],
        materials: vec![material([0.3, 0.6, 0.3, 1.], 0., 0.4)],
        images: Vec::new(),
        rig: Rig {
            nodes: vec![Node {
                name: None,
                parent: None,
                translation: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                scale: Vec3::ONE,
            }],
            joints: vec![Joint {
                node: 0,
                inverse_bind: Mat4::IDENTITY,
            }],
            morph_weights: vec![MorphWeight { node: 0, rest: 0. }],
            clips: Vec::new(),
        },
    }
}

/// A constant radiance field: 16-pixel cube faces in a 336×64 cube-UV atlas,
/// RGBA16F texels of (1/32, 1/32, 1/32, 1).
fn environment() -> EnvironmentMap {
    EnvironmentMap {
        panorama: image::RgbaImage::from_pixel(4, 2, image::Rgba([50, 50, 50, 255])),
        filtered: PmremAtlas {
            width: 336,
            height: 64,
            rgba16: [0x2800u16, 0x2800, 0x2800, 0x3c00]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .cycle()
                .take(336 * 64 * 8)
                .collect(),
        },
    }
}

/// What the frames name of the scene's content.
#[derive(Clone, Copy)]
struct Content {
    environment: EnvironmentId,
    deforming: InstanceId,
}

fn add_content(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
) -> Result<Content, String> {
    let bc = device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    for asset in [world(), alpha_content(bc)?, baked_box()] {
        let model = scene
            .add_asset(device, queue, asset)
            .map_err(|e| format!("add_asset: {e}"))?
            .model;
        scene
            .add_instance(device, queue, InstanceState::new(model), Mobility::Static)
            .map_err(|e| format!("add_instance: {e}"))?;
    }
    // Local lights away from the measured points, on the far side of the box.
    for light in [
        Light {
            position: Vec3::new(-3., 1.5, -1.),
            shape: LightShape::Point,
            color: [0.4, 0.6, 1.],
            intensity: 2.,
            range: 6.,
            baked: false,
            specular: 1.,
            casts_shadow: true,
            ..Default::default()
        },
        Light {
            position: Vec3::new(-2., 3., -2.),
            shape: LightShape::Spot {
                direction: Vec3::NEG_Y,
                inner_angle: 0.3,
                outer_angle: 0.5,
            },
            color: [1., 0.8, 0.6],
            intensity: 10.,
            range: 5.,
            baked: false,
            specular: 1.,
            casts_shadow: true,
            ..Default::default()
        },
        Light {
            position: Vec3::new(-4., 1., 0.),
            shape: LightShape::Rect {
                direction: Vec3::X,
                width_axis: Vec3::Z,
                width: 1.,
                height: 0.3,
            },
            color: [0.7, 0.9, 1.],
            intensity: 3.,
            range: 4.,
            baked: false,
            specular: 1.,
            casts_shadow: false,
            ..Default::default()
        },
    ] {
        scene
            .add_light(device, queue, light)
            .map_err(|e| format!("add_light: {e}"))?;
    }
    let paint = scene
        .add_decal_image(Image::Rgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([230, 170, 20, 255]),
        )))
        .map_err(|e| format!("add_decal_image: {e}"))?;
    scene
        .add_decal(
            device,
            queue,
            Decal {
                position: Vec3::new(-2., 0., 2.),
                size: Vec3::new(1., 0.3, 1.),
                normal_fade: 0.5,
                ..Decal::new(paint)
            },
        )
        .map_err(|e| format!("add_decal: {e}"))?;
    let model = scene
        .add_asset(device, queue, deforming())
        .map_err(|e| format!("add_asset: {e}"))?
        .model;
    let deforming = scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose: Mat4::from_translation(Vec3::new(-2.5, 0., 2.5)),
                ..InstanceState::new(model)
            },
            Mobility::Moving,
        )
        .map_err(|e| format!("add_instance: {e}"))?;
    scene
        .set_instance_baked_irradiance(
            queue,
            deforming,
            AmbientCube {
                irradiance: [[0.05; 3]; 6],
            },
        )
        .map_err(|e| format!("set_instance_baked_irradiance: {e}"))?;
    let glow = |position: [f32; 3]| Glow {
        position,
        color: [1., 0.5, 0.2, 0.8],
        soft_distance: 0.2,
        ..Default::default()
    };
    scene.update_effects(
        device,
        queue,
        &[
            glow([-3., 0.5, -1.]),
            glow([-2.5, 1.2, -1.]),
            glow([-2., 0.5, -1.]),
        ],
    );
    let heat = |position: [f32; 3], weight: f32| HeatDistortion {
        position,
        displacement: [4., -2.],
        weight,
    };
    scene
        .update_heat_distortion(
            queue,
            &[
                heat([-3., 1.5, -1.], 0.),
                heat([-2., 1.5, -1.], 0.),
                heat([-2.5, 2.2, -1.], 1.),
            ],
        )
        .map_err(|e| format!("update_heat_distortion: {e}"))?;
    scene.update_mist(device, queue, &[[-3., 0.3, 1.], [-2.5, 0.3, 1.5]]);
    scene
        .update_fog_volumes(
            device,
            queue,
            &[FogVolume {
                center: Vec3::new(-3., 1., -2.),
                density: 0.1,
                albedo: [0.9, 0.95, 1.],
                edge_fade: 0.5,
                ..Default::default()
            }],
        )
        .map_err(|e| format!("update_fog_volumes: {e}"))?;
    scene
        .set_baked_specular_probes(device, queue, &[probe(bc)])
        .map_err(|e| format!("set_baked_specular_probes: {e}"))?;
    install_atlas(device, queue, scene, bc)?;
    // Probes a metre apart over the ground about the box, which the
    // deforming box's ambient cube lies beneath.
    scene
        .set_dynamic_gi_volume(
            device,
            Some(DynamicGiVolume {
                origin: Vec3::new(-4., 0.25, -4.),
                spacing: Vec3::ONE,
                probes: [9, 3, 9],
            }),
        )
        .map_err(|e| format!("set_dynamic_gi_volume: {e}"))?;
    // An irradiance volume over the -x half of the ground, away from the
    // shadow check's points, written by region: a warm own light, and the
    // sky half hidden from its lower cells.
    let field = IrradianceVolume {
        origin: Vec3::new(-4., 0., -4.),
        cell_size: Vec3::ONE,
        cells: [4, 2, 8],
    };
    scene
        .set_irradiance_volume(device, queue, Some(field))
        .map_err(|e| format!("set_irradiance_volume: {e}"))?;
    let cells: Vec<_> = (0..64)
        .map(|index| IrradianceCell {
            irradiance: AmbientCube {
                irradiance: [[0.3, 0.2, 0.1]; 6],
            },
            sky_visibility: [if (index / 4) % 2 == 0 { 0.5 } else { 1. }; 6],
        })
        .collect();
    let region = PreparedIrradianceRegion::new(field.origin, field.cells, &cells)
        .map_err(|e| format!("PreparedIrradianceRegion: {e}"))?;
    scene
        .write_irradiance_cells(queue, &region)
        .map_err(|e| format!("write_irradiance_cells: {e}"))?;
    let environment = scene
        .add_environment(device, queue, &environment())
        .map_err(|e| format!("add_environment: {e}"))?;
    Ok(Content {
        environment,
        deforming,
    })
}

fn camera() -> Camera {
    let eye = Vec3::new(5., 6., 7.);
    Camera {
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0.5, 0., 0.5), Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
        eye,
    }
}

/// The output pixel `point` projects to: clip space to NDC, NDC's +Y up to
/// the texture's rows down.
fn pixel(camera: &Camera, point: Vec3) -> [u32; 2] {
    let clip = camera.projection * camera.view * Vec4::new(point.x, point.y, point.z, 1.);
    let ndc = clip / clip.w;
    [
        ((ndc.x * 0.5 + 0.5) * SIZE[0] as f32) as u32,
        ((0.5 - ndc.y * 0.5) * SIZE[1] as f32) as u32,
    ]
}

/// Rec. 709 luma of the encoded output, averaged over the 3×3 pixels
/// around `at`.
fn luma(pixels: &[u8], at: [u32; 2]) -> f32 {
    let mut sum = 0.;
    for y in at[1] - 1..=at[1] + 1 {
        for x in at[0] - 1..=at[0] + 1 {
            let p = &pixels[((y * SIZE[0] + x) * 4) as usize..][..3];
            sum += 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
        }
    }
    sum / 9.
}

async fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, String> {
    let stride = (texture.width() * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("smoke readback"),
        size: u64::from(stride) * u64::from(texture.height()),
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
                rows_per_image: Some(texture.height()),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    // The browser maps the buffer once control returns to it.
    let (send, receive) = futures::channel::oneshot::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    receive
        .await
        .map_err(|_| "readback dropped".to_string())?
        .map_err(|e| format!("readback: {e}"))?;
    let mapped = buffer.slice(..).get_mapped_range();
    Ok(mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].iter().copied())
        .collect())
}

/// What one configuration rendered.
struct Rendered {
    pixels: Vec<u8>,
    antialiasing: Antialiasing,
    fsr2_error: Option<String>,
    timed_frames: usize,
}

/// Renders `FRAMES` frames under `settings` with a new renderer, inside
/// error scopes, and reports what ran and the measured luma.
async fn configuration(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    content: Content,
    timing: &mut Option<GpuTiming>,
    settings: &Settings,
    atmosphere: bool,
) -> Result<String, String> {
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let rendered = render(device, queue, scene, content, timing, settings, atmosphere).await;
    let errors = [
        ("internal", internal.pop()),
        ("out-of-memory", memory.pop()),
        ("validation", validation.pop()),
    ];
    for (kind, error) in errors {
        if let Some(error) = error.await {
            return Err(format!("{kind} error: {error}"));
        }
    }
    let rendered = rendered?;
    let camera = camera();
    let lit = luma(&rendered.pixels, pixel(&camera, LIT));
    let shadowed = luma(&rendered.pixels, pixel(&camera, SHADOWED));
    // Luma is read from bytes, so it is never NaN.
    if lit <= 2. * shadowed + 10. {
        return Err(format!(
            "the sunlit ground (luma {lit:.1}) is not clearly brighter than the shadowed ground ({shadowed:.1})"
        ));
    }
    // FSR2 either runs or reports why not: a silent fallback fails.
    if settings.antialiasing == Antialiasing::Fsr2
        && (rendered.antialiasing == Antialiasing::Fsr2) == rendered.fsr2_error.is_some()
    {
        return Err(format!(
            "FSR2 chosen: {:?} in effect with reason {:?}",
            rendered.antialiasing, rendered.fsr2_error
        ));
    }
    let mut report = format!(
        "antialiasing {:?}, lit luma {lit:.1}, shadowed {shadowed:.1}",
        rendered.antialiasing
    );
    if let Some(reason) = rendered.fsr2_error {
        let _ = write!(report, ", FSR2 off: {reason}");
    }
    if timing.is_some() {
        let _ = write!(report, ", {} timed frames read", rendered.timed_frames);
    }
    Ok(report)
}

async fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    content: Content,
    timing: &mut Option<GpuTiming>,
    settings: &Settings,
    atmosphere: bool,
) -> Result<Rendered, String> {
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("smoke output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
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
    let mut renderer = Renderer::new(device, queue, output.format(), SIZE, 1., settings)
        .map_err(|e| format!("Renderer::new: {e}"))?;
    let mut frame = FrameInput::new(camera());
    frame.environment = Some(content.environment);
    frame.atmosphere = atmosphere;
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: SUN.normalize(),
        color: [1., 1., 1.],
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: 20.,
            cascades: 2,
        }),
        ..Default::default()
    });
    frame.hemisphere_light = HemisphereLight {
        sky_color: [0.2, 0.3, 0.5],
        ground_color: [0.05, 0.03, 0.02],
        intensity: 0.2,
    };
    if atmosphere {
        frame.mist = Mist {
            thin_color: [0.6, 0.65, 0.7],
            dense_color: [0.8, 0.85, 0.9],
            opacity: 0.5,
            height: 0.5,
            ..Default::default()
        };
        frame.fog = Fog {
            density: 0.02,
            height: 1.,
            height_falloff: 0.5,
            length: 30.,
            ..Fog::default()
        };
    }
    let mut timed_frames = 0;
    for index in 0..FRAMES {
        if let Some(timing) = timing.as_mut() {
            timed_frames += timing.begin_frame(device, queue).count();
        }
        let phase = index as f32 / FRAMES as f32;
        scene
            .set_instance_deformation(
                queue,
                content.deforming,
                &[Mat4::from_rotation_y(phase)],
                &[phase],
            )
            .map_err(|e| format!("set_instance_deformation: {e}"))?;
        frame.camera_cut = index == 0;
        frame.elapsed_seconds = index as f64 / 60.;
        renderer.resize(device, SIZE, 1., settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &frame,
            settings,
            &view,
            timing.as_ref(),
        );
        queue.submit([encoder.finish()]);
        if let Some(timing) = timing.as_mut() {
            timing.submitted(queue);
        }
        renderer.finish_frame(scene);
    }
    Ok(Rendered {
        pixels: read_pixels(device, queue, &output).await?,
        antialiasing: renderer.antialiasing_in_effect(settings),
        fsr2_error: renderer.fsr2_error().map(str::to_owned),
        timed_frames,
    })
}

#[wasm_bindgen]
pub async fn run() -> String {
    console_error_panic_hook::set_once();
    let mut report = String::new();
    match smoke(&mut report).await {
        Ok(()) => {}
        Err(error) => {
            let _ = writeln!(report, "FAIL setup: {error}");
        }
    }
    report
}

async fn smoke(report: &mut String) -> Result<(), String> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await
        .map_err(|e| format!("no WebGPU adapter: {e}"))?;
    let limits = adapter.limits();
    let _ = writeln!(
        report,
        "ok adapter: {:?}, {} sampled textures and {} storage buffers per stage, features {:?}",
        adapter.get_info(),
        limits.max_sampled_textures_per_shader_stage,
        limits.max_storage_buffers_per_shader_stage,
        adapter.features()
    );
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("SGL3D browser smoke"),
            required_features: sgl_3d::graphics_device::features(&adapter)
                | sgl_3d::graphics_device::fsr2_features(&adapter)
                | (adapter.features()
                    & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TIMESTAMP_QUERY)),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request_device: {e}"))?;
    let uncaptured = Arc::new(Mutex::new(Vec::<String>::new()));
    device.on_uncaptured_error(Arc::new({
        let uncaptured = uncaptured.clone();
        move |error| uncaptured.lock().unwrap().push(error.to_string())
    }));
    let mut timing = GpuTiming::new(&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    let content = add_content(&device, &queue, &mut scene)?;
    let high = Settings {
        screen_space_reflections: ScreenSpaceReflections::Full,
        ambient_occlusion: AmbientOcclusionQuality::High,
        ..Settings::default()
    };
    let configurations = [
        ("high, Crystal SSR, TAA", high, false),
        (
            "high, Velvet and world-space reflections, fog, mist, motion blur, heat",
            Settings {
                screen_space_reflections: ScreenSpaceReflections::Half,
                reflection_method: ReflectionMethod::Velvet,
                world_space_reflections: true,
                motion_blur: MotionBlur::Full,
                heat_distortion: true,
                ..high
            },
            true,
        ),
        (
            "FSR2 chosen",
            Settings {
                antialiasing: Antialiasing::Fsr2,
                ..high
            },
            false,
        ),
        (
            "low, SMAA, low dynamic GI",
            Settings {
                preset: RenderPreset::Low,
                antialiasing: Antialiasing::Smaa,
                dynamic_gi: DynamicGiQuality::Low,
                ..Settings::default()
            },
            false,
        ),
    ];
    for (name, settings, atmosphere) in configurations {
        let result = configuration(
            &device,
            &queue,
            &mut scene,
            content,
            &mut timing,
            &settings,
            atmosphere,
        )
        .await;
        let errors = std::mem::take(&mut *uncaptured.lock().unwrap());
        match result {
            Ok(_) if !errors.is_empty() => {
                let _ = writeln!(report, "FAIL {name}: uncaptured {errors:?}");
            }
            Ok(detail) => {
                let _ = writeln!(report, "ok {name}: {detail}");
            }
            Err(error) => {
                let _ = writeln!(report, "FAIL {name}: {error}");
            }
        }
    }
    Ok(())
}
