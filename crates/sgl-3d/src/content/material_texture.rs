//! A material's map: its image and how it is sampled, glTF's texture
//! (glTF 2.0 3.8: a texture is an image and a sampler).

/// A map of an [`asset::Material`](crate::asset::Material): the image it
/// samples and the sampler state it samples it with, as a glTF
/// `textureInfo`'s texture names its source and sampler. Two maps may
/// sample one image with different samplers; the image is uploaded once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialTexture {
    /// Index into the images the material is added with
    /// ([`Asset::images`](crate::asset::Asset::images)).
    pub image: usize,
    pub sampler: Sampler,
}

impl MaterialTexture {
    /// `image` sampled with glTF's default sampler ([`Sampler::default`]).
    pub fn new(image: usize) -> Self {
        Self {
            image,
            sampler: Sampler::default(),
        }
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
