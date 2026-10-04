# Changes and migrations

Migration guidance for agents maintaining games with SGL. Read all entries
after the game's current version through its target version, in version order.
`Unreleased` describes changes on Git that are not yet in a released version.
See the [update workflow](docs/README.md#updating-a-game) for exact pins and
validation in the consuming game.

Each consumer-visible change records affected crates/symbols, old and new
behavior, concrete migration steps (with before/after code when useful), and
what to exercise in the game afterward. Include settings/defaults, platform
and content-format changes even when the game still compiles. State explicitly
when no migration is needed. Keep entries concise; link to package docs for
full API details.

## Unreleased

### Compatibility policy and upgrade guidance

- **Scope:** all SGL crates. Releases may intentionally break compatibility;
  the supported workflow is agent-assisted migration using this changelog.
  Breaking changes during `0.x` advance the minor version.
- **Migration:** no API, runtime behavior, or content format changes in this
  documentation update. Games needing stability should change permissive
  requirements such as `sgl-3d = "0.1.0"` to `sgl-3d = "=0.1.0"` for each
  direct SGL dependency and commit `Cargo.lock`. Git consumers should pin a
  full commit `rev`. When adopting a later release, follow all intervening
  migration entries and validate the affected game workflows.

### SGL3D roadmap status

- **Scope:** `sgl-3d` documentation. The roadmap now separates implemented
  foundations and features from planned dynamic diffuse GI, DLSS/MetalFX,
  hardware ray tracing, and GPU-driven/occlusion culling. Old private issue
  numbers are removed; stable roadmap labels remain.
- **Migration:** none. This corrects status documentation; no rendering APIs,
  settings, defaults, or content formats changed. Use the
  [feature guide](crates/sgl-3d/docs/features.md) for available capabilities.

### Documentation cleanup

- **Scope:** consumer and contributor guides. Dependency requirements now
  point to Cargo manifests instead of repeating current glam/wgpu versions.
  Removed the redundant dependency inventory and contributor publishing section.
- **Migration:** no game-code or dependency changes. Continue using
  `sgl_3d::glam` for 3D math and explicit conversions at the 2D/3D boundary.

### Bundled third-party notices

- **Scope:** repository notices and contributor tooling. Notices now cover
  copied/ported code and bundled assets, including the IBM Plex test font.
  Removed the generated Cargo dependency licence inventory and its machinery;
  original bundled licences and source attribution remain intact.
- **Migration:** no game-code changes. Game agents must follow the new
  [distribution workflow](docs/licensing.md): generate notices from the game's
  locked graph and build selection, preserve asset notices, and include them
  in native/browser packages. SGL supplies a combined convenience bundle,
  package-local port notices, and explicit upstream refresh tooling. Carry
  the distribution rules into the game's own `AGENTS.md`.

## 0.1.0 — Initial public baseline

- **Scope:** `sgl-core`, `sgl-net`, `sgl-input`, `sgl-2d`, `sgl-3d`, and
  `sgl-post-fx`, all at `0.1.0`. This is the public repository's starting
  snapshot; it does not assert that a crates.io upload has taken place.
- **Migration from the private repository:** update Git dependency URLs from
  `stevepryde/stevegame` to `stevepryde/sgl` and select a revision from the new
  repository. Its history starts fresh, so old commit pins do not exist there.
  The import changed repository metadata and package versions, without
  changing game APIs or baked formats. Existing game code needs no API rewrite
  solely for this import. Build the game against the selected dependency
  revision before committing its updated lockfile.
