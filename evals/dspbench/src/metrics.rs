//! Effectiveness metrics (contract; implementation pending).
//!
//! The decision layer scores what the product experiences (VAD activations
//! against ground-truth speech intervals); the diagnostic layer explains
//! movements in the decision layer. Chain group delay is compensated using
//! the chain's own declared latency — the declared-latency contract paying
//! for itself.

use serde::Serialize;

/// Decision-layer score for one processed track against ground truth.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionScore {
    /// VAD activations whose onset lies outside every truth interval
    /// (with tolerance), per minute of audio.
    pub false_activations_per_min: f32,
    /// Truth intervals that produced no activation.
    pub missed_onsets: usize,
    /// Total truth intervals.
    pub total_onsets: usize,
    /// Median onset latency in ms over detected intervals (VAD SpeechStart
    /// commit minus truth start), after latency compensation.
    pub onset_latency_ms_p50: f32,
    /// Total activations observed.
    pub activations: usize,
}

/// Score a processed track with the shipped energy VAD against ground-truth
/// speech intervals (sample indices in the ORIGINAL clean timeline).
///
/// `chain_latency_samples` shifts truth to the processed timeline. An
/// activation matches a truth interval if its onset falls within
/// [start - tol, end]; `tol_ms` defaults ~120ms via the caller.
pub fn score_vad(
    processed: &[f32],
    sample_rate: u32,
    truth: &[(usize, usize)],
    vad_config: gemini_genai_rs::vad::VadConfig,
    chain_latency_samples: usize,
    tol_ms: u32,
) -> DecisionScore {
    use gemini_genai_rs::vad::{VadEvent, VoiceActivityDetector};

    let frame_size = vad_config.frame_size().max(1);
    let mut vad = VoiceActivityDetector::new(vad_config);

    // Convert the processed track to i16 PCM the same way the real chain's
    // output would be quantized before hitting the VAD.
    let samples_i16: Vec<i16> = processed
        .iter()
        .map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect();

    // Run the VAD frame by frame, recording the commit sample (frame-end
    // index of the frame that produced the event) for every SpeechStart.
    let mut activations: Vec<usize> = Vec::new();
    let mut offset = 0usize;
    for chunk in samples_i16.chunks(frame_size) {
        if let Some(VadEvent::SpeechStart) = vad.process_frame(chunk) {
            activations.push(offset + chunk.len());
        }
        offset += chunk.len();
    }

    // Truth intervals live in the original clean timeline; shift them into
    // the processed timeline by the chain's declared group delay.
    let shifted_truth: Vec<(usize, usize)> = truth
        .iter()
        .map(|&(a, b)| (a + chain_latency_samples, b + chain_latency_samples))
        .collect();

    let tol_samples = (u64::from(tol_ms) * u64::from(sample_rate) / 1000) as i64;

    // Greedy one-to-one matching, in time order: each activation claims the
    // earliest still-unmatched truth interval whose tolerance window
    // contains its onset.
    let mut matched = vec![false; shifted_truth.len()];
    let mut latencies_ms: Vec<f32> = Vec::new();
    let mut false_count = 0usize;

    for &onset in &activations {
        let mut found = None;
        for (i, &(start, end)) in shifted_truth.iter().enumerate() {
            if matched[i] {
                continue;
            }
            let window_start = start as i64 - tol_samples;
            if onset as i64 >= window_start && onset as i64 <= end as i64 {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => {
                matched[i] = true;
                let latency_samples = onset as i64 - shifted_truth[i].0 as i64;
                latencies_ms.push(latency_samples as f32 * 1000.0 / sample_rate as f32);
            }
            None => false_count += 1,
        }
    }

    let missed_onsets = matched.iter().filter(|&&m| !m).count();
    let duration_s = processed.len() as f32 / sample_rate as f32;
    let false_activations_per_min = if duration_s > 0.0 {
        false_count as f32 * 60.0 / duration_s
    } else {
        0.0
    };

    let onset_latency_ms_p50 = median(&mut latencies_ms);

    DecisionScore {
        false_activations_per_min,
        missed_onsets,
        total_onsets: truth.len(),
        onset_latency_ms_p50,
        activations: activations.len(),
    }
}

/// Median of `values` (0.0 for an empty slice). Sorts in place.
fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Segmental SNR (dB) of `processed` against the clean reference, after
/// shifting by the chain's declared latency; measured only inside truth
/// intervals (speech segments), 10ms segments, per-segment SNR clamped to
/// [-10, 35] dB before averaging (standard practice — extreme segments
/// otherwise dominate).
pub fn segmental_snr_db(
    processed: &[f32],
    clean: &[f32],
    sample_rate: u32,
    truth: &[(usize, usize)],
    chain_latency_samples: usize,
) -> f32 {
    const EPS: f64 = 1e-12;

    let seg_len = (sample_rate as usize * 10) / 1000;
    if seg_len == 0 {
        return 0.0;
    }

    let mut snr_sum = 0.0f64;
    let mut count = 0usize;

    for &(a, b) in truth {
        let a = a.min(clean.len());
        let b = b.min(clean.len());
        if b <= a {
            continue;
        }
        let mut s = a;
        while s < b {
            let end = (s + seg_len).min(b);
            let p_start = s + chain_latency_samples;
            let p_end = end + chain_latency_samples;
            if end > clean.len() || p_end > processed.len() || p_start >= p_end {
                s = end;
                continue;
            }
            let clean_seg = &clean[s..end];
            let proc_seg = &processed[p_start..p_end];

            let mut sum_clean_sq = 0.0f64;
            let mut sum_diff_sq = 0.0f64;
            for (&c, &p) in clean_seg.iter().zip(proc_seg.iter()) {
                let c = c as f64;
                let p = p as f64;
                sum_clean_sq += c * c;
                let d = c - p;
                sum_diff_sq += d * d;
            }

            let snr = 10.0 * ((sum_clean_sq + EPS) / (sum_diff_sq + EPS)).log10();
            snr_sum += snr.clamp(-10.0, 35.0);
            count += 1;
            s = end;
        }
    }

    if count == 0 {
        0.0
    } else {
        (snr_sum / count as f64) as f32
    }
}

/// Level statistics over 100ms blocks: (mean RMS dBFS, std-dev dB) —
/// the AGC's report card: mean near target, small deviation.
pub fn level_stats_dbfs(processed: &[f32], sample_rate: u32) -> (f32, f32) {
    const SILENCE_FLOOR_DBFS: f32 = -70.0;

    let block_len = (sample_rate as usize * 100) / 1000;
    if block_len == 0 || processed.is_empty() {
        return (-100.0, 0.0);
    }

    let mut levels: Vec<f32> = Vec::new();
    for block in processed.chunks(block_len) {
        if block.is_empty() {
            continue;
        }
        let sum_sq: f64 = block.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = (sum_sq / block.len() as f64).sqrt();
        let dbfs = (20.0 * (rms + 1e-9).log10()) as f32;
        if dbfs >= SILENCE_FLOOR_DBFS {
            levels.push(dbfs);
        }
    }

    if levels.is_empty() {
        return (-100.0, 0.0);
    }

    let mean = levels.iter().sum::<f32>() / levels.len() as f32;
    let variance =
        levels.iter().map(|&v| (v - mean) * (v - mean)).sum::<f32>() / levels.len() as f32;
    (mean, variance.sqrt())
}

/// Hann-window a slice (copy), used to shape DFT input for `log_spectral_distance_db`.
fn hann_window(x: &[f32]) -> Vec<f32> {
    let n = x.len();
    if n <= 1 {
        return x.to_vec();
    }
    x.iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos();
            s * w
        })
        .collect()
}

/// Naive O(n^2) real-input DFT magnitude at bin `k`. Offline use only —
/// windows here are 320 samples, so this is cheap enough not to bother
/// with an FFT.
fn dft_bin_magnitude(x: &[f32], k: usize) -> f32 {
    let n = x.len() as f32;
    let mut re = 0.0f32;
    let mut im = 0.0f32;
    for (i, &s) in x.iter().enumerate() {
        let theta = -2.0 * std::f32::consts::PI * k as f32 * i as f32 / n;
        re += s * theta.cos();
        im += s * theta.sin();
    }
    (re * re + im * im).sqrt()
}

/// DFT bin indices for a window of length `n` at `sample_rate` whose center
/// frequency falls in [100, 6000] Hz.
fn band_bins(n: usize, sample_rate: u32) -> Vec<usize> {
    let mut bins = Vec::new();
    for k in 0..=(n / 2) {
        let freq = k as f32 * sample_rate as f32 / n as f32;
        if (100.0..=6000.0).contains(&freq) {
            bins.push(k);
        }
    }
    bins
}

/// Log-spectral distance (dB) between processed and clean inside truth
/// intervals — near-end distortion during double-talk, the "did the AEC
/// mangle the user" number. 20ms windows, mean over bins 100-6000 Hz.
pub fn log_spectral_distance_db(
    processed: &[f32],
    clean: &[f32],
    sample_rate: u32,
    truth: &[(usize, usize)],
    chain_latency_samples: usize,
) -> f32 {
    const EPS: f32 = 1e-6;

    let win_len = (sample_rate as usize * 20) / 1000;
    if win_len < 2 {
        return 0.0;
    }

    let mut lsd_sum = 0.0f64;
    let mut count = 0usize;

    for &(a, b) in truth {
        let a = a.min(clean.len());
        let b = b.min(clean.len());
        if b <= a {
            continue;
        }
        let mut s = a;
        while s + win_len <= b {
            let end = s + win_len;
            let p_start = s + chain_latency_samples;
            let p_end = end + chain_latency_samples;
            if end > clean.len() || p_end > processed.len() {
                break;
            }

            let clean_win = hann_window(&clean[s..end]);
            let proc_win = hann_window(&processed[p_start..p_end]);
            let bins = band_bins(clean_win.len(), sample_rate);
            if !bins.is_empty() {
                let mut sq_sum = 0.0f64;
                for &k in &bins {
                    let cmag = dft_bin_magnitude(&clean_win, k);
                    let pmag = dft_bin_magnitude(&proc_win, k);
                    let d = 20.0 * ((cmag + EPS) / (pmag + EPS)).log10();
                    sq_sum += (d as f64) * (d as f64);
                }
                let lsd = (sq_sum / bins.len() as f64).sqrt();
                lsd_sum += lsd;
                count += 1;
            }

            s = end;
        }
    }

    if count == 0 {
        0.0
    } else {
        (lsd_sum / count as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_genai_rs::vad::VadConfig;
    use std::f32::consts::PI;

    const SR: u32 = 16_000;

    /// Sine-burst signal alternating `on_ms` voiced activity with `off_ms`
    /// silence, plus the ground-truth intervals (samples) — self-contained,
    /// no dependency on `signal.rs`.
    fn make_bursts(
        sample_rate: u32,
        amplitude: f32,
        freq_hz: f32,
        on_ms: u32,
        off_ms: u32,
        n_bursts: usize,
    ) -> (Vec<f32>, Vec<(usize, usize)>) {
        let on_len = (sample_rate as u64 * on_ms as u64 / 1000) as usize;
        let off_len = (sample_rate as u64 * off_ms as u64 / 1000) as usize;
        let mut sig = Vec::new();
        let mut truth = Vec::new();
        for _ in 0..n_bursts {
            let start = sig.len();
            for i in 0..on_len {
                let t = i as f32 / sample_rate as f32;
                sig.push(amplitude * (2.0 * PI * freq_hz * t).sin());
            }
            truth.push((start, sig.len()));
            sig.extend(std::iter::repeat_n(0.0f32, off_len));
        }
        (sig, truth)
    }

    #[test]
    fn score_vad_detects_clean_bursts() {
        let (sig, truth) = make_bursts(SR, 0.3, 440.0, 1000, 1000, 3);
        let score = score_vad(&sig, SR, &truth, VadConfig::noisy_street(), 0, 120);

        assert_eq!(score.total_onsets, 3);
        assert_eq!(score.missed_onsets, 0, "expected every burst detected");
        assert_eq!(
            score.false_activations_per_min, 0.0,
            "expected zero false activations on a clean signal"
        );
        assert!(
            score.onset_latency_ms_p50 >= 0.0 && score.onset_latency_ms_p50 <= 500.0,
            "onset latency out of expected range: {}",
            score.onset_latency_ms_p50
        );
    }

    #[test]
    fn score_vad_counts_false_activations_when_truth_empty() {
        let (sig, _truth) = make_bursts(SR, 0.3, 440.0, 1000, 1000, 3);
        let score = score_vad(&sig, SR, &[], VadConfig::noisy_street(), 0, 120);

        assert_eq!(score.total_onsets, 0);
        assert_eq!(score.missed_onsets, 0);
        assert!(
            score.false_activations_per_min > 0.0,
            "echo-only self-barge-in must register as false activations"
        );
    }

    #[test]
    fn score_vad_respects_latency_shift() {
        let (sig, truth) = make_bursts(SR, 0.3, 440.0, 1000, 1000, 3);
        let baseline = score_vad(&sig, SR, &truth, VadConfig::noisy_street(), 0, 120);

        let mut shifted_sig = vec![0.0f32; 800];
        shifted_sig.extend_from_slice(&sig);
        let shifted = score_vad(
            &shifted_sig,
            SR,
            &truth,
            VadConfig::noisy_street(),
            800,
            120,
        );

        assert_eq!(shifted.total_onsets, baseline.total_onsets);
        assert_eq!(shifted.missed_onsets, baseline.missed_onsets);
        assert_eq!(shifted.activations, baseline.activations);
        assert_eq!(shifted.false_activations_per_min, 0.0);
        assert_eq!(baseline.false_activations_per_min, 0.0);
        assert!(shifted.onset_latency_ms_p50 >= 0.0 && shifted.onset_latency_ms_p50 <= 500.0);
    }

    #[test]
    fn segmental_snr_perfect_alignment_is_max() {
        let n = SR as usize * 2;
        let latency = 100usize;
        let clean: Vec<f32> = (0..n)
            .map(|i| 0.2 * (2.0 * PI * 300.0 * (i as f32 / SR as f32)).sin())
            .collect();
        let truth = vec![(0usize, n)];

        let mut processed = vec![0.0f32; latency];
        processed.extend_from_slice(&clean);
        let snr = segmental_snr_db(&processed, &clean, SR, &truth, latency);
        assert!(
            (snr - 35.0).abs() < 0.5,
            "expected clamp ceiling, got {snr}"
        );

        // Now bury it in noise well above the signal level.
        let mut noisy = processed.clone();
        let mut state = 0x2545F4914F6CDD1Du64;
        for sample in noisy.iter_mut().skip(latency) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let r = ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0;
            *sample += r * 0.5;
        }
        let snr_noisy = segmental_snr_db(&noisy, &clean, SR, &truth, latency);
        assert!(
            snr_noisy < snr - 10.0,
            "expected much lower SNR under noise: {snr_noisy} vs {snr}"
        );
    }

    #[test]
    fn level_stats_reports_target() {
        let target_dbfs = -20.0f32;
        let rms_target = 10f32.powf(target_dbfs / 20.0);
        let amplitude = rms_target * std::f32::consts::SQRT_2;
        let n = SR as usize * 2;
        let sig: Vec<f32> = (0..n)
            .map(|i| amplitude * (2.0 * PI * 300.0 * (i as f32 / SR as f32)).sin())
            .collect();

        let (mean, std) = level_stats_dbfs(&sig, SR);
        assert!((mean - target_dbfs).abs() < 0.5, "mean {mean}");
        assert!(std < 0.5, "std {std}");
    }

    #[test]
    fn lsd_zero_for_identical() {
        let n = SR as usize;
        let clean: Vec<f32> = (0..n)
            .map(|i| 0.3 * (2.0 * PI * 500.0 * (i as f32 / SR as f32)).sin())
            .collect();
        let truth = vec![(0usize, n)];

        let lsd = log_spectral_distance_db(&clean, &clean, SR, &truth, 0);
        assert!(
            lsd.abs() < 1e-3,
            "expected ~0 for identical signals, got {lsd}"
        );
    }

    #[test]
    fn lsd_positive_for_filtered() {
        let n = SR as usize;
        let clean: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                0.1 * ((2.0 * PI * 300.0 * t).sin()
                    + (2.0 * PI * 1000.0 * t).sin()
                    + (2.0 * PI * 3000.0 * t).sin()
                    + (2.0 * PI * 5000.0 * t).sin())
            })
            .collect();

        // Aggressive one-pole lowpass — broad-spectrum distortion across the
        // whole 100-6000 Hz measurement band.
        let a = 0.9f32;
        let mut y = 0.0f32;
        let processed: Vec<f32> = clean
            .iter()
            .map(|&x| {
                y = a * y + (1.0 - a) * x;
                y
            })
            .collect();

        let truth = vec![(0usize, n)];
        let lsd = log_spectral_distance_db(&processed, &clean, SR, &truth, 0);
        assert!(lsd > 1.0, "expected significant distortion, got {lsd}");
    }
}
