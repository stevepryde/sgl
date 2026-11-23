//! Instanced glTF nodes (`EXT_mesh_gpu_instancing`, `Asset::instanced`)
//! installed as a game installs them, against the same scene exported by
//! Khronos glTF Blender I/O as ordinary nodes, in the scenario harness
//! (`specs/testing.md`, "SGL3D scenario harness"). The oracle is the
//! extension's equivalence: an instanced node is its expansion, so every
//! frame of the two must match, pixel for pixel up to rounding at edges,
//! under the camera, the sun's shadow maps and world-space rays, and the
//! camera must draw the same triangles. The fixture
//! (`crates/sgl-3d/tests/fixtures/gltf/InstancedProps`) turns and scales a
//! parent unevenly and mirrors three of its nine single-sided instances, so
//! a wrong composition order, a lost mirror (culled faces, a normal turned
//! in) or a wrong rotation changes the frame. No temporal feature runs:
//! antialiasing is off and every frame is a camera cut. Every compared pair
//! dumps (`SGL_SCENARIO_DUMP`) under `gltf_instancing/`.
//!
//! Coverage (requirement 3): at rest. Import makes plain data, which
//! neither motion, a cut, a resize nor a setting reaches; the instances it
//! makes are the scene's own, whose suites cover them in motion.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::View;
use scenario::image::Image;
use scenario::rig::{Dump, Gpu, HALF_STEP, Rig, gpu, ray_gpu};
use sgl_3d::asset::{Asset, load};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::settings::{self, Antialiasing};
use sgl_3d::{Camera, InstanceState, Mobility};
use sgl_3d::{DirectionalLight, DirectionalShadow, HemisphereLight};

const VIEW: View = View {
    size: [160, 120],
    fov_y: 0.9,
    near: 0.05,
};

fn fixture(expanded: bool) -> Asset {
    let directory = if expanded { "glTF-Expanded" } else { "glTF" };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/sgl-3d/tests/fixtures/gltf/InstancedProps")
        .join(directory)
        .join("InstancedProps.gltf");
    load(&path).expect("the fixture loads")
}

/// The fixture added whole and placed as a game places it: its model, and
/// one static instance per instanced transform, which other views (the
/// sun's shadow, rays) show only where `casting`.
fn install(rig: &mut Rig, asset: Asset, casting: bool) {
    let Gpu { device, queue } = &rig.gpu;
    let placements: Vec<Vec<Mat4>> = asset
        .instanced
        .iter()
        .map(|instanced| {
            instanced
                .nodes
                .iter()
                .flat_map(|node| node.transforms.iter().copied())
                .collect()
        })
        .collect();
    let ids = rig.scene.add_asset(device, queue, asset).unwrap();
    let mut add = |model, pose, capture_visible| {
        rig.scene
            .add_instance(
                device,
                queue,
                InstanceState {
                    pose,
                    capture_visible,
                    ..InstanceState::new(model)
                },
                Mobility::Static,
            )
            .unwrap();
    };
    add(ids.model, Mat4::IDENTITY, true);
    for (model, transforms) in ids.instanced.iter().zip(placements) {
        for pose in transforms {
            add(*model, pose, casting);
        }
    }
}

/// The fixture (or its expansion) lit by a shadow-casting sun, its ground
/// a mirror where `mirror_ground`, seen from above, through `settings`.
fn stage(
    gpu: &Gpu,
    expanded: bool,
    mirror_ground: bool,
    edit: impl Fn(&mut settings::Settings),
) -> Rig {
    staged(gpu, expanded, mirror_ground, true, edit)
}

fn staged(
    gpu: &Gpu,
    expanded: bool,
    mirror_ground: bool,
    casting: bool,
    edit: impl Fn(&mut settings::Settings),
) -> Rig {
    let mut rig = Rig::new(gpu, VIEW, Antialiasing::Off, 0.);
    let mut asset = fixture(expanded);
    if mirror_ground {
        let ground = asset
            .materials
            .iter_mut()
            .find(|material| material.name == "Ground")
            .unwrap();
        (ground.metallic, ground.roughness) = (1., 0.);
    }
    install(&mut rig, asset, casting);
    let eye = Vec3::new(5., 8., 10.);
    let view = &mut rig.views[0];
    view.input.camera = Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., -0.5, 0.), Vec3::Y),
        ..VIEW.camera(scenario::geometry::Pose::at(eye))
    };
    view.input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.4, -1., -0.3).normalize(),
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: 30.,
            cascades: 2,
        }),
        ..Default::default()
    });
    view.input.hemisphere_light = HemisphereLight {
        sky_color: [0.4, 0.45, 0.5],
        ground_color: [0.2; 3],
        intensity: 0.5,
    };
    edit(&mut view.settings);
    rig
}

/// The composite of a camera cut.
fn shot(rig: &mut Rig) -> Image {
    rig.views[0].input.camera_cut = true;
    rig.render(0);
    rig.read(DiagnosticTarget::Composite)
}

/// Whether `a` and `b` agree within the rounding of two stores into an
/// Rgba16Float target (relative) or 1e-4 (absolute, for near black).
fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    (0..3).all(|c| (a[c] - b[c]).abs() <= 2. * HALF_STEP * a[c].abs().max(b[c].abs()) + 1e-4)
}

/// Dumps `a` and `b` as `<what> instanced` and `<what> expanded`, then
/// asserts each pixel of `a` agrees with one of `b`'s within a pixel of it,
/// and each of `b`'s with one of `a`'s: the instanced and baked vertices
/// round apart by about 1e-6 m, which moves an edge or a shadow-map texel
/// by at most a pixel and nothing else.
fn assert_equivalent(a: &Image, b: &Image, what: &str) {
    let dump = Dump::new("gltf_instancing");
    dump.image(&format!("{what} instanced"), a);
    dump.image(&format!("{what} expanded"), b);
    let misses = differences(a, b);
    assert!(
        misses.is_empty(),
        "{what}: {} pixels differ, the first {:?}",
        misses.len(),
        &misses[..misses.len().min(4)]
    );
}

/// The pixels where `a` and `b` disagree as `assert_equivalent` judges.
fn differences(a: &Image, b: &Image) -> Vec<([u32; 2], [f32; 4], [f32; 4])> {
    let [w, h] = a.size;
    let near = |from: &Image, to: &Image, x: u32, y: u32| {
        let value = from.at([x, y]);
        (x.saturating_sub(1)..=(x + 1).min(w - 1)).any(|nx| {
            (y.saturating_sub(1)..=(y + 1).min(h - 1)).any(|ny| close(value, to.at([nx, ny])))
        })
    };
    let mut misses = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if !near(a, b, x, y) || !near(b, a, x, y) {
                misses.push(([x, y], a.at([x, y]), b.at([x, y])));
            }
        }
    }
    misses
}

// Defects: an instance placed off its expansion (the node and instance
// transforms composed in the wrong order, a parent dropped, a quaternion
// misread), a mirrored instance drawn with its winding unreversed (its
// single-sided faces culled) or its normal turned in, or an instance that
// casts no shadow. Oracle: the extension's equivalence with the expanded
// export, under the camera and the sun's cascades.
#[test]
fn instanced_props_draw_and_cast_shadows_as_their_expansion() {
    let Some(gpu) = gpu() else { return };
    let mut instanced = stage(&gpu, false, false, |_| {});
    let mut expanded = stage(&gpu, true, false, |_| {});
    let (a, b) = (shot(&mut instanced), shot(&mut expanded));
    assert_equivalent(&a, &b, "camera and shadow maps");
    // The fixture can show a missing caster: the props' shadows fall on
    // ground in view, so the frame without them differs widely.
    let shadowless = shot(&mut staged(&gpu, false, false, false, |_| {}));
    assert!(
        differences(&a, &shadowless).len() > 200,
        "the props shadow too little of the view"
    );
    // The camera draws every instance's triangles, as many as the
    // expansion's.
    let triangles = |rig: &mut Rig| {
        rig.render(1);
        let _ = rig.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        rig.render(2);
        let _ = rig.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        rig.views[0]
            .view
            .geometry_stats(&rig.gpu.device)
            .expect("a completed frame's statistics")
            .total()
            .1
    };
    assert_eq!(triangles(&mut instanced), triangles(&mut expanded));
}

// Defects: an instance that rays miss or meet off its expansion: its entry
// in the instance BVH or the TLAS, its inverse pose, or a mirrored
// instance's side judged from its unmirrored winding. Oracle: the
// extension's equivalence, on a mirror ground whose world-space reflections
// trace every instance, through the portable traversal and, where the
// device traces rays in hardware, the hardware path. A mirror's rays are
// one a pixel and deterministic. Ray-traced shadows are not compared: their
// rays sample the sun's disc and the denoiser spreads each, so a ray that
// passes a caster's edge within the rounding between baked and posed
// vertices (about 1e-6 m) shifts a few pixels of penumbra a few half-float
// steps, which no bound from the extension separates from a defect.
fn rays_meet_instances_as_their_expansion(hardware: bool) {
    let Some(gpu) = ray_gpu() else { return };
    let reflections = |settings: &mut settings::Settings| {
        settings.screen_space_reflections = settings::ScreenSpaceReflections::Full;
        settings.world_space_reflections = settings::WorldSpaceReflections::All;
        settings.hardware_ray_tracing = hardware;
    };
    let mut instanced = stage(&gpu, false, true, reflections);
    let settings = instanced.views[0].settings;
    if hardware
        && !instanced
            .renderer
            .ray_tracing_in_effect(&settings, &instanced.scene)
    {
        eprintln!("unsupported: the device has no ray queries; the hardware path is not judged");
        return;
    }
    let what = if hardware {
        "hardware rays"
    } else {
        "portable rays"
    };
    let a = shot(&mut instanced);
    let b = shot(&mut stage(&gpu, true, true, reflections));
    assert_equivalent(&a, &b, what);
}

#[test]
fn portable_rays_meet_instances_as_their_expansion() {
    rays_meet_instances_as_their_expansion(false);
}

#[test]
fn hardware_rays_meet_instances_as_their_expansion() {
    rays_meet_instances_as_their_expansion(true);
}
