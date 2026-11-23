use crate::baked_specular_probe::{LEVELS, SpecularProbeBox, SpecularProbeTexels};
use crate::settings::Settings;
use crate::test_support::half;
use crate::{
    Camera, FrameInput, InstanceId, InstanceState, LiveSpecularProbe, LiveSpecularProbeId,
    Mobility, ProbeRefresh, ProbeRefreshPace, Scene, TestRenderer,
};
use glam::{Mat4, Quat, Vec3};

const FACE_SIZE: u32 = 64;
const RADIANCE: f32 = 4.;
/// Where the probe captures from.
const CENTER: Vec3 = Vec3::new(3., -2., 5.);

/// A scene with one live probe at `CENTER` and a square emitter of
/// `RADIANCE`, a moving instance, two metres from it. Seen from the centre
/// the emitter covers the middle quarter of a face's width, edge to edge on
/// texel boundaries, and nothing else emits or lights.
struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: TestRenderer,
    settings: Settings,
    input: FrameInput,
    emitter: InstanceId,
    probe: LiveSpecularProbeId,
}

/// A square of side 1 in its model's XY plane, both sides emitting
/// `emission`, in `group`.
fn square(emission: f32, group: u32) -> crate::asset::Asset {
    use crate::asset::{CpuMesh, Vertex};
    let mut asset = crate::test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]
            .map(|[x, y]| Vertex {
                tangent: [0.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                position: [x, y, 0.],
                normal: [0., 0., 1.],
                uv: [0.5; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }];
    let material = &mut asset.materials[0];
    material.base = [0., 0., 0., 1.];
    material.metallic = 0.;
    material.roughness = 1.;
    material.double_sided = true;
    material.emissive = [emission; 3];
    material.visibility_group = group;
    asset
}

/// The pose of a square two metres from `CENTER` toward `direction`,
/// facing it, shifted by `offset`.
fn toward(direction: Vec3, offset: Vec3) -> Mat4 {
    Mat4::from_translation(CENTER + 2. * direction + offset)
        * Mat4::from_quat(Quat::from_rotation_arc(Vec3::Z, direction))
}

fn probe() -> LiveSpecularProbe {
    LiveSpecularProbe {
        center: CENTER,
        world_to_local: Mat4::from_translation(-CENTER),
        influence: SpecularProbeBox {
            min: Vec3::splat(-1.),
            max: Vec3::splat(1.),
        },
        blend: Vec3::ZERO,
        proxy: None,
        face_size: FACE_SIZE,
        hidden_groups: 0,
    }
}

impl Fixture {
    fn new() -> Option<Self> {
        let (device, queue) = crate::test_support::device()?;
        Some(Self::on(device, queue))
    }

    fn on(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let mut scene = Scene::new(&device, &queue);
        let ids = scene
            .add_asset(&device, &queue, square(RADIANCE, 0))
            .unwrap();
        let emitter = scene
            .add_instance(
                &device,
                &queue,
                InstanceState {
                    pose: toward(Vec3::X, Vec3::ZERO),
                    ..InstanceState::new(ids.model)
                },
                Mobility::Moving,
            )
            .unwrap();
        let probe = scene
            .add_live_specular_probe(&device, &queue, probe())
            .unwrap();
        let settings = Settings::default();
        let renderer = TestRenderer::for_test(&device, &queue, [64, 64], &settings);
        let input = FrameInput::new(Camera {
            view: Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: Vec3::ZERO,
        });
        Self {
            device,
            queue,
            scene,
            renderer,
            settings,
            input,
            emitter,
            probe,
        }
    }

    fn place(&mut self, pose: Mat4) {
        let state = *self.scene.instance(self.emitter).unwrap();
        self.scene
            .set_instance(&self.queue, self.emitter, InstanceState { pose, ..state })
            .unwrap();
    }

    fn refresh(&mut self, probe: LiveSpecularProbeId, pace: ProbeRefreshPace) -> ProbeRefresh {
        self.renderer
            .renderer
            .refresh_specular_probe(
                &self.device,
                &self.queue,
                &mut self.scene,
                &mut self.renderer.view,
                &self.input,
                &self.settings,
                None,
                probe,
                pace,
            )
            .unwrap()
    }

    /// Probe `probe`'s cube as the scene's array holds it, read back for
    /// the test: each level's six faces, level-major, as a bake returns
    /// them (`SpecularProbeRadiance`).
    fn cube(&self, probe: LiveSpecularProbeId) -> Vec<u16> {
        let cube = self.scene.live_probe_cube(probe).unwrap();
        let mut texels = Vec::new();
        for level in 0..LEVELS {
            let size = FACE_SIZE >> level;
            let row = (size * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("live probe evidence"),
                size: u64::from(row * size * 6),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = self.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &cube.texture,
                    mip_level: level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: cube.cube * 6,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 6,
                },
            );
            self.queue.submit([encoder.finish()]);
            buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            let mapped = buffer.get_mapped_range(..).unwrap();
            for line in mapped.chunks(row as usize) {
                texels.extend(
                    line[..size as usize * 8]
                        .chunks_exact(2)
                        .map(|value| u16::from_le_bytes([value[0], value[1]])),
                );
            }
        }
        texels
    }
}

/// What `expression`, WGSL over `world` and `direction` under a ray hit's
/// lit group 0, gives for each of `cases`, a receiver's position and
/// direction.
fn sampled(fixture: &mut Fixture, expression: &str, cases: &[(Vec3, Vec3)]) -> Vec<[f32; 4]> {
    let vec3 = |v: Vec3| format!("vec3({:?},{:?},{:?})", v.x, v.y, v.z);
    let list = |of: fn(&(Vec3, Vec3)) -> Vec3| {
        cases
            .iter()
            .map(|case| vec3(of(case)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let count = cases.len();
    let observation = format!(
        r#"@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let worlds=array<vec3<f32>,{count}>({});
 let directions=array<vec3<f32>,{count}>({});
 for (var i=0u;i<{count}u;i++) {{
  let world=worlds[i];
  let direction=directions[i];
  output[i]={expression};
 }}
}}
"#,
        list(|case| case.0),
        list(|case| case.1),
    );
    crate::test_support::observe_ray_hits(
        &fixture.device,
        &fixture.queue,
        &mut fixture.renderer,
        &mut fixture.scene,
        &fixture.input,
        &fixture.settings,
        &observation,
        count,
    )
}

/// The probes' and the sky's shares of a receiver's environment specular,
/// through the walk a ray hit and a capture take (`collection_environment`:
/// the grid's baked probes, then the live ones).
const ENVIRONMENT: &str = "vec4(collection_environment(world,direction,0.,1.,environment_map,\
    environment_sampler,0.,1.).probes.r,collection_environment(world,direction,0.,1.,\
    environment_map,environment_sampler,0.,1.).sky.r,0.,0.)";

/// The red channel of the texel nearest the centre of each face of level
/// `level` of a cube in `SpecularProbeRadiance` order.
fn face_centres(texels: &[u16], level: u32) -> [f32; 6] {
    let start: usize = (0..level)
        .map(|above| ((FACE_SIZE >> above) as usize).pow(2) * 6 * 4)
        .sum();
    let size = (FACE_SIZE >> level) as usize;
    std::array::from_fn(|face| {
        let texel = face * size * size + size / 2 * size + size / 2;
        half(&texels[start + texel * 4].to_le_bytes())
    })
}

/// The face of a stored cube a world direction's axis is on: WebGPU's cube
/// order, the cube mirrored in world Z (`SpecularProbeRadiance`).
fn stored_face(direction: Vec3) -> usize {
    [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::NEG_Z,
        Vec3::Z,
    ]
    .iter()
    .position(|axis| *axis == direction)
    .unwrap()
}

/// Asserts that `texels` holds the emitter toward `direction` alone: its
/// radiance at the middle of that face at the sharp level, nothing on the
/// other faces there, and at every rougher level the most toward it and
/// next to nothing on the opposite face, which the lobe about that face's
/// normal cannot reach.
fn assert_emitter_toward(texels: &[u16], direction: Vec3) {
    let lit = stored_face(direction);
    let opposite = stored_face(-direction);
    for level in 0..LEVELS {
        let centres = face_centres(texels, level);
        if level == 0 {
            for (face, value) in centres.iter().enumerate() {
                let expected = if face == lit { RADIANCE } else { 0. };
                assert!(
                    (value - expected).abs() <= RADIANCE / 512.,
                    "emitter toward {direction}: sharp face {face} holds {value}, \
                     expected {expected}"
                );
            }
        }
        assert!(
            centres[lit] > 0.,
            "emitter toward {direction}: level {level} holds none of it: {centres:?}"
        );
        for (face, value) in centres.iter().enumerate() {
            assert!(
                face == lit || *value < centres[lit],
                "emitter toward {direction}: level {level} is brightest on face {face}, \
                 not {lit}: {centres:?}"
            );
        }
        assert!(
            centres[opposite] <= centres[lit] / 100.,
            "emitter toward {direction}: level {level} shows it behind: {centres:?}"
        );
    }
}

// The acceptance fixture: an emissive moving object is moved between known
// cube directions and each next published cube holds it there, at every
// roughness level, and no longer where it was. Defects: a refresh that draws
// no moving instance or draws it at an old pose, a face rendered or resolved
// into another's layer, a publication that copies another cube, level or
// layer, or none. Oracle: the fixture's geometry and the stored cube's
// documented convention.
#[test]
fn a_refresh_publishes_a_moved_emitter_on_the_face_it_moved_to() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    for direction in [Vec3::X, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_X, Vec3::Y] {
        fixture.place(toward(direction, Vec3::ZERO));
        assert_eq!(
            fixture.refresh(fixture.probe, ProbeRefreshPace::Whole),
            ProbeRefresh::Published
        );
        assert_emitter_toward(&fixture.cube(fixture.probe), direction);
    }
}

// A refresh of a static scene and a bake of it hold the same cube: one
// capture and one prefilter. The emitter is off the face's centre by other
// amounts on each axis, on texel boundaries, so both captures rasterise the
// same texels whatever they render the face at, and a face mirrored or
// turned in either path disagrees. Oracle: the bake path, whose stored
// energy and orientation its own tests fix.
#[test]
fn a_refresh_holds_the_cube_a_bake_of_the_same_scene_returns() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    // Four texels along one edge and two along the other, at two metres.
    for (direction, offset) in [
        (Vec3::X, Vec3::new(0., 0.25, 0.125)),
        (Vec3::NEG_Y, Vec3::new(0.25, 0., 0.125)),
        (Vec3::Z, Vec3::new(0.25, 0.125, 0.)),
    ] {
        fixture.place(toward(direction, offset));
        // A bake shows static instances: the same square, added as one.
        let ids = fixture
            .scene
            .add_asset(&fixture.device, &fixture.queue, square(RADIANCE, 0))
            .unwrap();
        let fixed = fixture
            .scene
            .add_instance(
                &fixture.device,
                &fixture.queue,
                InstanceState {
                    pose: toward(direction, offset),
                    ..InstanceState::new(ids.model)
                },
                Mobility::Static,
            )
            .unwrap();
        // The moving emitter is in the refresh alone; hide it from both.
        let state = *fixture.scene.instance(fixture.emitter).unwrap();
        fixture
            .scene
            .set_instance(
                &fixture.queue,
                fixture.emitter,
                InstanceState {
                    capture_visible: false,
                    ..state
                },
            )
            .unwrap();
        let baked = fixture
            .renderer
            .capture_specular_probe(
                &fixture.device,
                &fixture.queue,
                &mut fixture.scene,
                &fixture.input,
                &fixture.settings,
                CENTER,
                FACE_SIZE,
            )
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(baked) = baked.texels else {
            unreachable!("captures return RGBA16F")
        };
        assert_eq!(
            fixture.refresh(fixture.probe, ProbeRefreshPace::Whole),
            ProbeRefresh::Published
        );
        let live = fixture.cube(fixture.probe);
        assert_eq!(live.len(), baked.len());
        assert_emitter_toward(&baked, direction);
        for (index, (live, baked)) in live.iter().zip(&baked).enumerate() {
            let (live, baked) = (half(&live.to_le_bytes()), half(&baked.to_le_bytes()));
            assert!(
                (live - baked).abs() <= 0.002 * RADIANCE,
                "emitter toward {direction}: component {index} is {live} live, {baked} baked"
            );
        }
        fixture.scene.remove_instance(fixed).unwrap();
    }
}

/// The energy a cube holds at its sharp level: each texel's radiance times
/// its solid angle, summed over the six faces.
fn energy(texels: &[u16]) -> f64 {
    let size = FACE_SIZE as usize;
    let side = 2. / size as f64;
    let mut sum = 0.;
    for (index, texel) in texels[..6 * size * size * 4].chunks_exact(4).enumerate() {
        let x = -1. + ((index % size) as f64 + 0.5) * side;
        let y = -1. + ((index / size % size) as f64 + 0.5) * side;
        // A texel's solid angle at (x, y) on the unit cube's face.
        let solid_angle = side * side / (1. + x * x + y * y).powf(1.5);
        sum += f64::from(half(&texel[0].to_le_bytes())) * solid_angle;
    }
    sum
}

// A refresh's faces are culled, each to its own view: an emitter across
// the edge between two faces, and one across a corner of three, is in
// every face that sees it, with the energy a bake's unculled faces store.
// Defect: a face's list culled by a volume that stops short of the face's
// edges or corners (a list culled by another face's view fails
// `a_refresh_publishes_a_moved_emitter_on_the_face_it_moved_to`). Oracle:
// the bake of the same scene.
#[test]
fn a_refresh_culls_each_face_to_its_own_view() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let state = *fixture.scene.instance(fixture.emitter).unwrap();
    fixture
        .scene
        .set_instance(
            &fixture.queue,
            fixture.emitter,
            InstanceState {
                capture_visible: false,
                ..state
            },
        )
        .unwrap();
    for (offset, faces) in [
        // Across the +X and +Y faces' edge, and their corner with +Z.
        (Vec3::new(0., 2., 0.), &[Vec3::X, Vec3::Y][..]),
        (Vec3::new(0., 2., 2.), &[Vec3::X, Vec3::Y, Vec3::Z][..]),
    ] {
        let ids = fixture
            .scene
            .add_asset(&fixture.device, &fixture.queue, square(RADIANCE, 0))
            .unwrap();
        // A square of side 2 turned to face the centre.
        let outward = (Vec3::X * 2. + offset).normalize();
        let pose = Mat4::from_translation(CENTER + Vec3::X * 2. + offset)
            * Mat4::from_quat(Quat::from_rotation_arc(Vec3::Z, outward))
            * Mat4::from_scale(Vec3::splat(2.));
        let fixed = fixture
            .scene
            .add_instance(
                &fixture.device,
                &fixture.queue,
                InstanceState {
                    pose,
                    ..InstanceState::new(ids.model)
                },
                Mobility::Static,
            )
            .unwrap();
        let baked = fixture
            .renderer
            .capture_specular_probe(
                &fixture.device,
                &fixture.queue,
                &mut fixture.scene,
                &fixture.input,
                &fixture.settings,
                CENTER,
                FACE_SIZE,
            )
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(baked) = baked.texels else {
            unreachable!("captures return RGBA16F")
        };
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
        let live = fixture.cube(fixture.probe);
        let (stored, expected) = (energy(&live), energy(&baked));
        assert!(
            expected > 0.5 && (stored - expected).abs() <= 0.02 * expected,
            "an emitter across {faces:?} stores energy {stored} live, {expected} baked"
        );
        // Each face it crosses holds some of it.
        let size = FACE_SIZE as usize;
        for face in faces {
            let texels = &live[stored_face(*face) * size * size * 4..][..size * size * 4];
            let lit = texels
                .chunks_exact(4)
                .filter(|texel| half(&texel[0].to_le_bytes()) > RADIANCE / 2.)
                .count();
            assert!(lit > 16, "the {face} face holds {lit} texels of it");
        }
        fixture.scene.remove_instance(fixed).unwrap();
    }
}

// The reflecting object stays out of its own probe and still shadows what
// the probe shows: a shell around the centre, in a group the probe hides,
// is on every face while the probe hides nothing; hidden, it is on none,
// the emitter beyond it shows, and the floor below is in the sun shadow it
// casts, dark beside the floor with the shell out of every view. Oracle:
// the rule (`LiveSpecularProbe::hidden_groups`) and the geometry.
#[test]
fn a_refresh_leaves_out_the_groups_its_probe_hides_and_keeps_their_shadow() {
    use crate::{DirectionalLight, DirectionalShadow};
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let queue = fixture.queue.clone();
    const SHELL: f32 = 0.5;
    // A unit cube about the centre, emitting from its inside faces too.
    let mut shell = crate::test_support::cube();
    let material = &mut shell.materials[0];
    material.base = [0., 0., 0., 1.];
    material.metallic = 0.;
    material.roughness = 1.;
    material.emissive = [SHELL; 3];
    material.visibility_group = 1;
    let ids = fixture
        .scene
        .add_asset(&fixture.device, &fixture.queue, shell)
        .unwrap();
    let state = InstanceState {
        pose: Mat4::from_translation(CENTER),
        ..InstanceState::new(ids.model)
    };
    let shell = fixture
        .scene
        .add_instance(&fixture.device, &fixture.queue, state, Mobility::Moving)
        .unwrap();
    slab(
        &mut fixture,
        CENTER - Vec3::Y * 1.5,
        Vec3::new(12., 0.1, 12.),
        Mobility::Static,
    );
    fixture.place(toward(Vec3::X, Vec3::ZERO));
    // Frames and refreshes show the shell's group; the sun shines down.
    fixture.input.visibility_mask = 1;
    fixture.input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::NEG_Y,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(DirectionalShadow {
            distance: 20.,
            cascades: 3,
        }),
        ..Default::default()
    });
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    for (face, value) in face_centres(&fixture.cube(fixture.probe), 0)
        .iter()
        .enumerate()
    {
        assert!(
            (value - SHELL).abs() <= SHELL / 512.,
            "the shell is missing from face {face}: {value}"
        );
    }
    // Hidden, the shell is gone from the faces and its shadow stays.
    fixture
        .scene
        .set_live_specular_probe(
            &queue,
            fixture.probe,
            LiveSpecularProbe {
                hidden_groups: 1,
                ..probe()
            },
        )
        .unwrap();
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let hidden = face_centres(&fixture.cube(fixture.probe), 0);
    // The emitter beyond where the shell was, and the black sky above.
    assert!(
        (hidden[stored_face(Vec3::X)] - RADIANCE).abs() <= RADIANCE / 512.
            && hidden[stored_face(Vec3::Y)] == 0.
            && hidden[stored_face(Vec3::NEG_X)] == 0.,
        "the hidden shell is on a face: {hidden:?}"
    );
    let shadowed = hidden[stored_face(Vec3::NEG_Y)];
    // The shell in no view at all casts nothing.
    fixture
        .scene
        .set_instance(
            &queue,
            shell,
            InstanceState {
                capture_visible: false,
                ..state
            },
        )
        .unwrap();
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let lit = face_centres(&fixture.cube(fixture.probe), 0)[stored_face(Vec3::NEG_Y)];
    assert!(
        lit > 0.05 && shadowed < 0.01 * lit,
        "the floor below the probe is {shadowed} in the hidden shell's shadow and {lit} \
         without the shell"
    );
}

// No consumer samples a part-updated cube: while a refresh is spread over
// calls the scene's cube holds the last publication, bit for bit, at every
// level and face, and the call that reports the publication replaces all of
// it. The emitter moves before the spread refresh starts, so a face or
// level written early would differ. A second probe's call meanwhile waits
// and changes nothing. Oracle: the bytes of the cube consumers sample.
#[test]
fn a_refresh_in_flight_leaves_the_published_cube_whole() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let other = fixture
        .scene
        .add_live_specular_probe(&fixture.device, &fixture.queue, probe())
        .unwrap();
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let published = fixture.cube(fixture.probe);
    assert_emitter_toward(&published, Vec3::X);
    for pace in [
        ProbeRefreshPace::FacePerCall,
        ProbeRefreshPace::FacesThenFilter,
    ] {
        let before = fixture.cube(fixture.probe);
        let to = if pace == ProbeRefreshPace::FacePerCall {
            Vec3::NEG_Z
        } else {
            Vec3::Y
        };
        fixture.place(toward(to, Vec3::ZERO));
        let calls = if pace == ProbeRefreshPace::FacePerCall {
            6
        } else {
            1
        };
        for call in 0..calls {
            assert_eq!(fixture.refresh(fixture.probe, pace), ProbeRefresh::InFlight);
            assert_eq!(
                fixture.refresh(other, ProbeRefreshPace::Whole),
                ProbeRefresh::Waiting
            );
            assert!(
                fixture.cube(fixture.probe) == before,
                "{pace:?}: call {call} changed the published cube"
            );
        }
        assert_eq!(
            fixture.refresh(fixture.probe, pace),
            ProbeRefresh::Published
        );
        assert_emitter_toward(&fixture.cube(fixture.probe), to);
    }
    // The other probe's cube is its own: unwritten until its refresh.
    assert!(fixture.cube(other).iter().all(|value| *value == 0));
    assert_eq!(
        fixture.refresh(other, ProbeRefreshPace::Whole),
        ProbeRefresh::Published
    );
    assert_emitter_toward(&fixture.cube(other), Vec3::Y);
    assert_emitter_toward(&fixture.cube(fixture.probe), Vec3::Y);
}

// What a receiver samples before and after a probe's first publication,
// through the walk a ray hit and a capture take (`collection_environment`:
// the grid's baked probes, then the live ones): before, the sky alone, as
// if the probe were not there; after, the probe alone, the emitter toward
// where it is. A receiver outside its influence keeps the sky. Oracle: the
// uniform sky's radiance 1, the emitter's 4, and the influence box.
#[test]
fn a_live_probe_lights_nothing_until_its_first_publication() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let environment = fixture
        .scene
        .add_environment(
            &fixture.device,
            &fixture.queue,
            &crate::test_support::environment([255; 4], &0x3c00u16.to_le_bytes()),
        )
        .unwrap();
    fixture.input.environment = crate::FrameEnvironment::Map(environment);
    fixture.input.reflection_environment = crate::EnvironmentLight {
        intensity: 1.,
        ..Default::default()
    };
    // Inside the influence toward the emitter and away from it, and outside.
    let cases = [
        (CENTER + Vec3::splat(0.25), Vec3::X),
        (CENTER + Vec3::splat(0.25), Vec3::NEG_X),
        (CENTER + Vec3::splat(3.), Vec3::X),
    ];
    let observe = |fixture: &mut Fixture| sampled(fixture, ENVIRONMENT, &cases);
    // The emitter's own surface reflects a little of the sky.
    let near = |value: f32, expected: f32| (value - expected).abs() <= 0.02;
    for (case, sample) in observe(&mut fixture).iter().enumerate() {
        assert!(
            near(sample[0], 0.) && near(sample[1], 1.),
            "before its first publication case {case} takes {sample:?}, not the sky alone"
        );
    }
    assert_eq!(
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole),
        ProbeRefresh::Published
    );
    let after = observe(&mut fixture);
    assert!(
        near(after[0][0], RADIANCE) && near(after[0][1], 0.),
        "toward the emitter the receiver takes {:?}, not the probe's emitter",
        after[0]
    );
    assert!(
        near(after[1][0], 1.) && near(after[1][1], 0.),
        "away from the emitter the receiver takes {:?}, not the sky the probe captured",
        after[1]
    );
    assert!(
        near(after[2][0], 0.) && near(after[2][1], 1.),
        "outside the influence the receiver takes {:?}, not the sky alone",
        after[2]
    );
}

// A published cube is the scene's, not the array's: adding and removing
// another live probe and replacing the baked probes each make a new array,
// and the probe's cube is in each, bit for bit, at its new place; a baked
// probe installed beside it is sampled as itself. Defects: a rebuild that
// drops or misplaces a live cube, or one that leaves the probe published
// over an empty cube. Oracle: the cube's bytes before the changes.
#[test]
fn a_published_cube_survives_the_collections_changes() {
    use crate::baked_specular_probe::{BakedSpecularProbe, SpecularProbeRadiance};
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let published = fixture.cube(fixture.probe);
    assert_emitter_toward(&published, Vec3::X);
    let (device, queue) = (fixture.device.clone(), fixture.queue.clone());
    let other = fixture
        .scene
        .add_live_specular_probe(&device, &queue, probe())
        .unwrap();
    assert!(fixture.cube(fixture.probe) == published, "adding a probe");
    // A baked probe of radiance 2 on every face and level, far away.
    let texels =
        crate::scene::probes::payload_size(wgpu::TextureFormat::Rgba16Float, FACE_SIZE) / 2;
    let baked = BakedSpecularProbe {
        center: Vec3::splat(100.),
        world_to_local: Mat4::from_translation(Vec3::splat(-100.)),
        influence: probe().influence,
        blend: Vec3::ZERO,
        proxy: None,
        radiance: SpecularProbeRadiance {
            face_size: FACE_SIZE,
            texels: SpecularProbeTexels::Rgba16Float(vec![0x4000; texels]),
        },
    };
    fixture
        .scene
        .set_baked_specular_probes(&device, &queue, &[baked.clone(), baked])
        .unwrap();
    assert_eq!(
        fixture.scene.live_probe_cube(fixture.probe).unwrap().cube,
        2
    );
    assert!(
        fixture.cube(fixture.probe) == published,
        "installing baked probes"
    );
    // Removing a live probe makes a new array from the old one: the baked
    // probes' cubes are in it, and a receiver inside one samples its 2.
    let baked_probe = |fixture: &mut Fixture| {
        sampled(fixture, ENVIRONMENT, &[(Vec3::splat(100.), Vec3::X)])[0][0]
    };
    assert_eq!(baked_probe(&mut fixture), 2., "an uploaded baked probe");
    fixture
        .scene
        .remove_live_specular_probe(&device, &queue, other)
        .unwrap();
    assert_eq!(
        baked_probe(&mut fixture),
        2.,
        "a baked probe after a removal"
    );
    let another = fixture
        .scene
        .add_live_specular_probe(&device, &queue, probe())
        .unwrap();
    assert_eq!(
        baked_probe(&mut fixture),
        2.,
        "a baked probe after an addition"
    );
    assert!(fixture.cube(fixture.probe) == published, "adding another");
    fixture
        .scene
        .remove_live_specular_probe(&device, &queue, another)
        .unwrap();
    fixture
        .scene
        .set_baked_specular_probes(&device, &queue, &[])
        .unwrap();
    assert_eq!(
        fixture.scene.live_probe_cube(fixture.probe).unwrap().cube,
        0
    );
    assert!(
        fixture.cube(fixture.probe) == published,
        "removing the others"
    );
    // It is still placed: a refresh moves the emitter in the same cube.
    fixture.place(toward(Vec3::NEG_Z, Vec3::ZERO));
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    assert_emitter_toward(&fixture.cube(fixture.probe), Vec3::NEG_Z);
}

// What cannot share the scene's one cube array is refused, and a refusal
// changes nothing: another face size, compressed baked probes beside live
// ones (the renderer cannot write BC6H), a seventeenth live probe, and a
// placement a baked probe would be refused for. Oracle: the documented
// rules (`Scene::add_live_specular_probe`).
#[test]
fn live_probes_are_refused_what_the_cube_array_cannot_hold() {
    use crate::baked_specular_probe::{BakedSpecularProbe, ProbeError, SpecularProbeRadiance};
    use crate::content::live_specular_probe::MAX_LIVE_PROBES;
    use crate::scene::SceneError;
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let (device, queue) = (fixture.device.clone(), fixture.queue.clone());
    let scene = &mut fixture.scene;
    let mixed = |result: Result<_, SceneError>| {
        matches!(result, Err(SceneError::Probe(ProbeError::MixedRadiance)))
    };
    let other_size = LiveSpecularProbe {
        face_size: FACE_SIZE * 2,
        ..probe()
    };
    assert!(mixed(
        scene
            .add_live_specular_probe(&device, &queue, other_size)
            .map(|_| ())
    ));
    assert!(mixed(scene.set_live_specular_probe(
        &queue,
        fixture.probe,
        other_size
    )));
    let sheared = LiveSpecularProbe {
        world_to_local: Mat4::from_scale(Vec3::new(1., 2., 1.)),
        ..probe()
    };
    assert!(matches!(
        scene.set_live_specular_probe(&queue, fixture.probe, sheared),
        Err(SceneError::Probe(ProbeError::InvalidProbe(_)))
    ));
    assert_eq!(scene.live_specular_probe(fixture.probe).unwrap(), probe());
    if device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        let blocks =
            crate::scene::probes::payload_size(wgpu::TextureFormat::Bc6hRgbUfloat, FACE_SIZE);
        let compressed = BakedSpecularProbe {
            center: Vec3::ZERO,
            world_to_local: Mat4::IDENTITY,
            influence: probe().influence,
            blend: Vec3::ZERO,
            proxy: None,
            radiance: SpecularProbeRadiance {
                face_size: FACE_SIZE,
                texels: SpecularProbeTexels::Bc6hUfloat(vec![0; blocks]),
            },
        };
        assert!(mixed(scene.set_baked_specular_probes(
            &device,
            &queue,
            &[compressed]
        )));
    }
    for _ in 1..MAX_LIVE_PROBES {
        scene
            .add_live_specular_probe(&device, &queue, probe())
            .unwrap();
    }
    assert!(matches!(
        scene.add_live_specular_probe(&device, &queue, probe()),
        Err(SceneError::Probe(ProbeError::TooManyProbes))
    ));
    // The first probe is still the scene's, in the first cube.
    assert_eq!(scene.live_probe_cube(fixture.probe).unwrap().cube, 0);
    scene
        .remove_live_specular_probe(&device, &queue, fixture.probe)
        .unwrap();
    assert!(matches!(
        scene.live_specular_probe(fixture.probe),
        Err(SceneError::UnknownLiveSpecularProbe)
    ));
}

// A render origin move translates a live probe with the scene: its
// placement and the centre its published cube was captured from, so the
// cube it shows is unchanged, and a refresh whose faces were captured about
// the old origin starts again rather than publish a cube of two frames.
// Oracle: `Scene::move_origin`'s rule, positions less the move.
#[test]
fn a_render_origin_move_carries_a_live_probe_and_restarts_its_refresh() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let (device, queue) = (fixture.device.clone(), fixture.queue.clone());
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let published = fixture.cube(fixture.probe);
    // Three faces of a second refresh, then the move.
    fixture.place(toward(Vec3::Y, Vec3::ZERO));
    for _ in 0..3 {
        fixture.refresh(fixture.probe, ProbeRefreshPace::FacePerCall);
    }
    let by = Vec3::new(16., -8., 32.);
    fixture.scene.move_origin(&device, &queue, by).unwrap();
    let moved = fixture.scene.live_specular_probe(fixture.probe).unwrap();
    assert_eq!(moved.center, CENTER - by);
    assert_eq!(
        moved.world_to_local.transform_point3(CENTER - by),
        Vec3::ZERO
    );
    assert!(fixture.cube(fixture.probe) == published);
    // The scene moved the emitter with it: the refresh starts over, six
    // faces and the filter, and shows it where it is.
    for call in 0..6 {
        assert_eq!(
            fixture.refresh(fixture.probe, ProbeRefreshPace::FacePerCall),
            ProbeRefresh::InFlight,
            "call {call} after the move"
        );
    }
    assert_eq!(
        fixture.refresh(fixture.probe, ProbeRefreshPace::FacePerCall),
        ProbeRefresh::Published
    );
    assert_emitter_toward(&fixture.cube(fixture.probe), Vec3::Y);
}

/// A slab of `size` about `center`: a lit grey surface, moving or static.
fn slab(fixture: &mut Fixture, center: Vec3, size: Vec3, mobility: Mobility) -> InstanceId {
    let mut asset = crate::test_support::cube();
    asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
    asset.materials[0].metallic = 0.;
    let ids = fixture
        .scene
        .add_asset(&fixture.device, &fixture.queue, asset)
        .unwrap();
    fixture
        .scene
        .add_instance(
            &fixture.device,
            &fixture.queue,
            InstanceState {
                pose: Mat4::from_translation(center) * Mat4::from_scale(size),
                ..InstanceState::new(ids.model)
            },
            mobility,
        )
        .unwrap()
}

// A refresh's sun shadow comes from cascades about the probe, drawn for it
// with the moving casters where they are now: a floor below the probe is
// dark under a moving occluder high above it, beyond the first cascade and
// far from the view's camera, lit once the occluder has moved away, and
// dark again when it is back. Defects: cascades left as the view's last
// frame fit them, static casters only, or a caster drawn at an old pose.
// Oracle: the geometry (the bake's `floor_under_occluder`).
#[test]
fn a_refresh_shadows_the_sun_by_moving_casters_where_they_are() {
    use crate::{DirectionalLight, DirectionalShadow};
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    slab(
        &mut fixture,
        CENTER - Vec3::Y,
        Vec3::new(12., 0.1, 12.),
        Mobility::Static,
    );
    // Above the first cascade's cube, which reaches 2 m, and its pancake.
    let above = CENTER + Vec3::Y * (12. + crate::view::cascades::SHADOW_PANCAKE_SIZE);
    let occluder = slab(
        &mut fixture,
        above,
        Vec3::new(6., 0.1, 6.),
        Mobility::Moving,
    );
    fixture.input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::NEG_Y,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(DirectionalShadow {
            distance: 20.,
            cascades: 3,
        }),
        ..Default::default()
    });
    let floor = |fixture: &mut Fixture, aside: f32| {
        let state = *fixture.scene.instance(occluder).unwrap();
        let pose = Mat4::from_translation(above + Vec3::X * aside)
            * Mat4::from_scale(Vec3::new(6., 0.1, 6.));
        fixture
            .scene
            .set_instance(&fixture.queue, occluder, InstanceState { pose, ..state })
            .unwrap();
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
        // The downward face's centre at full detail.
        face_centres(&fixture.cube(fixture.probe), 0)[stored_face(Vec3::NEG_Y)]
    };
    let shadowed = floor(&mut fixture, 0.);
    let lit = floor(&mut fixture, 100.);
    let again = floor(&mut fixture, 0.);
    assert!(
        lit > 0.05 && shadowed < 0.01 * lit && again < 0.01 * lit,
        "the floor below the probe is {shadowed} under the occluder, {lit} once it moved          away and {again} when it is back"
    );
}

// A refresh is not a frame: the frame after one renders what it would have
// without it, and draws no local-light shadow it would not have drawn: a
// refresh that placed its own layers in the view's atlas, as a bake does,
// would make the next frame place and draw the light's static layers
// again. Both runs refresh once before the first frame, so they sample the
// same probe; one refreshes again between the frames, which republishes
// the same cube. Oracle: the frames and their shadow statistics from the
// renderer that did not refresh between them.
#[test]
fn a_frame_after_a_refresh_renders_as_without_it() {
    use crate::diagnostics::DiagnosticTarget;
    use crate::{DirectionalLight, DirectionalShadow, Light, LightShape};
    let frames = |between: bool| {
        let mut fixture = Fixture::new()?;
        slab(
            &mut fixture,
            CENTER - Vec3::Y,
            Vec3::new(12., 0.1, 12.),
            Mobility::Static,
        );
        slab(
            &mut fixture,
            CENTER + Vec3::new(1., 0., 1.),
            Vec3::splat(0.5),
            Mobility::Static,
        );
        fixture
            .scene
            .add_light(
                &fixture.device,
                &fixture.queue,
                Light {
                    position: CENTER + Vec3::new(0., 3., 0.),
                    shape: LightShape::Point {
                        radius: LightShape::DEFAULT_RADIUS,
                    },
                    color: [1.; 3],
                    intensity: 20.,
                    range: 10.,
                    casts_shadow: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let eye = CENTER + Vec3::new(0., 5., 7.);
        fixture.input = FrameInput::new(Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, CENTER, Vec3::Y),
            projection: crate::perspective(1., 1., 0.1),
            eye,
        });
        fixture.input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(0.3, -1., 0.2).normalize(),
            color: [1.; 3],
            illuminance: 1.,
            shadow: Some(DirectionalShadow {
                distance: 30.,
                cascades: 2,
            }),
            ..Default::default()
        });
        let output = crate::view::targets::target(
            &fixture.device,
            "frame around the refresh",
            [64, 64],
            crate::shading::gbuffer::COLOR,
        );
        let refresh = |fixture: &mut Fixture| {
            let published = fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
            assert_eq!(published, ProbeRefresh::Published);
        };
        refresh(&mut fixture);
        for frame in 0..2 {
            if frame == 1 && between {
                refresh(&mut fixture);
            }
            fixture.input.elapsed_seconds = f64::from(frame) / 60.;
            fixture.input.camera_cut = frame == 0;
            let mut encoder = fixture.device.create_command_encoder(&Default::default());
            fixture.renderer.render(
                &fixture.device,
                &fixture.queue,
                &mut encoder,
                &mut fixture.scene,
                &fixture.input,
                &fixture.settings,
                &output,
                None,
            );
            fixture.queue.submit([encoder.finish()]);
            fixture.renderer.finish_frame(&mut fixture.scene);
        }
        let composite = fixture
            .renderer
            .view
            .diagnostic_target(DiagnosticTarget::Composite)
            .unwrap()
            .texture()
            .clone();
        Some((
            crate::diagnostics::read(&fixture.device, &fixture.queue, &composite, 8),
            fixture.renderer.view.local_shadow_stats(),
        ))
    };
    let Some((without, stats)) = frames(false) else {
        return;
    };
    let (with, refreshed) = frames(true).unwrap();
    assert!(stats.shadowed == 1 && stats.layers_drawn == 0, "{stats:?}");
    assert_eq!(refreshed, stats);
    assert!(
        with == without,
        "the frame after a refresh differs from the frame without it"
    );
}

// A refresh's scene lights take the shadows the view's last frame drew,
// from its frame atlas, and place or draw none of their own: a floor below
// the probe under an occluder is lit before the view has drawn the light's
// shadow, dark once a frame has, dark still when the light moved in that
// frame (its static layers then hold nothing, the register's S-31, but the
// frame atlas holds its every caster), and lit again once the scene's
// render origin has moved and no frame has placed the shadows there.
// Defects: a refresh bound to no shadows or to the static layers, records
// of another origin sampled as the scene's. Oracle: the geometry.
#[test]
fn a_refresh_takes_the_scene_lights_shadows_the_views_last_frame_drew() {
    use crate::{Light, LightShape};
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let (device, queue) = (fixture.device.clone(), fixture.queue.clone());
    slab(
        &mut fixture,
        CENTER - Vec3::Y,
        Vec3::new(12., 0.1, 12.),
        Mobility::Static,
    );
    slab(
        &mut fixture,
        CENTER + Vec3::Y * 1.5,
        Vec3::new(2., 0.1, 2.),
        Mobility::Static,
    );
    let light = Light {
        position: CENTER + Vec3::Y * 3.,
        shape: LightShape::Point {
            radius: LightShape::DEFAULT_RADIUS,
        },
        color: [1.; 3],
        intensity: 20.,
        range: 10.,
        casts_shadow: true,
        ..Default::default()
    };
    let id = fixture.scene.add_light(&device, &queue, light).unwrap();
    let eye = CENTER + Vec3::new(0., 5., 7.);
    let camera = Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, CENTER, Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    };
    let output = crate::view::targets::target(
        &device,
        "frame before the refresh",
        [64, 64],
        crate::shading::gbuffer::COLOR,
    );
    // The floor at the middle of the refreshed cube's downward face.
    let floor = |fixture: &mut Fixture| {
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
        face_centres(&fixture.cube(fixture.probe), 0)[stored_face(Vec3::NEG_Y)]
    };
    let frame = |fixture: &mut Fixture| {
        fixture.input.camera = camera;
        let mut encoder = fixture.device.create_command_encoder(&Default::default());
        fixture.renderer.render(
            &fixture.device,
            &fixture.queue,
            &mut encoder,
            &mut fixture.scene,
            &fixture.input,
            &fixture.settings,
            &output,
            None,
        );
        fixture.queue.submit([encoder.finish()]);
        fixture.renderer.finish_frame(&mut fixture.scene);
    };
    let unplaced = floor(&mut fixture);
    frame(&mut fixture);
    let shadowed = floor(&mut fixture);
    // The light moves a little, still above the occluder.
    fixture
        .scene
        .set_light(
            &queue,
            id,
            Light {
                position: light.position + Vec3::X * 0.05,
                ..light
            },
        )
        .unwrap();
    frame(&mut fixture);
    assert_eq!(fixture.renderer.view.local_shadow_stats().layers_drawn, 0);
    let moved = floor(&mut fixture);
    fixture
        .scene
        .move_origin(&device, &queue, Vec3::new(8., 0., -8.))
        .unwrap();
    let elsewhere = floor(&mut fixture);
    assert!(
        unplaced > 0.05
            && shadowed < 0.02 * unplaced
            && moved < 0.02 * unplaced
            && elsewhere > 0.5 * unplaced,
        "the floor below the probe is {unplaced} before a frame placed the shadow, \
         {shadowed} after, {moved} with the light moving and {elsewhere} after an origin move"
    );
}

// Under a procedural sky a refresh shows the sky the view's last frame
// computed, and computes none: nothing before the view's first frame under
// it, the sky's light on the upward face after. Defects: a refresh bound
// to no sky, or one that binds tables no frame has filled. Oracle: the sun
// is up, so the zenith is lit.
#[test]
fn a_refresh_shows_the_sky_the_views_last_frame_computed() {
    use crate::{DirectionalLight, FrameEnvironment, Sky};
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    fixture.input.environment = FrameEnvironment::Sky(Sky {
        ground_height: CENTER.y - 2.,
        ..Sky::default()
    });
    fixture.input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(0.3, -1., 0.2).normalize(),
        color: [1.; 3],
        illuminance: 10.,
        ..Default::default()
    });
    let zenith = |fixture: &mut Fixture| {
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
        face_centres(&fixture.cube(fixture.probe), 0)[stored_face(Vec3::Y)]
    };
    let before = zenith(&mut fixture);
    let output = crate::view::targets::target(
        &fixture.device,
        "frame under the sky",
        [64, 64],
        crate::shading::gbuffer::COLOR,
    );
    let mut encoder = fixture.device.create_command_encoder(&Default::default());
    fixture.renderer.render(
        &fixture.device,
        &fixture.queue,
        &mut encoder,
        &mut fixture.scene,
        &fixture.input,
        &fixture.settings,
        &output,
        None,
    );
    fixture.queue.submit([encoder.finish()]);
    fixture.renderer.finish_frame(&mut fixture.scene);
    let after = zenith(&mut fixture);
    assert!(
        before == 0. && after > 0.01,
        "the zenith is {before} before the view's first frame under the sky and {after} after"
    );
}

// A refresh is one capture from the centre its first call found: a probe
// moved while its refresh is spread over calls publishes the cube of the
// old centre, and its record corrects parallax about that centre, not the
// probe's new one; the next refresh captures from the new centre. The
// fixture's emitter lies toward +X of the first centre. A receiver at that
// centre looking along +X meets the proxy at its +X face, which lies toward
// +X of the first centre and 40 degrees below +X of the second. Oracle:
// box projection's construction (Lagarde and Zanuttini).
#[test]
fn a_refresh_captures_from_and_corrects_parallax_about_its_first_centre() {
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let boxed = SpecularProbeBox {
        min: Vec3::splat(-3.),
        max: Vec3::splat(3.),
    };
    let placed = LiveSpecularProbe {
        influence: boxed,
        proxy: Some(boxed),
        ..probe()
    };
    let queue = fixture.queue.clone();
    fixture
        .scene
        .set_live_specular_probe(&queue, fixture.probe, placed)
        .unwrap();
    let toward_x = |fixture: &mut Fixture| {
        sampled(
            fixture,
            "vec4(probe_radiance(0u,world,direction,0.,environment_sampler),0.)",
            &[(CENTER, Vec3::X)],
        )[0][0]
    };
    assert_eq!(
        fixture.refresh(fixture.probe, ProbeRefreshPace::FacePerCall),
        ProbeRefresh::InFlight
    );
    // The probe moves up within its proxy while its refresh is in flight.
    let moved = LiveSpecularProbe {
        center: CENTER + Vec3::Y * 2.5,
        ..placed
    };
    fixture
        .scene
        .set_live_specular_probe(&queue, fixture.probe, moved)
        .unwrap();
    while fixture.refresh(fixture.probe, ProbeRefreshPace::FacePerCall) != ProbeRefresh::Published {
    }
    let first = toward_x(&mut fixture);
    assert_emitter_toward(&fixture.cube(fixture.probe), Vec3::X);
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let second = toward_x(&mut fixture);
    assert!(
        (first - RADIANCE).abs() <= 0.02 * RADIANCE && second < 0.1 * RADIANCE,
        "the receiver sees {first} of the emitter through the cube of the first centre \
         and {second} through the next, captured from the new one"
    );
}

// The camera's surfaces take a live probe as they take a baked one, from
// the probes culled per screen tile: a mirror before the camera, inside
// the probe's influence and left out of its refresh, shows nothing of the
// emitter behind the camera before the first publication and the emitter
// after. Screen-space reflections cannot reach it: it is off screen.
// Defects: tile culling or source completion walking baked probes only.
// Oracle: the emitter's radiance, mirrored by a perfect metal.
#[test]
fn the_cameras_surfaces_reflect_a_published_live_probe() {
    use crate::diagnostics::DiagnosticTarget;
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    // The emitter two metres toward +Z of the centre, behind the camera.
    fixture.place(toward(Vec3::Z, Vec3::ZERO));
    let mut mirror = crate::test_support::cube();
    mirror.materials[0].base = [1.; 4];
    mirror.materials[0].metallic = 1.;
    mirror.materials[0].roughness = 0.;
    mirror.materials[0].visibility_group = 1;
    let ids = fixture
        .scene
        .add_asset(&fixture.device, &fixture.queue, mirror)
        .unwrap();
    fixture
        .scene
        .add_instance(
            &fixture.device,
            &fixture.queue,
            InstanceState {
                pose: Mat4::from_translation(CENTER - Vec3::Z * 0.9)
                    * Mat4::from_scale(Vec3::new(4., 4., 0.1)),
                ..InstanceState::new(ids.model)
            },
            Mobility::Static,
        )
        .unwrap();
    let eye = CENTER + Vec3::Z * 1.5;
    let frames = FrameInput {
        visibility_mask: 1,
        ..FrameInput::new(Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, CENTER - Vec3::Z, Vec3::Y),
            projection: crate::perspective(1., 1., 0.1),
            eye,
        })
    };
    let output = crate::view::targets::target(
        &fixture.device,
        "frame of the mirror",
        [64, 64],
        crate::shading::gbuffer::COLOR,
    );
    // The middle of the frame's HDR composite after a few frames.
    let mirrored = |fixture: &mut Fixture| {
        for frame in 0..4 {
            let mut input = frames;
            input.elapsed_seconds = f64::from(frame) / 60.;
            let mut encoder = fixture.device.create_command_encoder(&Default::default());
            fixture.renderer.render(
                &fixture.device,
                &fixture.queue,
                &mut encoder,
                &mut fixture.scene,
                &input,
                &fixture.settings,
                &output,
                None,
            );
            fixture.queue.submit([encoder.finish()]);
            fixture.renderer.finish_frame(&mut fixture.scene);
        }
        let composite = fixture
            .renderer
            .view
            .diagnostic_target(DiagnosticTarget::Composite)
            .unwrap()
            .texture()
            .clone();
        let texels = crate::diagnostics::read(&fixture.device, &fixture.queue, &composite, 8);
        half(&texels[(32 * 64 + 32) * 8..])
    };
    let before = mirrored(&mut fixture);
    // The probe hides the mirror's group; its refresh takes the frames' input.
    let queue = fixture.queue.clone();
    fixture
        .scene
        .set_live_specular_probe(
            &queue,
            fixture.probe,
            LiveSpecularProbe {
                hidden_groups: 1,
                ..probe()
            },
        )
        .unwrap();
    fixture.input = frames;
    assert_eq!(
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole),
        ProbeRefresh::Published
    );
    let after = mirrored(&mut fixture);
    assert!(
        before < 0.02 * RADIANCE && after > 0.8 * RADIANCE && after < 1.05 * RADIANCE,
        "the mirror shows {before} before the probe's first publication and {after} after"
    );
}

// A refresh draws through the geometry pipeline cache that
// `Renderer::compile` fills with a frame's jobs, and its own variants are
// none of the frame's: a refresh made while a frame's jobs are still
// outstanding, and one made after they ran and the frame rendered, both
// publish the cube a renderer that never compiled ahead publishes. Defects:
// a refresh that takes a planning pass's missing pipeline for a draw to
// skip, or that waits on or drops a job it does not own. Oracle: the cube
// from the fixture that compiled nothing ahead, on the same device kind.
#[test]
fn a_refresh_beside_a_frame_compiled_ahead_publishes_the_same_cube() {
    use crate::{DirectionalLight, DirectionalShadow, ViewFrame, ViewOutput};
    let build = || {
        let mut fixture = Fixture::new()?;
        slab(
            &mut fixture,
            CENTER - Vec3::Y,
            Vec3::new(12., 0.1, 12.),
            Mobility::Static,
        );
        fixture.input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(0.3, -1., 0.2).normalize(),
            color: [1.; 3],
            illuminance: 1.,
            shadow: Some(DirectionalShadow {
                distance: 20.,
                cascades: 2,
            }),
            ..Default::default()
        });
        Some(fixture)
    };
    // Renders, submits and finishes one frame.
    let frame = |fixture: &mut Fixture, output: &wgpu::TextureView| {
        let mut encoder = fixture.device.create_command_encoder(&Default::default());
        fixture.renderer.render(
            &fixture.device,
            &fixture.queue,
            &mut encoder,
            &mut fixture.scene,
            &fixture.input,
            &fixture.settings,
            output,
            None,
        );
        fixture.queue.submit([encoder.finish()]);
        fixture.renderer.finish_frame(&mut fixture.scene);
    };
    let target = |fixture: &Fixture| {
        crate::view::targets::target(
            &fixture.device,
            "frame beside the refreshes",
            [64, 64],
            crate::shading::gbuffer::COLOR,
        )
    };
    // The control: a refresh, a frame, and a second refresh, which holds a
    // bounce of the first (the register's X-6), with nothing compiled ahead.
    let Some(mut ordinary) = build() else {
        return;
    };
    let output = target(&ordinary);
    ordinary.refresh(ordinary.probe, ProbeRefreshPace::Whole);
    let first = ordinary.cube(ordinary.probe);
    frame(&mut ordinary, &output);
    ordinary.refresh(ordinary.probe, ProbeRefreshPace::Whole);
    let second = ordinary.cube(ordinary.probe);
    // It shows the scene: the emitter and the sunlit floor.
    let centres = face_centres(&first, 0);
    assert!(
        (centres[stored_face(Vec3::X)] - RADIANCE).abs() <= RADIANCE / 512.
            && centres[stored_face(Vec3::NEG_Y)] > 0.05,
        "the ordinary refresh holds {centres:?}"
    );
    let mut fixture = build().unwrap();
    let output = target(&fixture);
    let compile = |fixture: &mut Fixture| {
        fixture.renderer.renderer.compile(
            &fixture.device,
            &fixture.queue,
            &mut fixture.scene,
            &mut [ViewFrame {
                view: &mut fixture.renderer.view,
                input: &fixture.input,
                settings: &fixture.settings,
                output: ViewOutput::Texture(&output),
                timing: None,
            }],
        )
    };
    // The frame's jobs are held, neither run nor dropped, across a refresh.
    let held = compile(&mut fixture);
    assert!(!held.jobs.is_empty(), "the frame has pipelines to compile");
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    assert!(
        fixture.cube(fixture.probe) == first,
        "a refresh while a frame's jobs are outstanding"
    );
    // They run, the frame is ready and rendered, and a refresh follows.
    held.jobs.into_iter().for_each(|job| job.run());
    for _ in 0..8 {
        let compilation = compile(&mut fixture);
        if compilation.ready() {
            break;
        }
        compilation.jobs.into_iter().for_each(|job| job.run());
    }
    assert!(compile(&mut fixture).ready());
    frame(&mut fixture, &output);
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    assert!(
        fixture.cube(fixture.probe) == second,
        "a refresh after a frame compiled ahead"
    );
}

// A refreshed probe shows a spot light's measured profile, as the frame's
// surfaces do: a spot above the probe shines straight down at a floor, and
// its profile, added between two refreshes of the warmed probe, is dark
// along its axis and brightest across it, so the floor below the probe is
// lit without the profile and dark with it. Defects: a refresh's pipelines
// left specialised for a scene without profiles, or its light records read
// at another size than the scene writes. Oracle: the profile's candela, 0
// at its nadir.
#[test]
fn a_refresh_shows_a_spot_lights_profile() {
    use crate::{Light, LightProfile, LightShape, SpotProfile};
    let Some(mut fixture) = Fixture::new() else {
        return;
    };
    let (device, queue) = (fixture.device.clone(), fixture.queue.clone());
    slab(
        &mut fixture,
        CENTER - Vec3::Y,
        Vec3::new(12., 0.1, 12.),
        Mobility::Static,
    );
    let light = Light {
        position: CENTER + Vec3::Y * 3.,
        shape: LightShape::Spot {
            direction: Vec3::NEG_Y,
            inner_angle: 0.4,
            outer_angle: 0.6,
            radius: 0.,
        },
        color: [1.; 3],
        intensity: 20.,
        range: 10.,
        ..Default::default()
    };
    let id = fixture.scene.add_light(&device, &queue, light).unwrap();
    let floor = |fixture: &mut Fixture| {
        fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
        face_centres(&fixture.cube(fixture.probe), 0)[stored_face(Vec3::NEG_Y)]
    };
    let plain = floor(&mut fixture);
    // Axially symmetric: no light at the nadir, 1000 cd from 45 degrees.
    let profile = fixture.scene.add_light_profile(
        LightProfile::from_ies(
            "IESNA:LM-63-2002\nTILT=NONE\n1 -1 1 3 1 1 2 0 0 0\n1 1 10\n0 45 90\n0\n0 1000 1000\n",
        )
        .unwrap(),
    );
    fixture
        .scene
        .set_light(
            &queue,
            id,
            Light {
                profile: Some(SpotProfile {
                    profile,
                    zero_azimuth: Vec3::X,
                }),
                ..light
            },
        )
        .unwrap();
    let profiled = floor(&mut fixture);
    assert!(
        plain > 0.05 && profiled < 0.1 * plain,
        "the floor on the spot's axis is {plain} without its profile and {profiled} with it"
    );
}

/// How many times `name(` begins an action or command in a wgpu trace.
fn count(trace: &str, name: &str) -> usize {
    trace.matches(&format!("{name}(")).count()
}

// The live path keeps every texel on the GPU, and a warmed probe's refresh
// allocates nothing in the frame loop: wgpu's own record of what the device
// was asked to do during refreshes after the first (its API trace) holds
// the render and compute passes and the texture-to-texture copies of a
// publication; no texture write, no copy between a buffer and a texture,
// no buffer copy and no mappable buffer; and no buffer, texture, texture
// view, sampler, layout, shader or pipeline created. What it still creates
// is bind groups, which the test reports. The scene has a sun with a
// shadow and a casting light, so the cascades, the light list and the
// shadow records are among what a refresh writes. The control is the
// test's own readback of the cube, which the same trace then shows.
// Oracle: the device's commands.
#[test]
fn refreshes_read_nothing_back_and_upload_no_texels() {
    let Some(adapter) = crate::test_support::adapter() else {
        return;
    };
    // Under the workspace's `target/`, where test output goes.
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join(format!("sgl-live-probe-trace-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: crate::graphics_device::features(&adapter),
        required_limits: crate::graphics_device::limits(&adapter),
        trace: wgpu::Trace::Directory(directory.clone()),
        ..Default::default()
    }))
    .unwrap();
    let trace = || std::fs::read_to_string(directory.join("trace.ron")).unwrap();
    let mut fixture = Fixture::on(device, queue);
    slab(
        &mut fixture,
        CENTER - Vec3::Y,
        Vec3::new(12., 0.1, 12.),
        Mobility::Static,
    );
    fixture.input.directional_lights[0] = Some(crate::DirectionalLight {
        direction: Vec3::new(0.3, -1., 0.2).normalize(),
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(crate::DirectionalShadow {
            distance: 20.,
            cascades: 3,
        }),
        ..Default::default()
    });
    fixture
        .scene
        .add_light(
            &fixture.device,
            &fixture.queue,
            crate::Light {
                position: CENTER + Vec3::Y * 3.,
                casts_shadow: true,
                ..Default::default()
            },
        )
        .unwrap();
    // The first refresh creates the staging, its pipelines and the buffers
    // every later one writes.
    fixture.refresh(fixture.probe, ProbeRefreshPace::Whole);
    let warm = trace().len();
    let mut publications = 0;
    for (pace, direction) in [
        (ProbeRefreshPace::Whole, Vec3::NEG_X),
        (ProbeRefreshPace::FacesThenFilter, Vec3::Y),
        (ProbeRefreshPace::FacePerCall, Vec3::Z),
    ] {
        fixture.place(toward(direction, Vec3::ZERO));
        let mut probe = probe();
        probe.center += direction * 0.125;
        fixture
            .scene
            .set_live_specular_probe(&fixture.queue, fixture.probe, probe)
            .unwrap();
        while fixture.refresh(fixture.probe, pace) != ProbeRefresh::Published {}
        publications += 1;
    }
    let all = trace();
    let live = &all[warm..];
    for forbidden in [
        "WriteTexture",
        "CopyBufferToTexture",
        "CopyTextureToBuffer",
        "CopyBufferToBuffer",
        "CreateBuffer",
        "CreateTexture",
        "CreateTextureView",
        "CreateSampler",
        "CreateBindGroupLayout",
        "CreatePipelineLayout",
        "CreateComputePipeline",
        "CreateGeneralRenderPipeline",
        "CreateShaderModule",
        "CreateQuerySet",
    ] {
        assert_eq!(
            count(live, forbidden),
            0,
            "the live path recorded {forbidden}"
        );
    }
    assert!(
        !live.contains("MAP_READ") && !live.contains("MAP_WRITE"),
        "the live path created a mappable buffer"
    );
    // The work is there to be traced: each refresh renders six faces, and
    // its publication is a copy between textures.
    assert!(count(live, "RunRenderPass") >= publications * 6);
    assert!(count(live, "CopyTextureToTexture") >= publications);
    assert!(count(live, "RunComputePass") >= publications);
    // What a warmed refresh still creates: each face's lit and unlit group
    // 0, which bind what may change between calls (the view's shadow maps,
    // the environment, the scene's probes and lights, the GI volume).
    assert!(count(live, "CreateBindGroup") <= publications * 12);
    // What it writes to buffers: the capture's view, frame, light list and
    // draw-instance records and the probe's metadata record, by size.
    let written: u64 = live
        .split("WriteBuffer(")
        .skip(1)
        .map(|write| {
            let size = write.split("size: ").nth(1).unwrap();
            let digits: String = size.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().unwrap()
        })
        .sum();
    eprintln!(
        "live probe refresh: {} bind groups created, {} buffer writes, {written} bytes, over \
         {publications} publications; {} render passes, {} compute passes, {} texture copies",
        count(live, "CreateBindGroup"),
        count(live, "WriteBuffer"),
        count(live, "RunRenderPass"),
        count(live, "RunComputePass"),
        count(live, "CopyTextureToTexture"),
    );
    assert!(
        written < publications as u64 * u64::from(FACE_SIZE * FACE_SIZE * 8),
        "the live path wrote {written} bytes to buffers: a face's texels a publication or more"
    );
    // The control: a readback is in the trace when one happens.
    let traced = all.len();
    fixture.cube(fixture.probe);
    let read = &trace()[traced..];
    assert!(count(read, "CopyTextureToBuffer") > 0 && read.contains("MAP_READ"));
    drop(fixture);
    let _ = std::fs::remove_dir_all(&directory);
}
