# SGL3D temporal history

Every rule in SGL3D that discards temporal history, weakens it, withholds
jitter, restarts a stage, or zeroes, replaces or disregards a motion vector.
The [History contract](sgl3d-architecture.md#shared-contracts) says who owns
history and when a view resets it; this register lists what each owner
then does with it, rule by rule, so that an artefact can be traced to the rule
behind it and no rule exists only in code.

A rule is in one of three states:

- **Rule.** Intended behaviour. The entry is its specification, including
  behaviour SGL3D ships faithfully from an upstream engine.
- **Defect B-n.** Behaviour that is not intended. It stays listed, described as
  it is today, until its fix lands; the fix replaces the entry with the rule
  that is intended and removes the row from [Defects](#defects). A defect that
  a measurement shows to be a trade-off with no better alternative becomes a
  rule that states the trade-off.
- **Open question O-n.** Behaviour that is undecided or unmeasured. It is
  settled by a decision or a measurement, and then becomes a rule or a defect.

## Keeping this register

1. A change that adds, removes or alters a condition under which history or a
   motion vector is rejected updates its entry here in the same change, and
   adds one for a new condition. A rejection that is not in this register is a
   defect, whatever it does.
2. An entry states the trigger with its thresholds and units, what is rejected
   and over what scope, what restores accumulation and when, what in a game
   raises it, and what keeps it raised in a picture that looks still.
3. Scope is as narrow as the cause. A rule whose cause is local (a pixel, a
   surface, a light) does not reject across the view, the stage or the scene.
   A view-wide or stage-wide rule names why no narrower signal can carry it.
4. A threshold is stated in the units of what it measures and holds at any
   distance from the render origin, any frame rate and any render size the
   stage supports; where it does not, the entry says where it stops holding.
5. A rule ported from an engine names the source and says whether it is
   faithful. A faithful limitation is still listed, with what a game sees.
6. Every rule a game can raise without changing a setting has a guard: a test
   or a scenario that fails when the rule changes, under
   [Testing](testing.md). "None" in an entry's guard is work owed.

Identifiers are stable. Their prefixes: `F` the frame, the renderer and its views, `M` motion
vectors and the temporal masks' writers, `T` temporal antialiasing, `U` FSR
upscaling, `R` reflections, `S` shadows, `AO` ambient occlusion, `SI`
screen-space indirect light, `V` volumetric fog, `G` dynamic GI, `E` exposure, `X` other
stages.

## What a game does to keep history

The rules below that a game most often raises without meaning to, as the
contract a game follows:

- Render each frame of a scene with one `Renderer::render` call that lists
  every view drawn in it, and call `Renderer::finish_frame` once for every
  frame it renders and submits (F-3, F-12, F-13). A frame rendered and not
  finished, then rendered again as the same frame, restarts FSR (U-2) and
  leaves filtered fog unreprojected for one frame (V-5); every other history
  continues. A cut in a frame that is not finished holds until a frame is
  finished (F-1); a stage switched off in it restarts where it released its
  targets (F-4), as do TAA and Crystal where it rebuilt their context for a
  different TAA flag (F-5). Render
  every view that keeps
  temporal history in every frame of its scene: a view left out of a finished
  frame restarts (F-1, F-12). Edit the scene before `render` or after the
  submission, never in between; an edit made after the submission and before
  `finish_frame` belongs to the next frame (F-13).
- Set `FrameInput::camera_cut` when the camera jumps, and only then (F-1).
- Give moving objects `Mobility::Moving` and pose them every frame; a static
  instance that is moved writes no motion and re-arms antialiasing across
  the view (M-4, M-5, M-6, M-9).
- Change `FrameInput::visibility_mask`, models, geometry and what a material
  draws only when the content changes: each re-arms antialiasing across the
  view (F-10, M-9).
- Expect surfaces under blended materials, glow, mist, heat distortion and
  materials whose normal layers move or whose shader reads the time never to
  reach antialiasing's resting stability: they are marked as changing every
  frame (M-11, M-16, M-17a to M-17c). Set a shader's parameters and an
  instance's shader data only when they change: each change marks the
  material's surfaces for that frame (M-16); writing the values in effect
  marks nothing.

## Defects

None is open as of the fixes of the 2026-11-18 history audit
([audit log](audits.md)). A defect found later is listed here, with the
entries that describe it, until its fix lands.

## Open questions

None is open as of the fixes of the 2026-11-18 history audit. A question
found later is listed here, with the entries it concerns, until a decision or
a measurement settles it.

## Frame and renderer

Each view keeps its camera history (`View`, `view/history.rs`, `CameraHistory`): its last finished frame's unjittered view and projection, its jitter, its time, its visibility mask, the render origin it is expressed in, and the frames since the last reset. It issues the one reset every one of its stages follows, and each stage keeps what describes its GPU history as the view's last finished frame left it (`view::history::Kept`), so a stage restarts when it did not run in that frame and a frame rendered again continues as the first rendering did. The scene keeps the motion history (submitted poses, deformation slots, shader data, parameter blocks) and the rigid-change signal, committed by `Renderer::finish_frame`. A continuously running game most often trips the visibility-mask signal (F-10) and the scene's rigid-change signal where it spawns, hides or swaps models (M-9); a frame that was rendered but not finished, then rendered again, restarts FSR (F-3, U-2) and leaves filtered fog unreprojected for one frame (V-5).

### Signals

| Signal | Raised by | When | Cleared / committed | Consumed by |
|---|---|---|---|---|
| `HistoryFrame::valid` (false is a reset) | `View::begin_history` (`renderer/view.rs`), `CameraHistory::begin` | `FrameInput::camera_cut`; a pending reset, from `View::resize` or from a reset the view rendered and did not finish; a frame that does not follow the view's last finished frame (its first, another scene's, or one after a finished frame of the scene that did not render it) | advanced only when a frame of the view is finished: the next frame is valid unless raised again | DiligentFX context (TAA, Crystal SSR), FSR, exposure, motion blur, world-space reflections, ray-traced shadows, ambient occlusion's temporal filter, fog, the cull stage's early phase |
| `HistoryFrame::frames` (frames since the reset) | `CameraHistory` | advanced only when a frame of the view is finished (`View::settle` after `Renderer::finish_frame`) | 0 on a reset | the DiligentFX context's frame index (its pairs' halves by parity, and its continuity test against the last finished frame's), fog; also noise seeds (shadow filter, ambient occlusion, motion blur, fog) |
| `HistoryFrame::previous_camera` and `previous` | `CameraHistory::begin` | none, and previous = this frame's view-projection, after a reset; otherwise translated by any render-origin move | committed at `finish_frame` | the view's `previous_view_projection` (all motion), the DiligentFX previous camera, Velvet, world-space reflections, ray-traced shadows, ambient occlusion, fog, the cull stage's early phase |
| `HistoryFrame::previous_elapsed_seconds` | `CameraHistory::begin` | this frame's time after a reset | committed at `finish_frame` | `Frame.previous_elapsed_seconds` and `previous_animation_phase`, at which vertex functions evaluate motion |
| `HistoryFrame::visibility_changed` | `CameraHistory::begin` | `FrameInput::visibility_mask` differs from the last finished frame's and history is valid | when a frame with the new mask is finished | TAA's `rigid_object_motion` only; FSR never sees it |
| `Scene::rigid_objects_changed()` | `Instances` (add, `set_instance`, remove, `geometry_changed`), `Scene::set_model`, `Scene::set_material` | see M-9 | a finished frame, for what was raised before it began (`Scene::finish_frame`) | TAA's `rigid_object_motion` only; FSR never sees it |
| TAA camera motion | `PostFx::begin` (`TemporalAntiAliasing::camera_moved`) | see T-20 | per frame | TAA's `camera_motion` only |
| G-buffer motion: UV motion RG, depth motion B, its validity A | geometry passes, the sky, the receiver pass | every pixel, every frame | per frame | TAA (through the DiligentFX inputs pass), FSR, motion blur, TAA's motion reach, ambient occlusion, the reflection methods |
| Temporal masks: reactive, transparency and composition | the transparent stage's draws (M-11, M-16, M-17a–c) | while `Effective::temporal_masks` | cleared by the first writer each frame | FSR always; TAA's responsive masks |
| TAA motion reach | `stages/antialiasing/reach.rs` | every frame TAA runs | per frame | TAA's neighbourhood motion |
| Jitter (`HistoryFrame::camera.jitter`) | the DiligentFX context's `prepare` (TAA) or FSR's `prepare` | TAA: zero on its reset frames; FSR: every frame | committed with the camera at `finish_frame` | raster, the DiligentFX previous camera, the cull stage's pyramid reprojection |

### Camera history and resets

#### F-1 A view's one reset

- **Rule.** When `FrameInput::camera_cut` is set, a reset is pending (from `View::resize`, F-2, or because the view's last render was a reset that was not finished), or the frame does not follow the view's last finished frame (its first frame, a frame of another `Scene`, or one after a finished frame of the scene that did not render the view, F-12), the view's camera history begins without a previous frame: no previous camera, `valid` false, `frames` 0, the previous view-projection equal to this frame's, the previous time equal to this frame's, `visibility_changed` false. Every stage of the view that reads `valid` or `frames` restarts over the whole view. The frame after the next finished frame continues normally. A reset holds until a frame is finished: a view whose render was a reset and was not finished renders its next frame as a reset too, whatever that frame's `camera_cut` says, since the cut was never shown and so has not happened yet; the next frame therefore never continues from a half an unfinished reset overwrote (`View::settle` raises the pending reset, which only a finished frame clears).
- **Raised by.** A cut flag, a target-changing resize, the view's first frame, a different `Scene`, a finished frame of the scene without the view, a frame after an unfinished reset.
- **Stays triggered while.** The game leaves `camera_cut` set, alternates two scenes through one view, or leaves the view out of every other frame: every frame of it resets.
- **Source.** SGL3D's own (S3D-4). Code: `renderer/view.rs` (`View::begin_history`, `View::settle`), `view/history.rs` (`CameraHistory::begin`), `scene/mod.rs` (`Scene::last_finished`).
- **Guard.** `a_cut_a_resize_and_another_scene_restart_history`, `an_unfinished_cut_holds_until_a_frame_is_finished` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`); `a_view_that_skips_a_frame_restarts` (`tests/sgl-3d/tests/scenario_views.rs`); `a_moved_origin_reprojects_the_same_world_point` (`view/history.rs`); `a_camera_cut_frame_is_not_blurred` (`stages/motion_blur/tests.rs`).
- **Status.** Rule.

#### F-2 Pending reset from `View::resize`

- **Rule.** When `View::resize` recreates or drops the view's FSR context (FSR selected with no context, a context of another display size or version, or FSR deselected while one exists), or when the view's render, scene or output size changes, or bloom's target size changes (`RenderPreset::High` against `Low`), the view recreates its shared targets and raises a pending reset (F-1) for every one of its stages; other views are untouched. The pending reset clears only when a frame rendered with the current targets is finished.
- **Raised by.** Window resizes, device scale, `SceneResolution`, FSR mode, version or quality, the preset.
- **Stays triggered while.** Nothing: it needs a size or setting change. A preset change at an unchanged render size restarts every history although no stage's history lives in the shared targets.
- **Source.** SGL3D's own. Code: `renderer/view.rs` (`View::resize`, `View::settle`), `stages/antialiasing/mod.rs` (`Antialiasing::resize`).
- **Guard.** `resize_between_render_and_finish_restarts_history` (`renderer/size_tests.rs`); `upscaled_restarts` (`tests/sgl-3d/tests/scenario_fsr_upscaling.rs`); `a_cut_a_resize_and_another_scene_restart_history`; `a_view_keeps_its_history_while_another_appears_resizes_and_leaves` (`tests/sgl-3d/tests/scenario_views.rs`).
- **Status.** Rule.

#### F-3 A frame rendered again as the same frame restarts nothing but FSR

- **Rule.** When a frame is rendered and `finish_frame` does not follow, the scene commits nothing, and the next render begins with the same `frames`, previous camera and previous time. Where that render is the same frame again (the same views, with the same stages in effect), every stage continues from the state its last finished frame left (`view::history::Kept`): the DiligentFX context (TAA, Crystal SSR) from its last finished frame index, Velvet, world-space rays, ambient occlusion's temporal pass and the ray-traced shadows from their kept run (frame count and the half of each pair it writes), the shadows' slots, quality and seed too. A frame rendered again therefore reads and writes the same halves of every pair as the first rendering did. Two exceptions: FSR restarts (U-2), and filtered fog does not reproject for one frame (V-5). Where the unfinished render differs in kind from the next, the contract is narrower: an unfinished reset holds, so the next render resets too (F-1); and a stage that did not run in the unfinished render restarts on the next where that render released its targets (F-4), as do TAA and Crystal where that render rebuilt their context for a different TAA flag (F-5). Each view settles every stage's kept state once per render (`View::settle`): finished at the view's first `&mut self` method after `Renderer::finish_frame` committed that frame (each render holds its attempt's own flag, which `finish_frame` sets, so a view left out of later frames still settles its own as finished), unfinished when the view renders again first. A view method called between `render` and `finish_frame` leaves the render pending.
- **Raised by.** Dropping an encoded frame; rendering a frame twice; omitting `finish_frame`.
- **Stays triggered while.** The game never finishes: camera motion is always zero and nothing accumulates.
- **Source.** SGL3D's own (S3D-4; the History contract's "What a frame commits"). Code: `view/history.rs` (`Kept`, `StageHistory`), `renderer/view.rs` (`View::settle`).
- **Guard.** `an_unfinished_frame_restarts_nothing`, `an_unfinished_cut_holds_until_a_frame_is_finished` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`), `kept_state_survives_an_unfinished_frame_and_ends_when_the_stage_skips_one` (`view/history.rs`), and the stages' own: `crystal_keeps_its_history_across_an_unfinished_frame`, `velvet_keeps_its_history_across_an_unfinished_frame`, `world_reflections_keep_their_history_across_an_unfinished_frame`, `accumulated_visibility_survives_an_unfinished_frame`, `the_shadow_mask_survives_an_unfinished_frame`, `the_kept_result_survives_an_unfinished_frame`.
- **Status.** Rule.

#### F-4 A frame in which a stage did not run restarts it

- **Rule.** When the view's last finished frame did not run a stage (the DiligentFX context, FSR, Velvet, world-space rays, ambient occlusion's temporal pass, the ray-traced shadows, screen-space indirect light), the stage's kept state is none and it resets its history over the whole view; a reset in a frame the stage does not run needs nothing more, since after that frame is finished the stage keeps none. Accumulation resumes on the next frame. A frame that is not finished changes nothing kept, but a render in which a stage does not run releases some stages' GPU history at render, as a finished frame without them would, and those restart on the next frame although the render was not finished: the DiligentFX context (TAA and Crystal SSR) when the render ran neither TAA nor Crystal, and Crystal's histories alone when it ran TAA without Crystal (`renderer/frame.rs`, `render_view`); Velvet and world-space rays when their method was off (`Reflections::resolve`); screen-space indirect light when its setting is off or the camera cannot run it (`ScreenIndirect::begin`), and with it, where ambient occlusion has not run, the ambient occlusion stage and depth pyramid it made (`Opaque::encode_ambient_occlusion`), which hold no history (SI-1). Whether or not they ran, the whole DiligentFX context, Crystal's histories with it, is rebuilt at any render, finished or not, whose TAA flag differs from the context's (F-5): after a finished frame with TAA and Crystal, an unfinished render with antialiasing off or SMAA and Crystal on rebuilds it, the retry with TAA on rebuilds it again, and Crystal restarts although it ran in both. Stages that keep their objects across a render without them continue from their kept run: ambient occlusion's temporal pass and the ray-traced shadows; FSR too, since `Fsr::settle` ends its kept state only after an unfinished render that ran it (U-2). This follows the stages' own rule that a method that does not run keeps no history; moving the release to settle time would add machinery for a frame that is already a hitch.
- **Raised by.** A frame, finished or not, with a projection other than `perspective`'s infinite reversed-Z (TAA, Crystal SSR and FSR then do not run), diagnostics layers that disable TAA or FSR, a setting that switches the stage off for a frame.
- **Stays triggered while.** Nothing: it lasts one frame after each gap.
- **Source.** DiligentFX (DFX-12), applied to the last finished frame; the FSR side is SGL3D's adaptation of AMD's reset guidance. Code: `view/history.rs` (`Kept::settle`, `StageHistory::begin`), `view/post_fx.rs` (`PostFx::prepare`), `stages/antialiasing/fsr.rs` (`Fsr::prepare`).
- **Guard.** `kept_state_survives_an_unfinished_frame_and_ends_when_the_stage_skips_one` (`view/history.rs`) guards the kept run; `turning_the_bounce_off_releases_what_it_made` (`stages/opaque/screen_indirect/tests.rs`) that screen-space indirect light releases what it made in a render without it; none guards which stages release their targets in an unfinished render.
- **Status.** Rule.

### Antialiasing context and jitter

#### F-5 The DiligentFX context follows the TAA flag

- **Rule.** When `Effective::taa` changes, the view creates a new DiligentFX context, which starts without history; when neither TAA nor Crystal SSR runs it drops the context. A new context discards TAA's accumulation and closest-motion history, its previous depth and Crystal SSR's histories, and its first frame is unjittered (F-7). TAA and Crystal SSR ramp up from the next frame.
- **Raised by.** The antialiasing setting, TAA taking over while FSR is chosen but not running or after an FSR failure, a non-`perspective` projection, the TAA diagnostics layer.
- **Stays triggered while.** Nothing: it needs a setting or projection change. Turning TAA on or off also restarts Crystal SSR, whose inputs did not change.
- **Source.** SGL3D's own (one context shared as in Hydrogent). Code: `renderer/frame.rs` (`render_view`), `view/post_fx.rs` (`PostFx::new`).
- **Guard.** None.
- **Status.** Rule.

#### F-6 Crystal SSR is released when Crystal is not the method

- **Rule.** When a frame's screen-space method is not Crystal (SSR off, Velvet chosen, or a non-`perspective` projection), the DiligentFX context drops its SSR effect; the next Crystal frame starts a new effect with no history.
- **Raised by.** Settings, projection changes.
- **Stays triggered while.** Nothing: it needs a setting or projection change.
- **Source.** SGL3D's own; a new effect resets as DFX-49 describes. Code: `renderer/frame.rs` (`render_view`), `view/post_fx.rs` (`PostFx::release_screen_space_reflections`).
- **Guard.** None.
- **Status.** Rule.

#### F-7 TAA withholds jitter on its reset frames

- **Rule.** When the DiligentFX context resets its accumulation (F-1, F-4, F-5), `PostFx::prepare` returns zero jitter, so that frame rasterizes unjittered (TAA's mip bias of -0.5 still applies) and the camera history records zero jitter. FSR jitters every frame, reset or not. Jitter resumes on the next frame.
- **Raised by.** Any TAA reset.
- **Stays triggered while.** Nothing beyond the resets that cause it.
- **Source.** Hydrogent `HnPostProcessTask::Prepare`, faithful. Code: `view/post_fx.rs` (`PostFx::prepare`), `renderer/frame.rs` (`render_view`).
- **Guard.** None.
- **Status.** Rule.

#### F-8 Reset flags handed to FSR

- **Rule.** FSR resets when the frame's history is invalid, when it did not run in the view's last finished frame, when a render that ran it was not finished (U-2), when the render size differs from that frame's, or when its context is new. FSR receives neither `rigid_objects_changed` nor `visibility_changed`. See U-1 to U-4 for what FSR does with the reset.
- **Raised by.** F-1 to F-4, a change of FSR quality.
- **Stays triggered while.** The game leaves frames unfinished (U-2).
- **Source.** AMD FSR integration guidance (reset on cuts). Code: `stages/antialiasing/fsr.rs` (`Fsr::prepare`, `Fsr::settle`).
- **Guard.** `upscaled_restarts` (`tests/sgl-3d/tests/scenario_fsr_upscaling.rs`).
- **Status.** Rule.

#### F-9 TAA's view-wide re-arm signal

- **Rule.** TAA's `rigid_object_motion` is the scene's rigid-change signal (M-9) or a changed visibility mask (F-10), held until the frame is finished: the changes of the frame that no motion vector describes. Camera motion reaches TAA separately (T-20). TAA's use of the signal is T-21.
- **Raised by.** See M-9 and F-10.
- **Stays triggered while.** See M-9 and F-10.
- **Source.** SGL3D's own. Code: `stages/antialiasing/mod.rs` (`Antialiasing::encode`), `view/post_fx.rs` (`PostFx::temporal_anti_aliasing`).
- **Guard.** As M-9 and F-10.
- **Status.** Rule.

### Scene signals handed to TAA

#### F-10 A changed visibility mask re-arms TAA across the view

- **Rule.** When `FrameInput::visibility_mask` differs in any of its 32 bits from the last finished frame's and history is valid, `HistoryFrame::visibility_changed` is true and TAA's `rigid_object_motion` is raised for the whole view, whether or not any drawn material uses the switched bits. It stays true on every frame until a frame with the new mask is finished; a reset frame reports false. TAA then recovers over its hold.
- **Raised by.** A game switching visibility groups: interior and exterior sets, toggled props, signs blinking by group.
- **Stays triggered while.** The game changes a bit every frame or every few frames, including a bit no material uses: the whole view never reaches TAA's resting stability.
- **Source.** SGL3D's own. Code: `view/history.rs` (`CameraHistory::begin`, `CameraHistory::finish`), `stages/antialiasing/mod.rs` (`Antialiasing::encode`).
- **Guard.** `a_changed_visibility_mask_is_reported_until_a_frame_is_finished` (`view/history.rs`).
- **Status.** Rule.

#### F-11 A render-origin move translates history and resets nothing

- **Rule.** When `Scene::move_origin` moves the origin by a non-zero vector, `CameraHistory::begin` translates the previous camera by the difference in double precision, and the scene translates each instance's pose and submitted pose together, so no history restarts and no motion is written beyond rounding (about the last half-float step of motion). Instance shader data and material parameters are not translated.
- **Raised by.** Streaming worlds that keep the origin near the camera.
- **Stays triggered while.** Nothing. A game that anchors an effect in render-frame coordinates through instance shader data gets one frame of vertex-function motion on each move unless it sets the data again.
- **Source.** SGL3D's own (as Filament keeps antialiasing history across origin snaps). Code: `view/history.rs` (`CameraFrame::translated`), `scene/origin.rs` (`Scene::move_origin`), `scene/instances.rs` (`Instances::move_origin`).
- **Guard.** `a_moved_origin_reprojects_the_same_world_point` (`view/history.rs`); `a_move_translates_records_and_keeps_their_motion`, `a_still_camera_sees_no_motion_across_a_move` (`scene/origin_tests.rs`); `a_render_origin_move_moves_no_surface` (`renderer/shader_tests.rs`); `moving_the_origin_during_a_pan_changes_nothing` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

### More than one view

#### F-12 A view's history follows its scene's frames

- **Rule.** A `View` holds one camera history and a `Scene` one motion history. A frame's views all measure object motion from the scene's last finished frame. A view continues only when that frame rendered it; otherwise F-1 resets it. The scene records what a frame draws once, when `Renderer::render` begins its frame (`Scene::begin_frame`), and commits that record at `finish_frame` (F-13). Each render's attempt carries a flag that `finish_frame` sets, which the renderer and every view the frame rendered hold: a view settles its last render by that flag, so a view left out of the next finished frame still settles its own frame as finished and commits its statistics, frame-probe readback, culling pyramid, exposure and local shadow cache. The frame's views are listed in one `Renderer::render`; two renderers drawing one scene in the same frame is not a supported way to draw several views: the second renderer's `render` begins a new attempt of the scene, superseding the first's, so only the attempt begun last can be committed: called after both renders, the first renderer's `finish_frame` commits nothing and its views reset every frame; called after each render, both commit, the scene commits twice a frame and both renderers' views reset every frame.
- **Raised by.** A view rendered less often than its scene; a view shown again after being left out; a second renderer drawing the same scene in a frame (every view of the first resets, and of both when each renderer finishes before the next renders).
- **Stays triggered while.** The view skips frames: it never accumulates. Two renderers draw one scene every frame: the first's views, or both's, never accumulate.
- **Source.** Filament (per-view history, `FView::commitFrameHistory`), Bevy and Godot (one-frame scene history: `PreviousGlobalTransform`, the per-instance previous transform). Code: `renderer/mod.rs` (`Renderer::render`, `Renderer::finish_frame`), `renderer/view.rs` (`View::settle`, `View::begin_history`), `scene/mod.rs` (`Scene::begin_frame`, `Scene::finish_frame`, `Scene::last_finished`).
- **Guard.** `views_match_their_solo_runs`, `a_view_that_skips_a_frame_restarts` (`tests/sgl-3d/tests/scenario_views.rs`); `finish_frame_of_another_scene_commits_nothing`, `a_view_used_between_render_and_finish_keeps_its_history`, `a_view_left_out_of_a_frame_reads_back_its_own_finished_frame` (`renderer/view_tests.rs`).
- **Status.** Rule.

#### F-13 Commit takes what the frame drew

- **Rule.** `render` records what the frame draws (`Scene::begin_frame`, which returns the frame's attempt id): each instance's pose, model, visibility and shader data, each deformation's slot and revision, each material's shader parameter block, and the rigid-change signal's and static edits' marks up to it. `finish_frame` commits that record, and only for the attempt the scene last began and has not finished (`Scene::finish_frame(attempt)`); an instance added or a block created since the frame began commits nothing. An edit made after the frame's commands are submitted and before `finish_frame` belongs to the next frame: the next frame measures motion from what the finished frame drew, a deformation set since is written in it, and the rigid-change signal and static-edit bounds raised since stay pending for it. Whether such an edit raises the signal is judged against what the next frame follows: the frame being finished as much as the last finished frame (M-9). An edit between `render` and the submission uploads through the queue before the frame's commands run, so it changes what the frame draws, and is not allowed.
- **Raised by.** A loop that renders and submits, updates the scene, then calls `finish_frame`: it loses nothing.
- **Stays triggered while.** Nothing: no history is rejected.
- **Source.** SGL3D's own (S3D-4: the caller's `finish_frame` says the frame was submitted), recording at the frame's scene step as engines capture "previous" there: Wicked Engine 4323a33's `Scene::Update` (`wiScene.cpp:4711`), Bevy 9d12036's `PreviousGlobalTransform` (`crates/bevy_pbr/src/prepass/mod.rs`). Code: `scene/mod.rs` (`Scene::begin_frame`, `Scene::finish_frame`), `scene/instances.rs` (`Instances::prepare_frame`, `Instances::finish_frame`), `scene/deformation.rs` (`InstanceDeformation::finish`), `scene/materials/params.rs` (`ParamBlock::prepare_frame`, `ParamBlock::finish_frame`), `scene/static_edits.rs` (`StaticEdits::begin_frame`, `StaticEdits::finish`).
- **Guard.** `edits_after_submission_belong_to_the_next_frame` (`tests/sgl-3d/tests/scenario_views.rs`); `a_frame_commits_what_it_drew`, `an_unfinished_frame_commits_nothing`, `an_edit_in_the_window_is_judged_against_the_frame_it_follows` (`scene/content_tests.rs`).
- **Status.** Rule.

### Culling and targets

#### F-14 The occlusion pyramid is read only after a continuous frame

- **Rule.** When the camera history has no previous camera (a reset), the last finished frame built no depth pyramid, or the pyramid was recreated for a new render size, the cull stage's early phase tests the frustum alone; the late phase tests occlusion against this frame's depth, so this costs time and never a surface. The next frame reads the pyramid again.
- **Raised by.** Resets, `Settings::occlusion_culling` toggles, resizes.
- **Stays triggered while.** Nothing visible.
- **Source.** Bevy's two-phase culling. Code: `stages/cull.rs` (`Cull::encode_early`, `Cull::finish_frame`).
- **Guard.** `an_object_that_comes_into_view_is_drawn_in_that_frame`, `sections_behind_a_wall_are_culled_and_drawn_again_when_it_moves` (`stages/cull/occlusion_tests.rs`); `an_abandoned_frame_reports_nothing_and_the_next_reports_its_own` (`stages/cull/tests.rs`).
- **Status.** Rule.

#### F-15 The temporal masks exist only while a writer can mark them

- **Rule.** Each view holds the reactive and transparency-and-composition masks (R8, render size) when FSR runs, or when TAA runs and the scene holds a blended, transmissive or moving-surface material or draws glow, mist or heat (`Effective::temporal_masks`); otherwise it releases them and TAA binds no responsive masks. The masks keep no history: the first writer of each frame clears them.
- **Raised by.** The scene's content, decided scene-wide rather than by what the frame draws.
- **Stays triggered while.** Nothing: no history lives in the masks.
- **Source.** AMD's FSR sample for formats and blends; the allocation policy is SGL3D's own. Code: `renderer/effective.rs` (`resolve`), `view/targets.rs` (`SharedTargets::set_temporal_masks`), `stages/transparent/mod.rs` (`Transparent::encode`).
- **Guard.** `the_temporal_masks_are_held_exactly_while_a_writer_can_mark_them` (`renderer/effective.rs`); `temporal_masks_are_allocated_only_while_enabled` (`view/targets.rs`); `fsrs_masks_start_clear_each_frame_with_moving_surfaces` (`stages/transparent/composition_tests.rs`).
- **Status.** Rule.

#### F-16 Settings changes that restart history

- **Rule.** A change of antialiasing choice, FSR version or quality, scene resolution or preset restarts every history (F-2); turning TAA on or off restarts TAA and Crystal SSR (F-5); leaving Crystal restarts Crystal SSR (F-6); `occlusion_culling` affects only the cull stage's early phase (F-14); turning world-space reflections on clears the camera cull's LOD selection and the blended list's, switching geometry to its base level with no history signal (M-13). Ray-traced shadow quality restarts that stage (shadows area).
- **Raised by.** A settings menu.
- **Stays triggered while.** Nothing: each needs a setting change.
- **Source.** SGL3D's own. Code: `renderer/view.rs` (`View::resize`), `renderer/frame.rs` (`render_view`), `stages/prepare.rs` (`Prepare::run_view`).
- **Guard.** As listed under F-2, F-5, F-6, F-14.
- **Status.** Rule.

### Stages without history

- Receiver pass: rebuilds the surface every frame it runs.
- Transmission copy and volume layers: made again every frame that needs them.
- Temporal masks: cleared every frame by their first writer (F-15).
- TAA motion reach: computed from the frame's motion every frame TAA runs; dropped when TAA stops.
- Motion blur: keeps only the frame count for its noise.
- Sky: draws colour and motion from the frame's camera alone.
- Aerial volume: computed whole every frame it is in effect, so a cut, a resize, a new device or an
  unfinished frame needs nothing of it.

## Motion vectors

Geometry, the sky and blended receivers write unjittered motion into the G-buffer's motion target: current minus previous UV (+y down) in RG, previous minus current view depth in B and its validity in A. Motion is measured against the scene's motion history (each moving instance's pose, deformation slot and shader data in the last finished frame, each material's parameter block then) and each view's camera and time history. Where content changes without a motion vector, the transparent stage writes the reactive and transparency-and-composition masks instead. A continuously running game most often trips the rigid-change signal (M-9, raised by every moving instance), the composition mark on every material with a game shader (M-16), and the per-pixel marks of blended surfaces and effects (M-11, M-17a–c).

### Encoding

#### M-1 Motion is limited to two screens

- **Rule.** When a point's previous clip position has w ≤ 0, or its motion's longer axis exceeds 2 screens in UV (`GBUFFER_MOTION_LIMIT` = 2), `gbuffer_encode_motion` replaces the motion with one 2 screens long along its direction (`(2, 0)` for a point straight behind the previous camera), so every consumer reprojects off screen and takes no history at that pixel. The next frame's motion is measured afresh.
- **Raised by.** Fast camera turns, points crossing behind the camera, teleports without `camera_cut`, time jumps through vertex functions.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** Wicked Engine 4323a33 `visibility_velocityCS.hlsl` (positive previous w only); Wicked clamps to one screen, SGL3D to two. Code: `shading/gbuffer.wgsl` (`gbuffer_encode_motion`).
- **Guard.** `a_turn_past_the_view_moves_every_pixel_off_screen_along_the_turn` (`stages/opaque/tests.rs`).
- **Status.** Rule.

#### M-2 Depth motion and its validity

- **Rule.** `gbuffer_encode_velocity` writes previous minus current view depth (clip w) in B and 1 in A only when the previous w is above 0 and the depth change is at most 65504 m; otherwise B and A are 0. The sky always writes 0 and 0. Ambient occlusion rejects its history where A is 0; TAA's depth test then uses the camera reprojection alone. An orthographic camera gives B 0 and A 1, and no consumer of depth motion runs with one.
- **Raised by.** Points behind the previous camera, the sky.
- **Stays triggered while.** On the sky, every frame, by design.
- **Source.** NRD's 2.5D motion convention. Code: `shading/gbuffer.wgsl` (`gbuffer_encode_velocity`), `stages/opaque/backdrop.wgsl` (`backdrop_fs`).
- **Guard.** `temporal_visibility_accumulates_and_rejects_invalid_history` (`stages/opaque/ambient_occlusion/tests.rs`).
- **Status.** Rule.

#### M-20 Motion is unjittered

- **Rule.** Every frame, geometry measures motion between `stable_view_projection` (this frame's unjittered view-projection) and `previous_view_projection` (the last finished frame's, unjittered); jitter is applied only to the raster position, as a clip-space offset of 2 × jitter × w. Consumers that reproject rasterized data re-apply jitter themselves (the DiligentFX previous camera, the cull pyramid).
- **Raised by.** Every frame.
- **Stays triggered while.** Nothing. The current and previous clip positions are evaluated in the same order (the view-projection times the pose-transformed position), so an unchanged camera and pose write exactly zero motion at any distance from the origin; until the 2026-11-18 audit's B-12 the previous one was `(previous view-projection × previous model) × position`, which wrote 1.9e-4 px of motion 100 m sideways of the origin. Motion goes through a precomposed projection-times-view while raster positions apply view then projection, so the raster position and the motion differ by rounding far from the origin.
- **Source.** Diligent `TemporalAntiAliasing::GetJitteredProjMatrix` and AMD's FSR jitter documentation. Code: `stages/prepare.rs` (`Prepare::run_view`), `shading/vertex.wgsl` (`scene_raster_clip`), `shading/material_shader.wgsl` (`vertex`).
- **Guard.** `motion_vectors_match_the_script`, `a_still_scene_far_from_the_origin_writes_no_motion`, `jitter_is_sub_pixel_and_never_in_the_motion` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### M-3 A reset frame drops camera and time motion only

- **Rule.** On a reset frame (F-1) the previous view-projection and previous time equal this frame's, so camera motion and the time term of vertex functions are zero, while instance pose, deformation, instance-data and parameter changes since the last finished frame still write motion. Every temporal consumer resets on that frame; motion blur skips it (M-15).
- **Raised by.** Resets.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own. Code: `view/history.rs` (`CameraHistory::begin`).
- **Guard.** `a_camera_cut_frame_is_not_blurred` (`stages/motion_blur/tests.rs`).
- **Status.** Rule.

### Instances

#### M-4 Static instances write no motion

- **Rule.** A `Mobility::Static` instance's record measures motion from its current pose, so it writes no object motion, even in the frame `set_instance` moves it; that move uncovers its old footprint with no motion vector and raises the rigid-change signal (M-9).
- **Raised by.** Moving a static instance.
- **Stays triggered while.** The game moves a static instance every frame, which also raises M-9 every frame.
- **Source.** SGL3D's own. Code: `scene/instances.rs` (`Instance::previous_pose`).
- **Guard.** `static_instances_write_no_motion_and_moving_ones_measure_from_the_last_submitted_frame` (`scene/content_tests.rs`).
- **Status.** Rule.

#### M-5 A moving instance has no motion where its last pose does not apply

- **Rule.** A moving instance measures motion from its pose in the last finished frame only when it was `visible` then and its model is the same; a new instance, one whose state names another model, one hidden in the last finished frame and one at a reused index write no object motion for that frame. Motion resumes the next frame.
- **Raised by.** Spawns, model swaps, showing a hidden instance.
- **Stays triggered while.** Nothing: it lasts one frame. Each of these also raises M-9.
- **Source.** SGL3D's own. Code: `scene/instances.rs` (`Instance::previous_pose`, `Instances::finish_frame`).
- **Guard.** `instance_motion_uses_last_submitted_model_after_abandoned_updates`, `static_instances_write_no_motion_and_moving_ones_measure_from_the_last_submitted_frame`, `reused_indices_show_no_earlier_geometry_record_or_motion` (`scene/content_tests.rs`).
- **Status.** Rule.

#### M-6 A moving instance not posed again stops moving

- **Rule.** When a moving instance's pose changed in the last finished frame and `set_instance` is not called again before the next frame, `Instances::prepare_frame` rewrites its record with the previous pose equal to the current, so it writes no motion that frame.
- **Raised by.** Movers updated at a lower rate than the frame rate (motion alternates between a double step and zero, correct per frame).
- **Stays triggered while.** Nothing: it follows the game's updates.
- **Source.** SGL3D's own. Code: `scene/instances.rs` (`Instances::finish_frame`, `Instances::prepare_frame`).
- **Guard.** `static_instances_write_no_motion_and_moving_ones_measure_from_the_last_submitted_frame` (`scene/content_tests.rs`).
- **Status.** Rule.

#### M-7 Deformation motion only from a visible last frame

- **Rule.** A deforming instance's previous positions are the position slot it showed in the last finished frame only while it was `visible` then; in its first frame or after a hidden frame, previous positions equal current ones and deformation writes no motion that frame. Deformation never raises the rigid-change signal.
- **Raised by.** Spawns, showing a hidden deforming instance.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own (two position slots). Code: `scene/deformation.rs` (`InstanceDeformation::prepare`, `InstanceDeformation::finish`), `shading/vertex_pull.wgsl` (`scene_source_vertex`).
- **Guard.** `a_moving_joint_writes_its_screen_displacement_as_motion` (`scene/deformation/tests.rs`) covers motion being written; the first-frame zero is unguarded.
- **Status.** Rule.

#### M-8 Vertex-function motion and the parameter block's first frame

- **Rule.** Passes that write motion evaluate the material's vertex function twice: at this frame and at the last finished frame's time, phase, parameter block, instance shader data and pose. An instance's shader data with no finished frame yet takes this frame's as previous (no motion). A parameter block that has been in no finished frame takes this frame's as previous, as an instance's shader data does, so a material that gains a shader, or changes it, which gives it a new block, writes no motion from its parameters in its first frame. The next frame measures from the block the first frame drew.
- **Raised by.** Adding a material with a game shader mid-session, or changing a material's shader.
- **Stays triggered while.** Nothing: the parameters' motion resumes in the block's second frame.
- **Source.** Godot's motion-vector variant (`scene_forward_clustered.glsl`, `vertex_shader()` called twice). Code: `shading/material_shader.wgsl` (`vertex`, `material_vertex_context`), `scene/materials/params.rs` (`ParamBlock::set`, `ParamBlock::finish_frame`), `scene/instances.rs` (`Instance::shader_record`).
- **Guard.** `a_material_that_gains_a_shader_writes_no_parameter_motion_in_its_first_frame` (`tests/sgl-3d/tests/scenario_views.rs`); `a_frame_commits_what_it_drew` (`scene/content_tests.rs`); `motion_follows_the_shaders_time_and_parameters` (`renderer/shader_tests.rs`).
- **Status.** Rule.

#### M-9 The scene's rigid-change signal

- **Rule.** The scene raises `rigid_objects_changed` for the changes no motion vector describes: when a `visible` instance is added; when `set_instance` changes `visible` of an instance; when it changes the pose (exact matrix inequality) or the model of an instance visible before or after, unless the instance is `Mobility::Moving`, visible now, and its last-submitted pose applies (it was visible in the last finished frame and that frame's model is the one it now names: M-5, and, for an edit made while a frame is rendered and not yet finished, that frame drew it shown with that model too, since finishing it makes its record the pose the next frame measures from: F-13), whose move then writes motion; when an instance visible or shown in the last finished frame is removed; when `Scene::set_model` replaces geometry of a model an instance names and is visible, or names and has a submitted record (a moving instance visible in the last finished frame), or that the frame being rendered showed it with (the submitted record's model is not compared: a swap that made it differ already raised the signal, which only a finished frame begun after the swap, and so drawing the new model, retires); and when `Scene::set_material` makes an edit that changes what its users draw (`SurfaceMaterial::draws_other_geometry`). So it is raised for an appearance, a departure, showing or hiding, a model swap, any move of a `Mobility::Static` instance (M-4) and a move of a moving instance whose last-submitted pose does not apply, and not for a moving instance's move from its submitted pose, wherever it goes, on screen or off. No culling, distance or screen test applies. A finished frame clears what was raised before it rendered (`Scene::begin_frame` records the count raised, `Scene::finish_frame` commits it); what is raised after a frame began stays raised for the next. While it is raised, TAA's `rigid_object_motion` re-arms full rejection over the whole view (T-21), which recovers over TAA's hold. A moving instance's move instead arms the pixels it covers and leaves through its motion: its own pixels and the 1-pixel band of the closest-motion search below a pixel a frame (T-22, T-9), and the motion reach from a pixel to a 32-pixel tile a frame (T-23, M-14), as for deformation and vertex functions, which never raise it. Why the view-wide signal is needed for the rest: what it uncovers has no motion of its own and its previous neighbourhood may already hold it, so neither the depth test nor any per-pixel rule sees the change (T-17, `taa_adaptive_resting_thin_occluder_departure_does_not_leave_a_ghost`).
- **Raised by.** Spawns, despawns, showing and hiding, model swaps, streaming, moving a static instance, geometry replacement and drawing-changing material edits.
- **Stays triggered while.** The game adds, removes, shows, hides or swaps the model of a visible-flagged instance, moves a static one, or replaces geometry or edits a material in a way that changes what is drawn, every frame or every few frames, anywhere in the scene, on or off screen: the whole view never reaches TAA's resting stability.
- **Known limits.** A moving instance that moves faster than the motion reach covers (more than about a 32-pixel tile a frame, a jump, or out of the view in one frame) leaves its old outline to the depth test and the categorical sky alone (T-17, T-16), as deformation does (T-23).
- **Source.** Filament's global rigid-object signal (`PostProcessManager.cpp` `taa`, re-arm when objects moved), narrowed by SGL3D to changes without a motion vector because its hold is per pixel and the motion reach covers what moves (2026-11-18 history audit, B-1), and extended to departures, geometry and material edits. Code: `scene/instances.rs` (`Instances::geometry_changed`, `Scene::add_instance`, `Scene::set_instance`, `Scene::remove_instance`, `Instances::prepare_frame`, `Instances::finish_frame`), `scene/models.rs` (`Scene::set_model`), `scene/materials.rs` (`Scene::set_material`), `stages/antialiasing/mod.rs` (`Antialiasing::encode`).
- **Guard.** `rigid_recovery_tracks_departures_until_submission_and_ignores_unchanged_poses`, `rigid_recovery_leaves_moves_with_motion_to_the_pixels`, `material_edits_that_change_what_is_drawn_rearm_until_submission` (`scene/instances.rs`); `a_frame_commits_what_it_drew`, `an_unfinished_frame_commits_nothing`, `an_edit_in_the_window_is_judged_against_the_frame_it_follows` (`scene/content_tests.rs`); `appearance_and_departure_at_rest_resolve_within_the_hold`, `a_static_instance_moved_at_rest_resolves_within_the_hold`, `material_edits_at_rest_resolve_within_the_hold`, `at_rest_taa_holds_silhouettes_beside_a_rigid_mover_still`, `a_rigid_card_moves_from_rest`, `a_disocclusion_recovers` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### M-13 LOD switches change geometry without a signal

- **Rule.** The camera's GPU cull and the blended list choose each candidate's level every frame, the coarsest whose projected error is at most 0.5 px (`LOD_PIXELS`), with no hysteresis; a switch changes the triangles drawn while motion is measured on the new level, and raises neither the rigid-change signal nor a mask. While world-space reflections run, both use the base level only.
- **Raised by.** Camera distance or field-of-view changes, moving objects, toggling world-space reflections.
- **Stays triggered while.** An object or camera holds near a threshold distance and flips level every frame; the change is bounded to half a pixel of error.
- **Source.** SGL3D's own error metric over Bevy's meshlet culling form. Code: `stages/cull.wgsl` (`cull_level`), `view/lod.rs` (`LodSelector`), `stages/prepare.rs` (`Prepare::run_view`).
- **Guard.** `gpu_lod_is_never_coarser_than_the_cpu_admits` (`stages/cull/tests.rs`) guards selection only.
- **Status.** Rule.

### Sky and views without motion

#### M-10 Sky motion is the camera's rotation and the panorama's turn

- **Rule.** The sky writes the motion of its direction at infinity through both cameras (rotation only, camera translation ignored), encoded as M-1, with depth validity 0; where the current camera gives a direction no positive w (an orthographic camera) it writes zero motion. Where the frame and the last submitted frame both show a panorama (`Backdrop::Environment`), the previous direction is this frame's turned about +Y by the last frame's yaw less this frame's (`Frame::previous_backdrop_yaw`, from the camera history, F-1): the direction the same point of the panorama lay in, as an object's previous pose carries its vertices. After a reset (F-1), or where either frame's backdrop is a colour, nothing turns. Scope: every sky pixel.
- **Raised by.** Every frame; a game turning `Backdrop::Environment::yaw` adds the turn.
- **Stays triggered while.** Nothing. A turning panorama is motion, as a turning camera is: TAA arms the hold where it moves 0.01 px a frame or more (T-22, T-23) and reprojects it, and FSR and motion blur follow it.
- **Known limits.** The turn is the f32 difference of the two yaws: a yaw the game lets grow without wrapping loses precision in the panorama's sampling and its motion alike.
- **Source.** Diligent's `EnvMap.psh` `GetMotionVector`, Bevy's background motion vectors (`background_motion_vectors.wesl`) for the camera's rotation; neither turns its environment (Diligent's `g_EnvironmentRotation`, Bevy's `Skybox::rotation`) in the motion, which is SGL3D's own (2026-11-18 history audit, O-11). Code: `stages/opaque/backdrop.wgsl` (`sky_motion`); `view/history.rs` (`CameraHistory::set_backdrop`); `renderer/view.rs` (`View::begin_history`), with `view::backdrop_yaw`, which takes a procedural sky as yaw 0.
- **Guard.** `a_turn_past_the_view_moves_every_pixel_off_screen_along_the_turn`, `an_orthographic_camera_records_finite_motion` (`stages/opaque/tests.rs`); `motion_vectors_match_the_script` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`); `a_turning_backdrop_moves_the_sky` (`tests/sgl-3d/tests/scenario_history_taa.rs`): a still camera under a turning panorama, a camera turning with it, a cut, and a colour replaced by a panorama, against the script.
- **Status.** Rule.

#### M-10b A backdrop's brightness, colour or environment changes without motion or mask

- **Rule.** A change of the backdrop's brightness, its colour, the environment its panorama comes from, or between a colour and a panorama is a shading change: the sky writes only M-10's motion and marks neither temporal mask. TAA follows it through its box at the strength the pixel has (at rest each resolve leaves at most (1 − 0.1)(1 − 0.036) = 0.868 of a change where the neighbourhood is uniform); FSR through its own shading-change and luminance tests. A turn of the panorama's yaw is motion (M-10). Scope: the sky's pixels.
- **Raised by.** A game fading, recolouring or swapping the backdrop.
- **Stays triggered while.** The backdrop keeps changing: at rest the sky trails a continuous fade by the relaxed blend, about 5 frames to halve a step.
- **Source.** Diligent's `EnvMap.psh` and Bevy's background motion vectors write the camera's motion only and mark nothing. AMD's FSR documentation (`super-resolution-upscaler.md`, "Transparency and composition mask") offers that mask for shading motion vectors do not match, such as texture animation; SGL3D does not mark the sky: a backdrop change is the same over every neighbourhood of the sky, which the box follows, and FSR detects shading change itself. Settled by the 2026-11-18 history audit (O-11). Code: `stages/opaque/backdrop.wgsl` (`backdrop_color`, `sky_motion`).
- **Guard.** `a_backdrop_that_changes_at_rest_converges_through_the_box` (`tests/sgl-3d/tests/scenario_history_taa.rs`): a colour step at rest converges within the resting blend's bound every frame.
- **Status.** Rule.

#### M-19 Probe captures and shadow views carry no motion

- **Rule.** A probe-capture face's previous view-projection is its view-projection and its frame data's previous time is its own; shadow views have none. These views write no camera or time motion.
- **Raised by.** Probe captures and shadow passes.
- **Stays triggered while.** Nothing.
- **Source.** SGL3D's own. Code: `view/mod.rs` (`ViewData::probe_face`, `frame_uniform`).
- **Guard.** None.
- **Status.** Rule.

### Surfaces that write no motion of their own, and the masks they write

#### M-12 Receivers write their own motion

- **Rule.** When the scene holds a blended receiver of screen-space reflections, a screen-space method, TAA, FSR or motion blur runs, and the camera's blended list holds receivers, the receiver pass draws them strictly nearer than the opaque depth and replaces the G-buffer's motion (RG, B and A) at their pixels with the receiver's unjittered motion; what is seen through a receiver then reprojects by the receiver.
- **Raised by.** Glass, water and other blended receivers.
- **Stays triggered while.** Nothing.
- **Source.** HDRP's transparent depth prepass and motion vectors. Code: `stages/transparent/mod.rs` (`Transparent::encode_receivers`), `shading/geometry.wgsl` (`receiver_fs`).
- **Guard.** `a_receiver_writes_its_motion_and_the_surface_depth_not_the_opaque_depth` (`stages/transparent/receiver_tests.rs`).
- **Status.** Rule.

#### M-11 Blended surfaces write masks, not motion

- **Rule.** Blended and transmissive surfaces that are not receivers write no depth and no motion, so their pixels reproject by the opaque surface behind. While the frame has the temporal masks, each blended draw, receivers included (U-12), writes reactive = its own share capped at 0.9, where its own share is its coverage less what it transmits and, for a material that keeps its specular, 1 − (1 − that) × (1 − its mean split-sum reflectance); and transparency and composition = its coverage (its alpha when blended, the whole pixel when transmissive). Transparency and composition accumulate as m × (1 − mask) + mask; reactivity as the same union in units of the cap, r + mask × (1 − r / 0.9), so a pixel's reactivity never exceeds 0.9 (U-10). The masks are cleared each frame.
- **Raised by.** Every blended or transmissive surface drawn, moving or not.
- **Stays triggered while.** The surface is on screen: a still window is marked every frame, so TAA rectifies at its pixels in proportion to the marks and never fully settles there; FSR treats it as AMD documents.
- **Source.** AMD FidelityFX SDK 1.1.4 FSR documentation (reactive mask: alpha for alpha-blended objects, at most about 0.9) and sample (`translucencyps.hlsl`; the union, `fsrapirendermodule.cpp`), Filament's blending. Code: `shading/geometry.wgsl` (`blended_color`, `blended_masked_fs`), `shading/temporal_masks.wgsl` (`temporal_reactive`), `view/targets.rs` (`mask_targets`).
- **Guard.** `blended_surfaces_leave_depth_and_motion_unchanged`, `blended_surfaces_write_fsrs_masks` (`stages/transparent/blended_tests.rs`); `overlapping_reactive_writers_stay_within_amds_cap`, `a_receiver_marks_the_masks_as_a_blended_surface_does` (`stages/transparent/composition_tests.rs`).
- **Status.** Rule.

#### M-16 Surfaces whose shading changes where they stand still are marked 1

- **Rule.** While the frame has the temporal masks and the scene holds an opaque or masked material whose shading may change where its geometry stands still (its normal layers move, or it has a game shader), the transparent stage first clears the masks and then marks transparency and composition 1, leaving reactivity, over the camera's sets of the materials whose shading changes in this frame (`Scene::shading_changes`). A material's shading changes in a frame when: its normal layers move (`Material::record_moves`), every frame; its shader's module reads the frame's time or phase anywhere (`Shader::reads_time`, which covers the surface function and, through `custom`, what the vertex function passes it), every frame; or its shader parameter block differs from the last submitted frame's (`Material::parameters_changed`; a block that was in no finished frame has none and counts as unchanged, as an instance's data does, the rigid-change signal M-9 covering the edit that gave the material its shader), or the shader data of an instance of a model drawn with it differs from that instance's in the last submitted frame (`Instances::shader_data_edited`), in that frame alone. Never for having a shader alone; a write of the values already in effect is no change. An instance's data edit marks the material, so every instance drawn with it, not that instance alone. The sets are drawn at the opaque depth, tested equal, a masked material's cut-out texels discarded; but a masked material changed by an edit is drawn over its whole footprint at or in front of the opaque depth, its cut-out texels included, since the edit may have cut out texels that showed in the last submitted frame and the frame before marked nothing there. Scope: the material's pixels. Recovery: the frame after the change (TAA then holds them as T-25 says).
- **Raised by.** Water and scrolling normal layers; a game shader that reads `ctx.time` or `ctx.phase`; a game editing a shader's parameters or an instance's shader data.
- **Stays triggered while.** Normal layers move or the shader reads the time (also where it reads it without changing what it draws: the test is of the module's code), or the game changes the parameters or instance data every frame.
- **Source.** AMD FidelityFX SDK 1.1.4 sample animated textures (`AnimatedTexture.hlsl`, transparency and composition 1) and FSR documentation (`super-resolution-upscaler.md` 131, 192: the mask is for shading that motion vectors do not follow); the time test is Godot b130438's `is_animated()` reading of a material's code; the edit test and the footprint of an edited masked material are SGL3D's own, from the scene's history of parameter blocks and instance data. Code: `scene/shaders.rs` (`Scene::shading_changes`, `ShadingChanges::of`), `scene/materials/params.rs` (`ParamBlock::changed`), `scene/instances.rs` (`Instances::shader_data_edited`), `stages/transparent/mod.rs` (`Transparent::encode_composition`), `view/pipelines/pass.rs` (`GeometryPass::TemporalComposition`), `shading/geometry.wgsl` (`temporal_composition_fs`), `view/targets.rs` (`composition_targets`).
- **Guard.** `moving_opaque_surfaces_mark_fsrs_composition_mask`, `a_material_set_moving_marks_fsrs_composition_mask`, `a_shader_surface_marks_composition_only_in_frames_its_shading_changes`, `a_shader_reading_the_time_marks_composition_every_frame` (`stages/transparent/composition_tests.rs`); `a_still_shader_surface_holds_taas_resting_bound` (`tests/sgl-3d/tests/scenario_history_masks.rs`); `a_dissolve_by_shader_parameters_at_rest_resolves_within_the_hold`, `a_dissolve_by_instance_data_at_rest_resolves_within_the_hold` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### M-17a Heat distortion marks composition

- **Rule.** While the frame has the temporal masks, heat distortion writes transparency and composition = its vertex weight where the weight is above 0 and the heat lies in front of the opaque depth, else 0, accumulated as the blended draws accumulate theirs; it writes no reactivity. Its pixels keep the motion of the surface behind.
- **Raised by.** Heat distortion geometry with `Settings::heat_distortion` on.
- **Stays triggered while.** The heat geometry is on screen, every frame by design.
- **Source.** AMD FidelityFX SDK 1.1.4 `super-resolution-upscaler.md` (transparency and composition for shading motion vectors do not follow). Code: `stages/transparent/heat.wgsl` (`fs_masked`).
- **Guard.** `heat_distortion_marks_the_composition_mask` (`stages/transparent/composition_tests.rs`).
- **Status.** Rule.

#### M-17b Glow marks reactivity by the light it adds to the displayed frame

- **Rule.** While the frame has the temporal masks, additive glow writes reactive = max(r, g, b) × a, capped at 0.9, of its soft-faded radiance times the frame's exposure multiplier, and transparency and composition 0, accumulated as the blended draws accumulate theirs (M-11). The multiplier is the one the tone map and FSR apply: the fixed exposure's in a frame of fixed exposure; with automatic exposure, the one the last rendered frame adapted to, since this frame's is metered from the frame the glow is drawn onto (it differs by one frame's adaptation step, and in a view's first frame of automatic exposure it is 0, so that frame's glow marks nothing; that frame restarts history anyway).
- **Raised by.** Glow effects (`Scene::update_effects`), unless the effects diagnostics layer is off.
- **Stays triggered while.** The glow is on screen, every frame by design; a glow whose displayed light, brightest channel times alpha, reaches 0.9 marks its pixels at the cap.
- **Source.** AMD FidelityFX SDK 1.1.4 sample `particlerender.hlsl` 249–254 (reactivity max(r, g, b) × alpha of its particle colour, at the sample's exposure of 1); FSR multiplies its input by the exposure before every comparison (`ffx_fsr2_common.h` `PrepareRgb`); the 0.9 cap per AMD's documentation. Code: `stages/transparent/glow.wgsl` (`glow_soft_masked_fs`), `stages/transparent/effects.rs` (`Effects::depth_group`), `stages/exposure.rs` (`Exposure::view`).
- **Guard.** `glow_marks_reactivity_by_what_it_adds_to_the_displayed_frame` (`stages/transparent/composition_tests.rs`); `additive_effects_mark_fsr_reactivity_for_their_frame_only` (`stages/antialiasing/fsr/tests.rs`).
- **Status.** Rule.

#### M-17c Mist marks composition by alpha

- **Rule.** While the frame has the temporal masks, ground mist writes reactive 0 and transparency and composition = its alpha, accumulated as the blended draws accumulate theirs.
- **Raised by.** Mist positions (`Scene::update_mist`) in a frame with atmosphere.
- **Stays triggered while.** The mist is on screen, every frame by design.
- **Source.** AMD FidelityFX SDK 1.1.4 sample `translucencyps.hlsl` (translucent materials). Code: `stages/transparent/mist.wgsl` (`mist_masked_fs`).
- **Guard.** None.
- **Status.** Rule.

### Consumers' own thresholds

#### M-14 TAA's motion reach

- **Rule.** Every frame TAA runs, the reach takes the G-buffer's motion in render pixels, clamps each vector to 2 tiles (64 px, `TILE_SIZE` = 32), and writes for each 32 px tile the largest motion in its 3×3 tile neighbourhood; TAA arms its recovery hold where that is at least 1 px a frame (T area). Motion beyond 64 px reads as 64 px.
- **Raised by.** Camera motion (every tile), any mover.
- **Stays triggered while.** Anything moves at 1 px a frame or more: a slow continuous mover keeps the 96 px around it armed every frame, and a camera turning at 1 px a frame or more keeps the whole view armed.
- **Known limits.** A pixel a surface left is armed only while the surface is still within its 3x3 tile neighbourhood, so a deforming surface or a moving instance (M-9) that moves more than about a tile (32 px) a frame leaves its old outline to the depth test and the categorical sky (T-23).
- **Source.** Wicked Engine 4323a33 motion-blur tile passes after McGuire et al. 2012; the clamp is McGuire's, which Wicked omits. Code: `stages/antialiasing/reach.rs` (`Reach::encode`), `shading/motion_tiles.wgsl` (`motionblur_velocity`, `motionblur_neighborhoodMaxVelocity`).
- **Guard.** `at_rest_taa_holds_silhouettes_beyond_the_reach_of_motion_still`, `at_rest_taa_holds_silhouettes_within_the_reach_of_slow_motion_still` (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### M-15 Motion blur's thresholds

- **Rule.** Motion blur does not run on a reset frame (its moving instances' motion reaches back past the reset) or without a positive shutter; it copies a tile whose neighbourhood's largest blur is under 1 px, averages along the pixel's own motion where the largest and smallest blur differ by under 1 px, and clamps each blur vector to 64 px (2 tiles).
- **Raised by.** Resets, slow or uniform motion.
- **Stays triggered while.** Nothing.
- **Source.** Wicked Engine 4323a33 `motionblurCS.hlsl` (early-exit and cheap variants), clamp after McGuire et al. 2012; the reset skip is SGL3D's own. Code: `stages/motion_blur.rs` (`MotionBlur::encode`), `stages/motion_blur.wgsl` (`motionblur`).
- **Guard.** `a_moving_square_blurs_over_the_shutters_share_of_its_motion`, `a_static_frame_is_unchanged_by_motion_blur`, `a_camera_cut_frame_is_not_blurred` (`stages/motion_blur/tests.rs`).
- **Status.** Rule.

#### M-18 The DiligentFX inputs pass

- **Rule.** Every frame the DiligentFX context runs, its inputs pass converts the surface's motion to NDC (RG × (2, −2)) and passes depth motion and validity through, and converts depth to a projection with its far plane at 10 000 m (`FAR_PLANE`): surfaces beyond it are at the far plane, which TAA and Crystal SSR treat as background.
- **Raised by.** Every TAA or Crystal frame; scenery beyond 10 km.
- **Stays triggered while.** Scenery beyond 10 km is on screen: TAA treats it as sky.
- **Source.** SGL3D's adapter; the far plane per `sgl-post-fx` PROVENANCE DFX-33. Code: `view/post_fx_inputs.wgsl` (`fs_main`), `view/post_fx.rs` (`PostFx::begin`).
- **Guard.** None.
- **Status.** Rule.

## Temporal antialiasing (TAA)

TAA keeps one accumulated colour history with a confidence in its alpha, and a
closest-motion history whose B and A channels carry two recovery holds. SGL3D runs it
with `BICUBIC_FILTER | MOTION_ADAPTIVE_HISTORY` and `TemporalStabilityFactor` 0.88, so
history at rest is only lightly rectified and is pulled back to full strength by the
camera, motion, depth disocclusion, the scene's signal and the responsive masks. A
continuously running game most often trips the shader-material composition mark (T-29),
the masks' hold (T-25), a pixel's own sub-pixel motion (T-22) and the reach of
anything moving (T-23): each keeps pixels at full strength in a picture that looks
still. The view-wide signals (T-20, T-21) need the camera to move beyond f32 rounding,
or content to change with no motion vector.

### Resting and full strength

Every adaptive rule acts through one strength `s` in [0, 1] per pixel
(`ComputeTemporalAccumulationPS`):

- **At rest** (`s = 0`): box strength 0.1 (history moves a tenth of the way to its
  clipped value), current weight 0.12 × 0.3 = 0.036, so history keeps at most 0.964.
- **Full strength** (`s = 1`): box strength 1 (history fully clipped), current weight
  0.12, so history keeps at most 0.88.
- **Hold.** A recovery hold is 12 frames. The motion hold (B channel) ramps strength
  linearly from 1 to 0 over its 12 frames; the mask hold (A channel) keeps full strength
  for all 12 frames.

In the entries below, "full strength" means `s = 1` and "arms the hold" means the
pixel's motion hold is set to 12.

### History restarts

#### T-1 Accumulation reset

- **Rule.** When the effect has no previous frame (first execution, or buffers just
  recreated), or `FrameDesc::index` is not the previous executed index plus one
  (wrapping), or `TemporalAntiAliasingAttribs::reset_accumulation` is non-zero, the
  resolve writes the current colour with confidence 0.5, arms the hold and clears the
  mask hold, over the whole view. Accumulation recovers through the confidence ramp
  (T-26): history weight 0.88 after about 7 frames, 0.964 after about 26.
- **Raised by.** SGL3D's reset decision (T-4), which passes the camera history's frame
  count as the index.
- **Stays triggered while.** Nothing: it lasts one frame, unless T-4 fires again.
- **Source.** DiligentFX `TemporalAntiAliasing.cpp` (`UpdateConstantBuffer`), faithful
  but for "no history" as `None` rather than `~0u` (DFX-50).
  Code: `crates/sgl-post-fx/src/temporal_anti_aliasing.rs` (`AccumulationBufferInfo::update_constant_buffer`).
- **Guard.** `taa_feature_change_restarts_confidence_and_convergence`
  (`tests/sgl-post-fx/tests/execute.rs`); `a_cut_a_resize_and_another_scene_restart_history`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-2 Buffers recreated on a size or motion-format change

- **Rule.** When the render size changes, or `MOTION_ADAPTIVE_HISTORY` is toggled (the
  closest-motion format changes between RG16F and RGBA16F), the effect recreates its
  accumulated and closest-motion buffers cleared to zero and forgets its last frame, so
  the next frame is a T-1 reset over the whole view.
- **Raised by.** Window resize, render-scale or FSR quality change; a flag change by a
  direct `sgl-post-fx` caller.
- **Stays triggered while.** Nothing: it needs a setting change.
- **Source.** DiligentFX `AccumulationBufferInfo::Prepare`, modified so recreation resets
  (DFX-53); the format check is SGL's own.
  Code: `crates/sgl-post-fx/src/temporal_anti_aliasing.rs` (`AccumulationBufferInfo::prepare`).
- **Guard.** `taa_a_resize_resets_accumulation` (`tests/sgl-post-fx/tests/execute.rs`);
  `taa_adaptive_format_switch_resets_at_same_extent_in_both_directions`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`).
- **Status.** Rule.

#### T-3 Placeholder frame and withheld jitter

- **Rule.** When no resolve pipeline exists yet for the frame's feature flags and depth
  convention at `prepare_resources` (the first frame of a `TemporalAntiAliasing`, and the
  first after any feature-flag change), the effect writes the placeholder instead of
  resolving: current colour, confidence 0.5, the raw (not closest) motion, the hold
  armed and the mask hold cleared, over the whole view; `get_jitter_offset` returns zero
  for that frame. Accumulation recovers as after T-1.
- **Raised by.** Creation of SGL3D's post-effect context (T-6), always together with a
  T-4 reset; SGL3D's flags never change and it creates both mask-count variants in
  advance.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** DiligentFX's placeholder (DFX-2), modified to load the colour as the
  resolve does and to start confidence at 0.5 (DFX-47).
  Code: `crates/sgl-post-fx/src/temporal_anti_aliasing.rs` (`prepare_resources`,
  `compute_placeholder_texture`); `TAA_ComputeTemporalAccumulation.wgsl` (`ComputePlaceholderPS`).
- **Guard.** `taa_first_frame_takes_an_unfilterable_colour`,
  `taa_feature_change_restarts_confidence_and_convergence`
  (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

#### T-4 SGL3D's reset decision

- **Rule.** When the view's history is not valid (T-5), or the DiligentFX context
  did not run in the view's last finished frame (its kept frame index is not this
  frame's `HistoryFrame::frames` less one, or none), `PostFx::prepare` asks TAA (and
  Crystal SSR) to reset (T-1), renders the frame without jitter, clears the previous
  effect depth to the far plane and uses the current camera as the previous one, over
  the whole view. A frame rendered again reads and writes the same halves: its
  `frames` is the first rendering's, and the halves follow its parity. TAA's own rule
  (`sgl-post-fx`, DFX-12, DFX-50) repeats a repeated index's reset, and the frame after an
  unfinished reset is a reset again (F-1), so it repeats index 0 rather than meeting
  the index gap from the unfinished reset's 0 to its own.
- **Raised by.** Camera cuts, resizes and scene changes (T-5); a frame after one in
  which the context did not run (F-4, F-5).
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own, after Hydrogent's `HnPostProcessTask::Prepare`.
  Code: `crates/sgl-3d/src/view/post_fx.rs` (`PostFx::prepare`, `PostFx::settle`),
  `crates/sgl-post-fx/src/temporal_anti_aliasing.rs` (`update_constant_buffer`).
- **Guard.** `a_cut_a_resize_and_another_scene_restart_history`,
  `an_unfinished_frame_restarts_nothing`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`);
  `consecutive_frames_continue_across_the_frame_count_s_wrap`
  (`crates/sgl-3d/src/view/history.rs`); `a_repeated_frame_index_keeps_history`
  (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

#### T-5 Renderer resets

- **Rule.** When `FrameInput::camera_cut` is set, a `View::resize` changed the targets
  (any size, the High preset's bloom targets, or FSR started, stopped or recreated), or
  the scene differs from the last finished frame's, the frame's history is not valid and
  TAA resets through T-4 (the sources are F-1 and F-2) over the whole view.
- **Raised by.** Camera cuts, window and setting changes, scene switches.
- **Stays triggered while.** Nothing: each is one frame. A preset change between Low and
  High resets TAA even at an unchanged size.
- **Source.** SGL3D's own.
  Code: `crates/sgl-3d/src/renderer/view.rs` (`View::resize`, `View::begin_history`).
- **Guard.** `a_cut_a_resize_and_another_scene_restart_history`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-6 TAA dropped with the post-effect context

- **Rule.** When a frame runs neither TAA nor Crystal SSR, the view drops its
  post-effect context; when the frame's TAA choice differs from the context's, it
  recreates it. Either discards TAA's history over the whole view; the new context
  starts with T-3 and T-4.
- **Raised by.** Switching antialiasing; an FSR failure handing over to TAA; a frame whose
  projection is not SGL3D's infinite reversed-Z perspective (`p[3][3] == 0`,
  `p[2][3] == -1`, `p[2][2] == 0`, `p[3][2] > 0`), for which SMAA replaces TAA.
- **Stays triggered while.** Nothing: it needs a setting or camera-kind change. A game
  that alternates camera kinds (an orthographic cutscene) restarts TAA at each switch.
- **Source.** SGL3D's own.
  Code: `crates/sgl-3d/src/renderer/frame.rs` (frame setup of `post_fx`);
  `crates/sgl-3d/src/renderer/effective.rs` (`post_fx_camera`).
- **Guard.** None.
- **Status.** Rule.

### Reprojection

#### T-7 Reprojection outside the screen

- **Rule.** When a pixel's position less its closest motion lies outside the viewport,
  the resolve writes the current colour with confidence 0.5, arms the hold and clears
  the mask hold, for that pixel. It recovers through the confidence ramp and the hold.
- **Raised by.** Screen edges during camera motion; objects entering; surfaces behind the
  previous camera, whose G-buffer motion is set to the two-screen limit.
- **Stays triggered while.** Nothing beyond the band entering the view.
- **Source.** DiligentFX, faithful, using the closest motion (DFX-13).
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** `taa_border_pixels_take_the_screens_motion`,
  `taa_surfaces_behind_the_previous_camera_take_no_history`
  (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

#### T-9 Closest motion

- **Rule.** Every pixel takes the motion of the nearest-depth texel in its 3x3
  neighbourhood (clamped to the screen), which drives reprojection (from 0.01 pixels a
  frame; a still pixel reads its own history, T-11), motion-difference
  rejection, the stillness test, the motion amount, the box width and the depth test's
  tolerance, over a 1-pixel band at depth edges.
- **Raised by.** Depth edges: a background pixel beside a moving foreground takes the
  foreground's motion, reads history from where the foreground came from and arms the
  hold (T-22).
- **Stays triggered while.** A moving object is in view: its 1-pixel outline band.
- **Source.** Godot `taa_resolve.glsl` (`get_closest_pixel_velocity_3x3`) with DiligentFX's
  `ComputeClosestMotion.fx` search (DFX-13), clamped as Godot and Bevy clamp (DFX-42).
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`SampleClosestMotion`).
- **Guard.** `taa_closest_motion_is_the_nearest_depth_in_3x3`,
  `taa_border_pixels_take_the_screens_motion` (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

#### T-27 Bicubic history fetch

- **Rule.** Under `BICUBIC_FILTER` the resolve reads history colour and confidence with a
  five-tap Catmull-Rom filter through a linear clamp sampler, floored at zero. Its lobes
  overshoot at edges, which the box (T-13) bounds; a low-confidence neighbour lowers the
  confidence read by the pixels beside it, over a 4x4 texel footprint. A still pixel
  (T-11) reads at its own centre, where the filter returns its texel exactly; at rest
  the box applies a tenth of its clip, so a fetch a fraction of a texel off every frame
  would accumulate its overshoot.
- **Raised by.** Always.
- **Stays triggered while.** Nothing beyond a reset's ramp spreading about a pixel
  further.
- **Source.** DiligentFX (Jimenez, Filmic SMAA), faithful; Filament's deringing of the
  fetch is not ported.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`SamplePrevColorCatmullRom`).
- **Guard.** `taa_reprojecting_a_step_by_a_fraction_of_a_pixel_stays_within_its_levels`
  (`tests/sgl-post-fx/tests/execute.rs`); `moving_the_origin_during_a_pan_changes_nothing`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-34 Motion and depth-motion inputs

- **Rule.** See M-1, M-2, M-12, M-18 and M-20: TAA reprojects
  with the G-buffer's unjittered motion (a receiver's where one is the surface), takes
  depth motion as usable only where its validity W is above 0 (never for the sky), and
  treats effect depth beyond 10 km as sky.
- **Status.** Rule.

### Rejection terms

#### T-10 Motion-difference rejection

- **Rule.** When a pixel's closest motion differs from the previous frame's closest motion
  at its reprojected position by more than 2.5 pixels, the resolve moves 0.01 of the
  frame's weight per further pixel from history to the current frame, for that pixel
  (none left about 100 pixels beyond the threshold). It applies under every flag,
  subtracted from the history weight before the confidence cap (T-26). It recovers the
  next frame without the difference, then through the confidence ramp.
- **Raised by.** Motion changing between frames at a pixel: acceleration, an object
  crossing a differently moving background, camera shake.
- **Stays triggered while.** Continuous strong camera shake; nothing in a still picture.
- **Source.** Godot `taa_resolve.glsl` (`get_factor_disocclusion`) and `taa.cpp`
  (`disocclusion_threshold`), combined with DiligentFX's confidence (DFX-14); keeping it
  under the Filament-derived adaptive path is SGL3D's own.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`);
  constants in `TemporalAntiAliasingStructures.wgsl`.
- **Guard.** `taa_rejects_history_gradually_by_motion_change_not_speed`
  (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

#### T-13 Variance box, min/max box and clip

- **Rule.** Every pixel's history is clipped towards its 3x3 neighbourhood mean (clamped
  into the box) within a box of `clamp(1.1 × 1080 / height, 0.75, 1)` standard deviations
  of the tone-mapped colour `c / (1 + c)`, narrowing smoothly to zero width at a closest
  motion of 0.02 of the screen per frame (in UV units, so 2% of the width horizontally
  and of the height vertically), and intersected with the min/max box halfway between
  the cross's and the 3x3's. The adaptive path blends the clipped history in by the box
  strength (0.1 at rest, 1 at full strength); the legacy path uses it fully.
- **Raised by.** Any neighbourhood variation; speed; a flat neighbourhood gives a
  zero-width box.
- **Stays triggered while.** Sub-pixel features a whole 3x3 neighbourhood can miss (thin
  distant lights) are clipped a tenth of the way every frame at rest and fully whenever
  strength is full; at 2% of the screen per frame or more the history becomes the
  clamped mean.
- **Source.** Godot `taa_resolve.glsl` (`clip_history_3x3`, `clip_aabb`) and `taa.cpp`
  (`variance_dynamic`); Filament `taa.mat`'s min/max box intersection. Differs from
  Filament: the clip target is the mean, not the filtered centre, and Filament's
  minimum box half-width of 0.002 (`kMinBoxHalfWidth`) is not ported: it guards
  Filament's YCoCg clip towards a filtered reconstruction, where a channel flat over
  the neighbourhood put history noise outside a zero-width box every frame; here a
  flat channel's history equals its sample at rest, so a zero-width box neither
  moves it nor clips the other channels, and the floor measured no difference
  (`sgl-post-fx` PROVENANCE.md, "Minimum box half-width"; 2026-11-18 history audit, O-10).
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputePixelStatisticYCoCgSDR`,
  `ComputeVarianceGamma`, `ClipToAABB`).
- **Guard.** `taa_reprojecting_a_step_by_a_fraction_of_a_pixel_stays_within_its_levels`
  (`tests/sgl-post-fx/tests/execute.rs`); `a_channel_flat_at_rest_leaves_the_others_alone`
  (`tests/sgl-3d/tests/scenario_history_taa.rs`), a zero-width channel at rest.
- **Status.** Rule.

#### T-26 Confidence ramp

- **Rule.** The resolve keeps at most `min(confidence read, 1 − current weight − T-10's
  rejection)` of history and stores `min(cap, 1 / (2 − weight kept))` as the next
  confidence; the cap is 0.964 under `MOTION_ADAPTIVE_HISTORY` and
  `TemporalStabilityFactor` (0.985 for still pixels) otherwise. From a reset's 0.5 the
  weight rises as k/(k+1): 0.88 after about 7 frames, 0.964 after about 26. After a
  period at full strength (confidence 0.893) it takes about 18 frames to climb back to
  0.964, overlapping the 12-frame hold ramp, so full rest arrives a few frames after the
  hold ends. Scope: pixel.
- **Raised by.** Resets, holds and rejection.
- **Stays triggered while.** Nothing of its own; it lengthens every recovery slightly.
- **Source.** DiligentFX `ComputeCorrectedAlpha`, with the adaptive cap from Filament's
  feedback; combining DiligentFX's ramp with Filament's hold is SGL3D's own.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeMaxAlpha`, `ComputeCorrectedAlpha`).
- **Guard.** `taa_feature_change_restarts_confidence_and_convergence`,
  `taa_rejects_history_gradually_by_motion_change_not_speed`
  (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

#### T-8 SkipRejection

- **Rule.** When `skip_rejection` is non-zero, the resolve blends unclipped history at its
  stored confidence over the whole view and arms the hold every frame, so turning it off
  starts a 12-frame ramp from full strength.
- **Raised by.** A direct `sgl-post-fx` caller; SGL3D never sets it.
- **Stays triggered while.** Nothing in SGL3D.
- **Source.** DiligentFX, faithful.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** None.
- **Status.** Rule.

### Legacy flags (not used by SGL3D)

#### T-11 Still pixels

- **Rule.** A pixel whose closest motion is under 0.01 pixels on both axes is still.
  Under every flag a still pixel reads its own history: its previous position is its own
  centre for every history read (colour and confidence, the previous closest motion
  and, under `MOTION_ADAPTIVE_HISTORY`, both holds and the adaptive depth history
  position, which keeps its jitter alignment), so the bicubic fetch lands on its texel
  and returns it (T-27). Its motion is projection and interpolation residue, not
  movement. Without `MOTION_ADAPTIVE_HISTORY` a still pixel also keeps up to 0.985 of
  history and skips the depth test (T-12). Under `MOTION_ADAPTIVE_HISTORY` stillness
  also decides T-22, and the legacy depth test is computed but unused.
- **Raised by.** Classification only.
- **Stays triggered while.** Nothing. A surface moving slower than 0.01 pixels a frame
  keeps its history unmoved: after `n` frames its history lies `n` times its speed
  behind it, and the box (T-13) bounds what that lag leaves, as the relaxed blend does
  for any slow change at rest.
- **Source.** Bevy `taa.wesl` (`MIN_HISTORY_BLEND_RATE`), with SGL's 0.01-pixel threshold
  (DFX-19); reading a still pixel's own history is SGL3D's own (2026-11-18 history
  audit, B-12): reprojected by residue, the Catmull-Rom fetch sat a fraction of a texel
  off and its small over- and undershoot accumulated under the relaxed rectification at
  rest, taking an outline 0.4% below the sky with a camera 100 m sideways of the origin.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`, `ComputeMaxAlpha`).
- **Guard.** `at_rest_taa_holds_silhouettes_still_far_from_the_origin`
  (`at_100_m`, `at_100_m_along_the_forward_axis`)
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`), the read position;
  `taa_adaptive_static_projection_residue_is_not_surface_motion`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`).
- **Status.** Rule.

#### T-12 Legacy depth disocclusion

- **Rule.** Without `MOTION_ADAPTIVE_HISTORY`, when the largest of
  `exp(−|z_reprojected − z_previous| / max(z_reprojected, z_previous))` over the previous
  3x3 neighbourhood (linear depths through the previous projection) is at most 0.9, or the
  reprojected depth lies at or nearer than the previous camera's near plane, the pixel
  keeps no history and its confidence restarts at 0.5. It recovers through the
  confidence ramp.
- **Raised by.** Depth changes of about 10.5% or more; surfaces behind the previous
  camera.
- **Stays triggered while.** Nothing in SGL3D, which does not use these flags.
- **Source.** DiligentFX, faithful, with DFX-32, DFX-42 and DFX-43.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeDepthDisocclusion`);
  `ComputeReprojectedDepth.wgsl` (near-plane sentinel).
- **Guard.** `taa_surfaces_behind_the_previous_camera_take_no_history`,
  `taa_a_changed_near_plane_keeps_history` (`tests/sgl-post-fx/tests/execute.rs`).
- **Status.** Rule.

### Motion-adaptive strength and depth disocclusion

#### T-14 Strength

- **Rule.** Each pixel's strength is the largest of its motion amount (T-24), its motion
  hold over 12 (T-19) and its mask strength (T-25). Box strength is
  `max(mix(0.1, 1, s), disocclusion)`, alpha strength `max(mix(0.3, 1, s), disocclusion)`
  and current weight `0.12 × alpha strength`. Every other adaptive rule acts through it.
- **Raised by.** The rules that feed it.
- **Stays triggered while.** Any of its inputs does.
- **Source.** Filament `taa.mat` (`rawMotionAmount`, `historyBoxStrength`,
  `historyAlphaStrength`); Filament's sky override (`farness`) is not ported, and the
  mask strength is SGL3D's own.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** `taa_adaptive_retains_periodic_subpixel_geometry_without_disabling_rejection`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`); `at_rest_taa_holds_edges_between_surfaces_still`,
  `at_rest_taa_holds_silhouettes_against_the_sky_still`,
  `at_rest_taa_holds_silhouettes_against_a_far_surface_still`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-24 Motion amount

- **Rule.** A pixel's motion amount is `clamp((closest motion in pixels / 1)², 0, 1)`:
  1% at 0.1 pixels a frame, 25% at 0.5, full at 1. Because any motion of 0.01 pixels or
  more already arms the hold (T-22), a moving pixel is at full strength whatever its
  speed, and the amount matters only for motion under 0.01 pixels.
- **Raised by.** Pixel motion.
- **Stays triggered while.** Nothing of its own.
- **Source.** Filament `taa.mat` (`motionAmount`), faithful; T-22 removes Filament's
  gradual sub-pixel response.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** None specific.
- **Status.** Rule.

#### T-15 No usable expected depth

- **Rule.** When a pixel's camera-reprojected depth is at or nearer than the previous
  camera's near plane, and its depth motion is invalid (W at most 0, or the previous
  clip w not positive), disocclusion is 1: history is replaced by the current colour,
  strength is full and the hold is armed, for that pixel.
- **Raised by.** A camera moving forward past surfaces; surfaces behind the previous
  camera.
- **Stays triggered while.** Nothing in a still picture.
- **Source.** SGL3D's own, adapting Filament's expected depth with DFX-32's sentinel and
  NRD's 2.5D depth motion.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`AdaptiveDisocclusion`,
  `AdaptiveMotionExpectedDepth`).
- **Guard.** `taa_adaptive_keeps_history_of_surfaces_moving_in_depth`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`).
- **Status.** Rule.

#### T-16 Categorical sky

- **Rule.** Depth normalized so the near plane is 1 and the far plane 0. When a pixel is
  sky (exactly 0) and no texel of the previous 3x3 neighbourhood (about its
  jitter-aligned history position) was, or its current 3x3 neighbourhood holds no sky and
  every previous texel was sky, disocclusion is 1 for that pixel, however small the
  depth difference.
- **Raised by.** An object leaving or covering the sky. SGL3D writes the effect depth of
  the sky, and of every surface beyond the effect far plane of 10 km, as exactly 0.
- **Stays triggered while.** Nothing for the sky. A structure standing near 10 km is
  "sky" on one side of that distance and a surface on the other.
- **Source.** SGL3D's own (#791), replacing Filament's `farness` override.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`AdaptiveDisocclusion`);
  `crates/sgl-3d/src/view/post_fx_inputs.wgsl` (`fs_main`).
- **Guard.** `taa_adaptive_sky_and_a_far_surface_replacing_each_other_reject_history`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`);
  `the_sky_a_far_surface_leaves_is_a_disocclusion`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-17 Two-sided depth evidence

- **Rule.** History depth is read at the previous position shifted by the jitter
  difference. For each usable expected depth E (the camera reprojection, and the
  surface's own depth from its depth motion where W > 0), evidence is
  `min(|history − E| − tolerance, max(E − previous 3x3 max, previous 3x3 min − E))`,
  where tolerance is the current raw 3x3 depth range times
  `1 − smoothstep(1, 2, closest motion in pixels)`. The smaller evidence counts, and
  disocclusion is `smoothstep(0, 0.001, max(0, evidence))` in normalized depth, for that
  pixel. A step of `dz` metres at distance `z` exceeds 0.001 only when roughly
  `near × dz / z² > 0.001` (at a 0.1 m near plane: 1 m at 10 m, 100 m at 100 m), so
  disocclusions among distant scenery are not detected by depth.
- **Raised by.** Real disocclusions; silhouettes of anything moving more than 1 to 2
  pixels a frame.
- **Stays triggered while.** Nothing for still content: the tolerance bounds jitter
  exactly.
- **Source.** Filament `taa.mat` (`depthDisocclusion`, `jitterCorrection`,
  `depthNeighborhoodRange`, `disocclusionHistoryNeighborhood`, the 1-to-2-texel fade),
  modified: Filament skips the test where an object moves; SGL3D keeps it with the
  surface's own expected depth.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`AdaptiveDisocclusion`, `AdaptiveEvidence`).
- **Guard.** `taa_adaptive_depth_disocclusion_rearms_zero_motion_background`,
  `taa_adaptive_border_depth_alignment_keeps_valid_stable_history`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`); `a_disocclusion_recovers`,
  `depth_disocclusion_rejects_beyond_the_reach`,
  `a_surface_the_camera_follows_keeps_its_history`,
  `a_surface_moving_along_the_view_keeps_its_history`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-18 Applying disocclusion

- **Rule.** A pixel's disocclusion pulls its rectified history towards the current
  colour by that share, raises box and alpha strength to at least it, and arms the hold
  at `disocclusion × 12` frames. It does not lower the stored confidence, so the frames
  after a full disocclusion keep up to 0.88 of the fresh history at once.
- **Raised by.** T-15, T-16, T-17.
- **Stays triggered while.** Nothing of its own.
- **Source.** Filament `taa.mat`, faithful; arming the hold continuously is SGL3D's own.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** As T-17.
- **Status.** Rule.

### Recovery hold

#### T-19 Carrying the hold

- **Rule.** Every adaptive frame, a pixel's motion hold is the larger of the hold read at
  its reprojected position less one (at least 0) and `disocclusion × 12`, then 12 where
  T-20 to T-23 arm it; strength is at least the hold over 12. It is stored per pixel in
  the closest-motion history's B channel and travels with the reprojection. Recovery is
  12 frames after the last arming, plus T-26's lag.
- **Raised by.** T-18, T-20 to T-23, and every reset path (T-1, T-3, T-7, T-8).
- **Stays triggered while.** Any arming rule fires.
- **Source.** Filament `PostProcessManager.cpp` (`motionHoldRemaining`), modified: Filament
  holds one value per frame; SGL3D stores and transports it per pixel.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** `the_hold_lasts_its_frames_after_a_camera_step`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`);
  `taa_adaptive_camera_stop_holds_recovery_then_stabilizes`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`).
- **Status.** Rule.

#### T-20 Camera moved

- **Rule.** Each frame `PostFx::begin` decides on the CPU whether the camera moved
  (`TemporalAntiAliasing::camera_moved`) from the cameras it gives the effects, this
  frame's and the last finished frame's: each projection with its jitter removed
  (`(f2Jitter, 0, 0) × column.w` subtracted from every column), then the views and the
  unjittered projections compared element by element in f32, never their products.
  Elements `a` and `b` agree when `|a − b| ≤ 8 × f32::EPSILON × max(1, m)`: eight
  rounding steps of the magnitude `m` the element's rounding scales with. For rotation
  and projection elements `m` is `max(|a|, |b|)`. For the view's translation elements it
  is also the eye's distance from the render origin (the largest absolute component of
  either camera's eye, derived from its view): a translation element is
  `−dot(axis, eye)`, a sum of terms of the eye's magnitude that may cancel to near zero,
  so its noise is steps of the eye's magnitude however small it is (Hyperdrive's chase
  camera at rest 100 m from the origin: an element of 0.024 changing by 7.6e-6 to
  1.5e-5 between frames). When any element of
  either matrix disagrees, `camera_motion` is set and the resolve arms the hold at every
  pixel. A reset frame compares a camera with itself and reports no motion. Recovery is
  12 frames after the last frame the camera moved.
- **Raised by.** Any change of view or projection beyond that rounding: translation,
  rotation, field of view (a speed or boost FOV), aspect, near plane. Not by a camera at
  rest whose matrices are recomputed each frame from float state, anywhere from the
  render origin (a composed view accumulates a few half-steps per term), nor by the
  frame after a render-origin move, whose previous view is re-expressed in double
  precision and rounded once (F-11).
- **Stays triggered while.** The camera genuinely moves: the whole view, a followed craft
  still on screen included, stays at full strength. A camera that drifts by more than
  rounding every frame (smoothing that never converges, interpolation between physics
  ticks that leaves a residue) moves.
- **Known limits.** The tolerance scales with the distance from the origin, so far from
  it it admits a small real translation: up to eight steps of the eye's magnitude, about
  0.1 mm at 100 m (73 µm with the eye's largest component 76.5 m) and 4 to 5 mm at
  5 km. That is safe for the view-wide signal because a camera change that moves a pixel
  by 0.01 px or more arms that pixel through its own motion (T-22): at 1080 rows and a
  one-radian field 73 µm moves only surfaces within about 7 m by 0.01 px or more, and
  4.8 mm only those within about 475 m, and those pixels arm themselves. Rotation
  elements are below 1 in magnitude, so a turn of a hundredth of a pixel's angle at that
  view (1e-5 rad) moves the camera anywhere.
- **Source.** Filament `PostProcessManager.cpp` (`kCameraStaticThreshold` 1e-6 on the
  unjittered `projection * view`), modified: Filament compares double-precision matrices
  on the CPU with an absolute threshold; SGL3D compares f32 views and projections on the
  CPU with a tolerance relative to the magnitude each element's rounding scales with,
  since an absolute 1e-6 on f32 is a single rounding step at 16 (2026-11-18 history
  audit, B-2).
  Code: `crates/sgl-post-fx/src/temporal_anti_aliasing.rs`
  (`TemporalAntiAliasing::camera_moved`, `CAMERA_REST_STEPS`);
  `crates/sgl-3d/src/view/post_fx.rs` (`PostFx::begin`, `camera_attribs`);
  `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** `a_camera_at_rest_with_rounding_noise_has_not_moved`,
  `the_smallest_real_camera_changes_move_it`,
  `far_from_the_origin_a_translation_within_the_rounding_bound_is_left_to_the_pixels`,
  `a_view_rebuilt_from_a_rounded_eye_has_not_moved`,
  `at_100_m_a_translation_beyond_the_eyes_rounding_moves_the_camera`
  (`crates/sgl-post-fx/src/temporal_anti_aliasing.rs`);
  `a_camera_step_rearms_the_whole_view`,
  `the_hold_lasts_its_frames_after_a_camera_step`,
  `at_rest_taa_holds_silhouettes_still_far_from_the_origin`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`);
  `taa_adaptive_translated_camera_does_not_mistake_jitter_for_motion`,
  `taa_adaptive_camera_stop_holds_recovery_then_stabilizes`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`).
- **Status.** Rule.

#### T-21 Rigid-object and visibility signal

- **Rule.** When `TemporalAntiAliasingAttribs::rigid_object_motion` is non-zero, the
  resolve arms the hold at every pixel. SGL3D sets it to the scene's rigid-change signal
  (M-9) or a changed visibility mask (F-10), held until the frame is finished: changes
  no motion vector describes. Recovery is 12 frames after the last frame that raised it.
  A moving instance moved from its submitted pose does not raise it: the pixels it
  covers and leaves arm their own holds (T-22, T-9, T-23).
- **Raised by.** See M-9 and F-10: additions and removals, showing and hiding, model
  swaps and moves without a usable submitted pose of any instance whose `visible` flag
  is set before or after (culled, off screen or not), any move of a static instance,
  model replacement, material edits that change what is drawn, and a changed
  `FrameInput::visibility_mask`.
- **Stays triggered while.** The game keeps making such changes, anywhere, however small
  or far: spawning or despawning every frame, toggling visibility or visibility groups,
  moving static instances, swapping models. Moving instances posed every frame, traffic
  and other craft included, do not keep it raised.
- **Known limits.** What a moving instance leaves faster than the motion reach covers
  (more than about a 32-pixel tile a frame) has only the depth test and the categorical
  sky (T-17, T-16), as for deformation (T-23).
- **Source.** Filament `PostProcessManager.cpp` (`taa`, re-arm when `objectMotionVectors`
  exists) with `View.cpp`, modified: Filament re-arms for renderables in its culled
  visible set whose transform changed, because its hold is one value a frame; SGL3D's
  hold is per pixel and armed by motion and its reach, so it re-arms the view only for
  changes without a motion vector, culled or not, including appearances, departures,
  material edits and mask changes.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`);
  `crates/sgl-3d/src/stages/antialiasing/mod.rs` (`Antialiasing::encode`).
- **Guard.** `taa_adaptive_resting_thin_occluder_departure_does_not_leave_a_ghost`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`), the resolve's response to the flag;
  `appearance_and_departure_at_rest_resolve_within_the_hold`,
  `a_static_instance_moved_at_rest_resolves_within_the_hold`,
  `material_edits_at_rest_resolve_within_the_hold`,
  `at_rest_taa_holds_silhouettes_beside_a_rigid_mover_still`,
  `a_rigid_card_moves_from_rest`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-22 Pixel not still

- **Rule.** When a pixel's closest motion is 0.01 pixels or more on either axis, the
  resolve arms its hold and reprojects its history reads by that motion; under it, the
  pixel is still, reads its own history (T-11) and arms nothing. Recovery is 12 frames
  after it last moved.
- **Raised by.** Any motion of 0.01 pixels a frame or more: moving objects and
  deformation, the sky under camera rotation, and motion-vector residue above that size.
- **Stays triggered while.** A surface keeps any visible or invisible motion (idle bob,
  vertex-function sway, a followed craft with small noise in its pose relative to the
  camera): those pixels never relax.
- **Source.** SGL3D's own: DFX-19's threshold applied to Filament's hold.
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`ComputeTemporalAccumulationPS`).
- **Guard.** `taa_adaptive_static_projection_residue_is_not_surface_motion`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`);
  `at_rest_taa_holds_silhouettes_still_far_from_the_origin`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-23 Motion reach

- **Rule.** SGL3D reduces the frame's motion (in render pixels a frame, each vector
  clamped to 64 pixels) to the largest in the 3x3 neighbourhood of each 32-pixel tile.
  Where that is 1 pixel a frame or more, the resolve arms the pixel's hold, over every
  pixel within 32 to 63 pixels of the motion. It never reprojects. Recovery is 12 frames
  after the motion leaves the neighbourhood.
- **Raised by.** Anything moving 1 pixel a frame or more, the sky under camera rotation
  included.
- **Stays triggered while.** Something keeps moving: continuous traffic keeps a band of
  up to 63 pixels about itself at full strength (local, not view-wide).
- **Known limits.** A surface that leaves a pixel faster than the reach covers, more
  than about a tile (32 pixels) a frame or in one jump, leaves no motion near that pixel,
  so its old outline is rejected only by the depth test and the categorical sky (T-17,
  T-16): among distant scenery whose depth step is under the threshold, it ghosts for
  as long as the relaxed blend takes. This holds alike for deformation, vertex functions
  and moving instances, none of which raise the view-wide signal (M-9).
- **Source.** McGuire et al. 2012 and Wicked Engine's motion-blur tile passes; arming
  TAA's hold with them is SGL3D's own.
  Code: `crates/sgl-3d/src/stages/antialiasing/reach.rs` (`Reach::encode`);
  `crates/sgl-3d/src/shading/motion_tiles.wgsl`; `TAA_ComputeTemporalAccumulation.wgsl`
  (`SampleNeighborhoodMotionPixels`).
- **Guard.** `taa_adaptive_a_still_pixel_within_reach_of_motion_follows_a_step_at_full_feedback`,
  `taa_adaptive_a_neighbourhood_slower_than_the_threshold_changes_nothing`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`);
  `at_rest_taa_holds_silhouettes_beyond_the_reach_of_motion_still`,
  `at_rest_taa_holds_silhouettes_within_the_reach_of_slow_motion_still`,
  `a_deformation_jumps_beyond_the_reach`, `a_rigid_card_moves_from_rest`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

### Responsive masks

#### T-25 Mask strength and mask hold

- **Rule.** A pixel's mask value `m` is the largest value of the two temporal masks over
  its 3x3 neighbourhood, clamped to the screen (R8, so anything under 1/510 reads 0).
  While `m > 0`, strength is at least `m` and the mask hold is set to 12; once
  unmarked, the hold read at its reprojected position counts down by one per frame and
  strength is full while it is above 0. A pixel at the jittered edge of still marked
  content is marked through the neighbour the content covers on every phase, so it
  stays at strength `m`, and the hold begins once the content has left its
  neighbourhood. Scope: the pixel and its 3x3 neighbourhood. Recovery: 12 frames after
  the content last lay in the neighbourhood.
- **Raised by.** The mask writers (T-28).
- **Stays triggered while.** Marked content stays in the neighbourhood: those pixels
  hold strength `m`. Content narrower than a pixel that no sample of the neighbourhood
  hits on some jitter phases (the end of a sub-pixel glow line) leaves those phases
  unmarked, at full strength with a box that holds none of it, so they clip it away as
  T-13 does any sub-pixel feature at full strength: at the end of a half-pixel glow
  line of 0.05 over 0.1, a step of 0.0128 in v / (1 + v) against 0.0074 at the mask's
  strength. Mist whose drifting noise drives alpha to 0 in moving cells is content
  changing, and holds as it leaves.
- **Source.** SGL3D's own, with AMD FSR's reactive and transparency-and-composition masks
  as inputs and FSR's 3x3 dilation of them (FidelityFX SDK 1.1.4,
  `ffx_fsr3upscaler_prepare_reactivity.h` `DilateReactiveMasks`; 2026-11-18 history
  audit, B-15: read per pixel, the edge of still marked content alternated between `m`
  and full, at 2.7 times the temporal deviation of the mask's strength for a glow of
  0.05).
  Code: `TAA_ComputeTemporalAccumulation.wgsl` (`SampleResponsiveMask`,
  `ComputeTemporalAccumulationPS`).
- **Guard.** `taa_adaptive_a_marked_pixel_follows_a_step_at_full_feedback`,
  `taa_adaptive_a_pixel_marked_content_left_holds_at_full_strength`,
  `taa_adaptive_a_pixel_at_the_jittered_edge_of_still_marked_content_keeps_its_strength`
  (`tests/sgl-post-fx/tests/support/adaptive_taa.rs`);
  `marked_content_at_rest_responds_and_leaves_no_trail`,
  `at_rest_taa_holds_silhouettes_beside_marked_content_still`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`).
- **Status.** Rule.

#### T-28 How TAA uses each mask writer

- **Rule.** TAA does not distinguish the reactive mask from the transparency and
  composition mask: it takes the larger over the pixel's 3x3 neighbourhood as `m`
  (T-25), so each writer's mark reaches a pixel beyond what it covers. The writers (M-11, M-16, M-17a to M-17c) give:
  - Blended and transmissive surfaces (M-11): reactive up to 0.9 (also where they
    overlap), composition up to their coverage (1 for a transmissive non-blended one),
    so a near-opaque blended surface holds full or near-full strength on and through it
    every frame.
  - Opaque or masked materials whose shading changes (M-16): composition 1, so those
    pixels are at full strength in every frame their shading changes (see T-29).
  - Glow (M-17b): reactive up to 0.9 by `max(r, g, b) × alpha` of its light at the
    frame's exposure, so a glow's strength is its displayed brightness and its faint
    tails arm the mask hold.
  - Mist (M-17c): composition equal to its alpha, which drifts with time, so pixels
    whose neighbourhood the drifting noise leaves unmarked are held.
  - Heat distortion (M-17): composition equal to its weight where it lies in front of
    the opaque depth, including where its displacement is zero.

  The masks are bound only in frames whose scene can mark them; without them `m` is 0
  and the mask hold decays.
- **Raised by.** Blended or transmissive materials, materials whose shading changes, glow,
  mist and heat in view.
- **Stays triggered while.** Any of them is in view, moving or not.
- **Source.** AMD FidelityFX SDK 1.1.4 (FSR's reactive and composition masks) for the
  values; their use as TAA strength is SGL3D's own.
  Code: `crates/sgl-3d/src/view/post_fx.rs` (`PostFx::temporal_anti_aliasing`);
  `crates/sgl-3d/src/renderer/effective.rs` (`temporal_masks`).
- **Guard.** `marked_content_at_rest_responds_and_leaves_no_trail`
  (`tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`);
  `the_temporal_masks_are_held_exactly_while_a_writer_can_mark_them`
  (`crates/sgl-3d/src/renderer/effective.rs`).
- **Status.** Rule.

#### T-29 Shader materials held at full strength while their shading changes

- **Rule.** See M-16. An opaque or masked material with a game shader is marked
  composition 1 in the frames its shading changes: every frame while its module reads
  the time, else the frame after an edit of its parameters or of an instance's shader
  data. TAA then holds its pixels at full strength for those frames and the hold after
  them (T-25), and a shader that reads no time and whose inputs stay the same reaches
  resting stability.
- **Raised by.** Time-reading shaders; edits of shader parameters or instance data.
- **Stays triggered while.** The shader reads the time, or the game edits its inputs every frame.
- **Source.** See M-16.
- **Guard.** `a_still_shader_surface_holds_taas_resting_bound`
  (`tests/sgl-3d/tests/scenario_history_masks.rs`); the dissolve cases of
  `scenario_temporal_antialiasing.rs`.
- **Status.** Rule.

### How the rules combine

- T-20 and T-21 both arm the hold at every pixel; either alone keeps the whole view at
  full strength. Neither is raised by a camera at rest within f32 rounding or by
  moving instances posed from their submitted pose.
- T-22 makes T-24's gradual sub-pixel response moot; T-22, T-9 and T-23 together cover
  what anything with a motion vector leaves, up to about 32 pixels a frame.
- T-17 cannot see disocclusions among distant scenery, which is why T-21 stays
  view-wide for changes without a motion vector; what moves faster than T-23's reach
  shares that limit.
- T-25's dilation keeps the jittered edges of still marked content at `m`; only content
  narrower than a pixel that a whole neighbourhood misses on some phases still meets
  T-25's plateau at full strength, and T-13 clips it then.
- T-26's ramp lengthens every hold's recovery by a few frames.
- T-4 with T-1 makes an abandoned or re-rendered frame a full reset for TAA.

## FSR2 and FSR3 upscaling

FSR keeps its history in AMD's context: the upscaled colour, per-pixel locks, FSR2's temporal reactive factor and
FSR3's accumulation counter, all reprojected by the surface depth and motion at the render size. SGL3D restarts it
on the view's one reset, on any frame FSR did not see, and on a new render size, version or scene size;
everything else is AMD's own rejection, shipped as a faithful port (`sp-fidelity`, FidelityFX SDK 1.1.4). A
chase-camera racing game trips most: AMD's reconstructed previous depth marking the followed vehicle's lowest band
disoccluded at speed (U-17, U-40), the motion weights that keep moving scenery on a few frames of history (U-28,
U-47), FSR3's use of the reactive mask as shading change on glass and glow (U-43), and materials marked
transparency and composition 1 in the frames their shading changes (U-11).

In this section FSR2 velocity is in display pixels (`length(motion_uv × display size)`); FSR3 velocity is in
"4K pixels" (`length(motion_uv × (3840, 2160))`) whatever the display size. View depth is in metres, SGL3D's world
units, with `view_space_to_meters_factor` 1. AMD file names abbreviate
`sp-fidelity: vendor/sdk-1.1.4/sdk/include/FidelityFX/gpu/fsr2/ffx_fsr2_<name>.h` as `fsr2/<name>.h` and
`.../gpu/fsr3upscaler/ffx_fsr3upscaler_<name>.h` as `fsr3/<name>.h`; the port runs the same algorithms from
`sp-fidelity: shaders/wgsl/fsr2` and `shaders/wgsl/fsr3upscaler`.

### SGL3D's integration

#### U-1 Reset on the view's history reset

- **Rule.** When the view resets history (`FrameInput::camera_cut`, a `View::resize` that changed the
  targets or restarted FSR, or a different `Scene`), the FSR adapter dispatches with `reset` set, and AMD discards
  every pixel's history, locks and luminance over the whole view (U-21, U-38). FSR2 reaches full accumulation
  weight after about 9 frames (U-27); FSR3 shows two frames without history and full weight on the fourth (U-46).
- **Raised by.** Camera cuts, window, scale or settings resizes, scene switches.
- **Stays triggered while.** Nothing: it lasts one frame, unless the game sets `camera_cut` or changes its
  sizes every frame.
- **Source.** SGL3D's own; AMD's documentation asks for a reset on camera jump cuts.
  Code: `crates/sgl-3d/src/renderer/view.rs` (`View::begin_history`), `crates/sgl-3d/src/stages/antialiasing/fsr.rs`
  (`Fsr::prepare`, `Fsr::dispatch`).
- **Guard.** `a_cut_a_resize_and_another_scene_restart_history` (fsr2, fsr3) in
  `tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`; `restarts::fsr2`, `restarts::fsr3`
  (`upscaled_restarts`) in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-2 FSR restarts after a frame it did not see, and after one that was not finished

- **Rule.** FSR continues only from the view's last finished frame, and only when FSR ran in it at
  this render size (`Kept<FsrRun>`); otherwise the adapter resets AMD's context for the whole view
  (as U-1). A render that ran FSR and was not finished, whether or not its commands were submitted,
  ends the kept state when it is settled (`Fsr::settle`), so the next frame resets. This is the
  History contract's first exception (D-58): AMD's context advances its own frame state
  (`first_execution`, its resource frame index, the previous jitter offset and its constants) inside
  dispatch, when the commands are recorded (`sp-fidelity-0.3.2/src/fsr2/component.rs:1510, 1669,
  1700, 1933`), that state is private to sp-fidelity, and AMD's SDK offers no way to take it back.
  Recovery as U-1.
- **Raised by.** Frames in which FSR did not run (a camera other than `perspective`, U-6; the
  `disable.fsr` diagnostics layer; TAA running in its place); a frame rendered and not finished
  (dropped, rendered again, or `finish_frame` omitted).
- **Stays triggered while.** The game leaves frames unfinished: each restarts FSR once.
- **Source.** SGL3D's own. Code: `crates/sgl-3d/src/stages/antialiasing/fsr.rs` (`Fsr::prepare`,
  `Fsr::settle`), `crates/sgl-3d/src/view/history.rs` (`Kept`).
- **Guard.** `upscaled_restarts` with `Restart::Unfinished`
  (`tests/sgl-3d/tests/scenario_fsr_upscaling.rs`).
- **Status.** Rule.

#### U-3 A reset ends FSR's history in a frame FSR does not run

- **Rule.** When a frame's history is invalid and FSR does not run in it, that frame leaves FSR no
  kept state, so after it is finished the next FSR frame resets (as U-1). It follows from U-2's rule;
  no separate step drops the history.
- **Raised by.** A reset during a frame whose antialiasing is not FSR.
- **Stays triggered while.** Nothing: U-2 already resets after such a frame.
- **Source.** SGL3D's own. Code: `crates/sgl-3d/src/view/history.rs` (`Kept::settle`).
- **Guard.** None.
- **Status.** Rule.

#### U-4 Reset on a new render size, scene size or version

- **Rule.** When the render size differs from the last FSR frame's, the adapter resets; when the scene size or the
  FSR version changes, the stage destroys the context and creates a new one, whose first dispatch is AMD's first
  execution, and the view resets every history. A version change resets even at equal sizes. Recovery as U-1.
- **Raised by.** `Settings::fsr_quality`, the output size, device scale, `SceneResolution`, the preset's scene
  cap, `Antialiasing::Fsr2`/`Fsr3`.
- **Stays triggered while.** Nothing, unless a game changes resolution every frame: there is no dynamic-resolution
  path, and every render-size change resets.
- **Source.** SGL3D's own, as AMD's sample recreates its context (`UpdateFSRContext`).
  Code: `crates/sgl-3d/src/stages/antialiasing/mod.rs` (`Antialiasing::resize`),
  `crates/sgl-3d/src/renderer/view.rs` (`View::resize`).
- **Guard.** `restarts::fsr2`, `restarts::fsr3` (`upscaled_restarts`, quality changes) in
  `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-5 Failure falls back to TAA

- **Rule.** When FSR's context creation or a dispatch fails, the stage drops the context and its history and
  remembers the failure for that version for the view's life; that frame presents the render-size composite
  without antialiasing, TAA runs from then on with a new context, and the next `resize` changes the targets and
  resets every history.
- **Raised by.** Device or driver errors.
- **Stays triggered while.** The view lives: the failed version never runs again in it.
- **Source.** SGL3D's own. Code: `crates/sgl-3d/src/stages/antialiasing/mod.rs` (`Antialiasing::resize`,
  `Antialiasing::encode`), `crates/sgl-3d/src/renderer/effective.rs` (`antialiasing`).
- **Guard.** `a_failed_fsr_dispatch_completes_the_frame_and_taa_takes_over` and
  `fsr_renders_each_quality_or_falls_back_to_taa` in `crates/sgl-3d/src/stages/antialiasing/fsr/tests.rs`.
- **Status.** Rule.

#### U-6 No FSR and no jitter without a `perspective` camera

- **Rule.** When the camera's projection is not SGL3D's infinite reversed-Z perspective, the effective
  configuration runs neither FSR nor TAA, applies no jitter and presents the render-size frame as it is; the next
  FSR frame resets (U-2).
- **Raised by.** Orthographic, finite-far or off-centre projections.
- **Stays triggered while.** The camera keeps that projection.
- **Source.** SGL3D's own. Code: `crates/sgl-3d/src/renderer/effective.rs` (`resolve`).
- **Guard.** None.
- **Status.** Rule.

#### U-7 FSR never withholds jitter

- **Rule.** FSR jitters every frame, reset frames included, by AMD's sequence (`8 × (display/render)²` phases),
  whose index runs on across resets; TAA's restart frame is unjittered, FSR's is not. AMD's lock lifetimes use the
  phase count, which it snaps on a reset and otherwise moves by one a frame towards a new count.
- **Raised by.** Every FSR frame.
- **Stays triggered while.** Nothing is rejected.
- **Source.** AMD's FSR sample's jitter index, faithful. Code: `crates/sgl-3d/src/stages/antialiasing/fsr.rs`
  (`Fsr::prepare`).
- **Guard.** `jitter_is_sub_pixel_and_never_in_the_motion` (fsr2, fsr3) in
  `tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`.
- **Status.** Rule.

#### U-8 Motion beyond two screens is off-screen history

- **Rule.** FSR reads the G-buffer motion, current minus previous unjittered UV at the render size, with
  `motionVectorScale` minus the render size and no jitter cancellation; a point whose previous position lies more
  than two screens away or behind the previous camera is written at the two-screen limit, so FSR takes it as a new
  sample (U-22, U-39).
- **Raised by.** Scenery passing very close to the camera; content arriving from behind it.
- **Stays triggered while.** Nothing beyond genuinely new content.
- **Source.** See the motion encoding in the frame and motion section; its FSR consequence is AMD's.
  Code: `crates/sgl-3d/src/shading/gbuffer.wgsl` (`gbuffer_encode_motion`),
  `crates/sgl-3d/src/stages/antialiasing/fsr.rs` (`Fsr::dispatch`).
- **Guard.** `motion_vectors_match_the_script` in `tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`.
- **Status.** Rule.

#### U-16 Exposure and frame time

- **Rule.** FSR reads the frame's one exposure, the tone map's, with `preExposure` 1 and AMD's automatic exposure
  off. FSR2's lock and shading-change luma scale with it (U-31); FSR3 multiplies current and previous luma by the
  current exposure, so an exposure change is no shading change there. The frame time feeds only AMD's internal
  automatic exposure, which SGL3D does not bind, and changes no rejection.
- **Raised by.** Automatic exposure.
- **Stays triggered while.** See U-31.
- **Source.** SGL3D's own choice of input. Code: `crates/sgl-3d/src/stages/antialiasing/fsr.rs` (`Fsr::dispatch`).
- **Guard.** None beyond every FSR scenario's fixed exposure.
- **Status.** Rule.

### What the masks do in each version

The mask writers and their values are M-11, M-16 and M-17. This group states only how each FSR version uses the
reactive mask and the transparency and composition (T&C) mask. Beware AMD's naming: in FSR3's code the "reactive"
channel holds T&C and motion divergence, and the game's reactive mask feeds the "shading change" channel.

#### U-9 Masks start clear every frame

See M-11, M-16 and M-17a to M-17c (the first writer of a frame clears both masks; FSR binds them in every frame).

#### U-10 Overlapping writers combine within 0.9

- **Rule.** Every mask writer but the moving-surface pass blends transparency and composition as AMD's sample does,
  `m (1 - mask) + mask`, so overlapping draws raise a pixel towards 1. Reactivity combines as the same union in units
  of AMD's recommended cap: each draw outputs `r` = its value capped at 0.9 and `r / 0.9`
  (`shading/temporal_masks.wgsl`, `temporal_reactive`), and the target blends `r + mask (1 - r / 0.9)`, so
  `1 - mask / 0.9` is the product of each draw's `1 - r / 0.9` and a pixel's reactivity never exceeds 0.9 (two 0.9
  draws give 0.9, two 0.5 draws 0.72 where AMD's union gives 0.75). FSR2 and FSR3 then apply U-25 or U-43 at that
  value.
- **Raised by.** Two panes of glass, glass in front of glow.
- **Stays triggered while.** The camera looks through the followed vehicle's rear window at its own exhaust glow.
- **Source.** AMD's FSR sample (`fsrapirendermodule.cpp`, the union, which caps nothing) and FSR documentation
  (`super-resolution-upscaler.md` 180: reactivity near 1 does not give good results, so clamp it to about 0.9),
  applied to the value FSR reads. Code: `crates/sgl-3d/src/view/targets.rs` (`mask_targets`),
  `crates/sgl-3d/src/shading/temporal_masks.wgsl`.
- **Guard.** `overlapping_reactive_writers_stay_within_amds_cap`, `moving_opaque_surfaces_mark_fsrs_composition_mask`,
  `heat_distortion_marks_the_composition_mask` in `crates/sgl-3d/src/stages/transparent/composition_tests.rs`.
- **Status.** Rule.

#### U-11 Surfaces marked T&C 1 while their shading changes

- **Rule.** Opaque and masked materials whose normal layers move or whose shader reads the time are marked T&C 1
  over all their visible pixels every frame, and a shader material whose parameters or instances' data were edited
  in the frame after the edit (M-16). FSR2 then removes their locks, tightens their rectification box to 1 σ and
  drops luma instability (U-26); FSR3 sets its reactive channel to 1: box scale 1 instead of 3, locks drained and
  none created (U-45). Accumulation weight is unchanged.
- **Raised by.** Water, wet or animated road decks, time-reading shaders, edits of a shader's inputs.
- **Stays triggered while.** The layers move or the shader reads the time: a road deck with moving normal layers
  or a time-reading shader has no thin-feature protection and the tightest clamp on the largest surface on screen,
  every frame. A shader that reads no time, with its inputs unedited, is unmarked.
- **Source.** AMD's FSR sample's animated textures (`AnimatedTexture.hlsl`) for normal layers; the shader rule is
  M-16's. Code: `crates/sgl-3d/src/scene/shaders.rs` (`Scene::shading_changes`).
- **Guard.** `moving_opaque_surfaces_mark_fsrs_composition_mask`, `a_material_set_moving_marks_fsrs_composition_mask`,
  `a_shader_surface_marks_composition_only_in_frames_its_shading_changes`,
  `a_shader_reading_the_time_marks_composition_every_frame`
  in `crates/sgl-3d/src/stages/transparent/composition_tests.rs`.
- **Status.** Rule.

#### U-12 Blended receivers mark reactivity

- **Rule.** Blended surfaces write reactive and T&C (M-11), receivers included, although a receiver writes its own
  depth and motion (M-12): what the pixel shows through a receiver, and its traced reflection, follow no motion
  vector, and AMD asks alpha-blended objects to write motion vectors and their alpha as reactivity both. FSR2 and
  FSR3 then weaken their history as U-25 and U-43 say.
- **Raised by.** Glass, canopies and water made `Blend` receivers.
- **Stays triggered while.** A receiver is on screen, e.g. the followed vehicle's glass made a receiver.
- **Source.** AMD FidelityFX SDK 1.1.4 `super-resolution-upscaler.md` 172 ("all opaque, alpha-tested and
  alpha-blended objects should write their motion vectors… For alpha-blended objects it is also strongly advised
  that the alpha value of each covered pixel is stored to the corresponding pixel in the reactive mask") and 131
  (T&C for highly reflective surfaces); the sample's translucency (`translucencyps.hlsl`) for T&C. Code:
  `crates/sgl-3d/src/shading/geometry.wgsl` (`blended_masked_fs`).
- **Guard.** `a_receiver_marks_the_masks_as_a_blended_surface_does` in
  `crates/sgl-3d/src/stages/transparent/composition_tests.rs`; `blended_surfaces_write_fsrs_masks` in
  `crates/sgl-3d/src/stages/transparent/blended_tests.rs`.
- **Status.** Rule (settled from O-5 by the 2026-11-18 history audit).

#### U-13 Glow reactivity by its displayed light

- **Rule.** Additive glow writes reactivity from its radiance times the frame's exposure multiplier times alpha,
  capped at 0.9 (M-17b), so its value follows what it adds to the displayed frame and not the game's light units;
  FSR2 and FSR3 apply U-25 and U-43 at that value.
- **Raised by.** Exhaust flames, trails, neon drawn as glow.
- **Stays triggered while.** The glow is on screen; a glow whose displayed brightest channel times alpha reaches 0.9
  sits at the cap.
- **Source.** AMD's FSR sample (`particlerender.hlsl`), at the frame's exposure, modified by the 0.9 cap.
  Code: `crates/sgl-3d/src/stages/transparent/glow.wgsl` (`glow_soft_masked_fs`).
- **Guard.** `glow_marks_reactivity_by_what_it_adds_to_the_displayed_frame` in
  `crates/sgl-3d/src/stages/transparent/composition_tests.rs`; `additive_effects_mark_fsr_reactivity_for_their_frame_only`
  in `crates/sgl-3d/src/stages/antialiasing/fsr/tests.rs`.
- **Status.** Rule.

#### U-14 Mist marks T&C

See M-17c (mist writes its alpha as T&C and no reactivity; FSR uses it as U-26 and U-45).

#### U-15 Heat distortion marks T&C

See M-17a (heat writes its weight as T&C where it lies in front of the opaque depth, also where its
displacement is zero; FSR uses it as U-26 and U-45).

#### U-25 FSR2: reactivity weakens accumulation

- **Rule.** FSR2 dilates the reactive mask over the clamped 3×3 render pixels, each neighbour weighted
  `pow(r, 1 + 6 − 6 × similarity)` of its colour to the centre's, and takes this frame's factor
  `r = max(dilated reactive, temporal reactive (U-27), shading-change bump (U-31))`. It multiplies the base
  accumulation by `1 − r`, lock lifetime by `1 − r`, a clamped history's protection by `1 − √r`, luma instability
  by `1 − max(mask, r^(1/6))`, narrows the current frame's kernel towards its minimum by `r`, and carries `r` as
  the next frame's floor. At 0.9: one tenth of the accumulation, locks at a tenth.
- **Raised by.** Blended surfaces, glow (M-11, M-17b), U-27, U-31.
- **Stays triggered while.** Glass or glow is on screen; neighbours of similar colour take up to the same value,
  dissimilar ones about 0.9⁷ ≈ 0.48.
- **Source.** AMD `fsr2/depth_clip.h` (`PreProcessReactiveMasks`), `fsr2/accumulate.h`
  (`ComputeBaseAccumulationWeight`, `RectifyHistory`, `ComputeTemporalReactiveFactor`),
  `fsr2/postprocess_lock_status.h` (`UpdateLockStatus`), `fsr2/upsample.h`; faithful port (`sp-fidelity`).
- **Guard.** None in SGL3D beyond the scenarios' outcomes.
- **Status.** Rule.

#### U-26 FSR2: T&C and motion divergence remove protection

- **Rule.** FSR2's accumulation mask is `max(motion divergence (U-23, U-24), dilated T&C)`, the T&C dilated as in
  U-25. It multiplies lock lifetime by `1 − mask`, raises the rectification box's tightening factor to at least the
  mask (U-29), multiplies luma instability by `1 − mask`, and skips the luma history at 0.1 or more. It leaves the
  accumulation weight alone.
- **Raised by.** U-11, blended coverage, mist, heat (M-11, M-16, M-17a, M-17c); motion discontinuities.
- **Stays triggered while.** Marked materials are in view, and along every silhouette whose motion differs from
  its neighbours'.
- **Source.** AMD `fsr2/depth_clip.h` (`PreProcessReactiveMasks`), `fsr2/postprocess_lock_status.h`
  (`UpdateLockStatus`), `fsr2/accumulate.h` (`RectifyHistory`, `ComputeLumaInstabilityFactor`); faithful port
  (`sp-fidelity`).
- **Guard.** None in SGL3D beyond the scenarios' outcomes.
- **Status.** Rule.

#### U-43 FSR3: reactivity is shading change

- **Rule.** FSR3 takes the 3×3 maximum of the reactive mask times `ReactivenessScale` and puts
  `max(that, detected shading change (U-44))` in its shading-change channel. That channel scales the accumulation
  counter by `1 − s` every frame (U-46), tightens the box (U-48), narrows the kernel (U-51), drains locks by
  `2 s` a frame (U-50) and multiplies luma instability by `1 − s`. At 0.9 the counter settles at about 0.037,
  about 55 % new frame each frame; at 1 every frame is a sample without history.
- **Raised by.** Blended surfaces and glow (M-11, M-17b).
- **Stays triggered while.** The followed vehicle's glass or exhaust glow is on screen: those pixels and a ring one
  render pixel wide around them keep almost no history every frame.
- **Source.** AMD `fsr3/prepare_reactivity.h` (`DilateReactiveMasks`, `PrepareReactivity`,
  `UpdateAccumulation`); faithful port (`sp-fidelity`).
- **Guard.** None in SGL3D beyond the scenarios' outcomes.
- **Status.** Rule.

#### U-45 FSR3: T&C removes protection

- **Rule.** FSR3 samples the T&C mask bilinearly, undilated, and puts `max(that, motion divergence (U-42))` in its
  "reactive" channel. That channel raises the box's tightening factor to at least its square root (U-48), drains
  locks by twice its value a frame and scales new locks by one minus it (U-50), and multiplies luma instability by
  one minus it. It leaves the accumulation counter alone.
- **Raised by.** U-11, blended coverage, mist, heat (M-11, M-16, M-17a, M-17c); U-42.
- **Stays triggered while.** Marked materials are in view; the near road at speed (U-42).
- **Source.** AMD `fsr3/prepare_reactivity.h` (`DilateTransparencyAndCompositionMasks`, `PrepareReactivity`),
  `fsr3/accumulate.h` (`RectifyHistory`, `UpdateLockStatus`); faithful port (`sp-fidelity`).
- **Guard.** None in SGL3D beyond the scenarios' outcomes.
- **Status.** Rule.

### AMD FSR2 rules

#### U-17 Previous depth from current depth: nearer scenery marks a surface disoccluded

- **Rule.** FSR2 reconstructs last frame's depth by writing each render pixel's current 3×3-nearest depth at its
  previous position (`uv + motion`, motion below 0.1 display px taken as 0) into every bilinear tap of weight above
  0.01, nearest winning, cleared each frame. Where scenery is nearer than a surface now and was behind that surface
  on screen a frame ago, its nearer depth lands on the surface, and U-18 marks the surface disoccluded although it
  was visible.
- **Raised by.** Surface depth and motion: anything that overtakes another surface in depth while crossing it on
  screen.
- **Stays triggered while.** A followed vehicle moves fast over a road: the road now between the camera and the
  vehicle was behind the vehicle's lower body a frame ago, so the vehicle's lowest band is disoccluded every frame,
  about 0.25 m of a vehicle 3 m ahead at 1.6 m a frame and still several per cent of its height at 0.5 m a frame;
  barriers, posts and traffic passing nearer than it do the same at its sides. It shows as permanent shimmer on a
  vehicle that looks still.
- **Source.** AMD `fsr2/reconstruct_dilated_velocity_and_previous_depth.h` (`ReconstructPrevDepth`,
  `FindNearestDepth`), cleared in `fsr2/lock.h` (`ClearResourcesForNextFrame`); faithful port (`sp-fidelity`).
- **Guard.** Case U9, `chase::` (`upscaled_chase`) in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`, guards the
  inputs (depth, motion scale and sign, inverted and infinite depth flags) with its scenery placed outside this
  geometry; it does not judge the followed shapes' lowest 0.25 m.
- **Status.** Rule.

#### U-18 Depth clip

- **Rule.** FSR2 compares the current dilated view depth with the reconstructed previous depth (U-17) at the 4
  bilinear taps of `uv + motion` (motion below 0.01 display px taken as 0); a tap counts where the current depth is
  farther, with `required = 1.37e-5 × Kfov × length(render size) × max(depths)` (`Kfov` = corner over centre view
  distance, about 1.5 at a 60° vertical 16:9 view: about 4 % of depth at 1707×960) and contributes
  `pow(saturate(required / difference), p)`, `p = lerp(1, 3, saturate(length(render) / length(1920, 1080)))`; the
  clip is `1 − ` the weighted mean, 0 if no tap counts. It multiplies the base accumulation by `1 − clip`, kills
  the lock at 0.1 or more, tightens the box to at least the clip, biases the kernel by `0.25 × clip`, floors the
  carried reactive factor at `0.1 × clip` and skips the luma history at 0.1 or more, per pixel, sampled bilinearly.
  It clears the next frame without a disocclusion.
- **Raised by.** Real disocclusions; U-17.
- **Stays triggered while.** U-17 fires: the followed vehicle's lowest band at speed.
- **Source.** AMD `fsr2/depth_clip.h` (`ComputeDepthClip`, `DepthClip`); faithful port (`sp-fidelity`).
- **Guard.** `a_disocclusion_recovers` (fsr2) in `tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`;
  `upscaled_disocclusion` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-19 Depth clip off on vertically receding previous depth

- **Rule.** When the reconstructed previous depths at the pixel and the rows above and below fall by more than 1 %
  per render row down the screen (`d0 − d1 > 0.01 d1` and `d1 − d2 > 0.01 d2`), FSR2 sets the pixel's depth clip
  to 0, keeping its history.
- **Raised by.** Ground planes receding from the camera.
- **Stays triggered while.** A road deck fills the lower screen: real disocclusions on it (behind an overtaking
  vehicle's lower edge) are not clipped, and the rule can cancel part of U-17 on the followed vehicle's lowest rows.
- **Source.** AMD `fsr2/depth_clip.h` (`EvaluateSurface`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-20 Dilated motion at silhouettes

- **Rule.** FSR2 gives every render pixel the motion and depth of the nearest pixel of its 3×3, so background
  pixels within one render pixel of a nearer surface reproject with that surface's motion and are clamped (U-29).
- **Raised by.** Depth edges.
- **Stays triggered while.** A followed vehicle is over a streaming road: its outline's background ring takes the
  vehicle's near-zero motion every frame.
- **Source.** AMD `fsr2/reconstruct_dilated_velocity_and_previous_depth.h` (`FindNearestDepth`,
  `ReconstructAndDilate`); faithful port (`sp-fidelity`).
- **Guard.** `a_surface_the_camera_follows_keeps_its_history` (fsr2) in
  `tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`; `following::`, `chase::` in
  `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-21 Reset frame

- **Rule.** On a reset or the context's first execution FSR2 sets its frame index to 0, so every pixel is a new
  sample (history replaced by this frame's upsample, carried reactive factor 1), and clears the lock status, the
  history colour, the scene luminance and its automatic exposure; the first execution also clears both lock
  buffers and the prepared colour. Recovery as U-27.
- **Raised by.** U-1 to U-4.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** AMD `ffx_fsr2.cpp` (dispatch) and `fsr2/accumulate.h` (`InitParams`, `Accumulate`); faithful port
  (`sp-fidelity: src/fsr2/component.rs`).
- **Guard.** As U-1.
- **Status.** Rule.

#### U-22 History off screen

- **Rule.** When a display pixel's reprojected UV lies outside [0, 1], FSR2 takes it as a new sample: history
  replaced by the upsample, lock cleared, carried reactive factor 1 (U-27).
- **Raised by.** Camera motion at the frame's edges; U-8.
- **Stays triggered while.** The camera travels forward: the frame's border is new every frame.
- **Source.** AMD `fsr2/reproject.h` (`ComputeReprojectedUVs`); faithful port (`sp-fidelity`).
- **Guard.** `upscaled_panning`, `turning` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-23 Spatial motion divergence

- **Rule.** Where a render pixel's own motion exceeds 0.01 render px, FSR2 takes over the clamped 3×3 of input
  motion the least normalised agreement with the centre's direction and writes
  `saturate(1 − agreement) × saturate(max speed / 0.01 of the screen)` into the accumulation mask (U-26).
- **Raised by.** Motion discontinuities: a followed vehicle against the road, wheels, traffic.
- **Stays triggered while.** Any surface moves differently from its neighbours: the road side of a followed
  vehicle's outline is at about 1 every frame.
- **Source.** AMD `fsr2/depth_clip.h` (`ComputeMotionDivergence`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-24 Temporal motion divergence

- **Rule.** Where a pixel's dilated motion exceeds 1 display px, FSR2 compares it with last frame's dilated motion
  at the reprojected position: `lerp(0, 1 − saturate(|previous| / |current|), saturate((px / 20)³))`, less the 3×3
  depth divergence `1 − min/max` (0 where the sky is in the 3×3; 1 on the frame's border, whose off-screen taps
  count as depth 0), and the result joins the accumulation mask (U-26).
- **Raised by.** Any point whose screen motion grows from one frame to the next.
- **Stays triggered while.** The camera travels forward: approaching scenery speeds up on screen, about
  `1 − (z / (z + v))²` (0.57 at 3 m, 0.26 at 10 m at 1.6 m a frame), so the near road and roadside are marked every
  frame; their box is already at its tightest from velocity (U-29), so the added effect is lost locks.
- **Source.** AMD `fsr2/depth_clip.h` (`ComputeTemporalMotionDivergence`, `ComputeDepthDivergence`, `DepthClip`);
  faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-27 Carried temporal reactive factor

- **Rule.** FSR2 stores per display pixel, in its history's alpha, `min(0.99, carried)`, raised towards 0.4 by
  `saturate(velocity px)`, then `max(that², 0.1 × depth clip, dilated reactive)`; a new sample stores 1; motion of
  0.1 display px or more stores it negated, which marks the pixel as moving last frame (U-28). The next frame takes
  it as its reactive floor (U-25). After a new sample it decays 1, 0.98, 0.96, 0.92, 0.85, 0.72, 0.52, 0.27, 0.07:
  about 9 frames to full accumulation.
- **Raised by.** New samples, velocity, depth clip, reactive masks.
- **Stays triggered while.** Anything moves 1 display px or more a frame: its floor stays at 0.16, accumulation at
  most 0.84 of full.
- **Source.** AMD `fsr2/accumulate.h` (`ComputeTemporalReactiveFactor`), `fsr2/reproject.h`
  (`ReprojectHistoryColor`); faithful port (`sp-fidelity`).
- **Guard.** None in SGL3D beyond the scenarios' outcomes.
- **Status.** Rule.

#### U-28 Velocity weights

- **Rule.** FSR2's base accumulation `(1 − r)(1 − clip)` falls towards `10 × w` as velocity reaches 0.1 display px
  (or if the pixel moved last frame) and towards `w` as it reaches 20 display px, `w` being this frame's Lanczos
  weight times 1/12 (about 0.06); the new frame's share is `w / (accumulation + w)`: about 0.06 at rest, 0.09 above
  0.1 px, 0.5 at 20 px. It recovers one frame after motion stops.
- **Raised by.** Surface motion.
- **Stays triggered while.** The road and scenery at speed run on about two frames of history; a followed vehicle
  loses accumulation whenever the chase camera's spring or the body's bob moves it a tenth of a display pixel.
- **Source.** AMD `fsr2/accumulate.h` (`ComputeBaseAccumulationWeight`, `Accumulate`); faithful port
  (`sp-fidelity`).
- **Guard.** `following::`, `chase::` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs` (followed shapes at zero
  motion).
- **Status.** Rule.

#### U-29 Rectification

- **Rule.** FSR2 clamps history into `centre ± scale × σ` of the 3×3 current samples (weights
  `exp(lerp(−2, −3, saturate(px / 50)) × d²)`), intersected with their minimum and maximum, with
  `scale = lerp(min(20, (1 / (render/display ratio)²)³), 1, max(clip, mask, saturate(px / 20)))`: 1 at Native AA,
  about 11.4 at Quality and 20 at Performance at rest, 1 σ in motion. Where history is outside, it is pulled to the
  clamp unless protected by `max(luma instability, lock contribution) × (1 − √r)`, and the accumulation drops to at
  most 0.1 by the same unprotected share (new frame share about 0.38).
- **Raised by.** Local colour change; velocity, depth clip, accumulation mask.
- **Stays triggered while.** Moving content is clamped at 1 σ with the accumulation cut whenever outside; marked
  materials (U-11) are clamped at 1 σ at rest.
- **Source.** AMD `fsr2/accumulate.h` (`RectifyHistory`), `fsr2/upsample.h` (`ComputeUpsampledColorAndWeight`);
  faithful port (`sp-fidelity`) but for the edge corrections in U-37.
- **Guard.** `at_rest::` (`upscaled_at_rest`), `panning`, `turning` cases in
  `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-30 Kernel bias

- **Rule.** FSR2 narrows the current frame's Lanczos kernel from `min(1.99, 1 + (display/render − 1))` towards
  `max(1, 0.3 × (1 + that))` by `max(0.25 × clip, max(r, new sample))`, so new and reactive pixels take a sharper
  single-frame reconstruction.
- **Raised by.** U-18, U-25, new samples.
- **Stays triggered while.** As U-25 and U-18.
- **Source.** AMD `fsr2/upsample.h` (`ComputeMaxKernelWeight`, `ComputeUpsampledColorAndWeight`); faithful port
  (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-31 Shading change through the lock's luma

- **Rule.** FSR2 compares `pow(exposure × exp(log luma at 1/32 of the maximum render size), 1/6)` with the luma the
  pixel's lock status holds (set when a lock is made, otherwise moved halfway each frame):
  `diff = 1 − min/max`. It raises this frame's reactive factor by `saturate((diff − 0.1) × 10)` and kills a lock of
  lifetime above 1 when `diff` exceeds 0.1, per display pixel over a coarse luma.
- **Raised by.** Large lighting changes; exposure: a locked pixel loses its lock when the exposure changes by more
  than 0.9⁻⁶ ≈ 1.88× within its lifetime (about the jitter phase count in frames: 18 at Quality, 72 at Ultra
  Performance).
- **Stays triggered while.** Fast automatic exposure at a tunnel's mouth; large emissive surfaces that change.
- **Source.** AMD `fsr2/postprocess_lock_status.h` (`GetShadingChangeLuma`, `UpdateLockStatus`); faithful port
  (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-32 Lock loss

- **Rule.** FSR2 zeroes a pixel's lock when its estimated position next frame (`uv − motion`) leaves the screen,
  when depth clip reaches 0.1, and for new samples; multiplies its lifetime by `1 − r` and `1 − mask`; and lowers
  it each frame by `w / (phase count × 0.74 / 12)`. A lock's contribution `saturate(4 × saturate(lifetime − 1))`
  times the luma ratio is what shields history from U-29.
- **Raised by.** U-18, U-25, U-26, U-31, motion towards the frame's edge.
- **Stays triggered while.** Marked materials, glass, glow and motion-divergent silhouettes carry no locks; thin
  lights on them flicker.
- **Source.** AMD `fsr2/accumulate.h` (`FinalizeLockStatus`), `fsr2/postprocess_lock_status.h`
  (`UpdateLockStatus`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-33 Lock creation

- **Rule.** FSR2 locks a render pixel that is a luma ridge (`pow(perceived luma of the tone-mapped exposed colour,
  1/6)` above or below every dissimilar neighbour, similar meaning a ratio below 1.05) with no 2×2 quadrant around
  it entirely similar; a lock read above 127/255 sets lifetime 1, or 2 on a relock. Locks protect history; this
  rule withholds nothing.
- **Raised by.** Thin bright or dark features.
- **Stays triggered while.** Nothing is rejected.
- **Source.** AMD `fsr2/lock.h` (`ComputeThinFeatureConfidence`), `fsr2/reproject.h`
  (`ReprojectHistoryLockStatus`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-34 Luma instability

- **Rule.** Where depth clip, accumulation mask and shading-change difference are all below 0.1 and the sample is
  not new, FSR2 reads four frames of 8-bit tone-mapped luma; a pixel whose current luma is closer to an older
  same-sign entry than to the last, with a box luma σ large enough (`pow(saturate(σ / 0.1), 6) > 1/255`), counts as
  unstable, times `1 − max(mask, r^(1/6))`, and its history is shielded from U-29. Elsewhere it gives no shield.
- **Raised by.** Oscillating luma.
- **Stays triggered while.** Nothing is rejected; marked and reactive pixels lose the shield (U-25, U-26).
- **Source.** AMD `fsr2/accumulate.h` (`ComputeLumaInstabilityFactor`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-35 Input colour bound

- **Rule.** FSR2 divides input colour by `preExposure` (1), multiplies it by the exposure and clamps it to
  [0, 65504]; nothing is rejected.
- **Raised by.** Over-range radiance.
- **Stays triggered while.** Nothing is rejected.
- **Source.** AMD `fsr2/common.h` (`PrepareRgb`); faithful port (`sp-fidelity`).
- **Guard.** Every FSR2 scenario's radiance bound.
- **Status.** Rule.

#### U-36 Inactive AMD paths

- **Rule.** FSR2's internal automatic exposure is computed but not bound, its automatic reactive and T&C generation
  is off, and its generate-reactive call schedules nothing; none of them rejects history in SGL3D.
- **Raised by.** Nothing.
- **Stays triggered while.** Nothing.
- **Source.** AMD `ffx_fsr2.cpp`; faithful port (`sp-fidelity`, adaptation SDK-P25).
  Code: `crates/sgl-3d/src/stages/antialiasing/fsr.rs` (`Fsr::new`, `Fsr::dispatch`).
- **Guard.** None.
- **Status.** Rule.

#### U-37 Frame-edge corrections

- **Rule.** FSR2's accumulate pass leaves taps outside the render area out of the rectification box's weights and,
  by clamped loads, out of its bounds, and gives a new sample whose Lanczos weights sum to `FSR2_EPSILON` or less
  the box centre; at the frame's edges history is clamped as an interior pixel's is.
- **Raised by.** Pixels at the frame's edges.
- **Stays triggered while.** Nothing beyond the edges.
- **Source.** AMD `fsr2/upsample.h` (`ComputeUpsampledColorAndWeight`), modified: `sp-fidelity` adaptations SDK-P35,
  SDK-P36, SDK-P37, recorded upstream bug fixes.
- **Guard.** The port's oracle and FSR2's radiance bound in the scenarios.
- **Status.** Rule.

### AMD FSR3 rules

#### U-38 Reset frame

- **Rule.** On a reset or first execution FSR3 sets its frame index to 0, making every pixel a new sample, and
  clears the previous accumulation counter, its luminance mips and frame information; the first execution also
  clears both counters and both luma buffers. The counter then climbs as U-46: two frames without history, full
  weight on the fourth. Luma history is not cleared, but instability is computed only once the counter exceeds 0.9.
- **Raised by.** U-1 to U-4.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** AMD `ffx_fsr3upscaler.cpp` (dispatch), `fsr3/accumulate.h` (`InitPassData`); faithful port
  (`sp-fidelity: src/fsr3upscaler/component.rs`).
- **Guard.** As U-1.
- **Status.** Rule.

#### U-39 History off screen

- **Rule.** When a display pixel's reprojected UV lies outside [0, 1], FSR3 takes it as a new sample, reads its
  accumulation counter as 0 (two frames without history, U-46), and computes no shading change or luma instability
  there.
- **Raised by.** Camera motion at the frame's edges; U-8.
- **Stays triggered while.** The camera travels forward: the frame's border every frame.
- **Source.** AMD `fsr3/reproject.h` (`ComputeReprojectedUVs`), `fsr3/prepare_reactivity.h` (`UpdateAccumulation`);
  faithful port (`sp-fidelity`).
- **Guard.** `upscaled_panning`, `turning` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-40 Previous depth from current depth: nearer scenery marks a surface disoccluded

- **Rule.** FSR3 reconstructs last frame's depth as U-17 does, from the 3×3-nearest current depth written at
  `uv + motion`, nearest winning, cleared every dispatch, with motion below `lerp(0.25, 0.75, saturate(depth / 100 m))`
  4K px taken as 0. Where scenery is nearer than a surface now and was behind that surface on screen a frame ago,
  U-41 marks the surface disoccluded although it was visible.
- **Raised by.** As U-17.
- **Stays triggered while.** As U-17: a followed vehicle's lowest band over a road at speed, and its sides where
  barriers or traffic pass nearer than it, every frame; it shows as permanent shimmer on a vehicle that looks still.
  The thresholds assume SGL3D's units are metres.
- **Source.** AMD `fsr3/prepare_inputs.h` (`ReconstructPrevDepth`, `FindDepthExtents`), `fsr3/common.h`
  (`ReconstructedDepthMvPxThreshold`); faithful port (`sp-fidelity`).
- **Guard.** Case U9, `chase::` (`upscaled_chase`) in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`, guards the
  inputs with its scenery placed outside this geometry; it does not judge the followed shapes' lowest 0.25 m.
- **Status.** Rule.

#### U-41 Disocclusion

- **Rule.** FSR3 compares the current dilated view depth with the reconstructed previous depth (U-40) at the 4
  bilinear taps of `uv + motion` (motion below U-40's threshold taken as 0); only if every counted tap is farther now
  does it compute `1 − mean(saturate(required / difference))` with `required = 1.37e-5 × length(render size / 2) ×
  max(depths)` (about 1.3 % of depth at 1707×960), else 0. Full disocclusion sets the counter to `min(−1/3, it)`
  (two frames without history, U-46), drains locks by twice its value, removes the history's shield by `1 − d`,
  widens the kernel (U-51) and multiplies luma instability by `1 − d`. Full weight returns on the fourth frame.
- **Raised by.** Real disocclusions; U-40.
- **Stays triggered while.** U-40 fires.
- **Source.** AMD `fsr3/prepare_reactivity.h` (`ComputeDisocclusions`); faithful port (`sp-fidelity`).
- **Guard.** `a_disocclusion_recovers` (fsr3) in `tests/sgl-3d/tests/scenario_temporal_antialiasing.rs`;
  `upscaled_disocclusion` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-42 Motion divergence

- **Rule.** FSR3 compares a pixel's dilated motion with this frame's dilated motion at its reprojected position:
  `(1 − saturate(|there| / |here|)) × (min/max of the two depths) × saturate(4K px / 10)`, into its reactive channel
  (U-45).
- **Raised by.** Pixels whose previous position is now covered by a slower surface; approaching scenery.
- **Stays triggered while.** The camera travels forward: the near road and roadside are marked every frame (about
  0.37 at 3 m, 0.22 at 10 m at 1.6 m a frame), which drains their locks; a followed vehicle's own pixels, at
  near-zero motion, are not.
- **Source.** AMD `fsr3/prepare_reactivity.h` (`ComputeMotionDivergence`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-44 Detected shading change

- **Rule.** FSR3 compares, per render pixel, a sorted cross of five current luma samples with five from the previous
  frame at the reprojected position, both times the current exposure, takes the smallest signed relative difference
  `1 − min/max`, averages difference and sign over half, quarter and eighth render size, and takes the largest
  `|mean difference × mean sign|` as shading change (times `ShadingChangeScale`), joined with the reactive mask in
  one channel (U-43).
- **Raised by.** Light pools sweeping the road, reflections moving differently from their surface, blinking
  signs, moving normal layers; thin lights appearing and vanishing between jitter phases.
- **Stays triggered while.** A lit road at speed: specular and reflections on the deck and passing headlights keep
  its counter below full every frame, over neighbourhoods of half-resolution pixels and coarser.
- **Source.** AMD `fsr3/shading_change_pyramid.h` (`ComputeMinimumDifference`, `SpdLoadSourceImage`),
  `fsr3/shading_change.h` (`ShadingChange`), `fsr3/prepare_reactivity.h` (`ComputeShadingChange`); faithful port
  (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-46 Accumulation counter

- **Rule.** FSR3 reads the previous counter at the reprojected position (0 off screen), scales it by
  `1 − shading change`, lowers it towards `min(−1/3, it)` by the disocclusion, sets it to 0 at 0.015 or below, and
  stores `saturate(it + 1/3)`; the counter is the history weight before U-47. At 0 the pixel is an initial sample:
  the 3×3 box centre with no history. A full disocclusion gives two such frames, then 1/3, 2/3 and 1; a steady
  shading change `s` settles the counter at `(1 − s) / (3 s)`.
- **Raised by.** U-39, U-41, U-43, U-44.
- **Stays triggered while.** Glass, glow and the followed vehicle's lowest band at speed never reach full
  accumulation.
- **Source.** AMD `fsr3/prepare_reactivity.h` (`UpdateAccumulation`), `fsr3/upsample.h`
  (`ComputeUpsampledColorAndWeight`), `fsr3/accumulate.h` (`InitPassData`); faithful port (`sp-fidelity`).
- **Guard.** None in SGL3D beyond the scenarios' outcomes.
- **Status.** Rule.

#### U-47 Velocity weight

- **Rule.** FSR3 caps the history weight at `lerp(counter, 0.15, saturate(4K px × VelocityFactor / 0.5))`: full at
  0.5 4K px (about 0.25 display px at 1080p); the new frame's share, `w / (history + w)` with `w` about 0.046, goes
  from about 0.04 at rest to about 0.24 in motion, a window of about four frames. It returns as soon as motion stops.
- **Raised by.** Surface motion.
- **Stays triggered while.** The whole road and scenery at speed; a followed vehicle whenever the chase camera lags
  or the body moves a quarter of a display pixel. It shows as render-resolution aliasing on moving scenery.
- **Source.** AMD `fsr3/accumulate.h` (`ComputeBaseAccumulationWeight`); faithful port (`sp-fidelity`).
- **Guard.** `following::`, `chase::` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs` (followed shapes at zero
  motion).
- **Status.** Rule. `VelocityFactor`, AMD's control for this trade-off, stays at AMD's 1 and is not exposed: a history
  weight is SGL3D's to choose, never a game field (S3D-6), and zeroing AMD's velocity factor in Hyperdrive raised the error on a moving craft's flat regions against a supersampled reference from 0.0175 to 0.020, zeroing its shading-change and reactiveness scales as well left it there, and with exhaust effects in view all three at zero raised it from 0.018 to 0.055 (O-6, 2026-11-18).

#### U-48 Rectification

- **Rule.** FSR3 clamps history onto the ellipsoid `centre ± scale × σ` of the 3×3 current samples (weights
  `exp(−2.3 d²)`, luma axis × 1.7) with `scale = lerp(3, 1, T)`,
  `T = max(saturate(4K px / 20), saturate(0.75 − farthest depth / 20 m), 1 − counter, √reactive channel,
  shading change)`; the history keeps `max(luma instability, lock contribution) × counter × (1 − disocclusion)` of
  its own value.
- **Raised by.** Motion, near depth, a low counter, marks.
- **Stays triggered while.** Anything within 15 m is always clamped tighter than distant scenery (a vehicle 3 m
  ahead at about 1.8 σ at rest); moving content at 1 σ.
- **Source.** AMD `fsr3/accumulate.h` (`RectifyHistory`); faithful port (`sp-fidelity`).
- **Guard.** `at_rest::`, `chase::` in `tests/sgl-3d/tests/scenario_fsr_upscaling.rs`.
- **Status.** Rule.

#### U-49 Luma instability

- **Rule.** Where the reprojection is on screen and the counter exceeds 0.9, FSR3 marks a pixel unstable when its
  exposed luma is more similar to an older same-sign entry of its four-frame history than to the last, times
  `(1 − saturate(4K px / 20)) (1 − disocclusion) (1 − reactive channel) (1 − shading change)`; instability shields
  history from U-48.
- **Raised by.** Oscillating luma.
- **Stays triggered while.** Nothing is rejected; moving and marked pixels have no shield.
- **Source.** AMD `fsr3/luma_instability.h` (`LumaInstability`, `ComputeLumaInstabilityFactor`); faithful port
  (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-50 Locks

- **Rule.** FSR3 creates a lock of strength `1 − min/max` of the 3×3 exposed luma at a ridge (no quadrant similar
  within 0.9 of the range), above 0.01, scaled by `1 − reactive channel`; it zeroes the lock for new samples and
  where `uv − motion` leaves the screen, lowers it by `2 × max(shading change, reactive channel, disocclusion)` and
  by `0.1 / phase count` a frame, caps it at 2, and shields history by `saturate(lock − 1)`.
- **Raised by.** U-41, U-42, U-43, U-44, U-45.
- **Stays triggered while.** Marked materials, glass, glow, the near road at speed and the followed vehicle's lowest
  band carry no locks.
- **Source.** AMD `fsr3/prepare_reactivity.h` (`ComputeThinFeatureConfidence`), `fsr3/accumulate.h`
  (`UpdateLockStatus`); faithful port (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

#### U-51 Kernel bias

- **Rule.** FSR3 narrows the current frame's kernel from `min(1.99, display/render)` towards
  `max(1, 0.3 × (1 + that))` as `min(1 − 0.5 × disocclusion, 1 − shading change, saturate(5 × history weight))`
  falls.
- **Raised by.** U-41, U-43, U-44, U-47 (history weight 0.15 gives 0.75).
- **Stays triggered while.** Everything moving.
- **Source.** AMD `fsr3/upsample.h` (`ComputeMaxKernelWeight`, `ComputeUpsampledColorAndWeight`); faithful port
  (`sp-fidelity`).
- **Guard.** None.
- **Status.** Rule.

### Constants and flags left at AMD's defaults

#### U-52 AMD defaults SGL3D keeps

- **Rule.** SGL3D never calls `ffxFsr3UpscalerSetConstant`, so FSR3 runs with `VelocityFactor` 1 (U-47),
  `ReactivenessScale` 1 (U-43), `ShadingChangeScale` 1 (U-44), `AccumulationAddedPerFrame` 1/3 and
  `MinDisocclusionAccumulation` −1/3 (U-46). Neither version sets automatic exposure (SGL3D supplies the tone map's,
  U-16), motion-vector jitter cancellation (its motion is unjittered), display-resolution motion vectors, dynamic
  resolution (a constant SDK 1.1.4 does not read; SGL3D resets instead, U-4) or debug checking, and FSR3's
  `DRAW_DEBUG_VIEW` dispatch flag is never set, so AMD's view of motion, locks, depth, disocclusion, shading change
  and reactivity is unavailable.
- **Raised by.** Every FSR frame.
- **Stays triggered while.** Always.
- **Source.** AMD `ffx_fsr3upscaler.h` defaults; faithful port (`sp-fidelity: src/fsr3upscaler/component.rs`).
  Code: `crates/sgl-3d/src/stages/antialiasing/fsr.rs` (`Fsr::new`, `Fsr::dispatch`).
- **Guard.** None.
- **Status.** Rule. The constants and dispatch flags stay at AMD's defaults and none is exposed: thresholds, history
  weights and other internals are SGL3D's to choose, never game fields (S3D-6), and zeroing AMD's velocity factor in Hyperdrive raised the error on a moving craft's flat regions against a supersampled reference from 0.0175 to 0.020, zeroing its shading-change and reactiveness scales as well left it there, and with exhaust effects in view all three at zero raised it from 0.018 to 0.055 (O-6, 2026-11-18).

## Reflections

Three reflection paths keep temporal history: Crystal (DiligentFX screen-space reflections in
`sgl-post-fx`, driven through the post-effect context), Velvet (Godot's screen-space
reflections with Wicked Engine's temporal pass) and world-space rays (Wicked's RT reflection
denoiser). Velvet and world rays accumulate through one shared temporal reprojection. No
reflection path reads a frame-wide or scene-wide motion signal: whole-view restarts come only
from the view's reset, a frame the stage did not run in, a resize or a settings change. A continuously
running game trips the per-pixel rules: Crystal's reduced history weight where the receiver or the reflected source moves
(R-9), roughness-cutoff re-entry under jitter (R-6, R-25), world rays filling in where the
screen-space method lets go (R-30), and the variance resets that keep a blur on wherever a
rejection repeats (R-16, R-29).

### Crystal screen-space reflections

#### R-1 Crystal restarts on the view's reset

- **Rule.** When `HistoryFrame::valid` is false (a camera cut, a `View::resize` that
  changed the targets, FSR starting or stopping, or a different `Scene`), `PostFx::prepare`
  raises `reset_accumulation`; `ScreenSpaceReflection::reset_history` clears the radiance and
  variance histories to 0 and the temporal pass (`g_HistoryReset` = 1) outputs this frame's
  resolved radiance and variance 1 at every pixel. `PostFx::begin` clears the previous depth to
  the far plane and uses this frame's camera as the previous one; with TAA the frame is
  unjittered. Over the whole view. Accumulation resumes the next frame at weight 0.95.
- **Raised by.** `FrameInput::camera_cut`, resizes that change the targets, FSR start/stop,
  switching scene.
- **Stays triggered while.** Nothing: it lasts one frame (the blur it starts lasts longer, R-16).
- **Source.** SGL3D's own: DiligentFX SSR has no reset; the rule is DiligentFX TAA's
  `AccumulationBufferInfo::UpdateConstantBuffer` (DFX-12, DFX-49). Code:
  `sgl-3d/src/view/post_fx.rs` (`PostFx::prepare`, `PostFx::begin`),
  `sgl-post-fx/src/screen_space_reflection.rs` (`reset_history`).
- **Guard.** `camera_cut_restarts_history` (`tests/sgl-3d/tests/screen_space_reflections.rs`);
  `a_reset_frame_takes_the_current_radiance` (`sgl-post-fx/src/ssr_temporal_neighborhood_tests.rs`).
- **Status.** Rule.

#### R-2 Crystal restarts after a frame it did not run in

- **Rule.** When the stage did not run in the view's last finished frame (`PostFx::prepare`
  against its kept frame index), Crystal resets exactly as R-1. In `sgl-post-fx`,
  `ScreenSpaceReflection::reset_history` resets when the frame index is not the last one plus
  one (`last_frame_idx`, `None` before the first execution), except that a repeated index
  repeats that frame, its reset included. A frame rendered again reads and writes the same
  halves: its index is the first rendering's.
- **Raised by.** A frame without Crystal (F-4, F-6), a render that rebuilt the post-effect context
  because its TAA flag changed (F-5), the view's reset (F-1).
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own (DFX-12, DFX-49). Code: `sgl-3d/src/view/post_fx.rs`
  (`PostFx::prepare`, `PostFx::settle`), `sgl-post-fx/src/screen_space_reflection.rs` (`reset_history`).
- **Guard.** `a_reset_frame_takes_the_current_radiance` (frames 0, 1, 3);
  `a_repeated_frame_index_keeps_history` (`sgl-post-fx`, `ssr_temporal_neighborhood_tests.rs`);
  `crystal_keeps_its_history_across_an_unfinished_frame`
  (`tests/sgl-3d/tests/screen_space_reflections.rs`).
- **Status.** Rule.

#### R-3 Crystal recreates its histories on a size, half-resolution or depth-convention change

- **Rule.** When the render size changes, `FeatureFlags::HALF_RESOLUTION` toggles or reversed
  depth toggles, `ScreenSpaceReflection::prepare_resources` recreates every history zeroed and
  sets `last_frame_idx` to `None`, so the next execution resets as R-1; the context recreates
  its reprojected and previous depths on a size or format change. Whole stage; one frame.
- **Raised by.** Resizes; `Settings::screen_space_reflections` Half <-> Full.
- **Stays triggered while.** Nothing: it needs a setting change or resize.
- **Source.** DiligentFX recreates on size; the reset on recreation is SGL3D's (DFX-49, DFX-59).
  Code: `sgl-post-fx/src/screen_space_reflection.rs` (`prepare_resources`),
  `sgl-post-fx/src/post_fx_context.rs` (`prepare_resources`).
- **Guard.** `resize_restarts_at_the_new_size` (`tests/sgl-3d/tests/screen_space_reflections.rs`);
  `a_reset_frame_takes_the_current_radiance` (half-resolution frame).
- **Status.** Rule.

#### R-4 Crystal is dropped with its context

- **Rule.** When the effective method is not Crystal or screen-space reflections are off, the
  view drops Crystal (`PostFx::release_screen_space_reflections`); when `effective.post_fx`
  (TAA or Crystal) turns off or `effective.taa` differs from the living context's, the view
  recreates the whole `PostFx`. Either way Crystal's next frame has no history (R-2). Whole
  stage; one frame.
- **Raised by.** Changing the reflection method, turning SSR off, switching antialiasing between
  TAA and SMAA, Off or FSR (including the effective fallback to TAA when FSR does not run).
- **Stays triggered while.** Nothing: it needs a setting change. A game sees reflections restart
  when the player changes antialiasing, although Crystal's history does not depend on TAA.
- **Source.** SGL3D's own. Code: `sgl-3d/src/renderer/frame.rs` (`render_view`),
  `sgl-3d/src/view/post_fx.rs` (`PostFx::new`, `release_screen_space_reflections`).
- **Guard.** `switching_off_discards_history` (`tests/sgl-3d/tests/screen_space_reflections.rs`).
  The TAA toggle: None.
- **Status.** Rule.

#### R-5 Crystal has no transition fade

- **Rule.** The post-effect context is created with `transition_duration` 0, so
  `get_transition_alpha` returns 1 and `AlphaInterpolation` never fades Crystal's output in
  after creation or a reset, where DiligentFX fades it over 1 s.
- **Raised by.** Nothing: always in effect.
- **Stays triggered while.** Nothing: it withholds no history.
- **Source.** Modified from DiligentFX (DFX-2, DFX-24). Code: `sgl-3d/src/view/post_fx.rs`
  (`PostFx::new`), `sgl-post-fx/src/post_fx_context.rs` (`get_transition_alpha`).
- **Guard.** None.
- **Status.** Rule.

#### R-6 Pixels leaving and re-entering the reflection mask

- **Rule.** A pixel is in the mask when its traced lobe's perceptual roughness is at most
  `RoughnessThreshold` (0.2) and its depth is not background (`IsReflectionSample`). The
  temporal pass clears its targets to radiance 0 and variance 1 before drawing through the mask,
  so a pixel outside it holds no history. A pixel that re-enters reprojects bilinearly into those
  zero texels; if its depth test passes, the zero history is accepted, clamped up to
  mean − 2σ (R-15), and variance 1 starts the cleanup blur (R-16). Per pixel (2×2 footprint).
  Radiance recovers with time constant 20 frames, variance in about 65.
- **Raised by.** Roughness textures or animation near 0.2, receivers appearing or leaving,
  sky silhouettes of moving objects.
- **Stays triggered while.** Textured roughness straddling 0.2 under TAA or FSR jitter puts
  pixels in and out of the mask from frame to frame, each exit zeroing their history: in a
  jittered still view of a mirror floor whose roughness alternates 0.1 and 0.3 or varies
  0.1–0.3, 4–20 % of its pixels re-entered every frame. Composition fades the method out over
  the last 0.05 below the cutoff (R-25's `specular_trace_fade`, Bevy's), so a pixel near the
  cutoff shows little of it; measured through SGL3D's composite under TAA, keeping a pixel's
  history across its exit instead flickered as much or more (mean second difference 0.024 and
  0.028, 0.023 and 0.028, 0.051 and 0.104 on three roughness textures, zeroed and kept): the
  flicker that remains is the jittered roughness crossing the fade, which TAA resolves.
- **Source.** Threshold faithful to DiligentFX and AMD SSSR; the per-frame clear is SGL3D's
  (DFX-51) in place of upstream's stale history, equivalent to AMD's environment history for
  pixels without a ray (premultiplied radiance 0 is the environment in composition); eligibility
  decided once (DFX-62). Wicked passes untraced pixels through as zero with no fade; Bevy and
  Godot, which keep no reflection history, fade at the cutoff as SGL3D does. Code:
  `sgl-post-fx/shaders/wgsl/PostProcess/ScreenSpaceReflection/private/SSR_ComputeStencilMaskAndExtractRoughness.wgsl`,
  `sgl-post-fx/src/screen_space_reflection.rs` (`compute_temporal_accumulation`),
  `sgl-3d/src/shading/specular_lobes.wgsl` (`specular_trace_fade`).
- **Guard.** `pixels_returning_to_the_mask_keep_no_old_radiance`
  (`sgl-post-fx/src/ssr_temporal_neighborhood_tests.rs`); `texels_outside_the_mask_do_not_change_their_neighbours`;
  `reflections_are_continuous_across_the_roughness_cutoff` (`tests/sgl-3d/tests/screen_space_reflections.rs`, the fade).
- **Status.** Rule.

#### R-7 Denoiser tiles without a confident hit

- **Rule.** When no ray with confidence above 0 lies within `DenoiserReach` of an 8×8 tile
  (clamp(ceil(`SpatialReconstructionRadius`), 0, 8) plus 1 pixel at full resolution or 2 at
  half, plus 1 for the temporal neighbourhood, plus clamp(i32(2 × `BilateralCleanupSpatialSigmaFactor`), 0, 2);
  with SGL3D's attributes one tile of dilation), the temporal pass writes radiance history 0 and
  variance history 1 there. The radiance equals what the box clamp gives; the variance starts
  the cleanup blur (R-16) when hits return, about 65 frames.
- **Raised by.** Reflections leaving the screen, the edge vignette, thin reflected objects whose
  hits come and go.
- **Stays triggered while.** Sparse hits that flicker in and out of a tile under jitter (thin
  distant emitters) reset its variance each time, keeping the blur on there.
- **Source.** SGL3D's own module after AMD SSSR `ClassifyTiles` (DFX-29); unlike AMD, skipped
  tiles' histories are written. Code: `SSR_ComputeDenoiserTiles.wgsl`
  (`ClassifyDenoiserTilesPS`, `DilateDenoiserTilesPS`), `SSR_ComputeTemporalAccumulation.wgsl`.
- **Guard.** `skipping_tiles_without_hits_changes_no_radiance`, `tiles_that_stop_hitting_keep_no_history`
  (`sgl-post-fx/src/ssr_temporal_neighborhood_tests.rs`).
- **Status.** Rule.

#### R-8 Choice between the surface and the reflection-hit history

- **Rule.** The temporal pass reads history either at the incident point (this pixel moved by
  its motion vector) or at the reflection's virtual point (the hit distance beyond the surface
  along the view ray, projected through the previous camera alone; a point on or behind the
  previous camera's plane goes a viewport off screen). An off-screen candidate loses; otherwise
  the candidate whose confidence-premultiplied luminance is nearer the current pixel wins,
  with ties going to the hit. The history lookups use stable pixel coordinates. Motion validation compares camera and surface positions on the previous raster grid, including the change of jitter.
  Per pixel, every frame.
- **Raised by.** Any motion or jitter.
- **Stays triggered while.** Nothing by itself: it selects, it does not reject (R-9 and R-10 do).
- **Source.** Wicked Engine `ssr_temporalCS` and SEED DD18 through DiligentFX, faithful;
  modified by DFX-57 (in-bounds first), DFX-25 (AMD virtual point) and DFX-31 (behind-camera).
  Code: `SSR_Reprojection.wgsl` and `SSR_ComputeTemporalAccumulation.wgsl`.
- **Guard.** `offscreen_candidates_cannot_discard_valid_reflection_history`,
  `reflection_hits_behind_the_previous_camera_take_no_history`,
  `resolved_depth_is_the_virtual_point_along_the_view_ray`
  (`sgl-post-fx/src/ssr_temporal_neighborhood_tests.rs`).
- **Status.** Rule.

#### R-9 Reflection history follows the moving receiver and reflected source

- **Rule.** Judge how much of a previous pixel can represent this reflection using the overlap
  of two unit pixel footprints. Coincident centres have full overlap. A one-pixel separation
  along either axis leaves none. At the receiver, compare the observed surface motion with
  camera-only motion and with the virtual-point reprojection. Their greater overlap gives the
  support available from either explanation. If the virtual point supplied the history,
  the previous geometry depth at the accepted raster address, including the fallback
  search offset, gives another estimate of the reflecting plane.
  Translating a plane along its normal moves a stationary object's mirror image by twice
  that distance. A tangential translation leaves it in place.
  At each confirmed source hit, motion XY and valid depth motion reconstruct the source's
  previous position. Without depth motion its previous camera-space depth comes from the
  camera. Previous sky or a depth mismatch removes that ray's history support.
  Otherwise Crystal reflects the source displacement across the receiver's tangent plane
  and measures overlap in the previous camera's image. Reconstruction averages this evidence
  over the contributing measured rays with the same BRDF, tone-map and confidence weights.
  Receiver and source support scale the radiance weight. R-10 still tests the receiver depth.
- **Raised by.** A reflector moving independently of the camera where the reflection has its
  own parallax, or a reflected object moving independently at the hit. A change in incident
  light on a stationary surface can retain history even when the local spatial variance is zero.
- **Stays triggered while.** The motion leaves the two footprints disjoint. The next frame
  measures its own overlap.
- **Source.** Crystal's temporal pass and hit validation. The distinction between motion of
  the reflector and motion of the reflected object also underlies Microsoft's animated planar
  reflection example. [DirectX ray-tracing sample](https://github.com/microsoft/DirectX-Graphics-Samples/tree/master/Samples/Desktop/D3D12Raytracing/src/D3D12RaytracingRealTimeDenoisedAmbientOcclusion).
- **Guard.** `a_reflected_highlight_moves_by_its_own_parallax`,
  `a_slowly_moving_mirror_keeps_history_as_the_light_changes`,
  `a_mirror_sliding_in_its_plane_keeps_the_stationary_lights_history`,
  `a_moving_panel_reveals_the_reflected_background`,
  `a_wall_appearing_over_infinite_sky_lights_its_reflection` and
  `a_sign_moving_along_the_sightline_moves_in_the_floor_mirror` in `sgl-post-fx`.
- **Status.** Rule. The rejected surface-history workaround and the comparison with AMD’s
  denoiser are recorded in the [port notes](../crates/sgl-post-fx/PROVENANCE.md#crystal-reflection-history-819).

#### R-10 Disocclusion by the surface's own previous depth

- **Rule.** The temporal pass keeps history only when
  exp(−|Ze − Zp| / max(Ze, Zp)) > `SSR_DISOCCLUSION_THRESHOLD` (0.9), a relative depth
  difference under 10.54 %, where Zp is the previous depth, point-sampled at the candidate, and
  Ze the surface's expected depth in the previous camera (`LoadExpectedPrevDepth`). Where the
  motion input's W is above 0, Ze is the surface's camera Z now plus the depth motion in Z
  (previous minus current camera Z, NRD's viewZprev = viewZ + motion.z), which holds the
  surface's own motion and the camera's; a surface that was on or behind the previous camera's
  plane takes that camera's near-plane depth (R-13). Where W is 0, Ze is this frame's point
  projected through the previous camera alone (`ComputeReprojectedDepth`). Both are linearised
  with the previous projection (R-14). On failure R-11's search runs; if that fails, all history
  is discarded (radiance current, variance 1). Per pixel, one frame.
- **Raised by.** Real disocclusion and silhouettes. Static surfaces the camera approaches and
  surfaces the camera follows both pass. A surface without valid depth motion (behind the
  previous camera, or a depth change beyond 65504 m) is judged as static.
- **Stays triggered while.** Nothing in ordinary motion.
- **Source.** DiligentFX (`ScreenSpaceReflection.cpp` binds `GetReprojectedDepth`), modified
  (DFX-63, with DFX-43's projection fix): the surface's own depth, as AMD's reflection denoiser
  and Wicked compare history with this frame's depth, taken from depth motion as TAA's adaptive
  test (T-15, `AdaptiveMotionExpectedDepth`) and the shared reprojection (R-21) take it.
  Code: `SSR_ComputeTemporalAccumulation.wgsl` (`LoadExpectedPrevDepth`, `ComputeReprojection`,
  `ComputeDisocclusion`), `sgl-post-fx/shaders/wgsl/Common/private/ComputeReprojectedDepth.wgsl`.
- **Guard.** `a_surface_the_camera_follows_keeps_its_reflection_history`
  (`sgl-post-fx/src/ssr_followed_surface_tests.rs`: a plane 2 m ahead of a camera that travelled
  10 m along its axis keeps its history at every pixel, under both depth conventions);
  `a_changed_near_plane_keeps_history`, `surfaces_behind_the_previous_camera_take_no_history`
  (`sgl-post-fx/src/ssr_temporal_neighborhood_tests.rs`).
- **Status.** Rule.

#### R-11 Fallback search from the reprojected position outwards

- **Rule.** When R-10's test fails, the pass searches nine positions around the candidate: the
  candidate itself, then its four edge neighbours (up, left, right, down), then its four
  corners, weighting each position's four bilinear taps by `ComputeDisocclusion` > 0.45
  (relative difference under 79.9 %). A position replaces the best so far only with a strictly
  greater total weight, so ties keep the nearer position, and the search stops once the best
  exceeds 0.9. It succeeds when the best exceeds 0.1 and reads radiance from that position's
  weighted taps and variance there. On a uniform surface the candidate itself wins. Beyond
  79.9 % at every tap it fails and discards all history.
- **Raised by.** Silhouettes where the point-sampled test misses; depths 10.5–79.9 % from the
  expected depth.
- **Stays triggered while.** A silhouette the motion keeps in place: the history read there moves
  at most a pixel toward the side that matches, bounded by the 2σ clamp.
- **Source.** DiligentFX's weighted taps and thresholds, modified (DFX-63): the order of AMD's
  `FFX_DNSR_Reflections_PickReprojection` and Wicked's `ssr_temporalCS` (kept in R-21), which
  start at the reprojected position and move only to a strictly better weight. Code:
  `SSR_ComputeTemporalAccumulation.wgsl` (`ComputeReprojection`).
- **Guard.** `the_fallback_search_keeps_a_followed_surfaces_history_in_place`
  (`sgl-post-fx/src/ssr_followed_surface_tests.rs`: a uniform plane whose test fails at every
  position reads its own pixel's history).
- **Status.** Rule.

#### R-12 Off-screen history and hits behind the previous camera

- **Rule.** When the chosen previous position lies outside the viewport, or the virtual point
  lies on or behind the previous camera's plane (clip w ≤ 0, placed a viewport off screen), the
  temporal pass discards all history at the pixel. One frame per pixel.
- **Raised by.** Camera motion bringing content in at the screen edges; motion vectors limited
  to two screens for points behind the previous camera.
- **Stays triggered while.** Nothing beyond the edge band a moving camera uncovers.
- **Source.** DiligentFX faithful, plus SGL3D's behind-camera guard (DFX-31). Code:
  `SSR_ComputeTemporalAccumulation.wgsl` (`ComputeReflectionHitPosition`, `ComputeReprojection`).
- **Guard.** `reflection_hits_behind_the_previous_camera_take_no_history`,
  `offscreen_candidates_cannot_discard_valid_reflection_history`.
- **Status.** Rule.

#### R-13 Surfaces at or nearer than the previous near plane

- **Rule.** A surface on or behind the previous camera's plane has the previous camera's
  near-plane depth as its expected depth (R-10): `LoadExpectedPrevDepth` writes it where the
  depth motion puts the surface there, and `ComputeReprojectedDepth` where W is 0 and the camera
  does. The temporal pass discards all history where the expected depth is at or nearer than
  that plane (`IsAtOrNearerThanNearPlane`). One frame per pixel.
- **Raised by.** A camera that moves and turns so a surface was behind it last frame.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own (DFX-32). Code: `ComputeReprojectedDepth.wgsl`,
  `SSR_ComputeTemporalAccumulation.wgsl` (`ComputeReprojection`).
- **Guard.** `surfaces_behind_the_previous_camera_take_no_history`.
- **Status.** Rule.

#### R-14 Near-plane changes keep history

- **Rule.** The temporal pass linearises both the reprojected and the previous depth with the
  previous camera's projection, so a change of near or far plane between frames rejects nothing.
- **Raised by.** Nothing: it removes a rejection.
- **Stays triggered while.** Nothing.
- **Source.** SGL3D fix of DiligentFX (DFX-43). Code: `SSR_ComputeTemporalAccumulation.wgsl`
  (`ComputeReprojection`).
- **Guard.** `a_changed_near_plane_keeps_history`.
- **Status.** Rule.

#### R-15 Crystal's box clamp and history weights

- **Rule.** Every accepted history is clamped to the 3×3 mean ± 2σ (`SSR_TEMPORAL_VARIANCE_GAMMA`)
  of this frame's resolved radiance, per channel including confidence, then mixed at
  up to `TemporalRadianceStabilityFactor` 0.95, scaled by the receiver and source
  support in R-9. Variance mixes at `TemporalVarianceStabilityFactor` 0.9.
  The first supported history after a reset can already weigh 0.95. Neighbours
  outside the mask or in skipped tiles contribute 0 to the box.
- **Raised by.** Every frame.
- **Stays triggered while.** Reprojection succeeds. At mask edges the zero neighbours
  can admit a darker history into the box.
- **Source.** Wicked `ssr_temporalCS` constants through DiligentFX (DFX-23, DFX-25, DFX-49).
  Code: `SSR_ComputeTemporalAccumulation.wgsl` (`ComputePixelStatistic`).
- **Guard.** `departed_reflections_cannot_support_current_dark_history`.
- **Status.** Rule.

#### R-16 Variance reset holds the cleanup blur on

- **Rule.** Every path that keeps no history writes variance 1. Bilateral cleanup blurs a pixel
  with radius mix(0, 2, saturate(8 × roughness)) while its variance exceeds 0.001
  (`SSS_BILATERAL_VARIANCE_ESTIMATE_THRESHOLD`) and runs at all above 0.00005, truncated to
  i32(min(2σ, radius)), so 3×3 at perceptual roughness 1/16 or more. Variance decays by 0.9 a
  frame: about 65 frames.
- **Raised by.** R-1, R-2, R-6, R-7, R-10/R-11 failures, R-12, R-13.
- **Stays triggered while.** Any of those repeats at a pixel: the blur never turns off there,
  read as softness or shimmer rather than rejection.
- **Source.** DiligentFX faithful (`Output.Variance = 1.0f`). Code:
  `SSR_ComputeTemporalAccumulation.wgsl`, `SSR_ComputeBilateralCleanup.wgsl`
  (`ComputeBilateralCleanupPS`).
- **Guard.** `offscreen_candidates_cannot_discard_valid_reflection_history` (variance shows retained history).
- **Status.** Rule.

#### R-17 Crystal hit confidence

- **Rule.** A ray's confidence is 0 when its hit lies outside the viewport, within 2 pixels of
  its origin on both axes, on background, on a back face, or it ran out of lookups, left the
  viewport or reached the far plane (DFX-22, DFX-26, DFX-38, DFX-61), or its normal's squared
  length is more than 0.1 from 1 (DFX-52). Otherwise it is
  (1 − smoothstep(0, `DepthBufferThickness` 0.025, distance / view z))² times an edge vignette
  over 5 % of the screen on each side. Confidence premultiplies the ray's radiance and decides
  tile activity (R-7); the temporal pass sees a drop as a darker current sample. Per ray, per frame.
- **Raised by.** Hits crossing the vignette band as the camera moves; thin geometry under jitter.
- **Stays triggered while.** Hits that sit in the vignette or thickness band fluctuate under
  jitter, and the box clamp follows them.
- **Source.** AMD SSSR `ValidateHit` through DiligentFX, faithful, plus DFX-18, 22, 26, 38, 41,
  52, 58, 61. Code: `SSR_ComputeIntersection.wgsl` (`ValidateHit`, `ComputeIntersectionPS`).
- **Guard.** `rays_that_run_out_of_lookups_miss`, `zero_normals_report_a_miss`,
  `half_resolution_keeps_one_pixel_mirror_strips_beside_rough_surfaces`.
- **Status.** Rule.

#### R-18 Screen-space traces read this frame's radiance

- **Rule.** Neither screen-space method reads the previous frame's radiance: Crystal never sets
  `FeatureFlags::PREVIOUS_FRAME`, and Velvet's trace reprojection is the identity. Rays read the
  incident radiance source completion wrote this frame.
- **Raised by.** Nothing: always in effect.
- **Stays triggered while.** Nothing: it withholds no history.
- **Source.** SGL3D's own. Code: `sgl-3d/src/view/post_fx.rs` (`screen_space_reflections`),
  `sgl-3d/src/stages/reflections/velvet.rs` (`Velvet::encode`).
- **Guard.** None.
- **Status.** Rule.

### Shared temporal reprojection

#### R-19 Invalid depth motion and the previous near plane

- **Rule.** When the G-buffer motion's w is at most 0 (previous clip w ≤ 0, or a depth change
  beyond 65504 m) or the receiver's previous view depth, its depth now plus the depth motion,
  is at most the previous near plane, `temporal_accumulate` returns this frame's value with no
  history. Per pixel, one frame.
- **Raised by.** Surfaces behind the previous camera.
- **Stays triggered while.** Nothing in ordinary motion.
- **Source.** SGL3D's own (NRD's `viewZprev = viewZ + motion.z`, as ambient occlusion takes it).
  Code: `sgl-3d/src/stages/reflections/temporal_reprojection.wgsl` (`temporal_accumulate`).
- **Guard.** `hits_behind_the_previous_camera_take_no_history`,
  `a_receiver_the_camera_follows_keeps_its_history` (`sgl-3d/src/stages/reflections/velvet/temporal_tests.rs`).
- **Status.** Rule.

#### R-20 In-bounds candidates and the dual reprojection

- **Rule.** `temporal_accumulate` reprojects by the motion vector and by the reflection's virtual
  point through the previous camera alone (`temporal_reprojection_uv`, w ≤ 0 off screen). When
  both lie off screen it keeps no history; otherwise the in-bounds candidate whose luminance is
  nearer the 3×3 mean wins, and history is mixed only if the position after the vicinity search
  lies on screen. This selection uses its own depth and signal checks (compare Crystal, R-9). Per pixel.
- **Raised by.** Screen-edge disocclusion; hits behind the previous camera.
- **Stays triggered while.** Nothing beyond the edge band a moving camera uncovers.
- **Source.** Wicked `ssr_temporalCS`, faithful selection, modified: in-bounds first and the
  w guard. Code: `temporal_reprojection.wgsl` (`temporal_accumulate`, `temporal_reprojection_uv`).
- **Guard.** `an_offscreen_candidate_cannot_discard_valid_history` (`velvet/temporal_tests.rs`).
- **Status.** Rule.

#### R-21 Depth disocclusion by depth motion

- **Rule.** History is kept when exp(−|lin(history) − previous_depth| / previous_depth) > 0.9,
  a relative difference under 10.54 % of the receiver's previous view depth (its depth now plus
  its depth motion), against the stage's own depth history at its traced size, point-loaded and
  linearised with the near plane it was written under. Below that the pass searches the 3×3
  texels for a strictly better weight from the centre, then a four-tap bilinear fallback with
  averaged depth; still below 0.9, no history. Per pixel, one frame.
- **Raised by.** Real disocclusion and silhouettes. Static surfaces the camera approaches and
  surfaces the camera follows both pass.
- **Stays triggered while.** Nothing.
- **Source.** Wicked `GetDisocclusion` and `SamplePreviousColor`, faithful constants and search;
  modified to compare depth motion and to clamp edge reads. Code: `temporal_reprojection.wgsl`
  (`temporal_disocclusion`, `temporal_previous_color`).
- **Guard.** `a_camera_dolly_keeps_a_stationary_planes_history`,
  `a_receiver_the_camera_follows_keeps_its_history`, `the_left_edge_fallback_reads_the_edge_texel`,
  `a_changed_near_plane_keeps_history` (`velvet/temporal_tests.rs`).
- **Status.** Rule.

#### R-22 Shared box clamp and fixed response

- **Rule.** Every accepted history is clamped to the 3×3 mean ± 2σ (`TEMPORAL_SCALE`), all
  four channels, and mixed at `TEMPORAL_RESPONSE` 0.95, with no sample-count ramp after a
  reset; callers clamp the result at 0. Continuous, time constant 20 frames.
- **Raised by.** Every frame.
- **Stays triggered while.** It always applies; it attenuates, it does not reject.
- **Source.** Wicked `ssr_temporalCS`, faithful. Code: `temporal_reprojection.wgsl`
  (`temporal_accumulate`).
- **Guard.** `jittered_hits_settle_and_a_reset_passes_through` (`velvet/temporal_tests.rs`).
- **Status.** Rule.

### Velvet

#### R-23 Half resolution takes depth and motion from one pixel

- **Rule.** At half resolution Velvet's downsample keeps, for each 2×2 block (3×3 at an odd
  edge), the full-resolution pixel with the nearest depth, and copies that pixel's depth,
  normal-roughness and motion (screen motion, depth motion and its validity) to the traced size;
  the temporal pass reads all three there. Where a block straddles surfaces with different
  motion, R-21 tests the nearer surface by its own depth motion and reads history by its own
  motion vector. Per block.
- **Raised by.** Nothing beyond R-21: a silhouette is judged as the nearer surface's interior is.
- **Stays triggered while.** Nothing.
- **Source.** Godot's downsample (`screen_space_reflection_downsample.glsl`), which picks the
  pixel, modified to carry SGL3D's motion with it; world rays likewise read one pixel's depth and
  motion (R-28). Code: `sgl-3d/src/stages/reflections/velvet/godot_reflections_downsample.wgsl`,
  `godot_reflections_temporal.wgsl`, `velvet.rs` (`Velvet::encode`), `velvet/targets.rs`
  (`motion_half`).
- **Guard.** `a_straddling_block_takes_depth_and_motion_from_one_pixel`
  (`sgl-3d/src/stages/reflections/velvet/history_tests.rs`: a craft the camera follows
  over static background the camera approaches, with the silhouette inside a block, keeps history
  at every texel; with motion from the block's last pixel the straddling column, 32 texels, kept
  none).
- **Status.** Rule.

#### R-24 Velvet restarts

- **Rule.** When Velvet's targets are reallocated (render size or half resolution changed),
  when `HistoryFrame::valid` is false, when the stage did not run in the view's last finished
  frame, or after the stage was dropped (method not Velvet, SSR off), its count is 0 and the
  temporal pass writes this frame's trace to output and history. Whole stage; accumulation
  resumes the next frame at 0.95. A frame rendered again reads and writes the same halves.
- **Raised by.** The view's reset, frames without screen-space reflections, setting changes,
  resizes.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own, the DiligentFX TAA continuity rule. Code:
  `sgl-3d/src/stages/reflections/velvet.rs` (`Velvet::encode`), `sgl-3d/src/view/history.rs`
  (`StageHistory`),
  `sgl-3d/src/stages/reflections/mod.rs` (`Reflections::resolve`), `godot_reflections_temporal.wgsl`.
- **Guard.** `jittered_hits_settle_and_a_reset_passes_through`; `resize_restarts_at_the_new_size`,
  `content_edits_keep_history` (`tests/sgl-3d/tests/screen_space_reflections.rs`, both methods);
  `velvet_keeps_its_history_across_an_unfinished_frame`.
- **Status.** Rule.

#### R-25 Untraced pixels pass through

- **Rule.** Where the traced-size depth is 0 (sky) or perceptual roughness is at least
  `ROUGHNESS_CUTOFF` (0.7), Velvet's temporal pass writes this frame's value (0) to output and
  history. A pixel that becomes traceable reprojects into that zero; depth is stored for every
  pixel, so the depth test can pass and the zero is accepted, clamped up to mean − 2σ. Per pixel;
  time constant 20 frames.
- **Raised by.** Roughness textures or animation near 0.7; sky silhouettes.
- **Stays triggered while.** Textured roughness straddling 0.7 under jitter alternates pixels
  between pass-through and accumulation. Composition fades Velvet out over 0.6–0.7 (Godot's
  forward-pass fade), so they show little of it; measured through SGL3D's composite under TAA
  on roughness textures alternating or varying across 0.7, keeping their history across the
  exit instead flickered as much or more (mean second difference 0.0179 and 0.0271, 0.0152 and
  0.0156, 0.0154 and 0.0159, passed through and kept). R-6 has Crystal's measurement.
- **Source.** Wicked's pass-through where no reflection is needed; the fade is Godot's
  (`scene_forward_clustered.glsl`). Code: `godot_reflections_temporal.wgsl`,
  `godot_reflections_cutoff.wgsl`, `godot_reflections_trace.wgsl`,
  `sgl-3d/src/shading/specular_lobes.wgsl` (`specular_trace_fade`).
- **Guard.** `pixels_returning_to_the_traced_set_keep_no_old_radiance`
  (`sgl-3d/src/stages/reflections/velvet/history_tests.rs`: two runs that differ only in the
  history before half the plane leaves the traced set for a frame give equal output where it
  returns; with the old history kept, all 480 checked pixels differed);
  `reflections_are_continuous_across_the_roughness_cutoff` (the fade).
- **Status.** Rule.

#### R-26 Velvet hit validity

- **Rule.** A ray's validity is 0 when it did not descend below the finest level, reached the
  screen edge (`t >= t_max`), hit depth 0, or, within 2 pixels of its origin, met a normal facing
  along it; otherwise (1 − smoothstep(0, `DEPTH_TOLERANCE` 0.5 m, delta))² times a margin blend
  over 5 % of (width + height) and the ray-length fade (`FADE_IN` 0.15, `FADE_OUT` 2.0) within
  `MAX_STEPS` 64. A miss's reprojection depth is the receiver's own. Per ray, per frame.
- **Raised by.** Rays near the screen margin, the step budget or thin geometry.
- **Stays triggered while.** Rays in the margin or tolerance band fluctuate under jitter; the box
  clamp follows them.
- **Source.** Godot `screen_space_reflection.glsl`, faithful, except that an unfinished ray is a
  miss (as DFX-38). Code: `godot_reflections_trace.wgsl` (`main`), `velvet.rs` (constants).
- **Guard.** `tests/sgl-3d/tests/godot_traversal_budget.rs`.
- **Status.** Rule.

### World-space rays

#### R-27 World-ray restarts, and settings that do not restart them

- **Rule.** When the render size changes, `HistoryFrame::valid` is false, the stage did not run
  in the view's last finished frame (including frames in which the screen-space method was off,
  which skip the stage), or `Settings::world_space_reflections` was Off, the stage's count is 0: its
  temporal pass stores this frame's resolve and its current variance, and its jitter and hashes
  restart. A `Moving` <-> `All` change, the hardware or portable path, and a Crystal <-> Velvet
  switch restart nothing: earlier accumulation blends into the new configuration over about
  20 frames. A frame rendered again reads and writes the same halves.
- **Raised by.** The view's reset, resizes, toggling SSR or world rays.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own; Wicked writes only colour on its first frame. Code:
  `sgl-3d/src/stages/reflections/world.rs` (`WorldReflections::encode`),
  `world/world_reflections_denoise.wgsl` (`world_temporal`).
- **Guard.** `world_history_restarts_after_discontinuities`,
  `world_frames_after_a_resize_equal_a_new_effect`, `consecutive_world_frames_keep_accumulation`,
  `world_reflections_keep_their_history_across_an_unfinished_frame`
  (`tests/sgl-3d/tests/world_reflections.rs`).
- **Status.** Rule.

#### R-28 Texels without rays and under blended receivers

- **Rule.** Texels are judged at their block's first full-resolution pixel, whose depth and
  motion the temporal pass also reads. Where that pixel's lobe is not traced (depth 0, unlit, or
  roughness at or above the method's cutoff) or a blended receiver covers it, the temporal pass
  writes this frame's value with variance 0 and keeps no history; a covered texel stores depth 0,
  so the surface it uncovers fails disocclusion once and starts from its current rays.
- **Raised by.** Blended receivers moving over glossy opaque surfaces; roughness crossing the cutoff.
- **Stays triggered while.** A receiver moves: its edge leaves a one-frame no-history trail every frame.
- **Source.** SGL3D's own after Wicked's pass-through where no reflection is needed. Code:
  `world_reflections_denoise.wgsl` (`world_resolve`, `world_temporal`),
  `world_reflections_common.wgsl` (`world_receiver`, `world_under_receiver`).
- **Guard.** `texels_a_receiver_uncovers_take_their_current_rays_whole` (`sgl-3d/src/stages/reflections/world/tests.rs`).
- **Status.** Rule.

#### R-29 World variance reset holds the upsample blur on

- **Rule.** When R-21 rejects or the history position lies off screen, the world temporal pass
  writes variance 1 with no history response; otherwise it mixes variance at 0.9. The upsample
  blurs with radius up to 2 where variance exceeds 0.015 and runs at all above 0.005. From 1 the
  variance falls below 0.015 in about 40 frames.
- **Raised by.** Any rejection in the shared reprojection.
- **Stays triggered while.** A rejection repeats at a texel (edges of moving objects, R-28
  trails): the blur stays on there.
- **Source.** Wicked `ssr_temporalCS` (`varianceTemporalResponse` 0.9, white variance on
  disocclusion), faithful. Code: `world_reflections_denoise.wgsl` (`world_temporal`, `world_upsample`).
- **Guard.** None.
- **Status.** Rule.

#### R-30 Traced texels without a listed ray accumulate what their resolve borrows

- **Rule.** The classification lists a ray only where the screen-space result leaves the
  fallback a share above 0.001 and no receiver covers the pixel; every texel starts as a miss.
  The resolve gives neighbours without a ray no weight, so a texel without its own ray takes the
  rays of listed neighbours within its reconstruction reach, and writes 1e-5 where none has one;
  the temporal pass accumulates either as a valid sample wherever the lobe is traced. When the
  screen-space method then lets go of the texel, world rays start over that history and fill in
  over up to about 45 frames.
- **Raised by.** Screen-space confidence dropping: reflections leaving the screen, the vignette
  band, thin occluders.
- **Stays triggered while.** Nothing: each texel fills in once. Measured in SGL3D's renderer
  (a floor at roughness 0.05 reflecting a bright panel, world rays `All`, 240×160): with the
  screen-space method confident, 6 of 19808 lit pixels resolved no world ray at all, so the
  1e-5 history is rare. After a tilt that put the panel's top off screen while its reflection
  stayed, the composite over the bright region was off its converged value by 0.312 on average
  the first frame, 0.118 by the twelfth and 0.020 by the forty-fifth. Keeping no history where
  the texel's own ray was not listed, so the first rays stand whole, changed that to 0.283,
  0.133 and 0.022; listing a ray at every traced texel, so history was warm, to 0.117, 0.062
  and 0.018. The fill-in is the cost of the rays the classification saves where the
  screen-space method is confident, as a disocclusion's is.
- **Source.** SGL3D's own interaction; AMD SSSR's classification writes environment radiance for
  pixels without a ray, which is what premultiplied 0 composes to here. Code:
  `world_reflections_classify.wgsl` (`world_needs_ray`), `world_reflections_denoise.wgsl`
  (`world_resolve`, `world_temporal`).
- **Guard.** `confident_receivers_in_the_roughness_fade_still_trace_world_rays`
  (`sgl-3d/src/stages/reflections/world/tests.rs`: the gate lists rays wherever the screen-space
  method leaves a share).
- **Status.** Rule.

### Receivers

#### R-31 Receivers replace the surface's depth and motion

- **Rule.** When receivers are in effect and the blended list holds one, the receiver pass
  writes, where a receiver is nearer than the opaque surface, the surface depth and the
  receiver's full velocity (screen motion, depth motion and validity) over the G-buffer's
  motion. Crystal's and Velvet's histories there follow the receiver; the opaque surface's
  reflection history beneath is not kept (world rays: R-28). A receiver appearing or leaving
  changes the surface depth, and the depth tests reject there for one frame.
- **Raised by.** Blended receivers in view and their motion.
- **Stays triggered while.** Nothing beyond the edges a moving receiver uncovers.
- **Source.** SGL3D's own, after HDRP's transparent depth prepass and motion vectors. Code:
  `sgl-3d/src/stages/transparent/mod.rs` (`Transparent::encode_receivers`),
  `sgl-3d/src/shading/geometry.wgsl` (`receiver_fs`).
- **Guard.** `a_receiver_writes_its_motion_and_the_surface_depth_not_the_opaque_depth`
  (`sgl-3d/src/stages/transparent/receiver_tests.rs`).
- **Status.** Rule.

### Stages without history

- Source completion and probe culling (`stages/reflections/source.*`, `probe_culling.*`): rebuilt every frame.
- Velvet's trace, mip chain and resolve: per frame; only its temporal pass keeps history.
- The receiver pass: the surface is rebuilt in every frame it runs.

## Shadows

The ray-traced shadow stage keeps a screen-space history: a temporal mask pair for every slot,
and for slots 0 to 3 (slot 0 alone at Low) the AMD denoiser's moments and filter history. The
local-light atlas keeps a cache of shadow faces, which it redraws exactly when what a face shows
changes, so its rules cost time rather than history. Shadow-map filtering keeps no history but
relies on the antialiasing to resolve its per-frame rotation. In a continuously running game,
the rules tripped most are the slot restarts of local lights at the frustum's edge (S-6), which
discard nothing a returning light could use, and the velocity refresh on streaming scenery (S-13).

### Ray-traced shadows: stage and slot restarts

#### S-1 Renderer reset restarts the stage

- **Rule.** When the frame's history is not valid (the view's one reset: `camera_cut`, a
  resize that changed the targets, another `Scene`), the ray-traced shadow stage sets its frame
  count to 0, so every slot takes its current trace in the temporal blend and every denoised lane
  starts with zero moments and its current value as history, over the whole stage. The previous
  frame is forgotten even in a frame the stage does not run. Accumulation resumes the next frame;
  the denoiser's history weight reaches its 0.05 floor after 8 frames (S-22).
- **Raised by.** Camera cuts, resizes, scene switches.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own wiring of Wicked Engine's first-frame clear.
  Code: `src/stages/shadows/traced.rs` (`TracedShadows::encode`).
- **Guard.** `a_slot_whose_light_changed_and_a_camera_cut_restart_its_history`
  (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule.

#### S-2 A frame the stage did not run restarts it

- **Rule.** When the frame's history count is not one more than that of the last frame the stage
  ran, the stage restarts as in S-1. The stage does not run, and so breaks the sequence, in a frame
  without hardware rays (`ctx.hardware_rays` absent, including a frame whose acceleration
  structures fail to prepare), with `Settings::ray_traced_shadows` off, or whose slots hold no
  light (`renderer::effective::shadow_stages`: no shadowed directional light and no placed casting
  local light).
- **Raised by.** The only casting light's shadow opacity crossing 0.001, toggling the setting,
  acceleration-structure preparation failing.
- **Stays triggered while.** Nothing in steady play; a caster whose shadow opacity flickers about
  0.001, or intermittent preparation failures, restart the stage each time.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced.rs` (`TracedShadows::encode`),
  `src/renderer/effective.rs` (`shadow_stages`).
- **Guard.** None.
- **Status.** Rule.

#### S-3 Render-size change restarts the stage

- **Rule.** When the render size differs from the size the stage's targets were made at, the stage
  makes new targets, forgets its bind groups and restarts as in S-1.
- **Raised by.** Resize, FSR quality or scene-resolution changes (which also raise the view's reset).
- **Stays triggered while.** Nothing: it needs a size change.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced.rs` (`TracedShadows::encode`).
- **Guard.** None.
- **Status.** Rule.

#### S-4 Quality change restarts the stage

- **Rule.** When `Settings::ray_traced_shadow_quality` resolves differently from the frame before,
  the stage restarts as in S-1, since the quality decides which slots the denoiser keeps a history for.
- **Raised by.** A settings change.
- **Stays triggered while.** Nothing: it needs a setting change.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced.rs` (`TracedShadows::encode`).
- **Guard.** None (`the_quality_decides_which_slots_the_denoiser_filters` in
  `src/stages/shadows/traced/tests.rs` runs both qualities without asserting the restart).
- **Status.** Rule.

#### S-5 Directional slot restart

- **Rule.** When the shadowed directional light's index in `FrameInput::directional_lights` differs
  from the last frame's, or a shadowed directional light returns after a frame without one (its
  `shadow_opacity` at or below 0.001 counts as none), slot 0 sets its restart bit and takes its
  current trace and a denoiser first frame. Recovery as S-1 for that slot.
- **Raised by.** The game reordering or replacing its directional lights; a shadow fading through 0.001.
- **Stays triggered while.** Nothing, unless the game rebuilds its directional light list in a
  varying order every frame.
- **Source.** SGL3D's own; Wicked's slot is a light's index in its sorted entity array.
  Code: `src/stages/shadows/traced/slots.rs` (`Slots::assign`, `SlotLights::of`).
- **Guard.** `a_light_keeps_its_slot_while_placed_and_a_freed_slot_goes_to_the_best_ranked`
  (`src/stages/shadows/traced/slots.rs`).
- **Status.** Rule.

#### S-6 A local light that leaves the ranking frees its slot and restarts on return

- **Rule.** When a local light held in slots 1 to 15 is absent from the local atlas's ranking for a
  frame, it frees its slot at once; a light without a slot takes the lowest free slot, sets that
  slot's restart bit and starts with its current trace (and a denoiser first frame in slots 1 to 3).
  A light returning after a frame away is such a light: it takes its own slot again where no lower
  slot is free. A held light leaves the ranking only when its range sphere no longer reaches the
  camera's frustum, or by a change of the game's: it stops casting or is turned off, its range falls
  to the local shadow near plane or its shadow opacity to 0.001. Room does not take it out: the atlas
  keeps a placed light's placement when it finds no other (S-33). The restart on return discards
  nothing the light could use. A light whose range does not reach the view lights no pixel on
  screen, so every pixel it lights on its return was off screen or hidden the frame before, and its
  slot's history there, whether the slot was left empty or kept tracing it while it was away, is
  another surface's or the screen edge's; after a change of the game's, it records other lighting.
  Recovery as S-1 for that slot.
- **Raised by.** The camera turning so a light's range sphere grazes the frustum edge; a fading
  shadow; the game toggling a light's casting.
- **Stays triggered while.** A swaying chase camera keeps a light at the frustum edge leaving and
  re-entering: each re-entry restarts its slot, which costs the pixels it lights again nothing they
  would not lose as disocclusions. It re-enters the lowest free slot, so where another light took
  its slot meanwhile it may move between the denoised slots and the blended ones.
- **Source.** SGL3D's own. Godot's shadow atlas (b130438 `light_storage.cpp`,
  `_shadow_atlas_find_shadow`) keeps an absent light's slot until another light needs it, which
  serves its maps, whose contents stay valid while the light is away; a screen-space history does
  not. Measured by the guard below: with the slot kept for the light, traced or empty while it was
  away, and taken back without a restart, the slot it returned to differed from a camera cut's at
  5552 of 16384 pixels; with the restart, at none. Code: `src/stages/shadows/traced/slots.rs`
  (`Slots::assign`), `src/stages/shadows/local/plan.rs` (`Plan::prepare`, `candidates`),
  `src/stages/shadows/local/atlas.rs` (`Atlas::update`).
- **Guard.** `a_light_absent_for_a_frame_returns_to_the_lowest_free_slot_and_restarts_it`,
  `a_light_keeps_its_slot_while_placed_and_a_freed_slot_goes_to_the_best_ranked`,
  `more_lights_than_slots_fill_them_in_rank_order` (`src/stages/shadows/traced/slots.rs`);
  `a_light_that_leaves_the_view_restarts_its_slot_when_it_returns`
  (`src/stages/shadows/traced/tests/history.rs`);
  `a_denoised_slot_keeps_its_light_s_history_and_restarts_alone` (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule. Settled from B-11 (2026-11-18): the restart it described loses no usable history.

#### S-7 Baked-flag flip restarts a slot

- **Rule.** When a held light's `Light::baked` changes, its slot sets its restart bit, since its
  history then records another set of receivers' rays. Recovery as S-1 for that slot.
- **Raised by.** The game toggling `Light::baked`.
- **Stays triggered while.** Nothing: it lasts one frame.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced/slots.rs` (`Slots::assign`).
- **Guard.** `each_held_slot_says_whether_its_light_is_baked_this_frame` (`src/stages/shadows/traced/slots.rs`).
- **Status.** Rule.

#### S-8 Denoiser shape change keeps slot 0

- **Rule.** At High, the denoiser switches between four lanes (a light in slots 1 to 3) and slot 0
  alone; every shape shares its targets, so slot 0 continues and a light joining slots 1 to 3 is a
  newcomer with its restart bit (S-6). When no slot the shape filters holds a light, the denoiser
  does not run; the next light to take such a slot carries a restart bit, so its stale moments are
  never read.
- **Raised by.** Local lights joining or leaving slots 1 to 3.
- **Stays triggered while.** Nothing beyond S-6.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced/denoise.rs` (`Shape::of`, `Shape::runs`).
- **Guard.** `the_directional_light_keeps_its_history_across_the_denoiser_s_shapes`
  (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule.

### Ray-traced shadows: temporal blend (slots 4 to 15; slots 1 to 3 at Low)

#### S-9 A texel without a receiver takes its trace

- **Rule.** When a tracing texel's half-resolution linear depth is the sky's (at least 1e30: sky,
  or a pixel the G-buffer drew nothing lit at), the temporal blend writes its current words
  unblended over that texel.
- **Raised by.** Sky and unlit materials.
- **Stays triggered while.** Always there; no lighting reads those texels, so nothing shows.
- **Source.** Wicked Engine `rtshadow_denoise_temporalCS.hlsl` returns early at the sky; writing
  the current value is SGL3D's port. Code: `src/stages/shadows/traced/traced_temporal.wgsl`
  (`traced_shadow_temporal`).
- **Guard.** `thin_lit_features_against_the_sky_are_unoccluded` (`src/stages/shadows/traced/tests.rs`), indirectly.
- **Status.** Rule.

#### S-10 Off-screen reprojection takes the trace

- **Rule.** When the texel's pixel reprojected by its motion lies outside the render size, the
  temporal blend writes its current words unblended over that texel. It blends again the first
  frame its history is on screen.
- **Raised by.** Camera motion bringing new content in at the frame's edges.
- **Stays triggered while.** Nothing: only edge pixels, for one frame each.
- **Source.** Wicked Engine `rtshadow_denoise_temporalCS.hlsl` (`is_saturated(prevUV)`), faithful.
  Code: `src/stages/shadows/traced/traced_temporal.wgsl` (`traced_shadow_temporal`).
- **Guard.** None.
- **Status.** Rule.

#### S-11 The temporal blend's expected-depth test

- **Rule.** The temporal blend compares the stage's own last-frame linear depth at the reprojected
  texel with the linear depth the texel's surface is expected to have had from the last frame's
  camera: its linear depth now plus the G-buffer's depth motion (M-2), or, under a projection whose
  clip w is no view depth (orthographic, whose depth motion is 0), the camera's reprojection of its
  point; none where the depth motion is invalid. Where it has none, or the two differ by at least
  `mix(0.01, 0.1, (1 − dot(view, N))⁸)` of the expected depth (about 1% for a surface facing the
  camera, up to 10% edge on), it discards every non-denoised slot's history over that texel and
  writes the current trace. Accumulation resumes the first frame the test passes.
- **Raised by.** Disocclusions: a surface revealed behind one more than about 1% of its depth nearer;
  the background at an occluder's edge, where the nearest texel holds the occluder; a surface on or
  behind the previous camera. Under an orthographic camera, a surface moving along the view axis.
- **Stays triggered while.** Nothing in steady play: still scenery under any camera travel and a
  surface the camera follows keep their history. Depth that changes across a tracing pixel faster
  than the tolerance (a surface nearly edge on, beyond what the normal term allows) can reject at
  sub-texel reprojection error.
- **Source.** Modified from Wicked Engine `rtshadow_denoise_temporalCS.hlsl`
  (`abs(depth_current - depth_history) > 1`, a metre of this frame's depth, which rejected still
  scenery while the camera travelled more than a metre a frame along its view and let surfaces under
  a metre apart share history): the test and tolerance are AMD FidelityFX Denoiser's
  `FFX_DNSR_Shadows_IsDisoccluded`, which the denoiser applies to slots 0 to 3 (S-18), on the
  expected depth of NRD's 2.5D motion (`viewZprev = viewZ + motion.z`), as ambient occlusion and the
  reflections take it. Code: `src/stages/shadows/traced/traced_temporal.wgsl`
  (`traced_shadow_temporal`, `temporal_disoccluded`), `src/stages/shadows/traced/traced_common.wgsl`
  (`traced_previous_depth`).
- **Guard.** `still_scenery_keeps_its_shadow_history_as_the_camera_travels_toward_it`,
  `a_surface_the_camera_follows_keeps_its_shadow_history`,
  `a_surface_revealed_behind_a_near_one_takes_no_history`
  (`src/stages/shadows/traced/tests/history.rs`).
- **Status.** Rule.

#### S-12 Receivers' 3×3 variance box

- **Rule.** When history is taken, the temporal blend clamps each slot's history to
  `[min(mean − 2σ, current), max(mean + 2σ, current)]`, the mean and deviation over the 3×3 texels
  that hold a receiver (`TEMPORAL_SCALE` 2), per texel and slot.
- **Raised by.** Moving shadows and the per-frame ray noise (S-26).
- **Stays triggered while.** Always active: in a penumbra the box is built from single noisy rays,
  so it moves the history every frame; that is the intended trade.
- **Source.** Modified from Wicked Engine, whose `ResolverAABB` computes the box and leaves it
  unused; SGL3D applies it as Wicked's SSR and RT diffuse temporal passes do, over receivers alone.
  Code: `src/stages/shadows/traced/traced_temporal.wgsl` (`temporal_blend`).
- **Guard.** `a_thin_feature_s_history_is_clamped_by_receivers_alone` (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule.

#### S-13 Change response and velocity refresh

- **Rule.** The history's share is `mix(mix(0.88, 1, (1 − d)²), 0.2, saturate(v / 100))`, where
  `d = |current − history| / max(current, history, 0.2)` and `v` is the surface's motion in tracing
  pixels a frame, per texel and slot.
- **Raised by.** Screen-space motion of the surface, camera or object.
- **Stays triggered while.** Scenery streams past a moving camera: a surface moving 30 tracing
  pixels a frame keeps its history share pulled 30% of the way to 0.2, so local lights' ray-traced
  shadows on passing scenery stay noisier than at rest; a followed vehicle keeps the full response.
- **Source.** Wicked Engine `rtshadow_denoise_temporalCS.hlsl` (Unreal's velocity refresh), faithful.
  Code: `src/stages/shadows/traced/traced_temporal.wgsl` (`temporal_blend`).
- **Guard.** None.
- **Status.** Rule.

#### S-14 Restart bit and frame 0 in the blend

- **Rule.** A slot whose restart bit is set (S-5 to S-7), and every slot in the stage's frame 0
  (S-1 to S-4), takes its current trace in the temporal blend.
- **Raised by.** See S-1 to S-7.
- **Stays triggered while.** Nothing: one frame.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced/traced_temporal.wgsl` (`temporal_blend`).
- **Guard.** `a_slot_whose_light_changed_and_a_camera_cut_restart_its_history`,
  `a_denoised_slot_keeps_its_light_s_history_and_restarts_alone` (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule.

#### S-15 Empty words and denoised slots bypass the blend

- **Rule.** A word whose four slots hold no light keeps its traced zeros; the denoised slots (four
  at High, one at Low, `TracedParams.denoised`) take the denoiser's result and never this blend,
  so their history is S-16 to S-24's.
- **Raised by.** The slot table and quality.
- **Stays triggered while.** Not applicable: a routing rule.
- **Source.** Wicked Engine (its first four lights denoised), faithful; skipping empty words is SGL3D's own.
  Code: `src/stages/shadows/traced/traced_temporal.wgsl` (`temporal_word0`, `temporal_blend`).
- **Guard.** `the_quality_decides_which_slots_the_denoiser_filters` (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule.

### Ray-traced shadows: AMD denoiser (slots 0 to 3 at High; slot 0 at Low)

#### S-16 A lane's first frame

- **Rule.** When the stage's frame count is 0 or the lane's slot has its restart bit, the tile
  classification reads that lane's previous moments as zero and takes its current value as its
  reprojected history. Recovery as S-22.
- **Raised by.** S-1 to S-7.
- **Stays triggered while.** Nothing: one frame.
- **Source.** Modified: Wicked Engine clears its resources on its first frame; SGL3D restarts per lane.
  Code: `src/stages/shadows/traced/traced_denoise_tileclassification.wgsl`
  (`FFX_DNSR_Shadows_IsFirstFrame`, `FFX_DNSR_Shadows_ReadPreviousMomentsBuffer`).
- **Guard.** `a_slot_whose_light_changed_and_a_camera_cut_restart_its_history`,
  `each_denoised_slot_keeps_its_own_history` (`src/stages/shadows/traced/tests.rs`).
- **Status.** Rule.

#### S-17 Off-screen reprojection is a disocclusion

- **Rule.** When the reprojected UV is not strictly inside (0, 1), the tile classification discards
  the pixel's previous moments (its sample count restarts at 1); the reprojected value is still read
  at the clamped edge and clamped by S-20.
- **Raised by.** Camera motion at the frame's edges.
- **Stays triggered while.** Nothing: edge pixels, one frame each.
- **Source.** AMD FidelityFX Denoiser `ffx_denoiser_shadows_tileclassification.h`
  (`FFX_DNSR_Shadows_IsDisoccluded`), faithful. Code:
  `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl` (`FFX_DNSR_Shadows_IsDisoccluded`).
- **Guard.** None.
- **Status.** Rule.

#### S-18 The denoiser's expected-depth test

- **Rule.** The tile classification compares the stage's stored last-frame linear depth at the
  reprojected pixel with the depth the pixel's surface is expected to have had from the last
  frame's camera, as S-11 takes it (its linear depth plus the G-buffer's depth motion; the camera's
  reprojection of its point under an orthographic projection; none where the depth motion is
  invalid). Where it has none, or `|stored − expected| / expected` is at least
  `mix(0.01, 0.1, (1 − dot(view, N))⁸)`, it discards the previous moments over that tracing pixel,
  for every lane: the sample count becomes 1, the variance is boosted fifteenfold, and the blend
  takes about 94% of the current single ray. Accumulation resumes when the test passes, reaching its
  0.05 floor 8 frames later.
- **Raised by.** Disocclusions, as S-11; the background at a moving occluder's edge (S-19).
- **Stays triggered while.** Nothing in steady play: a vehicle the camera follows and still scenery
  under camera travel keep their moments.
- **Source.** AMD FidelityFX Denoiser `ffx_denoiser_shadows_tileclassification.h`
  (`FFX_DNSR_Shadows_IsDisoccluded`), modified: upstream, and Wicked Engine's
  `rtshadow_denoise_tileclassificationCS.hlsl` through the camera reprojection matrix it supplies,
  reprojects the pixel's current point through the camera, which holds for a still surface alone and
  rejected the moments of a surface the camera follows in every frame it travelled along its view by
  more than about 1% of the surface's depth (a vehicle 5 m ahead at 3 m/s at 60 fps); SGL3D takes the
  surface's own expected depth, NRD's 2.5D motion, and a surface with none is disoccluded. Code:
  `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl`
  (`FFX_DNSR_Shadows_IsDisoccluded`), `src/stages/shadows/traced/traced_denoise_tileclassification.wgsl`
  (`FFX_DNSR_Shadows_GetPreviousLinearDepth`), `src/stages/shadows/traced/traced_common.wgsl`
  (`traced_previous_depth`).
- **Guard.** `a_surface_the_camera_follows_keeps_its_shadow_history`,
  `still_scenery_keeps_its_shadow_history_as_the_camera_travels_toward_it`
  (`src/stages/shadows/traced/tests/history.rs`).
- **Status.** Rule.

#### S-19 Velocity of the nearest depth in the quad

- **Rule.** The tile classification reprojects each pixel by the motion of the nearest-depth pixel
  of its 2×2 quad, so at an occluder's edge the background takes the occluder's motion, reads
  another surface's history and usually fails S-18.
- **Raised by.** Edges of moving occluders.
- **Stays triggered while.** Nothing beyond the edge pixels of moving objects.
- **Source.** AMD FidelityFX Denoiser (`FFX_DNSR_Shadows_GetClosestVelocity`), faithful; quad reads
  through workgroup memory. Code: `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl`
  (`FFX_DNSR_Shadows_GetClosestVelocity`).
- **Guard.** None.
- **Status.** Rule.

#### S-20 History clamped to ±0.5σ of the local neighbourhood

- **Rule.** For every receiver, the tile classification clamps the reprojected history to the
  mean ± 0.5 deviations of the 17×17 Gaussian neighbourhood of this frame's hit bits
  (`KERNEL_RADIUS` 8), per pixel and lane.
- **Raised by.** Shadow motion and ray noise.
- **Stays triggered while.** Always active; in penumbrae the narrow box keeps the history moving
  with the noise, upstream's intended behaviour.
- **Source.** AMD FidelityFX Denoiser (`FFX_DNSR_Shadows_TileClassification`), faithful. Code:
  `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl` (`FFX_DNSR_Shadows_TileClassification`).
- **Guard.** `every_denoised_slot_matches_a_cpu_oracle_away_from_its_edges`
  (`src/stages/shadows/traced/tests.rs`), indirectly.
- **Status.** Rule.

#### S-21 Sample-count damper

- **Rule.** The tile classification multiplies both the sample count and accumulated squared deviation by `exp(−d² / 20)`, where
  `d = (history − mean) / max(0.5σ, 0.001)` over the same neighbourhood, per pixel and lane.
  Where the neighbourhood is uniform (σ near 0), any history deviation above about 0.003 drives the
  count to nearly 0. The count recovers by 1 a frame.
- **Raised by.** Shadow changes; small differences between a uniform region's history and mean.
- **Stays triggered while.** Mostly hidden: fully uniform tiles are skipped (S-23).
- **Source.** AMD FidelityFX Denoiser, modified to discount the squared-deviation sum with its sample weight. Code:
  `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl` (`FFX_DNSR_Shadows_TileClassification`).
- **Guard.** `changing_shadow_coverage_keeps_variance_bounded` (`src/stages/shadows/traced/tests/history.rs`).
- **Status.** Rule.

#### S-22 History weight and variance boost

- **Rule.** The tile classification blends towards the current value by
  `mix(0.05, 1, sqrt(max(8 − count, 0) / 8))` and boosts the variance by `max(16 − count, 1)` while
  the count is below 16 (variance 1 at count 1). A count below 8 follows the current value quickly;
  from 8 on, 5% of the current value enters each frame.
- **Raised by.** Every restart or rejection above lowers the count.
- **Stays triggered while.** Not a trigger: the ramp lasts 8 frames after each restart.
- **Source.** AMD FidelityFX Denoiser, faithful. Code:
  `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl` (`FFX_DNSR_Shadows_TileClassification`).
- **Guard.** None.
- **Status.** Rule.

#### S-23 Uniform tiles reset their moments

- **Rule.** When an 8×8 group's region of hit masks (three 8×4 tiles across, rows −2 to +3, about
  24×24 tracing pixels) is all lit or all shadowed for a lane, or the group holds no receiver, the
  tile classification writes the tile as cleared: value 0 or 1, variance 0, moments with count 1 for
  a receiver and 0 otherwise. The tile's history is discarded each frame it is uniform; a penumbra
  entering it starts from count 1 (S-22).
- **Raised by.** Fully lit or fully shadowed areas; moving shadows entering them.
- **Stays triggered while.** Always in uniform areas, invisibly; a moving penumbra edge entering a
  lit area is noisy for its first frames.
- **Source.** AMD FidelityFX Denoiser (`FFX_DNSR_Shadows_ClearTargets`), faithful; the per-lane vote
  is SGL3D's. Code: `src/stages/shadows/traced/ffx_denoiser_shadows_tileclassification.wgsl`
  (`FFX_DNSR_Shadows_TileClassification`, `FFX_DNSR_Shadows_ClearTargets`),
  `src/stages/shadows/traced/ffx_denoiser_shadows_filter.wgsl` (`FFX_DNSR_Shadows_FilterSoftShadowsPass`).
- **Guard.** `every_denoised_slot_matches_a_cpu_oracle_away_from_its_edges`
  (`src/stages/shadows/traced/tests.rs`), indirectly.
- **Status.** Rule.

#### S-24 Sample count kept in half precision

- **Rule.** The moments are stored in RGBA16F, so the sample count stops growing near 2048; the
  history weight already floors at count 8, so this changes nothing visible.
- **Raised by.** Long accumulation.
- **Stays triggered while.** Nothing visible.
- **Source.** Modified: Wicked Engine keeps R11G11B10. Code: `src/stages/shadows/traced/denoise.rs`
  (`Targets::new`).
- **Guard.** None.
- **Status.** Rule.

#### S-25 A frame not finished leaves the stage's history as its last finished frame left it

- **Rule.** A frame not finished leaves the stage's history as its last finished frame left it:
  the stage keeps its run (frame count and the half of each pair it writes), its slots' lights,
  the quality its history was made at and its seed as that frame left them
  (`Kept<TracedKept>`), so a frame rendered again assigns the same slots, draws with the same
  seed and reads and writes the same halves. `Slots::assign` works from the kept copy. A stage
  that did not run in the last finished frame keeps nothing and restarts as in S-1. Every GPU
  history the stage keeps is a pair indexed by its run: the temporal mask, the denoiser's moments
  and the denoiser's filter history, which its first filter pass writes and the next frame's tile
  classification reads (`denoise::Targets::history`, where Wicked keeps one texture,
  `rtshadow.scratch[i][1]`), so a frame whose commands were submitted and a frame whose encoder
  was dropped leave the frame rendered again the same history.
- **Raised by.** A game that renders a frame and abandons or re-renders it.
- **Stays triggered while.** Nothing.
- **Source.** SGL3D's own. Code: `src/stages/shadows/traced.rs` (`TracedShadows::encode`,
  `TracedShadows::settle`), `src/stages/shadows/traced/denoise.rs` (`Targets`,
  `Denoiser::encode`), `src/view/history.rs` (`Kept`).
- **Guard.** `the_shadow_mask_survives_an_unfinished_frame` (`src/stages/shadows/traced/tests.rs`,
  its encoder dropped and its commands submitted).
- **Status.** Rule.

#### S-26 Per-frame ray draw

- **Rule.** The trace draws a new point on each light every frame from a hash of the pixel and the
  stage's own seed, which advances every frame the stage runs. It drops no history; it is the noise
  every rule above must absorb, shown raw wherever S-11 or S-18 reject.
- **Raised by.** Every frame.
- **Stays triggered while.** Always.
- **Source.** Modified: Wicked Engine reads blue noise. Code: `src/stages/shadows/traced/traced_trace.wgsl` (`traced_cast`).
- **Guard.** None.
- **Status.** Rule.

### Local-light shadow atlas cache

These rules redraw a cached face exactly; they cost time, not history.

#### S-27 Whole cache stale

- **Rule.** When the scene differs, a finished frame's static edits went unexamined (the cache's
  last examined count plus one is below the scene's finished count) or the slot count changes, the
  cache forgets every slot and light view, and every face is redrawn.
- **Raised by.** Scene switches; a finished frame without the shadow plan.
- **Stays triggered while.** Nothing: one frame.
- **Source.** SGL3D's own. Code: `src/stages/shadows/local/cache.rs` (`Cache::begin`).
- **Guard.** `unfinished_frames_leave_nothing_reusable`, `shadow_records_follow_changes_and_dropped_frames`
  (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

#### S-28 An unfinished frame's faces are not reusable

- **Rule.** A face drawn in a frame that is not finished holds nothing reusable; the next frame draws it again.
- **Raised by.** Abandoned frames.
- **Stays triggered while.** The game abandons frames.
- **Source.** SGL3D's own. Code: `src/stages/shadows/local/cache.rs` (`Cache::draw`, `Cache::finish`).
- **Guard.** `unfinished_frames_leave_nothing_reusable` (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

#### S-29 Static layer key

- **Rule.** A face redraws its static layer when its `FaceKey` differs at all from the one it holds:
  light, `LightView` (position, range, shape), face, visibility mask, the materials' caster revision,
  what the shaders of static casters reaching the face read (the frame's `elapsed_seconds` exactly,
  where a shader reads time, and parameter revisions) and the material-map constants.
- **Raised by.** Caster material edits, visibility-mask changes, and the clock.
- **Stays triggered while.** Every frame for a face reached by a static caster whose shader reads
  time (cost only).
- **Source.** SGL3D's own, after Unreal's and HDRP's cached shadow maps (documentation only) and
  Godot's `is_animated()`. Code: `src/stages/shadows/local/plan.rs` (`Plan::place`).
- **Guard.** `shader_driven_casters_redraw_when_their_shaders_inputs_change`,
  `casters_whose_shaders_ignore_the_time_keep_their_shadows_as_it_passes` (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

#### S-30 Moving casters changed

- **Rule.** A face copies its static layer and redraws its moving casters when its list of moving
  casters differs: one entered or left, its pose (exact matrix equality), geometry, deformation
  revision, or its shaders' inputs and shader data changed.
- **Raised by.** Any moving instance in the light's range that changes pose.
- **Stays triggered while.** Traffic or any animated moving instance is in range (cost only).
- **Source.** SGL3D's own. Code: `src/stages/shadows/local/plan.rs` (`Plan::place`).
- **Guard.** `point_light_shadows_follow_current_opaque_blockers`,
  `a_frame_in_which_nothing_moved_in_range_draws_no_shadows` (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

#### S-31 A moved light draws everything and keeps no static layer

- **Rule.** When a light's `LightView` differs at all from the last finished frame's, every face
  draws every caster into the frame atlas, no static layer is kept, and its shadow record carries
  `layers` 0, so probe captures take no shadow from it. A static layer is drawn the frame after the
  light stops moving.
- **Raised by.** Lights attached to moving objects; any per-frame rewrite of a light that changes a
  bit of its position, range or direction.
- **Stays triggered while.** The light moves (cost every frame; captures lose its shadow).
- **Source.** SGL3D's own. Code: `src/stages/shadows/local/plan.rs` (`Plan::place`, `record`),
  `src/shading/local_shadow.wgsl`.
- **Guard.** `ray_hits_sample_the_static_layers` (`src/stages/shadows/local/tests.rs`), partly.
- **Status.** Rule.

#### S-32 Static edit within a face

- **Rule.** A face's slot is cleared, and its layer redrawn, when a pending static edit's bounds lie
  within its light's range and meet the face's frustum.
- **Raised by.** Static edits.
- **Stays triggered while.** Nothing: one frame per edit.
- **Source.** Godot `renderer_scene_cull.cpp` (`_instance_pair`, `_update_instance`), modified.
  Code: `src/stages/shadows/local/plan.rs` (`Plan::mark_static_edits`).
- **Guard.** `static_edits_restale_only_the_faces_they_reach` (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

#### S-33 Atlas reallocation and eviction

- **Rule.** A light that wants another slot size moves once it has held its slots for more than 30
  frames (`REALLOCATION_FRAMES`); a change of face count moves it at once; a slot held 30 frames by a
  light not seen this frame may go to another light; without room a light is unshadowed. A moved light
  redraws its layers; a light that loses its placement leaves the ranking (S-6).
- **Raised by.** Screen coverage changing across a power-of-two size; many casting lights.
- **Stays triggered while.** Coverage oscillates about a size boundary: one move per 30 frames (cost only).
- **Source.** Godot `light_storage.cpp` shadow atlas, faithful but counted in frames, not milliseconds.
  Code: `src/stages/shadows/local/atlas.rs` (`Atlas::update`).
- **Guard.** `lights_beyond_the_atlas_are_lit_unshadowed_and_counted` (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

#### S-34 Atlas resize forgets everything

- **Rule.** A change of atlas size (from the shadow quality) replaces both atlases and forgets every
  placement and static layer.
- **Raised by.** A shadow-quality change.
- **Stays triggered while.** Nothing: it needs a setting change.
- **Source.** SGL3D's own. Code: `src/stages/shadows/local.rs` (`Local::resize`).
- **Guard.** `shadows_follow_a_change_of_shadow_quality` (`src/stages/shadows/local/tests.rs`).
- **Status.** Rule.

### Shadow-map sampling

#### S-35 Temporal shadow filter

- **Rule.** At High shadow quality with TAA or FSR in effect, the camera's surfaces filter shadow
  maps with Jimenez '14's eight-tap spiral, rotated by interleaved-gradient noise of the pixel and the
  frames since history restarted. It drops no history; it relies on the antialiasing to resolve the
  rotation.
- **Raised by.** Every frame at that quality.
- **Stays triggered while.** Always; wherever TAA or FSR reject history, shadow edges show the
  per-frame rotation as shimmer.
- **Source.** Bevy `shadow_sampling.wesl`, faithful. Code: `src/shading/shadow_sampling.wgsl`
  (`sample_shadow_map_jimenez_fourteen`), `src/renderer/effective.rs`.
- **Guard.** None for the rotation.
- **Status.** Rule.

## Ambient occlusion

Ambient occlusion keeps a temporal history at the render size: accumulated visibility, a sample
count of at most 16 and the receiver's linear depth, after DiligentFX's SSAO temporal accumulation
over XeGTAO's spatially denoised result. It reprojects with the G-buffer's motion and tests depth
with the receiver's depth motion, so a surface the camera follows keeps its history. In a
continuously running game the motion-scaled variance gate (AO-8) keeps the count low on streaming
scenery, and grazing distant ground loses taps to the 1% depth test (AO-5).

### Temporal accumulation

#### AO-1 Stage restart

- **Rule.** When the render size changed, the frame's history is not valid, the history count is not
  one more than the last frame ambient occlusion ran, or its quality or radius differs from the last
  frame's (exact comparison), the temporal pass takes no history: visibility is the current value and
  the count 1 over the whole view. The count grows by 1 a frame to 16.
- **Raised by.** Camera cuts, resizes, scene switches, a frame with ambient occlusion off, quality or
  radius setting changes.
- **Stays triggered while.** Nothing: it needs an event or a setting change.
- **Source.** DiligentFX `ResetAccumulation`; the continuity, size, quality and radius conditions are
  SGL3D's own. Code: `src/stages/opaque/ambient_occlusion/temporal.rs` (`Temporal::encode`).
- **Guard.** `gtao_temporal_accumulation_restarts_on_camera_cuts`
  (`src/stages/opaque/ambient_occlusion/integration_tests.rs`);
  `temporal_visibility_accumulates_and_rejects_invalid_history`
  (`src/stages/opaque/ambient_occlusion/tests.rs`).
- **Status.** Rule.

#### AO-2 Sky writes no history

- **Rule.** Where device depth is 0 (sky), the temporal pass writes the current visibility with count
  1 and stored depth 0, so a later frame never takes that texel as a tap.
- **Raised by.** Sky.
- **Stays triggered while.** Always at the sky, invisibly.
- **Source.** Modified: DiligentFX discards background pixels. Code:
  `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** `temporal_visibility_accumulates_and_rejects_invalid_history`
  (`src/stages/opaque/ambient_occlusion/tests.rs`).
- **Status.** Rule.

#### AO-3 Invalid depth motion

- **Rule.** When the G-buffer's depth-motion validity is 0 or the expected previous depth
  (`linear depth + motion.z`) is not positive, the temporal pass takes no history for that pixel.
- **Raised by.** Surfaces behind the previous camera; anything geometry marks invalid.
- **Stays triggered while.** Nothing in normal play.
- **Source.** SGL3D's own (NRD's 2.5D motion). Code: `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** `temporal_visibility_accumulates_and_rejects_invalid_history`
  (`src/stages/opaque/ambient_occlusion/tests.rs`).
- **Status.** Rule.

#### AO-4 Off-screen taps

- **Rule.** A bilinear history tap outside the render size is dropped from the weighted sum.
- **Raised by.** Camera motion at the frame's edges.
- **Stays triggered while.** Nothing: edge pixels only.
- **Source.** Modified: DiligentFX clamps its taps. Code: `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** None.
- **Status.** Rule.

#### AO-5 1% depth test with depth motion

- **Rule.** A bilinear tap is kept only where its stored depth is positive and
  `|1 − (linear depth + motion.z) / stored depth| < 0.01`, per tap. Depth motion carries both the
  camera's and the surface's movement.
- **Raised by.** Disocclusion; depth gradients across the one-pixel footprint, which on grazing
  ground far away exceed 1% a pixel (a road 50 m away seen at 3° changes about 1 m a pixel).
- **Stays triggered while.** The camera moves over distant grazing ground: off-centre taps keep
  failing and accumulation is slower there; usually one tap passes (AO-6).
- **Source.** DiligentFX `SSAO_ComputeTemporalAccumulation.fx` (`IsCameraZSimilar`, 0.01), modified to
  compare the receiver's previous depth through depth motion instead of the current camera's Z.
  Code: `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** `temporal_visibility_keeps_history_when_the_receiver_follows_the_camera`,
  `temporal_visibility_accumulates_and_rejects_invalid_history`
  (`src/stages/opaque/ambient_occlusion/tests.rs`).
- **Status.** Rule.

#### AO-6 Minimum accepted weight

- **Rule.** When the kept taps' weights sum to 0.01 or less, the pixel takes no history.
- **Raised by.** Disocclusion (AO-5).
- **Stays triggered while.** As AO-5.
- **Source.** DiligentFX (`TotalWeight > 0.01`), faithful. Code: `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** `temporal_visibility_accumulates_and_rejects_invalid_history` (`src/stages/opaque/ambient_occlusion/tests.rs`).
- **Status.** Rule.

#### AO-7 Count capped at 16

- **Rule.** The sample count is capped at 16, so at least 1/16 of the current visibility enters every frame.
- **Raised by.** Long accumulation.
- **Stays triggered while.** Always once converged.
- **Source.** DiligentFX (`SSAO_MAX_HISTORY_LENGTH` 16), faithful. Code: `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** None.
- **Status.** Rule.

#### AO-8 Motion-scaled variance gate

- **Rule.** With `mf = clamp(1.025 − |(v.x · aspect, v.y)| · 128, 0, 1)` for the pixel's motion `v`
  in UV and `γ = mix(0.5, 2.5, mf²)`, when the history lies at least `γσ` from the mean of the 3×3
  current visibility, the count becomes `max(1, mf · count)`. `mf` falls below 1 above about 0.2
  pixels of motion a frame at 1080p and reaches 0 at about 8.6 pixels vertically (about 15
  horizontally at 16:9), where the count resets to 1.
- **Raised by.** Screen-space motion.
- **Stays triggered while.** Scenery streams past a moving camera: its count stays low wherever the
  noise puts history outside the box, so its occlusion is noisier than at rest; a followed vehicle
  keeps `mf` 1.
- **Source.** DiligentFX (`SSAO_TEMPORAL_MOTION_VECTOR_DIFF_FACTOR` 128, gamma 0.5 to 2.5), faithful.
  Code: `src/stages/opaque/ambient_occlusion/temporal.wgsl` (`resolve`).
- **Guard.** None.
- **Status.** Rule.

#### AO-9 Noise follows the history count

- **Rule.** XeGTAO's spatio-temporal noise index is the frames since history restarted, so its
  noise pattern restarts with history; it drops no history.
- **Raised by.** Every frame.
- **Stays triggered while.** Always.
- **Source.** XeGTAO (`288 · (NoiseIndex % 64)`), faithful. Code: `src/stages/opaque/mod.rs`
  (`encode_ambient_occlusion`).
- **Guard.** None.
- **Status.** Rule.

#### AO-10 A frame not finished leaves the accumulation as its last finished frame left it

- **Rule.** A frame not finished leaves the stage's history as its last finished frame left it:
  the temporal pass keeps its run (count and the half of its pair it writes), quality and radius
  as that frame left them (`Kept`), so a frame rendered again reads and writes the same halves.
  The pass restarts as in AO-1 when it did not run in that frame, or on a resize or a quality or
  radius change.
- **Raised by.** A game that renders a frame and abandons or re-renders it.
- **Stays triggered while.** Nothing.
- **Source.** SGL3D's own. Code: `src/stages/opaque/ambient_occlusion/temporal.rs`
  (`Temporal::encode`, `Temporal::settle`).
- **Guard.** `accumulated_visibility_survives_an_unfinished_frame`
  (`src/stages/opaque/ambient_occlusion/integration_tests.rs`).
- **Status.** Rule.

## Screen-space indirect light

The pass keeps, at half the render size, the accumulated light and coverage, a frame count of at
most 16, and each receiver's depth and normal. It reprojects with the G-buffer's motion; the next
frame's lighting reprojects again, per fragment.

#### SI-1 Stage restart

- **Rule.** When the render size changed, the frame's history is not valid, the last finished
  frame of the view's scene did not run the pass, or its level, its radius or the radius its
  depth pyramid is filtered for (ambient occlusion's where that runs, its own otherwise) differs
  from that frame's, nothing is kept: the frame's lighting takes no light and no coverage, and
  accumulation starts at count 1.
- **Raised by.** Camera cuts, resizes, scene switches, the setting turned on, a level or radius
  change, ambient occlusion turned on or off or its radius changed.
- **Stays triggered while.** Nothing: it needs an event.
- **Source.** SGL3D's own, as AO-1. Code: `src/stages/opaque/screen_indirect.rs`
  (`ScreenIndirect::begin`).
- **Guard.** `the_bounce_is_one_frame_late_and_restarts_on_a_cut_and_a_resize`
  (`tests/sgl-3d/tests/scenario_screen_space_indirect.rs`).
- **Status.** Rule.

#### SI-2 The bounce is one frame late

- **Rule.** A frame's lighting takes the result its last finished frame kept, never its own.
- **Raised by.** Every frame.
- **Stays triggered while.** Always.
- **Source.** SGL3D's own (D-65). Code: `src/shading/screen_indirect.wgsl`
  (`screen_indirect_read`).
- **Guard.** As SI-1.
- **Status.** Rule.

#### SI-3 Similarity test

- **Rule.** A kept texel is a tap only where its count is above 0, its normal does not oppose the
  receiver's, and its point lies within 0.003 of the receiver's view depth from the receiver's
  tangent plane at its previous position. It applies to the pass's own reprojection and to each
  fragment's read.
- **Raised by.** Disocclusion; edges between surfaces at half size.
- **Stays triggered while.** Nothing in a still picture.
- **Source.** Bevy `bevy_solari` `pixel_dissimilar`, faithful, evaluated in the previous view's
  space. Code: `src/shading/screen_indirect.wgsl` (`screen_indirect_similar`).
- **Guard.** `a_fragment_reads_the_light_kept_at_its_previous_position_and_none_across_an_edge`
  (`src/stages/opaque/screen_indirect/tests.rs`), `a_revealed_surface_takes_none_of_what_hid_it`
  (`tests/sgl-3d/tests/scenario_screen_space_indirect.rs`).
- **Status.** Rule.

#### SI-4 No valid tap

- **Rule.** Where the previous position is behind the previous camera or off screen, or the
  passing taps' weights sum to 0.01 or less, the pass takes no history and a fragment takes no
  light and no coverage.
- **Raised by.** Disocclusion, the frame's edges under camera motion.
- **Stays triggered while.** The camera moves: a band at the leading edge has no bounce for one
  frame.
- **Source.** DiligentFX (`TotalWeight > 0.01`), as AO-6. Code: `src/shading/screen_indirect.wgsl`
  (`screen_indirect_taps`, `screen_indirect_read`), `src/stages/opaque/screen_indirect.wgsl`
  (`accumulate`).
- **Guard.** As SI-3, for a fragment's read; none guards the pass's own off-screen test.
- **Status.** Rule.

#### SI-5 Count capped at 16

- **Rule.** The count is capped at 16, so at least 1/16 of the current gather enters every frame;
  there is no variance gate. A change in what lights a surface is followed at that rate: 2% of a
  removed source's bounce remains after 64 frames.
- **Raised by.** Lighting changes, moving emitters.
- **Stays triggered while.** Always once converged.
- **Source.** DiligentFX (`SSAO_MAX_HISTORY_LENGTH` 16), as AO-7. Code:
  `src/stages/opaque/screen_indirect.wgsl` (`accumulate`).
- **Guard.** `the_bounce_fades_after_its_source_leaves`
  (`tests/sgl-3d/tests/scenario_screen_space_indirect.rs`).
- **Status.** Rule.

#### SI-6 A frame not finished leaves the kept result as its last finished frame left it

- **Rule.** As AO-10: a frame rendered again reads and writes the same halves of the pair.
- **Raised by.** A game that renders a frame and abandons or re-renders it.
- **Stays triggered while.** Nothing.
- **Source.** SGL3D's own. Code: `src/stages/opaque/screen_indirect.rs` (`ScreenIndirect::begin`,
  `ScreenIndirect::settle`).
- **Guard.** `the_kept_result_survives_an_unfinished_frame`
  (`src/stages/opaque/screen_indirect/tests.rs`).
- **Status.** Rule.

## Volumetric fog

The fog keeps its last froxel volume, unfiltered, in one of two alternating volumes, and blends
each froxel with where it was last frame by a fixed 0.9, after Godot. It has no neighbourhood clamp
or change test, so moving lights and fog volumes trail rather than reject. A continuously running
game drops it only by resets, resizes and frames without fog; a changing `Fog::length` keeps it
(V-3), and a moved or changed `Fog::start` trails through the history as a changed density does,
with no restart. Under a procedural sky the integrated volume also holds aerial perspective (the fog over the
air, and the fog alone for the sky); the integration composes it afresh each frame from that
frame's aerial volume, so its aerial slices are never reprojected and only the froxels keep history.

### Froxel history

#### V-1 Renderer reset forgets both volumes

- **Rule.** When the frame's history is not valid, the fog forgets what both volumes hold, even in a
  frame without fog, so the next fog frame does not reproject. A froxel blending 0.9 of its history
  reaches 90% of a steady value in about 22 frames.
- **Raised by.** Camera cuts, resizes, scene switches.
- **Stays triggered while.** Nothing: one frame.
- **Source.** Modified: Godot blends from a cleared volume, so its fog fades in. Code:
  `src/stages/fog/volumes.rs` (`Holds::frame`).
- **Guard.** `a_reset_without_fog_ends_the_froxels_history` (`src/stages/fog/volumes.rs`).
- **Status.** Rule.

#### V-2 A frame without fog ends the history

- **Rule.** A frame reprojects only when the other volume holds the frame whose history count is one
  less, at any length (V-3); otherwise its whole volume takes no history and no jitter.
- **Raised by.** Fog off for a frame: `Settings::atmosphere` or `FrameInput::atmosphere` off; no
  medium with no fog volumes (a zero `Fog::density`, or a `Fog::start` at or beyond the volume's
  reach, `length` of view depth along the frame's corner ray); a `Fog::length` that is not positive
  and finite; or a camera without `perspective`'s projection (`renderer/effective.rs`, `fog`).
- **Stays triggered while.** Nothing: one frame.
- **Source.** SGL3D's own. Code: `src/stages/fog/volumes.rs` (`Holds::frame`).
- **Guard.** `a_reset_without_fog_ends_the_froxels_history` (`src/stages/fog/volumes.rs`).
- **Status.** Rule.

#### V-3 A changed fog length reprojects through the last length

- **Rule.** A froxel reprojects into the last frame's volume through the length that volume covered:
  its world point's last view depth is placed among the slices of that length (the stage keeps each
  volume's length with its history count), so `Fog::length` may differ from the last frame's by any
  amount and the frame keeps its history and jitter. Points beyond the last length fall outside the
  last volume and take none (V-6).
- **Raised by.** Nothing: no length drops history. A longer fog leaves its new far slices without
  history for one frame (V-6).
- **Stays triggered while.** Nothing.
- **Source.** Modified from Godot `volumetric_fog_process.glsl`, which divides the previous view depth
  by the current length (`prev_view.z /= -params.fog_frustum_end`) and never restarts on a length
  change, so it reads a stationary medium from the wrong slices while the length changes. Settled
  O-7, 2026-11-18: SGL3D had rejected history at any change of length, so a game computing its length
  with rounding noise never accumulated. Code: `src/stages/fog/volumes.rs` (`Holds::frame`),
  `src/stages/fog.rs` (`VolumetricFog::encode`), `src/stages/fog.wgsl` (`inject`).
- **Guard.** `a_length_with_rounding_noise_accumulates_as_an_unchanged_one` (a length one f32 step
  apart on alternate frames converges as the unchanged length's within ten half-float steps;
  0.021 apart under the old rule, 0 now), `changing_fog_range_keeps_a_stationary_volume_in_place`
  (the box stays at its depth across a doubled or halved length; Godot's lookup through the current
  length reads 0.0100 inside it) (`src/stages/fog/tests.rs`).
- **Status.** Rule.

#### V-4 A new froxel size starts afresh

- **Rule.** When the fog quality or the render size changes the froxel count, the fog makes new
  zeroed volumes and forgets their history. Aerial perspective coming into or out of effect changes
  only the integrated volume's slices: the fog makes that volume anew and keeps its froxels and their
  history.
- **Raised by.** Fog-quality changes, resizes.
- **Stays triggered while.** Nothing: it needs a size or setting change.
- **Source.** SGL3D's own. Code: `src/stages/fog.rs` (`VolumetricFog::prepare`, `froxels`),
  `src/stages/fog/volumes.rs` (`Volumes::resize_integrated`).
- **Guard.** `aerial_perspective_coming_into_effect_keeps_the_froxels_history`
  (`src/stages/fog/tests.rs`), for the integrated volume alone.
- **Status.** Rule.

#### V-5 A retried frame does not reproject the filtered volume

- **Rule.** When the slice filter runs it overwrites the other volume, which is then recorded as
  holding this frame, so a frame retried after submission without `finish_frame` does not take it as
  its predecessor. This is the History contract's second exception, named in the History contract
  ("What a frame commits").
- **Raised by.** A submitted frame retried without `finish_frame`.
- **Stays triggered while.** Nothing: one frame.
- **Source.** SGL3D's own. Code: `src/stages/fog.rs` (`VolumetricFog::encode`), `src/stages/fog/volumes.rs` (`Holds::filtered`).
- **Guard.** `a_submitted_filtered_frame_is_not_reprojected_as_previous_on_retry` (`src/stages/fog/tests.rs`).
- **Status.** Rule.

#### V-6 Per-froxel reprojection validity

- **Rule.** When a froxel's world position lies behind the last frame's camera (clip w at most 0) or
  outside the open unit box of the last frame's volume, the injection takes no history for it and
  samples its centre without jitter.
- **Raised by.** Turning the camera; froxels beyond last frame's frustum or length.
- **Stays triggered while.** Nothing: the frustum's edges for one frame.
- **Source.** Godot `volumetric_fog_process.glsl`, modified to reproject through the full previous
  view-projection. Code: `src/stages/fog.wgsl` (`inject`).
- **Guard.** `a_still_point_light_holds_its_froxel_steady_over_the_jitter_cycle` (`src/stages/fog/tests.rs`), indirectly.
- **Status.** Rule.

#### V-7 Fixed 0.9 history without clamp

- **Rule.** Where a froxel reprojects, it keeps 0.9 of its history (`TEMPORAL_REPROJECT_AMOUNT`) with
  no neighbourhood clamp, depth test or lighting-change test.
- **Raised by.** Every reprojecting frame.
- **Stays triggered while.** Always: moving lights, lights switched on or off and moving fog volumes
  leave trails that fade over 10 to 20 frames.
- **Source.** Godot (`volumetric_fog_temporal_reproject_amount` default 0.9), faithful. Code:
  `src/stages/fog.rs` (`VolumetricFog::encode`), `src/stages/fog.wgsl` (`inject`).
- **Guard.** None.
- **Status.** Rule.

#### V-8 Non-finite froxels

- **Rule.** A non-finite current froxel takes its history (0 where that is non-finite too); a
  non-finite history leaves the current froxel unblended.
- **Raised by.** Non-finite lighting or density.
- **Stays triggered while.** Nothing in normal play.
- **Source.** SGL3D's own. Code: `src/stages/fog.wgsl` (`inject`).
- **Guard.** None.
- **Status.** Rule.

#### V-9 Jitter follows the history count

- **Rule.** A reprojecting froxel samples at Godot's 16-entry Halton offset indexed by the frames
  since history restarted (modulo 16); it drops no history.
- **Raised by.** Every reprojecting frame.
- **Stays triggered while.** Always.
- **Source.** Modified: Godot indexes by the global frame number. Code: `src/stages/fog.rs` (`VolumetricFog::encode`).
- **Guard.** None.
- **Status.** Rule.

## Dynamic GI

The dynamic GI stage keeps world-space probe state: each probe's irradiance estimators, depth
moments, offset and class, stored at its lattice coordinate plus the volume's scroll. It takes no
view reset; it restarts for another scene or lattice and after a finished frame without it,
clears the planes a scroll brings in, and otherwise follows changes through Wicked's multiscale mean
estimator. A continuously running game trips its scroll clearing (G-4) while travelling, and an
unsnapped or float-noisy placement (G-2) restarts it every frame.

### Probe state

#### G-1 Restart for another scene or lattice

- **Rule.** When the scene's identity or its installed volume's lattice generation differs from the
  committed probes', the stage starts fresh probes, all unblended, which light nothing until traced.
  Unblended probes start nearest the camera first within the ray budget.
- **Raised by.** Scene switches; a new lattice (G-2).
- **Stays triggered while.** As G-2.
- **Source.** SGL3D's own, after RTXGI's scrolling volume (practice only). Code:
  `src/stages/dynamic_gi.rs` (`DynamicGi::prepare`).
- **Guard.** `the_probes_restart_only_for_another_placement_or_scene_or_a_frame_without_them`
  (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-2 A placement off the lattice is a new lattice

- **Rule.** `Scene::set_dynamic_gi_volume` makes a new lattice, and so G-1's restart, when the
  spacing or probe counts differ at all from the installed volume's, or when the new origin lies off
  the lattice by more than 1e-3 spacings plus its f32 rounding; an origin moved by whole spacings
  scrolls instead (G-4).
- **Raised by.** A game installing the volume at an unsnapped position, or computing its spacing with rounding noise.
- **Stays triggered while.** The game re-installs an unsnapped volume every frame: the probes restart
  every frame and never light anything.
- **Source.** SGL3D's own (RTXGI's scroll anchor practice). Code: `src/scene/dynamic_gi.rs`
  (`Scene::set_dynamic_gi_volume`), `src/scene/lattice.rs` (`steps`).
- **Guard.** `the_probes_restart_only_for_another_placement_or_scene_or_a_frame_without_them`,
  `the_scene_refuses_a_placement_that_is_not_a_lattice_or_does_not_fit` (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-3 A finished frame without the stage restarts it

- **Rule.** When a frame that does not run the stage (dynamic GI off, Basic binding tier, no volume
  installed) is finished, the committed probes are dropped and the next frame starts fresh. A frame
  that is not finished commits nothing.
- **Raised by.** Toggling the setting or the volume.
- **Stays triggered while.** Nothing: it needs a setting or content change.
- **Source.** SGL3D's own. Code: `src/stages/dynamic_gi.rs` (`DynamicGi::prepare`, `DynamicGi::finish_frame`).
- **Guard.** `the_probes_restart_only_for_another_placement_or_scene_or_a_frame_without_them` (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-4 A scroll clears the planes that enter

- **Rule.** When the volume scrolls by whole spacings, the probes whose lattice coordinates entered
  are reset to fresh, unblended probes and their data texel zeroed before allocation; a move of a
  whole lattice or more clears every probe. The others keep their state.
- **Raised by.** A volume that follows the camera.
- **Stays triggered while.** The camera travels: the leading planes are always fresh and light
  nothing until traced; at speed with a small volume much of it is unconverged.
- **Source.** RTXGI's `DDGIClearScrolledPlane` (practice only). Code: `src/stages/dynamic_gi.rs`
  (`DynamicGi::encode`), `src/stages/dynamic_gi/update.wgsl` (`scroll`, `ddgi_entered`).
- **Guard.** `a_scroll_starts_only_the_probes_that_enter`, `a_scroll_keeps_each_probe_where_it_lies`
  (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-5 A probe's first blend starts afresh

- **Rule.** When a probe that has not been blended traces, its irradiance estimators start at the
  turn's result, its sky visibility at the turn's share (1 where no ray contributes), its depth
  moments at the result (maximum distance where no ray contributes), its offset at 0 and its class
  from the turn's rays.
- **Raised by.** G-1 to G-4.
- **Stays triggered while.** Nothing: one turn per probe.
- **Source.** Modified: Wicked Engine starts every probe on its first frame. Code:
  `src/stages/dynamic_gi/update.wgsl` (`update_irradiance`, `update_depth`).
- **Guard.** `four_ray_updates_keep_unobserved_depth_and_irradiance_history` (`src/stages/dynamic_gi/update_tests.rs`).
- **Status.** Rule.

#### G-6 Multiscale mean estimator

- **Rule.** Each traced irradiance texel caps its sample at `0.1 + short mean + 8σ` and moves its
  long mean towards the sample by
  `clamp(smoothstep(0, 1, relative difference · max(0.02, inconsistency − 0.2)), 1/256, 1)` scaled by
  its variance-based reduction, with the short mean at 0.1, variance at 0.05, inconsistency at 0.08
  and the reduction at 0.1 per turn. A change beyond the noise is followed within a few turns; a
  sudden bright change is first held down by the cap.
- **Raised by.** Lighting changes and ray noise.
- **Stays triggered while.** Not a rejection; far probes follow slower because they take turns less often (G-12).
- **Source.** Wicked Engine `ddgi_updateCS.hlsl` (`MultiscaleMeanEstimator`), faithful. Code:
  `src/stages/dynamic_gi/common.wgsl` (`multiscale_mean_estimator`).
- **Guard.** `the_bounce_carries_light_between_the_walls_damped` (`src/stages/dynamic_gi/tests.rs`), indirectly.
- **Status.** Rule.

#### G-7 An unobserved texel keeps its history

- **Rule.** A texel to which no ray contributes (weight at most 1e-4) keeps its estimator, sky
  visibility or depth moments unchanged, where Wicked and RTXGI blend zero.
- **Raised by.** Few rays on dormant or inactive probes.
- **Stays triggered while.** Not a rejection.
- **Source.** SGL3D's own. Code: `src/stages/dynamic_gi/update.wgsl` (`update_irradiance`, `update_depth`).
- **Guard.** `four_ray_updates_keep_unobserved_depth_and_irradiance_history` (`src/stages/dynamic_gi/update_tests.rs`).
- **Status.** Rule.

#### G-8 Fixed depth, sky visibility and relocation blends

- **Rule.** Depth moments and the sky visibility blend at 0.02 and the probe offset at 0.01 a turn,
  the offset clamped to half a spacing; none has a rejection.
- **Raised by.** Every turn.
- **Stays triggered while.** Not a rejection: geometry changes reach the depth maps and the sky
  visibility over about 50 turns.
- **Source.** Wicked Engine `ddgi_updateCS.hlsl`, faithful; the sky visibility is SGL3D's own, at
  the depth blend's rate ([D-59](decisions.md)). Code: `src/stages/dynamic_gi/update.wgsl`
  (`update_irradiance`, `update_depth`).
- **Guard.** `four_ray_updates_keep_unobserved_depth_and_irradiance_history`
  (`src/stages/dynamic_gi/update_tests.rs`), which holds the sky visibility's blend to 0.02 a turn;
  `a_probe_records_the_share_of_its_rays_that_reach_the_sky` (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-9 Probe classification

- **Rule.** A probe is inactive (weighs nothing) when more than 0.25 of its rays meet back faces,
  and dormant (lights moving receivers alone) when no fixed ray meets a front face within its cell;
  the class is set from its first turn's rays, then from its 32 fixed rays over each cycle of 8 turns.
  A class change switches the probe's whole contribution; its state stays warm.
- **Raised by.** Geometry about the probe.
- **Stays triggered while.** A probe whose backface share sits near 0.25 can change class once per cycle.
- **Source.** RTXGI `ProbeClassificationCS.hlsl` (practice only), modified. Code:
  `src/stages/dynamic_gi/update.wgsl` (`update_depth`), `src/stages/dynamic_gi/common.wgsl` (`ddgi_probe_active`).
- **Guard.** `a_probes_class_holds_still_while_what_it_sees_does` (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-10 Convergence pause and its change detection

- **Rule.** A volume pauses (traces nothing, its probes holding their light) once the mean
  variability over a window of 16 turns of its slowest active probe falls by less than a tenth from
  the last window's, every probe has started and nothing changed. Any change in the scene's edit count,
  deformation edits on the hardware path, the frame data other than cascades and clock, the bound
  environment, a procedural sky's environment atlas revision (X-5), the ray budget or the
  local-lights switch, or a restart, resets the window.
- **Raised by.** Any content edit, including any moving instance's pose; under a procedural sky,
  every frame that writes its environment atlas (X-5), including the cycle a change of the camera's
  height starts, whose own backdrop values in the frame data (`Inputs::of` zeroes them) the window
  ignores.
- **Stays triggered while.** Traffic or any per-frame edit: the volume never pauses (cost only; no history is dropped).
- **Source.** RTXGI sample's pause (practice only), modified. Code: `src/stages/dynamic_gi/inputs.rs`
  (`Inputs::changes`), `src/stages/dynamic_gi/allocate.wgsl` (`ddgi_paused`),
  `src/stages/dynamic_gi/update.wgsl` (`settle`).
- **Guard.** `a_converged_volume_traces_nothing_until_what_its_light_follows_changes`,
  `the_clock_changes_the_probes_inputs_only_where_a_surface_moves`,
  `a_sky_atlas_refreshing_under_held_lights_changes_the_probes_environment`
  (`src/stages/dynamic_gi/tests.rs`).
- **Status.** Rule.

#### G-11 A ray-budget change keeps the probes

- **Rule.** A change of `Settings::dynamic_gi` (the most rays a probe traces) rebuilds the ray
  textures and keeps every probe's state.
- **Raised by.** A settings change.
- **Stays triggered while.** Not applicable.
- **Source.** SGL3D's own. Code: `src/stages/dynamic_gi.rs` (`DynamicGi::prepare`).
- **Guard.** None.
- **Status.** Rule.

#### G-12 Turn periods

- **Rule.** A probe traces once in a period that grows with its distance from the camera (up to
  `DDGI_PERIOD_CAP` frames), shortened while its light is inconsistent beyond 0.2, and lengthened by a
  power of two when requests exceed the budget; between turns it keeps its light.
- **Raised by.** Distance and the ray budget.
- **Stays triggered while.** Not a rejection: far probes answer a lighting change later.
- **Source.** Wicked Engine surfel GI ray budget and periods, modified. Code:
  `src/stages/dynamic_gi/allocate.wgsl` (`ddgi_period`, `ddgi_turn`, `ddgi_shortened_period`).
- **Guard.** `requests_past_the_budget_lengthen_every_period_and_starve_none` (`src/stages/dynamic_gi/allocation_tests.rs`).
- **Status.** Rule.

## Exposure and other stages

Automatic exposure keeps its adapted correction as history and snaps to its target only on a
view's reset or a switch from fixed exposure. The other post stages keep no history. Two
signals cross into other areas: the sky's motion and the temporal masks the transparent stage
writes, which TAA and FSR consume. On a still view the exposure moves only as far as the jitter
moves the metered frame, below one output code (E-3).

### Exposure

#### E-1 Reset snaps the correction to its target

- **Rule.** When the frame's history is not valid, automatic exposure sets its correction to this
  frame's target instead of adapting.
- **Raised by.** Camera cuts, resizes, scene switches.
- **Stays triggered while.** Nothing: one frame; a game that sets `camera_cut` often sees an exposure snap each time.
- **Source.** SGL3D's own (Bevy has no reset). Code: `src/stages/exposure.rs` (`Exposure::encode`),
  `src/stages/exposure.wgsl` (`compute_average`).
- **Guard.** `correction_adapts_at_the_authored_speeds_within_its_limits` (`src/stages/exposure/tests.rs`).
- **Status.** Rule.

#### E-2 A switch from fixed exposure snaps, pending until finished

- **Rule.** When automatic exposure follows a frame of fixed exposure, it snaps as in E-1, and the
  switch stays pending until a frame is finished. The adapted value advances in place on the GPU,
  so after a submitted frame that was not finished it may be one step ahead, which the History
  contract ("What a frame commits") names and no frame reads as a discontinuity.
- **Raised by.** The game switching `Exposure::automatic` on.
- **Stays triggered while.** Nothing: until one frame is finished.
- **Source.** SGL3D's own. Code: `src/stages/exposure.rs` (`Exposure::encode`, `Exposure::finish_frame`).
- **Guard.** `abandoned_automatic_exposure_still_snaps_then_adapts_after_finish` (`src/renderer/test_frame.rs`).
- **Status.** Rule.

#### E-3 Adaptation follows a target metered every frame

- **Rule.** Each frame automatic exposure moves its correction towards the target by
  `min(speed · dt, delta · min(speed · dt / 1.5, 1))` (darken and brighten speeds, `dt` from
  `frame_time_ms`, invalid speeds replaced by defaults), then clamps it to the authored correction
  range. The target is metered from the jittered, un-antialiased frame at the render size, through a
  10% to 90% percentile window of a 64-bin histogram (a quarter stop a bin). With an unchanged target
  the correction lands: its step shrinks until it no longer changes the f32. Under jitter the target
  moves only where samples cross a bin inside the window, and the correction follows by a share of
  the move (1/30 a frame brightening, 1/90 darkening, at the default speeds and 60 frames a second).
  Measured on a still jittered 192 by 108 view after settling (2026-11-18): the suites' stage and an
  unjittered frame hold the exposure exactly; a frame filled with stripes 1.5 px wide every 4 px
  moves it at most 0.0005 stops a frame and 0.0009 stops over six jitter cycles turned 0.05 rad off
  the pixel grid (TAA, FSR2 and FSR3 at Native AA), 0.002 to 0.003 stops at FSR3 Performance's
  96 by 54, and, aligned exactly with the pixel columns so every column samples in step, 0.007 to
  0.012 stops a frame and 0.014 to 0.019 stops over the cycles. One 8-bit sRGB output code at
  white is 0.0128 stops. FSR2 kills a lock when its exposed luma drifts by a 1.88x ratio within the
  lock's life (0.91 stops; `ffx_fsr2_postprocess_lock_status.h`, `fLuminanceDiff > 0.1` on the
  sixth root) and lowers its contribution by the sixth root of the ratio, at most 0.2% here; FSR3's
  shading-change, luma-instability and reactivity tests multiply current and previous luma by the
  same current exposure, so a change between frames cancels in them. TAA does not read exposure.
- **Raised by.** Every frame.
- **Stays triggered while.** Always, within the bound above. Fine periodic detail filling most of
  the frame and aligned with the pixel grid, the case that moves it most, can reach about one
  output code at white over a jitter cycle, never in one frame.
- **Source.** Bevy `auto_exposure.wesl`, faithful except that the step stops at the remaining delta.
  Metered before antialiasing because FSR reads the exposure, as Godot meters its jittered internal
  texture under FSR2 and AMD's own FSR auto exposure meters its jittered input (both adapt
  exponentially); Bevy, and Godot under TAA, meter the resolved frame. Settled O-8, 2026-11-18.
  Metering the antialiased frame instead (measured only, not adopted: the exposure stage moved after
  antialiasing, FSR reading the last frame's exposure, a change to the frame order) cut the aligned
  stripes' range to 0.0019 stops and left the turned stripes' at 0.0003 to 0.0006. Code:
  `src/stages/exposure.wgsl`
  (`compute_histogram`, `compute_average`).
- **Guard.** `still_exposure::taa`, `still_exposure::fsr2`, `still_exposure::fsr3`
  (`tests/sgl-3d/tests/history_misc.rs`: frame step and range under one output code on the turned
  stripes; with the exponential approach removed they step 0.021 to 0.025 stops and fail);
  `correction_adapts_at_the_authored_speeds_within_its_limits`, `a_long_frame_lands_on_the_target`
  (`src/stages/exposure/tests.rs`).
- **Status.** Rule.

### Signals other areas consume

#### X-1 Sky motion

- **Rule.** The sky writes motion for its direction through both cameras (rotation only), zero where
  the current projection gives the direction no positive w, and no depth motion (validity 0); consumers
  that require depth motion (AO-3) take no history at the sky, and ray-traced shadows treat it as no
  receiver (S-9). The full entry is M-10. A procedural sky is not turned (its backdrop yaw is 0 in both
  frames) and its change with the lights writes no motion.
- **Raised by.** Sky pixels.
- **Stays triggered while.** Always at the sky.
- **Source.** Diligent `EnvMap.psh` and Bevy's skybox motion vectors, faithful. Code: `src/stages/opaque/backdrop.wgsl` (`sky_motion`).
- **Guard.** None.
- **Status.** Rule.

#### X-2 Temporal masks from the transparent stage

See M-11, M-16 and M-17a to M-17c.

#### X-3 Per-frame noise tied to history

- **Rule.** Ambient occlusion's noise index, the fog's Halton offset and the shadow maps' temporal
  rotation follow the frames since history restarted; the ray-traced shadow seed and the dynamic GI
  ray rotation follow the stages' own counters. None drops history; each is the variation a temporal
  pass must absorb, so every rejection rule in this file shows it.
- **Raised by.** Every frame.
- **Stays triggered while.** Always.
- **Source.** See AO-9, V-9, S-35, S-26, G-12. Code: `src/stages/opaque/mod.rs`, `src/stages/fog.rs`,
  `src/shading/shadow_sampling.wgsl`, `src/stages/shadows/traced.rs`, `src/stages/dynamic_gi.rs`.
- **Guard.** None.
- **Status.** Rule.

#### X-4 Sky tables

- **Rule.** The sky stage's tables keep no history: each holds a function of its inputs and is computed again in any frame whose inputs differ from those it was computed from. What a table holds is recorded when the frame that computed it is finished. Each table is one copy, which a render overwrites whether or not its frame is finished, so an unfinished render that computed a table leaves its contents unknown, and the next frame computes it again whatever its inputs, a return to the last finished frame's included.
- **Raised by.** A change of `Sky`'s medium fields; for the sky-view table also a change of a directional light or of the camera's height above `Sky::ground_height`; for any table, an unfinished render that computed it.
- **Stays triggered while.** The inputs keep changing, and until a frame that computed the table is finished.
- **Source.** SGL3D's own; Bevy 9d12036 computes every table every frame (`atmosphere/node.rs`). Code: `src/stages/sky.rs`.
- **Guard.** The stage's encode-count test.
- **Status.** Rule.

#### X-5 Sky environment atlas

- **Rule.** The sky stage's environment atlas follows the sky-view table over a 16-step cycle, one step per finished frame, written in place. A cycle under way runs to its end before the next starts, so a change that arrives the frame after a capture reaches the sharp level 16 frames later, counting the frame of the change, and the roughest level, which diffuse environment light reads, 31 frames later; a change to an idle atlas reaches them in that frame and 15 frames later. During a cycle the faces and levels it has refiltered come from its capture and the rest from the cycle before, so neighbouring faces of a level can be a whole cycle apart; each level is filtered from the level before within one cycle, so a level's lineage is consistent. The whole cycle runs in the stage's first sky frame and in any frame that restarts the view's history. A frame rendered again runs the same step. An unfinished frame commits nothing of the cycle, and a sky-view table it computed is computed again in the next frame (X-4), which so starts the cycle that change needs.
- **Raised by.** The sky-view table computed since the last cycle's capture (X-4).
- **Stays triggered while.** The cycle runs; again while the table keeps changing.
- **Source.** SGL3D's own schedule; Godot's incremental sky radiance update is the practice. Code: `src/stages/sky/environment.rs` (`AtlasCycle`).
- **Guard.** The schedule test; `scenario_sky`'s 16-frame and cut scenarios.
- **Status.** Rule.

### Stages without history

- Directional shadow cascades: every layer is cleared and redrawn each frame; texel snapping is stabilisation, not reuse.
- Ray-traced shadow upsample: reads only this frame's temporal result.
- Bloom: a per-frame mip chain.
- Local exposure: a per-frame grid and blur of the image the tone map reads.
- Tone mapping and deband dither: per frame; the dither is screen-space and static.
- SMAA: three per-frame passes with no temporal state or jitter.
- FSR1 EASU and sharpening (RCAS): per-frame spatial.
- Post inputs and output: bind-group caches only.
- Probe prefilter: one pass per capture.
- Frame probe: diagnostics readback only.
- Backdrop draw: per frame (its motion is X-1).
- Transparent receivers, volume layers, transmission copy and heat snapshot: rebuilt every frame they run.
