//! The packed vertex's round trip: vertices packed by `shading::packed_vertex`
//! and decoded on the GPU by packed_vertex.wgsl, held against the vertices
//! given and the tolerances the architecture's Vertex encoding declares.
use super::packed_vertex::{PackedVertex, UvRect, pack};
use crate::asset::Vertex;
use crate::test_support;
use glam::{DMat3, DQuat, DVec3, Vec3};
use wgpu::util::DeviceExt;

/// The declared tolerance of a decoded normal or tangent, in degrees.
const FRAME_DEGREES: f64 = 0.01;

/// What the GPU decodes of a packed vertex.
struct Decoded {
    normal: DVec3,
    tangent: DVec3,
    handedness: f64,
    uv: [f64; 2],
    lightmap_uv: [f64; 2],
    color: [f64; 4],
    chart: u32,
}

/// `vertices`, each packed with its UV across `rect` and chart `chart`, as
/// packed_vertex.wgsl decodes them.
fn round_trip(vertices: &[(Vertex, UvRect, u16)]) -> Option<Vec<Decoded>> {
    let (device, queue) = test_support::device()?;
    let packed: Vec<PackedVertex> = vertices
        .iter()
        .map(|(vertex, rect, chart)| pack(vertex, rect, *chart))
        .collect();
    let rects: Vec<[f32; 4]> = vertices.iter().map(|(_, rect, _)| rect.words()).collect();
    let storage = |label, contents: &[u8], usage| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage: wgpu::BufferUsages::STORAGE | usage,
        })
    };
    let words = storage(
        "packed vertices",
        bytemuck::cast_slice(&packed),
        wgpu::BufferUsages::empty(),
    );
    let rect_buffer = storage(
        "uv rects",
        bytemuck::cast_slice(&rects),
        wgpu::BufferUsages::empty(),
    );
    let output = storage(
        "decoded vertices",
        &vec![0; vertices.len() * 64],
        wgpu::BufferUsages::COPY_SRC,
    );
    let source = super::compose(&[&super::PACKED_VERTEX])
        + r#"
@group(0) @binding(0) var<storage,read> vertices:array<u32>;
@group(0) @binding(1) var<storage,read> rects:array<vec4<f32>>;
@group(0) @binding(2) var<storage,read_write> decoded:array<vec4<f32>>;
@compute @workgroup_size(64) fn decode(@builtin(global_invocation_id) id:vec3<u32>) {
 let i=id.x;
 if i>=arrayLength(&rects) {
  return;
 }
 let at=i*PACKED_VERTEX_WORDS;
 let angle_chart=vertices[at+PACKED_VERTEX_ANGLE_CHART];
 let frame=packed_vertex_frame(vertices[at+PACKED_VERTEX_AXIS],angle_chart);
 decoded[i*4u]=vec4(frame.normal,f32(packed_vertex_chart(angle_chart)));
 decoded[i*4u+1u]=frame.tangent;
 decoded[i*4u+2u]=vec4(packed_vertex_uv(vertices[at+PACKED_VERTEX_UV],rects[i]),packed_vertex_lightmap_uv(vertices[at+PACKED_VERTEX_LIGHTMAP_UV]));
 decoded[i*4u+3u]=packed_vertex_color(vertices[at+PACKED_VERTEX_COLOR]);
}
"#;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("packed vertex round trip"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("decode"),
        compilation_options: Default::default(),
        cache: None,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[&words, &rect_buffer, &output]
            .into_iter()
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect::<Vec<_>>(),
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups((vertices.len() as u32).div_ceil(64), 1, 1);
    }
    queue.submit([encoder.finish()]);
    let words = test_support::read_words(&device, &queue, &output);
    let values: &[[f32; 4]] = bytemuck::cast_slice(&words);
    Some(
        values
            .chunks_exact(4)
            .map(|v| {
                let d = |x: f32| f64::from(x);
                Decoded {
                    normal: DVec3::new(d(v[0][0]), d(v[0][1]), d(v[0][2])),
                    chart: v[0][3] as u32,
                    tangent: DVec3::new(d(v[1][0]), d(v[1][1]), d(v[1][2])),
                    handedness: d(v[1][3]),
                    uv: [d(v[2][0]), d(v[2][1])],
                    lightmap_uv: [d(v[2][2]), d(v[2][3])],
                    color: v[3].map(d),
                }
            })
            .collect(),
    )
}

/// A vertex at the origin with `normal` and `tangent`, and nothing else.
fn framed(normal: DVec3, tangent: [f64; 4]) -> Vertex {
    Vertex {
        position: [0.; 3],
        normal: normal.as_vec3().to_array(),
        uv: [0.; 2],
        color: [1.; 4],
        lightmap_uv: [0.; 2],
        lightmap_bounds: [0., 0., 1., 1.],
        tangent: tangent.map(|value| value as f32),
    }
}

/// The angle between `a` and `b`, in degrees, whatever their lengths.
fn degrees(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b)).to_degrees()
}

/// `count` directions spread evenly over the sphere (a Fibonacci lattice).
fn sphere(count: usize) -> impl Iterator<Item = DVec3> {
    let golden = std::f64::consts::PI * (3. - 5f64.sqrt());
    (0..count).map(move |i| {
        let y = 1. - (i as f64 + 0.5) / count as f64 * 2.;
        let radius = (1. - y * y).sqrt();
        let theta = golden * i as f64;
        DVec3::new(radius * theta.cos(), y, radius * theta.sin())
    })
}

// Plausible defects: a frame quantised by truncation, as Godot's casts do,
// or its axis and angle taken by a method that loses precision near 0° or
// 180° of rotation; the frame's rows and columns, or the handedness's half,
// taken the other way round by the encoder or the decoder; an oblique
// tangent not projected onto the normal's plane. The oracle is the vertex
// given: its normal, its tangent projected onto the normal's plane, and its
// handedness, from frames all over the sphere at eight tangent turns and
// from rotations about axes all over the sphere within 1e-7 rad of 0° and
// 180°, decoded within the declared 0.01°.
#[test]
fn frames_decode_within_their_tolerance_at_every_rotation() {
    let mut vertices = Vec::new();
    for normal in sphere(4096) {
        let across = normal.any_orthonormal_vector();
        let up = normal.cross(across);
        for turn in 0..8 {
            let angle = turn as f64 * std::f64::consts::TAU / 8. + 0.1;
            let tangent = across * angle.cos() + up * angle.sin();
            for handedness in [1., -1.] {
                vertices.push(framed(
                    normal,
                    [tangent.x, tangent.y, tangent.z, handedness],
                ));
            }
        }
    }
    let pi = std::f64::consts::PI;
    for axis in sphere(512) {
        for angle in [
            0.,
            1e-7,
            1e-5,
            1e-3,
            0.1,
            pi / 2.,
            pi - 0.1,
            pi - 1e-3,
            pi - 1e-5,
            pi - 1e-7,
            pi,
        ] {
            // A frame whose rows are the rotation's: tangent, bitangent,
            // normal.
            let rotation = DMat3::from_quat(DQuat::from_axis_angle(axis, angle));
            let (tangent, normal) = (rotation.row(0), rotation.row(2));
            for handedness in [1., -1.] {
                vertices.push(framed(
                    normal,
                    [tangent.x, tangent.y, tangent.z, handedness],
                ));
            }
        }
    }
    let cases: Vec<_> = vertices
        .into_iter()
        .map(|vertex| (vertex, UvRect::default(), 0))
        .collect();
    let Some(decoded) = round_trip(&cases) else {
        return;
    };
    let (mut normal_error, mut tangent_error) = (0f64, 0f64);
    for ((vertex, _, _), decoded) in cases.iter().zip(&decoded) {
        let normal = Vec3::from_array(vertex.normal).as_dvec3().normalize();
        let given = Vec3::from_slice(&vertex.tangent[..3]).as_dvec3();
        let tangent = given - normal * normal.dot(given);
        normal_error = normal_error.max(degrees(decoded.normal, normal));
        tangent_error = tangent_error.max(degrees(decoded.tangent, tangent));
        assert_eq!(
            decoded.handedness,
            f64::from(vertex.tangent[3]),
            "normal {normal}, tangent {:?}",
            vertex.tangent
        );
    }
    eprintln!(
        "worst decoded normal {normal_error:.5}°, tangent {tangent_error:.5}° over {} frames",
        cases.len()
    );
    assert!(
        normal_error <= FRAME_DEGREES && tangent_error <= FRAME_DEGREES,
        "normal {normal_error}°, tangent {tangent_error}°, beyond {FRAME_DEGREES}°"
    );
}

// Plausible defects: an absent tangent (none, handedness not ±1, or along
// the normal) packed as given, so a zero or NaN tangent decodes, or given
// Godot's arbitrary tangent, which vanishes for a normal along (1, 1, -1);
// an oblique tangent kept oblique. The oracle is the contract: an absent
// tangent decodes as a unit vector in the normal's plane with handedness +1,
// and an oblique one as its projection onto that plane.
#[test]
fn absent_and_oblique_tangents_decode_in_the_normals_plane() {
    let diagonal = DVec3::new(1., 1., -1.).normalize();
    let absent = [
        (DVec3::Z, [0., 0., 0., 0.]),
        (DVec3::Z, [1., 0., 0., 0.5]),
        (DVec3::Z, [0., 0., 2., 1.]),
        (diagonal, [0., 0., 0., 0.]),
        (DVec3::NEG_Y, [0., 0., 0., 1.]),
    ];
    let oblique = framed(DVec3::Z, [1., 0., 1., -1.]);
    let cases: Vec<_> = absent
        .iter()
        .map(|&(normal, tangent)| framed(normal, tangent))
        .chain([oblique])
        .map(|vertex| (vertex, UvRect::default(), 0))
        .collect();
    let Some(decoded) = round_trip(&cases) else {
        return;
    };
    let tolerance = FRAME_DEGREES.to_radians().sin();
    for ((normal, tangent), decoded) in absent.iter().zip(&decoded) {
        let label = format!("normal {normal}, tangent {tangent:?}");
        assert!(
            (decoded.tangent.length() - 1.).abs() <= tolerance,
            "{label}: tangent {}",
            decoded.tangent
        );
        assert!(
            decoded.tangent.dot(*normal).abs() <= tolerance,
            "{label}: tangent {} off the normal's plane",
            decoded.tangent
        );
        assert_eq!(decoded.handedness, 1., "{label}");
    }
    let last = decoded.last().unwrap();
    assert!(
        degrees(last.tangent, DVec3::X) <= FRAME_DEGREES && last.handedness == -1.,
        "an oblique tangent decodes as {} of handedness {}",
        last.tangent,
        last.handedness
    );
}

/// The sRGB encoding of linear `value` in 0..=1 (IEC 61966-2-1).
fn srgb(value: f64) -> f64 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}

// Plausible defects: a UV, colour or lightmap UV quantised by truncation, or
// across a rectangle other than its mesh's; a colour stored linear, or not
// clamped; a negative lightmap UV kept, or a chart index lost to its
// neighbouring half-word. The oracle is each value given and the declared
// tolerances: a UV within its rectangle's extent over 131 070, a lightmap UV
// within 1/131 070, a colour within half an 8-bit step of its sRGB encoding
// once clamped to 0..1, a negative lightmap UV as (0, 0), and the chart as
// given.
#[test]
fn uvs_colours_and_charts_decode_within_their_tolerances() {
    let uvs = [
        [-37.5, 3.],
        [112.25, 3.],
        [0.1, 3.],
        [51.731, 3.],
        [-0.004, 3.],
    ];
    let mesh: Vec<Vertex> = uvs
        .iter()
        .map(|&uv| Vertex {
            uv,
            ..framed(DVec3::Z, [0.; 4])
        })
        .collect();
    let rect = UvRect::of(&mesh);
    let colors = [
        [-0.5, 0., 0.001, 0.0031308],
        [0.04, 0.2, 0.5, 0.99],
        [1., 1.7, 0.73, 0.5],
    ];
    let lightmaps = [[0.123456, 0.987654], [-0.5, 0.3], [0., -1.], [1., 0.5]];
    let charts = [0, 1, 7, 65535];
    let mut cases: Vec<_> = mesh.into_iter().map(|vertex| (vertex, rect, 0)).collect();
    cases.extend(colors.iter().map(|&color| {
        let vertex = Vertex {
            color,
            ..framed(DVec3::Z, [0.; 4])
        };
        (vertex, UvRect::default(), 0)
    }));
    cases.extend(lightmaps.iter().zip(charts).map(|(&lightmap_uv, chart)| {
        let vertex = Vertex {
            lightmap_uv,
            ..framed(DVec3::Z, [0.; 4])
        };
        (vertex, UvRect::default(), chart)
    }));
    let Some(decoded) = round_trip(&cases) else {
        return;
    };
    // An f32's rounding at the decoded value's magnitude.
    let rounding = |value: f64| value.abs().max(1.) * f64::from(f32::EPSILON);
    for (i, &uv) in uvs.iter().enumerate() {
        for axis in 0..2 {
            let given = f64::from(uv[axis]);
            let tolerance = f64::from(rect.extent[axis]) / 131070. + rounding(given);
            let error = (decoded[i].uv[axis] - given).abs();
            assert!(
                error <= tolerance,
                "UV {uv:?} axis {axis}: {} is {error} off, beyond {tolerance}",
                decoded[i].uv[axis]
            );
        }
    }
    for (i, color) in colors.iter().enumerate() {
        let decoded = &decoded[uvs.len() + i];
        for channel in 0..4 {
            let given = f64::from(color[channel]).clamp(0., 1.);
            let (expected, actual) = if channel < 3 {
                (srgb(given), srgb(decoded.color[channel].clamp(0., 1.)))
            } else {
                (given, decoded.color[channel])
            };
            assert!(
                (actual - expected).abs() <= 0.5 / 255. + 1e-6,
                "colour {color:?} channel {channel}: {}",
                decoded.color[channel]
            );
        }
    }
    for (i, (lightmap, chart)) in lightmaps.iter().zip(charts).enumerate() {
        let decoded = &decoded[uvs.len() + colors.len() + i];
        let expected = if lightmap.iter().any(|&value| value < 0.) {
            [0., 0.]
        } else {
            lightmap.map(f64::from)
        };
        for (actual, expected) in decoded.lightmap_uv.iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1. / 131070. + 1e-7,
                "lightmap UV {lightmap:?}: {:?}",
                decoded.lightmap_uv
            );
        }
        assert_eq!(decoded.chart, u32::from(chart), "chart");
    }
}
