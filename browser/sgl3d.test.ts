// The browser lane (testing.md 5): SGL3D's smoke test, built by
// `bun scripts/tasks.ts check-browser`, renders its scene on the page's
// WebGPU device in headless Chromium under Playwright. Every configuration
// must report `ok`, and the page must log no error.
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
/// The adapter line and the smoke test's four configurations.
const REPORTS = 5;

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
      page.on("pageerror", (error) => errors.push(`page error: ${error}`));
      page.on("console", (message) => {
        if (message.type() === "warning") console.log(`console warning: ${message.text()}`);
        if (message.type() === "error") errors.push(`console error: ${message.text()}`);
      });
      await page.goto(`http://${HOST}:${PORT}/`);
      const deadline = Date.now() + 150_000;
      let report: string | null = null;
      while (report === null) {
        if (Date.now() > deadline) throw new Error("the smoke test never reported");
        report = await page.evaluate(() => (window as unknown as { __sglReport?: string }).__sglReport ?? null);
        if (report === null) await new Promise((resolve) => setTimeout(resolve, 500));
      }
      console.log(report.trimEnd());
      const lines = report.trim().split("\n");
      expect(lines.filter((line) => line.startsWith("FAIL"))).toEqual([]);
      expect(lines.filter((line) => line.startsWith("ok ")).length).toBe(REPORTS);
      expect(errors).toEqual([]);
    } finally {
      await browser?.close();
      server?.stop(true);
    }
  },
  180_000,
);
