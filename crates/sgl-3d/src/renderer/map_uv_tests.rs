//! Material map coordinates (specs/sgl3d-architecture.md, Material map
//! coordinates) in rendered frames, on both binding tiers: a quad filling
//! an orthographic view, so each pixel's UVs are known, read from the HDR
//! composite. Texel oracles are Vulkan's texel filtering over the labelled
//! image (`magnified`) and its level selection over the checker's chain
//! (`minified`) at coordinates computed by hand from
//! KHR_texture_transform's worked examples; frame oracles are equivalences
//! the contract states: a map through a transform, or on the second set,
//! shades as the same map on a mesh whose first set holds those
//! coordinates.
use super::sampler_tests::{
    MINIFIED, at_lod, bound_and_in_shader, camera, checker, draw_meshes, harness, minified, uv,
};
use super::shader_tests::{Harness, SIZE, frame};
use crate::asset::{
    self, Image, MaterialTexture, Sampler, TextureFilter, TextureWrap, UvSet, UvTransform, Vertex,
};
use crate::test_support::{labelled, magnified};
use crate::{DirectionalLight, InstanceState, MaterialId, Mobility, ModelMesh, PreparedModel};
use glam::Vec3;
use std::f32::consts::FRAC_PI_2;

/// A UV set of the quad: its value at the view's top left and at its
/// bottom right.
type Span = ([f32; 2], [f32; 2]);

/// The quad filling the view in `material`, its first UV set running over
/// `first` and its second over `second`, each vertex with `tangent`.
fn quad(material: MaterialId, first: Span, second: Span, tangent: [f32; 4]) -> ModelMesh {
    // Its corners, as shares of the way from the view's top left to its
    // bottom right.
    let corners = [
        ([-1., -1.], [0., 1.]),
        ([1., -1.], [1., 1.]),
        ([1., 1.], [1., 0.]),
        ([-1., 1.], [0., 0.]),
    ];
    let at = |(from, to): Span, share: [f32; 2]| -> [f32; 2] {
        std::array::from_fn(|axis| from[axis] + share[axis] * (to[axis] - from[axis]))
    };
    ModelMesh {
        vertices: corners
            .map(|([x, y], share)| Vertex {
                position: [x, y, 0.],
                normal: [0., 0., 1.],
                uv: at(first, share),
                uv1: at(second, share),
                color: [1.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent,
                ..bytemuck::Zeroable::zeroed()
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

const WHOLE: Span = ([0.; 2], [1.; 2]);

fn nearest(wrap: TextureWrap) -> Sampler {
    Sampler {
        mag_filter: TextureFilter::Nearest,
        wrap: [wrap; 2],
        ..Sampler::default()
    }
}

/// An unlit material whose red channel is the labelled image through
/// `base` and whose green is the same image through `emission`.
fn two_maps(harness: &mut Harness, base: MaterialTexture, emission: MaterialTexture) -> MaterialId {
    let material = asset::Material {
        base: [1., 0., 0., 1.],
        emissive: [0., 1., 0.],
        unlit: true,
        double_sided: true,
        base_texture: Some(base),
        emissive_texture: Some(emission),
        ..Default::default()
    };
    let (device, queue) = (&harness.device, &harness.queue);
    harness
        .scene
        .add_materials(device, queue, &[material], &[labelled()])
        .unwrap_or_else(|error| panic!("{error}"))[0]
}

/// Asserts each pixel's red and green are the labelled image at
/// `expected`'s coordinates for that pixel, under `samplers`.
fn expect_texels(
    composite: &[[f32; 4]],
    samplers: [Sampler; 2],
    expected: impl Fn([u32; 2]) -> [[f32; 2]; 2],
    label: &str,
) {
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let observed = composite[(y * SIZE[0] + x) as usize];
            let coordinates = expected([x, y]);
            for channel in 0..2 {
                let texel = magnified(coordinates[channel], samplers[channel]);
                assert!(
                    (observed[channel] - texel).abs() < 0.01,
                    "{label}, pixel {x},{y}, channel {channel} at {:?}: {} for {texel}",
                    coordinates[channel],
                    observed[channel]
                );
            }
        }
    }
}

// Plausible defects: every map sampled at the first UV set, or at the
// set another map names; the second set not reaching the fragment (the
// vertex stage not reading its stream, the wrong rectangle); or the Basic
// tier's layout dropping it. One image, two maps: the base map on the
// second set and the emission map on the first, whose coordinates differ
// at every pixel (the second set covers the image's lower middle), then
// the reverse. Each is drawn with both maps in one sampler state (the
// bound sampler) and with the emission map in another (in the shader).
fn each_map_samples_its_own_uv_set(basic: bool) {
    let Some(mut harness) = harness(basic) else {
        return;
    };
    let second: Span = ([0.25, 0.5], [0.75, 1.]);
    for emission_state in [
        nearest(TextureWrap::Repeat),
        nearest(TextureWrap::ClampToEdge),
    ] {
        for (base_set, emission_set) in [(UvSet::Uv1, UvSet::Uv0), (UvSet::Uv0, UvSet::Uv1)] {
            let base = MaterialTexture {
                sampler: nearest(TextureWrap::Repeat),
                uv_set: base_set,
                ..MaterialTexture::new(0)
            };
            let emission = MaterialTexture {
                sampler: emission_state,
                uv_set: emission_set,
                ..MaterialTexture::new(0)
            };
            let material = two_maps(&mut harness, base, emission);
            let mesh = quad(material, WHOLE, second, [0.; 4]);
            let composite = draw_meshes(&mut harness, vec![mesh]);
            let on = |set: UvSet, pixel| match set {
                UvSet::Uv0 => uv(pixel, WHOLE),
                UvSet::Uv1 => uv(pixel, second),
            };
            expect_texels(
                &composite,
                [base.sampler, emission.sampler],
                |pixel| [on(base_set, pixel), on(emission_set, pixel)],
                &format!("base on {base_set:?}, emission on {emission_set:?}"),
            );
        }
    }
}

#[test]
fn each_map_samples_its_own_uv_set_extended() {
    each_map_samples_its_own_uv_set(false);
}

#[test]
fn each_map_samples_its_own_uv_set_basic() {
    each_map_samples_its_own_uv_set(true);
}

// Plausible defects: a transform applied to the image rather than the slot
// (both maps of one image moving alike), to no map or to the wrong one;
// the rotation turned the other way; scale after rotation or offset before
// it; or the transform skipped on the second set. The base map takes
// KHR_texture_transform's first worked example (offset (0, 1), rotation
// pi/2, scale 0.5: "the lower left quadrant of the source image, rotated
// clockwise 90 degrees"), which by its prose reads the image at
// (0.5 v, 1 - 0.5 u), and the emission map its second (offset (0, 1), scale
// (1, -1): a bottom-left origin), which reads (u, 1 - v); then the base
// map takes the first example on the second set.
fn each_map_samples_through_its_own_transform(basic: bool) {
    let Some(mut harness) = harness(basic) else {
        return;
    };
    let quadrant = UvTransform {
        offset: [0., 1.],
        rotation: FRAC_PI_2,
        scale: [0.5, 0.5],
    };
    let flipped = UvTransform {
        offset: [0., 1.],
        rotation: 0.,
        scale: [1., -1.],
    };
    let second: Span = ([0.2, 0.1], [0.8, 0.9]);
    for (base_set, emission_state) in [
        (UvSet::Uv0, nearest(TextureWrap::Repeat)),
        (UvSet::Uv1, nearest(TextureWrap::Repeat)),
        (UvSet::Uv0, nearest(TextureWrap::MirroredRepeat)),
    ] {
        let base = MaterialTexture {
            sampler: nearest(TextureWrap::Repeat),
            uv_set: base_set,
            transform: quadrant,
            ..MaterialTexture::new(0)
        };
        let emission = MaterialTexture {
            sampler: emission_state,
            transform: flipped,
            ..MaterialTexture::new(0)
        };
        let material = two_maps(&mut harness, base, emission);
        let composite = draw_meshes(&mut harness, vec![quad(material, WHOLE, second, [0.; 4])]);
        expect_texels(
            &composite,
            [base.sampler, emission.sampler],
            |pixel| {
                let [u, v] = match base_set {
                    UvSet::Uv0 => uv(pixel, WHOLE),
                    UvSet::Uv1 => uv(pixel, second),
                };
                let [eu, ev] = uv(pixel, WHOLE);
                [[0.5 * v, 1. - 0.5 * u], [eu, 1. - ev]]
            },
            &format!("the quadrant on {base_set:?}"),
        );
    }
}

#[test]
fn each_map_samples_through_its_own_transform_extended() {
    each_map_samples_through_its_own_transform(false);
}

#[test]
fn each_map_samples_through_its_own_transform_basic() {
    each_map_samples_through_its_own_transform(true);
}

// Plausible defect: the level of detail taken from the mesh's coordinates
// rather than the transformed ones, by the bound sampler or by the shader
// path. The quad's coordinates span a quarter of what shows the checker at
// level of detail 2, a texel a pixel (level of detail 0, magnified), and
// the slot scales them by 4: four texels a pixel. The oracle is Vulkan's
// level selection (`minified`) at the transformed coordinates and level of
// detail 2, under nearest mip selection: level 2's texels, which differ
// from level 0's linear magnification at every block edge. Each image is
// drawn as a prepared chain and as a decoded first level, whose chain the
// GPU builds before the frame samples it: at level 2 the checker's blocks
// are 2 texels, each texel one colour, so the built level is the prepared
// one whichever device averages it.
fn a_scaled_map_selects_the_level_its_coordinates_give(basic: bool) {
    let Some(mut harness) = harness(basic) else {
        return;
    };
    let chain = checker();
    let levels = chain.levels(true);
    let sampled = Sampler {
        mag_filter: TextureFilter::Linear,
        min_filter: TextureFilter::Nearest,
        mipmap_filter: Some(TextureFilter::Nearest),
        wrap: [TextureWrap::Repeat; 2],
    };
    let scaled = UvTransform {
        scale: [4., 4.],
        ..UvTransform::IDENTITY
    };
    let transformed = at_lod(2., 1.75 / MINIFIED as f32);
    let authored: Span = (transformed.0.map(|c| c / 4.), transformed.1.map(|c| c / 4.));
    let images = [
        (Image::Rgba8Mips(chain.clone()), "prepared"),
        (Image::Rgba8(levels[0].clone()), "built on the GPU"),
    ];
    for (image, chain_kind) in images {
        let ids = bound_and_in_shader(&mut harness, sampled, scaled, &image);
        for (id, path) in ids.into_iter().zip(["the bound sampler", "the shader"]) {
            let composite = draw_meshes(&mut harness, vec![quad(id, authored, WHOLE, [0.; 4])]);
            let mut compared = 0;
            for y in 0..SIZE[1] {
                for x in 0..SIZE[0] {
                    let at = uv([x, y], transformed);
                    let Some(expected) = minified(levels, at, sampled, 2.) else {
                        continue;
                    };
                    compared += 1;
                    let observed = composite[(y * SIZE[0] + x) as usize];
                    for channel in [0, 2] {
                        assert!(
                            (observed[channel] - expected[channel]).abs() < 0.04,
                            "a {chain_kind} chain through {path} at {at:?}: {observed:?} for {expected:?}"
                        );
                    }
                }
            }
            assert!(compared > 500, "{compared} pixels compared");
        }
    }
}

#[test]
fn a_scaled_map_selects_the_level_its_coordinates_give_extended() {
    a_scaled_map_selects_the_level_its_coordinates_give(false);
}

#[test]
fn a_scaled_map_selects_the_level_its_coordinates_give_basic() {
    a_scaled_map_selects_the_level_its_coordinates_give(true);
}

/// A normal map of one texel, tilted along +U and +V: [204, 153, 230]
/// decodes to (0.6, 0.2, 0.8).
fn tilted() -> Image {
    Image::Rgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([204, 153, 230, 255]),
    ))
}

/// A lit, rough dielectric whose normal map is `normal`, anisotropic by
/// `anisotropy`, which takes its normal map on its authored tangent frame.
fn relief(harness: &mut Harness, normal: MaterialTexture, anisotropy: f32) -> MaterialId {
    let material = asset::Material {
        metallic: 0.,
        roughness: 1.,
        normal_texture: Some(normal),
        anisotropy_strength: anisotropy,
        ..Default::default()
    };
    let (device, queue) = (&harness.device, &harness.queue);
    harness
        .scene
        .add_materials(device, queue, &[material], &[tilted()])
        .unwrap_or_else(|error| panic!("{error}"))[0]
}

/// `mesh` lit by one directional light off both of the quad's axes, alone:
/// the composite.
fn lit(harness: &mut Harness, mesh: ModelMesh) -> Vec<[f32; 4]> {
    lit_from(harness, mesh, Vec3::new(-0.5, -0.3, -0.8))
}

/// `mesh` lit by one directional light shining along `direction`, alone:
/// the composite.
fn lit_from(harness: &mut Harness, mesh: ModelMesh, direction: Vec3) -> Vec<[f32; 4]> {
    let (device, queue) = (&harness.device, &harness.queue);
    let model = harness
        .scene
        .add_model(device, queue, PreparedModel::new(vec![mesh]).unwrap())
        .unwrap();
    let instance = harness
        .scene
        .add_instance(device, queue, InstanceState::new(model), Mobility::Static)
        .unwrap();
    let mut input = frame(camera(), 0.);
    input.directional_lights[0] = Some(DirectionalLight {
        direction,
        color: [1.; 3],
        illuminance: 3.,
        ..Default::default()
    });
    harness.frame(&input);
    let composite = harness.composite();
    harness.scene.remove_instance(instance).unwrap();
    composite
}

/// The greatest difference between two composites' colours.
fn difference(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
        .fold(0., f32::max)
}

/// `span` through `transform`'s matrix, written out from the contract:
/// `u' = Sx cos r u + Sy sin r v + Ox`, `v' = -Sx sin r u + Sy cos r v + Oy`.
fn through(transform: UvTransform, [u, v]: [f32; 2]) -> [f32; 2] {
    let (sin, cos) = transform.rotation.sin_cos();
    let [sx, sy] = transform.scale;
    [
        sx * cos * u + sy * sin * v + transform.offset[0],
        -sx * sin * u + sy * cos * v + transform.offset[1],
    ]
}

/// The quad in `material` whose first UV set is `span`'s through
/// `transform` at each corner, with `tangent`.
fn baked_quad(
    material: MaterialId,
    span: Span,
    transform: UvTransform,
    tangent: [f32; 4],
) -> ModelMesh {
    let mut mesh = quad(material, span, WHOLE, tangent);
    for vertex in &mut mesh.vertices {
        vertex.uv = through(transform, vertex.uv);
    }
    mesh
}

// The frame a normal map's texel is taken on is the frame of the
// coordinates it was sampled at. Plausible defects: the derivative frame
// taken from the mesh's coordinates rather than the slot's (a rotated or
// mirrored map shading as an unrotated one), or from the first set for a
// map on the second. The oracle is the contract's equivalence: a normal
// map through a transform, or on the second set, shades as the same map,
// untransformed, on a mesh whose first set holds those coordinates; and
// the transformed map must differ from the untransformed one, or the case
// tells nothing. The map is one tilted texel, so only the frame matters.
fn a_normal_maps_frame_follows_its_slots_coordinates(basic: bool) {
    let Some(mut harness) = harness(basic) else {
        return;
    };
    let plain = relief(&mut harness, MaterialTexture::new(0), 0.);
    let reference = lit(&mut harness, quad(plain, WHOLE, WHOLE, [0.; 4]));
    for (label, transform) in [
        (
            "rotated, scaled and moved",
            UvTransform {
                offset: [0.1, 0.2],
                rotation: 2.2,
                scale: [2., 3.],
            },
        ),
        (
            "mirrored in U",
            UvTransform {
                offset: [1., 0.],
                rotation: 0.,
                scale: [-1., 1.],
            },
        ),
    ] {
        let slot = MaterialTexture {
            transform,
            ..MaterialTexture::new(0)
        };
        let transformed = relief(&mut harness, slot, 0.);
        let through_slot = lit(&mut harness, quad(transformed, WHOLE, WHOLE, [0.; 4]));
        let in_mesh = lit(&mut harness, baked_quad(plain, WHOLE, transform, [0.; 4]));
        assert!(
            difference(&through_slot, &in_mesh) < 0.01,
            "{label}: {} between the slot's transform and the mesh's coordinates",
            difference(&through_slot, &in_mesh)
        );
        assert!(
            difference(&through_slot, &reference) > 0.1,
            "{label} shades as the untransformed map"
        );
    }
    // On the second set, mirrored there, with a first set that tilts the
    // other way.
    let mirrored: Span = ([1., 0.], [0., 1.]);
    let on_second = relief(
        &mut harness,
        MaterialTexture {
            uv_set: UvSet::Uv1,
            ..MaterialTexture::new(0)
        },
        0.,
    );
    let second = lit(&mut harness, quad(on_second, WHOLE, mirrored, [0.; 4]));
    let first = lit(&mut harness, quad(plain, mirrored, WHOLE, [0.; 4]));
    assert!(
        difference(&second, &first) < 0.01,
        "the second set: {}",
        difference(&second, &first)
    );
    assert!(
        difference(&second, &reference) > 0.1,
        "a map on the second set shades on the first's frame"
    );
}

#[test]
fn a_normal_maps_frame_follows_its_slots_coordinates_extended() {
    a_normal_maps_frame_follows_its_slots_coordinates(false);
}

#[test]
fn a_normal_maps_frame_follows_its_slots_coordinates_basic() {
    a_normal_maps_frame_follows_its_slots_coordinates(true);
}

// An authored tangent frame is glTF's frame of the normal map's set as the
// mesh stores it (T along increasing U, B = (N x T) w toward decreasing V,
// the normal texture's +Y: glTF 2.0 3.7.2.1 and 3.9.3), and shading carries
// it through the slot's transform (the contract #861 generates against).
// Plausible defects: the authored frame used as it is under a rotating or
// mirroring transform, carried by the transform rather than its inverse, or
// carried as if B ran toward increasing V, which turns a rotated frame the
// other way. The quad's U runs along +X and its V along -Y, so its
// conformant tangent is +X with handedness +1 (B = N x T = +Y, toward
// decreasing V). The oracle is the equivalence: the same map, untransformed,
// on a mesh that holds the transformed coordinates and their conformant
// frame. For a rotation r, U' = cos r U + sin r V runs along
// (cos r, -sin r, 0) and V' along (-sin r, -cos r, 0), so B' = -dp/dV' is
// (sin r, cos r, 0) = N x T': handedness +1. For a mirror in U, U' runs
// along -X and B' stays +Y = -(N x T'): handedness -1. A slight anisotropy
// selects the authored frame and leaves the lobe isotropic to within the
// tolerance.
#[test]
fn an_authored_tangent_frame_is_carried_through_its_slots_transform() {
    let Some(mut harness) = harness(false) else {
        return;
    };
    let anisotropy = 1e-3;
    let authored = [1., 0., 0., 1.];
    let plain = relief(&mut harness, MaterialTexture::new(0), anisotropy);
    let reference = lit(&mut harness, quad(plain, WHOLE, WHOLE, authored));
    let rotation = 0.9f32;
    for (label, transform, carried) in [
        (
            "rotated",
            UvTransform {
                rotation,
                ..UvTransform::IDENTITY
            },
            [rotation.cos(), -rotation.sin(), 0., 1.],
        ),
        (
            "mirrored in U",
            UvTransform {
                offset: [1., 0.],
                rotation: 0.,
                scale: [-1., 1.],
            },
            [-1., 0., 0., -1.],
        ),
    ] {
        let slot = MaterialTexture {
            transform,
            ..MaterialTexture::new(0)
        };
        let transformed = relief(&mut harness, slot, anisotropy);
        let through_slot = lit(&mut harness, quad(transformed, WHOLE, WHOLE, authored));
        let in_mesh = lit(&mut harness, baked_quad(plain, WHOLE, transform, carried));
        assert!(
            difference(&through_slot, &in_mesh) < 0.01,
            "{label}: {} between the carried frame and the mesh's own",
            difference(&through_slot, &in_mesh)
        );
        assert!(
            difference(&through_slot, &reference) > 0.1,
            "{label} shades on the uncarried frame"
        );
    }
}

/// `mesh`, built over `WHOLE`, with its first and second UV sets `first`
/// and `second` of its base coordinates.
fn with_sets(
    mut mesh: ModelMesh,
    first: impl Fn([f32; 2]) -> [f32; 2],
    second: impl Fn([f32; 2]) -> [f32; 2],
) -> ModelMesh {
    for vertex in &mut mesh.vertices {
        let base = vertex.uv;
        vertex.uv = first(base);
        vertex.uv1 = second(base);
    }
    mesh
}

// A clearcoat normal map takes the frame of its own coordinates, as the
// base's does, and the authored frame only on the authored frame's set
// (the normal map's, the first without one). Plausible defects: the coat
// taken on the first set's frame though it samples the second, or on the
// authored frame because the relief binding, which holds a bump map where
// there is no normal map, samples the coat's set. The oracle is the
// equivalence: the coat on the second set, a quarter turn of the first,
// shades as the same coat on a first set holding those coordinates, on an
// isotropic material and on an anisotropic one whose bump map is on the
// second set; and either differs from the coat on the base coordinates.
// The light is the coat normal's mirror image of the view on the quarter
// turn's frame, so the coat's highlight shows there and not on the base
// coordinates: on glTF's frame (T along increasing U, B toward decreasing
// V) the tilted texel (0.6, 0.2, 0.8) lies along (0.2, -0.6, 0.8) on that
// frame (U' along -Y, V' along -X, so B' is +X) and along (0.6, 0.2, 0.8)
// on the base one (V along -Y, so B is +Y), and the view is +Z. Extended
// alone binds the coat's maps.
#[test]
fn a_coat_normal_maps_frame_follows_its_own_set() {
    let Some(mut harness) = harness(false) else {
        return;
    };
    let quarter = |[u, v]: [f32; 2]| [v, 1. - u];
    let base = |uv: [f32; 2]| uv;
    let ramp = Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| {
        let height = (10 * x + 5 * y) as u8;
        image::Rgba([height, height, height, 255])
    }));
    let mut coated = |anisotropy: f32, bump: Option<UvSet>, coat: UvSet| {
        let material = asset::Material {
            metallic: 0.,
            roughness: 1.,
            clearcoat: 1.,
            coat_roughness: 0.4,
            anisotropy_strength: anisotropy,
            bump_texture: bump.map(|uv_set| MaterialTexture {
                uv_set,
                ..MaterialTexture::new(1)
            }),
            bump_scale: 0.3,
            coat_normal_texture: Some(MaterialTexture {
                uv_set: coat,
                ..MaterialTexture::new(0)
            }),
            ..Default::default()
        };
        let (device, queue) = (&harness.device, &harness.queue);
        harness
            .scene
            .add_materials(device, queue, &[material], &[tilted(), ramp.clone()])
            .unwrap_or_else(|error| panic!("{error}"))[0]
    };
    let conformant = [1., 0., 0., 1.];
    let isotropic_second = coated(0., None, UvSet::Uv1);
    let isotropic_first = coated(0., None, UvSet::Uv0);
    let anisotropic_second = coated(1e-3, Some(UvSet::Uv1), UvSet::Uv1);
    let bumped_first = coated(0., Some(UvSet::Uv1), UvSet::Uv0);
    let coat = Vec3::new(0.2, -0.6, 0.8).normalize();
    let toward_light = 2. * coat.z * coat - Vec3::Z;
    let mut draw = |material, first: fn([f32; 2]) -> [f32; 2], tangent| {
        let mesh = with_sets(quad(material, WHOLE, WHOLE, tangent), first, quarter);
        lit_from(&mut harness, mesh, -toward_light)
    };
    let reference = draw(isotropic_first, base, [0.; 4]);
    for (label, on_second, on_first, tangent) in [
        ("isotropic", isotropic_second, isotropic_first, [0.; 4]),
        (
            "anisotropic with a bump map",
            anisotropic_second,
            bumped_first,
            conformant,
        ),
    ] {
        let second = draw(on_second, base, tangent);
        let first = draw(on_first, quarter, [0.; 4]);
        assert!(
            difference(&second, &first) < 0.01,
            "{label}: {} between the coat on the second set and its coordinates on the first",
            difference(&second, &first)
        );
        assert!(
            difference(&second, &reference) > 0.05,
            "{label}: the coat on the second set shades as on the base coordinates, within {}",
            difference(&second, &reference)
        );
    }
}
