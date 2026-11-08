import { expect, test } from "bun:test";
import { resolve, join } from "node:path";
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { lightingOptions, prepareSponzaLighting, skyFiles } from "./prepare-sponza-lighting";
test("sky export defaults and custom local paths", () => {
  expect(lightingOptions([]).prepared).toBe(resolve(".cache/sponza/prepared"));
  expect(lightingOptions(["--prepared", "local-high", "--source", "local.hdr"])).toEqual({prepared:resolve("local-high"),source:resolve("local.hdr")});
});
test("sky export rejects unknown or incomplete options", () => {
  for (const args of [["--prepared"], ["--source", "--prepared"], ["--unknown", "value"]]) expect(() => lightingOptions(args)).toThrow();
});
test("sky export never publishes neutral or geometry names", () => {
  for (const name of ["Sponza.glb", "environment.json", "panorama.png", "pmrem.rgba16"]) expect(skyFiles).not.toContain(name);
});

test("failed sky bake preserves source and existing outputs and cleans staging", async () => {
  await withExport(async ({ source, prepared }) => {
    const previous = new Map<string, string>();
    for (const name of [...protectedNames, ...skyFiles]) {
      previous.set(name, `previous ${name}`);
      await writeFile(join(prepared, name), previous.get(name)!);
    }
    let calls = 0;
    await expect(prepareSponzaLighting({ source, prepared }, async () => {
      calls++;
      if (calls === 2) throw new Error("bake failed");
    })).rejects.toThrow("bake failed");
    expect(await readFile(source, "utf8")).toBe("original source");
    for (const [name, value] of previous) expect(await readFile(join(prepared, name), "utf8")).toBe(value);
    expect((await readdir(prepared)).filter(name => name.startsWith(".sky-export-"))).toEqual([]);
  });
});

test("successful sky publication replaces only sky files and retains bake metadata", async () => {
  await withExport(async ({ source, prepared }) => {
    for (const name of protectedNames) await writeFile(join(prepared, name), `original ${name}`);
    await prepareSponzaLighting({ source, prepared }, async args => {
      if (args[0] === "cargo") {
        const work = args.at(-1)!;
        await writeFile(join(work, "SKY-PROCESSING.json"), JSON.stringify({ extraction: "test" }));
        await writeFile(join(work, "sky-panorama.png"), "original display sky");
      } else {
        const bake = args.at(-1)!;
        await mkdir(bake);
        await writeFile(join(bake, "environment.json"), JSON.stringify({ width: 768, height: 1024, backend: "test" }));
        await writeFile(join(bake, "pmrem.rgba16"), "lighting payload");
        await writeFile(join(bake, "THREE-LICENSE.txt"), "retained upstream notice");
      }
    });
    expect(await readFile(source, "utf8")).toBe("original source");
    for (const name of protectedNames) expect(await readFile(join(prepared, name), "utf8")).toBe(`original ${name}`);
    expect(await readFile(join(prepared, "sky-pmrem.rgba16"), "utf8")).toBe("lighting payload");
    expect(JSON.parse(await readFile(join(prepared, "SKY-PROCESSING.json"), "utf8")).pmrem.width).toBe(768);
    expect(JSON.parse(await readFile(join(prepared, "sky-environment.json"), "utf8")).atlas).toBe("sky-pmrem.rgba16");
    expect((await readdir(prepared)).filter(name => name.startsWith(".sky-export-"))).toEqual([]);
  });
});

test("refuses a sky output alias to the source before running exporters", async () => {
  await withExport(async ({ source, prepared }) => {
    await symlink(source, join(prepared, "sky-panorama.png"));
    let called = false;
    await expect(prepareSponzaLighting({ source, prepared }, async () => { called = true; })).rejects.toThrow();
    expect(called).toBe(false);
    expect(await readFile(source, "utf8")).toBe("original source");
  });
});

const protectedNames = ["Sponza.glb", "environment.json", "panorama.png", "pmrem.rgba16"];
async function withExport(body: (options: ReturnType<typeof lightingOptions>) => Promise<void>) {
  const root = await mkdtemp(join(tmpdir(), "sgl-sponza-sky-"));
  try {
    const source = join(root, "source.hdr"), prepared = join(root, "prepared");
    await mkdir(prepared);
    await writeFile(source, "original source");
    await body({ source, prepared });
  } finally { await rm(root, { recursive: true, force: true }); }
}
