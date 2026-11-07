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

test("another embedded scene uses the same timing validator and final readback contract", async () => {
  const {validateScene}=await import("../browser/measurement");
  const report={...observation(),benchmark:"embedded-scene-v1",scene:"another-scene",fixture:{schema:1,scene:"another-scene"},
    size:[640,360],capture:{width:640,height:360,excluded_from_timing:true},content:{triangles:12,glb_bytes:1000}};
  expect(()=>validateScene(report)).not.toThrow();
  for (const patch of [
    {fixture:{schema:1,scene:"wrong-scene"}}, {capture:{width:640,height:360,excluded_from_timing:false}},
    {content:{triangles:0,glb_bytes:1000}}, {schema:2}, {error:"scene binding limit exceeded"},
  ]) expect(()=>validateScene({...report,...patch})).toThrow();
});
