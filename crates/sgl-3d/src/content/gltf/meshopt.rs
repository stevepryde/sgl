//! `EXT_meshopt_compression` and `KHR_meshopt_compression`: compressed
//! buffer views, decoded once at import into their parent view's bytes,
//! below every accessor reader, so meshes, skins, animation channels,
//! sparse accessors and images read decoded bytes through the same
//! readers as an uncompressed file. A file that uses neither extension
//! takes no step here.
//!
//! The extensions' rules (KhronosGroup/glTF 5b00b91,
//! `extensions/2.0/Vendor/EXT_meshopt_compression/README.md` and
//! `extensions/2.0/Khronos/KHR_meshopt_compression/README.md`) are checked
//! before any decoding: KHR adds version 1 of the attribute stream and the
//! COLOR filter; EXT requires a parent `byteStride`, when present, to match.
//! As three.js r187's `GLTFLoader` (`GLTFMeshoptCompression`) does, a
//! buffer only compressed views use, such as a fallback buffer, is never
//! read: its bytes come from decoding alone.
use serde_json::Value;

use super::super::asset::Result;
use super::super::load_budget::Budget;
use super::super::meshopt::{filter, index, vertex};

/// Which extension a view uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Extension {
    Ext,
    Khr,
}

impl Extension {
    const ALL: [Self; 2] = [Self::Ext, Self::Khr];

    fn name(self) -> &'static str {
        match self {
            Self::Ext => "EXT_meshopt_compression",
            Self::Khr => "KHR_meshopt_compression",
        }
    }

    /// The newest attribute stream version it defines.
    fn attribute_version(self) -> u8 {
        match self {
            Self::Ext => 0,
            Self::Khr => 1,
        }
    }
}

/// A buffer view's extension object, as both specifications define it.
struct Object {
    buffer: u64,
    byte_offset: u64,
    byte_length: u64,
    byte_stride: u64,
    count: u64,
    mode: Mode,
    filter: Filter,
}

impl Object {
    /// Read from the JSON by hand rather than by a serde derive, which
    /// adds more to a browser build than these few fields need.
    fn parse(value: &Value) -> std::result::Result<Self, String> {
        if !value.is_object() {
            return Err("its extension object is not an object".into());
        }
        let number = |name: &str, default: Option<u64>| match value.get(name) {
            None => default.ok_or_else(|| format!("{name} is missing")),
            Some(number) => number
                .as_u64()
                .ok_or_else(|| format!("{name} is not a nonnegative integer")),
        };
        let word = |name: &str| match value.get(name) {
            None => Ok(None),
            Some(word) => word
                .as_str()
                .map(Some)
                .ok_or_else(|| format!("{name} is not a string")),
        };
        let mode = match word("mode")? {
            Some("ATTRIBUTES") => Mode::Attributes,
            Some("TRIANGLES") => Mode::Triangles,
            Some("INDICES") => Mode::Indices,
            Some(other) => return Err(format!("mode {other:?} is not one the extension defines")),
            None => return Err("mode is missing".into()),
        };
        let filter = match word("filter")? {
            None | Some("NONE") => Filter::None,
            Some("OCTAHEDRAL") => Filter::Octahedral,
            Some("QUATERNION") => Filter::Quaternion,
            Some("EXPONENTIAL") => Filter::Exponential,
            Some("COLOR") => Filter::Color,
            Some(other) => {
                return Err(format!("filter {other:?} is not one the extension defines"));
            }
        };
        Ok(Self {
            buffer: number("buffer", None)?,
            byte_offset: number("byteOffset", Some(0))?,
            byte_length: number("byteLength", None)?,
            byte_stride: number("byteStride", None)?,
            count: number("count", None)?,
            mode,
            filter,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Attributes,
    Triangles,
    Indices,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    None,
    Octahedral,
    Quaternion,
    Exponential,
    Color,
}

/// A compressed view, checked against its extension's rules.
struct View {
    index: usize,
    extension: Extension,
    /// The parent view's buffer, offset and length: where it decodes to.
    target: usize,
    offset: usize,
    end: usize,
    /// The compressed bytes' buffer and byte range.
    source: usize,
    source_offset: usize,
    source_end: usize,
    stride: usize,
    count: usize,
    mode: Mode,
    filter: Filter,
}

/// A file's compressed views, and the buffers decoding alone fills.
pub(super) struct Compressed {
    views: Vec<View>,
    unread: Vec<bool>,
}

impl Compressed {
    /// The compressed views of `document`, checked, or `None` when its
    /// `extensionsUsed` names neither extension.
    pub(super) fn find(document: &gltf::Document) -> Result<Option<Self>> {
        let used: Vec<Extension> = Extension::ALL
            .into_iter()
            .filter(|extension| {
                document
                    .extensions_used()
                    .any(|used| used == extension.name())
            })
            .collect();
        if used.is_empty() {
            return Ok(None);
        }
        let root = document.as_json();
        let mut views = Vec::new();
        for (index, view) in root.buffer_views.iter().enumerate() {
            let mut objects = used.iter().filter_map(|&extension| {
                let value = view.extensions.as_ref()?.others.get(extension.name())?;
                Some((extension, value))
            });
            let Some((extension, value)) = objects.next() else {
                continue;
            };
            if objects.next().is_some() {
                return Err(format!("buffer view {index} uses both EXT_meshopt_compression and KHR_meshopt_compression, which the extensions forbid; re-export valid glTF").into());
            }
            let parent = (
                view.buffer.value(),
                view.byte_offset.map_or(0, |offset| offset.0),
                view.byte_length.0,
                view.byte_stride.map(|stride| stride.0 as u64),
            );
            let view = check(root, index, extension, value, parent).map_err(|error| {
                format!(
                    "buffer view {index}: {}: {error}; re-export valid glTF",
                    extension.name()
                )
            })?;
            views.push(view);
        }
        let buffers = root.buffers.len();
        let mut compressed = vec![false; buffers];
        let mut uncompressed = vec![false; buffers];
        let mut source = vec![false; buffers];
        for view in &views {
            compressed[view.target] = true;
            source[view.source] = true;
        }
        for (index, view) in root.buffer_views.iter().enumerate() {
            if views
                .binary_search_by_key(&index, |view| view.index)
                .is_err()
            {
                uncompressed[view.buffer.value()] = true;
            }
        }
        // A buffer decoding alone fills: one tagged as a fallback, which
        // only compressed views may reference and no compressed view may
        // take bytes from, or one that compressed views are the only users
        // of.
        let mut unread = Vec::with_capacity(buffers);
        for (index, buffer) in root.buffers.iter().enumerate() {
            let fallback = used.iter().any(|extension| {
                buffer
                    .extensions
                    .as_ref()
                    .and_then(|extensions| extensions.others.get(extension.name()))
                    .and_then(|tag| tag.get("fallback"))
                    .and_then(Value::as_bool)
                    == Some(true)
            });
            let used_otherwise = uncompressed[index] || source[index];
            if fallback && used_otherwise {
                return Err(format!("buffer {index} is a meshopt fallback buffer, which only compressed buffer views may reference and none may take compressed bytes from; re-export valid glTF").into());
            }
            unread.push(fallback || (compressed[index] && !used_otherwise));
        }
        Ok(Some(Self { views, unread }))
    }

    /// Whether buffer `index` is filled by decoding alone, and so never
    /// read.
    pub(super) fn unread(&self, index: usize) -> bool {
        self.unread.get(index).copied().unwrap_or(false)
    }

    /// Decodes every compressed view into its parent view's bytes in
    /// `buffers`. An unread buffer is allocated to the end of its furthest
    /// view, never its declared length, after that length is charged to
    /// the decoded `budget`. A view whose buffer also holds compressed
    /// bytes decodes beside it, charged too, and is copied into place once
    /// every view has decoded, so no view reads another's output.
    pub(super) fn decode(
        &self,
        buffers: &mut [gltf::buffer::Data],
        budget: &mut Budget,
    ) -> Result<()> {
        let mut ends = vec![0usize; buffers.len()];
        let mut sources = vec![false; buffers.len()];
        for view in &self.views {
            ends[view.target] = ends[view.target].max(view.end);
            sources[view.source] = true;
        }
        for (index, buffer) in buffers.iter_mut().enumerate() {
            if self.unread[index] {
                budget.charge(ends[index] as u64).map_err(|error| {
                    format!("buffer {index}, decoded from meshopt-compressed views: {error}")
                })?;
                buffer.0 = vec![0; ends[index]];
            }
        }
        let mut deferred = Vec::new();
        for view in &self.views {
            let (source, target) = (view.source_offset..view.source_end, view.offset..view.end);
            if sources[view.target] {
                budget.charge(target.len() as u64).map_err(|error| {
                    format!(
                        "buffer view {}, decoded from {}: {error}",
                        view.index,
                        view.extension.name()
                    )
                })?;
                let mut decoded = vec![0; target.len()];
                let source = buffers[view.source]
                    .0
                    .get(source)
                    .ok_or_else(|| outside(view))?;
                decode_view(view, source, &mut decoded)?;
                deferred.push((view, target, decoded));
            } else {
                // No view takes compressed bytes from this buffer, so it
                // is not the source: decode straight into it.
                let mut bytes = std::mem::take(&mut buffers[view.target].0);
                let decoded = (|| {
                    let source = buffers[view.source]
                        .0
                        .get(source)
                        .ok_or_else(|| outside(view))?;
                    let out = bytes.get_mut(target).ok_or_else(|| outside(view))?;
                    decode_view(view, source, out)
                })();
                buffers[view.target].0 = bytes;
                decoded?;
            }
        }
        for (view, target, decoded) in deferred {
            buffers[view.target]
                .0
                .get_mut(target)
                .ok_or_else(|| outside(view))?
                .copy_from_slice(&decoded);
        }
        Ok(())
    }
}

fn outside(view: &View) -> String {
    format!(
        "buffer view {}: {} bytes lie outside their buffer; re-export valid glTF",
        view.index,
        view.extension.name()
    )
}

/// Checks buffer view `index`'s extension object `value` against its
/// extension's rules, `parent` being the view's own buffer, offset, length
/// and stride.
fn check(
    root: &gltf::json::Root,
    index: usize,
    extension: Extension,
    value: &Value,
    (target, offset, length, parent_stride): (usize, u64, u64, Option<u64>),
) -> std::result::Result<View, String> {
    let object = Object::parse(value)?;
    let (stride, count) = (object.byte_stride, object.count);
    if stride.checked_mul(count) != Some(length) {
        return Err(format!(
            "byteStride {stride} times count {count} is not the view's byteLength {length}"
        ));
    }
    if extension == Extension::Ext && parent_stride.is_some_and(|parent| parent != stride) {
        return Err(format!("byteStride {stride} differs from the view's"));
    }
    let valid = match object.mode {
        Mode::Attributes => stride.is_multiple_of(4) && (4..=256).contains(&stride),
        Mode::Triangles => count.is_multiple_of(3) && matches!(stride, 2 | 4),
        Mode::Indices => matches!(stride, 2 | 4),
    };
    if !valid {
        return Err(format!(
            "{:?} does not allow byteStride {stride} with count {count}",
            object.mode
        ));
    }
    let filtered = match object.filter {
        Filter::None => true,
        _ if object.mode != Mode::Attributes => false,
        Filter::Octahedral => matches!(stride, 4 | 8),
        Filter::Quaternion => stride == 8,
        Filter::Exponential => stride.is_multiple_of(4),
        Filter::Color => extension == Extension::Khr && matches!(stride, 4 | 8),
    };
    if !filtered {
        return Err(format!(
            "filter {:?} is not allowed for {:?} with byteStride {stride}",
            object.filter, object.mode
        ));
    }
    let within = |buffer: usize, offset: u64, length: u64| {
        root.buffers.get(buffer).is_some_and(|buffer| {
            offset
                .checked_add(length)
                .is_some_and(|end| end <= buffer.byte_length.0)
        })
    };
    let source = usize::try_from(object.buffer).unwrap_or(usize::MAX);
    if !within(source, object.byte_offset, object.byte_length) {
        return Err(format!(
            "its compressed bytes ({} from {} in buffer {}) do not lie within that buffer",
            object.byte_length, object.byte_offset, object.buffer
        ));
    }
    if !within(target, offset, length) {
        return Err(format!(
            "the view ({length} bytes from {offset} in buffer {target}) does not lie within its buffer"
        ));
    }
    // Both ends fit in u64, checked above; each must fit this target.
    let size = |value: u64| {
        usize::try_from(value)
            .map_err(|_| format!("{value} bytes exceed this target's address space"))
    };
    Ok(View {
        index,
        extension,
        target,
        offset: size(offset)?,
        end: size(offset + length)?,
        source,
        source_offset: size(object.byte_offset)?,
        source_end: size(object.byte_offset + object.byte_length)?,
        stride: size(stride)?,
        count: size(count)?,
        mode: object.mode,
        filter: object.filter,
    })
}

/// Decodes `view` from `source` into `out`, its parent view's bytes.
fn decode_view(view: &View, source: &[u8], out: &mut [u8]) -> Result<()> {
    let decoded = match view.mode {
        Mode::Attributes => vertex::decode(
            out,
            view.count,
            view.stride,
            source,
            view.extension.attribute_version(),
        ),
        Mode::Triangles => index::decode_triangles(out, view.count, view.stride, source),
        Mode::Indices => index::decode_sequence(out, view.count, view.stride, source),
    };
    decoded.map_err(|error| {
        format!(
            "buffer view {}: {} {:?} stream: {error}; re-export the asset",
            view.index,
            view.extension.name(),
            view.mode
        )
    })?;
    match view.filter {
        Filter::None => {}
        Filter::Octahedral => filter::octahedral(out, view.stride),
        Filter::Quaternion => filter::quaternion(out),
        Filter::Exponential => filter::exponential(out),
        Filter::Color => filter::color(out, view.stride),
    }
    Ok(())
}
