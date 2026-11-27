//! Safe live-panned stereo `rodio::Source` adapter.

#![cfg(all(feature = "playback", not(target_arch = "wasm32")))]

use core::num::NonZero;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;
use rodio::{ChannelCount, Sample, SampleRate, Source};
use std::sync::Arc;

/// Lock-free atomic left/right gain handle shared between [`crate::AudioEngine`]
/// and the audio mixer thread.
#[derive(Clone, Debug)]
pub(crate) struct StereoGains {
    left: Arc<AtomicU32>,
    right: Arc<AtomicU32>,
}

impl StereoGains {
    /// Creates a new gain handle initialized with `left` and `right` linear gains.
    #[must_use]
    pub fn new(left: f32, right: f32) -> Self {
        Self {
            left: Arc::new(AtomicU32::new(left.to_bits())),
            right: Arc::new(AtomicU32::new(right.to_bits())),
        }
    }

    /// Updates left and right linear gains atomically.
    pub fn set(&self, left: f32, right: f32) {
        self.left.store(left.to_bits(), Ordering::Relaxed);
        self.right.store(right.to_bits(), Ordering::Relaxed);
    }

    /// Reads the current left and right linear gains.
    #[must_use]
    pub fn get(&self) -> (f32, f32) {
        (
            f32::from_bits(self.left.load(Ordering::Relaxed)),
            f32::from_bits(self.right.load(Ordering::Relaxed)),
        )
    }
}

/// Safe stereo wrapper over any [`rodio::Source`] that converts mono or
/// multi-channel sources into stereo and applies live atomic left/right gains.
pub struct StereoPannedSource<S> {
    input: S,
    gains: StereoGains,
    pending_right: Option<Sample>,
}

impl<S> StereoPannedSource<S> {
    /// Wraps `input` with live stereo gains controlled by `gains`.
    #[must_use]
    pub(crate) fn new(input: S, gains: StereoGains) -> Self {
        Self {
            input,
            gains,
            pending_right: None,
        }
    }
}

impl<S: Source> Iterator for StereoPannedSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(right) = self.pending_right.take() {
            return Some(right);
        }

        let in_channels = usize::from(self.input.channels().get());
        let (left_gain, right_gain) = self.gains.get();

        match in_channels {
            1 => {
                let sample = self.input.next()?;
                self.pending_right = Some(sample * right_gain);
                Some(sample * left_gain)
            }
            2 => {
                let left = self.input.next()?;
                let right = self.input.next().unwrap_or(left);
                self.pending_right = Some(right * right_gain);
                Some(left * left_gain)
            }
            n => {
                let left = self.input.next()?;
                let right = self.input.next().unwrap_or(left);
                for _ in 2..n {
                    let _ = self.input.next();
                }
                self.pending_right = Some(right * right_gain);
                Some(left * left_gain)
            }
        }
    }
}

impl<S: Source> Source for StereoPannedSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        let in_len = self.input.current_span_len()?;
        let in_channels = usize::from(self.input.channels().get());
        let frames = in_len / in_channels;
        Some(frames * 2 + usize::from(self.pending_right.is_some()))
    }

    fn channels(&self) -> ChannelCount {
        // Stereo output (always 2 interleaved channels).
        const STEREO: ChannelCount = NonZero::new(2).unwrap();
        STEREO
    }

    fn sample_rate(&self) -> SampleRate {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }
}
