//! The scene's packed vertex (the architecture's Vertex encoding): how the ray
//! source keeps a game's `asset::Vertex` in eight words. This module encodes;
//! `packed_vertex.wgsl`, its twin, decodes.
//!
//! Everything but the position follows Godot b130438's attribute compression
//! (`servers/rendering/rendering_server.cpp`, `_surface_set_data` and
//! `_get_axis_angle` under `ARRAY_FLAG_COMPRESS_ATTRIBUTES`; MIT, see
//! LICENSE-godot.txt), with these departures: positions stay `f32`; every
//! value rounds to the nearest step, where Godot's casts truncate; the
//! frame's axis and angle come from its unit quaternion rather than
//! `Basis::get_axis_angle`, which loses precision approaching 0° and 180°;
//! an absent tangent is Duff et al.'s orthonormal vector (glam's
//! `any_orthonormal_vector`), where Godot's arbitrary one vanishes for
//! normals along (1, 1, -1); UVs span each mesh's rectangle rather than a
//! box about zero; and the colour is sRGB-encoded.
use crate::content::asset::{Vertex, tangent_frame};
use glam::{DMat3, DQuat, DVec2, DVec3, Vec2, Vec3};

/// One vertex as the ray source keeps it (the `PACKED_VERTEX_*` words of
/// packed_vertex.wgsl). Each 16-bit pair holds its first value in the low
/// half.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct PackedVertex {
    pub position: [f32; 3],
    /// The axis of the frame's rotation, octahedral, as two unorm16s.
    pub axis: u32,
    /// The rotation's angle as a unorm16 whose half holds the bitangent's
    /// handedness (low half), and the lightmap chart's index in its model's
    /// table (high half).
    pub angle_chart: u32,
    /// The UV across its mesh's rectangle (`UvRect`), as two unorm16s.
    pub uv: u32,
    /// RGBA8: the colour sRGB-encoded, the alpha linear.
    pub color: u32,
    /// The lightmap UV, as two unorm16s; (0, 0) is unassigned.
    pub lightmap_uv: u32,
}

/// The rectangle a mesh's UVs span, which its packed UVs are fractions of:
/// `min + fraction * extent`, as packed_vertex.wgsl's `vec4` (min in xy,
/// extent in zw).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UvRect {
    pub min: [f32; 2],
    pub extent: [f32; 2],
}

impl UvRect {
    /// The rectangle `vertices`' UVs span; empty for no vertices.
    pub fn of(vertices: &[Vertex]) -> Self {
        let Some((min, max)) = vertices
            .iter()
            .map(|vertex| Vec2::from_array(vertex.uv))
            .fold(None, |bounds: Option<(Vec2, Vec2)>, uv| {
                Some(bounds.map_or((uv, uv), |(min, max)| (min.min(uv), max.max(uv))))
            })
        else {
            return Self::default();
        };
        Self {
            min: min.to_array(),
            extent: (max - min).to_array(),
        }
    }

    /// As packed_vertex.wgsl reads it.
    pub fn words(&self) -> [f32; 4] {
        [self.min[0], self.min[1], self.extent[0], self.extent[1]]
    }
}

/// `value`, clamped to 0..=1, rounded to the nearest of `steps` + 1 steps.
fn unorm(value: f64, steps: f64) -> u32 {
    (value.clamp(0., 1.) * steps).round() as u32
}

fn unorm16_pair(value: DVec2) -> u32 {
    unorm(value.x, 65535.) | unorm(value.y, 65535.) << 16
}

/// Unit `v` on Godot's octahedron, in 0..=1 on each axis
/// (`Vector3::octahedron_encode`, `core/math/vector3.cpp`).
fn octahedral(v: DVec3) -> DVec2 {
    let n = v / (v.x.abs() + v.y.abs() + v.z.abs());
    let sign = |x: f64| if x >= 0. { 1. } else { -1. };
    let o = if n.z >= 0. {
        DVec2::new(n.x, n.y)
    } else {
        DVec2::new((1. - n.y.abs()) * sign(n.x), (1. - n.x.abs()) * sign(n.y))
    };
    o * 0.5 + 0.5
}

/// The `axis` and `angle_chart` words (`chart` aside) of a vertex's frame:
/// the rotation whose matrix rows are its tangent, its bitangent, the
/// normal × tangent, and its normal, as Godot's `Basis` holds the frame in
/// `_get_axis_angle`, with the handedness in the angle's half as Godot
/// encodes it. The tangent is projected onto the normal's plane first; an
/// absent one (`tangent_frame`) is any unit vector in that plane, of
/// handedness +1. The normal must be finite and not zero.
fn frame(vertex: &Vertex) -> (u32, u32) {
    let normal = DVec3::from_array(vertex.normal.map(f64::from))
        .try_normalize()
        .expect("a packed vertex's normal is finite and not zero");
    let (tangent, positive) = if tangent_frame(vertex) {
        let tangent = Vec3::from_slice(&vertex.tangent[..3]).as_dvec3();
        (
            (tangent - normal * normal.dot(tangent)).normalize(),
            vertex.tangent[3] > 0.,
        )
    } else {
        (normal.any_orthonormal_vector(), true)
    };
    let rows = DMat3::from_cols(tangent, normal.cross(tangent), normal).transpose();
    let mut rotation = DQuat::from_mat3(&rows);
    if rotation.w < 0. {
        rotation = -rotation;
    }
    let (axis, angle) = rotation.to_axis_angle();
    // Godot's halves: (angle / PI) / 2 above one half for +1, mirrored below
    // it for -1, neither ever one half itself.
    let half = angle / std::f64::consts::PI * 0.5;
    let code = if positive {
        unorm(half + 0.5, 65535.).max(32768)
    } else {
        unorm(0.5 - half, 65535.).min(32767)
    };
    (unorm16_pair(octahedral(axis)), code)
}

/// The sRGB encoding of linear `value` in 0..=1.
fn srgb(value: f64) -> f64 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}

/// `vertex`, whose normal is finite and not zero, packed with its UV across
/// `uv` and its lightmap chart `chart` in its model's table.
pub(crate) fn pack(vertex: &Vertex, uv: &UvRect, chart: u16) -> PackedVertex {
    let (axis, angle) = frame(vertex);
    let fraction = |value: f32, min: f32, extent: f32| {
        if extent > 0. {
            (f64::from(value) - f64::from(min)) / f64::from(extent)
        } else {
            0.
        }
    };
    let [r, g, b, a] = vertex.color.map(|channel| f64::from(channel).clamp(0., 1.));
    let lightmap = Vec2::from_array(vertex.lightmap_uv);
    PackedVertex {
        position: vertex.position,
        axis,
        angle_chart: angle | u32::from(chart) << 16,
        uv: unorm16_pair(DVec2::new(
            fraction(vertex.uv[0], uv.min[0], uv.extent[0]),
            fraction(vertex.uv[1], uv.min[1], uv.extent[1]),
        )),
        color: unorm(srgb(r), 255.)
            | unorm(srgb(g), 255.) << 8
            | unorm(srgb(b), 255.) << 16
            | unorm(a, 255.) << 24,
        lightmap_uv: if lightmap.cmplt(Vec2::ZERO).any() {
            0
        } else {
            unorm16_pair(lightmap.as_dvec2())
        },
    }
}

/// The WGSL twins of the packed vertex's words, which the layout test
/// compares with packed_vertex.wgsl.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use std::mem::{offset_of, size_of};
    [
        ("PACKED_VERTEX_WORDS", size_of::<PackedVertex>()),
        ("PACKED_VERTEX_POSITION", offset_of!(PackedVertex, position)),
        ("PACKED_VERTEX_AXIS", offset_of!(PackedVertex, axis)),
        (
            "PACKED_VERTEX_ANGLE_CHART",
            offset_of!(PackedVertex, angle_chart),
        ),
        ("PACKED_VERTEX_UV", offset_of!(PackedVertex, uv)),
        ("PACKED_VERTEX_COLOR", offset_of!(PackedVertex, color)),
        (
            "PACKED_VERTEX_LIGHTMAP_UV",
            offset_of!(PackedVertex, lightmap_uv),
        ),
    ]
    .into_iter()
    .map(|(name, bytes)| {
        crate::shading::layout_tests::Constant::new(
            "packed_vertex",
            name,
            naga::Literal::U32(u32::try_from(bytes / 4).unwrap()),
        )
    })
    .collect()
}
