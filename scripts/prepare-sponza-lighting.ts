// Explicit optional Sponza sky export. Geometry and neutral environment remain intact.
import { copyFile, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { assertNoInputFileAliases } from "./bistro-export-files";

export function lightingOptions(args: string[]) {
  const options = { source: ".cache/sponza/source/base/main_sponza/textures/kloppenheim_05_4k.hdr", prepared: ".cache/sponza/prepared" };
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index]; const value = args[index + 1];
    if (!value || value.startsWith("--") || (key !== "--source" && key !== "--prepared")) throw new Error("usage: bun scripts/prepare-sponza-lighting.ts [--source source.hdr] [--prepared directory]");
    options[key === "--source" ? "source" : "prepared"] = value;
  }
  return { source: resolve(options.source), prepared: resolve(options.prepared) };
}
export const skyFiles = ["sky-environment.json", "sky-panorama.png", "sky-pmrem.rgba16", "SKY-PROCESSING.json", "HDRI-ATTRIBUTION.txt", "THREE-LICENSE.txt"];
async function command(args: string[]) {
  const result = Bun.spawn(args, { stdout: "inherit", stderr: "inherit" });
  if (await result.exited !== 0) throw new Error(`Failed: ${args.join(" ")}`);
}
export async function prepareSponzaLighting(
  { source, prepared }: ReturnType<typeof lightingOptions>,
  runCommand: (args: string[]) => Promise<void> = command,
) {
  await mkdir(prepared, { recursive: true });
  // Also protect the existing scene and neutral files from accidental hard-link
  // or symlink aliases. Only the fixed new sky names are published.
  const inputs = [source];
  for (const name of ["Sponza.glb", "environment.json", "panorama.png", "pmrem.rgba16"]) {
    const file = join(prepared, name);
    if (await Bun.file(file).exists()) inputs.push(file);
  }
  await assertNoInputFileAliases(skyFiles.map(name => join(prepared, name)), inputs);
  const work = await mkdtemp(join(prepared, ".sky-export-"));
  try {
    await runCommand(["cargo", "run", "-p", "sgl-3d-examples", "--example", "sponza_environment", "--", source, work]);
    const bake = join(work, "pmrem-bake");
    await runCommand(["bun", "scripts/bistro-environment.ts", join(work, "sky-lighting.hdr"), bake]);
    const metadata = JSON.parse(await readFile(join(bake, "environment.json"), "utf8"));
    const processing = JSON.parse(await readFile(join(work, "SKY-PROCESSING.json"), "utf8"));
    processing.pmrem = metadata;
    await writeFile(join(work, "SKY-PROCESSING.json"), JSON.stringify(processing, null, 2) + "\n");
    await writeFile(join(work, "sky-environment.json"), JSON.stringify({ ...metadata, source: "kloppenheim_05_4k.hdr; explicit sun-reduced lighting", processing_metadata: "SKY-PROCESSING.json", display_panorama: "sky-panorama.png (original sun retained)", atlas: "sky-pmrem.rgba16" }, null, 2) + "\n");
    await copyFile(join(bake, "pmrem.rgba16"), join(work, "sky-pmrem.rgba16"));
    await copyFile(join(bake, "THREE-LICENSE.txt"), join(work, "THREE-LICENSE.txt"));
    await writeFile(join(work, "HDRI-ATTRIBUTION.txt"), "Kloppenheim 05 by Greg Zaal / Poly Haven, CC0.\nhttps://polyhaven.com/a/kloppenheim_05\nhttps://polyhaven.com/license\nSource HDR distributed in Intel's pinned Sponza archive. This sky's CC0 license is separate from Intel Sponza geometry/material licensing; existing Intel notices are unchanged.\nLighting derivative removes the measured sun lobe and reduces to 1024 pixels in linear radiance with solid-angle weighting. Original display sky retains the sun. See SKY-PROCESSING.json.\n");
    // Atomic per-file replacement avoids following existing output symlinks.
    // Publish metadata last, only after the actual WebGPU bake succeeded.
    for (const name of [...skyFiles.filter(name => name !== "sky-environment.json"), "sky-environment.json"]) await rename(join(work, name), join(prepared, name));
    console.log(`Prepared Sponza sky in ${prepared}; neutral environment and Sponza.glb unchanged.`);
  } finally { await rm(work, { recursive: true, force: true }); }
}
if (import.meta.main) await prepareSponzaLighting(lightingOptions(Bun.argv.slice(2)));
