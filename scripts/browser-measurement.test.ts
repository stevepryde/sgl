import { expect, test } from "bun:test";
import { validateBaseline } from "../browser/measurement";

// Hand-authored distribution observations, including an unavailable optional feature.
function observation() {
  const distribution = {samples:60,median_ms:2,p95_ms:4,max_ms:8};
  return {schema:1,scene:"textured-grid-v1",frames:60,cpu_encode:{...distribution},frame_complete_wall:{...distribution},
    camera_draws:{max:20},errors:[],gpu:{supported:false,measured_frames:0,missing_frames:60,missing_reason:"TIMESTAMP_QUERY unavailable"}};
}
test("an explicitly unavailable GPU timer leaves CPU measurements usable", () => {
  expect(() => validateBaseline(observation())).not.toThrow();
});
test("lost samples, reversed percentiles, WebGPU errors and empty draws fail", () => {
  for (const change of [
    (v:any) => v.cpu_encode.samples--,
    (v:any) => v.frame_complete_wall.p95_ms=1,
    (v:any) => v.gpu.missing_frames--,
    (v:any) => v.errors.push("GPU validation failed"),
    (v:any) => v.camera_draws.max=0,
    (v:any) => v.gpu.missing_reason=null,
    (v:any) => v.gpu.supported=true,
  ]) {
    const report = observation(); change(report);
    expect(() => validateBaseline(report)).toThrow();
  }
});
