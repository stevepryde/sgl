//! Micro-shadowing: a material's occlusion shadows its direct light where
//! the light arrives outside the cone that occlusion leaves open
//! (occlusion.wgsl's occlusion_micro_shadow, applied by surface.wgsl's
//! surface_direct_light, D-66). Each observes the production lobes on the
//! GPU, under lights alone: no environment or hemisphere light.
use super::lighting_model_tests::grid_wgsl;
use crate::{Light, LightShape, Scene, test_support};
use glam::Vec3;

/// The `outputs` rows `observation`, one invocation over the lighting
/// model tests' surfaces, writes.
fn observe(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &Scene,
    observation: &str,
    outputs: usize,
) -> Vec<[f32; 4]> {
    test_support::observe_surface(
        device,
        queue,
        scene,
        bytemuck::cast_slice(&[[0f32; 4]]),
        &format!("{}\n{observation}", grid_wgsl()),
        1,
        outputs,
    )
}

/// A rough dielectric under a white coat and sheen, its mapped normal +Z
/// and its geometry normal tilted off it, seen off its normal, of material
/// occlusion `occlusion`: every front lobe a light takes (base diffuse and
/// specular, sheen, coat).
const LAYERED: &str = r#"
fn layered(occlusion:f32)->Surface {
 var s=case_surface(normalize(vec3(-.3,.2,.9)),.5,vec3(.8,.5,.2),0.);
 s.geometry_normal=normalize(vec3(.4,.2,1.));
 s.coat=1.;
 s.coat_roughness=.3;
 s.sheen=vec3(.5);
 s.sheen_roughness=.5;
 s.occlusion=occlusion;
 return s;
}
"#;

// Plausible defects: the term left off direct light, applied to some
// lobes only (the base and not the sheen or coat), at the geometry normal
// or the view in place of the light, without its square, or Uncharted 4's
// fit (saturate(N·L + 2v² − 1)) in place of Chan's. The oracle is Chan's
// formula, saturate(N·L / √(1 − v))², computed by hand: a surface of
// occlusion v under a directional light at angle θ from its normal takes
// that share of what it takes at occlusion 1, in every channel. The bound
// is f32 rounding.
#[test]
fn micro_shadowing_follows_the_cone() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let scene = Scene::new(&device, &queue);
    // (occlusion, degrees from the normal, Chan's share by hand).
    let cases = [
        (0.5, 30., 1.),
        (0.5, 60., 0.5),
        (0.5, 80., 0.060_307_38),
        (0.9, 80., 0.301_536_9),
        (0., 60., 0.25),
    ];
    let shade: String = cases
        .iter()
        .enumerate()
        .map(|(index, (occlusion, degrees, _))| {
            let (sin, cos) = f32::to_radians(*degrees).sin_cos();
            let occlusion = *occlusion as f32;
            format!(
                r#"
 {{
  let light=LightSample(vec3({sin:?},0.,{cos:?}),vec3(1.),1.,0.,1.,NO_RECT_LIGHT,0.);
  let open=layered(1.);
  let occluded=layered({occlusion:?});
  result[{open}u]=vec4(surface_direct_light(open,case_reflectance(open),light),0.);
  result[{occluded}u]=vec4(surface_direct_light(occluded,case_reflectance(occluded),light),0.);
 }}"#,
                open = 2 * index,
                occluded = 2 * index + 1,
            )
        })
        .collect();
    let observation = format!(
        r#"{LAYERED}
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{{shade}
}}
"#
    );
    let rows = observe((&device, &queue), &scene, &observation, 2 * cases.len());
    for (index, (occlusion, degrees, share)) in cases.iter().enumerate() {
        let (open, occluded) = (rows[2 * index], rows[2 * index + 1]);
        for channel in 0..3 {
            assert!(open[channel] > 1e-3, "{open:?}");
            let observed = occluded[channel] / open[channel];
            assert!(
                (observed - share).abs() <= 1e-5 + 1e-5 * share,
                "occlusion {occlusion} at {degrees}°: channel {channel} keeps {observed}, Chan {share}"
            );
        }
    }
}

// Plausible defect: the term evaluated at full visibility, whose clamp to
// 0.9999 then darkens an unoccluded surface under grazing light (to a
// quarter at N·L 0.005). The oracle is equivalence with the unshadowed
// lobes: at occlusion 1 and N·L 0.005, what surface_direct_light gives is
// the light's radiance times surface_direct_brdf toward it. The bound is
// f32 rounding.
#[test]
fn full_visibility_changes_nothing() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let scene = Scene::new(&device, &queue);
    let observation = format!(
        r#"{LAYERED}
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let s=layered(1.);
 let reflectance=case_reflectance(s);
 let l=vec3(sqrt(1.-.005*.005),0.,.005);
 let radiance=vec3(2.,1.5,1.);
 result[0]=vec4(surface_direct_light(s,reflectance,LightSample(l,radiance,1.,0.,1.,NO_RECT_LIGHT,0.)),0.);
 result[1]=vec4(radiance*surface_direct_brdf(s,reflectance,l,0.,1.),0.);
}}
"#
    );
    let rows = observe((&device, &queue), &scene, &observation, 2);
    let (shaded, unshadowed) = (rows[0], rows[1]);
    for channel in 0..3 {
        assert!(unshadowed[channel] > 0., "{unshadowed:?}");
        assert!(
            (shaded[channel] - unshadowed[channel]).abs() <= 1e-6 * unshadowed[channel],
            "at occlusion 1 and N·L 0.005: {shaded:?}, unshadowed {unshadowed:?}"
        );
    }
}

// Plausible defects: a rectangle's light left unshadowed (the function's
// other branch), or shadowed at another direction than its centre's. The
// oracle is the same formula at the centre's cosine: a rectangle whose
// centre lies 60° from the normal lights a surface of occlusion 0.5 with
// saturate(0.5 / √0.5)² = 0.5 of what it gives at occlusion 1, by hand.
// The bound is f32 rounding.
#[test]
fn a_rectangle_takes_it_at_its_centre() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let centre = Vec3::new(60f32.to_radians().sin(), 0., 60f32.to_radians().cos()) * 3.;
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: centre,
                shape: LightShape::Rect {
                    direction: -centre.normalize(),
                    width_axis: Vec3::Y,
                    width: 1.,
                    height: 0.5,
                },
                color: [1.; 3],
                intensity: 2.,
                range: 100.,
                specular: 1.,
                casts_shadow: false,
                ..Default::default()
            },
        )
        .unwrap();
    let observation = format!(
        r#"{LAYERED}
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let open=layered(1.);
 let occluded=layered(.5);
 let light=scene_light_sample(0u,open.position,open.normal,open.coat_normal,open.geometry_normal,vec2(0.),SHADOW_RECEIVER_CAMERA,false,open.position,0.,vec3(0.),vec3(0.));
 result[0]=vec4(surface_direct_light(open,case_reflectance(open),light),f32(light.rect));
 result[1]=vec4(surface_direct_light(occluded,case_reflectance(occluded),light),0.);
}}
"#
    );
    let rows = observe((&device, &queue), &scene, &observation, 2);
    let (open, occluded) = (rows[0], rows[1]);
    assert_eq!(open[3], 0., "the sample names the rectangle");
    for channel in 0..3 {
        assert!(open[channel] > 1e-3, "{open:?}");
        let observed = occluded[channel] / open[channel];
        assert!(
            (observed - 0.5).abs() <= 1e-5,
            "a rectangle at 60° on occlusion 0.5: channel {channel} keeps {observed}, Chan 0.5"
        );
    }
}

// Plausible defect: the transmitted lobe shadowed too, at the cosine of a
// light behind the surface, which leaves a leaf lit from behind dark. The
// oracle is the rule: light reaching a surface's other side does not
// arrive through the cavities its front's occlusion stands for, so a leaf
// that passes its diffuse light through, lit from behind, gives the same at
// occlusion 0.5 as at 1. The bound is f32 rounding.
#[test]
fn the_other_side_takes_none() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let scene = Scene::new(&device, &queue);
    let observation = r#"
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
fn leaf(occlusion:f32)->Surface {
 var s=case_surface(normalize(vec3(.8,0.,.6)),.5,vec3(.3,.6,.2),0.);
 s.diffuse_transmission=1.;
 s.diffuse_transmission_color=vec3(.4,.8,.3);
 s.occlusion=occlusion;
 return s;
}
@compute @workgroup_size(1) fn observe() {
 let behind=normalize(vec3(-.5,.3,-.8));
 let light=LightSample(behind,vec3(1.),1.,1.,1.,NO_RECT_LIGHT,0.);
 let open=leaf(1.);
 let occluded=leaf(.5);
 result[0]=vec4(surface_direct_light(open,case_reflectance(open),light),0.);
 result[1]=vec4(surface_direct_light(occluded,case_reflectance(occluded),light),0.);
}
"#;
    let rows = observe((&device, &queue), &scene, observation, 2);
    let (open, occluded) = (rows[0], rows[1]);
    for channel in 0..3 {
        assert!(open[channel] > 1e-3, "{open:?}");
        assert!(
            (occluded[channel] - open[channel]).abs() <= 1e-6 * open[channel],
            "a leaf lit from behind at occlusion 0.5: {occluded:?}, at 1 {open:?}"
        );
    }
}
