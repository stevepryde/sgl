//! A live specular probe's refresh: the capture kept on the GPU. The
//! renderer holds the staging a refresh renders and filters into and the
//! refresh in flight, and each call does as much of it as its pace allows;
//! the call that completes the filter copies the cube into the scene's
//! probe array, whole (the architecture's Frame, "Live probe refresh").
//! It follows Godot b130438's `RendererSceneCull::render_probes` (an update
//! a probe at a time, faces and then the filter over frames) and three.js
//! r187's `CubeCamera.update` (explicit, six faces, `layers` to leave the
//! reflecting object out) in practice; no code of theirs is here.
use super::Capture;
use crate::baked_specular_probe::ProbeError;
use crate::content::identity::LiveSpecularProbeId;
use crate::content::live_specular_probe::{ProbeRefresh, ProbeRefreshPace};
use crate::renderer::{Renderer, View};
use crate::settings::Settings;
use crate::stages::prepare::CaptureViews;
use crate::stages::probe_prefilter::ProbePrefilter;
use crate::stages::shadows::local::RefreshRecords;
use crate::timing::GpuTiming;
use crate::{FrameInput, Scene};
use glam::{DVec3, Vec3};

/// What a renderer holds to refresh a scene's live probes, from its first
/// refresh until a frame of a scene without live probes.
pub(crate) struct LiveRefresh {
    /// The scene whose probes it refreshes.
    scene: u64,
    /// The staging: the face target, the cube the faces resolve into and
    /// the prefiltered cube, at the probes' face size, with the filter's
    /// pipelines.
    prefilter: ProbePrefilter,
    /// The refreshes' capture views and what they draw, whose buffers each
    /// call writes: a refresh creates none once they have grown to it.
    views: CaptureViews,
    /// The refreshes' copy of the view's frame's local-light shadow
    /// records.
    records: RefreshRecords,
    flight: Option<Flight>,
}

/// A refresh under way.
struct Flight {
    probe: LiveSpecularProbeId,
    /// Where every face captures from: the probe's centre at its first.
    center: Vec3,
    /// The scene's render origin `center` is in.
    origin: DVec3,
    /// The faces rendered, in cube order.
    faces: u32,
}

impl LiveRefresh {
    /// Whether a frame of `scene` keeps it: its scene, while that holds a
    /// live probe.
    pub(crate) fn serves(&self, scene: &Scene) -> bool {
        self.scene == scene.id && scene.has_live_probes()
    }
}

impl Renderer {
    /// Advances the refresh of live probe `probe` of `scene` by what `pace`
    /// allows a call: a capture as [`Self::capture_specular_probe_async`]
    /// makes one, kept on the GPU. The call that completes it copies the
    /// GGX-prefiltered cube into the probe's cube of the scene's array and
    /// returns [`ProbeRefresh::Published`]; until then the probe shows its
    /// last published cube, whole, and before its first it weighs nothing.
    /// Nothing is read back or uploaded, and nothing refreshes a probe
    /// unless the game calls this: the game chooses when and how often.
    ///
    /// It draws what a bake draws, and the moving `capture_visible`
    /// instances too, at their poses now and their deformation as of the
    /// last rendered frame, each face culled to its view. Pass the frame's
    /// own `input`: as in a frame, only materials whose visibility group
    /// `input.visibility_mask` selects are drawn, and its faces also leave
    /// out the probe's `hidden_groups`, the reflecting object's, which
    /// still cast their shadows in it (Godot's `ReflectionProbe.cull_mask`,
    /// three.js's `CubeCamera.layers`). Blended surfaces and effects are
    /// left out.
    /// Each face renders at the probe's face size. Its surfaces take
    /// `input`'s lights with the directional shadow in cascades about the
    /// probe's centre, moving casters included; the scene's lights with
    /// the shadows `view`'s last frame drew for them, so a light that
    /// frame gave no shadow casts none in the probe; a procedural sky as
    /// `view`'s last frame computed it, black before its first; and the
    /// installed probes, this one's last cube among them, so each refresh
    /// adds a bounce of the last.
    ///
    /// A refresh captures from the probe's centre as its first call found
    /// it; faces rendered by later calls show moving content at those
    /// calls' times. One refresh is in flight in a renderer at a time: a
    /// call for another probe returns [`ProbeRefresh::Waiting`] and does
    /// nothing, so finish one probe's refresh before starting another's. A
    /// refresh left unfinished ends when its probe is removed.
    ///
    /// Like a bake it borrows `view`'s shadow maps and its opaque stage, so
    /// call it between frames, not between `render` and `finish_frame`, and
    /// with the settings `view`'s frames use: another shadow quality
    /// reallocates the maps. It is not a frame: it takes nothing from, and
    /// commits nothing to, any view's history or shadow cache. The first
    /// refresh creates the staging, the buffers later ones write and its
    /// own programs (the filter's, and the pipeline variants its faces and
    /// cascades draw with, which [`Self::compile`] does not ready), so make
    /// it while a level loads; a refresh of a warmed probe creates only
    /// each face's two bind groups. It submits its own commands; pass a
    /// `timing`
    /// begun for the frame to time its passes (`probe refresh shadows`,
    /// `probe refresh faces`, `probe refresh filter`).
    ///
    /// `ProbeError::InvalidProbe` for a probe the scene does not hold.
    ///
    /// # Panics
    ///
    /// For a view created from another renderer.
    #[allow(clippy::too_many_arguments)]
    pub fn refresh_specular_probe(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        view: &mut View,
        input: &FrameInput,
        settings: &Settings,
        timing: Option<&GpuTiming>,
        probe: LiveSpecularProbeId,
        pace: ProbeRefreshPace,
    ) -> Result<ProbeRefresh, ProbeError> {
        assert!(
            view.renderer == self.id,
            "a view refreshes with the renderer it was created from (`View::new`)"
        );
        let Some(cube) = scene.live_probe_cube(probe) else {
            return Err(ProbeError::InvalidProbe(
                "the live probe is not in this scene",
            ));
        };
        let face_size = cube.probe.face_size;
        // The staging of this scene's face size, made at its first refresh.
        let mut state = self
            .refresh
            .take()
            .filter(|state| state.scene == scene.id && state.prefilter.face_size() == face_size)
            .unwrap_or_else(|| LiveRefresh {
                scene: scene.id,
                prefilter: ProbePrefilter::new(device, face_size, face_size),
                views: CaptureViews::new(device),
                records: RefreshRecords::default(),
                flight: None,
            });
        if let Some(flight) = &state.flight {
            let held = scene.live_probe_cube(flight.probe).is_some();
            if flight.probe != probe && held {
                self.refresh = Some(state);
                return Ok(ProbeRefresh::Waiting);
            }
            // A refresh whose probe was removed ends, and one whose faces
            // were captured about another render origin starts again.
            if !held || flight.origin != scene.origin() {
                state.flight = None;
            }
        }
        view.settle(false);
        self.ready_capture(device, queue, scene, view, (settings, timing));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("refresh live specular probe"),
        });
        let flight = state.flight.get_or_insert(Flight {
            probe,
            center: cube.probe.center,
            origin: scene.origin(),
            faces: 0,
        });
        let center = flight.center;
        let faces = match pace {
            ProbeRefreshPace::Whole | ProbeRefreshPace::FacesThenFilter => flight.faces..6,
            ProbeRefreshPace::FacePerCall => flight.faces..(flight.faces + 1).min(6),
        };
        // The filter follows the sixth face: in its call, or the next.
        let filter = pace == ProbeRefreshPace::Whole || flight.faces == 6;
        flight.faces = faces.end;
        if !faces.is_empty() {
            // A procedural sky is the view's, as its last frame computed
            // it for its camera: the refresh computes none.
            let sky = view.sky.views().map(|views| views.map(Clone::clone));
            self.encode_capture(
                (device, queue),
                scene,
                view,
                (input, settings),
                (
                    center,
                    Capture::Refresh {
                        hidden: cube.probe.hidden_groups,
                        records: &mut state.records,
                    },
                ),
                &mut state.views,
                (&state.prefilter, sky),
                (faces, timing),
                &mut encoder,
                |_| {},
            );
        }
        if filter {
            state.prefilter.encode(&mut encoder, timing);
            // Every level and face at once, after the last pass that draws.
            state
                .prefilter
                .publish(&mut encoder, &cube.texture, cube.cube);
        }
        queue.submit([encoder.finish()]);
        if filter {
            scene.publish_live_probe(queue, probe, center);
            state.flight = None;
        }
        self.refresh = Some(state);
        Ok(if filter {
            ProbeRefresh::Published
        } else {
            ProbeRefresh::InFlight
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
