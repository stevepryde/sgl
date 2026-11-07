// The common PMREM atlas is backend-specific. Never label a WebGL fallback
// as the WebGPU export accepted by SGL's PMREM lookup.
export function webGpuBakeProvenance(renderer, adapter) {
  if (!adapter || renderer.backend?.isWebGPUBackend !== true || !renderer.backend.device) {
    throw new Error("Bistro PMREM export requires an initialized WebGPU backend and adapter; WebGL fallback is not supported");
  }
  const info = adapter.info ?? {};
  return {
    backend: "WebGPU",
    backend_asserted: true,
    adapter_available: true,
    probed_adapter: { vendor: info.vendor ?? "", architecture: info.architecture ?? "", device: info.device ?? "", description: info.description ?? "" },
  };
}
