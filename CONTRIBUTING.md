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
cargo install wasm-bindgen-cli --version 0.2.126 --locked
bunx playwright install chromium
```

The wasm-bindgen CLI version must match the workspace dependency in
[Cargo.toml](Cargo.toml); it provides both `wasm-bindgen` and
`wasm-bindgen-test-runner`. Node.js runs the pure WASM tests. Playwright's
Chromium and a WebGPU-capable GPU run the browser lane. Native examples and
GPU tests need a supported graphics driver; Linux also needs the development
libraries used by winit and Gilrs, including the applicable window-system
libraries and libudev.

## Make a change

- Start with the existing implementation and a concrete consumer need.
  Preserve the game/library boundary in [architecture](specs/architecture.md).
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
provenance instructions.

## Publishing

The six library crates start at `0.1.0`; the direct-game example is never
published. The initial release is prepared here but must be uploaded by the
maintainer. Authenticate to crates.io using your approved local credential
setup; never put a registry token in repository files or command arguments.

Publish dependencies before their consumers from the repository root:

```sh
cargo publish -p sgl-core
cargo publish -p sgl-net
cargo publish -p sgl-input
cargo publish -p sgl-post-fx
cargo publish -p sgl-2d
cargo publish -p sgl-3d
```

`sgl-2d` needs the published `sgl-core` version, and `sgl-3d` needs
`sgl-post-fx`. Let each dependency become available in the registry before
publishing its consumer. Cargo performs its normal packaging and build
verification; do not bypass it with `--no-verify`.

For later releases, update the workspace package version and internal dependency
version requirements together. Keep `sgl-3d`'s development-only self-dependency
path-only; it enables diagnostic accessors in workspace tests and is omitted
from the published manifest. Refresh
`Cargo.lock` and the generated licence notices with
`bun scripts/license-notices.ts generate`. Keep release validation in the
[development workflow](#validate); publishing uses Cargo directly.
