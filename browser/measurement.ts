import { chromium } from "playwright";

/** The same real Chromium/WebGPU launch path for streaming and baseline. */
export async function runMeasurement(options: {
  page: "streaming" | "baseline";
  dist: string;
  query: string;
  timeoutMs?: number;
  capturePath?: string;
}): Promise<{ report: string; browserVersion: string; errors: string[] }> {
  const types: Record<string, string> = {
    ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".wasm": "application/wasm",
  };
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(request) {
      const path = new URL(request.url).pathname;
      // Serve only the generated module's basename, never arbitrary local paths.
      if (path !== "/" && !/^\/[a-zA-Z0-9_]+\.(js|wasm)$/.test(path)) {
        return new Response("not found", { status: 404 });
      }
      const file = path === "/" ? `browser/${options.page}.html` : `${options.dist}${path}`;
      const body = Bun.file(file);
      if (!(await body.exists())) return new Response("not found", { status: 404 });
      return new Response(body, { headers: {
        "content-type": types[path.slice(path.lastIndexOf("."))] ?? types[".html"],
        "cross-origin-opener-policy": "same-origin", "cross-origin-embedder-policy": "require-corp",
      }});
    },
  });
  let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
  try {
    browser = await chromium.launch({ channel: "chromium" });
    const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(`page error: ${error.stack ?? error}`));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(`console error: ${message.text()}`);
      if (message.type() === "warning") console.log(`console warning: ${message.text()}`);
    });
    await page.goto(`http://127.0.0.1:${server.port}/?${options.query}`);
    await page.waitForFunction(() => typeof (window as unknown as { __sglReport?: string }).__sglReport === "string",
      undefined, { timeout: options.timeoutMs ?? 15 * 60_000 });
    const report = await page.evaluate(() => (window as unknown as { __sglReport: string }).__sglReport);
    if (options.capturePath) {
      const parsed = JSON.parse(report);
      if (parsed.error) throw new Error(`capture unavailable: ${parsed.error}`);
      await page.locator("#capture").screenshot({path:options.capturePath});
    }
    return { report, browserVersion: browser.version(), errors };
  } finally {
    await browser?.close();
    server.stop(true);
  }
}

/** Structural/objective boundaries only: no image interpretation or speed threshold. */
export function validateBaseline(report: any): void {
  if (report.error) throw new Error(`baseline failed: ${report.error}`);
  if (report.schema !== 1 || report.scene !== "textured-grid-v1") throw new Error("unknown baseline schema/scene");
  if (!Number.isInteger(report.frames) || report.frames < 60) throw new Error("insufficient measured frames");
  for (const key of ["cpu_encode", "frame_complete_wall"]) {
    const value = report[key];
    if (value?.samples !== report.frames || ![value.median_ms,value.p95_ms,value.max_ms].every(Number.isFinite)
      || value.median_ms < 0 || value.p95_ms < value.median_ms || value.max_ms < value.p95_ms) {
      throw new Error(`invalid ${key} distribution`);
    }
  }
  if (report.errors?.length !== 0) throw new Error(`WebGPU errors: ${JSON.stringify(report.errors)}`);
  if (!(report.camera_draws?.max > 0)) throw new Error("scene encoded no camera draws");
  const gpu = report.gpu;
  if (!gpu || gpu.measured_frames + gpu.missing_frames !== report.frames) throw new Error("GPU sample accounting mismatch");
  if (gpu.supported && gpu.measured_frames === 0) throw new Error("timestamp feature enabled but no GPU timings returned");
  if (!gpu.supported && !gpu.missing_reason) throw new Error("missing timestamp reason");
  if (gpu.measured_frames > 0) {
    if (!Number.isFinite(gpu.total?.median_ms) || !Number.isFinite(gpu.total?.p95_ms)
      || Object.keys(gpu.passes ?? {}).length === 0) throw new Error("GPU timing distribution missing");
  }
}
