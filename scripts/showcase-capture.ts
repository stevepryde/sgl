import { resolve } from 'node:path';
import { mkdir } from 'node:fs/promises';

const [scene, assetDirectory, fixturePath, view, edition] = Bun.argv.slice(2);
if (!scene || !assetDirectory || !fixturePath || !view || !edition || !/^[a-z0-9-]+$/.test(scene) || !/^[a-z0-9-]+$/.test(edition)) {
  throw new Error('usage: bun scripts/showcase-capture.ts SCENE ASSET_DIRECTORY FIXTURE VIEW CAPTURE_SET');
}
const fixture = await Bun.file(fixturePath).json();
const directory = resolve('.cache/showcase/renders', scene, edition);
await mkdir(directory, { recursive: true });
const variants = [
  ['on', {}],
  ['ao-off', { ambient_occlusion: 'Off' }],
  ['gi-off', { dynamic_gi: 'Off' }],
  ['bloom-off', { bloom: 'Off' }],
  ['aa-off', { antialiasing: 'Off' }],
] as const;
for (const [name, overrides] of variants) {
  const settingsPath = resolve(directory, `${name}-settings.json`);
  await Bun.write(settingsPath, JSON.stringify({ ...fixture.settings, ...overrides }));
  const args = ['bun', 'scripts/tasks.ts', 'measure-browser', '--fixture', fixturePath,
    '--asset-dir', assetDirectory, '--view', view, '--size', '1920x1080', '--warmup', '720', '--frames', '60',
    '--settings', settingsPath, '--capture', resolve(directory, `${name}.png`), '--output', resolve(directory, `${name}.json`)];
  // Build once and reuse that module for all variants of this capture set.
  if (name !== 'on') args.push('--skip-build');
  console.log(`Capturing ${scene}/${name}`);
  const log = Bun.file(resolve(directory, `${name}.log`));
  const process = Bun.spawn(args, { stdout: log, stderr: log });
  if (await process.exited !== 0) throw new Error(`Capture failed: ${directory}/${name}.log`);
  console.log(`Saved ${scene}/${name}`);
}
