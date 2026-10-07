// Writes crates/sgl-3d/src/scene/sheen_dfg.r16: the directional albedo E of
// KHR_materials_sheen's Charlie lobe with Ashikhmin's visibility, which
// SGL3D's DFG table holds in its blue channel (scene/lookup_tables.rs) and
// sheen.wgsl reads to dim the base beneath the sheen and to weigh its
// indirect light.
//
// A port of Filament's generator (https://github.com/google/filament,
// revision ef1a133d6777299ad971a0f9612adf4aad281f46, Apache-2.0,
// crates/sgl-3d/src/LICENSE-filament.txt; Copyright (C) 2015 The Android
// Open Source Project): DFV_Charlie_Uniform, DistributionCharlie and
// VisibilityAshikhmin (libs/ibl/src/CubemapIBL.cpp 155–162, 178–181,
// 855–872), hemisphereUniformSample (66–71) and hammersley
// (libs/ibl/include/ibl/utilities.h 44–53), with CubemapIBL::DFG's 4096
// samples a texel and its roughness mapping, alpha = perceptual roughness²
// (1018–1028). Changed: evaluated in double precision, where Filament's
// are single; and the texels sit where SGL3D's DFG table (Bevy 9d12036's)
// puts them, N.V (x + 0.5) / 64 across and perceptual roughness
// (y + 0.5) / 64 down, where Filament's DFG image runs roughness up from
// (height − y + 0.5) / height.
//
// Usage: bun scripts/sheen-dfg.ts
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

if (Bun.argv.length > 2) throw new Error("usage: bun scripts/sheen-dfg.ts");

const SIZE = 64;
const SAMPLES = 4096;

function hammersley(i: number, iN: number): [number, number] {
  const tof = 0.5 / 0x80000000;
  let bits = i >>> 0;
  bits = ((bits << 16) | (bits >>> 16)) >>> 0;
  bits = (((bits & 0x55555555) << 1) | ((bits & 0xaaaaaaaa) >>> 1)) >>> 0;
  bits = (((bits & 0x33333333) << 2) | ((bits & 0xcccccccc) >>> 2)) >>> 0;
  bits = (((bits & 0x0f0f0f0f) << 4) | ((bits & 0xf0f0f0f0) >>> 4)) >>> 0;
  bits = (((bits & 0x00ff00ff) << 8) | ((bits & 0xff00ff00) >>> 8)) >>> 0;
  return [i * iN, bits * tof];
}

function hemisphereUniformSample(u: [number, number]): [number, number, number] {
  const phi = 2 * Math.PI * u[0];
  const cosTheta = 1 - u[1];
  const sinTheta = Math.sqrt(1 - cosTheta * cosTheta);
  return [sinTheta * Math.cos(phi), sinTheta * Math.sin(phi), cosTheta];
}

function distributionCharlie(NoH: number, linearRoughness: number): number {
  // Estevez and Kulla 2017, "Production Friendly Microfacet Sheen BRDF"
  const invAlpha = 1 / linearRoughness;
  const cos2h = NoH * NoH;
  const sin2h = 1 - cos2h;
  return ((2 + invAlpha) * Math.pow(sin2h, invAlpha * 0.5)) / (2 * Math.PI);
}

function visibilityAshikhmin(NoV: number, NoL: number): number {
  // Neubelt and Pettineo 2013, "Crafting a Next-gen Material Pipeline for The Order: 1886"
  return 1 / (4 * (NoL + NoV - NoL * NoV));
}

const saturate = (x: number) => Math.min(Math.max(x, 0), 1);

function dfvCharlieUniform(NoV: number, linearRoughness: number, numSamples: number): number {
  let r = 0;
  const V: [number, number, number] = [Math.sqrt(1 - NoV * NoV), 0, NoV];
  for (let i = 0; i < numSamples; i++) {
    const u = hammersley(i, 1 / numSamples);
    const H = hemisphereUniformSample(u);
    const VdotH = V[0] * H[0] + V[1] * H[1] + V[2] * H[2];
    const Lz = 2 * VdotH * H[2] - V[2];
    const VoH = saturate(VdotH);
    const NoL = saturate(Lz);
    const NoH = saturate(H[2]);
    if (NoL > 0) {
      const v = visibilityAshikhmin(NoV, NoL);
      const d = distributionCharlie(NoH, linearRoughness);
      r += v * d * NoL * VoH; // VoH comes from the Jacobian, 1/(4*VoH)
    }
  }
  // uniform sampling, the PDF is 1/2pi, 4 comes from the Jacobian
  return (r * (4 * 2 * Math.PI)) / numSamples;
}

// IEEE 754 binary16, rounded to nearest even.
function toHalf(value: number): number {
  const view = new DataView(new ArrayBuffer(4));
  view.setFloat32(0, value);
  const bits = view.getUint32(0);
  const sign = (bits >>> 16) & 0x8000;
  const exponent = (bits >>> 23) & 0xff;
  const mantissa = bits & 0x7fffff;
  if (exponent === 0xff) return sign | 0x7c00 | (mantissa ? 0x200 : 0);
  const unbiased = exponent - 127;
  if (unbiased > 15) return sign | 0x7c00;
  if (unbiased >= -14) {
    let half = ((unbiased + 15) << 10) | (mantissa >>> 13);
    const rest = mantissa & 0x1fff;
    if (rest > 0x1000 || (rest === 0x1000 && (half & 1))) half += 1;
    return sign | half;
  }
  if (unbiased < -25) return sign;
  const full = mantissa | 0x800000;
  const shift = -unbiased - 1;
  let half = full >>> shift;
  const rest = full & ((1 << shift) - 1);
  const midpoint = 1 << (shift - 1);
  if (rest > midpoint || (rest === midpoint && (half & 1))) half += 1;
  return sign | half;
}

const table = new DataView(new ArrayBuffer(SIZE * SIZE * 2));
for (let y = 0; y < SIZE; y++) {
  const roughness = (y + 0.5) / SIZE;
  for (let x = 0; x < SIZE; x++) {
    const NoV = (x + 0.5) / SIZE;
    table.setUint16((y * SIZE + x) * 2, toHalf(dfvCharlieUniform(NoV, roughness * roughness, SAMPLES)), true);
  }
}
const path = resolve(import.meta.dir, "../crates/sgl-3d/src/scene/sheen_dfg.r16");
await writeFile(path, new Uint8Array(table.buffer));
console.log(`wrote ${path}`);
