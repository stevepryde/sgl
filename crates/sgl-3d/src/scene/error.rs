//! Why a `Scene` operation refused its input. A refused operation changes
//! nothing.
use crate::baked_specular_probe::ProbeError;

/// A `Scene` operation's refusal.
#[derive(Debug)]
pub enum SceneError {
    /// The material was removed, or another scene issued its identity.
    UnknownMaterial,
    /// The model was removed, or another scene issued its identity.
    UnknownModel,
    /// The instance was removed, or another scene issued its identity.
    UnknownInstance,
    /// The light was removed, or another scene issued its identity.
    UnknownLight,
    /// The environment was removed, or another scene issued its identity.
    UnknownEnvironment,
    /// The decal was removed, or another scene issued its identity.
    UnknownDecal,
    /// The decal image was removed, or another scene issued its identity.
    UnknownDecalImage,
    /// A model's mesh uses the material.
    MaterialInUse,
    /// An instance uses the model, or it is a level of detail of a mesh.
    ModelInUse,
    /// A decal uses the decal image.
    DecalImageInUse,
    /// A material names an image index beyond the images added with it.
    MissingImage,
    /// An asset's mesh names a material index beyond the asset's materials.
    MissingMaterial,
    /// The model has no mesh at that index.
    MissingMesh,
    /// An image has no texels.
    EmptyImage,
    /// A compressed image's sides are not multiples of its block size, or it
    /// holds no level, more than a full chain, or a level of the wrong size.
    InvalidCompressedImage,
    /// A compressed image that materials sample as both sRGB colour and
    /// linear data needs `DownlevelFlags::VIEW_FORMATS`, which wgpu's GL
    /// backend lacks.
    CompressedImageViews,
    /// A mesh index names no vertex of its mesh.
    IndexOutOfRange,
    /// A vertex position is not finite.
    NonFiniteGeometry,
    /// A pose is not finite or not invertible.
    InvalidPose,
    /// Anisotropy strength is outside 0..=1 or its rotation is not finite.
    InvalidAnisotropy,
    /// An anisotropic material's mesh lacks authored tangent frames.
    MissingAnisotropyTangents,
    /// A masked material's alpha cutoff is not finite and nonnegative.
    InvalidAlphaCutoff,
    /// A level of detail's error is not finite and nonnegative.
    InvalidLod,
    /// A level of detail names a mesh of the model it details, which draws
    /// all its meshes.
    LodInSameModel,
    /// An ambient cube was given to a static instance, which takes baked
    /// diffuse light from lightmap charts and the irradiance atlas.
    StaticInstance,
    /// A light's position, colour, intensity, range or specular scale is
    /// not finite, or not positive where it must be, its shadow opacity is
    /// outside 0..=1, or a spot's direction is zero or its angles are not
    /// `0 <= inner <= outer < π/2`.
    InvalidLight,
    /// A decal's position, rotation or size is not finite, its size not
    /// positive, or its colour, mix or fades outside their ranges.
    InvalidDecal,
    /// A PMREM atlas's texels do not match its dimensions.
    InvalidEnvironmentMap,
    /// A lightmap's texels do not match its size.
    InvalidLightmap,
    /// Directionality neither is empty nor matches its chart with finite
    /// [0, 1] lobes, or it holds a lobe that rounds to the reserved all-zero
    /// RGBA8 lobe.
    InvalidDirectionality,
    /// An irradiance atlas's layers do not match its size (for a compressed
    /// atlas, nonzero multiples of 4).
    InvalidIrradianceAtlas,
    /// Irradiance outside the finite nonnegative RGBA16F range, or a
    /// compressed atlas's scale that is not finite and nonnegative.
    InvalidIrradiance,
    /// Block-compressed content needs `wgpu::Features::TEXTURE_COMPRESSION_BC`.
    CompressionUnsupported,
    /// Heat geometry is not a triangle list of at most
    /// `heat_distortion::MAX_VERTICES` vertices.
    HeatVertexCount,
    /// A heat vertex's position is not finite, its displacement not within
    /// `heat_distortion::MAX_DISPLACEMENT_PIXELS` or its weight not in 0..=1.
    InvalidHeatVertex,
    /// A fog volume's centre, rotation or size is not finite, its rotation is
    /// zero, its size not positive, or its density, albedo or edge fade
    /// negative or not finite.
    InvalidFogVolume,
    /// A mesh's influences or morph targets do not match its vertices, a
    /// vertex's weights are not finite and nonnegative with a positive sum,
    /// a displacement is not finite, or a joint or morph weight index is
    /// above 65535.
    InvalidDeformation,
    /// The instance's model does not deform, or its joint matrices or morph
    /// weights are fewer than its model takes, or not finite.
    DeformationMismatch,
    /// A deforming model's instances move and keep their model, and it
    /// takes no part in levels of detail.
    DeformingModel,
    /// A render origin (`Scene::move_origin`) is not finite.
    InvalidOrigin,
    /// A dynamic GI volume's origin or spacing is not finite, its spacing
    /// not positive, or it has fewer than two probes along an axis.
    InvalidDynamicGiVolume,
    /// The content would exceed a limit of the device.
    DeviceLimit,
    /// The specular probe collection was refused.
    Probe(ProbeError),
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownMaterial => "the material is not in this scene",
            Self::UnknownModel => "the model is not in this scene",
            Self::UnknownInstance => "the instance is not in this scene",
            Self::UnknownLight => "the light is not in this scene",
            Self::UnknownEnvironment => "the environment is not in this scene",
            Self::UnknownDecal => "the decal is not in this scene",
            Self::UnknownDecalImage => "the decal image is not in this scene",
            Self::MaterialInUse => "a model's mesh uses the material",
            Self::ModelInUse => "an instance uses the model, or it is a level of detail",
            Self::DecalImageInUse => "a decal uses the decal image",
            Self::MissingImage => "a material names an image that was not added with it",
            Self::MissingMaterial => "a mesh names a material the asset does not have",
            Self::MissingMesh => "the model has no mesh at that index",
            Self::EmptyImage => "an image has no texels",
            Self::CompressedImageViews => {
                "a compressed image sampled as both colour and data needs DownlevelFlags::VIEW_FORMATS"
            }
            Self::InvalidCompressedImage => {
                "a compressed image needs sides that are multiples of its block size and one to a full chain of levels, each of its size"
            }
            Self::IndexOutOfRange => "a mesh index names no vertex",
            Self::NonFiniteGeometry => "a vertex position is not finite",
            Self::InvalidPose => "a pose must be finite and invertible",
            Self::InvalidAnisotropy => {
                "anisotropy strength must be finite in 0..1 and rotation finite"
            }
            Self::MissingAnisotropyTangents => {
                "anisotropy requires authored nonzero tangent frames on every mesh using this material"
            }
            Self::InvalidAlphaCutoff => "an alpha cutoff must be finite and nonnegative",
            Self::InvalidLod => "LOD error must be finite and nonnegative",
            Self::LodInSameModel => "a level of detail must be a mesh of another model",
            Self::StaticInstance => {
                "a static instance takes baked light from charts, not an ambient cube"
            }
            Self::InvalidLight => {
                "a light needs a finite position, nonnegative colour, intensity and specular scale, a shadow opacity in 0..=1, a positive range, and a spot a nonzero direction with 0 <= inner <= outer < pi/2"
            }
            Self::InvalidDecal => {
                "a decal needs a finite pose, a positive size, and colour, mix and fades in range"
            }
            Self::InvalidEnvironmentMap => {
                "PMREM atlas dimensions do not match its RGBA16F data"
            }
            Self::InvalidLightmap => "static lightmap dimensions do not match its texels",
            Self::InvalidDirectionality => {
                "directionality must be empty or match the chart with finite [0,1] lobes; a lobe that rounds to all-zero RGBA8 is reserved"
            }
            Self::InvalidIrradianceAtlas => {
                "irradiance atlas layers do not match its size; compressed sizes are nonzero multiples of 4"
            }
            Self::InvalidIrradiance => {
                "irradiance and its scale must be finite, nonnegative and within RGBA16F range"
            }
            Self::CompressionUnsupported => {
                "compressed content needs wgpu::Features::TEXTURE_COMPRESSION_BC"
            }
            Self::HeatVertexCount => {
                "heat geometry must be a triangle list of at most 6144 vertices"
            }
            Self::InvalidHeatVertex => {
                "heat vertices require finite positions, displacement within +/-32 pixels and weight in 0..=1"
            }
            Self::InvalidFogVolume => {
                "fog volumes require a finite centre and rotation, a positive size and finite nonnegative density, albedo and edge fade"
            }
            Self::InvalidDeformation => {
                "influences and morph targets must match their mesh's vertices, with finite nonnegative weights of positive sum, finite displacements and indices of at most 65535"
            }
            Self::DeformationMismatch => {
                "a deformation needs a deforming instance and at least as many finite joint matrices and morph weights as its model takes"
            }
            Self::DeformingModel => {
                "a deforming model's instances move and keep their model, and it takes no part in levels of detail"
            }
            Self::InvalidOrigin => "a render origin must be finite",
            Self::InvalidDynamicGiVolume => {
                "a dynamic GI volume needs a finite origin, a finite positive spacing and at least two probes along each axis"
            }
            Self::DeviceLimit => "the content exceeds a limit of the device",
            Self::Probe(error) => return error.fmt(f),
        })
    }
}

impl std::error::Error for SceneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Probe(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ProbeError> for SceneError {
    fn from(error: ProbeError) -> Self {
        Self::Probe(error)
    }
}
