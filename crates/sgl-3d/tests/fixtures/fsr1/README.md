# FSR1 numeric oracle

`amd-reference.rgb32` was generated from AMD
FidelityFX SDK 1.1.4, revision
`c6efa6bf7f2027b3ec94f28578bb5965eabb9e55`,
`sdk/include/FidelityFX/gpu/fsr1/ffx_fsr1.h` and
`ffx_core_gpu_common.h` (MIT, see [the retained license](../../../src/LICENSE-amd-fidelityfx.txt)).

The generator extracts `ffxFsrPopulateEasuConstants`, `fsrEasuTapFloat`,
`fsrEasuSetFloat`, `ffxFsrEasuFloat`, `FsrRcasF` and the scalar approximate
reciprocal helpers directly from the SHA256-checked upstream headers. It
compiles those unchanged filter bodies with Clang's vector types and CPU
texture-gather/load callbacks. Only parameter syntax and constructors are
adapted for C++; no SGL WGSL is read. The SDK's original bit reciprocal approximations are retained.

Each fixture holds two 7×5 RGB float32 images, little-endian: EASU's gamma-2
output rounded through half floats, then RCAS's linear output before output
rounding/dither. Input is a 4×3 image: the linear RGB squares of
`((x+2*y)%4+1)/4`, `((3*x+y)%4+1)/4`, `((x+y)%3+1)/4`. Gathers and loads clamp
at image borders. RCAS uses 0.2 sharpness stops, with denoise disabled.

Regenerate with Apple Clang and Python 3:

```
python3 generate.py /path/to/FidelityFX-SDK/sdk/include/FidelityFX/gpu
```

The SDK is also vendored by `sp-fidelity` in its Cargo registry source. These
are deliberate reference fixtures; ordinary builds never regenerate them.
