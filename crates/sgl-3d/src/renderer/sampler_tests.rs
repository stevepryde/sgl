//! Material samplers (specs/sgl3d-architecture.md, Material samplers) in
//! rendered frames, on both binding tiers: an unlit quad filling an
//! orthographic view, so each pixel's UV is known, read from the HDR
//! composite. The oracle is Vulkan's texel filtering and image level
//! selection (Vulkan specification, Texel Filtering and Image Level(s)
//! Selection) over the fixture's known texels; glTF 2.0 defines its sampler
//! modes as OpenGL's, which Vulkan maps one to one. Each state is drawn
//! through group 2's sampler and sampled in the shader, in one material
//! that samples one image in two states.
use super::shader_tests::{Harness, SIZE, frame, settings};
use crate::asset::{self, Image, MaterialTexture, Sampler, TextureFilter, TextureWrap, Vertex};
use crate::test_support::{labelled, linear, magnified};
use crate::{Camera, InstanceState, MaterialId, Mobility, ModelMesh, PreparedModel};
use TextureFilter::{Linear, Nearest};
use TextureWrap::{ClampToEdge, MirroredRepeat, Repeat};
use glam::Vec3;

/// The most sampled textures per shader stage of a device of the Basic
/// binding tier.
const BASIC_SAMPLED_TEXTURES: u32 = 47;

/// A harness on a device of the Basic binding tier, or of the device's own,
/// where it has at least 47 sampled textures a stage; none without a GPU.
fn harness(basic: bool) -> Option<Harness> {
    if !basic {
        return Harness::new(settings());
    }
    let adapter = crate::test_support::adapter()?;
    let mut limits = crate::graphics_device::limits(&adapter);
    assert!(
        limits.max_sampled_textures_per_shader_stage >= BASIC_SAMPLED_TEXTURES,
        "the adapter binds fewer than {BASIC_SAMPLED_TEXTURES} sampled textures a stage"
    );
    limits.max_sampled_textures_per_shader_stage = BASIC_SAMPLED_TEXTURES;
    let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: crate::graphics_device::features(&adapter),
        required_limits: limits,
        ..Default::default()
    }))
    .unwrap();
    let harness = Harness::on(device, settings());
    assert_eq!(
        harness.renderer.binding_tier(),
        crate::graphics_device::BindingTier::Basic
    );
    Some(harness)
}

/// A camera looking down -Z at the quad at z = 0, whose x and y in -1..1
/// fill the view.
fn camera() -> Camera {
    let eye = Vec3::new(0., 0., 5.);
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: glam::camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1),
        eye,
    }
}

/// The UV of pixel `pixel` of a quad whose UVs run from `from` at the
/// view's top left to `to` at its bottom right.
fn uv(pixel: [u32; 2], (from, to): ([f32; 2], [f32; 2])) -> [f32; 2] {
    std::array::from_fn(|axis| {
        let share = (pixel[axis] as f32 + 0.5) / SIZE[axis] as f32;
        from[axis] + share * (to[axis] - from[axis])
    })
}

/// The quad over x `x` and y -1..1 in `material`, its UVs running from
/// `from` at its top left to `to` at its bottom right.
fn quad(material: MaterialId, x: [f32; 2], (from, to): ([f32; 2], [f32; 2])) -> ModelMesh {
    let corners = [
        ([x[0], -1.], [from[0], to[1]]),
        ([x[1], -1.], [to[0], to[1]]),
        ([x[1], 1.], [to[0], from[1]]),
        ([x[0], 1.], [from[0], from[1]]),
    ];
    let vertices = corners
        .map(|([x, y], uv)| Vertex {
            position: [x, y, 0.],
            normal: [0., 0., 1.],
            uv,
            color: [1.; 4],
            lightmap_bounds: [0., 0., 1., 1.],
            ..bytemuck::Zeroable::zeroed()
        })
        .to_vec();
    ModelMesh {
        vertices,
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

/// Draws the quad with UVs `uvs` (`uv`) in `material`, alone, and returns
/// the composite.
fn draw(harness: &mut Harness, material: MaterialId, uvs: ([f32; 2], [f32; 2])) -> Vec<[f32; 4]> {
    draw_meshes(harness, vec![quad(material, [-1., 1.], uvs)])
}

/// Draws `meshes`, alone, and returns the composite.
fn draw_meshes(harness: &mut Harness, meshes: Vec<ModelMesh>) -> Vec<[f32; 4]> {
    let (device, queue) = (&harness.device, &harness.queue);
    let model = harness
        .scene
        .add_model(device, queue, PreparedModel::new(meshes).unwrap())
        .unwrap();
    let instance = harness
        .scene
        .add_instance(device, queue, InstanceState::new(model), Mobility::Static)
        .unwrap();
    harness.frame(&frame(camera(), 0.));
    let composite = harness.composite();
    harness.scene.remove_instance(instance).unwrap();
    composite
}

/// An unlit material whose red channel is `image`'s through its base map in
/// `base` and whose green is its emission map's in `emission`, the same
/// image: base `[1, 0, 0]` and emission `[0, 1, 0]`.
fn two_states(harness: &mut Harness, image: Image, base: Sampler, emission: Sampler) -> MaterialId {
    let material = asset::Material {
        base: [1., 0., 0., 1.],
        emissive: [0., 1., 0.],
        unlit: true,
        double_sided: true,
        base_texture: Some(MaterialTexture {
            image: 0,
            sampler: base,
        }),
        emissive_texture: Some(MaterialTexture {
            image: 0,
            sampler: emission,
        }),
        ..Default::default()
    };
    let (device, queue) = (&harness.device, &harness.queue);
    harness
        .scene
        .add_materials(device, queue, &[material], &[image])
        .unwrap_or_else(|error| panic!("{error}"))[0]
}

fn sampler(mag_filter: TextureFilter, wrap: [TextureWrap; 2]) -> Sampler {
    Sampler {
        mag_filter,
        wrap,
        ..Sampler::default()
    }
}

// Plausible defects: a map sampled with another map's sampler, the bound
// sampler's state, or glTF's default whatever was authored; nearest or
// linear magnification swapped; a wrap mode applied to the wrong axis or
// mapped to another; a map in the shader-sampled state filtered or wrapped
// unlike the GPU's sampler would, or not sampled in the shader at all; or
// the Basic tier's layout dropping any of it. Over UVs -0.5..1.5, 2 texels
// to the pixel's 4 (magnified), pixels fall inside texels, between them and
// past both edges on both axes. The oracle is `magnified` over the known
// texels. Three states, each drawn through the bound sampler (the base
// map's, which wins the tie) and in the shader (the emission map's).
fn magnified_maps_filter_and_wrap_as_each_samples(basic: bool) {
    let Some(mut harness) = harness(basic) else {
        return;
    };
    let a = sampler(Nearest, [Repeat, ClampToEdge]);
    let b = sampler(Linear, [MirroredRepeat, Repeat]);
    let c = sampler(Linear, [ClampToEdge, MirroredRepeat]);
    let uvs = ([-0.5; 2], [1.5; 2]);
    for (base, emission) in [(a, b), (b, c), (c, a)] {
        let material = two_states(&mut harness, labelled(), base, emission);
        let composite = draw(&mut harness, material, uvs);
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                let at = uv([x, y], uvs);
                let observed = composite[(y * SIZE[0] + x) as usize];
                for (channel, state, path) in
                    [(0, base, "the bound sampler"), (1, emission, "the shader")]
                {
                    let expected = magnified(at, state);
                    assert!(
                        (observed[channel] - expected).abs() < 0.01,
                        "{state:?} through {path} at UV {at:?}: {} for {expected}",
                        observed[channel]
                    );
                }
            }
        }
    }
}

#[test]
fn magnified_maps_filter_and_wrap_as_each_samples_extended() {
    magnified_maps_filter_and_wrap_as_each_samples(false);
}

#[test]
fn magnified_maps_filter_and_wrap_as_each_samples_basic() {
    magnified_maps_filter_and_wrap_as_each_samples(true);
}

/// The minified image's side, 128 texels: four of them a pixel over UVs
/// 0..1, at level of detail 2.
const MINIFIED: u32 = 128;

/// The minified image's levels, prepared as a game prepares them
/// (`Rgba8MipChain`), so its every level's texels are known: level 0 a
/// checker of 8 × 8-texel red and blue blocks, each level below it the
/// chain's filtering of the last, its blocks halving, so each level differs
/// from the next.
fn checker() -> asset::Rgba8MipChain {
    asset::Rgba8MipChain::new(image::RgbaImage::from_fn(MINIFIED, MINIFIED, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    }))
    .unwrap()
}

/// Level `level` of `levels` at `uv`, repeating, in linear light: its
/// texel there or, `linear`, the four nearest blended; none where a
/// nearest texel lies within 0.02 texels of its neighbour, which the UV's
/// precision cannot tell apart.
fn level_sample(
    levels: &[image::RgbaImage],
    level: usize,
    uv: [f32; 2],
    linear: bool,
) -> Option<[f32; 3]> {
    let image = &levels[level];
    let size = [image.width() as i32, image.height() as i32];
    let texel = |[x, y]: [i32; 2]| {
        let rgba = image.get_pixel(x.rem_euclid(size[0]) as u32, y.rem_euclid(size[1]) as u32);
        [rgba[0], rgba[1], rgba[2]].map(self::linear)
    };
    let p: [f32; 2] = std::array::from_fn(|axis| uv[axis] * size[axis] as f32);
    if !linear {
        let edge = p.iter().any(|c| (c - c.round()).abs() < 0.02);
        return (!edge).then(|| texel(p.map(|c| c.floor() as i32)));
    }
    let q = p.map(|c| c - 0.5);
    let [i, j] = q.map(|c| c.floor() as i32);
    let [a, b] = q.map(|c| c - c.floor());
    let mix =
        |x: [f32; 3], y: [f32; 3], t: f32| std::array::from_fn(|c| x[c] * (1. - t) + y[c] * t);
    let row = |j| mix(texel([i, j]), texel([i + 1, j]), a);
    Some(mix(row(j), row(j + 1), b))
}

/// `levels` minified at `uv` at level of detail `lod` (above 0) under
/// `sampler`, by Vulkan's image level selection: level 0 without mipmaps,
/// the nearest level with nearest mip selection (half rounding down), else
/// the two nearest blended; each through the minification filter.
fn minified(
    levels: &[image::RgbaImage],
    uv: [f32; 2],
    sampler: Sampler,
    lod: f32,
) -> Option<[f32; 3]> {
    let linear = sampler.min_filter == Linear;
    let last = (levels.len() - 1) as f32;
    let d = lod.min(last);
    match sampler.mipmap_filter {
        None => level_sample(levels, 0, uv, linear),
        Some(Nearest) => level_sample(levels, ((d + 0.5).ceil() - 1.) as usize, uv, linear),
        Some(Linear) => {
            let level = d.floor() as usize;
            let finer = level_sample(levels, level, uv, linear)?;
            let coarser = level_sample(levels, (level + 1).min(last as usize), uv, linear)?;
            let t = d - d.floor();
            Some(std::array::from_fn(|c| {
                finer[c] * (1. - t) + coarser[c] * t
            }))
        }
    }
}

// Plausible defects: a non-mipmapped minification taking the magnification
// filter (a maximum level of detail of 0 makes every sample magnified) or
// sampling a mip level; nearest and linear mip selection swapped, or a level
// off by one; the minification filter ignored within a level; or the
// shader's level of detail unlike the GPU's. The quad shows the checker
// (`checker`) at a level of detail of 2 (UVs 0..1, offset 1.75 texels so
// every other pixel falls between texels across a block's edge), of 1.4
// for nearest mip selection and of 1.5 for linear, whose half-and-half
// blend a GPU that narrows trilinear blending still takes. The oracle is
// `minified` over the chain's known levels, at the nominal level of
// detail; the GPU approximates it, so a blend between levels may stray by
// its share of a few hundredths. Each state is drawn through the bound
// sampler and in the shader (`bound_and_in_shader`).
/// The checker (`image`: its prepared chain, or its first level for the
/// GPU to build the chain from) under `sampled` in two unlit materials whose red and
/// blue show its base map: the first samples it through the bound sampler,
/// as its only map; the second in the shader, outvoted by two maps in
/// glTF's default state that it samples but does not show (its
/// metallic-roughness map, which an unlit material ignores, and an emission
/// map under a black factor).
fn bound_and_in_shader(harness: &mut Harness, sampled: Sampler, image: &Image) -> Vec<MaterialId> {
    let bound = asset::Material {
        base: [1., 0., 1., 1.],
        unlit: true,
        double_sided: true,
        base_texture: Some(MaterialTexture {
            image: 0,
            sampler: sampled,
        }),
        ..Default::default()
    };
    let in_shader = asset::Material {
        mr_texture: Some(MaterialTexture::new(0)),
        emissive_texture: Some(MaterialTexture::new(0)),
        emissive: [0.; 3],
        ..bound.clone()
    };
    let (device, queue) = (&harness.device, &harness.queue);
    harness
        .scene
        .add_materials(
            device,
            queue,
            &[bound, in_shader],
            std::slice::from_ref(image),
        )
        .unwrap_or_else(|error| panic!("{error}"))
}

/// The UVs of a quad filling the view that shows the minified image at
/// level of detail `lod`, from `offset`.
fn at_lod(lod: f32, offset: f32) -> ([f32; 2], [f32; 2]) {
    let span = SIZE[0] as f32 * lod.exp2() / MINIFIED as f32;
    ([offset; 2], [offset + span; 2])
}

fn minified_maps_select_levels_as_each_samples(basic: bool, prepared: bool) {
    let Some(mut harness) = harness(basic) else {
        return;
    };
    let chain = checker();
    let levels = chain.levels(true);
    let image = if prepared {
        Image::Rgba8Mips(chain.clone())
    } else {
        Image::Rgba8(levels[0].clone())
    };
    let offset = 1.75 / MINIFIED as f32;
    let lod = |lod: f32| (lod, at_lod(lod, offset));
    let state = |mag_filter, min_filter, mipmap_filter| Sampler {
        mag_filter,
        min_filter,
        mipmap_filter,
        wrap: [Repeat; 2],
    };
    let cases = [
        (state(Linear, Nearest, None), lod(2.)),
        (state(Nearest, Linear, None), lod(2.)),
        (state(Linear, Nearest, Some(Nearest)), lod(2.)),
        (state(Linear, Nearest, Some(Nearest)), lod(1.4)),
        (state(Nearest, Linear, Some(Linear)), lod(1.5)),
    ];
    for (sampled, (lod, uvs)) in cases {
        let ids = bound_and_in_shader(&mut harness, sampled, &image);
        for (id, path) in ids.into_iter().zip(["the bound sampler", "the shader"]) {
            let composite = draw(&mut harness, id, uvs);
            for y in 0..SIZE[1] {
                for x in 0..SIZE[0] {
                    let at = uv([x, y], uvs);
                    let Some(expected) = minified(levels, at, sampled, lod) else {
                        continue;
                    };
                    let observed = composite[(y * SIZE[0] + x) as usize];
                    for channel in [0, 2] {
                        assert!(
                            (observed[channel] - expected[channel]).abs() < 0.04,
                            "{sampled:?} at level of detail {lod} through {path} at UV {at:?}: {observed:?} for {expected:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn minified_maps_select_levels_as_each_samples_extended() {
    minified_maps_select_levels_as_each_samples(false, true);
}

#[test]
fn minified_maps_select_levels_as_each_samples_basic() {
    minified_maps_select_levels_as_each_samples(true, true);
}

// Plausible defect: a frame drawing a decoded image before its mip chain
// is built (the chain's passes left out of the frame's scene work, or
// submitted after the frame), so a minified map reads unwritten levels,
// black; or the GPU's levels unlike the chain's rule. The same quads as
// above, their image added decoded (`Image::Rgba8`), so the frame builds
// its chain. The oracle is the same `minified` over the known levels of
// the checker: at the levels these read (1 to 3) its blocks are 4, 2 and 1
// texels, each texel's footprint one colour, so a level's texels are pure
// red or blue whichever device averages them.
#[test]
fn minified_maps_sample_the_chain_a_frame_built() {
    minified_maps_select_levels_as_each_samples(false, false);
}

// Plausible defect: the shader-sampled path dropping or doubling the view's
// mip bias, which a frame under TAA takes (-0.5), so a map it samples
// selects another level than the bound sampler does with the same state.
// The oracle is that sampler, through which the GPU applies the bias
// itself (`textureSampleBias`): the two materials of `bound_and_in_shader`
// side by side, each half of the view with the same UVs, under the same
// jitter, at a nominal level of detail of 1.8, which the bias takes to 1.3:
// nearest mip selection reads level 1 with it and level 2 without, and
// linear blends them 0.7 : 0.3 with it and 0.2 : 0.8 without.
#[test]
fn maps_sampled_in_the_shader_take_the_views_mip_bias() {
    let Some(mut harness) = Harness::new(crate::settings::Settings {
        antialiasing: crate::settings::Antialiasing::Taa,
        ..settings()
    }) else {
        return;
    };
    let chain = checker();
    let half = SIZE[0] / 2;
    // Half the view's width shows half the span, so both axes minify alike.
    let (from, to) = at_lod(1.8, 1.75 / MINIFIED as f32);
    let uvs = (from, [from[0] + (to[0] - from[0]) / 2., to[1]]);
    for mipmap_filter in [TextureFilter::Nearest, TextureFilter::Linear] {
        let sampled = Sampler {
            mag_filter: Linear,
            min_filter: Nearest,
            mipmap_filter: Some(mipmap_filter),
            wrap: [Repeat; 2],
        };
        let ids = bound_and_in_shader(&mut harness, sampled, &Image::Rgba8Mips(chain.clone()));
        let composite = draw_meshes(
            &mut harness,
            vec![quad(ids[0], [-1., 0.], uvs), quad(ids[1], [0., 1.], uvs)],
        );
        let mut differing = 0;
        for y in 0..SIZE[1] {
            for x in 0..half {
                let bound = composite[(y * SIZE[0] + x) as usize];
                let in_shader = composite[(y * SIZE[0] + x + half) as usize];
                if [0, 2]
                    .iter()
                    .any(|&c| (bound[c] - in_shader[c]).abs() >= 0.04)
                {
                    differing += 1;
                }
            }
        }
        // A pixel whose nearest texel lies within the UV's precision of its
        // neighbour may read either; a dropped bias changes most pixels.
        assert!(
            differing <= 8,
            "{mipmap_filter:?} mip selection: {differing} of {} pixels differ between the bound sampler and the shader",
            half * SIZE[1]
        );
    }
}
