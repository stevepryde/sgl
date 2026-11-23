// The startup comparison's content (examples/sgl-3d/BROWSER_BASELINE.md,
// "Startup against three.js"): one GLB and one layout per scene, which SGL
// and three.js both fetch and load. Geometry and textures follow the
// baseline's textured grid (browser_baseline.rs `cuboid` and `asset`).
import { deflateSync } from "node:zlib";

export type SceneName = "minimal" | "opening";
export const SCENES: SceneName[] = ["minimal", "opening"];

/** Column-major 4×4, as glTF and both libraries store it. */
type Matrix = number[];
const pose = (t: [number, number, number], s: [number, number, number]): Matrix =>
  [s[0], 0, 0, 0, 0, s[1], 0, 0, 0, 0, s[2], 0, t[0], t[1], t[2], 1];

export function layout(scene: SceneName) {
  const opening = scene === "opening";
  const prototypes = opening ? 16 : 1;
  const instances: { prototype: number; matrix: Matrix; moving: boolean }[] = [];
  if (opening) {
    for (let n = 0; n < 256; n++) {
      instances.push({
        prototype: n % 16,
        matrix: pose([(n % 16) * 1.8 - 13.5, 0, Math.floor(n / 16) * 1.8 - 13.5], [1, 0.6 + (n % 7) * 0.35, 1]),
        moving: n % 8 === 0,
      });
    }
  } else {
    instances.push({ prototype: 0, matrix: pose([0, 0, 0], [1, 1, 1]), moving: true });
  }
  // The ground: prototype 0 flattened, as the baseline's.
  instances.push({ prototype: 0, matrix: pose([0, -0.1, 0], opening ? [34, 0.1, 34] : [6, 0.1, 6]), moving: false });
  const lights = opening
    ? Array.from({ length: 8 }, (_, n) => ({
        position: [(n % 4) * 7 - 10.5, 4, Math.floor(n / 4) * 14 - 7],
        color: [1, 0.7, 0.4], intensity: 40, range: 12,
      }))
    : [];
  return {
    schema: 1,
    scene,
    size: [1280, 720],
    profile: scene,
    texture_size: opening ? 512 : 256,
    prototypes,
    // The node after the prototypes: alpha-masked and double-sided, drawn
    // by neither scene until the scripted input spawns it.
    mask_prototype: prototypes,
    mask_matrix: opening ? pose([12, 1, 0], [2, 3, 2]) : pose([1.4, 0, 0.2], [0.8, 1.2, 0.8]),
    instances,
    lights,
    sun: {
      direction: [-0.4, -1, -0.3], color: [1, 1, 1], illuminance: 3,
      shadow: opening ? { distance: 65, cascades: 4 } : null,
    },
    hemisphere: { sky: [0.3, 0.4, 0.6], ground: [0.1, 0.1, 0.1], intensity: 0.5 },
    camera: opening
      ? { eye: [24, 13, 0], target: [0, 1, 0], fov_y_degrees: 65, near: 0.1, far: 200 }
      : { eye: [2.6, 1.8, 2.6], target: [0, 0.5, 0], fov_y_degrees: 65, near: 0.1, far: 200 },
    yaw_step_degrees: 15,
    // Each key press the runner sends, in order, waited out.
    steps: [
      { key: "ArrowRight", kind: "camera" },
      { key: "ArrowRight", kind: "camera" },
      { key: "KeyM", kind: "material", first_use: true },
      { key: "KeyM", kind: "material-off" },
      { key: "KeyM", kind: "material", first_use: false },
      ...(opening ? [
        { key: "KeyO", kind: "effect", first_use: true },
        { key: "KeyO", kind: "effect-off" },
        { key: "KeyO", kind: "effect", first_use: false },
      ] : []),
    ],
  };
}

/** browser_baseline.rs `cuboid`: a unit box on the ground, eight cells a face edge, 768 triangles. */
function cuboid() {
  const positions: number[] = [], normals: number[] = [], uvs: number[] = [], indices: number[] = [];
  const faces: [number[], number[], number[]][] = [
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]], [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
    [[0, 1, 0], [1, 0, 0], [0, 0, -1]], [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 0], [0, 1, 0]], [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
  ];
  for (const [normal, u, v] of faces) {
    for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) {
      const start = positions.length / 3;
      for (const [dx, dy] of [[0, 0], [1, 0], [1, 1], [0, 1]]) {
        const s = ((x + dx) / 8) * 2 - 1, t = ((y + dy) / 8) * 2 - 1;
        for (let i = 0; i < 3; i++) positions.push((normal[i] + u[i] * s + v[i] * t) * 0.5 + (i === 1 ? 0.5 : 0));
        normals.push(...normal);
        uvs.push((s + 1) * 0.5, (t + 1) * 0.5);
      }
      indices.push(...[0, 1, 2, 0, 2, 3].map((i) => start + i));
    }
  }
  return { positions, normals, uvs, indices };
}

function png(size: number, pixel: (x: number, y: number) => [number, number, number, number]): Uint8Array {
  // Sub-filtered rows: a small deterministic noise keeps decoding real work
  // without making the file the raw pixels' size.
  const raw = new Uint8Array(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    const row = y * (size * 4 + 1);
    raw[row] = 1;
    let previous = [0, 0, 0, 0];
    for (let x = 0; x < size; x++) {
      const value = pixel(x, y);
      for (let c = 0; c < 4; c++) raw[row + 1 + x * 4 + c] = (value[c] - previous[c]) & 255;
      previous = value;
    }
  }
  const chunk = (type: string, data: Uint8Array) => {
    const out = new Uint8Array(12 + data.length);
    const view = new DataView(out.buffer);
    view.setUint32(0, data.length);
    out.set(new TextEncoder().encode(type), 4);
    out.set(data, 8);
    view.setUint32(8 + data.length, Bun.hash.crc32(out.subarray(4, 8 + data.length)));
    return out;
  };
  const header = new Uint8Array(13);
  new DataView(header.buffer).setUint32(0, size);
  new DataView(header.buffer).setUint32(4, size);
  header.set([8, 6, 0, 0, 0], 8);
  return concat([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", header),
    chunk("IDAT", new Uint8Array(deflateSync(raw))), chunk("IEND", new Uint8Array())]);
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) { out.set(part, at); at += part.length; }
  return out;
}

function noise(x: number, y: number, n: number): number {
  let h = Math.imul(x, 374761393) ^ Math.imul(y, 668265263) ^ Math.imul(n + 1, 2246822519);
  h = Math.imul(h ^ (h >>> 13), 1274126177);
  return ((h ^ (h >>> 16)) & 15) - 8;
}

const clamp = (value: number) => Math.max(0, Math.min(255, value));

/** One GLB: `prototypes` cuboids with the baseline's materials, then the masked one. */
export function glb(scene: SceneName): Uint8Array {
  const plan = layout(scene);
  const mesh = cuboid();
  const blobs: Uint8Array[] = [];
  const bufferViews: object[] = [];
  let offset = 0;
  const view = (bytes: Uint8Array, target?: number) => {
    const padded = concat([bytes, new Uint8Array((4 - (bytes.length % 4)) % 4)]);
    bufferViews.push({ buffer: 0, byteOffset: offset, byteLength: bytes.length, ...(target ? { target } : {}) });
    blobs.push(padded);
    offset += padded.length;
    return bufferViews.length - 1;
  };
  const f32 = (values: number[]) => new Uint8Array(new Float32Array(values).buffer);
  const min = [0, 1, 2].map((i) => Math.min(...mesh.positions.filter((_, j) => j % 3 === i)));
  const max = [0, 1, 2].map((i) => Math.max(...mesh.positions.filter((_, j) => j % 3 === i)));
  const vertices = mesh.positions.length / 3;
  const accessors = [
    { bufferView: view(f32(mesh.positions), 34962), componentType: 5126, count: vertices, type: "VEC3", min, max },
    { bufferView: view(f32(mesh.normals), 34962), componentType: 5126, count: vertices, type: "VEC3" },
    { bufferView: view(f32(mesh.uvs), 34962), componentType: 5126, count: vertices, type: "VEC2" },
    { bufferView: view(new Uint8Array(new Uint16Array(mesh.indices).buffer), 34963), componentType: 5123, count: mesh.indices.length, type: "SCALAR" },
  ];
  const size = plan.texture_size, block = size / 16;
  const images: object[] = [], materials: object[] = [];
  for (let n = 0; n < plan.prototypes; n++) {
    const image = png(size, (x, y) => {
      const checker = (Math.floor(x / block) + Math.floor(y / block)) % 2;
      return [clamp(80 + n * 7 + noise(x, y, n)), clamp(90 + checker * 70 + noise(y, x, n)), clamp(150 - n * 4 + noise(x + y, y, n)), 255];
    });
    images.push({ bufferView: view(image), mimeType: "image/png" });
    const blended = n % 16 >= 14;
    materials.push({
      name: `startup ${n}`,
      pbrMetallicRoughness: { baseColorTexture: { index: n }, baseColorFactor: [1, 1, 1, blended ? 0.45 : 1],
        metallicFactor: n % 3 === 0 ? 0.8 : 0, roughnessFactor: 0.15 + (n % 16) * 0.045 },
      ...(blended ? { alphaMode: "BLEND" } : {}),
    });
  }
  const maskImage = png(size, (x, y) => {
    const hole = (Math.floor(x / block) + Math.floor(y / block)) % 2;
    return [clamp(70 + noise(x, y, 99)), clamp(160 + noise(y, x, 99)), clamp(60 + noise(x, x + y, 99)), hole ? 0 : 255];
  });
  images.push({ bufferView: view(maskImage), mimeType: "image/png" });
  materials.push({
    name: "startup mask",
    pbrMetallicRoughness: { baseColorTexture: { index: plan.prototypes }, metallicFactor: 0, roughnessFactor: 0.6 },
    alphaMode: "MASK", alphaCutoff: 0.5, doubleSided: true,
  });
  const count = plan.prototypes + 1;
  const json = {
    asset: { version: "2.0", generator: "SGL startup measurement" },
    scene: 0,
    scenes: [{ nodes: Array.from({ length: count }, (_, i) => i) }],
    nodes: Array.from({ length: count }, (_, i) => ({ name: i < plan.prototypes ? `prototype ${i}` : "mask", mesh: i })),
    meshes: Array.from({ length: count }, (_, i) => ({
      name: i < plan.prototypes ? `prototype ${i}` : "mask",
      primitives: [{ attributes: { POSITION: 0, NORMAL: 1, TEXCOORD_0: 2 }, indices: 3, material: i }],
    })),
    materials,
    textures: Array.from({ length: count }, (_, i) => ({ sampler: 0, source: i })),
    samplers: [{ magFilter: 9729, minFilter: 9987, wrapS: 10497, wrapT: 10497 }],
    images,
    accessors,
    bufferViews,
    buffers: [{ byteLength: offset }],
  };
  let text = new TextEncoder().encode(JSON.stringify(json));
  text = concat([text, new Uint8Array((4 - (text.length % 4)) % 4).fill(0x20)]);
  const bin = concat(blobs);
  const header = new Uint8Array(12);
  const total = 12 + 8 + text.length + 8 + bin.length;
  new DataView(header.buffer).setUint32(0, 0x46546c67, true);
  new DataView(header.buffer).setUint32(4, 2, true);
  new DataView(header.buffer).setUint32(8, total, true);
  const chunkHeader = (length: number, type: number) => {
    const out = new Uint8Array(8);
    new DataView(out.buffer).setUint32(0, length, true);
    new DataView(out.buffer).setUint32(4, type, true);
    return out;
  };
  return concat([header, chunkHeader(text.length, 0x4e4f534a), text, chunkHeader(bin.length, 0x004e4942), bin]);
}
