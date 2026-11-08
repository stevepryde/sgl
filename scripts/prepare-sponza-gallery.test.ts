import { expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { galleryFiles, galleryOptions, prepareSponzaGallery } from "./prepare-sponza-gallery";
const sourceNames = ["Sponza.glb", "sky-environment.json", "sky-panorama.png", "sky-pmrem.rgba16"];
async function withPrepared(body: (prepared: string) => Promise<void>) {
  const prepared = await mkdtemp(join(tmpdir(), "sgl-sponza-gallery-"));
  try {
    for (const name of sourceNames) await writeFile(join(prepared, name), `original ${name}`);
    await body(prepared);
  } finally { await rm(prepared, { recursive: true, force: true }); }
}
test("gallery export accepts only an explicit prepared directory", () => {
  expect(galleryOptions([]).endsWith(".cache/sponza/prepared")).toBe(true);
  expect(galleryOptions(["--prepared", "/tmp/local-scene"])).toBe("/tmp/local-scene");
  for (const args of [["--prepared"], ["--unknown", "x"], ["--prepared", "--other"]]) expect(() => galleryOptions(args)).toThrow();
});
test("failed gallery capture preserves prior outputs and input assets", async () => {
  await withPrepared(async prepared => {
    for (const name of galleryFiles) await writeFile(join(prepared, name), `previous ${name}`);
    await expect(prepareSponzaGallery(prepared, async () => { throw new Error("capture failed"); })).rejects.toThrow("capture failed");
    for (const name of sourceNames) expect(await readFile(join(prepared, name), "utf8")).toBe(`original ${name}`);
    for (const name of galleryFiles) expect(await readFile(join(prepared, name), "utf8")).toBe(`previous ${name}`);
    expect((await readdir(prepared)).some(name => name.startsWith(".gallery-export-"))).toBe(false);
  });
});
test("gallery export publishes payload and provenance with a usable derived recipe", async () => {
  await withPrepared(async prepared => {
    await prepareSponzaGallery(prepared, async args => {
      expect(args).toContain("--capture-probe");
      const work = args.at(-1)!;
      await writeFile(join(work, "sponza-back-gallery-probe.rgba16"), "captured HDR payload");
      await writeFile(join(work, "specular-probe.json"), JSON.stringify({ capture: "retained actual recipe" }));
    });
    for (const name of sourceNames) expect(await readFile(join(prepared, name), "utf8")).toBe(`original ${name}`);
    const recipe = JSON.parse(await readFile(join(prepared, "sponza-gallery.json"), "utf8"));
    expect(recipe.specular_probe.rgba16).toBe("sponza-back-gallery-probe.rgba16");
    expect(recipe.notices).toContain("sponza-gallery-probe.json");
    expect(recipe.dynamic_gi_volume.probes).toEqual([21, 13, 15]);
    expect(JSON.parse(await readFile(join(prepared, "sponza-gallery-probe.json"), "utf8")).capture).toBe("retained actual recipe");
  });
});
test("gallery export rejects output aliases before running capture", async () => {
  await withPrepared(async prepared => {
    await symlink(join(prepared, "Sponza.glb"), join(prepared, galleryFiles[0]));
    let called = false;
    await expect(prepareSponzaGallery(prepared, async () => { called = true; })).rejects.toThrow();
    expect(called).toBe(false);
    expect(await readFile(join(prepared, "Sponza.glb"), "utf8")).toBe("original Sponza.glb");
  });
});
