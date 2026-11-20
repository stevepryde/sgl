// Explicit native local-reflection authoring; no runtime bakes or staleness gates.
import { mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { assertNoInputFileAliases } from "./bistro-export-files";

const root = resolve(import.meta.dir, "..");
const fixturePath = join(root, "examples/sgl-3d/fixtures/sponza-sunlit.json");
export const placementsPath = join(root, "examples/sgl-3d/fixtures/sponza-gallery-probes.json");
export const recipeName = "sponza-gallery.json";
type Placement = { rgba16: string };

export function galleryOptions(args: string[]): string {
  if (args.length === 0) return resolve(root, ".cache/sponza/prepared");
  if (args.length !== 2 || args[0] !== "--prepared" || !args[1] || args[1].startsWith("--")) {
    throw new Error("usage: bun scripts/prepare-sponza-gallery.ts [--prepared directory]");
  }
  return resolve(args[1]);
}
// Each placement's payload and, beside it, the capture metadata the native
// example writes as NAME.json for NAME.rgba16.
function probeFiles(placements: Placement[]): { payload: string; metadata: string }[] {
  return placements.map(({ rgba16: payload }) => {
    if (!payload.endsWith(".rgba16")) throw new Error(`Probe payload ${payload} must be named NAME.rgba16`);
    return { payload, metadata: payload.replace(/\.rgba16$/, ".json") };
  });
}
async function command(args: string[]) {
  const child = Bun.spawn(args, { cwd: root, stdout: "inherit", stderr: "inherit" });
  if (await child.exited !== 0) throw new Error("Native Sponza gallery capture failed");
}
// Captures every placement in one run of the renderer, then publishes the
// payloads, their metadata and the derived recipe, which names them all.
export async function prepareSponzaGallery(
  prepared: string,
  runCommand: (args: string[]) => Promise<void> = command,
) {
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
  const placements: Placement[] = JSON.parse(await readFile(placementsPath, "utf8")).specular_probes;
  const files = probeFiles(placements);
  const outputs = [...files.flatMap(f => [f.payload, f.metadata]), recipeName];
  const inputs = [fixturePath, placementsPath, ...["Sponza.glb", "sky-environment.json", "sky-panorama.png", "sky-pmrem.rgba16"].map(name => join(prepared, name))];
  for (const path of inputs) if (!await Bun.file(path).exists()) throw new Error(`Missing ${path}; prepare the scene and sky first`);
  await mkdir(prepared, { recursive: true });
  await assertNoInputFileAliases(outputs.map(name => join(prepared, name)), inputs);
  const work = await mkdtemp(join(prepared, ".gallery-export-"));
  try {
    // Use a small authoring framebuffer. Probe faces have their own size, and
    // exposure/tone mapping do not alter the linear HDR capture payload. The
    // capture renders the recipe without probes, so none sees another.
    await runCommand(["cargo", "run", "--release", "-p", "sgl-3d-examples", "--example", "bistro", "--",
      "--asset", join(prepared, "Sponza.glb"), "--fixture", fixturePath, "--view", "hero",
      "--size", "640x360", "--frames", "721", "--warmup", "720", "--capture-probe", placementsPath, "--output", work]);
    // Publish nothing unless every probe was captured.
    for (const name of outputs.slice(0, -1)) {
      if (!await stat(join(work, name)).then(s => s.isFile(), () => false)) throw new Error(`Capture did not write ${name}`);
    }
    fixture.scene = "intel-sponza-curtains-sunlit-gallery-v2";
    fixture.specular_probes = placements;
    fixture.lighting += ` ${placements.length} locally captured specular probes stand in for the sky's reflection inside the covered galleries, where the dynamic GI volume's sky visibility occludes it; they do not change diffuse light or materials.`;
    fixture.notices.push(...files.map(f => f.metadata));
    await writeFile(join(work, recipeName), JSON.stringify(fixture, null, 2) + "\n");
    // Per-file atomic replacement; the recipe is published after every payload
    // and its capture metadata. Source assets and checked-in recipes stay unchanged.
    for (const name of outputs) await rename(join(work, name), join(prepared, name));
    console.log(`Prepared ${join(prepared, recipeName)} with ${placements.length} probes; rerun explicitly after source or lighting changes.`);
  } finally { await rm(work, { recursive: true, force: true }); }
}
if (import.meta.main) await prepareSponzaGallery(galleryOptions(Bun.argv.slice(2)));
