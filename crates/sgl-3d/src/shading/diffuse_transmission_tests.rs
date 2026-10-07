//! KHR_materials_diffuse_transmission as SGL3D shades it (surface.wgsl's
//! transmitted lobe, D-32) against KHR's definition: the share of the light
//! the base diffuses that it passes to its other side, the specular layer
//! unchanged (README 205–216). Each observes the production shading on the
//! GPU.
use super::lighting_model_tests::{Layered, coupled_diffuse, grid_wgsl, observe_lit, view};
use crate::{Scene, test_support};
use glam::DVec3;

// Plausible defects: the base keeps all its diffuse light (the share passed
// through added on top), the transmitted lobe takes the base colour (Bevy
// 9d12036) or no Fresnel (Bevy's F0 0), couples at the light's own half
// vector below the surface in place of its mirror image's (the Khronos glTF
// Sample Renderer 0686eb2, pbr.frag 333–338), or faces the light's side.
// The oracle is KHR's definition, observed through the production lobes
// alone: under a light in front, a share t of the diffuse light leaves the
// front, d(t) = d(0) − t (d(0) − d(1)) with d(1) the specular alone; under
// the same light mirrored behind the surface, the back passes what the
// front lost, in the transmission colour over the base colour, channel by
// channel. The bound is f32 rounding.
#[test]
fn diffuse_transmission_moves_diffuse_light_to_the_other_side() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let scene = Scene::new(&device, &queue);
    let base = DVec3::new(0.8, 0.5, 0.2);
    let color = DVec3::new(0.3, 0.6, 0.9);
    let observation = format!(
        r#"{}
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let view=normalize(vec3(.8,0.,.6));
 let front=normalize(vec3(-.5,.3,.8));
 let back=vec3(front.xy,-front.z);
 let shares=array<f32,3>(0.,.4,1.);
 for (var index=0u;index<3u;index++) {{
  var s=case_surface(view,.5,vec3({:?},{:?},{:?}),0.);
  s.diffuse_transmission=shares[index];
  s.diffuse_transmission_color=vec3({:?},{:?},{:?});
  let reflectance=case_reflectance(s);
  result[2u*index]=vec4(surface_direct_light(s,reflectance,LightSample(front,vec3(1.),1.,1.,1.,NO_RECT_LIGHT,0.)),0.);
  result[2u*index+1u]=vec4(surface_direct_light(s,reflectance,LightSample(back,vec3(1.),1.,1.,1.,NO_RECT_LIGHT,0.)),0.);
 }}
}}
"#,
        grid_wgsl(),
        base.x as f32,
        base.y as f32,
        base.z as f32,
        color.x as f32,
        color.y as f32,
        color.z as f32,
    );
    let rows = test_support::observe_surface(
        &device,
        &queue,
        &scene,
        bytemuck::cast_slice(&[[0f32; 4]]),
        &observation,
        1,
        6,
    );
    let vector = |a: [f32; 4]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    let [front, back]: [Vec<DVec3>; 2] =
        [0, 1].map(|side| (0..3).map(|share| vector(rows[2 * share + side])).collect());
    let diffused = front[0] - front[2];
    assert!(diffused.min_element() > 0.01, "{front:?}");
    let within = |a: DVec3, b: DVec3| (a - b).abs().max_element() <= 1e-5 + 1e-4 * b.max_element();
    assert!(
        within(front[1], front[0] - diffused * 0.4),
        "a share 0.4 passed through leaves {:?} in front, KHR {:?}",
        front[1],
        front[0] - diffused * 0.4
    );
    assert!(
        back[0] == DVec3::ZERO,
        "nothing passed through: {:?}",
        back[0]
    );
    for (share, back) in [(0.4, back[1]), (1., back[2])] {
        let expected = diffused * share * color / base;
        assert!(
            within(back, expected),
            "a share {share} passed through: {back:?} behind, KHR {expected:?}"
        );
    }
}

// Plausible defects: the other side's indirect light weighted without the
// dielectric's scattering (Bevy's F0 0) or not at all, the front keeping
// all its diffuse light, or the transmitted lobe under lights uncoupled or
// facing the light's side. The oracles: light passed through moves from one
// side to the other, so in a white furnace about both sides a white surface
// reflects the same whatever it passes through; and under diffuse-only
// lights from every direction of both sides, the two sides together diffuse
// what glTF's dielectric BRDF does on one (lighting_model_tests::
// coupled_diffuse, its mirror image behind), the share passed through
// behind. The bounds are the environment's storage and f32 accumulation.
#[test]
fn diffuse_transmission_conserves_a_white_furnace() {
    let shares = [0., 0.5, 1.];
    let views = [0.3, 0.8];
    let mut cases = Vec::new();
    for nv in views {
        for share in shares {
            for light_specular in [1., 0.] {
                cases.push(Layered {
                    view: view(nv),
                    transmission: share,
                    light_specular,
                    ..Layered::default()
                });
            }
        }
    }
    let Some(observed) = observe_lit(&cases) else {
        return;
    };
    for (case, [above, below, environment]) in cases.iter().zip(&observed) {
        if case.light_specular > 0. {
            let opaque = observed[cases
                .iter()
                .position(|c| c.view == case.view && c.light_specular > 0.)
                .unwrap()][2];
            assert!(
                (*environment - opaque).abs().max_element() <= 1e-3 * opaque.max_element(),
                "{case:?}: reflects {environment:?} of a white furnace, {opaque:?} passing nothing through"
            );
        } else {
            let diffused = coupled_diffuse(case.view);
            let share = case.transmission;
            for (side, expected) in [(above, (1. - share) * diffused), (below, share * diffused)] {
                assert!(
                    (*side - DVec3::splat(expected)).abs().max_element() <= 0.001 * diffused,
                    "{case:?} under diffuse-only lights: {side:?}, glTF {expected}"
                );
            }
        }
    }
}

// Plausible defects: light passed through a volume unattenuated under
// lights or under the other side's indirect light, or attenuated by
// another law. The oracle is KHR_materials_volume's Beer-Lambert law: over
// a thickness x of attenuation coefficient sigma, a surface that passes all
// its diffuse light through passes exp(-sigma x) of what it passes across
// no volume, channel by channel, under lights from behind and in a white
// furnace alike (the Khronos glTF Sample Renderer 0686eb2 attenuates both,
// pbr.frag 199–201, 341–343). The bound is f32 rounding and the
// environment's storage.
#[test]
fn a_volume_attenuates_both_lobes_of_passed_light() {
    let sigma = [-2. * 0.5f64.ln(), -2. * 0.8f64.ln(), 0.];
    let case = |volume_thickness, attenuation| Layered {
        view: view(0.7),
        dielectric_f0: 0.,
        transmission: 1.,
        volume_thickness,
        attenuation,
        light_specular: 0.,
        ..Layered::default()
    };
    let Some(observed) = observe_lit(&[case(0., [0.; 3]), case(0.5, sigma)]) else {
        return;
    };
    let (clear, attenuated) = (observed[0], observed[1]);
    let transmittance = DVec3::from_array(sigma.map(|s| (-s * 0.5f64).exp()));
    for (label, clear, attenuated) in [
        ("lights from behind", clear[1], attenuated[1]),
        ("a white furnace", clear[2], attenuated[2]),
    ] {
        assert!(clear.min_element() > 0.1, "{label}: {clear:?}");
        assert!(
            (attenuated - clear * transmittance).abs().max_element() <= 2e-3 * clear.max_element(),
            "{label}: {attenuated:?} attenuated, {clear:?} clear, Beer-Lambert's {:?}",
            clear * transmittance
        );
    }
}

// Plausible defects: the other side's indirect light counted in the ambient
// light occlusion occludes (Shaded.ambient, the G-buffer's ambient target),
// so the front's ambient occlusion or material occlusion darkens light that
// reaches the back. The oracle is the departure D-32 records, Bevy
// 9d12036's: the transmitted lobe takes its ambient light unoccluded
// (pbr_functions.wesl 672–681). A surface that passes all its diffuse light
// through and reflects none reflects the same at material occlusion 0.5 as
// at 1, as a probe capture's surface and, through the ambient target,
// source completion occlude it. The bound is f32 rounding.
#[test]
fn the_transmitted_lobe_s_ambient_light_takes_no_occlusion() {
    let case = |occlusion| Layered {
        view: view(0.7),
        dielectric_f0: 0.,
        transmission: 1.,
        occlusion,
        ..Layered::default()
    };
    let Some(observed) = observe_lit(&[case(1.), case(0.5)]) else {
        return;
    };
    let (open, occluded) = (observed[0][2], observed[1][2]);
    assert!(open.min_element() > 0.5, "{open:?}");
    assert!(
        (occluded - open).abs().max_element() <= 1e-5 * open.max_element(),
        "at material occlusion 0.5: {occluded:?}, unoccluded {open:?}"
    );
}

// Plausible defects: a dynamic GI probe ray's hit drops a light behind it,
// as one its own side faces away from, so the light leaves pass through
// never reaches the volume; or its visibility ray leaves the hit's own face
// for a volume's transmitted lobe, which lies the thickness behind it. The
// oracles: S3D-5, the hit shades the one light it draws as every
// receiver's shading does, its transmitted lobe seen from the other side
// (surface_direct_light), diffuse light alone; and geometry, a slab's far
// face 0.5 m behind the hit covers it from the light, so at thickness 0 the
// ray finds the face and the hit takes none of the light, and at the
// slab's thickness the ray leaves from the face and finds nothing.
#[test]
fn a_probe_ray_s_hit_takes_the_light_behind_it_through() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = super::lighting_model_tests::quiet_settings();
    let mut renderer = crate::renderer::Renderer::for_test(&device, &queue, [16, 16], &settings);
    let mut scene = Scene::new(&device, &queue);
    let mut far = test_support::cube();
    far.materials[0].double_sided = true;
    far.meshes[0].vertices = [(-2., -2.), (2., -2.), (2., 2.), (-2., 2.)]
        .map(|(x, y)| crate::asset::Vertex {
            position: [x, y, -0.5],
            normal: [0., 0., 1.],
            uv: [0.; 2],
            color: [1.; 4],
            lightmap_uv: [0.; 2],
            lightmap_bounds: [0., 0., 1., 1.],
            tangent: [0.; 4],
        })
        .to_vec();
    far.meshes[0].indices = vec![0, 1, 2, 0, 2, 3];
    test_support::add_static(&device, &queue, &mut scene, far);
    let mut input = super::lighting_model_tests::dark_input();
    input.directional_lights[0] = Some(crate::DirectionalLight {
        direction: glam::Vec3::new(0.3, 0., 1.),
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(crate::DirectionalShadow {
            distance: 10.,
            cascades: 1,
        }),
        ..Default::default()
    });
    let observation = format!(
        r#"{}
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 var s=case_surface(normalize(vec3(-.4,0.,1.)),.5,vec3(1.),0.);
 s.diffuse_transmission=1.;
 s.diffuse_transmission_color=vec3(1.);
 s.lightmap_uv=vec2(-1.);
 let light=frame.directional_lights[0];
 let l=normalize(light.direction_to_light);
 output[0]=vec4(surface_direct_light(s,case_reflectance(s),LightSample(l,light.color*light.illuminance,0.,1.,0.,NO_RECT_LIGHT,0.)),0.);
 output[1]=vec4(probe_hit_light(s,cluster_range(s.position,vec2(0.)),vec3(0.)),0.);
 s.volume_thickness=.5;
 output[2]=vec4(probe_hit_light(s,cluster_range(s.position,vec2(0.)),vec3(0.)),0.);
}}
"#,
        grid_wgsl()
    );
    let rows = test_support::observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        &observation,
        3,
    );
    let vector = |a: [f32; 4]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    let (receiver, thin, thick) = (vector(rows[0]), vector(rows[1]), vector(rows[2]));
    assert!(receiver.min_element() > 0.01, "{receiver:?}");
    assert!(
        thin.max_element() < 1e-6,
        "a probe ray's hit at thickness 0 behind the slab's far face: {thin:?}"
    );
    assert!(
        (thick - receiver).abs().max_element() <= 1e-5 * receiver.max_element(),
        "a probe ray's hit at the slab's thickness, lit from behind: {thick:?}, a receiver {receiver:?}"
    );
}
