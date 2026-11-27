//! Retained audio engine supporting live native playback, browser Web Audio
//! playback (`wasm32`), and deterministic headless mode.

use crate::spatial::{Listener3d, SpatialMix, spatialize};
use core::fmt;
use glam::Vec3;
use std::collections::{HashMap, HashSet};

#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use crate::source::{StereoGains, StereoPannedSource};
#[cfg(all(feature = "playback", target_arch = "wasm32"))]
use crate::web::{DecodedClipLookup, WebAudioBackend, WebVoice};
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use std::io::Cursor;
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
use std::sync::Arc;

/// Error returned when audio initialization or playback fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    /// Failed to open the native audio output device or browser Web Audio
    /// `AudioContext`.
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

struct ClipEntry {
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    bytes: Arc<[u8]>,
    #[cfg_attr(not(feature = "playback"), allow(dead_code))]
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

#[cfg(all(feature = "playback", target_arch = "wasm32"))]
struct ActiveWebVoice {
    voice: WebVoice,
    clip: String,
    clip_revision: u64,
    looping: bool,
}

struct TrackedEmitter {
    state: EmitterState,
    mix: SpatialMix,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    voice: Option<ActiveVoice>,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    failed_decode: Option<(String, u64)>,
    #[cfg(all(feature = "playback", target_arch = "wasm32"))]
    web_voice: Option<ActiveWebVoice>,
}

#[cfg(all(feature = "playback", target_arch = "wasm32"))]
enum WebOneshotState {
    Pending { clip: String, clip_revision: u64 },
    Playing(WebVoice),
}

struct ActiveOneshot {
    volume: f32,
    spatial: Option<(Vec3, f32)>,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    player: Player,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    gains: StereoGains,
    #[cfg(all(feature = "playback", target_arch = "wasm32"))]
    web: Option<WebOneshotState>,
}

enum Backend {
    Headless,
    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    Native(MixerDeviceSink),
    #[cfg(all(feature = "playback", target_arch = "wasm32"))]
    Web(WebAudioBackend),
}

/// Spatial audio engine with live native/Web Audio playback and deterministic
/// headless mode.
pub struct AudioEngine {
    clips: HashMap<String, ClipEntry>,
    next_clip_revision: u64,
    listener: Listener3d,
    master_volume: f32,
    muted: bool,
    paused: bool,
    // Keep voices before `backend` so they stop and disconnect before the
    // output device or Web Audio `AudioContext` is dropped.
    emitters: HashMap<u64, TrackedEmitter>,
    oneshots: Vec<ActiveOneshot>,
    backend: Backend,
}

impl AudioEngine {
    /// Opens a live audio output backend when the `playback` feature is
    /// enabled (the default native output device via `rodio`, or a browser Web
    /// Audio `AudioContext` on `wasm32`); otherwise returns [`Self::headless`].
    pub fn open() -> Result<Self, AudioError> {
        #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
        {
            let mut sink = DeviceSinkBuilder::open_default_sink()
                .map_err(|err| AudioError::DeviceOpen(err.to_string()))?;
            sink.log_on_drop(false);
            Ok(Self::with_backend(Backend::Native(sink)))
        }
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        {
            let web = WebAudioBackend::open().map_err(AudioError::DeviceOpen)?;
            Ok(Self::with_backend(Backend::Web(web)))
        }
        #[cfg(not(feature = "playback"))]
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
            clips: HashMap::new(),
            next_clip_revision: 1,
            listener: Listener3d::default(),
            master_volume: 1.0,
            muted: false,
            paused: false,
            emitters: HashMap::new(),
            oneshots: Vec::new(),
            backend,
        }
    }

    /// Returns `true` when running without a live audio output backend (native
    /// or browser Web Audio).
    #[must_use]
    pub fn is_headless(&self) -> bool {
        matches!(self.backend, Backend::Headless)
    }

    /// Attempts to unlock and resume a suspended audio backend.
    ///
    /// On `wasm32` with the `playback` feature, calls `AudioContext::resume()`
    /// if the context is currently suspended and the engine is not paused via
    /// [`Self::set_paused`]. Call this from a browser user-gesture handler
    /// (such as a pointer or key event) to satisfy autoplay policies.
    ///
    /// No-op on native targets and in headless mode.
    #[cfg_attr(
        not(all(feature = "playback", target_arch = "wasm32")),
        allow(clippy::unused_self)
    )]
    pub fn resume(&mut self) {
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        if !self.paused
            && let Backend::Web(web) = &self.backend
        {
            web.resume_if_suspended();
        }
    }

    /// Loads or replaces encoded audio bytes (e.g., WAV or MP3) for `id`.
    ///
    /// On `wasm32` with live Web Audio playback, decoding runs asynchronously
    /// via `AudioContext::decodeAudioData`; emitters and one-shots triggered
    /// immediately on the same tick start automatically on [`Self::update`]
    /// once decoding finishes.
    #[cfg_attr(
        not(all(feature = "playback", not(target_arch = "wasm32"))),
        allow(clippy::needless_pass_by_value)
    )]
    pub fn load_clip(&mut self, id: impl Into<String>, bytes: Vec<u8>) {
        let id = id.into();
        let revision = self.next_clip_revision;
        self.next_clip_revision = self.next_clip_revision.wrapping_add(1).max(1);
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        if let Backend::Web(web) = &self.backend {
            web.decode_clip(&id, revision, &bytes);
        }
        #[cfg(not(all(feature = "playback", not(target_arch = "wasm32"))))]
        let _ = bytes;
        self.clips.insert(
            id,
            ClipEntry {
                #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
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
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        if let Backend::Web(web) = &self.backend {
            web.remove_clip(id);
        }
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
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        let was_paused = self.paused;
        self.paused = paused;

        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        if let Backend::Web(web) = &self.backend {
            if paused {
                for emitter in self.emitters.values() {
                    if let Some(active) = &emitter.web_voice {
                        active.voice.set_mix(SpatialMix::SILENT);
                    }
                }
                for oneshot in &self.oneshots {
                    if let Some(WebOneshotState::Playing(voice)) = &oneshot.web {
                        voice.set_mix(SpatialMix::SILENT);
                    }
                }
                web.suspend();
            } else {
                let listener = self.listener;
                let master = self.effective_master();
                for emitter in self.emitters.values() {
                    if let Some(active) = &emitter.web_voice {
                        let mix = if emitter.state.playing {
                            compute_mix_with(
                                &listener,
                                emitter.state.position.map(|pos| (pos, emitter.state.range)),
                                emitter.state.volume,
                                master,
                            )
                        } else {
                            SpatialMix::SILENT
                        };
                        active.voice.set_mix(mix);
                    }
                }
                for oneshot in &self.oneshots {
                    if let Some(WebOneshotState::Playing(voice)) = &oneshot.web {
                        let mix =
                            compute_mix_with(&listener, oneshot.spatial, oneshot.volume, master);
                        voice.set_mix(mix);
                    }
                }
                if was_paused {
                    web.resume_after_pause();
                } else {
                    web.resume_if_suspended();
                }
            }
        }
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
            #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
            failed_decode: None,
            #[cfg(all(feature = "playback", target_arch = "wasm32"))]
            web_voice: None,
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
    /// Returns `true` if the clip was found (and decoded or queued for async
    /// decoding on live playback), or `false` if the clip is missing or failed
    /// to decode.
    pub fn play_oneshot(&mut self, clip: &str, volume: f32, spatial: Option<(Vec3, f32)>) -> bool {
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

        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        if let Backend::Web(web) = &self.backend {
            let state = match web.lookup_clip(clip, entry.revision) {
                DecodedClipLookup::Ready(buffer) => {
                    let Ok(voice) = WebVoice::start(web.context(), &buffer, false, mix, true)
                    else {
                        return false;
                    };
                    WebOneshotState::Playing(voice)
                }
                DecodedClipLookup::Decoding => WebOneshotState::Pending {
                    clip: clip.to_owned(),
                    clip_revision: entry.revision,
                },
                DecodedClipLookup::Failed | DecodedClipLookup::Missing => return false,
            };
            if !self.paused {
                web.resume_if_suspended();
            }
            self.oneshots.push(ActiveOneshot {
                volume,
                spatial,
                web: Some(state),
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
            #[cfg(all(feature = "playback", target_arch = "wasm32"))]
            web: None,
        });
        true
    }

    /// Recomputes spatial gains for all active emitters and one-shots against
    /// the current listener, master volume, mute, and pause state, starts any
    /// queued voices whose browser decode finished, and prunes finished
    /// one-shots.
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
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        let clips = &self.clips;
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        let backend = &self.backend;

        self.oneshots.retain_mut(|oneshot| {
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
            #[cfg(all(feature = "playback", target_arch = "wasm32"))]
            {
                let _ = (paused, headless);
                let Backend::Web(web) = backend else {
                    let _ = mix;
                    return false;
                };
                match &mut oneshot.web {
                    None => false,
                    Some(WebOneshotState::Playing(voice)) => {
                        if voice.has_ended() {
                            return false;
                        }
                        voice.set_mix(mix);
                        true
                    }
                    Some(state @ WebOneshotState::Pending { .. }) => {
                        let WebOneshotState::Pending {
                            clip,
                            clip_revision,
                        } = state
                        else {
                            return false;
                        };
                        if clips
                            .get(clip.as_str())
                            .is_none_or(|entry| entry.revision != *clip_revision)
                        {
                            return false;
                        }
                        match web.lookup_clip(clip, *clip_revision) {
                            DecodedClipLookup::Decoding => true,
                            DecodedClipLookup::Ready(buffer) => {
                                let Ok(voice) =
                                    WebVoice::start(web.context(), &buffer, false, mix, true)
                                else {
                                    return false;
                                };
                                *state = WebOneshotState::Playing(voice);
                                true
                            }
                            DecodedClipLookup::Failed | DecodedClipLookup::Missing => false,
                        }
                    }
                }
            }
            #[cfg(not(feature = "playback"))]
            {
                let _ = (mix, paused, headless);
                false
            }
        });

        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        if !self.paused
            && !self.oneshots.is_empty()
            && let Backend::Web(web) = &self.backend
        {
            web.resume_if_suspended();
        }
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

    #[cfg_attr(not(feature = "playback"), allow(clippy::unused_self))]
    fn sync_emitter_playback(&mut self, id: u64) {
        #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
        self.sync_emitter_playback_native(id);
        #[cfg(all(feature = "playback", target_arch = "wasm32"))]
        self.sync_emitter_playback_web(id);
        #[cfg(not(feature = "playback"))]
        {
            let _ = id;
        }
    }

    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    fn sync_emitter_playback_native(&mut self, id: u64) {
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
            if emitter.failed_decode.as_ref().is_some_and(|(clip, rev)| {
                *clip == emitter.state.clip && *rev == clip_entry.revision
            }) {
                return;
            }
            if let Ok(decoder) = Decoder::new(Cursor::new(Arc::clone(&clip_entry.bytes))) {
                emitter.failed_decode = None;
                let gains = StereoGains::new(emitter.mix.left_gain, emitter.mix.right_gain);
                let player = Player::connect_new(sink.mixer());
                if self.paused {
                    player.pause();
                }
                if emitter.state.looping {
                    let source = StereoPannedSource::new(decoder.repeat_infinite(), gains.clone());
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
                emitter.failed_decode = Some((emitter.state.clip.clone(), clip_entry.revision));
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

    #[cfg(all(feature = "playback", target_arch = "wasm32"))]
    fn sync_emitter_playback_web(&mut self, id: u64) {
        let Backend::Web(web) = &self.backend else {
            return;
        };
        let Some(emitter) = self.emitters.get_mut(&id) else {
            return;
        };
        if !emitter.state.playing {
            emitter.web_voice = None;
            return;
        }
        let Some(clip_entry) = self.clips.get(&emitter.state.clip) else {
            emitter.web_voice = None;
            return;
        };

        let needs_start = match &emitter.web_voice {
            None => true,
            Some(active) => {
                active.clip != emitter.state.clip
                    || active.clip_revision != clip_entry.revision
                    || active.looping != emitter.state.looping
            }
        };

        if needs_start {
            emitter.web_voice = None;
            match web.lookup_clip(&emitter.state.clip, clip_entry.revision) {
                DecodedClipLookup::Ready(buffer) => {
                    if let Ok(voice) = WebVoice::start(
                        web.context(),
                        &buffer,
                        emitter.state.looping,
                        emitter.mix,
                        false,
                    ) {
                        emitter.web_voice = Some(ActiveWebVoice {
                            voice,
                            clip: emitter.state.clip.clone(),
                            clip_revision: clip_entry.revision,
                            looping: emitter.state.looping,
                        });
                    }
                    if !self.paused {
                        web.resume_if_suspended();
                    }
                }
                DecodedClipLookup::Decoding => {
                    if !self.paused {
                        web.resume_if_suspended();
                    }
                }
                DecodedClipLookup::Failed | DecodedClipLookup::Missing => {}
            }
        } else if let Some(active) = &emitter.web_voice {
            active.voice.set_mix(emitter.mix);
            if !self.paused {
                web.resume_if_suspended();
            }
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
        None => SpatialMix::centered(scaled_volume),
    }
}
