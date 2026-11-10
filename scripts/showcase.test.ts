import { test, expect } from 'bun:test';
import { chromium } from 'playwright';
import { resolve } from 'node:path';

// Browser behavior is the boundary: wrong routing, stale asynchronous images,
// broken keyboard controls, and mobile overflow cannot be caught by parsing JS.
test('showcase switches captures, restores links, handles missing images, and fits mobile', async () => {
  const catalog = await Bun.file(resolve(import.meta.dir, '../showcase/catalog.json')).json();
  catalog.scenes = catalog.scenes.filter((scene: any) => ['bistro', 'sponza'].includes(scene.id)).map((scene: any, index: number) => ({
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
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, hasTouch: true });
    page.setDefaultTimeout(5000);
    const errors: string[] = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.port}/?scene=bistro&feature=ao&view=split`);
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#scene-title').innerText()).toBe('Bistro');
    expect(requested).toContain('/renders/0-off.svg');
    expect(requested).toContain('/renders/0-on.svg');
    expect(await page.locator('#image-off').isVisible()).toBe(true);
    const slider = page.getByRole('slider', { name: 'Comparison position' });
    await slider.focus();
    await page.keyboard.press('ArrowRight');
    expect(await slider.getAttribute('aria-valuenow')).toBe('51');
    await page.keyboard.press('End');
    expect(await page.locator('#image-off').evaluate((image) => getComputedStyle(image).clipPath)).toBe('inset(0px 0% 0px 0px)');
    await page.keyboard.press('Home');
    expect(await slider.getAttribute('aria-valuenow')).toBe('0');
    const stage = page.locator('#stage');
    const bounds = (await stage.boundingBox())!;
    const y = bounds.y + bounds.height / 2;
    let nativeDrags = 0;
    await page.exposeFunction('recordNativeDrag', () => { nativeDrags++; });
    await page.evaluate(() => document.addEventListener('dragstart', () => (window as any).recordNativeDrag()));
    // Start on the image and drag through the handle: native image drag used to steal this gesture.
    await page.mouse.move(bounds.x + bounds.width * .25, y);
    await page.mouse.down();
    await page.mouse.move(bounds.x + bounds.width * .75, y, { steps: 12 });
    expect(Number(await slider.getAttribute('aria-valuenow'))).toBeCloseTo(75, 0);
    await page.mouse.up();
    await page.mouse.move(bounds.x + bounds.width * .4, y);
    expect(Number(await slider.getAttribute('aria-valuenow'))).toBeCloseTo(75, 0);
    expect(nativeDrags).toBe(0);
    // The handle captures the pointer beyond the image, clamps, and releases on mouse-up.
    await slider.locator('span').hover();
    await page.mouse.down();
    await page.mouse.move(bounds.x + bounds.width + 15, y, { steps: 8 });
    expect(await slider.getAttribute('aria-valuenow')).toBe('100');
    await page.mouse.up();
    await stage.click({ position: { x: bounds.width * .3, y: bounds.height * .5 } });
    expect(Number(await slider.getAttribute('aria-valuenow'))).toBeCloseTo(30, 0);
    // Touch input follows the same divider, without scrolling or starting native dragging.
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: bounds.x + bounds.width * .3, y }] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: bounds.x + bounds.width * .6, y }] });
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    expect(Number(await slider.getAttribute('aria-valuenow'))).toBeCloseTo(60, 0);
    await cdp.detach();
    await page.getByRole('button', { name: 'Toggle', exact: true }).click();
    expect(await slider.isVisible()).toBe(false);
    expect(await page.locator('#current-state').innerText()).toBe('Ambient occlusion On');
    const requestsBeforeToggle = requested.length;
    await stage.click();
    expect(await page.locator('#original').getAttribute('href')).toBe('/renders/0-off.svg');
    expect(await page.locator('#current-state').innerText()).toBe('Ambient occlusion Off');
    expect(await page.locator('#capture-report').getAttribute('href')).toBe('/renders/off.json');
    expect(await page.locator('#image-status').isVisible()).toBe(false);
    await stage.focus();
    await page.keyboard.press('Space');
    expect(await page.locator('#image-off').isVisible()).toBe(false);
    expect(await page.locator('#original').getAttribute('href')).toBe('/renders/0-on.svg');
    expect(requested.length).toBe(requestsBeforeToggle);
    await page.goBack();
    expect(await page.locator('#original').getAttribute('href')).toBe('/renders/0-off.svg');
    await page.getByRole('button', { name: 'Slide', exact: true }).click();
    expect(Number(await slider.getAttribute('aria-valuenow'))).toBeCloseTo(60, 0);
    expect(await page.locator('#current-state').isVisible()).toBe(false);
    await page.getByRole('button', { name: /Sponza.*Curtained/ }).click();
    await page.locator('#image-status').waitFor({ state: 'hidden' });
    expect(await page.locator('#scene-title').innerText()).toBe('Sponza');
    await page.getByRole('button', { name: 'Ambient occlusion', exact: true }).click();
    await page.getByText('This capture could not be loaded.', { exact: false }).waitFor();
    expect(await page.locator('#image-off').isVisible()).toBe(false);
    // A subsequent selection must recover from the failed image.
    await page.getByRole('button', { name: /Bistro.*Café/ }).click();
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
    await page.getByRole('button', { name: /Sponza.*Curtained/ }).click();
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
