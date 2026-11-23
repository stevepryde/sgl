//! A measured luminaire's intensity by direction (a light profile), read
//! from an IESNA LM-63 file and kept as the grid a spot light's profile
//! samples (`shading/light_profile.wgsl`).
//!
//! The reading is three.js r187's (7e89746a687bb88271253698fa108b1dd59825d0)
//! `examples/jsm/loaders/IESLoader.js` (`IESLamp`, `_getIESValues`), MIT
//! (`src/stages/post/smaa/LICENSE-three.txt`): the keyword lines up to
//! `TILT=`, an included tilt block read past, the lamp and ballast
//! numbers, the angles and the candela values; the candela values times
//! the multiplier and ballast factor; a symmetric
//! file's horizontal angles mirrored round the full circle, a last angle
//! between 180° and 360° closed by a 360° entry equal to the 0° one, as
//! three.js and Blender's Cycles do; values bilinear in the vertical and
//! horizontal angles between the measured ones; and the grid divided by
//! the file's peak. Changes: the numbers are read as one stream whatever
//! their line breaks; a file of photometric type A or B, tilt data in
//! another file, a horizontal range LM-63 does not define, or a number that
//! does not read is refused, where three.js reads every file as type C and
//! guesses at the rest, as it does a vertical range LM-63 does not define
//! for type C; the second ballast number multiplies only in the versions
//! that define it as the ballast-lamp photometric factor (1995 and before),
//! where three.js multiplies it in every version, though LM-63-2002 leaves
//! it for future use and LM-63-2019 makes it the file's generation type
//! (three.js normalises it away, but `peak_candela` keeps the multipliers);
//! a file measured from 90° to 270° is mirrored about
//! its 90°–270° plane in place, where three.js shifts it to start at 0°,
//! which turns it a quarter turn; the vertical angles span only 0° to 90°,
//! all a spot can shine through; and the grid's steps are the coarsest that
//! hold every measured angle, down to 0.25° vertically and 1° round the
//! luminaire, so the measured values of a file no finer are its samples
//! and its bilinear interpolation is the file's, where three.js resamples
//! every file to whole degrees. A finer file is resampled linearly to those
//! steps, so a peak between them is lowered.
//!
//! Type C photometry: vertical angle 0° is the nadir, the direction a spot
//! shines; horizontal angle 0° lies along the luminaire's length
//! (LM-63-2002, as Autodesk's "About IES Standard File Format" restates it:
//! "oriented parallel to the length of the luminaire"), and the horizontal
//! angles are "measured counterclockwise in a horizontal plane for Type C
//! photometry" (LM-63-2002's horizontal angles). That they turn
//! counterclockwise seen from above, from its zenith side, is SGL3D's
//! reading: LM-63 states no viewpoint, and the nearest statement is IES
//! LM-72-97's counterclockwise-from-east aiming system, which Photometric
//! Toolbox's "Modify Luminaire Test Position" describes "In Plan View
//! (looking down on the luminaire)" with the zero degree plane due east and
//! "+X axis tends toward +Y" looking toward -Z. Issue #885 tracks
//! confirming it; a mirrored reading would mirror asymmetric profiles.

/// The finest vertical step a profile's grid takes, in hundredths of a
/// degree: a file measured more finely is resampled to it, at most 361
/// samples from 0° to 90°.
const FINEST_VERTICAL: u32 = 25;
/// The finest horizontal step, in hundredths of a degree: at most 361
/// samples round the circle.
const FINEST_HORIZONTAL: u32 = 100;
/// 90° and 360°, in hundredths of a degree.
const RIGHT_ANGLE: u32 = 9000;
const FULL_TURN: u32 = 36000;

/// Why an LM-63 file is not a profile `LightProfile::from_ies` reads.
#[derive(Clone, Debug, PartialEq)]
pub enum IesError {
    /// The file has no `TILT=` line, too few numbers, or a number that does
    /// not read, or a count that is not a positive whole number.
    Malformed(&'static str),
    /// Its photometry is type A (3) or B (2): only type C (1) is read.
    UnsupportedPhotometricType(f64),
    /// `TILT=` names another file, which a profile cannot read.
    TiltFile,
    /// Its angles do not ascend, its vertical angles are not a range LM-63
    /// defines for type C (from 0° or 90° to 90° or 180°), or its
    /// horizontal angles are not one:
    /// 0° alone, 0° to 90°, 0° to 180°, 90° to 270°, or 0° to more than
    /// 180° and at most 360°.
    InvalidAngles,
    /// A candela value is negative or not finite, or the multiplier, the
    /// ballast factor or (before LM-63-2002) the ballast-lamp factor is not
    /// positive and finite.
    InvalidValues,
    /// It sends no light below 90° from its nadir, all a spot shines
    /// through.
    NoLight,
}

impl std::fmt::Display for IesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed LM-63 file: {what}"),
            Self::UnsupportedPhotometricType(kind) => write!(
                f,
                "LM-63 photometric type {kind} is not type C (1), the only one a light profile reads"
            ),
            Self::TiltFile => write!(f, "LM-63 tilt data in another file is not read"),
            Self::InvalidAngles => write!(
                f,
                "LM-63 angles must ascend, vertical ones from 0 or 90 to 90 or 180 degrees, horizontal ones 0, 0 to 90, 0 to 180, 90 to 270 or 0 to above 180 and at most 360"
            ),
            Self::InvalidValues => write!(
                f,
                "LM-63 candela values must be finite and nonnegative, and its multiplier and ballast factors positive"
            ),
            Self::NoLight => write!(
                f,
                "the LM-63 file sends no light below 90 degrees from its nadir"
            ),
        }
    }
}

impl std::error::Error for IesError {}

/// A measured luminaire's luminous intensity by direction, read from an
/// IESNA LM-63 (IES) file of type C photometry, which a spot light takes as
/// its profile (`SpotProfile`) once a scene holds it
/// (`Scene::add_light_profile`). It keeps its intensity from the nadir to
/// 90° and round the full circle, divided by the file's peak, so the
/// file's brightest measured direction is 1: a light with it shines
/// `Light::intensity` candela there and the profile's share of that
/// elsewhere. `peak_candela` is the file's own peak, for a light that
/// should shine as measured. Its grid holds every measured angle of a file
/// measured no finer than 0.25° from the nadir and 1° round the luminaire;
/// a finer file is resampled linearly to those steps.
#[derive(Clone, Debug, PartialEq)]
pub struct LightProfile {
    /// Its normalised intensity at each grid sample: `size[0]` vertical
    /// angles from 0° to 90° (x), for each of `size[1]` horizontal angles
    /// from 0° to 360° (y), or one for an axially symmetric file.
    pub(crate) samples: Vec<f32>,
    pub(crate) size: [u32; 2],
    peak_candela: f32,
}

/// The file's angles and candela values, as read.
struct Photometry {
    vertical: Vec<f64>,
    horizontal: Vec<f64>,
    /// `horizontal.len()` planes of `vertical.len()` values each.
    candela: Vec<f64>,
    /// The multiplier, ballast factor and, before LM-63-2002, ballast-lamp
    /// factor's product.
    scale: f64,
}

impl LightProfile {
    /// The profile an LM-63 file's text describes: any of its versions
    /// (1986, 1991, 1995, 2002, 2019), with `TILT=NONE` or `TILT=INCLUDE`,
    /// whose tilt factors, which describe the lamp at other angles than it
    /// was measured at, are read past. Refuses a file `IesError` names.
    pub fn from_ies(text: &str) -> Result<Self, IesError> {
        let photometry = read(text)?;
        let vertical = &photometry.vertical;
        let planes = photometry.horizontal.len();
        let peak = photometry.candela.iter().copied().fold(0., f64::max);
        let below_horizon = (0..planes).any(|plane| {
            vertical.iter().enumerate().any(|(at, &angle)| {
                angle < 90. && photometry.candela[plane * vertical.len() + at] > 0.
            })
        });
        if peak <= 0. || !below_horizon {
            return Err(IesError::NoLight);
        }
        let horizontal = Horizontal::new(&photometry.horizontal)?;
        let vertical_step = step(
            vertical.iter().copied().filter(|&angle| angle <= 90.),
            RIGHT_ANGLE,
            FINEST_VERTICAL,
        );
        let horizontal_step = step(
            photometry.horizontal.iter().copied(),
            horizontal.period(),
            FINEST_HORIZONTAL,
        );
        let columns = RIGHT_ANGLE / vertical_step + 1;
        let rows = if planes == 1 {
            1
        } else {
            FULL_TURN / horizontal_step + 1
        };
        let mut samples = Vec::with_capacity((columns * rows) as usize);
        for row in 0..rows {
            let azimuth = f64::from(row * horizontal_step) / 100.;
            for column in 0..columns {
                let polar = f64::from(column * vertical_step) / 100.;
                let value = horizontal.blend(azimuth, |plane| {
                    let values = &photometry.candela[plane * vertical.len()..][..vertical.len()];
                    along(vertical, values, polar)
                });
                samples.push((value / peak) as f32);
            }
        }
        Ok(Self {
            samples,
            size: [columns, rows],
            peak_candela: (peak * photometry.scale) as f32,
        })
    }

    /// The file's brightest luminous intensity in candela: its largest
    /// value times its candela multiplier and ballast factor, and in a file
    /// older than LM-63-2002 its ballast-lamp photometric factor (2002
    /// leaves that number for future use and 2019 makes it the file's
    /// generation type, neither a multiplier). A spot of this `intensity`
    /// shines as the file measured.
    pub fn peak_candela(&self) -> f32 {
        self.peak_candela
    }

    /// Its normalised intensity at grid sample `[column, row]`: vertical
    /// angle `column` steps from the nadir, horizontal angle `row` steps
    /// from 0°.
    #[cfg(test)]
    pub(crate) fn sample(&self, column: u32, row: u32) -> f32 {
        self.samples[(row * self.size[0] + column) as usize]
    }
}

/// The numbers after `TILT=` as LM-63 lays them out.
fn read(text: &str) -> Result<Photometry, IesError> {
    // LM-63-2002 and 2019 name themselves on their first line; the second
    // ballast number is a multiplier only before them.
    let first = text.lines().map(str::trim).find(|line| !line.is_empty());
    let ballast_lamp_factor = !first.is_some_and(|line| {
        line.starts_with("IESNA:LM-63-2002") || line.starts_with("IES:LM-63-2019")
    });
    let mut lines = text.lines();
    let tilt = lines
        .by_ref()
        .find_map(|line| line.trim_start().strip_prefix("TILT="))
        .ok_or(IesError::Malformed("no TILT= line"))?
        .trim();
    let rest: Vec<&str> = lines
        .flat_map(|line| line.split(|c: char| c.is_whitespace() || c == ','))
        .filter(|token| !token.is_empty())
        .collect();
    let mut numbers = rest.iter().map(|token| {
        token
            .parse::<f64>()
            .map_err(|_| IesError::Malformed("a value is not a number"))
    });
    let mut take = |n: usize| -> Result<Vec<f64>, IesError> {
        (0..n)
            .map(|_| {
                numbers
                    .next()
                    .unwrap_or(Err(IesError::Malformed("too few values")))
            })
            .collect()
    };
    let count = |value: f64| -> Result<usize, IesError> {
        if value.fract() == 0. && value >= 1. && value <= f64::from(u32::MAX) {
            Ok(value as usize)
        } else {
            Err(IesError::Malformed(
                "a count is not a positive whole number",
            ))
        }
    };
    match tilt {
        "NONE" => {}
        "INCLUDE" => {
            // The lamp-to-luminaire geometry and the tilt angles' count,
            // then as many angles and as many multiplying factors.
            let angles = count(take(2)?[1])?;
            take(2 * angles)?;
        }
        _ => return Err(IesError::TiltFile),
    }
    // Lamps, lumens a lamp, the candela multiplier, the vertical and
    // horizontal angles' counts, the photometric type, the units, width,
    // length and height; then the ballast factor, the ballast-lamp factor
    // and the input watts.
    let lamp = take(10)?;
    let ballast = take(3)?;
    let (multiplier, kind) = (lamp[2], lamp[5]);
    if kind != 1. {
        return Err(IesError::UnsupportedPhotometricType(kind));
    }
    let (vertical, horizontal) = (count(lamp[3])?, count(lamp[4])?);
    if vertical < 2 {
        return Err(IesError::Malformed("fewer than two vertical angles"));
    }
    let vertical = take(vertical)?;
    let horizontal = take(horizontal)?;
    let candela = take(vertical.len() * horizontal.len())?;
    let ascending = |angles: &[f64]| {
        angles.iter().all(|angle| angle.is_finite())
            && angles.windows(2).all(|pair| pair[0] < pair[1])
    };
    if !ascending(&vertical)
        || !ascending(&horizontal)
        || ![0., 90.].contains(&vertical[0])
        || ![90., 180.].contains(&vertical[vertical.len() - 1])
    {
        return Err(IesError::InvalidAngles);
    }
    let factor = |value: f64| value.is_finite() && value > 0.;
    if !candela
        .iter()
        .all(|value| value.is_finite() && *value >= 0.)
        || !factor(multiplier)
        || !factor(ballast[0])
        || (ballast_lamp_factor && !factor(ballast[1]))
    {
        return Err(IesError::InvalidValues);
    }
    let lamp_factor = if ballast_lamp_factor { ballast[1] } else { 1. };
    Ok(Photometry {
        vertical,
        horizontal,
        candela,
        scale: multiplier * ballast[0] * lamp_factor,
    })
}

/// How a file's horizontal angles cover the circle (LM-63-2002, type C).
enum Symmetry {
    /// One plane: the same in every direction about the axis.
    Axial,
    /// 0° to 90°: each quadrant mirrors the first.
    Quadrant,
    /// 0° to 180°: mirrored about the 0°–180° plane.
    Bilateral,
    /// 90° to 270°: mirrored about the 90°–270° plane.
    BilateralAcross,
    /// 0° to 360°.
    Full,
}

/// A file's horizontal angles, with the 360° plane three.js and Cycles add
/// to a full file that leaves it out.
struct Horizontal {
    symmetry: Symmetry,
    angles: Vec<f64>,
    /// Each of `angles`' plane in the file: the last is the first again
    /// where the 360° plane was added.
    planes: Vec<usize>,
}

impl Horizontal {
    fn new(angles: &[f64]) -> Result<Self, IesError> {
        let (first, last) = (angles[0], angles[angles.len() - 1]);
        let mut planes: Vec<usize> = (0..angles.len()).collect();
        let mut angles = angles.to_vec();
        let symmetry = if angles.len() == 1 {
            Symmetry::Axial
        } else if first == 0. && last == 90. {
            Symmetry::Quadrant
        } else if first == 0. && last == 180. {
            Symmetry::Bilateral
        } else if first == 90. && last == 270. {
            Symmetry::BilateralAcross
        } else if first == 0. && last > 180. && last <= 360. {
            if last < 360. {
                angles.push(360.);
                planes.push(0);
            }
            Symmetry::Full
        } else {
            return Err(IesError::InvalidAngles);
        };
        Ok(Self {
            symmetry,
            angles,
            planes,
        })
    }

    /// The span, in hundredths of a degree, whose multiples every mirror
    /// of the measured angles lies on: 180° for a mirrored file (whose
    /// mirrors are 180° less or more than an angle, or 360° less), 360°
    /// for a full one.
    fn period(&self) -> u32 {
        match self.symmetry {
            Symmetry::Full => FULL_TURN,
            _ => FULL_TURN / 2,
        }
    }

    /// The file's value at horizontal angle `azimuth` (0° to 360°): the
    /// measured planes either side of it, or of the angle it mirrors, each
    /// `value`d, blended linearly in angle.
    fn blend(&self, azimuth: f64, value: impl Fn(usize) -> f64) -> f64 {
        let azimuth = match self.symmetry {
            Symmetry::Axial => return value(0),
            Symmetry::Quadrant => {
                let half = azimuth % 180.;
                if half > 90. { 180. - half } else { half }
            }
            Symmetry::Bilateral => {
                if azimuth > 180. {
                    360. - azimuth
                } else {
                    azimuth
                }
            }
            Symmetry::BilateralAcross => {
                if azimuth < 90. {
                    180. - azimuth
                } else if azimuth > 270. {
                    540. - azimuth
                } else {
                    azimuth
                }
            }
            Symmetry::Full => azimuth,
        };
        let (at, weight) = interval(&self.angles, azimuth);
        lerp(value(self.planes[at]), value(self.planes[at + 1]), weight)
    }
}

/// The interval of ascending `angles` that holds `angle` (within their
/// range), as its first index and `angle`'s weight toward its second.
fn interval(angles: &[f64], angle: f64) -> (usize, f64) {
    let at = angles
        .windows(2)
        .position(|pair| angle <= pair[1])
        .unwrap_or(angles.len() - 2);
    (at, (angle - angles[at]) / (angles[at + 1] - angles[at]))
}

/// A plane's value at vertical angle `angle`: linear between the measured
/// angles about it, and zero below a file's that start at 90°.
fn along(angles: &[f64], values: &[f64], angle: f64) -> f64 {
    if angle < angles[0] || angle > angles[angles.len() - 1] {
        return 0.;
    }
    let (at, weight) = interval(angles, angle);
    lerp(values[at], values[at + 1], weight)
}

/// `a` and `b` mixed by `weight`, exactly `a` at 0 and `b` at 1, so a
/// measured angle takes its measured value.
fn lerp(a: f64, b: f64, weight: f64) -> f64 {
    a * (1. - weight) + b * weight
}

/// The coarsest step, in hundredths of a degree, that `angles` (degrees)
/// and `span` are all multiples of, and no finer than `finest`: the
/// largest common divisor of them, or `finest` where an angle is not a
/// whole number of hundredths or that divisor is finer.
fn step(angles: impl Iterator<Item = f64>, span: u32, finest: u32) -> u32 {
    let gcd = |mut a: u32, mut b: u32| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let mut divisor = span;
    for angle in angles {
        let hundredths = (angle * 100.).round();
        if (angle * 100. - hundredths).abs() > 1e-6 {
            return finest;
        }
        divisor = gcd(divisor, hundredths as u32 % span);
    }
    if divisor < finest {
        // A whole number of `finest` that divides `span`: `finest` divides
        // both spans.
        finest
    } else {
        divisor
    }
}

#[cfg(test)]
mod tests;
