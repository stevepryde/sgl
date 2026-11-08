import { expect, test } from "bun:test";
import { bulbMaterials, practicalShadowRecipe } from "./prepare-bistro-practical-lights";

test("practical shadow recipe keeps light placement/energy and source untouched", async () => {
  for (const profile of ["quality", "lean"]) {
    const source = await Bun.file(new URL(`../examples/sgl-3d/fixtures/bistro-${profile}.json`, import.meta.url)).json();
    const original = structuredClone(source);
    const authored = practicalShadowRecipe(source);
    expect(source).toEqual(original);
    expect(authored.point_lights).toHaveLength(111);
    expect(authored.point_lights).toEqual(source.point_lights.map((light: any) => ({ ...light, casts_shadow: true })));
    expect(authored.transmissive_materials).toEqual(bulbMaterials);
    expect(authored.settings).toEqual(source.settings);
    expect(authored.cameras).toEqual(source.cameras);
    expect(authored.exposure_stops).toEqual(source.exposure_stops);
    expect(authored.specular_probe).toBeUndefined();
    expect(authored.transmissive_materials).not.toContain("Streetlight_Metal");
  }
});

test("recipe rejects absent lights and preexisting material authoring", () => {
  for (const source of [{}, { point_lights: [] }, { point_lights: [null] }, { point_lights: [{}], transmissive_materials: [] }])
    expect(() => practicalShadowRecipe(source)).toThrow();
});
