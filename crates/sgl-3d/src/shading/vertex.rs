//! Vertex buffer layouts derived from the Rust structs the buffers hold: the
//! shadow casters' (`CasterVertex`) and every scene geometry draw's instances
//! (`DrawInstance`); and the glow's vertex record (`GlowVertex`).
use crate::content::transient::{Glow, GlowKind};

/// A type a vertex attribute holds, and the format the shader reads it in.
pub(crate) trait Attribute {
    const FORMAT: wgpu::VertexFormat;
}

impl Attribute for u32 {
    const FORMAT: wgpu::VertexFormat = wgpu::VertexFormat::Uint32;
}
impl Attribute for f32 {
    const FORMAT: wgpu::VertexFormat = wgpu::VertexFormat::Float32;
}
impl Attribute for [f32; 2] {
    const FORMAT: wgpu::VertexFormat = wgpu::VertexFormat::Float32x2;
}
impl Attribute for [f32; 3] {
    const FORMAT: wgpu::VertexFormat = wgpu::VertexFormat::Float32x3;
}
impl Attribute for [f32; 4] {
    const FORMAT: wgpu::VertexFormat = wgpu::VertexFormat::Float32x4;
}

/// The format of the field `field` selects.
pub(crate) const fn format<S, T: Attribute>(_field: fn(&S) -> &T) -> wgpu::VertexFormat {
    T::FORMAT
}

/// A vertex buffer's layout, derived from the type the buffer holds.
pub(crate) struct VertexLayout {
    pub buffer: wgpu::VertexBufferLayout<'static>,
    /// The field each attribute reads, in `buffer.attributes`' order.
    #[cfg(test)]
    pub fields: &'static [&'static str],
}

/// `vertex_layout!(Struct, [field, ...])`: a buffer of `#[repr(C)]` `Struct`s
/// stepped per vertex, whose named fields are its attributes at locations 0
/// onward, each at its offset and in its type's format.
/// `vertex_layout!(Struct, step, first, [field, ...])` steps it by `step`
/// (`Vertex` or `Instance`) with its attributes at locations `first` onward.
macro_rules! vertex_layout {
    ($struct:ty, [$($field:ident),* $(,)?]) => {
        $crate::shading::vertex::vertex_layout!($struct, Vertex, 0, [$($field),*])
    };
    ($struct:ty, $step:ident, $first:expr, [$($field:ident),* $(,)?]) => {
        $crate::shading::vertex::VertexLayout {
            buffer: wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<$struct>() as u64,
                step_mode: wgpu::VertexStepMode::$step,
                attributes: &{
                    let mut attributes = [$(wgpu::VertexAttribute {
                        format: $crate::shading::vertex::format(|vertex: &$struct| &vertex.$field),
                        offset: std::mem::offset_of!($struct, $field) as u64,
                        shader_location: 0,
                    }),*];
                    let mut index = 0;
                    while index < attributes.len() {
                        attributes[index].shader_location = $first + index as u32;
                        index += 1;
                    }
                    attributes
                },
            },
            #[cfg(test)]
            fields: &[$(stringify!($field)),*],
        }
    };
}
pub(crate) use vertex_layout;

/// What a shadow caster reads of a vertex from a vertex buffer: its
/// position, from its mesh's range of a positions slab (`scene::geometry`)
/// or a deforming instance's
/// deformed positions (`shading::deformation`). A masked material's casters
/// pull its texel coordinates and colour from the scene source. Every camera
/// and probe pass pulls whole vertices from the scene source instead
/// (`scene_source_vertex`), never a vertex buffer (`GeometryPass::pulled`).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CasterVertex {
    pub position: [f32; 3],
}

pub(crate) const CASTER_LAYOUT: VertexLayout = vertex_layout!(CasterVertex, [position]);

/// One instance of a scene geometry draw, in the frame's draw instances
/// (`view::draw_list::DrawInstances`), which every geometry pipeline steps
/// per instance at vertex buffer `DRAW_INSTANCE_SLOT` (`DrawInstance` in
/// bind_scene.wgsl): the index of the instance's object record in the
/// scene's object buffer, the drawn mesh's record in the scene source, and
/// the base vertex an indexed draw of it adds to its indices (its first
/// vertex in its positions slab, `scene::geometry`, zero for a deforming
/// instance's own positions), which a caster that reads the source's vertex
/// records by vertex index subtracts, as Bevy b56fc29's
/// `MeshUniform::first_vertex_index` (crates/bevy_pbr/src/render/mesh.rs)
/// is subtracted in `morph_vertex` (mesh.wgsl). A draw of many instances
/// reaches each one's record through its entry, as Bevy reaches each
/// instance's `MeshUniform` from its instance index.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawInstance {
    pub object: u32,
    pub mesh: u32,
    pub first_vertex: u32,
}

/// Its attributes follow `CasterVertex`'s position, at location 0.
pub(crate) const DRAW_INSTANCE_LAYOUT: VertexLayout =
    vertex_layout!(DrawInstance, Instance, 1, [object, mesh, first_vertex]);

/// The vertex buffer every geometry pipeline reads `DrawInstance`s from.
pub(crate) const DRAW_INSTANCE_SLOT: u32 = 0;
/// The vertex buffer shadow casters read their positions from.
pub(crate) const CASTER_SLOT: u32 = 1;
/// The vertex buffers of scene geometry pipelines, in slot order: the camera
/// and probe passes read the first, which pulls the rest of each vertex from
/// the scene source; shadow casters read both.
pub(crate) const GEOMETRY_BUFFERS: [wgpu::VertexBufferLayout<'static>; 2] =
    [DRAW_INSTANCE_LAYOUT.buffer, CASTER_LAYOUT.buffer];

/// One glow vertex (`Glow`) as the scene's glow buffer holds it, which
/// `glow_vs` (stages/transparent/glow.wgsl) reads: its kind as a `GLOW_*`
/// value, and the values of that kind, zero for the others.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct GlowVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
    pub kind: u32,
    pub soft_distance: f32,
    /// `GlowKind::Tapered`'s coordinates and profile.
    pub uv: [f32; 2],
    pub taper: f32,
    pub ripple_frequency: [f32; 2],
    pub ripple_amplitude: f32,
    /// `GlowKind::Line`'s other endpoint and offset across it.
    pub other: [f32; 3],
    pub offset: f32,
}

/// `GlowVertex::kind`: `GlowKind::Uniform`, `Tapered` and `Line`.
pub(crate) const GLOW_UNIFORM: u32 = 0;
pub(crate) const GLOW_TAPERED: u32 = 1;
pub(crate) const GLOW_LINE: u32 = 2;

impl GlowVertex {
    pub fn new(glow: &Glow) -> Self {
        let vertex = Self {
            position: glow.position,
            color: glow.color,
            kind: GLOW_UNIFORM,
            soft_distance: glow.soft_distance,
            ..bytemuck::Zeroable::zeroed()
        };
        match glow.kind {
            GlowKind::Uniform => vertex,
            GlowKind::Tapered { uv, profile } => Self {
                kind: GLOW_TAPERED,
                uv,
                // In range, so the ripple never takes alpha below zero, which
                // would subtract light under the additive blend.
                taper: profile.taper.max(0.),
                ripple_frequency: profile.ripple_frequency,
                ripple_amplitude: profile.ripple_amplitude.clamp(0., 0.5),
                ..vertex
            },
            GlowKind::Line { other, offset } => Self {
                kind: GLOW_LINE,
                other,
                offset,
                ..vertex
            },
        }
    }
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 2] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        Constant::new("glow", "GLOW_TAPERED", U32(GLOW_TAPERED)),
        Constant::new("glow", "GLOW_LINE", U32(GLOW_LINE)),
    ]
}
