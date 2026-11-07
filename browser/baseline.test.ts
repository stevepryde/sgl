import { expect, test } from "bun:test";
import { runMeasurement, validateBaseline } from "./measurement";

test("textured moving baseline returns complete objective WebGPU observations", async () => {
  const result = await runMeasurement({ page: "baseline", dist: process.env.SGL_BROWSER_PROBE_DIR ?? "target/browser-probe", query: "frames=60", timeoutMs: 180_000 });
  const report = JSON.parse(result.report);
  validateBaseline(report);
  expect(report.size).toEqual([1280,720]);
  expect(report.content.instances).toBe(257);
  expect(report.content.blended_instances).toBe(32);
  expect(report.blended_draws.max).toBeGreaterThan(0);
  expect(report.content.moving_instances).toBeGreaterThan(0);
  expect(report.content.source_texture_bytes).toBeGreaterThan(1024*1024);
  expect(report.startup.content_complete_ms).toBeGreaterThan(0);
  expect(report.streamed_upload.add_asset_instance_cpu.samples).toBe(8);
  expect(report.streamed_upload.frame_complete_wall.samples).toBe(8);
  expect(report.browser.cross_origin_isolated).toBe(true);
  expect(result.errors).toEqual([]);
}, 210_000);



for (const [name, query] of [["scaled", "scaled=1"], ["prepared", "prepared=1"]]) {
  test(`browser baseline exercises ${name} without losing frame/timer accounting`, async () => {
    const result = await runMeasurement({page:"baseline",dist:process.env.SGL_BROWSER_PROBE_DIR ?? "target/browser-probe",query:`frames=60&${query}`,timeoutMs:180_000});
    const report = JSON.parse(result.report);
    validateBaseline(report);
    expect(report[name === "scaled" ? "scaled" : "prepared_upload"]).toBe(true);
    if (name === "scaled" && report.gpu.supported) expect(report.gpu.passes["spatial upscale"].samples).toBeGreaterThan(0);
    if (name === "prepared") expect(report.startup.prepare_cpu_ms).toBeGreaterThan(0);
    expect(result.errors).toEqual([]);
  },210_000);
}
