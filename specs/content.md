# Content

Games own their data. SGL may extract loaders that several games already wrote.

The source games typically use RON for maps and config, PNG for art, postcard
for wire messages, and sometimes Aseprite JSON for sheets. Each game defines
its own types and directories.

## Requirements

1. A game chooses its formats, schemas, and layout. Changing them does not
   require an SGL schema change.
2. Texture helpers, when extracted, decode to CPU RGBA8 and leave GPU upload
   to the renderer.
3. Game logic is Rust. Data files are data.
4. The Aseprite JSON sheet loader lives in `sgl_2d::aseprite`.

## Acceptance

- A game can change or remove its content format without changing SGL.
- Extracted loaders accept caller-supplied bytes or paths and return concrete
  decoded values.
