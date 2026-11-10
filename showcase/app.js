const $ = (id) => document.getElementById(id);
const catalog = await fetch('/catalog.json').then((response) => {
  if (!response.ok) throw new Error('Could not load the scene catalog.');
  return response.json();
}).catch((error) => { $('image-status').textContent = error.message; return null; });

if (catalog) {
  let scene, feature, mode, generation = 0, position = 50, ready = false;
  let dragPointer = null;
  const stage = $('stage');
  const sceneButtons = new Map();
  for (const entry of catalog.scenes) {
    const button = document.createElement('button');
    button.type = 'button'; button.className = 'scene-card';
    const image = document.createElement('img');
    image.src = entry.thumbnail; image.alt = ''; image.width = 320; image.height = 180;
    image.draggable = false;
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
    position = Math.min(100, Math.max(0, Number(value)));
    $('image-off').style.clipPath = `inset(0 ${100 - position}% 0 0)`;
    $('divider').style.left = `${position}%`;
    $('divider').setAttribute('aria-valuenow', String(Math.round(position)));
    $('divider').setAttribute('aria-valuetext', `${Math.round(position)}% off, ${100 - Math.round(position)}% on`);
  }

  function endDrag() {
    if (dragPointer !== null && stage.hasPointerCapture(dragPointer)) stage.releasePointerCapture(dragPointer);
    dragPointer = null;
  }

  function showMode() {
    endDrag();
    const comparing = !!feature && ready;
    const sliding = comparing && mode === 'split';
    const toggling = comparing && mode !== 'split';
    stage.dataset.mode = comparing ? mode : 'overview';
    stage.tabIndex = toggling ? 0 : -1;
    if (toggling) {
      stage.setAttribute('role', 'button');
      stage.setAttribute('aria-label', `${feature.name}: ${mode}. Click to turn ${mode === 'on' ? 'off' : 'on'}.`);
      stage.setAttribute('aria-pressed', String(mode === 'on'));
    } else {
      stage.removeAttribute('role'); stage.removeAttribute('aria-label'); stage.removeAttribute('aria-pressed');
    }
    $('image-off').hidden = !comparing || mode === 'on';
    $('divider').hidden = $('image-labels').hidden = !sliding;
    $('current-state').hidden = !toggling;
    $('current-state').textContent = toggling ? `${feature.name} ${mode === 'on' ? 'On' : 'Off'}` : '';
    $('current-state').dataset.state = mode;
    if (mode === 'off') $('image-off').style.clipPath = 'none';
    else setSplit(position);
    document.querySelectorAll('[data-view]').forEach((button) => {
      button.setAttribute('aria-pressed', String(button.dataset.view === (mode === 'split' ? 'split' : 'toggle')));
    });
    const capture = feature ? (mode === 'off' ? feature.off : feature.on) : scene.hero;
    $('original').href = capture;
    $('original').textContent = mode === 'split' ? 'Full resolution (on) ↗' : 'Full resolution ↗';
    const report = feature ? (mode === 'off' ? feature.offReport : feature.onReport) : scene.report;
    $('capture-report').hidden = !report;
    if (report) $('capture-report').href = report;
  }

  async function render() {
    const params = new URLSearchParams(location.search);
    const nextScene = catalog.scenes.find((entry) => entry.id === params.get('scene')) ?? catalog.scenes[0];
    const nextFeature = nextScene.features.find((entry) => entry.id === params.get('feature'));
    mode = nextFeature ? (['on', 'off', 'split'].includes(params.get('view')) ? params.get('view') : 'split') : 'on';
    // View changes reuse the decoded pair, so clicking the image never flashes a loading overlay.
    if (scene === nextScene && feature === nextFeature) { showMode(); return; }
    const token = ++generation;
    scene = nextScene; feature = nextFeature; ready = false;
    document.title = `${scene.name}${feature ? ` · ${feature.name}` : ''} — SGL`;
    sceneButtons.forEach((button, id) => button.setAttribute('aria-pressed', String(id === scene.id)));
    $('scene-title').textContent = scene.name;
    $('scene-kind').textContent = scene.kind;
    $('scene-description').textContent = feature ? scene.comparisonDescription : scene.description;
    $('scene-credits').textContent = scene.credits;
    $('license').href = scene.license;
    $('capture-info').textContent = feature ? scene.comparisonInfo : scene.info;
    $('comparison-controls').hidden = !feature;
    $('feature-list').replaceChildren();
    for (const entry of [{ id: 'overview', name: 'Showcase render', description: 'The complete scene with all featured effects enabled.' }, ...scene.features]) {
      const button = document.createElement('button');
      button.type = 'button';
      const title = document.createElement('strong'); title.textContent = entry.name;
      const description = document.createElement('span'); description.textContent = entry.description;
      button.append(title, description);
      button.setAttribute('aria-label', entry.name);
      button.setAttribute('aria-pressed', String(entry.id === (feature?.id ?? 'overview')));
      button.addEventListener('click', () => navigate(scene.id, entry.id, entry.id === 'overview' ? 'on' : (!feature || mode === 'split' ? 'split' : 'on')));
      $('feature-list').append(button);
    }
    $('image-status').hidden = false; $('image-status').textContent = 'Loading capture…';
    stage.setAttribute('aria-busy', 'true');
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
      ready = true; showMode(); $('image-status').hidden = true;
    } catch {
      if (token !== generation) return;
      $('image-status').textContent = 'This capture could not be loaded. Try another scene or reload the page.';
      $('image-on').removeAttribute('src');
      showMode();
    } finally {
      if (token === generation) stage.setAttribute('aria-busy', 'false');
    }
  }

  document.querySelectorAll('[data-view]').forEach((button) => button.addEventListener('click', () => {
    if (feature) navigate(scene.id, feature.id, button.dataset.view === 'split' ? 'split' : (mode === 'off' ? 'off' : 'on'));
  }));
  function moveSplit(event) {
    const bounds = stage.getBoundingClientRect();
    setSplit((event.clientX - bounds.left) / bounds.width * 100);
  }
  stage.addEventListener('dragstart', (event) => event.preventDefault());
  stage.addEventListener('pointerdown', (event) => {
    if (!ready || mode !== 'split' || !feature || event.button !== 0 || !event.isPrimary) return;
    event.preventDefault();
    dragPointer = event.pointerId;
    stage.setPointerCapture(event.pointerId);
    $('divider').focus({ preventScroll: true });
    moveSplit(event);
  });
  stage.addEventListener('pointermove', (event) => {
    if (event.pointerId === dragPointer) moveSplit(event);
  });
  stage.addEventListener('pointerup', endDrag);
  stage.addEventListener('pointercancel', endDrag);
  stage.addEventListener('lostpointercapture', () => { dragPointer = null; });
  $('divider').addEventListener('keydown', (event) => {
    const steps = { ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1, PageDown: -10, PageUp: 10 };
    if (event.key === 'Home') setSplit(0);
    else if (event.key === 'End') setSplit(100);
    else if (event.key in steps) setSplit(position + steps[event.key]);
    else return;
    event.preventDefault();
  });
  function toggle() {
    if (ready && feature && mode !== 'split') navigate(scene.id, feature.id, mode === 'on' ? 'off' : 'on');
  }
  stage.addEventListener('click', toggle);
  stage.addEventListener('keydown', (event) => {
    if (event.target === stage && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); toggle(); }
  });
  $('share').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(location.href); $('share').textContent = 'Link copied'; }
    catch { $('share').textContent = 'Copy the address bar link'; }
    setTimeout(() => { $('share').textContent = 'Copy link'; }, 2500);
  });
  window.addEventListener('popstate', render);
  await render();
}
