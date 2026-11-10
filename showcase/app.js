const $ = (id) => document.getElementById(id);
const catalog = await fetch('/catalog.json').then((response) => {
  if (!response.ok) throw new Error('Could not load the scene catalog.');
  return response.json();
}).catch((error) => { $('image-status').textContent = error.message; return null; });

if (catalog) {
  let scene, feature, mode, generation = 0;
  const sceneButtons = new Map();
  for (const entry of catalog.scenes) {
    const button = document.createElement('button');
    button.type = 'button'; button.className = 'scene-card';
    const image = document.createElement('img');
    image.src = entry.thumbnail; image.alt = ''; image.width = 320; image.height = 180;
    const text = document.createElement('span');
    const title = document.createElement('strong'); title.textContent = entry.name;
    const subtitle = document.createElement('small'); subtitle.textContent = entry.subtitle;
    text.append(title, subtitle); button.append(image, text);
    button.addEventListener('click', () => navigate(entry.id, 'overview', 'on'));
    $('scene-list').append(button); sceneButtons.set(entry.id, button);
  }

  function navigate(sceneId, featureId, nextMode) {
    const params = new URLSearchParams({ scene: sceneId, feature: featureId, view: nextMode });
    history.pushState(null, '', `?${params}`);
    render();
  }

  function setSplit(value) {
    const position = Math.min(100, Math.max(0, Number(value)));
    $('split').value = String(position);
    $('split-value').textContent = `${Math.round(position)}%`;
    $('image-off').style.clipPath = `inset(0 ${100 - position}% 0 0)`;
    $('divider').style.left = `${position}%`;
  }

  function showMode() {
    const comparing = !!feature;
    $('stage').dataset.mode = mode;
    $('image-off').hidden = !comparing || mode === 'on';
    $('divider').hidden = $('image-labels').hidden = $('split-control').hidden = !comparing || mode !== 'split';
    if (mode === 'off') $('image-off').style.clipPath = 'none';
    else setSplit($('split').value);
    document.querySelectorAll('[data-mode]').forEach((button) => button.setAttribute('aria-pressed', String(button.dataset.mode === mode)));
    const capture = feature ? (mode === 'off' ? feature.off : feature.on) : scene.hero;
    $('original').href = capture;
    $('original').textContent = mode === 'split' ? 'Full resolution (on) ↗' : 'Full resolution ↗';
    const report = feature ? (mode === 'off' ? feature.offReport : feature.onReport) : scene.report;
    $('capture-report').hidden = !report;
    if (report) $('capture-report').href = report;
  }

  async function render() {
    const token = ++generation;
    const params = new URLSearchParams(location.search);
    scene = catalog.scenes.find((entry) => entry.id === params.get('scene')) ?? catalog.scenes[0];
    feature = scene.features.find((entry) => entry.id === params.get('feature'));
    mode = feature ? (['on', 'off', 'split'].includes(params.get('view')) ? params.get('view') : 'split') : 'on';
    document.title = `${scene.name}${feature ? ` · ${feature.name}` : ''} — SGL`;
    sceneButtons.forEach((button, id) => button.setAttribute('aria-pressed', String(id === scene.id)));
    $('scene-title').textContent = scene.name;
    $('scene-kind').textContent = scene.kind;
    $('scene-description').textContent = feature ? scene.comparisonDescription : scene.description;
    $('scene-credits').textContent = scene.credits;
    $('license').href = scene.license;
    $('capture-info').textContent = feature ? scene.comparisonInfo : scene.info;
    $('comparison-controls').hidden = !feature;
    $('feature-description').textContent = feature?.description ?? '';
    $('feature-list').replaceChildren();
    for (const entry of [{ id: 'overview', name: 'Showcase render' }, ...scene.features]) {
      const button = document.createElement('button');
      button.type = 'button'; button.textContent = entry.name;
      button.setAttribute('aria-label', entry.name);
      button.setAttribute('aria-pressed', String(entry.id === (feature?.id ?? 'overview')));
      button.addEventListener('click', () => navigate(scene.id, entry.id, entry.id === 'overview' ? 'on' : 'split'));
      $('feature-list').append(button);
    }
    $('image-status').hidden = false; $('image-status').textContent = 'Loading capture…';
    $('stage').setAttribute('aria-busy', 'true');
    showMode();
    // Decode both before revealing a comparison: a slow or missing image must
    // never show a stale frame from the previously selected scene or feature.
    const sources = feature ? [feature.on, feature.off] : [scene.hero];
    const selected = feature;
    try {
      const images = await Promise.all(sources.map(async (src) => {
        const image = new Image(); image.src = src; await image.decode(); return image;
      }));
      if (token !== generation) return;
      $('image-on').src = images[0].src;
      $('image-on').alt = `${scene.name}, ${selected ? `${selected.name} on` : 'showcase render'}`;
      if (selected) {
        $('image-off').src = images[1].src;
        $('image-off').alt = `${scene.name}, ${selected.name} off`;
      }
      showMode(); $('image-status').hidden = true;
    } catch {
      if (token !== generation) return;
      $('image-status').textContent = 'This capture could not be loaded. Try another scene or reload the page.';
      $('image-on').removeAttribute('src');
      $('image-off').hidden = $('divider').hidden = $('image-labels').hidden = $('split-control').hidden = true;
    } finally {
      if (token === generation) $('stage').setAttribute('aria-busy', 'false');
    }
  }

  document.querySelectorAll('[data-mode]').forEach((button) => button.addEventListener('click', () => {
    if (feature) navigate(scene.id, feature.id, button.dataset.mode);
  }));
  $('split').addEventListener('input', (event) => setSplit(event.target.value));
  function moveSplit(event) {
    const bounds = $('stage').getBoundingClientRect();
    setSplit((event.clientX - bounds.left) / bounds.width * 100);
  }
  $('stage').addEventListener('pointerdown', (event) => {
    if (mode !== 'split' || !feature || event.button !== 0) return;
    $('stage').setPointerCapture(event.pointerId); moveSplit(event);
  });
  $('stage').addEventListener('pointermove', (event) => {
    if ($('stage').hasPointerCapture(event.pointerId)) moveSplit(event);
  });
  $('share').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(location.href); $('share').textContent = 'Link copied'; }
    catch { $('share').textContent = 'Copy the address bar link'; }
    setTimeout(() => { $('share').textContent = 'Copy link'; }, 2500);
  });
  window.addEventListener('popstate', render);
  await render();
}
