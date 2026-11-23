//! The bounds and shapes every accessor is checked against before the gltf
//! crate's readers read it.

use gltf::accessor::{DataType, Dimensions};

use super::super::asset::Result;

/// Refuses an accessor the gltf crate's readers cannot read, which its
/// validation lets through and its readers (gltf 1.4.1 `mesh::Reader`,
/// `skin::Reader`, `animation::util::Reader` and `accessor::util::Iter`)
/// assume away, panicking or misreading otherwise: one holding a component
/// type or shape glTF 2.0 does not allow for its use (`types`, `shapes`),
/// one of no elements, and one whose elements or sparse substitutions do
/// not lie within their buffer views at a stride no narrower than an
/// element, whose views do not lie within their buffers, or whose sparse
/// indices are not strictly increasing and below its count (glTF 2.0
/// 3.6.2.3). `what` names its use.
///
/// It also refuses one whose element count its use does not allow, before
/// gltf's readers allocate that many, so what each use reads stays in
/// proportion to the file's data: `count` is the count glTF requires (the
/// primitive's vertices, the skin's joints, the sampler's keyframes),
/// itself fixed by data or the document, or None where nothing fixes it.
/// An accessor without a buffer view (a sparse one) declares its count
/// without data; where nothing fixes that count, its elements may take no
/// more bytes than the file's buffers hold (`buffer_bytes`, their total),
/// the bound its elements would meet in a buffer view.
pub(super) fn check_accessor(
    accessor: &gltf::Accessor<'_>,
    what: &str,
    types: &[DataType],
    shapes: &[Dimensions],
    count: Option<usize>,
    (buffers, buffer_bytes): (&[gltf::buffer::Data], usize),
) -> Result<()> {
    let index = accessor.index();
    let (data_type, shape) = (accessor.data_type(), accessor.dimensions());
    if !types.contains(&data_type) || !shapes.contains(&shape) {
        return Err(format!("{what} accessor {index} holds {data_type:?} {shape:?}, where glTF allows {types:?} {shapes:?}; re-export valid glTF").into());
    }
    let sparse = accessor.sparse();
    if accessor.count() == 0 || sparse.as_ref().is_some_and(|sparse| sparse.count() == 0) {
        return Err(
            format!("{what} accessor {index} has no elements; re-export valid glTF").into(),
        );
    }
    let elements = accessor.count();
    if let Some(count) = count
        && elements != count
    {
        return Err(format!("{what} accessor {index} has {elements} elements where its use needs {count}; re-export valid glTF").into());
    }
    if count.is_none()
        && accessor.view().is_none()
        && elements
            .checked_mul(accessor.size())
            .is_none_or(|bytes| bytes > buffer_bytes)
    {
        return Err(format!("{what} accessor {index} has {elements} elements without a buffer view, more than the file's {buffer_bytes} buffer bytes would hold; re-export valid glTF").into());
    }
    let size = accessor.size();
    let within = accessor
        .view()
        .is_none_or(|view| fits(&view, accessor.offset(), accessor.count(), size))
        && sparse.as_ref().is_none_or(|sparse| {
            let (indices, values) = (sparse.indices(), sparse.values());
            fits(
                &indices.view(),
                indices.offset(),
                sparse.count(),
                indices.index_type().size(),
            ) && fits(&values.view(), values.offset(), sparse.count(), size)
        });
    if !within {
        return Err(format!(
            "{what} accessor {index} does not fit its buffer view and buffer; re-export valid glTF"
        )
        .into());
    }
    if let Some(sparse) = sparse {
        use gltf::accessor::sparse::IndexType;
        use gltf::accessor::util::{ItemIter, SparseIndicesIter};

        let indices = sparse.indices();
        let view = indices.view();
        let stride = view.stride().unwrap_or(indices.index_type().size());
        // `fits` proved these offsets and every index are inside the
        // imported buffer. Use the same typed iterator and stride as gltf's
        // sparse reader, which otherwise silently drops invalid indices.
        let bytes = &buffers[view.buffer().index()].0
            [view.offset() + indices.offset()..view.offset() + view.length()];
        let indices = match indices.index_type() {
            IndexType::U8 => SparseIndicesIter::U8(ItemIter::new(bytes, stride)),
            IndexType::U16 => SparseIndicesIter::U16(ItemIter::new(bytes, stride)),
            IndexType::U32 => SparseIndicesIter::U32(ItemIter::new(bytes, stride)),
        };
        let mut previous = None;
        for sparse_index in indices.take(sparse.count()) {
            if sparse_index as usize >= elements
                || previous.is_some_and(|previous| sparse_index <= previous)
            {
                return Err(format!("{what} accessor {index} has sparse indices that are not strictly increasing and below its {elements} elements; re-export valid glTF").into());
            }
            previous = Some(sparse_index);
        }
    }
    Ok(())
}

/// Whether `count` elements of `size` bytes from `offset` lie within `view`
/// at its stride, an element's when it has none, as gltf's
/// `accessor::util::Iter::new` reads them, and `view` within its buffer.
fn fits(view: &gltf::buffer::View<'_>, offset: usize, count: usize, size: usize) -> bool {
    let stride = view.stride().unwrap_or(size);
    in_buffer(view)
        && stride >= size
        && count
            .checked_sub(1)
            .and_then(|last| last.checked_mul(stride))
            .and_then(|start| start.checked_add(offset))
            .and_then(|start| start.checked_add(size))
            .is_some_and(|end| end <= view.length())
}

/// Whether `view` lies within its buffer, which gltf slices it from
/// unchecked (gltf 1.4.1 `accessor::util::buffer_view_slice` and
/// `image::Data::from_source` add its offset and length unchecked, and the
/// latter indexes the buffer's data with them). `import_buffers` holds each
/// buffer's data to at least its declared length.
pub(super) fn in_buffer(view: &gltf::buffer::View<'_>) -> bool {
    view.offset()
        .checked_add(view.length())
        .is_some_and(|end| end <= view.buffer().length())
}
