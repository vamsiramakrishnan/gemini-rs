//! Adaptive jitter buffer for smooth playback of network audio.
//!
//! Network audio arrives in variable-size bursts. The jitter buffer
//! smooths playback by accumulating a configurable minimum depth
//! before starting playback, and adjusting depth dynamically based
//! on measured inter-arrival jitter (EWMA, similar to TCP RTT estimation).

use std::collections::VecDeque;
use std::time::Instant;

/// Configuration for the jitter buffer.
#[derive(Debug, Clone)]
pub struct JitterConfig {
    /// Sample rate in Hz (e.g., 24000 for Gemini output).
    pub sample_rate: u32,
    /// Minimum buffer depth in samples before playback starts.
    pub min_depth_samples: usize,
    /// Maximum buffer depth in samples (overflow drops oldest).
    pub max_depth_samples: usize,
    /// EWMA smoothing factor for jitter estimation (0.0–1.0).
    /// Lower = smoother, higher = more responsive.
    pub jitter_alpha: f64,
    /// Multiplier for jitter estimate to compute adaptive min depth.
    pub target_jitter_multiple: f64,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            sample_rate: 24000,
            min_depth_samples: 24000 / 5, // 200ms at 24kHz
            max_depth_samples: 24000 * 2, // 2 seconds
            jitter_alpha: 0.125,          // RFC 6298 default
            target_jitter_multiple: 2.0,
        }
    }
}

impl JitterConfig {
    /// Create a config for a given sample rate with sensible defaults.
    pub fn for_sample_rate(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            min_depth_samples: sample_rate as usize / 5,
            max_depth_samples: sample_rate as usize * 2,
            ..Default::default()
        }
    }
}

/// Current state of the jitter buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// Accumulating initial depth before playback.
    Filling,
    /// Normal playback — pulling samples for output.
    Playing,
    /// Underrun — generating silence while re-filling.
    Underrun,
}

/// Adaptive jitter buffer for audio playback.
pub struct AudioJitterBuffer {
    config: JitterConfig,
    queue: VecDeque<i16>,
    state: BufferState,
    /// Smoothed jitter estimate in microseconds.
    jitter_estimate_us: f64,
    /// Timestamp of last push.
    last_arrival: Option<Instant>,
    /// Total underrun events.
    underrun_count: u64,
}

impl AudioJitterBuffer {
    /// Create a new jitter buffer with the given configuration.
    pub fn new(config: JitterConfig) -> Self {
        let initial_capacity = config.max_depth_samples;
        Self {
            config,
            queue: VecDeque::with_capacity(initial_capacity),
            state: BufferState::Filling,
            jitter_estimate_us: 0.0,
            last_arrival: None,
            underrun_count: 0,
        }
    }

    /// Current buffer state.
    pub fn state(&self) -> BufferState {
        self.state
    }

    /// Number of samples currently buffered.
    pub fn depth(&self) -> usize {
        self.queue.len()
    }

    /// Depth in milliseconds.
    pub fn depth_ms(&self) -> f64 {
        self.queue.len() as f64 / self.config.sample_rate as f64 * 1000.0
    }

    /// Total underrun events since creation.
    pub fn underrun_count(&self) -> u64 {
        self.underrun_count
    }

    /// Current smoothed jitter estimate in microseconds.
    pub fn jitter_estimate_us(&self) -> f64 {
        self.jitter_estimate_us
    }

    /// Compute the adaptive minimum depth based on measured jitter.
    fn adaptive_min_depth(&self) -> usize {
        let jitter_samples = (self.jitter_estimate_us / 1_000_000.0
            * self.config.sample_rate as f64
            * self.config.target_jitter_multiple) as usize;
        jitter_samples.max(self.config.min_depth_samples)
    }

    /// Push audio samples into the buffer (called when network data arrives).
    pub fn push(&mut self, samples: &[i16]) {
        // Update jitter estimate
        let now = Instant::now();
        if let Some(last) = self.last_arrival {
            let interval_us = now.duration_since(last).as_micros() as f64;
            // EWMA jitter update (RFC 6298 style)
            let deviation = (interval_us - self.jitter_estimate_us).abs();
            self.jitter_estimate_us = self.jitter_estimate_us * (1.0 - self.config.jitter_alpha)
                + deviation * self.config.jitter_alpha;
        }
        self.last_arrival = Some(now);

        // Enforce max depth — drop oldest if overflow
        let total_after = self.queue.len() + samples.len();
        if total_after > self.config.max_depth_samples {
            let to_drop = total_after - self.config.max_depth_samples;
            self.queue.drain(..to_drop.min(self.queue.len()));
        }

        self.queue.extend(samples.iter());

        // State transitions
        if (self.state == BufferState::Filling || self.state == BufferState::Underrun)
            && self.queue.len() >= self.adaptive_min_depth()
        {
            self.state = BufferState::Playing;
        }
    }

    /// Pull audio samples for playback.
    ///
    /// Fills `out` with audio data. If the buffer underruns, fills remaining
    /// slots with silence (zero) for click-free output.
    ///
    /// Returns the number of real (non-silence) samples written.
    pub fn pull(&mut self, out: &mut [i16]) -> usize {
        match self.state {
            BufferState::Filling => {
                // Not ready yet — fill with silence
                out.fill(0);
                0
            }
            BufferState::Playing | BufferState::Underrun => {
                let available = self.queue.len().min(out.len());
                for (i, sample) in self.queue.drain(..available).enumerate() {
                    out[i] = sample;
                }

                // Fill remainder with silence if underrun
                if available < out.len() {
                    out[available..].fill(0);
                    if self.state == BufferState::Playing {
                        self.state = BufferState::Underrun;
                        self.underrun_count += 1;
                    }
                } else if self.state == BufferState::Underrun
                    && self.queue.len() >= self.adaptive_min_depth()
                {
                    self.state = BufferState::Playing;
                }

                available
            }
        }
    }

    /// Flush the buffer immediately (used for barge-in).
    ///
    /// Drops all buffered audio and resets to the Filling state.
    /// This produces instant silence when the user starts speaking.
    pub fn flush(&mut self) {
        self.queue.clear();
        self.state = BufferState::Filling;
        self.last_arrival = None;
    }

    /// Reset the buffer completely, including jitter estimates.
    pub fn reset(&mut self) {
        self.flush();
        self.jitter_estimate_us = 0.0;
        self.underrun_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_buffer() -> AudioJitterBuffer {
        AudioJitterBuffer::new(JitterConfig {
            sample_rate: 16000,
            min_depth_samples: 1600, // 100ms
            max_depth_samples: 16000,
            jitter_alpha: 0.125,
            target_jitter_multiple: 2.0,
        })
    }

    #[test]
    fn starts_in_filling_state() {
        let buf = make_buffer();
        assert_eq!(buf.state(), BufferState::Filling);
        assert_eq!(buf.depth(), 0);
    }

    #[test]
    fn filling_produces_silence() {
        let mut buf = make_buffer();
        buf.push(&vec![42i16; 800]); // < min_depth

        let mut out = [0i16; 160];
        let real = buf.pull(&mut out);
        assert_eq!(real, 0);
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn transitions_to_playing() {
        let mut buf = make_buffer();
        buf.push(&vec![100i16; 1600]); // = min_depth

        assert_eq!(buf.state(), BufferState::Playing);

        let mut out = [0i16; 160];
        let real = buf.pull(&mut out);
        assert_eq!(real, 160);
        assert!(out.iter().all(|&s| s == 100));
    }

    #[test]
    fn underrun_fills_silence() {
        let mut buf = make_buffer();
        buf.push(&vec![99i16; 1600]);
        assert_eq!(buf.state(), BufferState::Playing);

        // Drain most of the buffer
        let mut out = [0i16; 1600];
        buf.pull(&mut out);

        // Now try to pull more — underrun
        let mut out2 = [0i16; 160];
        let real = buf.pull(&mut out2);
        assert_eq!(real, 0);
        assert_eq!(buf.state(), BufferState::Underrun);
        assert_eq!(buf.underrun_count(), 1);
    }

    #[test]
    fn flush_clears_and_resets() {
        let mut buf = make_buffer();
        buf.push(&vec![42i16; 3200]);
        assert_eq!(buf.state(), BufferState::Playing);

        buf.flush();
        assert_eq!(buf.state(), BufferState::Filling);
        assert_eq!(buf.depth(), 0);
    }

    #[test]
    fn overflow_drops_oldest() {
        let mut buf = AudioJitterBuffer::new(JitterConfig {
            sample_rate: 16000,
            min_depth_samples: 100,
            max_depth_samples: 500,
            ..Default::default()
        });

        buf.push(&vec![1i16; 400]);
        buf.push(&vec![2i16; 200]); // total 600 > max 500 → drop 100 oldest

        assert!(buf.depth() <= 500);

        // The oldest samples (1s) were dropped, we should get some 1s then 2s
        let mut out = [0i16; 500];
        buf.pull(&mut out);
        // Last 200 should be 2s
        assert!(out[300..].iter().all(|&s| s == 2));
    }

    #[test]
    fn depth_ms_calculation() {
        let mut buf = make_buffer();
        buf.push(&vec![0i16; 1600]); // 100ms at 16kHz
        assert!((buf.depth_ms() - 100.0).abs() < 0.01);
    }
}
