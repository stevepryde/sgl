use super::*;
use crate::frame_input::CompensationCurve;
use crate::test_support::{hdr_texture, read};

/// The histogram `exposure` accumulated, read without averaging it.
fn histogram_of(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    exposure: &mut Exposure,
    frame: &wgpu::TextureView,
    automatic: &AutoExposure,
) -> Vec<u32> {
    exposure.upload(
        queue,
        Metering {
            automatic,
            stops: 0.,
            delta_time: 0.,
            reset: true,
        },
    );
    let group = exposure.group(device, frame);
    let size = frame.texture().size();
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("histogram readback"),
        size: 64 * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_bind_group(0, &group, &[]);
        pass.set_pipeline(&exposure.histogram_pipeline);
        pass.dispatch_workgroups(size.width.div_ceil(16), size.height.div_ceil(16), 1);
    }
    encoder.copy_buffer_to_buffer(&exposure.histogram, 0, &readback, 0, 64 * 4);
    queue.submit([encoder.finish()]);
    readback.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    bytemuck::cast_slice(&readback.get_mapped_range(..).unwrap()).to_vec()
}

/// One frame of auto exposure of `frame`; returns the correction in stops.
fn adapt(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    exposure: &mut Exposure,
    frame: &wgpu::TextureView,
    metering: Metering<'_>,
) -> f32 {
    let mut encoder = device.create_command_encoder(&Default::default());
    exposure.meter(device, queue, &mut encoder, frame, metering, None);
    queue.submit([encoder.finish()]);
    let multiplier: f32 = bytemuck::pod_read_unaligned(&read(device, queue, &exposure.exposure, 4));
    multiplier.log2() - metering.stops
}

/// A grey frame of `luminance`.
fn grey(device: &wgpu::Device, queue: &wgpu::Queue, luminance: f32) -> wgpu::TextureView {
    hdr_texture(
        device,
        queue,
        [64, 64],
        &[[luminance, luminance, luminance, 1.]; 64 * 64],
    )
}

// Defects: workgroups that add bins they do not own (Bevy adds 256
// invocations' worth into 64 bins), lost atomics or barriers, partial
// workgroups at the frame's edges counted or dropped wrongly, a mask read
// flipped or scaled, or luminance binned on the wrong scale. The oracle is a
// CPU histogram of a frame whose every pixel was built in the middle of a
// known bin of the 64-bin log2 histogram, weighted by its mask cell.
#[test]
fn histogram_counts_each_pixel_in_its_bin_by_its_mask_weight() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [37usize, 29];
    let automatic = AutoExposure {
        metering_mask: MeteringMask {
            weights: std::array::from_fn(|row| {
                std::array::from_fn(|column| ((row * 31 + column * 17) % 256) as u8)
            }),
        },
        ..Default::default()
    };
    let (min, max) = (MIN_LOG_LUMINANCE, MAX_LOG_LUMINANCE);
    let mut expected = vec![0u32; 64];
    let mut texels = Vec::new();
    for y in 0..size[1] {
        for x in 0..size[0] {
            let bin = (x * 7 + y * 13) % 64;
            let log_luminance = match bin {
                0 => min - 2.,
                63 => max + 1.,
                _ => min + (max - min) * (bin as f32 - 0.5) / 62.,
            };
            let luminance = log_luminance.exp2();
            texels.push([luminance, luminance, luminance, 1.]);
            let weight = automatic.metering_mask.weights[y * 16 / size[1]][x * 16 / size[0]];
            expected[bin] += (f32::from(weight) / 255. * 16.) as u32;
        }
    }
    let frame = hdr_texture(&device, &queue, size.map(|v| v as u32), &texels);
    let mut exposure = Exposure::new(&device);
    let actual = histogram_of(&device, &queue, &mut exposure, &frame, &automatic);
    assert_eq!(actual, expected);
}

// Defects: adaptation at the wrong speed or in the wrong direction's speed,
// a correction that ignores its limits or its compensation curve (points
// unpacked out of order or off by one), or a history reset that does not
// take the target. The oracle is the authored behaviour: steps of speed ×
// time while far from the target, convergence to the correction that brings
// the metered average to 1, the compensation curve's stops linearly
// interpolated between its points (within the histogram's quarter-stop
// bins), and the limits.
#[test]
fn correction_adapts_at_the_authored_speeds_within_its_limits() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut exposure = Exposure::new(&device);
    let dim = grey(&device, &queue, 1.);
    let bright = grey(&device, &queue, 16.);
    let automatic = AutoExposure {
        speed_brighten: 2.,
        speed_darken: 0.5,
        ..Default::default()
    };
    let frame = |reset| Metering {
        automatic: &automatic,
        stops: 0.,
        delta_time: 0.1,
        reset,
    };
    let bin = 16. / 62.;
    let start = adapt(&device, &queue, &mut exposure, &dim, frame(true));
    assert!(
        start.abs() <= bin,
        "a reset takes the target, 0 stops: {start}"
    );
    // The scene brightens by 4 stops: 0.2 stops a frame until within 1.5.
    let mut correction = start;
    for step in 0..10 {
        let next = adapt(&device, &queue, &mut exposure, &bright, frame(false));
        assert!(
            (next - correction + 0.2).abs() < 1e-4,
            "brightening step {step}: {correction} to {next}"
        );
        correction = next;
    }
    for _ in 0..200 {
        correction = adapt(&device, &queue, &mut exposure, &bright, frame(false));
    }
    assert!(
        (correction + 4.).abs() <= bin,
        "converges to -4 stops: {correction}"
    );
    // Back to the dim scene: 0.05 stops a frame.
    for step in 0..10 {
        let next = adapt(&device, &queue, &mut exposure, &dim, frame(false));
        assert!(
            (next - correction - 0.05).abs() < 1e-4,
            "darkening step {step}: {correction} to {next}"
        );
        correction = next;
    }
    // The authored stops are metered and corrected for: the multiplier is
    // unchanged by them while the correction is unlimited.
    let shifted = Metering {
        stops: 1.,
        ..frame(true)
    };
    let corrected = adapt(&device, &queue, &mut exposure, &bright, shifted);
    assert!(
        (corrected + 1. + 4.).abs() <= bin,
        "1 authored stop is corrected: {corrected}"
    );
    let limited = AutoExposure {
        correction_min: -1.,
        correction_max: 1.,
        ..automatic
    };
    let limited_frame = Metering {
        automatic: &limited,
        ..frame(true)
    };
    // Three points, two to a vector: each metered average takes the stops
    // interpolated on its own segment.
    let points = [[-4., -1.], [0., -3.], [4., -2.]];
    let curved = AutoExposure {
        compensation: CompensationCurve::new(&points).unwrap(),
        ..automatic
    };
    for log_luminance in [-2f32, 2.] {
        let segment = points
            .windows(2)
            .find(|pair| pair[1][0] >= log_luminance)
            .unwrap();
        let t = (log_luminance - segment[0][0]) / (segment[1][0] - segment[0][0]);
        let stops = segment[0][1] + (segment[1][1] - segment[0][1]) * t;
        let scene = grey(&device, &queue, log_luminance.exp2());
        let corrected = adapt(
            &device,
            &queue,
            &mut exposure,
            &scene,
            Metering {
                automatic: &curved,
                ..frame(true)
            },
        );
        assert!(
            (corrected - (stops - log_luminance)).abs() <= bin,
            "average 2^{log_luminance} compensated by {stops} stops: {corrected}"
        );
    }
    let darkest = adapt(&device, &queue, &mut exposure, &bright, limited_frame);
    assert_eq!(darkest, -1., "the correction stops at its lower limit");
    let brightest = adapt(
        &device,
        &queue,
        &mut exposure,
        &grey(&device, &queue, 1. / 64.),
        limited_frame,
    );
    assert_eq!(brightest, 1., "the correction stops at its upper limit");
}
