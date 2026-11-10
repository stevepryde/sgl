import { mkdir, copyFile, rm } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';

const root = resolve(import.meta.dir, '..');
const output = resolve(root, 'target/showcase/site');
const media = resolve(root, '.cache/showcase');
const catalog = await Bun.file(resolve(root, 'showcase/catalog.json')).json();
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
for await (const path of new Bun.Glob('**/*').scan({ cwd: resolve(root, 'showcase'), onlyFiles: true })) {
  if (!/\.(html|css|js|svg)$/.test(path) && !['catalog.json', '_headers'].includes(path)) continue;
  await mkdir(dirname(resolve(output, path)), { recursive: true });
  await copyFile(resolve(root, 'showcase', path), resolve(output, path));
}
// Copy only catalogued media. Original scene payloads and local diagnostic
// artifacts are never swept into the public site.
const paths = new Set<string>();
for (const scene of catalog.scenes) {
  paths.add(scene.hero); paths.add(scene.thumbnail);
  if (scene.report) paths.add(scene.report);
  for (const feature of scene.features) {
    for (const key of ['off', 'on', 'offReport', 'onReport']) if (feature[key]) paths.add(feature[key]);
  }
}
for (const path of paths) {
  await mkdir(dirname(resolve(output, path.slice(1))), { recursive: true });
  const local = Bun.file(resolve(media, path.slice(1)));
  // Existing published media are the source for a clean checkout. New captures
  // must be supplied in .cache/showcase before the first deployment.
  if (await local.exists()) await Bun.write(resolve(output, path.slice(1)), local);
  else {
    const response = await fetch(`https://sgl.inspiredjourneys.dev${path}`);
    if (!response.ok) throw new Error(`Missing ${path}: provide it in .cache/showcase or restore the published image (HTTP ${response.status}).`);
    await Bun.write(resolve(output, path.slice(1)), response);
  }
}
for (const name of ['BISTRO-LICENSE.txt', 'SPONZA-LICENSE.txt']) {
  await copyFile(resolve(root, 'assets/showcase', name), resolve(output, 'credits', name));
}
console.log(`Built ${output}`);
