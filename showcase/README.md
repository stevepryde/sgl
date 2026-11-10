# SGL render showcase

[sgl.inspiredjourneys.dev](https://sgl.inspiredjourneys.dev) is a static gallery
of genuine SGL3D captures. A left sidebar lists scenes, including the Bistro
terrace, Bistro motorcycle street view, and Sponza atrium. Each rendering feature
has a brief explanation. Slide mode lets you click or drag directly on the image
or divider; the focused divider also supports arrow keys, Page Up/Down and
Home/End. Toggle mode switches On/Off when you click the image (or press Enter
or Space), with the current state shown over the capture. There is no separate
slider below the image. It supports direct PNG links and shareable
scene/feature URLs. It switches captured images; it does not claim to run a
live renderer. No WebGPU support or scene download is required to view it.

The site follows the repository's existing HTML/JavaScript browser examples and
Bun tooling. It has no runtime dependencies or backend. Cloudflare Workers
Static Assets hosts the page, renders, capture reports, and original licences
together; no R2 bucket, database, or paid image transformation service is needed.
The Worker name and custom domain are in [wrangler.jsonc](wrangler.jsonc).

## Build and develop

From the repository root:

```sh
bun install --frozen-lockfile
bun scripts/showcase-build.ts
wrangler dev --config showcase/wrangler.jsonc
```

Use the installed Wrangler CLI. The build writes `target/showcase/site/` and
copies only the media named in [catalog.json](catalog.json). It takes new media
from `.cache/showcase/`, or downloads existing media from the published site
when building a clean checkout. Both directories are ignored by Git. Keep a
backup of new captures until they have been published. Do not use the build
output as the only copy of unpublished renders; each build replaces it.

The first deployment needs all catalogued media locally. Subsequent website
edits can reuse published images without downloading or preparing scene assets.

## Add a scene or comparison

1. Prepare an explicitly licensed scene using the existing
   [Bistro](../examples/sgl-3d/BISTRO.md) or
   [Sponza](../examples/sgl-3d/SPONZA.md) workflow, or supply another compatible
   glTF fixture. Preserve creator, source URL/revision, licence, environment
   attribution, and any authored changes. Scene inputs stay outside Git.
2. Capture through SGL3D. The helper below builds the browser example once,
   then captures a baseline and AO/GI/bloom/TAA-off variants at 1920 × 1080,
   with the same camera, exposure, 720 warm-up frames and 60 measured frames:

   ```sh
   bun scripts/showcase-capture.ts bistro /path/to/prepared-bistro examples/sgl-3d/fixtures/bistro-quality.json bevy 2026-11-10
   bun scripts/showcase-capture.ts bistro-motorcycle /path/to/prepared-bistro examples/sgl-3d/fixtures/bistro-quality.json bike 2026-11-11
   bun scripts/showcase-capture.ts sponza /path/to/prepared-sponza /path/to/prepared-sponza/sponza-gallery.json hero 2026-11-10
   ```

   Choose a new capture-set name when replacing published images, and update
   the catalog explicitly. The fixture must enable every feature being
   compared. For other features, use `measure-browser --settings` with one
   changed setting and matched capture parameters. Some features need authored
   scene content, camera motion, native rendering, or platform support; never
   manufacture a difference with image editing or label an unsupported fallback
   as enabled. Shadow quality has no `Off` setting.
3. Inspect the captures and renderer reports. Reports include effective feature
   choices, GPU errors, and per-pass timings. Do not present these authored
   examples as library defaults or universal performance measurements. Keep raw
   PNG pixels unchanged. A small resized WebP thumbnail is appropriate for the
   scene selector; keep it in `.cache/showcase/renders/` too.
4. Add the scene to `catalog.json`: `id`, `name`, `subtitle`, `kind`, `hero`,
   `thumbnail`, `info`, `description`, `comparisonInfo`,
   `comparisonDescription`, `credits`, `license`, and `features`. Each feature
   has `id`, `name`, `description`, `off`, `on`, and optional `offReport` and
   `onReport`. Paths are site-root-relative beneath `/renders/`. The optional
   scene `report` links the overview capture's report. Only publish comparisons
   with both images available. Add a credits page under `showcase/credits/`
   and retain the original licence text beside it (the existing scene licences
   remain in `assets/showcase/` and are copied during the build).
5. Build, then run `bun test scripts/showcase.test.ts` and the repository's
   required `bun scripts/tasks.ts check`. Tests exercise navigation, mouse/touch dragging,
   divider keyboard controls, image-click toggling, load failures, stale image
   handling, and mobile layout; they do not judge rendered appearance.

Cloudflare Static Assets has a per-file limit; keep each PNG below the
[current upload limit](https://developers.cloudflare.com/workers/static-assets/platform/limits/).
For an oversized capture, export at a smaller resolution or choose object
storage explicitly; do not silently reduce the original download's quality.

## Publish

The custom domain belongs to the Cloudflare account with the
`inspiredjourneys.dev` zone. Authenticate using Wrangler's account login or the
repository's approved encrypted credential mechanism; do not add plaintext
tokens or `.env` files. No deployment credentials are needed at runtime.

```sh
bun scripts/showcase-build.ts
wrangler deploy --config showcase/wrangler.jsonc
```

Deployment uploads the selected static files and configures only
`sgl.inspiredjourneys.dev`. It does not include library builds/tests, bake
regeneration, account inventories, or other environments. Run development
checks separately. Confirm the homepage, comparison links, PNGs, and credits
over HTTPS after deployment. Use Wrangler's deployment history and rollback
command to restore a prior deployment if needed.

README image URLs stay stable, for example:

```md
[![Bistro rendered with SGL3D](https://sgl.inspiredjourneys.dev/renders/bistro-sunlit-4k.png)](https://sgl.inspiredjourneys.dev/?scene=bistro&feature=gi&view=split)
```

Do not delete an older catalogued image that is still referenced by a README
or public link. Git retains site source, metadata, and licences—not render
binaries. No CI deployment or account resources are provisioned by the build.
