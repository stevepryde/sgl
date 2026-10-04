import { beforeAll, expect, test } from "bun:test";

import {
  cargoMetadata,
  duplicateVersions,
  hostTriple,
  normalDependencies,
  type ResolvedMetadata,
} from "./dependency-boundaries";

const GPU = ["wgpu", "winit"];
const NET_IO = ["tungstenite", "getrandom", "mio", "socket2", "web-sys"];

// What each crate must never link, on any target. Every entry is a spec
// requirement: architecture.md 3-4, netcode.md acceptance, rendering.md
// acceptance, and core.md 1.
const forbidden: Record<string, string[]> = {
  "sgl-core": ["sgl-net", "sgl-2d", ...GPU, ...NET_IO, "image", "fontdue"],
  "sgl-net": ["sgl-2d", "sgl-core", ...GPU, "image", "fontdue"],
  "sgl-2d": ["sgl-net", "tungstenite"],
  "sgl-direct-game": ["sgl-net"],
};

let host: ResolvedMetadata;
let wasm: ResolvedMetadata;

beforeAll(async () => {
  [host, wasm] = await Promise.all([
    hostTriple().then(cargoMetadata),
    cargoMetadata("wasm32-unknown-unknown"),
  ]);
});

for (const [crate, banned] of Object.entries(forbidden)) {
  test(`${crate} links none of ${banned.join(", ")} on native or wasm32`, () => {
    for (const [platform, metadata] of [
      ["native", host],
      ["wasm32", wasm],
    ] as const) {
      const reached = normalDependencies(metadata, crate);
      const hits = banned.filter((name) => reached.has(name));
      expect(hits, `${crate} on ${platform}`).toEqual([]);
    }
  });
}

test("sgl-net on wasm32 links no native socket or entropy crates", () => {
  const reached = normalDependencies(wasm, "sgl-net");
  expect(["tungstenite", "getrandom"].filter((n) => reached.has(n))).toEqual([]);
});

// sgl-3d retains its proven glam 0.30 math while the 2D client uses glam 0.33.
test("the workspace resolves one version of wgpu and wasm-bindgen", () => {
  const dupes = duplicateVersions(host);
  expect(["wgpu", "wasm-bindgen"].filter((n) => dupes.has(n))).toEqual([]);
});

// The checker must be able to see a real edge, or the tests above would pass
// on an empty graph. sgl-2d links wgpu natively by design (stack.md).
test("the checker sees sgl-2d's wgpu edge", () => {
  expect(normalDependencies(host, "sgl-2d").has("wgpu")).toBe(true);
  expect(normalDependencies(wasm, "sgl-2d").has("web-sys")).toBe(true);
});
