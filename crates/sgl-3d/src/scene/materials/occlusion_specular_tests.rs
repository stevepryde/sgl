//! A material's occlusion map in an image of its own and its
//! KHR_materials_specular maps through the real opaque stage, whose
//! G-buffer records what they make of the material, on both binding tiers,
//! through a real ray hit and through a probe capture.
use crate::asset::{
    Asset, CpuMesh, Image, Material, MaterialTexture, Sampler, TextureFilter, TextureWrap, UvSet,
    UvTransform, Vertex,
};
use crate::graphics_device::BindingTier;
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{AmbientOcclusionQuality, Settings};
use crate::{Backdrop, Camera, DirectionalLight, FrameInput, Scene, perspective, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// A square facing the camera, 2 m across, 4 m before it: U runs from 0 at
/// x = -1 to 1 at x = 1, V from 0 at y = 1 to 1 at y = -1.
fn square() -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                position: [x, y, -4.],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                ..bytemuck::Zeroable::zeroed()
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// The maps' texels, each pattern its own (2×2 images, texel column by U and
/// row by V): the occlusion map's red by column, the specular map's alpha by
/// row and the specular colour map's RGB (sRGB) along the diagonals, so
/// every quadrant takes another combination.
const OCCLUSION: [u8; 2] = [255, 76];
const SPECULAR: [u8; 2] = [255, 128];
const COLOR: [[u8; 3]; 2] = [[255, 255, 255], [160, 128, 64]];
/// The factors: an IOR whose F0 is 0.25, a specular colour factor that
/// takes white's red past 1 (where KHR_materials_specular clamps before the
/// strength scales), the strength and the occlusion strength.
const IOR: f32 = 3.;
const SPECULAR_FACTOR: f32 = 0.8;
const COLOR_FACTOR: [f32; 3] = [4.8, 2., 1.2];
const OCCLUSION_STRENGTH: f32 = 0.8;
const BASE: [f32; 4] = [0.5, 0.5, 0.5, 1.];

/// The quadrants, as texel column and row.
const QUADRANTS: [[usize; 2]; 4] = [[0, 0], [1, 0], [0, 1], [1, 1]];

/// The texels at quadrant `[column, row]`: the occlusion map's red, the
/// specular map's alpha and the specular colour map's RGB.
fn texels([column, row]: [usize; 2]) -> (u8, u8, [u8; 3]) {
    (OCCLUSION[column], SPECULAR[row], COLOR[(column + row) % 2])
}

/// sRGB's transfer function from an 8-bit code to linear (IEC 61966-2-1).
fn linear(code: u8) -> f32 {
    let c = f32::from(code) / 255.;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// What glTF makes of the material at quadrant `at`: the specular strength
/// and colour, factor times texel (KHR_materials_specular: the strength the
/// specular map's alpha, the colour the colour map's RGB decoded from
/// sRGB), and the occlusion, glTF 2.0's 1 + strength (texel - 1).
fn products(at: [usize; 2]) -> (f32, [f32; 3], f32) {
    let (occlusion, specular, color) = texels(at);
    (
        SPECULAR_FACTOR * f32::from(specular) / 255.,
        [0, 1, 2].map(|c| COLOR_FACTOR[c] * linear(color[c])),
        1. + OCCLUSION_STRENGTH * (f32::from(occlusion) / 255. - 1.),
    )
}

/// The dielectric's F0 at quadrant `at`, KHR_materials_specular's
/// min(((ior - 1) / (ior + 1))^2 * specularColor, 1) * specular.
fn dielectric_f0(at: [usize; 2]) -> [f32; 3] {
    let (specular, color, _) = products(at);
    let ior_f0 = ((IOR - 1.) / (IOR + 1.)).powi(2);
    color.map(|c| (ior_f0 * c).min(1.) * specular)
}

fn nearest(image: usize) -> MaterialTexture {
    MaterialTexture {
        sampler: Sampler {
            mag_filter: TextureFilter::Nearest,
            min_filter: TextureFilter::Nearest,
            mipmap_filter: None,
            ..Sampler::default()
        },
        ..MaterialTexture::new(image)
    }
}

fn pattern(texel: impl Fn([usize; 2]) -> [u8; 4]) -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(2, 2, |x, y| {
        image::Rgba(texel([x as usize, y as usize]))
    }))
}

/// The square in a dielectric of constant base colour with the patterned
/// occlusion, specular and specular colour maps, `metallic` its metallic
/// factor.
fn mapped(metallic: f32) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![square()];
    asset.images = vec![
        pattern(|at| [texels(at).0, 0, 0, 255]),
        pattern(|at| [0, 0, 0, texels(at).1]),
        pattern(|at| {
            let [r, g, b] = texels(at).2;
            [r, g, b, 255]
        }),
    ];
    asset.materials[0] = Material {
        base: BASE,
        metallic,
        roughness: 0.5,
        ior: IOR,
        specular: SPECULAR_FACTOR,
        specular_color: COLOR_FACTOR,
        occlusion_texture: Some(nearest(0)),
        occlusion_strength: OCCLUSION_STRENGTH,
        specular_texture: Some(nearest(1)),
        specular_color_texture: Some(nearest(2)),
        ..Default::default()
    };
    asset
}

/// The square in the material glTF defines quadrant `at` of `mapped` to
/// be: the specular strength and colour as factors, and the occlusion
/// packed in a one-texel metallic-roughness map whose roughness and
/// metallic channels leave their factors alone.
fn equivalent(at: [usize; 2]) -> Asset {
    let (specular, specular_color, _) = products(at);
    let mut asset = mapped(0.);
    asset.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([texels(at).0, 255, 255, 255]),
    ))];
    let material = &mut asset.materials[0];
    material.specular = specular;
    material.specular_color = specular_color;
    material.mr_texture = Some(nearest(0));
    material.occlusion_texture = Some(nearest(0));
    material.specular_texture = None;
    material.specular_color_texture = None;
    asset
}

/// A frame from the origin looking at the square, lit only by a light off
/// its normal, at N·L 0.6, where an occlusion of 0.44 micro-shadows it.
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
        direction: Vec3::new(0.8, 0., -0.6),
        color: [1.; 3],
        illuminance: 3.,
        shadow: None,
        ..Default::default()
    });
    input
}

fn settings() -> Settings {
    Settings {
        ambient_occlusion: AmbientOcclusionQuality::Off,
        ..Settings::default()
    }
}

/// The pixel at quadrant `[column, row]`'s centre, (±0.5, ±0.5) on the
/// square: 0.5 / (4 tan 0.5) of the half view off its centre.
fn pixel([column, row]: [usize; 2]) -> usize {
    let [x, y] = [column, row].map(|i| if i == 0 { 24 } else { 39 });
    (y * SIZE[0] + x) as usize
}

/// What the opaque stage records at each quadrant's centre: the base
/// lobe's F0 and F90, the material occlusion (gbuffer.wgsl's 8-bit code)
/// and the lit colour.
struct Probe {
    f0: [f32; 3],
    f90: f32,
    occlusion: f32,
    lit: [f32; 3],
}

/// `asset` drawn by the opaque stage on `device`, its probes at the
/// quadrants, and its scene and renderer, or none where the device's tier
/// is not `tier`.
fn shade(
    gpu: (&wgpu::Device, &wgpu::Queue),
    asset: Asset,
    tier: BindingTier,
) -> Option<(Vec<Probe>, Scene, TestRenderer)> {
    shade_with(gpu, asset, tier, false)
}

/// `shade`, its material evaluated through a game's shader that returns the
/// material surface it is given where `shaded`.
fn shade_with(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    asset: Asset,
    tier: BindingTier,
    shaded: bool,
) -> Option<(Vec<Probe>, Scene, TestRenderer)> {
    let settings = settings();
    let mut scene = Scene::new(device, queue);
    let (ids, _) = test_support::add_static(device, queue, &mut scene, asset);
    if shaded {
        let shader = scene
            .add_shader(crate::ShaderSource {
                label: "the material as it is given".into(),
                wgsl: r#"
struct ShaderParams { value:vec4<f32>, }
fn material_vertex(v:MaterialVertex,ctx:VertexContext,p:ShaderParams)->MaterialVertex { return v; }
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,p:ShaderParams)->MaterialSurface { return s; }
"#
                .into(),
            })
            .unwrap();
        test_support::set_shader(&mut scene, queue, ids.materials[0], (shader, 0.));
    }
    let mut renderer = TestRenderer::for_test(device, queue, SIZE, &settings);
    if renderer.binding_tier() != tier {
        eprintln!("skipping: the device takes another binding tier than {tier:?}");
        return None;
    }
    let input = input();
    let mut frame = renderer.prepare_test_frame(device, queue, &mut scene, &input, &settings);
    let mut encoder = device.create_command_encoder(&Default::default());
    let fused = renderer.test_fused_supported();
    renderer.encode_test_opaque(device, queue, &mut encoder, &scene, &mut frame, fused);
    queue.submit([encoder.finish()]);
    let targets = renderer.targets();
    let material = test_support::read(device, queue, targets.material.texture(), 8);
    let f0 = test_support::read(device, queue, targets.f0.texture(), 4);
    let color = test_support::read(device, queue, targets.color.texture(), 8);
    let probes = QUADRANTS
        .map(|at| {
            let p = pixel(at);
            let code = f32::from(f0[p * 4 + 3]);
            Probe {
                f0: [0, 1, 2].map(|c| f32::from(f0[p * 4 + c]) / 255.),
                f90: test_support::half(&material[p * 8 + 6..]),
                // gbuffer.wgsl's code: 1 + occlusion in 126ths, and 127
                // more where the surface takes baked scene lights.
                occlusion: (code - if code >= 128. { 128. } else { 1. }) / 126.,
                lit: [0, 1, 2].map(|c| test_support::half(&color[p * 8 + c * 2..])),
            }
        })
        .into();
    Some((probes, scene, renderer))
}

/// A device of the Basic binding tier: the renderer's limits with 47
/// sampled textures a stage, the most S3D-1 gives Basic.
fn basic_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = test_support::adapter()?;
    let mut limits = crate::graphics_device::limits(&adapter);
    if limits.max_sampled_textures_per_shader_stage < 47 {
        eprintln!("skipping: the adapter binds fewer than 47 sampled textures");
        return None;
    }
    limits.max_sampled_textures_per_shader_stage = 47;
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: crate::graphics_device::features(&adapter),
            required_limits: limits,
            ..Default::default()
        }))
        .unwrap(),
    )
}

/// The F0 code's tolerance: half an 8-bit step and rounding.
const F0_STEP: f32 = 0.5 / 255. + 1e-4;
/// The occlusion code's: half a 126th.
const OCCLUSION_STEP: f32 = 0.5 / 126. + 1e-4;

// Plausible defects: a map read from another channel (the strength from
// red, the occlusion from alpha), the colour map read as linear data, a
// texel applied outside KHR_materials_specular's clamp (min(F0 * colour *
// strength, 1) for min(F0 * colour, 1) * strength, which moves white's
// second row's red from 0.50 to 0.60) or not to F90, a factor dropped, a
// map sampled at another map's texel, or an occlusion map of its own read
// from the metallic-roughness map (white here) or not at all. Oracles: the
// G-buffer's F0, F90 and occlusion at each quadrant against the
// extensions' definitions computed here from the texels and factors; and
// the lit colour under one light against the material glTF defines that
// quadrant to be (the products in the factors, the occlusion packed),
// within 1%: the colour map is decoded from sRGB in hardware, which WebGPU
// does not specify bit for bit. Each quadrant combines the maps otherwise,
// so every map shows apart from the others: the occlusion changes along
// rows where F0 and F90 do not, the strength along columns where the
// occlusion does not.
#[test]
fn occlusion_and_specular_maps_shade_as_their_products() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let Some((probes, mut scene, mut renderer)) =
        shade((&device, &queue), mapped(0.), BindingTier::Extended)
    else {
        return;
    };
    for (at, probe) in QUADRANTS.into_iter().zip(&probes) {
        let (specular, _, occlusion) = products(at);
        let f0 = dielectric_f0(at);
        for c in 0..3 {
            assert!(
                (probe.f0[c] - f0[c]).abs() <= F0_STEP,
                "quadrant {at:?}: F0 {:?}, KHR_materials_specular's {f0:?}",
                probe.f0
            );
        }
        assert!(
            (probe.f90 - specular).abs() <= 1e-3,
            "quadrant {at:?}: F90 {}, the strength {specular}",
            probe.f90
        );
        assert!(
            (probe.occlusion - occlusion).abs() <= OCCLUSION_STEP,
            "quadrant {at:?}: occlusion {}, glTF's {occlusion}",
            probe.occlusion
        );
        let (expected, ..) = shade((&device, &queue), equivalent(at), BindingTier::Extended)
            .expect("the same device");
        let expected = &expected[QUADRANTS.iter().position(|q| *q == at).unwrap()];
        for c in 0..3 {
            assert!(
                expected.lit[c] > 0.01
                    && (probe.lit[c] - expected.lit[c]).abs() <= 0.01 * expected.lit[c],
                "quadrant {at:?}: lit {:?} with the maps, {:?} with their products",
                probe.lit,
                expected.lit
            );
        }
    }

    // A ray hit at each quadrant's centre takes the products themselves.
    let observation = r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {
 let targets=array<vec2<f32>,4>(vec2(-.5,.5),vec2(.5,.5),vec2(-.5,-.5),vec2(.5,-.5));
 for (var q=0u;q<4u;q++) {
  var ray:SceneRay;
  ray.direction=vec4(normalize(vec3(targets[q],-4.)),100.);
  let hit=scene_decode_hit(scene_trace_nearest(ray,SCENE_SIDES_AS_RASTER),ray.origin.xyz,ray.direction.xyz);
  let material=scene_material(hit.material_word);
  let s=ray_surface(hit,material,ray_base_color(hit,material),vec3(0.),-ray.direction.xyz,cluster_range(hit.position,vec2(0.)));
  output[q]=vec4(s.dielectric_f0,s.specular);
  output[4u+q]=vec4(s.occlusion,0.,0.,0.);
 }
}
"#;
    let hits = test_support::observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input(),
        &settings(),
        observation,
        8,
    );
    for (q, at) in QUADRANTS.into_iter().enumerate() {
        let (specular, _, occlusion) = products(at);
        let f0 = dielectric_f0(at);
        let expected = [f0[0], f0[1], f0[2], specular];
        for c in 0..4 {
            assert!(
                (hits[q][c] - expected[c]).abs() <= 2e-3,
                "ray hit at quadrant {at:?}: {:?}, the products {expected:?}",
                hits[q]
            );
        }
        assert!(
            (hits[4 + q][0] - occlusion).abs() <= 2e-3,
            "ray hit at quadrant {at:?}: occlusion {}, glTF's {occlusion}",
            hits[4 + q][0]
        );
    }
}

// Plausible defect: a material with a game's shader drawn from a program
// set that ignores the occlusion and specular maps' key (the shader's set
// composing the fallback provider), so the shader receives and the G-buffer
// records the factors alone. The shader returns the surface it is given;
// the oracle is the extensions' definitions at each quadrant, as for the
// material without a shader.
#[test]
fn a_shader_material_samples_these_maps_too() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let Some((probes, ..)) = shade_with((&device, &queue), mapped(0.), BindingTier::Extended, true)
    else {
        return;
    };
    for (at, probe) in QUADRANTS.into_iter().zip(&probes) {
        let (specular, _, occlusion) = products(at);
        let f0 = dielectric_f0(at);
        for (observed, expected) in probe.f0.iter().zip(f0) {
            assert!(
                (observed - expected).abs() <= F0_STEP,
                "quadrant {at:?}: F0 {:?}, KHR_materials_specular's {f0:?}",
                probe.f0
            );
        }
        assert!(
            (probe.f90 - specular).abs() <= 1e-3
                && (probe.occlusion - occlusion).abs() <= OCCLUSION_STEP,
            "quadrant {at:?}: F90 {} and occlusion {}, the products {specular} and {occlusion}",
            probe.f90,
            probe.occlusion
        );
    }
}

// Plausible defect: a specular map reaching a metal's F0 or F90, where
// KHR_materials_specular leaves the metal BRDF alone. Oracle: the same
// metal without the maps, whose F0 is its base colour and F90 1.
#[test]
fn a_metal_takes_nothing_of_its_specular_maps() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut plain = mapped(1.);
    plain.materials[0].specular_texture = None;
    plain.materials[0].specular_color_texture = None;
    let Some((metal, ..)) = shade((&device, &queue), mapped(1.), BindingTier::Extended) else {
        return;
    };
    let (plain, ..) = shade((&device, &queue), plain, BindingTier::Extended).unwrap();
    for (at, (metal, plain)) in QUADRANTS.into_iter().zip(metal.iter().zip(&plain)) {
        assert!(
            (metal.f90 - 1.).abs() <= 1e-3 && (plain.f90 - 1.).abs() <= 1e-3,
            "quadrant {at:?}: F90 {} with the maps, {} without",
            metal.f90,
            plain.f90
        );
        for ((mapped, unmapped), base) in metal.f0.iter().zip(&plain.f0).zip(BASE) {
            assert!(
                (mapped - base).abs() <= F0_STEP && mapped == unmapped,
                "quadrant {at:?}: F0 {:?} with the maps, {:?} without",
                metal.f0,
                plain.f0
            );
        }
    }
}

// Plausible defects: a Basic device sampling a map it does not bind (or
// binding one past S3D-1's floor and failing), shading an occlusion map of
// its own or the specular maps where they give way to their factors, or
// dropping packed occlusion, which it binds. Oracles: the factors alone,
// glTF's material without those maps (occlusion 1, F0
// min(0.25 * colour factor, 1) * strength factor, F90 the strength factor);
// and, for the packed one, glTF's occlusion of its texel.
#[test]
fn a_basic_device_shades_these_maps_factors_and_keeps_packed_occlusion() {
    let Some((device, queue)) = basic_device() else {
        return;
    };
    let Some((separate, ..)) = shade((&device, &queue), mapped(0.), BindingTier::Basic) else {
        return;
    };
    let ior_f0 = ((IOR - 1.) / (IOR + 1.)).powi(2);
    let f0 = COLOR_FACTOR.map(|c| (ior_f0 * c).min(1.) * SPECULAR_FACTOR);
    for (at, probe) in QUADRANTS.into_iter().zip(&separate) {
        assert!(
            (probe.occlusion - 1.).abs() <= OCCLUSION_STEP
                && (probe.f90 - SPECULAR_FACTOR).abs() <= 1e-3,
            "quadrant {at:?}: occlusion {} and F90 {} on Basic",
            probe.occlusion,
            probe.f90
        );
        for c in 0..3 {
            assert!(
                (probe.f0[c] - f0[c]).abs() <= F0_STEP,
                "quadrant {at:?}: F0 {:?} on Basic, the factors' {f0:?}",
                probe.f0
            );
        }
    }
    let at = [1, 0];
    let (packed, ..) = shade((&device, &queue), equivalent(at), BindingTier::Basic).unwrap();
    let (_, _, occlusion) = products(at);
    let observed = packed[QUADRANTS.iter().position(|q| *q == at).unwrap()].occlusion;
    assert!(
        (observed - occlusion).abs() <= OCCLUSION_STEP,
        "packed occlusion on Basic: {observed}, glTF's {occlusion}"
    );
}

// Plausible defect: a probe capture's faces, which are raster but bind
// their own passes, dropping an occlusion map of its own or a specular map
// (or reading them where another map samples). The capture holds the
// square in one combination of the maps (one-texel images: the coloured
// texel, the second strength and the occluding texel) under the light and
// the hemisphere's ambient light, which the capture occludes by the
// material's occlusion. The oracle is equivalence: the capture's sum of
// the same square in the material glTF defines it to be (the products in
// the factors, the occlusion packed), within 0.5%. The material without
// the maps differs from it by more than 5%, so a dropped map shows.
#[test]
fn a_capture_shades_these_maps_as_their_products() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let at = [1, 1];
    let one_texel = |asset: Asset| {
        let mut asset = asset;
        let (occlusion, specular, color) = texels(at);
        if asset.materials[0].specular_texture.is_some() {
            asset.images = vec![
                Image::Rgba8(image::RgbaImage::from_pixel(
                    1,
                    1,
                    image::Rgba([occlusion, 0, 0, 255]),
                )),
                Image::Rgba8(image::RgbaImage::from_pixel(
                    1,
                    1,
                    image::Rgba([0, 0, 0, specular]),
                )),
                Image::Rgba8(image::RgbaImage::from_pixel(
                    1,
                    1,
                    image::Rgba([color[0], color[1], color[2], 255]),
                )),
            ];
        }
        asset
    };
    let captured = |asset: Asset| -> Option<f32> {
        use crate::baked_specular_probe::SpecularProbeTexels;
        let settings = settings();
        let mut scene = Scene::new(&device, &queue);
        test_support::add_static(&device, &queue, &mut scene, asset);
        let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
        if renderer.binding_tier() != BindingTier::Extended {
            eprintln!("skipping: the device takes the Basic binding tier");
            return None;
        }
        let mut input = input();
        input.hemisphere_light.intensity = 1.;
        let radiance = renderer
            .capture_specular_probe(
                &device,
                &queue,
                &mut scene,
                &input,
                &settings,
                Vec3::ZERO,
                64,
            )
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F")
        };
        Some(
            texels
                .chunks_exact(4)
                .map(|texel| {
                    (0..3)
                        .map(|c| test_support::half(&texel[c].to_le_bytes()))
                        .sum::<f32>()
                })
                .sum(),
        )
    };
    let Some(mapped) = captured(one_texel(mapped(0.))) else {
        return;
    };
    let equivalent = captured(equivalent(at)).unwrap();
    let plain = captured(mapped_plain()).unwrap();
    assert!(
        (plain - equivalent).abs() > 0.05 * equivalent,
        "the fixture: {plain} without the maps, {equivalent} with their products"
    );
    assert!(
        (mapped - equivalent).abs() <= 0.005 * equivalent,
        "the capture holds {mapped} with the maps and {equivalent} with their products"
    );
}

// Plausible defects: an occlusion map of its own (binding 16, past the
// record's sixteen) read at the first UV set, without its transform, in
// the bound sampler state, or through another binding's set, transform or
// state (the word's bits for 16 read for 17, or the reverse); in raster or
// at a ray hit, whose second set no other map asks the hit to decode. The
// occlusion map is a 4×4 image of distinct reds on the second set, which is
// 5 × the first - 1, through KHR_texture_transform's first worked example
// (reading the image at (0.5 v, 1 - 0.5 u)), clamped to its edges, while the
// specular maps sample the first set untransformed in the other state, the
// one bound. At quadrant [column, row] the second set is
// (0.25 + 2.5 column, 0.25 + 2.5 row), so the transform reads (0.125 + 1.25
// row, 0.875 - 1.25 column): texel (3 row, 3 (1 - column)) once clamped,
// another texel under repeat, under no transform or on the first set.
// Oracles: glTF's occlusion of that texel and KHR_materials_specular's F0
// of the specular maps' texels, from the G-buffer and at a ray hit.
#[test]
fn an_occlusion_map_samples_its_own_set_transform_and_state() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let red = |[column, row]: [u32; 2]| 255 - 16 * (4 * row + column) as u8;
    let mut asset = mapped(0.);
    asset.images[0] = Image::Rgba8(image::RgbaImage::from_fn(4, 4, |x, y| {
        image::Rgba([red([x, y]), 0, 0, 255])
    }));
    for vertex in &mut asset.meshes[0].vertices {
        vertex.uv1 = vertex.uv.map(|c| 5. * c - 1.);
    }
    let occlusion = nearest(0);
    asset.materials[0].occlusion_texture = Some(MaterialTexture {
        sampler: Sampler {
            wrap: [TextureWrap::ClampToEdge; 2],
            ..occlusion.sampler
        },
        uv_set: UvSet::Uv1,
        transform: UvTransform {
            offset: [0., 1.],
            rotation: std::f32::consts::FRAC_PI_2,
            scale: [0.5, 0.5],
        },
        ..occlusion
    });
    let expected = |[column, row]: [usize; 2]| {
        let texel = [3 * row as u32, 3 * (1 - column as u32)];
        1. + OCCLUSION_STRENGTH * (f32::from(red(texel)) / 255. - 1.)
    };
    let Some((probes, mut scene, mut renderer)) =
        shade((&device, &queue), asset, BindingTier::Extended)
    else {
        return;
    };
    for (at, probe) in QUADRANTS.into_iter().zip(&probes) {
        let f0 = dielectric_f0(at);
        for (observed, wanted) in probe.f0.iter().zip(f0) {
            assert!(
                (observed - wanted).abs() <= F0_STEP,
                "quadrant {at:?}: F0 {:?}, KHR_materials_specular's {f0:?}",
                probe.f0
            );
        }
        assert!(
            (probe.occlusion - expected(at)).abs() <= OCCLUSION_STEP,
            "quadrant {at:?}: occlusion {}, glTF's {}",
            probe.occlusion,
            expected(at)
        );
    }
    let observation = r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {
 let targets=array<vec2<f32>,4>(vec2(-.5,.5),vec2(.5,.5),vec2(-.5,-.5),vec2(.5,-.5));
 for (var q=0u;q<4u;q++) {
  var ray:SceneRay;
  ray.direction=vec4(normalize(vec3(targets[q],-4.)),100.);
  let hit=scene_decode_hit(scene_trace_nearest(ray,SCENE_SIDES_AS_RASTER),ray.origin.xyz,ray.direction.xyz);
  let material=scene_material(hit.material_word);
  let s=ray_surface(hit,material,ray_base_color(hit,material),vec3(0.),-ray.direction.xyz,cluster_range(hit.position,vec2(0.)));
  output[q]=vec4(s.dielectric_f0,s.occlusion);
 }
}
"#;
    let hits = test_support::observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input(),
        &settings(),
        observation,
        4,
    );
    for (hit, at) in hits.iter().zip(QUADRANTS) {
        let f0 = dielectric_f0(at);
        let wanted = [f0[0], f0[1], f0[2], expected(at)];
        for (observed, value) in hit.iter().zip(wanted) {
            assert!(
                (observed - value).abs() <= 2e-3,
                "ray hit at quadrant {at:?}: F0 and occlusion {hit:?}, glTF's {wanted:?}"
            );
        }
    }
}

// Plausible defects: a material with one of these maps in effect added or
// removed without the scene's count following, so ray hits sample them in
// a scene without them (`LitConstants::occlusion_specular`, the cost a
// scene without them must not pay) or skip them while a material has one;
// or packed occlusion, or a Basic device's maps that give way to their
// factors, counted as such a map. The oracle is the scene's materials: it
// holds one exactly while a material's own occlusion, specular or specular
// colour map is in effect.
#[test]
fn the_scene_holds_these_maps_while_a_material_has_one_in_effect() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let packed = scene
        .add_asset(&device, &queue, equivalent([0, 0]))
        .unwrap();
    let holds = |scene: &Scene| crate::view::pipelines::LitConstants::of(scene).occlusion_specular;
    assert!(!holds(&scene), "packed occlusion alone");
    let mut lone = mapped_plain();
    lone.images = mapped(0.).images;
    lone.materials[0].specular_color_texture = Some(nearest(2));
    let lone = scene.add_asset(&device, &queue, lone).unwrap();
    assert!(holds(&scene), "added with a specular colour map");
    scene.remove_model(lone.model).unwrap();
    scene.remove_material(lone.materials[0]).unwrap();
    assert!(!holds(&scene), "removed");
    scene.remove_model(packed.model).unwrap();
    scene.remove_material(packed.materials[0]).unwrap();
    let Some((device, queue)) = basic_device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    scene.add_asset(&device, &queue, mapped(0.)).unwrap();
    assert!(!holds(&scene), "on Basic, where the maps give way");
}

/// The square in `mapped`'s factors without its maps.
fn mapped_plain() -> Asset {
    let mut asset = mapped(0.);
    let material = &mut asset.materials[0];
    material.occlusion_texture = None;
    material.specular_texture = None;
    material.specular_color_texture = None;
    asset.images.clear();
    asset
}
