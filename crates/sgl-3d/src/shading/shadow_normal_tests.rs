//! Where a shadow lookup offsets its receiver: along the geometry normal,
//! whatever the material's normal map makes of the shading normal, observed
//! in the lit colour of real frames.
use crate::asset::{Asset, CpuMesh, Image, Material, Vertex};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::view::cascades::SHADOW_PANCAKE_SIZE;
use crate::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, FrameInput, InstanceId, InstanceState,
    Light, LightShape, Mobility, Scene, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];
/// The receiver's distance in front of the camera.
const DEPTH: f32 = 5.;
/// The column whose pixel centre sees the shadow's edge on the receiver.
const EDGE_COLUMN: u32 = 40;

/// A quad facing +Z, its corners `min` and `max` in X and Y at `z`, its U
/// along +X and V along +Y.
fn quad(min: [f32; 2], max: [f32; 2], z: f32) -> CpuMesh {
    CpuMesh {
        vertices: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
            .map(|[s, t]| Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [
                    min[0] + (max[0] - min[0]) * s,
                    min[1] + (max[1] - min[1]) * t,
                    z,
                ],
                normal: [0., 0., 1.],
                uv: [s, t],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// `mesh` with a white, rough, single-sided material, its normal map
/// tilting the shading normal 37° toward +U when `tilted`.
fn white(mesh: CpuMesh, tilted: bool) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![mesh];
    asset.materials = vec![Material {
        base: [0.8, 0.8, 0.8, 1.],
        metallic: 0.,
        roughness: 1.,
        // X 0.6, Y 0, Z 0.8.
        normal_texture: tilted.then_some(0),
        ..Default::default()
    }];
    asset.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        4,
        4,
        image::Rgba([204, 128, 230, 255]),
    ))];
    asset
}

/// Where the receiver at `DEPTH` lies under the centre of `EDGE_COLUMN`.
fn edge(projection: Mat4) -> f32 {
    let ndc = (EDGE_COLUMN as f32 + 0.5) / SIZE[0] as f32 * 2. - 1.;
    ndc / projection.x_axis.x * DEPTH
}

fn add(
    scene: &mut Scene,
    gpu: (&wgpu::Device, &wgpu::Queue),
    asset: Asset,
    visible: bool,
) -> InstanceId {
    let (device, queue) = gpu;
    let model = scene.add_asset(device, queue, asset).unwrap().model;
    let state = InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible,
        capture_visible: true,
    };
    scene
        .add_instance(device, queue, state, Mobility::Static)
        .unwrap()
}

/// The light that shines along the view onto the receiver.
#[derive(Clone, Copy, Debug)]
enum Shining {
    /// A directional light; the occluder covers the receiver left of the
    /// edge, beyond the cascades' pancake behind the camera.
    Directional,
    /// A spot at the camera; the occluder covers the receiver left of the
    /// edge from halfway to it.
    Spot,
}

/// The shadowed share of the light the receiver takes at each column of
/// the middle row: the lit colour with the occluder casting over the lit
/// colour without it.
fn visibility(gpu: (&wgpu::Device, &wgpu::Queue), tilted: bool, shining: Shining) -> Vec<f32> {
    let (device, queue) = gpu;
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    add(
        &mut scene,
        gpu,
        white(quad([-4., -4.], [4., 4.], -DEPTH), tilted),
        true,
    );
    let projection = crate::perspective(1., 1., 0.1);
    let x = edge(projection);
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection,
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.baked_lighting = false;
    let occluder = match shining {
        Shining::Directional => {
            input.directional_lights[0] = Some(DirectionalLight {
                direction: Vec3::NEG_Z,
                color: [1.; 3],
                illuminance: 1.,
                shadow: Some(DirectionalShadow {
                    distance: 10.,
                    cascades: 2,
                }),
                ..Default::default()
            });
            quad([x - 8., -8.], [x, 8.], SHADOW_PANCAKE_SIZE + 10.)
        }
        Shining::Spot => {
            let light = Light {
                shape: LightShape::Spot {
                    direction: Vec3::NEG_Z,
                    inner_angle: 0.6,
                    outer_angle: 0.7,
                },
                intensity: DEPTH * DEPTH,
                range: 20.,
                casts_shadow: true,
                ..Default::default()
            };
            scene.add_light(device, queue, light).unwrap();
            quad([x / 2. - 4., -4.], [x / 2., 4.], -DEPTH / 2.)
        }
    };
    // The camera does not see the occluder; the light does.
    let occluder = add(&mut scene, gpu, white(occluder, false), false);
    let output = crate::view::targets::target(
        device,
        "shadow output",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    let mut row = |scene: &mut Scene| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        let color = test_support::read(device, queue, renderer.targets().color.texture(), 8);
        let start = (SIZE[1] / 2 * SIZE[0]) as usize;
        (0..SIZE[0] as usize)
            .map(|x| test_support::half(&color[(start + x) * 8..]))
            .collect::<Vec<_>>()
    };
    let shadowed = row(&mut scene);
    let mut state = *scene.instance(occluder).unwrap();
    state.capture_visible = false;
    scene.set_instance(queue, occluder, state).unwrap();
    let lit = row(&mut scene);
    shadowed.iter().zip(&lit).map(|(s, l)| s / l).collect()
}

// Plausible defect: the shadow lookup offsets the receiver along the mapped
// normal, so a normal map (or its scrolling layers) moves the shadow's edge
// texel by texel and frame by frame, for the directional cascades or the
// local lights. Bevy 9d12036 and Filament ef1a133 offset along the
// geometric normal. The oracle is that requirement: an occluder casts an
// edge onto a receiver facing the light at the centre of one pixel column,
// where the filter leaves part of the light; a normal map tilting the
// shading normal 37° across the edge, which moves a mapped-normal offset two
// shadow texels across it, leaves the share of the light each column takes
// as it is without the map.
#[test]
fn normal_maps_do_not_move_shadows() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    for shining in [Shining::Directional, Shining::Spot] {
        let label = format!("{shining:?}");
        let flat = visibility(gpu, false, shining);
        let tilted = visibility(gpu, true, shining);
        let at = EDGE_COLUMN as usize;
        assert!(
            (0.1..=0.9).contains(&flat[at]),
            "{label}: the edge column takes {} of the light; the fixture misses the edge",
            flat[at]
        );
        for column in at - 3..=at + 3 {
            assert!(
                (flat[column] - tilted[column]).abs() < 0.02,
                "{label}: column {column} takes {} of the light under a tilted normal map and {} without",
                tilted[column],
                flat[column]
            );
        }
    }
}
