// The startup comparison's page side, shared by SGL and three.js
// (examples/sgl-3d/BROWSER_BASELINE.md, "Startup against three.js"). Each
// library supplies an adapter; this module calls it in the same order on
// both and timestamps every phase from navigation start. The page paints
// nothing but the canvas, so First Contentful Paint is the first world frame.
//
// adapter: { library, init(), device(canvas, path) -> info, decode(glb, layout),
//   upload(layout), view(layout), prepare(), frame(seconds, state), completion(),
//   details() -> what ran and its WebGPU errors }

const FRAMES_AFTER = 30;

// The longest the page went without an animation frame while `work` ran,
// from its start to its end: how long a loading screen drawn each frame
// would stall. A library that decodes on the page's thread blocks it.
async function framesDuring(work) {
  const times = [performance.now()];
  let running = true;
  const tick = () => {
    if (!running) return;
    times.push(performance.now());
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  await work();
  running = false;
  times.push(performance.now());
  let longest = 0;
  for (let i = 1; i < times.length; i++) longest = Math.max(longest, times[i] - times[i - 1]);
  return longest;
}

export function run(adapter) {
  const query = new URLSearchParams(location.search);
  const scene = query.get("scene");
  const path = query.get("path") ?? "ordinary";
  const marks = {};
  const mark = (name) => { marks[name] = performance.now(); };
  mark("harness_start");
  const report = { schema: 1, benchmark: "startup-v1", library: adapter.library, scene, path, marks, steps: [], errors: [] };
  const status = { state: "loading", frames: 0, steps: 0, error: null };
  window.__startup = status;

  let fcp = null;
  const events = [];
  new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) if (entry.name === "first-contentful-paint") fcp = entry.startTime;
  }).observe({ type: "paint", buffered: true });
  // Event Timing reports an interaction's duration to the presentation of
  // the next frame, but only from 16 ms; the first input always.
  const observe = (list) => events.push(...list.getEntries());
  new PerformanceObserver(observe).observe({ type: "event", buffered: true, durationThreshold: 16 });
  new PerformanceObserver(observe).observe({ type: "first-input", buffered: true });

  const fail = (error) => {
    report.errors.push(String(error?.stack ?? error));
    status.error = String(error);
    status.state = "failed";
  };
  const state = { yaw: 0, mask: false, effect: false };
  const frameStarts = [];
  let pending = null;
  let layout;

  function tick(now) {
    if (status.state === "failed" || status.state === "stopped") return;
    const index = frameStarts.length;
    frameStarts.push(now);
    const start = performance.now();
    try {
      adapter.frame(index / 60, state);
    } catch (error) {
      fail(error);
      return;
    }
    const submitted = performance.now();
    if (index === 0) {
      marks.first_frame_start = start;
      marks.first_submitted = submitted;
      adapter.completion().then(() => { marks.first_gpu_complete = performance.now(); }, fail);
    }
    if (pending) {
      const step = pending;
      pending = null;
      Object.assign(step, { frame: index, render_start_ms: start, submitted_ms: submitted });
      adapter.completion().then(() => { step.gpu_complete_ms = performance.now(); }, fail);
    }
    status.frames = frameStarts.length;
    if (status.state === "loading" && index >= FRAMES_AFTER && marks.first_gpu_complete) status.state = "running";
    requestAnimationFrame(tick);
  }

  addEventListener("keydown", (event) => {
    if (!layout || event.repeat) return;
    const planned = layout.steps[report.steps.length];
    if (!planned || planned.key !== event.code) return fail(`unexpected key ${event.code}`);
    const step = { ...planned, event_ms: event.timeStamp, handler_ms: performance.now() };
    if (planned.kind === "camera") state.yaw += layout.yaw_step_degrees;
    else if (planned.kind.startsWith("material")) state.mask = !state.mask;
    else state.effect = !state.effect;
    report.steps.push(step);
    pending = step;
  });

  // The runner sends the next key once the previous step's frame reached
  // the GPU and FRAMES_AFTER more frames followed it.
  status.stepDone = (i) => {
    const step = report.steps[i];
    return status.state === "failed" || (step?.gpu_complete_ms !== undefined && frameStarts.length > step.frame + FRAMES_AFTER);
  };

  // Ends the render loop once the runner has what it needs.
  status.stop = () => {
    if (status.state !== "failed") status.state = "stopped";
  };

  status.finish = () => {
    const gap = (from) => {
      let longest = 0;
      for (let i = from + 1; i <= from + FRAMES_AFTER && i < frameStarts.length; i++) longest = Math.max(longest, frameStarts[i] - frameStarts[i - 1]);
      return longest;
    };
    report.first_contentful_paint_ms = fcp;
    report.startup_longest_frame_gap_ms = gap(0);
    for (const step of report.steps) {
      const entry = events.find((e) => e.name === "keydown" && Math.abs(e.startTime - step.event_ms) < 0.5);
      step.presented_ms = entry ? entry.startTime + entry.duration : null;
      step.event_processing_ms = entry ? [entry.processingStart, entry.processingEnd] : null;
      step.longest_frame_gap_ms = gap(step.frame);
    }
    report.frames = frameStarts.length;
    report.details = adapter.details();
    report.resources = [performance.getEntriesByType("navigation")[0], ...performance.getEntriesByType("resource")].map((e) => ({
      name: new URL(e.name).pathname, start_ms: e.startTime, response_end_ms: e.responseEnd,
      transfer_bytes: e.transferSize, encoded_bytes: e.encodedBodySize, decoded_bytes: e.decodedBodySize,
    }));
    report.browser = { user_agent: navigator.userAgent, device_pixel_ratio: devicePixelRatio, cross_origin_isolated: crossOriginIsolated };
    return JSON.stringify(report);
  };

  (async () => {
    await adapter.init();
    mark("code_ready");
    report.device = await adapter.device(document.getElementById("view"), path);
    mark("device_ready");
    layout = await (await fetch(`${scene}.json`)).json();
    const glb = new Uint8Array(await (await fetch(`${scene}.glb`)).arrayBuffer());
    mark("asset_fetched");
    report.asset_decode_longest_frame_gap_ms = await framesDuring(() => adapter.decode(glb, layout));
    mark("asset_decoded");
    await adapter.upload(layout);
    mark("asset_uploaded");
    await adapter.view(layout);
    mark("view_ready");
    if (path === "prepared") {
      await adapter.prepare();
      mark("prepared");
    }
    requestAnimationFrame(tick);
  })().catch(fail);
}
