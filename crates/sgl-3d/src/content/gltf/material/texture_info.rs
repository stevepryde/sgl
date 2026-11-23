//! A glTF textureInfo read as a material's map: its texture's image and
//! sampler, the UV set it names and its `KHR_texture_transform`.
use gltf::texture::{MagFilter, MinFilter, WrappingMode};
use serde_json::Value;

use super::super::super::asset::{
    MaterialTexture, Result, Sampler, TextureFilter, TextureWrap, UvSet, UvTransform,
};
use super::GLTF_PROPERTY_KEYS;

/// The extension's name, and its object's key in a textureInfo's
/// `extensions`.
pub(super) const TRANSFORM: &str = "KHR_texture_transform";

/// The texture coordinate set a textureInfo samples: its `texCoord`,
/// `tex_coord`, or the `texCoord` of its `KHR_texture_transform` object
/// `transform`, which "overrides the textureInfo texCoord value if
/// supplied, and if this extension is supported" (KhronosGroup/glTF
/// 08e10b3, extensions/2.0/Khronos/KHR_texture_transform/README.md).
/// `error` words a refusal.
pub(super) fn tex_coord(
    tex_coord: u64,
    transform: Option<&Value>,
    error: &dyn Fn(&str) -> String,
) -> Result<u64> {
    match transform.and_then(|transform| transform.get("texCoord")) {
        None => Ok(tex_coord),
        Some(set) => set.as_u64().ok_or_else(|| {
            error(&format!(
                "{TRANSFORM} texCoord must be a nonnegative integer"
            ))
            .into()
        }),
    }
}

/// The extension object `transform`'s offset, rotation and scale, each
/// finite, with the extension's defaults for those it leaves out: no
/// offset, no rotation and a scale of 1.
fn uv_transform(transform: &Value, error: &dyn Fn(&str) -> String) -> Result<UvTransform> {
    let object = transform
        .as_object()
        .ok_or_else(|| error(&format!("{TRANSFORM} must be an object")))?;
    for key in object.keys() {
        if !matches!(key.as_str(), "offset" | "rotation" | "scale" | "texCoord")
            && !GLTF_PROPERTY_KEYS.contains(&key.as_str())
        {
            return Err(error(&format!("{TRANSFORM} has unsupported property {key}")).into());
        }
    }
    let number = |value: &Value| value.as_f64().map(|v| v as f32).filter(|v| v.is_finite());
    let pair = |key: &str, default: [f32; 2]| -> Result<[f32; 2]> {
        let Some(value) = object.get(key) else {
            return Ok(default);
        };
        value
            .as_array()
            .filter(|values| values.len() == 2)
            .and_then(|values| Some([number(&values[0])?, number(&values[1])?]))
            .ok_or_else(|| error(&format!("{TRANSFORM} {key} must be two finite numbers")).into())
    };
    let identity = UvTransform::IDENTITY;
    Ok(UvTransform {
        offset: pair("offset", identity.offset)?,
        rotation: match object.get("rotation") {
            None => identity.rotation,
            Some(value) => number(value)
                .ok_or_else(|| error(&format!("{TRANSFORM} rotation must be a finite number")))?,
        },
        scale: pair("scale", identity.scale)?,
    })
}

/// `texture` as the map of a textureInfo whose `texCoord` is `set` and
/// whose `KHR_texture_transform` object, where it has one, is `transform`:
/// its image and sampler, the UV set it samples (`tex_coord`) and its
/// transform. SGL3D keeps two UV sets, so a map on `TEXCOORD_2` or above is
/// refused. `error` words a refusal.
pub(super) fn map(
    texture: gltf::Texture<'_>,
    set: u64,
    transform: Option<&Value>,
    error: &dyn Fn(&str) -> String,
) -> Result<MaterialTexture> {
    let uv_set = match tex_coord(set, transform, error)? {
        0 => UvSet::Uv0,
        1 => UvSet::Uv1,
        set => {
            return Err(error(&format!(
                "samples TEXCOORD_{set}; SGL3D keeps TEXCOORD_0 and TEXCOORD_1, export it on one of them"
            ))
            .into());
        }
    };
    Ok(MaterialTexture {
        uv_set,
        transform: transform
            .map(|transform| uv_transform(transform, error))
            .transpose()?
            .unwrap_or_default(),
        ..slot(texture)
    })
}

/// Extension property `key` of `value`, a glTF textureInfo, as a map
/// (`map`). `error` words a refusal.
pub(super) fn extension_texture(
    value: &Value,
    key: &str,
    document: &gltf::Document,
    error: &dyn Fn(&str) -> String,
) -> Result<Option<MaterialTexture>> {
    let Some(info) = value.get(key) else {
        return Ok(None);
    };
    let info = info
        .as_object()
        .ok_or_else(|| error(&format!("{key} must be an object")))?;
    let set = match info.get("texCoord") {
        None => 0,
        Some(set) => set
            .as_u64()
            .ok_or_else(|| error(&format!("{key} texCoord must be a nonnegative integer")))?,
    };
    let index = info
        .get("index")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| error(&format!("{key} index must be a nonnegative integer")))?;
    let texture = document
        .textures()
        .nth(index)
        .ok_or_else(|| error(&format!("{key} index out of range")))?;
    let transform = info
        .get("extensions")
        .and_then(|extensions| extensions.get(TRANSFORM));
    map(texture, set, transform, &|message| {
        error(&format!("{key} {message}"))
    })
    .map(Some)
}

/// `texture`'s image and sampler on the first UV set untransformed, as glTF 2.0 defines a sampler's absent
/// properties (3.8.4, `sampler.schema.json`): repeating on both axes, and
/// filtering the implementation chooses, trilinear for SGL3D. three.js r187
/// reads them alike (examples/jsm/loaders/GLTFLoader.js 3376–3383).
fn slot(texture: gltf::Texture<'_>) -> MaterialTexture {
    let sampler = texture.sampler();
    let wrap = |mode| match mode {
        WrappingMode::Repeat => TextureWrap::Repeat,
        WrappingMode::MirroredRepeat => TextureWrap::MirroredRepeat,
        WrappingMode::ClampToEdge => TextureWrap::ClampToEdge,
    };
    let (min_filter, mipmap_filter) = match sampler.min_filter() {
        Some(MinFilter::Nearest) => (TextureFilter::Nearest, None),
        Some(MinFilter::Linear) => (TextureFilter::Linear, None),
        Some(MinFilter::NearestMipmapNearest) => {
            (TextureFilter::Nearest, Some(TextureFilter::Nearest))
        }
        Some(MinFilter::LinearMipmapNearest) => {
            (TextureFilter::Linear, Some(TextureFilter::Nearest))
        }
        Some(MinFilter::NearestMipmapLinear) => {
            (TextureFilter::Nearest, Some(TextureFilter::Linear))
        }
        Some(MinFilter::LinearMipmapLinear) | None => {
            (TextureFilter::Linear, Some(TextureFilter::Linear))
        }
    };
    MaterialTexture {
        image: texture.source().index(),
        sampler: Sampler {
            mag_filter: match sampler.mag_filter() {
                Some(MagFilter::Nearest) => TextureFilter::Nearest,
                Some(MagFilter::Linear) | None => TextureFilter::Linear,
            },
            min_filter,
            mipmap_filter,
            wrap: [wrap(sampler.wrap_s()), wrap(sampler.wrap_t())],
        },
        ..MaterialTexture::new(texture.source().index())
    }
}
