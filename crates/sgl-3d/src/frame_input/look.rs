//! The frame's authored image: exposure, bloom, motion blur and colour
//! grading. Values and defaults are Bevy 9d12036's (`bevy_post_process`
//! `AutoExposure`, `AutoExposureCompensationCurve`, `Bloom` and `MotionBlur`,
//! `bevy_render` `ColorGrading`).

/// The frame's one exposure, which the tone map and FSR2 both take.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Exposure {
    /// Stops applied to the scene's linear light before tone mapping: 0
    /// leaves it, +1 doubles it. With `automatic` it is what metering sees
    /// and what the correction adds to, as Bevy's camera exposure is.
    pub stops: f32,
    /// Automatic exposure; `None` keeps the frame at `stops`.
    pub automatic: Option<AutoExposure>,
}

/// Bevy's auto exposure: a 64-bin histogram of the frame's log2 luminance
/// (after `Exposure::stops`) from -8 to 8 through a metering mask, whose
/// average without the darkest and brightest 10% of samples sets a target
/// that makes that average the compensation curve's stops above 1.
/// Luminance below the range is metered at -8; above it counts in the
/// highest bin. The correction moves toward the target at the authored
/// speeds, linearly while far from it and exponentially within 1.5 stops,
/// against jitter when the target keeps moving slightly, never stepping past
/// it however long the frame, and restarts at the target when history does. `frame_time_ms` times it. The range, filter and
/// 1.5 stops are Bevy's defaults and SGL3D's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoExposure {
    /// Stops per second at which the exposure follows a scene that got
    /// brighter.
    pub speed_brighten: f32,
    /// Stops per second at which the exposure follows a scene that got
    /// darker.
    pub speed_darken: f32,
    /// The least correction in stops, relative to `Exposure::stops`: the
    /// limit on darkening. With a nonfinite limit, or one above
    /// `correction_max`, the correction is unlimited both ways, as in Bevy.
    pub correction_min: f32,
    /// The greatest correction in stops: the limit on brightening. With a
    /// nonfinite limit, or one below `correction_min`, the correction is
    /// unlimited both ways, as in Bevy.
    pub correction_max: f32,
    pub compensation: CompensationCurve,
    pub metering_mask: MeteringMask,
}

impl Default for AutoExposure {
    /// Bevy's: brightening at 3 stops per second and darkening at 1, an
    /// unlimited correction, no compensation and every pixel metered alike.
    fn default() -> Self {
        Self {
            speed_brighten: 3.,
            speed_darken: 1.,
            correction_min: f32::MIN,
            correction_max: f32::MAX,
            compensation: CompensationCurve::default(),
            metering_mask: MeteringMask::UNIFORM,
        }
    }
}

/// Bevy's exposure compensation curve: stops added to the target for the
/// frame's metered average log2 luminance. Linear between its points and
/// constant beyond its ends; with none it adds nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CompensationCurve {
    points: [[f32; 2]; CompensationCurve::MAX_POINTS],
    len: usize,
}

/// Why a compensation curve was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompensationCurveError {
    /// More than `CompensationCurve::MAX_POINTS` points.
    TooManyPoints,
    /// A coordinate is not finite.
    NotFinite,
    /// The luminances do not strictly increase.
    NotIncreasing,
}

impl std::fmt::Display for CompensationCurveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooManyPoints => "a compensation curve has at most eight points",
            Self::NotFinite => "a compensation curve point is not finite",
            Self::NotIncreasing => "compensation curve luminances must strictly increase",
        })
    }
}

impl std::error::Error for CompensationCurveError {}

impl CompensationCurve {
    pub const MAX_POINTS: usize = 8;

    /// The curve through `points`, each a log2 luminance and the stops of
    /// compensation there, in increasing luminance.
    pub fn new(points: &[[f32; 2]]) -> Result<Self, CompensationCurveError> {
        if points.len() > Self::MAX_POINTS {
            return Err(CompensationCurveError::TooManyPoints);
        }
        if points.iter().flatten().any(|value| !value.is_finite()) {
            return Err(CompensationCurveError::NotFinite);
        }
        if points.windows(2).any(|pair| pair[1][0] <= pair[0][0]) {
            return Err(CompensationCurveError::NotIncreasing);
        }
        let mut curve = Self::default();
        curve.points[..points.len()].copy_from_slice(points);
        curve.len = points.len();
        Ok(curve)
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.points[..self.len]
    }
}

/// Bevy's metering mask as a 16×16 grid over the frame, rows top to bottom:
/// each weight scales how much the pixels under it count in metering, from
/// 0 (not at all) to 255 (fully), quantized to 16 levels as Bevy does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeteringMask {
    pub weights: [[u8; 16]; 16],
}

impl MeteringMask {
    /// Every pixel counts fully.
    pub const UNIFORM: Self = Self {
        weights: [[255; 16]; 16],
    };
}

/// Bevy's energy-conserving bloom: the scene downsampled through a mip
/// chain and upsampled back, each level blended into the next finer one,
/// and the result blended into the scene. SGL3D shapes the halo as Bevy's
/// `Bloom::NATURAL` does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomParameters {
    /// 0..=1: how likely light is to scatter; 0 is no bloom.
    pub intensity: f32,
}

impl Default for BloomParameters {
    /// Bevy's `Bloom::NATURAL`.
    fn default() -> Self {
        Self { intensity: 0.15 }
    }
}

/// Motion blur's authored strength, which `settings::MotionBlur` scales.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionBlurParameters {
    /// The share of the frame's interval the shutter is open: each pixel
    /// blurs over this share of its motion since the last frame, centred on
    /// it. 0.5 is film's 180°; 0 is no blur. Above 1 blurs further than
    /// anything moved, as Bevy allows for effect. Blur follows the frame
    /// rate: a faster frame moves less and blurs less.
    pub shutter_angle: f32,
}

impl Default for MotionBlurParameters {
    /// Bevy's: a 180° shutter.
    fn default() -> Self {
        Self { shutter_angle: 0.5 }
    }
}

/// Bevy's colour grading, applied to the exposed scene before tone mapping,
/// except `global.post_saturation`, after it, and Filament's AgX look,
/// within the tone map. The default changes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorGrading {
    pub global: ColorGradingGlobal,
    /// Applied to the darker parts of the image.
    pub shadows: ColorGradingSection,
    pub midtones: ColorGradingSection,
    /// Applied to the lighter parts of the image.
    pub highlights: ColorGradingSection,
    pub agx_look: AgxLook,
}

/// Filament's AgX looks (`AgxToneMapper::AgxLook`): a creative contrast and
/// saturation within AgX, after its curve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgxLook {
    /// AgX's base contrast, with no look.
    #[default]
    None,
    /// More contrast and saturation, for sRGB displays.
    Punchy,
    /// A golden tint, slightly washed out, for BT.1886 displays.
    Golden,
}

/// Grading of the whole image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGradingGlobal {
    /// Subtracted from the white point's CIE 1931 x: positive is warmer
    /// (redder), negative cooler. Luminance is unchanged.
    pub temperature: f32,
    /// Added to the white point's CIE 1931 y: positive is more magenta,
    /// negative greener.
    pub tint: f32,
    /// Hue rotation in radians.
    pub hue: f32,
    /// Saturation after tone mapping: 0 is greyscale, 1 unchanged.
    pub post_saturation: f32,
    /// Where midtones start and end, as the exposed scene's mean of R, G and
    /// B; sections blend over 0.1 either side.
    pub midtones_start: f32,
    pub midtones_end: f32,
}

impl Default for ColorGradingGlobal {
    fn default() -> Self {
        Self {
            temperature: 0.,
            tint: 0.,
            hue: 0.,
            post_saturation: 1.,
            midtones_start: 0.2,
            midtones_end: 0.7,
        }
    }
}

/// Grading of one section: saturation and contrast, then the ASC CDL
/// `(colour × gain + lift)^(1 / gamma)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGradingSection {
    /// 0 is greyscale, 1 unchanged.
    pub saturation: f32,
    /// Spread about 0.5: 1 unchanged.
    pub contrast: f32,
    pub gamma: f32,
    pub gain: f32,
    pub lift: f32,
}

impl Default for ColorGradingSection {
    fn default() -> Self {
        Self {
            saturation: 1.,
            contrast: 1.,
            gamma: 1.,
            gain: 1.,
            lift: 0.,
        }
    }
}
