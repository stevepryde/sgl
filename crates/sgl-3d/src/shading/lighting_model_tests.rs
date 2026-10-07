//! SGL3D's lighting model (D-32) against independent oracles: a white metal
//! conserves a white furnace's energy under direct light; direct light and
//! the environment reflect the same energy on every channel; one material
//! reflects the same irradiance alike whatever holds it; and a sized light's
//! highlight against the light its sphere or disc gives. Each observes the
//! production shading on the GPU and integrates or compares in f64.
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::static_lighting::{IrradianceAtlas, Lightmap};
use crate::{
    Backdrop, Camera, DirectionalLight, FrameInput, HemisphereLight, IrradianceCell,
    IrradianceVolume, Light, LightShape, Scene, asset, test_support,
};
use glam::{DVec3, Mat4, Vec3};
use std::f64::consts::{FRAC_PI_2, PI};

/// The hemisphere of directions each integral sums over: steps in polar
/// angle from the normal and in azimuth, and the invocations that share them.
/// A grid this fine integrates GGX's directional albedo to within 0.015% at
/// perceptual roughness 0.25 and above (checked against a 2^18-sample
/// Hammersley integral).
const THETA_STEPS: usize = 512;
const PHI_STEPS: usize = 1024;
const THREADS: usize = 256;

/// The solid angle of one grid cell at polar angle `theta`.
fn cell(theta: f64) -> f64 {
    theta.sin() * (FRAC_PI_2 / THETA_STEPS as f64) * (2. * PI / PHI_STEPS as f64)
}

/// The grid's polar angle and azimuth at row `row` and column `column`.
fn grid(row: usize, column: usize) -> (f64, f64) {
    (
        (row as f64 + 0.5) / THETA_STEPS as f64 * FRAC_PI_2,
        (column as f64 + 0.5) / PHI_STEPS as f64 * 2. * PI,
    )
}

fn polar(theta: f64, phi: f64) -> DVec3 {
    DVec3::new(
        theta.sin() * phi.cos(),
        theta.sin() * phi.sin(),
        theta.cos(),
    )
}

/// WGSL constants and helpers the integrals share: the grid, and a surface
/// facing +Z at the origin seen along `view` with perceptual roughness
/// `rough`, base colour `base` and metallic `metallic`, a dielectric of F0
/// 0.04 and F90 1 where it is not metal.
fn grid_wgsl() -> String {
    format!(
        r#"
const THETA_STEPS:u32={THETA_STEPS}u;
const PHI_STEPS:u32={PHI_STEPS}u;
const THREADS:u32={THREADS}u;
fn grid_direction(row:u32,column:u32)->vec3<f32> {{
 let theta=(f32(row)+.5)/f32(THETA_STEPS)*1.5707963267948966;
 let phi=(f32(column)+.5)/f32(PHI_STEPS)*6.283185307179586;
 return vec3(sin(theta)*cos(phi),sin(theta)*sin(phi),cos(theta));
}}
fn case_surface(view:vec3<f32>,rough:f32,base:vec3<f32>,metallic:f32)->Surface {{
 var s:Surface;
 s.normal=vec3(0.,0.,1.);
 s.geometry_normal=s.normal;
 s.coat_normal=s.normal;
 s.view=view;
 s.roughness=rough;
 s.base=vec4(base,1.);
 s.metallic=metallic;
 s.dielectric_f0=vec3(.04);
 s.specular=1.;
 s.environment_scale=1.;
 s.occlusion=1.;
 s.front=true;
 return s;
}}
fn case_reflectance(s:Surface)->SurfaceReflectance {{
 return surface_reflectance(s,surface_dfg(specular_nv(s.normal,s.view),s.roughness));
}}
"#
    )
}

/// One furnace: a surface facing +Z seen along `view`, under unit lights from
/// every direction of the hemisphere whose specular lobes `specular` scales,
/// under an iridescent film of strength, IOR and thickness in nanometres
/// `film`.
#[derive(Clone, Copy, Debug)]
struct Furnace {
    view: DVec3,
    rough: f64,
    base: [f64; 3],
    metallic: f64,
    specular: f64,
    film: [f64; 3],
}

const NO_FILM: [f64; 3] = [0., 1.3, 0.];

/// What each furnace's surface reflects: under its lights, integrated over
/// the hemisphere (the production surface_direct_light, summed by the GPU
/// over each grid row and in f64 here), and under a uniform white
/// environment (the environment path's response: the base lobe's split sum,
/// its multiple scattering and the diffuse weight, at radiance 1).
fn furnaces(cases: &[Furnace]) -> Option<Vec<(DVec3, DVec3)>> {
    let (device, queue) = test_support::device()?;
    let scene = Scene::new(&device, &queue);
    let packed: Vec<[[f32; 4]; 3]> = cases
        .iter()
        .map(|c| {
            [
                c.view.as_vec3().extend(c.rough as f32).to_array(),
                [c.base[0], c.base[1], c.base[2], c.metallic].map(|v| v as f32),
                [c.specular, c.film[0], c.film[1], c.film[2]].map(|v| v as f32),
            ]
        })
        .collect();
    let observation = format!(
        r#"{}
struct Case {{ view:vec4<f32>,base:vec4<f32>,light:vec4<f32> }}
@group(0) @binding(3) var<storage,read> cases:array<Case>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size({THREADS}) fn observe(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) thread:u32) {{
 let c=cases[group.x];
 var s=case_surface(c.view.xyz,c.view.w,c.base.rgb,c.base.w);
 s.iridescence=c.light.y;
 s.iridescence_ior=c.light.z;
 s.iridescence_thickness=c.light.w;
 let reflectance=case_reflectance(s);
 for (var row=thread;row<THETA_STEPS;row+=THREADS) {{
  var ring=vec3(0.);
  for (var column=0u;column<PHI_STEPS;column++) {{
   let light=LightSample(grid_direction(row,column),vec3(1.),1.,c.light.x,NO_RECT_LIGHT,0.);
   ring+=surface_direct_light(s,reflectance,light);
  }}
  result[group.x*THETA_STEPS+row]=vec4(ring,0.);
 }}
 if thread==0u {{
  let dfg=reflectance.view_dfg;
  let lobes=specular_lobes(s.normal,s.normal,s.view,reflectance.f0,reflectance.f90,s.roughness,dfg,0.,0.,vec4(0.),lookup_tables,environment_sampler);
  let ibl=surface_ibl_weights(s,reflectance);
  result[arrayLength(&cases)*THETA_STEPS+group.x]=vec4(lobes[0].response+ibl.multi+ibl.diffuse,0.);
 }}
}}
"#,
        grid_wgsl()
    );
    let rows = test_support::observe_surface(
        &device,
        &queue,
        &scene,
        bytemuck::cast_slice(&packed),
        &observation,
        cases.len() as u32,
        cases.len() * (THETA_STEPS + 1),
    );
    let vector = |a: [f32; 4]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    Some(
        (0..cases.len())
            .map(|case| {
                let direct = (0..THETA_STEPS)
                    .map(|row| vector(rows[case * THETA_STEPS + row]) * cell(grid(row, 0).0))
                    .sum();
                (direct, vector(rows[cases.len() * THETA_STEPS + case]))
            })
            .collect(),
    )
}

/// The unit view at cosine `nv` to +Z, in the XZ plane.
fn view(nv: f64) -> DVec3 {
    DVec3::new((1. - nv * nv).sqrt(), 0., nv)
}

/// The share of light a white dielectric of F0 0.04 and F90 1 seen along
/// `view` diffuses under unit lights from the whole hemisphere: glTF 2.0's
/// dielectric BRDF (Appendix B, fresnel_mix of a Lambertian base under the
/// specular layer, its Fresnel at v.h), integrated in f64.
fn coupled_diffuse(view: DVec3) -> f64 {
    let mut sum = 0.;
    for row in 0..THETA_STEPS {
        for column in 0..PHI_STEPS {
            let (theta, phi) = grid(row, column);
            let l = polar(theta, phi);
            let vh = view.dot((view + l).normalize()).clamp(0., 1.);
            let fresnel = 0.04 + 0.96 * (1. - vh).powi(5);
            sum += (1. - fresnel) / PI * l.z * cell(theta);
        }
    }
    sum
}

// Plausible defects: direct light's multiple scattering loses energy (three.js
// r185's heuristic reflected 0.90, 0.84 and 0.72 at roughness 0.5, 0.75 and
// 1 seen at N.V 0.5, and 0.90 at roughness 0.25 near grazing), takes no
// gain, or reads a DFG table that cannot reach roughness 1 (three.js's 16 ×
// 16 gave 0.89); a dielectric's diffuse takes no Fresnel coupling, or takes
// it at N.V or N.L in place of V.H. The oracles are energy conservation: a
// white metal with multiple scattering reflects all a white furnace gives it
// (Heitz et al. 2016), and glTF's dielectric BRDF integrated in f64. The
// metal's bound is the DFG table's accuracy: within 0.5% up to roughness
// 0.95 at N.V from 0.05 (Bevy's 64 × 64 table against f64 integrals), 3% at
// roughness 1, whose last texel holds roughness 0.992. The dielectric's is
// f32 accumulation.
#[test]
fn a_white_furnace_conserves_energy_under_direct_light() {
    let views = [0.1, 0.3, 0.5, 0.7, 0.9, 1.];
    let mut cases = Vec::new();
    for rough in [0.25, 0.5, 0.75, 1.] {
        for &nv in &views {
            cases.push(Furnace {
                view: view(nv),
                rough,
                base: [1.; 3],
                metallic: 1.,
                specular: 1.,
                film: NO_FILM,
            });
        }
    }
    let first_dielectric = cases.len();
    for &nv in &views {
        cases.push(Furnace {
            view: view(nv),
            rough: 0.5,
            base: [1.; 3],
            metallic: 0.,
            specular: 0.,
            film: NO_FILM,
        });
    }
    let Some(observed) = furnaces(&cases) else {
        return;
    };
    let mut worst = [0f64; 3];
    for (case, (direct, _)) in cases[..first_dielectric].iter().zip(&observed) {
        let bound = if case.rough < 1. { 0.01 } else { 0.035 };
        let deviation = (*direct - DVec3::ONE).abs().max_element();
        let at = usize::from(case.rough == 1.);
        worst[at] = worst[at].max(deviation);
        assert!(
            (*direct - DVec3::ONE).abs().max_element() <= bound,
            "white metal {case:?}: reflects {direct:?} of a white furnace"
        );
    }
    for (case, (direct, _)) in cases[first_dielectric..]
        .iter()
        .zip(&observed[first_dielectric..])
    {
        let expected = coupled_diffuse(case.view);
        worst[2] = worst[2].max((*direct - DVec3::splat(expected)).abs().max_element() / expected);
        assert!(
            (*direct - DVec3::splat(expected)).abs().max_element() <= 0.001 * expected,
            "white dielectric under diffuse-only lights {case:?}: {direct:?}, glTF {expected}"
        );
    }
    eprintln!(
        "white furnace: metal within {:.5} below roughness 1 and {:.5} at 1; dielectric within {:.6} of glTF",
        worst[0], worst[1], worst[2]
    );
}

// Plausible defects: a film's reflectance taken by the specular lobes but
// not from the diffuse beneath them, under the environment (its diffuse
// weight at the bare dielectric's F0) or under lights (coupled at the bare
// dielectric's Fresnel); or the diffuse kept by the film's weakest channel,
// or each channel's, in place of its strongest. The oracles: a film that
// absorbs nothing over a white base reflects all a white environment gives
// it; and under diffuse-only lights from the whole hemisphere a dielectric
// under a film keeps 1 less the strongest channel of the film's Fresnel at
// N.V (KHR_materials_iridescence's rgb_mix), the film the exact thin-film
// sum (iridescence_tests::thin_film). The environment's bound is f32
// rounding; the lights' is the thin film's (0.01) and the grid's.
#[test]
fn a_white_furnace_conserves_energy_under_a_film() {
    let film = [1., 1.8, 400.];
    let cases: Vec<Furnace> = [1., 0.7]
        .into_iter()
        .map(|nv| Furnace {
            view: view(nv),
            rough: 0.5,
            base: [1.; 3],
            metallic: 0.,
            specular: 0.,
            film,
        })
        .collect();
    let Some(observed) = furnaces(&cases) else {
        return;
    };
    for (case, (direct, environment)) in cases.iter().zip(&observed) {
        assert!(
            (*environment - DVec3::ONE).abs().max_element() <= 1e-4,
            "white dielectric under a film {case:?}: reflects {environment:?} of a white environment"
        );
        let thin = super::iridescence_tests::thin_film(film[1], 1.5, film[2], case.view.z);
        let kept = 1. - thin.into_iter().fold(0., f64::max);
        assert!(
            (*direct - DVec3::splat(kept)).abs().max_element() <= 0.011,
            "white dielectric under a film and diffuse-only lights {case:?}: {direct:?}, rgb_mix {kept}"
        );
    }
}

// Plausible defects: direct light and the environment scatter a metal's
// light by different models, so a rough metal is darker or brighter and
// shifts hue under the sun against the sky (three.js r185's direct heuristic
// against its environment's Fdez-Agüera: 10–35% darker; Filament's
// 1 + F0 (1/E - 1) against it: up to 19% brighter on iron at roughness 1);
// the lobe's Fresnel or visibility differs from what the DFG table
// integrates. The oracle is S3D-5's one model: the same uniform light, as
// lights from every direction or as the environment, reflects the same
// energy on every channel. The bound is the DFG table's accuracy, as above.
#[test]
fn direct_light_and_the_environment_reflect_alike() {
    let metals = [
        ("gold", [1., 0.766, 0.336]),
        ("copper", [0.955, 0.638, 0.538]),
        ("iron", [0.56, 0.57, 0.58]),
    ];
    let mut cases = Vec::new();
    for (_, base) in metals {
        for rough in [0.25, 0.5, 0.75, 1.] {
            for nv in [0.3, 0.7] {
                cases.push(Furnace {
                    view: view(nv),
                    rough,
                    base,
                    metallic: 1.,
                    specular: 1.,
                    film: NO_FILM,
                });
            }
        }
    }
    let Some(observed) = furnaces(&cases) else {
        return;
    };
    let mut worst = [0f64; 2];
    for (case, (direct, environment)) in cases.iter().zip(observed) {
        let bound = if case.rough < 1. { 0.01 } else { 0.035 };
        let ratio = direct / environment;
        let at = usize::from(case.rough == 1.);
        worst[at] = worst[at].max((ratio - DVec3::ONE).abs().max_element());
        assert!(
            (ratio - DVec3::ONE).abs().max_element() <= bound,
            "{case:?}: direct {direct:?} against the environment's {environment:?}"
        );
    }
    eprintln!(
        "direct against environment: within {:.5} below roughness 1 and {:.5} at 1",
        worst[0], worst[1]
    );
}

/// A camera at the origin looking down -Z, with no light, fill or
/// environment, and baked lighting on.
fn dark_input() -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.; 3],
        ground_color: [0.; 3],
        intensity: 0.,
    };
    input.baked_lighting = true;
    input
}

fn quiet_settings() -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        world_space_reflections: settings::WorldSpaceReflections::Off,
        dynamic_gi: settings::DynamicGiQuality::Off,
        ..Settings::default()
    }
}

/// The irradiance / PI every source holds in `one_rule_for_every_indirect_source`.
const IRRADIANCE: f32 = 0.5;

// Plausible defects: a source of indirect irradiance weights a material's
// diffuse by its own rule (lightmaps and atlas charts by 1 - F0 without
// multiple scattering, the hemisphere fill by 1, against the environment's
// and volumes' 1 - E plus multiple scattering), or leaves the metal's
// multiple scattering out. The oracle is the Surface shading contract: a
// surface reflects the same irradiance alike whatever holds it. A grey
// dielectric and a rough gold, seen at N.V 0.3 where 1 - F0 and 1 - E part
// by 6%, take irradiance / PI of 0.5 from the environment, the hemisphere
// fill, the irradiance volume, a lightmap, an irradiance atlas chart and a
// moving instance's ambient cube in turn; all match the environment's
// within binary16 storage and the environment's filtering, 0.5%.
#[test]
fn one_rule_for_every_indirect_source() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = quiet_settings();
    let mut renderer = Renderer::for_test(&device, &queue, [16, 16], &settings);
    let mut scene = Scene::new(&device, &queue);
    // A static square whose material takes the lightmap, and the chart the
    // atlas lights.
    let mut model = test_support::cube();
    model.meshes[0].vertices = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
        .map(|(x, z)| asset::Vertex {
            tangent: [0.; 4],
            position: [x, -50., z],
            normal: [0., 1., 0.],
            uv: [0.5; 2],
            color: [1.; 4],
            lightmap_uv: [0.5; 2],
            lightmap_bounds: [0., 0., 1., 1.],
        })
        .to_vec();
    model.meshes[0].indices = vec![0, 1, 2, 0, 2, 3];
    let (world, _) = test_support::add_static(&device, &queue, &mut scene, model);
    let irradiance = [IRRADIANCE; 3];
    scene
        .set_lightmap(
            &device,
            &queue,
            &Lightmap {
                size: [2, 2],
                uv_scale_offset: [1., 1., 0., 0.],
                irradiance: vec![irradiance; 4],
                directionality: vec![],
            },
            &world.materials,
        )
        .unwrap();
    scene
        .set_static_irradiance_atlas(
            &device,
            &queue,
            &IrradianceAtlas {
                size: [4, 4],
                irradiance: vec![irradiance; 16],
                back_irradiance: vec![irradiance; 16],
                directionality: vec![],
                back_directionality: vec![],
            },
        )
        .unwrap();
    // A volume about the receiver at (0, 0, 10), away from the others at the
    // origin.
    let volume = IrradianceVolume {
        origin: Vec3::new(-2., -2., 8.),
        cell_size: Vec3::splat(1.),
        cells: [4, 4, 4],
    };
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    let cells = vec![
        IrradianceCell {
            irradiance: crate::static_lighting::AmbientCube {
                irradiance: [irradiance; 6],
            },
            sky_visibility: [0.; 6],
        };
        64
    ];
    let region = crate::PreparedIrradianceRegion::new(volume.origin, volume.cells, &cells).unwrap();
    scene.write_irradiance_cells(&queue, &region).unwrap();
    let texel: Vec<u8> = [IRRADIANCE, IRRADIANCE, IRRADIANCE, 1.]
        .iter()
        .flat_map(|&value| test_support::to_half(value).to_le_bytes())
        .collect();
    let environment = scene
        .add_environment(
            &device,
            &queue,
            &test_support::environment([255; 4], &texel),
        )
        .unwrap();
    // Each source: what the frame lights and how the receiver takes it.
    // (name, environment, hemisphere fill, receiver position, lightmapped,
    // charted, moving)
    let sources = [
        ("environment", true, false, Vec3::ZERO, false, false, false),
        (
            "hemisphere fill",
            false,
            true,
            Vec3::ZERO,
            false,
            false,
            false,
        ),
        (
            "irradiance volume",
            false,
            false,
            Vec3::new(0., 0., 10.),
            false,
            false,
            false,
        ),
        ("lightmap", false, false, Vec3::ZERO, true, false, false),
        (
            "irradiance atlas",
            false,
            false,
            Vec3::ZERO,
            false,
            true,
            false,
        ),
        ("ambient cube", false, false, Vec3::ZERO, false, false, true),
    ];
    let mut observed = Vec::new();
    for (name, lit_by_environment, filled, position, lightmapped, charted, moving) in sources {
        let mut input = dark_input();
        if lit_by_environment {
            input.environment = Some(environment);
        }
        if filled {
            input.hemisphere_light = HemisphereLight {
                sky_color: [1.; 3],
                ground_color: [1.; 3],
                intensity: IRRADIANCE * std::f32::consts::PI,
            };
        }
        let observation = format!(
            r#"{}
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let view=normalize(vec3(.95394,.3,0.));
 for (var metal=0u;metal<2u;metal++) {{
  let gold=metal==1u;
  var s=case_surface(view,select(.5,.6,gold),select(vec3(.5),vec3(1.,.766,.336),gold),select(0.,1.,gold));
  s.normal=vec3(0.,1.,0.);
  s.geometry_normal=s.normal;
  s.coat_normal=s.normal;
  s.position=vec3({:?},{:?},{:?});
  s.baked={lightmapped};
  s.moving={moving};
  s.lightmap_uv=select(vec2(-1.),vec2(.5),{lightmapped}||{charted});
  s.lightmap_bounds=vec4(0.,0.,1.,1.);
  for (var face=0u;face<6u;face++) {{
   s.baked_irradiance[face]=vec4({IRRADIANCE:?});
  }}
  let context=ShadeContext(vec2(0.),SHADOW_RECEIVER_CAMERA,false,false,cluster_range(s.position,vec2(0.)),untraced_reflection());
  output[metal]=vec4(shade_lit(s,context).color,0.);
 }}
}}
"#,
            grid_wgsl(),
            position.x,
            position.y,
            position.z,
        );
        let rows = test_support::observe_ray_hits(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            &observation,
            2,
        );
        observed.push((name, rows));
    }
    let (_, reference) = &observed[0];
    assert!(reference.iter().all(|c| c[0] > 0.01), "{reference:?}");
    for (name, rows) in &observed[1..] {
        for (material, (row, expected)) in ["grey dielectric", "rough gold"]
            .iter()
            .zip(rows.iter().zip(reference))
        {
            for channel in 0..3 {
                assert!(
                    (row[channel] - expected[channel]).abs() <= 0.005 * expected[channel],
                    "{material} under the {name}: {row:?}, under the environment {expected:?}"
                );
            }
        }
    }
}

/// The light a white metal (F0 1) facing +Z at perceptual roughness `rough`
/// reflects toward `view` from a disc of directions about unit `centre`, of
/// angular radius `radius` and uniform radiance `radiance`: GGX with
/// height-correlated Smith visibility, integrated over the disc in f64.
fn disc_reference(view: DVec3, rough: f64, centre: DVec3, radius: f64, radiance: f64) -> f64 {
    let a2 = rough.powi(4);
    let tangent = centre.cross(DVec3::Y).normalize();
    let bitangent = centre.cross(tangent);
    let steps = 1000;
    let mut sum = 0.;
    for i in 0..steps {
        let theta = (i as f64 + 0.5) / steps as f64 * radius;
        for j in 0..steps {
            let phi = (j as f64 + 0.5) / steps as f64 * 2. * PI;
            let l =
                centre * theta.cos() + (tangent * phi.cos() + bitangent * phi.sin()) * theta.sin();
            let (nl, nv) = (l.z, view.z);
            if nl <= 0. {
                continue;
            }
            let nh = (view + l).normalize().z;
            let d = a2 / (PI * (nh * nh * (a2 - 1.) + 1.).powi(2));
            let visibility = 0.5
                / (nl * (nv * nv * (1. - a2) + a2).sqrt() + nv * (nl * nl * (1. - a2) + a2).sqrt());
            let solid_angle = theta.sin() * (radius / steps as f64) * (2. * PI / steps as f64);
            sum += d * visibility * nl * solid_angle;
        }
    }
    sum * radiance
}

/// A sized light's highlight on a white metal facing +Z: what it reflects
/// along `peak_view`, the mirror of the light's centre, and its energy, the
/// reflected light integrated over the hemisphere of views (each weighted by
/// its cosine). `light` draws the LightSample for a surface `s` in WGSL.
fn highlight(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    rough: f64,
    peak_view: DVec3,
    light: &str,
) -> (f64, f64) {
    let observation = format!(
        r#"{}
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
fn reflected(view:vec3<f32>)->f32 {{
 let s=case_surface(view,{rough:?},vec3(1.),1.);
 let reflectance=case_reflectance(s);
 return surface_direct_light(s,reflectance,{light}).g;
}}
@compute @workgroup_size({THREADS}) fn observe(@builtin(local_invocation_index) thread:u32) {{
 for (var row=thread;row<THETA_STEPS;row+=THREADS) {{
  var ring=0.;
  for (var column=0u;column<PHI_STEPS;column++) {{
   let view=grid_direction(row,column);
   ring+=reflected(view)*view.z;
  }}
  output[row]=vec4(ring,0.,0.,0.);
 }}
 if thread==0u {{
  output[THETA_STEPS]=vec4(reflected(vec3({:?},{:?},{:?})),0.,0.,0.);
 }}
}}
"#,
        grid_wgsl(),
        peak_view.x as f32,
        peak_view.y as f32,
        peak_view.z as f32,
    );
    let rows = test_support::observe_ray_hits(
        device,
        queue,
        renderer,
        scene,
        input,
        &quiet_settings(),
        &observation,
        THETA_STEPS + 1,
    );
    let energy = (0..THETA_STEPS)
        .map(|row| rows[row][0] as f64 * cell(grid(row, 0).0))
        .sum();
    (rows[THETA_STEPS][0] as f64, energy)
}

// Plausible defects: a light's radius or the directional light's disc leaves
// its highlight a point (a mirror-like surface then shows a sphere 8 to
// 3600 times too bright at its centre), widens it without Karis's
// normalisation (7 to 4700 times the energy), or widens it by its
// normal-incidence angle alone, which reflects about 1/cos of the energy of
// a light at an elevation from the normal (1.2 times it at 0.6 rad, 1.8 at
// 1.0, 3.7 at 1.3). The oracles are the sphere's or disc's own light: along
// the mirror direction of its centre, the f64 integral of GGX over the
// directions it fills at its radiance (a sphere of intensity I and radius r
// has radiance I / (π r²), a disc of illuminance E and angular radius θ,
// E / (π sin² θ)), and in all, its irradiance at the surface (I cos θ / d²,
// exact for a sphere above the horizon, and E cos θ), which a white metal
// reflects whole. The representative point is an approximation: against
// those integrals the shipped widening measured 0.61–0.98 along the mirror
// direction and 0.87–0.98 in energy over these cases at elevations 0.6, 1.0
// and 1.3 rad (f64), so the bounds are 0.55–1.1 and 0.8–1.1, which also
// hold the multiple-scattering gain and f32.
#[test]
fn a_sized_light_spreads_its_highlight_over_its_sphere_or_disc() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = quiet_settings();
    let mut renderer = Renderer::for_test(&device, &queue, [16, 16], &settings);
    let within = |ratio: f64, low: f64, high: f64| (low..=high).contains(&ratio);
    for elevation in [0.6f64, 1.0, 1.3] {
        let centre = DVec3::new(elevation.sin(), 0., elevation.cos());
        let mirror = DVec3::new(-centre.x, 0., centre.z);
        // (perceptual roughness, radius over distance)
        for (rough, size) in [(0.045, 0.05), (0.1, 0.05), (0.1, 0.2), (0.25, 0.2)] {
            let distance = 2.;
            let radius = size * distance;
            let mut scene = Scene::new(&device, &queue);
            scene
                .add_light(
                    &device,
                    &queue,
                    Light {
                        position: (centre * distance).as_vec3(),
                        shape: LightShape::Point {
                            radius: radius as f32,
                        },
                        color: [1.; 3],
                        intensity: 1.,
                        range: 1000.,
                        baked: false,
                        specular: 1.,
                        casts_shadow: false,
                        ..Default::default()
                    },
                )
                .unwrap();
            let (peak, energy) = highlight(
                (&device, &queue),
                &mut renderer,
                &mut scene,
                &dark_input(),
                rough,
                mirror,
                "scene_light_sample(0u,s.position,s.normal,s.geometry_normal,vec2(0.),SHADOW_RECEIVER_CAPTURE)",
            );
            let angular = (radius / distance).asin();
            let expected =
                disc_reference(mirror, rough, centre, angular, 1. / (PI * radius * radius));
            let irradiance = centre.z / (distance * distance);
            eprintln!(
                "sphere at elevation {elevation}, roughness {rough}, radius/distance {size}: mirror {:.3} of the sphere's, energy {:.3}",
                peak / expected,
                energy / irradiance
            );
            // Where the sphere is wider than the lobe, a mirror-like surface
            // shows it; on a rougher one the lobe blurs it and the energy
            // alone is the oracle.
            assert!(
                rough > 0.1 || within(peak / expected, 0.55, 1.1),
                "sphere at elevation {elevation}, roughness {rough}, radius/distance {size}: {peak} along the mirror, the sphere gives {expected}"
            );
            assert!(
                within(energy / irradiance, 0.8, 1.1),
                "sphere at elevation {elevation}, roughness {rough}, radius/distance {size}: reflects {energy} of irradiance {irradiance}"
            );
        }
        // The directional light's disc, at an angular radius of 0.05.
        let mut scene = Scene::new(&device, &queue);
        for rough in [0.045, 0.1] {
            let mut input = dark_input();
            input.directional_lights[0] = Some(DirectionalLight {
                direction: -centre.as_vec3(),
                illuminance: 1.,
                angular_diameter: 0.1,
                ..Default::default()
            });
            let (peak, energy) = highlight(
                (&device, &queue),
                &mut renderer,
                &mut scene,
                &input,
                rough,
                mirror,
                "directional_light_sample(0u,s.position,s.geometry_normal,ShadeContext(vec2(0.),SHADOW_RECEIVER_CAPTURE,false,false,cluster_range(s.position,vec2(0.)),untraced_reflection()))",
            );
            let angular = 0.05f64;
            let expected = disc_reference(
                mirror,
                rough,
                centre,
                angular,
                1. / (PI * angular.sin().powi(2)),
            );
            eprintln!(
                "disc at elevation {elevation}, roughness {rough}: mirror {:.3} of the disc's, energy {:.3}",
                peak / expected,
                energy / centre.z
            );
            assert!(
                within(peak / expected, 0.55, 1.1),
                "disc at elevation {elevation}, roughness {rough}: {peak} along the mirror, the disc gives {expected}"
            );
            assert!(
                within(energy / centre.z, 0.8, 1.1),
                "disc at elevation {elevation}, roughness {rough}: reflects {energy} of irradiance {}",
                centre.z
            );
        }
    }
}
