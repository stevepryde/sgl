import { mkdirSync } from "node:fs";

const [task, ...options] = Bun.argv.slice(2);

const USAGE =
  "usage: bun scripts/tasks.ts check | check-browser | mutants [--package <crate>] | " +
  "measure-browser [--frames <count>] [--radius <across>,<up>] [--occlusion]";

if (task === "check-browser") {
  if (options.length !== 0) throw new Error(USAGE);
} else if (task === "measure-browser") {
  if (options.length % 2 !== 0) throw new Error(USAGE);
} else if (task !== "mutants" && (task !== "check" || options.length !== 0)) {
  throw new Error(USAGE);
}

// Default CJS node runner can keep the event loop alive after tests; ESM node exits.
const wasmNodeTestEnv = {
  CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER: "wasm-bindgen-test-runner",
  WASM_BINDGEN_TEST_ONLY_NODE: "1",
  WASM_BINDGEN_USE_NODE_EXPERIMENTAL: "1",
};

// The sgl-2d GPU tests skip when no wgpu adapter exists. On macOS Metal is
// always present, so a skip there would hide a real failure: require the GPU.
// Elsewhere, set SGL_REQUIRE_GPU=1 on hosts known to have an adapter.
const requireGpu = process.platform === "darwin" || process.env.SGL_REQUIRE_GPU === "1";
console.log(
  requireGpu
    ? "GPU tests: required (SGL_REQUIRE_GPU=1)"
    : "GPU tests: may skip without an adapter (set SGL_REQUIRE_GPU=1 to require)",
);

const commands: Array<{ argv: string[]; env?: Record<string, string> }> = [
  { argv: ["bun", "scripts/license-notices.ts", "check"] },
  { argv: ["cargo", "fmt", "--check"] },
  { argv: ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"] },
  {
    argv: ["cargo", "test", "--workspace"],
    env: requireGpu ? { SGL_REQUIRE_GPU: "1" } : undefined,
  },
  // Release semantics differ (debug_assertions off); sgl-net must hold there.
  { argv: ["cargo", "test", "-p", "sgl-net", "--release"] },
  { argv: ["bun", "test", "scripts"] },
  // Every library builds for the browser, SGL3D and its ports included. Pure
  // tests also run there.
  { argv: ["cargo", "check", "--workspace", "--target", "wasm32-unknown-unknown"] },
  {
    argv: ["cargo", "test", "--workspace", "--target", "wasm32-unknown-unknown"],
    env: wasmNodeTestEnv,
  },
];

if (task === "check") {
  for (const { argv, env } of commands) {
    const result = Bun.spawnSync(argv, {
      stderr: "inherit",
      stdout: "inherit",
      env: env ? { ...process.env, ...env } : undefined,
    });
    if (result.exitCode !== 0) {
      process.exit(result.exitCode ?? 1);
    }
  }
}

// --- Mutation testing (testing.md 7): the periodic audit for tests that
// cannot fail. Not part of `check`; needs `cargo install cargo-mutants`.
// Each crate's report lands in `mutants.out/<crate>/mutants.out/`.

const MUTANT_CRATES = ["sgl-core", "sgl-net", "sgl-2d"] as const;

// GPU passes and native socket/thread workers only run against real devices
// and sockets; their mutants would mostly time out or be unviable. Globs
// with a slash match the whole path, hence the `**/` prefix.
const MUTANT_EXCLUDES: Record<string, string[]> = {
  "sgl-2d": [
    "**/canvas/gpu.rs",
    "**/canvas/test_gpu.rs",
    "**/canvas/sprite.rs",
    "**/canvas/light.rs",
    "**/canvas/blit.rs",
  ],
  "sgl-net": [
    "**/websocket/browser.rs",
    "**/websocket/native.rs",
    "**/websocket/native_write.rs",
    "**/udp/native.rs",
    "**/udp/threaded.rs",
  ],
};

async function mutants(args: string[]): Promise<void> {
  let crates: readonly string[] = MUTANT_CRATES;
  if (args.length === 2 && args[0] === "--package") {
    if (!MUTANT_CRATES.includes(args[1] as (typeof MUTANT_CRATES)[number])) {
      throw new Error(`unknown crate ${args[1]}; ${USAGE}`);
    }
    crates = [args[1]];
  } else if (args.length !== 0) {
    throw new Error(USAGE);
  }
  // cargo-mutants creates only the last path segment of `--output`.
  mkdirSync("mutants.out", { recursive: true });
  for (const crate of crates) {
    const argv = [
      "cargo",
      "mutants",
      "--package",
      crate,
      "--output",
      `mutants.out/${crate}`,
      "--timeout-multiplier",
      "3",
      "--no-shuffle",
      // Three parallel build trees: several times faster than one, without
      // the memory and disk cost of one per core.
      "--jobs",
      "3",
    ];
    for (const glob of MUTANT_EXCLUDES[crate] ?? []) {
      argv.push("--exclude", glob);
    }
    console.log(`\n== mutants: ${crate}`);
    // cargo-mutants exits non-zero when mutants survive or time out; that is
    // the report, not a task failure. A run that tested nothing is.
    Bun.spawnSync(argv, { stderr: "inherit", stdout: "inherit" });
    const dir = `mutants.out/${crate}/mutants.out`;
    const count = async (name: string) => {
      const file = Bun.file(`${dir}/${name}.txt`);
      return (await file.exists()) ? (await file.text()).split("\n").filter(Boolean).length : 0;
    };
    const [caught, missed, timeout, unviable] = await Promise.all(
      ["caught", "missed", "timeout", "unviable"].map(count),
    );
    const tested = caught + missed;
    if (tested === 0) {
      throw new Error(`${crate}: cargo mutants tested nothing; see the output above`);
    }
    const ratio = caught / tested;
    console.log(
      `${crate}: ${caught} caught, ${missed} missed, ${timeout} timed out, ${unviable} unviable ` +
        `(caught ratio ${(ratio * 100).toFixed(1)}%); see ${dir}/missed.txt`,
    );
  }
}

if (task === "mutants") {
  await mutants(options);
}

// --- Browser lane (testing.md 5): the browser-only code paths run in
// headless Chromium under Playwright: the WebSocket probe against a native
// fixture server, and SGL3D's smoke test on the page's WebGPU device.
// `check` runs it after its other steps; `check-browser` runs it alone. It
// needs Playwright's browser build and a GPU.

const PROBE_DIR = "target/browser-probe";

async function checkBrowser(): Promise<void> {
  if (!Bun.which("wasm-bindgen")) {
    throw new Error(
      "the browser lane needs the wasm-bindgen CLI matching the workspace pin: " +
        "`cargo install wasm-bindgen-cli --version 0.2.126`",
    );
  }
  const steps: string[][] = [
    ["bun", "install", "--frozen-lockfile"],
    ["cargo", "build", "-p", "sgl-net", "--example", "browser_probe", "--target", "wasm32-unknown-unknown"],
    ["cargo", "build", "-p", "sgl-3d", "--example", "browser_smoke", "--target", "wasm32-unknown-unknown"],
    // Built, not run: `measure-browser` runs it.
    ["cargo", "build", "-p", "sgl-3d", "--example", "browser_streaming", "--target", "wasm32-unknown-unknown"],
    ...["browser_probe", "browser_smoke"].map((example) => [
      "wasm-bindgen",
      "--target",
      "web",
      "--out-dir",
      PROBE_DIR,
      `target/wasm32-unknown-unknown/debug/examples/${example}.wasm`,
    ]),
    ["bun", "test", "./browser"],
  ];
  for (const argv of steps) {
    const result = Bun.spawnSync(argv, { stderr: "inherit", stdout: "inherit" });
    if (result.exitCode !== 0) process.exit(result.exitCode ?? 1);
  }
}

if (task === "check" || task === "check-browser") {
  await checkBrowser();
}

// --- Browser measurement (#24): the streaming example's world on the
// browser's WebGPU in headless Chromium, as the browser lane runs SGL3D,
// built in release. It prints what the CPU spends building and recording
// each view's draw list there. Not part of `check`; it needs the browser
// lane's setup and a GPU.

const MEASURE_DIR = "target/browser-measure";
const MEASURE_PORT = 8125;

async function measureBrowser(args: string[]): Promise<void> {
  let frames = 600;
  let radius = [4, 2];
  let occlusion = false;
  for (let at = 0; at < args.length; at += 2) {
    if (args[at] === "--frames") frames = Number(args[at + 1]);
    else if (args[at] === "--radius") radius = args[at + 1].split(",").map(Number);
    else if (args[at] === "--occlusion") {
      occlusion = true;
      at -= 1;
    } else throw new Error(USAGE);
  }
  if (!Number.isInteger(frames) || frames < 60) throw new Error("--frames must be at least 60");
  if (radius.length !== 2 || !radius.every((r) => Number.isInteger(r) && r >= 0)) {
    throw new Error("--radius takes two whole numbers, across and up");
  }
  const steps: string[][] = [
    ["bun", "install", "--frozen-lockfile"],
    [
      "cargo", "build", "--release", "-p", "sgl-3d", "--example", "browser_streaming",
      "--target", "wasm32-unknown-unknown",
    ],
    [
      "wasm-bindgen", "--target", "web", "--out-dir", MEASURE_DIR,
      "target/wasm32-unknown-unknown/release/examples/browser_streaming.wasm",
    ],
  ];
  for (const argv of steps) {
    const result = Bun.spawnSync(argv, { stderr: "inherit", stdout: "inherit" });
    if (result.exitCode !== 0) process.exit(result.exitCode ?? 1);
  }
  const { chromium } = await import("playwright");
  const types: Record<string, string> = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript",
    ".wasm": "application/wasm",
  };
  // Cross-origin isolation gives `performance.now()` its finest resolution.
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: MEASURE_PORT,
    async fetch(request) {
      const path = new URL(request.url).pathname;
      const file = path === "/" ? "browser/streaming.html" : `${MEASURE_DIR}${path}`;
      const body = Bun.file(file);
      if (!(await body.exists())) return new Response("not found", { status: 404 });
      const type = types[path.slice(path.lastIndexOf("."))] ?? types[".html"];
      return new Response(body, {
        headers: {
          "content-type": type,
          "cross-origin-opener-policy": "same-origin",
          "cross-origin-embedder-policy": "require-corp",
        },
      });
    },
  });
  const browser = await chromium.launch({ channel: "chromium" });
  try {
    const page = await browser.newPage();
    page.on("console", (message) => {
      if (message.type() === "error" || message.type() === "warning") {
        console.log(`console ${message.type()}: ${message.text()}`);
      }
    });
    const query =
      `frames=${frames}&across=${radius[0]}&up=${radius[1]}&occlusion=${occlusion ? 1 : 0}`;
    await page.goto(`http://127.0.0.1:${MEASURE_PORT}/?${query}`);
    const deadline = Date.now() + 15 * 60_000;
    let report: string | null = null;
    while (report === null) {
      if (Date.now() > deadline) throw new Error("the measurement never reported");
      report = await page.evaluate(
        () => (window as unknown as { __sglReport?: string }).__sglReport ?? null,
      );
      if (report === null) await new Promise((resolve) => setTimeout(resolve, 1000));
    }
    console.log(report.trimEnd());
    if (report.startsWith("FAIL")) process.exitCode = 1;
  } finally {
    await browser.close();
    server.stop(true);
  }
}

if (task === "measure-browser") {
  await measureBrowser(options);
}
