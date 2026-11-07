// Explicit offline export to SGL-supported KTX2 blocks. Basis is an encoder
// intermediate only; the browser receives final BC7/ETC2/ASTC, never Basis.
import { copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { embedPreparedGltf } from "./embedded-gltf";
import { assertNoInputFileAliases, assertSeparateExportOutput, browserEnvironmentFileNames, copyBrowserEnvironment } from "./bistro-export-files";

export type CompressionFormat = "bc7" | "etc2" | "astc4x4";

export async function withCompressionScratch<T>(output: string, action: (directory: string) => Promise<T>): Promise<T> {
  const parent = dirname(resolve(output));
  await mkdir(parent, { recursive: true });
  // mkdtemp owns a fresh directory even when sibling exports have the same
  // format and dimensions. Never remove a caller's preexisting directory.
  const directory = await mkdtemp(resolve(parent, ".bistro-compress-"));
  try { return await action(directory); }
  finally { await rm(directory, { recursive: true, force: true }); }
}

export function imageRoles(document: any, originalToRetained: number[]): Map<number, Set<string>> {
  const result = new Map<number, Set<string>>();
  for (const material of document.materials ?? []) {
    for (const [info, role] of [
      [material.pbrMetallicRoughness?.baseColorTexture, "color"],
      [material.emissiveTexture, "color"], [material.normalTexture, "normal"],
      [material.pbrMetallicRoughness?.metallicRoughnessTexture, "data"],
      [material.occlusionTexture, "data"],
    ] as const) {
      if (!info) continue;
      const index = originalToRetained[document.textures[info.index].source];
      const roles = result.get(index) ?? new Set<string>();
      roles.add(role); result.set(index, roles);
    }
  }
  return result;
}

export function compressible(image: { width: number; height: number }, roles: Set<string>): boolean {
  // Keep tiny constants, invalid block shapes and images needing two differently
  // filtered colour/data chains in ordinary RGBA. Do not change their meaning.
  return image.width % 4 === 0 && image.height % 4 === 0 && Math.max(image.width, image.height) >= 64
    && !(roles.has("color") && (roles.has("normal") || roles.has("data")));
}

export function alphaSensitiveImages(document: any, originalToRetained: number[]): Set<number> {
  const result = new Set<number>();
  for (const material of document.materials ?? []) {
    const texture = material.pbrMetallicRoughness?.baseColorTexture;
    if (texture && ["MASK", "BLEND"].includes(material.alphaMode)) result.add(originalToRetained[document.textures[texture.index].source]);
  }
  return result;
}

async function main() {
  const options = new Map<string, string>();
  let compressAlpha = false;
  for (let index = 2; index < Bun.argv.length; index++) {
    if (Bun.argv[index] === "--compress-alpha") { compressAlpha = true; continue; }
    const key = Bun.argv[index], value = Bun.argv[++index];
    if (!value || !["--prepared", "--rgba-package", "--output", "--ktx", "--mips-tool", "--python", "--format", "--threads"].includes(key)) throw new Error("usage: bun scripts/compress-bistro-browser.ts --prepared DIR --rgba-package DIR --output DIR --ktx PATH --mips-tool PATH --python PATH --format bc7|etc2|astc4x4 [--threads N]");
    options.set(key, value);
  }
  const required = (key: string) => { const value = options.get(key); if (!value) throw new Error(`missing ${key}`); return value; };
  const prepared = resolve(required("--prepared")), rgba = resolve(required("--rgba-package")), output = resolve(required("--output"));
  const ktx = resolve(required("--ktx")), mipsTool = resolve(required("--mips-tool")), python = resolve(required("--python"));
  const format = required("--format") as CompressionFormat, threads = Number(options.get("--threads") ?? 2);
  if (!["bc7", "etc2", "astc4x4"].includes(format) || !Number.isInteger(threads) || threads < 1 || threads > 16) throw new Error("invalid format or thread count");
  await assertSeparateExportOutput(output, [prepared, rgba]);
  await assertSeparateExportOutput(resolve(output, "Compressed"), [prepared, rgba]);
  const notices = ["LICENSE.txt", "README.md", "SGL-ATTRIBUTION.txt"];
  const run = (command: string[]) => {
    const result = Bun.spawnSync(command, { stdout: "pipe", stderr: "pipe" });
    if (result.exitCode) throw new Error(`${command.join(" ")}\n${result.stderr.toString()}`);
    return result.stdout.toString().trim();
  };
  const source = JSON.parse(await readFile(resolve(prepared, "BistroExterior.gltf"), "utf8"));
  const baseline = await Bun.file(resolve(rgba, "asset-stats.json")).json();
  if (!baseline.imageDeduplication?.enabled) throw new Error("--rgba-package must be the explicit deduplicated RGBA export");
  const roles = imageRoles(source, baseline.imageDeduplication.originalToRetainedImages);
  const alpha = alphaSensitiveImages(source, baseline.imageDeduplication.originalToRetainedImages);
  const environmentNames = await browserEnvironmentFileNames(rgba);
  const sourceFiles = [resolve(prepared, "BistroExterior.gltf"), ...[...source.buffers, ...source.images].map((item: { uri: string }) => resolve(prepared, decodeURIComponent(item.uri))), ...[...notices, ...environmentNames, "asset-stats.json", "BistroExterior.glb"].map(name => resolve(rgba, name))];
  const generatedNames = [...notices, ...environmentNames, "BistroExterior.glb", "BistroExterior.glb.part", "asset-stats.json", "compressed-images.json", "compression-quality.json", "COMPRESSION-TOOLS.json", "TEXTURE2DDECODER-LICENSE.txt"];
  for (const image of baseline.images) {
    const uses = roles.get(image.index) ?? new Set<string>(["data"]);
    if (!compressible(image, uses) || (!compressAlpha && alpha.has(image.index))) continue;
    const name = `Compressed/image-${String(image.index).padStart(3, "0")}`;
    generatedNames.push(`${name}.ktx2`);
    if (format !== "bc7") generatedNames.push(`${name}-level0.png`);
  }
  await assertNoInputFileAliases(generatedNames.map(name => resolve(output, name)), sourceFiles);
  await mkdir(resolve(output, "Compressed"), { recursive: true });
  // Temporary intermediates are not part of the delivered package. They are
  // regenerated by this explicit command, never checked for automatic staleness.
  const supplied = new Map<number, string>(), entries: any[] = [];
  let compressedBytes = 0, chainBytes = 0;
  await withCompressionScratch(output, async (work) => {
    for (const image of baseline.images) {
      const uses = roles.get(image.index) ?? new Set<string>(["data"]);
      if (!compressible(image, uses) || (!compressAlpha && alpha.has(image.index))) continue;
      const name = `image-${String(image.index).padStart(3, "0")}`, mipDirectory = resolve(work, name);
      const count = Number(run([mipsTool, resolve(prepared, image.name), mipDirectory, uses.has("color") ? "color" : "data"]));
      const intermediate = resolve(work, `${name}.ktx2`), file = `Compressed/${name}.ktx2`;
      const mipPaths = Array.from({ length: count }, (_, level) => resolve(mipDirectory, `${level}.png`));
      run([ktx, "create", "--format", uses.has("color") ? "R8G8B8A8_SRGB" : "R8G8B8A8_UNORM", "--assign-tf", uses.has("color") ? "srgb" : "linear", "--levels", String(count), "--encode", "uastc", "--uastc-quality", "2", "--threads", String(threads), ...mipPaths, intermediate]);
      run([ktx, "transcode", "--target", format === "etc2" ? "etc-rgba" : format === "astc4x4" ? "astc" : "bc7", "--zstd", "3", intermediate, resolve(output, file)]);
      const header = new DataView(await Bun.file(resolve(output, file)).slice(0, 80 + count * 24).arrayBuffer());
      let retained = 0;
      for (let level = 0; level < count; level++) retained += Number(header.getBigUint64(80 + level * 24 + 16, true));
      const bytes = (await stat(resolve(output, file))).size;
      entries.push({ index: image.index, ktx2: file, ...(format === "bc7" ? {} : { decoded_level_0: `Compressed/${name}-level0.png` }), width: image.width, height: image.height, roles: [...uses].sort(), source_png: image.name, levels: count, file_bytes: bytes, retained_block_bytes: retained });
      supplied.set(image.index, file);
      compressedBytes += bytes; chainBytes += retained;
      if (entries.length % 10 === 0) console.log(`${format}: encoded ${entries.length} images`);
    }
    const packed = await embedPreparedGltf(resolve(prepared, "BistroExterior.gltf"), resolve(output, "BistroExterior.glb"), { deduplicateImages: true, suppliedImages: supplied });
    const manifest = { schema: 1, format, images: entries, lossy: true, alpha_policy: compressAlpha ? "lossy alpha explicitly requested" : "MASK/BLEND base-colour images retained as RGBA", mip_filter: "sgl_3d::asset::Rgba8MipChain, linear-light colour / linear data; normal XYZ unchanged", encoder: { ktx: run([ktx, "--version"]), intermediate: "UASTC quality 2", final: format, supercompression: "Zstandard level 3" } };
    await writeFile(resolve(output, "compressed-images.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    for (const name of notices) await copyFile(resolve(rgba, name), resolve(output, name));
    const environmentFiles = await copyBrowserEnvironment(rgba, output);
    await writeFile(resolve(output, "SGL-ATTRIBUTION.txt"), `${await Bun.file(resolve(output, "SGL-ATTRIBUTION.txt")).text()}\nLossy browser ${format} export: SGL colour/data mip chains explicitly encoded\nthrough offline UASTC to final KTX2 ${format} blocks with Khronos KTX-Software.\nNormal XYZ channels and alpha retained; no normal-mode channel repacking.\nETC2/ASTC decoded companions, where present, decode the exact stored blocks.\n`);
    await writeFile(resolve(output, "asset-stats.json"), `${JSON.stringify({ ...baseline, ...packed, environmentFiles, rgbaReferenceImageBytes: packed.decodedImageBytes, decodedImageBytes: undefined, compression: { format, manifest: "compressed-images.json", alphaPolicy: manifest.alpha_policy, alphaSensitiveImages: [...alpha].sort((a, b) => a - b), compressedImageCount: entries.length, rgbaImageCount: packed.imageCount - entries.length, compressedFileBytes: compressedBytes, retainedBlockBytes: chainBytes, lossy: true }, notes: ["RGBA reference byte counts describe the pre-compression images, not compressed retention.", "Source bytes, block chains, RGBA fallback and decoded companions are reported separately; none is a process peak estimate."], loaderInspection: undefined, decodedGeometryBytes: undefined, loadedDecodedImageBytes: undefined, decodedGeometryAndImageBytes: undefined }, null, 2)}\n`);
    // The decoder also measures lossy level-zero colour/alpha/normal error.
    // ETC2/ASTC companions come from decoded final blocks, never the source PNG.
    console.log(run([python, resolve(import.meta.dir, "bistro-compression-quality.py"), output, prepared, "--ktx", ktx]));
    console.log(JSON.stringify({ format, output, images: entries.length, compressedBytes, chainBytes, glbBytes: packed.glbBytes }));
  });
}

if (import.meta.main) await main();
