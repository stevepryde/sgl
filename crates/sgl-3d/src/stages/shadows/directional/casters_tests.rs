//! What a GPU-built cascade's casters draw, against the CPU-built casters
//! probe captures keep.
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::shading::uniforms::{FrameUniform, ViewUniform};
use crate::stages::cull::Cull;
use crate::view::draw_list::gpu::camera_cull;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::pipelines::{GeometryPass, GeometryPipelines, LayerConstants};
use crate::view::population::Population;
use crate::view::{FrameViews, View};
use crate::{InstanceState, Mobility, Scene, test_support};
use bytemuck::Zeroable;
use glam::{Mat4, Vec3, camera};

/// The cascade's texels a side.
const TEXELS: u32 = 256;
/// The positions slabs' largest size here: 174,762 vertices.
const SLAB_BYTES: u64 = 2 << 20;

/// A `cells`² grid of shared vertices across `side` metres about the
/// origin in x and z, its heights a hash of each vertex's cell, so the
/// casters' depths vary across it.
fn grid(cells: u32, side: f32, seed: u32, material: usize) -> CpuMesh {
    let mut vertices = Vec::new();
    for row in 0..=cells {
        for column in 0..=cells {
            let hash = (row * 7919 + column * 104_729 + seed * 1_299_709) % 1000;
            let at = |cell: u32| (cell as f32 / cells as f32 - 0.5) * side;
            vertices.push(Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [at(column), hash as f32 * 1e-3, at(row)],
                normal: [0., 1., 0.],
                uv: [column as f32 / cells as f32, row as f32 / cells as f32],
                color: [1.; 4],
            });
        }
    }
    let mut indices = Vec::new();
    let corner = |row: u32, column: u32| row * (cells + 1) + column;
    for row in 0..cells {
        for column in 0..cells {
            let [a, b, c, d] = [
                corner(row, column),
                corner(row + 1, column),
                corner(row + 1, column + 1),
                corner(row, column + 1),
            ];
            indices.extend([a, b, c, a, c, d]);
        }
    }
    CpuMesh {
        vertices,
        indices,
        material,
        deformation: Default::default(),
    }
}

/// `mesh` with each pair of triangles, (a, b, c) and (a, c, d), given as
/// (a, b, c) and (c, d, a): the same triangles, which do not pair
/// (`SECTION_PAIRED`).
fn unpaired(mut mesh: CpuMesh) -> CpuMesh {
    for pair in mesh.indices.chunks_exact_mut(6) {
        pair.copy_from_slice(&[pair[0], pair[1], pair[2], pair[2], pair[5], pair[0]]);
    }
    mesh
}

/// `mesh` with a lone last triangle of its own, 2 m above its first corner.
fn with_lone_triangle(mut mesh: CpuMesh) -> CpuMesh {
    let first = mesh.vertices.len() as u32;
    let corner = Vec3::from_array(mesh.vertices[0].position) + Vec3::Y * 2.;
    for offset in [Vec3::ZERO, Vec3::Z, Vec3::X] {
        mesh.vertices.push(Vertex {
            position: (corner + offset).to_array(),
            ..mesh.vertices[0]
        });
    }
    mesh.indices.extend([first, first + 1, first + 2]);
    mesh
}

/// An asset of three grids, `seed`'s: a large single-sided opaque one whose
/// triangles pair, with a lone last triangle; a small double-sided one
/// masked over `test_support::half_cut_out`, whose triangles pair; and a
/// single-sided opaque one whose triangles do not.
fn terrain(seed: u32) -> Asset {
    let mut asset = test_support::masked(test_support::cube(), 0.5);
    let mut opaque = asset.materials[0].clone();
    opaque.alpha = crate::AlphaMode::Opaque;
    opaque.base_texture = None;
    opaque.double_sided = false;
    asset.materials.push(opaque);
    asset.meshes = vec![
        with_lone_triangle(grid(200, 9., seed, 1)),
        grid(90, 6., seed + 1, 0),
        unpaired(grid(60, 3., seed + 2, 1)),
    ];
    asset
}

// Plausible defects: a GPU-built cascade's caster reading another mesh's
// positions (the cull carrying no first vertex, or another mesh's), reading
// at the wrong stride, reading another slab's (sets not keyed by their slab,
// or the executor binding one set's slab for another's), or a slab's group
// left over its buffer from before the slab grew, so meshes placed since
// read stale positions. The oracle is the CPU builder that probe captures
// keep, an independent path to the same positions: the cascade's casters
// drawn indexed through the vertex fetch from the same slabs, unculled.
// Culling drops only what lies outside the cascade, so the two depth maps
// agree texel for texel. The scene's slabs are small, so its models, 52,406
// vertices each, fill two: the first slab grows at the second and third
// models, and the fourth model's large grid starts the second slab while
// its smaller ones fit the first's last room. One instance mirrors, so its
// sets differ. The opaque sets' sections whose triangles pair draw
// indexed, the rest pulled, the masked set's all; the large grid's last
// section is its lone triangle.
#[test]
fn a_gpu_built_cascade_draws_the_depth_its_cpu_built_casters_draw() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    scene.geometry.limit_positions(SLAB_BYTES);
    for (index, x) in [-15., -5., 5., 15.].into_iter().enumerate() {
        let model = scene
            .add_asset(&device, &queue, terrain(index as u32))
            .unwrap()
            .model;
        let mirror = if index == 2 { -1. } else { 1. };
        let pose = Mat4::from_translation(Vec3::new(x, 0., 0.))
            * Mat4::from_scale(Vec3::new(mirror, 1., 1.))
            * Mat4::from_rotation_x(0.3 * index as f32);
        let state = InstanceState {
            model,
            pose,
            visible: true,
            capture_visible: true,
        };
        scene
            .add_instance(&device, &queue, state, Mobility::Static)
            .unwrap();
    }
    assert_eq!(
        scene.geometry.positions_slabs(),
        2,
        "the models fill two slabs"
    );
    scene.prepare_frame(&device, &queue, Vec3::ZERO);

    // A light shining straight down onto all four, the cascade's x across
    // 40 m and depth across 20 m.
    let light = camera::rh::view::look_at_mat4(Vec3::new(0., 10., 0.), Vec3::ZERO, Vec3::NEG_Z);
    let cascade = camera::rh::proj::directx::orthographic(-20., 20., -6., 6., 20., 0.) * light;
    let mask = u32::MAX;
    let mut views = FrameViews::new(&device);
    // The cull stage encodes the camera's list with the cascades': a camera
    // over the same ground, whose list nothing here draws.
    let camera = View::camera(ViewUniform {
        view: light.to_cols_array_2d(),
        projection: crate::perspective(1., 1., 0.1).to_cols_array_2d(),
        ..ViewUniform::zeroed()
    });
    views.camera.list.prepare(
        &device,
        &queue,
        &scene,
        camera_cull(&camera, [TEXELS; 2], mask, true),
        false,
    );
    crate::stages::prepare::set_cascades(
        (&device, &queue),
        &scene,
        &mut views,
        mask,
        std::iter::once(cascade),
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    Cull::new(&device).encode(&device, &mut encoder, &scene, &views, None);
    queue.submit([encoder.finish()]);

    let mut pipelines = GeometryPipelines::new(
        &device,
        &crate::shading::bind::lit(
            &device,
            crate::shading::bind::BindingTier::of(&device.limits()),
        ),
        [
            &crate::shading::bind::shadow(&device),
            &crate::shading::bind::scene(&device),
            &crate::shading::bind::blended(&device),
            &crate::shading::bind::shadow_mask(&device),
            &crate::shading::bind::caster_positions(&device),
        ],
        LayerConstants::ALL,
    );
    pipelines.specialise(&device, LayerConstants::ALL, &scene, false);
    let frame = crate::scene::buffer(
        &device,
        "frame",
        bytemuck::bytes_of(&FrameUniform::zeroed()),
        wgpu::BufferUsages::UNIFORM,
    );
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cascade"),
        layout: &crate::shading::bind::shadow(&device),
        entries: &crate::shading::bind::uniforms(&views.cascades[0].buffer, &frame),
    });
    let depth = |draw: &dyn Fn(&mut wgpu::RenderPass<'_>)| -> Vec<f32> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cascade"),
            size: wgpu::Extent3d {
                width: TEXELS,
                height: TEXELS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cascade"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &group, &[]);
            draw(&mut pass);
        }
        queue.submit([encoder.finish()]);
        bytemuck::cast_slice(&test_support::read(&device, &queue, &texture, 4)).to_vec()
    };
    let gpu_built = depth(&|pass| {
        views.cascades[0]
            .list
            .draw(&scene, &pipelines, pass, GeometryPass::DirectionalShadow);
    });
    let mut captured = DrawList::default();
    let mut instances = DrawInstances::default();
    captured.build(
        &mut instances,
        &scene,
        &View::shadow_cascade(cascade),
        Some(mask),
        Population::CaptureShadow,
    );
    instances.upload(&device, &queue);
    let cpu_built = depth(&|pass| {
        captured.draw(
            &scene,
            &pipelines,
            &instances,
            pass,
            GeometryPass::CaptureShadow,
        );
    });
    let covered = cpu_built.iter().filter(|&&depth| depth > 0.).count();
    assert!(
        covered > (TEXELS * TEXELS) as usize / 4,
        "the casters cover the cascade: {covered} texels"
    );
    let differing = gpu_built
        .iter()
        .zip(&cpu_built)
        .filter(|(gpu, cpu)| gpu.to_bits() != cpu.to_bits())
        .count();
    assert_eq!(
        differing, 0,
        "texels where the GPU-built cascade's depth differs from the CPU-built casters'"
    );
    let ([pulled, paired], _) =
        crate::view::draw_list::gpu::read_early(&views.cascades[0].list, &device, &queue, &scene);
    assert!(
        !pulled.is_empty() && !paired.is_empty(),
        "the cascade drew sections of both kinds"
    );
}
