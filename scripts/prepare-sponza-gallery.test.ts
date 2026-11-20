import { expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { galleryOptions, prepareSponzaGallery, recipeName } from "./prepare-sponza-gallery";
const sourceNames = ["Sponza.glb", "sky-environment.json", "sky-panorama.png", "sky-pmrem.rgba16"];
async function withPrepared(body: (prepared: string) => Promise<void>) {
  const prepared = await mkdtemp(join(tmpdir(), "sgl-sponza-gallery-"));
  try {
    for (const name of sourceNames) await writeFile(join(prepared, name), `original ${name}`);
    await body(prepared);
  } finally { await rm(prepared, { recursive: true, force: true }); }
}
// Stands in for the native example: captures each placement of the file it
// is handed, writing NAME.rgba16 and NAME.json, except those in `skip`.
function fakeCapture(captured: string[], skip: (payload: string, index: number) => boolean = () => false) {
  return async (args: string[]) => {
    const placements: { rgba16: string }[] = JSON.parse(await readFile(args[args.indexOf("--capture-probe") + 1], "utf8")).specular_probes;
    const work = args[args.indexOf("--output") + 1];
    for (const [index, { rgba16 }] of placements.entries()) {
      if (skip(rgba16, index)) continue;
      captured.push(rgba16);
      await writeFile(join(work, rgba16), `payload of ${rgba16}`);
      await writeFile(join(work, rgba16.replace(/\.rgba16$/, ".json")), JSON.stringify({ capture: rgba16 }));
    }
  };
}
test("gallery export accepts only an explicit prepared directory", () => {
  expect(galleryOptions([]).endsWith(".cache/sponza/prepared")).toBe(true);
  expect(galleryOptions(["--prepared", "/tmp/local-scene"])).toBe("/tmp/local-scene");
  for (const args of [["--prepared"], ["--unknown", "x"], ["--prepared", "--other"]]) expect(() => galleryOptions(args)).toThrow();
});
test("failed gallery capture preserves prior outputs and input assets", async () => {
  await withPrepared(async prepared => {
    await writeFile(join(prepared, recipeName), "previous recipe");
    await expect(prepareSponzaGallery(prepared, async () => { throw new Error("capture failed"); })).rejects.toThrow("capture failed");
    for (const name of sourceNames) expect(await readFile(join(prepared, name), "utf8")).toBe(`original ${name}`);
    expect(await readFile(join(prepared, recipeName), "utf8")).toBe("previous recipe");
    expect((await readdir(prepared)).some(name => name.startsWith(".gallery-export-"))).toBe(false);
  });
});
test("gallery export publishes every captured probe and a recipe that names each", async () => {
  await withPrepared(async prepared => {
    const captured: string[] = [];
    await prepareSponzaGallery(prepared, fakeCapture(captured));
    expect(captured.length).toBeGreaterThan(1);
    for (const name of sourceNames) expect(await readFile(join(prepared, name), "utf8")).toBe(`original ${name}`);
    const recipe = JSON.parse(await readFile(join(prepared, recipeName), "utf8"));
    expect(recipe.specular_probes.map((probe: { rgba16: string }) => probe.rgba16).sort()).toEqual([...captured].sort());
    for (const payload of captured) {
      const metadata = payload.replace(/\.rgba16$/, ".json");
      expect(await readFile(join(prepared, payload), "utf8")).toBe(`payload of ${payload}`);
      expect(JSON.parse(await readFile(join(prepared, metadata), "utf8")).capture).toBe(payload);
      expect(recipe.notices).toContain(metadata);
    }
  });
});
test("a capture that misses one probe publishes nothing", async () => {
  await withPrepared(async prepared => {
    await writeFile(join(prepared, recipeName), "previous recipe");
    await expect(prepareSponzaGallery(prepared, fakeCapture([], (_, index) => index === 1))).rejects.toThrow("Capture did not write");
    expect(await readFile(join(prepared, recipeName), "utf8")).toBe("previous recipe");
    expect((await readdir(prepared)).filter(name => name.endsWith(".rgba16"))).toEqual(["sky-pmrem.rgba16"]);
  });
});
test("gallery export rejects output aliases before running capture", async () => {
  await withPrepared(async prepared => {
    await symlink(join(prepared, "Sponza.glb"), join(prepared, recipeName));
    let called = false;
    await expect(prepareSponzaGallery(prepared, async () => { called = true; })).rejects.toThrow();
    expect(called).toBe(false);
    expect(await readFile(join(prepared, "Sponza.glb"), "utf8")).toBe("original Sponza.glb");
  });
});
