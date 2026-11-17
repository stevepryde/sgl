# Contributing to SGL

AI coding agents write and maintain all of SGL and are its consumers; the
owner steers. A useful change
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
`VERSION` with the `wasm-bindgen` version locked in [Cargo.lock](Cargo.lock):
the CLI must match it exactly. It provides both
`wasm-bindgen` and `wasm-bindgen-test-runner`. Node.js runs the pure WASM tests. Playwright's
Chromium and a WebGPU-capable GPU run the browser lane. Native examples and
GPU tests need a supported graphics driver; Linux also needs the development
libraries used by winit and Gilrs, including the applicable window-system
libraries and libudev.

## Make a change

- Start with the existing implementation and a concrete consumer need.
  Preserve the game/library boundary in [architecture](specs/architecture.md).
  Plan a feature, setting, public type, port or refactor with the
  [architect skill](.claude/skills/sgl-architect/SKILL.md) before building it,
  and put its architecture fit in the issue.
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
- Library crates take the root `[workspace.lints]`. `sgl-input` mirrors its
  clippy table because its macOS FFI module needs `unsafe_code` denied rather
  than forbidden. `sgl-3d` and `sgl-post-fx` are exempt from clippy's pedantic
  group, as their manifests record; unsafe-code levels follow
  [stack](specs/stack.md).
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

## Deliver a change

Agents plan, implement, review and land every change ([D-50](specs/decisions.md),
[D-53](specs/decisions.md)). There is no pull request by default: open one
only where it adds something, such as work that needs a visible home while
unfinished, or a change the owner has asked to see before it lands.

1. **Plan.** The issue holds the plan and stays the record of why.
2. **Implement.** Work on a branch from the tip of `origin/main`. Run the
   required check on the final commit, push the branch, and report what
   changed, the evidence, anything not done, and the commit.
3. **Review, in proportion.** A change that adds or alters a stage, a shared
   contract, a layer boundary or a public type (AR-9), and any other whose
   consequence or uncertainty warrants it, gets an independent agent's review
   of the pushed branch against the contracts (AR-10). A localized change
   that follows established patterns gets the landing agent's own reading of
   the diff. Findings are resolved on the branch.
4. **Land.** Merge `main` into the branch, repeat the checks that could have
   been invalidated, then put the change on `main` as one commit by
   fast-forward push. Its message is the record: what was wrong or wanted,
   what changed and why, the evidence, how it was reviewed, known limits, and
   `Closes #n`. Delete the branch. Never force-push `main`.

Keep result reports out of `docs/`; that directory holds maintained consumer
guidance.

First-party changes use the repository's [MIT OR Apache-2.0 terms](LICENSE).
Third-party code keeps its original licence and attribution; follow
[THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt) and the owning package's
provenance instructions. The notices cover copied/ported code and bundled
assets, not dependencies fetched separately by Cargo. When adding or removing
bundled material, update `licenses/upstreams.json` and run
`bun scripts/refresh-licenses.ts`. Preserve the original licence files and source
headers. Dependency/port changes also refresh the distribution bundle; follow
[the licensing workflow](docs/licensing.md).
