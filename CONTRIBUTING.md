# Contributing to SGL

AI coding agents are SGL's primary consumers and authors. A useful change
makes a concrete game task easier while preserving explicit ownership,
small public boundaries, and examples an agent can adapt.

Read [AGENTS.md](AGENTS.md), then the relevant [spec](specs/README.md) and
package guide. For game integration rather than library changes, start with
[Building games with SGL](docs/README.md).

## Setup

Run commands from the repository root. Install Rust through rustup, Bun,
and Node.js. [rust-toolchain.toml](rust-toolchain.toml) selects Rust, Clippy,
rustfmt, and the `wasm32-unknown-unknown` target.

```sh
bun install --frozen-lockfile
bunx playwright install chromium
```

Install the matching CLI with
`cargo install wasm-bindgen-cli --version VERSION --locked`, replacing
`VERSION` with the workspace's `wasm-bindgen` version from
[Cargo.toml](Cargo.toml), without the leading `=`. It provides both
`wasm-bindgen` and `wasm-bindgen-test-runner`. Node.js runs the pure WASM tests. Playwright's
Chromium and a WebGPU-capable GPU run the browser lane. Native examples and
GPU tests need a supported graphics driver; Linux also needs the development
libraries used by winit and Gilrs, including the applicable window-system
libraries and libudev.

## Make a change

- Start with the existing implementation and a concrete consumer need.
  Preserve the game/library boundary in [architecture](specs/architecture.md).
- Breaking changes are an expected part of SGL's evolution. Prefer a clear
  API over compatibility shims, and give consumer agents actionable migration
  instructions in [CHANGELOG.md](CHANGELOG.md). Every consumer-visible change
  belongs under `Unreleased`, including behavior changes that still compile.
- Change the owning spec when behavior changes. Update package docs, agent
  guidance, and affected examples in the same change. Keep current guidance
  separate from the historical [decision log](specs/decisions.md).
- For SGL3D, follow its [architecture](specs/sgl3d-architecture.md) and
  [rendering development rules](specs/sgl3d.md#rendering-development).
  Keep licences and provenance with ported code.
- Add tests only for plausible defects with independent observable results.
  Follow [testing](specs/testing.md); source-text assertions and duplicated
  implementation logic do not establish correctness.

## Validate

Use focused package checks while iterating. At the final boundary, run:

```sh
bun scripts/tasks.ts check
```

This is the required check: licence notices, formatting, Clippy, native tests,
release-mode networking tests, repository tooling tests, WASM builds/tests,
and browser transport and rendering tests. The browser lane can be run alone:

```sh
bun scripts/tasks.ts check-browser
```

GPU tests are required on macOS. On other GPU-equipped hosts, set
`SGL_REQUIRE_GPU=1` when running checks. Report a missing tool, unavailable GPU,
or failing check precisely; do not describe skipped work as passing.
Mutation testing is a separate periodic audit, described in
[testing](specs/testing.md), not an extra submission gate.

For API documentation, run `cargo doc --workspace --no-deps`. Runnable entry
points are linked from the [consumer guide](docs/README.md).

## Describe the result

Explain the problem, resulting behavior, and validation in the change
summary. Mention affected consumers and known limitations. Keep result
reports out of `docs/`; that directory holds maintained consumer guidance.

First-party changes use the repository's [MIT OR Apache-2.0 terms](LICENSE).
Third-party code keeps its original licence and attribution; follow
[THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt) and the owning package's
provenance instructions. The notices cover copied/ported code and bundled
assets, not dependencies fetched separately by Cargo. When adding or removing
bundled material, update `licenses/upstreams.json` and run
`bun scripts/refresh-licenses.ts`. Preserve the original licence files and source
headers. Dependency/port changes also refresh the distribution bundle; follow
[the licensing workflow](docs/licensing.md).
