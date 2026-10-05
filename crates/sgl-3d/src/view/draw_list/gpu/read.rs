//! What a GPU-built view's cull appended, read back for tests.
use super::GpuList;
use crate::Scene;
use crate::shading::culling::{CullStatistics, DrawCommand, words};
use crate::shading::vertex::DrawInstance;

/// What `list`'s cull appended for one kind of command, from its `draws`
/// and a cluster list's words `regions`, read back: each set's command
/// `first_command` on, and the instances in its region `at` draw instances
/// on, in set order.
fn appended(
    scene: &Scene,
    (draws, regions): (&[u32], &[u32]),
    first_command: u32,
    at: u32,
) -> Vec<DrawInstance> {
    let mut appended = Vec::new();
    for (index, _, region) in scene.candidates.sets.iter() {
        let command =
            (words::<CullStatistics>() + (first_command + index) * words::<DrawCommand>()) as usize;
        let count: u32 = bytemuck::cast_slice::<u32, DrawCommand>(
            &draws[command..command + words::<DrawCommand>() as usize],
        )[0]
        .instance_count;
        assert!(
            count as usize <= region.len(),
            "a set's draw stays within its region"
        );
        let first = (at + region.start) as usize * words::<DrawInstance>() as usize;
        let entries = &regions[first..first + count as usize * words::<DrawInstance>() as usize];
        appended.extend_from_slice(bytemuck::cast_slice::<u32, DrawInstance>(entries));
    }
    appended
}

/// What each phase appended to `list`'s sets' regions, read back: the
/// early phase's draw instances, then the late phase's, each set's in set
/// order.
pub(crate) fn read_phases(
    list: &GpuList,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &Scene,
) -> [Vec<DrawInstance>; 2] {
    let read = |buffer| crate::test_support::read_words(device, queue, buffer);
    let draws = read(&list.draws);
    [
        appended(scene, (&draws, &read(&list.regions)), 0, 0),
        appended(scene, (&draws, &read(&list.late_regions)), list.commands, 0),
    ]
}

/// What the early phase appended to `list`'s sets' regions, read back: its
/// pulled draw instances, then its paired ones, each set's in set order;
/// and its statistics.
pub(crate) fn read_early(
    list: &GpuList,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &Scene,
) -> ([Vec<DrawInstance>; 2], CullStatistics) {
    let read = |buffer| crate::test_support::read_words(device, queue, buffer);
    let draws = read(&list.draws);
    let regions = read(&list.regions);
    let paired = if list.paired {
        appended(
            scene,
            (&draws, &regions),
            2 * list.commands,
            list.paired_region,
        )
    } else {
        Vec::new()
    };
    (
        [appended(scene, (&draws, &regions), 0, 0), paired],
        *bytemuck::from_bytes(bytemuck::cast_slice(
            &draws[..words::<CullStatistics>() as usize],
        )),
    )
}
