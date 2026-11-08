import { afterEach, expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { extractArchive, prepareSponzaSources, verifyArchive } from "./prepare-sponza";

const temporary: string[] = [];
afterEach(async () => { await Promise.all(temporary.splice(0).map(p => rm(p, { recursive: true, force: true }))); });
async function directory() {
  const path = await mkdtemp(resolve(tmpdir(), "sgl-sponza-"));
  temporary.push(path);
  return path;
}
async function zip(path: string, name: string, symlink = false) {
  const child = Bun.spawn(["python3", "-c", `import sys, zipfile
with zipfile.ZipFile(sys.argv[1], 'w') as z:
    z.writestr('safe.txt', 'safe')
    i = zipfile.ZipInfo(sys.argv[2])
    if sys.argv[3] == 'true':
        i.create_system = 3
        i.external_attr = 0o120777 << 16
    z.writestr(i, 'fixture')`, path, name, String(symlink)], { stdout: "pipe", stderr: "pipe" });
  expect(await child.exited).toBe(0);
}

test("verifies exact SHA-256 bytes and refuses corruption", async () => {
  const file = resolve(await directory(), "archive.zip");
  await writeFile(file, "abc");
  await verifyArchive(file, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  await writeFile(file, "abd");
  await expect(verifyArchive(file, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")).rejects.toThrow("SHA-256 mismatch");
});

test("extracts scene and retained notices into a fresh directory", async () => {
  const dir = await directory(), archive = resolve(dir, "source.zip"), output = resolve(dir, "source");
  await zip(archive, "scene/credits_license.txt");
  await extractArchive(archive, output);
  expect(await readFile(resolve(output, "scene/credits_license.txt"), "utf8")).toBe("fixture");
  expect(await readFile(resolve(output, "safe.txt"), "utf8")).toBe("safe");
});

for (const name of ["../escaped.txt", "/absolute.txt", "folder/../../escaped.txt", "C:/escaped.txt", "folder\\escaped.txt", "safe.txt", "SAFE.txt"]) {
  test(`rejects unsafe or duplicate ZIP path ${name} without publishing partial extraction`, async () => {
    const dir = await directory(), archive = resolve(dir, "source.zip"), output = resolve(dir, "source");
    await zip(archive, name);
    await expect(extractArchive(archive, output)).rejects.toThrow();
    expect(await stat(output).catch(() => null)).toBeNull();
    expect((await readdir(dir)).filter(name => name.startsWith("source.extracting-"))).toEqual([]);
    expect(await stat(resolve(dir, "escaped.txt")).catch(() => null)).toBeNull();
  });
}

test("rejects symbolic links", async () => {
  const dir = await directory(), archive = resolve(dir, "source.zip");
  await zip(archive, "link", true);
  await expect(extractArchive(archive, resolve(dir, "source"))).rejects.toThrow();
});

test("offline cache miss fails clearly without network access", async () => {
  await expect(prepareSponzaSources(await directory(), true)).rejects.toThrow("Offline cache is missing");
});


test("leaves unrelated staging data untouched", async () => {
  const dir = await directory(), archive = resolve(dir, "source.zip"), output = resolve(dir, "source");
  await writeFile(`${output}.extracting`, "prior data");
  await zip(archive, "scene.gltf");
  await extractArchive(archive, output);
  expect(await readFile(`${output}.extracting`, "utf8")).toBe("prior data");
});
