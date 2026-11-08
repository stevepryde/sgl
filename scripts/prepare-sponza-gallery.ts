// Explicit native local-reflection authoring; no runtime bakes or staleness gates.
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { assertNoInputFileAliases } from "./bistro-export-files";

const root = resolve(import.meta.dir, "..");
export const galleryFiles = ["sponza-back-gallery-probe.rgba16", "sponza-gallery-probe.json", "sponza-gallery.json"];
export function galleryOptions(args: string[]): string {
  if (args.length === 0) return resolve(root, ".cache/sponza/prepared");
  if (args.length !== 2 || args[0] !== "--prepared" || !args[1] || args[1].startsWith("--")) {
    throw new Error("usage: bun scripts/prepare-sponza-gallery.ts [--prepared directory]");
  }
  return resolve(args[1]);
}
async function command(args: string[]) {
  const child = Bun.spawn(args, { cwd: root, stdout: "inherit", stderr: "inherit" });
  if (await child.exited !== 0) throw new Error("Native Sponza gallery capture failed");
}
export async function prepareSponzaGallery(
  prepared: string,
  runCommand: (args: string[]) => Promise<void> = command,
) {
  const fixturePath = join(root, "examples/sgl-3d/fixtures/sponza-sunlit.json");
  const probePath = join(root, "examples/sgl-3d/fixtures/sponza-gallery-probe.json");
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
  const placement = JSON.parse(await readFile(probePath, "utf8"));
  const inputs = [fixturePath, probePath, ...["Sponza.glb", "sky-environment.json", "sky-panorama.png", "sky-pmrem.rgba16"].map(name => join(prepared, name))];
  for (const path of inputs) if (!await Bun.file(path).exists()) throw new Error(`Missing ${path}; prepare the scene and sky first`);
  await mkdir(prepared, { recursive: true });
  await assertNoInputFileAliases(galleryFiles.map(name => join(prepared, name)), inputs);
  const work = await mkdtemp(join(prepared, ".gallery-export-"));
  try {
    // Use a small authoring framebuffer. Probe faces have their own size, and
    // exposure/tone mapping do not alter the linear HDR capture payload.
    await runCommand(["cargo", "run", "--release", "-p", "sgl-3d-examples", "--example", "bistro", "--",
      "--asset", join(prepared, "Sponza.glb"), "--fixture", fixturePath, "--view", "hero",
      "--size", "640x360", "--frames", "721", "--warmup", "720", "--capture-probe", probePath, "--output", work]);
    await rename(join(work, "specular-probe.json"), join(work, "sponza-gallery-probe.json"));
    fixture.scene = "intel-sponza-curtains-sunlit-gallery-v1";
    fixture.specular_probe = placement;
    fixture.lighting += " A locally captured specular probe replaces outdoor sky reflections inside the covered back gallery; it does not darken diffuse light or change materials.";
    fixture.notices.push("sponza-gallery-probe.json");
    await writeFile(join(work, "sponza-gallery.json"), JSON.stringify(fixture, null, 2) + "\n");
    // Per-file atomic replacement; fixture is published after the payload and
    // capture metadata. Source assets and checked-in recipes stay unchanged.
    for (const name of galleryFiles) await rename(join(work, name), join(prepared, name));
    console.log(`Prepared ${join(prepared, "sponza-gallery.json")}; rerun explicitly after source or lighting changes.`);
  } finally { await rm(work, { recursive: true, force: true }); }
}
if (import.meta.main) await prepareSponzaGallery(galleryOptions(Bun.argv.slice(2)));
