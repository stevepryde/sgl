//! Vertex attributes and morph displacements read as floats: the one step
//! where a stored integer becomes its value, for every attribute SGL3D
//! reads through it.
//!
//! glTF 2.0 (3.7.2.1, 3.7.2.2) and `KHR_mesh_quantization` (KhronosGroup/glTF
//! 70b5e41 `extensions/2.0/Khronos/KHR_mesh_quantization/README.md`) allow
//! integer components. A normalized one converts by the extension's
//! int-to-float equations: `f = max(c / 127.0, -1.0)` (BYTE), `f = c / 255.0`
//! (UNSIGNED_BYTE), `f = max(c / 32767.0, -1.0)` (SHORT) and `f = c / 65535.0`
//! (UNSIGNED_SHORT); an unnormalized one is its integer value ("unnormalized
//! integer `2` corresponds to `2.0`"). As three.js r187's `GLTFLoader` does,
//! the integers are only converted here: the dequantisation scale and offset
//! the extension puts in a node's transform or a skin's inverse bind matrices
//! apply where those already apply, so they apply once.
//!
//! gltf 1.4.1's mesh readers do not honour `normalized`: `read_positions`,
//! `read_normals`, `read_tangents` and `read_morph_targets` read any bytes as
//! `f32`, and `read_tex_coords` panics on BYTE and SHORT. A FLOAT accessor
//! reads through the same `accessor::Iter<[f32; N]>` they use.
use gltf::accessor::DataType::{F32, I8, I16, U8, U16, U32};
use gltf::accessor::{DataType, Item, Iter};

/// The component types a `NORMAL` or `TANGENT`, or a morph displacement of
/// one, may hold: FLOAT, or BYTE or SHORT normalized ([`check_direction`];
/// `KHR_mesh_quantization`'s tables).
pub(super) const DIRECTION_TYPES: &[DataType] = &[F32, I8, I16];

/// `accessor`'s elements as floats, or `None` where gltf reads no elements
/// (an accessor with neither a buffer view nor sparse values). `check_accessor`
/// has checked it, and its component type is one of glTF's attribute types.
pub(super) fn read<const N: usize>(
    accessor: &gltf::Accessor<'_>,
    buffers: &[gltf::buffer::Data],
) -> Option<Vec<[f32; N]>>
where
    [f32; N]: Item,
    [i8; N]: Item,
    [u8; N]: Item,
    [i16; N]: Item,
    [u16; N]: Item,
{
    let normalized = accessor.normalized();
    match accessor.data_type() {
        F32 => elements(accessor, buffers).map(Iterator::collect),
        I8 if normalized => convert(accessor, buffers, |c: i8| (f32::from(c) / 127.0).max(-1.0)),
        I8 => convert(accessor, buffers, |c: i8| f32::from(c)),
        U8 if normalized => convert(accessor, buffers, |c: u8| f32::from(c) / 255.0),
        U8 => convert(accessor, buffers, |c: u8| f32::from(c)),
        I16 if normalized => convert(accessor, buffers, |c: i16| {
            (f32::from(c) / 32767.0).max(-1.0)
        }),
        I16 => convert(accessor, buffers, |c: i16| f32::from(c)),
        U16 if normalized => convert(accessor, buffers, |c: u16| f32::from(c) / 65535.0),
        U16 => convert(accessor, buffers, |c: u16| f32::from(c)),
        U32 => unreachable!("check_accessor refuses UNSIGNED_INT vertex attributes"),
    }
}

/// `primitive`'s `semantic` attribute as floats ([`read`]), `None` where
/// it has none.
pub(super) fn attribute<const N: usize>(
    primitive: &gltf::Primitive<'_>,
    semantic: gltf::Semantic,
    buffers: &[gltf::buffer::Data],
) -> Option<Vec<[f32; N]>>
where
    [f32; N]: Item,
    [i8; N]: Item,
    [u8; N]: Item,
    [i16; N]: Item,
    [u16; N]: Item,
{
    primitive
        .get(&semantic)
        .and_then(|accessor| read(&accessor, buffers))
}

/// Refuses an integer `NORMAL` or `TANGENT`, or a morph displacement of
/// one, that is not normalized, which `KHR_mesh_quantization` does not
/// allow. `what` names its use.
pub(super) fn check_direction(accessor: &gltf::Accessor<'_>, what: &str) -> Result<(), String> {
    if accessor.data_type() != F32 && !accessor.normalized() {
        return Err(format!(
            "{what} accessor {} holds unnormalized {:?}, where glTF allows FLOAT or normalized I8 or I16; re-export valid glTF",
            accessor.index(),
            accessor.data_type()
        ));
    }
    Ok(())
}

fn elements<'s, T: Item>(
    accessor: &gltf::Accessor<'_>,
    buffers: &'s [gltf::buffer::Data],
) -> Option<Iter<'s, T>> {
    Iter::new(accessor.clone(), |buffer: gltf::Buffer<'_>| {
        Some(&buffers[buffer.index()].0[..])
    })
}

fn convert<T: Copy, const N: usize>(
    accessor: &gltf::Accessor<'_>,
    buffers: &[gltf::buffer::Data],
    value: impl Fn(T) -> f32,
) -> Option<Vec<[f32; N]>>
where
    [T; N]: Item,
{
    elements::<[T; N]>(accessor, buffers)
        .map(|elements| elements.map(|element| element.map(&value)).collect())
}
