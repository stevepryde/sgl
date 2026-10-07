//! KHR_materials_sheen as SGL3D shades it (sheen.wgsl, D-32) against
//! independent oracles: the Charlie lobe's directional albedo integrated in
//! f64, the energy a white furnace gives, and one model under lights and
//! the environment alike. Each observes the production shading on the GPU
//! (lighting_model_tests::observe_lit).
use super::lighting_model_tests::{
    Layered, PHI_STEPS, THETA_STEPS, cell, grid, observe_lit, polar, view,
};
use glam::DVec3;
use std::f64::consts::PI;

/// The directional albedo seen at cosine `nv` of KHR_materials_sheen's
/// Charlie lobe at perceptual roughness `rough` (alpha its square, README
/// 101–111) with Ashikhmin's visibility (137–140), sheen colour 1:
/// integrated in f64 over the lighting tests' hemisphere grid.
fn charlie_albedo(nv: f64, rough: f64) -> f64 {
    let v = view(nv);
    let alpha = rough * rough;
    let mut sum = 0.;
    for row in 0..THETA_STEPS {
        for column in 0..PHI_STEPS {
            let (theta, phi) = grid(row, column);
            let l = polar(theta, phi);
            let h = (v + l).normalize();
            let sin2h = (1. - h.z * h.z).max(0.);
            let distribution = (2. + 1. / alpha) * sin2h.powf(0.5 / alpha) / (2. * PI);
            let visibility = 1. / (4. * (l.z + v.z - l.z * v.z));
            sum += distribution * visibility * l.z * cell(theta);
        }
    }
    sum
}

/// A black surface that reflects nothing but a white sheen of perceptual
/// roughness `rough`, seen at cosine `nv`.
fn sheen_alone(nv: f64, rough: f64) -> Layered {
    Layered {
        view: view(nv),
        base: [0.; 3],
        dielectric_f0: 0.,
        sheen: [1.; 3],
        sheen_rough: rough,
        ..Layered::default()
    }
}

/// A bare white Lambertian seen at cosine `nv`, which reflects all the
/// irradiance it takes.
fn lambertian(nv: f64) -> Layered {
    Layered {
        view: view(nv),
        dielectric_f0: 0.,
        ..Layered::default()
    }
}

// Plausible defects: the table's blue channel left empty or filled from
// another table, transposed (N.V and roughness swapped), generated at alpha
// = roughness or with Kulla's visibility in place of Ashikhmin's, or read
// at alpha in place of perceptual roughness. The oracle is the Charlie
// lobe's directional albedo integrated in f64 from KHR's definition; the
// environment lights a black surface's sheen by its albedo at the view,
// over a white Lambertian's response to the same environment, which
// cancels the environment's own storage and filtering. The bound comes
// from the table, before any shader ran: its 64 × 64 bilinear
// interpolation, binary16 storage and Filament's 4096-sample integration
// together miss the f64 albedo by at most 7.7e-4 at these points, which sit
// midway between texel centres (worst at N.V 0.125 and roughness 0.25,
// where the albedo is steepest; 3e-4 elsewhere), and hardware filtering's
// 8-bit weights add at most 1.2e-4 there; 1e-3 holds both.
#[test]
fn the_sheen_albedo_is_the_charlie_lobe_s() {
    let points: Vec<(f64, f64)> = [0.25, 0.5, 0.75, 63. / 64.]
        .into_iter()
        .flat_map(|rough| [0.125, 0.25, 0.5, 0.75, 63. / 64.].map(|nv| (nv, rough)))
        .collect();
    let mut cases: Vec<Layered> = points
        .iter()
        .map(|&(nv, rough)| sheen_alone(nv, rough))
        .collect();
    cases.extend(points.iter().map(|&(nv, _)| lambertian(nv)));
    let Some(observed) = observe_lit(&cases) else {
        return;
    };
    let mut worst = 0f64;
    for (index, &(nv, rough)) in points.iter().enumerate() {
        let albedo = observed[index][2].y / observed[points.len() + index][2].y;
        let expected = charlie_albedo(nv, rough);
        worst = worst.max((albedo - expected).abs());
        assert!(
            (albedo - expected).abs() <= 1e-3,
            "N.V {nv}, roughness {rough}: sheen albedo {albedo}, Charlie {expected}"
        );
    }
    eprintln!("sheen albedo: within {worst:.6} of the f64 Charlie integral");
}

// Plausible defects: the sheen dims neither the base's environment
// specular, its multiple scattering nor its diffuse light, or dims one twice;
// its lobe lies over the coat (three.js r185's finish), so the coat's
// Fresnel does not take from it. The oracle is energy conservation:
// KHR_materials_sheen scales the base by what the sheen's albedo leaves, so
// a white sheen over a base that reflects all of a white furnace reflects
// all of it too, and a sheen beneath a coat leaves the coat's furnace as it
// was without one (README 71, 147–153). The bound is the environment's
// storage and filtering (lighting_model_tests::one_rule_for_every_indirect_source).
#[test]
fn a_sheen_keeps_a_white_furnace_whole_beneath_any_coat() {
    let mut cases = Vec::new();
    for rough in [0.3, 0.7] {
        for nv in [0.3, 0.7, 1.] {
            for (metallic, dielectric_f0) in [(1., 0.04), (0., 0.04)] {
                cases.push(Layered {
                    view: view(nv),
                    metallic,
                    dielectric_f0,
                    sheen: [1.; 3],
                    sheen_rough: rough,
                    ..Layered::default()
                });
            }
        }
    }
    let first_coat = cases.len();
    for nv in [0.3, 0.7] {
        for sheen in [[1.; 3], [0.; 3]] {
            cases.push(Layered {
                view: view(nv),
                metallic: 1.,
                coat: 1.,
                sheen,
                sheen_rough: 0.5,
                ..Layered::default()
            });
        }
    }
    let Some(observed) = observe_lit(&cases) else {
        return;
    };
    for (case, [_, _, environment]) in cases[..first_coat].iter().zip(&observed) {
        assert!(
            (*environment - DVec3::ONE).abs().max_element() <= 0.005,
            "{case:?}: reflects {environment:?} of a white furnace"
        );
    }
    for pair in observed[first_coat..].chunks(2) {
        let (sheened, bare) = (pair[0][2], pair[1][2]);
        assert!(
            (sheened - bare).abs().max_element() <= 0.005 * bare.max_element(),
            "beneath a coat: {sheened:?} with a sheen, {bare:?} without"
        );
    }
}

// Plausible defects: lights and the environment dim the base by different
// rules (KHR's min of the view's and each light's dimming under lights,
// against the view's alone in the environment), or the lobe under lights
// is not the lobe the table integrates (Kulla's visibility, alpha at
// roughness, as three.js r185 pairs its lobe with a fit: D-32). The oracle is S3D-5's one model:
// the same uniform light, as lights from every direction or as the
// environment, reflects the same energy. The bounds are the white metal's
// under lights (lighting_model_tests), the table's (above) and the
// environment's.
#[test]
fn direct_light_and_the_environment_reflect_a_sheen_alike() {
    let mut cases = Vec::new();
    for rough in [0.3, 0.5, 0.8] {
        for nv in [0.3, 0.7] {
            cases.push(sheen_alone(nv, rough));
            cases.push(Layered {
                view: view(nv),
                metallic: 1.,
                sheen: [1.; 3],
                sheen_rough: rough,
                ..Layered::default()
            });
        }
    }
    let Some(observed) = observe_lit(&cases) else {
        return;
    };
    for (case, [above, _, environment]) in cases.iter().zip(observed) {
        let bound = if case.metallic > 0. { 0.015 } else { 0.007 };
        assert!(
            (above - environment).abs().max_element() <= bound,
            "{case:?}: lights {above:?} against the environment's {environment:?}"
        );
    }
}

// Plausible defects: a coloured sheen dims the base channel by channel, or
// by its weakest channel, where KHR dims it by the strongest (README 151).
// The oracle is KHR's definition with the sheen's albedo observed alone: a
// red sheen over a white base in a white furnace reflects red whole and
// green and blue less the albedo. The bound is the environment's.
#[test]
fn a_coloured_sheen_dims_the_base_by_its_strongest_channel() {
    let (nv, rough) = (0.5, 0.5);
    let cases = [
        Layered {
            view: view(nv),
            sheen: [1., 0., 0.],
            sheen_rough: rough,
            ..Layered::default()
        },
        sheen_alone(nv, rough),
        lambertian(nv),
    ];
    let Some(observed) = observe_lit(&cases) else {
        return;
    };
    let albedo = observed[1][2].y / observed[2][2].y;
    let expected = DVec3::new(1., 1. - albedo, 1. - albedo);
    assert!(
        (observed[0][2] - expected).abs().max_element() <= 0.005,
        "a red sheen of albedo {albedo}: {:?}, KHR {expected:?}",
        observed[0][2]
    );
}
