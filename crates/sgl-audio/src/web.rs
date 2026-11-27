//! Safe browser Web Audio playback backend for `wasm32`.

#![cfg(all(feature = "playback", target_arch = "wasm32"))]

use crate::spatial::SpatialMix;
use core::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AudioBuffer, AudioBufferSourceNode, AudioContext, AudioContextState, AudioScheduledSourceNode,
    Event, GainNode, StereoPannerNode,
};

struct DecodedClipEntry {
    revision: u64,
    state: DecodedClipState,
}

enum DecodedClipState {
    Decoding,
    Ready(AudioBuffer),
    Failed,
}

pub(crate) enum DecodedClipLookup {
    Decoding,
    Ready(AudioBuffer),
    Failed,
    Missing,
}

/// Active Web Audio voice graph:
/// `AudioBufferSourceNode -> StereoPannerNode -> GainNode -> destination`.
pub(crate) struct WebVoice {
    source: AudioBufferSourceNode,
    panner: StereoPannerNode,
    gain: GainNode,
    ended: Rc<Cell<bool>>,
    _onended: Option<Closure<dyn FnMut(Event)>>,
}

impl WebVoice {
    pub(crate) fn start(
        context: &AudioContext,
        buffer: &AudioBuffer,
        looping: bool,
        mix: SpatialMix,
        track_ended: bool,
    ) -> Result<Self, JsValue> {
        let source = context.create_buffer_source()?;
        let panner = context.create_stereo_panner()?;
        let gain = context.create_gain()?;

        source.set_buffer(Some(buffer));
        source.set_loop(looping);

        let ended = Rc::new(Cell::new(false));
        let onended = if track_ended {
            let ended_flag = Rc::clone(&ended);
            let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                ended_flag.set(true);
            });
            AudioScheduledSourceNode::set_onended(&source, Some(closure.as_ref().unchecked_ref()));
            Some(closure)
        } else {
            None
        };

        let voice = Self {
            source,
            panner,
            gain,
            ended,
            _onended: onended,
        };
        voice.set_mix(mix);

        voice.source.connect_with_audio_node(&voice.panner)?;
        voice.panner.connect_with_audio_node(&voice.gain)?;
        voice.gain.connect_with_audio_node(&context.destination())?;
        voice.source.start()?;

        Ok(voice)
    }

    pub(crate) fn set_mix(&self, mix: SpatialMix) {
        let pan = if mix.pan.is_finite() {
            mix.pan.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let gain = if mix.gain.is_finite() {
            mix.gain.max(0.0)
        } else {
            0.0
        };
        self.panner.pan().set_value(pan);
        self.gain.gain().set_value(gain);
    }

    #[must_use]
    pub(crate) fn has_ended(&self) -> bool {
        self.ended.get()
    }
}

impl Drop for WebVoice {
    fn drop(&mut self) {
        AudioScheduledSourceNode::set_onended(&self.source, None);
        let _ = AudioScheduledSourceNode::stop(&self.source);
        let _ = self.source.disconnect();
        let _ = self.panner.disconnect();
        let _ = self.gain.disconnect();
    }
}

/// Live Web Audio backend owning an `AudioContext` and asynchronous decode cache.
pub(crate) struct WebAudioBackend {
    context: AudioContext,
    decoded_clips: Rc<RefCell<HashMap<String, DecodedClipEntry>>>,
    desired_paused: Rc<Cell<bool>>,
    auto_resume_in_flight: Rc<Cell<bool>>,
}

impl WebAudioBackend {
    pub(crate) fn open() -> Result<Self, String> {
        let context = AudioContext::new().map_err(|err| js_error_message(&err))?;
        Ok(Self {
            context,
            decoded_clips: Rc::new(RefCell::new(HashMap::new())),
            desired_paused: Rc::new(Cell::new(false)),
            auto_resume_in_flight: Rc::new(Cell::new(false)),
        })
    }

    pub(crate) fn context(&self) -> &AudioContext {
        &self.context
    }

    /// Best-effort automatic resume during playback/update, throttled so a
    /// pre-gesture pending promise does not spam `context.resume()` every tick.
    /// Never suppresses explicit [`Self::resume_now`] calls.
    pub(crate) fn resume_if_suspended(&self) {
        if self.desired_paused.get()
            || self.context.state() != AudioContextState::Suspended
            || self.auto_resume_in_flight.get()
        {
            return;
        }
        let Ok(promise) = self.context.resume() else {
            return;
        };
        self.auto_resume_in_flight.set(true);
        let in_flight = Rc::clone(&self.auto_resume_in_flight);
        let desired_paused = Rc::clone(&self.desired_paused);
        let context = self.context.clone();
        spawn_local(async move {
            let _ = JsFuture::from(promise).await;
            in_flight.set(false);
            reconcile_context_state(&context, &desired_paused);
        });
    }

    /// Unconditionally invokes `AudioContext::resume()` synchronously when the
    /// context is not closed, preserving transient browser user activation even
    /// if an earlier pre-gesture `resume()` promise is still pending.
    pub(crate) fn resume_now(&self) {
        self.desired_paused.set(false);
        if self.context.state() == AudioContextState::Closed {
            return;
        }
        let Ok(promise) = self.context.resume() else {
            return;
        };
        let desired_paused = Rc::clone(&self.desired_paused);
        let context = self.context.clone();
        spawn_local(async move {
            let _ = JsFuture::from(promise).await;
            reconcile_context_state(&context, &desired_paused);
        });
    }

    /// Suspends the `AudioContext` and reconciles if rapid pause/resume toggles
    /// overlap while promises are settling.
    pub(crate) fn suspend(&self) {
        self.desired_paused.set(true);
        if self.context.state() == AudioContextState::Closed {
            return;
        }
        let Ok(promise) = self.context.suspend() else {
            return;
        };
        let desired_paused = Rc::clone(&self.desired_paused);
        let context = self.context.clone();
        spawn_local(async move {
            let _ = JsFuture::from(promise).await;
            reconcile_context_state(&context, &desired_paused);
        });
    }

    /// Starts asynchronous decoding of `bytes` for `(id, revision)` into the
    /// per-engine cache.
    pub(crate) fn decode_clip(&self, id: &str, revision: u64, bytes: &[u8]) {
        self.decoded_clips.borrow_mut().insert(
            id.to_owned(),
            DecodedClipEntry {
                revision,
                state: DecodedClipState::Decoding,
            },
        );

        let array_buffer = js_sys::Uint8Array::from(bytes).buffer();
        let Ok(promise) = self.context.decode_audio_data(&array_buffer) else {
            if let Some(entry) = self.decoded_clips.borrow_mut().get_mut(id)
                && entry.revision == revision
            {
                entry.state = DecodedClipState::Failed;
            }
            return;
        };

        let cache = Rc::clone(&self.decoded_clips);
        let clip_id = id.to_owned();
        spawn_local(async move {
            let result = JsFuture::from(promise).await;
            let mut map = cache.borrow_mut();
            let Some(entry) = map.get_mut(&clip_id) else {
                return;
            };
            if entry.revision != revision {
                return;
            }
            entry.state = match result.and_then(JsCast::dyn_into::<AudioBuffer>) {
                Ok(buffer) => DecodedClipState::Ready(buffer),
                Err(_) => DecodedClipState::Failed,
            };
        });
    }

    pub(crate) fn remove_clip(&self, id: &str) {
        self.decoded_clips.borrow_mut().remove(id);
    }

    #[must_use]
    pub(crate) fn lookup_clip(&self, id: &str, revision: u64) -> DecodedClipLookup {
        let map = self.decoded_clips.borrow();
        let Some(entry) = map.get(id) else {
            return DecodedClipLookup::Missing;
        };
        if entry.revision != revision {
            return DecodedClipLookup::Missing;
        }
        match &entry.state {
            DecodedClipState::Decoding => DecodedClipLookup::Decoding,
            DecodedClipState::Ready(buffer) => DecodedClipLookup::Ready(buffer.clone()),
            DecodedClipState::Failed => DecodedClipLookup::Failed,
        }
    }
}

impl Drop for WebAudioBackend {
    fn drop(&mut self) {
        if self.context.state() != AudioContextState::Closed
            && let Ok(promise) = self.context.close()
        {
            spawn_local(async move {
                let _ = JsFuture::from(promise).await;
            });
        }
    }
}

fn reconcile_context_state(context: &AudioContext, desired_paused: &Cell<bool>) {
    match (desired_paused.get(), context.state()) {
        (true, AudioContextState::Running) => {
            if let Ok(promise) = context.suspend() {
                spawn_local(async move {
                    let _ = JsFuture::from(promise).await;
                });
            }
        }
        (false, AudioContextState::Suspended) => {
            if let Ok(promise) = context.resume() {
                spawn_local(async move {
                    let _ = JsFuture::from(promise).await;
                });
            }
        }
        _ => {}
    }
}

fn js_error_message(err: &JsValue) -> String {
    if let Some(msg) = err.as_string() {
        return msg;
    }
    if let Some(js_err) = err.dyn_ref::<js_sys::Error>() {
        return String::from(js_err.message());
    }
    format!("{err:?}")
}
