// Explicit, opt-in content recipe. The source fixture and local assets are unchanged.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { assertNoInputFileAliases } from "./bistro-export-files";

export const bulbMaterials = [
  "MASTER_Light_Bulb", "Emissive_StreetLight",
  "Paris_StringLights_01_Orange_Color_Emissive",
  "Paris_StringLights_01_Red_Color_Emissive",
  "Paris_StringLights_01_White_Color_Emissive",
  "Paris_StringLights_01_Pink_Color_Emissive",
  "Paris_StringLights_01_Blue_Color_Emissive",
  "Paris_StringLights_01_Green_Color_Emissive",
];

export function practicalShadowRecipe(source: any): any {
  if (!Array.isArray(source.point_lights) || source.point_lights.length === 0)
    throw new Error("source fixture needs authored point_lights");
  if (source.transmissive_materials !== undefined)
    throw new Error("source fixture already authors transmissive_materials");
  const fixture = structuredClone(source);
  for (const light of fixture.point_lights) {
    if (!light || typeof light !== "object" || Array.isArray(light))
      throw new Error("point_lights entries must be objects");
    light.casts_shadow = true;
  }
  fixture.transmissive_materials = [...bulbMaterials];
  fixture.profile = "Opt-in shadowed practical lights with explicitly reauthored dielectric bulb shells; original quality/lean fixture remains the control.";
  fixture.lighting_authoring = {
    ...fixture.lighting_authoring,
    point_light_shadows: "All authored points request shadows. Only the eight named bulb materials become dielectric transmission=1, metallic=0; walls, fixture metal, chains and supports remain opaque casters. Atlas overflow can still leave lights unshadowed.",
    material_authoring: "Intentional content variant, not an imported source correction. Full transmission preserves source emission/maps but changes bulb appearance and follows SGL's existing transparent-surface limitations. No automatic emissive exclusion or whole-instance capture hiding.",
  };
  // A static room probe from the opaque/unshadowed rig does not describe this
  // lighting. Do not silently present it as a matching bake of the variant.
  if (fixture.specular_probe) delete fixture.specular_probe;
  return fixture;
}

if (import.meta.main) {
  const root = resolve(import.meta.dir, "..");
  let source = resolve(root, "examples/sgl-3d/fixtures/bistro-quality.json");
  let output = resolve(root, "target/practical-lights/bistro-shadowed.json");
  const args = Bun.argv.slice(2);
  for (let i = 0; i < args.length; i++) {
    const key = args[i];
    const value = args[++i];
    if (!value || !["--fixture", "--output"].includes(key))
      throw new Error("usage: bun scripts/prepare-bistro-practical-lights.ts [--fixture FILE] [--output FILE]");
    if (key === "--fixture") source = resolve(value);
    else output = resolve(value);
  }
  const fixture = practicalShadowRecipe(JSON.parse(await readFile(source, "utf8")));
  await assertNoInputFileAliases([output], [source]);
  await mkdir(dirname(output), { recursive: true });
  await writeFile(output, `${JSON.stringify(fixture, null, 2)}\n`);
  console.log(`Shadowed practical-light recipe: ${output}`);
}
