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
// never reaches the volume. The oracle is S3D-5: the hit shades the one
// light it draws as every receiver's shading does, its transmitted lobe
// seen from the other side (surface_direct_light), diffuse light alone.
#[test]
fn a_probe_ray_s_hit_takes_the_light_behind_it_through() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = super::lighting_model_tests::quiet_settings();
    let mut renderer = crate::renderer::Renderer::for_test(&device, &queue, [16, 16], &settings);
    let mut scene = Scene::new(&device, &queue);
    let mut input = super::lighting_model_tests::dark_input();
    input.directional_lights[0] = Some(crate::DirectionalLight {
        direction: glam::Vec3::new(0.3, 0., 1.),
        color: [1.; 3],
        illuminance: 1.,
        shadow: None,
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
 output[0]=vec4(probe_hit_light(s,cluster_range(s.position,vec2(0.)),vec3(0.)),0.);
 output[1]=vec4(surface_direct_light(s,case_reflectance(s),LightSample(l,light.color*light.illuminance,0.,1.,0.,NO_RECT_LIGHT,0.)),0.);
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
        2,
    );
    let vector = |a: [f32; 4]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    let (hit, receiver) = (vector(rows[0]), vector(rows[1]));
    assert!(receiver.min_element() > 0.01, "{receiver:?}");
    assert!(
        (hit - receiver).abs().max_element() <= 1e-5 * receiver.max_element(),
        "a probe ray's hit lit from behind: {hit:?}, a receiver {receiver:?}"
    );
}
