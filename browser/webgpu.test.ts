// The browser lane (testing.md 5): sgl-webgpu's page, built by
// `bun scripts/tasks.ts check-browser`, runs each check on the page's
// WebGPU device in headless Chromium under Playwright, through the backend
// SGL's own pages do not reach. Every check must report `ok`, and the page
// must log no error but the one the surface error check causes. Runs the full browser for its GPU, as sgl3d.test.ts
// explains.
import { expect, test } from "bun:test";
import { chromium } from "playwright";

const HOST = "127.0.0.1";
const PORT = 8125;
const DIST = process.env.SGL_BROWSER_PROBE_DIR ?? "target/browser-probe";
const CHECKS = [
  "buffer map round trip",
  "write and copy texels",
  "compute storage writes",
  "render bundle and indirect draw",
  "occlusion queries",
  "multisample resolve",
  "external image copy",
  "external image colour space",
  "depth slice",
  "texture from a WebGPU handle",
  "surface error",
  "error scopes",
  "compilation info",
  "surface present",
  "texture format tiers",
];

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
      const file = path === "/" ? "browser/webgpu.html" : `${DIST}${path}`;
      const body = Bun.file(file);
      if (!(await body.exists())) return new Response("not found", { status: 404 });
      const type = types[path.slice(path.lastIndexOf("."))] ?? types[".html"];
      return new Response(body, { headers: { "content-type": type } });
    },
  });
}

test(
  "sgl-webgpu passes every backend check on WebGPU in headless Chromium",
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
      const deadline = Date.now() + 60_000;
      let report: string | null = null;
      while (report === null) {
        if (Date.now() > deadline) throw new Error(`the page never reported${errors.length ? `:\n${errors.join("\n")}` : ""}`);
        report = await page.evaluate(() => (window as unknown as { __sglReport?: string }).__sglReport ?? null);
        if (report === null) await new Promise((resolve) => setTimeout(resolve, 200));
      }
      console.log(report.trimEnd());
      const lines = report.trim().split("\n");
      expect(lines.filter((line) => line.startsWith("FAIL"))).toEqual([]);
      for (const check of CHECKS) {
        expect(lines.some((line) => line.startsWith(`ok ${check}:`)), check).toBe(true);
      }
      // `create_surface` on a canvas holding a 2D context reports the
      // browser's reason on the console (crates/sgl-webgpu/README.md).
      expect(errors).toEqual([
        "console error: sgl-webgpu: canvas.getContext() returned null; webgpu not available or canvas already in use",
      ]);
    } finally {
      await browser?.close();
      server?.stop(true);
    }
  },
  90_000,
);
