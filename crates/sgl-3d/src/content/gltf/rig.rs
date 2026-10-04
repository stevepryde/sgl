//! A glTF document's rig as plain data: its node hierarchy, its skins'
//! joints in one list, the morph weights of its morphed mesh nodes, and its
//! animation clips.
use std::collections::HashMap;

use glam::{Mat4, Quat, Vec3};
use gltf::animation::{Interpolation as GltfInterpolation, util::ReadOutputs};

use super::super::asset::Result;
use super::super::deformation::{
    Channel, ChannelValues, Clip, Influence, Interpolation, Joint, MAX_INDEX, MorphDelta,
    MorphTarget, MorphWeight, Node, Rig,
};

/// The rig being imported, and where each skin's joints and each morphed
/// node's weights start in it.
pub(super) struct Rigging {
    pub rig: Rig,
    /// Each skin's first joint, by skin index.
    skins: Vec<u32>,
    /// Each morphed node's first weight.
    morphs: HashMap<usize, u32>,
}

impl Rigging {
    /// The document's nodes, skins and clips; empty when it has no skin,
    /// morph target or animation.
    pub fn new(document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Result<Self> {
        let mut rigging = Self {
            rig: Rig::default(),
            skins: Vec::new(),
            morphs: HashMap::new(),
        };
        let morphed = document
            .meshes()
            .flat_map(|mesh| mesh.primitives())
            .any(|primitive| primitive.morph_targets().next().is_some());
        if document.skins().next().is_none() && document.animations().next().is_none() && !morphed {
            return Ok(rigging);
        }
        let mut parents = vec![None; document.nodes().count()];
        for node in document.nodes() {
            for child in node.children() {
                parents[child.index()] = Some(node.index());
            }
        }
        rigging.rig.nodes = document
            .nodes()
            .map(|node| {
                let (translation, rotation, scale) = node.transform().decomposed();
                Node {
                    name: node.name().map(str::to_owned),
                    parent: parents[node.index()],
                    translation: Vec3::from_array(translation),
                    rotation: Quat::from_array(rotation),
                    scale: Vec3::from_array(scale),
                }
            })
            .collect();
        for skin in document.skins() {
            rigging.skins.push(rigging.rig.joints.len() as u32);
            let reader = skin.reader(|buffer| Some(&buffers[buffer.index()].0));
            let inverse_binds: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
                Some(matrices) => matrices.map(|m| Mat4::from_cols_array_2d(&m)).collect(),
                None => vec![Mat4::IDENTITY; skin.joints().count()],
            };
            if inverse_binds.len() != skin.joints().count()
                || inverse_binds.iter().any(|m| !m.is_finite())
            {
                return Err(format!(
                    "skin {} has {} finite inverse bind matrices for {} joints",
                    skin.index(),
                    inverse_binds.len(),
                    skin.joints().count()
                )
                .into());
            }
            rigging
                .rig
                .joints
                .extend(
                    skin.joints()
                        .zip(inverse_binds)
                        .map(|(node, inverse_bind)| Joint {
                            node: node.index(),
                            inverse_bind,
                        }),
                );
        }
        rigging.rig.clips = document
            .animations()
            .map(|animation| read_clip(&animation, buffers))
            .collect::<Result<_>>()?;
        Ok(rigging)
    }

    /// The first joint of `skin` and its joint count.
    pub fn skin(&self, skin: &gltf::Skin) -> (u32, u32) {
        (self.skins[skin.index()], skin.joints().count() as u32)
    }

    /// The first of the `count` morph weights of `node`, added on first use
    /// with its rest weights (the node's, else its mesh's, else zero).
    pub fn morph_weights(&mut self, node: &gltf::Node<'_>, count: usize) -> Result<u32> {
        if let Some(&first) = self.morphs.get(&node.index()) {
            return Ok(first);
        }
        let rest = node
            .weights()
            .or_else(|| node.mesh().and_then(|mesh| mesh.weights()))
            .map(<[f32]>::to_vec)
            .unwrap_or_else(|| vec![0.; count]);
        if rest.len() != count || rest.iter().any(|w| !w.is_finite()) {
            return Err(format!(
                "node {} has {} finite rest morph weights for {count} targets",
                node.index(),
                rest.len()
            )
            .into());
        }
        let first = self.rig.morph_weights.len() as u32;
        if (first as usize + count) > MAX_INDEX as usize + 1 {
            return Err(format!(
                "node {} has morph weights beyond the 65536 an asset may have",
                node.index()
            )
            .into());
        }
        self.rig
            .morph_weights
            .extend(rest.into_iter().map(|rest| MorphWeight {
                node: node.index(),
                rest,
            }));
        self.morphs.insert(node.index(), first);
        Ok(first)
    }
}

fn read_clip(animation: &gltf::Animation<'_>, buffers: &[gltf::buffer::Data]) -> Result<Clip> {
    let label = format!("animation {}", animation.index());
    let channels = animation
        .channels()
        .map(|channel| {
            let reader = channel.reader(|buffer| Some(&buffers[buffer.index()].0));
            let times: Vec<f32> = reader
                .read_inputs()
                .ok_or_else(|| format!("{label}: a channel has no keyframe times"))?
                .collect();
            if times.is_empty()
                || times.iter().any(|t| !t.is_finite())
                || times.windows(2).any(|pair| pair[1] < pair[0])
            {
                return Err(
                    format!("{label}: keyframe times must be finite and increasing").into(),
                );
            }
            let values = match reader
                .read_outputs()
                .ok_or_else(|| format!("{label}: a channel has no keyframe values"))?
            {
                ReadOutputs::Translations(values) => {
                    ChannelValues::Translation(values.map(Vec3::from_array).collect())
                }
                ReadOutputs::Rotations(values) => {
                    ChannelValues::Rotation(values.into_f32().map(Quat::from_array).collect())
                }
                ReadOutputs::Scales(values) => {
                    ChannelValues::Scale(values.map(Vec3::from_array).collect())
                }
                ReadOutputs::MorphTargetWeights(values) => {
                    ChannelValues::MorphWeights(values.into_f32().collect())
                }
            };
            let interpolation = match channel.sampler().interpolation() {
                GltfInterpolation::Step => Interpolation::Step,
                GltfInterpolation::Linear => Interpolation::Linear,
                GltfInterpolation::CubicSpline => Interpolation::CubicSpline,
            };
            let per_key = if interpolation == Interpolation::CubicSpline {
                3
            } else {
                1
            };
            let count = match &values {
                ChannelValues::Translation(v) | ChannelValues::Scale(v) => v.len(),
                ChannelValues::Rotation(v) => v.len(),
                ChannelValues::MorphWeights(v) => {
                    let targets = channel
                        .target()
                        .node()
                        .mesh()
                        .and_then(|mesh| mesh.primitives().next())
                        .map_or(0, |primitive| primitive.morph_targets().count());
                    if targets == 0 {
                        return Err(format!(
                            "{label}: morph weights animate a node without morph targets"
                        )
                        .into());
                    }
                    if v.len() % targets == 0 {
                        v.len() / targets
                    } else {
                        usize::MAX
                    }
                }
            };
            if count != times.len() * per_key {
                return Err(
                    format!("{label}: a channel's values do not match its keyframes").into(),
                );
            }
            Ok(Channel {
                node: channel.target().node().index(),
                interpolation,
                times,
                values,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Clip {
        name: animation.name().map(str::to_owned),
        channels,
    })
}

/// A skinned primitive's influences on the joints of its skin, which starts
/// at joint `first` of the asset's rig and has `count` joints: each with
/// finite, nonnegative weights of positive sum, naming joints of its skin
/// among the 65536 an asset may have.
pub(super) fn read_influences<'a, 's, F>(
    reader: &gltf::mesh::Reader<'a, 's, F>,
    (first, count): (u32, u32),
    vertices: usize,
    label: &str,
) -> Result<Vec<Influence>>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let joints: Vec<[u16; 4]> = reader
        .read_joints(0)
        .ok_or_else(|| format!("{label}: a skinned primitive lacks JOINTS_0"))?
        .into_u16()
        .collect();
    let weights: Vec<[f32; 4]> = reader
        .read_weights(0)
        .ok_or_else(|| format!("{label}: a skinned primitive lacks WEIGHTS_0"))?
        .into_f32()
        .collect();
    if joints.len() != vertices || weights.len() != vertices {
        return Err(format!("{label}: joint and weight counts differ from positions").into());
    }
    joints
        .into_iter()
        .zip(weights)
        .map(|(joints, weights)| {
            if joints.iter().any(|&joint| u32::from(joint) >= count) {
                return Err(format!("{label}: a vertex names a joint its skin lacks").into());
            }
            if joints.iter().any(|&joint| first + u32::from(joint) > MAX_INDEX) {
                return Err(
                    format!("{label}: a vertex names a joint beyond the 65536 an asset may have")
                        .into(),
                );
            }
            if weights.iter().any(|w| !w.is_finite() || *w < 0.)
                || weights.iter().sum::<f32>() <= 0.
            {
                return Err(format!(
                    "{label}: a vertex's joint weights must be finite and nonnegative with a positive sum"
                )
                .into());
            }
            Ok(Influence {
                joints: joints.map(|joint| first + u32::from(joint)),
                weights,
            })
        })
        .collect()
}

/// A primitive's morph targets, scaled by the rig's weights from `first`,
/// in the space its vertices were baked to: positions by `transform`;
/// normals by `normal_transform`, and tangents by `transform` projected off
/// the baked normal, each at the factor that normalized its baked vector.
/// Without authored `tangents`, a target displaces no tangent.
pub(super) fn read_morph_targets<'a, 's, F>(
    reader: &gltf::mesh::Reader<'a, 's, F>,
    first: u32,
    (transform, normal_transform): (Mat4, Mat4),
    (normals, authored_tangents): (&[[f32; 3]], Option<&[[f32; 4]]>),
    label: &str,
) -> Result<Vec<MorphTarget>>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let count = normals.len();
    let read = |values: Option<Vec<[f32; 3]>>, target: usize| -> Result<Vec<Vec3>> {
        let Some(values) = values else {
            return Ok(vec![Vec3::ZERO; count]);
        };
        if values.len() != count || values.iter().flatten().any(|v| !v.is_finite()) {
            return Err(
                format!("{label}: morph target {target} does not match its positions").into(),
            );
        }
        Ok(values.into_iter().map(Vec3::from_array).collect())
    };
    reader
        .read_morph_targets()
        .enumerate()
        .map(|(target, (positions, target_normals, tangents))| {
            let positions = read(positions.map(Iterator::collect), target)?;
            let target_normals = read(target_normals.map(Iterator::collect), target)?;
            let tangents = read(tangents.map(Iterator::collect), target)?;
            let deltas = (0..count)
                .map(|vertex| {
                    // The baked normal was normalized after `normal_transform`,
                    // and the baked tangent after `transform` and projection
                    // off it; their displacements keep those scales.
                    let normal =
                        normal_transform.transform_vector3(Vec3::from_array(normals[vertex]));
                    let baked_normal = normal.normalize();
                    let tangent = authored_tangents.map_or(Vec3::ZERO, |authored| {
                        let [x, y, z, _] = authored[vertex];
                        let transformed = transform.transform_vector3(Vec3::new(x, y, z));
                        let projected = transformed - baked_normal * baked_normal.dot(transformed);
                        transform.transform_vector3(tangents[vertex]) / projected.length()
                    });
                    MorphDelta {
                        position: transform.transform_vector3(positions[vertex]).to_array(),
                        normal: (normal_transform.transform_vector3(target_normals[vertex])
                            / normal.length())
                        .to_array(),
                        tangent: tangent.to_array(),
                    }
                })
                .collect();
            Ok(MorphTarget {
                weight: first + target as u32,
                deltas,
            })
        })
        .collect()
}
