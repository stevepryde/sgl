# Changes and migrations

Migration guidance for agents maintaining games with SGL. Read all entries
after the game's current version through its target version, in version order.
`Unreleased` describes changes on Git that are not yet in a released version.
See the [update workflow](docs/README.md#updating-a-game) for dependency requirements
and validation in the consuming game.

One bullet per change: the crate and symbol, the old and new behaviour in a
clause, and the migration in a line or a short code sample. Record changes
that still compile but alter what a game sees (defaults, settings, units,
platforms, asset and bake formats), and say when no game-code change is
needed. Rationale, measurements and full API details belong in the package
docs and specs the entry links.

## Unreleased

- `sgl-core` `CollisionConfig`: public fields allowed literals and mutations to bypass validation; fields are now private. Construct with `CollisionConfig::new` and read values through `up()`, `skin()`, `snap_distance()`, `normal_threshold()`, and `block_epsilon()`; reconstruct to change a value.

- `sgl-core` `move_and_collide`/`sweep_aabb`: motion below `f32::EPSILON` was dropped or treated as parallel and could be reported blocked in empty space; every finite nonzero remainder is now swept and applied when clear. No game-code changes are needed.

- `sgl-core` `move_and_collide` and `snap_to_ground`: contacts ending exactly on a broadphase grid-cell boundary could be missed; swept queries now preserve that end boundary. No game-code changes are needed.
- `sgl-net` WebSocket bind/connect: invalid subprotocol names formerly bound or attempted a handshake; they now fail configuration unless they are one non-empty HTTP token. Rename names containing commas, whitespace, delimiters or non-ASCII characters on both peers.

- `sgl-net` WebSocket inbound admission: an over-cap message on a full lane formerly waited on native or closed with `InboundOverflow` in the browser; it now immediately closes with `ProtocolViolation`. No game-code changes are needed.

- `sgl-net` `NativeWebSocketClient::connect`: a silent first resolved address formerly exhausted the connection deadline; TCP attempts now share the remaining budget so later addresses can connect. No game-code changes are needed.
- `sgl-net` UDP `Disconnect` handling: a valid disconnect arriving before its confirm formerly admitted a ghost connection on that confirm; the server now retires the cookie before admission. No game-code changes are needed.

- `sgl-net` UDP `Endpoint::disconnect`: closing peers formerly ignored newly received reliable fragments; they now buffer and acknowledge them under the existing limits without returning new messages. No game-code changes are needed.
- `sgl-3d` `CompressedImage::from_ktx2`: a corrupt KTX2 data format descriptor formerly panicked; it now returns `Err`. No game-code changes are needed.

- `sgl-3d` `asset::load`/`load_slice`: malformed glTF accessor references, animation targets and image sources formerly panicked; they now return `Err` naming the element. No game-code changes are needed.
- `sgl-3d` `LightShape::Spot`: a closed or unrepresentably narrow casting cone formerly built a non-finite shadow view; it now releases its shadow slots until its projection is finite. No game-code changes are needed.
- `sgl-2d` `UiFrame::number_field`: large values formerly opened drafts beyond the typing limit and rejected insertions; oversized opening drafts now use scientific notation. No game-code changes are needed.

- `sgl-2d` `UiFrame::number_field`: vertical drags formerly opened typing as clicks; movement past the click threshold on either axis now prevents typing while only horizontal travel changes the value. No game-code changes are needed.

- `sgl-2d` `UiFrame::line_edit_clear`: long drafts formerly drew text and the caret beneath the clear button; both now stay within the space to its left. No game-code changes are needed.

- `sgl-2d` `Context`: windows larger than the device texture limit formerly panicked during startup or resize; swapchain dimensions are now capped per axis to that limit. No game-code changes are needed.
- `sgl-3d` `Exposure`: abandoning the first automatic frame after fixed exposure formerly lost its snap to target; the next submitted automatic frame now snaps. No game-code changes are needed.
- `sgl-3d` dynamic GI: sparse probe turns formerly blended unobserved depth and irradiance texels toward zero; those texels now keep their history, and fresh unobserved depth starts at the maximum ray distance. No game-code changes are needed.

- `sgl-post-fx` SSR: glossy grazing sample PDFs formerly relied on backend half-float overflow behavior; stored PDFs now clamp to the finite range so reconstruction keeps their weight. No game-code changes are needed.

- `sgl-3d` `Renderer` geometry pipelines: each change of rect lights, decals, films, sheens, diffuse transmission, transmissive materials or diagnostics layers formerly kept every earlier pipeline set for the renderer's lifetime; only the current and previous sets are now kept. No game-code changes are needed.
- `sgl-3d` hardware ray tracing: a model whose indices start 4 GiB or more into the ray source formerly failed wgpu's BLAS build validation and panicked; it is now left out of the TLAS as a model past the device's BLAS limits is. No game-code changes are needed.

- `sgl-net` UDP server challenge limiter: IPv4 clients of a dual-stack (`[::]`) socket formerly shared one challenge rate-limit bucket; IPv4-mapped sources are now bucketed by their IPv4 /24. No game-code changes are needed.
- `sgl-net` UDP server `Disconnect` handling: a client that closed while holding a newer challenge's cookie than the admitted one formerly left the server's connection open until `timeout_ms`; the server now ends it on that disconnect. No game-code changes are needed.
- `sgl-net` `ThreadedUdpServer`: if the worker panicked while holding the command queue, `send`, `capacity`, `disconnect` and `stop_admission` formerly panicked until `poll` joined it, and `poll` could panic on a poisoned ingress queue; none panics now, and until that `poll` `send` returns `UnknownConnection` and `capacity` zero. No game-code changes are needed.
- `sgl-net` `BrowserWebSocketClient` flush: a latest state over `latest_buffered_bytes` formerly held lane frames to that watermark; lane frames now continue up to `reliable_buffered_bytes` while the state stays coalesced, and the state leaves once the bytes buffered ahead of it have drained, never past `reliable_buffered_bytes`. No game-code changes are needed.

- `sgl-net` `ThreadedUdpServer`: after a worker panic, `send`, `capacity`, `disconnect` and `poll` could panic on its poisoned queues; they no longer do, and until `poll` reports the ends `send` returns `UnknownConnection` and `capacity` zero. No game-code changes are needed.
- `sgl-net` `BrowserWebSocketClient` flush: a latest state over `latest_buffered_bytes` formerly held lane frames to that watermark; lane frames now continue up to `reliable_buffered_bytes` while the state stays coalesced. No game-code changes are needed.

- `sgl-3d` `asset::load`/`load_slice`: a GLB header declaring a length below 12 bytes panicked, and a zero-scale matrix on a joint or a joint's ancestor, a non-integer `EXT_materials_bump` `texCoord`, a `baseColorFactor` the scene rejects, or a negative or non-finite `emissiveFactor` or `emissiveStrength` loaded; each now fails the load, naming the node or material. Re-export affected assets; no game-code changes are needed.

- `sgl-3d` `CompressedImage`: a side above 16384 passed scene validation (overflowing on wasm32); scenes now refuse it with `SceneError::InvalidCompressedImage`, as `from_ktx2` does. Store sides no larger than 16384; no game-code changes are needed otherwise.

- `sgl-2d` `UiFrame::confirm_modal`: the pointer press or Escape that opened a modal could also confirm or cancel it on that frame; the modal now ignores both until it has been shown for a frame. No game-code changes are needed for a modal submitted every frame while open.

- `sgl-2d` 2D lights: a zero-height light exactly on a normal-mapped pixel wrote NaN into the light accumulation; it now lights that pixel at full strength, matching `normal_factor`. No game-code changes are needed.

- `sgl-core` `move_and_collide` one-way platforms with a tilted `up`: blocking tested motion against `up`, so a body could fall through the top face or stick walking along it; it now tests motion into the floor face. No game-code changes are needed.

- `sgl-net` `udp::Endpoint`: a confirm carrying a newer cookie formerly changed an admitted connection's cookie and could stop both directions; the server now keeps its admitted cookie. No game-code changes are needed.

- `sgl-net` UDP `Endpoint::disconnect`: closing peers previously stopped
  sending acknowledgements and keepalives; they now continue through the
  existing grace so a remote holding data does not time out just because
  `close_grace_ms` exceeds `timeout_ms`. No game-code changes needed.

- `sgl-core` `CollisionConfig::new`: an `up` with finite components but an
  overflowing length normalized to zero; it now panics as for other unusable
  directions. Supply a normalizable direction; valid games need no game-code changes.
- `sgl-2d` `UiFrame` draw ordering: long ordinary, panel and popover lists
  could cover higher UI layers; draws now stay within their own band, so
  panels, popovers and modals stay above lower layers at any widget count.
  No game-code changes.
- `sgl-net` native WebSocket server and client sockets previously left Nagle's
  algorithm enabled; they now disable it before the handshake where the
  platform supports `TCP_NODELAY`. No game-code changes needed.
- `sgl-post-fx` SSR: missed rays no longer set the reflection's reprojection
  depth; only confirmed hits supply the virtual-point distance. No game-code
  changes.
- `sgl-net` `udp::DatagramTransport::send`: temporarily refused datagrams now keep their queued work instead of dropping messages or advancing reliable retries, and return `io::Result<()>`. Standard adapters need no game-code changes; custom transports must return their send result and classify temporary local buffer exhaustion as `WouldBlock`.

- `sgl-net` UDP `Endpoint`, `UdpServer`, `UdpClient` and
  `ThreadedUdpServer`: dropping an endpoint or shutting down its worker now
  notifies peers instead of leaving them to time out; a stopped or panicked
  worker reports each live connection `Local` after its queued messages.
  No game-code changes.
- `sgl-net` native WebSocket: unfinished frame fragments could stall the
  shared worker; each socket turn now bounds raw bytes read and yields to
  other peers and outbound messages. No game-code changes.
- `sgl-post-fx` SSR temporal accumulation could select an off-screen
  reflection hit and discard valid surface history; it now compares only
  candidates in the previous viewport. No game-code changes.
- `sgl-post-fx` `ScreenSpaceReflection` at half resolution previously dropped
  reflective blocks beside rough surfaces; each block now traces an eligible
  surface when one is present. No game-code changes are needed.
- `sgl-post-fx` `TemporalAntiAliasing`: the placeholder for a new feature
  set copied input alpha as history confidence; it now starts at reset
  confidence, so the next frame ramps up history weight. No game-code
  changes needed.
- `sgl-post-fx` `PostFXContext::execute`: switching from a caller camera buffer
  to inline cameras now writes only the context’s own buffer instead of
  overwriting the caller’s data or failing validation. No game-code changes.
- `sgl-post-fx` `ScreenSpaceReflection`: conventional depth now keeps its
  resolved depth at 32 bits instead of 16, preserving distant reflection
  reprojection; reversed depth is unchanged. No game-code changes.
- `sgl-3d` `AlphaMode::Blend` with `keeps_specular: true`: alpha left
  diffuse transmission at full strength; it now fades that light with
  coverage, as it fades reflected diffuse light. No game-code changes.
- `sgl-3d` `SurfaceMaterial::transmission` with a sheen: refracted light
  bypassed the sheen layer; it now dims beneath that layer with the rest
  of the base light. No game-code changes.
- `sgl-3d` `asset::load`, `load_slice` and `CompressedImage::from_ktx2`:
  decoded images were not budgeted and KTX2 level expansion was unbounded;
  PNG/JPEG sides above 16384 or decoded assets beyond the load budget, and
  KTX2 chains with invalid dimensions, mip counts or decoded level sizes,
  now fail before oversized allocation. Split oversized assets or re-export
  malformed textures; no game-code changes are needed. Refresh distribution
  notices for the vendored MIT OR Apache-2.0 Zstandard decoder.

- `sgl-core` `move_and_collide` and `snap_to_ground`: skin back-off now
  stays on the swept path instead of moving along only the contact axis;
  diagonal slides stop earlier along the surface by the backed-off motion's
  tangential component. No game-code changes unless a game depended on the
  exact stop position.
- `sgl-2d` `confirm_modal` and dropdown popovers: controls inside
  `overlay_panel_begin` could act and draw over them; they now stay below
  and are blocked by the dialog or popover. No game-code changes.

- `sgl-net` UDP `EndpointConfig::close_retransmits`: earlier transmissions
  could exhaust a graceful close's resend budget; each unacknowledged
  fragment now gets up to this many more sends after disconnect, within
  `close_grace_ms`, regardless of its earlier sends. No game-code changes.
- `sgl-3d` `Scene::set_instance_baked_irradiance`, material
  `normal_scale`/`coat_normal_scale`/`bump_scale` and
  `Scene::set_lightmap`'s `uv_scale_offset` previously accepted invalid values;
  they now reject non-finite scales/offsets and irradiance outside finite
  `0..=65504` with the existing typed errors. Supply finite material and chart
  values and in-range irradiance; valid games need no game-code changes.

- `sgl-3d` `ColorGrading`: the shadows-to-midtones transition blended
  highlights with midtones; it now blends shadows with midtones. Games with
  distinct shadows and highlights grading see their dark tones change.
  No game-code changes needed.
- `sgl-post-fx` `ScreenSpaceReflection` (and `sgl-3d`'s Crystal SSR):
  half-resolution mirror pixels read a neighbouring block's ray on odd
  rows and columns; they now read their own 2×2 block, removing the
  one-pixel reflection shift. No game-code changes needed.

- `sgl-core` `SequenceStep::RandomPause`: the documented range changes from
  `[min, max)` to `[min, max]` to include the existing rounding behavior.
  Playback and seeded sequences are unchanged. No game-code changes.
- `sgl-net` `NativeWebSocketServer::poll`: a lane backlog could indefinitely
  hide a peer's latest state; each peer's first message turn now returns its
  newest pending state, at most once per poll, within the global event budget.
  No game-code changes needed.
- `sgl-2d` `LightPass::prepare`: shadow geometry beyond the device's buffer
  limit or u32 draw range now drops surplus whole occluder edges instead of
  ending the frame with a validation error. No game-code changes needed.

- `sgl-3d` glTF import: buffer and image URIs could read any file and
  allocate without a source-byte cap; they now accept embedded data or
  relative regular files without parent segments or symlinks, enforce
  buffer `byteLength`, and bound source reads. Move referenced files beside
  the glTF (or into its subdirectories), replace symlinks with files, and
  re-export buffers whose lengths differ; otherwise no game-code changes.
- `sgl-3d` glTF import: deep node hierarchies could overflow the stack
  and abort; they now load at any depth within the asset load budget.
  No game-code changes needed.
- `sgl-3d` glTF import: sparse accessors with unordered, repeated or
  out-of-range indices previously loaded with missing substitutions; they
  now fail the load. Re-export valid glTF; no game-code changes needed.


- `sgl-core` `FrameAnimation::tick` and `AnimationSequence::tick`: negative
  `dt` accumulated hidden time debt and zero `dt` could advance sequence
  steps; both now ignore nonpositive `dt`. Use positive elapsed time to enter
  sequence steps; otherwise no game-code changes needed.
- `sgl-3d` `ColorGrading::global.hue`: negative rotations could produce
  incorrect colours; they now wrap correctly every 2π, including below −π.
  No game-code changes needed.
- `sgl-3d` `AutoExposure`: near-black pixels previously lowered the average;
  they now only position its percentile window, so scenes with large dark
  areas expose darker. No game-code changes needed.
- `sgl-3d` world-space reflections: primary LOD surfaces could reflect or
  block themselves against full-detail ray geometry; primary raster now retains
  full detail while effective world-space reflections run. Turning them off
  restores primary LOD selection. No game-code changes are needed.
- `sgl-3d` `Scene::update_fog_volumes`, `Scene::update_mist` and
  `Scene::update_effects`: oversized submissions now return
  `SceneError::DeviceLimit` and retain the previous content instead of
  failing GPU validation. `update_mist` and `update_effects` now return
  `Result<(), SceneError>`; handle or propagate their result (`?`).

- `sgl-post-fx` `ScreenSpaceReflection` (and `sgl-3d`'s Crystal SSR): rays
  near the bottom or right edge of a viewport whose size is not a multiple
  of 64 skipped surfaces there (SGL3D showed probes or sky instead); they
  now find them. No game-code change needed.
- `sgl-3d` `BakedSpecularProbe::proxy`: a proxy box reaching more than
  about 1e19 m from its capture centre made box projection sample an
  arbitrary texel; it now samples toward the ray's hit on the proxy, which
  acts as distant at that size. No game-code changes.
- `sgl-3d` glTF import: a file with many morph-weight channels on a mesh
  of many primitives took time quadratic in its size to load; it no
  longer does. No game-code change needed.
- `sgl-3d` glTF import: a small file whose morph-weight keyframes or
  instanced meshes declared gigabytes of data could abort the process; a
  scene listing a root twice or a child as a root now fails the load, as
  does one whose meshes (each instance counted), skins and clips would
  take more than 4 GiB natively or 1 GiB in the browser. No game-code
  change needed.
- `sgl-2d` `Renderer::read_scene`, `Renderer::capture_scene` and
  `CaptureError`, and `sgl-3d` `diagnostics::read`: a browser build compiled
  them and hung the tab when called; they are now native-only, a compile
  error on `wasm32`. No game-code changes for native games; a browser game
  drops those calls.
- `sgl-3d` glTF import, `deformation::MorphDelta::tangent`: a morphed
  vertex whose `TANGENT` lies along its normal (loaded as absent) got a
  non-finite delta, so `Scene::add_asset` refused the file with
  `SceneError::InvalidDeformation`, or, a few ulps off, a huge one; its
  delta is now zero. No game-code change needed.
- `sgl-3d` `PreparedModel::new` (and `Scene::add_asset`): a vertex with a
  non-finite `uv` or `lightmap_uv`, or a mesh whose UVs span more than `f32`
  holds, loaded, an infinite UV collapsing its whole mesh's UVs onto one
  texel; the mesh is now refused with `SceneError::NonFiniteGeometry`. No
  game-code change needed.
- `sgl-net` UDP client: a deny that overtook the accept of a retried confirm
  (or whose accept was lost) left the server holding that connection until
  `timeout_ms`; the client now answers the deny with a disconnect, sent
  three times as every UDP disconnect is, so the server reports
  `Disconnected { reason: Peer }` right after `Connected`. No game-code
  change needed.
- `sgl-net` UDP endpoint: a peer closed for `ProtocolViolation`,
  `InboundOverflow` or a stalled send (`TimedOut`) was sent one disconnect,
  so losing it left the remote holding the connection until `timeout_ms`;
  that disconnect is now sent three times, as a caller's is. No game-code
  change needed.
- `sgl-3d` `Scene::add_decal`/`set_decal`: a decal atlas too large for the
  CPU to build (one 8192-square image in the browser) panicked when the
  next frame packed it, and an image wider than the device's textures could
  stall or overflow packing; both are now refused with `SceneError::DeviceLimit`
  when the decal is added, at most 2 GiB of atlas in the browser and 8 GiB
  natively. In the browser, keep the decal images in use within that
  ([Decals](crates/sgl-3d/README.md#decals)); no other game-code changes.
- `sgl-3d` orthographic `Camera` shading and `SurfaceContext::view`: the
  view vector pointed from each surface toward `Camera::eye`, so highlights
  and Fresnel varied across a flat surface and vanished on one between the
  eye and a near plane behind it; it is now the camera's backward axis at
  every surface. No game-code changes.
- `sgl-3d` deformation (`Scene::set_instance_deformation`): a deforming mesh
  above 4,194,240 vertices failed device validation every frame it deformed;
  it now deforms. No game-code changes needed.
- `sgl-net` native WebSocket keepalive: an unsolicited 8-byte pong arriving
  after the reply to this side's ping, before the next `poll` or `flush`,
  discarded the reply, so this side never pinged again and its `rtt` stopped
  updating; the reply now counts. No game-code changes needed.
- `sgl-2d` `UiFrame::dropdown`: for one frame after a selection,
  click-away, Escape, `Ui::close_popups` or `Ui::cancel_interactions` closed
  the popover, widgets under where it was could not hover or take a press;
  they now respond at once. No game-code change needed.
- `sgl-3d` volumetric fog (`FrameInput::fog`), ray-traced shadows
  (`Settings::ray_traced_shadows`) and FSR2 (`Antialiasing::Fsr2`): a reset
  (`FrameInput::camera_cut`, a resize or another scene) in a frame where the
  feature did not run could leave its history from before the reset in use
  when it resumed; any reset now ends it. No game-code change needed.
- `sgl-net` WebSocket transports (`NativeWebSocketServer`,
  `NativeWebSocketClient`, `BrowserWebSocketClient`): a peer closed for
  `ProtocolViolation` or `InboundOverflow` discarded the messages received
  before the offending frame; they are now delivered before its
  `Disconnected`. No game-code change needed.
- `sgl-3d` `Scene::set_instance_deformation`: a joint matrix scaling an axis
  to zero gave its skinned vertices NaN normals, and a mirrored skin turned
  them against its deformed faces; skinned normals now face the deformed
  winding, a flattened surface takes its flattened normal, and a normal the
  skin collapses keeps its unskinned direction. No game-code change needed.
- `sgl-post-fx` `TemporalAntiAliasing::prepare_resources`: the frame after a
  resize resolved against the zeroed, recreated history instead of resetting
  accumulation, which shifted bright HDR texels; it now takes the current
  colour, as after `reset_accumulation`. No game-code changes needed.
- `sgl-3d` glTF import and `asset::Vertex::tangent`: a tangent a few f32
  ulps off its normal counted as a frame, its rounding normalised into an
  arbitrary tangent; a tangent within 0.02° of its normal is now along it:
  absent on a material without anisotropy, refused on an anisotropic one
  (import error, or `SceneError::MissingAnisotropyTangents`). No game-code
  change needed.
- `sgl-3d` `Scene::set_baked_specular_probes`: when the probes' influence
  boxes together spanned more than about 7e12 m on each axis, probe captures
  and world-space ray hits found none of them; they now do. A box whose `max - min` overflows f32
  (e.g. `-f32::MAX..f32::MAX`) is now refused with
  `ProbeError::InvalidProbe`; give such a box a smaller, finite extent.
- `sgl-post-fx` `ScreenSpaceReflection` and
  `ScreenSpaceReflectionAttribs::most_detailed_mip`: a zero world normal
  gave its ray a NaN direction, a non-unit one a skewed ray, and a
  `most_detailed_mip` past the depth hierarchy's top level started rays at
  a level that does not exist; a normal whose squared length is more than
  0.1 from 1 now reports a miss, and `most_detailed_mip` counts as the top
  level at most. A game feeding SSR directly writes unit world normals;
  SGL3D games need no change.
- `sgl-3d` `AutoExposure::speed_brighten`/`speed_darken`: a NaN speed made
  the adapted exposure NaN until a history reset, a negative one moved it
  away from its target, an infinite one adapted instantly, and a speed of 0
  over an infinite `FrameInput::frame_time_ms` moved it or made it NaN; a
  nonfinite or negative speed now takes the default (3 and 1), and 0 holds.
  For instant adaptation, use `f32::MAX` instead of `f32::INFINITY`.
- `sgl-2d` `SpritePass::upload_normal` and `Renderer::upload_normal_map`: a
  second normal map under a different handle for a diffuse was ignored
  (returning `Ok`, even when mis-sized); it now replaces the first, and a
  mis-sized one returns `TextureError::SizeMismatch`. A call before the
  diffuse's upload, previously a debug panic or silent no-op, returns the
  new `TextureError::DiffuseNotUploaded`. Upload the diffuse first; an
  exhaustive `match` on `TextureError` adds the variant.
- `sgl-input` `Gamepads` on macOS: dropping an owner cleared every
  controller callback, including those a newer owner had installed, so the
  newer one stopped receiving input; it now clears only callbacks it still
  holds. No game-code change needed.
- `sgl-post-fx` `ScreenSpaceReflection` (and `sgl-3d`'s SSR): pixels that
  left the reflection mask kept their last radiance history, which the next
  frames' bilinear reprojection blended back in as faint ghosting at mask
  edges; they now hold no history (zero radiance, variance 1). No game-code
  change needed.
- `sgl-post-fx` `ScreenSpaceReflectionAttribs::temporal_radiance_stability_factor`
  and `ScreenSpaceReflection::execute`: the default was 1.0, and a frame
  whose history was reset, or recreated by a resize or half-resolution
  change, mixed in the zeroed history, so reflections stayed dark; the
  default is now 0.95 and such a frame takes the current frame's radiance.
  A game that relied on 1.0 sets it
  explicitly; otherwise no game-code change needed (SGL3D already used 0.95).
- `sgl-3d` `geometry::obstructed_distance`: its absolute determinant cutoff
  missed every hit on a triangle with sub-millimetre edges and grazing hits
  on centimetre ones; the cutoff now scales with the triangle's edges, so
  small triangles are hit like large ones. No game-code change needed.
- `sgl-3d` glTF import and `Ignored::Attribute` (new): a `TEXCOORD_2`+,
  `COLOR_1`+ or `_NAME` attribute, or a `TANGENT` along its normal on a
  material without anisotropy, failed the load; the attribute is now left
  out and listed in `Asset::ignored`, and the tangent is absent (zero). A
  game matching `Ignored` exhaustively adds an arm for `Attribute`;
  otherwise no game-code change needed.
- `sgl-3d` glTF import: a sparse accessor without a buffer view declaring a
  huge count aborted the process; the load now fails with an error naming
  it when an accessor's count differs from what glTF requires (POSITION's
  for attributes and morph targets, the joints' for inverse binds, the
  keyframes' for values) or when a sparse accessor without a buffer view
  that nothing else counts declares more elements than the file's buffers
  would hold. No game-code change needed.
- `sgl-3d` point and spot lights: a light was dropped wherever the mapped
  base normal faced away from its centre, so a clearcoat over normal-mapped
  flakes lost the coat's highlight and a sphere light's highlight popped off
  as its centre crossed the horizon; it now reaches a surface while the
  base or the coat normal faces any of its sphere, as directional lights
  already did, in lighting and ray-traced shadows alike; the directional
  light's ray-traced shadow now traces its whole disc too. No game-code
- `sgl-post-fx` `ScreenSpaceReflection` (and `sgl-3d`'s SSR): a ray that left
  past the far plane, such as a floor reflecting the sky under infinite
  reversed-Z, stored a length beyond RGBA16F's range, becoming infinity and
  NaN in the denoiser or a wrong direction where the backend saturates, or a
  short meaningless one where its last step overshot the plane; it is now
  stored 65504 long along its direction. No game-code change needed.
- `sgl-3d` anisotropic materials: a tangent that skinning or morphing
  cancelled or turned along its normal, or a ray hit whose vertex tangents
  all vanished, shaded NaN; it now takes an arbitrary tangent in the
  normal's plane, keeping its handedness (+1 where it has none), as a vertex
  without a tangent does. No game-code change needed.
- `sgl-2d` `fetch_runtime_bundle` (wasm): listed assets were fetched from
  `assets/{path}` relative to the page, whatever the manifest URL; they are
  now fetched relative to the manifest's directory. Serve the manifest from
  the asset root, e.g. `fetch_runtime_bundle("assets/manifest.txt")`.
- `sgl-3d` `Scene::set_baked_specular_probes` and `Scene::move_origin`: a
  tiny probe influence box far from the render origin overflowed the probe
  grid's cell count, panicking with overflow checks or else building a
  wrongly sized grid; it now builds within budget. No game-code change
  needed.
- `sgl-3d` `Scene::set_instance_deformation`: on a device created with a
  uniform offset alignment of 32 bytes (an adapter's real limits), a frame with two or more deforming meshes failed
  wgpu validation; the deform stage's records are now aligned. No game-code
  change needed.
- `sgl-input` `Gamepads` on macOS: replacing a live owner in place
  (`pads = Gamepads::new()?`) turned background event delivery off, losing
  releases while unfocused; it now stays on until the last owner drops. No
- `sgl-2d` `Ui::begin` / `UiFrame::end`: a release and a new press in one
  frame (button still down) dropped the press's held button, scrollbar,
  number-field or splitter drag; they now count as release then press, so the
  earlier drag ends and the second press holds, scrubs or resizes. No
  game-code change needed.
- `sgl-input` `Gamepads::poll`: a button pressed while the macOS handler was
  being installed could yield two `ButtonPressed` events and one release; a
  press of a button already held is now dropped on every target. No
  game-code change needed.
- `sgl-2d` `UiFrame::scroll_area_begin`: content was clipped to the whole
  area, so a press on the scrollbar thumb also pressed the row under it; while
  the scrollbar shows, content is now clipped `Ui::SCROLLBAR_WIDTH` (new, 8 px)
  short of the right edge. Keep row hit areas, actions and nested scroll
  areas within `rect.max.x - Ui::SCROLLBAR_WIDTH`.
- `sgl-3d` glTF import: an animation sampler with two keyframes at the same
  time loaded; it now fails the load, as glTF requires strictly increasing
  times. Re-export the file with distinct keyframe times; no game-code
  change needed.
- `sgl-net` UDP client: a `ConnectDeny` arriving after the accept (reordered)
  or naming a cookie the client was not confirming removed the connection and
  reported `Denied` after `Connected`; it is now ignored, and only a deny for
  the confirm the client sent refuses it. No game-code change needed.
- `sgl-net` `MemoryServerIo::send`: after either end disconnected it returned
  `SendError::Disconnected`; it now returns `UnknownConnection`, like every
  other server transport. A game that matched `Disconnected` from a server
  send should match `UnknownConnection`; otherwise no game-code change needed.
- `sgl-post-fx` `TemporalAntiAliasing::execute`: the first frame for a
  feature set sampled the colour through a filterable binding, failing
  validation for an unfilterable colour such as `Rgba32Float` on WebGPU, and
  copied +infinity to the output; it now loads the colour as later frames do,
  clamped to [0, 65504], so negative colour and alpha become 0 on that frame.
  No game-code change needed.
- `sgl-3d` local-light shadows: a shadowed light with a large range at or
  behind the camera overflowed its atlas slot size, panicking with overflow
  checks or else taking the smallest slot; it now takes the largest slot.
  No game-code change needed.
- `sgl-2d` `UiFrame::dropdown`: on the first frame an opened popover was drawn,
  nothing shielded the widgets under it, so they could hover or fire with a
  row; the popover now blocks them from that frame. No game-code change
  needed.
- `sgl-core` `random::Rng::usize`: on wasm32 it reduced only 32 bits of the
  draw, giving different values than native for the same seed; it now gives
  native's values on every target. Browser builds that persisted values
  derived from it see a different sequence; no game-code change needed.
- `sgl-3d` `Scene::update_effects`: glow in fog added the fog's in-scattered
  light again on top of the background, so even a black glow brightened the
  fog; glow now adds only its own light dimmed by the fog in front of it, so
  glow in fog looks dimmer. No game-code change needed.
- `sgl-post-fx` `TemporalAntiAliasing` and `ScreenSpaceReflection`, and
  `sgl-3d` Velvet and world-space reflections: a colour texel of +infinity
  (radiance past RGBA16F's range on backends that round overflow to
  infinity) tone-mapped to NaN, which spread through the frame; it now reads
  as 65504, binary16's largest finite value, and their outputs stay finite.
  No game-code change needed.
- `sgl-2d` `UiFrame::number_field`: pointer movement arriving in the same
  frame as the release was dropped, so a quick drag could open typing; it
  now scrubs the value before deciding between drag and click. No game-code
  changes needed.
- `sgl-2d` `UiFrame::number_field`: a press and release arriving in one
  frame dropped the typing focus the click had just opened; it now enters
  typing like a two-frame click. No game-code changes needed.
- `sgl-3d` `Settings::screen_space_reflections`: on Metal (and Vulkan without
  robust access) the screen's edge columns and rows could reconstruct a wrong
  surface normal for their rays, offsetting and bouncing them; edge pixels now
  take the same normal as the interior on every backend. No game-code change
  needed.
- `sgl-net` `memory_duplex`: flushed unreliable messages the peer had not
  polled counted against the sender's unreliable bounds, so `send` returned
  `WouldBlock` and the peer later got the oldest; now only unflushed ones
  count and an unpolled peer drops its oldest past the lane's bounds, as on
  WebSocket and UDP. No game-code change needed.
- `sgl-2d` `UiFrame::scroll_area_end`: one wheel tick scrolled every nested
  scroll area under the pointer; it now scrolls only the innermost hovered area
  with content to scroll, applied at its end (so it shows from the next frame,
  not the same one), and `scroll_area_end` takes the area's offset. Pass the
  offset given to its `scroll_area_begin`:
  `frame.scroll_area_end(&mut offset);`.
- `sgl-2d` `AsepriteSheet::parse`: a one-frame Hash export, whose key
  Aseprite writes without a frame number (`"player.aseprite"`), failed with
  `FrameKey` or `MissingFrameIndex`; a single-entry Hash now loads as frame
  0. No game-code change needed.
- `sgl-net` UDP transport: a first round trip measured as 0 ms (loopback,
  sub-millisecond LAN) was treated as unmeasured, keeping the 200 ms
  retransmission timeout; it now seeds the estimate, giving the 50 ms floor,
  so retransmission timing changes on very fast links. No game-code change
  needed.
- `sgl-3d` dynamic GI (`DynamicGiVolume::spacing`): past a spacing of about
  170 m the probes' depth moments overflowed half precision, breaking the
  sample's visibility test (NaN irradiance on some backends); they are now
  stored in units of the longest spacing, so any spacing works. No game-code
  change needed.
- `sgl-3d` orthographic cameras: with the near plane behind the camera
  (a negative near), lights behind the camera were dropped from the
  clusters and surfaces between the near plane and the camera got no
  directional shadow; both now reach the near plane. No game-code change
  needed.
- `sgl-net` native WebSocket `Disconnected` reason: a peer that dropped
  the TCP stream without a Close, or sent a frame breaking WebSocket
  framing (reserved bits, an unmasked client frame), was reported as
  `Transport`; now `Peer` and `ProtocolViolation` respectively. No
  game-code change needed unless the game branches on `Transport`.
- `sgl-3d` `Renderer::capture_specular_probe`: local-light shadows of static
  casters whose shader reads the time were drawn at the last frame's time,
  and the next frame at the capture's time reused that stale layer; they
  are now drawn at the capture's `elapsed_seconds`. No game-code change
  needed.
- `sgl-2d`, `sgl-3d`, `sgl-post-fx`, `sgl-input`: required `bytemuck`
  `=1.25.2`, `winit` `=0.30.13` and (native `sgl-2d`) `pollster` `=0.4.0`
  exactly, so Cargo could not resolve beside a crate needing a newer patch;
  now caret ranges (`^1.25.2`, `^0.30.13`, `^0.4.0`). No game-code change
  needed; `cargo update` may pick newer compatible releases.
- `sgl-3d` `Scene::add_shader`: a declaration of a built-in's or SGL3D's
  name after a line comment ended by `\r` or another non-`\n` line break,
  or separated by U+200E/U+200F, was accepted; it is now refused with
  `ShaderError::NameTaken`, as the same declaration on a new line was. No
  game-code changes needed unless a shader relied on it: rename the helper.
- `sgl-3d` `Settings::fog_quality`: the fog's froxel volume grew without
  bound as the frame narrowed, so a tall portrait frame (60×1900 at High)
  panicked creating it, and its froxels were wider than tall; it now holds
  the quality's side across the geometric mean of the frame's sides in
  square froxels, each side within the device's 3D texture limit (at 16:9
  High 170×96 where it was 163×100, Low 85×48 where it was 81×50). No
- `sgl-3d` `Scene::add_shader`: a `break if` loop whose counter starts past
  its limit, so its first step wraps (`var i=4294967295u; … i+=1u; break if
  i>4294967000u;`), was accepted as one iteration; it is now refused with
  `ShaderError::UnboundedLoop`, as start plus step must fit the counter's
  type. Games with such a loop: start the counter at or below its limit.
- `sgl-net` `NativeWebSocketClient::connect`: a `wss://` URL connected in
  plaintext; now any scheme but `ws` fails with `InvalidInput` before
  connecting. Games passing `wss://` to the native client: use `ws://` and
  terminate TLS at the deployment edge.
- `sgl-core` `sweep_aabb` (and `move_and_collide`, `snap_to_ground`): a body
  exactly touching a collider at coordinates that round could miss it and
  move straight through; now a body exactly touching a collider contacts it
  at `t = 0` when moving toward it. No game-code change needed.
- `sgl-3d` `HeatDistortion::displacement`: under FSR2 it shifted the frame
  by render pixels, larger than documented; it now shifts by the scene
  pixels it documents at every antialiasing setting, so heat under FSR2
  looks smaller than before. No game-code change needed.
- `sgl-net` UDP server endpoints: after 64 other joins within a departed
  client's cookie lifetime, a late duplicate of its confirm reported
  `Connected` for a client that was gone; each verified confirm is now kept
  until its cookie expires, so the duplicate is dropped. No game-code change
  needed.
- `sgl-3d` `Settings::screen_space_reflections` (Velvet) and
  `Settings::world_space_reflections`: within half a texel of the top or
  left edge, the history fallback took colour and depth from one texel
  inward, and the world-ray resolve reused an edge pixel for a neighbour off
  the grid; the fallback now reads the edge texel and the resolve skips the
  neighbour. No game-code changes needed.
- `sgl-post-fx` `post_fx_context::FeatureFlags::HALF_PRECISION_DEPTH`:
  toggling it at an unchanged size kept the old depth format and panicked at
  the next resize; `PostFXContext::prepare_resources` now recreates the
  depths when the flag changes. The flag needs a device with
  `TEXTURE_FORMAT_16BIT_NORM` and `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`
  whose adapter renders to R16Unorm. No game-code change needed.
- `sgl-net` UDP endpoints: a handshake whose confirms or accept were lost
  until the cookie expired (5 to 10 s) never completed; the server, even
  after `stop_admission`, now accepts a request or confirm for a connection
  it holds at any cookie age, and the client requests again after four
  unanswered confirms, takes the cookie the accept names, and takes a
  payload carrying its nonces as the accept. No game-code change needed.
- `sgl-3d` `Settings::world_space_reflections`: on rare frames each world
  ray traced the next 2×2 block's pixel (the last outside the screen), and
  at a render 1 pixel wide or tall it read past the edge; each now traces a
  pixel of its own block inside the screen. No game-code changes needed.
- `sgl-net` `ThreadedUdpServer`: a connection that ended at the worker (peer
  close, timeout, protocol violation) discarded the messages it had
  delivered but the caller had not yet polled and reported `Disconnected`
  first; polls now return those messages, then `Disconnected`. No game-code
  change needed.
- `sgl-3d` `Settings::world_space_reflections`: a receiver in the
  screen-space method's roughness fade that the method saw with full
  confidence traced no world ray, leaving its faded share to probes and sky;
  it now traces one, as composition gives that share to world rays. No
  game-code changes needed.
- `sgl-2d` `UiFrame::dropdown`: an open dropdown scrolled fully out of its
  clip stayed open and kept the keyboard captured; it now closes at
  `UiFrame::end`. No game-code changes needed.
- `sgl-3d` `Scene::add_materials`, `add_asset` and `set_material`: a
  metallic, roughness, clearcoat, clearcoat roughness or base alpha outside
  0..=1, or a base colour, emission or `environment_scale` that is negative
  or not finite, was accepted and reached the GPU; it is now refused with
  new `SceneError::InvalidMaterialFactors`. Migration: keep those values in
  range; exhaustive matches on `SceneError` add the variant.
- `sgl-3d` `asset::load*`: a KHR material extension object (anisotropy,
  clearcoat, iridescence, sheen, diffuse transmission, transmission, volume,
  dispersion) carrying its own `extensions` property was refused as
  unsupported; it now loads, as glTF's `glTFProperty` allows. No game-code
  change needed.
- `sgl-2d` `Renderer::set_target_size`: a size wider or taller than the
  device's maximum texture dimension clamped only that axis, distorting the
  aspect, letterboxing the frame and shrinking `ui_size()`; both axes now
  scale together, so the frame fills the window and `ui_size()` stays the
  window's logical size. No game-code change needed.
- `sgl-net` `BrowserWebSocketClient`: a reconnect attempt whose `WebSocket`
  constructor threw (a `SecurityError`, say) reported no `Disconnected`, so
  a game waiting after `Reconnecting` waited forever; each such attempt now
  reports `Disconnected { Transport }` like any other failed attempt. No
  game-code change needed.
- `sgl-2d` `SpritePass::prepare`: a frame with more instances than the
  device's `max_buffer_size` holds (about 3.3 million at the 256 MiB
  default) grew the instance buffer past it, a wgpu validation error; it now
  draws the first that many (world, then screen) and drops the rest, which
  `draw_stats` reflects. No game-code change needed.
- `sgl-3d` `asset::load*`: a node transform with an absolute determinant
  below 1e-10 was refused as singular, so a uniform scale at or below about
  4.6e-4 failed to load; singularity is now judged relative to the node's
  axis lengths, so small scales load and only a collapsed axis is refused.
  No game-code change needed.
- `sgl-2d` `TextRenderer::draw`: glyphs shared a cache slot across a
  quarter-pixel size range, so text laid out by whichever size drew first
  and drifted from `measure`; glyphs are now cached per exact size and
  layout matches a cold cache and `measure`. No game-code change needed.
- `sgl-3d` `asset::load*`: an `EXT_materials_bump` map without
  `bumpFactor` loaded with `bump_scale` 0 (no effect) and a factor above 1
  was refused; now the factor defaults to 1 and loads in `0..=100`, as the
  extension's schema sets. No game-code change needed; bump maps that had no
  effect now show.
- `sgl-3d` `asset::load*`: every glTF image was decoded (and asked of
  `LoadOptions::images`), so one only an unsupported texture extension
  (`KHR_texture_basisu`) or an ignored map used could fail the load; now
  only images a sampled material map uses are asked for and decoded, and
  each other is a one-texel placeholder listed as the new
  `Ignored::Image { image }`. No game-code change needed; exhaustive
  matches on `Ignored` add the arm.
- `sgl-net` `NativeWebSocketClient::connect`: tried only the host's first
  resolved address (so `localhost` could be refused when `::1` came first),
  host resolution was unbounded, and the TCP connect and HTTP upgrade each
  took a full `handshake_timeout`; now each resolved address is tried in
  order within one `handshake_timeout` covering resolution and the upgrade
  too. No game-code change needed.
- `sgl-2d` `Overlay::circle`: a negative radius drew a full circle; a
  radius that is not positive (including NaN) now emits nothing. No
  game-code change needed.
- `sgl-core` `Grid2::filled`: returned `Self` and panicked when the cells
  could not be allocated (the largest byte grid on wasm32); now returns
  `Result<Self, GridError>` with the new `GridError::TooLarge`. Add `?` or
  `.expect("grid fits")`: `Grid2::filled(w, h, 0u8)?`; exhaustive matches
  on `GridError` add the `TooLarge` arm.
- `sgl-2d` `TextRenderer::draw` / `measure`: a negative size panicked in
  `draw` and measured negative in `measure`; a size that is not positive
  and finite now draws nothing and measures `Vec2::ZERO`. No game-code
  change needed.
- `sgl-net` `ClientIo::disconnect` / `ServerIo::disconnect`: caller-polled
  UDP and memory reported nothing for a connection the caller closed (UDP
  sometimes `Disconnected { Peer }`), threaded UDP reported `Local` or,
  when its endpoint found the peer gone first, that end, and WebSocket
  `Local` or `Peer` by timing; now a connection still open at `disconnect`
  reports exactly one `Disconnected { reason: Local }` on every transport
  (a UDP client in its handshake too, with no `Connected`), and one
  already ended reports that end; `ServerIoMux` forwards either, where it
  dropped them. Games that clean up when they call
  `disconnect`: ignore that event, or move the cleanup to it.
- `sgl-3d` `Light::specular`: a diffuse-only light (0) dimmed a clearcoated
  base by the coat's Fresnel, and a weight above 1 dimmed a sheened base
  further; the coat's and sheen's dimming now follow
  `specular` up to 1, and dynamic GI probe hits take no coat. No game-code
  changes needed.
- `sgl-3d` local-light shadows: a material shader's parameters
  (`Scene::set_shader_parameters`), the frame's time and a moving instance's
  `Scene::set_instance_shader_data` left cached shadows stale; casters whose
  shader moves or cuts them now redraw where those change, every frame the
  time advances for a shader that reads `time` or `phase`. No game-code
  changes needed.
- `sgl-net` UDP client handshake: a client confirmed the last challenge it
  received, so challenges to its retried request that arrived reversed
  across a cookie epoch left the join failing at the timeout after a ghost
  `Connected` on the server; it now confirms the first. No game-code
  changes needed.
- `sgl-net` `DatagramTransport::receive`: returned `Option`, so any socket
  error ended the endpoint's poll as if the socket were empty (on Windows an
  oversized datagram or ICMP error starved every other peer); it now
  returns `io::Result<Option<(usize, SocketAddr)>>`, `Ok(None)` when empty
  and `Err` for a failed receive the endpoint skips. Custom transports:
  return `Ok(None)` for `WouldBlock`, `Err` for other errors, and wrap a
  datagram in `Ok(Some(..))`.
- `sgl-3d` `Scene::add_shader`: loops that could run for ever were accepted
  as counted (a test or step read from a `let` computed before the loop, or
  a `break if` loop re-entered by an outer loop without restarting its
  counter); they are now refused with `ShaderError::UnboundedLoop`. The test
  and the step must read the counter in the loop's own test and update
  statements (where a `for` loop puts them); a value computed anywhere else
  is refused, including a `break if` test read before the step (previously
  accepted and miscounted). Set a nested `break if` loop's counter just
  before it.
- `sgl-3d` `Renderer::new` `output_format`: a non-sRGB 8-bit output (a
  browser canvas's `Bgra8Unorm` or `Rgba8Unorm`) took linear colour and
  looked dark; the tone map now writes it sRGB-encoded, a float output too
  in the browser. sRGB outputs and native float outputs are unchanged. No
  game-code changes needed.
- `sgl-3d` `Scene::add_shader`: a module declaring a WGSL built-in's name
  (`fn smoothstep`, `fn saturate`, a predeclared type or enumerant) was
  accepted and replaced it in SGL3D's own calls; it is now refused with
  `ShaderError::NameTaken`. Migration: rename such helpers (for example
  `my_smoothstep`).
- `sgl-2d` `TextRenderer::draw`: a glyph larger than a `GLYPH_PAGE_SIZE`
  page (a large size at a high pixel scale) panicked; it now gets a page of
  its own sized to it, published by `end_frame` like any page, and one past
  `canvas::text::MAX_GLYPH_PAGE_SIZE` (8192) is not drawn.
  A page past the device's texture limit fails its upload with
  `TextureError::TooLarge`: log that error rather than unwrapping the
  upload.
- `sgl-3d` shader contract `MaterialVertex::tangent`: documented as all
  zero where a mesh has no tangents, but such a vertex always received an
  arbitrary unit tangent in the normal's plane with handedness +1; the
  contract now says so. Behaviour is unchanged. A game shader that tested
  `tangent.w == 0.` to fall back never took that branch: where it is used
  on meshes without authored tangents, derive the frame in
  `material_surface` (for example from screen derivatives), or flag those
  meshes through `ShaderParams` or shader data.
- `sgl-3d` `Scene`: the draw candidate, set and level-of-detail chain
  buffers doubled past the device's storage binding limit once they held
  over half of it, failing the frame's cull bind group; their growth now
  stops at the limit. No game-code changes needed.
- `sgl-2d` light shadows (`shadow_triangles`, `LightPass`): a light close to
  a long occluder edge lit part of the area behind it inside its footprint
  (the wall-torch case); such edges now get a far cap that covers the
  footprint. No game-code changes needed.
- `sgl-2d` `TextRenderer`: glyph pages were never reused, so text whose size
  changed every frame opened pages without bound, and an infinite outline or
  shadow width hung `draw`; a full atlas now empties and reuses its least
  recently used page (same handle, republished by `end_frame`), and ring
  widths are capped at `canvas::text::MAX_RING_WIDTH` (64 px), non-finite
  ones drawing no ring. Glyph instances from `end_frame` are valid for that
  frame only; call it once per presented frame. No game-code changes needed
  for games that already do.
- `sgl-input` `Gamepads::poll` on Windows, Linux and the web (Gilrs): a
  repeated `Connected` for a pad reset its held state and is now ignored, and
  a `ButtonReleased` with no reported press (a button held when the pad
  connected) is now dropped on every target. Input already held at connection
  is still not reported until it changes, a gilrs limitation. No game-code
  changes needed.
- `sgl-post-fx` half-resolution SSR (SGL3D's `ScreenSpaceReflections::Half`):
  a one-pixel-wide or -tall frame created zero-sized textures, a wgpu
  validation error; each half-resolution side is now at least one texel. No
  game-code changes needed.
- `sgl-2d` `Renderer` screen channel: when `set_target_size` scaled a very
  large target down (over 4096² px), the UI laid out over the smaller
  target, too large and misaligned with the pointer; it now keeps the
  requested size over `ui_scale`. Lay out over the new `Renderer::ui_size()`
  instead of `target_size() / ui_scale`, and set the text raster scale to the
  new `Renderer::ui_pixel_scale()`.
- `sgl-3d` Velvet and world-space reflections: a frame whose camera changed
  its near plane read the reflection depth history with the new near plane
  and discarded the history; it is now read with the near plane that wrote
  it. No game-code changes needed.
- `sgl-2d` `AseDirection`: `pingpong_reverse` tags parsed as `Other` and
  played forward; they are now `AseDirection::PingpongReverse`, whose
  `AseTag::frame_order` runs down and back up without repeating either end.
  Add the variant to exhaustive matches.
- `sgl-3d` `asset::load*`: a mesh with `TEXCOORD_1`, or a texture SGL3D does
  not sample (such as an ignored occlusion map) with nearest or other
  non-trilinear sampling, failed the load; `TEXCOORD_1` now loads as
  `Vertex::lightmap_uv`, the mesh's static irradiance atlas chart, and only
  sampled maps' sampling is checked. On a static instance while an atlas is
  installed, a charted vertex samples the atlas and takes no baked lights:
  set `lightmap_uv` to `[0, 0]` on models the bake does not chart (a second
  UV map exported for AO, say). Otherwise no game-code changes needed.
- `sgl-2d` `UiFrame::dropdown`: an open dropdown that stopped being
  submitted kept its popup open and the keyboard captured, blocking Tab and
  Enter elsewhere; it now closes at `UiFrame::end`. No game-code changes
  needed.
- `sgl-2d` `AsepriteSheet::parse` / `load`: a frame rect reaching past
  `meta.size` was accepted and sampled neighbouring atlas pixels; it is now
  `AsepriteError::FrameOutsideSheet { index, frame, sheet_size }`. Add the
  variant to exhaustive matches; re-export sheets whose frames overrun.
- `sgl-post-fx` `PostFXContext`: the blue-noise draw's vertex range wrapped
  at frame indices 1,431,655,765 and 2,863,311,530 (a debug-build panic, a
  skipped update in release), and its R2 noise lost precision from about
  87,000 frames and was constant beyond about 11 million; the frame index now
  cycles every 256 frames. No game-code changes needed.
- `sgl-3d` SMAA (`Antialiasing::Smaa`, and where it stands in for TAA): it
  ran on the unexposed HDR scene, so the exposure changed which edges it
  found; it now runs after tone mapping, on display colour, before the
  resample to the output. No game-code changes needed.
- `sgl-net` UDP `stop_admission`: a stopped server ignored the handshake
  confirm of a client it had already accepted, so a lost accept left that
  client unconnected until both timed out; it now answers connections it
  has and refuses only new ones. No game-code changes needed.
- `sgl-net` UDP: an unacknowledged reliable fragment was resent every
  round-trip timeout and closed the peer `TimedOut` after
  `EndpointConfig::max_reliable_transmissions` sends (about 600 ms on a
  LAN); resends now back off up to 1 s, and a peer is closed `TimedOut`
  only after `timeout_ms` of silence, or 2 × (`timeout_ms` + 1 s) in which
  a lane it keeps answering on acknowledges nothing. Migration: delete any
  `max_reliable_transmissions` field from `EndpointConfig` literals; set
  `timeout_ms` for how long a stalled peer may last.
- `sgl-input` `Gamepads::poll` on macOS: input queued before a controller
  was unplugged was dropped (a tap then unplug between polls lost the tap);
  it is now reported before the `Disconnected` event. No game-code changes
  needed.
- `sgl-3d` Velvet reflections (`ReflectionMethod::Velvet`): a ray that ran out
  of steps before confirming a hit was accepted by depth proximity and now
  reports a miss, so streaks near surfaces at the step cap give way to the
  fallback. No game-code changes needed.
- `sgl-net` `BrowserWebSocketClient`: a close caused by a received frame
  (`InboundOverflow`, or a `ProtocolViolation` found by the lane queues)
  never reconnected; it now follows `ReconnectPolicy` like other non-local
  closes, so expect `Reconnecting` after it. No game-code changes needed.
- `sgl-net` `ReconnectPolicy::max_attempts` (browser): every `Connected`
  restarted the count, so a connection that kept failing soon after it
  opened reconnected forever at the first delay; the count now restarts only
  after a connection stayed up at least `max_delay_ms`, so such a loop backs
  off and stops after `max_attempts`. No game-code changes needed.
- `sgl-core` `ColliderSet::insert` / `query`: a huge finite box walked every
  grid cell it spanned (effectively hanging); a collider over 1024 cells is
  now kept apart and a query over more cells than colliders scans the
  colliders. No game-code changes needed.
- `sgl-core` `AnimationSequence` ping-pong: a trailing step replayed the end
  frame; a leading step skipped the reverse pass or replayed the start frame.
  The bounce now turns on the first and last frame steps, playing outer
  steps once per turnaround. No game-code changes needed.
- `sgl-2d` `Overlay`: under a world-unit camera every fill, line and outline
  on the world channel shrank by `pixels_per_unit`; the new `units:
  WorldUnits` field (default logical pixels) makes positions, sizes and
  widths world units. World gizmos set `units: camera.units()`; struct
  literals without `..Overlay::new(white)` add `units`; pixel overlays need
  no other change.
- `sgl-3d` `asset::load*`: a glTF mesh whose morphed primitives have
  different numbers of morph targets loaded, the extra targets driven by
  another node's weights; it now fails the load naming the primitive.
  Primitives without targets still load unmorphed, and a mesh whose first
  primitive has none no longer refuses its morph-weight animation. No
  game-code changes needed; give every morphed primitive of the mesh the
  same shape keys.
- `sgl-net` `BrowserWebSocketConfig::latest_buffered_bytes`: any nonzero
  value was accepted, and one below the largest latest-state frame blocked
  every lane once a large state was sent; `BrowserWebSocketClient::connect`
  now rejects values below `ENVELOPE_HEADER_LEN + MAX_LATEST_STATE_BYTES`
  (1186). Raise a smaller watermark to at least that; the 64 KiB default is
  unaffected.
- `sgl-net` `udp::LANE_MESSAGES_PER_PEER_PER_POLL` (32, native) is now public:
  the most lane messages a `ThreadedUdpServer` poll returns per peer, which
  `specs/netcode.md` already named. No game-code changes needed.
- `sgl-2d` `UiFrame::dropdown` / `overlay_panel_begin`: a popover's or
  panel's clipped-away part still blocked widgets beneath it (and kept the
  popover open when pressed); only the visible part now blocks and counts as
  inside. No game-code changes needed.
- `sgl-post-fx` TAA: under conventional (not reversed) depth the closest-
  motion search took an out-of-screen neighbour as nearest, so the 1-pixel
  border read zero motion and ghosted while the camera moved; neighbours now
  clamp to the screen. No game-code changes needed.
- `sgl-post-fx` SSR and TAA temporal passes: the reprojected depth was
  linearised with the current projection, so a near or far plane that
  changed between frames rejected history for a frame; it now uses the
  previous camera's. No game-code changes needed.
- `sgl-2d` `UiFrame::scroll_area_begin` / `scroll_area_end`: the area
  replaced the enclosing clip and its end reset the clip to `None`; it now
  clips within the enclosing clip, nests, and restores the enclosing clip at
  its end. Drop any `set_clip` that only restored the outer clip after
  `scroll_area_end`.
- `sgl-3d` dynamic GI (`Settings::dynamic_gi`): a probe refused for want of
  the frame's ray budget kept its rays counted, refusing later probes that
  fit; it now gives them back, so the frame uses its budget. No game-code
  changes needed.
- `sgl-net` `BrowserWebSocketClient::disconnect`: during reconnect backoff
  it left the scheduled retry armed, so a later `poll` reconnected; it now
  cancels the retry. No game-code changes needed.
- `sgl-net` native WebSocket `disconnect`: a graceful close no longer drops
  frames still waiting on a blocked socket or its Close frame; it now ends,
  as `Local`, once the peer answers the Close or ends the stream, or at
  `GRACEFUL_CLOSE_TIMEOUT_MS`. No game-code changes needed.
- `sgl-core` `move_and_collide` / `snap_to_ground`: a body that started
  inside a solid collider could move through it; it may now move out or
  along it but not deeper, and a body within `skin` below a one-way
  platform's top lands on it. `CollisionConfig::new` now panics on a negative
  or non-finite `skin`, `snap_distance` or `block_epsilon`. No game-code
  changes needed unless a game passes such a value.
- `sgl-core` `AnimationSequence::tick`: in `Repeat`/`PingPongRepeat`, a
  zero-time pass stopped after as many advances as there are steps, so
  zero-duration frames delayed a following action and dropped the tick's
  time; it now stops after as many as it has frame and step positions. No
  game-code changes needed.
- `sgl-core` `AnimationSequence`: a ping-pong sequence of one step with one
  frame (or one pause or action) played it twice per bounce; it now plays it
  once, so a `PingPongOnce` completes after one frame duration. No game-code
  changes needed.
- `sgl-3d` `asset::load*`: a glTF node that is its own ancestor overflowed
  the stack at load or in `Rig::joint_matrices`, and a node with two parents
  loaded; both now fail the load with an error naming the node, and
  `Rig::joint_matrices` on a game-built rig with a parent cycle returns
  (wrong matrices for the cycle's joints) instead of overflowing. No
  game-code changes needed; re-export a file that now fails.
- `examples/direct-game` took `Renderer::white_texture`'s handle from a
  throwaway `Assets`, so it aliased the first texture of the game's own cache;
  it now keeps one `Assets<Texture>` and draws a second texture from it.
  Games that copied it: pass the game's texture cache to `white_texture`.
- `sgl-net` `Delivery::LatestState` on WebSocket: corrected the 0.4.0
  promise that a flushed state leaves ahead of lane frames flushed after it;
  it waits only for frames flushed with or before it, and lane frames
  flushed later may leave first. Behaviour is unchanged. No game-code
  changes needed.
- `sgl-2d` `UiFrame::password_edit_clear`: the clear button released the
  buffer's allocation; it now zeroizes in place and keeps the preallocated
  capacity. No game-code changes needed.
- All SGL crates: published packages no longer ship examples, tests, test
  fixtures or unused vendored reference sources; read those in the repository.
  Licences and notices still ship. No game-code changes needed.
- `sgl-3d` `AutoExposure`: a long frame (`FrameInput::frame_time_ms`)
  within a frame's step of the target stepped the correction past it, by
  stops after a hitch; it now lands on the target. No game-code changes
  needed.
- `sgl-post-fx` half-resolution SSR (`FeatureFlags::HALF_RESOLUTION`): a 2×2
  block on a silhouette could trace from its background pixel, a NaN ray
  under an infinite reversed-Z projection; that pixel now reports a miss.
  No game-code changes needed.
- `sgl-3d` `asset::load*`: a glTF accessor of a component type or shape
  glTF does not allow for its use, of no elements, or past its buffer view
  or buffer, or an image view past its buffer, panicked or was misread; it
  now fails the load with an error naming it.
  No game-code changes needed; re-export a file that now fails.
- `sgl-post-fx` SSR (SGL3D's `Crystal` reflections): a ray towards the camera
  from a surface under one unit away was projected behind the camera and
  traced mirrored, behind the surface; its end is now clipped to the near
  plane (`CameraAttribs::set_clip_planes`, already required). The WGSL
  `ProjectDirection` (`PostFX_Common`) takes the near plane's view Z as a
  fifth argument: WGSL calling it adds it (`g_Camera.fNearPlaneZ`). No other
  game-code changes needed.
- `sgl-2d` `DrawList::sort`: a NaN `z` could panic or misorder the other
  sprites; NaN now draws last and the rest stay ascending and stable. No
  game-code changes needed.
- `sgl-core` `derive_stream_seed`: components no longer cancel (chunk
  `(65536, 0)` and `(0, 1)` shared a stream); every derived seed changes,
  so content re-derived from a persisted base seed (generated worlds,
  replays) changes on upgrade. Games that need the old output regenerate it,
  or store the derived seeds before upgrading.
- `sgl-input` `Gamepad::name`, `Gamepad::is_pressed`, `Gamepad::value` and
  `Gamepads::gamepad` are now `#[must_use]`: discarding their result warns.
  Use or remove such calls; no other game-code changes are needed.
- `sgl-net` `ThreadedUdpServer::disconnect`: reliable and unreliable
  messages accepted before it but not yet handed to the endpoint were
  dropped; they are now sent before the graceful close, within the same
  `close_grace_ms`. No game-code changes needed.
- `sgl-net` `NativeWebSocketClient::connect`: an IPv6-literal URL such as
  `ws://[::1]:9000/game/ws` failed host resolution; it now connects. No
  game-code changes needed.
- `sgl-net` `OriginPolicy` and `NativeWebSocketClientConfig::origin`: IPv6
  literal origins such as `http://[::1]:3000` were rejected as
  non-canonical; they are now accepted in the browser's compressed
  lowercase form. No game-code changes needed.
- `sgl-2d` texture uploads: an empty, oversized or short-`rgba` texture, or
  a mis-sized normal map, panicked; `Renderer::upload_texture`,
  `upload_normal_map`, `upload_light_cookie`, `SpritePass::upload`,
  `upload_normal` and `LightPass::upload_cookie` now return
  `Result<(), sgl_2d::canvas::TextureError>`, and `Renderer::replace_texture`
  / `SpritePass::replace` `Result<bool, TextureError>`, changing nothing on
  error. Handle or `.expect` each result.
- `sgl-core` `StateHasher`: the contract promised distinct digests for any
  different write sequences, but writes are untagged (`u16(0x1234)` equals
  `u8(0x34); u8(0x12)`); it now promises them only within one schema.
  Encoding and digests unchanged. Games that hash several kinds of state in
  one stream, or change what they write, add a leading tag or version.
- `sgl-core` `FrameAnimation::tick` / `AnimationSequence::tick`: a NaN or
  infinite `dt` could hang a repeating animation or freeze a `Once` one, and
  is now ignored. In `Repeat`/`PingPongRepeat`, a frame duration too small
  for `f32` to subtract from the accumulated time hung the tick; it now drops
  the remainder. `FrameAnimation::new` now panics on an infinite `fps` (a
  `Once` animation completed on its first tick; `Repeat` hung): pass a
  finite `fps`. No other game-code changes needed.
- `sgl-3d` `Renderer::finish_frame`: a `Renderer::resize` that changed the
  targets between `render` and `finish_frame` no longer loses its history
  reset; the next frame restarts history. No game-code changes needed.
- `sgl-post-fx` SSR: a roughness-0 surface seen from below its mapped
  normal's horizon, or, at importance-sample bias 0, at the blue noise's
  largest value, gave a NaN ray and PDF; it now traces the mirror direction,
  the noise stays below 1, and the resolve clamps N·V above 0. No game-code
  changes needed.
- `sgl-post-fx` SSR (`HierarchicalRaymarch`, SGL3D's `Crystal` reflections):
  a ray that runs out of `max_traversal_intersections` before confirming a
  hit was accepted by proximity and now reports a miss, so streaks near
  surfaces at the step cap give way to the fallback. No game-code changes
  needed.
- `sgl-2d` `UiFrame::splitter`: dragging with `max < min` or a NaN bound
  panicked; now `max` wins and a NaN bound is ignored. No game-code changes
  needed.

## 0.4.0 — 2026-10-09

- Move every SGL crate to `0.4.0` together. Breaking: update `sgl-net`
  games, `FixedClock` callers, `SurfaceMaterial` and `Settings` literals
  and exhaustive `SceneError` matches as the entries below say.
- `sgl-net` `SendError::ReliableOverflow` → `SendError::WouldBlock`: a full
  reliable queue no longer disconnects the peer on any transport; the send is
  refused whole and may be retried. `Delivery::ReliableOrdered` →
  `Delivery::RELIABLE_ORDERED` (`Delivery::Reliable(Lane)`);
  `DisconnectReason::ReliableOverflow` → `InboundOverflow` (inbound only).
  `ClientIo::capacity(lane)` / `ServerIo::capacity(conn, lane)` are new
  required methods. `BrowserWebSocketConfig::reliable_buffered_bytes` now
  paces sends and must be at least `MAX_WEBSOCKET_FRAME_BYTES`;
  `ThreadedUdpServer::send` refuses connections it has not announced.
  Migration: rename the variants (exhaustive matches use
  `Delivery::Reliable(_)`); treat `WouldBlock` as "keep it and retry
  later", not as a lost connection; implement `capacity` on any
  `ServerIo`/`ClientIo` wrapper.
- `sgl-net` lanes: `RELIABLE_LANES` 1 → 4 independent lanes (each ordered on
  its own, unordered across lanes, own bounds, shared by weight). The new
  `ReliableConfig`/`LaneConfig` (`reliable` on `EndpointConfig`,
  `NativeWebSocketServerConfig`, `NativeWebSocketClientConfig` and
  `BrowserWebSocketConfig`; `memory_duplex_with`) replace
  `RELIABLE_OUTBOUND_*`, `RELIABLE_INBOUND_*`, `memory::MAX_RELIABLE_QUEUED`
  and `ThreadedUdpConfig::{reliable_queue_messages, reliable_queue_bytes,
  event_queue_messages}`, with the same values per lane (`DEFAULT_LANE_*`);
  `EndpointConfig::global_reliable_*_items` → `global_reliable_*_messages`,
  defaults 4,096 → 12,288 messages and 4 → 24 MiB.
  Migration: rename the constants and global fields, and move removed
  `ThreadedUdpConfig` bounds to `EndpointConfig::reliable.lanes[n]`; one-lane
  games change nothing else; others pick `Lane::new(n)` and set
  `config.reliable.lanes[n].weight`.
- `sgl-net` `MAX_RELIABLE_MESSAGE_BYTES` (64 KiB) →
  `ReliableConfig::max_message_bytes` (default
  `DEFAULT_RELIABLE_MESSAGE_BYTES`, 64 KiB; at most
  `RELIABLE_MESSAGE_BYTES_LIMIT`, 16 MiB); both ends must set the same cap,
  and `global_reliable_{outbound,inbound}_bytes` must be at least it. A lane
  holding no reliable bytes admits one message of any size up to the cap.
  Migration: use the constant or the configured value; delete game-side
  segmentation.
- `sgl-net` `Delivery::Unreliable(Lane)` (new): messages of at most
  `MAX_UNRELIABLE_BYTES` (1168) sent once, unordered, delivered at most
  once; a full send queue refuses with `WouldBlock`, and a receiver that is
  not polled drops its oldest unpolled ones. Migration: exhaustive
  `Delivery` matches add an arm.
- `sgl-net` wire format: UDP version 3 and WebSocket envelope version 2;
  old and new builds cannot connect, so rebuild servers and clients
  together and change the WebSocket subprotocol.
  `udp::MAX_RELIABLE_FRAGMENT_BYTES` 1168 → 1150; WebSocket frames carry at
  most `WEBSOCKET_FRAGMENT_BYTES` (16 KiB), so `MAX_WEBSOCKET_FRAME_BYTES`
  is 16,406 and an oversized frame is `ProtocolViolation` (was
  `Transport`). `Envelope` borrows its payload and gains
  `fragment: Fragment`, `encode_envelope(magic, &envelope)`, and
  `EnvelopeError` gains `InvalidLane`, `InvalidFlags` and `InvalidTotal`.
  `SimulatedConfig` gains `lane_loss_per_10k`. Migration: add
  `lane_loss_per_10k: [0; RELIABLE_LANES]` or `..Default::default()` to
  `SimulatedConfig` literals; direct envelope users pass an `Envelope`.
- `sgl-net` native WebSocket and UDP receivers: a lane past its `inbound_*`
  bounds (on UDP, or `EndpointConfig::global_reliable_inbound_messages`)
  stops taking messages until `poll` makes room, so the sender gets
  `WouldBlock` (was disconnected with `InboundOverflow`). A UDP sender
  waits while keepalives arrive; a native WebSocket sender still times out
  after its `timeout_ms`. No game-code changes needed; browser receivers
  still close on overflow, so size their `inbound_*` bounds for one poll
  interval.
- `sgl-net` UDP: a flush packs messages, latest state and acknowledgements
  into shared datagrams (was one item per datagram), so
  `max_packets_per_peer_flush` bounds datagrams, not messages. No game-code
  changes needed.
- `sgl-net` WebSocket: flushed latest state leaves ahead of lane frames
  flushed after it (was behind every released reliable frame), and a newer
  `send` no longer withholds it. No game-code changes needed.
- `sgl-core` `FixedClock`: accumulates exact `Duration` time (was `f32`
  seconds): `begin_frame` takes the frame's `Duration`, the `fixed_dt` field
  is now the `fixed_dt()` method beside `fixed_step()`, and `dropped_dt`
  reports unsimulated time. New opt-in `with_catch_up(hz, CatchUp { .. })`
  runs several steps per frame; `with_hz` and `new` keep one step per
  frame. Migration: pass the frame's elapsed `Duration` (e.g.
  `Duration::from_secs_f32(dt)` for an existing `f32` delta) and call
  `fixed_dt()`.
- `sgl-2d` `SpritePass::upload`: a handle already uploaded now has its pixels
  replaced (was ignored), and `SpritePass::replace` is public, so glyph pages
  from `TextRenderer::end_frame` update the existing pass. New
  `SpritePass::draw_stats` reports per-channel draws. No game-code changes
  required; games that upload unchanged textures every frame should upload
  only on change.
- `sgl-3d` programmable surfaces (new): `Scene::add_shader` takes a game's
  WGSL `material_vertex`, `material_surface` and `ShaderParams`, used by
  `SurfaceMaterial::shader` (`None` by default), with per-vertex data from
  `PreparedModel::with_shader_data`; blended materials may read
  `scene_volume_path`
  ([Programmable surfaces](crates/sgl-3d/README.md#programmable-surfaces)).
  Migration: `SurfaceMaterial` literals without `..` add `shader: None`;
  exhaustive `SceneError` matches add `UnknownShader`, `ShaderInUse`,
  `ShaderParameters`, `Shader`, `InvalidDisplacementBound` and
  `ShaderDataLength`.
- `sgl-3d` `Settings::volume_paths` (new, on by default; saved settings
  without it load on) and `Renderer::volume_paths_in_effect`. Migration: a
  `Settings` literal without `..` adds `volume_paths: true`.
- Docs: the [consumer guide](docs/README.md#physics) recommends custom
  arcade physics for most games and Rapier only where simulated physics is
  the game. No game-code changes needed.

## 0.3.0 — 2026-10-08

- Move every SGL crate to `0.3.0` together. Breaking: update `Asset`,
  material and `AlphaMode::Blend` literals and exhaustive `SceneError`
  matches as the entries below say.
- `sgl-3d` volumetric fog: a point or spot light's inverse square in the
  fog now adds the froxel's squared diagonal, so near lights no longer pulse
  or leave puffs, and their near fog is dimmer. Raise the light's
  `fog_energy` if its halo looks too faint; surfaces are unchanged.
- `sgl-3d` `Renderer::capture_specular_probe`: mip 0 held the one sample at
  each texel's centre, so a sub-texel emitter was stored at whole-texel
  energy or not at all; now each face renders at 2048 texels a side (or
  `face_size`, if larger) and is box-averaged to `face_size`. Captures take
  longer and use more memory, and fail in the browser before rendering. No
  game-code changes; re-export baked specular probes to pick it up; old
  exports still load.
- `sgl-3d` glTF loading: an unsupported extension listed only in
  `extensionsUsed` (an anisotropy texture's extensions included) no longer
  fails the load; it is left out and listed in the new `Asset::ignored`
  (`asset::Ignored`). Only unsupported `extensionsRequired` fail. Migration:
  `Asset` literals add `ignored: Vec::new()`; read `asset.ignored` after a
  load to see what was left out.
- `sgl-3d` occlusion maps, a load error before: one packed in the
  metallic-roughness image's red channel on `TEXCOORD_0` (ORM) occludes
  ambient diffuse, lightmap, atlas and ambient-cube diffuse and environment
  specular, taking the lesser of it and `Settings::ambient_occlusion`'s
  visibility where both apply; one in its own image or on another UV set
  loads unsampled (`Ignored::OcclusionMap`). New
  `asset::Material::occlusion_texture` (`None`), `occlusion_strength` (1)
  and `SurfaceMaterial::occlusion_strength`.
- `sgl-3d` `KHR_materials_ior` and `KHR_materials_specular`, a load error
  before: their factors set a dielectric's F0 and F90 through new `ior`
  (1.5), `specular` (1) and `specular_color` (`[1.; 3]`) on
  `asset::Material` and `SurfaceMaterial`; the defaults keep F0 0.04 and
  F90 1. Specular textures load unsampled (`Ignored::SpecularMap`).
  Migration: exhaustive material literals add the fields or take
  `..Default::default()`.
- `sgl-3d` `Scene::add_materials` and `set_material` refuse an IOR below 1,
  `specular` outside 0..=1 or a specular colour that is negative or not
  finite (`SceneError::InvalidReflectance`), and `occlusion_strength` outside
  0..=1 (`SceneError::InvalidOcclusion`). Migration: exhaustive matches on
  `SceneError` add both variants.
- `sgl-3d` binding tiers: a device with 48 or more sampled textures per
  shader stage takes `BindingTier::Extended`, any other `Basic`, reported by
  the new `Renderer::binding_tier()` (`graphics_device::BindingTier`). On
  `Basic`, devices with 21 to 47 (iOS GPUs older than Apple4, some Vulkan
  drivers), a material's anisotropy map gives way to its anisotropy
  factors, where it shaded before. No game-code change is needed; Metal on
  macOS, DX12 and Chrome's WebGPU take `Extended`.
- `sgl-3d` device floor: WebGPU's default limits (16 sampled textures per
  stage), down from 21, so a browser's default WebGPU adapter runs SGL3D,
  on `Basic`. On `Basic` baked light from a lightmap or irradiance atlas is
  non-directional and `Settings::dynamic_gi` resolves to `Off`, reported by
  the new `Renderer::dynamic_gi_in_effect(&settings)`; devices with 21 to
  47 lose both, where they ran before. No game-code change is needed.
- `sgl-3d` direct light: rough metals keep their multiply scattered energy
  under every light type (they were darker than under an even sky, and
  coloured metals shifted hue). Migration: none; lower any light intensity
  raised to make up for dark metals.
- `sgl-3d` environment specular reads a finer DFG table, so rough
  reflections change slightly. No game-code change.
- `sgl-3d` ray hits shade down to perceptual roughness 0.045, as raster does
  (was 0.0525). No game-code change.
- `sgl-3d` Fresnel is Schlick's fifth power (was Epic's exp2 fit), so
  highlights change slightly toward grazing. No game-code change.
- `sgl-3d` direct light on dielectrics: diffuse keeps only what the
  specular's Fresnel leaves, slightly darker. No game-code change.
- `sgl-3d` `LightShape::Point`/`Spot` `radius` and
  `DirectionalLight::angular_diameter` now also size specular highlights
  (they sized only ray ends), so the defaults widen highlights on smooth
  surfaces. Migration: none; a radius or angular diameter of 0 keeps a
  point's highlight and hardens ray-traced shadows.
- `sgl-3d` lightmaps, irradiance atlases, ambient cubes and the hemisphere
  fill light a material as the environment's diffuse light does (they took
  1 − F0 and 1, and no multiple scattering on metals). No game-code change
  or re-bake.
- `sgl-3d` multiple-scattered specular from the environment, hemisphere fill
  and volumes is darkened by `Settings::ambient_occlusion` and occlusion
  maps as specular (was left undarkened), so rough metals darken in creases.
  No game-code change.
- `sgl-3d` a lightmap's, atlas chart's or ambient cube's multiple-scattered
  specular takes the occlusion map as specular (was linearly), so occluded
  rough metals are slightly brighter. No game-code change.
- `sgl-3d` `DiagnosticTarget::Composite`'s alpha is 1 on opaque surfaces
  (was their base colour's alpha); the presented frame's is 1, as before.
  No game-code change.
- `sgl-3d` `KHR_materials_clearcoat` textures: load error → loaded and
  shaded on the `Extended` tier, the clearcoat normal map tilting the coat.
  New `asset::Material::clearcoat_texture`, `coat_roughness_texture`,
  `coat_normal_texture` (`None`), `coat_normal_scale` (1) and
  `SurfaceMaterial::coat_normal_scale`. Migration: literals add them or
  `..Default::default()`.
- `sgl-3d` `KHR_materials_iridescence`: unsupported → a thin film. New
  `asset::Material::iridescence` (0), `iridescence_ior` (1.3),
  `iridescence_thickness` (`[100., 400.]`), `iridescence_texture`,
  `iridescence_thickness_texture` (`None`), the first three on
  `SurfaceMaterial`. Migration: as above.
- `sgl-3d` `Scene::add_materials`/`set_material`: new
  `SceneError::InvalidIridescence` for a film outside its bounds.
  Migration: exhaustive `SceneError` matches add it.
- `sgl-3d` `Basic` tier: clearcoat and iridescence maps fall back to their
  factors, the clearcoat normal to the geometry normal. No game-code change.
- `sgl-3d` glTF: a primitive without `TEXCOORD_0` whose material has an
  emissive, normal or bump map loaded → refused. Migration: export UV0.
- `sgl-3d` `AlphaMode::Blend` gains `keeps_specular`: `true` fades only
  diffuse and emitted light by alpha, keeping reflections and highlights
  at full strength (glass); `false` is the previous behaviour, and glTF
  `BLEND` loads it. Migration: `Blend { receives_screen_space_reflections:
  r }` becomes `Blend { receives_screen_space_reflections: r,
  keeps_specular: false }`; patterns match `Blend { .. }`.
- `sgl-3d` transmission: new `transmission`, `thickness`,
  `attenuation_distance`, `attenuation_color` and `dispersion` on
  `SurfaceMaterial` and `asset::Material` (with `transmission_texture` and
  `thickness_texture`), loaded from `KHR_materials_transmission`,
  `KHR_materials_volume` and `KHR_materials_dispersion`, refract what lies
  behind on `Extended` and blend it through on `Basic`. Migration:
  exhaustive material literals take `..Default::default()` or the fields.
- `sgl-3d` a transmissive material draws with the blended surfaces whatever
  its alpha mode: it casts no shadow and rays pass through it. No game-code
  change.
- `sgl-3d` glTF files using those extensions, loaded opaque before, now load
  transmissive. No game-code change.
- `sgl-3d` `Scene::add_materials`/`set_material`: new
  `SceneError::InvalidTransmission` for values outside the extensions'
  bounds. Migration: exhaustive `SceneError` matches add it.
- `sgl-3d` `KHR_materials_sheen`, left out before: a sheen layer for cloth,
  through new `sheen_color` (`[0.; 3]`) and `sheen_roughness` (0) on
  `asset::Material` and `SurfaceMaterial` and `sheen_color_texture` and
  `sheen_roughness_texture` on `asset::Material`. Migration: exhaustive
  material literals add the fields or take `..Default::default()`.
- `sgl-3d` `KHR_materials_diffuse_transmission`, left out before: light
  passed through leaves, paper and, with `thickness`, volumes such as wax,
  through new
  `diffuse_transmission` (0) and `diffuse_transmission_color` (`[1.; 3]`)
  on `asset::Material` and `SurfaceMaterial` and
  `diffuse_transmission_texture` and `diffuse_transmission_color_texture`
  on `asset::Material`. Migration: as for the sheen.
- `sgl-3d` sheen and diffuse transmission maps bind on the `Extended`
  binding tier only; on `Basic` their factors apply alone. New
  `SceneError::InvalidSheen` and `InvalidDiffuseTransmission` refuse values
  outside glTF's ranges. No game-code change.

## 0.2.1 — 2026-10-07

- Move every SGL crate to `0.2.1` together; no API changes and no game-code
  changes.
- `Settings::ambient_occlusion` costs less GPU time; its output is
  unchanged.

## 0.2.0 — 2026-10-06

No baked or exported asset format changed: nothing needs re-baking. SGL3D's
[agent guide](crates/sgl-3d/docs/README.md),
[features](crates/sgl-3d/docs/features.md) and
[settings](crates/sgl-3d/docs/settings.md) describe 0.2.0.

### Upgrade steps

- Move every SGL crate to `0.2.0` (or `=0.2.0`) together; never mix 0.1 and
  0.2 crates. `sgl-core` and `sgl-input` have no API changes.
- Require wgpu and naga `30.0.1` (30.0.0 panics on WebGPU under current
  `wasm-bindgen`) and `sp-fidelity`/`sp-fidelity-wgpu` `0.2`, as caret
  requirements, not `=` pins. In the browser, require `wasm-bindgen` 0.2.127
  or later, with a `wasm-bindgen-cli` matching the game's `Cargo.lock`
  exactly: `cargo install wasm-bindgen-cli --version 0.2.129 --locked` for
  SGL's lockfile.
- glam 0.33: `sgl_3d::glam` moves from 0.30 to the glam of `sgl_core::math`
  and `sgl-2d`; drop conversions that only bridged them. A direct glam
  requirement needs 0.33.2. `Mat4::look_at_rh`/`look_to_rh` become
  `glam::camera::rh::view::look_at_mat4`/`look_to_mat4`, and projections
  `glam::camera::rh::proj::directx::{perspective, orthographic,
  perspective_infinite_reverse}` (same arguments).
- Game code calling wgpu, such as a 3D game's device and surface setup
  ([wgpu 30 changes](https://github.com/gfx-rs/wgpu/blob/v30.0.0/CHANGELOG.md)):
  `surface_texture.present()` becomes `queue.present(surface_texture)`;
  `SurfaceConfiguration` adds `color_space: wgpu::SurfaceColorSpace::Auto`
  (the old output) and `RequestAdapterOptions` `apply_limit_buckets: false`;
  `get_mapped_range` returns a `Result`; `VertexState::buffers` takes
  `&[Option<VertexBufferLayout>]`; `TextureUsages::TRANSIENT` is
  `TRANSIENT_ATTACHMENT`; WGSL integer vertex outputs declare
  `@interpolate(flat)`; Metal refuses a stage with more than 29 buffers and
  acceleration structures together.
- Regenerate the game's [licence notices](#licence-notices).

### sgl-2d

- `sgl_2d::render` (`Renderer`, `Sprite`, `SpriteBatch`, `TextureId` and the
  rest) is removed: port to the unchanged `canvas`, as
  [`examples/direct-game`](examples/direct-game/src/main.rs) does.
  - `render::Renderer::new(window)` becomes
    `canvas::Context::try_new_async(window, vsync)` (`try_new` natively) and
    `canvas::Renderer::new(&context, logical_w, logical_h, clear_srgb)`
    (`set_target_size` for native resolution); `resize` moves to `Context`.
    The canvas needs wgpu's default limits, not downlevel ones.
  - Textures become `assets::Texture`s in `Assets<Texture>`, uploaded with
    `Renderer::upload_texture` and drawn by handle (`white_texture` for flat
    quads).
  - `SpriteBatch::push(Sprite)` becomes `DrawList::push(SpriteInstance)`
    (`push_screen` for UI): `src: Some(Rect)`, `scale = size / source size`
    (× `pixels_per_unit` in world units), `position` at the centre (offset
    other pivots by `(0.5 - pivot) × size`, rotated), `rot`, ordered by `z`.
  - `view_projection` becomes `canvas::Camera` (`with_units`, `center`,
    `zoom`; no rotation). Tints and clear colours are sRGB
    (`canvas::linear_to_srgb`), or tints stay linear with
    `Renderer::with_lighting(LightingSpace::Linear)`.
  - A frame is `context.acquire()` (`None` replaces `FrameOutcome::Skipped`),
    `renderer.render(&context, &frame, &mut draw_list, &camera)`,
    `frame.present()`.
- `ui::edit_apply` is removed: use `UiFrame::line_edit`, or keep its logic in
  the game (backspace pops a character, then non-control characters append
  up to the limit).

### sgl-net

- `NativeWebSocketServer` keeps accepting after an accept error, where it
  closed the listener; server and client keep a backpressured peer when a
  ping or pong is due, where they disconnected it with
  `DisconnectReason::Transport`. No game-code changes.

### sgl-post-fx

Only for code that drives `sgl-post-fx` itself; SGL3D games need nothing.

- Pass `depth_buffer_srv` and `motion_vectors_srv` (filterable float, such
  as `Rg16Float`) to `temporal_anti_aliasing::RenderAttributes`, and drop
  `post_fx_context::RenderAttributes::motion_vectors_srv`,
  `CreateInfo::compute_closest_motion` and
  `PostFXContext::get_closest_motion_vectors`: TAA finds the closest motion.
- Set each `CameraAttribs`' clip planes with `set_clip_planes(near, far)`
  (far first for reversed-Z), or SSR and TAA keep no history.
- `ScreenSpaceReflectionAttribs::max_traversal_intersections` is capped at 256
  and `spatial_reconstruction_radius` at 8.

### sgl-3d: breaking changes

- **Settings** gains `fsr2_sharpening` (`true`), `fsr2_sharpness` (0.8),
  `smaa_quality` (`Medium`), `anisotropic_filtering` (`X8`), `shadow_quality`
  (`High`), `hardware_ray_tracing` and `ray_traced_shadows` (`false`),
  `ray_traced_shadow_quality` (`Preset`), `occlusion_culling` (`false`),
  `dynamic_gi` (`High`) and `fog_filter` (`true`); `Diagnostics` gains
  `dynamic_gi`. `world_space_reflections` is `WorldSpaceReflections`: `true`
  becomes `Moving`, `false` `Off`. A saved file holding the old bool fails to
  load: convert it where the game loads settings, or drop it (`Off`).
- **Struct literals** of `Settings`, `Light`, `DirectionalLight`,
  `asset::Material`, `SurfaceMaterial`, `asset::LoadOptions`, `Fog`, `Mist`
  and `ColorGrading` that name every field miss new ones (below, plus
  `Fog::sky_affect`, `Mist::drift`, `ColorGrading::agx_look`): build them
  with `..Default::default()`, and use `Decal::new(base_color)`,
  `InstanceState::new(model)` and `..DirectionalShadow::DEFAULT` in a
  `const`. New fields default to 0.1.0's look.
- **Device floor:** 21 sampled textures per shader stage (was 17; no known
  adapter offers 17–20) and `DownlevelFlags::INDIRECT_EXECUTION`, which
  `graphics_device::limits` requests; a game requesting its own must too.
- **Models:** `Scene::add_model` and `set_model` take a `PreparedModel`, not
  `Vec<ModelMesh>`: `scene.add_model(&device, &queue,
  PreparedModel::new(meshes)?)?`. `PreparedModel::new` validates and is
  `Send`, so prepare run-time geometry on worker threads.
- **Scene errors:** exhaustive matches add `TooManyLightmapCharts { .. }`,
  `TooManyLods`, `TooManySections`, `InvalidNormalLayers`, `InvalidOrigin`,
  `InvalidDynamicGiVolume`, `InvalidIrradianceVolume`,
  `InvalidIrradianceRegion` and `IrradianceRegionOutside`. Newly refused: a
  zero vertex normal (`NonFiniteGeometry`), over 65,536 distinct
  `lightmap_bounds` or 256 morph targets in a mesh, over 8 LODs a mesh
  (`set_mesh_lods`), a mesh over 8,388,608 triangles, and `add_instance`
  when the ray source is full (`DeviceLimit`).
- **Lights:** `LightShape::Point` is `Point { radius }` and `Spot` gains
  `radius` (use `LightShape::DEFAULT_RADIUS`; match `Point { .. }`). `Light`
  gains `fog_energy` and `shadow_opacity` (1); `DirectionalLight` those and
  `angular_diameter` (`SUN_ANGULAR_DIAMETER`). Sizes affect only rays.
  `add_light`/`set_light` refuse a negative or non-finite radius or fog
  energy, or an opacity outside 0..=1 (`InvalidLight`).
- **Directional shadows:** delete `DirectionalShadow::first_split`; SGL3D
  places the splits. A zero or non-finite `distance` no longer turns the
  shadow off (use `shadow: None`); distances stop at 8192 m.
- **glTF loading:** `asset::LoadOptions<'a>` gains a lifetime and the `Sync`
  callbacks `images` (return `ImageSource::Supplied(image)`, such as a BC7
  chain, to skip decoding a `GltfImage`) and `nodes`.
  `asset::load_slice_filtered(&bytes, pred)` becomes
  `load_slice_with_options(&bytes, LoadOptions { nodes: Some(&pred),
  ..LoadOptions::default() })`. `load_slice` now decodes `data:` URI images.
- **Materials:** `AlphaMode::Blend` is `AlphaMode::Blend {
  receives_screen_space_reflections }` (`false` is 0.1.0's; match
  `Blend { .. }`). `asset::Material` and `SurfaceMaterial` gain
  `normal_layers` (`None`) and `emits_into_gi` (`true`).
- **Frame time:** `FrameInput::elapsed_seconds` is `f64` (`as_secs_f64()`).
- **Internals now SGL3D's:** delete `FrameInput::crystal` and
  `CrystalParameters`; `Fog::detail_spread` and `temporal_reprojection`;
  `BloomParameters::low_frequency_boost`, `low_frequency_boost_curvature` and
  `high_pass_frequency`; `AutoExposure::min_log_luminance`,
  `max_log_luminance`, `filter_low`, `filter_high` and
  `exponential_transition_distance`. SGL3D keeps the old defaults. Auto
  exposure meters log2 luminance −8..8: to meter outside it, set
  `Exposure::stops = s` and shift the compensation curve's x by +s and
  `correction_min`/`correction_max` by −s.
- **Glow:** `effects::Glow::kind: f32` is `GlowKind`, which takes the removed
  `uv` and `other`: kind 0 is `Uniform`, 1 `Tapered { uv, profile:
  GlowProfile::default() }`, 2 `Line { other, offset }` (`offset` was
  `uv[0]`). `Glow` is no longer `Pod`.
- **Geometry statistics:** `Renderer::geometry_stats(&device)` takes
  `&mut self` and returns `Option<GeometryStats>` a few frames late (`None`
  until one arrives); opaque camera draws count per 128-triangle section.
  `geometry_stats_for_model(&device, &scene, model)` needs `diagnostics` and
  returns `Result<Option<_>, _>`. `lod` is a module (`MeshLod`,
  `MAX_MESH_LODS`).
- **Ambient occlusion:** a zero or non-finite
  `FrameInput::ambient_occlusion_radius` no longer turns AO off: set
  `Settings::ambient_occlusion` to `Off`. Radii clamp to 0.01–10000 m.

### sgl-3d: behaviour changes

These compile unchanged; check them on the game's routes.

- **Atmosphere off by default:** `FrameInput::new` sets `atmosphere: false`:
  set it `true` on frames that should show fog and mist.
- **Fog:** `Fog::ambient` defaults to 0 (was 1), so fog away from lights no
  longer glows (set `1.` for the old look); `Settings::fog_filter` blurs the
  froxels (`false` is 0.1.0's fog); directional shadows in fog fade over
  10–30 cm behind an occluder.
- **Shadows:** cascades split at 0.1, 0.2 and 0.5 of `distance` (Godot's),
  so near shadows are a little softer; receivers offset along the geometry
  normal, so shadow edges on normal-mapped surfaces and water stop crawling.
- **Coated materials:** `clearcoat` dims baked diffuse light (lightmaps,
  atlas charts, ambient cubes) as it dims live light.
- **TAA** keeps history under fast motion, as Godot's: fast surfaces are
  antialiased and softer. Timing group `DiligentFX closest motion` is now
  in `TAA`. TAA and reflections take no history from surfaces behind the
  last camera, and Crystal reflections stop smearing in fast motion.
- **Dither:** the output is always dithered by up to half an 8-bit step:
  exact image comparisons re-capture or allow one code value.
- **Draw order:** opaque and masked draws are built on the GPU, so coplanar
  surfaces in different draws have no defined winner: give an overlay a
  depth offset or make it a `Decal`.
- **World-space reflections** reach 1000 m (was 100 m); timing group
  `world reflection classify` joins `world reflection rays`.
- **Vertex packing:** vertex colours clamp to 0..1 and UVs are 16-bit across
  each mesh's UV rectangle: split a mesh tiled so often that 1/131,070 of its
  UV extent shows. Preparing a model costs more: check remeshing budgets.
- **Decals:** the scene's first decal, or removing its last, recompiles the
  lit pipelines: add decals at load if the hitch shows.
- **FSR2** falls back to TAA with `Renderer::fsr2_error` when wgpu rejects a
  pass, where it panicked, and runs at scene sizes under 64 pixels.

### sgl-3d: new features

- [Dynamic diffuse GI](crates/sgl-3d/README.md#dynamic-diffuse-gi):
  `Scene::set_dynamic_gi_volume` and `Settings::dynamic_gi`; a fixture a
  scene light stands for sets `emits_into_gi: false`.
- [Irradiance volume](crates/sgl-3d/README.md#irradiance-volume) the game
  computes and writes by region: `Scene::set_irradiance_volume`,
  `PreparedIrradianceRegion`, `write_irradiance_cells`.
- [Hardware ray tracing](crates/sgl-3d/README.md#hardware-ray-tracing)
  (opt-in; Metal on macOS 15 Apple silicon, Vulkan, DX12 tier 1.1 with DXC;
  no browser): request `graphics_device::ray_tracing_features(&adapter)`
  with `experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() }`
  and set `Settings::hardware_ray_tracing`; `Renderer::ray_tracing_in_effect`,
  `ray_tracing_error` and `ray_tracing_stats` report it. No feature
  requires it. Ray-traced shadows (`Settings::ray_traced_shadows`,
  `ray_traced_shadow_quality`) soften by light size while it is in effect;
  without it the shadow maps shadow everything.
- Occlusion culling (opt-in): `Settings::occlusion_culling`; timing groups
  `cull late`, `depth pyramid`, `geometry late`.
- [Scrolling normal layers](crates/sgl-3d/README.md#scrolling-normal-layers)
  animate water without uploads and mark FSR2's composition mask (timing
  group `FSR2 composition`).
- [Blended receivers](crates/sgl-3d/README.md#blended-receivers) take
  screen-space reflections (timing group `receivers`); world-space
  reflections `All` reflect static geometry too, best with hardware rays.
- `Scene::move_origin` keeps a large world near the origin
  ([lifecycle](crates/sgl-3d/README.md#retained-scene-and-frame-lifecycle)).
- [Settings](crates/sgl-3d/docs/settings.md) for shadow quality, SMAA
  quality, anisotropic filtering and FSR2 sharpening; the look fields above.
- [Diagnostics](crates/sgl-3d/README.md#validation-and-diagnostics) counters,
  resource sizes, per-view draws and times, and the `streaming` example.

### Licence notices

- `sgl-3d` and `sgl-post-fx` ship `THIRD_PARTY_NOTICES.txt` for their ported
  code, now including AMD FidelityFX Denoiser, AMD's single-pass downsampler,
  bcdec and Spartan Engine (all MIT). SGL's notices name Stephen Pryde.
- Follow the [distribution workflow](docs/licensing.md): regenerate the
  game's notices from its lockfile, targets and features (for the new ports
  and wgpu 30's dependencies), ship them in native and browser packages, and
  carry the rules into the game's `AGENTS.md`.

### From a Git revision between 0.1.0 and 0.2.0

Apply the entries above that are new since the pin (compare the
[changelog before the release](https://github.com/stevepryde/sgl/blob/095e23613c160aa8e001061f01b8e22040d4bf6a/CHANGELOG.md)),
then:

- `SceneResources::mesh_buffers`/`mesh_buffer_count` are
  `geometry_live`/`geometry_buffers`; `BuildStep` loses `RayWrite` and
  `MeshBuffers` and gains `Pack`, `Place` and `Write`.
- Delete uses of the instance-visibility oracle
  (`Diagnostics::instance_visibility`, `InstanceVisibility`,
  `Renderer::take_instance_visibility`, `InstanceVisibilityReport`) and of
  `DirectionalShadow::pancake_size`.
- `SceneError::TooManyLightmapCharts` is `TooManyLightmapCharts { mesh }`;
  `DynamicGiReport` gains `frame` and `skipped`.
- `Settings::ray_traced_shadow_quality` defaults to `Preset` (Low on the Low
  preset): set `High` for the earlier look.
- Rectangle lights lying on their fixtures are no longer shadowed by them in
  ray-traced shadows and dynamic GI, so what they light is brighter.

## 0.1.0 — Initial public baseline

- **Scope:** `sgl-core`, `sgl-net`, `sgl-input`, `sgl-2d`, `sgl-3d`, and
  `sgl-post-fx`, all at `0.1.0`. This is the public repository's starting
  snapshot, also published on crates.io.
- **Migration from the private repository:** update Git dependency URLs from
  `stevepryde/stevegame` to `stevepryde/sgl` and select a revision from the new
  repository. Its history starts fresh, so old commit pins do not exist there.
  The import changed repository metadata and package versions, without
  changing game APIs or baked formats. Existing game code needs no API rewrite
  solely for this import. Build the game against the selected dependency
  revision before committing its updated lockfile.
