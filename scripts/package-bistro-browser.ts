// Offline, example-owned packaging of prepared Bistro content. This does not
// simplify geometry, change materials, or alter SGL's browser load budget.
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { embedPreparedGltf } from "./embedded-gltf";
import { assertNoInputFileAliases, assertSeparateExportOutput, browserEnvironmentFileNames, copyBrowserEnvironment } from "./bistro-export-files";

const usage = "usage: bun scripts/package-bistro-browser.ts --prepared DIR --environment DIR --output DIR [--deduplicate-images]";
const options = new Map<string, string>();
let deduplicateImages = false;
for (let index = 2; index < Bun.argv.length; index++) {
  if (Bun.argv[index] === "--deduplicate-images") { deduplicateImages = true; continue; }
  const key = Bun.argv[index], value = Bun.argv[++index];
  if (!["--prepared", "--environment", "--output"].includes(key) || !value) throw new Error(usage);
  options.set(key, resolve(value));
}
const prepared = options.get("--prepared"), environment = options.get("--environment"), output = options.get("--output");
if (!prepared || !environment || !output) throw new Error(usage);
await assertSeparateExportOutput(output, [prepared, environment]);
const notices = ["LICENSE.txt", "README.md", "SGL-ATTRIBUTION.txt"];
const environmentNames = await browserEnvironmentFileNames(environment);
const document = await Bun.file(resolve(prepared, "BistroExterior.gltf")).json();
const sourceFiles = [resolve(prepared, "BistroExterior.gltf"), ...[...document.buffers, ...(document.images ?? [])].map((item: { uri: string }) => resolve(prepared, decodeURIComponent(item.uri))), ...notices.map(name => resolve(prepared, name)), ...environmentNames.map(name => resolve(environment, name))];
await assertNoInputFileAliases([...notices, ...environmentNames, "asset-stats.json", "BistroExterior.glb", "BistroExterior.glb.part"].map(name => resolve(output, name)), sourceFiles);
await mkdir(output, { recursive: true });
const packed = await embedPreparedGltf(resolve(prepared, "BistroExterior.gltf"), resolve(output, "BistroExterior.glb"), { deduplicateImages });
for (const name of notices) await copyFile(resolve(prepared, name), resolve(output, name));
const environmentFiles = await copyBrowserEnvironment(environment, output);
await writeFile(resolve(output, "SGL-ATTRIBUTION.txt"), `${await Bun.file(resolve(output, "SGL-ATTRIBUTION.txt")).text()}\nBrowser packaging: geometry and corrected PNG images embedded in core glTF 2.0 GLB;\ngeometry, materials, scene hierarchy, animation and authored transforms unchanged.\n${deduplicateImages ? "Byte-identical PNG images shared; texture entries, samplers and material texture bindings preserved.\n" : ""}Environment remains a separate explicit load.\n`);
const stats = {
  asset: "BistroExterior.glb",
  sourceRevision: "816a1d6533c55787b3fabc9b75660c0d0c57ebc6",
  ...packed,
  // The loader bounds source bytes and decoded data independently. Its BIN
  // copy is charged alongside the original GLB, not alongside decoded pixels.
  browserBudgetBytes: 1 << 30,
  environmentFiles,
  notes: [
    "decodedImageBytes is RGBA8 level zero for all images; unused images may be skipped by the loader",
    "decoded geometry, skins and animation share a separate 1 GiB cap with decoded images",
    "sourceBudgetBytes excludes decoded data; it is not a process peak-memory estimate",
    "GPU mip chains, ray-scene data, transient preparation, WASM/JS fetch copies and environment use additional memory",
    "the environment is fetched after GLB decoding/upload to avoid retaining both source payloads",
  ],
};
await writeFile(resolve(output, "asset-stats.json"), `${JSON.stringify(stats, null, 2)}\n`);
console.log(JSON.stringify({ ...stats, images: undefined }, null, 2));
