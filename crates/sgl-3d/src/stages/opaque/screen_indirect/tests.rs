//! The pass over the analytic plane-and-wall fixture (`test_geometry`), and
//! the kept result's read over authored texels.
use super::{Inputs, ScreenIndirect};
use crate::settings::ScreenSpaceIndirect;
use crate::stages::opaque::ambient_occlusion::AmbientOcclusion;
use crate::stages::opaque::test_geometry::{PlaneAndWall, Wall, plane_and_wall, wall_visibility};
use crate::view::history::{CameraFrame, HistoryFrame};
use glam::{Mat4, Vec3, Vec4};

const SIZE: [u32; 2] = [129, 113];
/// The least reach the pass takes, which the fixture's wall spans.
const RADIUS: f32 = 1.;

fn projection() -> Mat4 {
    crate::perspective(60f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1)
}

/// The camera's view of the fixture: turned 180° about y with the scene
/// where `rotated` (`PlaneAndWall::rotated`).
fn camera_view(rotated: bool) -> Mat4 {
    if rotated {
        Mat4::from_diagonal(Vec4::new(-1., 1., -1., 1.))
    } else {
        Mat4::IDENTITY
    }
}

/// A frame's history: the camera at the origin seeing the fixture turned
/// as `rotated` says, unmoved since the last frame where `valid`, else
/// restarted.
fn history(valid: bool, rotated: bool) -> HistoryFrame {
    let camera = CameraFrame {
        view: camera_view(rotated),
        projection: projection(),
        jitter: [0.; 2],
    };
    let stable = camera.projection * camera.view;
    HistoryFrame {
        stable,
        previous: stable,
        previous_camera: valid.then_some(camera),
        valid,
        frames: u32::from(valid),
        camera,
        origin: glam::DVec3::ZERO,
        elapsed_seconds: 0.,
        previous_elapsed_seconds: 0.,
        visibility_mask: 0,
        visibility_changed: false,
        backdrop_yaw: None,
        previous_backdrop_yaw: 0.,
    }
}

/// The kept result one frame wrote: each half-size texel's light and
/// coverage, and its geometry (depth, count, and the normal's octahedral
/// coordinates).
struct KeptTexels {
    light: Vec<[f32; 4]>,
    geometry: Vec<[f32; 4]>,
}

impl KeptTexels {
    fn half() -> [u32; 2] {
        SIZE.map(|extent| extent.div_ceil(2))
    }

    fn at(&self, x: u32, y: u32) -> [f32; 4] {
        self.light[(y * Self::half()[0] + x) as usize]
    }
}

/// The kept texels of `bytes`, Rgba32Uint as `screen_indirect_pack` packs
/// them: light and coverage, then depth, count and the normal's two
/// signed 16-bit coordinates.
fn texels(bytes: &[u8]) -> KeptTexels {
    let half = |at: &[u8]| crate::test_support::half(at);
    let snorm = |at: &[u8]| f32::from(i16::from_le_bytes([at[0], at[1]])) / 32767.;
    let (light, geometry) = bytes
        .chunks_exact(16)
        .map(|t| {
            (
                [half(&t[0..]), half(&t[2..]), half(&t[4..]), half(&t[6..])],
                [
                    half(&t[8..]),
                    half(&t[10..]),
                    snorm(&t[12..]),
                    snorm(&t[14..]),
                ],
            )
        })
        .unzip();
    KeptTexels { light, geometry }
}

/// The pass and the depth pyramid it reads, over one device.
struct Pass {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pyramid: AmbientOcclusion,
    stage: ScreenIndirect,
    motion: wgpu::TextureView,
}

impl Pass {
    fn new() -> Option<Self> {
        let (device, queue) = crate::test_support::device()?;
        // No motion: no history is taken.
        let motion = crate::test_support::hdr_texture(
            &device,
            &queue,
            SIZE,
            &vec![[0.; 4]; (SIZE[0] * SIZE[1]) as usize],
        );
        Some(Self {
            pyramid: AmbientOcclusion::new(&device, &queue),
            stage: ScreenIndirect::new(&device),
            motion,
            device,
            queue,
        })
    }

    /// A frame of `fixture` at `level` and `radius` with `history`, settled
    /// as `finished`: what it kept, and the half of the pair it wrote.
    fn frame(
        &mut self,
        fixture: &PlaneAndWall,
        (level, radius): (ScreenSpaceIndirect, f32),
        history: &HistoryFrame,
        finished: bool,
    ) -> (KeptTexels, wgpu::TextureView) {
        let targets = plane_and_wall(&self.device, &self.queue, SIZE, projection(), fixture);
        self.frame_of(targets, fixture.rotated, (level, radius), history, finished)
    }

    /// `frame` of the depth, normals and lit colour `targets`, seen by a
    /// camera turned as `rotated` says (`camera_view`).
    fn frame_of(
        &mut self,
        [depth, normals, color]: [wgpu::TextureView; 3],
        rotated: bool,
        (level, radius): (ScreenSpaceIndirect, f32),
        history: &HistoryFrame,
        finished: bool,
    ) -> (KeptTexels, wgpu::TextureView) {
        let Self {
            device,
            queue,
            pyramid,
            stage,
            motion,
        } = self;
        let settings = crate::view::effective::ScreenIndirect {
            level,
            radius,
            pyramid_radius: radius,
        };
        stage.begin(device, queue, Some(settings), SIZE, history);
        let mut view: crate::shading::uniforms::ViewUniform = bytemuck::Zeroable::zeroed();
        view.view = camera_view(rotated).to_cols_array_2d();
        view.projection = projection().to_cols_array_2d();
        let mut encoder = device.create_command_encoder(&Default::default());
        pyramid.encode_depth_pyramid(
            device,
            queue,
            &mut encoder,
            &depth,
            &view,
            radius,
            (None, super::TIMING_GROUP),
        );
        let inputs = Inputs {
            depth_pyramid: pyramid.depth_view().unwrap(),
            hilbert: pyramid.hilbert(),
            depth: &depth,
            normals: &normals,
            color: &color,
            motion,
        };
        stage.encode(
            device,
            queue,
            &mut encoder,
            &inputs,
            &view,
            history,
            settings,
            None,
        );
        queue.submit([encoder.finish()]);
        let written = stage.written().unwrap().clone();
        let bytes = crate::test_support::read(device, queue, written.texture(), 16);
        stage.settle(finished);
        (texels(&bytes), written)
    }

    /// The first frame of `fixture` at `level` with the least radius.
    fn first(&mut self, fixture: &PlaneAndWall, level: ScreenSpaceIndirect) -> KeptTexels {
        self.frame(
            fixture,
            (level, RADIUS),
            &history(false, fixture.rotated),
            true,
        )
        .0
    }
}

/// The plane's receivers beside the fixture's wall at which the oracle is
/// held: three rows, every texel whose receiver lies 0.08 to 0.95 m from
/// the wall, with that distance.
fn beside_wall() -> Vec<([u32; 2], f64)> {
    let p00 = f64::from(projection().x_axis.x);
    let mut receivers = Vec::new();
    for y in [20, 28, 36] {
        for x in 0..KeptTexels::half()[0] {
            let pixel = f64::from(2 * x) + 0.5;
            let world_x = (pixel / f64::from(SIZE[0]) * 2. - 1.) * 3. / p00;
            let distance = 0.6 - world_x;
            if (0.08..=0.95).contains(&distance) {
                receivers.push(([x, y], distance));
            }
        }
    }
    receivers
}

fn red_wall(wall: Wall) -> PlaneAndWall {
    PlaneAndWall {
        wall: Some(wall),
        ..PlaneAndWall::plane()
    }
}

/// The mean and largest absolute error of `measured` against `expected`.
fn errors(pairs: impl Iterator<Item = (f64, f64)>) -> (f64, f64) {
    let errors: Vec<f64> = pairs.map(|(m, e)| (m - e).abs()).collect();
    assert!(!errors.is_empty());
    (
        errors.iter().sum::<f64>() / errors.len() as f64,
        errors.iter().copied().fold(0., f64::max),
    )
}

// Defect: the sector weights, the slice setup, the thickness or the reach
// do not integrate what the receiver's cosine-weighted hemisphere sees, so
// the bounce is too dim, too bright or missing (a gain stands in for a
// measure). Oracle: an independent f64 ray integral over the fixture's
// planes (`wall_visibility`): on the plane, with the wall the only occluder
// and the only emitter, of radiance 1 in red, light / L and coverage both
// equal one less the unoccluded share of the cosine-weighted hemisphere
// within the radius. Budgets are ambient occlusion's for this fixture, and
// a result of zero fails both.
#[test]
fn gathered_light_matches_an_independent_hemisphere_integral() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        for rotated in [false, true] {
            let fixture = PlaneAndWall {
                rotated,
                ..red_wall(Wall::Facing([1., 0., 0.]))
            };
            let kept = pass.first(&fixture, level);
            let receivers = beside_wall();
            let expected = |distance: f64| 1. - wall_visibility(distance, f64::from(RADIUS));
            let light = errors(
                receivers
                    .iter()
                    .map(|&([x, y], d)| (f64::from(kept.at(x, y)[0]), expected(d))),
            );
            let coverage = errors(
                receivers
                    .iter()
                    .map(|&([x, y], d)| (f64::from(kept.at(x, y)[3]), expected(d))),
            );
            let zero = errors(receivers.iter().map(|&(_, d)| (0., expected(d))));
            println!(
                "{level:?} rotated={rotated}: light MAE {:.4} max {:.4}; coverage MAE {:.4} max {:.4}; zero MAE {:.4} max {:.4}",
                light.0, light.1, coverage.0, coverage.1, zero.0, zero.1
            );
            assert!(
                zero.0 > 0.16 && zero.1 > 0.30,
                "the fixture must reject zero"
            );
            for (name, (mean, max)) in [("light", light), ("coverage", coverage)] {
                assert!(mean <= 0.16, "{level:?} {name} mean error {mean}");
                assert!(max <= 0.30, "{level:?} {name} largest error {max}");
            }
        }
    }
}

// Defect: the search spans less than the radius that cuts its samples (as
// SSRT3's halved projection of it does, D-65), so a surface between half
// the radius and the radius from a receiver goes unseen. Oracle: the
// hemisphere integral (`wall_visibility`) is above 0 for every receiver
// closer to the wall than the 1 m radius, so on the plane every receiver
// 0.6 to 0.95 m from the wall takes some coverage, at both levels.
#[test]
fn receivers_toward_the_radius_take_coverage() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        let kept = pass.first(&red_wall(Wall::Facing([1., 0., 0.])), level);
        let far: Vec<_> = beside_wall()
            .into_iter()
            .filter(|&(_, distance)| (0.6..=0.95).contains(&distance))
            .collect();
        assert!(!far.is_empty());
        for ([x, y], distance) in far {
            assert!(
                1. - wall_visibility(distance, f64::from(RADIUS)) > 0.,
                "the oracle must see the wall at {distance} m"
            );
            assert!(
                kept.at(x, y)[3] > 0.,
                "{level:?}: the receiver {distance:.3} m from the wall takes no coverage"
            );
        }
    }
}

// Defect: a plane covers or lights itself (its own samples at its horizon
// counted, or the thickness turned toward the camera). Oracle: a lit plane
// with nothing on it is unoccluded, its mean coverage within ambient
// occlusion's open-plane budget, and gives itself no light.
#[test]
fn an_open_plane_gathers_nothing() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        let fixture = PlaneAndWall {
            plane_color: [1.; 3],
            ..PlaneAndWall::plane()
        };
        let kept = pass.first(&fixture, level);
        let mean =
            kept.light.iter().map(|t| f64::from(t[3])).sum::<f64>() / kept.light.len() as f64;
        println!("{level:?}: open plane mean coverage {mean:.6}");
        assert!(mean <= 0.025, "{level:?}: open plane coverage {mean}");
        assert!(
            kept.light.iter().all(|t| t[..3] == [0.; 3]),
            "{level:?}: the plane lit itself"
        );
    }
}

// Defect: the bounce mixes channels, or a weight that is not a measure
// (a gain, a sum past the hemisphere) makes light. Oracle: under an emitter
// red only, every receiver's green and blue are exactly 0, its coverage
// lies within 0 to 1, and its light is at most the brightest lit colour in
// reach (1) times its coverage.
#[test]
fn the_bounce_keeps_its_emitters_colour_and_creates_no_energy() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        let kept = pass.first(&red_wall(Wall::Facing([1., 0., 0.])), level);
        let mut lit = 0;
        for texel in &kept.light {
            let [r, g, b, coverage] = *texel;
            assert_eq!([g, b], [0.; 2], "{level:?}: another channel lit");
            assert!(
                (0. ..=1.).contains(&coverage),
                "{level:?}: coverage {coverage}"
            );
            // Half-float rounding of each.
            assert!(
                r <= coverage * 1.001 + 1e-3,
                "{level:?}: light {r} past coverage {coverage}"
            );
            lit += usize::from(r > 0.);
        }
        assert!(lit > 0, "{level:?}: nothing was lit");
    }
}

// Defect: an emitter's light reaches a receiver it faces away from (the
// emitter's normal ignored), or the facing test also drops its coverage.
// Oracle: a lit card 1 m from the camera, facing it and so away from the
// plane 2 m behind it, within a 3 m radius, gives the plane exactly no
// light and still covers the plane beside it.
#[test]
fn a_surface_facing_away_gives_no_light() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    let rows = 50..65;
    let fixture = PlaneAndWall {
        plate: Some(rows.clone()),
        plate_color: [1., 0., 0.],
        ..PlaneAndWall::plane()
    };
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        let (kept, _) = pass.frame(&fixture, (level, 3.), &history(false, false), true);
        for (index, texel) in kept.light.iter().enumerate() {
            assert_eq!(texel[..3], [0.; 3], "{level:?}: texel {index} lit");
        }
        // The plane's rows beside the card.
        let beside = (0..KeptTexels::half()[0]).flat_map(|x| {
            [
                kept.at(x, rows.start / 2 - 2)[3],
                kept.at(x, rows.end / 2 + 2)[3],
            ]
        });
        let mean = beside.clone().sum::<f32>() / beside.count() as f32;
        println!("{level:?}: coverage beside the card {mean:.4}");
        assert!(mean > 0.05, "{level:?}: the card covers nothing: {mean}");
    }
}

// Defect: the sky, whose depth is the pyramid's largest, occludes or lights
// what lies beside it. Oracle: with bright sky where the wall stood, the
// plane takes exactly no light and no coverage, and nothing is kept at the
// sky.
#[test]
fn the_sky_gives_no_light_and_covers_nothing() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        let kept = pass.first(&red_wall(Wall::Sky([8.; 3])), level);
        let p00 = projection().x_axis.x;
        for y in 0..KeptTexels::half()[1] {
            for x in 0..KeptTexels::half()[0] {
                let ray_x = ((2 * x) as f32 + 0.5) / SIZE[0] as f32 * 2. - 1.;
                let sky = ray_x > 0. && 0.6 / (ray_x / p00) < 3.;
                let index = (y * KeptTexels::half()[0] + x) as usize;
                assert_eq!(
                    kept.light[index], [0.; 4],
                    "{level:?}: ({x}, {y}) sky={sky}"
                );
                if sky {
                    assert_eq!(kept.geometry[index], [0.; 4], "{level:?}: kept at the sky");
                }
            }
        }
    }
}

// Defect: the search's lack of falloff lets a near surface light and
// occlude whatever lies behind it on screen at any distance. Oracle: a lit
// card 2 m in front of the plane, past the 1 m radius, leaves the plane
// beside it exactly unlit and uncovered.
#[test]
fn a_surface_beyond_the_radius_gives_no_light_and_covers_nothing() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    let rows = 50..65;
    for level in [ScreenSpaceIndirect::Low, ScreenSpaceIndirect::High] {
        let fixture = PlaneAndWall {
            plate: Some(rows.clone()),
            plate_color: [4.; 3],
            ..PlaneAndWall::plane()
        };
        let kept = pass.first(&fixture, level);
        let mut checked = 0;
        for y in 0..KeptTexels::half()[1] {
            if rows.contains(&(2 * y)) {
                continue;
            }
            for x in 0..KeptTexels::half()[0] {
                assert_eq!(kept.at(x, y), [0.; 4], "{level:?}: plane ({x}, {y})");
                checked += 1;
            }
        }
        assert!(checked > 0);
    }
}

/// EXT_texture_shared_exponent's decoding of `word`, as the gather reads
/// a bounce source's colour.
fn rgb9e5(word: u32) -> [f64; 3] {
    let scale = 2f64.powi((word >> 27) as i32 - 15 - 9);
    [0, 9, 18].map(|shift| f64::from((word >> shift) & 511) * scale)
}

// Defect: the bounce sources' levels are sized otherwise than the texture
// that holds them, so where a level's last row or column of the frame
// falls outside it (an odd half size), the prefilter drops its writes there
// and the gather loads outside the texture: the frame's bottom and right
// edges take light from nothing, or from texels that are not theirs.
// Oracle: an independent mean of the lit colour over the full-size pixels
// each texel covers, the frame's edge pixels repeated past it as the
// prefilter's clamped loads repeat them. Every texel of every level that a
// pixel of the frame falls in holds that mean, and the gather's load at the
// frame's bottom-right pixel returns it at each level. The fixture's
// 129x113 frame leaves every level's last column and row partly outside it.
#[test]
fn every_bounce_source_level_holds_the_frame_to_its_edges() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    // Lit colour that differs in every pixel, facing the camera.
    let color = |x: u32, y: u32| {
        [
            (f64::from(x) + 0.5) / f64::from(SIZE[0]),
            (f64::from(y) + 0.5) / f64::from(SIZE[1]),
            0.25,
        ]
    };
    let targets = crate::stages::opaque::test_geometry::rasterize(
        &pass.device,
        &pass.queue,
        SIZE,
        &format!(
            "var o:Output; o.normal=gbuffer_encode_normals(vec3(0.,0.,1.),vec3(0.,0.,1.)); o.color=vec4(pixel.x/{w}.,pixel.y/{h}.,0.25,1.); o.depth=0.1/3.; return o;",
            w = SIZE[0],
            h = SIZE[1],
        ),
    );
    pass.frame_of(
        targets,
        false,
        (ScreenSpaceIndirect::Low, RADIUS),
        &history(false, false),
        true,
    );
    // The mean colour of level `level`'s texel `[x, y]`.
    let mean = |level: u32, x: u32, y: u32| {
        let side = 2u32 << level;
        let mut sum = [0.; 3];
        for py in y * side..(y + 1) * side {
            for px in x * side..(x + 1) * side {
                let c = color(px.min(SIZE[0] - 1), py.min(SIZE[1] - 1));
                for channel in 0..3 {
                    sum[channel] += c[channel];
                }
            }
        }
        sum.map(|total| total / f64::from(side * side))
    };
    let close = |got: [f64; 3], want: [f64; 3]| {
        // Shared-exponent rounding: a 512th of the largest channel.
        got.iter()
            .zip(want)
            .all(|(g, w)| (g - w).abs() <= 2. / 512. * 0.8 + 1e-6)
    };
    let targets = pass.stage.targets.as_ref().unwrap();
    let sources = targets.sources.texture();
    for level in 0..super::SOURCE_MIPS {
        let extent = sources
            .size()
            .mip_level_size(level, wgpu::TextureDimension::D2);
        let covered = SIZE.map(|side| side.div_ceil(2 << level));
        assert!(
            extent.width >= covered[0] && extent.height >= covered[1],
            "level {level} holds {}x{} texels; the frame falls in {covered:?}",
            extent.width,
            extent.height
        );
        let bytes = crate::test_support::read_mip(&pass.device, &pass.queue, sources, level);
        for y in 0..covered[1] {
            for x in 0..covered[0] {
                let at = ((y * extent.width + x) * 8) as usize;
                let word = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                let (got, want) = (rgb9e5(word), mean(level, x, y));
                assert!(
                    close(got, want),
                    "level {level} texel ({x}, {y}) holds {got:?}, the mean of its pixels {want:?}"
                );
            }
        }
    }
    // The gather's load at the frame's bottom-right pixel, at each depth mip.
    let corner = SIZE.map(|side| (f64::from(side) - 0.5) / f64::from(side));
    let queries: Vec<[f32; 4]> = (0..=4)
        .map(|mip| [corner[0] as f32, corner[1] as f32, mip as f32, 0.])
        .collect();
    let module = pass
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounce source load"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    crate::shading::compose(&[&super::SCREEN_INDIRECT_PASS]),
                    r#"
@group(1) @binding(0) var<storage,read> queries:array<vec4<f32>>;
@group(1) @binding(1) var<storage,read_write> loaded:array<vec4<f32>>;
@compute @workgroup_size(1)
fn observe_source(@builtin(global_invocation_id) id:vec3<u32>) {
 let query=queries[id.x];
 loaded[id.x]=vec4(load_source(query.xy,u32(query.z)).color,0.);
}
"#
                )
                .into(),
            ),
        });
    let pipeline = pass
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("bounce source load"),
            layout: None,
            module: &module,
            entry_point: Some("observe_source"),
            compilation_options: Default::default(),
            cache: None,
        });
    let inputs = crate::scene::buffer(
        &pass.device,
        "source queries",
        bytemuck::cast_slice(&queries),
        wgpu::BufferUsages::STORAGE,
    );
    let loaded = pass.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("loaded sources"),
        size: (queries.len() * 16) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let group = |index, entries: &[(u32, wgpu::BindingResource<'_>)]| {
        pass.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bounce source load"),
            layout: &pipeline.get_bind_group_layout(index),
            entries: &entries
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: resource.clone(),
                })
                .collect::<Vec<_>>(),
        })
    };
    let pass_inputs = group(
        0,
        &[
            (0, pass.stage.params.as_entire_binding()),
            (21, wgpu::BindingResource::TextureView(&targets.sources)),
        ],
    );
    let observed = group(
        1,
        &[
            (0, inputs.as_entire_binding()),
            (1, loaded.as_entire_binding()),
        ],
    );
    let mut encoder = pass.device.create_command_encoder(&Default::default());
    {
        let mut compute = encoder.begin_compute_pass(&Default::default());
        compute.set_pipeline(&pipeline);
        compute.set_bind_group(0, &pass_inputs, &[]);
        compute.set_bind_group(1, &observed, &[]);
        compute.dispatch_workgroups(queries.len() as u32, 1, 1);
    }
    pass.queue.submit([encoder.finish()]);
    let words = crate::test_support::read_words(&pass.device, &pass.queue, &loaded);
    let last = SIZE.map(|side| side - 1);
    for mip in 0..=4u32 {
        let level = mip.max(1) - 1;
        let got: [f64; 3] =
            std::array::from_fn(|c| f64::from(f32::from_bits(words[mip as usize * 4 + c])));
        let want = mean(level, last[0] >> (level + 1), last[1] >> (level + 1));
        assert!(
            close(got, want),
            "depth mip {mip}: the bottom-right pixel loads {got:?}, its texel's mean {want:?}"
        );
    }
}

// Defect: a fragment reads the kept result at the wrong place (its current
// position, flipped rows, the wrong texel scale), or reads across an edge
// into another surface's light, or reads where nothing passes. Oracle:
// authored texels of two surfaces, red at 2 m on the left and green at 4 m
// on the right, both facing the camera, and a row whose count is 0: a
// point on either surface reads its own light and coverage exactly, at its
// centre and at the edge where its footprint straddles the other; a point
// between them, one behind the camera, one off screen, one whose normal
// opposes the texels', and one over the uncounted row read none.
#[test]
fn a_fragment_reads_the_light_kept_at_its_previous_position_and_none_across_an_edge() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let render = [16u32, 8];
    let half = [8u32, 4];
    let projection = crate::perspective(1., 2., 0.1);
    let red = [1., 0., 0., 0.5];
    let green = [0., 1., 0., 0.25];
    let mut light = Vec::new();
    let mut geometry = Vec::new();
    for y in 0..half[1] {
        for x in 0..half[0] {
            let (texel, depth) = if x < 4 { (red, 2.) } else { (green, 4.) };
            light.push(texel);
            // Normal +z, octahedral (0, 0); the last row counts nothing.
            geometry.push([depth, if y == 3 { 0. } else { 4. }, 0., 0.]);
        }
    }
    let kept = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("authored kept result"),
        size: wgpu::Extent3d {
            width: half[0],
            height: half[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // As screen_indirect_pack packs them: half floats in pairs, and the
    // normal's octahedral (0, 0) as two signed 16-bit zeros.
    let pair = |a: f32, b: f32| {
        u32::from(crate::test_support::to_half(a))
            | u32::from(crate::test_support::to_half(b)) << 16
    };
    let words: Vec<u32> = light
        .iter()
        .zip(&geometry)
        .flat_map(|(l, g): (&[f32; 4], &[f32; 4])| {
            [pair(l[0], l[1]), pair(l[2], l[3]), pair(g[0], g[1]), 0]
        })
        .collect();
    queue.write_texture(
        kept.as_image_copy(),
        bytemuck::cast_slice(&words),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(half[0] * 16),
            rows_per_image: Some(half[1]),
        },
        kept.size(),
    );
    let view = super::KeptView::of(
        &CameraFrame {
            view: Mat4::IDENTITY,
            projection,
            jitter: [0.; 2],
        },
        render,
    );
    // A point at render pixel position `pixel` (centres at .5) and view
    // depth `depth`, as its last frame's clip position.
    let clip = |pixel: [f32; 2], depth: f32| {
        let ndc = [
            pixel[0] / render[0] as f32 * 2. - 1.,
            1. - pixel[1] / render[1] as f32 * 2.,
        ];
        let point = Vec3::new(
            ndc[0] * depth / projection.x_axis.x,
            ndc[1] * depth / projection.y_axis.y,
            -depth,
        );
        projection * point.extend(1.)
    };
    let facing = Vec4::Z;
    let none = [0.; 4];
    let cases = [
        ("left centre", clip([2.5, 2.5], 2.), facing, red),
        ("right centre", clip([12.5, 2.5], 4.), facing, green),
        // Texel 3.5: half its weight on each surface's texels.
        ("left at the edge", clip([7.5, 2.5], 2.), facing, red),
        ("right at the edge", clip([7.5, 2.5], 4.), facing, green),
        ("between them", clip([7.5, 2.5], 3.), facing, none),
        (
            "behind the camera",
            Vec4::new(0., 0., 0.1, -1.),
            facing,
            none,
        ),
        ("off screen", clip([-4., 2.5], 2.), facing, none),
        ("opposed", clip([2.5, 2.5], 2.), -Vec4::Z, none),
        ("uncounted row", clip([2.5, 6.5], 2.), facing, none),
    ];
    let queries: Vec<[[f32; 4]; 2]> = cases
        .iter()
        .map(|&(_, c, n, _)| [c.to_array(), n.to_array()])
        .collect();
    let uniform = crate::scene::buffer(
        &device,
        "kept view",
        bytemuck::bytes_of(&view),
        wgpu::BufferUsages::UNIFORM,
    );
    let inputs = crate::scene::buffer(
        &device,
        "reads",
        bytemuck::cast_slice(&queries),
        wgpu::BufferUsages::STORAGE,
    );
    let results = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read light"),
        size: (cases.len() * 16) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let source = format!(
        "{}\n{}",
        crate::shading::compose(&[&crate::shading::SCREEN_INDIRECT]),
        r#"
@group(0) @binding(0) var kept:texture_2d<u32>;
@group(0) @binding(1) var<uniform> kept_view:ScreenIndirectView;
@group(0) @binding(2) var<storage,read> queries:array<array<vec4<f32>,2>>;
@group(0) @binding(3) var<storage,read_write> results:array<vec4<f32>>;
@compute @workgroup_size(1)
fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 let query=queries[id.x];
 let taken=screen_indirect_read(kept,kept_view,query[0],query[1].xyz);
 results[id.x]=vec4(taken.light,taken.coverage);
}
"#
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kept result read"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("kept result read"),
        layout: None,
        module: &module,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let kept_view = kept.create_view(&Default::default());
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kept result read"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&kept_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: inputs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: results.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(cases.len() as u32, 1, 1);
    }
    queue.submit([encoder.finish()]);
    let words = crate::test_support::read_words(&device, &queue, &results);
    for (index, (name, _, _, expected)) in cases.iter().enumerate() {
        let read: Vec<f32> = words[index * 4..index * 4 + 4]
            .iter()
            .map(|&w| f32::from_bits(w))
            .collect();
        for (channel, (&got, &want)) in read.iter().zip(expected).enumerate() {
            assert!(
                (got - want).abs() <= 1e-6,
                "{name}: channel {channel} read {got}, expected {want}"
            );
        }
    }
}

// Defect: a frame rendered and not finished advances the pair, so the next
// frame reads what the abandoned frame wrote, or writes over the last
// finished frame's result (SI-6). Oracle: after a finished frame of the
// red wall and an unfinished frame of the plane alone, the next frame
// reads the half the finished frame wrote, still holding exactly what it
// kept, and writes the half the unfinished frame wrote.
#[test]
fn the_kept_result_survives_an_unfinished_frame() {
    let Some(mut pass) = Pass::new() else {
        return;
    };
    let settings = (ScreenSpaceIndirect::Low, RADIUS);
    let (finished, _) = pass.frame(
        &red_wall(Wall::Facing([1., 0., 0.])),
        settings,
        &history(false, false),
        true,
    );
    let kept_bytes = |pass: &Pass| {
        let read = pass.stage.read().expect("the frame continues the last");
        (
            read.half,
            crate::test_support::read(&pass.device, &pass.queue, read.kept.texture(), 16),
        )
    };
    let plane = PlaneAndWall::plane();
    let (_, abandoned_written) = pass.frame(&plane, settings, &history(true, false), false);
    // The next frame begins as the abandoned one did.
    let settings_frame = crate::view::effective::ScreenIndirect {
        level: settings.0,
        radius: settings.1,
        pyramid_radius: settings.1,
    };
    assert!(pass.stage.begin(
        &pass.device,
        &pass.queue,
        Some(settings_frame),
        SIZE,
        &history(true, false),
    ));
    let (half, bytes) = kept_bytes(&pass);
    let read = texels(&bytes);
    assert_eq!(
        (read.light, read.geometry),
        (finished.light, finished.geometry),
        "the next frame reads other than the finished frame kept"
    );
    assert_eq!(half, 0, "the finished frame wrote the first half");
    assert_eq!(
        pass.stage.written().unwrap(),
        &abandoned_written,
        "the next frame writes another half than the abandoned one"
    );
}

// Defect: turning the setting off leaves what it made allocated: its own
// targets, or, where ambient occlusion is off, the ambient occlusion stage
// and its depth pyramid it created to read (about 11 MB at 1080p), though
// it holds no targets while off. Oracle: the frames' stages, read after
// each frame: with ambient occlusion off, a frame with the setting off
// holds neither; with ambient occlusion on, its stage stays, as ambient
// occlusion keeps it, and the bounce's targets go.
#[test]
fn turning_the_bounce_off_releases_what_it_made() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [32, 24];
    let mut scene = crate::Scene::new(&device, &queue);
    let input = crate::FrameInput::new(crate::Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 4. / 3., 0.1),
        eye: Vec3::ZERO,
    });
    let output =
        crate::view::targets::target(&device, "lifetime", size, crate::shading::gbuffer::COLOR);
    let settings = |bounce, occlusion| crate::settings::Settings {
        screen_space_indirect: bounce,
        ambient_occlusion: occlusion,
        ..crate::settings::Settings::default()
    };
    use crate::settings::AmbientOcclusionQuality as Occlusion;
    let (on, off) = (ScreenSpaceIndirect::Low, ScreenSpaceIndirect::Off);
    for (occlusion, held_while_off) in [
        (Occlusion::Off, (false, false)),
        (Occlusion::Medium, (true, false)),
    ] {
        let mut renderer =
            crate::TestRenderer::for_test(&device, &queue, size, &settings(on, occlusion));
        for (bounce, held) in [
            (on, (true, true)),
            (on, (true, true)),
            (off, held_while_off),
        ] {
            let settings = settings(bounce, occlusion);
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.render(
                &device,
                &queue,
                &mut encoder,
                &mut scene,
                &input,
                &settings,
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            renderer.finish_frame(&mut scene);
            assert_eq!(
                renderer.view.opaque.holds_occlusion_and_bounce(),
                held,
                "ambient occlusion {occlusion:?}, bounce {bounce:?}: (occlusion stage, bounce targets)"
            );
        }
    }
}
