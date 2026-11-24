//! Actual raster derivatives and actual BLAS hits, with a glTF tangent-space oracle.
//! Normal texture [204,153,230] decodes to direction [153,51,205]. On the
//! authored XY plane, +U is +X (+U is -X in the mirrored islands), V runs
//! down the plane, along -Y, as glTF stores an upright picture, and the front
//! normal is +Z. A texel's +X lies along increasing U and its +Y toward
//! decreasing V, up the image: +Y (glTF 2.0 3.9.3). The authored tangents are
//! conformant: T along increasing U and B = (N x T) w toward decreasing V
//! (3.7.2.1), so +X with handedness +1, and -X with -1 where U is mirrored.
//! Back views reverse the complete lighting normal.
//! This catches wrong derivative signs, a bitangent toward increasing V,
//! mirrored UV handedness and back-side reversal; merely making raster and
//! compute agree cannot satisfy the oracle.
//! Authority: glTF2.0 sections 3.9.3/3.9.5 and material.normalTexture;
//! Three185.1 TangentUtils.js and WGSLNodeBuilder.js (dFdy -> -dpdy).
//! MaterialNode.js selects normalMap before bumpMap, a rule the scene's maps
//! in effect own (`scene::materials::maps`, its `tier_tests`): the plane's
//! varying bump texture shades only where the material has no normal map.
//! With the diagnostic disabling the normal-map stage, a normal-mapped
//! surface keeps its geometry normal. A clearcoat normal map, beside the base
//! one, takes the base map's frame (KHR_materials_clearcoat; the Khronos glTF
//! Sample Renderer 0686eb2 material_info.glsl 196–207): texture [64,192,220]
//! decodes to [-127,129,185], its X and Y at scale 0.5.
use super::normal_probe::{self, Observe, Probe};
use super::*;
use crate::asset::{Asset, CpuMesh, Material, Vertex};
use crate::shading::bind::group2::MaterialMap;
use crate::shading::material::MaterialMaps;
use glam::{Mat4, Vec3};

const CENTERS: [f32; 4] = [-3., -1., 1., 3.];
const PIXEL: [u8; 4] = [204, 153, 230, 255];
const COAT_PIXEL: [u8; 4] = [64, 192, 220, 255];

// The fixture's orthographic views: cases 2/3 view the same +Z face from -Z
// (180 degrees around Y), reversing screen X only, about their centres.
const VERTEX: &str = r#"
@vertex fn fixture_vs(v:FixtureVertex)->Fragment {
 var o:Fragment;
 o.clip=vec4((v.color.x+(v.position.x-v.color.x)*v.color.y)/4.,v.position.y,0.,1.);
 let model=objects[0].model;
 o.world=(model*vec4(v.position,1.)).xyz;o.normal=object_normal(model,v.normal);o.tangent=object_tangent(model,v.tangent,v.normal);o.uv=v.uv;o.color=v.color;
 return o;
}
"#;

/// The four planes, their maps' coordinates as the module header says, or
/// where `stretched`, V half as dense as U and U leaning along V: U still
/// runs along +X (-X mirrored) at fixed V, so its ∂p/∂U is +X (-X), and V
/// still runs down the plane.
fn plane_asset(has_normal_map: bool, stretched: bool) -> Asset {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (case, center) in CENTERS.into_iter().enumerate() {
        let first = vertices.len() as u32;
        for [x, y] in [[-0.75, -0.75], [0.75, -0.75], [0.75, 0.75], [-0.75, 0.75]] {
            let (u, v) = if stretched {
                (x / 1.5 + 0.5 + 0.2 * y, 0.5 - y / 3.)
            } else {
                (x / 1.5 + 0.5, 0.5 - y / 1.5)
            };
            vertices.push(Vertex {
                uv1: [0.0; 2],
                tangent: [0.0; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [center + x, y, 0.],
                normal: [0., 0., 1.],
                uv: [if case % 2 == 0 { u } else { 1. - u }, v],
                // Fixture orthographic camera: cases 2/3 view the same +Z face
                // from -Z (180 degrees around Y), reversing screen X only.
                color: [center, if case < 2 { 1. } else { -1. }, 0., 1.],
            });
        }
        indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    Asset {
        meshes: vec![CpuMesh {
            vertices,
            indices,
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            visibility_group: 0,
            casts_directional_shadow: true,
            name: "known tangent-space slope".into(),
            base: [1.; 4],
            emissive: [0.; 3],
            metallic: 0.,
            roughness: 0.5,
            ior: 1.5,
            specular: 1.,
            specular_color: [1.; 3],
            clearcoat: 0.,
            coat_roughness: 0.,
            clearcoat_texture: None,
            coat_roughness_texture: None,
            coat_normal_texture: None,
            coat_normal_scale: 1.,
            iridescence: 0.,
            iridescence_ior: 1.3,
            iridescence_thickness: [100., 400.],
            iridescence_texture: None,
            iridescence_thickness_texture: None,
            sheen_color: [0.; 3],
            sheen_roughness: 0.,
            sheen_color_texture: None,
            sheen_roughness_texture: None,
            diffuse_transmission: 0.,
            diffuse_transmission_color: [1.; 3],
            diffuse_transmission_texture: None,
            diffuse_transmission_color_texture: None,
            base_texture: None,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.,
            emissive_texture: None,
            normal_texture: has_normal_map.then_some(crate::asset::MaterialTexture::new(0)),
            normal_scale: 1.,
            normal_layers: None,
            bump_texture: Some(crate::asset::MaterialTexture::new(1)),
            bump_scale: 1.,
            transmission: 0.,
            transmission_texture: None,
            thickness: 0.,
            thickness_texture: None,
            attenuation_distance: f32::INFINITY,
            attenuation_color: [1.; 3],
            dispersion: 0.,
            double_sided: true,
            unlit: false,
            emits_into_gi: true,
            alpha: crate::AlphaMode::Opaque,
        }],
        images: vec![
            crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba(PIXEL))),
            // A genuine nonconstant slope in both UV axes; the old sequential
            // composition measurably rotates the known normal in every case.
            crate::asset::Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| {
                let height = ((x + 2 * y) * 5) as u8;
                image::Rgba([height, height, height, 255])
            })),
            crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba(COAT_PIXEL))),
        ],
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    }
}

#[test]
fn authored_normal_map_axes_mirrored_uv_and_back_faces() {
    material_normal_oracle(true, true, false, Observe::Base, false);
}

// The normal-map stage switched off leaves a normal-mapped surface its
// geometry normal.
#[test]
fn disabled_normal_stage_keeps_the_geometry_normal() {
    material_normal_oracle(true, false, false, Observe::Base, false);
    material_normal_oracle(true, false, false, Observe::Coat, false);
}

// A bump map alone: the affine height's slope in both UV axes, per pixel in
// raster and per metre in secondary rays (D-45), under mirrored UVs and back
// faces.
#[test]
fn affine_bump_height_axes_mirrored_uv_and_back_faces() {
    material_normal_oracle(false, true, false, Observe::Base, false);
}

#[test]
fn anisotropic_authored_frames_mirrored_shear_and_back_faces() {
    material_normal_oracle(true, true, true, Observe::Base, false);
    material_normal_oracle(true, true, true, Observe::Axis, false);
}

// The coat's normal map on the base map's frame, the derivative frame and,
// on an anisotropic material, the authored one, under mirrored UVs and back
// faces, beside a base normal map it must not follow.
#[test]
fn coat_normal_map_takes_the_base_maps_frame() {
    material_normal_oracle(true, true, false, Observe::Coat, false);
    material_normal_oracle(true, true, true, Observe::Coat, false);
}

// A stretched and sheared unwrap (`plane_asset`'s `stretched`) keeps glTF's
// frame: T along ∂p/∂U, unit, and B = (N x T) w toward decreasing V, unit
// and perpendicular to it, as MikkTSpace builds a tangent from the same
// coordinates (∂p/∂u normalised, its bitangent sign times N x T) and as the
// Khronos glTF Sample Renderer 0686eb2's derivative fallback takes it
// (material_info.glsl 164-171), so the expected normals are the unstretched
// planes'. Plausible defect: the frame kept as the coordinates' gradients,
// three.js's cotangent frame scaled by the longer axis, whose B is half as
// long here (the texel's Y slope halved) and whose T leans with the shear.
#[test]
fn stretched_sheared_coordinates_keep_gltfs_unit_frame() {
    material_normal_oracle(true, true, false, Observe::Base, true);
    material_normal_oracle(true, true, false, Observe::Coat, true);
}

fn material_normal_oracle(
    has_normal_map: bool,
    normal_maps_enabled: bool,
    authored: bool,
    observe: Observe,
    stretched: bool,
) {
    let observe_axis = observe == Observe::Axis;
    let coat = observe == Observe::Coat;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut asset = plane_asset(has_normal_map, stretched);
    let world = if authored {
        Mat4::from_cols_array(&[
            -2., 1., 0.4, 0., 1., 3., 0.2, 0., 0., 0.5, 0.5, 0., 0., 0., 0., 1.,
        ])
    } else {
        Mat4::IDENTITY
    };
    if coat {
        asset.materials[0].clearcoat = 1.;
        asset.materials[0].coat_normal_texture = Some(crate::asset::MaterialTexture::new(2));
        asset.materials[0].coat_normal_scale = 0.5;
    }
    if authored {
        asset.materials[0].anisotropy_strength = 0.6;
        asset.materials[0].anisotropy_rotation = std::f32::consts::FRAC_PI_4;
        for (i, vertex) in asset.meshes[0].vertices.iter_mut().enumerate() {
            let direction = if (i / 4) % 2 == 0 { 1. } else { -1. };
            vertex.tangent = [direction, 0., 0., direction];
        }
    }
    let material = MaterialUniform::new(
        &crate::SurfaceMaterial {
            base: [1.; 4],
            emission: [0.; 3],
            metallic: 0.,
            roughness: 0.5,
            ior: 1.5,
            specular: 1.,
            specular_color: [1.; 3],
            occlusion_strength: 1.,
            clearcoat: if coat { 1. } else { 0. },
            coat_roughness: 0.,
            coat_normal_scale: 0.5,
            iridescence: 0.,
            iridescence_ior: 1.3,
            iridescence_thickness: [100., 400.],
            sheen_color: [0.; 3],
            sheen_roughness: 0.,
            diffuse_transmission: 0.,
            diffuse_transmission_color: [1.; 3],
            normal_scale: 1.,
            normal_layers: None,
            bump_scale: 1.,
            transmission: 0.,
            thickness: 0.,
            attenuation_distance: f32::INFINITY,
            attenuation_color: [1.; 3],
            dispersion: 0.,
            anisotropy_strength: if authored { 0.6 } else { 0. },
            anisotropy_rotation: std::f32::consts::FRAC_PI_4,
            environment_scale: 1.,
            visibility_group: 0,
            unlit: false,
            emits_into_gi: true,
            double_sided: true,
            alpha: crate::AlphaMode::Opaque,
            shader: None,
            emission_render_texture: None,
            planar_reflector: None,
        },
        // A normal map, else the bump map, as the scene puts them in
        // effect (`scene::materials::maps::InEffect`).
        MaterialMaps::of(
            [if has_normal_map {
                MaterialMap::Normal
            } else {
                MaterialMap::Bump
            }]
            .into_iter()
            .chain(coat.then_some(MaterialMap::CoatNormal)),
        ),
        &Default::default(),
        &Default::default(),
    );
    let ray_values: Vec<[f32; 8]> = CENTERS
        .into_iter()
        .enumerate()
        .map(|(case, x)| {
            let side = if case < 2 { 1. } else { -1. };
            let center = world.transform_point3(Vec3::new(x, 0., 0.));
            let normal = if authored {
                Vec3::new(1., -0.8, 7.).normalize()
            } else {
                Vec3::Z
            };
            let origin = center + normal * side;
            [
                origin.x,
                origin.y,
                origin.z,
                0.001,
                -normal.x * side,
                -normal.y * side,
                -normal.z * side,
                2.,
            ]
        })
        .collect();
    // Group 2's relief binding holds the normal map, else the bump map.
    let observed = normal_probe::observe(
        &device,
        &queue,
        &Probe {
            asset: &asset,
            world,
            material,
            relief: usize::from(!has_normal_map),
            coat: 2,
            normal_maps_enabled,
            observe,
            vertex: VERTEX,
            views: 1,
            size: [128, 32],
            rays: &ray_values,
        },
    );
    let raster_pixels = &observed.raster;
    let secondary = &observed.rays;
    let mut failures = Vec::new();
    for case in 0..4 {
        // Khronos texture channel mapping plus the independently authored
        // plane axes above. No ray/shader frame math is copied into this oracle.
        // The tangent-space texel: the coat's, its X and Y at its scale,
        // else the base normal map's.
        let texel = if coat {
            Vec3::new(-127. * 0.5, 129. * 0.5, 185.)
        } else {
            Vec3::new(153., 51., 205.)
        };
        let front_normal = |texels_per_step: f32| {
            if !has_normal_map {
                // Height rises 5/255 and 10/255 per texel along U and V, so
                // the front normal is proportional to (-dh/dx, -dh/dy, 1)
                // over one step: V runs along -Y, and mirroring U reverses
                // the authored X slope.
                let x = 5. * texels_per_step;
                Vec3::new(
                    if case % 2 == 0 { -x } else { x },
                    10. * texels_per_step,
                    255.,
                )
                .normalize()
            } else if normal_maps_enabled {
                Vec3::new(
                    if case % 2 == 0 { texel.x } else { -texel.x },
                    texel.y,
                    texel.z,
                )
                .normalize()
            } else {
                Vec3::Z
            }
        };
        let side = if case < 2 { 1. } else { -1. };
        // Secondary rays take the physical height gradient per metre
        // (D-45): 16 texels across each 1.5 m plane axis. Raster follows Three r185's
        // BumpMapNode, whose normalized positional derivatives take the
        // gradient per pixel: 16 texels across the 24 pixels each plane
        // covers (a quarter of 128 columns wide at 1.5/4 of clip X, and
        // 1.5/2 of 32 rows tall).
        for (path, texels_per_step) in [("raster", 16. / 24.), ("secondary", 16. / 1.5)] {
            let expected = if authored {
                // Independent world-space surface plane: transformed authored
                // edges (-2,1,.4) and (1,3,.2) give the authored normal below.
                // T runs along increasing U, the first edge (reversed where U
                // is mirrored), and B = (N x T) w toward decreasing V, the
                // second edge's side: the world matrix mirrors, so w is
                // read from the geometry here, not from the vertices.
                let n = Vec3::new(1., -0.8, 7.).normalize();
                let t = Vec3::new(-2., 1., 0.4).normalize() * if case % 2 == 0 { 1. } else { -1. };
                let b = n.cross(t) * n.cross(t).dot(Vec3::new(1., 3., 0.2)).signum();
                let mapped = (t * texel.x + b * texel.y + n * texel.z).normalize() * side;
                if observe_axis {
                    let direction = (t + b) * side;
                    (direction - mapped * direction.dot(mapped)).normalize()
                } else {
                    mapped
                }
            } else {
                front_normal(texels_per_step) * side
            };
            let actual = if path == "raster" {
                raster_pixels[16 * 128 + 16 + case * 32]
            } else {
                secondary[case]
            };
            {
                let normal = Vec3::from_array(actual[..3].try_into().unwrap());
                eprintln!(
                    "case={case} {path}: {normal:?}, expected {expected:?}, facing={}",
                    actual[3]
                );
                // R8 linear-filter precision is amplified by the raster's
                // finite differences. Keep the exact affine-height oracle;
                // allow its observed quantization, far below a reversed slope.
                let tolerance = if !has_normal_map && path == "raster" {
                    0.006
                } else {
                    0.002
                };
                if normal.distance(expected) > tolerance
                    || (actual[3] - if observe_axis { 0.6 } else { side }).abs() > 1e-6
                {
                    failures.push(format!("case {case} {path}: {normal:?} != {expected:?}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
