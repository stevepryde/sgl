//! Sprites' GPU layouts (`sprite.wgsl`): one record per sprite, a batch's
//! look, the batch's group and a view's order of a blended batch, and the
//! normal a view orders a batch's sprites along.
use crate::content::sprite::{Sprite, SpriteBatch, SpriteBlend, SpriteFacing, SpriteSizing};
use glam::{Mat4, Vec3};

/// `sprite.wgsl`, the facing and corner maths.
pub(crate) static SPRITE: super::Module = super::Module {
    name: "sprite",
    source: include_str!("sprite.wgsl"),
    deps: &[],
};

/// One sprite as the vertex stage reads it (`SpriteRecord`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SpriteRecord {
    pub position: [f32; 3],
    pub rotation: f32,
    pub size: [f32; 2],
    pub pivot: [f32; 2],
    pub region: [f32; 4],
    pub tint: [f32; 4],
}

impl SpriteRecord {
    pub fn new(sprite: &Sprite) -> Self {
        Self {
            position: sprite.position.to_array(),
            rotation: sprite.rotation,
            size: sprite.size.to_array(),
            pivot: sprite.pivot.to_array(),
            region: [
                sprite.region.min[0],
                sprite.region.min[1],
                sprite.region.max[0],
                sprite.region.max[1],
            ],
            tint: sprite.tint,
        }
    }
}

/// A batch's look as its draws read it (`SpriteBatch`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SpriteBatchUniform {
    pub axis: [f32; 3],
    pub flags: u32,
    pub cutoff: f32,
    pub padding: [f32; 3],
}

/// `SPRITE_FIXED_SIZE`.
const FIXED_SIZE: u32 = 1;

impl SpriteBatchUniform {
    pub fn new(batch: &SpriteBatch) -> Self {
        Self {
            axis: match batch.facing {
                SpriteFacing::Camera => [0.; 3],
                SpriteFacing::Axis(axis) => axis.normalize().to_array(),
            },
            flags: match batch.sizing {
                SpriteSizing::World => 0,
                SpriteSizing::Fixed => FIXED_SIZE,
            },
            cutoff: match batch.blend {
                SpriteBlend::Blended => 0.,
                SpriteBlend::Masked { cutoff } => cutoff,
            },
            padding: [0.; 3],
        }
    }
}

/// The bindings of a batch's group (`SPRITE_DRAW`'s group 1).
pub(crate) mod group {
    pub const BATCH: u32 = 0;
    pub const RECORDS: u32 = 1;
    pub const IMAGE: u32 = 2;
    pub const SAMPLER: u32 = 3;
    /// A blended batch's order in a view, in its own group 2.
    pub const ORDER: u32 = 0;
}

fn storage(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A batch's group: its look, its records, its image and its sampler.
pub(crate) fn batch_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sprite batch"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: group::BATCH,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            storage(group::RECORDS),
            wgpu::BindGroupLayoutEntry {
                binding: group::IMAGE,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: group::SAMPLER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// A view's order of a blended batch: the index of the sprite each
/// instance draws, back to front.
pub(crate) fn order_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sprite order"),
        entries: &[storage(group::ORDER)],
    })
}

/// A batch of `facing`'s sprite axes in the world, right then up, for the
/// view whose world-to-view matrix is `view_matrix`: `sprite.wgsl`'s
/// `sprite_axes`, which places the corners, for the CPU's ordering of a
/// batch; the GPU's alone draws.
pub(crate) fn facing_axes(view_matrix: &Mat4, facing: SpriteFacing) -> [Vec3; 2] {
    let right = view_matrix.row(0).truncate();
    let up = view_matrix.row(1).truncate();
    let SpriteFacing::Axis(axis) = facing else {
        return [right, up];
    };
    let back = view_matrix.row(2).truncate();
    let axis = axis.normalize();
    let handed = right.cross(up).dot(back).signum();
    let turned = axis.cross(back) * handed;
    let turned = if turned.length_squared() < 1e-12 {
        right - axis * axis.dot(right)
    } else {
        turned
    };
    [turned.normalize(), axis]
}

/// The normal of every sprite's plane of a batch of `facing` in the view
/// whose world-to-view matrix is `view_matrix`, on the view's side: its
/// sprites are parallel quads, back to front in ascending order of
/// `order_key`.
pub(crate) fn facing_normal(view_matrix: &Mat4, facing: SpriteFacing) -> Vec3 {
    let [right, up] = facing_axes(view_matrix, facing);
    let back = view_matrix.row(2).truncate();
    let normal = right.cross(up).normalize();
    if normal.dot(back) < 0. {
        -normal
    } else {
        normal
    }
}

/// Where a sprite at `position` falls in a batch's back-to-front order in a
/// view at `eye` whose sprites' planes have normal `normal` toward it
/// (`facing_normal`): along the normal under an orthographic projection,
/// whose rays are parallel, and, under a perspective one, by its plane's
/// distance from the eye, farthest first, since a ray from the eye meets
/// parallel planes on one side of it in order of their distance and never
/// planes on both sides. A camera-facing batch's planes all lie ahead of
/// a perspective eye, where both orders agree, so its order needs no eye
/// (`perspective` and the facing decide `eye_matters`).
pub(crate) fn order_key(position: Vec3, normal: Vec3, eye: Vec3, eye_matters: bool) -> f32 {
    if eye_matters {
        -(position - eye).dot(normal).abs()
    } else {
        position.dot(normal)
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "sprites",
            "SpriteRecord",
            SpriteRecord,
            [position, rotation, size, pivot, region, tint]
        ),
        mirror!(
            "sprites",
            "SpriteBatch",
            SpriteBatchUniform,
            [axis, flags, cutoff]
        ),
    ]
}
