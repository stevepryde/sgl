//! A material's sheen and diffuse transmission maps through the real opaque
//! stage, whose G-buffer and lit colour take them, and through a real ray
//! hit.
use crate::asset::{Asset, CpuMesh, Image, Vertex};
use crate::graphics_device::BindingTier;
use crate::renderer::Renderer;
use crate::settings::{AmbientOcclusionQuality, Settings};
use crate::{Backdrop, Camera, DirectionalLight, FrameInput, Scene, perspective, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// A square facing the camera, 2 m across, 4 m before it.
fn square() -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                position: [x, y, -4.],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [0.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// An image of `texel` throughout.
fn flat(texel: [u8; 4]) -> Image {
    Image::Rgba8(image::RgbaImage::from_pixel(8, 8, image::Rgba(texel)))
}

/// sRGB's transfer function from an 8-bit code to linear.
fn linear(code: u8) -> f32 {
    let c = f32::from(code) / 255.;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The colour maps' texels, sRGB; the share maps' alpha.
const SHEEN_TEXEL: [u8; 4] = [153, 51, 102, 255];
const SHEEN_ROUGHNESS_TEXEL: [u8; 4] = [0, 0, 0, 230];
const TRANSMISSION_TEXEL: [u8; 4] = [0, 0, 0, 153];
const TRANSMISSION_COLOR_TEXEL: [u8; 4] = [204, 102, 51, 255];
const SHEEN: [f32; 3] = [0.9, 0.8, 0.7];
const SHEEN_ROUGHNESS: f32 = 1.;
const TRANSMISSION: f32 = 0.9;
const TRANSMISSION_COLOR: [f32; 3] = [1., 0.8, 0.6];

/// The square in a white dielectric with a sheen and diffuse transmission: their
/// factors and maps where `mapped`, else the maps' products already in
/// their factors, as glTF defines them.
fn square_asset(mapped: bool) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![square()];
    let material = &mut asset.materials[0];
    material.base = [1.; 4];
    material.metallic = 0.;
    material.roughness = 0.5;
    if mapped {
        asset.images = vec![
            flat(SHEEN_TEXEL),
            flat(SHEEN_ROUGHNESS_TEXEL),
            flat(TRANSMISSION_TEXEL),
            flat(TRANSMISSION_COLOR_TEXEL),
        ];
        material.sheen_color = SHEEN;
        material.sheen_roughness = SHEEN_ROUGHNESS;
        material.sheen_color_texture = Some(0);
        material.sheen_roughness_texture = Some(1);
        material.diffuse_transmission = TRANSMISSION;
        material.diffuse_transmission_color = TRANSMISSION_COLOR;
        material.diffuse_transmission_texture = Some(2);
        material.diffuse_transmission_color_texture = Some(3);
    } else {
        let (sheen, transmission) = products();
        material.sheen_color = sheen.0;
        material.sheen_roughness = sheen.1;
        material.diffuse_transmission = transmission.0;
        material.diffuse_transmission_color = transmission.1;
    }
    asset
}

/// The maps' products with their factors, as KHR_materials_sheen and
/// KHR_materials_diffuse_transmission define them: the colours times the
/// colour maps' RGB, sRGB-decoded, and the roughness and share times their
/// maps' alpha.
fn products() -> (([f32; 3], f32), (f32, [f32; 3])) {
    let alpha = |texel: [u8; 4]| f32::from(texel[3]) / 255.;
    let colour = |factor: [f32; 3], texel: [u8; 4]| {
        [0, 1, 2].map(|channel| factor[channel] * linear(texel[channel]))
    };
    (
        (
            colour(SHEEN, SHEEN_TEXEL),
            SHEEN_ROUGHNESS * alpha(SHEEN_ROUGHNESS_TEXEL),
        ),
        (
            TRANSMISSION * alpha(TRANSMISSION_TEXEL),
            colour(TRANSMISSION_COLOR, TRANSMISSION_COLOR_TEXEL),
        ),
    )
}

/// A frame from the origin looking at the square, lit only by a light
/// behind it.
fn input() -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.baked_lighting = false;
    input.hemisphere_light.intensity = 0.;
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::Z,
        color: [1.; 3],
        illuminance: 1.,
        shadow: None,
        ..Default::default()
    });
    input
}

// Plausible defects: a map read from another channel (the roughness and the
// share from red, where KHR keeps them in alpha), a colour map read as
// linear data, a factor dropped, or one path (raster or ray hit) leaving a
// map out. The oracle is the two extensions' definitions: each value is
// its factor times its map's channel. Raster renders the square with the
// maps and again with their products already in the factors; the base
// reflectance the G-buffer records beneath the sheen (F90, the dielectric's
// 1 dimmed by the sheen's albedo at the view) and the light a light behind
// the square passes through to the camera must agree within binary16. A
// ray hit's surface takes the products themselves.
#[test]
fn sheen_and_diffuse_transmission_maps_scale_their_factors() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = Settings {
        ambient_occlusion: AmbientOcclusionQuality::Off,
        ..Settings::default()
    };
    let input = input();
    // The pixel at the square's centre, which faces the camera.
    let at = ((SIZE[1] / 2) * SIZE[0] + SIZE[0] / 2) as usize;
    let mut observed = Vec::new();
    for mapped in [true, false] {
        let mut scene = Scene::new(&device, &queue);
        test_support::add_static(&device, &queue, &mut scene, square_asset(mapped));
        let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        if renderer.binding_tier() != BindingTier::Extended {
            eprintln!(
                "skipping: the device takes the Basic binding tier, which binds none of these maps"
            );
            return;
        }
        let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        let fused = renderer.test_fused_supported();
        renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
        queue.submit([encoder.finish()]);
        let targets = renderer.targets();
        let material = test_support::read(&device, &queue, targets.material.texture(), 8);
        let color = test_support::read(&device, &queue, targets.color.texture(), 8);
        let f90 = test_support::half(&material[at * 8 + 6..]);
        let lit: Vec<f32> = (0..3)
            .map(|channel| test_support::half(&color[at * 8 + channel * 2..]))
            .collect();
        observed.push((f90, lit, scene, renderer));
    }
    let (mapped, products) = (&observed[0], &observed[1]);
    assert!(
        mapped.0 < 0.98 && (mapped.0 - products.0).abs() <= 2e-3,
        "the base beneath the sheen: F90 {} with the maps, {} with their products",
        mapped.0,
        products.0
    );
    for channel in 0..3 {
        assert!(
            products.1[channel] > 0.001
                && (mapped.1[channel] - products.1[channel]).abs() <= 2e-3 * products.1[channel],
            "light passed through: {:?} with the maps, {:?} with their products",
            mapped.1,
            products.1
        );
    }

    let ((sheen, sheen_roughness), (transmission, transmission_color)) = products();
    let (_, _, mut scene, mut renderer) = observed.into_iter().next().unwrap();
    let observation = r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {
 var ray:SceneRay;
 ray.direction=vec4(0.,0.,-1.,100.);
 let hit=scene_decode_hit(scene_trace_nearest(ray,SCENE_SIDES_AS_RASTER),ray.origin.xyz,ray.direction.xyz);
 let material=scene_material(hit.material_word);
 let s=ray_surface(hit,material,ray_base_color(hit,material),vec3(0.),vec3(0.,0.,1.),cluster_range(hit.position,vec2(0.)));
 output[0]=vec4(s.sheen,s.sheen_roughness);
 output[1]=vec4(s.diffuse_transmission_color,s.diffuse_transmission);
}
"#;
    let hit = test_support::observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        observation,
        2,
    );
    let expected = [
        [sheen[0], sheen[1], sheen[2], sheen_roughness],
        [
            transmission_color[0],
            transmission_color[1],
            transmission_color[2],
            transmission,
        ],
    ];
    for (observed, expected) in hit.iter().zip(expected) {
        for channel in 0..4 {
            assert!(
                (observed[channel] - expected[channel]).abs() <= 2e-3,
                "ray hit: {observed:?}, the maps' products {expected:?}"
            );
        }
    }
}
