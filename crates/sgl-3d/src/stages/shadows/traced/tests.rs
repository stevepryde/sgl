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

// The smallest scene first: one point light, one block, one frame.
// Plausible defects: the trace reading another pixel's depth or normal than
// the mask's, the visibility of one light written into another's slot or
// another slot's layer or channel, rays toward the wrong end (a light's
// direction reversed), rays meeting the receiver's own face, rays that
// ignore static or moving instances, a light's slot naming another light,
// rays toward a light that does not reach the receiver (beyond its range
// or a spot's cone), and the upsample fetching other texels than the one
// an even pixel holds. The oracle is the boxes' geometry in f64, the slab
// test from each floor pixel's position (its depth reconstructed in f64)
// toward each light, beside its reach: a light that does not reach the
// floor leaves no visibility there.
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
    ];
    let point = Light {
        position: Vec3::new(-2., 4., 1.),
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
            radius: LightShape::DEFAULT_RADIUS,
        },
        intensity: 80.,
        range: 15.,
        casts_shadow: true,
        ..Light::default()
    };
    let (mut scene, ids) = scene(gpu, 8., &blocks, &[point, spot]);
    let sun = DirectionalLight {
        direction: Vec3::new(0.35, -1., 0.25),
        illuminance: 3.,
        shadow: Some(DirectionalShadow::DEFAULT),
        ..DirectionalLight::default()
    };
    let size = [128, 96];
    let camera = camera(Vec3::new(0., 10., 8.), Vec3::new(0., 0., -0.5), size);
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut input = input(camera, Some(sun));
    input.camera_cut = true;
    let observed = render(gpu, &mut renderer, &mut scene, &input, &settings);
    assert!(renderer.ray_traced_shadows_in_effect(&settings));
    let slots = [
        observed.slot(SHADOW_MASK_DIRECTIONAL),
        observed.slot(ids[0].index() as u32),
        observed.slot(ids[1].index() as u32),
    ];
    assert_eq!(slots[0], 0, "the directional light holds slot 0");
    let to_sun = -sun.direction.as_dvec3().normalize();
    let texels = floor_texels(&observed, &camera);
    let mut decided = [[0; 2]; 3];
    for &(pixel, position) in &texels {
        let expected = [
            occluded(&blocks, position, to_sun, (0.01, f64::from(f32::MAX))).map(|o| Some(!o)),
            local_visibility(&point, position, &blocks),
            local_visibility(&spot, position, &blocks),
        ];
        for (light, (expected, slot)) in expected.into_iter().zip(slots).enumerate() {
            let Some(visible) = expected else {
                continue;
            };
            let byte = observed.mask(slot, pixel);
            let expected = if visible == Some(true) { 255 } else { 0 };
            assert_eq!(
                byte, expected,
                "light {light} in slot {slot} at {pixel:?} ({position:?}): the oracle says {visible:?}"
            );
            decided[light][usize::from(visible == Some(true))] += 1;
        }
    }
    for (light, [shadowed, lit]) in decided.into_iter().enumerate() {
        assert!(
            shadowed > 20 && lit > 20,
            "light {light}: only {shadowed} shadowed and {lit} lit floor texels decided of {}",
            texels.len()
        );
    }
    // A slot no light holds stays empty in the table.
    let (_, table) = observed.mask.as_ref().unwrap();
    let held = table
        .lights
        .iter()
        .flatten()
        .filter(|&&key| key != crate::shading::shadow_mask::SHADOW_MASK_EMPTY)
        .count();
    assert_eq!(held, 3, "of {RT_SHADOW_LIGHTS} slots");
}

/// The lit colour's red over the floor pixels of tracing texels where the
/// oracle `visible` decides a light's visibility (Some), with ray-traced
/// shadows and without: (decided visibility, red with, red without).
fn lit_with_and_without(
    gpu: (&wgpu::Device, &wgpu::Queue),
    (scene, input): (&mut Scene, &FrameInput),
    size: [u32; 2],
    visible: impl Fn(DVec3) -> Option<bool>,
) -> Vec<(bool, f32, f32)> {
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
            visible(position).map(|visible| (visible, frames[0].red[at], frames[1].red[at]))
        })
        .collect()
}

/// Asserts that ray-traced shadows darken the `samples` the oracle shadows
/// and leave the lit ones as the maps light them, which leave every one lit.
fn assert_rays_alone_shadow(samples: &[(bool, f32, f32)], label: &str) {
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
// beyond 9 m; and a point light 1 cm above a double-sided plate, within
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
    let size = [128, 96];
    let camera = camera(Vec3::new(0., 7., 7.), Vec3::ZERO, size);
    // The directional light beyond its cascades.
    let block = Block::new(
        Vec3::new(0., 1., 0.),
        Vec3::new(1., 0.2, 0.6),
        Mobility::Static,
    );
    let (mut sun_scene, _) = scene(gpu, 8., &[block], &[]);
    let sun = DirectionalLight {
        direction: Vec3::new(0.8, -1., 0.5),
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: 1.,
            cascades: 1,
        }),
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
// restart bit ignored or never set), a camera cut leaving the stage's
// history in place, or history reprojected from another texel. The oracle
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
        intensity: 60.,
        range: 20.,
        casts_shadow,
        ..Light::default()
    };
    let (mut scene, ids) = scene(gpu, 6., &grate(), &[at(-0.3, true), at(0.3, false)]);
    let [first, second] = [ids[0], ids[1]];
    let camera = camera(Vec3::new(0., 9., 3.), Vec3::ZERO, size);
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut input = input(camera, None);
    input.camera_cut = true;
    let mut frame = |scene: &mut Scene, cut: bool| {
        input.camera_cut = cut;
        render(gpu, &mut renderer, scene, &input, &settings)
    };
    frame(&mut scene, true);
    let settled = frame(&mut scene, false);
    let [_, between, _] = blended(&settled, &camera, first.index() as u32);
    assert_eq!(between, 0, "a still light's history matches its trace");
    // The control: the light moves half a stripe without a restart, and
    // its history shows.
    scene.set_light(&queue, first, at(0.3, true)).unwrap();
    let control = frame(&mut scene, false);
    let [_, between, _] = blended(&control, &camera, first.index() as u32);
    assert!(
        between > 100,
        "the moved light's history shows at {between} texels"
    );
    // The other light takes the first's slot: its history restarts.
    scene.set_light(&queue, first, at(0.3, false)).unwrap();
    scene.set_light(&queue, second, at(-0.3, true)).unwrap();
    let changed = frame(&mut scene, false);
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
    let cut = frame(&mut scene, true);
    let [shadowed, between, lit] = blended(&cut, &camera, second.index() as u32);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "after a camera cut the slot holds {shadowed} shadowed, {between} blended, {lit} lit"
    );
}
