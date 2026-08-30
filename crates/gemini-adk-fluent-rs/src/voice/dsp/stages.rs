//! Time-domain stage library: high-pass, AGC, and limiter.
//!
//! Three production-grade stages implementing the canonical order:
//! `HPF → AEC → denoise → AGC → gate → limiter`. Each stage assumes what
//! the previous one guarantees.

use std::f32::consts::PI;

use super::{AudioBus, DspStage};

/// RBJ audio-EQ-cookbook biquad high-pass filter in Direct Form II transposed.
///
/// Removes DC offset and sub-speech rumble/handling noise that bias the energy
/// VAD's adaptive noise floor. Operates in the canonical position *before* echo
/// cancellation (which needs the linear signal) and *before* denoise (which is
/// nonlinear and breaks the AEC model).
///
/// Recomputes coefficients if the input `bus.sample_rate` differs from
/// construction, flushing state in the process.
pub struct HighPass {
    cutoff_hz: f32,
    q: f32,
    sample_rate: u32,
    // Direct Form II transposed state.
    z1: f32,
    z2: f32,
    // Cached normalized coefficients.
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl HighPass {
    /// Construct a high-pass filter at the given cutoff and Q.
    ///
    /// # Panics
    ///
    /// Panics if `cutoff_hz` is outside (0, nyquist) or `q <= 0`.
    pub fn new(cutoff_hz: f32, q: f32, sample_rate: u32) -> Self {
        assert!(cutoff_hz > 0.0 && cutoff_hz < sample_rate as f32 / 2.0);
        assert!(q > 0.0);
        let mut f = Self {
            cutoff_hz,
            q,
            sample_rate,
            z1: 0.0,
            z2: 0.0,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
        f.recompute_coefficients();
        f
    }

    /// Preset: speech-optimized high-pass at 100 Hz with Q = 1/√2 (0.707).
    pub fn speech(sample_rate: u32) -> Self {
        Self::new(100.0, std::f32::consts::FRAC_1_SQRT_2, sample_rate)
    }

    fn recompute_coefficients(&mut self) {
        let nyquist = self.sample_rate as f32 / 2.0;
        if self.cutoff_hz >= nyquist {
            // Clamp to just below nyquist to avoid NaN.
            return;
        }

        let omega = 2.0 * PI * self.cutoff_hz / self.sample_rate as f32;
        let sin_omega = omega.sin();
        let cos_omega = omega.cos();
        let alpha = sin_omega / (2.0 * self.q);

        // RBJ high-pass coefficients.
        let b0_raw = (1.0 + cos_omega) / 2.0;
        let b1_raw = -(1.0 + cos_omega);
        let b2_raw = (1.0 + cos_omega) / 2.0;
        let a0_raw = 1.0 + alpha;
        let a1_raw = -2.0 * cos_omega;
        let a2_raw = 1.0 - alpha;

        // Normalize by a0.
        self.b0 = b0_raw / a0_raw;
        self.b1 = b1_raw / a0_raw;
        self.b2 = b2_raw / a0_raw;
        self.a1 = a1_raw / a0_raw;
        self.a2 = a2_raw / a0_raw;

        // Flush state on recomputation.
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

impl DspStage for HighPass {
    fn name(&self) -> &'static str {
        "hpf"
    }

    fn process(&mut self, bus: &mut AudioBus) {
        if bus.sample_rate != self.sample_rate {
            self.sample_rate = bus.sample_rate;
            self.recompute_coefficients();
        }

        let (b0, b1, b2, a1, a2) = (self.b0, self.b1, self.b2, self.a1, self.a2);
        let mut z1 = self.z1;
        let mut z2 = self.z2;

        for s in bus.samples.iter_mut() {
            let x = *s;
            let y = b0 * x + z1;
            z1 = b1 * x - a1 * y + z2;
            z2 = b2 * x - a2 * y;
            *s = y;
        }

        self.z1 = z1;
        self.z2 = z2;
    }

    fn latency_samples(&self) -> usize {
        0
    }
}

/// WebRTC-AGC2-style slow digital gain control.
///
/// Stabilizes the speech level so the VAD's dB thresholds mean the same thing
/// on every microphone. Sits *after* the denoiser so it does not amplify noise.
///
/// The stage calculates per-block RMS, updates a running speech-level estimate
/// (EWMA) when the block is likely speech (via external probability or internal
/// energy gate), and applies a linear gain ramp to move the level toward the
/// target without zipper noise. Gain never exceeds the configured bounds.
pub struct Agc {
    /// Target speech level in dBFS.
    target_rms_dbfs: f32,
    /// Maximum gain in dB.
    max_gain_db: f32,
    /// Minimum gain in dB.
    min_gain_db: f32,
    /// Slew rate in dB/s.
    slew_db_per_sec: f32,
    /// Sample rate in Hz.
    sample_rate: u32,
    /// Current gain in linear (not dB).
    current_gain_linear: f32,
    /// Speech level estimate (EWMA) in dBFS.
    speech_level_dbfs: f32,
    /// EWMA weight for the level (1 second time constant).
    level_alpha: f32,
    /// Noise floor estimate (EWMA) in dBFS.
    noise_floor_dbfs: f32,
    /// Noise floor EWMA weight.
    noise_alpha: f32,
    /// External speech probability (0-1), set via `set_speech_probability`.
    /// If never set, falls back to internal energy gate.
    speech_probability: Option<f32>,
}

impl Agc {
    /// Construct an AGC with explicit parameters.
    pub fn new(
        target_rms_dbfs: f32,
        max_gain_db: f32,
        min_gain_db: f32,
        slew_db_per_sec: f32,
        sample_rate: u32,
    ) -> Self {
        // EWMA weight for 1 second at block rate (assume ~10 ms blocks = 100 Hz).
        let level_alpha = 1.0 / (1.0 + 100.0 * 1.0);
        let noise_alpha = level_alpha * 0.5;

        Self {
            target_rms_dbfs,
            max_gain_db,
            min_gain_db,
            slew_db_per_sec,
            sample_rate,
            current_gain_linear: 1.0,
            speech_level_dbfs: -30.0,
            level_alpha,
            noise_floor_dbfs: -60.0,
            noise_alpha,
            speech_probability: None,
        }
    }

    /// Preset: speech-optimized AGC.
    /// Target = -18 dBFS, max gain = +30 dB, min = -10 dB, slew = 6 dB/s.
    pub fn speech_default(sample_rate: u32) -> Self {
        Self::new(-18.0, 30.0, -10.0, 6.0, sample_rate)
    }

    /// Set external speech probability (0-1) from an external source (e.g. RNNoise VAD).
    /// Blocks with p >= 0.5 are treated as speech and adapt the gain.
    /// If never called, falls back to internal energy gate.
    pub fn set_speech_probability(&mut self, p: f32) {
        self.speech_probability = Some(p.clamp(0.0, 1.0));
    }

    fn dbfs_from_rms(rms: f32) -> f32 {
        if rms <= 0.0 {
            -120.0
        } else {
            20.0 * rms.log10()
        }
    }

    fn linear_from_db(db: f32) -> f32 {
        10.0_f32.powf(db / 20.0)
    }
}

impl DspStage for Agc {
    fn name(&self) -> &'static str {
        "agc"
    }

    fn process(&mut self, bus: &mut AudioBus) {
        if bus.sample_rate != self.sample_rate {
            self.sample_rate = bus.sample_rate;
            // Recalculate EWMA weight for new block rate.
            let block_rate_hz = (self.sample_rate as f32) / 160.0; // ~10 ms blocks
            self.level_alpha = 1.0 / (1.0 + block_rate_hz * 1.0);
            self.noise_alpha = self.level_alpha * 0.5;
        }

        // Calculate block RMS.
        let mut energy = 0.0f64;
        for &s in bus.samples.iter() {
            energy += f64::from(s) * f64::from(s);
        }
        let rms = if bus.samples.is_empty() {
            0.0
        } else {
            (energy / bus.samples.len() as f64).sqrt() as f32
        };

        let rms_dbfs = Self::dbfs_from_rms(rms);

        // Determine if this block is likely speech.
        let is_speech = if let Some(prob) = self.speech_probability {
            prob >= 0.5
        } else {
            // Internal energy gate: is RMS above 3x the noise floor?
            rms_dbfs > self.noise_floor_dbfs + 9.5 // 3x in dB
        };

        // Update noise floor (always).
        self.noise_floor_dbfs =
            self.noise_alpha * rms_dbfs + (1.0 - self.noise_alpha) * self.noise_floor_dbfs;

        // Update speech level estimate only for speech blocks.
        if is_speech {
            self.speech_level_dbfs =
                self.level_alpha * rms_dbfs + (1.0 - self.level_alpha) * self.speech_level_dbfs;
        }

        // Calculate target gain: move current level to target.
        let gain_needed_db = self.target_rms_dbfs - self.speech_level_dbfs;
        let target_gain_db = gain_needed_db.clamp(self.min_gain_db, self.max_gain_db);

        // Slew-rate-limit the gain.
        let max_delta_db =
            self.slew_db_per_sec / (self.sample_rate as f32 / bus.samples.len() as f32);
        let current_gain_db = 20.0 * self.current_gain_linear.log10().max(-120.0);
        let new_gain_db = (current_gain_db + max_delta_db.clamp(-max_delta_db, max_delta_db))
            .min(target_gain_db)
            .max(current_gain_db - max_delta_db);
        let new_gain_linear = Self::linear_from_db(new_gain_db);

        // Apply gain with linear ramp to avoid zipper noise.
        let gain_step = (new_gain_linear - self.current_gain_linear) / bus.samples.len() as f32;
        for (i, s) in bus.samples.iter_mut().enumerate() {
            let gain = self.current_gain_linear + gain_step * (i as f32 + 1.0);
            *s *= gain;
        }

        self.current_gain_linear = new_gain_linear;
    }

    fn latency_samples(&self) -> usize {
        0
    }
}

/// Lookahead brick-wall limiter for clipping prevention.
///
/// Last stage; guarantees the exit conversion never clips. Uses a lookahead
/// delay line to detect peaks before they arrive, enabling instant attack.
/// Release is exponential with the configured time constant.
pub struct Limiter {
    /// Ceiling level (linear, typically 0.98).
    ceiling: f32,
    /// Lookahead buffer length in samples.
    lookahead_samples: usize,
    /// Release time constant in seconds.
    release_time_s: f32,
    /// Sample rate in Hz.
    sample_rate: u32,
    /// Delay line for lookahead.
    delay_line: Vec<f32>,
    /// Write position in delay line.
    delay_pos: usize,
    /// Current gain (0-1).
    current_gain: f32,
}

impl Limiter {
    /// Construct a limiter with explicit parameters.
    ///
    /// # Panics
    ///
    /// Panics if `ceiling <= 0.0` or `sample_rate == 0`.
    pub fn new(ceiling: f32, lookahead_ms: f32, release_ms: f32, sample_rate: u32) -> Self {
        assert!(ceiling > 0.0);
        assert!(sample_rate > 0);
        let lookahead_samples = ((lookahead_ms * sample_rate as f32) / 1000.0).max(1.0) as usize;
        Self {
            ceiling,
            lookahead_samples,
            release_time_s: release_ms / 1000.0,
            sample_rate,
            delay_line: vec![0.0; lookahead_samples],
            delay_pos: 0,
            current_gain: 1.0,
        }
    }

    /// Preset: speech-optimized limiter.
    /// Ceiling = 0.98, lookahead = 5 ms, release = 50 ms.
    pub fn speech_default(sample_rate: u32) -> Self {
        Self::new(0.98, 5.0, 50.0, sample_rate)
    }

    /// Find the maximum absolute value in the lookahead window.
    fn lookahead_peak(&self) -> f32 {
        self.delay_line.iter().map(|&s| s.abs()).fold(0.0, f32::max)
    }

    /// Compute release gain decay per sample.
    fn release_decay_per_sample(&self) -> f32 {
        // e^(-dt / tau), where tau = release_time_s, dt = 1/sample_rate.
        (-1.0 / (self.sample_rate as f32 * self.release_time_s)).exp()
    }
}

impl DspStage for Limiter {
    fn name(&self) -> &'static str {
        "limiter"
    }

    fn process(&mut self, bus: &mut AudioBus) {
        if bus.sample_rate != self.sample_rate {
            self.sample_rate = bus.sample_rate;
            // Rebuild delay line if the rate changed.
            let lookahead_samples = ((5.0 * bus.sample_rate as f32) / 1000.0).max(1.0) as usize;
            if lookahead_samples != self.lookahead_samples {
                self.delay_line.resize(lookahead_samples, 0.0);
                self.delay_pos = 0;
            }
        }

        let release_decay = self.release_decay_per_sample();
        let ceiling = self.ceiling;

        for s in bus.samples.iter_mut() {
            // True lookahead: emit the sample that entered the delay line
            // `lookahead_samples` ago, scaled by a gain that has already
            // seen every sample between it and "now" — the gain reaches
            // full attenuation BEFORE a peak arrives at the output. This
            // is the delay `latency_samples()` declares.
            let delayed = self.delay_line[self.delay_pos];
            self.delay_line[self.delay_pos] = *s;
            self.delay_pos = (self.delay_pos + 1) % self.lookahead_samples;

            // Gain needed to keep the window's peak at the ceiling. The
            // window still contains the emitted sample's own magnitude
            // until this very iteration, so it too is bounded.
            let peak = self.lookahead_peak().max(delayed.abs());
            let target_gain = if peak > ceiling {
                ceiling / peak.max(1e-6)
            } else {
                1.0
            };

            // Attack is instant; release recovers exponentially toward the
            // target with the configured time constant (dividing by the
            // per-sample decay grows the gain by ~1/tau per sample).
            self.current_gain = if target_gain < self.current_gain {
                target_gain
            } else {
                (self.current_gain / release_decay).min(target_gain)
            };

            *s = delayed * self.current_gain;
        }
    }

    fn latency_samples(&self) -> usize {
        self.lookahead_samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (energy / samples.len() as f64).sqrt() as f32
    }

    #[test]
    fn hpf_removes_dc() {
        let mut hpf = HighPass::speech(16_000);
        let mut samples = vec![0.5; 8_000]; // 0.5 s of DC at 16 kHz
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: 16_000,
        };
        hpf.process(&mut bus);

        // Check the last 20% (last 1600 samples).
        let tail = &bus.samples[6_400..];
        let tail_rms = rms(tail);
        assert!(tail_rms < 0.005, "DC residue too high: RMS = {}", tail_rms);
    }

    #[test]
    fn hpf_passes_speech_band() {
        // Test 1 kHz sine (speech band).
        let mut hpf = HighPass::speech(16_000);
        let sample_rate = 16_000;
        let freq = 1000.0;
        let amplitude = 0.5;

        let mut samples: Vec<f32> = (0..16_000)
            .map(|i| amplitude * (2.0 * PI * freq * i as f32 / sample_rate as f32).sin())
            .collect();

        let input_rms = rms(&samples);
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate,
        };
        hpf.process(&mut bus);

        // After settling, RMS should be within 1 dB.
        let output_rms = rms(&bus.samples[2_000..]);
        let ratio_db = 20.0 * (output_rms / input_rms).log10();
        assert!(
            ratio_db > -1.0,
            "1 kHz attenuation too high: {} dB",
            ratio_db
        );

        // Test 50 Hz (attenuated).
        let mut hpf = HighPass::speech(16_000);
        let freq = 50.0;
        let mut samples: Vec<f32> = (0..16_000)
            .map(|i| amplitude * (2.0 * PI * freq * i as f32 / sample_rate as f32).sin())
            .collect();

        let input_rms = rms(&samples);
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate,
        };
        hpf.process(&mut bus);

        let output_rms = rms(&bus.samples[2_000..]);
        let ratio_db = 20.0 * (output_rms / input_rms).log10();
        assert!(ratio_db < -10.0, "50 Hz not attenuated: {} dB", ratio_db);
    }

    #[test]
    fn agc_raises_quiet_speech() {
        let mut agc = Agc::speech_default(16_000);
        let sample_rate = 16_000;
        let freq = 1000.0;
        let amplitude = 0.02; // Quiet

        // Generate 5 seconds of simulated speech (1 kHz).
        let mut samples: Vec<f32> = (0..80_000)
            .map(|i| amplitude * (2.0 * PI * freq * i as f32 / sample_rate as f32).sin())
            .collect();

        let input_rms = rms(&samples);

        // Process in blocks with speech probability.
        for chunk in samples.chunks_mut(160) {
            let mut bus = AudioBus {
                samples: &mut chunk.to_vec(),
                sample_rate,
            };
            agc.set_speech_probability(1.0);
            agc.process(&mut bus);
            chunk.copy_from_slice(bus.samples);
        }

        let output_rms = rms(&samples);
        let gain_applied = output_rms / input_rms;
        assert!(
            gain_applied >= 3.0,
            "AGC did not raise level enough: gain = {}x",
            gain_applied
        );
        assert!(
            agc.current_gain_linear <= 10.0_f32.powf(30.0 / 20.0),
            "AGC exceeded max gain"
        );
    }

    #[test]
    fn agc_holds_gain_in_silence() {
        let mut agc = Agc::speech_default(16_000);
        let sample_rate = 16_000;

        // Converge the gain on some speech.
        let mut samples = vec![0.1; 160];
        for _ in 0..100 {
            let mut bus = AudioBus {
                samples: &mut samples,
                sample_rate,
            };
            agc.set_speech_probability(1.0);
            agc.process(&mut bus);
        }

        let initial_gain_db = 20.0 * agc.current_gain_linear.log10();

        // Switch to silence.
        let mut silence = vec![0.001; 160];
        for _ in 0..10 {
            let mut bus = AudioBus {
                samples: &mut silence,
                sample_rate,
            };
            agc.set_speech_probability(0.0);
            agc.process(&mut bus);
        }

        let final_gain_db = 20.0 * agc.current_gain_linear.log10();
        let delta_db = (final_gain_db - initial_gain_db).abs();
        assert!(delta_db < 1.0, "Gain drifted in silence: {} dB", delta_db);
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut limiter = Limiter::speech_default(16_000);
        let sample_rate = 16_000;
        let freq = 1000.0;
        let ceiling = 0.98;

        // Generate 6 dB overdriven sine (amplitude 1.5).
        let mut samples: Vec<f32> = (0..16_000)
            .map(|i| 1.5 * (2.0 * PI * freq * i as f32 / sample_rate as f32).sin())
            .collect();

        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate,
        };
        limiter.process(&mut bus);

        for &s in bus.samples.iter() {
            assert!(
                s.abs() <= ceiling + 1e-4,
                "Limiter exceeded ceiling: {}",
                s.abs()
            );
        }
    }

    #[test]
    fn limiter_passes_quiet_at_unity() {
        let mut limiter = Limiter::speech_default(16_000);
        let sample_rate = 16_000;
        let freq = 1000.0;

        // Generate quiet sine (amplitude 0.1).
        let mut samples: Vec<f32> = (0..16_000)
            .map(|i| 0.1 * (2.0 * PI * freq * i as f32 / sample_rate as f32).sin())
            .collect();

        let input_rms = rms(&samples);

        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate,
        };
        limiter.process(&mut bus);

        // After lookahead delay, should pass at unity gain within 0.1 dB.
        let lookahead = limiter.latency_samples();
        let output_rms = rms(&bus.samples[lookahead..]);
        let ratio_db = 20.0 * (output_rms / input_rms).log10();
        assert!(
            ratio_db.abs() < 0.1,
            "Quiet passage not at unity: {} dB",
            ratio_db
        );
    }

    #[test]
    fn limiter_reports_lookahead_latency() {
        let limiter = Limiter::speech_default(16_000);
        let expected_samples = ((5.0_f32 * 16_000.0) / 1000.0).max(1.0) as usize;
        assert_eq!(
            limiter.latency_samples(),
            expected_samples,
            "Latency does not match lookahead"
        );
    }

    #[test]
    fn limiter_actually_delays_by_declared_latency() {
        // The declared latency must be REAL: an impulse fed at t=0 comes
        // out at t=lookahead, not at t=0 (a "lookahead" limiter that emits
        // the current sample undelayed cannot attack before the peak).
        let mut limiter = Limiter::speech_default(16_000);
        let lookahead = limiter.latency_samples();
        let mut samples = vec![0.0f32; lookahead * 3];
        samples[0] = 0.5;
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: 16_000,
        };
        limiter.process(&mut bus);
        let peak_at = bus
            .samples
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(
            peak_at, lookahead,
            "impulse must emerge exactly one lookahead later"
        );
    }

    #[test]
    fn limiter_release_is_gradual_not_instant() {
        // After a limiting event ends, the gain must recover on the
        // configured time constant, not snap back within one sample.
        let mut limiter = Limiter::new(0.98, 5.0, 50.0, 16_000);
        // 30 ms of hard overdrive, then quiet signal at 0.5.
        let mut samples: Vec<f32> = Vec::new();
        samples.extend(std::iter::repeat_n(1.96f32, 480)); // needs gain 0.5
        samples.extend(std::iter::repeat_n(0.5f32, 1600));
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: 16_000,
        };
        limiter.process(&mut bus);
        // Shortly after the overdrive leaves the window the quiet samples
        // are still attenuated near 0.5x...
        let early = bus.samples[480 + 160]; // 10 ms into the quiet region
        assert!(
            early < 0.5 * 0.75,
            "gain released almost instantly: quiet sample {early}"
        );
        // ...but several release constants later it is back near unity.
        let late = *bus.samples.last().unwrap();
        assert!(
            late > 0.5 * 0.9,
            "gain failed to recover: quiet sample {late}"
        );
    }
}
