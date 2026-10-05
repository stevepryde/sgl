//! Two-phase occlusion culling through the renderer's frames, at its
//! boundaries: what each phase appends to the camera's sets, the image, and
//! the depth pyramid, read back against geometric oracles.
use crate::asset::Asset;
use crate::content::identity::Identity;
use crate::renderer::Renderer;
use crate::settings::{AmbientOcclusionQuality, Settings};
use crate::shading::gbuffer;
use crate::shading::vertex::DrawInstance;
use crate::view::culling::tests::mesh;
use crate::{Camera, FrameInput, InstanceId, InstanceState, Mobility, Scene, test_support};
use glam::{Mat4, Vec3};
use std::collections::BTreeSet;

/// A render size whose pyramid's first level is square (128×128), so every
/// level's reduction reads within the level above.
const SIZE: [u32; 2] = [160, 128];
/// The section test's: a first level of 256×256.
const FINE: [u32; 2] = [320, 256];

fn settings() -> Settings {
    Settings {
        occlusion_culling: true,
        ambient_occlusion: AmbientOcclusionQuality::Off,
        ..Settings::default()
    }
}

/// The vertical field of view, in radians.
const FOV: f32 = 1.;

/// The camera at the origin looking down -Z, at `size`.
fn input_at(size: [u32; 2]) -> FrameInput {
    FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(FOV, size[0] as f32 / size[1] as f32, 0.1),
        eye: Vec3::ZERO,
    })
}

/// The camera at the origin looking down -Z, at `SIZE`.
fn input() -> FrameInput {
    input_at(SIZE)
}

/// A renderer and its output, which renders whole frames.
struct Frames {
    renderer: Renderer,
    output: wgpu::TextureView,
    size: [u32; 2],
}

impl Frames {
    /// None, after saying why, on a device that cannot cull occlusion.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        Self::at(device, queue, SIZE)
    }

    /// `new` at `size`.
    fn at(device: &wgpu::Device, queue: &wgpu::Queue, size: [u32; 2]) -> Option<Self> {
        let renderer = Renderer::for_test(device, queue, size, &settings());
        if !renderer.occlusion_culling_in_effect(&settings()) {
            eprintln!(
                "skipping occlusion culling: the device binds {} storage textures a stage",
                device.limits().max_storage_textures_per_shader_stage
            );
            return None;
        }
        Some(Self {
            renderer,
            output: crate::view::targets::target(device, "output", size, gbuffer::COLOR),
            size,
        })
    }

    /// Renders, submits and finishes one frame of `scene` seen as `input`,
    /// and returns what each phase appended to the camera's sets.
    fn frame(
        &mut self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        scene: &mut Scene,
        input: &FrameInput,
    ) -> [Vec<DrawInstance>; 2] {
        let mut encoder = device.create_command_encoder(&Default::default());
        self.renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            input,
            &settings(),
            &self.output,
            None,
        );
        queue.submit([encoder.finish()]);
        self.renderer.finish_frame(scene);
        self.renderer.test_camera_phases(device, queue, scene)
    }

    /// The object whose surface the opaque stage recorded at `pixel`, if
    /// any.
    fn source_at(&self, gpu: (&wgpu::Device, &wgpu::Queue), pixel: [u32; 2]) -> Option<u32> {
        let texture = self.renderer.targets().source_id.texture();
        let words = test_support::read(gpu.0, gpu.1, texture, 8);
        let at = ((pixel[1] * self.size[0] + pixel[0]) * 8) as usize;
        let id = u32::from_le_bytes(words[at..at + 4].try_into().unwrap());
        id.checked_sub(1)
    }
}

/// Adds `asset` and an instance of it at `pose`.
fn add(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    asset: Asset,
    pose: Mat4,
    mobility: Mobility,
) -> InstanceId {
    let model = scene.add_asset(device, queue, asset).unwrap().model;
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose,
                ..InstanceState::new(model)
            },
            mobility,
        )
        .unwrap()
}

/// The objects `drawn` names.
fn objects(drawn: &[DrawInstance]) -> BTreeSet<u32> {
    drawn.iter().map(|drawn| drawn.object).collect()
}

/// A wall 8 m square and 0.2 m thick facing the camera at 10 m, a unit box
/// hidden behind it at 20 m and one in front of it at 5 m, off its centre.
fn wall_and_boxes(gpu: (&wgpu::Device, &wgpu::Queue), scene: &mut Scene) -> [InstanceId; 3] {
    let wall = add(
        gpu,
        scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(0., 0., -10.)) * Mat4::from_scale(Vec3::new(8., 8., 0.2)),
        Mobility::Moving,
    );
    let behind = add(
        gpu,
        scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(0., 0., -20.)),
        Mobility::Static,
    );
    let front = add(
        gpu,
        scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(1.5, 0., -5.)),
        Mobility::Static,
    );
    [wall, behind, front]
}

/// The object record index of `id`, which draw instances name.
fn object(id: InstanceId) -> u32 {
    id.index() as u32
}

// Plausible defects: the early phase testing nothing (no pyramid bound, the
// flag never set, the pyramid never built for the next frame), testing with
// the wrong matrices or pose so a visible box is culled or a hidden one
// kept, the comparison reversed, or a reset frame testing a stale pyramid.
// The oracle is the scene's geometry: the box at 20 m lies wholly behind the
// wall from the camera, the box at 5 m in front of it. A frame whose camera
// history continues draws the wall and the front box and neither phase
// draws the hidden box; the first frame and a camera cut test no occlusion,
// so they draw all three in the early phase and nothing late.
#[test]
fn a_box_behind_a_wall_is_culled_and_one_in_front_is_kept() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let [wall, behind, front] = wall_and_boxes(gpu, &mut scene).map(object);
    let all = BTreeSet::from([wall, behind, front]);
    let mut cut = input();
    cut.camera_cut = true;
    for (frame, input) in [input(), input(), cut, input()].iter().enumerate() {
        let [early, late] = frames.frame(gpu, &mut scene, input);
        let (early, late) = (objects(&early), objects(&late));
        if frame == 0 || input.camera_cut {
            assert_eq!(early, all, "frame {frame} tests no occlusion");
            assert!(late.is_empty(), "frame {frame} has no late phase's draws");
        } else {
            assert!(early.contains(&front), "frame {frame} keeps the front box");
            assert!(
                early.contains(&wall) || late.contains(&wall),
                "frame {frame} draws the wall"
            );
            assert!(
                !early.contains(&behind) && !late.contains(&behind),
                "frame {frame} culls the hidden box: early {early:?}, late {late:?}"
            );
        }
    }
}

// Plausible defects: the late phase testing against the last frame's
// pyramid or not running, its pyramid not built from the early set's depth,
// its list or dispatch never filled, or its draws never issued, so an
// object hidden last frame and visible now is missing for a frame. The
// oracle is the geometry and the image: once the wall moves out of view,
// the box behind it is visible; the frame it moves in, the late phase
// appends it (the early phase still finds it behind the last frame's wall)
// and the opaque stage records it at its pixel, the middle of the view.
#[test]
fn an_object_that_comes_into_view_is_drawn_in_that_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let [wall_id, behind_id, _] = wall_and_boxes(gpu, &mut scene);
    let (wall, behind) = (object(wall_id), object(behind_id));
    let middle = [SIZE[0] / 2, SIZE[1] / 2];
    for _ in 0..2 {
        frames.frame(gpu, &mut scene, &input());
    }
    assert_eq!(
        frames.source_at(gpu, middle),
        Some(wall),
        "the wall hides the box"
    );
    let state = InstanceState {
        pose: Mat4::from_translation(Vec3::new(100., 0., -10.)),
        ..*scene.instance(wall_id).unwrap()
    };
    scene.set_instance(&queue, wall_id, state).unwrap();
    let [early, late] = frames.frame(gpu, &mut scene, &input());
    assert!(
        !objects(&early).contains(&behind) && objects(&late).contains(&behind),
        "the late phase draws the box the early phase found behind the last frame's wall"
    );
    assert_eq!(
        frames.source_at(gpu, middle),
        Some(behind),
        "the box shows the frame the wall moves"
    );
}

// Plausible defect: the occlusion test projecting a box that reaches the
// camera's near plane (its corners behind the camera flip sign through the
// divide), or a near-plane test reversed or dropped, so such a box takes a
// rectangle and a nearest depth behind what the camera sees and is culled.
// The oracle is the geometry and the image: a floor 40 m square passes
// under the camera, half of it behind, and a wall 3 m ahead stands on it;
// the floor between the camera and the wall fills the bottom of the view,
// so on every frame the floor is drawn, early or late, and the opaque stage
// records it at the bottom middle pixel.
#[test]
fn a_floor_through_the_near_plane_is_kept() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let Some(mut frames) = Frames::new(&device, &queue) else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let floor = object(add(
        gpu,
        &mut scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(0., -1.5, 0.))
            * Mat4::from_scale(Vec3::new(40., 0.2, 40.)),
        Mobility::Static,
    ));
    add(
        gpu,
        &mut scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(0., 18.5, -3.))
            * Mat4::from_scale(Vec3::new(40., 40., 0.2)),
        Mobility::Static,
    );
    // The bottom row looks 0.5 rad down, meeting the floor's top (y = -1.4)
    // about 2.6 m ahead, before the wall.
    let bottom = [SIZE[0] / 2, SIZE[1] - 1];
    for frame in 0..3 {
        let [early, late] = frames.frame(gpu, &mut scene, &input());
        assert!(
            objects(&early).contains(&floor) || objects(&late).contains(&floor),
            "frame {frame} draws the floor"
        );
        assert_eq!(
            frames.source_at(gpu, bottom),
            Some(floor),
            "frame {frame} shows the floor"
        );
    }
}

/// A plane at z = -15 facing the camera, x in -6..6 and y in -0.4..0.4, of
/// 0.1 m cells built a column at a time, so each 128-triangle section is
/// 0.8 m square.
fn strips() -> Asset {
    let triangles = (-60..60).flat_map(|column| {
        (-4..4).flat_map(move |row| {
            let p = Vec3::new(column as f32 * 0.1, row as f32 * 0.1, -15.);
            let (x, y) = (Vec3::X * 0.1, Vec3::Y * 0.1);
            [[p, p + x, p + x + y], [p, p + x + y, p + y]]
        })
    });
    let mut asset = test_support::cube();
    asset.meshes = vec![mesh(triangles)];
    asset
}

// Plausible defects: the early section cull never queueing an occluded
// section, or queueing it as the wrong entry or section, the late section
// cull's queue workgroups never dispatched or reading the queue at the
// wrong place, so a section hidden last frame inside an instance visible in
// both frames is never tested again and stays missing, or one hidden
// throughout is still drawn. The oracle is the geometry: from the camera, a
// wall 3 m wide at 10 m hides x in -2.25..2.25 of a plane at 15 m. Every
// section with a triangle outside that shadow is drawn in every frame; a
// section well inside it is culled once the wall has been seen: one whose
// x range, widened on each side by the most a pyramid texel the test reads
// can reach past it (its larger side, 0.8 m, as Bevy's test picks the
// finest level whose 4×4 texels hold its rectangle, and two first-level
// texels), lies within the shadow. Once the wall moves away, the late
// phase draws every section the early phase withheld, the plane itself
// having been drawn early.
#[test]
fn sections_behind_a_wall_are_culled_and_drawn_again_when_it_moves() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let Some(mut frames) = Frames::at(&device, &queue, FINE) else {
        return;
    };
    let input = || input_at(FINE);
    let mut scene = Scene::new(&device, &queue);
    let wall_id = add(
        gpu,
        &mut scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(0., 0., -10.)) * Mat4::from_scale(Vec3::new(3., 4., 0.2)),
        Mobility::Moving,
    );
    let plane = object(add(
        gpu,
        &mut scene,
        strips(),
        Mat4::IDENTITY,
        Mobility::Static,
    ));
    let positions: Vec<Vec3> = strips().meshes[0]
        .vertices
        .iter()
        .map(|vertex| Vec3::from_array(vertex.position))
        .collect();
    // Each section's x range, by its first index.
    let range = |drawn: &DrawInstance| {
        let corners = &positions[drawn.first_index as usize..][..drawn.triangles as usize * 3];
        corners.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.x), hi.max(p.x))
        })
    };
    let sections = |drawn: &[DrawInstance]| -> BTreeSet<u32> {
        drawn
            .iter()
            .filter(|drawn| drawn.object == plane)
            .map(|drawn| drawn.first_index)
            .collect()
    };
    // The shadow of the wall's 3 m on the plane: 1.5 m at 10 m is 2.25 m at
    // 15 m.
    let shadow = 2.25;
    // A first-level pyramid texel's width at 15 m: the view's width there
    // over the level's 256 texels.
    let texel = 2. * 15. * (FOV / 2.).tan() * FINE[0] as f32 / FINE[1] as f32 / 256.;
    let reach = 0.8 + 2. * texel;
    let [first, _] = frames.frame(gpu, &mut scene, &input());
    let every: Vec<DrawInstance> = first
        .iter()
        .filter(|drawn| drawn.object == plane)
        .copied()
        .collect();
    let shown: BTreeSet<u32> = every
        .iter()
        .filter(|drawn| {
            let (lo, hi) = range(drawn);
            lo < -shadow || hi > shadow
        })
        .map(|drawn| drawn.first_index)
        .collect();
    let hidden: BTreeSet<u32> = every
        .iter()
        .filter(|drawn| {
            let (lo, hi) = range(drawn);
            lo - reach > -shadow && hi + reach < shadow
        })
        .map(|drawn| drawn.first_index)
        .collect();
    assert!(
        !hidden.is_empty() && !shown.is_empty(),
        "the plane has both kinds"
    );
    for frame in 1..3 {
        let [early, late] = frames.frame(gpu, &mut scene, &input());
        let drawn: BTreeSet<u32> = sections(&early).union(&sections(&late)).copied().collect();
        assert!(
            shown.is_subset(&drawn),
            "frame {frame} draws every section in view"
        );
        assert!(
            hidden.is_disjoint(&drawn),
            "frame {frame} culls the sections behind the wall"
        );
    }
    let state = InstanceState {
        pose: Mat4::from_translation(Vec3::new(100., 0., -10.)),
        ..*scene.instance(wall_id).unwrap()
    };
    scene.set_instance(&queue, wall_id, state).unwrap();
    let [early, late] = frames.frame(gpu, &mut scene, &input());
    assert!(
        !sections(&early).is_empty(),
        "the plane, visible both frames, is drawn early"
    );
    assert!(
        hidden.is_subset(&sections(&late)),
        "the late phase draws the sections the wall hid"
    );
    let drawn: BTreeSet<u32> = sections(&early).union(&sections(&late)).copied().collect();
    let all: BTreeSet<u32> = every.iter().map(|drawn| drawn.first_index).collect();
    assert_eq!(drawn, all, "every section is drawn once the wall has gone");
}

/// Reads level `level` of the R32Float `texture`.
fn read_level(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    texture: &wgpu::Texture,
    level: u32,
) -> (Vec<f32>, [u32; 2]) {
    let size = texture.size().mip_level_size(level, texture.dimension());
    let row = (size.width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pyramid level"),
        size: u64::from(row * size.height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = buffer.get_mapped_range(..);
    let mut texels = Vec::new();
    for line in mapped.chunks(row as usize) {
        texels.extend_from_slice(bytemuck::cast_slice(&line[..(size.width * 4) as usize]));
    }
    (texels, [size.width, size.height])
}

// Plausible defects in the port: a quadrant, row or level written from
// another's texels (the wave remap, the workgroup memory's indexing, the
// second pass's reads of the first's last level), a level never written, a
// maximum in place of the minimum, or the virtual source sampled at the
// wrong place, so a texel holds a depth nearer than something it covers
// (the test is then not conservative and culls visible geometry) or one
// from elsewhere. The oracle is the depth target and the geometry of the
// pyramid: a texel of level L covers the depth's area under its UV
// rectangle, so it must be no nearer (in reversed-Z, no greater) than the
// farthest depth texel whose centre lies under it, and no farther than the
// farthest within one depth texel of it, which a 2×2 gather can reach. A
// tilted plane fills the view, with boxes before it, so depth varies
// everywhere and nothing is far (zero).
#[test]
fn the_pyramid_holds_the_farthest_depth_each_texel_covers() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    // A size whose pyramid needs both passes (256×256, nine levels) and
    // whose virtual source (512×512) is larger than the depth.
    const SIZE: [u32; 2] = [300, 260];
    let renderer = Renderer::for_test(&device, &queue, SIZE, &settings());
    if !renderer.occlusion_culling_in_effect(&settings()) {
        return;
    }
    let mut frames = Frames {
        renderer,
        output: crate::view::targets::target(&device, "output", SIZE, gbuffer::COLOR),
        size: SIZE,
    };
    let mut scene = Scene::new(&device, &queue);
    add(
        gpu,
        &mut scene,
        test_support::cube(),
        Mat4::from_translation(Vec3::new(0., 0., -30.))
            * Mat4::from_rotation_y(0.6)
            * Mat4::from_rotation_x(0.3)
            * Mat4::from_scale(Vec3::new(200., 200., 1.)),
        Mobility::Static,
    );
    for (x, y, z) in [
        (-3., 1., -8.),
        (2., -1.5, -12.),
        (0.5, 2., -6.),
        (4., 0., -15.),
    ] {
        add(
            gpu,
            &mut scene,
            test_support::cube(),
            Mat4::from_translation(Vec3::new(x, y, z)) * Mat4::from_rotation_z(0.4),
            Mobility::Static,
        );
    }
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1),
        eye: Vec3::ZERO,
    });
    input.camera_cut = true;
    frames.frame(gpu, &mut scene, &input);
    let depth: Vec<f32> = bytemuck::cast_slice(&test_support::read(
        &device,
        &queue,
        frames.renderer.targets().depth.texture(),
        4,
    ))
    .to_vec();
    assert!(depth.iter().all(|&d| d > 0.), "the plane fills the view");
    let pyramid = frames.renderer.test_pyramid().expect("a pyramid").clone();
    assert_eq!(
        [pyramid.width(), pyramid.height(), pyramid.mip_level_count()],
        [256, 256, 9]
    );
    let farthest = |x: std::ops::Range<i64>, y: std::ops::Range<i64>| -> Option<f32> {
        let xs = x.start.max(0)..x.end.min(i64::from(SIZE[0]));
        let ys = y.start.max(0)..y.end.min(i64::from(SIZE[1]));
        ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
            .map(|(x, y)| depth[(y * i64::from(SIZE[0]) + x) as usize])
            .reduce(f32::min)
    };
    for level in 0..pyramid.mip_level_count() {
        let (texels, [width, height]) = read_level(gpu, &pyramid, level);
        for (at, &value) in texels.iter().enumerate() {
            let (x, y) = (at as u32 % width, at as u32 / width);
            // The texel's rectangle in depth texels, [lo, hi) on each axis.
            let lo = [x as f64 / f64::from(width), y as f64 / f64::from(height)];
            let hi = [
                (x + 1) as f64 / f64::from(width),
                (y + 1) as f64 / f64::from(height),
            ];
            let span = |axis: usize| {
                let size = f64::from(SIZE[axis]);
                (lo[axis] * size, hi[axis] * size)
            };
            let ((x0, x1), (y0, y1)) = (span(0), span(1));
            // Depth texels whose centres lie under the rectangle.
            let centred = |a: f64, b: f64| (a - 0.5).ceil() as i64..((b - 0.5).floor() as i64 + 1);
            if let Some(inner) = farthest(centred(x0, x1), centred(y0, y1)) {
                assert!(
                    value <= inner,
                    "level {level} texel ({x}, {y}) holds {value}, nearer than {inner} it covers"
                );
            }
            let reach =
                |a: f64, b: f64| (a - 0.5).floor() as i64 - 1..((b - 0.5).ceil() as i64 + 2);
            let outer = farthest(reach(x0, x1), reach(y0, y1)).unwrap();
            assert!(
                value >= outer,
                "level {level} texel ({x}, {y}) holds {value}, farther than {outer} within reach"
            );
        }
    }
}
