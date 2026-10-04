//! Caller-authored static specular probes: local image-based lighting with
//! parallax-corrected cubemaps (Lagarde and Zanuttini, SIGGRAPH 2012), as
//! HDRP, Unreal, Godot, Wicked and Bevy use under screen-space reflections.
//! Each probe is a prefiltered cube with an influence box, where receivers use
//! it, and an optional proxy box, which corrects parallax. Capture is a
//! blocking authoring operation; installing a collection only uploads it.
use glam::{Mat4, Vec3};

/// Probes in one installed collection: Wicked Engine's per-frame entity array
/// (SHADER_ENTITY_COUNT in ShaderInterop_Renderer.h, commit 4323a33), which
/// its tiled culling walks as eight 32-entity buckets. Wicked, Godot and Bevy
/// fill that array each frame from the probes a CPU frustum pass finds
/// visible; SGL3D installs the whole collection as the array instead, and the
/// per-tile frustum and depth test (probe_culling.wgsl) does that culling.
/// Every probe stays resident in one cube array, as Unreal's reflection
/// captures do, so the device's array layers bound it too.
pub(crate) const MAX_PROBES: usize = 256;

/// Mip levels of a prefiltered probe: perceptual roughness 0, 1/6, ..., 1.
pub(crate) const LEVELS: u32 = 7;

/// An axis-aligned box in a probe's local frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpecularProbeBox {
    pub min: Vec3,
    pub max: Vec3,
}

/// A GGX-prefiltered cube: perceptual roughness level/6 at mip `level`, seven
/// levels from `face_size` down. `texels` run level-major, then the six faces
/// in WebGPU cube order, then rows. Faces follow the capture views' Z-mirrored
/// convention.
#[derive(Clone, Debug)]
pub struct SpecularProbeRadiance {
    pub face_size: u32,
    pub texels: SpecularProbeTexels,
}

/// A probe's stored radiance.
#[derive(Clone, Debug)]
pub enum SpecularProbeTexels {
    /// RGBA texels, each u16 an IEEE-754 binary16 value, as captures return.
    Rgba16Float(Vec<u16>),
    /// BC6H unsigned-float blocks, 16 bytes per 4x4 texels (a level smaller
    /// than a block takes one), as engines ship probes: an eighth of the size
    /// on disk and in memory. Installing them needs
    /// `wgpu::Features::TEXTURE_COMPRESSION_BC`.
    Bc6hUfloat(Vec<u8>),
}

impl SpecularProbeTexels {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Rgba16Float(texels) => bytemuck::cast_slice(texels),
            Self::Bc6hUfloat(blocks) => blocks,
        }
    }

    /// Its texel values: unsigned BC6H decodes only finite, nonnegative
    /// binary16 values.
    pub(crate) fn validate_radiance(&self) -> Result<(), ProbeError> {
        if let Self::Rgba16Float(values) = self {
            for (component, &value) in values.iter().enumerate() {
                // Reject infinities/NaNs and negative values, accepting signed zero.
                if value & 0x7c00 == 0x7c00 || (value & 0x8000 != 0 && value & 0x7fff != 0) {
                    return Err(ProbeError::InvalidRadiance { component });
                }
            }
        }
        Ok(())
    }
}

/// One probe of a collection.
#[derive(Clone, Debug)]
pub struct BakedSpecularProbe {
    /// Capture position.
    pub center: Vec3,
    /// Rigid world-to-local transform of the influence and proxy boxes.
    pub world_to_local: Mat4,
    /// Receivers inside use this probe. Its weight is one `blend` (per local
    /// axis) inside each face, falling linearly to zero at the face; zero on
    /// an axis is a hard edge. Overlapping probes are normalised by their
    /// total weight, and where the total is below one the sky fills the rest.
    pub influence: SpecularProbeBox,
    pub blend: Vec3,
    /// The geometry the capture sees, for box-projected parallax. It may
    /// extend well beyond the influence (a corridor's whole length). `None`
    /// treats what the probe sees as distant, as for open outdoor spaces.
    pub proxy: Option<SpecularProbeBox>,
    pub radiance: SpecularProbeRadiance,
}

/// Invalid caller data or failure of the explicit GPU authoring operation.
#[derive(Debug)]
pub enum ProbeError {
    InvalidProbe(&'static str),
    InvalidDimensions,
    InvalidPayloadLength { expected: usize, actual: usize },
    InvalidRadiance { component: usize },
    MixedRadiance,
    CompressionUnsupported,
    TooManyProbes,
    Readback(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProbe(reason) => write!(f, "invalid specular probe: {reason}"),
            Self::InvalidDimensions => write!(
                f,
                "specular probe faces must be a power of two from 64 within device limits"
            ),
            Self::InvalidPayloadLength { expected, actual } => write!(
                f,
                "specular probe radiance requires {expected} bytes, received {actual}"
            ),
            Self::InvalidRadiance { component } => write!(
                f,
                "specular probe component {component} is negative or non-finite"
            ),
            Self::MixedRadiance => write!(
                f,
                "a collection's probes share one face size and texel encoding"
            ),
            Self::CompressionUnsupported => write!(
                f,
                "BC6H specular probes need wgpu::Features::TEXTURE_COMPRESSION_BC"
            ),
            Self::TooManyProbes => write!(
                f,
                "a collection holds at most {MAX_PROBES} probes within the device's array layers"
            ),
            Self::Readback(reason) => {
                write!(f, "static specular capture readback failed: {reason}")
            }
        }
    }
}
impl std::error::Error for ProbeError {}

fn validate_box(b: SpecularProbeBox, what: &'static str) -> Result<(), ProbeError> {
    if !b.min.is_finite() || !b.max.is_finite() || !b.min.cmplt(b.max).all() {
        return Err(ProbeError::InvalidProbe(what));
    }
    Ok(())
}

impl BakedSpecularProbe {
    /// Its position, transform, influence, blend and proxy.
    pub(crate) fn validate_placement(&self) -> Result<(), ProbeError> {
        let m = self.world_to_local;
        if !self.center.is_finite() || !m.is_finite() {
            return Err(ProbeError::InvalidProbe("coordinates must be finite"));
        }
        let axes = [
            m.x_axis.truncate(),
            m.y_axis.truncate(),
            m.z_axis.truncate(),
        ];
        let affine = Vec3::new(m.x_axis.w, m.y_axis.w, m.z_axis.w);
        if affine.abs().max_element() > 1e-5
            || (m.w_axis.w - 1.).abs() > 1e-5
            || axes
                .iter()
                .any(|axis| (axis.length_squared() - 1.).abs() > 1e-4)
            || axes[0].dot(axes[1]).abs() > 1e-4
            || axes[0].dot(axes[2]).abs() > 1e-4
            || axes[1].dot(axes[2]).abs() > 1e-4
            || (m.determinant() - 1.).abs() > 1e-4
        {
            return Err(ProbeError::InvalidProbe(
                "transform must be a rigid rotation and translation",
            ));
        }
        validate_box(self.influence, "influence must have positive extent")?;
        if !self.blend.is_finite() || self.blend.min_element() < 0. {
            return Err(ProbeError::InvalidProbe(
                "blend distances must be finite and nonnegative",
            ));
        }
        if let Some(proxy) = self.proxy {
            validate_box(proxy, "proxy must have positive extent")?;
            let local = m.transform_point3(self.center);
            if !local.cmpge(proxy.min).all() || !local.cmple(proxy.max).all() {
                return Err(ProbeError::InvalidProbe(
                    "capture center must lie inside the proxy",
                ));
            }
        }
        Ok(())
    }
}
