# Testing

The specs in this directory are the anchor: tests enforce them so a feature a
game relies on is not dropped or changed by accident. `bun scripts/tasks.ts
check` is the required check.

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
   a pixel result reasoned from the scene's geometry. The required `check`
   runs the browser lane after its native and WASM steps, and
   `bun scripts/tasks.ts check-browser` runs it alone. It needs Playwright's
   browser build and a GPU.
6. Dependency boundaries ([architecture](architecture.md)) are checked from
   the resolved cargo graph, not from source text.
7. Mutation testing (`bun scripts/tasks.ts mutants`, needs `cargo-mutants`)
   is the periodic audit for tests that cannot fail; it is not part of
   `check`.

## Repository tooling

Run `bun install` at the repository root for the Playwright dependency used by
the browser lane. The required `check` runs the repository's Bun tooling tests
alongside the native and WASM Rust checks and the browser lane. SGL3D rendering
checks follow [its development rules](sgl3d.md#rendering-development).
