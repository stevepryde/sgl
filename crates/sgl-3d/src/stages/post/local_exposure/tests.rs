//! Local exposure's oracle is the same tone map at contrasts of 1 on an
//! input scaled by Patry's formula, `2^((c - 1) B)`: no test reimplements
//! the tone map or the grid. Brackets rely only on the tone curves being
//! monotone on grey. Frames are 256 x 128, so the grid's cells are 4 x 4
//! texels.
use super::super::tests::{exposure_texture, look};
use super::super::{HDR, Output, Post, Presentation, Sizes, target};
use crate::LocalExposure;
use crate::test_support::{half, hdr_texture, read};

const SIZE: [u32; 2] = [256, 128];
/// The tone-mapped capture's rounding (binary16, values below 1) and the
/// grid's binary16 sums: well under the smallest effect asserted.
const TOLERANCE: f32 = 2e-3;

/// A post stage presenting `SIZE` frames.
fn post(device: &wgpu::Device) -> Post {
    post_of(device, SIZE)
}

/// A post stage presenting frames of `size`.
fn post_of(device: &wgpu::Device, size: [u32; 2]) -> Post {
    let sizes = Sizes {
        render: size,
        scene: size,
        output: size,
    };
    Post::new(device, Output::texture(HDR), sizes, false)
}

/// A grey frame of `SIZE` whose texel at x, y has the linear value `value(x, y)`.
fn frame(value: impl Fn(u32, u32) -> f32) -> Vec<[f32; 4]> {
    frame_of(SIZE, value)
}

/// A grey frame of `size` whose texel at x, y has the linear value `value(x, y)`.
fn frame_of(size: [u32; 2], value: impl Fn(u32, u32) -> f32) -> Vec<[f32; 4]> {
    (0..size[1])
        .flat_map(|y| (0..size[0]).map(move |x| (x, y)))
        .map(|(x, y)| {
            let v = value(x, y);
            [v, v, v, 1.]
        })
        .collect()
}

/// The tone-mapped capture's red channel of `texels` under an exposure
/// multiplier of `exposure` and local exposure at `contrasts`
/// (highlight, shadow).
fn render(
    post: &mut Post,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texels: &[[f32; 4]],
    exposure: f32,
    [highlight_contrast, shadow_contrast]: [f32; 2],
) -> Vec<f32> {
    let size = post.scene_size;
    let scene = hdr_texture(device, queue, size, texels);
    let exposure = exposure_texture(device, queue, exposure);
    let output = target(device, "local exposure output", size, HDR);
    let mut look = look(&exposure);
    look.local_exposure = LocalExposure {
        highlight_contrast,
        shadow_contrast,
    };
    let mut encoder = device.create_command_encoder(&Default::default());
    post.present(
        (device, &crate::view::compile::Compiler::new(device)),
        queue,
        &mut encoder,
        &scene,
        Presentation {
            bloom: false,
            smaa: None,
            capture: true,
            spatial: false,
            sharpness: None,
        },
        look,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    read(device, queue, post.tone_map.tone_mapped().texture(), 8)
        .chunks_exact(8)
        .map(half)
        .collect()
}

/// The neutral render (contrasts of 1, exposure 1) of a uniform frame of
/// `value`: the tone curve on grey at `value`.
fn neutral(post: &mut Post, device: &wgpu::Device, queue: &wgpu::Queue, value: f32) -> f32 {
    render(post, device, queue, &frame(|_, _| value), 1., [1., 1.])[0]
}

/// Linear grey at `v` stops about middle grey.
fn grey(v: f32) -> f32 {
    0.18 * v.exp2()
}

// Defects: a wrong pivot, the highlight and shadow contrasts swapped, the
// grid's normalisation or range mapping wrong. Oracle: a uniform frame's
// base is its own V, k, so at contrasts c it presents as the neutral frame
// scaled by 2^((c - 1) k): by 2^(-k/2) at 0.5, and unchanged where only
// the other side's contrast is below 1.
#[test]
fn a_uniform_frame_moves_toward_middle_grey_by_the_formula() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    for k in [2f32, 0., -2.] {
        let input = frame(|_, _| grey(k));
        let scaled = neutral(&mut post, &device, &queue, grey(k) * (-k / 2.).exp2());
        let unchanged = neutral(&mut post, &device, &queue, grey(k));
        let other_side = if k > 0. { [1., 0.5] } else { [0.5, 1.] };
        for (contrasts, expected) in [([0.5, 0.5], scaled), (other_side, unchanged)] {
            let actual = render(&mut post, &device, &queue, &input, 1., contrasts);
            for (i, value) in actual.into_iter().enumerate() {
                assert!(
                    (value - expected).abs() <= TOLERANCE,
                    "V {k}, contrasts {contrasts:?}, texel {i}: {value}, expected {expected}"
                );
            }
        }
    }
}

/// Flat halves: V `left` left of the middle column, `right` from it.
fn halves(left: f32, right: f32) -> impl Fn(u32, u32) -> f32 {
    move |x, _| {
        if x < SIZE[0] / 2 {
            grey(left)
        } else {
            grey(right)
        }
    }
}

// Defect: the grid built before the exposure, or without it. Oracle: the
// tone map exposes the scene before local exposure, so a frame at L / e
// under an exposure of e presents as the frame at L under 1, local
// exposure on.
#[test]
fn local_exposure_follows_the_frames_exposure() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    let exposed = render(
        &mut post,
        &device,
        &queue,
        &frame(halves(3., -3.)),
        1.,
        [0.5, 0.5],
    );
    let darker = frame(|x, y| halves(3., -3.)(x, y) / 4.);
    let under = render(&mut post, &device, &queue, &darker, 4., [0.5, 0.5]);
    for (i, (a, b)) in exposed.iter().zip(&under).enumerate() {
        assert!((a - b).abs() <= TOLERANCE, "texel {i}: {b}, expected {a}");
    }
}

// Defects: ringing or overshoot (a filter with negative lobes), a
// Gaussian-only base (a halo beyond the bound), a bilateral-only base (no
// departure beside the edge, the case that rings on gradients). Oracle: a
// normalised blur of a step is monotone and within its levels, and the
// bilateral share (0.4) holds each side's own level up to the edge, since
// the halves lie 4.5 slices apart, beyond the grid blur's and the slice
// interpolation's reach. So at contrasts 0.5, with halves at V = +3 and
// -3: beyond 15 cells of the edge (the Gaussian's 13 and the B-spline's
// 2) each side presents as its plateau scaled by 2^(-+1.5); along a row
// each half's multiplier moves monotonically toward the edge; everywhere
// it stays within 2^(0.5 x 0.6 x 3) of its plateau; and beside the edge it
// departs by at least half that.
#[test]
fn a_step_edge_keeps_each_side_flat_without_ringing() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    let actual = render(
        &mut post,
        &device,
        &queue,
        &frame(halves(3., -3.)),
        1.,
        [0.5, 0.5],
    );
    let bound = 0.5 * 0.6 * 3f32;
    // Per side: V, plateau multiplier, the levels at the plateau, at the
    // bound either way and at half the bound either way.
    let mut sides = Vec::new();
    for v in [3f32, -3.] {
        let plateau = grey(v) * (-0.5 * v).exp2();
        let mut level = |stops: f32| neutral(&mut post, &device, &queue, plateau * stops.exp2());
        let levels = [
            level(0.),
            level(-bound),
            level(bound),
            level(-bound / 2.),
            level(bound / 2.),
        ];
        sides.push(levels);
    }
    let middle = SIZE[0] / 2;
    let far = 15 * 4;
    for y in 0..SIZE[1] {
        let row = &actual[(y * SIZE[0]) as usize..((y + 1) * SIZE[0]) as usize];
        for (x, &value) in row.iter().enumerate() {
            let x = x as u32;
            let [plateau, low, high, _, _] = sides[usize::from(x >= middle)];
            assert!(
                value >= low - TOLERANCE && value <= high + TOLERANCE,
                "{x}, {y}: {value} leaves {low}..={high}"
            );
            if x + far < middle || x >= middle + far {
                assert!(
                    (value - plateau).abs() <= TOLERANCE,
                    "{x}, {y}: {value}, plateau {plateau}"
                );
            }
        }
        // The left side brightens and the right darkens toward the edge:
        // both rise with x.
        for x in 1..SIZE[0] {
            if x != middle {
                let (before, after) = (row[x as usize - 1], row[x as usize]);
                assert!(
                    after >= before - TOLERANCE,
                    "row {y} not monotone at {x}: {before} then {after}"
                );
            }
        }
        let [_, _, _, _, left_half] = sides[0];
        let [_, _, _, right_half, _] = sides[1];
        let (left, right) = (row[middle as usize - 1], row[middle as usize]);
        assert!(
            left > left_half && right < right_half,
            "row {y}: beside the edge {left} and {right} depart less than half the bound \
             ({left_half}, {right_half})"
        );
    }
}

/// The stops, relative to `scale`, by which each texel of `captured`, whose
/// input was `value` at every texel, was multiplied: each capture inverted
/// through the neutral tone curve, sampled by one frame of `value` x
/// `scale` x 2^s with s over -1.2..=1.2 stops (the curve is monotone on
/// grey; a run of equal binary16 values inverts to its middle).
fn stops_of(
    post: &mut Post,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    value: f32,
    scale: f32,
    captured: &[f32],
) -> Vec<f32> {
    let count = (SIZE[0] * SIZE[1]) as usize;
    let at = |i: usize| -1.2 + 2.4 * i as f32 / (count - 1) as f32;
    let ramp = frame(|x, y| value * scale * at((y * SIZE[0] + x) as usize).exp2());
    let curve = render(post, device, queue, &ramp, 1., [1., 1.]);
    captured
        .iter()
        .map(|&c| {
            let below = curve.partition_point(|&t| t < c);
            let through = curve.partition_point(|&t| t <= c);
            assert!(
                below > 0 && through < count,
                "{c} outside the inverted range"
            );
            let middle = (below + through - 1) as f32 / 2.;
            -1.2 + 2.4 * middle / (count - 1) as f32
        })
        .collect()
}

// Defects: the base's Gaussian upsampled in cell-sized blocks (nearest
// neighbour) or not blurred at all, which put a cell's change of the base
// into one texel step. Oracle: the Gaussian of a step of 6 stops rises
// no faster than its kernel's peak, 6 / (sigma sqrt(2 pi)) stops a cell at
// sigma 13/3 cells, which positive, unit-sum smoothing after it (the
// B-spline, the bilinear lookup) cannot steepen; a cell is 4 texels and the
// Gaussian's share 0.6, so at contrasts 0.5 each half's multiplier changes
// by at most 0.5 x 0.6 x 6 / (13/3 sqrt(2 pi)) / 4 = 0.0414 stops a texel,
// plus 0.005 for inverting binary16 captures. A cell-sized block steps by
// about four times that.
#[test]
fn the_base_rises_smoothly_across_a_step() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    let actual = render(
        &mut post,
        &device,
        &queue,
        &frame(halves(3., -3.)),
        1.,
        [0.5, 0.5],
    );
    let middle = (SIZE[0] / 2) as usize;
    let sigma = 13f32 / 3.;
    let bound = 0.5 * 0.6 * 6. / (sigma * (2. * std::f32::consts::PI).sqrt()) / 4. + 0.005;
    let mut steepest = 0f32;
    for (v, columns) in [(3f32, 0..middle), (-3., middle..SIZE[0] as usize)] {
        let row = |y: usize| &actual[y * SIZE[0] as usize..][columns.clone()];
        for y in [0, SIZE[1] as usize / 2, SIZE[1] as usize - 1] {
            let stops = stops_of(
                &mut post,
                &device,
                &queue,
                grey(v),
                (-0.5 * v).exp2(),
                row(y),
            );
            for (x, pair) in stops.windows(2).enumerate() {
                let step = (pair[1] - pair[0]).abs();
                steepest = steepest.max(step);
                assert!(
                    step <= bound,
                    "V {v}, row {y}, texel {x} of its half: the multiplier steps {step} stops, above {bound}"
                );
            }
        }
    }
    eprintln!("steepest texel step {steepest} stops against {bound}");
}

// Defect: a cell partition that misses or drops texels where the scene's
// size is not a multiple of the cell count (truncated cell widths, an
// off-by-one end). Oracle, on a 250 x 130 frame (64 x 34 cells, neither a
// multiple), with a bright column at the right edge and a bright row at
// the bottom on a dark field: the last two columns and rows share their
// edge cells under any partition into cells at least three texels wide,
// and Patry's build weights texels by slice alone, so moving the column
// and row between them changes no other texel beyond a binary16 step; and
// the column and row must count, so removing them moves texels a few
// cells inside: the edge cells' mean V moves by at least 8 / 4 = 2 stops,
// which the Gaussian (weight at least 0.9 / (13/3 sqrt(2 pi)) per cell two
// cells away) and its share at contrast 0.5 turn into at least 0.05 stops
// there, which on the dark field's tone curve is over 1%.
#[test]
fn every_texel_of_an_uneven_partition_counts_once() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [250, 130];
    let mut post = post_of(&device, size);
    let mut render_with = |bright: Option<u32>| {
        let input = frame_of(size, |x, y| {
            let edge = bright.is_some_and(|back| x == size[0] - back || y == size[1] - back);
            if edge { grey(5.) } else { grey(-3.) }
        });
        render(&mut post, &device, &queue, &input, 1., [0.5, 0.5])
    };
    let last = render_with(Some(1));
    let moved = render_with(Some(2));
    let none = render_with(None);
    let at = |image: &[f32], [x, y]: [u32; 2]| image[(y * size[0] + x) as usize];
    for y in 0..size[1] - 2 {
        for x in 0..size[0] - 2 {
            let (a, b) = (at(&last, [x, y]), at(&moved, [x, y]));
            assert!(
                (a - b).abs() <= a.abs() / 1024.,
                "{x}, {y}: {a} with the edge texels last, {b} one in"
            );
        }
    }
    for probe in [[240, 60], [120, 122]] {
        let (with, without) = (at(&last, probe), at(&none, probe));
        assert!(
            (with - without).abs() > 0.01 * without,
            "{probe:?}: {with} with the edge column and row, {without} without: they did not count"
        );
    }
}

// Defect: contrast scaled per pixel instead of per region. Oracle: the same
// halves with a one-texel checker a quarter stop above and below each
// level keep their regions' bases (a cell's mean is its level; the grid
// slice at a texel's own V lies within the checker's two levels), so far
// from the edge every texel presents within 2^(+-(1 - c) x 0.4 x 0.25) =
// 2^(+-0.05) of its own value times its region's multiplier, where a
// per-pixel scaling would move it 0.125 stops.
#[test]
fn local_detail_is_kept_whole() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    let detail = |x: u32, y: u32| {
        let level = if x < SIZE[0] / 2 { 3. } else { -3. };
        let offset = if (x + y).is_multiple_of(2) {
            0.25
        } else {
            -0.25
        };
        (level, level + offset)
    };
    let input = frame(|x, y| grey(detail(x, y).1));
    let actual = render(&mut post, &device, &queue, &input, 1., [0.5, 0.5]);
    let spread = 0.5 * 0.4 * 0.25f32;
    let mut brackets = std::collections::HashMap::new();
    let far = 15 * 4;
    let middle = SIZE[0] / 2;
    for y in 0..SIZE[1] {
        for x in (0..SIZE[0]).filter(|&x| x + far < middle || x >= middle + far) {
            let (level, v) = detail(x, y);
            let [low, high] = *brackets.entry(v.to_bits()).or_insert_with(|| {
                let scaled = grey(v) * (-0.5 * level).exp2();
                [-spread, spread].map(|s| neutral(&mut post, &device, &queue, scaled * s.exp2()))
            });
            let value = actual[(y * SIZE[0] + x) as usize];
            assert!(
                value >= low - TOLERANCE / 4. && value <= high + TOLERANCE / 4.,
                "{x}, {y} (V {v}): {value} leaves {low}..={high}"
            );
        }
    }
}

// Defect: a subsampled or position-weighted build, which flickers as small
// features move. Oracle: Patry's build weights texels by slice alone, not
// by position within a cell, so one bright texel moved within its cell
// leaves every other texel as it was, within a binary16 step.
#[test]
fn a_feature_moving_within_its_cell_changes_nothing_else() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    let positions = [[100, 60], [103, 63]];
    let frames = positions.map(|[bx, by]| {
        let input = frame(|x, y| {
            if [x, y] == [bx, by] {
                grey(5.)
            } else {
                grey(-3.)
            }
        });
        render(&mut post, &device, &queue, &input, 1., [0.5, 0.5])
    });
    for (i, (a, b)) in frames[0].iter().zip(&frames[1]).enumerate() {
        let [x, y] = [i as u32 % SIZE[0], i as u32 / SIZE[0]];
        if positions.contains(&[x, y]) {
            continue;
        }
        assert!((a - b).abs() <= a.abs() / 1024., "{x}, {y}: {a} then {b}");
    }
}

// Defect: local exposure's targets made, or kept, on frames that do not
// run it, or kept across a resize. Oracle: Frame item 15, "at contrasts
// of 1 it runs no pass, holds no target and takes no sample"; contrasts
// are clamped to 0..=1 and non-finite ones are 1.
#[test]
fn targets_are_held_only_while_a_contrast_is_below_1() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut post = post(&device);
    assert!(!post.local_exposure.holds_targets());
    let input = frame(halves(3., -3.));
    for (contrasts, held) in [
        ([1., 1.], false),
        ([0.75, 1.], true),
        ([1., 1.], false),
        ([f32::NAN, 1.5], false),
        ([1., 0.5], true),
    ] {
        render(&mut post, &device, &queue, &input, 1., contrasts);
        assert_eq!(
            post.local_exposure.holds_targets(),
            held,
            "contrasts {contrasts:?}"
        );
    }
    post.resize(
        &device,
        Sizes {
            render: SIZE,
            scene: SIZE,
            output: SIZE,
        },
        false,
    );
    assert!(!post.local_exposure.holds_targets(), "kept across a resize");
    assert!(
        !post.tone_map.binds_local_exposure(),
        "the tone map's lookup kept the released views across a resize"
    );
}
