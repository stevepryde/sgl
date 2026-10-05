//! Ray-traced shadows observed in real frames on a device with ray queries:
//! the shadow mask against a brute-force CPU oracle of occlusion, the
//! lighting pass taking it where the maps have no shadow, and its history
//! restarting. Each test reports itself unsupported, never passed, where
//! the adapter has no ray queries.
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::content::identity::Identity;
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::shading::shadow_mask::{RT_SHADOW_LIGHTS, SHADOW_MASK_DIRECTIONAL, ShadowMaskSlots};
use crate::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, FrameInput, InstanceState, Light,
    LightId, LightShape, Mobility, Scene, test_support,
};
use glam::{DVec3, DVec4, Mat4, Quat, Vec3};

/// Ray-traced shadows on the hardware path, with nothing else that changes
/// the image from frame to frame.
fn settings(ray_traced_shadows: bool) -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        hardware_ray_tracing: true,
        ray_traced_shadows,
        ..Settings::default()
    }
}

/// A floor quad facing +Y at height 0, `half` metres to each side, of
/// material `double_sided` where asked.
fn quad(half: f32, double_sided: bool) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, z)| Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * half, 0., z * half],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    asset.materials[0].double_sided = double_sided;
    asset
}

/// An axis-aligned box: an occluder the oracle tests and a cube instance
/// drawn there.
#[derive(Clone, Copy)]
struct Block {
    centre: Vec3,
    half: Vec3,
    mobility: Mobility,
}

impl Block {
    const fn new(centre: Vec3, half: Vec3, mobility: Mobility) -> Self {
        Self {
            centre,
            half,
            mobility,
        }
    }

    /// Whether the segment from `origin` along unit `direction` over
    /// `[t_min, t_max]` passes through the box grown by `grow` (shrunk where
    /// negative): the slab test.
    fn crosses(
        self,
        grow: f64,
        origin: DVec3,
        direction: DVec3,
        (t_min, t_max): (f64, f64),
    ) -> bool {
        let centre = self.centre.as_dvec3();
        let half = self.half.as_dvec3() + grow;
        let (mut near, mut far) = (t_min, t_max);
        for axis in 0..3 {
            let (from, along) = (origin[axis] - centre[axis], direction[axis]);
            if along.abs() < 1e-12 {
                if from.abs() > half[axis] {
                    return false;
                }
                continue;
            }
            let (a, b) = ((-half[axis] - from) / along, (half[axis] - from) / along);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        near < far
    }
}

/// How far the oracle grows and shrinks a box, in metres, for f32 arithmetic
/// to decide a segment as f64 does.
const MARGIN: f64 = 2e-3;

/// Whether `blocks` occlude the segment from `origin` along unit
/// `direction` over `interval`; none where growing or shrinking them by
/// `MARGIN` decides otherwise.
fn occluded(
    blocks: &[Block],
    origin: DVec3,
    direction: DVec3,
    interval: (f64, f64),
) -> Option<bool> {
    let grown = blocks
        .iter()
        .any(|block| block.crosses(MARGIN, origin, direction, interval));
    let shrunk = blocks
        .iter()
        .any(|block| block.crosses(-MARGIN, origin, direction, interval));
    (grown == shrunk).then_some(grown)
}

/// A scene of a floor of `half` metres, `blocks` and `lights`.
fn scene(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    half: f32,
    blocks: &[Block],
    lights: &[Light],
) -> (Scene, Vec<LightId>) {
    let mut scene = Scene::new(device, queue);
    test_support::add_static(device, queue, &mut scene, quad(half, false));
    let cube = scene
        .add_asset(device, queue, test_support::cube())
        .unwrap()
        .model;
    for block in blocks {
        let state = InstanceState {
            pose: Mat4::from_scale_rotation_translation(
                block.half * 2.,
                Quat::IDENTITY,
                block.centre,
            ),
            ..InstanceState::new(cube)
        };
        scene
            .add_instance(device, queue, state, block.mobility)
            .unwrap();
    }
    let lights = lights
        .iter()
        .map(|&light| scene.add_light(device, queue, light).unwrap())
        .collect();
    (scene, lights)
}

/// A camera at `eye` looking at `target` over an output of `size`.
fn camera(eye: Vec3, target: Vec3, size: [u32; 2]) -> Camera {
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: crate::perspective(0.9, size[0] as f32 / size[1] as f32, 0.1),
        eye,
    }
}

/// A frame's input: `camera`, a black backdrop and no ambient light, and
/// `directional`.
fn input(camera: Camera, directional: Option<DirectionalLight>) -> FrameInput {
    let mut input = FrameInput::new(camera);
    input.backdrop = Backdrop::Color([0.; 3]);
    input.hemisphere_light.intensity = 0.;
    input.diffuse_environment.intensity = 0.;
    input.reflection_environment.intensity = 0.;
    input.directional_lights[0] = directional;
    input
}

/// What one frame left: its depth, its lit colour's red, and the shadow
/// mask's bytes with its slot table, where the stage ran.
struct Observed {
    size: [u32; 2],
    depth: Vec<f32>,
    red: Vec<f32>,
    mask: Option<(Vec<u8>, ShadowMaskSlots)>,
}

impl Observed {
    /// Slot `slot`'s visibility at `pixel`, as a byte.
    fn mask(&self, slot: usize, [x, y]: [u32; 2]) -> u8 {
        let (bytes, _) = self.mask.as_ref().expect("the ray-traced shadow stage ran");
        let [width, height] = self.size.map(|side| side as usize);
        let (layer, channel) = (slot / 4, slot % 4);
        bytes[((layer * height + y as usize) * width + x as usize) * 4 + channel]
    }

    /// The slot that holds `key`.
    fn slot(&self, key: u32) -> usize {
        let (_, table) = self.mask.as_ref().expect("the ray-traced shadow stage ran");
        let keys: Vec<u32> = table.lights.iter().flatten().copied().collect();
        keys.iter()
            .position(|&held| held == key)
            .unwrap_or_else(|| panic!("no slot holds {key}: {keys:?}"))
    }

    /// The world position the depth puts at the centre of `pixel`, under
    /// `camera`'s unjittered view-projection, in f64; none where nothing
    /// was drawn.
    fn position(&self, camera: &Camera, [x, y]: [u32; 2]) -> Option<DVec3> {
        let [width, height] = self.size;
        let z = self.depth[(y * width + x) as usize];
        if z <= 0. {
            return None;
        }
        let inverse = (camera.projection.as_dmat4() * camera.view.as_dmat4()).inverse();
        let uv = [
            (f64::from(x) + 0.5) / f64::from(width),
            (f64::from(y) + 0.5) / f64::from(height),
        ];
        let h: DVec4 =
            inverse * DVec4::new(uv[0] * 2. - 1., (1. - uv[1]) * 2. - 1., f64::from(z), 1.);
        Some(h.truncate() / h.w)
    }
}

/// Renders `input` of `scene` with `settings` and reads what it left.
fn render(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
) -> Observed {
    let size = renderer.render_size();
    let output = crate::view::targets::target(
        device,
        "ray-traced shadow frames",
        size,
        crate::shading::gbuffer::COLOR,
    );
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        input,
        settings,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(scene);
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
    let depth = test_support::read(device, queue, renderer.targets().depth.texture(), 4)
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let red = test_support::read(device, queue, renderer.targets().color.texture(), 8)
        .chunks_exact(8)
        .map(test_support::half)
        .collect();
    let mask = renderer
        .traced_shadows()
        .map(|(mask, table)| (test_support::read(device, queue, mask.texture(), 4), table));
    Observed {
        size,
        depth,
        red,
        mask,
    }
}

/// Three faint casting lights about `eye`, whose ranges hold the camera so
/// that they cover the whole view and outrank, in the atlas's ranking, the
/// lights a test watches, which then take slots 4 and on: those the
/// temporal blend alone fills, where the denoiser filters slots 0 to 3.
fn decoys(eye: Vec3) -> [Light; 3] {
    [-1., 0., 1.].map(|offset| Light {
        position: eye + Vec3::new(offset, 0.5, 0.),
        intensity: 1e-3,
        range: 40.,
        casts_shadow: true,
        ..Light::default()
    })
}

/// The tracing texels of `decisions` (by their full-resolution pixel 2q)
/// whose decision every texel within `reach` texels shares, each of them a
/// decided floor texel: the texels the denoiser's filters (7 texels) and
/// neighbourhood (8) see as uniform, which it leaves 0 or 1.
fn interior(decisions: &std::collections::HashMap<[u32; 2], bool>, reach: i32) -> Vec<[u32; 2]> {
    decisions
        .iter()
        .filter(|&(&[x, y], &decided)| {
            (-reach..=reach).all(|dy| {
                (-reach..=reach).all(|dx| {
                    let near = [x as i32 + dx * 2, y as i32 + dy * 2];
                    near.iter().all(|&side| side >= 0)
                        && decisions.get(&near.map(|side| side as u32)) == Some(&decided)
                })
            })
        })
        .map(|(&pixel, _)| pixel)
        .collect()
}

/// The denoiser's reach in tracing texels: its local neighbourhood's
/// radius, beyond its filters' 1 + 2 + 4.
const DENOISER_REACH: i32 = 8;

/// The floor pixels of tracing texels: each full-resolution pixel 2q the
/// trace and the mask share, with the world position its depth gives, on
/// the floor (height 0).
fn floor_texels(observed: &Observed, camera: &Camera) -> Vec<([u32; 2], DVec3)> {
    let [width, height] = observed.size;
    (0..height)
        .step_by(2)
        .flat_map(|y| (0..width).step_by(2).map(move |x| [x, y]))
        .filter_map(|pixel| {
            let position = observed.position(camera, pixel)?;
            (position.y.abs() < 1e-3).then_some((pixel, position))
        })
        .collect()
}

/// The oracle's visibility of `light` from the floor at `position`: none
/// where it does not reach it (out of range or outside a spot's cone, as
/// light_reach.wgsl decides) or the arithmetic cannot decide; otherwise
/// whether `blocks` leave the segment to the light's position clear.
fn local_visibility(light: &Light, position: DVec3, blocks: &[Block]) -> Option<Option<bool>> {
    let to_light = light.position.as_dvec3() - position;
    let distance = to_light.length();
    let direction = to_light / distance;
    let range = f64::from(light.range);
    if (distance - range).abs() < 1e-3 {
        return None;
    }
    let mut reached = distance < range && direction.y > 0.;
    if let LightShape::Spot {
        direction: spot,
        outer_angle,
        ..
    } = light.shape
    {
        let cosine = spot.as_dvec3().normalize().dot(-direction);
        let outer = f64::from(outer_angle).cos();
        if (cosine - outer).abs() < 1e-3 {
            return None;
        }
        reached &= cosine > outer;
    }
    if !reached {
        return Some(None);
    }
    occluded(blocks, position, direction, (0.01, distance)).map(|occluded| Some(!occluded))
}

// Every slot held, over two frames: the sun, three decoys that outrank the
// rest for the denoised slots, then a point light, a spot light and ten
// spot lights in a ring about the first block, each casting its shadow its
// own way, through a camera cut and then a still frame, whose temporal
// blend of a history equal to its trace must leave the trace. Plausible
// defects: the trace reading another pixel's depth or normal than the
// mask's, the visibility of one light written into another's slot or
// another slot's layer or channel, the temporal blend or the upsample
// taking one word or layer from another's, the denoiser's tiles or filters
// scrambling a denoised slot away from its shadows' edges, rays toward the
// wrong end (a light's direction reversed), rays meeting the receiver's
// own face, rays that ignore static or moving instances, a light's slot
// naming another light, rays toward a light that does not reach the
// receiver (beyond its range or a spot's cone), and the upsample fetching
// other texels than the one an even pixel holds. The oracle is the boxes'
// geometry in f64, the slab test from each floor pixel's position (its
// depth reconstructed in f64) toward each light, beside its reach: a light
// that does not reach the floor leaves no visibility there. The denoised
// sun is the oracle's away from its edges, which the denoiser filters.
#[test]
fn the_mask_matches_a_cpu_oracle_of_occlusion() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let blocks = [
        Block::new(
            Vec3::new(0., 1.4, 0.),
            Vec3::new(0.7, 0.25, 0.5),
            Mobility::Static,
        ),
        Block::new(
            Vec3::new(-1.6, 2., -1.5),
            Vec3::splat(0.4),
            Mobility::Moving,
        ),
        Block::new(
            Vec3::new(2.4, 2.5, -1.3),
            Vec3::new(0.3, 0.2, 0.3),
            Mobility::Static,
        ),
        // Above the local lights: a broad shadow of the sun alone on the
        // floor before the other blocks, in full view, whose inside lies
        // beyond the denoiser's reach from its edges.
        Block::new(
            Vec3::new(-3.9, 5.5, 2.35),
            Vec3::new(3., 0.05, 1.75),
            Mobility::Static,
        ),
    ];
    // Hard lights: rays end at their centres and directions.
    let point = Light {
        position: Vec3::new(-2., 4., 1.),
        shape: LightShape::Point { radius: 0. },
        intensity: 50.,
        range: 9.,
        casts_shadow: true,
        ..Light::default()
    };
    let spot = Light {
        position: Vec3::new(2.5, 5., -1.),
        shape: LightShape::Spot {
            direction: Vec3::new(0., -1., 0.1),
            inner_angle: 0.3,
            outer_angle: 0.45,
            radius: 0.,
        },
        intensity: 80.,
        range: 15.,
        casts_shadow: true,
        ..Light::default()
    };
    let eye = Vec3::new(0., 10., 8.);
    let decoys = decoys(eye);
    let ringed = RT_SHADOW_LIGHTS - 1 - decoys.len() - 2;
    let ring = (0..ringed).map(|index| {
        let angle = index as f32 / ringed as f32 * std::f32::consts::TAU;
        let position = Vec3::new(3.2 * angle.cos(), 4.5, 3.2 * angle.sin());
        Light {
            position,
            shape: LightShape::Spot {
                direction: blocks[0].centre - position,
                inner_angle: 0.5,
                outer_angle: 0.7,
                radius: 0.,
            },
            intensity: 20.,
            range: 12.,
            casts_shadow: true,
            ..Light::default()
        }
    });
    let lights: Vec<Light> = [point, spot].into_iter().chain(ring).collect();
    let all: Vec<Light> = decoys
        .iter()
        .copied()
        .chain(lights.iter().copied())
        .collect();
    let (mut scene, ids) = scene(gpu, 8., &blocks, &all);
    let sun = DirectionalLight {
        direction: Vec3::new(0.35, -1., 0.25),
        illuminance: 3.,
        shadow: Some(DirectionalShadow::DEFAULT),
        angular_diameter: 0.,
        ..DirectionalLight::default()
    };
    let size = [256, 192];
    let camera = camera(eye, Vec3::new(0., 0., -0.5), size);
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut input = input(camera, Some(sun));
    let to_sun = -sun.direction.as_dvec3().normalize();
    for cut in [true, false] {
        let frame = if cut { "cut" } else { "still" };
        input.camera_cut = cut;
        let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
        assert!(renderer.ray_traced_shadows_in_effect(&settings));
        let mut held: Vec<usize> = std::iter::once(SHADOW_MASK_DIRECTIONAL)
            .chain(ids.iter().map(|id| id.index() as u32))
            .map(|key| observed.slot(key))
            .collect();
        held.sort_unstable();
        assert_eq!(
            held,
            (0..RT_SHADOW_LIGHTS).collect::<Vec<_>>(),
            "every slot holds one light"
        );
        let sun_slot = observed.slot(SHADOW_MASK_DIRECTIONAL);
        assert_eq!(sun_slot, 0, "the directional light holds slot 0");
        let slots: Vec<usize> = ids[decoys.len()..]
            .iter()
            .map(|id| observed.slot(id.index() as u32))
            .collect();
        assert!(
            slots
                .iter()
                .all(|&slot| slot >= super::denoise::DENOISED_SLOTS as usize),
            "the decoys outrank the checked lights: {slots:?}"
        );
        let texels = floor_texels(&observed, &camera);
        let mut decided = vec![[0; 2]; 1 + slots.len()];
        let mut sunlit = std::collections::HashMap::new();
        for &(pixel, position) in &texels {
            if let Some(visible) =
                occluded(&blocks, position, to_sun, (0.01, f64::from(f32::MAX))).map(|o| !o)
            {
                sunlit.insert(pixel, visible);
            }
            for (light, (&slot, source)) in slots.iter().zip(&lights).enumerate() {
                let Some(visible) = local_visibility(source, position, &blocks) else {
                    continue;
                };
                let byte = observed.mask(slot, pixel);
                let expected = if visible == Some(true) { 255 } else { 0 };
                assert_eq!(
                    byte, expected,
                    "frame {frame}: light {light} in slot {slot} at {pixel:?} ({position:?}): the oracle says {visible:?}",
                );
                decided[1 + light][usize::from(visible == Some(true))] += 1;
            }
        }
        for pixel in interior(&sunlit, DENOISER_REACH) {
            let visible = sunlit[&pixel];
            assert_eq!(
                observed.mask(sun_slot, pixel),
                if visible { 255 } else { 0 },
                "frame {frame}: the sun in slot 0 at {pixel:?}: the oracle says {visible}"
            );
            decided[0][usize::from(visible)] += 1;
        }
        for (light, [shadowed, lit]) in decided.into_iter().enumerate() {
            assert!(
                shadowed > 20 && lit > 20,
                "frame {frame}: light {light}: only {shadowed} shadowed and {lit} lit floor texels decided of {}",
                texels.len()
            );
        }
    }
}

/// The lit colour's red over the floor pixels of tracing texels where the
/// oracle `visible` decides a light's visibility (Some), with ray-traced
/// shadows and without: (pixel, decided visibility, red with, red
/// without).
fn lit_with_and_without(
    gpu: (&wgpu::Device, &wgpu::Queue),
    (scene, input): (&mut Scene, &FrameInput),
    size: [u32; 2],
    visible: impl Fn(DVec3) -> Option<bool>,
) -> Vec<([u32; 2], bool, f32, f32)> {
    let frames: Vec<_> = [true, false]
        .map(|traced| {
            let settings = settings(traced);
            let mut renderer = Renderer::for_test(gpu.0, gpu.1, size, &settings);
            let observed = render(gpu, &mut renderer, scene, input, &settings);
            assert_eq!(observed.mask.is_some(), traced);
            observed
        })
        .into();
    floor_texels(&frames[0], &input.camera)
        .into_iter()
        .filter_map(|([x, y], position)| {
            let at = (y * size[0] + x) as usize;
            visible(position).map(|visible| ([x, y], visible, frames[0].red[at], frames[1].red[at]))
        })
        .collect()
}

/// Asserts that ray-traced shadows darken the `samples` the oracle shadows
/// and leave the lit ones as the maps light them, which leave every one lit,
/// away from the shadows' edges, which the denoiser filters.
fn assert_rays_alone_shadow(samples: &[([u32; 2], bool, f32, f32)], label: &str) {
    let decisions = samples
        .iter()
        .map(|&(pixel, visible, _, _)| (pixel, visible))
        .collect();
    let inside: std::collections::HashSet<_> =
        interior(&decisions, DENOISER_REACH).into_iter().collect();
    let samples: Vec<_> = samples
        .iter()
        .filter(|sample| inside.contains(&sample.0))
        .map(|&(_, visible, with, without)| (visible, with, without))
        .collect();
    let shadowed: Vec<_> = samples.iter().filter(|sample| !sample.0).collect();
    let lit: Vec<_> = samples.iter().filter(|sample| sample.0).collect();
    assert!(
        shadowed.len() > 20 && lit.len() > 20,
        "{label}: {} shadowed and {} lit samples",
        shadowed.len(),
        lit.len()
    );
    for &&(_, with, without) in &shadowed {
        assert!(
            without > 0.05 && with < without * 0.05,
            "{label}: a shadowed pixel is {with} with rays and {without} with the maps"
        );
    }
    for &&(_, with, without) in &lit {
        assert!(
            without > 0.05 && (with - without).abs() <= without * 0.02,
            "{label}: a lit pixel is {with} with rays and {without} with the maps"
        );
    }
}

// Plausible defects: the lighting pass composing the provider that holds no
// slot, binding another mask or slot table, looking up a light's slot by
// another key, or reading another layer or channel than its slot's; the
// opaque stage taking its fused form, which binds no mask; and the
// directional light's rays stopping at the cascades' distance. The oracle
// is geometric, where the maps hold no shadow and rays do: a directional
// shadow whose cascades reach 1 m from the camera, while the floor lies
// beyond 7 m; and a point light 1 cm above a double-sided plate, within
// the 2 cm the local-light maps' faces clip at their near plane, while a
// ray from the floor beneath it ends at the light and so meets the plate,
// which covers the rays from the floor's +x half alone.
// With rays, a pixel the oracle shadows is dark and a lit one as the maps
// light it; with the maps, both are lit.
#[test]
fn the_lighting_pass_takes_the_shadows_the_maps_lack_from_the_mask() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [384, 288];
    let camera = camera(Vec3::new(0., 7., 7.), Vec3::ZERO, size);
    // The directional light beyond its cascades: a broad plate whose
    // shadow falls toward the camera, in full view.
    let block = Block::new(
        Vec3::new(0., 4., -2.),
        Vec3::new(3., 0.1, 2.5),
        Mobility::Static,
    );
    let (mut sun_scene, _) = scene(gpu, 8., &[block], &[]);
    let sun = DirectionalLight {
        direction: Vec3::new(0.3, -1., 0.8),
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: 1.,
            cascades: 1,
        }),
        angular_diameter: 0.,
        ..DirectionalLight::default()
    };
    let to_sun = -sun.direction.as_dvec3().normalize();
    let samples = lit_with_and_without(
        gpu,
        (&mut sun_scene, &input(camera, Some(sun))),
        size,
        |position| occluded(&[block], position, to_sun, (0.01, f64::from(f32::MAX))).map(|o| !o),
    );
    assert_rays_alone_shadow(&samples, "directional light beyond its cascades");
    // A point light over a plate within its maps' near plane.
    let light = Light {
        position: Vec3::new(0., 3., 0.),
        shape: LightShape::Point { radius: 0. },
        intensity: 40.,
        range: 10.,
        casts_shadow: true,
        ..Light::default()
    };
    let (mut plate_scene, _) = scene(gpu, 8., &[], &[light]);
    let plate = plate_scene
        .add_asset(&device, &queue, quad(0.3, true))
        .unwrap()
        .model;
    plate_scene
        .add_instance(
            &device,
            &queue,
            InstanceState {
                pose: Mat4::from_scale_rotation_translation(
                    Vec3::new(0.1, 1., 1.),
                    Quat::IDENTITY,
                    Vec3::new(0.03, 2.99, 0.),
                ),
                ..InstanceState::new(plate)
            },
            Mobility::Static,
        )
        .unwrap();
    // The plate for the oracle, thicker than its margin so that its slab
    // decides the rays through it.
    let plate_block = Block::new(
        Vec3::new(0.03, 2.99, 0.),
        Vec3::new(0.03, 0.003, 0.3),
        Mobility::Static,
    );
    let samples = lit_with_and_without(
        gpu,
        (&mut plate_scene, &input(camera, None)),
        size,
        |position| {
            // Beneath the light, where its maps' downward face holds the
            // floor.
            let below = light.position.as_dvec3() - position;
            if below.x.abs().max(below.z.abs()) >= below.y * 0.9 {
                return None;
            }
            local_visibility(&light, position, &[plate_block]).flatten()
        },
    );
    assert_rays_alone_shadow(&samples, "point light over a plate within its near plane");
}

/// A grate of bars 1.2 m above the floor, 0.24 m apart across z, which a
/// light above it stripes onto the floor about every three tracing texels.
fn grate() -> Vec<Block> {
    (-10..=10)
        .map(|bar| {
            Block::new(
                Vec3::new(0., 1.2, bar as f32 * 0.24),
                Vec3::new(3., 0.03, 0.06),
                Mobility::Static,
            )
        })
        .collect()
}

/// How many of the floor's even pixels hold a visibility of the slot that
/// holds `key` between unoccluded and occluded, and how many hold each.
fn blended(observed: &Observed, camera: &Camera, key: u32) -> [usize; 3] {
    let slot = observed.slot(key);
    let mut counts = [0; 3];
    for (pixel, _) in floor_texels(observed, camera) {
        counts[match observed.mask(slot, pixel) {
            0 => 0,
            255 => 2,
            _ => 1,
        }] += 1;
    }
    counts
}

// Plausible defects: a slot whose light changed keeping its history (the
// restart bit ignored or never set), a camera cut, a frame without the
// stage or a resize leaving the stage's history in place, or history
// reprojected from another texel. The oracle
// is the restart itself: a light striped through a grate, whose stripes
// are about three tracing texels wide, so that every texel's 3×3
// neighbourhood holds both values and the variance clamp keeps any
// history; history from stripes elsewhere then shows as visibilities
// between 0 and 1, and a restarted slot holds the trace's 0 or 1 alone. A
// frame that moves the light without a restart is the control: its
// history does show.
#[test]
fn a_slot_whose_light_changed_and_a_camera_cut_restart_its_history() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [128, 128];
    let at = |z: f32, casts_shadow: bool| Light {
        position: Vec3::new(0., 6., z),
        shape: LightShape::Point { radius: 0. },
        intensity: 60.,
        range: 20.,
        casts_shadow,
        ..Light::default()
    };
    let eye = Vec3::new(0., 9., 3.);
    let [one, two, three] = decoys(eye);
    let (mut scene, ids) = scene(
        gpu,
        6.,
        &grate(),
        &[one, two, three, at(-0.3, true), at(0.3, false)],
    );
    let [first, second] = [ids[3], ids[4]];
    let camera = camera(eye, Vec3::ZERO, size);
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut input = input(camera, None);
    input.camera_cut = true;
    let mut frame = |scene: &mut Scene, cut: bool, settings: &Settings| {
        input.camera_cut = cut;
        render(gpu, &mut renderer, scene, &input, settings)
    };
    frame(&mut scene, true, &settings);
    let settled = frame(&mut scene, false, &settings);
    let [_, between, _] = blended(&settled, &camera, first.index() as u32);
    assert_eq!(between, 0, "a still light's history matches its trace");
    // The control: the light moves half a stripe without a restart, and
    // its history shows.
    scene.set_light(&queue, first, at(0.3, true)).unwrap();
    let control = frame(&mut scene, false, &settings);
    let [_, between, _] = blended(&control, &camera, first.index() as u32);
    assert!(
        between > 100,
        "the moved light's history shows at {between} texels"
    );
    // The other light takes the first's slot: its history restarts.
    scene.set_light(&queue, first, at(0.3, false)).unwrap();
    scene.set_light(&queue, second, at(-0.3, true)).unwrap();
    let changed = frame(&mut scene, false, &settings);
    assert_eq!(
        changed.slot(second.index() as u32),
        settled.slot(first.index() as u32),
        "the second light takes the first's freed slot"
    );
    let [shadowed, between, lit] = blended(&changed, &camera, second.index() as u32);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "after its light changed the slot holds {shadowed} shadowed, {between} blended, {lit} lit"
    );
    // A camera cut restarts every slot.
    scene.set_light(&queue, second, at(0.3, true)).unwrap();
    let cut = frame(&mut scene, true, &settings);
    let [shadowed, between, lit] = blended(&cut, &camera, second.index() as u32);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "after a camera cut the slot holds {shadowed} shadowed, {between} blended, {lit} lit"
    );
    // A frame without the stage restarts it: the maps shadow that frame,
    // and the next, with the light moved back, holds its trace alone.
    let maps = Settings {
        ray_traced_shadows: false,
        ..settings
    };
    assert!(frame(&mut scene, false, &maps).mask.is_none());
    scene.set_light(&queue, second, at(-0.3, true)).unwrap();
    let resumed = frame(&mut scene, false, &settings);
    let [shadowed, between, lit] = blended(&resumed, &camera, second.index() as u32);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "after a frame without the stage the slot holds {shadowed} shadowed, {between} blended, {lit} lit"
    );
    // So does a resize, which reallocates its targets.
    scene.set_light(&queue, second, at(0.3, true)).unwrap();
    renderer.resize(&device, [120, 120], 1., &settings);
    let mut input = self::input(camera, None);
    input.camera_cut = false;
    let resized = render(gpu, &mut renderer, &mut scene, &input, &settings);
    let [shadowed, between, lit] = blended(&resized, &camera, second.index() as u32);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "after a resize the slot holds {shadowed} shadowed, {between} blended, {lit} lit"
    );
}

// Plausible defects: history reprojected along the motion the wrong way,
// by a scaled or flipped motion, or from the texel beside the nearest; and
// the upsample fetching other texels for an odd pixel than the two or four
// about it, or weighing them otherwise than evenly at a pixel halfway
// between them. The oracle is a camera looking straight down on the grate's
// stripes, whose floor lies at one depth, moved along the floor so that
// the floor's image shifts by exactly two tracing texels: history
// reprojected right holds the same stripes as the trace, so every floor
// texel keeps its 0 or 1, where history from anywhere else lies across a
// stripe's edge and blends; and each odd pixel of the floor is the mean of
// the tracing texels about it, which the even pixels show.
#[test]
fn history_follows_the_camera_and_odd_pixels_take_the_texels_about_them() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [128, 128];
    let light = Light {
        position: Vec3::new(0., 6., -0.3),
        shape: LightShape::Point { radius: 0. },
        intensity: 60.,
        range: 20.,
        casts_shadow: true,
        ..Light::default()
    };
    let height = 9.;
    let [one, two, three] = decoys(Vec3::new(0., height, 0.));
    let (mut scene, ids) = scene(gpu, 6., &grate(), &[one, two, three, light]);
    let key = ids[3].index() as u32;
    let fov = 0.9_f32;
    let looking_down = |x: f32| Camera {
        view: glam::camera::rh::view::look_at_mat4(
            Vec3::new(x, height, 0.),
            Vec3::new(x, 0., 0.),
            Vec3::NEG_Z,
        ),
        projection: crate::perspective(fov, 1., 0.1),
        eye: Vec3::new(x, height, 0.),
    };
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut input = input(looking_down(0.), None);
    input.camera_cut = true;
    let still = render(gpu, &mut renderer, &mut scene, &input, &settings);
    // The upsample: on the floor, every odd pixel is the mean of the even
    // pixels about it, which hold the tracing texels.
    let slot = still.slot(key);
    let floor = |observed: &Observed, camera: &Camera, [x, y]: [u32; 2]| {
        observed
            .position(camera, [x, y])
            .is_some_and(|position| position.y.abs() < 1e-3)
    };
    let mut odd = 0;
    for y in 1..size[1] - 2 {
        for x in 1..size[0] - 2 {
            if x % 2 == 0 && y % 2 == 0 {
                continue;
            }
            let about: Vec<[u32; 2]> = [x - x % 2, x + x % 2]
                .into_iter()
                .flat_map(|ex| [y - y % 2, y + y % 2].map(move |ey| [ex, ey]))
                .collect();
            if !about
                .iter()
                .chain([&[x, y]])
                .all(|&pixel| floor(&still, &input.camera, pixel))
            {
                continue;
            }
            let mean = about
                .iter()
                .map(|&pixel| f32::from(still.mask(slot, pixel)))
                .sum::<f32>()
                / about.len() as f32;
            let value = f32::from(still.mask(slot, [x, y]));
            assert!(
                (value - mean).abs() <= 1.,
                "odd pixel {x}, {y} holds {value}, its texels' mean {mean}"
            );
            odd += 1;
        }
    }
    assert!(odd > 1000, "only {odd} odd floor pixels checked");
    // The camera moves 2 tracing texels (4 pixels) along the floor's image.
    let pixels_per_metre = size[1] as f32 / (2. * height * (fov / 2.).tan());
    let moved = looking_down(4. / pixels_per_metre);
    let mut input = self::input(moved, None);
    input.camera_cut = false;
    let followed = render(gpu, &mut renderer, &mut scene, &input, &settings);
    let [shadowed, between, lit] = blended(&followed, &moved, key);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "after the camera moved the slot holds {shadowed} shadowed, {between} blended, {lit} lit"
    );
}

/// Wicked Engine's get_tangentspace and hemispherepoint_cos in f64: the
/// point of the cosine-weighted unit hemisphere about unit `normal` that
/// `u` and `v` in [0, 1) draw.
fn hemisphere_point(normal: DVec3, u: f64, v: f64) -> DVec3 {
    let helper = if normal.x.abs() > 0.99 {
        DVec3::Z
    } else {
        DVec3::X
    };
    let tangent = normal.cross(helper).normalize();
    let binormal = normal.cross(tangent).normalize();
    let phi = v * std::f64::consts::TAU;
    let cos_theta = (1. - u).sqrt();
    let sin_theta = (1. - cos_theta * cos_theta).sqrt();
    tangent * (phi.cos() * sin_theta) + binormal * (phi.sin() * sin_theta) + normal * cos_theta
}

/// The share of the rays toward a light, over a stratified grid of the
/// draws light_surface.wgsl makes, that `blocks` grown by `grow` leave
/// clear from `position`; `ray` gives each draw's direction and length.
fn lit_share(
    blocks: &[Block],
    grow: f64,
    position: DVec3,
    ray: impl Fn(f64, f64) -> (DVec3, f64),
) -> f64 {
    const STRATA: usize = 12;
    let mut clear = 0;
    for i in 0..STRATA {
        for j in 0..STRATA {
            let (u, v) = (
                (i as f64 + 0.5) / STRATA as f64,
                (j as f64 + 0.5) / STRATA as f64,
            );
            let (direction, length) = ray(u, v);
            if !blocks
                .iter()
                .any(|block| block.crosses(grow, position, direction, (0.01, length)))
            {
                clear += 1;
            }
        }
    }
    f64::from(clear) / (STRATA * STRATA) as f64
}

/// Where the oracle puts a floor texel against a light with a size: wholly
/// shadowed, wholly lit, or in the middle of its penumbra (a quarter to
/// three quarters lit); none near those bounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Penumbra {
    Umbra,
    Lit,
    Middle,
}

/// The oracle's `Penumbra` of each floor texel of `observed` against a
/// light whose rays `ray` draws, which `blocks` occlude: decided only where
/// growing and shrinking the blocks by `MARGIN` agree.
fn penumbrae(
    observed: &Observed,
    camera: &Camera,
    blocks: &[Block],
    ray: impl Fn(DVec3, f64, f64) -> (DVec3, f64),
) -> std::collections::HashMap<[u32; 2], Penumbra> {
    floor_texels(observed, camera)
        .into_iter()
        .filter_map(|(pixel, position)| {
            let most = lit_share(blocks, -MARGIN, position, |u, v| ray(position, u, v));
            let least = lit_share(blocks, MARGIN, position, |u, v| ray(position, u, v));
            let penumbra = if most == 0. {
                Penumbra::Umbra
            } else if least == 1. {
                Penumbra::Lit
            } else if least > 0.25 && most < 0.75 {
                Penumbra::Middle
            } else {
                return None;
            };
            Some((pixel, penumbra))
        })
        .collect()
}

/// Asserts that `observed`'s `slot` holds `penumbrae`: under 0.1 inside the
/// umbra and over 0.9 inside the lit floor (3 texels from either's edge,
/// within which the denoiser filters), and between in most of the
/// penumbra's middle. Returns how many of the floor's texels hold a
/// visibility between 0.04 and 0.96.
fn assert_penumbrae(
    observed: &Observed,
    slot: usize,
    penumbrae: &std::collections::HashMap<[u32; 2], Penumbra>,
    label: &str,
) -> usize {
    let inside = |kind: Penumbra| {
        let decisions = penumbrae
            .iter()
            .map(|(&pixel, &penumbra)| (pixel, penumbra == kind))
            .collect();
        interior(&decisions, 3)
            .into_iter()
            .filter(move |pixel| penumbrae[pixel] == kind)
    };
    let umbra: Vec<_> = inside(Penumbra::Umbra).collect();
    let lit: Vec<_> = inside(Penumbra::Lit).collect();
    assert!(
        umbra.len() > 20 && lit.len() > 20,
        "{label}: {} umbra and {} lit texels inside",
        umbra.len(),
        lit.len()
    );
    for pixel in umbra {
        let byte = observed.mask(slot, pixel);
        assert!(byte < 26, "{label}: umbra texel {pixel:?} holds {byte}");
    }
    for pixel in lit {
        let byte = observed.mask(slot, pixel);
        assert!(byte > 229, "{label}: lit texel {pixel:?} holds {byte}");
    }
    let middle: Vec<_> = penumbrae
        .iter()
        .filter(|&(_, &penumbra)| penumbra == Penumbra::Middle)
        .map(|(&pixel, _)| observed.mask(slot, pixel))
        .collect();
    let between = middle
        .iter()
        .filter(|&&byte| byte > 10 && byte < 245)
        .count();
    assert!(
        middle.len() > 30 && between * 10 > middle.len() * 6,
        "{label}: {between} of {} texels in the penumbra's middle hold neither 0 nor 1",
        middle.len()
    );
    penumbrae
        .keys()
        .filter(|&&pixel| (10..245).contains(&observed.mask(slot, pixel)))
        .count()
}

// Plausible defects: a light's size ignored, so its rays all end at its
// centre (a point or spot light's radius, a directional light's angular
// diameter) and its shadow stays hard; the size taken in other units (a
// diameter for a radius, degrees for radians) or drawn about the wrong
// axis, which moves the penumbra's bounds; and the draws not turning from
// frame to frame, which leaves a penumbra's texels 0 or 1, never between.
// The oracle is the geometry: an occluder between the floor and the light,
// and the share of the rays toward the light, drawn over a stratified grid
// as light_surface.wgsl draws them, that each floor texel sees clear, in
// f64. Once converged, the umbra holds 0, the lit floor 1 and the
// penumbra's middle neither.
//
// A denoised slot cannot tell a soft shadow from a hard one at its edge:
// AMD's tile classification softens a hard edge by design
// (ffx_denoiser_shadows_tileclassification.h 380-405, which Wicked runs),
// as the spatial variance about the edge keeps the sample count damped
// and the variance boosted, so its filters blur a few texels each side.
// So the point light is compared with a hard one in a slot the denoiser
// leaves (decoys outrank it for the denoised ones), where the temporal
// blend keeps a hard edge exactly 0 or 1; and the sun, which always holds
// denoised slot 0, beyond the denoiser's reach of its hard edge, where a
// hard sun leaves 0 or 1 and one with a size leaves its penumbra's middle.
#[test]
fn a_light_with_a_size_softens_its_shadow() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    // Converged frames of a scene with `blocks`, `lights` and `sun`, seen by
    // `camera` over `size`.
    let frames = |blocks: &[Block],
                  lights: &[Light],
                  sun: Option<DirectionalLight>,
                  camera: Camera,
                  size: [u32; 2]| {
        let (mut scene, ids) = scene(gpu, 8., blocks, lights);
        let settings = settings(true);
        let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
        let mut input = input(camera, sun);
        let mut observed = None;
        for frame in 0..32 {
            input.camera_cut = frame == 0;
            observed = Some(render(gpu, &mut renderer, &mut scene, &input, &settings));
        }
        (observed.unwrap(), ids)
    };
    // A point light of radius 0.8 m, over a slab, looked at from the side so
    // that the slab hides only the floor beyond its shadows.
    let size = [256, 256];
    let eye = Vec3::new(0., 9., 9.);
    let camera = camera(eye, Vec3::ZERO, size);
    let slab = Block::new(
        Vec3::new(0., 2.5, 0.),
        Vec3::new(1.6, 0.05, 1.6),
        Mobility::Static,
    );
    let point = |radius: f32| Light {
        position: Vec3::new(0.4, 6., 0.3),
        shape: LightShape::Point { radius },
        intensity: 60.,
        range: 20.,
        casts_shadow: true,
        ..Light::default()
    };
    let decoys = decoys(eye);
    let point_frames = |radius: f32| {
        let lights: Vec<Light> = decoys.iter().copied().chain([point(radius)]).collect();
        let (observed, ids) = frames(&[slab], &lights, None, camera, size);
        let slot = observed.slot(ids[decoys.len()].index() as u32);
        assert!(
            slot >= super::denoise::DENOISED_SLOTS as usize,
            "the decoys outrank the point light: slot {slot}"
        );
        (observed, slot)
    };
    let (soft, slot) = point_frames(0.8);
    let centre = point(0.8).position.as_dvec3();
    let point_penumbrae = penumbrae(&soft, &camera, &[slab], |position, u, v| {
        let end = centre + hemisphere_point((centre - position).normalize(), u, v) * 0.8;
        let to = end - position;
        (to.normalize(), to.length())
    });
    let soft_between = assert_penumbrae(&soft, slot, &point_penumbrae, "point light");
    let (hard, slot) = point_frames(0.);
    let hard_between = point_penumbrae
        .keys()
        .filter(|&&pixel| (10..245).contains(&hard.mask(slot, pixel)))
        .count();
    assert!(
        hard_between * 4 < soft_between,
        "a point light of size 0 leaves {hard_between} texels between, one of radius 0.8 m {soft_between}"
    );
    // The sun, 30° across, past a wall, looked at from above: the wall's
    // shadow widens away from it, its penumbra's middle reaching beyond the
    // denoiser's reach of the hard shadow's edge.
    let size = [512, 512];
    let height = 9.;
    let looking_at = Vec3::new(1.2, 0., 0.);
    let camera = Camera {
        view: glam::camera::rh::view::look_at_mat4(
            looking_at + Vec3::Y * height,
            looking_at,
            Vec3::NEG_Z,
        ),
        projection: crate::perspective(0.9, 1., 0.1),
        eye: looking_at + Vec3::Y * height,
    };
    let wall = Block::new(
        Vec3::new(0., 1.5, 0.),
        Vec3::new(0.05, 1.5, 3.),
        Mobility::Static,
    );
    let sun = |angular_diameter: f32| DirectionalLight {
        direction: Vec3::new(0.6, -1., 0.),
        illuminance: 3.,
        shadow: Some(DirectionalShadow::DEFAULT),
        angular_diameter,
        ..DirectionalLight::default()
    };
    let toward = -sun(0.).direction.as_dvec3().normalize();
    let disc = (15f64).to_radians().tan();
    let (soft, _) = frames(&[wall], &[], Some(sun(30f32.to_radians())), camera, size);
    let sun_penumbrae = penumbrae(&soft, &camera, &[wall], |_, u, v| {
        (
            (toward + hemisphere_point(toward, u, v) * disc).normalize(),
            f64::from(f32::MAX),
        )
    });
    assert_penumbrae(&soft, 0, &sun_penumbrae, "the sun");
    let (hard, _) = frames(&[wall], &[], Some(sun(0.)), camera, size);
    let hard_decisions: std::collections::HashMap<[u32; 2], bool> =
        penumbrae(&hard, &camera, &[wall], |_, _, _| {
            (toward, f64::from(f32::MAX))
        })
        .into_iter()
        .map(|(pixel, penumbra)| (pixel, penumbra == Penumbra::Lit))
        .collect();
    let mut beyond = 0;
    let mut between = 0;
    for pixel in interior(&hard_decisions, DENOISER_REACH) {
        let byte = hard.mask(0, pixel);
        let lit = hard_decisions[&pixel];
        assert!(
            if lit { byte > 229 } else { byte < 26 },
            "a hard sun holds {byte} at {pixel:?}, which the oracle lights: {lit}"
        );
        if sun_penumbrae.get(&pixel) == Some(&Penumbra::Middle) {
            beyond += 1;
            between += usize::from((10..245).contains(&soft.mask(0, pixel)));
        }
    }
    assert!(
        beyond > 20 && between * 10 > beyond * 6,
        "{between} of {beyond} texels of the sun's penumbra beyond the denoiser's reach of the hard edge hold neither 0 nor 1"
    );
}
