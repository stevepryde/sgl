//! `KHR_draco_mesh_compression` (cargo feature `draco`): primitives whose
//! indices and attributes a Draco stream holds, decoded once at import into
//! the accessors the extension names, so every reader after it (accessor
//! checks, dequantisation, skins, morph targets, topology, batching) reads
//! them as it reads an uncompressed file. A file whose `extensionsUsed` and
//! `extensionsRequired` do not name the extension takes no step here.
//!
//! The extension (KhronosGroup/glTF 5b00b91,
//! `extensions/2.0/Khronos/KHR_draco_mesh_compression/README.md`) is "based
//! on Draco bitstream version 2.2", and its conformance section says how a
//! loader that processes it reads a primitive:
//!
//! > The loader must process `KHR_draco_mesh_compression` first. The loader
//! > must get the data from `KHR_draco_mesh_compression`'s `bufferView`
//! > property and decompress the data using a Draco decoder to a Draco
//! > geometry.
//! >
//! > Then the loader must process `attributes` and `indices` properties of
//! > the `primitive`. When loading each `accessor`, you must ignore the
//! > `bufferView` and `byteOffset` of the `accessor` and go to the
//! > previously decoded Draco geometry in the `primitive` to get the data of
//! > indices and attributes. A loader must use the decompressed data to fill
//! > the `accessors` or render the decompressed Draco geometry directly.
//! >
//! > If additional attributes are defined in `primitive`'s `attributes`, but
//! > not defined in `KHR_draco_mesh_compression`'s `attributes`, then the
//! > loader must process the additional attributes as usual.
//!
//! Each accessor the extension names is therefore repointed, before the
//! document is built, to its own range of one new buffer that decoding alone
//! fills, at its own component type and type: "The `accessors` properties
//! corresponding to the `attributes` and `indices` of the `primitives` must
//! match the decompressed data", so the decoded attribute's component type
//! and count must equal the accessor's, and the accessor, as in three.js
//! r187's `GLTFDracoMeshCompressionExtension`, decides `normalized`. Its
//! attributes are found by "the correspondent attribute id specified in the
//! `attributes`", Draco's unique id. The mode "must be either `TRIANGLES` or
//! `TRIANGLE_STRIP`"; a strip's decoded indices are its indices accessor's
//! data and expand as any strip does, as three.js r187 does
//! (`toTrianglesDrawMode` after `createDracoPrimitive`).
//!
//! The spec has a loader ignore those views "when loading each accessor" of
//! the primitive, so an accessor something else also reads (a primitive
//! without the extension, a morph target, a skin or an animation) keeps its
//! own data there, and the Draco primitive reads a copy repointed in its
//! place. A buffer only the repointed accessors used, such as an
//! uncompressed fallback, is never read, as three.js reads only the Draco
//! `bufferView`.
//!
//! The decoded accessors are charged to the decoded budget before they are
//! allocated, and the budget must also hold, beside them, the decoder's
//! transient connectivity: [`CONNECTIVITY_BYTES`] a triangle the largest
//! stream's indices accessors declare. The decoder may not produce more
//! triangles than they declare, nor more attribute bytes than the rest of
//! the budget. Other scratch the decoder keeps (per-attribute corner
//! tables at seams, quantized working values) is bounded by those counts
//! but not charged.
//! The decoder is `draco-core` (a safe-Rust port of Google's Draco,
//! Apache-2.0), decoding Edgebreaker (standard and valence) and sequential
//! streams.
use std::collections::HashMap;

use draco_core::{
    DataType as DracoType, DecodeLimits, DecoderBuffer, Mesh, MeshDecoder, PointIndex,
};
use gltf::accessor::DataType;
use gltf::json;
use serde_json::Value;

use super::super::asset::Result;
use super::super::load_budget::Budget;

/// The extension's name.
pub(super) const EXTENSION: &str = "KHR_draco_mesh_compression";

/// The decoder's working memory a triangle, which the decoded budget must
/// hold before a stream decodes: draco-core 2.4.0 keeps the face list
/// (12 bytes a triangle), its corner table's corner-to-vertex and opposite
/// corners (24) and vertex corners (at most 12, a vertex per corner), with
/// 16 to spare for the traversal's per-corner state. Its attributes'
/// decoded values are limited separately.
const CONNECTIVITY_BYTES: u64 = 64;

/// What a repointed accessor holds: the stream's triangles, or the
/// attribute with this unique id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Indices,
    Attribute(u32),
}

/// A Draco stream, decoded once however many primitives name it.
struct Stream {
    /// Its buffer view.
    view: usize,
    /// The first primitive that names it, for messages.
    label: String,
    /// The most triangles its indices accessors declare, which the decoder
    /// may not exceed.
    faces: u64,
}

/// Where a Draco primitive names an accessor: its indices, or the
/// attribute of this semantic.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Slot {
    Indices,
    Attribute(json::validation::Checked<json::mesh::Semantic>),
}

/// An accessor decoding fills: from which stream, with what, and where in
/// the new buffer.
struct Fill {
    accessor: usize,
    stream: usize,
    target: Target,
    /// For messages: the semantic, or `indices`.
    name: String,
    offset: usize,
    length: usize,
}

/// A file's Draco-compressed primitives, planned: the accessors they fill
/// and the buffer those now read from.
pub(super) struct Draco {
    streams: Vec<Stream>,
    fills: Vec<Fill>,
    /// The buffer the fills lie in, and its length.
    buffer: usize,
    length: usize,
    /// Per buffer, whether decoding alone fills it or nothing reads it.
    unread: Vec<bool>,
}

impl Draco {
    /// Plans `root`'s Draco-compressed primitives and repoints the
    /// accessors they fill to a new buffer, or returns `None` when neither
    /// `extensionsUsed` nor `extensionsRequired` names the extension.
    pub(super) fn plan(root: &mut json::Root) -> Result<Option<Self>> {
        if !root
            .extensions_used
            .iter()
            .chain(&root.extensions_required)
            .any(|used| used == EXTENSION)
        {
            return Ok(None);
        }
        let mut streams: Vec<Stream> = Vec::new();
        let mut by_view: HashMap<usize, usize> = HashMap::new();
        let mut fills: Vec<Fill> = Vec::new();
        let mut filled: HashMap<usize, usize> = HashMap::new();
        let mut length = 0usize;
        // Each slot a Draco primitive fills, and the accessor it names.
        let mut slots: Vec<((usize, usize, Slot), usize)> = Vec::new();
        for (mesh, data) in root.meshes.iter().enumerate() {
            for (primitive, data) in data.primitives.iter().enumerate() {
                let Some(value) = data
                    .extensions
                    .as_ref()
                    .and_then(|extensions| extensions.others.get(EXTENSION))
                else {
                    continue;
                };
                let label = format!("mesh {mesh} primitive {primitive}");
                let fail = |reason: String| -> Box<dyn std::error::Error> {
                    format!("{label}: {EXTENSION}: {reason}; re-export valid glTF").into()
                };
                let (view, attributes) = parse(value).map_err(&fail)?;
                if view >= root.buffer_views.len() {
                    return Err(fail(format!("bufferView {view} is out of range")));
                }
                use json::mesh::Mode;
                if !matches!(
                    data.mode,
                    json::validation::Checked::Valid(Mode::Triangles | Mode::TriangleStrip)
                ) {
                    return Err(fail(
                        "its primitive's mode is neither TRIANGLES nor TRIANGLE_STRIP, which the extension requires".into(),
                    ));
                }
                let indices = data.indices.ok_or_else(|| {
                    fail("its primitive has no indices accessor for the decoded triangles".into())
                })?;
                let mut targets = vec![(
                    indices.value(),
                    Target::Indices,
                    "indices".to_owned(),
                    Slot::Indices,
                )];
                for (name, id) in attributes {
                    let (semantic, accessor) = data
                        .attributes
                        .iter()
                        .find_map(|(semantic, accessor)| match semantic {
                            json::validation::Checked::Valid(valid)
                                if valid.to_string() == name =>
                            {
                                Some((semantic.clone(), accessor.value()))
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            fail(format!("it names attribute {name}, which its primitive does not have; the extension's attributes must be a subset of the primitive's"))
                        })?;
                    targets.push((
                        accessor,
                        Target::Attribute(id),
                        name,
                        Slot::Attribute(semantic),
                    ));
                }
                let stream = *by_view.entry(view).or_insert_with(|| {
                    streams.push(Stream {
                        view,
                        label: label.clone(),
                        faces: 0,
                    });
                    streams.len() - 1
                });
                for (accessor, target, name, slot) in targets {
                    slots.push(((mesh, primitive, slot), accessor));
                    let data = root.accessors.get(accessor).ok_or_else(|| {
                        fail(format!("its {name} accessor {accessor} is out of range"))
                    })?;
                    let count = data.count.0;
                    if count == 0 {
                        return Err(fail(format!(
                            "its {name} accessor {accessor} has no elements"
                        )));
                    }
                    if target == Target::Indices {
                        let entry = &mut streams[stream];
                        entry.faces = entry.faces.max(count / 3);
                    }
                    if let Some(&existing) = filled.get(&accessor) {
                        let existing = &fills[existing];
                        if existing.stream != stream || existing.target != target {
                            return Err(fail(format!(
                                "its {name} accessor {accessor} is also filled from another Draco stream or attribute"
                            )));
                        }
                        continue;
                    }
                    let size = element_size(data).ok_or_else(|| {
                        fail(format!(
                            "its {name} accessor {accessor} has an invalid componentType or type"
                        ))
                    })?;
                    let bytes = usize::try_from(count)
                        .ok()
                        .and_then(|count| count.checked_mul(size))
                        .ok_or_else(|| {
                            fail(format!(
                                "its {name} accessor {accessor}'s byte length overflows"
                            ))
                        })?;
                    let offset = length
                        .checked_next_multiple_of(4)
                        .ok_or_else(|| fail("decoded byte length overflows".into()))?;
                    length = offset
                        .checked_add(bytes)
                        .ok_or_else(|| fail("decoded byte length overflows".into()))?;
                    filled.insert(accessor, fills.len());
                    fills.push(Fill {
                        accessor,
                        stream,
                        target,
                        name,
                        offset,
                        length: bytes,
                    });
                }
            }
        }
        if fills.is_empty() {
            return Ok(None);
        }

        // An accessor anything else reads keeps its data for them, and the
        // Draco primitives read a copy; another is repointed in place, and
        // the view it read before may leave its buffer unread.
        let shared = read_elsewhere(root, &slots);
        let mut former = vec![false; root.buffer_views.len()];
        let mut copies = HashMap::new();
        let buffer = root.buffers.len();
        root.buffers.push(serde_json::from_value(serde_json::json!({
            "byteLength": length,
        }))?);
        for fill in &fills {
            let view = root.buffer_views.len();
            root.buffer_views
                .push(serde_json::from_value(serde_json::json!({
                    "buffer": buffer,
                    "byteOffset": fill.offset,
                    "byteLength": fill.length,
                }))?);
            let index = if shared[fill.accessor] {
                root.accessors.push(root.accessors[fill.accessor].clone());
                copies.insert(fill.accessor, root.accessors.len() - 1);
                root.accessors.len() - 1
            } else {
                if let Some(view) = root.accessors[fill.accessor]
                    .buffer_view
                    .and_then(|view| former.get_mut(view.value()))
                {
                    *view = true;
                }
                fill.accessor
            };
            let accessor = &mut root.accessors[index];
            accessor.buffer_view = Some(json::Index::new(view as u32));
            accessor.byte_offset = None;
        }
        for ((mesh, primitive, slot), accessor) in slots {
            let Some(&copy) = copies.get(&accessor) else {
                continue;
            };
            let data = &mut root.meshes[mesh].primitives[primitive];
            let copy = json::Index::new(copy as u32);
            match slot {
                Slot::Indices => data.indices = Some(copy),
                Slot::Attribute(semantic) => {
                    data.attributes.insert(semantic, copy);
                }
            }
        }
        let unread = unread(root, &former, &streams, buffer);
        Ok(Some(Self {
            streams,
            fills,
            buffer,
            length,
            unread,
        }))
    }

    /// Whether buffer `index` is filled by decoding alone or read by
    /// nothing, and so never read from its source.
    pub(super) fn unread(&self, index: usize) -> bool {
        self.unread.get(index).copied().unwrap_or(false)
    }

    /// Decodes every stream and fills its accessors, after the new buffer's
    /// length is charged to the decoded `budget` and before it is
    /// allocated. Each decode is limited to the triangles its indices
    /// accessors declare, whose connectivity must fit what remains of the
    /// budget, and to the rest of it for its attribute values.
    pub(super) fn decode(
        &self,
        root: &json::Root,
        buffers: &mut [gltf::buffer::Data],
        budget: &mut Budget,
    ) -> Result<()> {
        // The accessors, and the largest stream's connectivity beside them,
        // before either is allocated.
        let connectivity = self
            .streams
            .iter()
            .map(|stream| stream.faces.saturating_mul(CONNECTIVITY_BYTES))
            .max()
            .unwrap_or(0);
        budget
            .check((self.length as u64).saturating_add(connectivity))
            .and_then(|()| budget.charge(self.length as u64))
            .map_err(|error| {
                format!("{EXTENSION} decoded accessors and the decoder's connectivity: {error}")
            })?;
        let mut data = vec![0u8; self.length];
        for (index, stream) in self.streams.iter().enumerate() {
            let fail = |reason: String| -> Box<dyn std::error::Error> {
                format!(
                    "{}: {EXTENSION} stream in buffer view {}: {reason}",
                    stream.label, stream.view
                )
                .into()
            };
            let view = &root.buffer_views[stream.view];
            let start = view.byte_offset.map_or(0, |offset| offset.0);
            let bytes = usize::try_from(start)
                .ok()
                .zip(usize::try_from(view.byte_length.0).ok())
                .and_then(|(start, length)| {
                    buffers
                        .get(view.buffer.value())?
                        .0
                        .get(start..start.checked_add(length)?)
                })
                .ok_or_else(|| {
                    fail("the view does not lie within its buffer; re-export valid glTF".into())
                })?;
            // Checked above against the remainder.
            let connectivity = stream.faces.saturating_mul(CONNECTIVITY_BYTES);
            let limits = DecodeLimits::default()
                .with_max_faces(stream.faces)
                .with_max_decoded_bytes(budget.remaining().saturating_sub(connectivity));
            let mut mesh = Mesh::new();
            MeshDecoder::new()
                .decode(
                    &mut DecoderBuffer::new(bytes).with_limits(limits),
                    &mut mesh,
                )
                .map_err(|error| fail(format!("{error}; re-export the asset")))?;
            for fill in self.fills.iter().filter(|fill| fill.stream == index) {
                let accessor = &root.accessors[fill.accessor];
                let out = &mut data[fill.offset..fill.offset + fill.length];
                let filled = match fill.target {
                    Target::Indices => indices(&mesh, accessor, out),
                    Target::Attribute(id) => attribute(&mesh, id, accessor, out),
                };
                filled.map_err(|reason| {
                    fail(format!(
                        "{} accessor {}: {reason}; re-export valid glTF",
                        fill.name, fill.accessor
                    ))
                })?;
            }
        }
        buffers[self.buffer].0 = data;
        Ok(())
    }
}

/// The extension object's `bufferView` and `attributes`, both required by
/// its schema.
fn parse(value: &Value) -> std::result::Result<(usize, Vec<(String, u32)>), String> {
    let view = value
        .get("bufferView")
        .ok_or("bufferView is missing")?
        .as_u64()
        .and_then(|view| usize::try_from(view).ok())
        .ok_or("bufferView is not a nonnegative integer")?;
    let attributes = value
        .get("attributes")
        .ok_or("attributes is missing")?
        .as_object()
        .ok_or("attributes is not an object")?
        .iter()
        .map(|(name, id)| {
            id.as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .map(|id| (name.clone(), id))
                .ok_or_else(|| format!("attribute {name}'s id is not an unsigned 32-bit integer"))
        })
        .collect::<std::result::Result<_, _>>()?;
    Ok((view, attributes))
}

/// An accessor's element size in bytes, `None` for an invalid type.
fn element_size(accessor: &json::Accessor) -> Option<usize> {
    use json::validation::Checked::Valid;
    match (accessor.component_type, accessor.type_) {
        (Valid(component), Valid(shape)) => Some(component.0.size() * shape.multiplicity()),
        _ => None,
    }
}

/// The glTF component type a Draco data type is, `None` for one glTF has
/// no accessor type for.
fn gltf_type(data_type: DracoType) -> Option<DataType> {
    Some(match data_type {
        DracoType::Int8 => DataType::I8,
        DracoType::Uint8 => DataType::U8,
        DracoType::Int16 => DataType::I16,
        DracoType::Uint16 => DataType::U16,
        DracoType::Uint32 => DataType::U32,
        DracoType::Float32 => DataType::F32,
        _ => return None,
    })
}

/// Writes `mesh`'s triangles into `out` at `accessor`'s component type.
fn indices(
    mesh: &Mesh,
    accessor: &json::Accessor,
    out: &mut [u8],
) -> std::result::Result<(), String> {
    let faces = mesh.faces();
    let count = accessor.count.0;
    if faces.len() as u64 * 3 != count {
        return Err(format!(
            "it holds {count} indices where the stream decodes {} triangles",
            faces.len()
        ));
    }
    let json::validation::Checked::Valid(component) = accessor.component_type else {
        return Err("its componentType is invalid".into());
    };
    let size = component.0.size();
    let points = mesh.num_points();
    for (index, slot) in faces.iter().flatten().zip(out.chunks_exact_mut(size)) {
        let index = index.0;
        if index as usize >= points {
            return Err(format!(
                "the stream decodes index {index} for {points} points"
            ));
        }
        let wide = index.to_le_bytes();
        let fits = match size {
            1 => index <= u32::from(u8::MAX),
            2 => index <= u32::from(u16::MAX),
            _ => true,
        };
        if !fits {
            return Err(format!(
                "index {index} does not fit its {:?} componentType",
                component.0
            ));
        }
        slot.copy_from_slice(&wide[..size]);
    }
    Ok(())
}

/// Writes the attribute with unique id `id` into `out`, point by point
/// through its point-to-value map, at `accessor`'s component type, which
/// it must already have.
fn attribute(
    mesh: &Mesh,
    id: u32,
    accessor: &json::Accessor,
    out: &mut [u8],
) -> std::result::Result<(), String> {
    use json::validation::Checked::Valid;
    let attribute = mesh
        .attribute_by_unique_id(id)
        .ok_or_else(|| format!("the stream has no attribute with id {id}"))?;
    let (Valid(component), Valid(shape)) = (accessor.component_type, accessor.type_) else {
        return Err("its componentType or type is invalid".into());
    };
    let decoded = gltf_type(attribute.data_type());
    let components = usize::from(attribute.num_components());
    if decoded != Some(component.0) || components != shape.multiplicity() {
        return Err(format!(
            "it holds {:?} {:?} where the stream's attribute {id} decodes {components} {:?} components",
            component.0,
            shape,
            attribute.data_type()
        ));
    }
    let points = mesh.num_points();
    let count = accessor.count.0;
    if points as u64 != count {
        return Err(format!(
            "it holds {count} elements where the stream decodes {points} points"
        ));
    }
    let size = component.0.size() * components;
    let stride = usize::try_from(attribute.byte_stride())
        .map_err(|_| "the stream's attribute has a negative stride")?;
    let values = attribute.buffer().data();
    for (point, slot) in out.chunks_exact_mut(size).enumerate() {
        let value = attribute.mapped_index(PointIndex(point as u32)).0 as usize;
        let element = value
            .checked_mul(stride)
            .and_then(|start| values.get(start..start.checked_add(size)?))
            .ok_or_else(|| {
                format!("point {point}'s value {value} lies outside the decoded attribute")
            })?;
        slot.copy_from_slice(element);
    }
    Ok(())
}

/// Per accessor of `root`, whether anything but the Draco primitives'
/// `slots` reads it: another primitive's attribute or indices, a morph
/// target, a skin's inverse bind matrices or an animation sampler.
fn read_elsewhere(root: &json::Root, slots: &[((usize, usize, Slot), usize)]) -> Vec<bool> {
    let draco: std::collections::HashSet<&(usize, usize, Slot)> =
        slots.iter().map(|(slot, _)| slot).collect();
    let mut read = vec![false; root.accessors.len()];
    let mut mark = |accessor: Option<json::Index<json::Accessor>>| {
        if let Some(read) = accessor.and_then(|accessor| read.get_mut(accessor.value())) {
            *read = true;
        }
    };
    for (mesh, data) in root.meshes.iter().enumerate() {
        for (primitive, data) in data.primitives.iter().enumerate() {
            if !draco.contains(&(mesh, primitive, Slot::Indices)) {
                mark(data.indices);
            }
            for (semantic, accessor) in &data.attributes {
                if !draco.contains(&(mesh, primitive, Slot::Attribute(semantic.clone()))) {
                    mark(Some(*accessor));
                }
            }
            for target in data.targets.iter().flatten() {
                for accessor in [target.positions, target.normals, target.tangents] {
                    mark(accessor);
                }
            }
        }
    }
    for skin in &root.skins {
        mark(skin.inverse_bind_matrices);
    }
    for sampler in root
        .animations
        .iter()
        .flat_map(|animation| &animation.samplers)
    {
        mark(Some(sampler.input));
        mark(Some(sampler.output));
    }
    read
}

/// Per buffer of `root`, whether nothing reads it once the repointed
/// accessors have left their `former` views: a buffer at least one of
/// those views lies in, which no view still in use does, nor a
/// compressed view's extension, or `added`, which decoding fills.
fn unread(root: &json::Root, former: &[bool], streams: &[Stream], added: usize) -> Vec<bool> {
    let mut live = vec![false; root.buffer_views.len()];
    let mut mark = |view: Option<json::Index<json::buffer::View>>| {
        if let Some(view) = view.and_then(|view| live.get_mut(view.value())) {
            *view = true;
        }
    };
    for accessor in &root.accessors {
        mark(accessor.buffer_view);
        if let Some(sparse) = &accessor.sparse {
            mark(Some(sparse.indices.buffer_view));
            mark(Some(sparse.values.buffer_view));
        }
    }
    for image in &root.images {
        mark(image.buffer_view);
    }
    for stream in streams {
        mark(Some(json::Index::new(stream.view as u32)));
    }
    let mut read = vec![false; root.buffers.len()];
    let mut orphaned = vec![false; root.buffers.len()];
    for (index, view) in root.buffer_views.iter().enumerate() {
        let extensions = view
            .extensions
            .as_ref()
            .map(|extensions| &extensions.others);
        // A compressed view (Meshopt's) is in use, and so is the buffer
        // its extension takes bytes from.
        for object in extensions.into_iter().flat_map(|others| others.values()) {
            if let Some(buffer) = object
                .get("buffer")
                .and_then(Value::as_u64)
                .and_then(|buffer| read.get_mut(usize::try_from(buffer).ok()?))
            {
                *buffer = true;
            }
        }
        let buffer = view.buffer.value();
        if buffer >= read.len() {
            continue;
        }
        if live[index] || extensions.is_some_and(|others| !others.is_empty()) {
            read[buffer] = true;
        } else if former.get(index).copied().unwrap_or(false) {
            orphaned[buffer] = true;
        }
    }
    (0..root.buffers.len())
        .map(|buffer| buffer == added || (orphaned[buffer] && !read[buffer]))
        .collect()
}
