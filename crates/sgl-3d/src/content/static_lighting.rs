//! Caller-baked, camera-independent diffuse light on static instances' UV
//! charts.
//! Values are irradiance / PI, separate from albedo and visible emission.
/// Camera-independent linear irradiance / PI for both sides of fixed geometry.
/// Each layer is row-major; UV (0,0) must address reserved black padding.
#[derive(Clone, Debug)]
pub struct IrradianceAtlas {
    pub size: [u32; 2],
    pub irradiance: Vec<[f32; 3]>,
    pub back_irradiance: Vec<[f32; 3]>,
    /// Optional row-major world-space irradiance lobe, encoded as for
    /// `Lightmap::directionality`; empty retains scalar irradiance.
    pub directionality: Vec<[f32; 4]>,
    pub back_directionality: Vec<[f32; 4]>,
}

/// A block-compressed [`IrradianceAtlas`], as engines ship lightmaps (Unity
/// and Godot store HDR lightmaps as BC6H): irradiance takes an eighth of its
/// RGBA16F size and directionality a quarter of its RGBA8 size, on disk and on
/// the GPU. Each field holds the front then the back layer as row-major 4x4
/// blocks of 16 bytes; both sizes are multiples of 4. Installing it needs
/// `wgpu::Features::TEXTURE_COMPRESSION_BC`.
#[derive(Clone, Debug)]
pub struct CompressedIrradianceAtlas {
    pub size: [u32; 2],
    /// BC6H unsigned-float irradiance / PI.
    pub irradiance: Vec<u8>,
    /// BC7 directionality, encoded as `IrradianceAtlas::directionality`;
    /// empty retains scalar irradiance.
    pub directionality: Vec<u8>,
    /// Multiplies the decoded irradiance in the shader, as Unreal's lightmap
    /// scale does, so a uniform source-power change needs no re-encode.
    pub scale: f32,
}

/// Baked diffuse irradiance / PI along +X,-X,+Y,-Y,+Z,-Z in world axes.
/// Squared-normal interpolation is a six-lobe approximation, not specular radiance.
#[derive(Clone, Copy, Debug, Default)]
pub struct AmbientCube {
    pub irradiance: [[f32; 3]; 6],
}
impl AmbientCube {
    pub(crate) fn packed(self) -> [[f32; 4]; 6] {
        self.irradiance.map(|v| [v[0], v[1], v[2], 0.])
    }
}

/// A static surface chart authored by the caller from its actual geometry.
/// It is installed as an uncompressed [`IrradianceAtlas`] layer is: RGBA16F
/// irradiance and RGBA8Unorm directionality.
#[derive(Clone, Debug)]
pub struct Lightmap {
    pub size: [u32; 2],
    /// `normalized_chart_uv = material_uv * xy + zw`. X clamps and Y repeats.
    pub uv_scale_offset: [f32; 4],
    /// Row-major linear RGB irradiance / PI, including source radiance;
    /// finite, nonnegative and within RGBA16F range.
    pub irradiance: Vec<[f32; 3]>,
    /// Optional row-major world-space irradiance lobe: the shader scales the
    /// texel by max(a + dot(w, n), 0) with xyz = w / 8 + .5 and w = a / 2.
    /// Empty preserves nondirectional light. See the package README.
    pub directionality: Vec<[f32; 4]>,
}

/// Whether `values` is empty or holds `count` finite [0, 1] lobes, none of
/// which encodes to the reserved all-zero RGBA8 lobe.
pub(crate) fn valid_directionality(values: &[[f32; 4]], count: usize) -> bool {
    (values.is_empty() || values.len() == count)
        && values
            .iter()
            .flatten()
            .all(|v| v.is_finite() && (0. ..=1.).contains(v))
        && values.iter().all(|&lobe| lobe_rgba8(lobe) != [0; 4])
}

/// A [0, 1] lobe as the RGBA8Unorm texel the GPU stores.
pub(crate) fn lobe_rgba8(lobe: [f32; 4]) -> [u8; 4] {
    lobe.map(|v| (v * 255.).round() as u8)
}

// Positive finite binary32 to binary16, round-to-nearest-even. Input is bounded
// by the atlas upload contract, including subnormal/underflow irradiance.
pub(crate) fn irradiance_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 255) as i32 - 127;
    if exponent < -25 {
        return 0;
    }
    let significand = (bits & 0x7fffff) | 0x800000;
    let shift = if exponent < -14 {
        (-exponent - 1) as u32
    } else {
        13
    };
    let retained = significand >> shift;
    let remainder = significand & ((1u32 << shift) - 1);
    let halfway = 1u32 << (shift - 1);
    let rounded =
        retained + u32::from(remainder > halfway || (remainder == halfway && retained & 1 != 0));
    if exponent < -14 {
        rounded as u16
    } else {
        (((exponent + 14) as u32) * 1024 + rounded) as u16
    }
}
