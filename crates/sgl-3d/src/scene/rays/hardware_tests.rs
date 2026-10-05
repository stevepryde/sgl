//! The hardware trace (the architecture's Hardware ray tracing,
//! *Validation*) on a device with ray queries, against a brute-force CPU
//! oracle that tests every world triangle with f64 planes and edges: no
//! BVH, no TLAS, no Moller-Trumbore solve and none of the shaders' code.
//! The scene is built, deformed and its structures built as a renderer's
//! prepare builds them, then rays go through the scene ray function set the
//! hardware form's query module composes, under each form the device's
//! backend runs (`forms`): the baseline on every one, the candidate form
//! where the backend lowers its loop (Vulkan, DX12), whichever the
//! renderer takes by default. Each test reports itself unsupported, never
//! passed, where the adapter has no ray queries; the candidate form has run
//! on no device yet (#23).
use super::{Function, Query};
use crate::asset::{Asset, CpuMesh, Material, Vertex};
use crate::content::identity::Identity;
use crate::shading::RayQueryForm;
use crate::stages::deform::Deform;
use crate::test_support;
use crate::{AlphaMode, InstanceState, Mobility, Scene};
use glam::{DMat4, DVec3, Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

const AS_RASTER: u32 = 0;
const BOTH: u32 = 1;

/// A vertex at `position` facing `normal`, with alpha `alpha`.
fn vertex(position: Vec3, normal: Vec3, alpha: f32) -> Vertex {
    Vertex {
        tangent: [0.; 4],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
        position: position.to_array(),
        normal: normal.to_array(),
        uv: [0.; 2],
        color: [1., 1., 1., alpha],
    }
}

/// A square of side `size` about `centre`, facing `facing` (an axis): its
/// two triangles wound counter-clockwise seen from that side, of material
/// `material`, its corners' alphas `alphas` in winding order.
fn square(centre: Vec3, size: f32, facing: Vec3, material: usize, alphas: [f32; 4]) -> CpuMesh {
    let u = if facing.x.abs() > 0.5 {
        Vec3::new(0., facing.x, 0.)
    } else if facing.y.abs() > 0.5 {
        Vec3::new(0., 0., facing.y)
    } else {
        Vec3::new(facing.z, 0., 0.)
    };
    let v = facing.cross(u);
    let half = size / 2.;
    let corners = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)];
    CpuMesh {
        vertices: corners
            .iter()
            .zip(alphas)
            .map(|(&(a, b), alpha)| vertex(centre + (u * a + v * b) * half, facing, alpha))
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

/// A square every corner of which is opaque.
fn solid(centre: Vec3, size: f32, facing: Vec3, material: usize) -> CpuMesh {
    square(centre, size, facing, material, [1.; 4])
}

/// A material of `alpha` mode, sided as `double_sided` says, in visibility
/// group `group`.
fn material(alpha: AlphaMode, double_sided: bool, group: u32) -> Material {
    let mut material = test_support::cube().materials[0].clone();
    material.alpha = alpha;
    material.double_sided = double_sided;
    material.visibility_group = group;
    material.base = [1.; 4];
    material
}

fn opaque() -> Material {
    material(AlphaMode::Opaque, false, 0)
}

fn blended() -> Material {
    material(
        AlphaMode::Blend {
            receives_screen_space_reflections: false,
        },
        false,
        0,
    )
}

fn masked() -> Material {
    material(AlphaMode::Mask { cutoff: 0.5 }, false, 0)
}

fn asset(meshes: Vec<CpuMesh>, materials: Vec<Material>) -> Asset {
    Asset {
        meshes,
        materials,
        ..test_support::cube()
    }
}

/// An instance the test placed: its entry's index, its model, its content,
/// pose and kind, and the joint matrix that deforms it whole, if it deforms.
struct Placed {
    index: u32,
    model: crate::ModelId,
    asset: Asset,
    pose: Mat4,
    moving: bool,
    joint: Option<Mat4>,
}

/// Adds `asset` and an instance of it at `pose`, deformed whole by `joint`
/// where it deforms; its record.
fn place(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    mut asset: Asset,
    pose: Mat4,
    mobility: Mobility,
    joint: Option<Mat4>,
) -> Placed {
    if joint.is_some() {
        for mesh in &mut asset.meshes {
            mesh.deformation.influences = vec![
                crate::deformation::Influence {
                    joints: [0; 4],
                    weights: [1., 0., 0., 0.],
                };
                mesh.vertices.len()
            ];
        }
    }
    let model = scene.add_asset(device, queue, asset.clone()).unwrap().model;
    let id = scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose,
                ..InstanceState::new(model)
            },
            mobility,
        )
        .unwrap();
    if let Some(joint) = joint {
        scene
            .set_instance_deformation(queue, id, &[joint], &[])
            .unwrap();
    }
    Placed {
        index: Identity::index(id) as u32,
        model,
        asset,
        pose,
        moving: mobility == Mobility::Moving,
        joint,
    }
}

/// What the oracle makes of a triangle a ray meets.
#[derive(Clone, Copy, Debug)]
enum Rule {
    /// It stops rays: on both sides where double-sided, else on its front.
    Solid { double_sided: bool },
    /// It stops them, as `Solid`, where its alpha (its corners' alphas
    /// interpolated, times the material's) reaches `cutoff`.
    Masked { double_sided: bool, cutoff: f32 },
}

/// One world triangle: its instance's entry, mesh and triangle; its
/// corners in index order, their alphas and normals; its authored front
/// (its object-space winding carried to the world, mirrored poses undone);
/// whether it moves; and its rule.
struct Triangle {
    id: (u32, u32, u32),
    corners: [DVec3; 3],
    alphas: [f64; 3],
    normals: [DVec3; 3],
    front: DVec3,
    moving: bool,
    rule: Rule,
}

/// Every triangle of `placed` that rays may stop at under the visibility
/// mask `mask`: none of a blended material, or of a group the mask hides.
fn triangles(placed: &[Placed], mask: u32) -> Vec<Triangle> {
    let mut triangles = Vec::new();
    for instance in placed {
        let pose = DMat4::from_cols_array(&instance.pose.to_cols_array().map(f64::from));
        let joint = instance.joint.map_or(DMat4::IDENTITY, |joint| {
            DMat4::from_cols_array(&joint.to_cols_array().map(f64::from))
        });
        let world = pose * joint;
        let normal_matrix = world.inverse().transpose();
        for (mesh_id, mesh) in instance.asset.meshes.iter().enumerate() {
            let material = &instance.asset.materials[mesh.material];
            let rule = match material.alpha {
                AlphaMode::Blend { .. } => continue,
                _ if material.visibility_group & mask != material.visibility_group => continue,
                AlphaMode::Opaque => Rule::Solid {
                    double_sided: material.double_sided,
                },
                AlphaMode::Mask { cutoff } => Rule::Masked {
                    double_sided: material.double_sided,
                    cutoff,
                },
            };
            for (triangle, indices) in mesh.indices.chunks_exact(3).enumerate() {
                let vertices = [0, 1, 2].map(|i| &mesh.vertices[indices[i] as usize]);
                let corners = vertices.map(|vertex| {
                    world.transform_point3(DVec3::from_array(vertex.position.map(f64::from)))
                });
                let winding = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
                triangles.push(Triangle {
                    id: (instance.index, mesh_id as u32, triangle as u32),
                    corners,
                    alphas: vertices.map(|vertex| f64::from(vertex.color[3] * material.base[3])),
                    normals: vertices.map(|vertex| {
                        normal_matrix
                            .transform_vector3(DVec3::from_array(vertex.normal.map(f64::from)))
                            .normalize()
                    }),
                    front: if world.determinant() < 0. {
                        -winding
                    } else {
                        winding
                    },
                    moving: instance.moving,
                    rule,
                });
            }
        }
    }
    triangles
}

/// The kinds of instances a ray takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kinds {
    All,
    Static,
    Moving,
}

/// A triangle a ray met within its interval: the triangle, its distance
/// and identity, the barycentrics of its second and third corners, and
/// whether the ray's rules accept it.
#[derive(Clone, Copy, Debug)]
struct Met {
    triangle: usize,
    t: f64,
    id: (u32, u32, u32),
    barycentrics: [f64; 2],
    accepted: bool,
}

/// How far from an edge, a cut-out boundary or the ray's plane a meeting
/// must lie for f32 arithmetic to decide it as f64 does.
const MARGIN: f64 = 1e-4;

/// Every triangle of `triangles` but `receiver` that `ray` meets within its
/// interval, its end excluded where `open_end`, nearest first, each judged
/// by the ray's `sides` (AS_RASTER rejects a single-sided back face); none
/// for a ray whose meetings f32 arithmetic could decide otherwise (near an
/// edge, a cut-out boundary or grazing).
fn meet(
    triangles: &[Triangle],
    ray: [f32; 8],
    (sides, kinds, open_end): (u32, Kinds, bool),
    receiver: Option<(u32, u32, u32)>,
) -> Option<Vec<Met>> {
    let origin = DVec3::new(ray[0].into(), ray[1].into(), ray[2].into());
    let direction = DVec3::new(ray[4].into(), ray[5].into(), ray[6].into());
    let (t_min, t_max) = (f64::from(ray[3]), f64::from(ray[7]));
    let mut met = Vec::new();
    for (index, triangle) in triangles.iter().enumerate() {
        if receiver == Some(triangle.id) {
            continue;
        }
        let wanted = match kinds {
            Kinds::All => true,
            Kinds::Static => !triangle.moving,
            Kinds::Moving => triangle.moving,
        };
        if !wanted {
            continue;
        }
        let [a, b, c] = triangle.corners;
        let normal = (b - a).cross(c - a);
        let denominator = normal.dot(direction);
        if denominator.abs() < MARGIN * normal.length() * direction.length() {
            if denominator != 0. {
                return None;
            }
            continue;
        }
        let t = normal.dot(a - origin) / denominator;
        let point = origin + direction * t;
        let area = normal.length_squared();
        let weight = |p: DVec3, q: DVec3| (q - p).cross(point - p).dot(normal) / area;
        let bary = [weight(b, c), weight(c, a), weight(a, b)];
        if bary.iter().any(|w| w.abs() < MARGIN) {
            return None;
        }
        if bary.iter().any(|&w| w < 0.) {
            continue;
        }
        if (t - t_min).abs() < MARGIN || (t - t_max).abs() < MARGIN {
            return None;
        }
        let end = if open_end { t >= t_max } else { t > t_max };
        if t < t_min || end {
            continue;
        }
        let front = triangle.front.dot(direction) < 0.;
        let (double_sided, cutoff) = match triangle.rule {
            Rule::Solid { double_sided } => (double_sided, None),
            Rule::Masked {
                double_sided,
                cutoff,
            } => (double_sided, Some(f64::from(cutoff))),
        };
        let mut accepted = front || sides == BOTH || double_sided;
        if let Some(cutoff) = cutoff {
            let alpha: f64 = (0..3).map(|i| bary[i] * triangle.alphas[i]).sum();
            if (alpha - cutoff).abs() < MARGIN * 10. {
                return None;
            }
            accepted &= alpha >= cutoff;
        }
        met.push(Met {
            triangle: index,
            t,
            id: triangle.id,
            barycentrics: [bary[1], bary[2]],
            accepted,
        });
    }
    met.sort_by(|a, b| a.t.total_cmp(&b.t));
    Some(met)
}

/// Distances the oracle and the hardware agree on.
const T_TOLERANCE: f64 = 1e-4;

/// The answers the architecture allows a nearest ray that met `met`: the
/// accepted meetings at the nearest accepted distance, and, where a
/// rejected meeting lies at exactly that distance, those at the next
/// accepted distance too, since one opaque query cannot list ties and the
/// re-trace steps past the rejected one; a miss where every accepted
/// distance is so tied.
fn allowed(met: &[Met]) -> (Vec<Met>, bool) {
    let mut allowed = Vec::new();
    let mut accepted = met.iter().filter(|met| met.accepted).peekable();
    while let Some(&first) = accepted.peek() {
        let t = first.t;
        let group: Vec<Met> = met
            .iter()
            .filter(|other| other.accepted && (other.t - t).abs() < T_TOLERANCE)
            .copied()
            .collect();
        allowed.extend(&group);
        let tied = met
            .iter()
            .any(|other| !other.accepted && (other.t - t).abs() < T_TOLERANCE);
        if !tied {
            return (allowed, false);
        }
        while accepted
            .peek()
            .is_some_and(|next| (next.t - t).abs() < T_TOLERANCE)
        {
            accepted.next();
        }
    }
    (allowed, true)
}

/// A batch of rays through one function, accepting one side policy and
/// leaving one receiver (as `Query::bind` takes it).
struct Batch<'a> {
    function: Function,
    sides: u32,
    receiver: [u32; 2],
    rays: &'a [[f32; 8]],
}

/// The hardware forms `device`'s backend runs: the baseline on every one,
/// and the candidate form where its shader backend lowers a candidate loop
/// (`RayQueryForm::lowered`).
fn forms(device: &wgpu::Device) -> Vec<RayQueryForm> {
    let mut forms = vec![RayQueryForm::Baseline];
    if RayQueryForm::lowered(device.adapter_info().backend) {
        forms.push(RayQueryForm::Candidates);
    }
    forms
}

/// Waits for the queue's work, failing rather than waiting forever.
fn wait(device: &wgpu::Device) {
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        })
        .expect("the GPU finished the frame");
}

/// One frame of `scene` seen from `eye` with the groups `mask` shows, as a
/// renderer's prepare and deform pass run it while hardware ray tracing is
/// in effect in `form`, then each of `batches` traced through `query`, the
/// dispatch over that form: each batch's hits.
fn frame(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    (query, deform, form): (&Query, &mut Deform, RayQueryForm),
    mask: u32,
    batches: &[Batch<'_>],
) -> Vec<Vec<[u32; 8]>> {
    let eye = Vec3::new(0., 0., 10.);
    scene.prepare_frame(device, queue, eye);
    scene
        .prepare_acceleration_structures(device, queue, eye, form)
        .expect("the device holds a TLAS");
    scene.update_rays(device, queue, mask, true);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut encoder = device.create_command_encoder(&Default::default());
    deform.dispatch(device, queue, &mut encoder, scene, None);
    scene.encode_acceleration_structures(&mut encoder);
    let tlas = scene
        .acceleration_structures()
        .expect("structures were built")
        .tlas()
        .0;
    let readbacks: Vec<_> = batches
        .iter()
        .map(|batch| {
            let rays = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("hardware oracle rays"),
                contents: bytemuck::cast_slice(batch.rays),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let size = rays.size();
            let hits = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hardware oracle hits"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hardware oracle readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let bindings = query.bind(
                device,
                (&rays, &hits),
                (batch.function, batch.sides, batch.receiver),
                Some(tlas),
            );
            query.trace(
                &mut encoder,
                &scene.scene_group,
                &bindings,
                batch.rays.len() as u32,
            );
            encoder.copy_buffer_to_buffer(&hits, 0, &readback, 0, size);
            readback
        })
        .collect();
    queue.submit([encoder.finish()]);
    scene.finish_frame();
    for readback in &readbacks {
        readback.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    }
    wait(device);
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
    readbacks
        .iter()
        .map(|readback| {
            let hits = bytemuck::cast_slice(&readback.get_mapped_range(..)).to_vec();
            readback.unmap();
            hits
        })
        .collect()
}

/// Asserts a nearest ray's hardware `hit` is one the architecture allows
/// for what the ray `met`.
fn assert_nearest(hit: &[u32; 8], met: &[Met], label: &str) {
    let (allowed, miss) = allowed(met);
    if hit[0] == 0 {
        assert!(miss, "{label}: missed; the oracle allows {allowed:?}");
        return;
    }
    let t = f64::from(f32::from_bits(hit[4]));
    let barycentrics = [f32::from_bits(hit[5]), f32::from_bits(hit[6])].map(f64::from);
    let id = (hit[1], hit[2], hit[3]);
    let matched = allowed.iter().find(|met| met.id == id);
    let Some(met) = matched else {
        panic!("{label}: hit {id:?} at {t}; the oracle allows {allowed:?}, miss {miss}");
    };
    assert!(
        (met.t - t).abs() < T_TOLERANCE,
        "{label}: hit {id:?} at {t}, the oracle's at {}",
        met.t
    );
    for (actual, expected) in barycentrics.iter().zip(met.barycentrics) {
        assert!(
            (actual - expected).abs() < 1e-3,
            "{label}: barycentrics {barycentrics:?}, the oracle's {:?}",
            met.barycentrics
        );
    }
}

/// Asserts a visibility ray's hardware answer (`visible`) is one the
/// architecture allows for what the ray `met`: occluded where it met an
/// accepted triangle, unless every accepted distance is tied with a
/// rejected triangle's.
fn assert_visible(visible: bool, met: &[Met], label: &str) {
    let (allowed, miss) = allowed(met);
    if visible {
        assert!(miss, "{label}: visible past {allowed:?}");
    } else {
        assert!(
            !allowed.is_empty(),
            "{label}: occluded; the oracle met {met:?}"
        );
    }
}

// The smallest hardware trace, first: one triangle, one ray, under each
// form. Plausible defects: the query reading another instance, mesh or
// triangle than the hardware committed (custom index, geometry or
// primitive), barycentrics in another vertex order than the portable
// solve's, or a committed hit mistaken for a miss. The oracle is the
// triangle's plane and edges in f64.
#[test]
fn one_ray_meets_one_triangle() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    for form in forms(&device) {
        let mut scene = Scene::new(&device, &queue);
        let mut mesh = solid(Vec3::new(0.2, 0.1, -2.), 2., Vec3::Z, 0);
        mesh.indices.truncate(3);
        let placed = [place(
            gpu,
            &mut scene,
            asset(vec![mesh], vec![opaque()]),
            Mat4::IDENTITY,
            Mobility::Static,
            None,
        )];
        let query = Query::with_path(&device, Some(form));
        let mut deform = Deform::new(&device);
        let ray = [0.3, -0.2, 3., 0., 0.01, 0.02, -1., 100.];
        let hits = frame(
            gpu,
            &mut scene,
            (&query, &mut deform, form),
            0,
            &[Batch {
                function: Function::Nearest,
                sides: AS_RASTER,
                receiver: [0; 2],
                rays: &[ray],
            }],
        );
        let met = meet(
            &triangles(&placed, 0),
            ray,
            (AS_RASTER, Kinds::All, false),
            None,
        )
        .unwrap();
        assert_eq!(met.len(), 1, "the ray meets the triangle");
        assert_nearest(&hits[0][0], &met, &format!("{form:?}: one ray"));
    }
}

/// Six squares of half-size `half` about `centre`, each facing out: a
/// closed single-sided box of material 0.
fn closed_box(centre: Vec3, half: f32) -> Vec<CpuMesh> {
    [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ]
    .map(|facing| solid(centre + facing * half, half * 2., facing, 0))
    .to_vec()
}

/// The oracle scene: a wall behind everything, and before it a single-sided
/// square seen from its back and a double-sided one seen from its back; a
/// model whose blended square hides an opaque
/// one; an all-blended square; a hidden group's square before an opaque
/// one; a masked square (a predicate instance under the baseline form, in
/// the TLAS under the candidate form); a mirrored moving square; a
/// moving square; two instances of one square at one pose (a tie both
/// accept); one model's two coplanar squares wound opposite ways (a tie a
/// single-sided rule splits); two nested closed boxes; and a deforming
/// instance of an opaque and a masked square, horizontal at rest and
/// turned upright by its joint. Rays come from the camera, from inside the
/// inner box and from below. Each is placed in `scene`.
fn oracle_scene(gpu: (&wgpu::Device, &wgpu::Queue), scene: &mut Scene) -> Vec<Placed> {
    let gradient = [0., 1., 1., 1.];
    let mut placed = Vec::new();
    let mut add = |asset, pose, mobility, joint| {
        placed.push(place(gpu, scene, asset, pose, mobility, joint));
    };
    let at = Mat4::from_translation;
    let one = |mesh| asset(vec![mesh], vec![opaque()]);
    add(
        one(solid(Vec3::new(0., 0., -6.), 16., Vec3::Z, 0)),
        Mat4::IDENTITY,
        Mobility::Static,
        None,
    );
    add(
        one(solid(Vec3::ZERO, 2.5, Vec3::NEG_Z, 0)),
        at(Vec3::new(-4., 3., 0.)),
        Mobility::Static,
        None,
    );
    add(
        asset(
            vec![
                solid(Vec3::new(0., 0., 1.), 2.5, Vec3::Z, 0),
                solid(Vec3::new(0., 0., -1.), 2.5, Vec3::Z, 1),
            ],
            vec![blended(), opaque()],
        ),
        at(Vec3::new(-4., -3., 0.)),
        Mobility::Static,
        None,
    );
    add(
        asset(vec![solid(Vec3::ZERO, 2.5, Vec3::Z, 0)], vec![blended()]),
        at(Vec3::new(0., 4.5, 2.)),
        Mobility::Static,
        None,
    );
    add(
        asset(
            vec![
                solid(Vec3::new(0., 0., 1.), 2.5, Vec3::Z, 0),
                solid(Vec3::new(0., 0., -2.), 2.5, Vec3::Z, 1),
            ],
            vec![material(AlphaMode::Opaque, false, 1), opaque()],
        ),
        at(Vec3::new(0., -4.5, 0.)),
        Mobility::Static,
        None,
    );
    add(
        asset(
            vec![square(Vec3::ZERO, 2.5, Vec3::Z, 0, gradient)],
            vec![masked()],
        ),
        at(Vec3::new(4., 3., 0.5)),
        Mobility::Static,
        None,
    );
    add(
        one(solid(Vec3::new(0.3, 0., 0.), 2.5, Vec3::Z, 0)),
        Mat4::from_scale_rotation_translation(
            Vec3::new(-1., 1., 1.),
            Quat::IDENTITY,
            Vec3::new(4., -3., 0.),
        ),
        Mobility::Moving,
        None,
    );
    add(
        one(solid(Vec3::ZERO, 1.5, Vec3::Z, 0)),
        at(Vec3::new(-6.5, 0., 2.)),
        Mobility::Moving,
        None,
    );
    for _ in 0..2 {
        add(
            one(solid(Vec3::ZERO, 1.5, Vec3::Z, 0)),
            at(Vec3::new(2.2, 0., 3.)),
            Mobility::Static,
            None,
        );
    }
    add(
        asset(
            vec![
                solid(Vec3::ZERO, 1.5, Vec3::NEG_Z, 0),
                solid(Vec3::ZERO, 1.5, Vec3::Z, 0),
            ],
            vec![opaque()],
        ),
        at(Vec3::new(-2.2, 0., 3.)),
        Mobility::Static,
        None,
    );
    add(
        asset(
            vec![solid(Vec3::ZERO, 2., Vec3::NEG_Z, 0)],
            vec![material(AlphaMode::Opaque, true, 0)],
        ),
        at(Vec3::new(-6.5, 4.5, 1.)),
        Mobility::Static,
        None,
    );
    let mut boxes = closed_box(Vec3::ZERO, 1.5);
    boxes.extend(closed_box(Vec3::ZERO, 0.5));
    add(
        asset(boxes, vec![opaque()]),
        at(Vec3::new(0., 0., -3.)),
        Mobility::Static,
        None,
    );
    add(
        asset(
            vec![
                solid(Vec3::ZERO, 2., Vec3::Y, 0),
                square(Vec3::new(2.2, 0., 0.), 2., Vec3::Y, 1, gradient),
            ],
            vec![opaque(), masked()],
        ),
        at(Vec3::new(5., 0.5, 4.)),
        Mobility::Moving,
        Some(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)),
    );
    placed
}

/// The oracle scene's rays: a jittered grid from the camera toward the
/// wall, rays leaving the centre of the nested boxes every way, and rays
/// from below toward the squares' edges and backs, each over `interval`'s
/// choice of interval for ray `i`.
fn oracle_rays(interval: impl Fn(usize) -> (f32, f32)) -> Vec<[f32; 8]> {
    // A fixed PRNG chooses physical cases; it derives no expected hit.
    let mut state = 0x5eed_1234u32;
    let mut random = move || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (state >> 8) as f32 / 16_777_216.
    };
    let mut rays = Vec::new();
    let mut push = |origin: Vec3, direction: Vec3| {
        let (t_min, t_max) = interval(rays.len());
        rays.push([
            origin.x,
            origin.y,
            origin.z,
            t_min,
            direction.x,
            direction.y,
            direction.z,
            t_max,
        ]);
    };
    for row in 0..40 {
        for column in 0..40 {
            let x = -7.5 + 15. * (column as f32 + random()) / 40.;
            let y = -7.5 + 15. * (row as f32 + random()) / 40.;
            let direction = Vec3::new(random() - 0.5, random() - 0.5, -8.) * 0.125;
            push(Vec3::new(x, y, 10.), direction);
        }
    }
    for _ in 0..256 {
        let direction = Vec3::new(random() - 0.5, random() - 0.5, random() - 0.5);
        push(Vec3::new(0.01, 0.02, -3.03), direction.normalize() * 0.7);
    }
    for _ in 0..256 {
        let origin = Vec3::new(random() * 14. - 7., -9., random() * 8. - 5.);
        let direction = Vec3::new(random() - 0.5, 1., random() - 0.5);
        push(origin, direction);
    }
    rays
}

/// Asserts a decoded hit (`Function::Decoded`) is the hit decode of the
/// triangle the oracle's ray met, `met`, the one the architecture allows:
/// its distance, its geometric normal (its authored front) and its
/// interpolated shading normal, turned to face the ray.
fn assert_decoded(hit: &[u32; 8], met: &Met, triangles: &[Triangle], ray: [f32; 8], label: &str) {
    assert_eq!(hit[0], 1, "{label}: the decode missed {met:?}");
    let words = |at: usize| {
        DVec3::new(
            f32::from_bits(hit[at]).into(),
            f32::from_bits(hit[at + 1]).into(),
            f32::from_bits(hit[at + 2]).into(),
        )
    };
    let (normal, geometric) = (words(1), words(4));
    let t = f64::from(f32::from_bits(hit[7]));
    assert!(
        (t - met.t).abs() < T_TOLERANCE,
        "{label}: decoded at {t}, met {met:?}"
    );
    let triangle = &triangles[met.triangle];
    let expected = triangle.front.normalize();
    assert!(
        geometric.distance(expected) < 1e-3,
        "{label}: geometric normal {geometric}, the oracle's {expected} ({met:?})"
    );
    let [u, v] = met.barycentrics;
    let interpolated =
        (triangle.normals[0] * (1. - u - v) + triangle.normals[1] * u + triangle.normals[2] * v)
            .normalize();
    let direction = DVec3::new(ray[4].into(), ray[5].into(), ray[6].into());
    let facing = if expected.dot(direction) < 0. {
        interpolated
    } else {
        -interpolated
    };
    assert!(
        normal.distance(facing) < 1e-3,
        "{label}: shading normal {normal}, the oracle's {facing} ({met:?})"
    );
}

/// What the hardware's answers under `form` for `rays` through `function`,
/// accepting `sides` and leaving `receiver`, must be by the oracle over
/// `triangles`; the rays it decided.
fn check(
    (triangles, form): (&[Triangle], RayQueryForm),
    (function, sides, receiver): (Function, u32, Option<(u32, u32, u32)>),
    rays: &[[f32; 8]],
    hits: &[[u32; 8]],
) -> usize {
    let (kinds, open_end) = match function {
        Function::MovingNearest => (Kinds::Moving, false),
        Function::StaticVisible => (Kinds::Static, true),
        _ => (Kinds::All, false),
    };
    let sides = match function {
        Function::MovingNearest | Function::StaticVisible | Function::NearestExceptReceiver => {
            AS_RASTER
        }
        _ => sides,
    };
    let mut decided = 0;
    for (i, (ray, hit)) in rays.iter().zip(hits).enumerate() {
        let Some(met) = meet(triangles, *ray, (sides, kinds, open_end), receiver) else {
            continue;
        };
        decided += 1;
        let label = format!("{form:?} {function:?} sides {sides} ray {i} {ray:?}");
        match function {
            Function::Visible | Function::StaticVisible => {
                assert_visible(hit[0] == 1, &met, &label)
            }
            Function::Decoded => match allowed(&met) {
                (allowed, false) if allowed.len() == 1 => {
                    assert_decoded(hit, &allowed[0], triangles, *ray, &label)
                }
                (allowed, miss) if allowed.is_empty() && miss => {
                    assert_eq!(hit[0], 0, "{label}: decoded a hit the oracle misses")
                }
                _ => decided -= 1,
            },
            _ => assert_nearest(hit, &met, &label),
        }
    }
    decided
}

// Plausible defects, each a wrong answer against the oracle: the hardware's
// side or winding read instead of the object-space winding (mirrored square,
// boxes seen from inside); a double-sided material's back face rejected; no re-trace past a rejected hit, or one that skips
// a nearer occluder (blended, hidden and back faces before an opaque one;
// nested boxes; any-hit visibility rays whose first hit is rejected); the
// re-trace stepping back onto the same hit; masks selecting the wrong kinds
// (static-only and moving-only functions); the open end of the static
// segment ignored; predicate instances or left-out instances not walked;
// blended-only models in the TLAS; a deforming instance's BLAS built before
// its deformation or from its rest pose, or its hits judged or decoded at
// rest; cut-out texels not cut on a deforming instance's committed hits;
// barycentrics in another vertex order; a receiver's own triangle not left
// (by index, mesh and triangle as the hardware reports them), or a nearest
// ray leaving one that takes one kind only (world-space reflections' `All`
// reach); a hit's
// distance or normals decoded from the rest pose or the wrong triangle.
// Under the candidate form, besides: a masked mesh's candidate confirmed
// on a cut-out texel or never confirmed, a candidate judged without the
// predicate's other rules, a confirmed hit judged again as another
// triangle, or a masked model left on the walk.
// Run under each form the device's backend runs, twice: at the device's
// limits, where the TLAS holds every opaque and deforming instance (and,
// under the candidate form, the masked one), and with the TLAS held to the
// four nearest, where the walk covers the rest. An exactly
// tied pair is accepted either way; one the single-sided rule splits may
// skip to the hit behind it, the limitation the architecture states.
#[test]
fn hardware_rays_match_the_oracle() {
    let probe = test_support::ray_tracing_device(|limits| limits);
    let Some(forms) = probe.map(|(device, _)| forms(&device)) else {
        return;
    };
    for (form, capacity) in forms
        .into_iter()
        .flat_map(|form| [(form, None), (form, Some(4))])
    {
        let Some((device, queue)) = test_support::ray_tracing_device(|limits| wgpu::Limits {
            max_tlas_instance_count: capacity.unwrap_or(limits.max_tlas_instance_count),
            ..limits
        }) else {
            return;
        };
        let gpu = (&device, &queue);
        let mut scene = Scene::new(&device, &queue);
        let placed = oracle_scene(gpu, &mut scene);
        let deforming = placed.last().expect("the deforming instance").index;
        let masked = placed
            .iter()
            .find(|placed| {
                placed.joint.is_none()
                    && matches!(placed.asset.materials[0].alpha, AlphaMode::Mask { .. })
            })
            .expect("the masked square")
            .index;
        let query = Query::with_path(&device, Some(form));
        let mut deform = Deform::new(&device);
        let nearest = oracle_rays(|_| (0., 100.));
        let segments = oracle_rays(|i| {
            let t_max = 2. + (i % 37) as f32 * 0.5;
            (0.05 * (i % 3) as f32, t_max)
        });
        // The moving rays leave the deforming instance's first triangle,
        // the static ones the wall's, and the nearest rays of both kinds
        // either, as a receiver's own triangle: its index plus one and the
        // word of its first index.
        let receiver = |placed: &Placed| {
            let indices = scene.models.get(placed.model).unwrap().ray_meshes[0].indices;
            ([placed.index + 1, indices], Some((placed.index, 0, 0)))
        };
        let (moving, moving_id) = receiver(placed.last().unwrap());
        let (wall, wall_id) = receiver(&placed[0]);
        let none = ([0; 2], None);
        let batches = [
            (Function::Nearest, AS_RASTER, none, &nearest),
            (Function::Nearest, BOTH, none, &nearest),
            (Function::Visible, AS_RASTER, none, &segments),
            (Function::Visible, BOTH, none, &segments),
            (Function::MovingNearest, AS_RASTER, none, &nearest),
            (
                Function::MovingNearest,
                AS_RASTER,
                (moving, moving_id),
                &nearest,
            ),
            (Function::StaticVisible, AS_RASTER, none, &segments),
            (
                Function::StaticVisible,
                AS_RASTER,
                (wall, wall_id),
                &segments,
            ),
            (Function::NearestExceptReceiver, AS_RASTER, none, &nearest),
            (
                Function::NearestExceptReceiver,
                AS_RASTER,
                (wall, wall_id),
                &nearest,
            ),
            (
                Function::NearestExceptReceiver,
                AS_RASTER,
                (moving, moving_id),
                &nearest,
            ),
            (Function::Decoded, BOTH, none, &nearest),
        ];
        let hits = frame(
            gpu,
            &mut scene,
            (&query, &mut deform, form),
            0,
            &batches.map(|(function, sides, (receiver, _), rays)| Batch {
                function,
                sides,
                receiver,
                rays,
            }),
        );
        let acceleration = scene.acceleration_structures().expect("structures");
        assert!(
            acceleration.holds(deforming as usize),
            "the deforming instance, nearest the eye, is held"
        );
        if capacity.is_none() {
            // Only the candidate form's loop judges a masked model's
            // triangles in the TLAS; the baseline walks it.
            assert_eq!(
                acceleration.holds(masked as usize),
                form == RayQueryForm::Candidates,
                "{form:?}: the masked square's instance in the TLAS"
            );
        }
        let triangles = triangles(&placed, 0);
        for (&(function, sides, (_, receiver), rays), hits) in batches.iter().zip(&hits) {
            let decided = check((&triangles, form), (function, sides, receiver), rays, hits);
            assert!(
                decided * 10 > rays.len() * 9,
                "{form:?} {function:?}: the oracle decided only {decided} of {} rays",
                rays.len()
            );
        }
    }
}
