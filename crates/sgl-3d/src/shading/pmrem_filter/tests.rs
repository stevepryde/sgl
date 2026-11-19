use super::{Atlas, PMREM_FILTER};
use crate::shading::Module;
use crate::test_support;

/// Each query's texel direction (the writer's) read back through
/// `pmrem_bilinear` (the reader) from an atlas that holds each texel's own
/// coordinates.
static ROUNDTRIP: Module = Module {
    name: "pmrem_roundtrip_test",
    source: r"
@group(0) @binding(0) var map:texture_2d_array<f32>;
@group(0) @binding(1) var nearest:sampler;
// Face size, face, texel x and y within the face.
@group(0) @binding(2) var<storage,read> queries:array<vec4<u32>>;
@group(0) @binding(3) var<storage,read> mips:array<f32>;
@group(0) @binding(4) var<storage,read_write> found:array<vec2<f32>>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&queries) {
  return;
 }
 let q=queries[id.x];
 let direction=pmrem_texel_direction(q.zw,q.y,q.x);
 found[id.x]=pmrem_bilinear(map,nearest,direction,mips[id.x]).xy;
}
",
    deps: &[&PMREM_FILTER],
};

// Plausible defects: the writer and the reader of the atlas disagreeing on
// where a level or a face lies, on a face's orientation (a flipped or
// mirrored face, two faces swapped) or on the one-texel border, so a sky's
// atlas is read from the wrong texels. The oracle is a roundtrip through an
// independent implementation: pmrem_bilinear, the reader every map's atlas
// already goes through (Three's bilinearCubeUV), must find at the direction
// the writer gives a texel exactly that texel, for interior texels of every
// face of every level of the 128-texel atlas. (A border texel's direction
// lies on a neighbouring face, whose edge it duplicates.)
#[test]
fn every_texel_the_writer_fills_is_where_the_reader_looks() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let atlas = Atlas { cube: 128 };
    let [width, height] = atlas.size();
    let texels: Vec<[f32; 4]> = (0..height)
        .flat_map(|y| (0..width).map(move |x| [x as f32, y as f32, 0., 1.]))
        .collect();
    let map = test_support::hdr_texture(&device, &queue, [width, height], &texels);
    let mut queries = Vec::new();
    let mut mips = Vec::new();
    let mut expected = Vec::new();
    for level in 0..atlas.levels() {
        let placed = atlas.level(level);
        let size = placed.size;
        for face in 0..6 {
            for texel in [
                [1, 1],
                [size - 2, 1],
                [1, size - 2],
                [size - 2, size - 2],
                [size / 2, 3],
                [5, size / 2 + 1],
            ] {
                queries.push([size, face, texel[0], texel[1]]);
                mips.push(atlas.mip(level));
                let corner = placed.face_origin(face);
                expected.push([(corner[0] + texel[0]) as f32, (corner[1] + texel[1]) as f32]);
            }
        }
    }
    let program = crate::shading::compose(&[&ROUNDTRIP]);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pmrem roundtrip"),
        source: wgpu::ShaderSource::Wgsl(program.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    use wgpu::util::DeviceExt;
    let storage = |contents: &[u8], usage| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents,
            usage: wgpu::BufferUsages::STORAGE | usage,
        })
    };
    let queries_buffer = storage(bytemuck::cast_slice(&queries), wgpu::BufferUsages::empty());
    let mips_buffer = storage(bytemuck::cast_slice(&mips), wgpu::BufferUsages::empty());
    let found = storage(&vec![0; expected.len() * 8], wgpu::BufferUsages::COPY_SRC);
    let nearest = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&map.texture().create_view(
                    &wgpu::TextureViewDescriptor {
                        dimension: Some(wgpu::TextureViewDimension::D2Array),
                        ..Default::default()
                    },
                )),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&nearest),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: queries_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: mips_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: found.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups((queries.len() as u32).div_ceil(64), 1, 1);
    }
    queue.submit([encoder.finish()]);
    let words = test_support::read_words(&device, &queue, &found);
    for (index, expected) in expected.iter().enumerate() {
        let found = [
            f32::from_bits(words[index * 2]),
            f32::from_bits(words[index * 2 + 1]),
        ];
        assert_eq!(
            found, *expected,
            "query {:?} (level mip {}) reads the wrong texel",
            queries[index], mips[index]
        );
    }
}
