// Explicit, local-only content setup. Download pins identify and verify the selected
// upstream archives; they are never runtime asset-staleness checks.
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, rename, rm, stat, statfs, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import manifest from "./sponza-sources.json";
import { embedPreparedGltf } from "./embedded-gltf";

const root = resolve(import.meta.dir, "..");
export async function verifyArchive(path: string, expected: string): Promise<void> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  const actual = hash.digest("hex");
  if (actual !== expected) throw new Error(`SHA-256 mismatch for ${path}: expected ${expected}, got ${actual}. Remove the bad archive and rerun; pins are not updated automatically.`);
}

async function run(command: string[]): Promise<string> {
  const child = Bun.spawn(command, { stdout: "pipe", stderr: "inherit" });
  const output = await new Response(child.stdout).text();
  if (await child.exited !== 0) throw new Error(`${command[0]} failed`);
  return output;
}

// Python's standard-library ZIP reader supports ZIP64 and checks CRCs while
// copying. Validate every member before writing any of them. Extract into a
// fresh staging directory so pre-existing symlinks cannot redirect writes.
export async function extractArchive(archive: string, destination: string): Promise<void> {
  const staging = await mkdtemp(`${destination}.extracting-`);
  try {
    await run(["python3", "-c", String.raw`
import os, pathlib, shutil, stat, sys, zipfile
archive, output = sys.argv[1:]
with zipfile.ZipFile(archive) as z:
    seen = set()
    total = 0
    for entry in z.infolist():
        name = entry.filename
        parts = name.rstrip('/').split('/')
        mode = entry.external_attr >> 16
        if (not name or '\\' in name or ':' in name or name.startswith('/') or
            any(p in ('', '.', '..') for p in parts) or
            stat.S_ISLNK(mode) or (stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR))):
            raise ValueError('Unsafe ZIP member: ' + name)
        key = name.rstrip('/').casefold()
        if key in seen:
            raise ValueError('Duplicate ZIP member: ' + name)
        seen.add(key)
        total += entry.file_size
    if total > 40 * 1024**3:
        raise ValueError('ZIP expands beyond the 40 GiB safety limit')
    if shutil.disk_usage(output).free < total + 1024**3:
        raise ValueError('Insufficient disk space to extract ZIP')
    for entry in z.infolist():
        target = pathlib.Path(output, entry.filename)
        if entry.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with z.open(entry) as source, target.open('xb') as dest:
                shutil.copyfileobj(source, dest, 1024 * 1024)
`, archive, staging]);
    await rename(staging, destination);
  } catch (error) {
    await rm(staging, { recursive: true, force: true });
    throw error;
  }
}

export async function prepareSponzaSources(cache: string, offline = false): Promise<{ base: string; curtains: string }> {
  cache = resolve(cache);
  const downloads = resolve(cache, "downloads");
  await mkdir(downloads, { recursive: true });
  const results: Record<string, string> = {};
  for (const asset of manifest.archives) {
    const archive = resolve(downloads, `${asset.id}.zip`);
    if (!await stat(archive).then(s => s.isFile()).catch(() => false)) {
      if (offline) throw new Error(`Offline cache is missing ${archive}. Run without --offline once.`);
      const disk = await statfs(downloads);
      if (disk.bavail * disk.bsize < asset.bytes + 1024 ** 3) throw new Error(`Insufficient disk space to download ${asset.id}`);
      console.log(`Downloading Intel Sponza ${asset.id} (${asset.bytes} bytes)…`);
      await run(["curl", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https", "--connect-timeout", "30", "--retry", "2", "--speed-limit", "1024", "--speed-time", "120", "--max-time", "7200", "--output", `${archive}.part`, asset.url]);
      await verifyArchive(`${archive}.part`, asset.sha256);
      await rename(`${archive}.part`, archive);
    } else {
      console.log(`Verifying cached ${asset.id} archive…`);
      await verifyArchive(archive, asset.sha256);
    }
    const source = resolve(cache, "source", asset.id);
    const scene = resolve(source, asset.gltf);
    if (!await stat(resolve(source, ".extracted.json")).then(s => s.isFile()).catch(() => false)) {
      if (await stat(source).catch(() => null)) throw new Error(`Incomplete source directory ${source}; remove it before re-extracting.`);
      await mkdir(resolve(cache, "source"), { recursive: true });
      await extractArchive(archive, source);
      await readFile(scene); // Refuse a package whose expected scene is absent.
      await writeFile(resolve(source, ".extracted.json"), JSON.stringify({ source: asset.url, gltf: asset.gltf }, null, 2));
    }
    await stat(scene);
    results[asset.id] = scene;
  }
  // Keep the complete original notices in source/, alongside this provenance.
  await writeFile(resolve(cache, "PROVENANCE.json"), JSON.stringify(manifest, null, 2) + "\n");
  return results as { base: string; curtains: string };
}

if (import.meta.main) {
  let cache = resolve(root, ".cache/sponza");
  let offline = false;
  let fetchOnly = false;
  let maximum = 512;
  for (let i = 2; i < Bun.argv.length; i++) {
    const arg = Bun.argv[i];
    if (arg === "--cache" && Bun.argv[i + 1]) cache = resolve(Bun.argv[++i]);
    else if (arg === "--offline") offline = true;
    else if (arg === "--fetch-only") fetchOnly = true;
    else if (arg === "--max-texture" && Bun.argv[i + 1]) maximum = Number(Bun.argv[++i]);
    else throw new Error("usage: bun scripts/prepare-sponza.ts [--cache DIR] [--offline] [--fetch-only] [--max-texture N]");
  }
  if (!Number.isInteger(maximum) || maximum < 1 || maximum > 16384) throw new Error("invalid --max-texture (1..16384)");
  const sources = await prepareSponzaSources(cache, offline);
  console.log(JSON.stringify(sources, null, 2));
  if (!fetchOnly) {
    const output = resolve(cache, "prepared");
    const child = Bun.spawn(["cargo", "run", ...(offline ? ["--offline"] : []), "--release", "-p", "sgl-3d-examples", "--example", "sponza_prepare", "--", "--base", sources.base, "--curtains", sources.curtains, "--output", output, "--max-texture", String(maximum)], { cwd: root, stdout: "inherit", stderr: "inherit" });
    if (await child.exited !== 0) throw new Error("Sponza conversion failed");
    const stats = await embedPreparedGltf(resolve(output, "Sponza.gltf"), resolve(output, "Sponza.glb"));
    await writeFile(resolve(output, "asset-stats.json"), JSON.stringify(stats, null, 2) + "\n");
    for (const asset of manifest.archives) {
      await copyFile(resolve(cache, "source", asset.id, asset.license), resolve(output, `${asset.id.toUpperCase()}-LICENSE.txt`));
    }
    await writeFile(resolve(output, "SOURCE.json"), JSON.stringify({ ...manifest, assetStats: "asset-stats.json", modifications: `Base and colorful curtains combined; textures resized to at most ${maximum}px; embedded GLB; neutral environment.` }, null, 2) + "\n");
    await writeFile(resolve(output, "SGL-ATTRIBUTION.txt"), `Intel Sponza 2022, commissioned by Frank Meinl, sponsored by Anton Kaplanyan.
Sponza Addon Package Crew: Katica Putica, Cristiano Siqueira, Timothy Heath,
Justin Prazen, Sebastian Herholz, Bruce Cherniak, Anton Kaplanyan.
Reference photos: Katica Putica, Princino.photo, Dubrovnik, Croatia, www.princinophoto.com.
Source: ${manifest.sourcePage}
License: Creative Commons Attribution 4.0 International (https://creativecommons.org/licenses/by/4.0/).
The original notices additionally say: For personal use and educational use.
Limited commercial use for marketing and print purposes.
Both original notices are retained verbatim. Clarify the conflicting preamble
with Intel before redistribution or other commercial use. This cache is for local evaluation.
Changes: base and colorful curtains combined, texture resize, embedded GLB,
and neutral environment. See SOURCE.json and the original credits_license files.
`);
    console.log(`Sponza prepared locally: ${resolve(output, "Sponza.glb")}`);
  }
}
