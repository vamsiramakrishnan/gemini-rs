//! A speech enhancer in the box *(feature `denoise`)*.
//!
//! [`Denoiser`] is an RNNoise-based noise suppressor
//! ([`nnnoiseless`](https://crates.io/crates/nnnoiseless), pure Rust) packaged
//! as a [`MicProcessor`](super::MicProcessor) — drop it into
//! [`pump_processed`](super::pump_processed) and the session's VAD sees
//! denoised audio.
//!
//! Why it earns its place: the client-side energy VAD has two noise
//! pathologies, measured on TTS speech over synthesized noise. Continuous
//! broadband noise (white, line hiss) triggers one false activation at call
//! start and then latches the detector open for the rest of the call; pink
//! (ambience-shaped) noise instead raises the adaptive floor until real
//! speech at ≤10 dB SNR is *missed*. With this stage ahead of the VAD, both
//! disappear in the same benchmark: zero false activations, zero stuck-open
//! time, and every utterance detected down to 0 dB SNR — at ~0.007× realtime
//! on one CPU core.
//!
//! Two honest limits, from the same measurements:
//!
//! - **Competing speech passes through.** A speech *enhancer* preserves
//!   speech — babble noise and a second talker in the room still reach the
//!   VAD. Level discrimination ([`NoiseGate`](super::NoiseGate) calibrated
//!   between the near and far talkers' levels) is the mono-mic tool for
//!   "the person closer to the microphone"; chain it *after* the denoiser
//!   so it gates on clean levels.
//! - **It buffers 10 ms.** RNNoise operates on 480-sample frames at 48 kHz;
//!   input is resampled, processed per block, and resampled back, so output
//!   trails input by up to one block (plus the first block, emitted as
//!   silence while the network warms up).
//!
//! Heavier alternative: DeepFilterNet (also Rust, tract CPU inference at
//! ~0.12× realtime) scores the same on these noise benchmarks and better on
//! very low-SNR speech quality, but its inference crate is only published
//! as a git dependency, which a crates.io release cannot carry — so it
//! stays an application-side `impl MicProcessor` (the eval harness in the
//! repository history has a working one) rather than an SDK feature.

use super::MicProcessor;

const DENOISE_HZ: u32 = 48_000;
const FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE; // 480 = 10 ms

/// RNNoise noise suppression as a mic-chain stage. One instance per stream —
/// the network is stateful across frames.
pub struct Denoiser {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    mic_hz: u32,
    /// 48 kHz samples waiting to fill a 480-sample block.
    pending: Vec<f32>,
    /// The first processed block is warm-up noise; it is replaced by
    /// silence so the session never hears it.
    warmed_up: bool,
}

impl Denoiser {
    /// A denoiser for a microphone stream at `mic_hz` (the same rate given
    /// to [`pump_processed`](super::pump_processed)).
    pub fn new(mic_hz: u32) -> Self {
        Self {
            state: nnnoiseless::DenoiseState::new(),
            mic_hz,
            pending: Vec::with_capacity(FRAME * 2),
            warmed_up: false,
        }
    }
}

impl MicProcessor for Denoiser {
    fn process(&mut self, frame: &mut Vec<i16>) {
        if frame.is_empty() {
            return;
        }
        // Up to the model's rate; RNNoise expects f32 samples in i16 range.
        self.pending.extend(
            super::resample(frame, self.mic_hz, DENOISE_HZ)
                .iter()
                .map(|&s| s as f32),
        );

        let mut out48: Vec<i16> = Vec::with_capacity(self.pending.len());
        let mut input = [0.0f32; FRAME];
        let mut output = [0.0f32; FRAME];
        while self.pending.len() >= FRAME {
            input.copy_from_slice(&self.pending[..FRAME]);
            self.pending.drain(..FRAME);
            self.state.process_frame(&mut output, &input);
            if self.warmed_up {
                out48.extend(output.iter().map(|&s| s.clamp(-32768.0, 32767.0) as i16));
            } else {
                out48.extend(std::iter::repeat_n(0i16, FRAME));
                self.warmed_up = true;
            }
        }

        // Back to the mic rate. The frame the pump forwards may be shorter
        // or longer than the one it handed us — it is a stream, not a
        // sample-aligned transform.
        *frame = super::resample(&out48, DENOISE_HZ, self.mic_hz);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic full-scale-ish white noise at the mic rate.
    fn noise_frame(len: usize, seed: &mut u64) -> Vec<i16> {
        (0..len)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                (seed.wrapping_mul(0x2545F4914F6CDD1D) >> 50) as i16
            })
            .collect()
    }

    fn rms(samples: &[i16]) -> f64 {
        (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64)
            .sqrt()
    }

    #[test]
    fn strongly_attenuates_stationary_noise() {
        let mut denoiser = Denoiser::new(16_000);
        let mut seed = 0xDECAF | 1;
        let mut in_rms = 0.0;
        let mut out_rms = 0.0;
        // 20 ms frames; skip the first 10 (warm-up + floor estimation).
        for i in 0..50 {
            let mut frame = noise_frame(320, &mut seed);
            let level_in = rms(&frame);
            denoiser.process(&mut frame);
            if i >= 10 && !frame.is_empty() {
                in_rms += level_in;
                out_rms += rms(&frame);
            }
        }
        // Sustained speech-free noise is attenuated (the full effect in a
        // real stream is larger — RNNoise's own VAD gates harder between
        // utterances; see the benchmark in the module docs). This guards
        // "suppresses, doesn't corrupt", not the exact figure.
        assert!(
            out_rms < in_rms * 0.6,
            "expected ≥4 dB suppression of white noise, got in={in_rms:.0} out={out_rms:.0}"
        );
    }

    #[test]
    fn preserves_stream_duration_within_one_block() {
        let mut denoiser = Denoiser::new(8_000);
        let mut seed = 3u64;
        let mut fed = 0usize;
        let mut got = 0usize;
        for _ in 0..40 {
            let mut frame = noise_frame(160, &mut seed); // 20 ms @ 8 kHz
            fed += frame.len();
            denoiser.process(&mut frame);
            got += frame.len();
        }
        // Output may trail input by up to one 10 ms block (80 samples @ 8 kHz).
        assert!(fed - got <= 80, "fed {fed}, got {got}");
    }

    #[test]
    fn empty_frames_flow_through() {
        let mut denoiser = Denoiser::new(16_000);
        let mut frame = Vec::new();
        denoiser.process(&mut frame);
        assert!(frame.is_empty());
    }
}
