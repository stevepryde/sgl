import { expect, test } from "bun:test";
import { webGpuBakeProvenance } from "./bistro-environment-backend.js";

test("PMREM export refuses missing adapters, uninitialized devices and WebGL fallback", () => {
  const adapter = { info: { vendor: "test-vendor" } };
  expect(() => webGpuBakeProvenance({ backend: { isWebGPUBackend: true, device: {} } }, null)).toThrow("requires an initialized WebGPU");
  expect(() => webGpuBakeProvenance({ backend: { isWebGLBackend: true, device: {} } }, adapter)).toThrow("WebGL fallback");
  expect(() => webGpuBakeProvenance({ backend: { isWebGPUBackend: true } }, adapter)).toThrow("initialized WebGPU");
  expect(webGpuBakeProvenance({ backend: { isWebGPUBackend: true, device: {} } }, adapter)).toMatchObject({ backend: "WebGPU", backend_asserted: true, adapter_available: true, probed_adapter: { vendor: "test-vendor" } });
});
