import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { embedPreparedGltf } from "./embedded-gltf";

// Defect: a PNG view can point into padding/geometry, or an unaligned source
// can corrupt GLB chunks. Oracle: the GLB 2.0 container layout and the literal
// independent geometry/PNG payloads, read back from the produced artifact.
test("embeds distinct geometry and PNG payloads without changing scene data", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "sgl-bistro-package-"));
  try {
    const prepared = resolve(root, "prepared"), output = resolve(root, "output");
    await mkdir(prepared);
    const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a7V0AAAAASUVORK5CYII=", "base64");
    await writeFile(resolve(prepared, "geometry.bin"), new Uint8Array([10, 20, 30]));
    await writeFile(resolve(prepared, "image.png"), png);
    await writeFile(resolve(prepared, "BistroExterior.gltf"), JSON.stringify({
      asset: { version: "2.0" }, buffers: [{ uri: "geometry.bin", byteLength: 3 }],
      bufferViews: [{ buffer: 0, byteLength: 3 }], images: [{ uri: "image.png" }],
      materials: [{ alphaMode: "BLEND", extensions: { KHR_materials_transmission: { transmissionFactor: 0.7 } } }],
      meshes: [], nodes: [{ translation: [2, 3, 4] }], scenes: [{ nodes: [0] }], scene: 0,
    }));
    for (const name of ["LICENSE.txt", "README.md", "SGL-ATTRIBUTION.txt", "environment.json", "pmrem.rgba16", "panorama.png", "THREE-LICENSE.txt"]) {
      await writeFile(resolve(prepared, name), `fixture ${name}`);
    }
    await writeFile(resolve(prepared, "environment.json"), "{}");
    const result = Bun.spawnSync(["bun", resolve(import.meta.dir, "package-bistro-browser.ts"), "--prepared", prepared, "--environment", prepared, "--output", output]);
    expect(result.exitCode).toBe(0);
    const bytes = Buffer.from(await Bun.file(resolve(output, "BistroExterior.glb")).arrayBuffer());
    expect(bytes.subarray(0, 8)).toEqual(Buffer.from([0x67, 0x6c, 0x54, 0x46, 2, 0, 0, 0]));
    expect(bytes.readUInt32LE(8)).toBe(bytes.length);
    expect(bytes.subarray(16, 20).toString()).toBe("JSON");
    const jsonLength = bytes.readUInt32LE(12);
    const document = JSON.parse(bytes.subarray(20, 20 + jsonLength).toString());
    const binHeader = 20 + jsonLength;
    expect(bytes.subarray(binHeader + 4, binHeader + 8)).toEqual(Buffer.from([0x42, 0x49, 0x4e, 0]));
    const bin = bytes.subarray(binHeader + 8);
    expect(bin.subarray(0, 4)).toEqual(Buffer.from([10, 20, 30, 0]));
    const image = document.images[0], view = document.bufferViews[image.bufferView];
    expect(image.uri).toBeUndefined();
    expect(document.buffers[0].uri).toBeUndefined();
    expect(bin.subarray(view.byteOffset, view.byteOffset + view.byteLength)).toEqual(png);
    expect(document.nodes[0].translation).toEqual([2, 3, 4]);
    expect(document.materials[0]).toEqual({ alphaMode: "BLEND", extensions: { KHR_materials_transmission: { transmissionFactor: 0.7 } } });
    expect(await Bun.file(resolve(output, "LICENSE.txt")).text()).toBe("fixture LICENSE.txt");
    expect(await Bun.file(resolve(output, "pmrem.rgba16")).text()).toBe("fixture pmrem.rgba16");
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("refuses escaped/symlinked URIs and missing geometry without producing an asset", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "sgl-embedded-gltf-uri-"));
  try {
    const prepared = resolve(root, "prepared"), input = resolve(prepared, "scene.gltf"), output = resolve(root, "scene.glb");
    await mkdir(prepared);
    await writeFile(resolve(root, "outside.bin"), new Uint8Array([7]));
    await symlink(resolve(root, "outside.bin"), resolve(prepared, "link.bin"));
    for (const uri of ["../outside.bin", "%2e%2e/outside.bin", "link.bin", "missing.bin"]) {
      await writeFile(input, JSON.stringify({ asset: { version: "2.0" }, buffers: [{ uri, byteLength: 1 }] }));
      await expect(embedPreparedGltf(input, output)).rejects.toThrow();
      expect(await Bun.file(output).exists()).toBe(false);
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

// Defect: compacting image indices can silently bind a different image to a
// material or collapse samplers/texture transforms. Oracle: red/blue PNG
// payloads and independently authored sampler/material bindings in the GLB.
test("deduplicates only image payloads and remaps each texture source", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "sgl-gltf-dedup-"));
  try {
    const red = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==", "base64");
    const blue = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYPj/HwADAgH/5ncLrgAAAABJRU5ErkJggg==", "base64");
    await writeFile(resolve(root, "geometry.bin"), new Uint8Array([7]));
    await writeFile(resolve(root, "red.png"), red);
    await writeFile(resolve(root, "red-copy.png"), red);
    await writeFile(resolve(root, "blue.png"), blue);
    const material = { alphaMode: "MASK", alphaCutoff: 0.3, pbrMetallicRoughness: { baseColorTexture: { index: 1, extensions: { KHR_texture_transform: { offset: [0.1, 0.2], scale: [2, 3] } } }, metallicRoughnessTexture: { index: 0 } }, normalTexture: { index: 2, scale: 0.7 } };
    const samplers = [{ wrapS: 33071 }, { wrapS: 33648 }, { wrapS: 10497 }];
    const input = resolve(root, "scene.gltf"), output = resolve(root, "scene.glb");
    await writeFile(input, JSON.stringify({
      asset: { version: "2.0" }, buffers: [{ uri: "geometry.bin", byteLength: 1 }],
      images: [{ uri: "red.png" }, { uri: "red-copy.png" }, { uri: "blue.png" }],
      textures: [{ source: 2, sampler: 0 }, { source: 1, sampler: 1 }, { source: 0, sampler: 2 }],
      samplers, materials: [material], nodes: [{ translation: [4, 5, 6] }],
    }));
    const stats = await embedPreparedGltf(input, output, { deduplicateImages: true });
    const bytes = Buffer.from(await Bun.file(output).arrayBuffer()), jsonLength = bytes.readUInt32LE(12);
    const document = JSON.parse(bytes.subarray(20, 20 + jsonLength).toString()), bin = bytes.subarray(28 + jsonLength);
    expect(document.images).toHaveLength(2);
    expect(document.textures).toEqual([{ source: 1, sampler: 0 }, { source: 0, sampler: 1 }, { source: 0, sampler: 2 }]);
    for (const [textureIndex, expected] of [[0, blue], [1, red], [2, red]] as const) {
      const image = document.images[document.textures[textureIndex].source], view = document.bufferViews[image.bufferView];
      expect(bin.subarray(view.byteOffset, view.byteOffset + view.byteLength)).toEqual(expected);
    }
    expect(document.samplers).toEqual(samplers);
    expect(document.materials[0]).toEqual(material);
    expect(document.nodes[0].translation).toEqual([4, 5, 6]);
    expect(stats.imageDeduplication.originalToRetainedImages).toEqual([0, 0, 1]);
    // Supplied-image indices belong to the retained array, not the source
    // array: blue was source index 2 but is retained index 1.
    const suppliedOutput = resolve(root, "supplied.glb");
    await embedPreparedGltf(input, suppliedOutput, { deduplicateImages: true, suppliedImages: new Map([[1, "Compressed/blue.ktx2"]]) });
    const suppliedBytes = Buffer.from(await Bun.file(suppliedOutput).arrayBuffer()), suppliedJsonLength = suppliedBytes.readUInt32LE(12);
    const suppliedDocument = JSON.parse(suppliedBytes.subarray(20, 20 + suppliedJsonLength).toString());
    expect(suppliedDocument.images[1]).toEqual({ uri: "Compressed/blue.ktx2" });
    expect(suppliedDocument.images[0].bufferView).toBeDefined();
    expect(suppliedDocument.textures).toEqual([{ source: 1, sampler: 0 }, { source: 0, sampler: 1 }, { source: 0, sampler: 2 }]);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("deduplication refuses image metadata and extension-owned image references", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "sgl-gltf-dedup-metadata-"));
  try {
    const input = resolve(root, "scene.gltf"), output = resolve(root, "scene.glb");
    for (const metadata of [{ name: "authored label" }, { extras: { category: "normal" } }, { extensions: { EXT_example: { source: 1 } } }]) {
      await writeFile(input, JSON.stringify({ images: [{ uri: "image.png", ...metadata }] }));
      await expect(embedPreparedGltf(input, output, { deduplicateImages: true })).rejects.toThrow("metadata");
    }
    for (const references of [{ extensions: { EXT_example: { image: 0 } } }, { textures: [{ extensions: { KHR_texture_basisu: { source: 0 } } }] }]) {
      await writeFile(input, JSON.stringify({ images: [], ...references }));
      await expect(embedPreparedGltf(input, output, { deduplicateImages: true })).rejects.toThrow("references");
    }
    expect(await Bun.file(output).exists()).toBe(false);
  } finally { await rm(root, { recursive: true, force: true }); }
});
