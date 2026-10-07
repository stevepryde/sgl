//! KHR_materials_iridescence's thin film (iridescence.wgsl, surface_f0s)
//! against an exact thin-film sum: what the base lobe's Fresnel gives at the
//! view's mirror direction, observed through the production surface
//! functions on the GPU.
use crate::{Scene, test_support};
use std::f64::consts::PI;

/// Schlick's reflectance at cosine `cos` of an interface of reflectance `f0`
/// at normal incidence, toward 1 at grazing: KHR_materials_iridescence's
/// interface model.
fn schlick(f0: f64, cos: f64) -> f64 {
    f0 + (1. - f0) * (1. - cos).max(0.).powi(5)
}

/// One lobe of Wyman, Sloan and Shirley 2013's piecewise Gaussian fit.
fn lobe(wavelength: f64, mean: f64, below: f64, above: f64) -> f64 {
    let spread = if wavelength < mean { below } else { above };
    (-0.5 * ((wavelength - mean) / spread).powi(2)).exp()
}

/// The CIE 1931 colour matching functions at `wavelength` nanometres, by
/// Wyman, Sloan and Shirley 2013's multi-lobe fit ("Simple Analytic
/// Approximations to the CIE XYZ Color Matching Functions", JCGT 2(2)),
/// independent of the Gaussian fits the extension's Fourier form uses.
fn colour_matching(wavelength: f64) -> [f64; 3] {
    let l = wavelength;
    [
        1.056 * lobe(l, 599.8, 37.9, 31.0) + 0.362 * lobe(l, 442.0, 16.0, 26.7)
            - 0.065 * lobe(l, 501.1, 20.4, 26.2),
        0.821 * lobe(l, 568.8, 46.9, 40.5) + 0.286 * lobe(l, 530.9, 16.3, 31.1),
        1.217 * lobe(l, 437.0, 11.8, 36.0) + 0.681 * lobe(l, 459.0, 26.0, 13.8),
    ]
}

/// CIE XYZ to linear Rec. 709 (sRGB primaries, D65).
const XYZ_TO_REC709: [[f64; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

/// The reflectance a film of IOR `film` and `thickness` nanometres over a
/// dielectric of IOR `base` has from air at cosine `cos`, in linear Rec.
/// 709: the exact two-interface (Airy) sum at each wavelength, every order
/// of the bounces, its interfaces' magnitudes Schlick's at their angles
/// (Snell's law into the film) and signed as Fresnel's amplitudes at normal
/// incidence, integrated over 380–780 nm against the colour matching
/// functions. The colour follows the extension's convention (Belcour and
/// Barla 2017, as its README specifies the evaluation): the sum averaged
/// over phase, the incoherent reflectance, is achromatic; the interference
/// about it is integrated against the curves, normalised by Y's, and
/// converted to Rec. 709 without white balance.
pub(crate) fn thin_film(film: f64, base: f64, thickness: f64, cos: f64) -> [f64; 3] {
    let cos_film = (1. - (1. - cos * cos) / (film * film)).sqrt();
    let interface = |a: f64, b: f64| ((a - b) / (a + b)).powi(2);
    let r12 = schlick(interface(film, 1.), cos);
    let r23 = schlick(interface(base, film), cos_film);
    let a12 = r12.sqrt().copysign(1. - film);
    let a23 = r23.sqrt().copysign(film - base);
    let incoherent = (r12 + r23 - 2. * r12 * r23) / (1. - r12 * r23);
    let mut xyz = [0.; 3];
    let mut y_sum = 0.;
    for step in 0..=2000 {
        let wavelength = 380. + step as f64 * 0.2;
        let phase = 4. * PI * film * thickness * cos_film / wavelength;
        let (c, s) = (phase.cos(), phase.sin());
        let numerator = (a12 + a23 * c).powi(2) + (a23 * s).powi(2);
        let denominator = (1. + a12 * a23 * c).powi(2) + (a12 * a23 * s).powi(2);
        let interference = numerator / denominator - incoherent;
        let curves = colour_matching(wavelength);
        for (sum, curve) in xyz.iter_mut().zip(curves) {
            *sum += interference * curve;
        }
        y_sum += curves[1];
    }
    std::array::from_fn(|row| {
        incoherent
            + (0..3)
                .map(|column| XYZ_TO_REC709[row][column] * xyz[column] / y_sum)
                .sum::<f64>()
    })
}

/// A dielectric of IOR 1.5 (F0 0.04) under a film.
#[derive(Clone, Copy, Debug)]
struct Case {
    strength: f64,
    film: f64,
    thickness: f64,
    cos: f64,
}

/// What the base lobe's Fresnel gives each case at the view's mirror
/// direction: pbr_fresnel_schlick at N.V of surface_f0 toward surface_f90,
/// the reflectance the specular lobes and the G-buffer's F0 and F90 carry.
fn observed(cases: &[Case]) -> Option<Vec<[f64; 3]>> {
    let (device, queue) = test_support::device()?;
    let scene = Scene::new(&device, &queue);
    let packed: Vec<[f32; 4]> = cases
        .iter()
        .map(|c| [c.strength, c.film, c.thickness, c.cos].map(|v| v as f32))
        .collect();
    let observation = r#"
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];
 var s:Surface;
 s.normal=vec3(0.,0.,1.);
 s.geometry_normal=s.normal;
 s.coat_normal=s.normal;
 s.view=vec3(sqrt(1.-c.w*c.w),0.,c.w);
 s.base=vec4(1.);
 s.dielectric_f0=vec3(.04);
 s.specular=1.;
 s.iridescence=c.x;
 s.iridescence_ior=c.y;
 s.iridescence_thickness=c.z;
 result[id.x]=vec4(pbr_fresnel_schlick(c.w,surface_f0(s),surface_f90(s)),0.);
}
"#;
    let rows = test_support::observe_surface(
        &device,
        &queue,
        &scene,
        bytemuck::cast_slice(&packed),
        observation,
        cases.len().div_ceil(64) as u32,
        cases.len(),
    );
    Some(
        rows.iter()
            .map(|row| [row[0], row[1], row[2]].map(f64::from))
            .collect(),
    )
}

// Plausible defects: the optical path difference taken at the incident
// angle in place of the film's (Snell's law), without its factor of two or
// in micrometres; a phase rule flipped; the spectral sensitivity left in
// XYZ; the strength ignored; a film of no thickness left a residue (the
// Fourier form's floor gives 0.047 for 0.04). The oracle is the exact
// thin-film sum (thin_film), its colour matching an independent fit. Its
// bound covers what the extension's form approximates, the second-order
// expansion of the bounces and its Gaussian curves: 0.0073 at most over
// these films on a dielectric, in f64, at normal incidence and at N.V 0.7,
// where the Schlick curve the film is refit to reaches its value. A metal
// base, whose bounces the second order truncates further, is not compared.
#[test]
fn the_film_reflects_as_an_exact_thin_film_sum() {
    let mut cases = Vec::new();
    for film in [1.3, 1.8] {
        for thickness in [150., 250., 400., 600., 900.] {
            for cos in [1., 0.7] {
                cases.push(Case {
                    strength: 1.,
                    film,
                    thickness,
                    cos,
                });
            }
        }
    }
    let films = cases.len();
    for cos in [1., 0.7] {
        cases.push(Case {
            strength: 1.,
            film: 1.3,
            thickness: 0.,
            cos,
        });
    }
    cases.push(Case {
        strength: 0.5,
        film: 1.8,
        thickness: 400.,
        cos: 1.,
    });
    let Some(observed) = observed(&cases) else {
        return;
    };
    let mut worst = 0f64;
    for (case, seen) in cases[..films].iter().zip(&observed) {
        let expected = thin_film(case.film, 1.5, case.thickness, case.cos);
        let deviation = (0..3)
            .map(|channel| (seen[channel] - expected[channel]).abs())
            .fold(0., f64::max);
        worst = worst.max(deviation);
        assert!(
            deviation <= 0.01,
            "{case:?}: the base lobe reflects {seen:?}, a thin film {expected:?}"
        );
    }
    // No film where it has no thickness: the bare dielectric's Schlick.
    for (case, seen) in cases[films..films + 2].iter().zip(&observed[films..]) {
        let bare = schlick(0.04, case.cos);
        assert!(
            seen.iter().all(|value| (value - bare).abs() <= 1e-4),
            "{case:?}: reflects {seen:?}, the bare dielectric {bare}"
        );
    }
    // Half the film's strength reflects half its Fresnel over the bare
    // dielectric's (KHR_materials_iridescence's mix by iridescenceFactor).
    let case = cases[films + 2];
    let film = thin_film(case.film, 1.5, case.thickness, case.cos);
    let seen = observed[films + 2];
    for channel in 0..3 {
        let expected = 0.5 * 0.04 + 0.5 * film[channel];
        assert!(
            (seen[channel] - expected).abs() <= 0.005,
            "{case:?}: reflects {seen:?}, half a thin film {film:?}"
        );
    }
    eprintln!("thin film: within {worst:.5} of the exact sum");
}
