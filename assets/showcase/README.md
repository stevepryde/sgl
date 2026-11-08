# SGL render showcase

These are unretouched PNG captures from SGL3D's real WebGPU renderer on
8 November 2026. The original capture pixels and resolution are preserved;
there is no AI image generation, compositing, or post-capture colour adjustment.
The images demonstrate authored scenes and settings, not library defaults or
a claim of performance on every device. No meshes, textures, HDR environments,
or bake payloads are distributed here.

## Bistro

[bistro-sunlit-4k.png](bistro-sunlit-4k.png) is the 3840 × 2160 sunlit quality capture, using the
`bevy` camera in the [Bistro quality fixture](../../examples/sgl-3d/fixtures/bistro-quality.json).
The sun is 70 degrees above the horizon, reaching the café terrace and most
of the street. An expanded 2,592-probe GI volume covers the visible façades
and courtyard, retaining definition in the indirect lighting.
The capture uses full geometry, 1024-pixel alpha-preserving BC7 textures, TAA,
16× anisotropic filtering, Medium AO, four High shadow cascades, Low diffuse
GI, ACES at +0.6 EV, and 111 authored local lights. The local lights do not
cast shadows in this capture. It was captured at frame 839 after 720 warm-up
frames. This capture does not use hardware ray tracing, SSR, or world-space
reflections. See the [Bistro example guide](../../examples/sgl-3d/BISTRO.md)
to run the scene.

Scene: **Amazon Lumberyard Bistro**, Amazon, distributed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
The [source conversion](https://github.com/qian-o/GLTF-Assets/tree/816a1d6533c55787b3fabc9b75660c0d0c57ebc6/Bistro)
was prepared for SGL with texture conversion, material-channel corrections,
and authored lighting and camera settings. The original
[licence text](BISTRO-LICENSE.txt) is retained.
Lighting environment: [San Giuseppe Bridge](https://polyhaven.com/a/san_giuseppe_bridge)
by Greg Zaal / Poly Haven, CC0; rotated and sun-reduced for coherent authored lighting.

## Sponza

[sponza.png](sponza.png) is the final 1920 × 1080 sunlit gallery capture, using
full-resolution rendering with no upscaling, ACES at +2.1 EV, 4,095 diffuse GI
probes covering the building, and a locally captured HDR reflection probe for
the covered gallery. The capture followed 720 warm-up and 60 measured frames.
See the [Sponza example guide](../../examples/sgl-3d/SPONZA.md) to run the scene.

Scene citation: **Frank Meinl and Anton Kaplanyan (2022), Intel Sample Library**.
[Sponza source](https://www.intel.com/content/www/us/en/developer/topic-technology/graphics-research/samples.html).
Sponza Addon Package Crew: Katica Putica, Cristiano Siqueira, Timothy Heath,
Justin Prazen, Sebastian Herholz, Bruce Cherniak, and Anton Kaplanyan.
Reference-photo credit: Katica Putica / Princino.photo, Dubrovnik, Croatia.

The original [Sponza credits and licence](SPONZA-LICENSE.txt) are retained in
full, including both CC BY 4.0 and the preamble allowing personal/educational
use and limited commercial use for marketing and print. These showcase renders
do not relicense the underlying scene or grant permission to redistribute it.
SGL's preparation combines the base scene and Curtains add-on, converts the
materials/textures, and authors the sun, sky, GI, camera, and local reflection.
Lighting environment: [Kloppenheim 05](https://polyhaven.com/a/kloppenheim_05)
by Greg Zaal / Poly Haven, CC0; processed for coherent sun/sky lighting.

The third-party scene rights above are separate from SGL's source-code licence.
