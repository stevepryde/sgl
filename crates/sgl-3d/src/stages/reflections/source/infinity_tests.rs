//! Source completion of a lit colour texel that holds +infinity.
use super::tests::{no_fog, probe};
use super::*;
use crate::test_support::{half, hdr_texture, read};
use crate::view::bindings::FogVolume;
use glam::camera;
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

const INF: f32 = f32::INFINITY;
/// Binary16's largest finite value, the scene colour's bound
/// (`shading::scene_color`).
const BOUND: f32 = 65504.;
/// The fixture's row: finite texels beside one infinite in all three
/// channels and one infinite in red alone.
const LIT: [[f32; 3]; 5] = [
    [0.5, 0.25, 0.125],
    [INF, INF, INF],
    [0.25, 0.5, 0.125],
    [INF, 0.5, 0.25],
    [0.125, 0.25, 0.5],
];
/// The multiple scattering's share of the ambient light lit colour's alpha
/// holds.
const SHARE: f32 = 0.25;

/// What completion combines with lit colour in one case.
#[derive(Clone, Copy, Debug)]
struct Case {
    /// The receivers' ambient light, or none; with it their visibility is
    /// 0.5, so completion takes what occlusion hides out of lit colour.
    ambient: Option<f32>,
    /// The ambient light is infinite too where lit colour is, as an
    /// over-range ambient light makes both.
    infinite_ambient: bool,
    /// The fog's in-scattered light and transmittance, or no fog.
    fog: Option<[f32; 4]>,
    /// A probe holds the receivers, or only the sky reflects.
    probe: bool,
}

// Plausible defect: completion turning a lit colour texel of +infinity,
// which RGBA16F holds where a lit or emissive value passed binary16's range
// on a backend that rounds overflow to infinity (#464), into NaN, which
// presents black: an infinite term times a transmittance of 0, or infinity
// less infinity where occlusion takes an infinite ambient light out of an
// infinite lit colour. On Metal before #801 the composite was +infinity with
// thin fog or none, NaN under opaque fog, and the environment specular
// alone (black) where occlusion took an infinite ambient light out.
// Oracle: the Scene colour contract (specs/sgl3d-architecture.md,
// `shading::scene_color`): read through the bound, an infinite texel is
// binary16's largest finite value, 65504, and every finite value reads
// unchanged. So completion of a frame holding infinity, in one channel or
// all three, writes a composite and incident radiance bit-identical to those
// of the same frame holding 65504 there, over every texel, the finite
// neighbours included; with ambient light and occlusion on and off, with fog
// thin and opaque and without it, and with a probe or the sky reflecting.
#[test]
fn infinite_lit_colour_completes_as_the_scene_colour_bound() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [LIT.len() as u32, 1];
    let texture = |texels: &[[f32; 4]]| hdr_texture(&device, &queue, size, texels);
    // A rough dielectric facing the camera, lit, without baked lights:
    // material occlusion 1 (code 127) or 0.5 (code 64).
    let f0 = |code: u8| {
        device
            .create_texture_with_data(
                &queue,
                &wgpu::TextureDescriptor {
                    label: Some("dielectric F0"),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: crate::shading::gbuffer::F0,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &[10, 10, 10, code].repeat(LIT.len()),
            )
            .create_view(&Default::default())
    };
    let open = f0(127);
    let occluded = f0(64);
    let normal = texture(&[[0.; 4]; LIT.len()]);
    let material = texture(&[[0.5, 0.5, 0., 1.]; LIT.len()]);
    // Isotropic, at environment scale 1.
    let anisotropy = texture(&[[0., 0., 0., 1.]; LIT.len()]);
    let sky = hdr_texture(&device, &queue, [1, 1], &[[0.5, 1., 2., 1.]])
        .texture()
        .create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
    let projection = camera::rh::proj::directx::orthographic(-10., 10., -1., 1., 100., 0.1);
    let reflection_camera = reflection_camera::Camera::new(Mat4::IDENTITY, projection);
    let projected = projection * Vec4::new(0., 0., -10., 1.);
    let depth = crate::view::targets::target(
        &device,
        "view depth ten",
        size,
        wgpu::TextureFormat::Depth32Float,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(projected.z / projected.w),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    queue.submit([encoder.finish()]);
    let no_fog = no_fog(&device);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let lookup_tables = crate::scene::lookup_tables::lookup_tables(&device, &queue);
    let parameters = environment_uniform(&device, 0., 1.);
    let no_probes = crate::scene::probes::UploadedProbes::empty(&device);
    let far = Vec3::splat(20.);
    let probes = crate::scene::probes::UploadedProbes::new(
        &device,
        &queue,
        &[probe([2., 1., 0.5], -far, far, 0.)],
    )
    .unwrap();
    let visibility = super::tests::visible(&device, &queue);
    let mut source = ReflectionSource::new(
        &device,
        size,
        Variant {
            environment: true,
            incident: true,
        },
    );
    let output = ReflectionSource::target(&device, size, "complete opaque beauty");
    // The composite and incident radiance completion writes in `case` for
    // lit colour `lit`, as binary16 bits.
    let mut complete = |case: Case, infinity: f32| -> Vec<u16> {
        let lit: Vec<[f32; 4]> = LIT
            .iter()
            .map(|rgb| {
                let [r, g, b] = rgb.map(|v| if v == INF { infinity } else { v });
                [r, g, b, SHARE]
            })
            .collect();
        let lit = texture(&lit);
        let ambient: Vec<[f32; 4]> = LIT
            .iter()
            .map(|rgb| {
                let light = case.ambient.unwrap_or(0.);
                let [r, g, b] = rgb.map(|v| {
                    if v == INF && case.infinite_ambient {
                        infinity
                    } else {
                        light
                    }
                });
                [r, g, b, 1.]
            })
            .collect();
        let ambient = texture(&ambient);
        let fog_volume = case.fog.map(|fog| {
            device
                .create_texture_with_data(
                    &queue,
                    &wgpu::TextureDescriptor {
                        label: Some("uniform fog"),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D3,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    bytemuck::cast_slice(&fog.map(crate::test_support::to_half)),
                )
                .create_view(&Default::default())
        });
        let collection = if case.probe { &probes } else { &no_probes };
        let input = Inputs {
            fog: FogVolume {
                view: fog_volume.as_ref().unwrap_or(&no_fog.0),
                sampler: &no_fog.1,
            },
            frame_fog: case.fog.map(|_| SourceFog {
                // A 100 m fog of slices spaced evenly, without aerial slices.
                layout: crate::shading::fog::FogLayout {
                    inverse_length: 1. / 100.,
                    inverse_detail_spread: 1.,
                    aerial_reach: 0.,
                    fog_slices: 1,
                    aerial_slices: 0,
                },
                sky_affect: 1.,
            }),
            camera: reflection_camera,
            scene: &lit,
            ambient: &ambient,
            output: &output,
            normal: &normal,
            anisotropy: &anisotropy,
            f0: if case.ambient.is_some() {
                &occluded
            } else {
                &open
            },
            depth: &depth,
            lookup_tables: &lookup_tables,
            ambient_occlusion: &visibility,
            environment: Environment {
                sky: &sky,
                sampler: &sampler,
                parameters: &parameters,
                material: &material,
                baked: &collection.view,
                collection: &collection.metadata,
            },
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        source.encode(&mut encoder, &device, &queue, input, None);
        queue.submit([encoder.finish()]);
        [output.texture(), source.incident.texture()]
            .into_iter()
            .flat_map(|target| read(&device, &queue, target, 8))
            .collect::<Vec<u8>>()
            .chunks_exact(2)
            .map(|bits| u16::from_le_bytes([bits[0], bits[1]]))
            .collect()
    };
    let mut failures = Vec::new();
    let mut cases = 0;
    for ambient in [None, Some(0.5)] {
        for infinite_ambient in [false, true] {
            if infinite_ambient && ambient.is_none() {
                continue;
            }
            for fog in [None, Some([0.1, 0.2, 0.3, 0.5]), Some([0.1, 0.2, 0.3, 0.])] {
                for probe in [false, true] {
                    let case = Case {
                        ambient,
                        infinite_ambient,
                        fog,
                        probe,
                    };
                    cases += 1;
                    let infinite = complete(case, INF);
                    let bounded = complete(case, BOUND);
                    let values = |bits: &[u16]| -> Vec<f32> {
                        bits.iter().map(|b| half(&b.to_le_bytes())).collect()
                    };
                    let non_finite = infinite.iter().any(|b| b & 0x7c00 == 0x7c00);
                    if non_finite || infinite != bounded {
                        failures.push(format!(
                            "{case:?}:\n  infinite {:?}\n  bounded  {:?}",
                            values(&infinite),
                            values(&bounded)
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {cases} cases differ from the bound (composite, then incident radiance; 4 channels a texel):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
