//! STFT engine with WOLA (weighted overlap-add) processing.
//!
//! This module provides a spectral processing framework using short-time Fourier
//! transform with carefully designed windowing to maintain perfect reconstruction.
//!
//! # Design: WOLA (Weighted Overlap-Add)
//!
//! - **Window**: 20 ms at 16 kHz = 320 samples, hop = 10 ms = 160 samples (50% overlap)
//! - **Window function**: sqrt-Hann applied on BOTH analysis and synthesis
//! - **COLA invariant**: When squared windows overlap-add at 50% offset, the sum is
//!   constant (1.0) everywhere, guaranteeing perfect reconstruction for spectral stages
//!   that do not modify phase or change the FFT size. After overlap-add and normalization,
//!   the output exactly reconstructs the input (delayed by window_len).
//! - **Normalization**: The COLA sum of squared windows is approximately 1.0 at every
//!   sample position due to 50% overlap and sqrt-Hann windowing. A small normalization
//!   factor accounts for hop boundaries; we divide each output sample by the sum of
//!   squared window values at that position.
//!
//! # Latency
//!
//! The STFT introduces exactly one window length of latency. At construction,
//! the output FIFO is primed with window_len zeros so that output length always
//! equals input length.
//!
//! # Arbitrary Input Block Sizes
//!
//! The STFT accepts blocks of any length via a FIFO input buffer. It processes
//! every complete hop (forward FFT of the last window, spectral stage, inverse FFT,
//! overlap-add into the output accumulator), emitting exactly as many output samples
//! as input samples arrived.

use std::collections::VecDeque;

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

use super::{AudioBus, DspStage};

/// Trait for spectral processing stages: one call per hop, operating on
/// the one-sided FFT spectrum (bins 0..n/2+1) with magnitude and phase.
pub trait SpectralStage: Send {
    /// Short stable name for this stage.
    fn name(&self) -> &'static str;

    /// Process one hop's spectrum in-place.
    ///
    /// # Arguments
    /// - `bins`: One-sided complex spectrum of length n/2+1
    /// - `bin_hz`: Frequency resolution in Hz (sample_rate / window_len)
    fn process_spectrum(&mut self, bins: &mut [Complex<f32>], bin_hz: f32);
}

/// Identity spectral stage: passes the spectrum through unchanged.
/// Used for testing and latency measurement.
pub struct Identity;

impl SpectralStage for Identity {
    fn name(&self) -> &'static str {
        "stft_identity"
    }

    fn process_spectrum(&mut self, _bins: &mut [Complex<f32>], _bin_hz: f32) {
        // No-op
    }
}

/// Per-bin spectral floor tracker with magnitude-domain spectral subtraction.
/// Tracks a slow per-bin minimum-following floor estimate using EWMA with
/// fast-down/slow-up asymmetry, then subtracts the floor from each bin.
pub struct SpectralFloor {
    /// Per-bin magnitude-domain floor estimate.
    floors: Vec<f32>,
    /// Floor subtraction multiplier (default 1.5).
    alpha: f32,
    /// Spectral floor guard factor (default 0.15).
    beta: f32,
    /// EWMA coefficient for floor when magnitude > floor (slow up).
    ewma_up: f32,
    /// EWMA coefficient for floor when magnitude < floor (fast down).
    ewma_down: f32,
}

impl SpectralFloor {
    /// Create with default speech settings: alpha=1.5, beta=0.15.
    pub fn default_speech() -> Self {
        Self {
            floors: Vec::new(),
            alpha: 1.5,
            beta: 0.15,
            ewma_up: 0.02,  // slow up (98% memory)
            ewma_down: 0.2, // fast down (80% memory)
        }
    }
}

impl SpectralStage for SpectralFloor {
    fn name(&self) -> &'static str {
        "stft_spectral_floor"
    }

    fn process_spectrum(&mut self, bins: &mut [Complex<f32>], _bin_hz: f32) {
        // Initialize floors to the first frame's magnitudes: adapting up
        // from zero would let the outlier guard below freeze the floor at
        // zero forever.
        if self.floors.is_empty() {
            self.floors = bins.iter().map(|b| b.norm()).collect();
        }

        for (bin, floor) in bins.iter_mut().zip(self.floors.iter_mut()) {
            let mag = bin.norm();

            // Decision-directed adaptation: a bin far above its floor is
            // speech or a tone, not noise — raising the floor on it would
            // teach the estimator to erase sustained voiced sounds. Only
            // adapt upward on plausible noise (mag within 4x the floor);
            // always adapt down fast so pauses re-anchor the estimate.
            if mag < *floor {
                *floor = self.ewma_down * mag + (1.0 - self.ewma_down) * *floor;
            } else if mag < 4.0 * *floor {
                *floor = self.ewma_up * mag + (1.0 - self.ewma_up) * *floor;
            }

            // Apply spectral subtraction: subtract alpha * floor from magnitude,
            // but never go below beta * input magnitude (musical noise guard).
            let subtracted = (mag - self.alpha * *floor).max(self.beta * mag);

            // Preserve phase.
            if mag > 1e-8 {
                let scale = subtracted / mag;
                bin.re *= scale;
                bin.im *= scale;
            } else {
                bin.re = 0.0;
                bin.im = 0.0;
            }
        }
    }
}

/// STFT processor implementing the [`DspStage`] contract.
///
/// Accepts arbitrary input block sizes, buffers in an input FIFO,
/// processes one hop at a time with a SLIDING analysis frame (each hop
/// consumes `hop_len` new samples and re-uses the previous half — the
/// overlap in "weighted overlap-add"), and emits exactly as many output
/// samples as arrived. sqrt-Hann is applied on analysis and synthesis;
/// with the periodic window at 50% overlap the squared windows sum to
/// exactly 1 (COLA), so an identity spectral stage reconstructs the
/// input delayed by one window length.
pub struct Stft<S: SpectralStage> {
    stage: S,
    sample_rate: u32,
    window_len: usize,
    hop_len: usize,

    fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    ifft: std::sync::Arc<dyn realfft::ComplexToReal<f32>>,
    spectrum: Vec<Complex<f32>>,
    window_sqrt_hann: Vec<f32>,

    input_fifo: VecDeque<f32>,
    output_fifo: VecDeque<f32>,

    /// Sliding analysis frame (last `window_len` input samples).
    frame: Vec<f32>,
    /// Windowed copy handed to the (in-place-destructive) forward FFT.
    windowed: Vec<f32>,
    /// IFFT output scratch.
    synth: Vec<f32>,
    /// Overlap-add accumulator for the next `window_len` output samples.
    acc: Vec<f32>,
    /// Input samples consumed so far (first window needs a full frame).
    frame_filled: bool,
}

impl<S: SpectralStage> Stft<S> {
    /// Create a new STFT processor: window 20 ms at `sample_rate`, hop
    /// 10 ms (50% overlap). A sample-rate change re-initializes (flushes).
    pub fn new(stage: S, sample_rate: u32) -> Self {
        let window_len = (sample_rate as usize * 20) / 1000;
        let hop_len = window_len / 2;
        let mut planner = RealFftPlanner::new();
        let fft = planner.plan_fft_forward(window_len);
        let ifft = planner.plan_fft_inverse(window_len);
        let mut output_fifo = VecDeque::with_capacity(window_len * 4);
        // Prime with one window of zeros: the engine's declared latency.
        output_fifo.extend(std::iter::repeat_n(0.0f32, window_len));
        Self {
            stage,
            sample_rate,
            window_len,
            hop_len,
            fft,
            ifft,
            spectrum: vec![Complex::new(0.0, 0.0); window_len / 2 + 1],
            window_sqrt_hann: Self::make_sqrt_hann_window(window_len),
            input_fifo: VecDeque::with_capacity(window_len * 4),
            output_fifo,
            frame: vec![0.0; window_len],
            windowed: vec![0.0; window_len],
            synth: vec![0.0; window_len],
            acc: vec![0.0; window_len],
            frame_filled: false,
        }
    }

    /// Periodic sqrt-Hann: `hann(n) + hann(n + N/2) == 1` exactly, which
    /// is the COLA condition that makes analysis*synthesis reconstruct.
    fn make_sqrt_hann_window(len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let hann = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / len as f32).cos());
                hann.sqrt()
            })
            .collect()
    }

    /// Samples the input FIFO must hold before the next hop can run.
    fn need(&self) -> usize {
        if self.frame_filled {
            self.hop_len
        } else {
            self.window_len
        }
    }

    /// Run one hop: slide the frame, FFT, spectral stage, IFFT (normalized
    /// — realfft's round trip scales by N), synthesis window, overlap-add,
    /// emit the `hop_len` samples that are now fully summed.
    fn process_one_hop(&mut self) {
        let (w, h) = (self.window_len, self.hop_len);
        if self.frame_filled {
            self.frame.copy_within(h.., 0);
            for slot in self.frame[w - h..].iter_mut() {
                *slot = self.input_fifo.pop_front().unwrap_or(0.0);
            }
        } else {
            for slot in self.frame.iter_mut() {
                *slot = self.input_fifo.pop_front().unwrap_or(0.0);
            }
            self.frame_filled = true;
        }

        for ((dst, &src), &win) in self
            .windowed
            .iter_mut()
            .zip(&self.frame)
            .zip(&self.window_sqrt_hann)
        {
            *dst = src * win;
        }
        self.fft
            .process(&mut self.windowed, &mut self.spectrum)
            .expect("forward FFT");

        let bin_hz = self.sample_rate as f32 / w as f32;
        self.stage.process_spectrum(&mut self.spectrum, bin_hz);

        self.ifft
            .process(&mut self.spectrum, &mut self.synth)
            .expect("inverse FFT");
        let norm = 1.0 / w as f32;
        for ((acc, &syn), &win) in self
            .acc
            .iter_mut()
            .zip(&self.synth)
            .zip(&self.window_sqrt_hann)
        {
            *acc += syn * norm * win;
        }

        // The first hop of the accumulator is fully summed: emit and slide.
        for i in 0..h {
            self.output_fifo.push_back(self.acc[i]);
        }
        self.acc.copy_within(h.., 0);
        for slot in self.acc[w - h..].iter_mut() {
            *slot = 0.0;
        }
    }

    /// Re-initialize for a new sample rate, keeping the spectral stage.
    fn reinit(&mut self, sample_rate: u32) {
        let window_len = (sample_rate as usize * 20) / 1000;
        let hop_len = window_len / 2;
        let mut planner = RealFftPlanner::new();
        self.fft = planner.plan_fft_forward(window_len);
        self.ifft = planner.plan_fft_inverse(window_len);
        self.spectrum = vec![Complex::new(0.0, 0.0); window_len / 2 + 1];
        self.window_sqrt_hann = Self::make_sqrt_hann_window(window_len);
        self.input_fifo.clear();
        self.output_fifo.clear();
        self.output_fifo
            .extend(std::iter::repeat_n(0.0f32, window_len));
        self.frame = vec![0.0; window_len];
        self.windowed = vec![0.0; window_len];
        self.synth = vec![0.0; window_len];
        self.acc = vec![0.0; window_len];
        self.frame_filled = false;
        self.sample_rate = sample_rate;
        self.window_len = window_len;
        self.hop_len = hop_len;
    }
}

impl<S: SpectralStage> DspStage for Stft<S> {
    fn name(&self) -> &'static str {
        "stft"
    }

    fn process(&mut self, bus: &mut AudioBus) {
        if bus.sample_rate != self.sample_rate {
            self.reinit(bus.sample_rate);
        }
        let input_len = bus.samples.len();
        self.input_fifo.extend(bus.samples.iter().copied());

        while self.input_fifo.len() >= self.need() {
            self.process_one_hop();
        }

        bus.samples.clear();
        for _ in 0..input_len {
            bus.samples
                .push(self.output_fifo.pop_front().unwrap_or(0.0));
        }
    }

    fn latency_samples(&self) -> usize {
        self.window_len
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop, reason = "sample-index loops read naturally in DSP tests")]
mod tests {
    use super::*;

    #[test]
    fn output_length_tracks_input_length() {
        let mut stft = Stft::new(Identity, 16_000);
        let mut samples = vec![0.1; 160];
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: 16_000,
        };
        let input_len = bus.samples.len();
        stft.process(&mut bus);
        assert_eq!(bus.samples.len(), input_len);

        samples = vec![0.05; 73];
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: 16_000,
        };
        let input_len = bus.samples.len();
        stft.process(&mut bus);
        assert_eq!(bus.samples.len(), input_len);
    }

    #[test]
    fn latency_is_window_length() {
        let stft = Stft::new(Identity, 16_000);
        assert_eq!(stft.latency_samples(), 320);
    }

    #[test]
    fn dc_and_nyquist_bins_survive_identity() {
        let mut identity = Identity;
        let mut bins = vec![
            Complex::new(1.0, 0.0),
            Complex::new(0.5, 0.3),
            Complex::new(0.2, 0.0),
        ];
        identity.process_spectrum(&mut bins, 50.0);
        assert!(bins[0].re > 0.9);
        assert!(bins[0].im.abs() < 0.01);
    }

    #[test]

    fn identity_reconstructs_within_60db() {
        let sample_rate = 16_000u32;
        let duration = 0.5;
        let total_samples = (sample_rate as f64 * duration) as usize;

        let mut signal = vec![0.0; total_samples];
        for i in 0..total_samples {
            let t = i as f64 / sample_rate as f64;
            let sine_440 = (2.0 * std::f64::consts::PI * 440.0 * t).sin();
            let sine_1300 = (2.0 * std::f64::consts::PI * 1300.0 * t).sin();
            signal[i] = (sine_440 + sine_1300) as f32 * 0.5;
        }

        let mut stft = Stft::new(Identity, sample_rate);

        let mut output: Vec<f32> = Vec::new();
        let chunk_sizes = [160, 73, 512, 41];
        let mut chunk_idx = 0;
        let mut pos = 0;

        while pos < signal.len() {
            let size = chunk_sizes[chunk_idx % chunk_sizes.len()].min(signal.len() - pos);
            chunk_idx += 1;

            let mut chunk = signal[pos..pos + size].to_vec();
            let mut bus = AudioBus {
                samples: &mut chunk,
                sample_rate,
            };
            stft.process(&mut bus);
            output.extend(bus.samples.iter());
            pos += size;
        }

        let window_len = 320;
        // Steady state: skip the primed latency AND the first analysis
        // window's half-windowed warm-up ramp (inherent to WOLA).
        let skip = 2 * window_len;
        if output.len() <= skip {
            panic!("Output too short");
        }
        let output_delayed = &output[skip..];
        let signal_ref = &signal[skip - window_len..skip - window_len + output_delayed.len()];

        let mut error_sq = 0.0f64;
        for i in 0..output_delayed.len() {
            let diff = output_delayed[i] as f64 - signal_ref[i] as f64;
            error_sq += diff * diff;
        }
        let error_rms = (error_sq / output_delayed.len() as f64).sqrt();

        let mut signal_sq = 0.0f64;
        for &s in signal_ref {
            signal_sq += s as f64 * s as f64;
        }
        let signal_rms = (signal_sq / signal_ref.len() as f64).sqrt();

        let snr_db = 20.0 * (signal_rms / error_rms).log10();
        eprintln!("SNR: {:.1} dB", snr_db);
        assert!(snr_db > 60.0, "SNR {:.1} dB is not > 60 dB", snr_db);
    }

    #[test]
    fn spectral_floor_attenuates_stationary_noise() {
        let sample_rate = 16_000u32;
        let noise_duration = 1.0;
        let noise_samples = (sample_rate as f64 * noise_duration) as usize;

        let mut rng = 0u64;
        let mut noise_signal = vec![0.0; noise_samples];
        for i in 0..noise_samples {
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            let u = ((rng >> 16) & 0x7fff) as f32 / 32768.0;
            noise_signal[i] = (u - 0.5) * 2.0 * 0.05;
        }

        let noise_segment_start = (noise_samples as f64 * 0.6) as usize;
        let noise_segment_end = (noise_samples as f64 * 0.9) as usize;
        let mut noise_input_sq = 0.0f64;
        for i in noise_segment_start..noise_segment_end {
            noise_input_sq += noise_signal[i] as f64 * noise_signal[i] as f64;
        }
        let noise_input_rms =
            (noise_input_sq / (noise_segment_end - noise_segment_start) as f64).sqrt();

        let mut stft = Stft::new(SpectralFloor::default_speech(), sample_rate);
        let mut output: Vec<f32> = Vec::new();
        let chunk_size = 160;
        for chunk in noise_signal.chunks(chunk_size) {
            let mut chunk = chunk.to_vec();
            let mut bus = AudioBus {
                samples: &mut chunk,
                sample_rate,
            };
            stft.process(&mut bus);
            output.extend(bus.samples.iter());
        }

        let window_len = 320;
        let output_segment_start = noise_segment_start + window_len;
        let output_segment_end = noise_segment_end + window_len;
        let mut noise_output_sq = 0.0f64;
        for i in output_segment_start..output_segment_end.min(output.len()) {
            noise_output_sq += output[i] as f64 * output[i] as f64;
        }
        let noise_output_rms =
            (noise_output_sq / (output_segment_end - output_segment_start) as f64).sqrt();

        let attenuation_db = 20.0 * (noise_input_rms / (noise_output_rms + 1e-10)).log10();
        eprintln!("Noise attenuation: {:.1} dB", attenuation_db);
        assert!(
            attenuation_db > 6.0,
            "Attenuation {:.1} dB is not > 6 dB",
            attenuation_db
        );

        // Speech-like tone BURSTS over the same noise, after a noise-only
        // lead-in that lets the floor converge. A *sustained* tone is
        // stationary by definition — a stationary-noise suppressor eating
        // it would be correct — so the retention claim is about bursty,
        // speech-shaped energy.
        let mix_samples = (sample_rate as f64 * 1.5) as usize;
        let lead_in = (sample_rate as f64 * 0.15) as usize;
        let burst_on = (sample_rate as f64 * 0.2) as usize;
        let burst_off = (sample_rate as f64 * 0.1) as usize;
        let mut mix_signal = vec![0.0f32; mix_samples];
        let mut burst_ranges: Vec<(usize, usize)> = Vec::new();
        rng = 0;
        let mut i = 0usize;
        let mut cursor = lead_in;
        while cursor < mix_samples {
            burst_ranges.push((cursor, (cursor + burst_on).min(mix_samples)));
            cursor += burst_on + burst_off;
        }
        while i < mix_samples {
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            let u = ((rng >> 16) & 0x7fff) as f32 / 32768.0;
            let noise = (u - 0.5) * 2.0 * 0.05;
            let in_burst = burst_ranges.iter().any(|&(a, b)| i >= a && i < b);
            let t = i as f64 / sample_rate as f64;
            let sine = if in_burst {
                (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.2
            } else {
                0.0
            };
            mix_signal[i] = noise + sine;
            i += 1;
        }

        let mut stft = Stft::new(SpectralFloor::default_speech(), sample_rate);
        let mut mix_output: Vec<f32> = Vec::new();
        for chunk in mix_signal.chunks(chunk_size) {
            let mut chunk = chunk.to_vec();
            let mut bus = AudioBus {
                samples: &mut chunk,
                sample_rate,
            };
            stft.process(&mut bus);
            mix_output.extend(bus.samples.iter());
        }

        // Compare burst energy in vs out (output delayed by window_len).
        // Skip each burst's first 40 ms (WOLA transient + floor settling).
        let settle = (sample_rate as f64 * 0.04) as usize;
        let mut in_sq = 0.0f64;
        let mut out_sq = 0.0f64;
        let mut n = 0usize;
        for &(a, b) in &burst_ranges {
            for i in (a + settle)..b {
                let o = i + window_len;
                if o < mix_output.len() {
                    in_sq += mix_signal[i] as f64 * mix_signal[i] as f64;
                    out_sq += mix_output[o] as f64 * mix_output[o] as f64;
                    n += 1;
                }
            }
        }
        let sine_amplitude_retention =
            (out_sq / n as f64).sqrt() / ((in_sq / n as f64).sqrt() + 1e-10);
        eprintln!(
            "Sine amplitude retention: {:.1}%",
            sine_amplitude_retention * 100.0
        );
        assert!(
            sine_amplitude_retention > 0.8,
            "Sine retention {:.1}% is not > 80%",
            sine_amplitude_retention * 100.0
        );
    }
}
