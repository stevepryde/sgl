# Stack

- Rust 2024 with overflow checks. Existing core/net/2D workspace crates forbid
  unsafe code. SGL3D denies unsafe by default, retaining narrowly reviewed
  experimental GPU/shader opt-ins; `sgl-post-fx` forbids it.
  `sgl-input` denies unsafe except in its macOS FFI module.
- wgpu 29 and winit 0.30 for the caller-driven client renderer.
- Native transports: `std` UDP, polled from the caller's thread or owned by a
  bounded worker; tungstenite WebSocket, served by one I/O worker thread per
  server or client over nonblocking sockets with mio readiness. Browser
  transports: `web-sys` WebSocket.
- BLAKE3 where hashing or transport cookies need it.
- serde and serde_json in `sgl-2d` for the Aseprite JSON sheet loader.
- 3D presentation: Rust/wgpu through `sgl-3d`, native or on the browser's
  WebGPU, with game-owned window or canvas and input handling. SGL3D retains
  glam 0.30; `sgl-2d` retains glam 0.33.
- SGL library crates support crates.io publication, starting at 0.1.0;
  Git and local checkout dependencies remain supported. Examples are not published. Bun runs repository tooling and the
  Playwright browser tests of the transport and SGL3D.

Games commonly use glam, `image` (PNG), fontdue, RON, and postcard. Those
belong in SGL when extracted code needs them, not before.

The [SGL3D contract](sgl3d.md) defines the 3D path, native and browser. The
existing 2D renderer and browser WebSocket transport remain available.
