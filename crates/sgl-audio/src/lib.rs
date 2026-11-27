//! Deterministic 3D spatialization and safe audio playback for SGL games.
//!
//! - [`Listener3d`], [`SpatialMix`], and [`spatialize`] provide deterministic
//!   distance attenuation and constant-power stereo panning on every target
//!   (including `wasm32` and headless builds with `--no-default-features`).
//! - [`AudioEngine`] manages cached audio clips, tracked looping/one-shot
//!   emitters, and live stereo-panned playback using lock-free atomic gain
//!   updates with zero `unsafe` code.

mod engine;
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
mod source;
mod spatial;

pub use engine::{AudioEngine, AudioError, EmitterState};
#[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
pub use source::StereoPannedSource;
pub use spatial::{Listener3d, SpatialMix, spatialize};

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::FRAC_1_SQRT_2;
    use glam::Vec3;
    use std::collections::HashSet;

    fn assert_close(a: f32, b: f32, eps: f32) {
        assert!(
            (a - b).abs() <= eps,
            "expected {a} to be within {eps} of {b} (diff = {})",
            (a - b).abs()
        );
    }

    #[test]
    fn attenuation_is_unity_at_zero_monotone_and_zero_at_range() {
        let listener = Listener3d::default();
        let range = 10.0;

        let at_origin = spatialize(&listener, Vec3::ZERO, range, 1.0);
        assert_close(at_origin.gain, 1.0, 1e-6);
        assert_close(at_origin.pan, 0.0, 1e-6);
        assert_close(at_origin.left_gain, FRAC_1_SQRT_2, 1e-6);
        assert_close(at_origin.right_gain, FRAC_1_SQRT_2, 1e-6);

        let mut prev_gain = at_origin.gain;
        for step in 1_u16..=10 {
            let d = f32::from(step);
            let mix = spatialize(&listener, Vec3::new(0.0, 0.0, -d), range, 1.0);
            assert!(
                mix.gain < prev_gain || (step == 10 && mix.gain == 0.0),
                "gain should strictly decrease until range: d={d}, prev={prev_gain}, cur={}",
                mix.gain
            );
            prev_gain = mix.gain;
        }

        let at_range = spatialize(&listener, Vec3::new(0.0, 0.0, -range), range, 1.0);
        assert_eq!(at_range, SpatialMix::SILENT);

        let beyond_range = spatialize(&listener, Vec3::new(0.0, 0.0, -range * 2.5), range, 1.0);
        assert_eq!(beyond_range, SpatialMix::SILENT);
    }

    #[test]
    fn non_positive_range_acts_as_centered_2d() {
        let listener = Listener3d::default();
        let mix = spatialize(&listener, Vec3::new(100.0, 50.0, -20.0), 0.0, 0.8);
        assert_close(mix.gain, 0.8, 1e-6);
        assert_close(mix.pan, 0.0, 1e-6);
        assert_close(mix.left_gain, 0.8 * FRAC_1_SQRT_2, 1e-6);
        assert_close(mix.right_gain, 0.8 * FRAC_1_SQRT_2, 1e-6);

        let neg_range = spatialize(&listener, Vec3::new(-50.0, 0.0, 0.0), -5.0, 0.5);
        assert_close(neg_range.gain, 0.5, 1e-6);
        assert_close(neg_range.pan, 0.0, 1e-6);
    }

    #[test]
    fn stereo_panning_left_right_center_behind_and_constant_power() {
        let listener = Listener3d::default();
        let range = 20.0;

        // Hard right (+X)
        let right = spatialize(&listener, Vec3::new(4.0, 0.0, 0.0), range, 1.0);
        assert_close(right.pan, 1.0, 1e-6);
        assert_close(right.left_gain, 0.0, 1e-6);
        assert_close(right.right_gain, right.gain, 1e-6);

        // Hard left (-X)
        let left = spatialize(&listener, Vec3::new(-4.0, 0.0, 0.0), range, 1.0);
        assert_close(left.pan, -1.0, 1e-6);
        assert_close(left.left_gain, left.gain, 1e-6);
        assert_close(left.right_gain, 0.0, 1e-6);

        // Directly ahead (-Z) and directly behind (+Z) both center pan
        let front = spatialize(&listener, Vec3::new(0.0, 0.0, -4.0), range, 1.0);
        let behind = spatialize(&listener, Vec3::new(0.0, 0.0, 4.0), range, 1.0);
        assert_close(front.pan, 0.0, 1e-6);
        assert_close(behind.pan, 0.0, 1e-6);
        assert_close(front.left_gain, front.right_gain, 1e-6);
        assert_close(behind.left_gain, behind.right_gain, 1e-6);

        // Constant-power energy preservation at arbitrary diagonal angle
        let diag = spatialize(&listener, Vec3::new(3.0, 0.0, -4.0), range, 0.9);
        assert!(diag.pan > 0.0 && diag.pan < 1.0);
        let energy = diag.left_gain * diag.left_gain + diag.right_gain * diag.right_gain;
        assert_close(energy, diag.gain * diag.gain, 1e-5);
    }

    #[test]
    fn rotated_listener_flips_pan_sides() {
        // Facing +X with up +Y -> right is +Z.
        let listener = Listener3d {
            position: Vec3::new(1.0, 2.0, 3.0),
            forward: Vec3::X,
            up: Vec3::Y,
        };
        assert_close(listener.right().distance(Vec3::Z), 0.0, 1e-6);

        let to_right = spatialize(&listener, Vec3::new(1.0, 2.0, 8.0), 20.0, 1.0);
        assert_close(to_right.pan, 1.0, 1e-6);
        assert_close(to_right.left_gain, 0.0, 1e-6);
        assert!(to_right.right_gain > 0.0);

        let to_left = spatialize(&listener, Vec3::new(1.0, 2.0, -2.0), 20.0, 1.0);
        assert_close(to_left.pan, -1.0, 1e-6);
        assert!(to_left.left_gain > 0.0);
        assert_close(to_left.right_gain, 0.0, 1e-6);
    }

    #[test]
    fn headless_engine_clips_emitters_controls_and_retain() {
        let mut engine = AudioEngine::headless();
        assert!(engine.is_headless());
        assert!(!engine.has_clip("torch"));

        engine.load_clip("torch", vec![1, 2, 3]);
        assert!(engine.has_clip("torch"));
        assert!(engine.play_oneshot("torch", 0.75, Some((Vec3::new(2.0, 0.0, 0.0), 10.0))));
        assert!(!engine.play_oneshot("missing", 1.0, None));

        // Hot-reload clip bytes
        engine.load_clip("torch", vec![4, 5, 6, 7]);
        assert!(engine.has_clip("torch"));

        engine.sync_emitter(
            1,
            EmitterState {
                clip: "torch".to_owned(),
                position: Some(Vec3::new(5.0, 0.0, 0.0)),
                range: 10.0,
                volume: 1.0,
                playing: true,
                looping: true,
            },
        );
        engine.sync_emitter(
            2,
            EmitterState {
                clip: "torch".to_owned(),
                position: None,
                range: 0.0,
                volume: 0.5,
                playing: true,
                looping: false,
            },
        );

        let mix1 = engine.emitter_mix(1).expect("emitter 1 tracked");
        assert_close(mix1.gain, 0.25, 1e-6);
        assert_close(mix1.pan, 1.0, 1e-6);

        let mix2 = engine.emitter_mix(2).expect("emitter 2 tracked");
        assert_close(mix2.gain, 0.5, 1e-6);
        assert_close(mix2.pan, 0.0, 1e-6);

        // Move listener to emitter 1's position and update
        engine.set_listener(Listener3d {
            position: Vec3::new(5.0, 0.0, 0.0),
            ..Listener3d::default()
        });
        assert_eq!(engine.listener().position, Vec3::new(5.0, 0.0, 0.0));
        engine.update();
        let mix1_at_source = engine.emitter_mix(1).unwrap();
        assert_close(mix1_at_source.gain, 1.0, 1e-6);
        assert_close(mix1_at_source.pan, 0.0, 1e-6);

        // Master volume scaling
        engine.set_master_volume(0.5);
        engine.update();
        assert_close(engine.emitter_mix(1).unwrap().gain, 0.5, 1e-6);

        // Mute silences output
        engine.set_muted(true);
        engine.update();
        assert_eq!(engine.emitter_mix(1).unwrap(), SpatialMix::SILENT);
        engine.set_muted(false);
        engine.update();
        assert_close(engine.emitter_mix(1).unwrap().gain, 0.5, 1e-6);

        // Pause silences computed mix and restores on resume
        engine.set_paused(true);
        engine.update();
        assert_eq!(engine.emitter_mix(1).unwrap(), SpatialMix::SILENT);
        engine.set_paused(false);
        engine.update();
        assert_close(engine.emitter_mix(1).unwrap().gain, 0.5, 1e-6);

        // Stopping an emitter silences it
        engine.sync_emitter(
            1,
            EmitterState {
                clip: "torch".to_owned(),
                position: Some(Vec3::new(5.0, 0.0, 0.0)),
                range: 10.0,
                volume: 1.0,
                playing: false,
                looping: true,
            },
        );
        assert_eq!(engine.emitter_mix(1).unwrap(), SpatialMix::SILENT);

        // Retain only emitter 2, then remove it
        let keep = HashSet::from([2_u64]);
        engine.retain_emitters(&keep);
        assert!(engine.emitter_mix(1).is_none());
        assert!(engine.emitter_mix(2).is_some());

        engine.remove_emitter(2);
        assert!(engine.emitter_mix(2).is_none());

        engine.remove_clip("torch");
        assert!(!engine.has_clip("torch"));
    }

    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    fn synthesize_mono_pcm_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_bytes = u32::try_from(samples.len() * 2).expect("small test wav");
        let riff_size = 36 + data_bytes;
        let byte_rate = sample_rate * 2;
        let mut wav = Vec::with_capacity((44 + data_bytes) as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&riff_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes()); // PCM chunk size
        wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM format
        wav.extend_from_slice(&1_u16.to_le_bytes()); // Mono
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes()); // Block align
        wav.extend_from_slice(&16_u16.to_le_bytes()); // Bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for &sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    #[cfg(all(feature = "playback", not(target_arch = "wasm32")))]
    #[test]
    fn wav_decode_and_live_stereo_panning_smoke() {
        use crate::source::StereoGains;
        use rodio::{Decoder, Source};
        use std::io::Cursor;

        let pcm: Vec<i16> = vec![16_384; 32];
        let wav = synthesize_mono_pcm_wav(44_100, &pcm);
        let decoder = Decoder::new(Cursor::new(wav)).expect("valid 16-bit PCM WAV decodes");

        let gains = StereoGains::new(1.0, 0.0);
        let mut panned = StereoPannedSource::new(decoder, gains.clone());
        assert_eq!(panned.channels().get(), 2);
        assert_eq!(panned.sample_rate().get(), 44_100);

        // First frame: hard left
        let l0 = panned.next().expect("left sample");
        let r0 = panned.next().expect("right sample");
        assert!(l0 > 0.4 && l0 < 0.6, "expected ~0.5 left, got {l0}");
        assert_close(r0, 0.0, 1e-6);

        // Live gain update without recreating stream: hard right
        gains.set(0.0, 0.5);
        let l1 = panned.next().expect("left sample");
        let r1 = panned.next().expect("right sample");
        assert_close(l1, 0.0, 1e-6);
        assert!(r1 > 0.2 && r1 < 0.3, "expected ~0.25 right, got {r1}");

        // Remaining frames drain cleanly
        let remaining: Vec<f32> = panned.collect();
        assert_eq!(remaining.len(), (pcm.len() - 2) * 2);
    }
}
