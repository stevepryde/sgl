//! LM-63 files read as profiles, judged by the files' own numbers and the
//! symmetry identities LM-63-2002 defines for type C photometry.
use super::*;
use wasm_bindgen_test::wasm_bindgen_test;

/// An LM-63-2002 file of type C photometry with `tilt` (the text after
/// `TILT=` and any tilt block), a candela multiplier of `multiplier`, a
/// ballast factor of 0.9 and a ballast-lamp factor of 1, at `vertical` and
/// `horizontal` angles, with `candela` listed plane by plane, each number
/// on a line of its own so the reading cannot rely on line breaks.
fn file(
    tilt: &str,
    multiplier: f64,
    vertical: &[f64],
    horizontal: &[f64],
    candela: &[f64],
) -> String {
    let mut text = format!(
        "IESNA:LM-63-2002\n[TEST] profile\n[MANUFAC] SGL\nTILT={tilt}\n1 -1 {multiplier} {} {} 1 2 0.1 0.2 0.0\n0.9\n1\n12\n",
        vertical.len(),
        horizontal.len()
    );
    for value in vertical.iter().chain(horizontal).chain(candela) {
        text.push_str(&format!("{value}\n"));
    }
    text
}

/// `file` with no tilt and a multiplier of 1.
fn plain(vertical: &[f64], horizontal: &[f64], candela: &[f64]) -> String {
    file("NONE", 1., vertical, horizontal, candela)
}

const ROUND: [f64; 5] = [0., 90., 180., 270., 360.];

/// A full file whose planes at 0°, 90°, 180° and 270° differ, at 0°, 30°,
/// 60° and 90° from the nadir, with the 360° plane its 0° one again.
fn asymmetric() -> (Vec<f64>, String) {
    let planes = [
        [800., 1000., 600., 100.],
        [800., 700., 300., 50.],
        [800., 400., 200., 0.],
        [800., 500., 450., 25.],
    ];
    let candela: Vec<f64> = planes
        .iter()
        .chain([&planes[0]])
        .flatten()
        .copied()
        .collect();
    let text = plain(&[0., 30., 60., 90.], &ROUND, &candela);
    (candela, text)
}

fn assert_close(got: f32, want: f64, what: &str) {
    assert!(
        (f64::from(got) - want).abs() <= 1e-6,
        "{what}: {got}, not {want}"
    );
}

// Plausible defects: planes read in the wrong order or transposed (values
// plane by plane read as angle by angle), the peak taken from one plane,
// or a grid whose samples are not the measured values. The oracle is the
// file: its steps of 30° and 90° hold every measured angle, so each grid
// sample is one measured value over the file's largest, 1000.
#[wasm_bindgen_test(unsupported = test)]
fn a_full_file_s_samples_are_its_values_over_its_peak() {
    let (candela, text) = asymmetric();
    let profile = LightProfile::from_ies(&text).unwrap();
    assert_eq!(profile.size, [4, 5]);
    for row in 0..5 {
        for column in 0..4 {
            assert_close(
                profile.sample(column, row),
                candela[(row * 4 + column) as usize] / 1000.,
                &format!("vertical {column}, horizontal {row}"),
            );
        }
    }
}

// Plausible defects: the multiplier or a ballast factor left out of the
// peak, or applied twice; the second ballast number taken as a multiplier
// in a version where it is none. The oracle is each version's fields: the
// file's largest number, 1000, times its multiplier 2.5 and ballast factor
// 0.9, and in LM-63-1995 times its ballast-lamp photometric factor (0.8
// here), where LM-63-2002 leaves that number for future use and LM-63-2019
// makes it the file generation type (1.1, an accredited laboratory's).
#[wasm_bindgen_test(unsupported = test)]
fn the_peak_is_the_largest_value_times_the_multiplier_and_ballast_factors() {
    let (candela, _) = asymmetric();
    let text = file("NONE", 2.5, &[0., 30., 60., 90.], &ROUND, &candela);
    let profile = LightProfile::from_ies(&text).unwrap();
    assert_close(profile.peak_candela(), 1000. * 2.5 * 0.9, "peak");
    assert_close(profile.sample(1, 0), 1., "the peak's sample");
    let versioned = |header: &str, second: &str| {
        let text = text
            .replace("IESNA:LM-63-2002", header)
            .replace("\n0.9\n1\n12\n", &format!("\n0.9\n{second}\n12\n"));
        LightProfile::from_ies(&text).unwrap().peak_candela()
    };
    assert_close(
        versioned("IES:LM-63-2019", "1.10000"),
        1000. * 2.5 * 0.9,
        "2019",
    );
    assert_close(
        versioned("IESNA:LM-63-2002", "1.1"),
        1000. * 2.5 * 0.9,
        "2002",
    );
    assert_close(
        versioned("IESNA:LM-63-1995", "0.8"),
        1000. * 2.5 * 0.9 * 0.8,
        "1995",
    );
}

// Plausible defects: a step finer or coarser than the measured angles
// allow, or values between measured angles that are not linear in the
// angle, or light beyond the last measured vertical angle. The oracle is
// hand arithmetic: vertical angles 0°, 10°, 60° and 90° share a step of
// 10°, so the grid's 10 columns hold them; 20° to 50° lie a fifth, two,
// three and four fifths of the way from 10° to 60°, and 70° and 80° a
// third and two thirds of the way from 60° to 90°.
#[wasm_bindgen_test(unsupported = test)]
fn the_grid_steps_hold_every_angle_and_interpolate_between_them() {
    let profile = LightProfile::from_ies(&plain(
        &[0., 10., 60., 90.],
        &[0.],
        &[400., 1000., 200., 50.],
    ))
    .unwrap();
    assert_eq!(profile.size, [10, 1]);
    let expected = [0.4, 1., 0.84, 0.68, 0.52, 0.36, 0.2, 0.15, 0.1, 0.05];
    for (column, want) in expected.into_iter().enumerate() {
        assert_close(
            profile.sample(column as u32, 0),
            want,
            &format!("{column}0 degrees"),
        );
    }
}

// Plausible defects: a horizontal step finer than the file needs that
// still lands on its angles, which wastes texels but passes the grid
// test above, or one that misses an angle. The oracle is the file's angles
// 0°, 22.5°, 45°, …, 360°: their step is 22.5°, so 17 rows.
#[wasm_bindgen_test(unsupported = test)]
fn the_horizontal_step_is_the_file_s() {
    let horizontal: Vec<f64> = (0..17).map(|i| f64::from(i) * 22.5).collect();
    let candela: Vec<f64> = (0..17).flat_map(|_| [1000., 500.]).collect();
    let profile = LightProfile::from_ies(&plain(&[0., 90.], &horizontal, &candela)).unwrap();
    assert_eq!(profile.size, [2, 17]);
}

// Plausible defect: a file measured more finely than the grid's finest
// step taken at its own step, an unbounded grid. The oracle is the stated
// bound: vertical angles every 0.1° take the 0.25° step, 361 columns.
#[wasm_bindgen_test(unsupported = test)]
fn a_finer_file_takes_the_finest_step() {
    let vertical: Vec<f64> = (0..=900).map(|i| f64::from(i) / 10.).collect();
    let candela: Vec<f64> = vertical.iter().map(|angle| 1000. - angle).collect();
    let profile = LightProfile::from_ies(&plain(&vertical, &[0.], &candela)).unwrap();
    assert_eq!(profile.size, [361, 1]);
    // 0.25° lies between 0.2° and 0.3°, where the file is linear.
    assert_close(profile.sample(1, 0), (1000. - 0.25) / 1000., "0.25°");
}

/// The grid's value at horizontal angle `degrees` (a multiple of its step)
/// on the vertical sample `column`.
fn at(profile: &LightProfile, column: u32, degrees: f64) -> f32 {
    let step = 360. / f64::from(profile.size[1] - 1);
    let row = degrees / step;
    assert_eq!(row.fract(), 0., "{degrees}° is not a row");
    profile.sample(column, row as u32)
}

// Plausible defects: a quadrant file mirrored only once, or about the
// wrong plane. The oracle is LM-63's quadrant symmetry: each quadrant
// mirrors the first, so I(180° − φ) = I(180° + φ) = I(360° − φ) = I(φ).
#[wasm_bindgen_test(unsupported = test)]
fn a_quadrant_file_mirrors_into_every_quadrant() {
    let profile = LightProfile::from_ies(&plain(
        &[0., 90.],
        &[0., 30., 90.],
        &[1000., 100., 1000., 300., 1000., 700.],
    ))
    .unwrap();
    assert_eq!(profile.size[1], 13, "a 30° step round the circle");
    for (degrees, want) in [
        (30., 0.3),
        (150., 0.3),
        (210., 0.3),
        (330., 0.3),
        (90., 0.7),
        (270., 0.7),
        (180., 0.1),
        (360., 0.1),
    ] {
        assert_close(at(&profile, 1, degrees), want, &format!("{degrees}°"));
    }
}

// Plausible defect: a bilateral file mirrored about the 90°–270° plane.
// The oracle is LM-63's symmetry about the 0°–180° plane: I(360° − φ) =
// I(φ).
#[wasm_bindgen_test(unsupported = test)]
fn a_bilateral_file_mirrors_about_its_0_to_180_plane() {
    let profile = LightProfile::from_ies(&plain(
        &[0., 90.],
        &[0., 90., 180.],
        &[1000., 100., 1000., 500., 1000., 900.],
    ))
    .unwrap();
    for (degrees, want) in [(0., 0.1), (90., 0.5), (180., 0.9), (270., 0.5), (360., 0.1)] {
        assert_close(at(&profile, 1, degrees), want, &format!("{degrees}°"));
    }
}

// Plausible defect: a file measured from 90° to 270° shifted to start at
// 0°, which turns it a quarter turn (as three.js r187 does). The oracle is
// LM-63's symmetry about the 90°–270° plane: I(180° − φ) = I(φ), so 0°
// and 360° take the 180° plane, and the measured planes keep their angles.
#[wasm_bindgen_test(unsupported = test)]
fn a_file_from_90_to_270_mirrors_about_that_plane_in_place() {
    let profile = LightProfile::from_ies(&plain(
        &[0., 90.],
        &[90., 180., 270.],
        &[1000., 200., 1000., 600., 1000., 400.],
    ))
    .unwrap();
    for (degrees, want) in [(0., 0.6), (90., 0.2), (180., 0.6), (270., 0.4), (360., 0.6)] {
        assert_close(at(&profile, 1, degrees), want, &format!("{degrees}°"));
    }
}

// Plausible defect: a full file that leaves out its 360° plane left
// without one, so the angles past its last plane read nothing or wrap
// wrongly. The oracle is LM-63's 360° plane, which is its 0° one.
#[wasm_bindgen_test(unsupported = test)]
fn a_full_file_without_its_360_plane_closes_on_its_0_plane() {
    let profile = LightProfile::from_ies(&plain(
        &[0., 90.],
        &[0., 90., 180., 270.],
        &[1000., 100., 1000., 300., 1000., 500., 1000., 700.],
    ))
    .unwrap();
    for (degrees, want) in [(270., 0.7), (360., 0.1)] {
        assert_close(at(&profile, 1, degrees), want, &format!("{degrees}°"));
    }
}

// Plausible defect: a tilt block read as lamp data. The oracle is the
// file without one: the same numbers after it read alike.
#[wasm_bindgen_test(unsupported = test)]
fn an_included_tilt_block_is_read_past() {
    let (candela, plain) = asymmetric();
    let tilted = file(
        "INCLUDE\n1\n3\n0 45 90\n1.0 0.95 0.9",
        1.,
        &[0., 30., 60., 90.],
        &ROUND,
        &candela,
    );
    assert_eq!(
        LightProfile::from_ies(&tilted).unwrap(),
        LightProfile::from_ies(&plain).unwrap()
    );
}

// Plausible defects: a file a profile cannot read taken anyway, with its
// numbers misplaced or its light where LM-63 puts none. The oracle is the
// contract's list of refusals.
#[wasm_bindgen_test(unsupported = test)]
fn files_a_profile_cannot_read_are_refused() {
    let read = |text: &str| LightProfile::from_ies(text).unwrap_err();
    let (candela, text) = asymmetric();
    assert!(matches!(
        read("IESNA:LM-63-2002\n1 2 3"),
        IesError::Malformed(_)
    ));
    assert!(matches!(
        read(text.trim_end().rsplit_once('\n').unwrap().0),
        IesError::Malformed(_)
    ));
    assert!(matches!(
        read(&text.replace("TILT=NONE", "TILT=NONE\nx")),
        IesError::Malformed(_)
    ));
    assert_eq!(
        read(&text.replace("TILT=NONE", "TILT=lamp.tlt")),
        IesError::TiltFile
    );
    for kind in ["2", "3"] {
        let typed = text.replace(" 1 2 0.1", &format!(" {kind} 2 0.1"));
        assert_eq!(
            read(&typed),
            IesError::UnsupportedPhotometricType(kind.parse().unwrap())
        );
    }
    assert_eq!(
        read(&plain(&[0., 60., 30.], &[0.], &[1., 1., 1.])),
        IesError::InvalidAngles
    );
    // LM-63's type C vertical ranges run from 0° or 90° to 90° or 180°.
    for vertical in [[0., 60.], [10., 90.], [0., 120.]] {
        assert_eq!(
            read(&plain(&vertical, &[0.], &[1., 1.])),
            IesError::InvalidAngles
        );
    }
    assert_eq!(
        read(&plain(&[0., 90.], &[0., 45., 135.], &[1.; 6])),
        IesError::InvalidAngles
    );
    assert_eq!(
        read(&plain(&[0., 90.], &[90., 180.], &[1.; 4])),
        IesError::InvalidAngles
    );
    let mut negative = candela.clone();
    negative[3] = -1.;
    assert_eq!(
        read(&plain(&[0., 30., 60., 90.], &ROUND, &negative)),
        IesError::InvalidValues
    );
    assert_eq!(
        read(&file("NONE", 0., &[0., 90.], &[0.], &[1., 1.])),
        IesError::InvalidValues
    );
    assert_eq!(
        read(&plain(&[90., 180.], &[0.], &[0., 1000.])),
        IesError::NoLight
    );
    assert_eq!(
        read(&plain(&[0., 90.], &[0.], &[0., 0.])),
        IesError::NoLight
    );
}
