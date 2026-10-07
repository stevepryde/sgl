//! The anisotropic GGX lobe, its place in direct light and the KHR bent
//! normal, against independent f64 equations: KHR_materials_anisotropy
//! (acfcbe65e40c53d6d3aa55a7299982bf2c01c75d) directional roughness
//! `alpha_t = mix(alpha_b, 1, strength²)`, `alpha_b = roughness²`, and its
//! distribution in the reciprocal-ellipse form (the shader uses the rescaled
//! vector form), height-correlated Smith visibility, and Schlick's Fresnel
//! `f0 + (f90 - f0) (1 - v.h)^5` (Khronos glTF Sample Renderer brdf.glsl
//! F_Schlick and BRDF_specularGGXAnisotropy). At strength 0 the lobe is
//! isotropic GGX. Direct light layers it as KHR_materials_clearcoat layers a
//! base under a coat, with the surface's multiple-scattering gain and coat
//! Fresnel read back from the shader, not recomputed here.
use glam::{DQuat, DVec3};
use std::f64::consts::PI;

/// The specular lobe of `n`, `v` and `l` at perceptual roughness `rough`,
/// stretched along unit tangent `t` by `strength`, reflecting `f0` at normal
/// and `f90` at grazing incidence.
fn specular([n, v, l, t]: [DVec3; 4], rough: f64, strength: f64, f0: DVec3, f90: f64) -> DVec3 {
    let b = n.cross(t).normalize();
    let h = (v + l).normalize();
    let nv = n.dot(v).clamp(0., 1.);
    let nl = n.dot(l).clamp(0., 1.);
    let ab = rough * rough;
    let at = ab + (1. - ab) * strength * strength;
    let ellipse = (h.dot(t) / at).powi(2) + (h.dot(b) / ab).powi(2) + h.dot(n).powi(2);
    let d = 1. / (PI * at * ab * ellipse * ellipse);
    let projected_v = ((at * t.dot(v)).powi(2) + (ab * b.dot(v)).powi(2) + nv * nv).sqrt();
    let projected_l = ((at * t.dot(l)).powi(2) + (ab * b.dot(l)).powi(2) + nl * nl).sqrt();
    let visibility = 0.5 / (nl * projected_v + nv * projected_l);
    let fresnel = f0 + (DVec3::splat(f90) - f0) * (1. - v.dot(h).clamp(0., 1.)).powi(5);
    fresnel * d * visibility
}

/// KHR's bent normal: the view projected perpendicular to the bitangent,
/// blended toward the normal by `(1 - strength (1 - roughness))⁴`.
fn bent_normal(n: DVec3, v: DVec3, t: DVec3, rough: f64, strength: f64) -> DVec3 {
    let b = n.cross(t).normalize();
    let projected = (v - b * b.dot(v)).normalize();
    let blend = (1. - strength * (1. - rough)).powi(4);
    (projected * (1. - blend) + n * blend).normalize()
}

fn direction(theta: f64, phi: f64) -> DVec3 {
    DVec3::new(
        theta.sin() * phi.cos(),
        theta.sin() * phi.sin(),
        theta.cos(),
    )
}

const F0: DVec3 = DVec3::new(0.54, 0.49, 0.44);

// Plausible defects: the tangent and bitangent roughness swapped, the
// strength or the roughness not squared, visibility clamped at grazing
// (KHR's illustrative clamp), Fresnel by Epic's exp2 fit or toward 1
// whatever the F90, the bent normal blended the wrong way, or direct light
// that takes the isotropic lobe, drops the multiple-scattering gain or
// leaves the coat off. The oracle is the KHR and Khronos equations in f64 on
// the exact uploaded inputs.
#[test]
fn anisotropy_gpu_matches_independent_brdf() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    // (n, v, l, t), with the roughness, strength, F90 and coat strength.
    let mut cases: Vec<[[f32; 4]; 4]> = Vec::new();
    for rotation in [
        DQuat::IDENTITY,
        DQuat::from_euler(glam::EulerRot::XYZ, 0.81, -1.17, 0.63),
    ] {
        for rough in [0.15, 0.3, 0.7] {
            for strength in [0., 0.00001, 0.6, 1.] {
                for theta in [0.12, 0.87, 1.565] {
                    for phi in [0., 0.61, 1.57, 2.36] {
                        for axis in [0., 0.43, 1.57, 2.29] {
                            let n = rotation * DVec3::Z;
                            let v = rotation * direction(theta, phi);
                            let l = rotation * direction(theta * 0.83, phi + 2.81);
                            let t = rotation * DVec3::new(f64::cos(axis), f64::sin(axis), 0.);
                            let f90 = if phi == 0.61 { 0.7 } else { 1. };
                            let coat = if axis == 0. { 0. } else { 0.65 };
                            cases.push([
                                [n.x as f32, n.y as f32, n.z as f32, rough],
                                [v.x as f32, v.y as f32, v.z as f32, strength],
                                [l.x as f32, l.y as f32, l.z as f32, f90],
                                [t.x as f32, t.y as f32, t.z as f32, coat],
                            ]);
                        }
                    }
                }
            }
        }
    }
    let observation = r#"
struct Case { n:vec4<f32>,v:vec4<f32>,l:vec4<f32>,t:vec4<f32> }
@group(0) @binding(3) var<storage,read> cases:array<Case>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
const F0=vec3(.54,.49,.44);
// A metal of base F0 with the case's coat at roughness 0.21: its direct
// light is its specular lobes alone.
fn case_surface(c:Case)->Surface {
 var surface:Surface;
 surface.base=vec4(F0,1.);
 surface.metallic=1.;
 surface.normal=c.n.xyz;
 surface.geometry_normal=c.n.xyz;
 surface.view=c.v.xyz;
 surface.roughness=c.n.w;
 surface.coat=c.t.w;
 surface.coat_roughness=.21;
 surface.anisotropy=vec4(c.t.xyz,c.v.w);
 return surface;
}
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];
 let axis=vec4(c.t.xyz,c.v.w);
 let surface=case_surface(c);
 // The view's DFG lookup fixed, and the case's F90.
 var reflectance=surface_reflectance(surface,vec2(.8,.025));
 reflectance.f90=c.l.w;
 let light=LightSample(c.l.xyz,vec3(1.),1.,1.,NO_RECT_LIGHT,0.);
 result[id.x*4u]=vec4(pbr_anisotropic_specular(c.n.xyz,c.v.xyz,c.l.xyz,c.n.w,F0,c.l.w,axis),1.);
 result[id.x*4u+1u]=vec4(pbr_anisotropy_bent_normal(c.n.xyz,c.v.xyz,axis,c.n.w),1.);
 result[id.x*4u+2u]=vec4(surface_direct_light(surface,reflectance,light),reflectance.coat_fresnel);
 result[id.x*4u+3u]=vec4(reflectance.multiscatter,0.);
}
"#;
    let scene = crate::Scene::new(&device, &queue);
    let rows = crate::test_support::observe_surface(
        &device,
        &queue,
        &scene,
        bytemuck::cast_slice(&cases),
        observation,
        (cases.len() as u32).div_ceil(64),
        cases.len() * 4,
    );
    let vector = |a: [f32; 4]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    let mut worst = [0f64; 3];
    for (i, c) in cases.iter().enumerate() {
        // The equations on the exact uploaded float inputs.
        let [n, v, l, t] = c.map(vector);
        let [rough, strength, f90, coat] = c.map(|row| row[3] as f64);
        let expected = specular([n, v, l, t], rough, strength, F0, f90);
        let actual = vector(rows[4 * i]);
        // f32 dot products and normalisation cancel near a narrow lobe's
        // peak: within 0.05% of the lobe, and 0.1% for the isotropic form,
        // whose 1 - (n.h)² cancels at the peak (0.079% measured on Apple
        // M5). Epic's Fresnel fit departs from Schlick's by 0.1–0.3% and the
        // other defects by far more.
        let relative = if strength == 0. { 0.001 } else { 0.0005 };
        let bound = |expected: DVec3| expected.abs() * relative + DVec3::splat(2e-6);
        let error = ((actual - expected).abs() / bound(expected)).max_element();
        worst[0] = worst[0].max(error);
        assert!(
            error <= 1.,
            "lobe, case {i}: actual={actual:?}, f64={expected:?}, inputs={c:?}"
        );
        let bent_error = vector(rows[4 * i + 1]).distance(bent_normal(n, v, t, rough, strength));
        worst[1] = worst[1].max(bent_error / 2e-5);
        assert!(
            bent_error < 2e-5,
            "bent normal, case {i}: error={bent_error}"
        );
        // The coat: KHR_materials_clearcoat's isotropic GGX layer of F0 0.04
        // and F90 1, over the base dimmed by its Fresnel toward the view.
        let coat_fresnel = rows[4 * i + 2][3] as f64;
        let gain = vector(rows[4 * i + 3]);
        let coat_lobe = specular([n, v, l, t], 0.21, 0., DVec3::splat(0.04), 1.);
        let expected =
            (expected * gain * (1. - coat_fresnel) + coat_lobe * coat) * n.dot(l).clamp(0., 1.);
        let actual = vector(rows[4 * i + 2]);
        let error = ((actual - expected).abs() / bound(expected)).max_element();
        worst[2] = worst[2].max(error);
        assert!(
            error <= 1.,
            "direct light, case {i}: actual={actual:?}, expected={expected:?}"
        );
    }
    eprintln!(
        "{} cases: worst error over its bound: lobe {:.3}, bent normal {:.3}, direct light {:.3}",
        cases.len(),
        worst[0],
        worst[1],
        worst[2]
    );
}
