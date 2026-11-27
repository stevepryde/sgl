//! Retained audio engine supporting both live native playback and headless mode.

use crate::spatial::{Listener3d, SpatialMix, spatialize};
use core::fmt;
use glam::Vec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use crate::source::{StereoGains, StereoPannedSource};
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use std::io::Cursor;

/// Error returned when audio initialization or playback fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    /// Failed to open the native audio output device.
    DeviceOpen(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceOpen(message) => write!(f, "failed to open audio output device: {message}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Desired state of a tracked emitter passed to [`AudioEngine::sync_emitter`].
#[derive(Clone, Debug, PartialEq)]
pub struct EmitterState {
    /// Clip identifier loaded via [`AudioEngine::load_clip`].
    pub clip: String,
    /// Optional 3D world position; `None` plays as centered non-attenuated 2D audio.
    pub position: Option<Vec3>,
    /// Maximum audible distance in world units (`<= 0.0` disables attenuation).
    pub range: f32,
    /// Emitter linear volume multiplier (`>= 0.0`).
    pub volume: f32,
    /// Whether the emitter should be actively playing.
    pub playing: bool,
    /// Whether the emitter loops continuously while playing.
    pub looping: bool,
}

impl Default for EmitterState {
    fn default() -> Self {
        Self {
            clip: String::new(),
            position: None,
            range: 0.0,
            volume: 1.0,
            playing: false,
            looping: false,
        }
    }
}

#[cfg_attr(
    not(all(feature = "playback", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
struct ClipEntry {
    bytes: Arc<[u8]>,
    revision: u64,
}

#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
struct ActiveVoice {
    player: Player,
    gains: StereoGains,
    clip: String,
    clip_revision: u64,
    looping: bool,
}

struct TrackedEmitter {
    state: EmitterState,
    mix: SpatialMix,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    voice: Option<ActiveVoice>,
}

struct ActiveOneshot {
    volume: f32,
    spatial: Option<(Vec3, f32)>,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    player: Player,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    gains: StereoGains,
}

enum Backend {
    Headless,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    Native(MixerDeviceSink),
}

/// Spatial audio engine with live native playback and deterministic headless mode.
pub struct AudioEngine {
    backend: Backend,
    clips: HashMap<String, ClipEntry>,
    next_clip_revision: u64,
    listener: Listener3d,
    master_volume: f32,
    muted: bool,
    paused: bool,
    emitters: HashMap<u64, TrackedEmitter>,
    oneshots: Vec<ActiveOneshot>,
}

impl AudioEngine {
    /// Opens the default native audio output device when the `playback` feature
    /// is enabled on non-`wasm32` targets; otherwise returns [`Self::headless`].
    pub fn open() -> Result<Self, AudioError> {
        #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
        {
            let mut sink = DeviceSinkBuilder::open_default_sink()
                .map_err(|err| AudioError::DeviceOpen(err.to_string()))?;
            sink.log_on_drop(false);
            Ok(Self::with_backend(Backend::Native(sink)))
        }
        #[cfg(not(all(feature = "playback", not(target_arch = "wasm32"))))]
        {
            Ok(Self::headless())
        }
    }

    /// Creates a headless audio engine that never touches audio hardware.
    ///
    /// Tracks clips, listener pose, master volume, mute/pause state, emitters,
    /// and computed [`SpatialMix`] values deterministically.
    #[must_use]
    pub fn headless() -> Self {
        Self::with_backend(Backend::Headless)
    }

    fn with_backend(backend: Backend) -> Self {
        Self {
            backend,
            clips: HashMap::new(),
            next_clip_revision: 1,
            listener: Listener3d::default(),
            master_volume: 1.0,
            muted: false,
            paused: false,
            emitters: HashMap::new(),
            oneshots: Vec::new(),
        }
    }

    /// Returns `true` when running without a native audio output device.
    #[must_use]
    pub fn is_headless(&self) -> bool {
        matches!(self.backend, Backend::Headless)
    }

    /// Loads or replaces encoded audio bytes (e.g., WAV or MP3) for `id`.
    pub fn load_clip(&mut self, id: impl Into<String>, bytes: Vec<u8>) {
        let revision = self.next_clip_revision;
        self.next_clip_revision = self.next_clip_revision.wrapping_add(1).max(1);
        self.clips.insert(
            id.into(),
            ClipEntry {
                bytes: Arc::from(bytes),
                revision,
            },
        );
    }

    /// Returns whether a clip with `id` is currently loaded.
    #[must_use]
    pub fn has_clip(&self, id: &str) -> bool {
        self.clips.contains_key(id)
    }

    /// Removes a loaded clip from the cache.
    pub fn remove_clip(&mut self, id: &str) {
        self.clips.remove(id);
    }

    /// Updates the 3D listener pose used for spatialization.
    pub fn set_listener(&mut self, listener: Listener3d) {
        self.listener = listener;
    }

    /// Returns the current 3D listener pose.
    #[must_use]
    pub fn listener(&self) -> Listener3d {
        self.listener
    }

    /// Sets the master linear volume multiplier (`>= 0.0`).
    pub fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = sanitize_volume(volume);
    }

    /// Mutes or unmutes all output without losing emitter state.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Pauses or resumes all active playback.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Creates, updates, restarts (if the clip or looping mode changed), or
    /// stops the emitter identified by `id`.
    pub fn sync_emitter(&mut self, id: u64, state: EmitterState) {
        let mix = self.compute_emitter_mix(&state);
        let entry = self.emitters.entry(id).or_insert_with(|| TrackedEmitter {
            state: state.clone(),
            mix,
            #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
            voice: None,
        });
        entry.state = state;
        entry.mix = mix;
        self.sync_emitter_playback(id);
    }

    /// Stops and removes emitter `id`.
    pub fn remove_emitter(&mut self, id: u64) {
        self.emitters.remove(&id);
    }

    /// Removes all tracked emitters whose IDs are not present in `keep`.
    pub fn retain_emitters(&mut self, keep: &HashSet<u64>) {
        self.emitters.retain(|id, _| keep.contains(id));
    }

    /// Plays a one-shot clip from the loaded clip cache, optionally spatialized
    /// at `(position, range)`.
    ///
    /// Returns `true` if the clip was found (and decoded on native playback),
    /// or `false` if the clip is missing or could not be decoded.
    pub fn play_oneshot(
        &mut self,
        clip: &str,
        volume: f32,
        spatial: Option<(Vec3, f32)>,
    ) -> bool {
        let Some(entry) = self.clips.get(clip) else {
            return false;
        };
        let volume = sanitize_volume(volume);
        let mix = self.compute_mix(spatial, volume);

        #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
        if let Backend::Native(sink) = &self.backend {
            let Ok(decoder) = Decoder::new(Cursor::new(Arc::clone(&entry.bytes))) else {
                return false;
            };
            let gains = StereoGains::new(mix.left_gain, mix.right_gain);
            let source = StereoPannedSource::new(decoder, gains.clone());
            let player = Player::connect_new(sink.mixer());
            if self.paused {
                player.pause();
            }
            player.append(source);
            self.oneshots.push(ActiveOneshot {
                volume,
                spatial,
                player,
                gains,
            });
            return true;
        }

        // Headless mode: clip exists in cache.
        let _ = (entry, mix);
        self.oneshots.push(ActiveOneshot {
            volume,
            spatial,
            #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
            player: Player::new().0,
            #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
            gains: StereoGains::new(mix.left_gain, mix.right_gain),
        });
        true
    }

    /// Recomputes spatial gains for all active emitters and one-shots against
    /// the current listener, master volume, mute, and pause state, and prunes
    /// finished one-shots.
    pub fn update(&mut self) {
        let emitter_ids: Vec<u64> = self.emitters.keys().copied().collect();
        for id in emitter_ids {
            if let Some(emitter) = self.emitters.get(&id) {
                let mix = self.compute_emitter_mix(&emitter.state);
                if let Some(emitter_mut) = self.emitters.get_mut(&id) {
                    emitter_mut.mix = mix;
                }
            }
            self.sync_emitter_playback(id);
        }

        let listener = self.listener;
        let master = self.effective_master();
        let paused = self.paused;
        let headless = self.is_headless();

        self.oneshots.retain(|oneshot| {
            let mix = compute_mix_with(&listener, oneshot.spatial, oneshot.volume, master);
            #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
            {
                if headless {
                    let _ = mix;
                    return false;
                }
                if oneshot.player.empty() {
                    return false;
                }
                oneshot.gains.set(mix.left_gain, mix.right_gain);
                if paused {
                    oneshot.player.pause();
                } else {
                    oneshot.player.play();
                }
                true
            }
            #[cfg(not(all(feature = "playback", not(target_arch = "wasm32"))))]
            {
                let _ = (mix, paused, headless);
                false
            }
        });
    }

    /// Returns the last computed [`SpatialMix`] for emitter `id`, if tracked.
    #[must_use]
    pub fn emitter_mix(&self, id: u64) -> Option<SpatialMix> {
        self.emitters.get(&id).map(|emitter| emitter.mix)
    }

    fn effective_master(&self) -> f32 {
        if self.muted || self.paused {
            0.0
        } else {
            self.master_volume
        }
    }

    fn compute_emitter_mix(&self, state: &EmitterState) -> SpatialMix {
        if !state.playing {
            return SpatialMix::SILENT;
        }
        let spatial = state.position.map(|pos| (pos, state.range));
        self.compute_mix(spatial, state.volume)
    }

    fn compute_mix(&self, spatial: Option<(Vec3, f32)>, volume: f32) -> SpatialMix {
        compute_mix_with(&self.listener, spatial, volume, self.effective_master())
    }

    #[cfg_attr(
        not(all(feature = "playback", not(target_arch = "wasm32"))),
        allow(clippy::unused_self)
    )]
    fn sync_emitter_playback(&mut self, id: u64) {
        #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
        {
            let Backend::Native(sink) = &self.backend else {
                return;
            };
            let Some(emitter) = self.emitters.get_mut(&id) else {
                return;
            };
            if !emitter.state.playing {
                emitter.voice = None;
                return;
            }
            let Some(clip_entry) = self.clips.get(&emitter.state.clip) else {
                emitter.voice = None;
                return;
            };

            let needs_start = match &emitter.voice {
                None => true,
                Some(voice) => {
                    voice.clip != emitter.state.clip
                        || voice.clip_revision != clip_entry.revision
                        || voice.looping != emitter.state.looping
                }
            };

            if needs_start {
                if let Ok(decoder) = Decoder::new(Cursor::new(Arc::clone(&clip_entry.bytes))) {
                    let gains = StereoGains::new(emitter.mix.left_gain, emitter.mix.right_gain);
                    let player = Player::connect_new(sink.mixer());
                    if self.paused {
                        player.pause();
                    }
                    if emitter.state.looping {
                        let source =
                            StereoPannedSource::new(decoder.repeat_infinite(), gains.clone());
                        player.append(source);
                    } else {
                        let source = StereoPannedSource::new(decoder, gains.clone());
                        player.append(source);
                    }
                    emitter.voice = Some(ActiveVoice {
                        player,
                        gains,
                        clip: emitter.state.clip.clone(),
                        clip_revision: clip_entry.revision,
                        looping: emitter.state.looping,
                    });
                } else {
                    emitter.voice = None;
                }
            } else if let Some(voice) = &emitter.voice {
                voice
                    .gains
                    .set(emitter.mix.left_gain, emitter.mix.right_gain);
                if self.paused {
                    voice.player.pause();
                } else {
                    voice.player.play();
                }
            }
        }
        #[cfg(not(all(feature = "playback", not(target_arch = "wasm32"))))]
        {
            let _ = id;
        }
    }
}

fn sanitize_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.max(0.0)
    } else {
        0.0
    }
}

fn compute_mix_with(
    listener: &Listener3d,
    spatial: Option<(Vec3, f32)>,
    volume: f32,
    effective_master: f32,
) -> SpatialMix {
    let scaled_volume = sanitize_volume(volume) * effective_master;
    if scaled_volume == 0.0 {
        return SpatialMix::SILENT;
    }
    match spatial {
        Some((position, range)) => spatialize(listener, position, range, scaled_volume),
        None => SpatialMix::from_gain_pan(scaled_volume, 0.0),
    }
}
