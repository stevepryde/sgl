import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

// Real HTTP + filesystem boundary: a download/excerpt failure must not overwrite
// retained licence text. A successful refresh must retain provenance and vendor bytes.
async function fixture(run: (root: string, base: string) => Promise<void>) {
  await mkdir("target", { recursive: true });
  const root = await mkdtemp(resolve("target/refresh-licenses-test-"));
  const server = Bun.serve({
    port: 0,
    fetch(request) {
      switch (new URL(request.url).pathname) {
        case "/license": return new Response("Original author notice\nPermission terms\n");
        case "/excerpt": return new Response("before\nSTART\nRetained attribution\nEND\nafter\n");
        default: return new Response("not found", { status: 404 });
      }
    },
  });
  try {
    for (const path of ["scripts", "licenses", "vendor", "crates/sgl-3d", "crates/sgl-post-fx"]) {
      await mkdir(resolve(root, path), { recursive: true });
    }
    for (const name of ["refresh-licenses.ts", "license-notices.ts"]) {
      await writeFile(resolve(root, "scripts", name), await readFile(resolve(import.meta.dir, name)));
    }
    await run(root, server.url.toString());
  } finally {
    server.stop(true);
    await rm(root, { recursive: true, force: true });
  }
}
async function refresh(root: string) {
  const child = Bun.spawn(["bun", "scripts/refresh-licenses.ts"], { cwd: root, stdout: "pipe", stderr: "pipe" });
  const [status, stdout, stderr] = await Promise.all([
    child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
  ]);
  return { status, output: stdout + stderr };
}

test("refresh leaves licences intact when a later upstream download fails", async () => {
  await fixture(async (root, base) => {
    await writeFile(resolve(root, "vendor/LICENSE"), "retained licence\n");
    await writeFile(resolve(root, "licenses/upstreams.json"), JSON.stringify([
      { component: "Fixture", path: "vendor/LICENSE", url: base + "license" },
    ]));
    const supplements = JSON.stringify([{ source: base + "missing", text: "retained supplement" }]);
    await writeFile(resolve(root, "licenses/supplemental-notices.json"), supplements);
    const result = await refresh(root);
    expect(result.status).not.toBe(0);
    expect(result.output).toContain("HTTP 404");
    expect(await readFile(resolve(root, "vendor/LICENSE"), "utf8")).toBe("retained licence\n");
    expect(await readFile(resolve(root, "licenses/supplemental-notices.json"), "utf8")).toBe(supplements);
  });
});

test("refresh preserves provenance, excerpts attribution and avoids rewriting unchanged vendor bytes", async () => {
  await fixture(async (root, base) => {
    await writeFile(resolve(root, "vendor/LICENSE"), "old licence\n");
    await writeFile(resolve(root, "vendor/UNCHANGED"), "Original author notice\r\nPermission terms\r\n");
    await writeFile(resolve(root, "licenses/upstreams.json"), JSON.stringify([
      { component: "Fixture", path: "vendor/LICENSE", url: base + "license", prefix: "Ported from fixture.\n\n" },
      { component: "Fixture", path: "vendor/UNCHANGED", url: base + "license" },
    ]));
    await writeFile(resolve(root, "licenses/supplemental-notices.json"), JSON.stringify([
      { source: base + "excerpt", text: "old", start: "START\n", end: "END" },
    ]));
    const result = await refresh(root);
    expect(result.status).toBe(0);
    expect(await readFile(resolve(root, "vendor/LICENSE"), "utf8"))
      .toBe("Ported from fixture.\n\nOriginal author notice\nPermission terms\n");
    expect(await readFile(resolve(root, "vendor/UNCHANGED"), "utf8"))
      .toBe("Original author notice\r\nPermission terms\r\n");
    const [supplement] = JSON.parse(await readFile(resolve(root, "licenses/supplemental-notices.json"), "utf8"));
    expect(supplement.text).toBe("START\nRetained attribution\n");
  });
});
