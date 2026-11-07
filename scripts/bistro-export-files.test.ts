import { expect, test } from "bun:test";
import { link, mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { assertNoInputFileAliases, assertSeparateExportOutput, copyBrowserEnvironment } from "./bistro-export-files";
import { embedPreparedGltf } from "./embedded-gltf";

test("offline export paths refuse directory, symlink and hardlink input aliases", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "bistro-export-alias-"));
  try {
    const source = resolve(root, "source"), output = resolve(root, "output"), alias = resolve(root, "alias");
    await mkdir(source); await mkdir(output);
    await writeFile(resolve(source, "geometry.bin"), "source bytes");
    await symlink(source, alias);
    for (const destination of [source, alias, resolve(source, "nested"), root]) await expect(assertSeparateExportOutput(destination, [source])).rejects.toThrow("separate");
    await assertSeparateExportOutput(output, [source]);
    await link(resolve(source, "geometry.bin"), resolve(output, "geometry.bin"));
    await expect(assertNoInputFileAliases([resolve(output, "geometry.bin")], [resolve(source, "geometry.bin")])).rejects.toThrow("aliases an input");
    const scene = resolve(source, "scene.gltf");
    await writeFile(scene, JSON.stringify({ asset: { version: "2.0" }, buffers: [{ uri: "geometry.bin", byteLength: 12 }] }));
    await expect(embedPreparedGltf(scene, scene)).rejects.toThrow("aliases an input");
    await symlink(resolve(source, "geometry.bin"), resolve(output, "scene.glb.part"));
    await expect(embedPreparedGltf(scene, resolve(output, "scene.glb"))).rejects.toThrow("aliases an input");
    expect(await readFile(resolve(source, "geometry.bin"), "utf8")).toBe("source bytes");
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("both packaging routes retain bounded derived environment provenance and reject input reuse", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "bistro-export-provenance-"));
  try {
    const prepared = resolve(root, "prepared"), rgba = resolve(root, "rgba"), compressed = resolve(root, "compressed");
    await mkdir(prepared);
    await writeFile(resolve(prepared, "BistroExterior.gltf"), JSON.stringify({ asset: { version: "2.0" }, buffers: [{ uri: "geometry.bin", byteLength: 3 }], materials: [], textures: [], images: [] }));
    await writeFile(resolve(prepared, "geometry.bin"), new Uint8Array([1, 2, 3]));
    for (const name of ["LICENSE.txt", "README.md", "SGL-ATTRIBUTION.txt", "pmrem.rgba16", "panorama.png", "THREE-LICENSE.txt", "ENVIRONMENT-PROCESSING.json", "HDRI-ATTRIBUTION.txt"]) await writeFile(resolve(prepared, name), `fixture ${name}`);
    await writeFile(resolve(prepared, "environment.json"), JSON.stringify({ processing_metadata: "ENVIRONMENT-PROCESSING.json" }));
    const pack = (output: string) => Bun.spawnSync(["bun", resolve(import.meta.dir, "package-bistro-browser.ts"), "--prepared", prepared, "--environment", prepared, "--output", output, "--deduplicate-images"]);
    let result = pack(rgba);
    expect(result.exitCode).toBe(0);
    // No images: exercise the actual compression packaging route without an
    // encoder/GPU. /usr/bin/true stands in only for unused tool invocations.
    const compress = (output: string) => Bun.spawnSync(["bun", resolve(import.meta.dir, "compress-bistro-browser.ts"), "--prepared", prepared, "--rgba-package", rgba, "--output", output, "--format", "bc7", "--ktx", "/usr/bin/true", "--mips-tool", "/usr/bin/true", "--python", "/usr/bin/true"]);
    result = compress(compressed);
    expect(result.exitCode).toBe(0);
    for (const directory of [rgba, compressed]) {
      const metadata = await Bun.file(resolve(directory, "environment.json")).json();
      expect(metadata.processing_metadata).toBe("ENVIRONMENT-PROCESSING.json");
      for (const name of [metadata.processing_metadata, "HDRI-ATTRIBUTION.txt"]) expect(await readFile(resolve(directory, name), "utf8")).toBe(`fixture ${name}`);
      expect((await Bun.file(resolve(directory, "asset-stats.json")).json()).environmentFiles.map((file: any) => file.name)).toContain("HDRI-ATTRIBUTION.txt");
    }
    const preserved = new Map(await Promise.all((await readdir(rgba)).map(async name => [name, await readFile(resolve(rgba, name))] as const)));
    result = compress(rgba);
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr.toString()).toContain("separate from input");
    for (const [name, bytes] of preserved) expect(await readFile(resolve(rgba, name))).toEqual(bytes);
    for (const [index, name] of ["asset-stats.json", "compressed-images.json", "BistroExterior.glb", "BistroExterior.glb.part", "compression-quality.json", "COMPRESSION-TOOLS.json"].entries()) {
      for (const kind of ["hardlink", "symlink"]) {
        const unsafe = resolve(root, `unsafe-${index}-${kind}`);
        await mkdir(unsafe);
        if (kind === "hardlink") await link(resolve(rgba, "asset-stats.json"), resolve(unsafe, name));
        else await symlink(resolve(rgba, "asset-stats.json"), resolve(unsafe, name));
        result = compress(unsafe);
        expect(result.exitCode).not.toBe(0);
        expect(result.stderr.toString()).toContain("aliases an input");
        // Alias rejection must precede even Compressed/ directory creation.
        expect(await readdir(unsafe)).toEqual([name]);
        expect(await readFile(resolve(rgba, "asset-stats.json"))).toEqual(preserved.get("asset-stats.json")!);
      }
    }
    const unsafePackage = resolve(root, "unsafe-package"); await mkdir(unsafePackage);
    await link(resolve(prepared, "LICENSE.txt"), resolve(unsafePackage, "asset-stats.json"));
    result = pack(unsafePackage);
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr.toString()).toContain("aliases an input");
    expect(await readdir(unsafePackage)).toEqual(["asset-stats.json"]);
    expect(await readFile(resolve(prepared, "LICENSE.txt"), "utf8")).toBe("fixture LICENSE.txt");
    const alias = resolve(root, "alias"); await symlink(prepared, alias);
    for (const destination of [prepared, alias]) {
      result = pack(destination);
      expect(result.exitCode).not.toBe(0);
      expect(result.stderr.toString()).toContain("separate from input");
    }
    await writeFile(resolve(prepared, "environment.json"), '{"processing_metadata":"../outside.json"}');
    await expect(copyBrowserEnvironment(prepared, compressed)).rejects.toThrow("unsupported");
    await writeFile(resolve(prepared, "environment.json"), '{"processing_metadata":"ENVIRONMENT-PROCESSING.json"}');
    await rm(resolve(prepared, "ENVIRONMENT-PROCESSING.json"));
    await expect(copyBrowserEnvironment(prepared, compressed)).rejects.toThrow("missing");
  } finally { await rm(root, { recursive: true, force: true }); }
});
