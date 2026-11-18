//! Views and the frame contract in the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): what a frame commits and when (the
//! architecture's Scene content, "Edits and frames", and the register's
//! F-13 and M-8 in `specs/sgl3d-history.md`), judged by the motion the next
//! frame writes against the script's analytic truth.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose};
use scenario::rig::{Rig, gpu};
use scenario::temporal::{
    DEPTH, STILL_DEPTH, STILL_UV, Scenario, VIEW, background, card, motion_follows_the_script,
    still,
};
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::Vec3;
use sgl_3d::settings::Antialiasing;
use sgl_3d::{MaterialShader, ShaderSource, SurfaceMaterial};

/// F-13. Plausible defect: `finish_frame` commits the scene as it is when it
/// runs, not as the frame drew it, so a loop that submits a frame, poses the
/// scene for the next and then finishes the frame it submitted makes every
/// moving instance's next frame measure its motion from its new pose: zero
/// motion. The fixture: case 1's card moving across before the background
/// while the camera pans, each frame's poses set after the previous frame
/// is submitted and before it is finished (`Rig::render_then`). The oracle
/// is case 1's, analytic (`motion_follows_the_script`): the last frame's
/// motion is the script's from the previous frame's poses to its own.
#[test]
fn edits_after_submission_belong_to_the_next_frame() {
    let Some(gpu) = gpu() else { return };
    let (back, back_pose) = background();
    let card_start = Pose::at(Vec3::new(-0.6, 0.2, -DEPTH));
    let across = Motion::translate(Vec3::new(0.02, 0., 0.));
    let pan = VIEW.pan(0.37, DEPTH);
    let camera = |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    let script = |f: u32| vec![back_pose, across.pose(card_start, f)];
    let scenario = Scenario {
        shapes: vec![back, card()],
        camera: &camera,
        script: &script,
    };
    let mut rig = scenario.rig(&gpu, VIEW, Antialiasing::Taa);
    let frame = 3;
    scenario.pose(&mut rig, 0);
    for f in 0..frame {
        rig.render_then(f, |rig| scenario.pose(rig, f + 1));
    }
    rig.render(frame);
    let worst = motion_follows_the_script(&rig, &scenario, frame, "posed after submission")
        .unwrap_or_else(|failure| panic!("{failure}"));
    eprintln!(
        "posed after submission: largest error {} px, {} m",
        worst[0] * VIEW.size[0] as f32,
        worst[1]
    );
}

/// A game's shader whose vertex function moves every vertex by its
/// parameters' `offset` metres, whatever the time: its motion changes only
/// when the game sets other parameters.
const OFFSET_SHADER: &str = r#"
struct ShaderParams {
 offset:vec3<f32>,
 unused:f32,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 out.position+=params.offset;
 return out;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
"#;

/// M-8 (B-8 of the 2026-11-18 history audit). Plausible defect: a material
/// that gains a shader gets a parameter block whose previous copy is zero,
/// so a frame that draws it with parameters set before its first frame
/// measures the vertex function's motion from zero parameters. The
/// fixture: a still card before a still camera, drawn plainly for a few
/// frames, that then gains `OFFSET_SHADER` with a 0.1 m offset (about
/// 2.5 px at `DEPTH`), set before the frame that first draws it. The oracle
/// is analytic: nothing moved between the two frames' evaluations, so the
/// card's interior writes zero motion within case 1's `STILL_UV` and
/// `STILL_DEPTH`, in the frame that gains the shader and the one after.
#[test]
fn a_material_that_gains_a_shader_writes_no_parameter_motion_in_its_first_frame() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, scenario::temporal::SKY);
    let (id, materials) = rig.add_editable(&card(), Pose::at(Vec3::new(0., 0., -DEPTH)), 0);
    rig.look(still(0));
    let gained = 3;
    for frame in 0..gained {
        rig.render(frame);
    }
    let shader = rig
        .scene
        .add_shader(ShaderSource {
            wgsl: OFFSET_SHADER.into(),
            label: "offset".into(),
        })
        .expect("the offset shader");
    let offset = 0.1f32;
    for &material in &materials {
        let values = rig.scene.material(material).expect("a fixture material");
        let shader = Some(MaterialShader {
            shader,
            displacement_bound: offset,
        });
        rig.scene
            .set_material(
                &rig.gpu.queue,
                material,
                SurfaceMaterial { shader, ..values },
            )
            .expect("a shaded card");
        rig.scene
            .set_shader_parameters(
                &rig.gpu.queue,
                material,
                bytemuck::cast_slice(&[offset, 0., 0., 0.]),
            )
            .expect("the offset");
    }
    for frame in gained..gained + 2 {
        rig.render(frame);
        let motion = rig.read(DiagnosticTarget::Motion);
        let interior = rig
            .read(DiagnosticTarget::SourceId)
            .region_of(diagnostics::source_id(id))
            .eroded(1);
        assert!(
            !interior.is_empty(),
            "frame {frame}: the card drew no interior"
        );
        let mut largest = [0f32; 2];
        for pixel in interior.iter() {
            let [r, g, b, _] = motion.at(pixel);
            largest = [
                largest[0].max(r.abs().max(g.abs())),
                largest[1].max(b.abs()),
            ];
        }
        eprintln!(
            "frame {frame}: largest motion {} px, depth motion {} m",
            largest[0] * VIEW.size[0] as f32,
            largest[1]
        );
        assert!(
            largest[0] <= STILL_UV && largest[1] <= STILL_DEPTH,
            "frame {frame}, {} after the card gained its shader: its still interior wrote {} px and {} m of motion",
            frame - gained,
            largest[0] * VIEW.size[0] as f32,
            largest[1]
        );
    }
}
