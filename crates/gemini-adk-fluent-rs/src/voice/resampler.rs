//! Streaming, anti-aliased sample-rate conversion for mono PCM16.
//!
//! Phone audio crosses three rates: 8 kHz on the line, 16 kHz into the Live
//! API and 24 kHz out of it. Converting 24 kHz down to 8 kHz without a
//! low-pass filter folds everything between 4 and 12 kHz (sibilants,
//! breath, the top of the model's voice) back into the audible band as
//! aliasing; converting chunk by chunk without carrying filter state across
//! chunks adds a click at every chunk boundary.
//!
//! [`StreamResampler`] is a rational polyphase FIR resampler. Its low-pass
//! prototype is a Blackman-windowed sinc with its cutoff just below the
//! lower of the two Nyquist frequencies. It keeps its history between calls,
//! so a stream resampled in 20 ms chunks is sample-for-sample the same as
//! the whole stream resampled at once. It needs no dependencies and runs
//! 16 multiply-adds per output sample.

/// Filter taps per polyphase branch. 16 taps with a Blackman window give
/// roughly 70 dB stop-band rejection with a transition band of about 10% of
/// the lower Nyquist frequency, at under a millisecond of delay at 8 kHz.
const TAPS_PER_PHASE: usize = 16;
/// The pass-band edge, as a fraction of the lower Nyquist frequency.
const CUTOFF: f64 = 0.9;

/// A stateful resampler from `from_hz` to `to_hz`. See the module docs.
#[derive(Debug, Clone)]
pub struct StreamResampler {
    /// Upsampling factor (L).
    up: usize,
    /// Downsampling factor (M).
    down: usize,
    /// `bank[phase][tap]`: the prototype filter split into `up` phases.
    bank: Vec<[f32; TAPS_PER_PHASE]>,
    /// The last `TAPS_PER_PHASE - 1` input samples, then the current input.
    history: Vec<f32>,
    /// Position of the next output sample, in upsampled samples, relative to
    /// the first sample in `history`.
    cursor: u64,
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl StreamResampler {
    /// A resampler from `from_hz` to `to_hz`. Equal rates pass samples
    /// through unchanged.
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        let from_hz = from_hz.max(1);
        let to_hz = to_hz.max(1);
        let g = gcd(from_hz, to_hz);
        let up = (to_hz / g) as usize;
        let down = (from_hz / g) as usize;
        if up == down {
            return Self {
                up: 1,
                down: 1,
                bank: Vec::new(),
                history: Vec::new(),
                cursor: 0,
            };
        }
        // Prototype at the upsampled rate (from_hz * up). The cutoff is the
        // lower Nyquist frequency times CUTOFF, normalized to that rate.
        let length = TAPS_PER_PHASE * up;
        let fc = CUTOFF * 0.5 / up.max(down) as f64;
        let centre = (length - 1) as f64 / 2.0;
        let mut prototype = vec![0f64; length];
        for (n, tap) in prototype.iter_mut().enumerate() {
            let x = n as f64 - centre;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let w = n as f64 / (length - 1) as f64;
            let blackman = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * w).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * w).cos();
            // Gain `up`: zero-stuffing divides the signal's energy by `up`.
            *tap = sinc * blackman * up as f64;
        }
        let bank = (0..up)
            .map(|phase| {
                let mut taps = [0f32; TAPS_PER_PHASE];
                for (j, tap) in taps.iter_mut().enumerate() {
                    *tap = prototype[phase + j * up] as f32;
                }
                taps
            })
            .collect();
        Self {
            up,
            down,
            bank,
            history: vec![0.0; TAPS_PER_PHASE - 1],
            cursor: ((TAPS_PER_PHASE - 1) * up) as u64,
        }
    }

    /// Resample the next chunk of the stream.
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if self.up == self.down {
            return input.to_vec();
        }
        self.history.extend(input.iter().map(|&s| f32::from(s)));
        let available = self.history.len() as u64;
        let mut out = Vec::with_capacity(input.len() * self.up / self.down + 1);
        loop {
            // The newest input sample this output depends on.
            let newest = self.cursor / self.up as u64;
            if newest >= available {
                break;
            }
            let phase = (self.cursor % self.up as u64) as usize;
            let taps = &self.bank[phase];
            let newest = newest as usize;
            let mut acc = 0f32;
            for (j, &tap) in taps.iter().enumerate() {
                acc += tap * self.history[newest - j];
            }
            out.push(acc.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16);
            self.cursor += self.down as u64;
        }
        // Keep what the next outputs still need.
        let keep_from = self.history.len() - (TAPS_PER_PHASE - 1);
        self.history.drain(..keep_from);
        self.cursor -= (keep_from * self.up) as u64;
        out
    }

    /// Forget the stream so far, e.g. after playback was flushed.
    pub fn reset(&mut self) {
        if self.up != self.down {
            self.history.clear();
            self.history.resize(TAPS_PER_PHASE - 1, 0.0);
            self.cursor = ((TAPS_PER_PHASE - 1) * self.up) as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, rate: u32, samples: usize, amplitude: f64) -> Vec<i16> {
        (0..samples)
            .map(|n| {
                (amplitude * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate)).sin())
                    as i16
            })
            .collect()
    }

    fn rms(samples: &[i16]) -> f64 {
        // Skip the filter's start-up.
        let s = &samples[samples.len() / 4..];
        (s.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
    }

    #[test]
    fn durations_are_preserved() {
        for (from, to) in [
            (24_000, 8_000),
            (8_000, 16_000),
            (16_000, 24_000),
            (48_000, 16_000),
        ] {
            let mut r = StreamResampler::new(from, to);
            let out = r.process(&vec![0i16; from as usize]); // one second
            let expected = to as usize;
            assert!(
                out.len().abs_diff(expected) <= 16,
                "{from}->{to}: {}",
                out.len()
            );
        }
    }

    #[test]
    fn speech_band_passes_and_aliases_are_removed() {
        // 1 kHz passes 24k -> 8k at unit gain.
        let mut r = StreamResampler::new(24_000, 8_000);
        let pass = r.process(&tone(1_000.0, 24_000, 24_000, 10_000.0));
        let gain = rms(&pass) / (10_000.0 / 2f64.sqrt());
        assert!((gain - 1.0).abs() < 0.05, "pass-band gain {gain}");

        // 7 kHz is above the 4 kHz Nyquist of 8 kHz: without a filter it
        // folds to 1 kHz at full level. It must be suppressed.
        let mut r = StreamResampler::new(24_000, 8_000);
        let alias = r.process(&tone(7_000.0, 24_000, 24_000, 10_000.0));
        assert!(
            rms(&alias) < 10_000.0 / 2f64.sqrt() * 0.01,
            "alias rms {}",
            rms(&alias)
        );

        // The linear interpolator this replaces lets it through.
        let linear =
            super::super::resample(&tone(7_000.0, 24_000, 24_000, 10_000.0), 24_000, 8_000);
        assert!(rms(&linear) > 1_000.0);
    }

    #[test]
    fn chunked_equals_whole() {
        let input = tone(440.0, 8_000, 8_000, 8_000.0);
        let mut whole = StreamResampler::new(8_000, 16_000);
        let expected = whole.process(&input);
        let mut chunked = StreamResampler::new(8_000, 16_000);
        let mut got = Vec::new();
        for chunk in input.chunks(160) {
            got.extend(chunked.process(chunk));
        }
        assert_eq!(got, expected);
    }

    #[test]
    fn equal_rates_pass_through() {
        let mut r = StreamResampler::new(16_000, 16_000);
        assert_eq!(r.process(&[1, 2, 3]), [1, 2, 3]);
    }
}
