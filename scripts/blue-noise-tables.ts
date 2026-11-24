// Writes sgl-post-fx's blue-noise tables as the bytes its post-effect
// context uploads (post_fx_context.rs, `SOBOL_256D` and `SCRAMBLING_TILE`):
// `Sobol_256d`, an Owen-scrambled Sobol sequence of 256 samples of 256
// dimensions (256 bytes), and `ScramblingTile`, a 128×128 tile of 8-byte
// scrambling keys (131,072 bytes), each value one byte in the order the
// source lists them.
//
// The source is DiligentFX's
// PostProcess/Common/src/SamplerBlueNoiseErrorDistribution_128x128_OptimizedFor_2d2d2d2d_1spp.cpp
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b, Apache-2.0,
// crates/sgl-post-fx/vendor/DiligentFX/License.txt; Copyright Diligent
// Graphics LLC), vendored unedited under crates/sgl-post-fx/vendor/. Its
// tables are C arrays of decimal bytes, which the crate used to parse at run
// time; the binary is the same values without the text (#895).
// `post_fx_context::tests` compares them, entry for entry.
//
// Usage: bun scripts/blue-noise-tables.ts
import { mkdir, readFile, writeFile } from "node:fs/promises";

if (Bun.argv.length > 2) throw new Error("usage: bun scripts/blue-noise-tables.ts");

const CRATE = "crates/sgl-post-fx";
const SOURCE = `${CRATE}/vendor/DiligentFX/PostProcess/Common/src/SamplerBlueNoiseErrorDistribution_128x128_OptimizedFor_2d2d2d2d_1spp.cpp`;
const OUT = `${CRATE}/src/blue_noise`;

const text = await readFile(SOURCE, "utf8");

// `static const uint8_t NAME[A * B * ...] = {v, v, ...};` as bytes, checked
// against its declared size.
function table(name: string): Uint8Array {
  const match = new RegExp(`static const uint8_t ${name}\\[([^\\]]+)\\]\\s*=\\s*\\{([^}]*)\\}`).exec(text);
  if (!match) throw new Error(`${name} not found in ${SOURCE}`);
  const size = match[1].split("*").reduce((product, factor) => product * Number(factor.trim()), 1);
  const values = match[2].split(",").map((value) => value.trim()).filter((value) => value !== "");
  if (values.length !== size) throw new Error(`${name} holds ${values.length} values, declared ${size}`);
  return Uint8Array.from(values, (value) => {
    if (!/^\d+$/.test(value) || Number(value) > 255) throw new Error(`${name}: ${value} is not a byte`);
    return Number(value);
  });
}

await mkdir(OUT, { recursive: true });
await writeFile(`${OUT}/sobol_256d.bin`, table("Sobol_256d"));
await writeFile(`${OUT}/scrambling_tile.bin`, table("ScramblingTile"));
