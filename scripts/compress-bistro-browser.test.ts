import { expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { alphaSensitiveImages, compressible, imageRoles, withCompressionScratch } from "./compress-bistro-browser";

test("concurrent exports own distinct scratch and clean up only their own directory on failure", async () => {
  const parent = await mkdtemp(join(tmpdir(), "bistro-compression-test-"));
  try {
    const old = join(parent, ".bistro-compress-bc7-512");
    await mkdir(old);
    await writeFile(join(old, "keep"), "preexisting");
    let firstScratch = "", secondScratch = "";
    let started!: () => void, finish!: () => void;
    const firstStarted = new Promise<void>((resolve) => { started = resolve; });
    const finishFirst = new Promise<void>((resolve) => { finish = resolve; });
    const first = withCompressionScratch(join(parent, "first"), async (directory) => {
      firstScratch = directory;
      await writeFile(join(directory, "first"), "still running");
      started();
      await finishFirst;
      expect(await readFile(join(directory, "first"), "utf8")).toBe("still running");
    });
    await firstStarted;
    try {
      await expect(withCompressionScratch(join(parent, "second"), async (directory) => {
        secondScratch = directory;
        expect(directory).not.toBe(firstScratch);
        await writeFile(join(directory, "second"), "temporary");
        throw new Error("synthetic encoder failure");
      })).rejects.toThrow("synthetic encoder failure");
      expect(await Bun.file(join(firstScratch, "first")).exists()).toBe(true);
      expect(await Bun.file(join(secondScratch, "second")).exists()).toBe(false);
    } finally { finish(); await first; }
    expect(await readdir(parent)).toEqual([".bistro-compress-bc7-512"]);
    expect(await readFile(join(old, "keep"), "utf8")).toBe("preexisting");
  } finally { await rm(parent, { recursive: true, force: true }); }
});

test("compression uses post-dedup material roles and preserves mixed-role filtering", () => {
  const roles = imageRoles({ textures: [{ source: 2 }, { source: 0 }, { source: 1 }], materials: [
    { pbrMetallicRoughness: { baseColorTexture: { index: 1 }, metallicRoughnessTexture: { index: 2 } }, normalTexture: { index: 0 } },
  ] }, [0, 0, 1]);
  expect([...roles.get(0)!].sort()).toEqual(["color", "data"]);
  expect([...roles.get(1)!]).toEqual(["normal"]);
  expect(compressible({ width: 512, height: 512 }, roles.get(0)!)).toBe(false);
  expect(compressible({ width: 512, height: 512 }, roles.get(1)!)).toBe(true);
  expect(compressible({ width: 16, height: 16 }, new Set(["color"])) ).toBe(false);
  expect(compressible({ width: 510, height: 512 }, new Set(["data"])) ).toBe(false);
});

test("MASK/BLEND alpha-sensitive base maps remain separate from opaque and normal maps", () => {
  const document = { textures: [{ source: 2 }, { source: 0 }, { source: 1 }], materials: [
    { alphaMode: "MASK", pbrMetallicRoughness: { baseColorTexture: { index: 0 } } },
    { alphaMode: "BLEND", pbrMetallicRoughness: { baseColorTexture: { index: 2 } } },
    { alphaMode: "OPAQUE", pbrMetallicRoughness: { baseColorTexture: { index: 1 } } },
  ] };
  expect([...alphaSensitiveImages(document, [0, 1, 2])].sort()).toEqual([1, 2]);
});
