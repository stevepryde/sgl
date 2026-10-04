// The frame probe's counters (stages/frame_probe.rs): 32 words for each
// observed stage, the primary raster's coverage in words 24 to 28.
@group(0) @binding(2) var<storage,read_write> stats:array<atomic<u32>>;
