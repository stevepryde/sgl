# Testing

The specs in this directory are the anchor: tests enforce them so a feature a
game relies on is not dropped or changed by accident. A change runs the
tests that can fail because of it; `bun scripts/tasks.ts check` is the full
check, run once before a release or over a batch of landed changes
([D-54](decisions.md), [Contributing](../CONTRIBUTING.md#validate)).

## Requirements

1. A test exists only if it can fail when the implementation is wrong, and its
   expected value comes from a spec requirement, an external reference (RFC,
   published algorithm, hand-computed pixel), a brute-force model, or a
   roundtrip identity — never from the constant, formula, or source text under
   test. Do not assert a constant against itself, scan source for strings, or
   duplicate implementation logic in a test.
2. Prefer, in order: pure unit and property tests; deterministic integration
   (`SimulatedNetwork`, `memory_duplex`, headless `wgpu`); real I/O smoke
   (loopback sockets, threads, browser) with bounded deadlines and never as the
   only coverage of a state machine.
3. Tests are deterministic: seeds are fixed (property tests run from a
   fixed seed; `PROPTEST_CASES` widens a local run), time is virtual outside
   the real I/O tier, and nothing sleeps outside the real I/O tier.
4. Frozen values (digests, RNG streams, pixel bytes, golden images) are
   contracts. Changing one requires a `decisions.md` entry. SGL3D fixtures that
   pin the output of a rendering implementation being replaced are deleted or
   regenerated with it under [RD-3](sgl3d.md#rendering-development) instead.
   Tests read tracked fixtures and never write them; diagnostic output goes
   to the ignored `.cache/` or `target/`.
5. Pure tests run on native and `wasm32` (Node) by using
   `#[wasm_bindgen_test(unsupported = test)]`; native-only tests keep
   `#[test]` under a `not(target_arch = "wasm32")` gate. GPU tests must run, not skip,
   on any host with a wgpu adapter: `check` sets `SGL_REQUIRE_GPU=1` on macOS
   and honours it elsewhere, turning a missing adapter into a failure.
   Browser-only code (`websocket/browser.rs`) and SGL3D on WebGPU (the
   `browser_smoke` example) run in headless Chromium under Playwright via
   the browser lane (never through WebDriver or chromedriver;
   `bunx playwright install chromium` fetches the pinned browser). SGL3D's
   test launches the `chromium` channel, the full browser in its new
   headless mode, which exposes the GPU's WebGPU adapter; the default
   headless shell exposes none on macOS. It fails on any WebGPU error and on
   a pixel result reasoned from the scene's geometry. The full `check`
   runs the browser lane after its native and WASM steps, and
   `bun scripts/tasks.ts check-browser` runs it alone. It needs Playwright's
   browser build and a GPU.
6. Dependency boundaries ([architecture](architecture.md)) are checked from
   the resolved cargo graph, not from source text.
7. Mutation testing (`bun scripts/tasks.ts mutants`, needs `cargo-mutants`)
   is the periodic audit for tests that cannot fail; it is not part of
   `check`.
8. A test is cheap alone: it runs by name in seconds, on the smallest
   fixture and fewest frames that still let its oracle discriminate, and one
   test covers one concern, so a change reruns only what it bears on. A test
   that cannot be made cheap without losing its oracle is named `long_…` and
   marked `#[ignore = "long: <why>"]`; the full check runs those and nothing
   else does.

## SGL3D scenario harness

`tests/sgl-3d` holds SGL3D's scenario harness (`tests/scenario/`): scripted,
deterministic multi-frame scenes rendered through the public API on a real
device and judged by numbers read from the frame's targets. It protects
rendering behaviour without a game or a person looking at it.

1. A scenario is a fixture whose truth is known by construction, a script of
   camera and object motion by frame index, the settings under test, and
   assertions. It uses only the public API and the `diagnostics` feature.
2. An assertion names its oracle: analytic (computed from the fixture and
   script), reference (the frame supersampled with the temporal feature under
   test off), equivalence (two scripts a contract says look the same), bound
   (finite, non-negative, within the fixture's radiance range, energy not
   created), or a number a spec states. Requirement 1 applies: no stored
   image, no threshold fitted to the current output, no interpretation of the
   picture. A threshold is comparative where a comparison states the
   requirement, and otherwise says where it comes from.
3. A change to a rendering feature in the feature guide lands with that
   feature's suite, or extends it. A suite covers the feature at rest, under
   camera translation and rotation, with an object moving across and along
   the view, with an object the camera follows, across an appearance and a
   departure, a camera cut and a resize, and at each of its settings levels,
   and says which of these cannot affect the feature and which it does not
   cover yet.
4. A fix for a rendering defect adds the scenario that fails without it, at
   the lowest boundary that shows it: the frame when only the frame shows it.
5. Scenarios run on the native lane, each alone by name (requirement 8
   above), and write nothing but an opt-in dump under an ignored directory,
   on which nothing asserts.

The harness is test-support modules that each scenario binary includes: the
rig, the fixtures and readback (`rig`), the analytic truth (`geometry`), the
regions and metrics (`image`), and the temporal antialiasing suites' shared
stage, scripts and oracles (`temporal`). A suite is a `tests/scenario_<feature>.rs`
binary; `scenario_geometry` tests the analytic truth against values computed
by hand. With `SGL_SCENARIO_DUMP` naming a directory (for example
`SGL_SCENARIO_DUMP=.cache/scenarios`), the scenarios that dump, which each
suite names, write their frames as PNG and their metrics as JSON under
`<dir>/<scenario>/`.

## Repository tooling

Run `bun install` at the repository root for the Playwright dependency used by
the browser lane. The required `check` runs the repository's Bun tooling tests
alongside the native and WASM Rust checks and the browser lane. SGL3D rendering
checks follow [its development rules](sgl3d.md#rendering-development).
