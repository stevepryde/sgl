import { mkdir, copyFile, rename, rm } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';

export async function buildShowcase({
  root = resolve(import.meta.dir, '..'),
  origin = 'https://sgl.inspiredjourneys.dev',
  downloadTimeoutSeconds = 120,
} = {}) {
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
  let completed = 0;
  for (const path of paths) {
    const cached = resolve(media, path.slice(1));
    const destination = resolve(output, path.slice(1));
    const progress = `[${++completed}/${paths.size}]`;
    if (await Bun.file(cached).exists()) console.log(`${progress} Using cached ${path}`);
    else {
      console.log(`${progress} Downloading ${path} (timeout ${downloadTimeoutSeconds}s)`);
      await mkdir(dirname(cached), { recursive: true });
      const partial = `${cached}.${process.pid}.part`;
      try {
        // Bun 1.3.9's fetch/Response-to-file path can spin indefinitely on these
        // captures. Use curl's bounded transfer, as the scene preparation does.
        const child = Bun.spawn(['curl', '--fail', '--location', '--silent', '--show-error',
          '--connect-timeout', '15', '--max-time', String(downloadTimeoutSeconds),
          '--output', partial, `${origin}${path}`], { stdout: 'ignore', stderr: 'pipe' });
        const [status, error] = await Promise.all([child.exited, new Response(child.stderr).text()]);
        if (status !== 0) throw new Error(`Could not download ${path}: ${error.trim()}\nRerun to reuse completed downloads. If this capture is unpublished, place it at ${cached}.`);
        // Only complete transfers become reusable captures. Failures and timeouts
        // must never leave a truncated image under its final cache filename.
        await rename(partial, cached);
        console.log(`${progress} Downloaded ${path}`);
      } finally {
        await rm(partial, { force: true });
      }
    }
    await mkdir(dirname(destination), { recursive: true });
    await copyFile(cached, destination);
  }
  for (const name of ['BISTRO-LICENSE.txt', 'SPONZA-LICENSE.txt']) {
    await copyFile(resolve(root, 'assets/showcase', name), resolve(output, 'credits', name));
  }
  console.log(`Built ${output}`);
}

if (import.meta.main) await buildShowcase();
