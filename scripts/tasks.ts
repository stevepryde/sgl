import { mkdirSync, rmSync } from "node:fs";

const [task, ...options] = Bun.argv.slice(2);

const USAGE =
  "usage: bun scripts/tasks.ts check | check-browser | mutants [--package <crate>] | " +
  "measure-browser [--baseline | --bistro] [--frames <count>] [--radius <across>,<up>] [--occlusion] [--output <json>] [--skip-build] [--scaled] [--prepared] [--capture <png>] [--asset-dir <directory>] [--fixture <json>] [--settings <json>] [--view <camera>] [--size <WxH>] [--warmup <count>] [--exposure <stops>] [--aa taa|smaa|fsr2|off] [--cascades <1..4>] [--motion] [--rect-light] [--sequence <directory>] [--observe-gi] [--no-normal-maps] [--ray-data enabled|disabled] [--compressed-manifest <basename>]";

if (task === "check-browser") {
  if (options.length !== 0) throw new Error(USAGE);
} else if (task === "measure-browser") {
  // measureBrowser checks its own options.
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
const requireGpuEnv = process.env.SGL_REQUIRE_GPU;
const requireGpu = process.platform === "darwin" || (!!requireGpuEnv && requireGpuEnv !== "0");
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
  { argv: ["cargo", "test", "-p", "sgl-net", "-p", "sgl-net-tests", "--release"] },
  { argv: ["bun", "test", "scripts"] },
  // Every library builds for the browser, SGL3D and its ports included. Pure
  // tests also run there.
  { argv: ["cargo", "check", "--workspace", "--target", "wasm32-unknown-unknown"] },
  {
    argv: ["cargo", "test", "--workspace", "--target", "wasm32-unknown-unknown"],
    env: wasmNodeTestEnv,
  },
];

// The wasm tests' runner and the browser lane both come from the wasm-bindgen
// CLI; say how to install it before any step needs it.
function requireWasmBindgenCli(): void {
  if (!Bun.which("wasm-bindgen") || !Bun.which("wasm-bindgen-test-runner")) {
    throw new Error(
      "the wasm-bindgen CLI matching the wasm-bindgen Cargo.lock holds is missing: " +
        "`cargo install wasm-bindgen-cli --version <it> --locked`",
    );
  }
}

if (task === "check") {
  requireWasmBindgenCli();
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
      // Integration tests live in the crate's unpublished companion package.
      "--test-package",
      crate,
      "--test-package",
      `${crate}-tests`,
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
    // A run that never starts must not leave the previous run's files to count.
    rmSync(`mutants.out/${crate}`, { recursive: true, force: true });
    // cargo-mutants exits 2 when mutants survive and 3 when they time out;
    // that is the report, not a task failure. Any other non-zero code (or
    // cargo's 101 when cargo-mutants is missing) is. So is a run that tested nothing.
    const { exitCode, signalCode } = Bun.spawnSync(argv, { stderr: "inherit", stdout: "inherit" });
    if (exitCode !== 0 && exitCode !== 2 && exitCode !== 3) {
      const status = signalCode ? `signal ${signalCode}` : `exit code ${exitCode}`;
      throw new Error(`${crate}: cargo mutants failed with ${status}; see the output above`);
    }
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
  requireWasmBindgenCli();
  const steps: string[][] = [
    ["bun", "install", "--frozen-lockfile"],
    ["cargo", "build", "-p", "sgl-net-examples", "--example", "browser_probe", "--target", "wasm32-unknown-unknown"],
    ["cargo", "build", "-p", "sgl-3d-examples", "--example", "browser_smoke", "--target", "wasm32-unknown-unknown"],
    ["cargo", "build", "-p", "sgl-3d-examples", "--example", "browser_baseline", "--target", "wasm32-unknown-unknown"],
    // Built, not run: explicit prepared assets are needed by the Bistro benchmark.
    ["cargo", "build", "-p", "sgl-3d-examples", "--example", "browser_bistro", "--target", "wasm32-unknown-unknown"],
    // Built, not run: `measure-browser` runs it.
    ["cargo", "build", "-p", "sgl-3d-examples", "--example", "browser_streaming", "--target", "wasm32-unknown-unknown"],
    ...["browser_probe", "browser_smoke", "browser_baseline"].map((example) => [
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

const MEASURE_DIR = process.env.SGL_BROWSER_MEASURE_DIR ?? "target/browser-measure";

async function measureBrowser(args: string[]): Promise<void> {
  let frames = 600;
  let baseline = false;
  let bistro = false;
  let assetDir = "target/bistro-browser-512";
  let fixturePath: string | undefined;
  let settingsPath: string | undefined;
  const sceneOptions: Record<string, unknown> = {};
  let skipBuild = false;
  let scaled = false;
  let prepared = false;
  let capturePath: string | undefined;
  let sequenceDir: string | undefined;
  let output: string | undefined;
  let radius = [4, 2];
  let occlusion = false;
  for (let at = 0; at < args.length; at += 1) {
    if (args[at] === "--baseline") baseline = true;
    else if (args[at] === "--bistro") bistro = true;
    else if (args[at] === "--asset-dir" && at+1<args.length) assetDir=args[++at];
    else if (args[at] === "--fixture" && at+1<args.length) fixturePath=args[++at];
    else if (args[at] === "--settings" && at+1<args.length) settingsPath=args[++at];
    else if (args[at] === "--view" && at+1<args.length) sceneOptions.view=args[++at];
    else if (args[at] === "--aa" && at+1<args.length) sceneOptions.aa=args[++at];
    else if (args[at] === "--warmup" && at+1<args.length) sceneOptions.warmup=Number(args[++at]);
    else if (args[at] === "--exposure" && at+1<args.length) sceneOptions.exposure=Number(args[++at]);
    else if (args[at] === "--cascades" && at+1<args.length) sceneOptions.cascades=Number(args[++at]);
    else if (args[at] === "--observe-gi") sceneOptions.observe_gi=true;
    else if (args[at] === "--no-normal-maps") sceneOptions.normal_maps=false;
    else if (args[at] === "--ray-data" && at+1<args.length) sceneOptions.ray_data=args[++at];
    else if (args[at] === "--compressed-manifest" && at+1<args.length) sceneOptions.compressed_manifest=args[++at];
    else if (args[at] === "--motion") sceneOptions.motion=true;
    else if (args[at] === "--rect-light") sceneOptions.rect=true;
    else if (args[at] === "--size" && at+1<args.length) {
      const dimensions=args[++at].split("x").map(Number);
      if (dimensions.length!==2||!dimensions.every(n=>Number.isInteger(n)&&n>0&&n<=4096)) throw new Error("--size needs WxH within 1..4096");
      [sceneOptions.width,sceneOptions.height]=dimensions;
    }
    else if (args[at] === "--scaled") scaled = true;
    else if (args[at] === "--sequence" && at+1<args.length) {sequenceDir=args[++at];sceneOptions.sequence=true;sceneOptions.motion=true;}
    else if (args[at] === "--capture" && at + 1 < args.length) capturePath = args[++at];
    else if (args[at] === "--prepared") prepared = true;
    else if (args[at] === "--skip-build") skipBuild = true;
    else if (args[at] === "--output" && at + 1 < args.length) output = args[++at];
    else if (args[at] === "--occlusion") occlusion = true;
    else if (args[at] === "--frames" && at + 1 < args.length) frames = Number(args[++at]);
    else if (args[at] === "--radius" && at + 1 < args.length) {
      radius = args[++at].split(",").map(Number);
    } else throw new Error(USAGE);
  }
  if (!Number.isInteger(frames) || frames < 60) throw new Error("--frames must be at least 60");
  if (radius.length !== 2 || !radius.every((r) => Number.isInteger(r) && r >= 0)) {
    throw new Error("--radius takes two whole numbers, across and up");
  }
  // An explicit descriptor selects the same embedded-scene route without a scene-specific flag.
  if (fixturePath) bistro=true;
  if (sceneOptions.warmup !== undefined && (!Number.isInteger(sceneOptions.warmup) || Number(sceneOptions.warmup)<0 || Number(sceneOptions.warmup)>3600)) throw new Error("--warmup must be 0..3600");
  if (sceneOptions.exposure !== undefined && !Number.isFinite(sceneOptions.exposure)) throw new Error("--exposure must be finite");
  if (sceneOptions.cascades !== undefined && (!Number.isInteger(sceneOptions.cascades) || Number(sceneOptions.cascades)<1 || Number(sceneOptions.cascades)>4)) throw new Error("--cascades must be 1..4");
  if (sceneOptions.aa !== undefined && !["taa","smaa","fsr2","off"].includes(String(sceneOptions.aa))) throw new Error("--aa needs taa|smaa|fsr2|off");
  if (sceneOptions.ray_data !== undefined && !["enabled","disabled"].includes(String(sceneOptions.ray_data))) throw new Error("--ray-data needs enabled|disabled");
  if (baseline && bistro) throw new Error("choose --baseline or --bistro");
  if (!bistro && (Object.keys(sceneOptions).length || fixturePath || settingsPath || assetDir!=="target/bistro-browser-512")) throw new Error("scene options require --bistro or --fixture");
  if (bistro && (scaled || prepared || occlusion)) throw new Error("--bistro uses explicit fixture/settings instead of --scaled, --prepared or --occlusion");
  if ((baseline || bistro) && frames > 3600) throw new Error("--baseline --frames must not exceed 3600");
  if ((scaled || prepared || capturePath || output) && !baseline && !bistro) throw new Error("--scaled, --prepared, --capture and --output require --baseline or --bistro");
  const example = bistro ? "browser_bistro" : baseline ? "browser_baseline" : "browser_streaming";
  const buildMetadataPath = `${MEASURE_DIR}/${example}.build.json`;
  // Capture provenance at the build boundary, not when a saved binary is rerun.
  const buildSourceHash = skipBuild ? null : await measurementSourceHash();
  const steps: string[][] = [
    ["bun", "install", "--frozen-lockfile"],
    [
      "cargo", "build", "--release", "-p", "sgl-3d-examples", "--example", example,
      "--target", "wasm32-unknown-unknown",
    ],
    [
      "wasm-bindgen", "--target", "web", "--out-dir", MEASURE_DIR,
      `${process.env.CARGO_TARGET_DIR ?? "target"}/wasm32-unknown-unknown/release/examples/${example}.wasm`,
    ],
  ];
  for (const argv of skipBuild ? [] : steps) {
    const result = Bun.spawnSync(argv, { stderr: "inherit", stdout: "inherit" });
    if (result.exitCode !== 0) process.exit(result.exitCode ?? 1);
  }
  const moduleHash = new Bun.CryptoHasher("sha256").update(await Bun.file(`${MEASURE_DIR}/${example}_bg.wasm`).arrayBuffer()).digest("hex");
  if (!skipBuild) await Bun.write(buildMetadataPath, JSON.stringify({mode:"release",source_sha256:buildSourceHash,module_sha256:moduleHash}));
  const { runMeasurement, validateBaseline, validateScene } = await import("../browser/measurement");
  if (fixturePath) sceneOptions.fixture=await Bun.file(fixturePath).json();
  if (settingsPath) sceneOptions.settings=await Bun.file(settingsPath).json();
  if (bistro) sceneOptions.frames=frames;
  const result = await runMeasurement({page: bistro ? "bistro" : baseline ? "baseline" : "streaming", dist: MEASURE_DIR, capturePath, sequenceDir,
    assetDir:bistro?assetDir:undefined,run:bistro?sceneOptions:undefined,
    query: `frames=${frames}&across=${radius[0]}&up=${radius[1]}&occlusion=${occlusion ? 1 : 0}&scaled=${scaled ? 1 : 0}&prepared=${prepared ? 1 : 0}&capture=${capturePath ? 1 : 0}`});
  if (baseline || bistro) {
    const report = JSON.parse(result.report);
    if (bistro) {
      report.run_parameters={...sceneOptions,fixture:undefined,settings:undefined};
    }
    report.browser ??= {};
    report.browser.chromium_version = result.browserVersion;
    report.browser.errors = result.errors;
    const metadataFile = Bun.file(buildMetadataPath);
    const metadata = await metadataFile.exists() ? await metadataFile.json() : null;
    const matchingBuild = metadata?.module_sha256 === moduleHash ? metadata : null;
    report.build = {mode:matchingBuild?.mode ?? null, source_sha256:matchingBuild?.source_sha256 ?? null,
      module_sha256:moduleHash, current_checkout_sha256:await measurementSourceHash(),
      missing_reason:matchingBuild ? null : "No matching saved build metadata; binary build mode and source revision unknown"};
    if (output) await Bun.write(output, JSON.stringify(report, null, 2) + "\n");
    console.log(JSON.stringify(report, null, 2));
    (bistro ? validateScene : validateBaseline)(report);
    if (bistro && (output || capturePath || sequenceDir)) {
      const {dirname,join}=await import("node:path");
      const artifactDirectories=new Set([
        output ? dirname(output) : undefined,
        capturePath ? dirname(capturePath) : undefined,
        sequenceDir,
      ].filter((directory): directory is string => typeof directory==="string"));
      for (const name of report.fixture.notices ?? []) {
        if (typeof name!=="string" || !/^[a-zA-Z0-9_.-]+$/.test(name)) throw new Error("notice must be an asset basename");
        for (const directory of artifactDirectories) {
          await Bun.write(join(directory,`${report.scene}-${name}`),Bun.file(join(assetDir,name)));
        }
      }
    }
    if (result.errors.length) throw new Error(result.errors.join("\n"));
  } else {
    console.log(result.report.trimEnd());
    if (result.report.startsWith("FAIL") || result.errors.length) process.exitCode = 1;
  }
}

if (task === "measure-browser") {
  await measureBrowser(options);
}

// Evidence provenance only; this does not reject content or trigger any bake.
async function measurementSourceHash(): Promise<string> {
  const paths = ["Cargo.toml", "Cargo.lock", "examples/sgl-3d/Cargo.toml", "examples/sgl-3d/examples/browser_baseline.rs", "examples/sgl-3d/examples/browser_bistro.rs", "examples/sgl-3d/examples/support/bistro_scene.rs", "browser/measurement.ts", "browser/bistro.html", "scripts/tasks.ts"];
  for await (const path of new Bun.Glob("crates/**/*.{rs,wgsl,toml}").scan(".")) paths.push(path);
  const hash = new Bun.CryptoHasher("sha256");
  for (const path of paths.sort()) { hash.update(path); hash.update(await Bun.file(path).arrayBuffer()); }
  return hash.digest("hex");
}
