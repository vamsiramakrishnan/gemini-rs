//! DSP foundation for the mic chain: a float audio bus, a stage contract,
//! and a metered chain runner.
//!
//! The original mic chain passed `Vec<i16>` from stage to stage — every hop
//! re-quantized (adding noise floor) and every boundary was a hidden clip
//! point. This module is the engineer's version: samples are converted to
//! `f32` **once** on entry, every stage processes in float with headroom,
//! and one saturating conversion happens at the exit — where clipping is
//! *counted*, not silent.
//!
//! ```ignore
//! // `ignore`: the resampler/STFT stages need the `dsp` feature and the
//! // denoiser the `denoise` feature; `live` is a `Live` builder.
//! let chain = DspChain::new(16_000)
//!     .stage(HighPass::speech_default(16_000))   // DC / rumble removal
//!     .stage(IntStage::new(Denoiser::new(16_000))) // legacy i16 stage, one boundary
//!     .stage(Agc::default_speech())
//!     .stage(Limiter::default_ceiling());
//! let metrics = chain.metrics();                  // live per-stage meters
//! live.mic_processor(chain);                      // drop into the existing seam
//! ```
//!
//! # Design rules
//!
//! - **Allocation-free steady state**: scratch buffers are owned by the
//!   chain and stages; the hot path only does arithmetic. (A stage may
//!   resize its output — resamplers legitimately change length — but must
//!   not allocate per call once warmed.)
//! - **Uniform measurement**: the chain, not the stages, meters peak/RMS
//!   in and out of every stage plus exit clipping, so every stage is
//!   observed identically and stages stay pure.
//! - **Latency is declared**: every stage reports its group delay via
//!   [`DspStage::latency_samples`]; [`DspChain::total_latency_samples`]
//!   sums the chain's causal budget so turn-commit timestamps can cite it.
//!
//! # Canonical stage order
//!
//! `HPF → AEC → denoise → AGC → gate → limiter` — each stage assumes what
//! the previous one guarantees: echo cancellation needs the *linear* signal
//! (before the nonlinear denoiser breaks the echo-path model), gain control
//! wants denoised speech so it does not amplify noise, and the limiter is
//! last so nothing after it can clip.

#[cfg(feature = "dsp")]
pub mod aec;
#[cfg(feature = "dsp")]
pub mod resample;
pub mod stages;
#[cfg(feature = "dsp")]
pub mod stft;

#[cfg(feature = "dsp")]
pub use aec::{Aec, AecConfig, AecFarEnd};
#[cfg(feature = "dsp")]
pub use resample::SincResampler;
pub use stages::{Agc, HighPass, Limiter};
#[cfg(feature = "dsp")]
pub use stft::{Identity, SpectralFloor, SpectralStage, Stft};

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use serde::Serialize;

use gemini_adk_rs::live::InputAudioProcessor;

/// One block of audio moving through the chain: `f32` samples in
/// `[-1.0, 1.0]` nominal range (headroom above is legal between stages)
/// plus the sample rate a stage may change (resamplers).
pub struct AudioBus<'a> {
    /// The samples. Stages mutate in place and may change the length.
    pub samples: &'a mut Vec<f32>,
    /// Sample rate of `samples` in Hz.
    pub sample_rate: u32,
}

/// A single processing stage on the float bus.
pub trait DspStage: Send {
    /// Short stable name, shown in metrics snapshots.
    fn name(&self) -> &'static str;
    /// Process one block in place.
    fn process(&mut self, bus: &mut AudioBus);
    /// Group delay this stage introduces, in samples at the bus rate
    /// (lookahead, filter delay, block buffering). Default 0.
    fn latency_samples(&self) -> usize {
        0
    }
}

/// Wrap a legacy integer-domain [`InputAudioProcessor`] (e.g. the RNNoise
/// [`Denoiser`](crate::voice::Denoiser) or [`NoiseGate`](crate::voice::NoiseGate))
/// as a [`DspStage`]. This is the *one* deliberate int boundary in a float
/// chain — the cost of reusing a proven stage unchanged.
pub struct IntStage<P: InputAudioProcessor> {
    inner: P,
    name: &'static str,
    scratch: Vec<i16>,
    latency: usize,
}

impl<P: InputAudioProcessor> IntStage<P> {
    /// Wrap `inner`, reported under `name` in metrics.
    pub fn named(inner: P, name: &'static str) -> Self {
        Self {
            inner,
            name,
            scratch: Vec::new(),
            latency: 0,
        }
    }

    /// Declare the wrapped processor's internal buffering (it cannot
    /// declare it itself — the integer trait has no latency contract).
    /// E.g. the RNNoise denoiser buffers one 10 ms block: 160 samples.
    pub fn with_latency(mut self, samples: usize) -> Self {
        self.latency = samples;
        self
    }
}

impl<P: InputAudioProcessor> DspStage for IntStage<P> {
    fn name(&self) -> &'static str {
        self.name
    }

    fn latency_samples(&self) -> usize {
        self.latency
    }

    fn process(&mut self, bus: &mut AudioBus) {
        self.scratch.clear();
        self.scratch.extend(
            bus.samples
                .iter()
                .map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16),
        );
        self.inner.process_frame(&mut self.scratch);
        bus.samples.clear();
        bus.samples
            .extend(self.scratch.iter().map(|&s| f32::from(s) / 32768.0));
    }
}

/// Live meters for one stage, updated per block, readable concurrently.
#[derive(Default)]
struct StageMeter {
    /// Max |sample| seen at stage output since start (f32 bits).
    peak_out: AtomicU32,
    /// EWMA of block RMS at stage output (f32 bits; single writer).
    rms_out: AtomicU32,
    /// Blocks processed.
    blocks: AtomicU64,
}

/// Point-in-time view of one stage's meters.
#[derive(Debug, Clone, Serialize)]
pub struct StageSnapshot {
    /// Stage name.
    pub name: &'static str,
    /// Max |sample| at the stage output since start.
    pub peak_out: f32,
    /// Smoothed RMS at the stage output.
    pub rms_out: f32,
    /// Declared group delay in samples.
    pub latency_samples: usize,
}

/// Point-in-time view of the whole chain.
#[derive(Debug, Clone, Serialize)]
pub struct ChainSnapshot {
    /// Per-stage meters, in chain order.
    pub stages: Vec<StageSnapshot>,
    /// Samples clipped at the exit conversion since start.
    pub exit_clipped: u64,
    /// Total declared group delay in samples at the bus rate.
    pub total_latency_samples: usize,
    /// Blocks processed.
    pub blocks: u64,
}

/// Shared metrics handle — clone freely; reading never blocks the chain.
#[derive(Clone)]
pub struct ChainMetrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    meters: Vec<StageMeter>,
    names: Vec<&'static str>,
    latencies: Vec<usize>,
    exit_clipped: AtomicU64,
    blocks: AtomicU64,
}

impl ChainMetrics {
    /// A point-in-time snapshot of every stage's meters.
    pub fn snapshot(&self) -> ChainSnapshot {
        let stages = self
            .inner
            .meters
            .iter()
            .zip(&self.inner.names)
            .zip(&self.inner.latencies)
            .map(|((meter, name), &latency)| StageSnapshot {
                name,
                peak_out: f32::from_bits(meter.peak_out.load(Ordering::Relaxed)),
                rms_out: f32::from_bits(meter.rms_out.load(Ordering::Relaxed)),
                latency_samples: latency,
            })
            .collect();
        ChainSnapshot {
            stages,
            exit_clipped: self.inner.exit_clipped.load(Ordering::Relaxed),
            total_latency_samples: self.inner.latencies.iter().sum(),
            blocks: self.inner.blocks.load(Ordering::Relaxed),
        }
    }
}

/// The metered float chain. Build with [`stage`](Self::stage), hand to the
/// existing `mic_processor(..)` seam — it implements [`InputAudioProcessor`].
pub struct DspChain {
    stages: Vec<Box<dyn DspStage>>,
    sample_rate: u32,
    bus: Vec<f32>,
    metrics: Option<ChainMetrics>,
}

impl DspChain {
    /// An empty chain at `sample_rate` (an empty chain is bit-transparent).
    pub fn new(sample_rate: u32) -> Self {
        Self {
            stages: Vec::new(),
            sample_rate,
            bus: Vec::new(),
            metrics: None,
        }
    }

    /// Append a stage (chain order is processing order).
    pub fn stage(mut self, stage: impl DspStage + 'static) -> Self {
        self.stages.push(Box::new(stage));
        self.metrics = None; // rebuilt lazily to match the stage list
        self
    }

    /// Shared live-metrics handle for this chain.
    pub fn metrics(&mut self) -> ChainMetrics {
        self.ensure_metrics();
        self.metrics.as_ref().expect("just built").clone()
    }

    /// Total declared group delay across all stages, in samples.
    pub fn total_latency_samples(&self) -> usize {
        self.stages.iter().map(|s| s.latency_samples()).sum()
    }

    fn ensure_metrics(&mut self) {
        if self.metrics.is_none() {
            self.metrics = Some(ChainMetrics {
                inner: Arc::new(MetricsInner {
                    meters: self.stages.iter().map(|_| StageMeter::default()).collect(),
                    names: self.stages.iter().map(|s| s.name()).collect(),
                    latencies: self.stages.iter().map(|s| s.latency_samples()).collect(),
                    exit_clipped: AtomicU64::new(0),
                    blocks: AtomicU64::new(0),
                }),
            });
        }
    }
}

impl InputAudioProcessor for DspChain {
    fn process_frame(&mut self, frame: &mut Vec<i16>) {
        self.ensure_metrics();
        // Entry: one int -> float conversion.
        self.bus.clear();
        self.bus
            .extend(frame.iter().map(|&s| f32::from(s) / 32768.0));

        let metrics = self.metrics.as_ref().expect("ensured").inner.clone();
        // Rate changes (a resampler mid-chain) propagate stage-to-stage
        // WITHIN this frame only; the chain's own input rate is fixed, so
        // the next frame's fresh PCM is labeled correctly again.
        let mut rate = self.sample_rate;
        for (stage, meter) in self.stages.iter_mut().zip(&metrics.meters) {
            let mut bus = AudioBus {
                samples: &mut self.bus,
                sample_rate: rate,
            };
            stage.process(&mut bus);
            rate = bus.sample_rate;

            // Uniform metering at the stage output.
            let mut peak = 0.0f32;
            let mut energy = 0.0f64;
            for &s in self.bus.iter() {
                peak = peak.max(s.abs());
                energy += f64::from(s) * f64::from(s);
            }
            let rms = if self.bus.is_empty() {
                0.0
            } else {
                (energy / self.bus.len() as f64).sqrt() as f32
            };
            meter.peak_out.fetch_max(peak.to_bits(), Ordering::Relaxed);
            let prev = f32::from_bits(meter.rms_out.load(Ordering::Relaxed));
            let ewma = if meter.blocks.load(Ordering::Relaxed) == 0 {
                rms
            } else {
                prev * 0.9 + rms * 0.1
            };
            meter.rms_out.store(ewma.to_bits(), Ordering::Relaxed);
            meter.blocks.fetch_add(1, Ordering::Relaxed);
        }

        // Exit: one saturating float -> int conversion, clipping counted.
        let mut clipped = 0u64;
        frame.clear();
        frame.extend(self.bus.iter().map(|&s| {
            let scaled = (s * 32768.0).round();
            if !(-32768.0..=32767.0).contains(&scaled) {
                clipped += 1;
            }
            scaled.clamp(-32768.0, 32767.0) as i16
        }));
        if clipped > 0 {
            metrics.exit_clipped.fetch_add(clipped, Ordering::Relaxed);
        }
        metrics.blocks.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Gain(f32);
    impl DspStage for Gain {
        fn name(&self) -> &'static str {
            "gain"
        }
        fn process(&mut self, bus: &mut AudioBus) {
            for s in bus.samples.iter_mut() {
                *s *= self.0;
            }
        }
    }

    #[cfg(feature = "dsp")]
    #[test]
    fn rate_change_does_not_persist_across_frames() {
        // A mid-chain resampler rewrites the bus rate for LATER stages in
        // the SAME frame only. The next frame's fresh input PCM arrives at
        // the chain's input rate again — persisting the output rate fed
        // 16 kHz-labeled audio to a resampler built for 48 kHz (debug
        // panic; silent rate corruption in release).
        let mut chain = DspChain::new(48_000).stage(
            crate::voice::dsp::resample::SincResampler::new(48_000, 16_000),
        );
        let mut frame = vec![1000i16; 960]; // 20 ms at 48 kHz
        chain.process_frame(&mut frame);
        let mut frame2 = vec![1000i16; 960];
        chain.process_frame(&mut frame2); // panicked before the fix
    }

    #[test]
    fn empty_chain_is_bit_transparent() {
        let mut chain = DspChain::new(16_000);
        let original: Vec<i16> = (-40..40).map(|i| (i * 400) as i16).collect();
        let mut frame = original.clone();
        chain.process_frame(&mut frame);
        assert_eq!(frame, original);
    }

    #[test]
    fn exit_clipping_is_counted_not_silent() {
        let mut chain = DspChain::new(16_000).stage(Gain(4.0));
        let metrics = chain.metrics();
        let mut frame = vec![20_000i16; 160];
        chain.process_frame(&mut frame);
        assert!(frame.iter().all(|&s| s == i16::MAX || s == i16::MIN));
        let snap = metrics.snapshot();
        assert_eq!(snap.exit_clipped, 160);
        assert!(snap.stages[0].peak_out > 2.0);
    }

    #[test]
    fn int_adapter_round_trips_a_passthrough() {
        struct Nop;
        impl InputAudioProcessor for Nop {
            fn process_frame(&mut self, _frame: &mut Vec<i16>) {}
        }
        let mut chain = DspChain::new(16_000).stage(IntStage::named(Nop, "nop"));
        let original: Vec<i16> = (0..160).map(|i| (i * 100 - 8000) as i16).collect();
        let mut frame = original.clone();
        chain.process_frame(&mut frame);
        // Symmetric 1/32768 scaling makes the boundary exact for i16 values.
        assert_eq!(frame, original);
    }

    #[test]
    fn metrics_report_stage_names_and_latency() {
        struct Delayed;
        impl DspStage for Delayed {
            fn name(&self) -> &'static str {
                "delayed"
            }
            fn process(&mut self, _bus: &mut AudioBus) {}
            fn latency_samples(&self) -> usize {
                80
            }
        }
        let mut chain = DspChain::new(16_000).stage(Delayed).stage(Gain(1.0));
        let snap = chain.metrics().snapshot();
        assert_eq!(snap.stages.len(), 2);
        assert_eq!(snap.stages[0].name, "delayed");
        assert_eq!(snap.total_latency_samples, 80);
        assert_eq!(chain.total_latency_samples(), 80);
    }
}
