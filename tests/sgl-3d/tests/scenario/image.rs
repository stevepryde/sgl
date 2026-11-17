//! Decoded frame targets, regions chosen by what was drawn, and the metrics
//! a scenario asserts on over a region.
use glam::Vec2;

/// A decoded target: four channels a texel, row-major. Depth, identity and
/// single-channel targets fill R; integer channels are exact below 2^24.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub size: [u32; 2],
    pub texels: Vec<[f32; 4]>,
}

impl Image {
    pub fn at(&self, [x, y]: [u32; 2]) -> [f32; 4] {
        self.texels[(y * self.size[0] + x) as usize]
    }

    /// The mean of a texel's RGB: the radiance of the grey fixtures.
    pub fn value(&self, pixel: [u32; 2]) -> f32 {
        let [r, g, b, _] = self.at(pixel);
        (r + g + b) / 3.
    }

    /// The box filter of each `factor` by `factor` block of RGB in the space
    /// `to` maps a channel into and `from` maps back: a resolve that
    /// averages its samples there converges on this. Alpha is averaged as
    /// it is.
    pub fn downsampled_through(
        &self,
        factor: u32,
        to: impl Fn(f32) -> f32,
        from: impl Fn(f32) -> f32,
    ) -> Image {
        let size = self.size.map(|s| s / factor);
        let weight = 1. / (factor * factor) as f32;
        let texels = pixels(size)
            .map(|[x, y]| {
                let mut sum = [0.; 4];
                for dy in 0..factor {
                    for dx in 0..factor {
                        let texel = self.at([x * factor + dx, y * factor + dy]);
                        for c in 0..3 {
                            sum[c] += to(texel[c]) * weight;
                        }
                        sum[3] += texel[3] * weight;
                    }
                }
                [from(sum[0]), from(sum[1]), from(sum[2]), sum[3]]
            })
            .collect();
        Image { size, texels }
    }

    /// The pixels whose R holds `id` (a `SourceId` target's instance).
    pub fn region_of(&self, id: u32) -> Region {
        Region::from_fn(self.size, |pixel| self.at(pixel)[0] == id as f32)
    }
}

/// Every pixel of a target of `size`, row by row.
pub fn pixels(size: [u32; 2]) -> impl Iterator<Item = [u32; 2]> {
    (0..size[1]).flat_map(move |y| (0..size[0]).map(move |x| [x, y]))
}

/// The pixel's centre.
pub fn centre([x, y]: [u32; 2]) -> Vec2 {
    Vec2::new(x as f32 + 0.5, y as f32 + 0.5)
}

/// A set of pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub size: [u32; 2],
    pub mask: Vec<bool>,
}

impl Region {
    pub fn from_fn(size: [u32; 2], mut includes: impl FnMut([u32; 2]) -> bool) -> Self {
        Self {
            size,
            mask: pixels(size).map(&mut includes).collect(),
        }
    }

    pub fn contains(&self, [x, y]: [u32; 2]) -> bool {
        self.mask[(y * self.size[0] + x) as usize]
    }

    pub fn len(&self) -> usize {
        self.mask.iter().filter(|&&m| m).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = [u32; 2]> + '_ {
        pixels(self.size).filter(|&pixel| self.contains(pixel))
    }

    /// Its pixels whose every neighbour within `by` pixels (a square) is in
    /// it, and that lie `by` pixels inside the target: its interior.
    pub fn eroded(&self, by: u32) -> Region {
        let by = by as i64;
        Region::from_fn(self.size, |[x, y]| {
            (-by..=by).all(|dy| {
                (-by..=by).all(|dx| {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    nx >= 0
                        && ny >= 0
                        && nx < self.size[0] as i64
                        && ny < self.size[1] as i64
                        && self.contains([nx as u32, ny as u32])
                })
            })
        })
    }

    pub fn and(&self, other: &Region) -> Region {
        Region::from_fn(self.size, |p| self.contains(p) && other.contains(p))
    }

    pub fn or(&self, other: &Region) -> Region {
        Region::from_fn(self.size, |p| self.contains(p) || other.contains(p))
    }

    pub fn not(&self) -> Region {
        Region::from_fn(self.size, |p| !self.contains(p))
    }
}

/// Mean and largest absolute error and root-mean-square error of an image
/// against an oracle over a region, on [`Image::value`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Error {
    pub mean: f32,
    pub max: f32,
    pub rmse: f32,
    pub pixels: usize,
    /// Where the largest error is.
    pub worst: [u32; 2],
}

/// `image` against `oracle` over `region`.
pub fn error(image: &Image, oracle: &Image, region: &Region) -> Error {
    error_by(image, region, |pixel| oracle.value(pixel))
}

/// `image` against the constant `value` over `region`.
pub fn error_to(image: &Image, value: f32, region: &Region) -> Error {
    error_by(image, region, |_| value)
}

fn error_by(image: &Image, region: &Region, truth: impl Fn([u32; 2]) -> f32) -> Error {
    assert_eq!(
        image.size, region.size,
        "an image and its region differ in size"
    );
    let mut out = Error::default();
    let (mut sum, mut squares) = (0f64, 0f64);
    for pixel in region.iter() {
        let difference = (image.value(pixel) - truth(pixel)).abs();
        sum += f64::from(difference);
        squares += f64::from(difference) * f64::from(difference);
        if difference > out.max {
            out.max = difference;
            out.worst = pixel;
        }
        out.pixels += 1;
    }
    assert!(out.pixels > 0, "a metric over an empty region");
    out.mean = (sum / out.pixels as f64) as f32;
    out.rmse = (squares / out.pixels as f64).sqrt() as f32;
    out
}

/// How much a region changes over frames at rest: the largest per-pixel
/// temporal standard deviation and the largest change from one frame to the
/// next, on `measure` of [`Image::value`] (the identity, or the space a
/// requirement is stated in).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stability {
    pub std_dev: f32,
    pub max_step: f32,
}

pub fn stability(frames: &[Image], region: &Region, measure: impl Fn(f32) -> f32) -> Stability {
    assert!(frames.len() > 1, "stability needs two frames or more");
    let mut out = Stability::default();
    for pixel in region.iter() {
        let values: Vec<f32> = frames.iter().map(|f| measure(f.value(pixel))).collect();
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        let variance =
            values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / values.len() as f32;
        out.std_dev = out.std_dev.max(variance.sqrt());
        for pair in values.windows(2) {
            out.max_step = out.max_step.max((pair[1] - pair[0]).abs());
        }
    }
    out
}

/// The first index from which every error stays at or below `epsilon`: the
/// frames a residual takes to converge.
pub fn frames_to_converge(errors: &[f32], epsilon: f32) -> Option<usize> {
    let last_above = errors.iter().rposition(|&e| e > epsilon);
    match last_above {
        None => Some(0),
        Some(i) if i + 1 < errors.len() => Some(i + 1),
        Some(_) => None,
    }
}
