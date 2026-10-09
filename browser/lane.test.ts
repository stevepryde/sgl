// The browser lane (testing.md 5): the wasm probe built by
// `bun scripts/tasks.ts check-browser` runs in headless Chromium under
// Playwright against the native fixture server, and its report must read
// `ok` on every scenario. Lives outside `scripts/` so `bun test scripts`
// (part of `check`) never picks it up.
import { expect, test } from "bun:test";
import { chromium } from "playwright";

/// Pinned so the fixture server's exact Origin policy admits the page.
const HOST = "127.0.0.1";
const PORT = 8123;
const DIST = process.env.SGL_BROWSER_PROBE_DIR ?? "target/browser-probe";
const SCENARIOS = 8;

async function startFixture(): Promise<Bun.Subprocess> {
  const fixture = Bun.spawn(["cargo", "run", "-q", "-p", "sgl-net", "--example", "ws_fixture_server"], {
    stdout: "pipe",
    stderr: "inherit",
  });
  const reader = fixture.stdout.getReader();
  const deadline = Date.now() + 60_000;
  let banner = "";
  while (!banner.includes("listening")) {
    if (Date.now() > deadline) throw new Error("fixture server did not start");
    const { value, done } = await reader.read();
    if (done) throw new Error("fixture server exited before listening");
    banner += new TextDecoder().decode(value);
  }
  return fixture;
}

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
      const file = path === "/" ? "browser/index.html" : `${DIST}${path}`;
      const body = Bun.file(file);
      if (!(await body.exists())) return new Response("not found", { status: 404 });
      const type = types[path.slice(path.lastIndexOf("."))] ?? types[".html"];
      return new Response(body, { headers: { "content-type": type } });
    },
  });
}

test(
  "the browser WebSocket client passes every probe scenario in headless Chromium",
  async () => {
    // Everything that must be torn down is started inside the `try`, so a
    // launch failure (typically Chromium not installed for the pinned
    // Playwright: `bunx playwright install chromium`) cannot leak the
    // fixture server.
    let fixture: Bun.Subprocess | undefined;
    let server: ReturnType<typeof servePage> | undefined;
    let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
    try {
      fixture = await startFixture();
      server = servePage();
      browser = await chromium.launch();
      const page = await browser.newPage();
      page.on("pageerror", (error) => console.error(`page error: ${error}`));
      await page.goto(`http://${HOST}:${PORT}/`);
      const deadline = Date.now() + 90_000;
      let report: string | null = null;
      while (report === null) {
        if (Date.now() > deadline) throw new Error("the probe never reported");
        report = await page.evaluate(() => (window as unknown as { __sglReport?: string }).__sglReport ?? null);
        if (report === null) await new Promise((resolve) => setTimeout(resolve, 500));
      }
      console.log(report.trimEnd());
      const lines = report.trim().split("\n");
      expect(lines.filter((line) => line.startsWith("FAIL"))).toEqual([]);
      expect(lines.filter((line) => line.startsWith("ok ")).length).toBe(SCENARIOS);
    } finally {
      await browser?.close();
      server?.stop(true);
      fixture?.kill();
      await fixture?.exited;
    }
  },
  120_000,
);
