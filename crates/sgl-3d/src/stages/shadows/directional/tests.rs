//! Which directional light casts the shadow, and what its cascades shadow,
//! observed in the lit colour of real frames.
use crate::asset::{CpuMesh, Vertex};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::view::cascades::SHADOW_PANCAKE_SIZE;
use crate::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, FrameInput, InstanceState, Mobility,
    Scene, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [32, 32];

/// A quad facing +Z at `center`, `half` metres from its centre to each side.
fn quad(center: Vec3, half: f32) -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (center + Vec3::new(x, y, 0.) * half).to_array(),
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// Two cascades out to 10 m.
fn two_cascades() -> DirectionalShadow {
    DirectionalShadow {
        distance: 10.,
        cascades: 2,
    }
}

/// An occluder's place on the receivers' axis, toward a light shining along
/// -Z, 10 m beyond the cascades' pancake: every cascade clamps it to its near
/// plane.
fn beyond_the_pancake() -> Vec3 {
    Vec3::new(0., 0., SHADOW_PANCAKE_SIZE + 10.)
}

/// A light shining along -Z onto the receiver, red or green, with or without
/// a shadow.
fn light(red: bool, shadow: bool) -> Option<DirectionalLight> {
    Some(DirectionalLight {
        direction: Vec3::NEG_Z,
        color: if red { [1., 0., 0.] } else { [0., 1., 0.] },
        illuminance: 1.,
        shadow: shadow.then_some(two_cascades()),
        ..Default::default()
    })
}

// Plausible defects: the shadow goes to a fixed slot rather than the first
// light that has one, to every such light, or to none; a light without one
// is shadowed; a light that is off (zero illuminance, or a direction that does
// not normalise) takes it. The oracle is geometric: an occluder the camera
// does not see covers the receiver from the light, so a shadowed light adds
// nothing there and an unshadowed one lights it. A red and a green light tell
// the slots apart.
#[test]
fn the_first_directional_light_that_casts_takes_the_shadow() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut place = |center, half, visible| {
        let mut asset = test_support::cube();
        asset.meshes = vec![quad(center, half)];
        asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
        asset.materials[0].double_sided = true;
        let model = scene.add_asset(&device, &queue, asset).unwrap().model;
        let state = InstanceState {
            model,
            pose: Mat4::IDENTITY,
            visible,
            capture_visible: true,
        };
        scene
            .add_instance(&device, &queue, state, Mobility::Static)
            .unwrap();
    };
    place(Vec3::new(0., 0., -3.), 1., true);
    // Between the light and the receiver, seen only by the light.
    place(Vec3::new(0., 0., -2.), 1.5, false);
    let camera = Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    };
    let mut input = FrameInput::new(camera);
    input.backdrop = Backdrop::Color([0.; 3]);
    input.baked_lighting = false;
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = crate::view::targets::target(
        &device,
        "directional shadow output",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    let center = ((SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) * 8) as usize;
    // The lit colour's red and green at the receiver's centre.
    let mut observe = |lights| -> [f32; 2] {
        input.directional_lights = lights;
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
        let color = test_support::read(&device, &queue, renderer.targets().color.texture(), 8);
        [0, 2].map(|channel| test_support::half(&color[center + channel..]))
    };
    let dark = observe([None; 2]);
    let lit = |now: f32, before: f32| now > before + 0.01;
    let same = |now: f32, before: f32| (now - before).abs() < 0.001;

    // An unshadowed light reaches the receiver.
    let both_open = observe([light(true, false), light(false, false)]);
    assert!(
        lit(both_open[0], dark[0]) && lit(both_open[1], dark[1]),
        "{both_open:?} against {dark:?}"
    );
    // The second light has a shadow: it alone is shadowed.
    let second = observe([light(true, false), light(false, true)]);
    assert!(
        same(second[0], both_open[0]) && same(second[1], dark[1]),
        "{second:?}: only the green light should be shadowed"
    );
    // Both have one: the first casts it, the second stays unshadowed.
    let first = observe([light(true, true), light(false, true)]);
    assert!(
        same(first[0], dark[0]) && same(first[1], both_open[1]),
        "{first:?}: only the red light should be shadowed"
    );
    // A light that is off casts nothing: the next one with a shadow casts it.
    for off in [
        DirectionalLight {
            illuminance: 0.,
            ..light(true, true).unwrap()
        },
        DirectionalLight {
            direction: Vec3::ZERO,
            ..light(true, true).unwrap()
        },
        DirectionalLight {
            direction: Vec3::NAN,
            ..light(true, true).unwrap()
        },
    ] {
        let after_off = observe([Some(off), light(false, true)]);
        assert!(
            same(after_off[0], dark[0]) && same(after_off[1], dark[1]),
            "{after_off:?}: {off:?} is no light, and the green light should be shadowed"
        );
    }
}

/// A quad facing +Y at `center`, `half` metres from its centre to each side.
fn floor(center: Vec3, half: f32) -> CpuMesh {
    let mut mesh = quad(Vec3::ZERO, half);
    for vertex in &mut mesh.vertices {
        let [x, y, _] = vertex.position;
        vertex.position = (center + Vec3::new(x, 0., -y)).to_array();
        vertex.normal = [0., 1., 0.];
    }
    mesh
}

/// A renderer and an empty scene of white single-sided quads, lit only by
/// the frame's directional lights.
struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    settings: Settings,
    size: [u32; 2],
}

impl Fixture {
    fn new((device, queue): (wgpu::Device, wgpu::Queue), size: [u32; 2]) -> Self {
        let settings = Settings {
            antialiasing: settings::Antialiasing::Off,
            bloom: settings::Bloom::Off,
            atmosphere: false,
            ..Settings::default()
        };
        let renderer = Renderer::for_test(&device, &queue, size, &settings);
        let scene = Scene::new(&device, &queue);
        Self {
            device,
            queue,
            scene,
            renderer,
            settings,
            size,
        }
    }

    /// Adds `mesh` as a static instance the camera sees when `visible`; the
    /// light always does.
    fn place(&mut self, mesh: CpuMesh, visible: bool) -> crate::InstanceId {
        self.place_asset(Self::white(mesh), visible)
    }

    /// `mesh` with a white single-sided material.
    fn white(mesh: CpuMesh) -> crate::asset::Asset {
        let mut asset = test_support::cube();
        asset.meshes = vec![mesh];
        asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
        asset.materials[0].metallic = 0.;
        asset.materials[0].double_sided = false;
        asset
    }

    /// Adds `asset` as a static instance the camera sees when `visible`; the
    /// light always does.
    fn place_asset(&mut self, asset: crate::asset::Asset, visible: bool) -> crate::InstanceId {
        let (device, queue) = (&self.device, &self.queue);
        let model = self.scene.add_asset(device, queue, asset).unwrap().model;
        let state = InstanceState {
            model,
            pose: Mat4::IDENTITY,
            visible,
            capture_visible: true,
        };
        self.scene
            .add_instance(device, queue, state, Mobility::Static)
            .unwrap()
    }

    /// Whether the light sees `instance`.
    fn cast(&mut self, instance: crate::InstanceId, casts: bool) {
        let mut state = *self.scene.instance(instance).unwrap();
        state.capture_visible = casts;
        self.scene
            .set_instance(&self.queue, instance, state)
            .unwrap();
    }

    /// The lit colour's red at each of `pixels` in a frame of `input`.
    fn observe(&mut self, input: &FrameInput, pixels: &[[u32; 2]]) -> Vec<f32> {
        let (device, queue) = (&self.device, &self.queue);
        let output = crate::view::targets::target(
            device,
            "shadow output",
            self.size,
            crate::shading::gbuffer::COLOR,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        self.renderer.render(
            device,
            queue,
            &mut encoder,
            &mut self.scene,
            input,
            &self.settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
        let color = test_support::read(device, queue, self.renderer.targets().color.texture(), 8);
        pixels
            .iter()
            .map(|&[x, y]| test_support::half(&color[((y * self.size[0] + x) * 8) as usize..]))
            .collect()
    }
}

/// A frame from the origin looking along -Z, lit only by `light`.
fn frame(light: DirectionalLight) -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.baked_lighting = false;
    input.directional_lights[0] = Some(light);
    input
}

// Plausible defects: a cascade's casters culled against its near plane, or
// clipped there by the rasterizer, so that a caster between the light and
// the part of the view the cascade covers casts nothing into it. Bevy draws
// directional casters with unclipped depth, emulated in the shader where
// the device lacks DEPTH_CLIP_CONTROL, and culls them without the near
// plane. The oracle is geometric: the light shines along the camera's view
// onto a receiver ahead, and an occluder behind the camera, beyond the
// cascades' pancake toward the light, covers it from the light.
#[test]
fn casters_between_the_light_and_a_cascade_cast_into_it() {
    for (label, without) in [
        ("unclipped depth", wgpu::Features::empty()),
        (
            "emulated unclipped depth",
            wgpu::Features::DEPTH_CLIP_CONTROL,
        ),
    ] {
        let Some(device) = test_support::device_without(without) else {
            return;
        };
        let native = device
            .0
            .features()
            .contains(wgpu::Features::DEPTH_CLIP_CONTROL);
        if native != without.is_empty() {
            eprintln!("skipping {label}: the adapter has no DEPTH_CLIP_CONTROL");
            continue;
        }
        let mut fixture = Fixture::new(device, SIZE);
        fixture.place(quad(Vec3::new(0., 0., -5.), 1.), true);
        let occluder = fixture.place(quad(beyond_the_pancake(), 1.5), false);
        let input = frame(DirectionalLight {
            direction: Vec3::NEG_Z,
            color: [1.; 3],
            illuminance: 1.,
            shadow: Some(two_cascades()),
            ..Default::default()
        });
        let center = [[SIZE[0] / 2, SIZE[1] / 2]];
        let shadowed = fixture.observe(&input, &center)[0];
        fixture.cast(occluder, false);
        let lit = fixture.observe(&input, &center)[0];
        assert!(
            lit > 0.05 && shadowed < 0.01 * lit,
            "{label}: the receiver is {shadowed} with the occluder and {lit} without"
        );
    }
}

// Plausible defects: a masked material's casters drawn whole (no discard),
// discarding other texels than the material cuts out (UV, vertex colour or
// cutoff taken wrongly), or the masked casters' unclipped depth missing. The
// oracle is geometric: the light shines along the camera's view, and an
// occluder the camera does not see, beyond the cascades' pancake toward the
// light, its base map cut out over its left half, covers the receiver. Behind its cut-out half the receiver is as lit as it
// is without the occluder; behind its opaque half it is dark.
#[test]
fn masked_casters_shadow_with_their_opaque_texels_only() {
    for (label, without) in [
        ("unclipped depth", wgpu::Features::empty()),
        (
            "emulated unclipped depth",
            wgpu::Features::DEPTH_CLIP_CONTROL,
        ),
    ] {
        let Some(device) = test_support::device_without(without) else {
            return;
        };
        let native = device
            .0
            .features()
            .contains(wgpu::Features::DEPTH_CLIP_CONTROL);
        if native != without.is_empty() {
            eprintln!("skipping {label}: the adapter has no DEPTH_CLIP_CONTROL");
            continue;
        }
        let mut fixture = Fixture::new(device, SIZE);
        fixture.place(quad(Vec3::new(0., 0., -5.), 2.), true);
        // The occluder's U runs along +X: its left half is cut out.
        let mut occluder = quad(beyond_the_pancake(), 1.5);
        for (vertex, uv) in
            occluder
                .vertices
                .iter_mut()
                .zip([[0., 1.], [1., 1.], [1., 0.], [0., 0.]])
        {
            vertex.uv = uv;
        }
        let occluder =
            fixture.place_asset(test_support::masked(Fixture::white(occluder), 0.5), false);
        let input = frame(DirectionalLight {
            direction: Vec3::NEG_Z,
            color: [1.; 3],
            illuminance: 1.,
            shadow: Some(two_cascades()),
            ..Default::default()
        });
        // The receiver at x = -0.6 and +0.6 m, behind U = 0.3 and 0.7.
        let pixels = [[12, SIZE[1] / 2], [19, SIZE[1] / 2]];
        let masked = fixture.observe(&input, &pixels);
        fixture.cast(occluder, false);
        let lit = fixture.observe(&input, &pixels);
        assert!(
            lit[0] > 0.05 && (masked[0] - lit[0]).abs() < 0.01 * lit[0],
            "{label}: behind the cut-out half the receiver is {} with the occluder and {} without",
            masked[0],
            lit[0]
        );
        assert!(
            lit[1] > 0.05 && masked[1] < 0.01 * lit[1],
            "{label}: behind the opaque half the receiver is {} with the occluder and {} without",
            masked[1],
            lit[1]
        );
    }
}

// Plausible defects: a cascade not drawn, drawn into or sampled from another
// layer, taken for the wrong view depths, or a shadow beyond its distance.
// The oracle is geometric: receivers facing the camera along the light at
// depths in each cascade, each covered from the light by a smaller occluder
// the camera does not see, are dark where covered and lit without the
// occluders, except one beyond the shadow distance, which stays lit.
#[test]
fn every_cascade_shadows_its_part_of_the_view() {
    let Some(device) = test_support::device() else {
        return;
    };
    const SIZE: [u32; 2] = [64, 64];
    let mut fixture = Fixture::new(device, SIZE);
    let shadow = DirectionalShadow {
        distance: 200.,
        cascades: 4,
    };
    let input = frame(DirectionalLight {
        direction: Vec3::NEG_Z,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(shadow),
        ..Default::default()
    });
    // Depths in cascades 0 to 3 (bounds 20, 40, 100 and 200 m) and beyond the
    // distance, each at its own place across the view.
    let receivers = [
        (3., -0.35),
        (25., -0.15),
        (70., 0.05),
        (160., 0.25),
        (230., 0.42),
    ];
    let tan = 0.5f32.tan();
    let mut pixels = Vec::new();
    let mut occluders = Vec::new();
    for (depth, across) in receivers {
        fixture.place(
            quad(Vec3::new(across * depth, 0., -depth), 0.06 * depth),
            true,
        );
        let occluder = quad(Vec3::new(across * depth, 0., -0.85 * depth), 0.03 * depth);
        occluders.push(fixture.place(occluder, false));
        let x = (across / tan * 0.5 + 0.5) * SIZE[0] as f32;
        pixels.push([x as u32, SIZE[1] / 2]);
    }
    let shadowed = fixture.observe(&input, &pixels);
    for occluder in occluders {
        fixture.cast(occluder, false);
    }
    let lit = fixture.observe(&input, &pixels);
    for (index, (depth, _)) in receivers.iter().enumerate() {
        let (shadowed, lit) = (shadowed[index], lit[index]);
        assert!(lit > 0.05, "the receiver {depth} m away is unlit: {lit}");
        if *depth < shadow.distance {
            assert!(
                shadowed < 0.01 * lit,
                "the receiver {depth} m away is {shadowed} with its occluder and {lit} without"
            );
        } else {
            assert!(
                (shadowed - lit).abs() < 0.001 * lit,
                "the receiver beyond the shadow distance is shadowed: {shadowed} against {lit}"
            );
        }
    }
}

// Ray hits have no camera depth, so they take the first cascade whose map
// holds them. Plausible defects: a hit takes the cascade of its camera view
// depth, which may not hold it (a hit behind the camera has negative depth
// and the nearest cascade lies ahead), and is left unshadowed. The oracle is
// geometric: an occluder above a point 10 m behind the camera, beyond the
// map of cascade 0 (which reaches about 6 m behind), covers it from a light
// shining down, observed through the shading function ray hits call.
#[test]
fn ray_hits_take_the_cascade_that_holds_them() {
    let Some(device) = test_support::device() else {
        return;
    };
    let mut fixture = Fixture::new(device, SIZE);
    let occluder = fixture.place(floor(Vec3::new(0., 3., 10.), 1.), false);
    let input = frame(DirectionalLight {
        direction: Vec3::NEG_Y,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(DirectionalShadow {
            distance: 200.,
            cascades: 4,
        }),
        ..Default::default()
    });
    let observe = |fixture: &mut Fixture| -> [f32; 2] {
        let (device, queue) = (&fixture.device, &fixture.queue);
        let prepared = fixture.renderer.prepare_test_frame(
            device,
            queue,
            &mut fixture.scene,
            &input,
            &fixture.settings,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        fixture.renderer.encode_test_shadows(
            device,
            queue,
            &mut encoder,
            &fixture.scene,
            &prepared,
            None,
        );
        queue.submit([encoder.finish()]);
        let observed = observe_shadow(
            device,
            queue,
            &fixture.renderer,
            &fixture.scene,
            // The point behind the camera as a ray hit and, for contrast, as
            // the camera's surface.
            "output[0]=vec4(directional_shadow_visibility(0u,vec3(0.,0.,10.),vec3(0.,1.,0.),vec2(0.),SHADOW_RECEIVER_CAPTURE),\
             directional_shadow_visibility(0u,vec3(0.,0.,10.),vec3(0.,1.,0.),vec2(0.),SHADOW_RECEIVER_CAMERA),0.,0.);",
        );
        fixture.scene.finish_frame();
        [observed[0], observed[1]]
    };
    let [hit, camera] = observe(&mut fixture);
    fixture.cast(occluder, false);
    let [open, _] = observe(&mut fixture);
    assert!(
        hit < 0.01 && open > 0.99,
        "a hit behind the camera is {hit} under the occluder and {open} without"
    );
    // The camera's selection by view depth does not hold this point.
    assert!(camera > 0.99, "{camera}");
}

// A cascade's map records a caster up to the shadow's pancake toward the
// light beyond its slice at the caster's own depth (Godot's pancake), so
// the metres between a point and its occluder can be read from the map.
// Plausible defects: the near plane left on the slice, so such a caster is
// clamped to it and the occluder reads as the near plane; the margin moved
// away from the light; the depth scale not following the wider range. The
// oracle is geometric: under a light shining straight down, a floor the
// camera does not see, above the top of the nearest cascades' slices
// (cascade 0 tops out about 11 m up), lies exactly as far above each
// point as its height difference.
#[test]
fn casters_within_the_pancake_keep_their_own_depth() {
    let Some(device) = test_support::device() else {
        return;
    };
    let mut fixture = Fixture::new(device, SIZE);
    const HEIGHT: f32 = 12.;
    fixture.place(floor(Vec3::new(0., HEIGHT, -110.), 120.), false);
    let input = frame(DirectionalLight {
        direction: Vec3::NEG_Y,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(DirectionalShadow {
            distance: 200.,
            cascades: 4,
        }),
        ..Default::default()
    });
    // (view depth, height): points in cascades 0, 0, 1 and 2, in view.
    let points = [(0.3, 0.), (3., 1.), (30., 5.), (60., 0.)];
    let (device, queue) = (&fixture.device, &fixture.queue);
    let prepared = fixture.renderer.prepare_test_frame(
        device,
        queue,
        &mut fixture.scene,
        &input,
        &fixture.settings,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    fixture.renderer.encode_test_shadows(
        device,
        queue,
        &mut encoder,
        &fixture.scene,
        &prepared,
        None,
    );
    queue.submit([encoder.finish()]);
    let positions = points
        .iter()
        .map(|(depth, height)| format!("vec3(0.,{height:?},{:?})", -depth))
        .collect::<Vec<_>>()
        .join(",");
    // The metres from each point to the occluder its cascade's map records
    // over it: the depth difference over the projection's depth per metre.
    let statement = format!(
        "var points=array<vec3<f32>,4>({positions});\n\
         var metres=vec4(0.);\n\
         for(var i=0;i<4;i++) {{\n\
          let point=points[i];\n\
          let cascade=get_cascade_index((view.view*vec4(point,1.)).z);\n\
          let local=world_to_directional_light_local(cascade,vec4(point,1.));\n\
          let texel=vec2<i32>(local.xy*vec2<f32>(textureDimensions(directional_shadow_map)));\n\
          let occluder=textureLoad(directional_shadow_map,texel,i32(cascade),0);\n\
          let clip=frame.shadow_cascades[cascade].clip_from_world;\n\
          metres[i]=(occluder-local.z)/length(vec3(clip[0].z,clip[1].z,clip[2].z));\n\
         }}\n\
         output[0]=metres;"
    );
    let metres = observe_shadow(device, queue, &fixture.renderer, &fixture.scene, &statement);
    fixture.scene.finish_frame();
    for ((depth, height), metres) in points.into_iter().zip(metres) {
        let expected = HEIGHT - height;
        assert!(
            (metres - expected).abs() < 0.01,
            "the floor reads {metres} m above the point {depth} m away, not {expected}"
        );
    }
}

/// The share of the shadowed directional light that reaches each of
/// `points` in the fog (`SHADOW_RECEIVER_MEDIUM`), in a frame of `input`.
fn medium_light(fixture: &mut Fixture, input: &FrameInput, points: &[Vec3]) -> Vec<f32> {
    let (device, queue) = (&fixture.device, &fixture.queue);
    let prepared = fixture.renderer.prepare_test_frame(
        device,
        queue,
        &mut fixture.scene,
        input,
        &fixture.settings,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    fixture.renderer.encode_test_shadows(
        device,
        queue,
        &mut encoder,
        &fixture.scene,
        &prepared,
        None,
    );
    queue.submit([encoder.finish()]);
    let observed = points
        .chunks(4)
        .flat_map(|chunk| {
            let mut calls = chunk
                .iter()
                .map(|point| {
                    format!(
                        "directional_shadow_visibility(0u,vec3({:?},{:?},{:?}),vec3(0.),vec2(0.),SHADOW_RECEIVER_MEDIUM)",
                        point.x, point.y, point.z
                    )
                })
                .collect::<Vec<_>>();
            calls.resize(4, "0.".into());
            observe_shadow(
                device,
                queue,
                &fixture.renderer,
                &fixture.scene,
                &format!("output[0]=vec4({});", calls.join(",")),
            )
            .into_iter()
            .take(chunk.len())
        })
        .collect();
    fixture.scene.finish_frame();
    observed
}

// The fog takes Godot's fog tap: the light fades by exp(-INV_FOG_FADE * the
// metres a point lies behind its occluder), INV_FOG_FADE being 10.
// Plausible defects: the depth difference taken the wrong way round, scaled
// by the cascade's depth per metre instead of its metres per unit of depth,
// or offset toward the light as a surface's is; a cascade or layer that does
// not hold the point; a shadow beyond the shadow distance. The oracle is
// that formula over the geometry: a light shining straight down onto a
// floor the camera does not see, and points in each cascade's part of the
// view a known distance below or above it.
#[test]
fn the_fog_fades_the_light_by_the_metres_behind_its_occluder() {
    let Some(device) = test_support::device() else {
        return;
    };
    let mut fixture = Fixture::new(device, SIZE);
    const HEIGHT: f32 = 1.;
    fixture.place(floor(Vec3::new(0., HEIGHT, -110.), 120.), false);
    let shadow = DirectionalShadow {
        distance: 200.,
        cascades: 4,
    };
    let input = frame(DirectionalLight {
        direction: Vec3::NEG_Y,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(shadow),
        ..Default::default()
    });
    // (view depth, metres below the floor): points in cascades 0 to 3
    // (bounds 20, 40, 100 and 200 m), one above the floor and one beyond the
    // shadow distance.
    let points = [
        (3., 0.02),
        (30., 0.05),
        (70., 0.1),
        (150., 0.2),
        (30., -0.05),
        (220., 0.2),
    ];
    let positions: Vec<Vec3> = points
        .iter()
        .map(|&(depth, below)| Vec3::new(0., HEIGHT - below, -depth))
        .collect();
    let observed = medium_light(&mut fixture, &input, &positions);
    for ((depth, below), observed) in points.into_iter().zip(observed) {
        let expected = if depth < shadow.distance && below > 0. {
            (-10. * below).exp()
        } else {
            1.
        };
        assert!(
            (observed - expected).abs() < 2e-3,
            "a point {depth} m away and {below} m below the floor receives {observed} of the light, not {expected}"
        );
    }
}

// The fog's fade is measured from a caster within the cascade's pancake,
// not from the cascade's near plane (#67). Plausible defects: the cascade
// fitted tightly to its slice, so a caster toward the light from it is
// recorded at the near plane, the fog just behind that plane receives most
// of the light however far the caster is, and a point between the plane and
// the caster is unshadowed. The oracle is Godot's fade over the geometry:
// light shining straight down onto a floor 5 m above the top of cascade 0's
// slice (about 8.2 m up at the default shadow's 15.09 m first far bound)
// reaches points in cascade 0 as exp(-10 x their metres below the floor),
// essentially none at the top of the view; and a floor 20 m up, with the
// sun behind and above the camera, leaves none in the fog just in front of
// the camera.
#[test]
fn the_fog_fades_from_casters_within_the_pancake() {
    let Some(device) = test_support::device() else {
        return;
    };
    let shadow = DirectionalShadow::default();
    let light = |direction| DirectionalLight {
        direction,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(shadow),
        ..Default::default()
    };
    let tan = 0.5f32.tan();
    // Godot's first split of the default shadow seen from a 0.1 m near
    // plane: 0.1 m + 0.1 x 149.9 m.
    let top = 15.09 * tan;

    let mut fixture = Fixture::new(device, SIZE);
    let height = top + 5.;
    fixture.place(floor(Vec3::new(0., height, -110.), 120.), false);
    // (view depth, metres below the floor): two points of cascade 0 above
    // the view, just below the floor, and one at the top of the view near
    // the cascade's far bound.
    let points = [(3., 0.05), (3., 0.1), (15., height - 15. * tan + 0.01)];
    let positions: Vec<Vec3> = points
        .iter()
        .map(|&(depth, below)| Vec3::new(0., height - below, -depth))
        .collect();
    let observed = medium_light(&mut fixture, &frame(light(Vec3::NEG_Y)), &positions);
    for ((depth, below), observed) in points.into_iter().zip(observed) {
        let expected = (-10. * below).exp();
        assert!(
            (observed - expected).abs() < 2e-3,
            "a point {depth} m away and {below} m below a floor 5 m above cascade 0 receives {observed} of the light, not {expected}"
        );
    }

    let Some(device) = test_support::device() else {
        return;
    };
    let mut fixture = Fixture::new(device, SIZE);
    fixture.place(floor(Vec3::new(0., 20., 0.), 60.), false);
    let depths = [0.15, 0.2, 0.3, 0.5];
    let positions: Vec<Vec3> = depths
        .iter()
        .map(|&depth| Vec3::new(0., 0., -depth))
        .collect();
    let observed = medium_light(
        &mut fixture,
        &frame(light(Vec3::new(0., -1., -1.))),
        &positions,
    );
    for (depth, observed) in depths.into_iter().zip(observed) {
        assert!(
            observed < 1e-3,
            "the fog {depth} m in front of the camera, under a floor 20 m up, receives {observed} of the light"
        );
    }
}

/// Runs `statement` in a compute shader over the shading library with the
/// camera's lit group 0, and returns the `vec4` it writes to `output[0]`.
fn observe_shadow(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    scene: &Scene,
    statement: &str,
) -> [f32; 4] {
    let source = format!(
        "{}\n@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;\n\
         @compute @workgroup_size(1) fn observe() {{\n {statement}\n}}\n",
        crate::shading::lit_compute_library()
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shadow observation"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("shadow observation"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("shadow observation"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow observation"),
                bind_group_layouts: &[
                    Some(renderer.test_lit_layout()),
                    Some(scene.scene_layout()),
                    None,
                    Some(&layout),
                ],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let output = crate::scene::buffer(
        device,
        "shadow observation",
        &[0; 16],
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shadow observation readback"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow observation"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, renderer.test_camera_lit(), &[]);
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 16);
    queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        send.send(result).unwrap()
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receive.recv().unwrap().unwrap();
    bytemuck::cast_slice::<u8, f32>(&readback.get_mapped_range(..))
        .try_into()
        .unwrap()
}
