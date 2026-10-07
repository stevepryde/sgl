//! A material's clearcoat and iridescence maps through the real opaque
//! stage, whose G-buffer reflections and source completion read, and through
//! a real ray hit.
use crate::asset::{CpuMesh, Image, Vertex};
use crate::graphics_device::BindingTier;
use crate::renderer::Renderer;
use crate::settings::{
    AmbientOcclusionQuality, ScreenSpaceReflections, Settings, WorldSpaceReflections,
};
use crate::shading::iridescence_tests::thin_film;
use crate::{Camera, FrameInput, Scene, perspective, test_support};
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

// Plausible defects: a map read from another channel (three.js's node path
// takes clearcoat roughness from red) or another map's binding, its factor
// dropped, or one path (raster or ray hit) leaving a map out; the coat's
// normal not reaching the surface or the G-buffer's normal.BA (the geometry
// normal written there, or the base normal), or not framed by the UVs; the
// film's strength or thickness map ignored, or its thickness not mixed from
// the thinnest to the thickest. The oracle is KHR_materials_clearcoat's and
// KHR_materials_iridescence's definitions: clearcoat is the factor times the
// clearcoat map's red channel, its roughness the factor times the roughness
// map's green, the film's strength the factor times the iridescence map's
// red and its thickness the thinnest mixed toward the thickest by the
// thickness map's green; the coat's normal is the clearcoat normal map's
// texel, its X and Y at its scale, on the square's tangent frame (+U along
// +X, +V along -Y, the normal +Z: glTF's textureInfo axes), while the base
// keeps the geometry normal; at normal incidence the F0 the G-buffer records
// (and a ray hit's surface_f0) is the film's reflectance, the exact
// thin-film sum within its bound (iridescence_tests). Each map's other
// channels are zero, so another channel or map reads something else.
#[test]
fn clearcoat_and_iridescence_maps_scale_their_factors() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut asset = test_support::cube();
    asset.meshes = vec![square()];
    // Clearcoat 153/255 = 0.6 in red; its roughness 51/255 = 0.2 in green;
    // the film's strength 1 in red; its thickness 102/255 = 0.4 in green;
    // a clearcoat normal texel.
    asset.images = vec![
        flat([153, 0, 0, 255]),
        flat([0, 51, 0, 255]),
        flat([255, 0, 0, 255]),
        flat([0, 102, 0, 255]),
        flat([64, 192, 220, 255]),
    ];
    let material = &mut asset.materials[0];
    material.base = [1.; 4];
    material.metallic = 0.;
    material.roughness = 0.5;
    material.clearcoat = 0.5;
    material.coat_roughness = 0.8;
    material.clearcoat_texture = Some(0);
    material.coat_roughness_texture = Some(1);
    material.iridescence = 1.;
    material.iridescence_ior = 1.8;
    material.iridescence_thickness = [200., 700.];
    material.iridescence_texture = Some(2);
    material.iridescence_thickness_texture = Some(3);
    material.coat_normal_texture = Some(4);
    material.coat_normal_scale = 0.5;
    test_support::add_static(&device, &queue, &mut scene, asset);
    let coat = 0.5 * 0.6;
    let coat_roughness = 0.8 * 0.2;
    let tangent = Vec3::new(64., 192., 220.) / 255. * 2. - Vec3::ONE;
    let coat_normal = Vec3::new(tangent.x * 0.5, -tangent.y * 0.5, tangent.z).normalize();
    let film = thin_film(1.8, 1.5, 200. + 0.4 * 500., 1.);

    let projection = perspective(1., 1., 0.1);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection,
        eye: Vec3::ZERO,
    });
    let settings = Settings {
        ambient_occlusion: AmbientOcclusionQuality::Off,
        screen_space_reflections: ScreenSpaceReflections::Half,
        world_space_reflections: WorldSpaceReflections::Moving,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    if renderer.binding_tier() != BindingTier::Extended {
        eprintln!(
            "skipping: the device takes the Basic binding tier, which binds none of these maps"
        );
        return;
    }
    let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
    let forms = if renderer.test_fused_supported() {
        vec![true, false]
    } else {
        vec![false]
    };
    // The pixel at the square's centre, which faces the camera.
    let at = ((SIZE[1] / 2) * SIZE[0] + SIZE[0] / 2) as usize;
    for fused in forms {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
        queue.submit([encoder.finish()]);
        let targets = renderer.targets();
        let materials = test_support::read(&device, &queue, targets.material.texture(), 8);
        let normals = test_support::read(&device, &queue, targets.normal.texture(), 8);
        let [base_x, base_y, coat_x, coat_y] =
            [0, 2, 4, 6].map(|byte| test_support::half(&normals[at * 8 + byte..]));
        let [expected_base, expected_coat] = [Vec3::Z, coat_normal].map(test_support::octahedral);
        assert!(
            (coat_x - expected_coat[0]).abs() < 3e-3
                && (coat_y - expected_coat[1]).abs() < 3e-3
                && (base_x - expected_base[0]).abs() < 3e-3
                && (base_y - expected_base[1]).abs() < 3e-3,
            "fused {fused}: normal.RGBA {:?}, expected base {expected_base:?} and coat {expected_coat:?}",
            [base_x, base_y, coat_x, coat_y]
        );
        let f0 = test_support::read(&device, &queue, targets.f0.texture(), 4);
        let recorded_coat_roughness = test_support::half(&materials[at * 8..]);
        let recorded_coat = test_support::half(&materials[at * 8 + 4..]);
        assert!(
            (recorded_coat - coat).abs() < 2e-3
                && (recorded_coat_roughness - coat_roughness).abs() < 2e-3,
            "fused {fused}: coat {recorded_coat} at roughness {recorded_coat_roughness}, expected {coat} at {coat_roughness}"
        );
        let recorded_f0: Vec<f64> = f0[at * 4..at * 4 + 3]
            .iter()
            .map(|&v| f64::from(v) / 255.)
            .collect();
        for channel in 0..3 {
            assert!(
                (recorded_f0[channel] - film[channel]).abs() <= 0.01 + 1. / 255.,
                "fused {fused}: F0 {recorded_f0:?}, a thin film {film:?}"
            );
        }
    }

    // A ray hit along the camera's axis.
    let observation = r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {
 // A ray built field by field: naga's pipeline-constant pass refuses a
 // constant SceneRay constructor (ExpressionAlreadyInScope).
 var ray:SceneRay;
 ray.direction=vec4(0.,0.,-1.,100.);
 let hit=scene_decode_hit(scene_trace_nearest(ray,SCENE_SIDES_AS_RASTER),ray.origin.xyz,ray.direction.xyz);
 let material=scene_material(hit.material_word);
 let s=ray_surface(hit,material,ray_base_color(hit,material),vec3(0.),vec3(0.,0.,1.),cluster_range(hit.position,vec2(0.)));
 output[0]=vec4(s.coat,s.coat_roughness,0.,0.);
 output[1]=vec4(surface_f0(s),0.);
 output[2]=vec4(s.coat_normal,0.);
}
"#;
    let observed = test_support::observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        observation,
        3,
    );
    let [hit_coat, hit_coat_roughness, ..] = observed[0];
    assert!(
        (hit_coat - coat).abs() < 1e-3 && (hit_coat_roughness - coat_roughness).abs() < 1e-3,
        "ray hit: coat {hit_coat} at roughness {hit_coat_roughness}, expected {coat} at {coat_roughness}"
    );
    let hit_coat_normal = Vec3::from_slice(&observed[2][..3]);
    assert!(
        hit_coat_normal.distance(coat_normal) < 2e-3,
        "ray hit: coat normal {hit_coat_normal}, expected {coat_normal}"
    );
    for channel in 0..3 {
        assert!(
            (f64::from(observed[1][channel]) - film[channel]).abs() <= 0.01,
            "ray hit: F0 {:?}, a thin film {film:?}",
            observed[1]
        );
    }
}
