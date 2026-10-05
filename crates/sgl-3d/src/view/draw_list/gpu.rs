//! A GPU-built view's draw list (the architecture's "GPU draw lists and
//! occlusion culling"): the camera's opaque and masked surfaces and each
//! directional cascade's casters. The view owns what its cull writes and
//! its passes draw from: its cull for the frame (`CullView`), its lists
//! (the visible lists, the late list and the late section queue, with
//! their counts), the dispatch buffer that drives the indirect dispatches,
//! its cluster lists, which hold every set's region of draw instances, one
//! for each phase it culls, and its draws: its statistics words, then each
//! set's early indirect command, then each set's late one, then, for the
//! camera with diagnostics, each candidate's appended sections. The cull
//! stage (`stages::cull`) writes them from the scene's candidates; the
//! shadow and opaque stages draw them through the executor here, one
//! `draw_indirect` per set and phase whose instances are the sections that
//! passed, with the set's region of that phase's cluster list bound as the
//! draw-instance buffer, as Bevy 9d12036's meshlet hardware raster draws
//! its visible clusters (crates/bevy_pbr/src/meshlet/
//! visibility_buffer_raster_node.rs 592, visibility_buffer_hardware_raster.wesl).
//! Only the camera culls a late phase, while occlusion culling runs.
use super::Binder;
use crate::Scene;
use crate::content::identity::MaterialId;
use crate::shading::culling::{
    CANDIDATE_STATISTICS_WORDS, CULL_CAMERA, CULL_CANDIDATE_STATISTICS, CULL_DISPATCH_WORDS,
    CULL_FRUSTUM, CULL_LOD, CULL_NEAR, CULL_WORKGROUP, CullListsHeader, CullStatistics, CullView,
    DrawCommand, dispatch_side, late_entries, words,
};
use crate::shading::vertex::{DRAW_INSTANCE_SLOT, DrawInstance};
use crate::view::View;
use crate::view::culling::Frustum;
use crate::view::pipelines::{GeometryPass, GeometryPipelines, Variant};
use crate::view::population::camera_variant;
use glam::{DMat4, Mat4};
use std::ops::Range;

/// A phase of a GPU-built view's draws: the early phase's, which prepare
/// culls, and, while the camera culls occlusion, the late phase's: what
/// the early phase found occluded and the late phase finds visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Early,
    Late,
}

/// One set a view draws this frame: its index, which is its command's, its
/// region, and the pipeline variant and material it draws with.
struct DrawnSet {
    index: u32,
    region: Range<u32>,
    variant: Variant,
    material: MaterialId,
}

pub(crate) struct GpuList {
    /// The frame's `CullView`.
    uniform: wgpu::Buffer,
    /// `CullLists`: its header, then its lists' entries.
    lists: wgpu::Buffer,
    /// The indirect dispatches the finalizes write (`CULL_DISPATCH_WORDS`).
    dispatch: wgpu::Buffer,
    /// Every set's region of draw instances, for the early phase and for
    /// the late, each placed as the set's record says.
    regions: wgpu::Buffer,
    late_regions: wgpu::Buffer,
    /// Its statistics, each set's early command, each set's late command
    /// and the candidates' statistics.
    draws: wgpu::Buffer,
    /// What `draws` holds as each frame starts, which the frame's encoder
    /// copies over it: zero statistics and counts, and every command
    /// `DrawCommand::RESET`.
    reset: wgpu::Buffer,
    /// The commands a phase holds room for in `draws`: the late ones follow
    /// the early ones, and the candidates' statistics both.
    commands: u32,
    /// Whether the frame culls a late phase.
    late: bool,
    /// The candidates it culls this frame, and whether it counts their
    /// statistics.
    candidates: u32,
    candidate_statistics: bool,
    drawn: Vec<DrawnSet>,
}

/// A buffer of at least `size` bytes, or `buffer` when it holds them.
fn grown(
    device: &wgpu::Device,
    buffer: &mut wgpu::Buffer,
    size: u64,
    label: &str,
    usage: wgpu::BufferUsages,
) -> bool {
    if buffer.size() >= size {
        return false;
    }
    *buffer = crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some(label),
            size: size.next_power_of_two(),
            usage,
            mapped_at_creation: false,
        },
    );
    true
}

/// The bytes of `count` records of `T`, for a binding that is never empty.
fn bytes_of<T>(count: u32) -> u64 {
    u64::from(count.max(1)) * std::mem::size_of::<T>() as u64
}

const STORAGE: wgpu::BufferUsages = wgpu::BufferUsages::STORAGE;

impl GpuList {
    pub fn new(device: &wgpu::Device) -> Self {
        let buffer = |label, size, usage| {
            crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage,
                    mapped_at_creation: false,
                },
            )
        };
        Self {
            uniform: buffer(
                "cull view",
                std::mem::size_of::<CullView>() as u64,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            lists: buffer("cull lists", 16, STORAGE),
            dispatch: buffer(
                "cull dispatch",
                u64::from(CULL_DISPATCH_WORDS) * 4,
                STORAGE | wgpu::BufferUsages::INDIRECT,
            ),
            regions: buffer("cluster list", 16, STORAGE),
            late_regions: buffer("late cluster list", 16, STORAGE),
            draws: buffer("view draws", 16, STORAGE),
            reset: buffer("view draws reset", 16, wgpu::BufferUsages::COPY_SRC),
            commands: 0,
            late: false,
            candidates: 0,
            candidate_statistics: false,
            drawn: Vec::new(),
        }
    }

    /// Prepares the frame's cull of `scene` from `view` (its planes, level
    /// of detail and flags; the counts are filled here), with a late phase
    /// when `late`: grows its buffers to the scene's candidates, sets and
    /// regions, uploads the cull and chooses the sets it draws, those its
    /// population can show under the frame's mask.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        mut view: CullView,
        late: bool,
    ) {
        let candidates = scene.candidates.end();
        let sets = &scene.candidates.sets;
        let statistics = view.flags & CULL_CANDIDATE_STATISTICS != 0;
        // The queue holds a slot for each section a visible candidate's
        // chosen level draws: no more than its set's region holds.
        let queue_capacity = if late { sets.region_end() } else { 0 };
        let entries = if late {
            late_entries(candidates, queue_capacity)
        } else {
            u64::from(candidates)
        };
        grown(
            device,
            &mut self.lists,
            std::mem::size_of::<CullListsHeader>() as u64
                + entries.max(1) * std::mem::size_of::<[u32; 2]>() as u64,
            "cull lists",
            STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        );
        let regions_usage = STORAGE | wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_SRC;
        let regions = bytes_of::<DrawInstance>(sets.region_end());
        grown(
            device,
            &mut self.regions,
            regions,
            "cluster list",
            regions_usage,
        );
        if late {
            grown(
                device,
                &mut self.late_regions,
                regions,
                "late cluster list",
                regions_usage,
            );
        }
        let commands = sets.end().max(1).next_power_of_two().max(self.commands);
        let statistics_words = if statistics {
            candidates.max(1).next_power_of_two() * CANDIDATE_STATISTICS_WORDS
        } else {
            0
        };
        let draws = std::mem::size_of::<CullStatistics>() as u64
            + 2 * u64::from(commands) * std::mem::size_of::<DrawCommand>() as u64
            + u64::from(statistics_words) * 4;
        let usage = STORAGE
            | wgpu::BufferUsages::INDIRECT
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC;
        let reallocated = grown(device, &mut self.draws, draws, "view draws", usage);
        if reallocated || commands != self.commands {
            // The candidates' statistics follow the commands, so the reset
            // they start from is written again where they move.
            self.reset = crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("view draws reset"),
                    size: self.draws.size(),
                    usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            );
            let mut reset = vec![0u32; (self.draws.size() / 4) as usize];
            let first = words::<CullStatistics>() as usize;
            for command in reset[first..first + (2 * commands * words::<DrawCommand>()) as usize]
                .chunks_exact_mut(words::<DrawCommand>() as usize)
            {
                command.copy_from_slice(bytemuck::cast_slice(&[DrawCommand::RESET]));
            }
            crate::counters::write_buffer(queue, &self.reset, 0, bytemuck::cast_slice(&reset));
            self.commands = commands;
        }
        self.candidates = candidates;
        self.candidate_statistics = statistics;
        self.late = late;
        let workgroups = candidates.div_ceil(CULL_WORKGROUP);
        view.candidates = candidates;
        view.candidate_side = dispatch_side(workgroups)[0];
        view.candidate_statistics =
            words::<CullStatistics>() + 2 * commands * words::<DrawCommand>();
        view.late_command = commands;
        view.queue_capacity = queue_capacity;
        crate::counters::write_buffer(queue, &self.uniform, 0, bytemuck::bytes_of(&view));
        let camera = view.flags & CULL_CAMERA != 0;
        self.drawn.clear();
        for (index, key, region) in sets.iter() {
            let material = scene.drawn_material(key.material);
            let shown = if camera {
                material.enabled(Some(view.visibility_mask))
            } else {
                material.casts_directional_shadow(view.visibility_mask)
            };
            if !shown {
                continue;
            }
            self.drawn.push(DrawnSet {
                index,
                region,
                variant: camera_variant(&material.values, key.mirrored, key.deforms),
                material: key.material,
            });
        }
    }

    /// Starts the frame's lists in `encoder`, before the cull writes them:
    /// zero counts and statistics, and every command reset. An abandoned
    /// frame's encoder takes its reset with it, so the next frame starts
    /// from its own.
    pub fn encode_reset(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.copy_buffer_to_buffer(&self.reset, 0, &self.draws, 0, self.reset.size());
        encoder.clear_buffer(
            &self.lists,
            0,
            Some(std::mem::size_of::<CullListsHeader>() as u64),
        );
    }

    /// The early instance cull's workgroups along x and y.
    pub fn instance_workgroups(&self) -> [u32; 2] {
        dispatch_side(self.candidates.div_ceil(CULL_WORKGROUP))
    }

    /// Whether the frame culls any candidate.
    pub fn culls(&self) -> bool {
        self.candidates > 0
    }

    /// Whether the frame culls a late phase.
    pub fn late(&self) -> bool {
        self.late
    }

    /// What the cull binds of it: its cull, lists, dispatch, cluster lists
    /// and draws.
    pub fn buffers(&self) -> CullBuffers<'_> {
        CullBuffers {
            view: &self.uniform,
            lists: &self.lists,
            dispatch: &self.dispatch,
            regions: &self.regions,
            late_regions: &self.late_regions,
            draws: &self.draws,
        }
    }

    /// Its draws and where its candidates' statistics start, in bytes,
    /// with how many there are; none without them.
    pub fn statistics(&self) -> (&wgpu::Buffer, Option<(u64, u32)>) {
        let at =
            u64::from(words::<CullStatistics>() + 2 * self.commands * words::<DrawCommand>()) * 4;
        (
            &self.draws,
            self.candidate_statistics.then_some((at, self.candidates)),
        )
    }

    /// The phases the frame culls: the early one, and the late one while
    /// it culls occlusion.
    fn phases(&self) -> &'static [Phase] {
        if self.late {
            &[Phase::Early, Phase::Late]
        } else {
            &[Phase::Early]
        }
    }

    /// The draws `draw` issues: one per set it draws and phase it culls.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn draws(&self) -> usize {
        self.drawn.len() * self.phases().len()
    }

    /// Issues every phase's draws in `pass`, whose group 0 the caller
    /// bound, and returns how many it issued: per set, in set order, its
    /// early draw, then its late one where the frame culls a late phase.
    pub fn draw(
        &self,
        scene: &Scene,
        pipelines: &GeometryPipelines,
        pass: &mut wgpu::RenderPass<'_>,
        kind: GeometryPass,
    ) -> usize {
        self.issue(scene, pipelines, pass, kind, self.phases())
    }

    /// Issues `phase`'s draws alone in `pass`, as `draw` issues them: none
    /// for a late phase the frame does not cull.
    pub fn draw_phase(
        &self,
        scene: &Scene,
        pipelines: &GeometryPipelines,
        pass: &mut wgpu::RenderPass<'_>,
        kind: GeometryPass,
        phase: Phase,
    ) -> usize {
        if phase == Phase::Late && !self.late {
            return 0;
        }
        self.issue(scene, pipelines, pass, kind, &[phase])
    }

    /// Per set, in set order, one indirect draw of each of `phases`'
    /// commands, whose instances are the sections in the set's region of
    /// that phase's cluster list, with its pipeline and material.
    fn issue(
        &self,
        scene: &Scene,
        pipelines: &GeometryPipelines,
        pass: &mut wgpu::RenderPass<'_>,
        kind: GeometryPass,
        phases: &[Phase],
    ) -> usize {
        if self.drawn.is_empty() {
            return 0;
        }
        pass.set_bind_group(1, &scene.scene_group, &[]);
        let mut binder = Binder::new(pipelines, kind);
        let stride = std::mem::size_of::<DrawInstance>() as u64;
        let command = std::mem::size_of::<DrawCommand>() as u64;
        let first = std::mem::size_of::<CullStatistics>() as u64;
        for set in &self.drawn {
            binder.bind(pass, scene, set.variant, set.material);
            let region = u64::from(set.region.start) * stride..u64::from(set.region.end) * stride;
            for phase in phases {
                let (regions, index) = match phase {
                    Phase::Early => (&self.regions, set.index),
                    Phase::Late => (&self.late_regions, self.commands + set.index),
                };
                pass.set_vertex_buffer(DRAW_INSTANCE_SLOT, regions.slice(region.clone()));
                pass.draw_indirect(&self.draws, first + u64::from(index) * command);
            }
        }
        self.drawn.len() * phases.len()
    }
}

/// What each phase appended to `list`'s sets' regions, read back: the
/// early phase's draw instances, then the late phase's, each set's in set
/// order.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn read_phases(
    list: &GpuList,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &Scene,
) -> [Vec<DrawInstance>; 2] {
    let read = |buffer| crate::test_support::read_words(device, queue, buffer);
    let draws = read(&list.draws);
    let regions = [read(&list.regions), read(&list.late_regions)];
    let mut phases = [Vec::new(), Vec::new()];
    for (index, _, region) in scene.candidates.sets.iter() {
        for (phase, first_command) in [(0, 0), (1, list.commands)] {
            let command = (words::<CullStatistics>()
                + (first_command + index) * words::<DrawCommand>())
                as usize;
            let count: u32 = bytemuck::cast_slice::<u32, DrawCommand>(
                &draws[command..command + words::<DrawCommand>() as usize],
            )[0]
            .instance_count;
            assert!(
                count as usize <= region.len(),
                "a set's draw stays within its region"
            );
            let first = region.start as usize * words::<DrawInstance>() as usize;
            let entries =
                &regions[phase][first..first + count as usize * words::<DrawInstance>() as usize];
            phases[phase].extend_from_slice(bytemuck::cast_slice::<u32, DrawInstance>(entries));
        }
    }
    phases
}

/// What the cull stage binds of a GPU-built view.
#[derive(Clone, Copy)]
pub(crate) struct CullBuffers<'a> {
    pub view: &'a wgpu::Buffer,
    pub lists: &'a wgpu::Buffer,
    pub dispatch: &'a wgpu::Buffer,
    pub regions: &'a wgpu::Buffer,
    pub late_regions: &'a wgpu::Buffer,
    pub draws: &'a wgpu::Buffer,
}

/// The cull of the camera `view` at `size` pixels under the frame's `mask`:
/// its population, its clip volume with the near plane (unless `culling`,
/// the diagnostics layer, is off), its level of detail where it has pixels
/// and, with diagnostics, its candidates' statistics.
pub(crate) fn camera_cull(view: &View, size: [u32; 2], mask: u32, culling: bool) -> CullView {
    let camera = Mat4::from_cols_array_2d(&view.uniform.view);
    let projection = Mat4::from_cols_array_2d(&view.uniform.projection);
    let (planes, plane_errors) = Frustum::planes(camera, projection, view.uniform.jitter);
    // As the CPU builder's `LodSelector`: none for a camera without pixels
    // or with transforms the bound cannot take.
    let lod = !size.contains(&0) && camera.is_finite() && projection.is_finite();
    let absolute = |m: Mat4| DMat4::from_cols_array(&m.as_dmat4().to_cols_array().map(f64::abs));
    let flags = CULL_CAMERA
        | CULL_NEAR
        | if culling { CULL_FRUSTUM } else { 0 }
        | if lod { CULL_LOD } else { 0 }
        | if cfg!(feature = "diagnostics") {
            CULL_CANDIDATE_STATISTICS
        } else {
            0
        };
    CullView {
        planes,
        plane_errors,
        lod_clip_from_world: (projection.as_dmat4() * camera.as_dmat4())
            .as_mat4()
            .to_cols_array_2d(),
        lod_magnitude: (absolute(projection) * absolute(camera))
            .as_mat4()
            .to_cols_array_2d(),
        lod_size: size.map(|side| side as f32),
        visibility_mask: mask,
        flags,
        ..bytemuck::Zeroable::zeroed()
    }
}

/// The cull of the directional cascade `view` under the frame's `mask`:
/// its casters, against its clip volume without its near plane, since a
/// caster between the light and the cascade casts into it (Bevy pushes a
/// cascade frustum's near plane to infinity), at level 0, since shadows
/// keep the original geometry.
pub(crate) fn cascade_cull(view: &View, mask: u32) -> CullView {
    let (planes, plane_errors) = Frustum::planes(
        Mat4::from_cols_array_2d(&view.uniform.view),
        Mat4::from_cols_array_2d(&view.uniform.projection),
        view.uniform.jitter,
    );
    CullView {
        planes,
        plane_errors,
        visibility_mask: mask,
        flags: CULL_FRUSTUM,
        ..bytemuck::Zeroable::zeroed()
    }
}
