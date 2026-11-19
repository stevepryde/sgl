//! The procedural sky's model (`FrameEnvironment::Sky`), shared by the sky
//! stage's tables and the opaque stage's backdrop draw: `sky.wgsl` (the
//! medium, the tables' sizes and mappings and the transmittance integral,
//! with Bruneton's mapping in `sky_bruneton.wgsl`), the Rust mirror of the
//! game's sky as the GPU takes it, and the Rust twin of the transmittance
//! integral, which `view::directional_light` resolves each directional light
//! through: its colour as it reaches the camera through the air.
use crate::content::lighting::Sky;
use glam::Vec3;

/// Bruneton's transmittance mapping and distances, part of [`SKY`].
pub(crate) static SKY_BRUNETON: super::Module = super::Module {
    name: "sky_bruneton",
    source: include_str!("sky_bruneton.wgsl"),
    deps: &[],
};
/// The sky's model: its medium, tables and mappings.
pub(crate) static SKY: super::Module = super::Module {
    name: "sky",
    source: include_str!("sky.wgsl"),
    deps: &[&SKY_BRUNETON],
};

// Twins of sky.wgsl's constants, which the twin of its integral reads.
/// The planet's surface and the top of its atmosphere, metres from its centre.
pub(crate) const SKY_GROUND_RADIUS: f32 = 6_360_000.;
pub(crate) const SKY_TOP_RADIUS: f32 = 6_460_000.;
/// Rayleigh scattering per metre at the ground, and its scale height.
pub(crate) const SKY_RAYLEIGH_SCATTERING: [f32; 3] = [5.802e-6, 13.558e-6, 33.1e-6];
pub(crate) const SKY_RAYLEIGH_SCALE_HEIGHT: f32 = 8000.;
/// Mie scattering and extinction per metre at the ground, and its scale
/// height.
pub(crate) const SKY_MIE_SCATTERING: f32 = 3.996e-6;
pub(crate) const SKY_MIE_EXTINCTION: f32 = 4.440e-6;
pub(crate) const SKY_MIE_SCALE_HEIGHT: f32 = 1200.;
/// Ozone absorption per metre at its peak, its peak's height and the tent's
/// half width.
pub(crate) const SKY_OZONE_ABSORPTION: [f32; 3] = [0.650e-6, 1.881e-6, 0.085e-6];
pub(crate) const SKY_OZONE_PEAK: f32 = 25_000.;
pub(crate) const SKY_OZONE_HALF_WIDTH: f32 = 15_000.;
/// The transmittance integral's steps.
pub(crate) const SKY_TRANSMITTANCE_SAMPLES: u32 = 40;
/// The aerial volume's columns across and down, its depth slices, and how
/// far along the view ray the last ends, metres.
pub(crate) const AERIAL_SIDE: u32 = 32;
pub(crate) const AERIAL_SLICES: u32 = 32;
pub(crate) const AERIAL_REACH: f32 = 32_000.;
/// How far above the ground the camera stands at least, metres (Bevy's
/// `clamp_to_surface` epsilon). No shader reads it: the CPU places the
/// camera (`camera_radius`), and the shaders take its radius.
const SKY_EPSILON: f32 = 1.;

/// `SkyAtmosphere` in sky.wgsl: the game's sky as the stage takes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SkyAtmosphereUniform {
    pub ground_albedo: [f32; 3],
    pub rayleigh: f32,
    pub mie: f32,
    pub ozone: f32,
    /// WGSL rounds `SkyAtmosphere` up to its 16-byte alignment.
    pub padding: [f32; 2],
}

impl SkyAtmosphereUniform {
    /// `sky`'s medium as the GPU takes it (`Sky::resolved`).
    pub fn of(sky: &Sky) -> Self {
        let sky = sky.resolved();
        Self {
            ground_albedo: sky.ground_albedo,
            rayleigh: sky.rayleigh,
            mie: sky.mie,
            ozone: sky.ozone,
            padding: [0.; 2],
        }
    }
}

/// The camera's distance from the planet's centre under `sky`: the planet's
/// radius plus the eye's height above `Sky::ground_height`, at least 1 m
/// (Bevy's `clamp_to_surface`).
pub(crate) fn camera_radius(sky: &Sky, eye: Vec3) -> f32 {
    let altitude = eye.y - sky.resolved().ground_height;
    let altitude = if altitude.is_finite() {
        altitude.max(SKY_EPSILON)
    } else {
        SKY_EPSILON
    };
    SKY_GROUND_RADIUS + altitude
}

/// `sky.wgsl`'s `sky_densities`: Rayleigh's, Mie's and ozone's density at
/// radius `r`, scaled by the game's.
fn densities(atm: &SkyAtmosphereUniform, r: f32) -> Vec3 {
    let h = (r - SKY_GROUND_RADIUS).clamp(0., SKY_TOP_RADIUS - SKY_GROUND_RADIUS);
    let rayleigh = (-h / SKY_RAYLEIGH_SCALE_HEIGHT).exp();
    let mie = (-h / SKY_MIE_SCALE_HEIGHT).exp();
    let ozone = (1. - (h - SKY_OZONE_PEAK).abs() / SKY_OZONE_HALF_WIDTH).max(0.);
    Vec3::new(rayleigh * atm.rayleigh, mie * atm.mie, ozone * atm.ozone)
}

/// The medium's extinction per metre at radius `r`: its absorption plus its
/// scattering (`sample_density`'s two components).
fn extinction(atm: &SkyAtmosphereUniform, r: f32) -> Vec3 {
    let density = densities(atm, r);
    let absorption = Vec3::splat((SKY_MIE_EXTINCTION - SKY_MIE_SCATTERING) * density.y)
        + Vec3::from(SKY_OZONE_ABSORPTION) * density.z;
    let scattering = Vec3::from(SKY_RAYLEIGH_SCATTERING) * density.x
        + Vec3::splat(SKY_MIE_SCATTERING * density.y);
    absorption + scattering
}

fn distance_to_top_atmosphere_boundary(r: f32, mu: f32) -> f32 {
    let discriminant = (r * r * (mu * mu - 1.) + SKY_TOP_RADIUS * SKY_TOP_RADIUS).max(0.);
    (-r * mu + discriminant.sqrt()).max(0.)
}

/// The zenith cosine of the planet's horizon from radius `r`.
fn horizon_mu(r: f32) -> f32 {
    -(1. - (SKY_GROUND_RADIUS * SKY_GROUND_RADIUS) / (r * r))
        .max(0.)
        .sqrt()
}

/// The twin of `sky.wgsl`'s `ray_optical_depth` for a ray that leaves the
/// atmosphere: the medium's optical depth from radius `r` along zenith
/// cosine `mu` to the top of the atmosphere, over
/// `SKY_TRANSMITTANCE_SAMPLES` uniform steps sampled at their midpoints.
fn optical_depth(atm: &SkyAtmosphereUniform, r: f32, mu: f32) -> Vec3 {
    let t_max = distance_to_top_atmosphere_boundary(r, mu);
    let dt = t_max / SKY_TRANSMITTANCE_SAMPLES as f32;
    (0..SKY_TRANSMITTANCE_SAMPLES)
        .map(|i| {
            let t = dt * (i as f32 + 0.5);
            let r_i = (t * t + 2. * r * mu * t + r * r).sqrt();
            extinction(atm, r_i) * dt
        })
        .sum()
}

/// The air's transmittance from radius `r` toward zenith cosine `mu`, as a
/// read of the transmittance table gives it: its integral to the top of the
/// atmosphere where the direction clears the planet, and below the horizon
/// the horizon's value, which the table's clamped read returns (Bevy's
/// `sample_transmittance_lut`, whose mapping holds only directions that
/// clear the planet), never the short path to the ground.
pub(crate) fn transmittance(atm: &SkyAtmosphereUniform, r: f32, mu: f32) -> Vec3 {
    let mu = mu.max(horizon_mu(r));
    (-optical_depth(atm, r, mu)).exp()
}

/// The share of a disc `angular_diameter` radians across whose centre lies
/// at zenith cosine `mu` that the planet's horizon leaves in sight from
/// radius `r`: Bevy 9d12036's `calculate_visible_sun_ratio`
/// (`atmosphere/functions.wesl`), with `acos` for its `fast_acos_4`.
pub(crate) fn visible_sun_ratio(r: f32, mu: f32, angular_diameter: f32) -> f32 {
    // The angle between the horizon and the zenith.
    let horizon_angle = horizon_mu(r).acos();
    let sun_zenith_angle = mu.clamp(-1., 1.).acos();
    let half = angular_diameter * 0.5;
    // Wholly above the horizon.
    if sun_zenith_angle + half <= horizon_angle {
        return 1.;
    }
    // Wholly below it.
    if sun_zenith_angle - half >= horizon_angle {
        return 0.;
    }
    // Bevy's linear share across the disc.
    let d = (horizon_angle - sun_zenith_angle) / half;
    (0.5 + d * 0.5).clamp(0., 1.)
}

/// The solid angle of a disc `angular_diameter` radians across, as Bevy's
/// `sample_sun_radiance` takes it: π (θ / 2)².
pub(crate) fn disc_solid_angle(angular_diameter: f32) -> f32 {
    angular_diameter * angular_diameter * 0.25 * std::f32::consts::PI
}

/// The layout this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "sky_transmittance",
        "SkyAtmosphere",
        SkyAtmosphereUniform,
        [ground_albedo, rayleigh, mie, ozone]
    )]
}

/// The constants this module shares with sky.wgsl.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    let f = |name, value| Constant::new("sky_transmittance", name, naga::Literal::F32(value));
    vec![
        f("SKY_GROUND_RADIUS", SKY_GROUND_RADIUS),
        f("SKY_TOP_RADIUS", SKY_TOP_RADIUS),
        f("SKY_RAYLEIGH_SCALE_HEIGHT", SKY_RAYLEIGH_SCALE_HEIGHT),
        f("SKY_MIE_SCATTERING", SKY_MIE_SCATTERING),
        f("SKY_MIE_EXTINCTION", SKY_MIE_EXTINCTION),
        f("SKY_MIE_SCALE_HEIGHT", SKY_MIE_SCALE_HEIGHT),
        f("SKY_OZONE_PEAK", SKY_OZONE_PEAK),
        f("SKY_OZONE_HALF_WIDTH", SKY_OZONE_HALF_WIDTH),
        Constant::new(
            "sky_transmittance",
            "SKY_TRANSMITTANCE_SAMPLES",
            naga::Literal::U32(SKY_TRANSMITTANCE_SAMPLES),
        ),
        Constant::new(
            "sky_transmittance",
            "AERIAL_SIDE",
            naga::Literal::U32(AERIAL_SIDE),
        ),
        Constant::new(
            "sky_transmittance",
            "AERIAL_SLICES",
            naga::Literal::U32(AERIAL_SLICES),
        ),
        f("AERIAL_REACH", AERIAL_REACH),
    ]
}

/// The vector constants this module shares with sky.wgsl.
#[cfg(test)]
pub(crate) fn vector_constants() -> Vec<(&'static str, &'static str, [f32; 3])> {
    vec![
        (
            "sky_transmittance",
            "SKY_RAYLEIGH_SCATTERING",
            SKY_RAYLEIGH_SCATTERING,
        ),
        (
            "sky_transmittance",
            "SKY_OZONE_ABSORPTION",
            SKY_OZONE_ABSORPTION,
        ),
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
