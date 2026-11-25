// The browser lane (testing.md 5): SGL3D's smoke test, built by
// `bun scripts/tasks.ts check-browser`, renders its scene on the page's
// WebGPU device in headless Chromium under Playwright. Every configuration
// must report `ok`, and the page must log no error. The page runs on half
// the default wasm stack (`SMOKE_STACK_BYTES`), so "memory access out of
// bounds" most likely means that stack ran out (#893).
//
// Playwright's default headless browser, the headless shell, exposes no
// WebGPU adapter on macOS (only SwiftShader's, under --enable-unsafe-webgpu).
// The `chromium` channel runs the full browser in its new headless mode,
// which exposes the GPU's.
import { expect, test } from "bun:test";
import { chromium } from "playwright";

const HOST = "127.0.0.1";
const PORT = 8124;
const DIST = process.env.SGL_BROWSER_PROBE_DIR ?? "target/browser-probe";

function servePage() {
  const types: Record<string, string> = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript",
    ".wasm": "application/wasm",
  };
  return Bun.serve({
    hostname: HOST,
    port: PORT,
    async fetch(request) {
      const path = new URL(request.url).pathname;
      const file = path === "/" ? "browser/sgl3d.html" : `${DIST}${path}`;
      const body = Bun.file(file);
      if (!(await body.exists())) return new Response("not found", { status: 404 });
      const type = types[path.slice(path.lastIndexOf("."))] ?? types[".html"];
      return new Response(body, { headers: { "content-type": type } });
    },
  });
}

test(
  "SGL3D renders its smoke scene on WebGPU in headless Chromium",
  async () => {
    let server: ReturnType<typeof servePage> | undefined;
    let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
    try {
      server = servePage();
      browser = await chromium.launch({ channel: "chromium" });
      const page = await browser.newPage();
      const errors: string[] = [];
      page.on("pageerror", (error) => errors.push(`page error: ${error.stack ?? error}`));
      page.on("console", (message) => {
        if (message.type() === "warning") console.log(`console warning: ${message.text()}`);
        if (message.type() === "error") errors.push(`console error: ${message.text()}`);
      });
      await page.goto(`http://${HOST}:${PORT}/`);
      const deadline = Date.now() + 150_000;
      let report: string | null = null;
      while (report === null) {
        if (Date.now() > deadline) throw new Error(`the smoke test never reported${errors.length ? `:\n${errors.join("\n")}` : ""}`);
        report = await page.evaluate(() => (window as unknown as { __sglReport?: string }).__sglReport ?? null);
        if (report === null) await new Promise((resolve) => setTimeout(resolve, 500));
      }
      console.log(report.trimEnd());
      const lines = report.trim().split("\n");
      expect(lines.filter((line) => line.startsWith("FAIL"))).toEqual([]);
      expect(lines.some((line) => line.startsWith("ok specular probe capture:"))).toBe(true);
      expect(lines.some((line) => line.startsWith("ok HDR effect:"))).toBe(true);
      expect(lines.some((line) => line.startsWith("ok outline:"))).toBe(true);
      expect(lines.some((line) => line.startsWith("ok live specular probe:"))).toBe(true);
      // Compile jobs create with WebGPU's asynchronous calls: still pending in
      // the task that ran them, then the ordinary bytes once settled.
      expect(lines.some((line) => line.startsWith("ok asynchronous compile jobs:"))).toBe(true);
      expect(lines.some((line) => line.startsWith("ok browser image decode:"))).toBe(true);
      // sgl-2d's canvas device as the game requests it, and one device
      // running an SGL3D view with sgl-2d's HUD drawn over it.
      expect(lines.some((line) => line.startsWith("ok 2D canvas device request:"))).toBe(true);
      expect(lines.some((line) => line.startsWith("ok 2D HUD over the 3D view:"))).toBe(true);
      // The frame compiled ahead ran its jobs and passed the same checks.
      expect(lines.find((line) => line.includes("compiled ahead:"))).toMatch(/^ok .* compile jobs run ahead/);
      // Chromium grants texture-formats-tier2, which sgl-webgpu maps, so the
      // page's device runs the port's SDK-formats set; a device requested
      // without it runs the core set. A device that silently fell to the
      // core set would pass every other check.
      expect(lines).toContain("ok FSR resource set: SdkFormats");
      expect(lines).toContain("ok FSR resource set without tier2: Core");
      for (const method of ["FSR2", "FSR3"]) {
        const ran = lines.find((line) => line.startsWith(`ok ${method} Quality, core set:`));
        expect(ran).toContain(`antialiasing ${method[0]}${method.slice(1).toLowerCase()},`);
        expect(ran).not.toContain("FSR off");
      }
      // Chromium grants what FSR's resource sets need, so FSR runs:
      // the renderer's effective antialiasing is the one selected, with no
      // reason for a fallback, at Native AA and at an upscaling quality.
      for (const method of ["FSR2", "FSR3"]) {
        for (const quality of ["Native AA", "Quality"]) {
          const ran = lines.find((line) => line.startsWith(`ok ${method} ${quality}:`));
          expect(ran).toContain(`antialiasing ${method[0]}${method.slice(1).toLowerCase()},`);
          expect(ran).not.toContain("FSR off");
        }
      }
      expect(errors).toEqual([]);
    } finally {
      await browser?.close();
      server?.stop(true);
    }
  },
  180_000,
);
