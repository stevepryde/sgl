//! A ray hit's map coordinates (specs/sgl3d-architecture.md, Material map
//! coordinates): its second UV set as the hit decodes it, each map
//! binding's set and transform as the record carries them to
//! `scene_map_uv`, and the cut-out test's. Oracles are the labelled
//! image's known texels (`magnified`) at coordinates computed by hand.
use super::sampler_tests::{MAP_COUNT, TEST_MAP, map_field, run};
use crate::asset::{
    Asset, CpuMesh, Material, MaterialTexture, Sampler, TextureFilter, UvSet, UvTransform, Vertex,
};
use crate::test_support::{labelled, magnified};

/// A square over x and y in -1..1 at z = 0 facing +Z, as two triangles:
/// its first UV set runs from (0, 0) at (-1, 1) to (1, 1) at (1, -1), its
/// second from `second.0` to `second.1`.
fn square(second: ([f32; 2], [f32; 2])) -> CpuMesh {
    let vertices = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
        .map(|(x, y): (f32, f32)| {
            let share = [(x + 1.) * 0.5, (1. - y) * 0.5];
            Vertex {
                position: [x, y, 0.],
                normal: [0., 0., 1.],
                uv: share,
                uv1: [0, 1]
                    .map(|axis| second.0[axis] + share[axis] * (second.1[axis] - second.0[axis])),
                color: [1.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                ..bytemuck::Zeroable::zeroed()
            }
        })
        .to_vec();
    CpuMesh {
        vertices,
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

fn asset(mesh: CpuMesh, material: Material, image: crate::asset::Image) -> Asset {
    Asset {
        meshes: vec![mesh],
        materials: vec![material],
        images: vec![image],
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    }
}

/// Traces each probe's ray down -Z from z = 3 and writes its hit's base
/// colour's red and emission map's green, each where its binding samples,
/// and 1; -1 for a miss.
const TRACE: &str = r#"
@group(0) @binding(0) var<storage,read> probes:array<vec4<f32>>;
@group(0) @binding(1) var<storage,read_write> colors:array<vec4<f32>>;
@compute @workgroup_size(1) fn sample(@builtin(global_invocation_id) id:vec3<u32>) {
 let origin=vec3(probes[id.x].xy,3.);
 let direction=vec3(0.,0.,-1.);
 let raw=scene_trace_nearest(SceneRay(vec4(origin,0.),vec4(direction,10.)),SCENE_SIDES_AS_RASTER);
 let hit=scene_decode_hit(raw,origin,direction);
 if !hit.hit {
  colors[id.x]=vec4(-1.);
  return;
 }
 let m=scene_material(hit.material_word);
 let base=scene_base_color(m,hit.uv,hit.uv1,vec4(1.));
 let emission=scene_sample_map(m.values.map_samplers,m.textures[SCENE_TEXTURE_EMISSION],MATERIAL_SLOT_EMISSION,scene_map_uv(m,MATERIAL_SLOT_EMISSION,hit.uv,hit.uv1),true);
 colors[id.x]=vec4(base.r,emission.g,0.,1.);
}
"#;

/// The hits of rays down -Z at each of `points` over `asset`.
fn trace(asset: &Asset, points: &[[f32; 2]]) -> Option<Vec<[f32; 4]>> {
    run(
        asset,
        &crate::shading::SCENE_RAYS_PORTABLE,
        TRACE,
        |_| {
            points
                .iter()
                .flat_map(|&[x, y]| [x, y, 0., 0.].map(f32::to_bits))
                .collect()
        },
        points.len(),
    )
}

fn nearest() -> Sampler {
    Sampler {
        mag_filter: TextureFilter::Nearest,
        ..Sampler::default()
    }
}

// Plausible defects: a hit's second UV set not decoded, read from the first
// set's words or rectangle, or interpolated from other vertices; a
// transform not reaching the ray record, read from another binding's rows
// or applied to the wrong set. The square's base map is on the second set
// through KHR_texture_transform's first worked example, which reads the
// image at (0.5 v, 1 - 0.5 u), and its emission map on the first set
// through the second example, at (u, 1 - v). Points fall in both of the
// square's triangles; the oracle is the labelled image's texel at each.
#[test]
fn a_hit_reads_each_map_at_its_slots_set_through_its_transform() {
    let second = ([0.2, 0.1], [0.8, 0.9]);
    let material = Material {
        base_texture: Some(MaterialTexture {
            sampler: nearest(),
            uv_set: UvSet::Uv1,
            transform: UvTransform {
                offset: [0., 1.],
                rotation: std::f32::consts::FRAC_PI_2,
                scale: [0.5, 0.5],
            },
            ..MaterialTexture::new(0)
        }),
        emissive_texture: Some(MaterialTexture {
            sampler: nearest(),
            transform: UvTransform {
                offset: [0., 1.],
                rotation: 0.,
                scale: [1., -1.],
            },
            ..MaterialTexture::new(0)
        }),
        ..Default::default()
    };
    let points = [[-0.7, 0.55], [0.3, 0.35], [-0.15, -0.6], [0.72, -0.8]];
    let Some(colors) = trace(&asset(square(second), material, labelled()), &points) else {
        return;
    };
    for ([x, y], color) in points.into_iter().zip(colors) {
        let [s, t] = [(x + 1.) * 0.5, (1. - y) * 0.5];
        let [u1, v1] = [
            second.0[0] + s * (second.1[0] - second.0[0]),
            second.0[1] + t * (second.1[1] - second.0[1]),
        ];
        let expected = [
            magnified([0.5 * v1, 1. - 0.5 * u1], nearest()),
            magnified([s, 1. - t], nearest()),
        ];
        assert!(
            (color[0] - expected[0]).abs() < 1e-3 && (color[1] - expected[1]).abs() < 1e-3,
            "the hit at {x}, {y}: {color:?}, expected {expected:?}"
        );
    }
}

// A material on its first UV set alone leaves a hit's second set
// undecoded: a mesh may still carry one (another material's, or a game's),
// and the hit's maps must not read it. Plausible defect: the hit decode or
// the resolution keyed on the mesh having a second set rather than on the
// slot. Oracle: the first set's texel.
#[test]
fn a_hit_of_a_first_set_material_reads_the_first_set() {
    let material = Material {
        base_texture: Some(MaterialTexture {
            sampler: nearest(),
            ..MaterialTexture::new(0)
        }),
        ..Default::default()
    };
    let points = [[-0.7, 0.55], [0.72, -0.8]];
    let mesh = square(([5., 7.], [6., 9.]));
    let Some(colors) = trace(&asset(mesh, material, labelled()), &points) else {
        return;
    };
    for ([x, y], color) in points.into_iter().zip(colors) {
        let expected = magnified([(x + 1.) * 0.5, (1. - y) * 0.5], nearest());
        assert!(
            (color[0] - expected).abs() < 1e-3,
            "the hit at {x}, {y}: {}, expected {expected}",
            color[0]
        );
    }
}

// A ray passes a masked surface where its base map's slot cuts it out (the
// acceptance predicate of every trace, as raster's discard). Plausible
// defects: the predicate sampling the first UV set whatever the slot
// names, or skipping its transform. The base map (`half_cut_out`: its left
// half cut out) is on the second set, which holds the square's coordinates
// mirrored in U, through a transform that mirrors them back (u' = 1 - u);
// the first set lies wholly in the opaque half. Rays through the square's
// left half pass and through its right half hit; read from the first set
// all hit, and from the second untransformed the halves swap.
#[test]
fn a_ray_passes_a_masked_surface_where_its_base_map_cuts_it_out() {
    let mut mesh = square(([1., 0.], [0., 1.]));
    for vertex in &mut mesh.vertices {
        vertex.uv = [0.9, 0.5];
    }
    let material = Material {
        alpha: crate::AlphaMode::Mask { cutoff: 0.5 },
        base_texture: Some(MaterialTexture {
            uv_set: UvSet::Uv1,
            transform: UvTransform {
                offset: [1., 0.],
                rotation: 0.,
                scale: [-1., 1.],
            },
            ..MaterialTexture::new(0)
        }),
        ..Default::default()
    };
    let image = crate::asset::Image::Rgba8(crate::test_support::half_cut_out());
    let points = [[-0.6, 0.4], [-0.2, -0.7], [0.3, 0.6], [0.8, -0.5]];
    let Some(colors) = trace(&asset(mesh, material, image), &points) else {
        return;
    };
    for ([x, y], color) in points.into_iter().zip(colors) {
        let hit = if x > 0. { 1. } else { -1. };
        assert_eq!(color[3], hit, "the ray at {x}, {y}");
    }
}

/// Samples each probe's map where its binding samples at a point whose UV
/// sets are the probe's, as a ray hit resolves it.
const RESOLVE: &str = r#"
struct Probe { material:u32, map:u32, uv:vec2<f32>, uv1:vec2<f32> }
@group(0) @binding(0) var<storage,read> probes:array<Probe>;
@group(0) @binding(1) var<storage,read_write> colors:array<vec4<f32>>;
@compute @workgroup_size(1) fn sample(@builtin(global_invocation_id) id:vec3<u32>) {
 let probe=probes[id.x];
 let m=scene_material(probe.material);
 let map=test_map(m,probe.map);
 colors[id.x]=scene_sample_map(m.values.map_samplers,map.x,map.y,scene_map_uv(m,map.y,probe.uv,probe.uv1),true);
}
"#;

// Every map binding carries its own set and transform to a ray hit.
// Plausible defects: a binding's bit or rows taken from its neighbour's
// (the record's bits shifted, the rows indexed by texture word rather than
// binding), a transform read past the first few bindings wrongly, or the
// set and the transform halves of the bits swapped. Each of the fifteen
// maps a hit reads samples the labelled image on the first set (even
// maps) or the second (odd), moved by its own whole texels: map k by
// (k mod 4, k / 4) quarters of the image, none for map 0. The oracle is the
// texel at its set's coordinates plus its offset; no two maps on a set
// read the same texel.
#[test]
fn every_map_binding_carries_its_own_set_and_transform_to_ray_hits() {
    let (first, second) = ([0.1, 0.1], [0.6, 0.35]);
    let slot = |map: usize| MaterialTexture {
        sampler: nearest(),
        uv_set: if map.is_multiple_of(2) {
            UvSet::Uv0
        } else {
            UvSet::Uv1
        },
        transform: UvTransform {
            offset: [(map % 4) as f32 * 0.25, (map / 4) as f32 * 0.25],
            ..UvTransform::IDENTITY
        },
        ..MaterialTexture::new(0)
    };
    let mut material = Material::default();
    for map in 0..MAP_COUNT {
        *map_field(&mut material, map) = Some(slot(map));
    }
    let mesh = square(([0.; 2], [1.; 2]));
    let Some(colors) = run(
        &asset(mesh, material, labelled()),
        &crate::shading::SCENE_RAYS,
        &format!("{TEST_MAP}{RESOLVE}"),
        |fixture| {
            (0..MAP_COUNT as u32)
                .flat_map(|map| {
                    [fixture.materials[0][0], map]
                        .into_iter()
                        .chain(first.into_iter().chain(second).map(f32::to_bits))
                })
                .collect()
        },
        MAP_COUNT,
    ) else {
        return;
    };
    for (map, color) in colors.iter().enumerate() {
        let slot = slot(map);
        let at = if slot.uv_set == UvSet::Uv0 {
            first
        } else {
            second
        };
        let expected = magnified(
            [
                at[0] + slot.transform.offset[0],
                at[1] + slot.transform.offset[1],
            ],
            nearest(),
        );
        assert!(
            (color[0] - expected).abs() < 1e-4,
            "map {map}: {} for {expected}",
            color[0]
        );
    }
}
