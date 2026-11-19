import { test, expect } from 'bun:test';
import { mkdtemp, mkdir, readdir, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { buildShowcase } from './showcase-build';

// Exercise actual HTTP transfers and disk artifacts: stalled/partial responses
// must fail without poisoning the cache, and a retry must keep completed work.
test('showcase downloads are bounded, recoverable, and cached only after completion', async () => {
  await mkdir('target', { recursive: true });
  const root = await mkdtemp(resolve('target/showcase-download-test-'));
  let stalled = true;
  let missing = false;
  const requests: string[] = [];
  const capture = new Uint8Array([137, 80, 78, 71, 0, 255, 128, 23]);
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch(request) {
    const path = new URL(request.url).pathname;
    requests.push(path);
    if (path.endsWith('hero.png')) return new Response(capture);
    if (missing) return new Response('not published', { status: 404 });
    if (stalled) return new Response(new ReadableStream({
      start(controller) { controller.enqueue(new Uint8Array([137, 80])); },
      // Deliberately leave the body open after headers and partial image bytes.
    }));
    return new Response(capture);
  } });
  const options = { root, origin: server.url.origin, downloadTimeoutSeconds: 1 };
  try {
    await mkdir(resolve(root, 'showcase/credits'), { recursive: true });
    await mkdir(resolve(root, 'assets/showcase'), { recursive: true });
    await Bun.write(resolve(root, 'showcase/index.html'), '<!doctype html><title>Showcase</title>');
    await Bun.write(resolve(root, 'showcase/credits/bistro.html'), '<title>Bistro credits</title>');
    await Bun.write(resolve(root, 'showcase/catalog.json'), JSON.stringify({ scenes: [{
      hero: '/renders/hero.png', thumbnail: '/renders/thumb.png', features: [],
    }] }));
    for (const name of ['BISTRO-LICENSE.txt', 'SPONZA-LICENSE.txt']) {
      await Bun.write(resolve(root, 'assets/showcase', name), `Retained ${name}`);
    }
    const started = performance.now();
    await expect(buildShowcase(options)).rejects.toThrow('Could not download /renders/thumb.png');
    expect(performance.now() - started).toBeLessThan(5000);
    expect(await Bun.file(resolve(root, '.cache/showcase/renders/hero.png')).bytes()).toEqual(capture);
    expect(await readdir(resolve(root, '.cache/showcase/renders'))).toEqual(['hero.png']);
    expect(await Bun.file(resolve(root, 'target/showcase/site/renders/thumb.png')).exists()).toBe(false);

    stalled = false; missing = true;
    await expect(buildShowcase(options)).rejects.toThrow('404');
    expect(await readdir(resolve(root, '.cache/showcase/renders'))).toEqual(['hero.png']);
    missing = false;
    await buildShowcase(options);
    expect(requests.filter((path) => path === '/renders/hero.png')).toHaveLength(1);
    expect(await Bun.file(resolve(root, 'target/showcase/site/renders/thumb.png')).bytes()).toEqual(capture);
    expect(await Bun.file(resolve(root, 'target/showcase/site/credits/BISTRO-LICENSE.txt')).text()).toBe('Retained BISTRO-LICENSE.txt');
    const downloaded = requests.length;
    await buildShowcase(options);
    expect(requests.length).toBe(downloaded);
  } finally {
    server.stop(true);
    await rm(root, { recursive: true, force: true });
  }
}, 15_000);

test('showcase deploys the live captures or the complete local refresh', async () => {
  await mkdir('target', { recursive: true });
  const root = await mkdtemp(resolve('target/showcase-refresh-test-'));
  const path = 'renders/current/terrace/on.png';
  const assembled = Bun.file(resolve(root, 'target/showcase/site', path));
  const capturedMedia = resolve(root, 'captures');
  let liveCapture = 'first published render';
  let downloads = 0;
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch() {
    downloads += 1;
    return new Response(liveCapture);
  } });
  const options = { root, origin: server.url.origin };
  try {
    await Bun.write(resolve(root, 'showcase/credits/terrace.html'), '<title>Terrace credits</title>');
    await Bun.write(resolve(root, 'showcase/catalog.json'), JSON.stringify({ scenes: [{
      hero: `/${path}`, thumbnail: `/${path}`, features: [],
    }] }));
    for (const name of ['BISTRO-LICENSE.txt', 'SPONZA-LICENSE.txt']) {
      await Bun.write(resolve(root, 'assets/showcase', name), `Retained ${name}`);
    }
    await Bun.write(resolve(root, '.cache/showcase', path), 'stale local capture');
    await buildShowcase(options);
    expect(await assembled.text()).toBe('first published render');

    liveCapture = 'newly published render';
    await buildShowcase(options);
    expect(await assembled.text()).toBe('newly published render');

    await Bun.write(resolve(capturedMedia, path), 'fresh local render');
    await buildShowcase({ ...options, capturedMedia });
    expect(await assembled.text()).toBe('fresh local render');

    await rm(resolve(capturedMedia, path));
    await expect(buildShowcase({ ...options, capturedMedia })).rejects.toThrow('ENOENT');
    expect(downloads).toBe(2);
  } finally {
    server.stop(true);
    await rm(root, { recursive: true, force: true });
  }
});
