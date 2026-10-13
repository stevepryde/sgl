use super::*;
use sgl_3d::glam::camera;

// A floor viewed along its geometry normal. The +Y probe face is green and
// +X is red: a 45-degree base normal map reflects red while the geometry-normal
// coat reflects green. These distinct light sources give an optical oracle
// independent of the attachment encoding and decoding.
#[test]
fn coated_base_normal_and_geometry_coat_see_separate_probe_faces() {
    let mut failures = Vec::new();
    for method in [
        None,
        Some(settings::ReflectionMethod::Velvet),
        Some(settings::ReflectionMethod::Crystal),
    ] {
        for resolution in [
            settings::ScreenSpaceReflections::Full,
            settings::ScreenSpaceReflections::Half,
        ] {
            if method.is_none() && resolution == settings::ScreenSpaceReflections::Half {
                continue;
            }
            let mut observations = Vec::new();
            for (mapped, coat) in [(false, 1.), (true, 0.), (true, 1.)] {
                let mut floor = material([0.9, 0.9, 0.9, 1.], 1., 0.05);
                floor.clearcoat = coat;
                floor.coat_roughness = 0.05;
                let mut asset = world(floor, 0.);
                for vertex in &mut asset.meshes[0].vertices {
                    vertex.uv = [vertex.position[0], vertex.position[2]];
                }
                if mapped {
                    asset
                        .images
                        .push(sgl_3d::asset::Image::Rgba8(image::RgbaImage::from_pixel(
                            1,
                            1,
                            image::Rgba([218, 128, 218, 255]),
                        )));
                    asset.materials[0].normal_texture = Some(0);
                }
                let Some(mut frames) = Frames::new(asset, environment(0.), method.is_some(), SIZE)
                else {
                    return;
                };
                if let Some(method) = method {
                    frames.settings.reflection_method = method;
                    frames.settings.screen_space_reflections = resolution;
                }
                let eye = Vec3::new(0., 5., 0.);
                frames.input.camera.eye = eye;
                frames.input.camera.view =
                    camera::rh::view::look_at_mat4(eye, Vec3::new(0., -1., 0.), Vec3::Z);
                let mut texels = Vec::new();
                for mip in 0..7 {
                    for face in 0..6 {
                        // Radiance four (binary16 0x4400), otherwise black.
                        let rgba = match face {
                            0 => [0x4400, 0, 0, 0x3c00],
                            2 => [0, 0x4400, 0, 0x3c00],
                            _ => [0, 0, 0, 0x3c00],
                        };
                        for _ in 0..(64usize >> mip).pow(2) {
                            texels.extend_from_slice(&rgba);
                        }
                    }
                }
                let probe = sgl_3d::BakedSpecularProbe {
                    center: Vec3::ZERO,
                    world_to_local: Mat4::IDENTITY,
                    influence: sgl_3d::SpecularProbeBox {
                        min: Vec3::splat(-30.),
                        max: Vec3::splat(30.),
                    },
                    blend: Vec3::ZERO,
                    proxy: None,
                    radiance: sgl_3d::SpecularProbeRadiance {
                        face_size: 64,
                        texels: SpecularProbeTexels::Rgba16Float(texels),
                    },
                };
                frames
                    .scene
                    .set_baked_specular_probes(&frames.device, &frames.queue, &[probe])
                    .unwrap();
                frames.render(FRAMES, true);
                let rgb = frames.composite()[(SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) as usize];
                eprintln!("{method:?} {resolution:?} mapped={mapped} coat={coat}: {rgb:?}");
                observations.push(rgb);
            }
            let [flat, base, coated] = observations.as_slice() else {
                unreachable!()
            };
            assert!(
                flat[1] > 3. && flat[0] < 0.01,
                "flat metal should reflect the green overhead probe: {flat:?}"
            );
            assert!(
                base[0] > 3. && base[1] < 0.01,
                "mapped bare metal should reflect the red side probe: {base:?}"
            );
            if !(coated[0] > 3. && coated[0] < base[0] && coated[1] > 0.1 && coated[1] < 0.25) {
                failures.push(format!("{method:?} {resolution:?}: mapped base must retain red with coat attenuation and geometry coat must add green at dielectric F0=0.04: {coated:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// Exercise the actual stable-material producer and RGBA16F render attachment.
// Authored normals sample the sphere, independently of the encoding. Allow a
// full binary16 ULP (2^-11) per signed coordinate, including directed rounding.
// Octahedron reconstruction amplifies this by at most sqrt(6), and normalization
// by sqrt(3): asin(sqrt(18) * 2^-11) < 0.12 degrees. Unsigned remapping doubles
// coordinate error; actual GPU storage must stay within the signed bound.
#[test]
fn stable_normal_attachment_preserves_authored_directions() {
    const GRID: u32 = 32;
    const PIXELS_PER_QUAD: u32 = 4;
    const EXTENT: u32 = GRID * PIXELS_PER_QUAD;
    let normals: Vec<_> = (0..GRID * GRID)
        .map(|i| {
            let z = 1. - 2. * (i as f32 + 0.5) / (GRID * GRID) as f32;
            let azimuth = i as f32 * 2.399_963_1;
            let radius = (1. - z * z).sqrt();
            Vec3::new(radius * azimuth.cos(), radius * azimuth.sin(), z).normalize()
        })
        .collect();
    let asset = Asset {
        meshes: normals
            .iter()
            .enumerate()
            .map(|(i, &normal)| {
                let x = (i as u32 % GRID) as f32 * 2. / GRID as f32 - 1.;
                let y = (i as u32 / GRID) as f32 * 2. / GRID as f32 - 1.;
                let step = 2. / GRID as f32;
                quad(
                    [
                        Vec3::new(x, y, 0.5),
                        Vec3::new(x + step, y, 0.5),
                        Vec3::new(x + step, y + step, 0.5),
                        Vec3::new(x, y + step, 0.5),
                    ],
                    normal,
                    0,
                )
            })
            .collect(),
        materials: vec![material([0.5; 4], 0., 0.5)],
        images: vec![],
        rig: Default::default(),
        ignored: Vec::new(),
    };
    let Some(mut frames) = Frames::new(asset, environment(0.), false, [EXTENT; 2]) else {
        return;
    };
    frames.input.camera = Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::Z,
    };
    frames.render(1, true);
    let packed = frames.texels::<4>(frames.target(DiagnosticTarget::Normal));
    let mut worst_error = 0_f64;
    for (i, authored) in normals.iter().enumerate() {
        let x = (i as u32 % GRID) * PIXELS_PER_QUAD + PIXELS_PER_QUAD / 2;
        let y = EXTENT - 1 - ((i as u32 / GRID) * PIXELS_PER_QUAD + PIXELS_PER_QUAD / 2);
        let texel = packed[(y * EXTENT + x) as usize];
        for pair in [0, 2] {
            let x = f64::from(texel[pair]);
            let y = f64::from(texel[pair + 1]);
            let z = 1. - x.abs() - y.abs();
            let fold = (-z).clamp(0., 1.);
            let decoded =
                sgl_3d::glam::DVec3::new(x - fold.copysign(x), y - fold.copysign(y), z).normalize();
            let authored = authored.as_dvec3().normalize();
            let error = decoded
                .cross(authored)
                .length()
                .atan2(decoded.dot(authored))
                .to_degrees();
            worst_error = worst_error.max(error);
            assert!(
                error < 0.12,
                "normal {i}, pair {pair}: {error}° from authored {authored:?}, decoded {decoded:?}"
            );
        }
    }
    eprintln!("stable normal maximum angular error: {worst_error}°");
}
