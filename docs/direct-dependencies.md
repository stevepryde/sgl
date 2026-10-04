# Direct dependencies

Key runtime dependencies and their roles are listed below. Exact dependencies
and versions live in the package manifests and `Cargo.lock`.

| Dependency | Purpose |
|---|---|
| wgpu | Persistent native/WebGPU sprite and 3D rendering |
| winit | Window handle type used to create a surface; the game owns the loop |
| bytemuck | Checked GPU buffer byte views |
| glam | Shared 2D math and renderer matrices |
| image | CPU-side PNG/JPEG decoding and capture encoding |
| gltf | 3D model, material, rig, and animation loading |
| ktx2, ruzstd, bcdec_rs | Block-compressed material containers, Zstandard decompression, and BC7 decoding |
| rustc-hash | Fast keys for renderer draw-list bins |
| serde, serde_json | Structured data and Aseprite metadata decoding |
| fontdue | On-demand glyph rasterization for canvas text |
| fastrand | Seeded deterministic RNG compatibility for extracted games |
| zeroize | Clearing sensitive immediate-UI text buffers |
| gilrs | Windows/Linux controller discovery, mappings and events |
| objc2, objc2-game-controller, block2 | Main-thread macOS Game Controller bindings and owned callbacks |
| pollster | Native blocking wrapper for async canvas GPU initialization |
| tungstenite | Native WebSocket handshake and frames |
| mio | Native WebSocket readiness: one worker waits on every socket without `unsafe` |
| wasm-bindgen, js-sys, web-sys | Rust/browser interop and WebSocket transport |
| getrandom, blake3 | UDP cookies and deterministic hashing |
| sp-fidelity, sp-fidelity-wgpu | AMD FSR2 port and its wgpu backend, run by SGL3D's FSR2 antialiasing |

`wasm-bindgen-futures` and `console_error_panic_hook` are used by browser-facing
code, the direct game example and SGL3D's browser smoke test.

`bun scripts/license-notices.ts check` verifies the locked dependency graph,
`licenses/cargo-license-inventory.json`, and `THIRD_PARTY_NOTICES.txt`.
