//! Deterministic scene-signal generators.
//!
//! Everything is seeded: the same scene definition always produces the same
//! samples, so metric deltas between chain variants are attributable to the
//! chain, never to the material.

#![allow(clippy::needless_range_loop)] // sample-index loops read naturally in DSP code

use std::f32::consts::PI;

/// Xorshift64* PRNG — tiny, seedable, dependency-free.
pub struct Rng(pub u64);

impl Rng {
    /// Seeded generator (seed 0 is remapped to a fixed non-zero value).
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    /// Next value in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D) >> 40) as f32) / (1u64 << 24) as f32
    }

    /// Uniform in [-1, 1).
    pub fn next_bipolar(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

/// RMS of a slice (0.0 for empty).
pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = x.iter().map(|s| s * s).sum();
    (sum_sq / x.len() as f32).sqrt()
}

/// Scale a signal to an RMS given in dBFS (1.0 = 0 dBFS).
pub fn set_level_dbfs(x: &mut [f32], level_dbfs: f32) {
    let current_rms = rms(x);
    if current_rms < 1e-10 {
        return; // Silent input, no-op
    }
    let target_rms = 10f32.powf(level_dbfs / 20.0);
    let scale = target_rms / current_rms;
    for s in x.iter_mut() {
        *s *= scale;
    }
}

/// White noise at the given RMS (linear full-scale units).
pub fn white(rng: &mut Rng, len: usize, rms_target: f32) -> Vec<f32> {
    let mut out = vec![0.0; len];
    for s in out.iter_mut() {
        *s = rng.next_bipolar();
    }
    set_level_dbfs(&mut out, 20.0 * rms_target.log10());
    out
}

/// Pink (1/f) noise at the given RMS using a 3-biquad -3dB/octave approximation.
///
/// Uses cascaded low-pass filters to approximate 1/f spectral shape, with
/// coefficients chosen to give roughly -3 dB/octave response.
pub fn pink(rng: &mut Rng, len: usize, rms_target: f32) -> Vec<f32> {
    // Cascade approach: white noise through multiple stages
    let mut out = vec![0.0; len];

    // Multi-stage cascade to build up the pink spectrum
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;

    // Filter coefficients chosen to approximate pink spectrum
    const A0: f32 = 0.0043;
    const A1: f32 = 0.0169;
    const A2: f32 = 0.0630;
    const A3: f32 = 0.2183;

    for s in out.iter_mut() {
        let white = rng.next_bipolar();

        // Cascade stages
        s0 = A0 * white + (1.0 - A0) * s0;
        s1 = A1 * s0 + (1.0 - A1) * s1;
        s2 = A2 * s1 + (1.0 - A2) * s2;
        s3 = A3 * s2 + (1.0 - A3) * s3;

        // Mix stages: accumulate cascaded outputs to approximate pink spectrum
        *s = s0 + s1 + s2 + s3;
    }

    set_level_dbfs(&mut out, 20.0 * rms_target.log10());
    out
}

/// Direct-form convolution (offline; scenes are seconds long).
pub fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
    if x.is_empty() || h.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0.0; x.len() + h.len() - 1];
    for (n, out_sample) in out.iter_mut().enumerate() {
        for (m, &hm) in h.iter().enumerate() {
            if n >= m && n - m < x.len() {
                *out_sample += x[n - m] * hm;
            }
        }
    }
    out
}

/// Sparse decaying synthetic room impulse response with unit direct path
/// scaled so total gain is `gain_db`.
///
/// Creates h[0]=1.0, then `taps-1` reflections with exponential decay
/// (last ~1% of first) and alternating-ish signs. 20-40% of taps are zeroed
/// (sparse). Final RIR is scaled so sum(|h|) = 10^(gain_db/20).
pub fn synth_rir(rng: &mut Rng, taps: usize, gain_db: f32) -> Vec<f32> {
    if taps == 0 {
        return vec![1.0];
    }
    let mut h = vec![0.0; taps];
    h[0] = 1.0;

    // Exponential decay: last tap ~ 1% of first
    let decay_rate = 0.01f32.powf(1.0 / (taps - 1) as f32);

    // Alternating-ish signs: roughly every other tap, with randomization
    for i in 1..taps {
        let magnitude = decay_rate.powi(i as i32);
        let sign = if rng.next_f32() < 0.4 || (i % 2) == 1 {
            -1.0
        } else {
            1.0
        };
        h[i] = sign * magnitude;
    }

    // Zero out 20-40% of taps to make it sparse
    let sparse_fraction = 0.2 + 0.2 * rng.next_f32();
    let sparse_count = ((taps as f32) * sparse_fraction) as usize;
    for _ in 0..sparse_count {
        let idx = (rng.next_f32() * (taps - 1) as f32) as usize + 1; // never zero h[0]
        if idx < taps {
            h[idx] = 0.0;
        }
    }

    // Scale so sum(|h|) = 10^(gain_db/20)
    let sum_abs: f32 = h.iter().map(|v| v.abs()).sum();
    let target_gain = 10f32.powf(gain_db / 20.0);
    if sum_abs > 1e-10 {
        let scale = target_gain / sum_abs;
        for v in h.iter_mut() {
            *v *= scale;
        }
    }

    h
}

/// A speech-like test signal: harmonic bursts with pitch/amplitude
/// modulation, alternating `on_ms` voiced activity with `off_ms` silence.
/// Returns the samples plus the ground-truth activity intervals in samples
/// (start, end) — free labels, because we composed the scene.
///
/// Each burst features:
///   - Fundamental wandering between 110-220 Hz (controlled by RNG)
///   - 3-5 harmonics with decreasing amplitude
///   - Amplitude modulation at 3-6 Hz (syllabic rhythm)
///   - 20 ms raised-cosine onset/offset ramps for smooth edges
///
/// The entire voiced portion is scaled so its RMS = 10^(level_dbfs/20).
pub fn speech_proxy(
    rng: &mut Rng,
    sample_rate: u32,
    duration_s: f32,
    on_ms: u32,
    off_ms: u32,
    level_dbfs: f32,
) -> (Vec<f32>, Vec<(usize, usize)>) {
    let sr_f = sample_rate as f32;
    let total_samples = (duration_s * sr_f) as usize;
    let mut out = vec![0.0; total_samples];
    let mut truth = Vec::new();

    let on_samples = ((on_ms as f32 / 1000.0) * sr_f) as usize;
    let off_samples = ((off_ms as f32 / 1000.0) * sr_f) as usize;
    let ramp_samples = ((20.0 / 1000.0) * sr_f) as usize; // 20 ms ramps

    let mut phase = 0.0;
    let mut am_phase = 0.0;
    let mut sample_idx = 0;

    while sample_idx < total_samples {
        // Voiced burst
        if sample_idx + on_samples <= total_samples {
            let burst_start = sample_idx;
            let fundamental = 110.0 + 110.0 * rng.next_f32(); // 110-220 Hz
            let num_harmonics = 3 + (rng.next_f32() * 2.9) as usize; // 3-5
            let am_rate = 3.0 + 3.0 * rng.next_f32(); // 3-6 Hz

            for i in 0..on_samples {
                // Amplitude modulation at syllabic rate
                am_phase = (am_phase + am_rate / sr_f) % 1.0;
                // Depth 0.4 with floor 0.2: real syllabic energy dips, but
                // never to digital zero (which fragments VAD truth matching).
                let am = 0.6 + 0.4 * (2.0 * PI * am_phase).sin();

                // Harmonic stack
                let mut harmonic_out = 0.0;
                for h in 1..=num_harmonics {
                    let freq = fundamental * h as f32;
                    let harmonic_phase = (phase + freq / sr_f * i as f32) * 2.0 * PI;
                    let harmonic_amp = 1.0 / h as f32; // Decreasing amplitude
                    harmonic_out += harmonic_amp * harmonic_phase.sin();
                }
                harmonic_out /= num_harmonics as f32; // Normalize by harmonic count

                // Apply raised-cosine ramps
                let ramp = if i < ramp_samples {
                    // Onset ramp
                    0.5 * (1.0 - (PI * (i as f32 / ramp_samples as f32)).cos())
                } else if i >= on_samples - ramp_samples {
                    // Offset ramp
                    let offset_i = i - (on_samples - ramp_samples);
                    0.5 * (1.0 + (PI * (offset_i as f32 / ramp_samples as f32)).cos())
                } else {
                    1.0
                };

                out[sample_idx + i] = am * ramp * harmonic_out;
            }

            truth.push((burst_start, sample_idx + on_samples));
            phase = (phase + fundamental / sr_f * on_samples as f32) % 1.0;
            sample_idx += on_samples;
        }

        // Silence
        if sample_idx + off_samples <= total_samples {
            phase = (phase + rng.next_f32() * 0.1) % 1.0; // Randomize phase between bursts
            sample_idx += off_samples;
        } else {
            break;
        }
    }

    // Scale the entire voiced portion to target RMS
    if !truth.is_empty() {
        let mut voiced_samples = Vec::new();
        for &(start, end) in &truth {
            voiced_samples.extend_from_slice(&out[start..end]);
        }
        if !voiced_samples.is_empty() {
            let voiced_rms = rms(&voiced_samples);
            if voiced_rms > 1e-10 {
                let target_rms = 10f32.powf(level_dbfs / 20.0);
                let scale = target_rms / voiced_rms;
                for s in out.iter_mut() {
                    *s *= scale;
                }
            }
        }
    }

    (out, truth)
}

/// Speech-shaped babble proxy at the given RMS.
///
/// Creates a proxy by summing 6-8 detuned speech_proxy-like harmonic wanderers
/// with random phases. This is not recorded babble but an algorithmic proxy.
/// Normalized to the requested RMS.
pub fn babble(rng: &mut Rng, len: usize, rms_target: f32) -> Vec<f32> {
    let mut out = vec![0.0; len];
    let sr_f = 16000.0; // Assumed sample rate for babble generation
    let num_talkers = 6 + (rng.next_f32() * 2.9) as usize; // 6-8 talkers

    for talker_idx in 0..num_talkers {
        // Each talker is a slow harmonic wanderer
        let fundamental = 80.0 + (talker_idx as f32) * 60.0 + rng.next_f32() * 60.0; // Spread across voice range
        let num_harmonics = 3 + (rng.next_f32() * 2.9) as usize;
        let mut phase = rng.next_f32();
        let mut am_phase = rng.next_f32();

        for i in 0..len {
            // Slow AM (more continuous than speech)
            am_phase = (am_phase + 1.5 / sr_f) % 1.0;
            let am = 0.6 + 0.4 * (2.0 * PI * am_phase).sin();

            // Harmonic stack with frequency wandering
            let wander_amount = 0.05 * (0.1 * i as f32 / sr_f).sin();
            let wandered_fundamental = fundamental * (1.0 + wander_amount);

            let mut harmonic_out = 0.0;
            for h in 1..=num_harmonics {
                let _freq = wandered_fundamental * h as f32;
                let harmonic_phase = phase * 2.0 * PI;
                harmonic_out += (1.0 / h as f32) * harmonic_phase.sin();
            }
            harmonic_out /= num_harmonics as f32;

            out[i] += am * harmonic_out / num_talkers as f32;
            phase = (phase + wandered_fundamental / sr_f) % 1.0;
        }
    }

    set_level_dbfs(&mut out, 20.0 * rms_target.log10());
    out
}

/// Street-traffic proxy (low-frequency rumble + intermittent horn events).
///
/// Combines:
/// - Pink noise low-passed to ~300 Hz (rumble)
/// - Intermittent "horn" events: 0.2-0.5 s bursts around 400/600 Hz,
///   a few per 10 seconds
///
/// Normalized to the requested RMS.
pub fn traffic(rng: &mut Rng, len: usize, rms_target: f32, sample_rate: u32) -> Vec<f32> {
    let sr_f = sample_rate as f32;
    let mut out = vec![0.0; len];

    // Low-frequency rumble: pink noise low-passed at ~300 Hz
    let mut lp_state = 0.0f32;
    const LP_ALPHA: f32 = 0.05; // One-pole LP at ~300 Hz for 16 kHz

    for i in 0..len {
        let pink_sample = {
            let mut lp1 = 0.0f32;
            let mut lp2 = 0.0f32;
            let mut lp3 = 0.0f32;
            let white = rng.next_bipolar();
            lp1 = 0.02 * white + 0.98 * lp1;
            lp2 = 0.05 * lp1 + 0.95 * lp2;
            lp3 = 0.1 * lp2 + 0.9 * lp3;
            lp3
        };

        lp_state = LP_ALPHA * pink_sample + (1.0 - LP_ALPHA) * lp_state;
        out[i] += 0.3 * lp_state;
    }

    // Intermittent horn events: a few per 10 seconds
    let event_interval_samples = (10.0 * sr_f) as usize;
    let mut horn_idx = (rng.next_f32() * event_interval_samples as f32) as usize;

    while horn_idx < len {
        let duration_ms = 200.0 + rng.next_f32() * 300.0; // 0.2-0.5 s
        let duration_samples = ((duration_ms / 1000.0) * sr_f) as usize;
        let freq1 = 400.0 + rng.next_f32() * 100.0; // ~400 Hz
        let freq2 = 600.0 + rng.next_f32() * 100.0; // ~600 Hz
        let mut horn_phase = 0.0;

        for j in 0..duration_samples {
            if horn_idx + j >= len {
                break;
            }
            let f1 = (2.0 * PI * freq1 * horn_phase).sin();
            let f2 = (2.0 * PI * freq2 * horn_phase).sin();
            out[horn_idx + j] += 0.4 * (f1 + f2) * 0.5;
            horn_phase = (horn_phase + 1.0 / sr_f) % 1.0;
        }

        horn_idx += duration_samples + event_interval_samples;
    }

    set_level_dbfs(&mut out, 20.0 * rms_target.log10());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms() {
        // Empty should be 0
        assert_eq!(rms(&[]), 0.0);

        // Constant signal
        let const_sig = vec![1.0; 100];
        assert!((rms(&const_sig) - 1.0).abs() < 1e-5);

        // sin wave with amplitude 1 has RMS ~ 1/sqrt(2)
        let mut sig = vec![0.0; 1000];
        for (i, s) in sig.iter_mut().enumerate() {
            *s = (2.0 * PI * i as f32 / 100.0).sin();
        }
        let r = rms(&sig);
        assert!((r - 1.0 / 2.0_f32.sqrt()).abs() < 0.01);
    }

    #[test]
    fn test_set_level_dbfs() {
        // Silent input should be no-op
        let mut silent = vec![0.0; 100];
        set_level_dbfs(&mut silent, -40.0);
        assert!(silent.iter().all(|&s| s == 0.0));

        // Scale to -20 dBFS (0.1 RMS)
        let mut sig = vec![1.0; 100];
        set_level_dbfs(&mut sig, -20.0);
        let r = rms(&sig);
        assert!((r - 0.1).abs() < 1e-3);
    }

    #[test]
    fn test_white_rms() {
        // White noise should hit target RMS within 5%
        let mut rng = Rng::new(12345);
        let target_rms = 0.2;
        let sig = white(&mut rng, 8000, target_rms);
        let measured_rms = rms(&sig);
        let error_pct = ((measured_rms - target_rms).abs() / target_rms) * 100.0;
        assert!(error_pct < 5.0, "White RMS error {:.1}%", error_pct);
    }

    #[test]
    fn test_pink_rms() {
        // Pink noise should hit target RMS within 5%
        let mut rng = Rng::new(54321);
        let target_rms = 0.15;
        let sig = pink(&mut rng, 8000, target_rms);
        let measured_rms = rms(&sig);
        let error_pct = ((measured_rms - target_rms).abs() / target_rms) * 100.0;
        assert!(error_pct < 5.0, "Pink RMS error {:.1}%", error_pct);
    }

    #[test]
    fn test_pink_vs_white_spectrum() {
        // Pink noise should have more energy in low frequencies than white
        let mut rng1 = Rng::new(11111);
        let mut rng2 = Rng::new(11111);
        let white_sig = white(&mut rng1, 16000, 0.1);
        let pink_sig = pink(&mut rng2, 16000, 0.1);

        // Compare low-frequency content: pink low-pass filtered should have
        // a higher ratio to the unfiltered signal than white does
        let mut white_lp = 0.0f32;
        let mut pink_lp = 0.0f32;
        let lp_alpha = 0.02; // ~300 Hz equivalent low-pass

        let mut white_energy_lp = 0.0f32;
        let mut white_energy_total = 0.0f32;
        let mut pink_energy_lp = 0.0f32;
        let mut pink_energy_total = 0.0f32;

        for (w, p) in white_sig.iter().zip(pink_sig.iter()) {
            white_lp = lp_alpha * w + (1.0 - lp_alpha) * white_lp;
            pink_lp = lp_alpha * p + (1.0 - lp_alpha) * pink_lp;

            white_energy_lp += white_lp * white_lp;
            white_energy_total += w * w;
            pink_energy_lp += pink_lp * pink_lp;
            pink_energy_total += p * p;
        }

        let white_ratio = white_energy_lp / white_energy_total;
        let pink_ratio = pink_energy_lp / pink_energy_total;

        // Pink should have higher ratio of low-frequency energy than white
        assert!(
            pink_ratio > white_ratio,
            "Pink ratio {:.3} should be > white ratio {:.3}",
            pink_ratio,
            white_ratio
        );
    }

    #[test]
    fn test_convolve() {
        // Test with simple 3x3 example:
        // x = [1, 2, 3]
        // h = [1, 0, 1]
        // Expected: [1, 2, 4, 2, 3]
        let x = vec![1.0, 2.0, 3.0];
        let h = vec![1.0, 0.0, 1.0];
        let result = convolve(&x, &h);
        assert_eq!(result.len(), 5);
        assert!((result[0] - 1.0).abs() < 1e-5);
        assert!((result[1] - 2.0).abs() < 1e-5);
        assert!((result[2] - 4.0).abs() < 1e-5);
        assert!((result[3] - 2.0).abs() < 1e-5);
        assert!((result[4] - 3.0).abs() < 1e-5);
    }

    #[test]
    fn test_synth_rir_gain_bound() {
        // RIR should have sum(|h|) = 10^(gain_db/20)
        let mut rng = Rng::new(99999);
        let gain_db = -10.0;
        let rir = synth_rir(&mut rng, 50, gain_db);

        let target_gain = 10f32.powf(gain_db / 20.0);
        let sum_abs: f32 = rir.iter().map(|v| v.abs()).sum();

        let error = (sum_abs - target_gain).abs();
        assert!(
            error < 1e-3,
            "RIR gain bound failed: got {:.4}, expected {:.4}",
            sum_abs,
            target_gain
        );
    }

    #[test]
    fn test_speech_proxy_burst_level() {
        // speech_proxy should produce bursts at requested level
        let mut rng = Rng::new(42);
        let (sig, truth) = speech_proxy(&mut rng, 16000, 2.0, 500, 500, -20.0);

        assert!(!truth.is_empty(), "Should have at least one truth interval");

        // Measure RMS inside and outside bursts
        let mut burst_samples = Vec::new();
        for &(start, end) in &truth {
            burst_samples.extend_from_slice(&sig[start..end]);
        }
        let burst_rms = rms(&burst_samples);

        // Target for -20 dBFS is 0.1
        let expected_rms = 0.1;
        let error_pct = ((burst_rms - expected_rms).abs() / expected_rms) * 100.0;
        assert!(
            error_pct < 5.0,
            "Burst RMS error {:.1}%: got {:.4}, expected {:.4}",
            error_pct,
            burst_rms,
            expected_rms
        );

        // Check silence between bursts is very low
        let mut silence_samples = Vec::new();
        let mut truth_sorted = truth.clone();
        truth_sorted.sort();
        let mut last_end = 0;
        for &(start, end) in &truth_sorted {
            if start > last_end {
                silence_samples.extend_from_slice(&sig[last_end..start]);
            }
            last_end = end;
        }
        if last_end < sig.len() {
            silence_samples.extend_from_slice(&sig[last_end..]);
        }

        if !silence_samples.is_empty() {
            let silence_rms = rms(&silence_samples);
            // Silence should be very quiet, at least 40 dB below the signal
            assert!(
                silence_rms < burst_rms / 100.0,
                "Silence not quiet enough: {:.4} vs burst {:.4}",
                silence_rms,
                burst_rms
            );
        }
    }

    #[test]
    fn test_babble_rms() {
        // Babble should hit target RMS within 5%
        let mut rng = Rng::new(67890);
        let target_rms = 0.12;
        let sig = babble(&mut rng, 8000, target_rms);
        let measured_rms = rms(&sig);
        let error_pct = ((measured_rms - target_rms).abs() / target_rms) * 100.0;
        assert!(error_pct < 5.0, "Babble RMS error {:.1}%", error_pct);
    }

    #[test]
    fn test_traffic_rms() {
        // Traffic should hit target RMS within 5%
        let mut rng = Rng::new(11223);
        let target_rms = 0.18;
        let sig = traffic(&mut rng, 8000, target_rms, 16000);
        let measured_rms = rms(&sig);
        let error_pct = ((measured_rms - target_rms).abs() / target_rms) * 100.0;
        assert!(error_pct < 5.0, "Traffic RMS error {:.1}%", error_pct);
    }
}
