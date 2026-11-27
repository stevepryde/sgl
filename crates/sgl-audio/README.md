# sgl-audio

Deterministic 3D spatialization and safe audio playback for SGL games.

## Features

- **Deterministic 3D spatialization** (`Listener3d`, `SpatialMix`, `spatialize`):
  always available with zero audio-device dependencies, suitable for headless
  tests, servers, and `wasm32` (`--no-default-features` compiles with zero
  audio or `web-sys` dependencies).
- **Safe live stereo panning** (`AudioEngine`):
  - **Native**: backed by `rodio` behind the default `playback` feature, with
    lock-free atomic stereo gain updates so moving emitters and listeners pan
    smoothly without recreating audio streams or using `unsafe` code.
  - **Browser (`wasm32`)**: backed by the Web Audio API (`AudioContext`,
    `AudioBufferSourceNode` -> `StereoPannerNode` -> `GainNode` -> destination)
    behind the same default `playback` feature, with asynchronous clip decoding
    and live pan/gain updates.
- **Headless mode** (`AudioEngine::headless`): tracks loaded clips, listener
  pose, master volume, mute/pause state, emitters, and computed `SpatialMix`
  without opening an audio output device.

## Browser Web Audio Playback (`wasm32`)

- **Autoplay unlock**: browsers typically start a new `AudioContext` in the
  `suspended` state until a user gesture occurs. Call `AudioEngine::resume()`
  from a user input handler (pointer/key event) to unlock playback cleanly;
  `AudioEngine` also automatically attempts to resume a suspended context when
  unpaused playback is active (`play_oneshot`, `sync_emitter`, `update`).
- **Asynchronous decoding & same-tick play**: `load_clip` starts decoding
  asynchronously via `AudioContext::decodeAudioData`. Emitters and one-shots
  triggered on the same tick before decoding finishes are queued and start
  automatically on `update()` once decoded.
- **Supported formats**: on native targets, WAV, PCM, and MP3 are decoded via
  `rodio`/`symphonia`. In browsers, decoding is delegated to the browser's
  `AudioContext::decodeAudioData`, supporting standard browser-decoded formats
  such as WAV, MP3, Ogg Vorbis/Opus (where supported by the browser), and
  AAC/MP4.
- **Fallback**: if `AudioContext` creation fails, `AudioEngine::open()` returns
  `Err(AudioError::DeviceOpen(_))` so games can fall back to
  `AudioEngine::headless()`.

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
