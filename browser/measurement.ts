import { chromium } from "playwright";

/** The same real Chromium/WebGPU launch path for streaming and baseline. */
export async function runMeasurement(options: {
  page: "streaming" | "baseline" | "bistro";
  assetDir?: string;
  run?: Record<string, unknown>;
  dist: string;
  query: string;
  timeoutMs?: number;
  capturePath?: string;
  sequenceDir?: string;
}): Promise<{ report: string; browserVersion: string; errors: string[] }> {
  const types: Record<string, string> = {
    ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".wasm": "application/wasm",
    ".json":"application/json", ".glb":"model/gltf-binary", ".ktx2":"image/ktx2", ".png":"image/png", ".txt":"text/plain; charset=utf-8", ".rgba16":"application/octet-stream",
  };
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(request) {
      const path = new URL(request.url).pathname;
      if (path === "/run.json" && options.page === "bistro") return Response.json(options.run ?? {});
      // Asset serving is confined to basenames, or the fixed Compressed/ folder,
      // inside the explicitly selected fixture directory.
      const asset = options.assetDir && /^\/assets\/(?:Compressed\/)?[a-zA-Z0-9_.-]+\.(glb|ktx2|json|png|rgba16|txt)$/.test(path);
      if (path !== "/" && !asset && !/^\/[a-zA-Z0-9_]+\.(js|wasm)$/.test(path)) {
        return new Response("not found", { status: 404 });
      }
      const file = path === "/" ? `browser/${options.page}.html` : asset ? `${options.assetDir}/${path.slice("/assets/".length)}` : `${options.dist}${path}`;
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
    let fail: (error: Error) => void = () => {};
    const failed = new Promise<never>((_,reject) => {fail=reject;});
    // Keep an early crash/panic observed even if it precedes the report wait.
    void failed.catch(() => {});
    page.on("pageerror", (error) => {errors.push(`page error: ${error.stack ?? error}`);fail(error);});
    page.on("crash", () => {const error=new Error("Chromium renderer crashed");errors.push(error.message);fail(error);});
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(`console error: ${message.text()}`);
      if (message.type() === "warning") console.log(`console warning: ${message.text()}`);
    });
    await page.goto(`http://127.0.0.1:${server.port}/?${options.query}`);
    let report: string;
    try {
      await Promise.race([page.waitForFunction(() => typeof (window as unknown as { __sglReport?: string }).__sglReport === "string",
        undefined, { timeout: options.timeoutMs ?? 15 * 60_000 }),failed]);
      report = await page.evaluate(() => (window as unknown as { __sglReport: string }).__sglReport);
    } catch (error) {
      let phase: string | null=null;
      try {phase=await page.evaluate(() => (window as unknown as {__sglPhase?:string}).__sglPhase ?? null);} catch {}
      report=JSON.stringify({schema:1,error:String(error),phase,browser:{phase,errors}});
    }
    if (options.capturePath) {
      const parsed = JSON.parse(report);
      if (parsed.error) return {report,browserVersion:browser.version(),errors};
      if (options.page === "bistro") {
        const data = await page.evaluate(() => (window as unknown as {__sglCapture:string}).__sglCapture);
        if (!data?.startsWith("data:image/png;base64,")) throw new Error("PNG readback missing");
        await Bun.write(options.capturePath, Buffer.from(data.slice("data:image/png;base64,".length),"base64"));
      } else await page.locator("#capture").screenshot({path:options.capturePath});
    }
    if (options.sequenceDir && !JSON.parse(report).error) {
      const frames = await page.evaluate(() => (window as unknown as {__sglSequence:string[]}).__sglSequence);
      if (!Array.isArray(frames) || frames.length!==JSON.parse(report).temporal_capture?.frames) throw new Error("temporal capture count mismatch");
      for (let i=0;i<frames.length;i++) {
        if (!frames[i].startsWith("data:image/png;base64,")) throw new Error("temporal PNG missing");
        await Bun.write(`${options.sequenceDir}/${String(i).padStart(3,"0")}.png`,Buffer.from(frames[i].slice("data:image/png;base64,".length),"base64"));
      }
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
  validateMeasurements(report);
}

function validateMeasurements(report: any): void {
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

/** Embedded scenes share baseline timing/error boundaries, not a visual oracle. */
export function validateScene(report: any): void {
  if (report.error) throw new Error(`scene failed at ${report.browser?.phase ?? report.phase ?? "unknown phase"}: ${report.error}`);
  if (report.schema !== 1 || report.benchmark !== "embedded-scene-v1" || report.fixture?.schema !== 1 || report.scene !== report.fixture.scene) throw new Error("unknown scene fixture");
  if (typeof report.scene!=="string" || !/^[a-zA-Z0-9_-]{1,128}$/.test(report.scene)) throw new Error("scene id must be a short stable basename");
  if (!(report.content?.triangles > 0) || !(report.content?.glb_bytes > 0)) throw new Error("empty embedded scene");
  if (report.capture?.width !== report.size?.[0] || report.capture?.height !== report.size?.[1] || !report.capture?.excluded_from_timing) throw new Error("invalid final readback");
  validateMeasurements(report);
}
