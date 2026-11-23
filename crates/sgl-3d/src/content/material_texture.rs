//! A material's map: its image, how it is sampled and where, glTF's
//! textureInfo (glTF 2.0 3.8: a texture is an image and a sampler; 3.9.2: a
//! textureInfo names the texture and its texture coordinate set).

/// A map of an [`asset::Material`](crate::asset::Material): the image it
/// samples, the sampler state it samples it with, and the texture
/// coordinates it samples it at, as a glTF `textureInfo` names its texture
/// and `texCoord` and `KHR_texture_transform` its transform. All four belong
/// to the map, not the image: two maps may sample one image with different
/// samplers, sets or transforms, and the image is uploaded once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialTexture {
    /// Index into the images the material is added with
    /// ([`Asset::images`](crate::asset::Asset::images)).
    pub image: usize,
    pub sampler: Sampler,
    /// Which of its mesh's UV sets the map samples (glTF
    /// `textureInfo.texCoord`, or `KHR_texture_transform`'s `texCoord`
    /// where it gives one).
    pub uv_set: UvSet,
    /// How that set's coordinates are moved before the map samples them
    /// (`KHR_texture_transform`).
    pub transform: UvTransform,
}

impl MaterialTexture {
    /// `image` sampled with glTF's default sampler ([`Sampler::default`])
    /// at the first UV set, untransformed.
    pub fn new(image: usize) -> Self {
        Self {
            image,
            sampler: Sampler::default(),
            uv_set: UvSet::default(),
            transform: UvTransform::default(),
        }
    }
}

/// One of a mesh's two UV sets, which a map samples
/// ([`MaterialTexture::uv_set`]). SGL3D keeps two, glTF's floor for a
/// client (glTF 2.0 3.7.2.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UvSet {
    /// [`Vertex::uv`](crate::asset::Vertex::uv), glTF's `TEXCOORD_0`.
    #[default]
    Uv0,
    /// [`Vertex::uv1`](crate::asset::Vertex::uv1), glTF's `TEXCOORD_1`. A
    /// mesh without one reads zero. It is not the lightmap chart, which
    /// [`Vertex::lightmap_uv`](crate::asset::Vertex::lightmap_uv) alone
    /// selects.
    Uv1,
}

/// How a map's texture coordinates are moved before it samples them:
/// `KHR_texture_transform`'s offset, rotation and scale, applied as the
/// extension orders them, scale, then rotation, then offset. The origin is
/// the image's upper left corner, U to the right and V down, so a positive
/// rotation turns the coordinates counter-clockwise about it, the image
/// clockwise on the surface. Each value is finite, or the material is
/// refused. Fixed when the material is added, as its maps are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvTransform {
    /// Added last, in the image's widths and heights (`offset`).
    pub offset: [f32; 2],
    /// Radians (`rotation`).
    pub rotation: f32,
    /// Applied first, to U and to V (`scale`): 2 repeats the image twice,
    /// a negative one mirrors it.
    pub scale: [f32; 2],
}

impl UvTransform {
    /// No transform: the map samples its UV set as the mesh holds it.
    pub const IDENTITY: Self = Self {
        offset: [0.; 2],
        rotation: 0.,
        scale: [1.; 2],
    };
}

impl Default for UvTransform {
    /// [`Self::IDENTITY`].
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// How a map is filtered and wrapped: glTF's sampler. Every state glTF
/// allows is honoured on both binding tiers. Anisotropic filtering
/// (`Settings::anisotropic_filtering`) applies only to a map whose three
/// filters are all `Linear`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sampler {
    /// The filter where a texel covers more than a pixel (glTF
    /// `magFilter`).
    pub mag_filter: TextureFilter,
    /// The filter within a mip level where a texel covers less than a pixel
    /// (glTF `minFilter`'s first word).
    pub min_filter: TextureFilter,
    /// How the level is chosen under minification: `None` samples level 0
    /// alone (glTF `NEAREST` and `LINEAR` minification), `Some(Nearest)`
    /// the nearest level, `Some(Linear)` a blend of the two nearest (glTF
    /// `minFilter`'s `MIPMAP_` word).
    pub mipmap_filter: Option<TextureFilter>,
    /// U then V (glTF `wrapS`, `wrapT`).
    pub wrap: [TextureWrap; 2],
}

impl Default for Sampler {
    /// glTF's sampler when a texture names none: repeating, and trilinear
    /// filtering, which glTF leaves to the implementation.
    fn default() -> Self {
        Self {
            mag_filter: TextureFilter::Linear,
            min_filter: TextureFilter::Linear,
            mipmap_filter: Some(TextureFilter::Linear),
            wrap: [TextureWrap::Repeat; 2],
        }
    }
}

/// A texture filter: the nearest texel, or the four nearest blended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureFilter {
    Nearest,
    Linear,
}

/// What a map samples outside 0..1 along an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureWrap {
    /// The image repeats (glTF `REPEAT`).
    Repeat,
    /// The image repeats, every other copy mirrored (glTF
    /// `MIRRORED_REPEAT`).
    MirroredRepeat,
    /// The edge texels extend (glTF `CLAMP_TO_EDGE`).
    ClampToEdge,
}
