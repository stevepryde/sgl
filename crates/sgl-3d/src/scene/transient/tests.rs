//! Exercise the public updates against actual, deliberately small device limits.
//! A missed rejection reaches wgpu validation; replacing content before rejection
//! loses the previously accepted draw/list. The existing renderer tests cover
//! drawing and origin moves, rather than duplicating their shader logic here.
use super::*;
use crate::{Scene, test_support};

#[test]
fn oversized_transient_updates_keep_accepted_content() {
    let Some(adapter) = test_support::adapter() else {
        return;
    };
    let mut limits = crate::graphics_device::limits(&adapter);
    limits.max_buffer_size = 262_144;
    limits.max_storage_buffer_binding_size = 4096;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits,
        ..Default::default()
    }))
    .unwrap();
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut scene = Scene::new(&device, &queue);
    let volume = FogVolume {
        center: Vec3::new(1., 2., 3.),
        density: 0.25,
        ..Default::default()
    };
    scene
        .update_fog_volumes(&device, &queue, &[volume])
        .unwrap();
    let fog = scene.transient.fog_volumes.clone();
    let refused_fog = vec![FogVolume::default(); 37];
    assert!(matches!(
        scene.update_fog_volumes(&device, &queue, &refused_fog),
        Err(SceneError::DeviceLimit)
    ));
    assert_eq!(scene.transient.fog_volumes, fog);
    assert_eq!(scene.transient.fog_volume_list.len(), 1);
    assert_eq!(scene.transient.fog_volume_list[0].center, volume.center);
    assert_eq!(scene.transient.fog_volume_list[0].density, volume.density);
    // Fog binds the entire buffer: the accepted content must still bind after
    // a refused replacement, even though all refused records fit max_buffer_size.
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: scene.transient.fog_volumes.as_entire_binding(),
        }],
    });

    let position = [1.25, 2.5, 3.75];
    scene.update_mist(&device, &queue, &[position]).unwrap();
    let mist = scene.transient.mist.clone();
    let refused_mist = vec![[9.; 3]; 21_846];
    assert!(matches!(
        scene.update_mist(&device, &queue, &refused_mist),
        Err(SceneError::DeviceLimit)
    ));
    assert_eq!(scene.transient.mist, mist);
    assert_eq!(scene.transient.mist_positions, vec![position]);

    let glow = Glow {
        position,
        color: [1.; 4],
        ..Default::default()
    };
    // Grow beyond half the buffer limit, then by one vertex. Uncapped
    // geometric doubling would exceed the device limit on the second update.
    let capacity = 262_144 / std::mem::size_of::<GlowVertex>();
    let first = capacity * 3 / 4;
    let mut vertices = vec![glow; first];
    scene.update_effects(&device, &queue, &vertices).unwrap();
    vertices.push(glow);
    scene.update_effects(&device, &queue, &vertices).unwrap();
    let accepted_glow = scene.transient.glow.clone();
    let refused_glow = vec![Glow::default(); capacity + 1];
    assert!(matches!(
        scene.update_effects(&device, &queue, &refused_glow),
        Err(SceneError::DeviceLimit)
    ));
    assert_eq!(scene.transient.glow, accepted_glow);
    assert_eq!(scene.transient.glow_count as usize, vertices.len());
    let words = test_support::read_words(&device, &queue, &scene.transient.glow);
    assert_eq!(
        words[..3]
            .iter()
            .map(|&v| f32::from_bits(v))
            .collect::<Vec<_>>(),
        position
    );
    // Accepted same-length content can still move without reallocation or
    // losing the glow count after a refused replacement.
    scene
        .move_origin(&device, &queue, Vec3::new(1., 2., 3.))
        .unwrap();
    let words = test_support::read_words(&device, &queue, &scene.transient.glow);
    assert_eq!(
        words[..3]
            .iter()
            .map(|&v| f32::from_bits(v))
            .collect::<Vec<_>>(),
        [0.25, 0.5, 0.75]
    );
    assert_eq!(scene.transient.glow_count as usize, vertices.len());
    assert_eq!(scene.transient.fog_volume_list[0].center, Vec3::ZERO);
    assert_eq!(scene.transient.mist_positions, vec![[0.25, 0.5, 0.75]]);
    assert!(pollster::block_on(device.pop_error_scope()).is_none());
}

// Wider native slice lengths must not truncate into the u32 draw/index range.
// Exercising the sizing boundary avoids allocating billions of real vertices.
#[cfg(target_pointer_width = "64")]
#[test]
fn transient_record_counts_cannot_wrap_the_draw_range() {
    assert!(matches!(
        record_bytes::<GlowVertex>(u32::MAX as usize + 1, u64::MAX),
        Err(SceneError::DeviceLimit)
    ));
}
