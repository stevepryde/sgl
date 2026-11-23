import { expect, test } from "bun:test";
import { layout } from "../browser/startup-fixture";
import { validateStartup } from "../browser/startup";

// Hand-authored loads: one per library, as the page harness reports them.
function load(library: "sgl" | "three") {
  const steps = layout("minimal").steps.map((step, i) => ({ ...step, submitted_ms: 500 + i * 100, gpu_complete_ms: 505 + i * 100 }));
  return {
    library, scene: "minimal", cache: "cold", run: 0, marks: { first_submitted: 200 }, first_contentful_paint_ms: 220,
    device: { backend: "WebGPU", revision: library === "three" ? "187" : undefined, adapter: { vendor: "apple", architecture: "metal-3" } },
    steps, errors: [], page_errors: [], details: { errors: [] },
  };
}
const summary = () => ({ loads: [load("sgl"), load("three")] });

test("a comparison on WebGPU, one adapter, with the world painted after its frame is valid", () => {
  expect(() => validateStartup(summary())).not.toThrow();
});

test("a WebGL2 fallback, an early paint, two adapters, a page error or a missing input invalidate it", () => {
  for (const change of [
    (s: any) => s.loads[1].device.backend = "WebGL2",
    (s: any) => s.loads[1].device.revision = "186",
    (s: any) => s.loads[0].first_contentful_paint_ms = 150,
    (s: any) => s.loads[0].first_contentful_paint_ms = null,
    (s: any) => s.loads[1].device.adapter = { vendor: "intel", architecture: "gen-12lp" },
    (s: any) => s.loads[0].page_errors.push("page error: boom"),
    (s: any) => s.loads[1].details.errors.push("GPU validation failed"),
    (s: any) => s.loads[0].steps.pop(),
    (s: any) => s.loads[0] = { failed: "browser deadline (4 minutes) passed", scene: "minimal", library: "sgl", cache: "cold", run: 0 },
  ]) {
    const s = summary();
    change(s);
    expect(() => validateStartup(s)).toThrow();
  }
});
