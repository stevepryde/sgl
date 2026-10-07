//! Composition of the opaque surfaces under a blended receiver.
use super::tests::{no_fog, visible};
use super::*;
use crate::test_support::{half, hdr_texture, read};
use crate::view::bindings::FogVolume;
use glam::camera;
use glam::{Mat4, Vec4};

/// A depth target of `size` cleared to the reversed-Z depth of a point
/// `distance` metres ahead under `projection`.
fn depth_at(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    projection: Mat4,
    distance: f32,
) -> wgpu::TextureView {
    let view = crate::view::targets::target(
        device,
        "fixture depth",
        size,
        wgpu::TextureFormat::Depth32Float,
    );
    let projected = projection * Vec4::new(0., 0., -distance, 1.);
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("fixture depth"),
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(projected.z / projected.w),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    queue.submit([encoder.finish()]);
    view
}

// Plausible defects: composition adds the screen-space method's result to an
// opaque surface under a blended receiver, although the method traced the
// receiver there (the reflection shows on the receiver and again on what is
// seen through it), or drops that surface's environment specular. The oracle
// is that a smooth white metal reflects its surroundings once: in a uniform
// environment of radiance 2 it returns that radiance times its specular
// response with no method; where it is the surface and the method saw
// radiance 3 with full confidence, 3 times the same response; under a
// receiver, its environment again.
#[test]
fn opaque_surfaces_under_a_receiver_keep_their_environment_specular_once() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [2, 1];
    let texture = |texel: [f32; 4]| hdr_texture(&device, &queue, size, &[texel; 2]);
    let black = texture([0.; 4]);
    // No ambient diffuse, and no irradiance volume: its sky visibility 1.
    let ambient = texture([0., 0., 0., 1.]);
    // An almost smooth (0.05) white metal facing the camera, lit.
    let normal = texture([0.; 4]);
    let material = texture([1., 0.05, 0., 1.]);
    // Isotropic, at environment scale 1.
    let anisotropy = texture([0., 0., 0., 1.]);
    let f0 = crate::view::targets::target(
        &device,
        "white metal F0",
        size,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("white metal F0"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &f0,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    queue.submit([encoder.finish()]);
    let sky = hdr_texture(&device, &queue, [1, 1], &[[2., 2., 2., 1.]])
        .texture()
        .create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
    let reflected = texture([3., 3., 3., 1.]);
    let projection = camera::rh::proj::directx::orthographic(-10., 10., -1., 1., 100., 0.1);
    let reflection_camera = reflection_camera::Camera::new(Mat4::IDENTITY, projection);
    let opaque_depth = depth_at(&device, &queue, size, projection, 10.);
    let receiver_depth = depth_at(&device, &queue, size, projection, 5.);
    let fog = no_fog(&device);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let lookup_tables = crate::scene::lookup_tables::lookup_tables(&device, &queue);
    let parameters = environment_uniform(&device, 0., 1.);
    let probes = crate::scene::probes::UploadedProbes::empty(&device);
    let visibility = visible(&device, &queue);
    let mut source = ReflectionSource::new(
        &device,
        size,
        Variant {
            environment: true,
            incident: true,
        },
    );
    let output = ReflectionSource::target(&device, size, "complete opaque beauty");
    // The beauty of the white metal after completion and composition with
    // the method's `cutoff` (0: none), its result `reflected` and the
    // surface's `surface_depth`.
    let mut compose = |cutoff: f32, surface_depth: &wgpu::TextureView| -> Vec<f32> {
        queue.write_buffer(
            &parameters,
            0,
            bytemuck::cast_slice(&[0., 1., 0.05, cutoff * cutoff]),
        );
        let input = || Inputs {
            fog: FogVolume {
                view: &fog.0,
                sampler: &fog.1,
            },
            frame_fog: None,
            camera: reflection_camera,
            scene: &black,
            ambient: &ambient,
            output: &output,
            normal: &normal,
            anisotropy: &anisotropy,
            f0: &f0,
            depth: &opaque_depth,
            lookup_tables: &lookup_tables,
            ambient_occlusion: &visibility,
            environment: Environment {
                sky: &sky,
                sampler: &sampler,
                parameters: &parameters,
                material: &material,
                baked: &probes.view,
                collection: &probes.metadata,
            },
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        source.encode(&mut encoder, &device, &queue, input(), None);
        source.compose(
            &mut encoder,
            &device,
            input(),
            surface_depth,
            &reflected,
            None,
            None,
        );
        queue.submit([encoder.finish()]);
        read(&device, &queue, output.texture(), 8)
            .chunks_exact(2)
            .map(half)
            .collect()
    };
    let environment = compose(0., &opaque_depth);
    let surface = compose(0.2, &opaque_depth);
    let under_receiver = compose(0.2, &receiver_depth);
    for pixel in 0..2 {
        for channel in 0..3 {
            let i = pixel * 4 + channel;
            let response = environment[i] / 2.;
            assert!(
                (0.9..=1.).contains(&response),
                "a smooth white metal's response {response}"
            );
            assert!(
                (surface[i] - 3. * response).abs() < 0.01,
                "the surface reflects {} where the method saw 3 (response {response})",
                surface[i]
            );
            assert!(
                (under_receiver[i] - environment[i]).abs() < 0.004,
                "under a receiver the surface reflects {}, its environment {}",
                under_receiver[i],
                environment[i]
            );
        }
    }
}
