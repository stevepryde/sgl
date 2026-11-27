//! Sprite batches observed in real frames: where a sprite's corners land in
//! each camera, the atlas cell it shows, what occludes it and what it
//! occludes, the order blended ones compose in, the depth, motion and
//! temporal masks they write, and what an empty batch costs. Every
//! expectation comes from the fixture's geometry and the cameras' matrices,
//! worked by hand, never from the shaders.
use crate::asset::{Image, Sampler, TextureFilter, TextureWrap};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::sprite::{Sprite, SpriteBatch, SpriteBlend, SpriteFacing, SpriteRegion, SpriteSizing};
use crate::{Camera, FrameInput, Scene, SpriteBatchId, test_support};
use glam::{Vec2, Vec3};

const SIZE: [u32; 2] = [64, 64];
const FOV: f32 = 1.;
const NEAR: f32 = 0.1;

fn settings(antialiasing: settings::Antialiasing) -> Settings {
    Settings {
        antialiasing,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    }
}

/// A camera at `eye` looking at `target`, up +Y.
fn camera(eye: Vec3, target: Vec3) -> Camera {
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: crate::perspective(FOV, 1., NEAR),
        eye,
    }
}

/// Where world point `p` lands in `camera`'s frame, in pixels from the top
/// left.
fn pixel(camera: &Camera, p: Vec3) -> Vec2 {
    let clip = camera.projection * camera.view * p.extend(1.);
    let ndc = clip.truncate().truncate() / clip.w;
    Vec2::new(
        (ndc.x * 0.5 + 0.5) * SIZE[0] as f32,
        (0.5 - ndc.y * 0.5) * SIZE[1] as f32,
    )
}

/// A frame of `scene` drawn by one renderer.
struct Frames {
    renderer: TestRenderer,
    output: wgpu::TextureView,
    settings: Settings,
}

impl Frames {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, settings: Settings) -> Self {
        Self {
            renderer: TestRenderer::for_test(device, queue, SIZE, &settings),
            output: crate::view::targets::target(device, "sprite frames", SIZE, gbuffer::COLOR),
            settings,
        }
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        camera: Camera,
    ) {
        let input = FrameInput::new(camera);
        let mut encoder = device.create_command_encoder(&Default::default());
        self.renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &input,
            &self.settings,
            &self.output,
            None,
        );
        queue.submit([encoder.finish()]);
        self.renderer.finish_frame(scene);
    }

    /// The composed HDR frame's RGB at each pixel.
    fn composite(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<[f32; 3]> {
        let texture = self.renderer.targets().composite.texture();
        test_support::read(device, queue, texture, 8)
            .chunks(8)
            .map(|texel| std::array::from_fn(|c| test_support::half(&texel[c * 2..c * 2 + 2])))
            .collect()
    }

    /// The opaque depth at each pixel.
    fn depth(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<f32> {
        let texture = self.renderer.targets().depth.texture();
        test_support::read(device, queue, texture, 4)
            .chunks(4)
            .map(|texel| f32::from_le_bytes(texel.try_into().unwrap()))
            .collect()
    }

    /// The G-buffer's motion (current minus previous UV) at each pixel.
    fn motion(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<[f32; 2]> {
        let texture = self.renderer.targets().motion.texture();
        test_support::read(device, queue, texture, 8)
            .chunks(8)
            .map(|texel| [0, 1].map(|c| test_support::half(&texel[c * 2..c * 2 + 2])))
            .collect()
    }
}

fn at<T: Copy>(texels: &[T], p: Vec2) -> T {
    texels[p.y as usize * SIZE[0] as usize + p.x as usize]
}

/// A `width` by `height` image of `texel(x, y)`.
fn image(width: u32, height: u32, texel: impl Fn(u32, u32) -> [u8; 4]) -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba(texel(x, y))
    }))
}

/// Nearest texels from level 0 alone, clamped: each texel exactly.
const PIXEL_ART: Sampler = Sampler {
    mag_filter: TextureFilter::Nearest,
    min_filter: TextureFilter::Nearest,
    mipmap_filter: None,
    wrap: [TextureWrap::ClampToEdge; 2],
};

/// A batch of `sprites` of an image of `texel`, in a scene of its own.
fn batch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    image: &Image,
    blend: SpriteBlend,
    sprites: &[Sprite],
) -> SpriteBatchId {
    let image = scene.add_sprite_image(device, queue, image).unwrap();
    let id = scene
        .add_sprite_batch(&SpriteBatch {
            sampler: PIXEL_ART,
            blend,
            ..SpriteBatch::new(image)
        })
        .unwrap();
    scene.set_sprites(device, queue, id, sprites).unwrap();
    id
}

const MASKED: SpriteBlend = SpriteBlend::Masked { cutoff: 0.5 };

fn white() -> Image {
    image(1, 1, |_, _| [255; 4])
}

/// Signed distances in pixels of `p` from each edge of the convex polygon
/// `corners`, positive inside whichever way it winds.
fn inside(corners: &[Vec2; 4], p: Vec2) -> f32 {
    let area: f32 = (0..4)
        .map(|i| corners[i].perp_dot(corners[(i + 1) % 4]))
        .sum();
    (0..4)
        .map(|i| {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            (b - a).perp_dot(p - a) / (b - a).length() * area.signum()
        })
        .fold(f32::INFINITY, f32::min)
}

// Plausible defects: the pivot taken from the wrong corner or with y down,
// the rotation clockwise or about the centre, the facing taken from the
// jittered projection or from one camera for every view, size applied
// before the pivot offset, an axis-locked card leaning with the camera's
// pitch, a kept size not scaled by view depth. The oracle: in two cameras facing different ways, each corner is
// the pivot's position plus the camera's right and up (from how the camera
// was built: +X and +Y looking down -Z; -Z and +Y looking down -X) times
// the corner's offset from the pivot, scaled by the size and turned
// anticlockwise, projected by hand; for a card locked to +Y seen from
// above, its up is +Y and its right +Y crossed with the camera's backward
// axis (Godot's BILLBOARD_FIXED_Y); a sprite kept on screen is its size
// times its view depth (three.js's sizeAttenuation off). Every pixel whose centre
// lies a pixel inside that quad holds the masked sprite's depth, every one a
// pixel outside it holds none.
#[test]
fn a_sprites_corners_follow_its_pivot_and_rotation_in_each_camera() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let sprite = Sprite {
        position: Vec3::new(0.3, -0.2, -5.),
        size: Vec2::new(2., 1.),
        pivot: Vec2::new(0.2, 0.9),
        rotation: 0.5,
        ..Sprite::default()
    };
    // A camera 3 m up looking down at the sprite: its backward axis.
    let above = Vec3::new(0., 3., 0.);
    let back = (above - sprite.position).normalize();
    // Each case: the camera, the facing, its right and up in the world, and
    // the sizing with the size the sprite is given and the world size it
    // takes (a kept size is its size at 1 m of view depth: 5 m here).
    let world = (SpriteSizing::World, sprite.size, sprite.size);
    let cameras = [
        (
            camera(Vec3::ZERO, Vec3::new(0., 0., -5.)),
            SpriteFacing::Camera,
            Vec3::X,
            Vec3::Y,
            world,
        ),
        (
            camera(Vec3::new(5., 0., -5.), Vec3::new(0., 0., -5.)),
            SpriteFacing::Camera,
            Vec3::NEG_Z,
            Vec3::Y,
            world,
        ),
        // Upright along +Y whatever the camera's pitch: its right across
        // both the axis and the camera's backward axis, its up the axis.
        (
            camera(above, sprite.position),
            SpriteFacing::Axis(Vec3::Y * 2.),
            Vec3::Y.cross(back).normalize(),
            Vec3::Y,
            world,
        ),
        // Kept on screen: a fifth of the size, five times as far as 1 m.
        (
            camera(Vec3::new(0.3, -0.2, 0.), Vec3::new(0.3, -0.2, -5.)),
            SpriteFacing::Camera,
            Vec3::X,
            Vec3::Y,
            (SpriteSizing::Fixed, sprite.size / 5., sprite.size),
        ),
    ];
    for (camera, facing, right, up, (sizing, given, size)) in cameras {
        let mut scene = Scene::new(&device, &queue);
        let placed = Sprite {
            size: given,
            ..sprite
        };
        let id = batch(&device, &queue, &mut scene, &white(), MASKED, &[placed]);
        let image = scene.sprites.batches.get(id).unwrap().look.image;
        let look = SpriteBatch {
            facing,
            sizing,
            sampler: PIXEL_ART,
            blend: MASKED,
            ..SpriteBatch::new(image)
        };
        scene.set_sprite_batch(&device, &queue, id, &look).unwrap();
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
        frames.render(&device, &queue, &mut scene, camera);
        let depth = frames.depth(&device, &queue);
        let (sin, cos) = sprite.rotation.sin_cos();
        let corners = [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)].map(|(x, y)| {
            let offset = (Vec2::new(x, y) - (sprite.pivot - 0.5)) * size;
            let turned = Vec2::new(
                cos * offset.x - sin * offset.y,
                sin * offset.x + cos * offset.y,
            );
            pixel(&camera, sprite.position + right * turned.x + up * turned.y)
        });
        let (mut covered, mut clear) = (0, 0);
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let distance = inside(&corners, p);
                let drawn = at(&depth, p) > 0.;
                if distance > 1. {
                    assert!(drawn, "{p} lies inside {corners:?}");
                    covered += 1;
                } else if distance < -1. {
                    assert!(!drawn, "{p} lies outside {corners:?}");
                    clear += 1;
                }
            }
        }
        assert!(
            covered > 20 && clear > 20,
            "{covered} inside, {clear} outside"
        );
    }
}

// Plausible defects: the region's v taken up rather than down, the cell
// read from the whole image, the rotation turning the image within the
// quad the wrong way. The oracle: a 2 by 2 sheet whose top right cell is
// red on its top row and green on its bottom row and whose other cells are
// blue; a sprite showing that cell (`SpriteRegion::cell(2, 2, 1)`) shows red
// above its centre and green below upright, red on its left a quarter turn
// anticlockwise, and red below half a turn round, in the composed frame's
// linear values of those texels.
#[test]
fn a_rotating_sprite_shows_its_atlas_cell_turned_with_it() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let sheet = image(4, 4, |x, y| match (x >= 2, y) {
        (true, 0) => [255, 0, 0, 255],
        (true, 1) => [0, 255, 0, 255],
        _ => [0, 0, 255, 255],
    });
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let centre = pixel(&camera, Vec3::new(0., 0., -5.));
    let red = [1., 0., 0.];
    let green = [0., 1., 0.];
    let quarter = std::f32::consts::FRAC_PI_2;
    // The pixel a quarter of the sprite's side from its centre, toward
    // screen up, left and down.
    let up = Vec2::new(0., -5.);
    let left = Vec2::new(-5., 0.);
    for (rotation, toward_red) in [(0., up), (quarter, left), (2. * quarter, -up)] {
        let mut scene = Scene::new(&device, &queue);
        let sprite = Sprite {
            position: Vec3::new(0., 0., -5.),
            size: Vec2::splat(2.),
            rotation,
            region: SpriteRegion::cell(2, 2, 1),
            ..Sprite::default()
        };
        batch(&device, &queue, &mut scene, &sheet, MASKED, &[sprite]);
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
        frames.render(&device, &queue, &mut scene, camera);
        let composite = frames.composite(&device, &queue);
        assert_eq!(
            at(&composite, centre + toward_red),
            red,
            "rotation {rotation}"
        );
        assert_eq!(
            at(&composite, centre - toward_red),
            green,
            "rotation {rotation}"
        );
    }
}

/// An unlit square facing +Z at depth `z` from x = `left` to `right` and y
/// = -2 to 2, of linear colour `base` (its alpha blending it where below 1).
fn square(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    z: f32,
    [left, right]: [f32; 2],
    base: [f32; 4],
) {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(left, -2.), (right, -2.), (right, 2.), (left, 2.)]
            .map(|(x, y)| crate::asset::Vertex {
                uv1: [0.0; 2],
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, y, z],
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset.materials[0].base = base;
    asset.materials[0].unlit = true;
    if base[3] < 1. {
        asset.materials[0].alpha = crate::AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        };
    }
    test_support::add_static(device, queue, scene, asset);
}

// Plausible defects: sprites drawn without the depth test, or masked ones
// writing no depth, or writing it where they are cut out. The oracle: an
// opaque blue square at 3 m covering the left of the view hides a red
// sprite 5 m away there, which shows to its right; the opaque depth is the
// square's (0.1 / 3 under the reversed infinite projection) where it lies
// and the masked sprite's (0.1 / 5) where it shows; a blended sprite there
// writes none.
#[test]
fn opaque_geometry_in_front_hides_a_sprite_and_a_masked_one_writes_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let left = pixel(&camera, Vec3::new(-1., 0., -5.));
    let right = pixel(&camera, Vec3::new(1., 0., -5.));
    let red_image = image(1, 1, |_, _| [255, 0, 0, 255]);
    for blend in [MASKED, SpriteBlend::Blended] {
        let mut scene = Scene::new(&device, &queue);
        square(
            &device,
            &queue,
            &mut scene,
            -3.,
            [-4., 0.],
            [0., 0., 1., 1.],
        );
        let sprite = Sprite::new(Vec3::new(0., 0., -5.), Vec2::splat(4.));
        batch(&device, &queue, &mut scene, &red_image, blend, &[sprite]);
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
        frames.render(&device, &queue, &mut scene, camera);
        let composite = frames.composite(&device, &queue);
        assert_eq!(at(&composite, left), [0., 0., 1.], "{blend:?}");
        assert_eq!(at(&composite, right), [1., 0., 0.], "{blend:?}");
        let depth = frames.depth(&device, &queue);
        assert!((at(&depth, left) - NEAR / 3.).abs() < 1e-6);
        let sprite_depth = if blend == MASKED { NEAR / 5. } else { 0. };
        assert!(
            (at(&depth, right) - sprite_depth).abs() < 1e-6,
            "{blend:?}: {}",
            at(&depth, right)
        );
    }
}

/// `front` over `back` over black, premultiplied, each of alpha 0.5.
fn over(front: [f32; 3], back: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|c| front[c] * 0.5 + 0.5 * back[c] * 0.5)
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 2e-3)
}

// Plausible defects: a batch's sprites drawn in the game's order or front
// to back, or ordered once for every view. The oracle: a green sprite 4 m
// down -Z and a red one 6 m down it, half transparent and given nearest
// first; seen from the origin looking down -Z the green one is in front,
// from 10 m down -Z looking back the red one is, each composed over the
// other by premultiplied alpha.
#[test]
fn overlapping_blended_sprites_compose_back_to_front_in_each_camera() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let half = |position: f32, tint: [f32; 3]| Sprite {
        tint: [tint[0], tint[1], tint[2], 0.5],
        ..Sprite::new(Vec3::new(0., 0., position), Vec2::splat(2.))
    };
    let (red, green) = ([1., 0., 0.], [0., 1., 0.]);
    batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        SpriteBlend::Blended,
        &[half(-4., green), half(-6., red)],
    );
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
    for (camera, front, back) in [
        (camera(Vec3::ZERO, Vec3::NEG_Z), green, red),
        (camera(Vec3::new(0., 0., -10.), Vec3::ZERO), red, green),
    ] {
        frames.render(&device, &queue, &mut scene, camera);
        let composite = frames.composite(&device, &queue);
        let overlap = at(&composite, pixel(&camera, Vec3::new(0., 0., -5.)));
        assert!(near(overlap, over(front, back)), "{overlap:?}");
    }
}

// Plausible defects: blended sprites drawn after every blended surface, or
// before them, whatever their depths. The oracle: a half transparent blue
// surface 5 m away and a half transparent red sprite 4 m or 6 m away
// compose with the nearer one over the farther.
#[test]
fn a_blended_sprite_and_a_blended_surface_compose_by_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let (red, blue) = ([1., 0., 0.], [0., 0., 1.]);
    for (z, front, back) in [(-4., red, blue), (-6., blue, red)] {
        let mut scene = Scene::new(&device, &queue);
        square(
            &device,
            &queue,
            &mut scene,
            -5.,
            [-4., 4.],
            [0., 0., 1., 0.5],
        );
        let sprite = Sprite {
            tint: [1., 0., 0., 0.5],
            ..Sprite::new(Vec3::new(0., 0., z), Vec2::splat(2.))
        };
        batch(
            &device,
            &queue,
            &mut scene,
            &white(),
            SpriteBlend::Blended,
            &[sprite],
        );
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
        frames.render(&device, &queue, &mut scene, camera);
        let composite = frames.composite(&device, &queue);
        let overlap = at(&composite, pixel(&camera, Vec3::new(0., 0., z)));
        assert!(
            near(overlap, over(front, back)),
            "sprite at {z}: {overlap:?}"
        );
    }
}

// Plausible defects: masked sprites writing no motion, the background's, or
// motion measured with the jittered projection. The oracle: the camera
// steps 0.2 m along +X between two frames of a still masked sprite 5 m away
// in front of an opaque square 8 m away; the motion at the sprite is the
// change of where its centre projects (current minus previous UV, worked
// from the two cameras' matrices), and differs from the square's, which is
// farther and so moves less.
#[test]
fn a_masked_sprite_writes_the_motion_of_its_own_point() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    square(
        &device,
        &queue,
        &mut scene,
        -8.,
        [-8., 8.],
        [0., 0., 1., 1.],
    );
    let centre = Vec3::new(0., 0., -5.);
    batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        MASKED,
        &[Sprite::new(centre, Vec2::splat(2.))],
    );
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
    let first = camera(Vec3::ZERO, Vec3::NEG_Z);
    let second = camera(Vec3::new(0.2, 0., 0.), Vec3::new(0.2, 0., -1.));
    frames.render(&device, &queue, &mut scene, first);
    frames.render(&device, &queue, &mut scene, second);
    let motion = frames.motion(&device, &queue);
    let uv = |camera: &Camera, p| pixel(camera, p) / SIZE[0] as f32;
    let expected = uv(&second, centre) - uv(&first, centre);
    let drawn = at(&motion, pixel(&second, centre));
    assert!(
        (drawn[0] - expected.x).abs() < 1e-3 && drawn[1].abs() < 1e-3,
        "{drawn:?} against {expected}"
    );
    let behind = Vec3::new(3., 0., -8.);
    let farther = uv(&second, behind) - uv(&first, behind);
    assert!(
        (farther.x - expected.x).abs() > 5e-3,
        "the depths are told apart"
    );
}

/// The reactive and transparency-and-composition masks at `p`, 0..=255.
fn masks(frames: &Frames, device: &wgpu::Device, queue: &wgpu::Queue, p: Vec2) -> [u8; 2] {
    let masks = frames
        .renderer
        .targets()
        .temporal_masks()
        .expect("a TAA frame with sprites holds the masks");
    masks
        .each_ref()
        .map(|mask| at(&test_support::read(device, queue, mask.texture(), 1), p))
}

// Plausible defects: masked sprites marking every frame (no accumulation on
// a still foliage card) or none (a moved sprite smears); blended ones
// marking nothing. The oracle: AMD's rules as the architecture states them
// (Frame item 9): a masked batch marks transparency and composition 1 in a
// frame after its sprites changed and 0 in one after they did not, and
// leaves reactivity; a blended sprite of alpha 0.6 marks both by its alpha.
#[test]
fn sprites_mark_the_temporal_masks_as_they_change() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let left = Vec3::new(-1.2, 0., -5.);
    let right = Vec3::new(1.2, 0., -5.);
    let mut scene = Scene::new(&device, &queue);
    let masked = batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        MASKED,
        &[Sprite::new(left, Vec2::splat(2.))],
    );
    let blended = Sprite {
        tint: [1., 1., 1., 0.6],
        ..Sprite::new(right, Vec2::splat(2.))
    };
    batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        SpriteBlend::Blended,
        &[blended],
    );
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
    let read =
        |frames: &Frames| [left, right].map(|p| masks(frames, &device, &queue, pixel(&camera, p)));
    frames.render(&device, &queue, &mut scene, camera);
    let added = read(&frames);
    frames.render(&device, &queue, &mut scene, camera);
    let still = read(&frames);
    scene
        .set_sprites(
            &device,
            &queue,
            masked,
            &[Sprite::new(left + Vec3::Y * 0.1, Vec2::splat(2.))],
        )
        .unwrap();
    frames.render(&device, &queue, &mut scene, camera);
    let moved = read(&frames);
    assert_eq!(added[0], [0, 255], "a new masked batch");
    assert_eq!(still[0], [0, 0], "a still masked batch");
    assert_eq!(moved[0], [0, 255], "a moved masked batch");
    // 0.6 of 255, each way rounded.
    for frame in [added, still, moved] {
        let [reactive, composition] = frame[1];
        assert!(
            reactive.abs_diff(153) <= 1 && composition.abs_diff(153) <= 1,
            "{frame:?}"
        );
    }
}

// Plausible defects (#877): a TAA frame binds the temporal masks, and TAA
// reads them, for a masked batch that did not change, as every frame did
// while the scene held a sprite; or binds none in the frame a masked batch
// changed, or while a blended batch is drawn; or for a blended batch out of
// the view. The oracle is the register's F-15 with M-21 and M-22: a masked
// batch marks the masks in the frames after it is added or changes, a
// blended batch in every frame the view draws it, and nothing else does.
#[test]
fn sprites_bind_the_masks_only_in_frames_they_mark() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let ahead = camera(Vec3::ZERO, Vec3::NEG_Z);
    let behind = camera(Vec3::ZERO, Vec3::Z);
    let left = Vec3::new(-1.2, 0., -5.);
    let mut scene = Scene::new(&device, &queue);
    let masked = batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        MASKED,
        &[Sprite::new(left, Vec2::splat(2.))],
    );
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
    let mut bound = |scene: &mut Scene, camera| {
        frames.render(&device, &queue, scene, camera);
        frames.renderer.targets().temporal_masks().is_some()
    };
    assert!(bound(&mut scene, ahead), "a masked batch added");
    assert!(!bound(&mut scene, ahead), "the masked batch still");
    let moved = [Sprite::new(left + Vec3::Y * 0.1, Vec2::splat(2.))];
    scene.set_sprites(&device, &queue, masked, &moved).unwrap();
    assert!(bound(&mut scene, ahead), "the masked batch moved");
    assert!(!bound(&mut scene, ahead), "the masked batch still again");
    batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        SpriteBlend::Blended,
        &[Sprite::new(Vec3::new(1.2, 0., -5.), Vec2::splat(2.))],
    );
    assert!(bound(&mut scene, ahead), "a blended batch in view");
    assert!(bound(&mut scene, ahead), "the blended batch still in view");
    assert!(!bound(&mut scene, behind), "both behind the view");
}

// Plausible defect: an empty batch, or a scene's sprite image with no
// batch to draw, creating a pipeline, a buffer, a pass or an upload in a
// frame. The oracle: wgpu's own count of the device's pipelines and shader
// modules and the library's count of buffers it created and bytes it
// uploaded over two frames, equal with and without an empty batch, and
// with and without a sprite image (beyond the image's own upload). The
// device is waited on after each frame, so the frame's geometry statistics
// readback has arrived and the next frame reuses its buffer: unwaited, the
// second frame makes a second buffer or not as the readback happens to
// have arrived, and the counts differ by chance.
#[test]
fn an_empty_batch_costs_nothing() {
    let run = |image: bool, empty: bool| {
        let (instance, device, queue) = test_support::counted_device()?;
        let mut scene = Scene::new(&device, &queue);
        if image {
            let image = scene.add_sprite_image(&device, &queue, &white()).unwrap();
            if empty {
                scene.add_sprite_batch(&SpriteBatch::new(image)).unwrap();
            }
        }
        let started = crate::counters::snapshot();
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
        let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
        for _ in 0..2 {
            frames.render(&device, &queue, &mut scene, camera);
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        }
        let counted = crate::counters::snapshot().since(&started);
        let hub = instance.generate_report().unwrap();
        let hub = hub.hub_report();
        Some((
            hub.render_pipelines.num_allocated + hub.compute_pipelines.num_allocated,
            hub.shader_modules.num_allocated,
            hub.buffers.num_allocated,
            counted.buffers_created,
            counted.uploaded_bytes(),
        ))
    };
    let (Some(none), Some(image), Some(empty)) =
        (run(false, false), run(true, false), run(true, true))
    else {
        return;
    };
    assert_eq!(image, none, "a sprite image no batch shows");
    assert_eq!(empty, none, "an empty batch");
}

// Plausible defects: a removed masked batch, or one whose look changed,
// leaving neither depth nor a mark where it stood, so its outline stays in
// TAA's history; or re-arming TAA across the whole view for it, so every
// still surface shimmers for a change that touched a few pixels. The
// oracle: the architecture's rule (Frame item 9, the register's M-21): in
// the frame after the edit the quads the batch last drew are marked 1,
// cut-out texels included, a pixel far from them is not, and the scene
// raises no view-wide signal; a frame later nothing is marked and nothing
// is kept. The looks change alone, the sprites staying: an image whose
// every texel is cut out (the new look draws nothing), and a facing locked
// along the view's axis, which turns the quad edge-on, so only the old
// look's own placement marks where it stood.
#[test]
fn removing_or_restyling_a_masked_batch_marks_what_it_drew_and_nothing_else() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let centre = Vec3::new(-1.5, 0., -5.);
    // Opaque on its left half, cut out on its right.
    let half = image(2, 1, |x, _| [255, 255, 255, if x == 0 { 255 } else { 0 }]);
    let cut_out = centre + Vec3::new(0.3, 0., 0.);
    let corner = centre + Vec3::new(-0.4, 0.4, 0.);
    let far = Vec3::new(1.5, 0., -5.);
    type Edit = fn(&mut Scene, &wgpu::Device, &wgpu::Queue, SpriteBatchId);
    let edits: [(&str, Edit); 3] = [
        ("removed", |scene, _, _, id| {
            scene.remove_sprite_batch(id).unwrap()
        }),
        ("cut out", |scene, device, queue, id| {
            let clear = image(1, 1, |_, _| [255, 255, 255, 0]);
            let look = SpriteBatch {
                image: scene.add_sprite_image(device, queue, &clear).unwrap(),
                ..scene.sprites.batches.get(id).unwrap().look
            };
            scene.set_sprite_batch(device, queue, id, &look).unwrap();
        }),
        ("turned edge-on", |scene, device, queue, id| {
            let look = SpriteBatch {
                facing: SpriteFacing::Axis(Vec3::Z),
                ..scene.sprites.batches.get(id).unwrap().look
            };
            scene.set_sprite_batch(device, queue, id, &look).unwrap();
        }),
    ];
    for (name, edit) in edits {
        let mut scene = Scene::new(&device, &queue);
        let id = batch(
            &device,
            &queue,
            &mut scene,
            &half,
            MASKED,
            &[Sprite::new(centre, Vec2::splat(1.))],
        );
        // Another batch keeps the masks held whatever the edit leaves.
        batch(
            &device,
            &queue,
            &mut scene,
            &white(),
            SpriteBlend::Blended,
            &[Sprite::new(Vec3::new(0., 2., -5.), Vec2::splat(0.5))],
        );
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
        frames.render(&device, &queue, &mut scene, camera);
        frames.render(&device, &queue, &mut scene, camera);
        edit(&mut scene, &device, &queue, id);
        assert!(
            !scene.rigid_objects_changed(),
            "{name}: no view-wide signal"
        );
        frames.render(&device, &queue, &mut scene, camera);
        let marks = composition(&frames, &device, &queue);
        for p in [centre, cut_out, corner] {
            assert_eq!(at(&marks, pixel(&camera, p)), 255, "{name}: {p}");
        }
        assert_eq!(at(&marks, pixel(&camera, far)), 0, "{name}: far from it");
        frames.render(&device, &queue, &mut scene, camera);
        let marks = composition(&frames, &device, &queue);
        assert_eq!(
            at(&marks, pixel(&camera, corner)),
            0,
            "{name}: a frame later"
        );
        assert!(scene.sprites.departed.is_empty(), "{name}: freed");
    }
}

/// The transparency-and-composition mask at each pixel, 0..=255, of the
/// last frame.
fn composition(frames: &Frames, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
    let masks = frames.renderer.targets().temporal_masks().unwrap();
    test_support::read(device, queue, masks[1].texture(), 1)
}

// Plausible defects: a changed masked batch marking only where its sprites
// stand now, so what it left where it stood keeps its history: a sprite that
// jumped, a batch cleared, or one whose sprites left the view. The oracle:
// the architecture's rule (Frame item 9, M-21): in the frame after the
// edit, the last submitted frame's sprite's centre, which nothing covers
// now, is marked 1; in the frame after that, which no edit preceded,
// nothing is.
#[test]
fn a_changed_masked_batch_marks_where_its_sprites_stood() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let left = Vec3::new(-1.5, 0., -5.);
    let sprite = |at| [Sprite::new(at, Vec2::splat(1.))];
    for (edit, name) in [
        (Some(Vec3::new(1.5, 0., -5.)), "jumped"),
        (None, "cleared"),
        (Some(Vec3::new(40., 0., -5.)), "left the view"),
    ] {
        let mut scene = Scene::new(&device, &queue);
        let id = batch(&device, &queue, &mut scene, &white(), MASKED, &sprite(left));
        let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
        frames.render(&device, &queue, &mut scene, camera);
        frames.render(&device, &queue, &mut scene, camera);
        let placed = edit.map(sprite);
        scene
            .set_sprites(
                &device,
                &queue,
                id,
                placed.as_ref().map_or(&[][..], |p| &p[..]),
            )
            .unwrap();
        frames.render(&device, &queue, &mut scene, camera);
        let marked = at(&composition(&frames, &device, &queue), pixel(&camera, left));
        frames.render(&device, &queue, &mut scene, camera);
        let after = frames
            .renderer
            .targets()
            .temporal_masks()
            .map(|_| at(&composition(&frames, &device, &queue), pixel(&camera, left)));
        assert_eq!(marked, 255, "{name}: where it stood");
        assert!(
            after.is_none_or(|value| value == 0),
            "{name}: a frame later"
        );
    }
}

// Plausible defect: an edit made after a frame's submission and before its
// `finish_frame` (which belongs to the next frame) overwriting the sprites
// that frame drew, so the next frame cannot mark where they stood. The
// oracle: the rule of Frame item 9 with the edit window of "Edits and
// frames": the frame after such an edit marks the submitted frame's
// sprite's centre 1.
#[test]
fn an_edit_before_finish_frame_keeps_what_the_frame_drew_for_the_marks() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let camera = camera(Vec3::ZERO, Vec3::NEG_Z);
    let [first, second, third] = [-1.5, 0., 1.5].map(|x| Vec3::new(x, 0., -5.));
    let sprite = |at| [Sprite::new(at, Vec2::splat(1.))];
    let mut scene = Scene::new(&device, &queue);
    let id = batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        MASKED,
        &sprite(first),
    );
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
    frames.render(&device, &queue, &mut scene, camera);
    scene
        .set_sprites(&device, &queue, id, &sprite(second))
        .unwrap();
    // A frame drawing `second`, submitted; the edit to `third` follows its
    // submission, before its finish.
    let input = FrameInput::new(camera);
    let mut encoder = device.create_command_encoder(&Default::default());
    frames.renderer.render(
        &device,
        &queue,
        &mut encoder,
        &mut scene,
        &input,
        &frames.settings,
        &frames.output,
        None,
    );
    queue.submit([encoder.finish()]);
    scene
        .set_sprites(&device, &queue, id, &sprite(third))
        .unwrap();
    frames.renderer.finish_frame(&mut scene);
    frames.render(&device, &queue, &mut scene, camera);
    let composition = composition(&frames, &device, &queue);
    assert_eq!(at(&composition, pixel(&camera, second)), 255);
    assert_eq!(at(&composition, pixel(&camera, first)), 0);
}

// Plausible defect: cards locked to an axis ordered by their position along
// their normal, which is right only while every card lies beyond the eye;
// under a camera looking down steeply a ray meets the nearer of two cards
// first though its position along the normal is the smaller. The oracle:
// worked by hand (the reviewer's case): the eye 10 m up looking at
// (0, 0, -2), cards upright along +Y, 2 by 6 m from the ground, at z = 1
// (red) and z = 2 (green), half transparent; the cards' normal is +Z, the
// ray to the green card's point (0, 0.5, 2) crosses z = 1 at y = 5.25,
// inside the red card, so red is composed over green there.
#[test]
fn axis_locked_cards_compose_by_distance_under_a_steep_camera() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let card = |z: f32, tint: [f32; 3]| Sprite {
        pivot: Vec2::new(0.5, 0.),
        tint: [tint[0], tint[1], tint[2], 0.5],
        ..Sprite::new(Vec3::new(0., 0., z), Vec2::new(2., 6.))
    };
    let (red, green) = ([1., 0., 0.], [0., 1., 0.]);
    let id = batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        SpriteBlend::Blended,
        &[card(1., red), card(2., green)],
    );
    let image = scene.sprites.batches.get(id).unwrap().look.image;
    scene
        .set_sprite_batch(
            &device,
            &queue,
            id,
            &SpriteBatch {
                facing: SpriteFacing::Axis(Vec3::Y),
                sampler: PIXEL_ART,
                ..SpriteBatch::new(image)
            },
        )
        .unwrap();
    let camera = camera(Vec3::new(0., 10., 0.), Vec3::new(0., 0., -2.));
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
    frames.render(&device, &queue, &mut scene, camera);
    let composite = frames.composite(&device, &queue);
    let overlap = at(&composite, pixel(&camera, Vec3::new(0., 0.5, 2.)));
    assert!(near(overlap, over(red, green)), "{overlap:?}");
}

#[test]
fn additive_sprites_sum_their_light_without_writing_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let sprites = [
        Sprite {
            tint: [0.6, 0.0, 0.0, 0.5],
            ..Sprite::new(Vec3::new(0.0, 0.0, -5.0), Vec2::splat(2.0))
        },
        Sprite {
            tint: [0.4, 0.8, 0.0, 0.5],
            ..Sprite::new(Vec3::new(0.0, 0.0, -4.5), Vec2::splat(2.0))
        },
    ];
    let _id = batch(
        &device,
        &queue,
        &mut scene,
        &white(),
        SpriteBlend::Additive,
        &sprites,
    );
    let cam = camera(Vec3::ZERO, Vec3::new(0.0, 0.0, -5.0));

    // Render without temporal masks (ADDITIVE_FS_ENTRY) and check exact additive sum + no depth write.
    let mut frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Off));
    frames.render(&device, &queue, &mut scene, cam);
    let composite = frames.composite(&device, &queue);
    let depth = frames.depth(&device, &queue);
    let centre = pixel(&cam, Vec3::new(0.0, 0.0, -5.0));
    let expected = [0.6 * 0.5 + 0.4 * 0.5, 0.8 * 0.5, 0.0];
    let actual = at(&composite, centre);
    assert!(
        near(actual, expected),
        "expected additive sum {expected:?}, got {actual:?}"
    );
    assert_eq!(
        at(&depth, centre),
        0.0,
        "additive sprites must not write depth"
    );

    // Also exercise the temporal-masked additive pipeline (ADDITIVE_MASKED_FS_ENTRY).
    let mut taa_frames = Frames::new(&device, &queue, settings(settings::Antialiasing::Taa));
    taa_frames.render(&device, &queue, &mut scene, cam);
    let taa_composite = taa_frames.composite(&device, &queue);
    let taa_actual = at(&taa_composite, centre);
    assert!(
        near(taa_actual, expected),
        "expected TAA additive sum {expected:?}, got {taa_actual:?}"
    );
}
