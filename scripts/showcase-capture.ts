import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { buildShowcase } from './showcase-build';

const root = resolve(import.meta.dir, '..');
const { values } = parseArgs({
  args: Bun.argv.slice(2),
  options: {
    'bistro-assets': { type: 'string', default: 'target/bistro-quality' },
    'sponza-assets': { type: 'string', default: '.cache/sponza/prepared' },
    help: { type: 'boolean', short: 'h' },
  },
});
if (values.help) {
  console.log('Usage: bun scripts/showcase-capture.ts [--bistro-assets DIR] [--sponza-assets DIR]');
  process.exit(0);
}

const bistroAssets = resolve(values['bistro-assets']);
const sponzaAssets = resolve(values['sponza-assets']);
const bistro = {
  assets: bistroAssets,
  fixture: resolve(root, 'examples/sgl-3d/fixtures/bistro-quality.json'),
};
const comparisonSize = '1920x1080';
const inputs = new Map([
  ['bistro', { ...bistro, view: 'bevy', overviewSize: '3840x2160' }],
  ['bistro-motorcycle', { ...bistro, view: 'bike', overviewSize: comparisonSize }],
  ['sponza', {
    assets: sponzaAssets,
    fixture: resolve(sponzaAssets, 'sponza-gallery.json'),
    view: 'hero',
    overviewSize: comparisonSize,
  }],
]);
const featureSettings = new Map([
  ['ao', 'ambient_occlusion'],
  ['gi', 'dynamic_gi'],
  ['bloom', 'bloom'],
  ['aa', 'antialiasing'],
]);

type CaptureInput = NonNullable<ReturnType<typeof inputs.get>>;
type Capture = { image: string; report: string };
type ShowcaseScene = {
  id: string;
  hero: string;
  report: string;
  thumbnail: string;
  features: { id: string; on: string; onReport: string; off: string; offReport: string }[];
};

const catalog: { scenes: ShowcaseScene[] } = await Bun.file(resolve(root, 'showcase/catalog.json')).json();
const workspace = resolve(root, 'target/showcase');
await mkdir(workspace, { recursive: true });
const capturedMedia = await mkdtemp(resolve(workspace, 'capture-'));
const capturedFile = (path: string) => resolve(capturedMedia, path.slice(1));
let builtRenderer = false;

async function capture(
  input: CaptureInput, settings: Record<string, unknown>, size: string, output: Capture,
) {
  const image = capturedFile(output.image);
  const report = capturedFile(output.report);
  await mkdir(dirname(image), { recursive: true });
  await mkdir(dirname(report), { recursive: true });
  const settingsPath = `${image}.settings.json`;
  await Bun.write(settingsPath, JSON.stringify(settings));
  const args = ['bun', 'scripts/tasks.ts', 'measure-browser', '--fixture', input.fixture,
    '--asset-dir', input.assets, '--view', input.view, '--size', size, '--warmup', '720',
    '--frames', '60', '--settings', settingsPath, '--capture', image, '--output', report];
  if (builtRenderer) args.push('--skip-build');
  const log = Bun.file(`${image}.log`);
  console.log(`Capturing ${output.image} at ${size}`);
  const child = Bun.spawn(args, { cwd: root, stdout: log, stderr: log });
  if (await child.exited !== 0) throw new Error(`Capture failed. See ${log.name}`);
  builtRenderer = true;
}

try {
  for (const scene of catalog.scenes) {
    const input = inputs.get(scene.id);
    if (!input) throw new Error(`Missing showcase capture input for ${scene.id}`);
    const { settings } = await Bun.file(input.fixture).json();
    const baseline = scene.features[0];
    await capture(input, settings, comparisonSize, { image: baseline.on, report: baseline.onReport });
    if (scene.hero !== baseline.on) {
      await capture(input, settings, input.overviewSize, { image: scene.hero, report: scene.report });
    }
    const thumbnail = capturedFile(scene.thumbnail);
    await mkdir(dirname(thumbnail), { recursive: true });
    const preview = Bun.spawn(['cwebp', '-quiet', '-q', '85', '-resize', '640', '0',
      capturedFile(scene.hero), '-o', thumbnail], { cwd: root, stderr: 'inherit' });
    if (await preview.exited !== 0) throw new Error(`Thumbnail failed for ${scene.id}`);

    for (const feature of scene.features) {
      const field = featureSettings.get(feature.id);
      if (!field) throw new Error(`Missing showcase capture setting for ${feature.id}`);
      await capture(input, { ...settings, [field]: 'Off' }, comparisonSize, {
        image: feature.off, report: feature.offReport,
      });
    }
  }
  await buildShowcase({ root, capturedMedia });
} catch (error) {
  console.error(`Showcase capture output retained in ${capturedMedia}`);
  throw error;
}
await rm(capturedMedia, { recursive: true, force: true });
