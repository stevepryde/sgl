# sgl-audio

Deterministic 3D spatialization and safe audio playback for SGL games.

## Features

- **Deterministic 3D spatialization** (`Listener3d`, `SpatialMix`, `spatialize`):
  always available with zero audio-device dependencies, suitable for headless
  tests, servers, and `wasm32`.
- **Safe live stereo panning** (`AudioEngine`): native playback backed by
  `rodio` behind the default `playback` feature, with lock-free atomic stereo
  gain updates so moving emitters and listeners pan smoothly without recreating
  audio streams or using `unsafe` code.
- **Headless mode** (`AudioEngine::headless`): tracks loaded clips, listener
  pose, master volume, mute/pause state, emitters, and computed `SpatialMix`
  without opening an audio output device.

## Spatialization Model

- **Distance attenuation**: quadratic smooth rolloff from `1.0` at distance `0`
  to exact `0.0` at distance `>= range` (`t = clamp(1 - d / range, 0, 1)`,
  `atten = t * t`). When `range <= 0`, the emitter is treated as non-attenuated
  2D audio centered at `pan = 0`.
- **Constant-power stereo panning**: `pan = clamp(dir.dot(listener.right()), -1, 1)`
  (and `0` when `d < 1e-4`), mapped via `theta = (pan + 1) * PI / 4` to
  `left_gain = gain * cos(theta)` and `right_gain = gain * sin(theta)`,
  preserving unity stereo energy (`left_gain^2 + right_gain^2 == gain^2`) across
  all pan positions.
