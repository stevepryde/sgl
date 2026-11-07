//! Known tangent-space maps versus the same shading normal supplied by vertices.
//! The oracle is a world-space direction and equivalence of the public raster paths.
#![cfg(not(target_arch = "wasm32"))]
use sgl_3d::{
    asset::{Asset, CpuMesh, Image, Material, Vertex},
    diagnostics::{self, DiagnosticTarget},
    glam::{Mat4, Vec3},
    settings::{self, Settings},
    *,
};
// Tangent mode 0: absent, 1: supplied but derivative TBN, 2: authored TBN
// selected by anisotropy. Materials are diffuse-only to isolate normal response.
const SIZE: [u32; 2] = [96, 96];
fn asset(texel: [u8; 4], mapped: bool, tangent: u32) -> Asset {
    let n = Vec3::new(texel[0] as f32, texel[1] as f32, texel[2] as f32).map(|v| v / 127.5 - 1.);
    let normal = if mapped { Vec3::Z } else { n.normalize() };
    let vertices = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
        .map(|[u, v]| Vertex {
            position: [(u - 0.5) * 8., (v - 0.5) * 8., -5.],
            normal: normal.to_array(),
            uv: [u, v],
            tangent: if tangent > 0 {
                [1., 0., 0., 1.]
            } else {
                [0.; 4]
            },
            color: [1.; 4],
            lightmap_uv: [0.; 2],
            lightmap_bounds: [0., 0., 1., 1.],
        })
        .to_vec();
    Asset {
        meshes: vec![CpuMesh {
            vertices,
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            base: [0.5, 0.5, 0.5, 1.],
            metallic: 0.,
            roughness: 1.,
            specular: 0.,
            normal_texture: mapped.then_some(0),
            anisotropy_strength: if tangent == 2 { 0.2 } else { 0. },
            ..Default::default()
        }],
        images: vec![Image::Rgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba(texel),
        ))],
        rig: Default::default(),
        ignored: vec![],
    }
}
struct ResultImage {
    normals: Vec<Vec3>,
    rgb: Vec<Vec3>,
    ao: Vec<u32>,
    image: Vec<u8>,
}
#[allow(clippy::too_many_arguments)]
fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    output: &wgpu::TextureView,
    texel: [u8; 4],
    mapped: bool,
    tangent: u32,
    rotation: Mat4,
    ao: bool,
) -> ResultImage {
    let mut scene = Scene::new(device, queue);
    let model = scene
        .add_asset(device, queue, asset(texel, mapped, tangent))
        .unwrap()
        .model;
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose: rotation,
                ..InstanceState::new(model)
            },
            Mobility::Static,
        )
        .unwrap();
    let mut input = FrameInput::new(Camera {
        eye: Vec3::ZERO,
        view: rotation.inverse(),
        projection: perspective(55f32.to_radians(), 1., 0.1),
    });
    input.camera_cut = true;
    input.hemisphere_light = HemisphereLight {
        sky_color: [1.; 3],
        ground_color: [1.; 3],
        intensity: 1.,
    };
    input.directional_lights[0] = Some(DirectionalLight {
        direction: rotation.transform_vector3(Vec3::new(-0.3, -0.4, -1.).normalize()),
        color: [1.; 3],
        illuminance: 1.,
        shadow: None,
        ..Default::default()
    });
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        scene_resolution: settings::SceneResolution::Full,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: if ao {
            settings::AmbientOcclusionQuality::Ultra
        } else {
            settings::AmbientOcclusionQuality::Off
        },
        ..Default::default()
    };
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        &mut scene,
        &input,
        &settings,
        output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut scene);
    let read = |target, bpp| {
        diagnostics::read(
            device,
            queue,
            renderer.diagnostic_target(target).unwrap().texture(),
            bpp,
        )
    };
    let ns = read(DiagnosticTarget::Normal, 8);
    // Decode the documented octahedral attachment, not the material/TBN implementation.
    let normals = ns
        .chunks_exact(8)
        .map(|p| {
            let x = diagnostics::half(p);
            let y = diagnostics::half(&p[2..]);
            let mut n = Vec3::new(x, y, 1. - x.abs() - y.abs());
            if n.z < 0. {
                n.x = (1. - y.abs()) * x.signum();
                n.y = (1. - x.abs()) * y.signum();
            }
            n.normalize()
        })
        .collect();
    let rgb = read(DiagnosticTarget::Composite, 8)
        .chunks_exact(8)
        .map(|p| {
            Vec3::new(
                diagnostics::half(p),
                diagnostics::half(&p[2..]),
                diagnostics::half(&p[4..]),
            )
        })
        .collect();
    let ao = if ao {
        read(DiagnosticTarget::AmbientOcclusion, 4)
            .chunks_exact(4)
            .map(|p| u32::from_le_bytes(p.try_into().unwrap()))
            .collect()
    } else {
        vec![255; 96 * 96]
    };
    ResultImage {
        normals,
        rgb,
        ao,
        image: diagnostics::read(device, queue, output.texture(), 4),
    }
}
#[test]
fn known_normal_maps_match_vertex_normals_with_rotation_and_ao() {
    let required = cfg!(target_os = "macos")
        || std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    let adapter =
        match pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())) {
            Ok(adapter) => adapter,
            Err(error) => {
                assert!(!required, "GPU required for normal-map evidence: {error}");
                eprintln!("skipping normal-map GPU test: {error}");
                return;
            }
        };
    eprintln!("normal controls adapter: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: graphics_device::features(&adapter),
        required_limits: graphics_device::limits(&adapter),
        ..Default::default()
    }))
    .unwrap();
    let output = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("normal controls"),
            size: wgpu::Extent3d {
                width: 96,
                height: 96,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
        .create_view(&Default::default());
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        ..Default::default()
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        SIZE,
        1.,
        &settings,
    )
    .unwrap();
    let mut montage = image::RgbaImage::new(96 * 4, 96 * 6);
    let mut normal_montage = image::RgbaImage::new(96 * 4, 96 * 6);
    let mut row = 0;
    for (label, texel) in [
        ("flat", [128, 128, 255, 255]),
        ("+X", [204, 128, 230, 255]),
        ("+Y", [128, 204, 230, 255]),
    ] {
        for rotated in [false, true] {
            let rotation = if rotated {
                Mat4::from_rotation_y(0.63) * Mat4::from_rotation_z(0.37)
            } else {
                Mat4::IDENTITY
            };
            let tangent_normal = Vec3::new(
                texel[0] as f32 / 127.5 - 1.,
                texel[1] as f32 / 127.5 - 1.,
                texel[2] as f32 / 127.5 - 1.,
            )
            .normalize();
            let expected = rotation.transform_vector3(tangent_normal);
            for tangent in [0, 1, 2] {
                for ao in [false, true] {
                    let mapped = draw(
                        &device,
                        &queue,
                        &mut renderer,
                        &output,
                        texel,
                        true,
                        tangent,
                        rotation,
                        ao,
                    );
                    let reference = draw(
                        &device,
                        &queue,
                        &mut renderer,
                        &output,
                        texel,
                        false,
                        tangent,
                        rotation,
                        ao,
                    );
                    let (mut normal_error, mut color_error, mut ao_error) = (0f32, 0f32, 0u32);
                    for y in 24..72 {
                        for x in 24..72 {
                            let i = y * 96 + x;
                            normal_error = normal_error.max(mapped.normals[i].distance(expected));
                            color_error = color_error
                                .max((mapped.rgb[i] - reference.rgb[i]).abs().max_element());
                            ao_error = ao_error.max(mapped.ao[i].abs_diff(reference.ao[i]));
                        }
                    }
                    eprintln!(
                        "{label} rotated={rotated} tangents={tangent} ao={ao}: normal vector error={normal_error:.6}, HDR abs error={color_error:.6}, AO byte error={ao_error}"
                    );
                    assert!(
                        normal_error < 0.015,
                        "{label}: world-space normal {normal_error}"
                    );
                    assert!(
                        color_error < 0.015,
                        "{label}: mapped/reference lighting {color_error}"
                    );
                    assert!(ao_error <= 3, "{label}: mapped/reference AO {ao_error}");
                    if tangent == 0 {
                        let column = if ao { 2 } else { 0 };
                        for (offset, observation) in [(0, &mapped), (1, &reference)] {
                            let tile = image::RgbaImage::from_fn(96, 96, |x, y| {
                                let n = observation.normals[(y * 96 + x) as usize];
                                let rgb = ((n + Vec3::ONE) * 127.5)
                                    .to_array()
                                    .map(|v| v.clamp(0., 255.).round() as u8);
                                image::Rgba([rgb[0], rgb[1], rgb[2], 255])
                            });
                            image::imageops::replace(
                                &mut normal_montage,
                                &tile,
                                (column + offset) * 96,
                                row * 96,
                            );
                        }
                        for (offset, pixels) in [(0, &mapped.image), (1, &reference.image)] {
                            let tile = image::RgbaImage::from_raw(96, 96, pixels.clone()).unwrap();
                            image::imageops::replace(
                                &mut montage,
                                &tile,
                                (column + offset) * 96,
                                row * 96,
                            );
                        }
                    }
                }
            }
            row += 1;
        }
    }
    if let Ok(directory) = std::env::var("SGL_NORMAL_CONTROL_OUTPUT") {
        std::fs::create_dir_all(&directory).unwrap();
        normal_montage
            .save(std::path::Path::new(&directory).join("normal-directions.png"))
            .unwrap();
        std::fs::write(std::path::Path::new(&directory).join("README.txt"), "Controlled SGL normal-map evidence. Rows: flat +Z, rotated flat +Z, +X tilt, rotated +X tilt, +Y tilt, rotated +Y tilt. Columns: mapped AO off, equivalent vertex normal AO off, mapped AO Ultra, equivalent vertex normal AO Ultra. normal-controls.png: rendered diffuse lighting. normal-directions.png: world-space G-buffer normals encoded for display as RGB=(N+1)/2. Adjacent mapped/reference tiles should agree. Numerical comparisons use the central 48x48 pixels; image appearance is not the acceptance criterion. Tangent modes 0/1/2 are tested numerically; images show mode 0. See run.log for all 36 cases.\n").unwrap();
        montage
            .save(std::path::Path::new(&directory).join("normal-controls.png"))
            .unwrap();
    }
}
