// The startup comparison's runner (examples/sgl-3d/BROWSER_BASELINE.md,
// "Startup against three.js"): SGL and three.js r187 load the same scene in
// Playwright's `chromium` channel, cold in a fresh profile and then warm in
// the same browser, with the same scripted key presses. Not part of `check`.
import { chromium } from "playwright";
import { mkdirSync, rmSync } from "node:fs";
import { glb, layout, type SceneName } from "./startup-fixture";

export type Library = "sgl" | "three";
export type StartupOptions = {
  /** wasm-bindgen output holding browser_baseline.js and its module. */
  wasmDir: string;
  /** Where the fixture and the three.js bundle are written. */
  dir: string;
  scenes: SceneName[];
  runs: number;
  path: "ordinary" | "prepared";
  captureDir?: string;
};

const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".wasm": "application/wasm",
  ".glb": "model/gltf-binary", ".json": "application/json",
};

export async function runStartup(options: StartupOptions) {
  mkdirSync(options.dir, { recursive: true });
  for (const scene of options.scenes) {
    await Bun.write(`${options.dir}/${scene}.glb`, glb(scene));
    await Bun.write(`${options.dir}/${scene}.json`, JSON.stringify(layout(scene)));
  }
  // A production three.js page ships one bundled, minified module.
  const bundle = await Bun.build({ entrypoints: ["browser/startup-three.js"], outdir: options.dir, naming: "three-app.js", minify: true, target: "browser", format: "esm" });
  if (!bundle.success) throw new Error(`three.js bundle failed: ${bundle.logs.join("\n")}`);
  const files: Record<string, string> = {
    "/sgl.html": "browser/startup-sgl.html", "/three.html": "browser/startup-three.html",
    "/startup-harness.js": "browser/startup-harness.js", "/three-app.js": `${options.dir}/three-app.js`,
    "/browser_baseline.js": `${options.wasmDir}/browser_baseline.js`, "/browser_baseline_bg.wasm": `${options.wasmDir}/browser_baseline_bg.wasm`,
  };
  for (const scene of options.scenes) for (const ext of ["glb", "json"]) files[`/${scene}.${ext}`] = `${options.dir}/${scene}.${ext}`;
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(request) {
      const path = new URL(request.url).pathname;
      const file = files[path] && Bun.file(files[path]);
      if (!file || !(await file.exists())) return new Response("not found", { status: 404 });
      // Cacheable, so the warm load finds every file in the HTTP cache.
      return new Response(file, { headers: { "content-type": TYPES[path.slice(path.lastIndexOf("."))], "cache-control": "max-age=3600",
        "cross-origin-opener-policy": "same-origin", "cross-origin-embedder-policy": "require-corp" } });
    },
  });
  const loads: any[] = [];
  let browserVersion = "";
  try {
    for (let run = 0; run < options.runs; run++) {
      // Alternate which library goes first, so drift does not favour one.
      const order: Library[] = run % 2 === 0 ? ["sgl", "three"] : ["three", "sgl"];
      for (const scene of options.scenes) for (const library of order) {
        // On a shared machine a headless browser occasionally stops
        // responding; its pair is measured again in a fresh browser.
        for (let attempt = 1; ; attempt++) {
          const pair = await measurePair({ origin: `http://127.0.0.1:${server.port}`, scene, library, path: options.path,
            profiles: `${options.dir}/profiles`, captureDir: run === 0 ? options.captureDir : undefined });
          if (pair.browserVersion) browserVersion = pair.browserVersion;
          for (const report of pair.reports) {
            Object.assign(report, { run, scene, library, attempt });
            console.error(`run ${run + 1}/${options.runs} ${scene} ${library} ${report.cache}: ${report.failed ?? report.errors?.[0] ?? `first visible ${report.first_contentful_paint_ms?.toFixed(1)} ms`}`);
          }
          // A page that ran and reported errors is a result, not a stall.
          const stalled = pair.reports.length < 2 || pair.reports.some((r: any) => r.failed);
          if (!stalled || attempt === 3) {
            loads.push(...pair.reports);
            break;
          }
          console.error(`run ${run + 1}/${options.runs} ${scene} ${library}: attempt ${attempt} stalled; retrying in a fresh browser`);
        }
      }
    }
  } finally {
    server.stop(true);
  }
  const gzip: Record<string, number> = {};
  for (const [path, file] of Object.entries(files)) gzip[path] = Bun.gzipSync(new Uint8Array(await Bun.file(file).arrayBuffer())).length;
  return summarize(loads, { browserVersion, gzip, options });
}

type Pair = { origin: string; scene: SceneName; library: Library; path: string; profiles: string; captureDir?: string };

/**
 * One fresh-profile browser (empty HTTP, code and Dawn shader caches) loads
 * the page cold, then warm. It runs in a child process with a deadline:
 * on a shared machine Playwright under Bun occasionally stops driving a
 * browser, and killing the child and its tagged browser keeps the run going.
 */
async function measurePair(pair: Pair): Promise<{ browserVersion?: string; reports: any[] }> {
  // Chromium ignores an unknown switch; it names this browser's processes.
  const tag = `sgl-startup-${crypto.randomUUID()}`;
  const child = Bun.spawn(["bun", import.meta.path, JSON.stringify({ ...pair, tag })], { stdout: "pipe", stderr: "inherit" });
  const timer = setTimeout(() => child.kill(9), 240_000);
  const output = await new Response(child.stdout).text();
  await child.exited;
  clearTimeout(timer);
  Bun.spawnSync(["pkill", "-9", "-f", tag]);
  rmSync(`${pair.profiles}/${tag}`, { recursive: true, force: true });
  try {
    return JSON.parse(output.trim().split("\n").at(-1) ?? "");
  } catch {
    return { reports: [{ failed: `browser stalled or exited (${child.signalCode ?? child.exitCode})`, cache: "cold" }] };
  }
}

if (import.meta.main) {
  const pair: Pair & { tag: string } = JSON.parse(Bun.argv[2]);
  // A new profile directory: its disk cache starts empty and keeps files
  // of every size for the warm load, as a player's browser does.
  const context = await chromium.launchPersistentContext(`${pair.profiles}/${pair.tag}`, { channel: "chromium", timeout: 60_000,
    args: [`--${pair.tag}`], viewport: { width: 1280, height: 720 }, deviceScaleFactor: 1 });
  const page = context.pages()[0] ?? await context.newPage();
  const browserVersion = (await (await context.newCDPSession(page)).send("Browser.getVersion")).product;
  const reports = [];
  for (const cache of ["cold", "warm"] as const) {
    const report = await load(page, `${pair.origin}/${pair.library}.html?scene=${pair.scene}&path=${pair.path}`, layout(pair.scene).steps);
    reports.push({ ...report, cache });
    if (report.failed) break;
    if (pair.captureDir) {
      mkdirSync(pair.captureDir, { recursive: true });
      await page.locator("#view").screenshot({ path: `${pair.captureDir}/${pair.scene}-${pair.library}-${cache}.png` });
    }
  }
  console.log(JSON.stringify({ browserVersion, reports }));
  await Promise.race([context.close(), new Promise((resolve) => setTimeout(resolve, 10_000))]);
  process.exit(0);
}

async function load(page: any, url: string, steps: { key: string }[]) {
  const errors: string[] = [], warnings: string[] = [];
  const onError = (error: Error) => errors.push(`page error: ${error.stack ?? error}`);
  const onConsole = (message: any) => {
    if (message.type() === "error") errors.push(`console error: ${message.text()}`);
    if (message.type() === "warning") warnings.push(message.text());
  };
  page.on("pageerror", onError);
  page.on("console", onConsole);
  try {
    // Each load starts from an empty page, not from the last one's frames.
    await page.goto("about:blank");
    await page.goto(url);
    await page.waitForFunction(() => ["running", "failed"].includes((window as any).__startup?.state), undefined, { timeout: 180_000, polling: 250 });
    for (let i = 0; i < steps.length; i++) {
      if (await page.evaluate(() => (window as any).__startup.state) === "failed") break;
      await page.keyboard.press(steps[i].key);
      await page.waitForFunction((i: number) => (window as any).__startup.stepDone(i), i, { timeout: 120_000, polling: 250 });
    }
    // Event Timing entries arrive after the frame that ends them is presented.
    await page.waitForTimeout(300);
    const report = JSON.parse(await page.evaluate(() => (window as any).__startup.finish()));
    await page.evaluate(() => (window as any).__startup.stop());
    return { ...report, page_errors: errors, console_warnings: warnings };
  } catch (error) {
    return { failed: String(error), page_errors: errors, console_warnings: warnings };
  } finally {
    page.off("pageerror", onError);
    page.off("console", onConsole);
  }
}

/** Phase boundaries from one load, in ms; `null` where a library has no such boundary. */
export function phases(report: any) {
  const m = report.marks;
  const code = report.resources.filter((r: any) => /\.(js|wasm)$/.test(r.name));
  const start = m.prepared ?? m.view_ready;
  return {
    document_received: report.resources[0].response_end_ms,
    code_received: Math.max(...code.map((r: any) => r.response_end_ms)),
    code_ready: m.code_ready,
    device: m.device_ready - m.code_ready,
    asset_fetch: m.asset_fetched - m.device_ready,
    asset_decode: m.asset_decoded - m.asset_fetched,
    asset_decode_longest_frame_gap: report.asset_decode_longest_frame_gap_ms ?? null,
    asset_upload: m.asset_uploaded - m.asset_decoded,
    view_setup: m.view_ready - m.asset_uploaded,
    prepare: m.prepared === undefined ? null : m.prepared - m.view_ready,
    wait_for_first_frame: m.first_frame_start - start,
    first_frame_encode_submit: m.first_submitted - m.first_frame_start,
    first_frame_gpu_complete_proxy: m.first_gpu_complete - m.first_submitted,
    first_frame_presented_after_submit: report.first_contentful_paint_ms - m.first_submitted,
    first_visible_world: report.first_contentful_paint_ms,
    startup_longest_frame_gap: report.startup_longest_frame_gap_ms,
    // SGL's own split of asset_upload's CPU time.
    ...Object.fromEntries(Object.entries(report.details?.upload_cpu_ms ?? {}).map(([k, v]) => [`asset_upload_cpu_${k}`, v])),
  };
}

function stepName(step: any, index: number) {
  return `${index + 1} ${step.kind}${step.first_use === true ? " (first use)" : step.first_use === false ? " (repeat)" : ""}`;
}

function inputs(report: any) {
  return Object.fromEntries(report.steps.map((s: any, i: number) => [stepName(s, i), {
    visible_response: s.presented_ms === null ? null : s.presented_ms - s.event_ms,
    event_to_submit: s.submitted_ms - s.event_ms,
    submit_to_gpu_complete_proxy: s.gpu_complete_ms - s.submitted_ms,
    longest_frame_gap: s.longest_frame_gap_ms,
  }]));
}

function spread(values: (number | null)[]) {
  const known = values.filter((v): v is number => v !== null && Number.isFinite(v)).sort((a, b) => a - b);
  if (known.length === 0) return { samples: 0, missing: values.length };
  return { samples: known.length, missing: values.length - known.length, median: known[(known.length - 1) >> 1], min: known[0], max: known[known.length - 1] };
}

function summarize(loads: any[], context: { browserVersion: string; gzip: Record<string, number>; options: StartupOptions }) {
  const ok = loads.filter((l) => !l.failed);
  const groups: Record<string, any> = {};
  for (const scene of context.options.scenes) for (const library of ["sgl", "three"]) for (const cache of ["cold", "warm"]) {
    const group = ok.filter((l) => l.scene === scene && l.library === library && l.cache === cache);
    if (group.length === 0) continue;
    const per = group.map(phases);
    const steps = group.map(inputs);
    const delivered = group.map((l) => l.resources.reduce((sum: number, r: any) => sum + r.transfer_bytes, 0));
    groups[`${scene}/${library}/${cache}`] = {
      loads: group.length,
      phases_ms: Object.fromEntries(Object.keys(per[0]).map((k) => [k, spread(per.map((p: any) => p[k]))])),
      inputs_ms: Object.fromEntries(Object.keys(steps[0]).map((name) => [name, Object.fromEntries(Object.keys(steps[0][name]).map((k) =>
        [k, spread(steps.map((s: any) => s[name]?.[k] ?? null))]))])),
      delivered_bytes: spread(delivered),
      files: group[0].resources.map((r: any) => ({ name: r.name, transfer_bytes: r.transfer_bytes, encoded_bytes: r.encoded_bytes,
        decoded_bytes: r.decoded_bytes, gzip_bytes_not_delivered: context.gzip[r.name] ?? null })),
    };
  }
  const first = (library: string) => ok.find((l) => l.library === library);
  return {
    schema: 1,
    benchmark: "startup-v1",
    path: context.options.path,
    runs: context.options.runs,
    browser: { chromium_version: context.browserVersion, user_agent: ok[0]?.browser.user_agent, device_pixel_ratio: ok[0]?.browser.device_pixel_ratio,
      cross_origin_isolated: ok[0]?.browser.cross_origin_isolated },
    adapter: { sgl: first("sgl")?.device.adapter ?? null, three: first("three")?.device.adapter ?? null },
    backend: { sgl: first("sgl")?.details.adapter ?? null, three: first("three") ? { renderer: first("three").device.backend, revision: first("three").device.revision } : null },
    sgl_canvas: first("sgl")?.details.canvas ?? null,
    groups,
    loads,
  };
}

/** Fails a comparison that cannot be trusted; speed is never judged here. */
export function validateStartup(summary: any): void {
  const problems: string[] = [];
  for (const l of summary.loads) {
    const where = `${l.scene ?? "?"}/${l.library ?? "?"}/${l.cache} run ${l.run}`;
    if (l.failed) { problems.push(`${where}: ${l.failed}`); continue; }
    const errors = [...l.errors, ...l.page_errors, ...(l.details?.errors ?? [])];
    if (errors.length) problems.push(`${where}: errors ${JSON.stringify(errors)}`);
    // A silent WebGL2 fallback in three.js would invalidate every number.
    if (l.device?.backend !== "WebGPU") problems.push(`${where}: backend ${l.device?.backend}, not WebGPU`);
    if (l.library === "three" && l.device.revision !== "187") problems.push(`${where}: three.js r${l.device.revision}, not r187`);
    if (l.first_contentful_paint_ms === null) problems.push(`${where}: no First Contentful Paint`);
    else if (!(l.first_contentful_paint_ms >= l.marks.first_submitted)) problems.push(`${where}: First Contentful Paint precedes the first world frame`);
    if (l.steps.length !== layout(l.scene).steps.length || l.steps.some((s: any) => s.gpu_complete_ms === undefined)) problems.push(`${where}: scripted inputs incomplete`);
  }
  const adapters = new Set(summary.loads.filter((l: any) => !l.failed).map((l: any) => JSON.stringify(l.device.adapter)));
  if (adapters.size !== 1) problems.push(`the libraries ran on different adapters: ${[...adapters].join(" vs ")}`);
  if (problems.length) throw new Error(`startup comparison invalid:\n${problems.join("\n")}`);
}

/** Median (min–max) tables for a terminal or an issue. */
export function table(summary: any): string {
  // Only a visible response can be missing for a reason: Event Timing reports 16 ms or more.
  const cell = (s: any, response = false) => {
    if (!s || s.samples === 0) return response && s?.missing ? "<16" : "—";
    const range = `${s.median.toFixed(1)} (${s.min.toFixed(1)}–${s.max.toFixed(1)})`;
    return s.missing ? `${range}, ${s.missing}× ${response ? "<16" : "missing"}` : range;
  };
  const out: string[] = [];
  for (const scene of summary.groups ? [...new Set(Object.keys(summary.groups).map((k) => k.split("/")[0]))] : []) {
    const columns = ["sgl/cold", "three/cold", "sgl/warm", "three/warm"].filter((c) => summary.groups[`${scene}/${c}`]);
    const groups = columns.map((c) => summary.groups[`${scene}/${c}`]);
    out.push(`\n### ${scene} (ms from navigation start for the first rows, else durations; median (min–max) of ${summary.runs} runs)\n`);
    out.push(`| Phase | ${columns.join(" | ")} |`, `| --- |${columns.map(() => " ---: |").join("")}`);
    for (const key of Object.keys(groups[0].phases_ms)) out.push(`| ${key} | ${groups.map((g) => cell(g.phases_ms[key])).join(" | ")} |`);
    for (const name of Object.keys(groups[0].inputs_ms)) for (const key of Object.keys(groups[0].inputs_ms[name])) {
      out.push(`| input ${name}: ${key} | ${groups.map((g) => cell(g.inputs_ms[name]?.[key], key === "visible_response")).join(" | ")} |`);
    }
    out.push(`| delivered bytes | ${groups.map((g) => g.delivered_bytes.median).join(" | ")} |`);
    out.push(`| gzip bytes (not delivered) | ${groups.map((g) => g.files.reduce((sum: number, f: any) => sum + (f.gzip_bytes_not_delivered ?? 0), 0)).join(" | ")} |`);
  }
  return out.join("\n");
}
