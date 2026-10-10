//! The ray allocation's passes (allocate.wgsl) on probes the test lays out:
//! which probes trace in each frame, with how many rays, against the frame's
//! budget. The probes' states and inconsistencies are set directly, so the
//! frames' requests are known from the spec's distance periods and rays
//! (the architecture's Dynamic diffuse GI): every frame within a spacing of
//! the camera, every eighth at 128 spacings, with an eighth of the tier's
//! most rays there.
use super::DynamicGi;
use super::buffers::{Allocation, Convergence, STRIDES, VolumeUniform, budget, dispatch};
use super::frame::{frustum, rotation};
use super::pipelines::ALLOCATE;
use crate::renderer::Renderer;
use crate::settings::{DynamicGiQuality, Settings};
use crate::shading::dynamic_gi as layout;
use crate::{DynamicGiVolume, test_support};
use glam::{Mat4, Vec3};

const MOST_RAYS: u32 = layout::MOST_RAYS;

/// A blended probe at rest, active and with a surface in its cell, as
/// `ddgi_pack_probe` packs it: no offset, blended, no back faces, surfaced.
fn surfaced_probe() -> [u32; 4] {
    [
        0,
        u32::from(test_support::to_half(1.)) << 16,
        0f32.to_bits(),
        1 << 24,
    ]
}

/// One frame's allocation: the rays each probe traces beside its fixed
/// rays, by its index, and every ray the frame traces.
struct Frame {
    rays: Vec<u32>,
    traced: u32,
}

/// Runs the allocation for `frames` frames of `placement`'s probes, every
/// one blended, active and surfaced, the most inconsistent texel of probe
/// `index` at `inconsistency(index)`, seen from `eye` looking along -Z at
/// High, with what they follow changing so the volume never pauses.
fn allocate(
    placement: DynamicGiVolume,
    eye: Vec3,
    inconsistency: impl Fn(usize) -> f32,
    frames: u32,
) -> Option<Vec<Frame>> {
    let (device, queue) = test_support::device()?;
    let settings = Settings {
        dynamic_gi: DynamicGiQuality::High,
        ..Settings::default()
    };
    let renderer = Renderer::for_test(&device, &queue, [16, 16], &settings);
    let stage: &DynamicGi = renderer.test_dynamic_gi();
    let count = placement.probes.iter().product::<u32>();
    let storage = |label, contents: &[u8]| {
        crate::scene::buffer(
            &device,
            label,
            contents,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        )
    };
    let texels = (layout::COLOR_RESOLUTION * layout::COLOR_RESOLUTION) as usize;
    let mut variance = vec![0u32; count as usize * texels * 6];
    for probe in 0..count as usize {
        for texel in 0..texels {
            variance[(probe * texels + texel) * 6 + 5] =
                u32::from(test_support::to_half(inconsistency(probe)));
        }
    }
    let variance = storage("variance", bytemuck::cast_slice(&variance));
    let states = vec![surfaced_probe(); count as usize];
    let states = storage("probe states", bytemuck::cast_slice(&states));
    let ray_counts = storage("ray counts", &vec![0; count as usize * 4]);
    let traced_probes = storage("traced probes", &vec![0; count as usize * 4]);
    let allocation = storage("allocation", &[0; std::mem::size_of::<Allocation>()]);
    let convergence = storage("convergence", &[0; std::mem::size_of::<Convergence>()]);
    let list = super::volume::texture(
        &device,
        "ray list",
        layout::ray_texture_size(u64::from(budget(MOST_RAYS))),
        wgpu::TextureFormat::Rg32Uint,
    );
    let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &stage.layouts.allocate,
        entries: &[
            entry(0, stage.uniform.as_entire_binding()),
            entry(1, variance.as_entire_binding()),
            entry(2, states.as_entire_binding()),
            entry(3, ray_counts.as_entire_binding()),
            entry(4, allocation.as_entire_binding()),
            entry(5, wgpu::BindingResource::TextureView(&list)),
            entry(6, traced_probes.as_entire_binding()),
            entry(7, stage.moving_bounds.as_entire_binding()),
            entry(8, convergence.as_entire_binding()),
        ],
    });
    let view = Mat4::from_translation(-eye);
    let projection = crate::perspective(2.5, 1., 0.1);
    let results = (0..frames)
        .map(|frame| {
            let uniform = VolumeUniform {
                origin: placement.origin.to_array(),
                max_distance: 1.,
                spacing: placement.spacing.to_array(),
                max_rays: MOST_RAYS,
                probes: placement.probes,
                probe_count: count,
                rotation: rotation(frame)
                    .to_cols_array_2d()
                    .map(|[x, y, z]| [x, y, z, 0.]),
                frustum: frustum(projection * view),
                eye: eye.to_array(),
                frame,
                rays: 0,
                budget: budget(MOST_RAYS),
                traced: 0,
                padding: 0,
                scroll: [0; 3],
                padding_scroll: 0,
                scrolled: [0; 3],
                moving_count: 0,
                changed: 1,
                padding_changed: [0; 3],
            };
            queue.write_buffer(&stage.uniform, 0, bytemuck::bytes_of(&uniform));
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.clear_buffer(&allocation, 0, None);
            encoder.clear_buffer(&convergence, 0, None);
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_bind_group(0, &group, &[]);
                pass.set_pipeline(&stage.pipelines.rank);
                pass.dispatch_workgroups(count.div_ceil(64), 1, 1);
                pass.set_pipeline(&stage.pipelines.threshold);
                pass.dispatch_workgroups(1, 1, 1);
                pass.set_pipeline(&stage.pipelines.allocate);
                let [x, y] = dispatch(count);
                pass.dispatch_workgroups(x, y, 1);
            }
            queue.submit([encoder.finish()]);
            Frame {
                rays: test_support::read_words(&device, &queue, &ray_counts),
                traced: test_support::read_words(&device, &queue, &allocation)
                    [std::mem::offset_of!(Allocation, rays) / 4],
            }
        })
        .collect();
    Some(results)
}

/// `probes` probes one metre apart about `centre`.
fn block(centre: Vec3, probes: [u32; 3]) -> DynamicGiVolume {
    let extent = Vec3::from_array(probes.map(|n| n as f32)) - 1.;
    DynamicGiVolume {
        origin: centre - extent * 0.5,
        spacing: Vec3::ONE,
        probes,
    }
}

// 4,000 probes 125 to 136 spacings from the camera, all in its view, take
// their turns every eighth frame with at most an eighth of the most rays,
// 32 at High. Half are changing (their most inconsistent texel at 1), the
// other half settled. Their turns at those rays ask for under a third of
// the frame's budget, but the changing half tracing every frame would ask
// for more than it holds. A changing probe's shortened turns take only what
// the others' turns leave, so every settled probe still takes its turn in
// every 8 frames, where shortened turns that counted toward the stride
// lengthened every period fourfold; the changing ones trace their eighth
// of the most rays, and take more turns than their distance gives them.
#[test]
fn shortened_turns_take_only_what_the_distance_turns_leave() {
    let changing = |index: usize| index.is_multiple_of(2);
    let Some(frames) = allocate(
        block(Vec3::new(0., 0., -130.), [40, 10, 10]),
        Vec3::ZERO,
        |index| if changing(index) { 1. } else { 0. },
        32,
    ) else {
        return;
    };
    let probes = frames[0].rays.len();
    for frame in &frames {
        assert!(frame.traced <= budget(MOST_RAYS), "{} rays", frame.traced);
        for (index, &rays) in frame.rays.iter().enumerate() {
            let expected = if changing(index) { MOST_RAYS / 8 } else { 4 };
            assert!(rays == 0 || rays == expected, "probe {index}: {rays}");
        }
    }
    for window in frames.chunks_exact(8) {
        for index in (0..probes).filter(|&index| !changing(index)) {
            assert!(
                window.iter().any(|frame| frame.rays[index] > 0),
                "settled probe {index} took no turn in 8 frames"
            );
        }
    }
    let turns: usize = frames
        .iter()
        .map(|frame| {
            (0..probes)
                .filter(|&index| changing(index) && frame.rays[index] > 0)
                .count()
        })
        .sum();
    let distance_turns = probes / 2 * frames.len() / 8;
    assert!(turns > distance_turns, "{turns} turns");
}

// 4,000 changing probes 8 to 25 spacings from the camera ask on their
// turns for several times the frame's budget. Every probe's period
// lengthens so their requests fit, and none starves: each traces within 64
// frames, where requests past the budget tracing nothing in dispatch order
// would leave the last probes dark, and no frame passes the budget.
#[test]
fn requests_past_the_budget_lengthen_every_period_and_starve_none() {
    let Some(frames) = allocate(
        block(Vec3::new(0., 0., -12.), [40, 10, 10]),
        Vec3::ZERO,
        |_| 1.,
        64,
    ) else {
        return;
    };
    for frame in &frames {
        assert!(frame.traced <= budget(MOST_RAYS), "{} rays", frame.traced);
    }
    let starved: Vec<usize> = (0..frames[0].rays.len())
        .filter(|&index| frames.iter().all(|frame| frame.rays[index] == 0))
        .collect();
    assert!(starved.is_empty(), "{} probes starved", starved.len());
}

// A probe takes one turn in every period of frames, its period lengthened
// by the frame's stride, and its turns under a stride are among its turns
// under every shorter one, so a stride that changes from frame to frame
// never skips its turns. (Phases that did not nest let probes miss every
// turn while the stride alternated between two values near the budget: on
// Hyperdrive at Low with the camera still, 2.2k rays a frame where their
// turns take 8.5k.) Checked over whole cycles of the longest stride
// for 256 probes and a spread of periods.
#[test]
fn a_probes_turns_under_a_stride_are_among_its_turns_under_every_shorter_one() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const PROBES: u32 = 256;
    const PERIODS: [u32; 6] = [1, 2, 3, 5, 8, 32];
    let frames = 32 << (STRIDES - 1);
    let source = format!(
        "{}
@group(1) @binding(0) var<storage,read_write> test_turns:array<u32>;
const TEST_PERIODS=array<u32,{}>({});
@compute @workgroup_size(64)
fn test_turn(@builtin(global_invocation_id) id:vec3<u32>) {{
 var strides=0u;
 for (var stride=0u;stride<DDGI_STRIDES;stride++) {{
  if ddgi_turn(id.z,TEST_PERIODS[id.y],stride,id.x) {{
   strides|=1u<<stride;
  }}
 }}
 test_turns[(id.z*{}u+id.y)*{frames}u+id.x]=strides;
}}",
        crate::shading::compose(&[&ALLOCATE]),
        PERIODS.len(),
        PERIODS.map(|period| format!("{period}u")).join(","),
        PERIODS.len(),
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("dynamic GI turns"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("dynamic GI turns"),
        layout: None,
        module: &module,
        entry_point: Some("test_turn"),
        compilation_options: Default::default(),
        cache: None,
    });
    let words = (PROBES * PERIODS.len() as u32 * frames) as usize;
    let turns = crate::scene::buffer(
        &device,
        "dynamic GI turns",
        &vec![0; words * 4],
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(1),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: turns.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(frames / 64, PERIODS.len() as u32, PROBES);
    }
    queue.submit([encoder.finish()]);
    let turns = test_support::read_words(&device, &queue, &turns);
    for probe in 0..PROBES as usize {
        for (index, period) in PERIODS.into_iter().enumerate() {
            let at = (probe * PERIODS.len() + index) * frames as usize;
            let strides = &turns[at..at + frames as usize];
            for stride in 0..STRIDES {
                let length = (period << stride) as usize;
                let frames_taken: Vec<usize> = (0..strides.len())
                    .filter(|&frame| strides[frame] & 1 << stride != 0)
                    .collect();
                assert!(
                    frames_taken.first().is_some_and(|&first| first < length)
                        && frames_taken
                            .windows(2)
                            .all(|pair| pair[1] - pair[0] == length),
                    "probe {probe}, period {period}, stride {stride}: {frames_taken:?}"
                );
            }
            for (frame, &taken) in strides.iter().enumerate() {
                // Each stride's turns among the shorter's: no bit set above
                // a clear one.
                assert_eq!(
                    taken & (taken + 1),
                    0,
                    "probe {probe}, period {period}, frame {frame}: strides {taken:#b}"
                );
            }
        }
    }
}

// With each of the frame's shared counts (its budget, the starting probes'
// room in the boundary bin and the shortened turns' spare rays) 68 rays
// short of its limit, a probe asking for 132 is refused and a later one
// asking for 36 still takes them, leaving the count where the first found it
// plus 36. (A refused request that kept its rays left the later probe
// refused, under-using the frame.) The requests are made one after another,
// by one invocation, so their order is the test's.
#[test]
fn a_refused_request_leaves_its_rays_to_a_later_one_that_fits() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const ROOM: u32 = 1000;
    let source = format!(
        "{}
@group(1) @binding(0) var<storage,read_write> test_results:array<u32>;
@compute @workgroup_size(1)
fn test_reserve() {{
 atomicStore(&allocation.reserved,volume.budget-68u);
 test_results[0]=u32(reserve_budget(132u));
 test_results[1]=u32(reserve_budget(36u));
 test_results[2]=atomicLoad(&allocation.reserved);
 allocation.ramp_bins=ramp_bin(2.);
 allocation.ramp_room={ROOM}u;
 atomicStore(&allocation.ramp_taken,{ROOM}u-68u);
 test_results[3]=u32(ramp_starts(2.,132u));
 test_results[4]=u32(ramp_starts(2.,36u));
 test_results[5]=atomicLoad(&allocation.ramp_taken);
 allocation.spare={ROOM}u;
 atomicStore(&allocation.spare_taken,{ROOM}u-68u);
 test_results[6]=u32(ddgi_shortened_turn(0u,2u,1u,132u));
 test_results[7]=u32(ddgi_shortened_turn(0u,2u,1u,36u));
 test_results[8]=atomicLoad(&allocation.spare_taken);
}}",
        crate::shading::compose(&[&ALLOCATE]),
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("dynamic GI reservations"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("dynamic GI reservations"),
        layout: None,
        module: &module,
        entry_point: Some("test_reserve"),
        compilation_options: Default::default(),
        cache: None,
    });
    let budget = budget(MOST_RAYS);
    let volume = VolumeUniform {
        budget,
        ..bytemuck::Zeroable::zeroed()
    };
    let buffer = |label, contents: &[u8], usage| {
        crate::scene::buffer(
            &device,
            label,
            contents,
            usage | wgpu::BufferUsages::COPY_SRC,
        )
    };
    let volume = buffer(
        "volume",
        bytemuck::bytes_of(&volume),
        wgpu::BufferUsages::UNIFORM,
    );
    let allocation = buffer(
        "allocation",
        &[0; std::mem::size_of::<Allocation>()],
        wgpu::BufferUsages::STORAGE,
    );
    let results = buffer("results", &[0; 9 * 4], wgpu::BufferUsages::STORAGE);
    fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry {
            binding,
            resource: buffer.as_entire_binding(),
        }
    }
    let stage_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[entry(0, &volume), entry(4, &allocation)],
    });
    let test_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(1),
        entries: &[entry(0, &results)],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &stage_group, &[]);
        pass.set_bind_group(1, &test_group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    queue.submit([encoder.finish()]);
    let results = test_support::read_words(&device, &queue, &results);
    for (name, limit, taken) in [
        ("budget", budget, &results[0..3]),
        ("starting room", ROOM, &results[3..6]),
        ("spare", ROOM, &results[6..9]),
    ] {
        assert_eq!(
            taken,
            [0, 1, limit - 68 + 36],
            "{name}: refused, taken, count"
        );
    }
}
