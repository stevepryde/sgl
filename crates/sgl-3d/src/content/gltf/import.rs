//! The document's import: its JSON read and checked, its required
//! extensions refused or cleared, and its buffers read.

use std::path::Path;

use super::super::asset::Result;
use super::super::load_budget::Budget;
use super::{Ignored, SUPPORTED_EXTENSIONS, instancing, meshopt, rig, sources};

/// The document in `bytes`, validated, its buffers, with compressed
/// buffer views decoded (`meshopt`), and whether it requires
/// `EXT_mesh_gpu_instancing`, which decides what an instanced node SGL3D
/// cannot instance does (`instancing::Plan`). glTF 2.0 5.17.2: a
/// file that requires an extension SGL3D does not support fails to load.
/// The gltf crate's validation knows none of the material extensions SGL3D
/// reads itself, so once every required one is known to be supported the
/// list is cleared for it; all its structural validation remains.
pub(super) fn import(
    bytes: &[u8],
    base: Option<&Path>,
    (budget, decoded_budget): (&mut Budget, &mut Budget),
) -> Result<(gltf::Document, Vec<gltf::buffer::Data>, Unread)> {
    // The gltf crate subtracts the 12-byte header from the length the GLB
    // header declares unchecked, so a header declaring less is refused here.
    if bytes.starts_with(b"glTF") {
        let length = bytes
            .get(8..12)
            .and_then(|length| <[u8; 4]>::try_from(length).ok())
            .map(u32::from_le_bytes);
        if length.is_none_or(|length| length < 12) {
            return Err("the GLB header is truncated or declares a length below its own 12 bytes; re-export valid glTF".into());
        }
    }
    // Keep the BIN chunk borrowed until its size has passed the source budget.
    let glb = bytes
        .starts_with(b"glTF")
        .then(|| gltf::binary::Glb::from_slice(bytes))
        .transpose()?;
    let json = glb.as_ref().map_or(bytes, |glb| glb.json.as_ref());
    let mut root: gltf::json::Root = serde_json::from_slice(json)?;
    if let Some(extension) = root
        .extensions_required
        .iter()
        .find(|extension| !SUPPORTED_EXTENSIONS.contains(&extension.as_str()))
    {
        return Err(format!("the file requires glTF extension {extension}, which SGL3D does not support; bake it into the export or add renderer support").into());
    }
    check_root(&root)?;
    let morph_attributes = rig::ignored_morph_attributes(json, &root)?;
    let instancing_required = root
        .extensions_required
        .iter()
        .any(|name| name == instancing::EXTENSION);
    root.extensions_required.clear();
    let document = gltf::Document::from_json(root)?;
    let blob = glb.as_ref().and_then(|glb| glb.bin.as_deref());
    let compressed = meshopt::Compressed::find(&document)?;
    let unread = |buffer| {
        compressed
            .as_ref()
            .is_some_and(|views| views.unread(buffer))
    };
    let mut buffers = sources::buffers(&document, base, blob, budget, unread)?;
    if let Some(compressed) = compressed {
        compressed.decode(&mut buffers, decoded_budget)?;
    }
    Ok((
        document,
        buffers,
        Unread {
            instancing_required,
            morph_attributes,
        },
    ))
}

/// What the import found in the document's JSON that the parsed document
/// no longer holds.
pub(super) struct Unread {
    /// Whether the file requires `EXT_mesh_gpu_instancing`, a requirement
    /// cleared from the document for the gltf crate's validation.
    pub(super) instancing_required: bool,
    /// The morph target attributes gltf-json drops as it parses
    /// (`Ignored::MorphTargetAttribute`).
    pub(super) morph_attributes: Vec<Ignored>,
}

/// Check the references and source variants gltf-json 1.4.1 leaves unchecked
/// before its own validation indexes POSITION or its readers unwrap them.
fn check_root(root: &gltf::json::Root) -> Result<()> {
    use gltf::json::validation::Checked;
    for (mesh, data) in root.meshes.iter().enumerate() {
        for (primitive, data) in data.primitives.iter().enumerate() {
            for (attribute, accessor) in &data.attributes {
                if accessor.value() >= root.accessors.len() {
                    return Err(format!("mesh {mesh} primitive {primitive} attribute {attribute:?}: accessor {} is out of range", accessor.value()).into());
                }
            }
            for (target, data) in data.targets.iter().flatten().enumerate() {
                for (attribute, accessor) in [
                    ("POSITION", data.positions),
                    ("NORMAL", data.normals),
                    ("TANGENT", data.tangents),
                ] {
                    if let Some(accessor) = accessor
                        && accessor.value() >= root.accessors.len()
                    {
                        return Err(format!("mesh {mesh} primitive {primitive} morph target {target} {attribute}: accessor {} is out of range", accessor.value()).into());
                    }
                }
            }
        }
    }
    for (animation, data) in root.animations.iter().enumerate() {
        for (channel, data) in data.channels.iter().enumerate() {
            if data.target.node.value() >= root.nodes.len() {
                return Err(format!(
                    "animation {animation} channel {channel}: target node {} is out of range",
                    data.target.node.value()
                )
                .into());
            }
            if !matches!(data.target.path, Checked::Valid(_)) {
                return Err(format!(
                    "animation {animation} channel {channel}: invalid target path"
                )
                .into());
            }
        }
    }
    for (image, data) in root.images.iter().enumerate() {
        if data.uri.is_some() == data.buffer_view.is_some() {
            return Err(
                format!("image {image}: exactly one of uri and bufferView is required").into(),
            );
        }
        if data.buffer_view.is_some() && data.mime_type.is_none() {
            return Err(format!("image {image}: bufferView requires mimeType").into());
        }
    }
    Ok(())
}
