//! The outline on real frames, against an oracle computed here: quads
//! facing the camera placed so their pictures cover chosen pixel
//! rectangles, every surface unlit black over a black backdrop, so the
//! output holds nothing but the outline. A pixel's expected outline is the
//! contract's, found by brute force over every covered pixel: none on a
//! covered pixel; else `width + 1 − d` of the colour of the nearest covered
//! pixel's visibility, d the distance between their centres. Its resources
//! and compilation are tested with the renderer's
//! (`renderer::compile_tests`).
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::content::identity::Identity;
use crate::deformation::{Influence, MeshDeformation};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, Settings};
use crate::{
    Backdrop, Camera, FrameInput, InstanceId, InstanceState, Mobility, Outline, Renderer, Scene,
    View, ViewFrame, ViewOutput, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 48];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The camera's distance from the plane z = 0, and its vertical field of
/// view.
const EYE: f32 = 4.;
const FOV_Y: f32 = 1.;
const RED: [f32; 4] = [1., 0., 0., 1.];
const GREEN: [f32; 4] = [0., 1., 0., 1.];
/// Codes the output may differ from the blend by: its rounding and post's
/// dither of the black picture under it.
const TOLERANCE: i32 = 2;

/// A rectangle of output pixels, `[left, top, right, bottom]` in pixel
/// coordinates (a pixel's centre is at its index plus 0.5).
type Rect = [f32; 4];

fn camera(size: [u32; 2]) -> Camera {
    let eye = Vec3::new(0., 0., EYE);
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: crate::perspective(FOV_Y, size[0] as f32 / size[1] as f32, 0.1),
        eye,
    }
}

/// The world point at depth `z` whose picture is output pixel coordinate
/// `pixel`, under `camera(SIZE)`.
fn unproject(pixel: [f32; 2], z: f32) -> Vec3 {
    let projection = camera(SIZE).projection;
    let ndc = [
        pixel[0] / SIZE[0] as f32 * 2. - 1.,
        1. - pixel[1] / SIZE[1] as f32 * 2.,
    ];
    let depth = EYE - z;
    Vec3::new(
        ndc[0] * depth / projection.x_axis.x,
        ndc[1] * depth / projection.y_axis.y,
        z,
    )
}

/// A quad at depth `z` facing the camera, single-sided, whose picture is
/// `rect`, its U running from 0 at the left to 1 at the right; its
/// vertices' x negated where `mirrored`, for a pose that mirrors them back.
fn quad(rect: Rect, z: f32, mirrored: bool) -> CpuMesh {
    let [left, top, right, bottom] = rect;
    let corners = [
        ([left, bottom], [0., 1.]),
        ([right, bottom], [1., 1.]),
        ([right, top], [1., 0.]),
        ([left, top], [0., 0.]),
    ];
    let sign = if mirrored { -1. } else { 1. };
    CpuMesh {
        vertices: corners
            .iter()
            .map(|&(pixel, uv)| {
                let p = unproject(pixel, z);
                Vertex {
                    position: [p.x * sign, p.y, p.z],
                    normal: [0., 0., 1.],
                    uv,
                    color: [1.; 4],
                    lightmap_uv: [0.; 2],
                    lightmap_bounds: [0., 0., 1., 1.],
                    tangent: [1., 0., 0., 1.],
                }
            })
            .collect(),
        // Counter-clockwise from the camera, mirrored or mirrored back.
        indices: if mirrored {
            vec![0, 2, 1, 0, 3, 2]
        } else {
            vec![0, 1, 2, 0, 2, 3]
        },
        material: 0,
        deformation: MeshDeformation::default(),
    }
}

/// An asset of `mesh` with an unlit black, single-sided material.
fn black(mesh: CpuMesh) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![mesh];
    let material = &mut asset.materials[0];
    material.base = [0., 0., 0., 1.];
    material.unlit = true;
    material.double_sided = false;
    asset
}

/// What the oracle needs of a frame: which pixels the selection covers,
/// and of those, which the oracle cannot call in sight or hidden (within
/// the occlusion test's bracket of an occluder's silhouette) and which are
/// hidden.
struct Expected {
    covered: Vec<bool>,
    hidden: Vec<bool>,
    uncertain: Vec<bool>,
}

impl Expected {
    /// The pixels whose centres lie inside any of `rects`, in sight.
    fn covering(rects: &[Rect]) -> Self {
        let count = (SIZE[0] * SIZE[1]) as usize;
        let mut covered = vec![false; count];
        for (index, covered) in covered.iter_mut().enumerate() {
            let centre = centre(index);
            for rect in rects {
                // The fixture keeps every edge clear of pixel centres, so the
                // raster's rule for a centre on an edge never decides.
                for (value, edge) in [(centre[0], rect[0]), (centre[0], rect[2])]
                    .into_iter()
                    .chain([(centre[1], rect[1]), (centre[1], rect[3])])
                {
                    assert!(
                        (value - edge).abs() > 0.05,
                        "an edge at {edge} meets a centre"
                    );
                }
                *covered |= inside(centre, *rect);
            }
        }
        Self {
            covered,
            hidden: vec![false; count],
            uncertain: vec![false; count],
        }
    }

    /// The covered pixels inside `occluder`'s picture hidden, and those
    /// within one and a half pixels of its edge uncertain.
    fn occluded_by(mut self, occluder: Rect) -> Self {
        for index in 0..self.covered.len() {
            let c = centre(index);
            self.hidden[index] = inside(c, occluder);
            let grown = [
                occluder[0] - 1.5,
                occluder[1] - 1.5,
                occluder[2] + 1.5,
                occluder[3] + 1.5,
            ];
            let shrunk = [
                occluder[0] + 1.5,
                occluder[1] + 1.5,
                occluder[2] - 1.5,
                occluder[3] - 1.5,
            ];
            self.uncertain[index] = inside(c, grown) && !inside(c, shrunk);
        }
        self
    }

    /// Checks `pixels` (RGBA8) against an outline `width` pixels wide,
    /// red in sight and green hidden, and returns how many pixels it
    /// covers at all.
    fn check(&self, pixels: &[u8], width: f32, what: &str) -> usize {
        let mut outlined = 0;
        for (index, pixel) in pixels.chunks_exact(4).enumerate() {
            let [r, g, b, _] = [0, 1, 2, 3].map(|c| i32::from(pixel[c]));
            let at = (index as u32 % SIZE[0], index as u32 / SIZE[0]);
            assert!(b <= TOLERANCE, "{what}: blue at {at:?}");
            let (alpha, colour) = self.expected(index, width);
            let code = |share: f32| (share * 255.).round() as i32;
            match colour {
                Some(hidden) => {
                    let (want_r, want_g) = if hidden {
                        (0, code(alpha))
                    } else {
                        (code(alpha), 0)
                    };
                    assert!(
                        (r - want_r).abs() <= TOLERANCE && (g - want_g).abs() <= TOLERANCE,
                        "{what}: ({r}, {g}) at {at:?}, expected ({want_r}, {want_g})"
                    );
                }
                // In sight or hidden: either colour, at the coverage.
                None => assert!(
                    (r + g - code(alpha)).abs() <= TOLERANCE && r.min(g) <= TOLERANCE,
                    "{what}: ({r}, {g}) at {at:?}, expected {} of one colour",
                    code(alpha)
                ),
            }
            outlined += usize::from(alpha > 0.);
        }
        outlined
    }

    /// Pixel `index`'s outline coverage, and whether its colour is the
    /// hidden one, None where the nearest covered pixels' visibility is
    /// uncertain.
    fn expected(&self, index: usize, width: f32) -> (f32, Option<bool>) {
        if self.covered[index] {
            return (0., Some(false));
        }
        let c = centre(index);
        let mut nearest = f32::INFINITY;
        let mut found: Vec<usize> = Vec::new();
        for other in (0..self.covered.len()).filter(|&other| self.covered[other]) {
            let o = centre(other);
            let d = ((o[0] - c[0]).powi(2) + (o[1] - c[1]).powi(2)).sqrt();
            if d < nearest - 1e-4 {
                nearest = d;
                found.clear();
            }
            if d < nearest + 1e-4 {
                found.push(other);
            }
        }
        let alpha = (width + 1. - nearest).clamp(0., 1.);
        if found.iter().any(|&other| self.uncertain[other]) {
            return (alpha, None);
        }
        // At equal distances, a pixel in sight wins.
        (alpha, Some(found.iter().all(|&other| self.hidden[other])))
    }
}

fn centre(index: usize) -> [f32; 2] {
    [
        (index as u32 % SIZE[0]) as f32 + 0.5,
        (index as u32 / SIZE[0]) as f32 + 0.5,
    ]
}

fn inside(c: [f32; 2], rect: Rect) -> bool {
    c[0] > rect[0] && c[0] < rect[2] && c[1] > rect[1] && c[1] < rect[3]
}

/// A device, a scene and one view of an RGBA8 output, drawing black
/// frames whose outlines are the only colour.
struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: TestRenderer,
    settings: Settings,
    output: wgpu::Texture,
}

fn output(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("outline test output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// The frame's input: the camera over a black backdrop, outlining
/// `outline`.
fn input(outline: Option<Outline>) -> FrameInput {
    let mut input = FrameInput::new(camera(SIZE));
    input.backdrop = Backdrop::Color([0.; 3]);
    input.outline = outline;
    input
}

/// An outline of `selection` `width` pixels wide, red in sight and green
/// hidden.
fn outline(selection: crate::SelectionId, width: f32) -> Outline {
    Outline {
        width,
        visible_color: RED,
        hidden_color: GREEN,
        ..Outline::new(selection)
    }
}

impl Fixture {
    fn new(settings: Settings) -> Option<Self> {
        Self::with_format(settings, FORMAT)
    }

    /// With an output of `format`.
    fn with_format(settings: Settings, format: wgpu::TextureFormat) -> Option<Self> {
        let (device, queue) = test_support::device()?;
        let scene = Scene::new(&device, &queue);
        let renderer = TestRenderer::new(&device, &queue, format, SIZE, 1., &settings).unwrap();
        let output = output(&device, format);
        Some(Self {
            device,
            queue,
            scene,
            renderer,
            settings,
            output,
        })
    }

    /// Without antialiasing or bloom, so each frame stands alone.
    fn plain() -> Option<Self> {
        Self::new(Settings {
            antialiasing: settings::Antialiasing::Off,
            bloom: settings::Bloom::Off,
            ..Settings::default()
        })
    }

    /// `mesh` as an unlit black instance at `pose`.
    fn add(&mut self, mesh: CpuMesh, pose: Mat4) -> InstanceId {
        let (_, instance) =
            test_support::add_static(&self.device, &self.queue, &mut self.scene, black(mesh));
        let state = *self.scene.instance(instance).unwrap();
        self.scene
            .set_instance(&self.queue, instance, InstanceState { pose, ..state })
            .unwrap();
        instance
    }

    /// Renders and finishes the frame `input`, and returns the output.
    fn render(&mut self, input: &FrameInput) -> Vec<u8> {
        let bytes = self.output.format().block_copy_size(None).unwrap();
        let output = self.output.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            input,
            &self.settings,
            &output,
            None,
        );
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
        test_support::read(&self.device, &self.queue, &self.output, bytes)
    }
}

const SELECTED: Rect = [16.25, 14.25, 30.75, 32.75];
const NEIGHBOUR: Rect = [38.25, 14.25, 50.75, 32.75];

// Plausible defects: an outline drawn inside the selection, at a width in
// another unit (scene or render pixels, or a blur radius), with a hard or
// wrongly scaled edge, around an unselected instance, or missing where the
// selection meets the backdrop. The oracle is the contract over the
// fixture's analytic pictures: a selected quad's pixels, an unselected
// neighbour 7.5 pixels away, and an outline 2.5 pixels wide whose last
// pixels are partly covered.
#[test]
fn the_outline_rings_its_selection_at_its_width() {
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    fixture.add(quad(NEIGHBOUR, 0., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[selected]).unwrap();
    let pixels = fixture.render(&input(Some(outline(selection, 2.5))));
    let outlined = Expected::covering(&[SELECTED]).check(&pixels, 2.5, "ring");
    assert!(outlined > 100, "{outlined} pixels outlined");
}

// Plausible defects: a line drawn where two selected instances meet (each
// outlined alone instead of their union), or a mirrored instance's mask
// culled by the unmirrored side so it goes unoutlined. The oracle is the
// contract over the union of two quads that share an edge, one of them
// posed mirrored: no outline between them, one ring around both.
#[test]
fn a_selected_group_has_no_edge_between_its_members() {
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let left = [14.25, 14.25, 31.6, 32.75];
    let right = [31.6, 14.25, 49.75, 32.75];
    let a = fixture.add(quad(left, 0., false), Mat4::IDENTITY);
    let b = fixture.add(
        quad(right, 0., true),
        Mat4::from_scale(Vec3::new(-1., 1., 1.)),
    );
    let selection = fixture.scene.add_selection(&[a, b, a]).unwrap();
    let pixels = fixture.render(&input(Some(outline(selection, 2.))));
    Expected::covering(&[left, right]).check(&pixels, 2., "group");
}

// Plausible defects: the occlusion test inverted, made against the wrong
// depth (the mask's own, the render-size depth unscaled), or a hidden part
// drawn in the visible colour. The oracle is the contract: an unselected
// quad a metre in front of the selected one covers its right part, so the
// outline beside that part is green and beside the rest red, except
// within the bracket's pixel and a half of the occluder's silhouette,
// where either colour is allowed.
#[test]
fn an_occluded_part_takes_the_hidden_colour() {
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    let occluder = [24.25, 8.25, 40.75, 40.75];
    fixture.add(quad(occluder, 1., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[selected]).unwrap();
    let pixels = fixture.render(&input(Some(outline(selection, 2.))));
    let expected = Expected::covering(&[SELECTED]).occluded_by(occluder);
    expected.check(&pixels, 2., "occluded");
    let green = pixels.chunks_exact(4).filter(|p| p[1] > 200).count();
    let red = pixels.chunks_exact(4).filter(|p| p[0] > 200).count();
    assert!(green > 20 && red > 20, "red {red}, green {green}");
}

// Plausible defect: the mask drawn without the material's alpha cut, so a
// leaf, fence or grate is outlined as its whole quad. The oracle is the
// contract over the fixture's masked quad, whose base map cuts out its
// left half (`test_support::half_cut_out`): the outline rings the right
// half alone.
#[test]
fn an_alpha_cut_out_is_outlined_where_it_is_cut() {
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let rect = [20.25, 14.25, 41.25, 32.75];
    let mut asset = test_support::masked(black(quad(rect, 0., false)), 0.5);
    asset.materials[0].base = [0., 0., 0., 1.];
    let (_, cut) =
        test_support::add_static(&fixture.device, &fixture.queue, &mut fixture.scene, asset);
    let selection = fixture.scene.add_selection(&[cut]).unwrap();
    let pixels = fixture.render(&input(Some(outline(selection, 2.))));
    let middle = (rect[0] + rect[2]) / 2.;
    Expected::covering(&[[middle, rect[1], rect[2], rect[3]]]).check(&pixels, 2., "cut");
}

// Plausible defects: the mask drawn at the mesh's bind pose or at the last
// frame's deformation, or culled by its bind-pose bounds. The oracle is the
// contract: a quad bound to one joint, ten metres to the right at bind,
// posed by its joint into the picture and then moved, is outlined where
// each frame's joint puts it and nowhere else.
#[test]
fn a_moving_skinned_mesh_is_outlined_where_it_is_deformed() {
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let rect = [16.25, 14.25, 30.75, 32.75];
    let mut mesh = quad(rect, 0., false);
    let bind = Vec3::new(10., 0., 0.);
    for vertex in &mut mesh.vertices {
        vertex.position = (Vec3::from(vertex.position) + bind).to_array();
    }
    mesh.deformation = MeshDeformation {
        influences: vec![
            Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            4
        ],
        morph_targets: Vec::new(),
    };
    let (device, queue) = (&fixture.device, &fixture.queue);
    let model = fixture.scene.add_asset(device, queue, black(mesh)).unwrap();
    let skinned = fixture
        .scene
        .add_instance(
            device,
            queue,
            InstanceState {
                model: model.model,
                pose: Mat4::IDENTITY,
                visible: true,
                capture_visible: true,
            },
            Mobility::Moving,
        )
        .unwrap();
    let selection = fixture.scene.add_selection(&[skinned]).unwrap();
    // The joint's world offset that moves the quad's picture `pixels`
    // across, at its depth.
    let step = |pixels: f32| unproject([32. + pixels, 24.], 0.).x;
    for shift in [0., 14.] {
        let joint = Mat4::from_translation(Vec3::new(step(shift), 0., 0.) - bind);
        fixture
            .scene
            .set_instance_deformation(&fixture.queue, skinned, &[joint], &[])
            .unwrap();
        let pixels = fixture.render(&input(Some(outline(selection, 2.))));
        let moved = [rect[0] + shift, rect[1], rect[2] + shift, rect[3]];
        Expected::covering(&[moved]).check(&pixels, 2., &format!("moved {shift}"));
    }
}

// Plausible defects: one view's selection, mask or targets shared with
// another view of the same frame, so a split screen outlines in both
// halves what either player targets. The oracle is the contract: two views
// of one frame with selections of their own each outline theirs alone.
#[test]
fn two_views_outline_their_own_selections() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut add = |rect| {
        test_support::add_static(&device, &queue, &mut scene, black(quad(rect, 0., false))).1
    };
    let (first, second) = (add(SELECTED), add(NEIGHBOUR));
    let selections = [first, second].map(|instance| scene.add_selection(&[instance]).unwrap());
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let renderer = Renderer::new(&device);
    let mut views = [(); 2]
        .map(|()| View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &settings).unwrap());
    let outputs = [(); 2].map(|()| output(&device, FORMAT));
    let inputs = selections.map(|selection| input(Some(outline(selection, 2.))));
    let targets = outputs
        .each_ref()
        .map(|output| output.create_view(&Default::default()));
    let mut renderer = renderer;
    let [one, two] = &mut views;
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &mut scene,
        &mut [one, two]
            .into_iter()
            .zip(&inputs)
            .zip(&targets)
            .map(|((view, input), target)| ViewFrame {
                view,
                input,
                settings: &settings,
                output: ViewOutput::Texture(target),
                timing: None,
            })
            .collect::<Vec<_>>(),
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut scene);
    for (output, rect) in outputs.iter().zip([SELECTED, NEIGHBOUR]) {
        let pixels = test_support::read(&device, &queue, output, 4);
        Expected::covering(&[rect]).check(&pixels, 2., "view");
    }
}

// Plausible defects: the width taken in scene or render pixels, or the
// mask drawn at the render size and stretched, so a game rendering at half
// resolution gets an outline twice as wide and stepped. The oracle is the
// contract, the same ring in output pixels as at full resolution, with the
// scene and render sizes half the output's.
#[test]
fn the_width_is_in_output_pixels_at_half_resolution() {
    let Some(mut fixture) = Fixture::new(Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        scene_resolution: settings::SceneResolution::Half,
        ..Settings::default()
    }) else {
        return;
    };
    assert_eq!(fixture.renderer.view.sizes.render, [32, 24]);
    let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[selected]).unwrap();
    let pixels = fixture.render(&input(Some(outline(selection, 3.))));
    Expected::covering(&[SELECTED]).check(&pixels, 3., "half resolution");
}

// Plausible defect: the mask drawn with the camera's jitter, or composited
// before TAA resolves the frame, so a still outline shimmers or ghosts
// frame to frame. The oracle is the contract at rest, frame after frame
// under TAA: the same ring, pixel for pixel, as without it.
#[test]
fn the_outline_holds_still_under_taa() {
    let Some(mut fixture) = Fixture::new(Settings {
        antialiasing: settings::Antialiasing::Taa,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    }) else {
        return;
    };
    let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[selected]).unwrap();
    let expected = Expected::covering(&[SELECTED]);
    for frame in 0..4 {
        let pixels = fixture.render(&input(Some(outline(selection, 2.))));
        expected.check(&pixels, 2., &format!("frame {frame}"));
    }
}

// Plausible defects: a selection that names an instance of another scene
// or a removed one accepted, or a removed instance still outlined. The
// oracle is the contract: refused with `UnknownInstance`, and a selection
// whose only instance was removed draws nothing.
#[test]
fn a_selection_holds_only_live_instances() {
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[selected]).unwrap();
    let mut other = Scene::new(&fixture.device, &fixture.queue);
    let (_, stranger) = test_support::add_static(
        &fixture.device,
        &fixture.queue,
        &mut other,
        black(quad(SELECTED, 0., false)),
    );
    assert!(matches!(
        fixture
            .scene
            .set_selection(selection, &[selected, stranger]),
        Err(crate::SceneError::UnknownInstance)
    ));
    fixture.scene.remove_instance(selected).unwrap();
    let pixels = fixture.render(&input(Some(outline(selection, 2.))));
    assert!(pixels.chunks_exact(4).all(|p| p[0] <= 2 && p[1] <= 2));
    assert!(
        !fixture.renderer.view.outline.holds_targets(),
        "a selection that draws nothing holds its targets"
    );
    fixture.scene.remove_selection(selection).unwrap();
    fixture.render(&input(Some(outline(selection, 2.))));
    assert!(!fixture.renderer.view.outline.holds_targets());
}

// Plausible defect: the in-sight test made against the opaque depth's
// texels as if they were sampled where the unjittered mask is, so a
// selected surface turned from the camera, whose depth changes across a
// pixel by more than the test's margin, reads as hidden behind itself on
// the frames the jitter moves its samples one way, and its outline
// flickers to the hidden colour with nothing in front of it. The oracle is
// the contract: with nothing between the camera and the selection, no
// pixel takes the hidden colour, on any frame under TAA, whichever way the
// quad is turned.
#[test]
fn a_turned_selection_in_sight_stays_in_sight_under_jitter() {
    let Some(mut fixture) = Fixture::new(Settings {
        antialiasing: settings::Antialiasing::Taa,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    }) else {
        return;
    };
    let centre = unproject([24., 24.], 0.);
    let quad = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[quad]).unwrap();
    let state = *fixture.scene.instance(quad).unwrap();
    for turn in [
        glam::Quat::from_rotation_y(0.9),
        glam::Quat::from_rotation_y(-0.9),
        glam::Quat::from_rotation_x(0.9),
        glam::Quat::from_rotation_x(-0.9),
    ] {
        let pose = Mat4::from_translation(centre)
            * Mat4::from_quat(turn)
            * Mat4::from_translation(-centre);
        fixture
            .scene
            .set_instance(&fixture.queue, quad, InstanceState { pose, ..state })
            .unwrap();
        for frame in 0..4 {
            let pixels = fixture.render(&input(Some(outline(selection, 2.))));
            let hidden = pixels.chunks_exact(4).filter(|p| p[1] > 2).count();
            let red = pixels.chunks_exact(4).filter(|p| p[0] > 200).count();
            assert!(
                hidden == 0 && red > 40,
                "turned {turn:?}, frame {frame}: {hidden} hidden, {red} red"
            );
        }
    }
}

// Plausible defect: the colour written in the wrong transfer for the
// output: not decoded for an sRGB attachment, which then encodes it again
// (a mid grey shows far brighter), decoded for an output that stores what
// is written, or left encoded in a linear float output. The oracle is the
// contract, the colour displayed as given, with sRGB's transfer function
// (IEC 61966-2-1) computed here: a mid grey 0.5 at full coverage reads code
// 128 of an RGBA8 output, encoding in the shader or in the attachment, and
// 0.2140 linear in an RGBA16F one.
#[test]
fn the_colour_is_displayed_as_given_on_every_output() {
    let grey = Outline {
        width: 2.,
        visible_color: [0.5, 0.5, 0.5, 1.],
        ..Outline::new(crate::SelectionId::issue(0, 0))
    };
    let linear = ((0.5f32 + 0.055) / 1.055).powf(2.4);
    let expected = Expected::covering(&[SELECTED]);
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba16Float,
    ] {
        let Some(mut fixture) = Fixture::with_format(
            Settings {
                antialiasing: settings::Antialiasing::Off,
                bloom: settings::Bloom::Off,
                ..Settings::default()
            },
            format,
        ) else {
            return;
        };
        let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
        let selection = fixture.scene.add_selection(&[selected]).unwrap();
        let pixels = fixture.render(&input(Some(Outline { selection, ..grey })));
        let bytes = pixels.len() / (SIZE[0] * SIZE[1]) as usize;
        let mut whole = 0;
        for (index, pixel) in pixels.chunks_exact(bytes).enumerate() {
            if expected.expected(index, 2.).0 < 1. {
                continue;
            }
            whole += 1;
            if bytes == 4 {
                assert!(
                    pixel[..3].iter().all(|&code| code.abs_diff(128) <= 2),
                    "{format:?}: {pixel:?}"
                );
            } else {
                let red = test_support::half(pixel);
                assert!(
                    (red - linear).abs() < 0.003,
                    "{format:?}: {red}, not {linear}"
                );
            }
        }
        assert!(whole > 50, "{format:?}: {whole} pixels outlined whole");
    }
}

// Plausible defects: the in-sight test reading a depth without the masked
// sprites the opaque depth gains at Frame item 7, so a foliage card in
// front of a target leaves it drawn in sight, or reading blended sprites
// as occluders, so a smoke puff in front of it turns it hidden. The oracle
// is the contract (alpha-cut surfaces hide the selection, blended ones do
// not): a masked sprite a metre in front of the selected quad's right part
// hides it as the occluder quad does, and a blended one in front of its
// left part hides nothing.
#[test]
fn a_masked_sprite_hides_the_selection_and_a_blended_one_does_not() {
    use crate::sprite::{Sprite, SpriteBatch, SpriteBlend};
    let Some(mut fixture) = Fixture::plain() else {
        return;
    };
    let selected = fixture.add(quad(SELECTED, 0., false), Mat4::IDENTITY);
    let selection = fixture.scene.add_selection(&[selected]).unwrap();
    let image =
        crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([255; 4])));
    let image = fixture
        .scene
        .add_sprite_image(&fixture.device, &fixture.queue, &image)
        .unwrap();
    let masked = [24.25, 8.25, 40.75, 40.75];
    let blended = [8.25, 8.25, 20.75, 40.75];
    for (rect, blend) in [
        (masked, SpriteBlend::Masked { cutoff: 0.5 }),
        (blended, SpriteBlend::Blended),
    ] {
        let batch = fixture
            .scene
            .add_sprite_batch(&SpriteBatch {
                blend,
                ..SpriteBatch::new(image)
            })
            .unwrap();
        let [low, high] = [[rect[0], rect[3]], [rect[2], rect[1]]].map(|p| unproject(p, 1.));
        let sprite = Sprite {
            tint: [0., 0., 0., 1.],
            ..Sprite::new((low + high) / 2., (high - low).truncate())
        };
        fixture
            .scene
            .set_sprites(&fixture.device, &fixture.queue, batch, &[sprite])
            .unwrap();
    }
    let pixels = fixture.render(&input(Some(outline(selection, 2.))));
    Expected::covering(&[SELECTED])
        .occluded_by(masked)
        .check(&pixels, 2., "sprites");
}
