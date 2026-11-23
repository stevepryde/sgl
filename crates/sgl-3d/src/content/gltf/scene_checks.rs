//! The checks a scene passes before the load reads it: its roots, and the
//! bytes its decode will produce charged to the decoded budget.

use glam::{Mat4, Quat, Vec3};
use gltf::animation::Property;

use super::super::asset::{Result, Vertex};
use super::super::deformation;
use super::super::load_budget::Budget;
use super::{instancing, topology};

/// Refuses a scene whose walks would read a subtree more than once, before
/// any walk of it: one that lists a node among its roots more than once or
/// a node with a parent, which glTF 2.0 forbids (3.5.1, and the scene
/// schema's `uniqueItems`: a scene's `nodes` are distinct root nodes) and
/// which would read a subtree's meshes, instancing and lights again each
/// time it is listed. `parents` holds each node's parent, its hierarchy a
/// set of trees. What the load decodes is charged after it
/// (`charge_scene`).
pub(super) fn check_roots(scene: &gltf::Scene<'_>, parents: &[Option<usize>]) -> Result<()> {
    let mut listed = vec![false; parents.len()];
    for node in scene.nodes().map(|node| node.index()) {
        if std::mem::replace(&mut listed[node], true) {
            return Err(format!(
                "scene {} lists root node {node} more than once; re-export valid glTF",
                scene.index()
            )
            .into());
        }
        if let Some(parent) = parents[node] {
            return Err(format!("scene {} lists node {node} as a root, but node {parent} is its parent; re-export valid glTF", scene.index()).into());
        }
    }
    Ok(())
}

/// Charge the bytes a load produces before reading accessors: each scene
/// node's primitive vertices, indices, skin influences and morph deltas;
/// every skin's joints and every animation channel's keys. An instanced
/// node's mesh is charged once however many instanced nodes use it, and
/// each instanced node its instances' transforms, by its accessors'
/// declared count (`plan`). Selected and
/// unselected nodes count, as before. Minimum one-element charges also bound
/// the counting work for empty declarations. Byte products and accumulation
/// are checked, and the vertex index boundary remains independent of budget.
pub(super) fn charge_scene(
    document: &gltf::Document,
    scene: &gltf::Scene<'_>,
    plan: &instancing::Plan,
    budget: &mut Budget,
) -> Result<()> {
    use std::mem::size_of;
    let mut vertices_total = 0u64;
    let mut instanced_meshes = vec![false; document.meshes().len()];
    let mut nodes: Vec<gltf::Node<'_>> = scene.nodes().collect();
    while let Some(node) = nodes.pop() {
        nodes.extend(node.children());
        if let Some(instancing::NodeUse::Instanced { count, .. }) = plan.node(node.index()) {
            budget.charge_count(*count as u64, size_of::<Mat4>() as u64)?;
            if let Some(mesh) = node.mesh()
                && std::mem::replace(&mut instanced_meshes[mesh.index()], true)
            {
                continue;
            }
        }
        let vertex_size = size_of::<Vertex>()
            + node
                .skin()
                .map_or(0, |_| size_of::<deformation::Influence>());
        for primitive in node.mesh().into_iter().flat_map(|mesh| mesh.primitives()) {
            let vertices = primitive
                .get(&gltf::Semantic::Positions)
                .map_or(0, |positions| positions.count()) as u64;
            vertices_total = vertices_total
                .checked_add(vertices)
                .filter(|&count| count <= u64::from(u32::MAX))
                .ok_or("scene vertices exceed the u32 index range; split the asset")?;
            let authored = primitive
                .indices()
                .map_or(vertices, |indices| indices.count() as u64);
            // A strip or fan is charged as the triangle list it becomes;
            // a mode the read refuses, as authored.
            let indices =
                topology::triangle_list_len(primitive.mode(), authored).unwrap_or(authored);
            budget.charge_count(vertices.max(1), vertex_size as u64)?;
            budget.charge_count(indices.max(1), size_of::<u32>() as u64)?;
            let delta_size = (size_of::<deformation::MorphDelta>() as u64)
                .checked_mul(primitive.morph_targets().count() as u64)
                .ok_or("morph target byte count overflow")?;
            budget.charge_count(vertices.max(1), delta_size)?;
        }
    }
    for skin in document.skins() {
        budget.charge_count(
            skin.joints().count().max(1) as u64,
            size_of::<deformation::Joint>() as u64,
        )?;
    }
    for channel in document
        .animations()
        .flat_map(|animation| animation.channels())
    {
        let value = match channel.target().property() {
            Property::Translation | Property::Scale => size_of::<Vec3>(),
            Property::Rotation => size_of::<Quat>(),
            Property::MorphTargetWeights => size_of::<f32>(),
        };
        let sampler = channel.sampler();
        budget.charge_count(
            sampler.input().count().max(1) as u64,
            size_of::<f32>() as u64,
        )?;
        budget.charge_count(sampler.output().count().max(1) as u64, value as u64)?;
    }
    Ok(())
}
