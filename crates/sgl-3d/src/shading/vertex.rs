//! Vertex buffer layouts derived from the Rust structs the buffers hold: the
//! shadow casters' (`CasterVertex`) and every scene geometry draw's instances
//! (`DrawInstance`).

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
/// position, from its mesh's position buffer or a deforming instance's
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
/// scene's object buffer, and the drawn mesh's record in the scene source. A draw of many instances reaches
/// each one's record through its entry, as Bevy reaches each instance's
/// `MeshUniform` from its instance index.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawInstance {
    pub object: u32,
    pub mesh: u32,
}

/// Its attributes follow `CasterVertex`'s position, at location 0.
pub(crate) const DRAW_INSTANCE_LAYOUT: VertexLayout =
    vertex_layout!(DrawInstance, Instance, 1, [object, mesh]);

/// The vertex buffer every geometry pipeline reads `DrawInstance`s from.
pub(crate) const DRAW_INSTANCE_SLOT: u32 = 0;
/// The vertex buffer shadow casters read their positions from.
pub(crate) const CASTER_SLOT: u32 = 1;
/// The vertex buffers of scene geometry pipelines, in slot order: the camera
/// and probe passes read the first, which pulls the rest of each vertex from
/// the scene source; shadow casters read both.
pub(crate) const GEOMETRY_BUFFERS: [wgpu::VertexBufferLayout<'static>; 2] =
    [DRAW_INSTANCE_LAYOUT.buffer, CASTER_LAYOUT.buffer];
