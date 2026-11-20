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
copies the media named in [catalog.json](catalog.json). A site-only build takes
the current captures from the published site. It caches historical captures in
`.cache/showcase/` and reuses completed downloads on retry. Each transfer
prints its path and has a 15-second connection timeout and a two-minute total
deadline. Both directories are ignored by Git. Each build replaces the site
output.

## Capture and build

Use the [browser setup](../CONTRIBUTING.md#setup) on a WebGPU-capable machine and
install `cwebp`. Prepare the [Bistro quality package](../examples/sgl-3d/BISTRO.md)
at `target/bistro-quality` and the [Sponza gallery package](../examples/sgl-3d/SPONZA.md)
at `.cache/sponza/prepared`. The Bistro scenes use the
[Bistro Daylight recipe](../examples/sgl-3d/BISTRO.md#bistro-daylight), whose four
local reflection probes are generated content: capture them into the Bistro
package first with the probe command in its
[reflections section](../examples/sgl-3d/BISTRO.md#bistro-daylight-reflections),
and again whenever the recipe changes. The Sponza package must include
`sponza-gallery.json` and its gallery reflection probes; regenerate them with
`bun scripts/prepare-sponza-gallery.ts` if the recipe names fewer probes than
[`sponza-gallery-probes.json`](../examples/sgl-3d/fixtures/sponza-gallery-probes.json)
or still carries the old `specular_probe` key. Run from the repository root.

```sh
bun scripts/showcase-capture.ts
```

Use `--bistro-assets DIR` and `--sponza-assets DIR` when the prepared packages
are elsewhere. The harness builds the release WebGPU example once, then uses
the Bistro terrace (`bevy`) and motorcycle (`bike`) views of
`bistro-daylight.json` and the Sponza gallery view. Over each fixture's own
settings it applies one showcase quality overlay, set at the top of the
harness: showcase renders are single stills, so every quality level is at
its highest and every feature that improved matched browser captures is on
(Ultra AO, High GI, High soft shadows, 16× anisotropic filtering, sharpening,
Long contact shadows, full-resolution Crystal SSR and world-space reflections
of the whole scene). Features that degraded a still, or that the browser
cannot run, are listed there as left out. The overlay applies to the baseline
and to each comparison, so a comparison still differs by its one setting. It
renders each 1920 × 1080 baseline and AO/GI/bloom/TAA-off comparison with 720
warm-up frames and 60 measured frames. The Bistro terrace also gets a
3840 × 2160 overview. The harness writes the PNGs, WebP thumbnails, and JSON
reports to the assembled site under `target/showcase/site/renders/current/`.
Failed runs keep their output and logs under `target/showcase/capture-*/`.

## Add a scene or comparison

1. Prepare an explicitly licensed scene using the existing
   [Bistro](../examples/sgl-3d/BISTRO.md) or
   [Sponza](../examples/sgl-3d/SPONZA.md) workflow, or supply another compatible
   glTF fixture. Preserve creator, source URL/revision, licence, environment
   attribution, and any authored changes. Scene inputs stay outside Git.
2. Add the fixture, prepared asset directory, and camera to
   [the capture harness](../scripts/showcase-capture.ts). The fixture must
   enable every feature being compared. For other features, use
   `measure-browser --settings` with one changed setting and matched capture
   parameters. Some features need authored scene content, camera motion,
   native rendering, or platform support. Shadow quality has no `Off` setting.
3. Add the scene to `catalog.json` with `id`, `name`, `subtitle`, `kind`, `hero`,
   `report`, `thumbnail`, `info`, `description`, `comparisonInfo`,
   `comparisonDescription`, `credits`, `license`, and `features`. Give each
   feature `id`, `name`, `description`, `off`, `on`, `offReport`, and `onReport`.
   Use `/renders/current/<scene>/` for generated images and reports. All
   comparisons for one scene share a baseline. The scene `report` links the
   overview capture's report. Add a credits page under `showcase/credits/` and
   keep the original licence text with it. The build copies the existing scene
   licences from `assets/showcase/`.
4. Run the harness and inspect the captures and renderer reports. Reports
   contain effective feature choices, GPU errors, and per-pass timings. Keep
   the original PNG pixels. The harness resizes the overview to make the WebP
   thumbnail for the scene selector.
5. Run `bun test scripts/showcase.test.ts` for navigation, mouse/touch dragging,
   divider keyboard controls, image-click toggling, load failures, stale image
   handling, and mobile layout.

Cloudflare Static Assets has a per-file limit; keep each PNG below the
[current upload limit](https://developers.cloudflare.com/workers/platform/limits/#static-assets).
For an oversized capture, export at a smaller resolution or choose object
storage explicitly; do not silently reduce the original download's quality.

## Publish

Install `spdeploy`, Bun, curl, and Wrangler on the publishing machine. With
the capture prerequisites above installed, regenerate the captures and publish
the site in one operation from the repository root.

```sh
spdeploy --operation refresh --no-ui
```

Supply alternate prepared packages with `--var bistro_assets=/path` and
`--var sponza_assets=/path`. For a website-only change, reuse the published
captures.

```sh
spdeploy --operation deploy --no-ui
```

The root [deploy.yml](../deploy.yml) assembles the site and its catalogued media
with `local_artifact`, then publishes through Wrangler to Workers Static Assets.
Bare `spdeploy` offers the website-only `deploy` operation by default.

The custom domain belongs to the Cloudflare account with the
`inspiredjourneys.dev` zone. Authenticate using Wrangler's account login or the
repository's approved encrypted credential mechanism; do not add plaintext
tokens or `.env` files. No deployment credentials are needed at runtime.

To inspect the refresh plan before publishing, run the dry run.

```sh
spdeploy --operation refresh --dry-run --no-ui
```

Deployment uploads the selected static files to `sgl.inspiredjourneys.dev`.
Run development checks separately. Confirm the homepage, comparison links,
PNGs, and credits over HTTPS after deployment. Use Wrangler's deployment
history and rollback command to restore a prior deployment if needed.

README image URLs stay stable, for example:

```md
[![Bistro rendered with SGL3D](https://sgl.inspiredjourneys.dev/renders/bistro-sunlit-4k.png)](https://sgl.inspiredjourneys.dev/?scene=bistro&feature=gi&view=split)
```

Keep historical capture URLs in `retainedRenders` so direct links, including
the original README images, keep working. Git retains site source, metadata,
and licences. Cloudflare hosts the render binaries.
