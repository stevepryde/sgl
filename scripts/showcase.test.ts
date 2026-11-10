import { test, expect } from 'bun:test';
import { chromium } from 'playwright';
import { resolve } from 'node:path';

// Browser behavior is the boundary: wrong routing, stale asynchronous images,
// broken keyboard controls, and mobile overflow cannot be caught by parsing JS.
test('showcase switches captures, restores links, handles missing images, and fits mobile', async () => {
  const catalog = await Bun.file(resolve(import.meta.dir, '../showcase/catalog.json')).json();
  catalog.scenes = catalog.scenes.slice(0, 2).map((scene: any, index: number) => ({
    ...scene,
    hero: `/renders/${index}-hero.svg`, thumbnail: `/renders/${index}-hero.svg`,
    features: [{ id: 'ao', name: 'Ambient occlusion', description: 'Contact shading.',
      off: `/renders/${index}-off.svg`, on: `/renders/${index}-on.svg`,
      offReport: '/renders/off.json', onReport: '/renders/on.json' }],
  }));
  const requested: string[] = [];
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/catalog.json') return Response.json(catalog);
    if (path.startsWith('/renders/')) {
      requested.push(path);
      if (path === '/renders/1-off.svg') return new Response('missing', { status: 404 });
      if (path === '/renders/0-on.svg') await new Promise((done) => setTimeout(done, 100));
      return new Response('<svg xmlns="http://www.w3.org/2000/svg" width="160" height="90"><rect width="160" height="90" fill="olive"/></svg>', { headers: { 'content-type': 'image/svg+xml' } });
    }
    const file = Bun.file(resolve(import.meta.dir, '../showcase', path === '/' ? 'index.html' : path.slice(1)));
    return new Response(file);
  } });
  const browser = await chromium.launch({ channel: 'chromium' });
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    page.setDefaultTimeout(5000);
    const errors: string[] = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.port}/?scene=bistro&feature=ao&view=split`);
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#scene-title').innerText()).toBe('Bistro');
    expect(requested).toContain('/renders/0-off.svg');
    expect(requested).toContain('/renders/0-on.svg');
    expect(await page.locator('#image-off').isVisible()).toBe(true);
    await page.locator('#split').focus();
    await page.keyboard.press('ArrowRight');
    expect(await page.locator('#split-value').innerText()).toBe('51%');
    await page.getByRole('button', { name: 'Off', exact: true }).click();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#original').getAttribute('href')).toBe('/renders/0-off.svg');
    expect(await page.locator('#split-control').isVisible()).toBe(false);
    await page.getByRole('button', { name: 'On', exact: true }).click();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#image-off').isVisible()).toBe(false);
    expect(await page.locator('#original').getAttribute('href')).toBe('/renders/0-on.svg');
    await page.goBack();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#original').getAttribute('href')).toBe('/renders/0-off.svg');
    await page.getByRole('button', { name: /Sponza.*Intel/ }).click();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#scene-title').innerText()).toBe('Sponza');
    await page.getByRole('button', { name: 'Ambient occlusion', exact: true }).click();
    await page.getByText('This capture could not be loaded.', { exact: false }).waitFor();
    expect(await page.locator('#image-off').isVisible()).toBe(false);
    // A subsequent selection must recover from the failed image.
    await page.getByRole('button', { name: /Bistro.*Amazon/ }).click();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    let release: () => void = () => {};
    let started: () => void = () => {};
    const blocked = new Promise<void>((resolve) => { release = resolve; });
    const loading = new Promise<void>((resolve) => { started = resolve; });
    await page.route('**/renders/0-on.svg', async (route) => {
      started(); await blocked; await route.continue();
    });
    await page.getByRole('button', { name: 'Ambient occlusion', exact: true }).click();
    await loading;
    await page.getByRole('button', { name: /Sponza.*Intel/ }).click();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    const oldImage = page.waitForResponse('**/renders/0-on.svg');
    release(); await oldImage;
    // Let the intentionally delayed, superseded image finish decoding.
    await page.waitForTimeout(200);
    expect(await page.locator('#image-on').getAttribute('alt')).toBe('Sponza, showcase render');
    await page.setViewportSize({ width: 375, height: 812 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    expect(await page.getByRole('button', { name: 'Showcase render' }).isVisible()).toBe(true);
    expect(errors).toEqual([]);
  } finally { await browser.close(); server.stop(true); }
}, 30_000);
