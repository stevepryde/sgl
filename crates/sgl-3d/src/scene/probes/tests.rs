use super::*;
use crate::baked_specular_probe::{SpecularProbeBox, SpecularProbeRadiance};
use glam::{Mat4, Vec3};

fn fixture() -> BakedSpecularProbe {
    BakedSpecularProbe {
        center: Vec3::ZERO,
        world_to_local: Mat4::IDENTITY,
        influence: SpecularProbeBox {
            min: Vec3::splat(-2.),
            max: Vec3::splat(2.),
        },
        blend: Vec3::splat(0.5),
        proxy: Some(SpecularProbeBox {
            min: Vec3::splat(-2.),
            max: Vec3::splat(2.),
        }),
        radiance: SpecularProbeRadiance {
            face_size: 64,
            texels: SpecularProbeTexels::Rgba16Float(vec![
                0x3c00;
                payload_size(
                    wgpu::TextureFormat::Rgba16Float,
                    64
                ) / 2
            ]),
        },
    }
}

// Malformed asset input must be rejected before texture creation/upload,
// which otherwise reports a GPU validation error or admits poisoned radiance.
#[test]
fn malformed_radiance_and_truncated_levels_are_rejected() {
    fn values(probe: &mut BakedSpecularProbe) -> &mut Vec<u16> {
        match &mut probe.radiance.texels {
            SpecularProbeTexels::Rgba16Float(values) => values,
            SpecularProbeTexels::Bc6hUfloat(_) => unreachable!(),
        }
    }
    let mut probe = fixture();
    for invalid in [0x7c00, 0xfc00, 0x7e00, 0xbc00, 0x8001] {
        values(&mut probe)[17] = invalid;
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidRadiance { component: 17 })
        ));
    }
    values(&mut probe)[17] = 0x8000; // IEEE negative zero is finite and nonnegative.
    assert!(probe.validate(&wgpu::Limits::default()).is_ok());
    values(&mut probe).pop();
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidPayloadLength { .. })
    ));
}

#[test]
fn box_projection_rejects_scale_shear_and_outside_capture_origin() {
    let mut probe = fixture();
    probe.world_to_local = Mat4::from_scale(Vec3::new(1., 2., 1.));
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidProbe(_))
    ));
    probe.world_to_local = Mat4::IDENTITY;
    probe.world_to_local.y_axis.x = 0.2;
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidProbe(_))
    ));
    probe.world_to_local =
        Mat4::from_rotation_y(0.7) * Mat4::from_translation(Vec3::new(-10., 0., 0.));
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidProbe(_))
    ));
    probe.center = Vec3::new(10., 0., 0.);
    assert!(probe.validate(&wgpu::Limits::default()).is_ok());
    probe.blend.x = -0.1;
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidProbe(_))
    ));
}

// Defect: a box wider than f32 can hold passes as finite with `min <
// max`, and its infinite extent reaches the probe grid and shading.
// Independent signal: f32 overflow of `max - min`.
#[test]
fn boxes_wider_than_f32_can_hold_are_rejected() {
    let mut probe = fixture();
    let huge = SpecularProbeBox {
        min: Vec3::splat(-1e13),
        max: Vec3::splat(1e13),
    };
    probe.influence = huge;
    probe.proxy = Some(huge);
    assert!(probe.validate(&wgpu::Limits::default()).is_ok());
    let overflowing = SpecularProbeBox {
        min: Vec3::new(-1., -f32::MAX, -1.),
        max: Vec3::new(1., f32::MAX, 1.),
    };
    probe.influence = overflowing;
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidProbe(_))
    ));
    probe.influence = huge;
    probe.proxy = Some(overflowing);
    assert!(matches!(
        probe.validate(&wgpu::Limits::default()),
        Err(ProbeError::InvalidProbe(_))
    ));
}

#[test]
fn unaddressable_face_sizes_are_rejected_before_payload_access() {
    let mut probe = fixture();
    for face_size in [0, 32, 96, u32::MAX] {
        probe.radiance.face_size = face_size;
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidDimensions)
        ));
    }
}

// Defect: box projection normalised `hit - centre` by its length, whose
// square overflows f32 once the proxy reaches about 1e19 m, and a proxy
// near f32's range overflowed the hit distance too, so the probe was
// sampled toward zero or NaN: an arbitrary texel. Oracle: Lagarde and
// Zanuttini's construction. A proxy that large acts as distant, so a
// receiver near the capture centre samples the face its reflection
// direction's major axis names; an ordinary proxy is sampled toward the
// ray's hit on it from the capture centre. Each face holds its own value
// (+X 1, -X 2, +Y 3, -Y 4, +Z 5, -Z 6) on every level.
#[test]
fn box_projection_samples_the_face_toward_the_proxy_hit() {
    use crate::settings::{self, Settings};
    use std::f32::consts::FRAC_PI_2;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    // The probe frame turns world (x, y, z) about `origin` into local
    // (-y, x, z); local (u, v, w) is world origin + (v, -u, w).
    let origin = Vec3::new(10., -4., 3.);
    let world_to_local = Mat4::from_rotation_z(FRAC_PI_2) * Mat4::from_translation(-origin);
    let local_to_world = world_to_local.inverse();
    let texels: Vec<u16> = (0..LEVELS)
        .flat_map(|level| {
            (0..6u16).flat_map(move |face| {
                let value = crate::test_support::to_half(f32::from(face + 1));
                [value, 0, 0, 0x3c00].repeat((64usize >> level).pow(2))
            })
        })
        .collect();
    // The capture at local (0.9, 0, 0), inside a proxy of half-width
    // `half`.
    let probe = |half: f32| BakedSpecularProbe {
        center: local_to_world.transform_point3(Vec3::new(0.9, 0., 0.)),
        world_to_local,
        influence: SpecularProbeBox {
            min: Vec3::splat(-1.),
            max: Vec3::splat(1.),
        },
        blend: Vec3::ZERO,
        proxy: Some(SpecularProbeBox {
            min: Vec3::splat(-half),
            max: Vec3::splat(half),
        }),
        radiance: SpecularProbeRadiance {
            face_size: 64,
            texels: SpecularProbeTexels::Rgba16Float(texels.clone()),
        },
    };
    let mut scene = Scene::new(&device, &queue);
    scene
        .set_baked_specular_probes(
            &device,
            &queue,
            &[probe(1.), probe(1e20), probe(f32::MAX / 2.)],
        )
        .unwrap();
    let receiver = origin + Vec3::new(0.2, 0.1, -0.3);
    // (probe, receiver, world direction, expected face value).
    let mut cases = vec![
        // From the frame's origin along local (1, 0.3, 0), world -Y, the
        // ray meets the unit proxy at local (1, 0.3, 0), which lies along
        // local (0.1, 0.3, 0), world +X, from the capture.
        (0, origin, Vec3::new(0.3, -1., 0.), 1.),
    ];
    for index in [1, 2] {
        for (direction, face) in [
            (Vec3::X, 1.),
            (Vec3::NEG_X, 2.),
            (Vec3::Y, 3.),
            (Vec3::NEG_Y, 4.),
            (Vec3::new(0.36, -0.8, 0.48), 4.),
            (Vec3::new(-0.9, 0.3, 0.2), 2.),
        ] {
            cases.push((index, receiver, direction, face));
        }
    }
    // From local (-0.49, -0.49, -0.49) f32::MAX along local (1, 0.95,
    // 0.9), world (0.95, -1, 0.9), the ray meets the f32::MAX / 2 proxy
    // on its local +X face, at about (1, 0.9, 0.8) of its half-width,
    // which lies along world -Y from the capture.
    cases.push((
        2,
        local_to_world.transform_point3(Vec3::splat(-0.49 * f32::MAX)),
        Vec3::new(0.95, -1., 0.9),
        4.,
    ));
    let vec3 = |v: Vec3| format!("vec3({:?},{:?},{:?})", v.x, v.y, v.z);
    let count = cases.len();
    let observation = format!(
        r#"@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let probes=array<u32,{count}>({});
 let worlds=array<vec3<f32>,{count}>({});
 let directions=array<vec3<f32>,{count}>({});
 for (var i=0u;i<{count}u;i++) {{
  output[i]=vec4(probe_radiance(probes[i],worlds[i],directions[i],0.,environment_sampler),0.);
 }}
}}
"#,
        cases
            .iter()
            .map(|c| format!("{}u", c.0))
            .collect::<Vec<_>>()
            .join(","),
        cases
            .iter()
            .map(|c| vec3(c.1))
            .collect::<Vec<_>>()
            .join(","),
        cases
            .iter()
            .map(|c| vec3(c.2.normalize()))
            .collect::<Vec<_>>()
            .join(","),
    );
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        world_space_reflections: settings::WorldSpaceReflections::Off,
        dynamic_gi: settings::DynamicGiQuality::Off,
        ..Settings::default()
    };
    let mut renderer = crate::TestRenderer::for_test(&device, &queue, [16, 16], &settings);
    let input = crate::FrameInput::new(crate::Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let sampled = crate::test_support::observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        &observation,
        count,
    );
    for ((index, _, direction, face), sample) in cases.iter().zip(&sampled) {
        assert!(
            (sample[0] - face).abs() < 0.01,
            "probe {index} toward {direction}: sampled {sample:?}, expected face {face}"
        );
    }
}
