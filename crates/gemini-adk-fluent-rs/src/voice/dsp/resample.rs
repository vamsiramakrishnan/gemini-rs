//! Windowed-sinc resampling as a chain stage (feature `dsp`).
//!
//! The free function [`resample`](crate::voice::resample) is a linear
//! interpolator — deliberately simple, and fine for conversational speech.
//! But linear interpolation is a first-order filter: its stopband barely
//! attenuates, so content above the target Nyquist aliases back into the
//! speech band. A DSP chain deserves the real thing: [`rubato`]'s
//! polyphase windowed-sinc resampler (128-tap sinc, Blackman-Harris
//! window, 0.95 cutoff), with the filter's group delay *declared* through
//! [`DspStage::latency_samples`] instead of ignored.
//!
//! The stage buffers arbitrary input block sizes into fixed 10 ms chunks
//! (rubato wants fixed input), and rewrites `bus.sample_rate` to the
//! target rate — stages after it operate at the new rate.

use std::collections::VecDeque;

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use super::{AudioBus, DspStage};

/// High-quality streaming resampler stage.
pub struct SincResampler {
    inner: SincFixedIn<f32>,
    from_hz: u32,
    to_hz: u32,
    chunk: usize,
    input_fifo: VecDeque<f32>,
    in_scratch: Vec<Vec<f32>>,
    out_scratch: Vec<Vec<f32>>,
    output_fifo: VecDeque<f32>,
    latency_out: usize,
    /// Cumulative input samples accepted (for exact long-term rate).
    in_total: u64,
    /// Cumulative output samples emitted.
    out_total: u64,
}

impl SincResampler {
    /// A mono resampler from `from_hz` to `to_hz` with speech-grade
    /// quality (128-tap sinc, Blackman-Harris, cutoff 0.95).
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        let chunk = (from_hz / 100).max(1) as usize; // 10 ms of input
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let inner =
            SincFixedIn::<f32>::new(f64::from(to_hz) / f64::from(from_hz), 1.0, params, chunk, 1)
                .expect("valid resampler parameters");
        let latency_out = inner.output_delay();
        let in_scratch = vec![vec![0.0f32; chunk]];
        let out_scratch = inner.output_buffer_allocate(true);
        Self {
            inner,
            from_hz,
            to_hz,
            chunk,
            input_fifo: VecDeque::with_capacity(chunk * 4),
            in_scratch,
            out_scratch,
            output_fifo: VecDeque::with_capacity(chunk * 4),
            latency_out,
            in_total: 0,
            out_total: 0,
        }
    }
}

impl DspStage for SincResampler {
    fn name(&self) -> &'static str {
        "resample"
    }

    fn process(&mut self, bus: &mut AudioBus) {
        debug_assert_eq!(
            bus.sample_rate, self.from_hz,
            "SincResampler built for {} Hz fed {} Hz",
            self.from_hz, bus.sample_rate
        );
        let in_len = bus.samples.len();
        self.input_fifo.extend(bus.samples.iter().copied());

        while self.input_fifo.len() >= self.chunk {
            for slot in self.in_scratch[0].iter_mut() {
                *slot = self.input_fifo.pop_front().unwrap_or(0.0);
            }
            let (_, out_len) = self
                .inner
                .process_into_buffer(&self.in_scratch, &mut self.out_scratch, None)
                .expect("fixed-size chunk");
            self.output_fifo
                .extend(self.out_scratch[0][..out_len].iter().copied());
        }

        // Emit the rate-converted share of the CUMULATIVE input, not of
        // this call alone: per-call truncation would permanently drop the
        // fractional remainder (one sample per call at 48k -> 16k never
        // emits anything), drifting the long-term rate and stranding audio
        // in the FIFO. A startup deficit (FIFO shorter than the share)
        // carries forward and is recovered as the resampler fills.
        self.in_total += in_len as u64;
        let due = self.in_total * u64::from(self.to_hz) / u64::from(self.from_hz);
        let want = due.saturating_sub(self.out_total) as usize;
        bus.samples.clear();
        let take = want.min(self.output_fifo.len());
        for _ in 0..take {
            bus.samples
                .push(self.output_fifo.pop_front().unwrap_or(0.0));
        }
        self.out_total += take as u64;
        bus.sample_rate = self.to_hz;
    }

    fn latency_samples(&self) -> usize {
        // Declared at the OUTPUT rate: sinc group delay plus one input chunk
        // of buffering, converted.
        self.latency_out
            + (self.chunk as u64 * u64::from(self.to_hz) / u64::from(self.from_hz)) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, rate: u32, seconds: f32, amp: f32) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    fn run_in_chunks(stage: &mut SincResampler, input: &[f32], from_hz: u32) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        for chunk in input.chunks(173) {
            buf.clear();
            buf.extend_from_slice(chunk);
            let mut bus = AudioBus {
                samples: &mut buf,
                sample_rate: from_hz,
            };
            stage.process(&mut bus);
            out.extend_from_slice(&buf);
        }
        out
    }

    #[test]
    fn preserves_tone_across_48k_to_16k() {
        let mut stage = SincResampler::new(48_000, 16_000);
        let input = sine(1_000.0, 48_000, 1.0, 0.5);
        let out = run_in_chunks(&mut stage, &input, 48_000);
        // Steady state: skip the declared latency, measure RMS.
        let skip = stage.latency_samples() * 2;
        let tail = &out[skip..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        let expected = 0.5 / std::f32::consts::SQRT_2;
        assert!(
            (rms - expected).abs() / expected < 0.05,
            "rms {rms} vs {expected}"
        );
    }

    #[test]
    fn rejects_content_above_target_nyquist() {
        // 12 kHz tone at 48 kHz input is above the 8 kHz Nyquist of a
        // 16 kHz output: a linear interpolator aliases it in loudly; the
        // sinc filter must crush it.
        let mut stage = SincResampler::new(48_000, 16_000);
        let input = sine(12_000.0, 48_000, 1.0, 0.5);
        let out = run_in_chunks(&mut stage, &input, 48_000);
        let skip = stage.latency_samples() * 2;
        let tail = &out[skip..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(rms < 0.01, "alias energy leaked: rms {rms}");
    }

    #[test]
    fn tiny_blocks_still_emit_the_full_rate_share() {
        // One sample per call at 48k -> 16k: per-call truncation computes
        // floor(1/3) = 0 forever and strands every output sample in the
        // FIFO. The cumulative accounting must emit ~1/3 of the input.
        let mut stage = SincResampler::new(48_000, 16_000);
        let mut emitted = 0usize;
        let mut buf = Vec::with_capacity(1);
        for _ in 0..48_000 {
            buf.clear();
            buf.push(0.25f32);
            let mut bus = AudioBus {
                samples: &mut buf,
                sample_rate: 48_000,
            };
            stage.process(&mut bus);
            emitted += buf.len();
        }
        let expected = 16_000usize;
        assert!(
            (emitted as i64 - expected as i64).unsigned_abs() as usize
                <= stage.latency_samples() + 320,
            "emitted {emitted} of ~{expected}"
        );
    }

    #[test]
    fn output_rate_and_length_track_ratio() {
        let mut stage = SincResampler::new(48_000, 16_000);
        let input = vec![0.0f32; 48_000];
        let out = run_in_chunks(&mut stage, &input, 48_000);
        let expected = 16_000usize;
        assert!(
            (out.len() as i64 - expected as i64).unsigned_abs() as usize
                <= stage.latency_samples() + 320,
            "len {} vs ~{expected}",
            out.len()
        );
    }
}
