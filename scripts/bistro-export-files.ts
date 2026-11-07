// Fixed offline export boundaries and environment files, not a scene registry.
import { copyFile, realpath, stat } from "node:fs/promises";
import { basename, dirname, isAbsolute, relative, resolve, sep } from "node:path";

async function canonicalDestination(path: string): Promise<string> {
  path = resolve(path);
  try { return await realpath(path); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return resolve(await canonicalDestination(dirname(path)), basename(path));
  }
}

function within(path: string, directory: string): boolean {
  const local = relative(directory, path);
  return !isAbsolute(local) && local !== ".." && !local.startsWith(`..${sep}`);
}

export async function assertSeparateExportOutput(output: string, inputs: readonly string[]): Promise<void> {
  const destination = await canonicalDestination(output);
  for (const input of inputs) {
    const source = await canonicalDestination(input);
    if (within(destination, source) || within(source, destination)) throw new Error("export output must be separate from input directories (including symlink aliases)");
  }
}

export async function assertNoInputFileAliases(outputs: readonly string[], inputs: readonly string[]): Promise<void> {
  const sources = await Promise.all(inputs.map(async path => ({ path: await realpath(path), info: await stat(path) })));
  for (const output of outputs) {
    const actual = await canonicalDestination(output);
    const info = await stat(output).catch((error) => {
      if (error.code !== "ENOENT") throw error;
      return undefined;
    });
    if (sources.some(source => source.path === actual || (info && info.dev === source.info.dev && info.ino === source.info.ino))) throw new Error("export destination aliases an input file");
  }
}

export async function browserEnvironmentFileNames(source: string): Promise<string[]> {
  const files = ["environment.json", "pmrem.rgba16", "panorama.png", "THREE-LICENSE.txt"];
  // These are the only optional names produced by the explicit Bistro HDR
  // experiment. Never turn processing_metadata into an arbitrary copy path.
  for (const name of ["ENVIRONMENT-PROCESSING.json", "HDRI-ATTRIBUTION.txt"]) {
    try { await stat(resolve(source, name)); files.push(name); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  }
  const metadata = JSON.parse(await Bun.file(resolve(source, "environment.json")).text());
  if (metadata.processing_metadata !== undefined && metadata.processing_metadata !== "ENVIRONMENT-PROCESSING.json") throw new Error("unsupported environment processing_metadata filename");
  if (metadata.processing_metadata && !files.includes(metadata.processing_metadata)) throw new Error("environment processing_metadata file is missing");
  return files;
}

export async function copyBrowserEnvironment(source: string, output: string): Promise<{ name: string; bytes: number }[]> {
  const files = await browserEnvironmentFileNames(source);
  await assertNoInputFileAliases(files.map(name => resolve(output, name)), files.map(name => resolve(source, name)));
  const result = [];
  for (const name of files) {
    await copyFile(resolve(source, name), resolve(output, name));
    result.push({ name, bytes: (await stat(resolve(output, name))).size });
  }
  return result;
}
