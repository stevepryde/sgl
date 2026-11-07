// Explicit content export, never a runtime prerequisite or automatic staleness
// check. Assets stay in ignored target/. See examples/sgl-3d/BISTRO.md.
import { mkdir, readFile, rename, stat, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { assertSeparateExportOutput } from "./bistro-export-files";

export const BISTRO_REVISION = "816a1d6533c55787b3fabc9b75660c0d0c57ebc6";
const repository = "qian-o/GLTF-Assets";
const scene = "BistroExterior";
const root = resolve(import.meta.dir, "..");
let output = resolve(root, "target/bistro");
let ktx = "ktx";
let maximum = 1024;
let fetchOnly = false;
const args = Bun.argv.slice(2);
for (let index = 0; index < args.length; index++) {
  const arg = args[index];
  const value = () => {
    const next = args[++index];
    if (!next) throw new Error(`${arg} requires a value`);
    return next;
  };
  switch (arg) {
    case "--output": output = resolve(value()); break;
    case "--ktx": ktx = value(); break;
    case "--max-texture": maximum = Number(value()); break;
    case "--fetch-only": fetchOnly = true; break;
    default: throw new Error("usage: bun scripts/prepare-bistro.ts [--ktx PATH] [--max-texture N] [--output DIR] [--fetch-only]");
  }
}
if (!Number.isInteger(maximum) || maximum < 1 || maximum > 16384) throw new Error("invalid --max-texture (1..16384)");
const source = resolve(output, "source");
if (!fetchOnly) await assertSeparateExportOutput(resolve(output, "prepared"), [source]);

async function download(path: string): Promise<void> {
  const destination = resolve(source, path);
  // These are a fixed asset's local relative URIs, never a destination chosen
  // by a remote URL or an extension's arbitrary external reference.
  const local = relative(source,destination);
  if (local === ".." || local.startsWith(`..${sep}`) || isAbsolute(local)) throw new Error(`asset URI escapes source directory: ${path}`);
  if (await stat(destination).then(s => s.size > 0).catch(() => false)) return;
  const raw = `https://raw.githubusercontent.com/${repository}/${BISTRO_REVISION}/Bistro/${path}`;
  let response = await fetch(raw);
  if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
  let bytes = new Uint8Array(await response.arrayBuffer());
  // Geometry is stored in Git LFS; requesting raw Git content returns a
  // pointer. media.githubusercontent.com serves the actual published bytes.
  if (bytes.length < 1024 && new TextDecoder().decode(bytes).startsWith("version https://git-lfs.github.com/spec/v1")) {
    response = await fetch(`https://media.githubusercontent.com/media/${repository}/${BISTRO_REVISION}/Bistro/${path}`);
    if (!response.ok) throw new Error(`${path} LFS: HTTP ${response.status}`);
    bytes = new Uint8Array(await response.arrayBuffer());
  }
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(`${destination}.part`, bytes);
  await rename(`${destination}.part`, destination);
}

await download(`${scene}.gltf`);
const document = JSON.parse(await readFile(resolve(source, `${scene}.gltf`), "utf8"));
const paths = [...new Set<string>([
  ...document.buffers.map((buffer: { uri: string }) => buffer.uri),
  ...document.images.map((image: { uri: string }) => image.uri),
  "LICENSE.txt", "README.md", "san_giuseppe_bridge_4k.hdr",
])];
let next = 0;
let completed = 0;
await Promise.all(Array.from({ length: 6 }, async () => {
  while (next < paths.length) {
    await download(paths[next++]);
    completed++;
    if (completed % 25 === 0 || completed === paths.length) console.log(`Bistro source: ${completed}/${paths.length}`);
  }
}));
if (fetchOnly) process.exit(0);
const result = Bun.spawnSync([
  "cargo", "run", "--release", "-p", "sgl-3d-examples", "--example", "bistro_prepare", "--",
  "--source", source, "--output", resolve(output, "prepared"), "--ktx", ktx,
  "--max-texture", String(maximum),
], { cwd: root, stdout: "inherit", stderr: "inherit" });
if (result.exitCode !== 0) process.exit(result.exitCode ?? 1);
const environment = Bun.spawnSync([
  "bun", "scripts/bistro-environment.ts", resolve(source,"san_giuseppe_bridge_4k.hdr"), resolve(output,"prepared"),
],{cwd:root,stdout:"inherit",stderr:"inherit"});
if(environment.exitCode!==0) process.exit(environment.exitCode??1);
console.log(`Bistro prepared: ${resolve(output, "prepared", `${scene}.gltf`)}`);
