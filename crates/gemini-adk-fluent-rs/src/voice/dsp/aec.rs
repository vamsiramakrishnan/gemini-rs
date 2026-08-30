//! Acoustic echo cancellation: subtract the bot's own voice from the mic
//! before anything else touches it.
//!
//! # Why this stage exists
//!
//! On an open-speaker device (phone speakerphone, conference room, a laptop
//! with no headset) the bot's own synthesized voice leaves the speaker and
//! re-enters the microphone a few milliseconds to a few hundred milliseconds
//! later, attenuated and reshaped by the room. Under client-interruption
//! authority (the model treats any mic energy as "the user is talking, stop")
//! that echo makes the bot interrupt itself mid-sentence. The fix is not a
//! bigger VAD threshold — it is removing the echo from the signal the VAD
//! (and the ASR, and the model) ever sees.
//!
//! The far-end reference — the audio actually handed to the speaker — is
//! available in this SDK on the playback path. [`Aec::new`] returns an
//! [`AecFarEnd`] handle; feed it the same PCM the speaker plays
//! ([`AecFarEnd::push_pcm16`] / [`AecFarEnd::push_f32`]) and this stage
//! predicts and subtracts the echo before it reaches the rest of the mic
//! chain. **This stage must run before the denoiser** — RNNoise (and any
//! other nonlinear enhancer) rewrites the spectrum in ways that break the
//! linear room-response model the adaptive filter is trying to learn; feed
//! it a denoised signal and it converges on garbage, if it converges at all.
//!
//! # Algorithm: partitioned-block frequency-domain NLMS (overlap-save)
//!
//! This is a linear echo canceller — it models the echo path as an FIR
//! filter of `tail_ms` and adapts that filter with normalized LMS, done in
//! the frequency domain per 10&nbsp;ms block for efficiency (one FFT pair
//! per block handles a filter tail hundreds of taps long). It does **not**
//! handle nonlinear echo (cheap-speaker clipping/distortion) — that needs a
//! nonlinear residual-echo suppressor layered after this stage, out of scope
//! here.
//!
//! - Block size `B` = 10&nbsp;ms of samples (160 at 16&nbsp;kHz). FFT size
//!   `N = 2B` (overlap-save discipline: a linear `B`-tap convolution result
//!   is only valid in the *last* `B` samples of a `2B`-point circular
//!   convolution, so every inverse transform here keeps only that half and
//!   discards the first `B` as circular-wraparound garbage).
//! - The echo path is split into `P = ceil(tail_ms / 10ms)` partitions of
//!   `B` taps each, one weight vector `W_p` (complex, `B+1` one-sided bins)
//!   per partition. Only **one** forward FFT of the far-end is computed per
//!   block; the `P` per-partition spectra are a rolling history of that same
//!   transform (a ring buffer), not `P` separate transforms — this is the
//!   entire point of "partitioned" convolution.
//! - Echo estimate: `Y = Σ_p W_p ⊙ X_p`; the time-domain estimate is the last
//!   `B` samples of `IFFT(Y)`, scaled by `1/N` (`realfft`/`rustfft` transforms
//!   are unnormalized in both directions, so a forward+inverse round trip
//!   scales amplitude by `N` unless corrected — see the source for where
//!   that correction lands).
//! - NLMS update in the frequency domain: `W_p[k] += μ · conj(X_p[k]) ·
//!   E[k] / (Px[k] + ε)`, where `E = FFT([zeros(B), e])` (error zero-padded
//!   at the *front* — the adjoint of "keep only the last `B` samples" used
//!   to form the estimate) and `Px[k]` is an EWMA (0.9 retained / 0.1 new,
//!   the same convention [`DspChain`](super::DspChain) uses for its meters)
//!   of `Σ_p |X_p[k]|²`.
//! - Gradient constraint, applied every block: IFFT each just-updated `W_p`
//!   back to time domain, zero the *last* `B` samples (an unconstrained
//!   frequency-domain update can grow acausal/wraparound content that a
//!   real `B`-tap filter can't have), FFT back. At this block size the cost
//!   (`2P` extra transforms/block) is cheap enough to just always pay it.
//!
//! # Double-talk protection
//!
//! Adapting while the near-end user is also speaking teaches the filter to
//! partially cancel the user, which is exactly backwards. Gating is cheap
//! and Geigel-style: adaptation only runs when the far-end block power
//! exceeds a floor (there is nothing to learn from if the bot is silent)
//! *and* the mic peak for the block does not exceed `0.9 ×` the largest
//! far-end peak seen in the last `P` blocks — a mic level comparable to (or
//! louder than) the recent far-end implies near-end speech is present, since
//! real echo return loss attenuates. A trip freezes adaptation (not echo
//! cancellation — the existing filter keeps subtracting its prediction) for
//! a ~30-block (~300&nbsp;ms) hangover so a single loud consonant doesn't
//! cause the filter to start re-adapting mid-sentence.
//!
//! # Bulk delay
//!
//! `delay_ms` (default 40) compensates the latency between "audio handed to
//! [`AecFarEnd`]" and "that audio arrives at the mic via air/room path" —
//! mostly playback buffering, not room propagation. It is implemented as a
//! FIFO the far-end reference passes through before it ever reaches the
//! filter. **`tail_ms` must cover whatever misalignment remains** after this
//! coarse compensation (clock drift, a `delay_ms` that's an estimate rather
//! than measured, etc.) — the adaptive filter can only pull in echo that
//! falls inside its `P`-partition window relative to the delayed reference;
//! echo arriving *earlier* than the (delayed) reference cannot be modeled by
//! a causal filter at all, and echo arriving later than `tail_ms` past it
//! is simply not learned.
//!
//! # Known failure modes (stated, not hidden)
//!
//! - **Far-end underrun**: if [`AecFarEnd`] hasn't been fed enough audio to
//!   fill the delay line for a given mic block, the missing far-end samples
//!   are treated as silence. No echo is predicted for that block — the raw
//!   (uncancelled) mic audio passes through for whatever portion is missing.
//! - **Startup**: for the first `delay_ms` worth of audio the delay line is
//!   still draining its zero-fill primer, so there is nothing to cancel yet
//!   even if far-end audio is already flowing.
//! - **This stage always adds exactly one block (`B` samples) of latency**,
//!   declared honestly via [`DspStage::latency_samples`] — mic audio is
//!   buffered internally until a full block is available, processed, and
//!   the *previous* block's result is what comes out, so arbitrary input
//!   chunk sizes are supported (a stage caller need not chunk to `B` itself)
//!   at the cost of that fixed one-block delay.
//! - **Mono, single far-end source only.** Stereo far-end / multiple
//!   simultaneous playback streams are out of scope.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use super::{AudioBus, DspStage};

/// Far-end block power below this is "the bot isn't talking" — don't adapt,
/// there is nothing to learn an echo path from.
const FAR_ACTIVE_THRESHOLD: f32 = 1e-6;
/// Geigel double-talk ratio: mic peak vs. the recent far-end peak.
const GEIGEL_RATIO: f32 = 0.9;
/// Blocks to keep adaptation frozen after a double-talk trip (~300 ms at
/// the 10 ms block size).
const HANGOVER_BLOCKS: u32 = 30;
/// NLMS regularization floor — prevents division blow-up when the far-end
/// spectrum is near zero. Negligible next to any real signal's power.
const EPS: f32 = 1e-6;
/// EWMA retain weight for the `Px` power estimate and the ERLE meters —
/// matches the smoothing convention already used by
/// [`DspChain`](super::DspChain)'s stage meters.
const EWMA_RETAIN: f32 = 0.9;
/// How much far-end audio [`AecFarEnd`] buffers before dropping the oldest
/// samples — bounds memory if the far-end producer runs ahead of the mic
/// consumer indefinitely.
const FAR_QUEUE_SECONDS: usize = 2;

/// Tunables for [`Aec::new`]. All three fields have the module's tested
/// defaults via [`Default`].
pub struct AecConfig {
    /// Length of the modeled echo tail, in milliseconds. Rounded up to a
    /// whole number of 10 ms partitions. Must cover the room's actual
    /// reverberant echo plus any residual misalignment left after
    /// `delay_ms` — see the module docs on bulk delay.
    pub tail_ms: u32,
    /// Bulk delay compensation applied to the far-end reference before it
    /// enters the filter, in milliseconds — models playback pipeline
    /// latency between "handed to the speaker" and "captured by the mic".
    pub delay_ms: u32,
    /// NLMS step size. The theoretical bound for a power-normalized
    /// update is `0 < mu < 2`; the practical bound on narrowband bursty
    /// far ends (speech through a speaker) measured far below it:
    /// 0.25+ diverges within seconds, 0.1 reaches a bad marginal
    /// equilibrium (gradient-noise misadjustment above the echo level),
    /// 0.05 holds through every measured stress with ERLE +8..11 dB on
    /// the worst-case harmonic proxy and 12+ dB on broadband far ends.
    /// Default 0.05 — raise it only with `evals/dspbench` watching.
    pub mu: f32,
}

impl Default for AecConfig {
    fn default() -> Self {
        Self {
            tail_ms: 128,
            delay_ms: 40,
            mu: 0.05,
        }
    }
}

/// Handle the playback side feeds with the same audio being sent to the
/// speaker. Cheap to clone (shares one queue via `Arc`); safe to call from
/// a different task/thread than the one driving [`Aec::process`].
#[derive(Clone)]
pub struct AecFarEnd {
    queue: Arc<Mutex<VecDeque<f32>>>,
    max_len: usize,
}

impl AecFarEnd {
    /// Push PCM16 samples (converted to `f32` by `/32768`), oldest dropped
    /// first if the internal buffer is over its bound.
    pub fn push_pcm16(&self, samples: &[i16]) {
        let mut q = self.queue.lock();
        for &s in samples {
            push_bounded(&mut q, self.max_len, f32::from(s) / 32768.0);
        }
    }

    /// Push `f32` samples directly (nominal `[-1.0, 1.0]`), oldest dropped
    /// first if the internal buffer is over its bound.
    pub fn push_f32(&self, samples: &[f32]) {
        let mut q = self.queue.lock();
        for &s in samples {
            push_bounded(&mut q, self.max_len, s);
        }
    }
}

fn push_bounded(q: &mut VecDeque<f32>, max_len: usize, sample: f32) {
    if q.len() >= max_len {
        q.pop_front();
    }
    q.push_back(sample);
}

/// Partitioned-block frequency-domain NLMS acoustic echo canceller. See the
/// module docs for the algorithm and its stated limits.
pub struct Aec {
    sample_rate: u32,
    /// Block size `B`, in samples (10 ms).
    block: usize,
    /// FFT size `N = 2B`.
    fft_len: usize,
    /// Number of `B`-tap partitions covering `tail_ms`.
    partitions: usize,
    mu: f32,

    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
    r2c_scratch: Vec<Complex32>,
    c2r_scratch: Vec<Complex32>,
    /// Reusable time-domain scratch, length `fft_len`. Contents are garbage
    /// between steps within a block — never read without first being
    /// written in the same block.
    time_scratch: Vec<f32>,

    /// Ring buffer of the far-end spectrum, one slot per partition; `head`
    /// names the slot holding the newest (`X_0`) transform.
    x_hist: Vec<Vec<Complex32>>,
    head: usize,
    /// Adaptive filter weights, one vector per logical partition (fixed
    /// indexing — `w[p]` always means "coefficients for lag `p` blocks",
    /// unlike `x_hist` which rotates).
    w: Vec<Vec<Complex32>>,
    /// EWMA of the far-end power spectrum, `Σ_p |X_p[k]|²`.
    px: Vec<f32>,
    /// Previous far-end block, for building the overlap-save `[prev, cur]`
    /// window.
    far_hist_prev: Vec<f32>,
    y_freq: Vec<Complex32>,
    e_freq: Vec<Complex32>,

    mic_block: Vec<f32>,
    far_block: Vec<f32>,
    error: Vec<f32>,

    in_fifo: VecDeque<f32>,
    out_fifo: VecDeque<f32>,
    far_queue: Arc<Mutex<VecDeque<f32>>>,
    far_delay_buf: VecDeque<f32>,

    far_peak_hist: VecDeque<f32>,
    hangover: u32,

    erle_ever_active: bool,
    erle_mic_pow: f32,
    erle_err_pow: f32,
}

impl Aec {
    /// Build the canceller and its paired far-end feed handle. `sample_rate`
    /// must match the [`AudioBus`] this stage will be run on.
    pub fn new(config: AecConfig, sample_rate: u32) -> (Self, AecFarEnd) {
        let block = ((u64::from(sample_rate) * 10 / 1000) as usize).max(1);
        let fft_len = block * 2;
        let bins = block + 1;

        let tail_samples = u64::from(config.tail_ms) * u64::from(sample_rate) / 1000;
        let partitions = ((tail_samples as f64 / block as f64).ceil() as usize).max(1);
        let delay_samples = (u64::from(config.delay_ms) * u64::from(sample_rate) / 1000) as usize;

        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(fft_len);
        let c2r = planner.plan_fft_inverse(fft_len);
        let r2c_scratch = r2c.make_scratch_vec();
        let c2r_scratch = c2r.make_scratch_vec();

        let zero_bins = || vec![Complex32::new(0.0, 0.0); bins];
        let x_hist = (0..partitions).map(|_| zero_bins()).collect();
        let w = (0..partitions).map(|_| zero_bins()).collect();

        let mut out_fifo = VecDeque::with_capacity(block * 2);
        out_fifo.extend(std::iter::repeat_n(0.0f32, block));

        let mut far_delay_buf = VecDeque::with_capacity(delay_samples + block);
        far_delay_buf.extend(std::iter::repeat_n(0.0f32, delay_samples));

        let far_queue = Arc::new(Mutex::new(VecDeque::new()));
        let max_len = sample_rate as usize * FAR_QUEUE_SECONDS;

        let aec = Self {
            sample_rate,
            block,
            fft_len,
            partitions,
            mu: config.mu,
            r2c,
            c2r,
            r2c_scratch,
            c2r_scratch,
            time_scratch: vec![0.0; fft_len],
            x_hist,
            head: 0,
            w,
            px: vec![0.0; bins],
            far_hist_prev: vec![0.0; block],
            y_freq: zero_bins(),
            e_freq: zero_bins(),
            mic_block: Vec::with_capacity(block),
            far_block: Vec::with_capacity(block),
            error: Vec::with_capacity(block),
            in_fifo: VecDeque::new(),
            out_fifo,
            far_queue: Arc::clone(&far_queue),
            far_delay_buf,
            far_peak_hist: VecDeque::with_capacity(partitions),
            hangover: 0,
            erle_ever_active: false,
            erle_mic_pow: 0.0,
            erle_err_pow: 0.0,
        };
        (
            aec,
            AecFarEnd {
                queue: far_queue,
                max_len,
            },
        )
    }

    /// Echo Return Loss Enhancement, in dB: `10·log10(EWMA(mic power) /
    /// EWMA(error power))`, measured only over blocks where the far-end was
    /// active. `0.0` before any far-end activity has ever been observed —
    /// there is nothing yet to report a ratio over.
    pub fn erle_db(&self) -> f32 {
        if !self.erle_ever_active {
            return 0.0;
        }
        10.0 * (self.erle_mic_pow.max(1e-20) / self.erle_err_pow.max(1e-20)).log10()
    }

    fn process_block(&mut self) {
        let block = self.block;
        let n = self.fft_len;
        let r2c = Arc::clone(&self.r2c);
        let c2r = Arc::clone(&self.c2r);

        // 1. Pull one block of mic samples and the delayed far-end reference.
        self.mic_block.clear();
        self.mic_block.extend(self.in_fifo.drain(0..block));

        {
            let mut q = self.far_queue.lock();
            self.far_delay_buf.extend(q.drain(..));
        }
        let avail = self.far_delay_buf.len().min(block);
        self.far_block.clear();
        self.far_block.extend(self.far_delay_buf.drain(0..avail));
        // Underrun: missing far-end samples are silence (no echo predicted
        // for them) — see the module docs' failure-modes section.
        self.far_block.resize(block, 0.0);

        // 2. Overlap-save window [prev B, cur B] -> one forward FFT, stored
        // as the newest ring slot (X_0).
        self.time_scratch[..block].copy_from_slice(&self.far_hist_prev);
        self.time_scratch[block..].copy_from_slice(&self.far_block);
        self.head = (self.head + 1) % self.partitions;
        r2c.process_with_scratch(
            &mut self.time_scratch,
            &mut self.x_hist[self.head],
            &mut self.r2c_scratch,
        )
        .expect("aec: far-end forward fft (fixed sizes)");
        self.far_hist_prev.copy_from_slice(&self.far_block);

        // 3. Echo estimate Y = sum_p W_p * X_p; keep the last B samples of
        // IFFT(Y)/N (overlap-save: the first B are circular garbage).
        for c in &mut self.y_freq {
            *c = Complex32::new(0.0, 0.0);
        }
        for p in 0..self.partitions {
            let idx = (self.head + self.partitions - p) % self.partitions;
            let xp = &self.x_hist[idx];
            let wp = &self.w[p];
            for ((y, w), x) in self.y_freq.iter_mut().zip(wp.iter()).zip(xp.iter()) {
                *y += *w * *x;
            }
        }
        c2r.process_with_scratch(
            &mut self.y_freq,
            &mut self.time_scratch,
            &mut self.c2r_scratch,
        )
        .expect("aec: echo inverse fft (fixed sizes)");
        let norm = 1.0 / n as f32;

        // 4. Error = mic - echo estimate. This is the stage's output.
        let echo_tail = &self.time_scratch[block..];
        self.error.clear();
        self.error.extend(
            self.mic_block
                .iter()
                .zip(echo_tail.iter())
                .map(|(&mic, &y)| mic - y * norm),
        );
        let mic_pow = mean_power(&self.mic_block);
        let err_pow = mean_power(&self.error);

        // 5. Error spectrum for the gradient: E = FFT([zeros(B), e]) — the
        // zero-padding-at-the-front is the adjoint of step 3's "keep only
        // the last B samples".
        self.time_scratch[..block].fill(0.0);
        self.time_scratch[block..].copy_from_slice(&self.error);
        r2c.process_with_scratch(
            &mut self.time_scratch,
            &mut self.e_freq,
            &mut self.r2c_scratch,
        )
        .expect("aec: error forward fft (fixed sizes)");

        // 6. Px EWMA of the far-end power spectrum (NLMS normalizer).
        // Held frozen while the far end is silent: decaying it through
        // inter-burst gaps would make the next burst onset see a fraction
        // of the true power and overshoot the NLMS stability bound —
        // measured as divergence on speech-like (bursty) far ends.
        let far_active = mean_power(&self.far_block) > FAR_ACTIVE_THRESHOLD;
        if far_active {
            for px in &mut self.px {
                *px *= EWMA_RETAIN;
            }
            for p in 0..self.partitions {
                let idx = (self.head + self.partitions - p) % self.partitions;
                let xp = &self.x_hist[idx];
                for (px, x) in self.px.iter_mut().zip(xp.iter()) {
                    *px += (1.0 - EWMA_RETAIN) * x.norm_sqr();
                }
            }
        }

        // 7. Far-end-activity + Geigel double-talk gating.
        let far_peak = self.far_block.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        let mic_peak = self.mic_block.iter().fold(0.0f32, |m, &s| m.max(s.abs()));

        if self.far_peak_hist.len() == self.partitions {
            self.far_peak_hist.pop_front();
        }
        self.far_peak_hist.push_back(far_peak);
        let max_far_peak = self.far_peak_hist.iter().fold(0.0f32, |m, &s| m.max(s));

        let double_talk = far_active && mic_peak > GEIGEL_RATIO * max_far_peak;
        if double_talk {
            self.hangover = HANGOVER_BLOCKS;
        }
        let adapting = far_active && self.hangover == 0;
        if self.hangover > 0 {
            self.hangover -= 1;
        }

        // 8. NLMS update, gated by double-talk/far-end-activity.
        if adapting {
            let mu = self.mu;
            // Leaky NLMS: damp weight components the far end never
            // excites. Narrowband input leaves most bins unexcited; energy
            // accumulating there from error noise is what turned marginal
            // stability into a bad equilibrium (output above mic) on
            // sustained harmonic far ends. ~0.1% decay per 10 ms block is
            // invisible to converged echo paths (they are re-excited every
            // block) and, together with the unexcited-bin gate below, fatal
            // to the parasitic modes. (0.1%/block measured too strong: it
            // capped converged broadband ERLE at ~9 dB.)
            const LEAK: f32 = 0.9999;
            for wp in &mut self.w {
                for w in wp.iter_mut() {
                    *w *= LEAK;
                }
            }
            // Spectral regularization: floor every bin's normalizer at 1%
            // of the mean bin power. In bins where a narrowband far end
            // has no energy, Px is microscopic and the update would divide
            // by EPS alone — the weights there random-walk on error noise
            // until feedback diverges (measured on harmonic far ends).
            let px_mean = self.px.iter().sum::<f32>() / self.px.len().max(1) as f32;
            let px_floor = px_mean * 0.01;
            for p in 0..self.partitions {
                let idx = (self.head + self.partitions - p) % self.partitions;
                let xp = &self.x_hist[idx];
                let wp = &mut self.w[p];
                let updates = wp
                    .iter_mut()
                    .zip(xp.iter())
                    .zip(self.px.iter())
                    .zip(self.e_freq.iter());
                for (((w, x), px), e) in updates {
                    // Bins the far end doesn't excite get NO update at all
                    // (their weights only leak toward zero): with narrowband
                    // input, updating unexcited bins lets error noise
                    // accumulate parasitic weight energy that ends up
                    // louder than the echo it was meant to cancel.
                    if *px < px_floor {
                        continue;
                    }
                    // Floor the normalizer with this bin's instantaneous
                    // power: whatever the EWMA lags, the effective step
                    // stays <= mu, inside the NLMS stability bound.
                    let denom = px.max(x.norm_sqr()) + EPS;
                    let grad = x.conj() * *e;
                    let mut updated = *w + grad * (mu / denom);
                    if !updated.re.is_finite() || !updated.im.is_finite() {
                        updated = Complex32::new(0.0, 0.0);
                    }
                    *w = updated;
                }
            }
        }

        // 9. Gradient constraint (every block): project each partition back
        // onto a causal B-tap filter so an unconstrained frequency-domain
        // step can't grow acausal/wraparound content.
        for p in 0..self.partitions {
            c2r.process_with_scratch(
                &mut self.w[p],
                &mut self.time_scratch,
                &mut self.c2r_scratch,
            )
            .expect("aec: constraint inverse fft (fixed sizes)");
            for s in &mut self.time_scratch {
                *s *= norm;
            }
            for s in &mut self.time_scratch[block..] {
                *s = 0.0;
            }
            r2c.process_with_scratch(
                &mut self.time_scratch,
                &mut self.w[p],
                &mut self.r2c_scratch,
            )
            .expect("aec: constraint forward fft (fixed sizes)");
        }

        // 10. ERLE bookkeeping — only on blocks where cancellation is
        // measurable: far end active AND no double-talk (during double
        // talk the error carries near-end speech by design; feeding those
        // blocks poisons the meter for many seconds afterwards).
        if adapting {
            // Slow meter (~2 s memory): the fast Px constant would let
            // burst onsets and AM valleys drag the reading far below the
            // converged cancellation the waveforms show.
            const ERLE_RETAIN: f32 = 0.995;
            if self.erle_ever_active {
                self.erle_mic_pow = self.erle_mic_pow * ERLE_RETAIN + mic_pow * (1.0 - ERLE_RETAIN);
                self.erle_err_pow = self.erle_err_pow * ERLE_RETAIN + err_pow * (1.0 - ERLE_RETAIN);
            } else {
                self.erle_mic_pow = mic_pow;
                self.erle_err_pow = err_pow;
                self.erle_ever_active = true;
            }
        }
        self.out_fifo.extend(self.error.iter().copied());
    }
}

fn mean_power(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (energy / samples.len() as f64) as f32
}

impl DspStage for Aec {
    fn name(&self) -> &'static str {
        "aec"
    }

    fn process(&mut self, bus: &mut AudioBus) {
        debug_assert_eq!(
            bus.sample_rate, self.sample_rate,
            "Aec was built for a different sample rate than the bus it's running on"
        );
        let want = bus.samples.len();
        self.in_fifo.extend(bus.samples.iter().copied());
        while self.in_fifo.len() >= self.block {
            self.process_block();
        }
        debug_assert!(self.out_fifo.len() >= want, "aec output FIFO underrun");
        bus.samples.clear();
        bus.samples.extend(self.out_fifo.drain(0..want));
    }

    fn latency_samples(&self) -> usize {
        self.block
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stays_stable_on_narrowband_bursty_far_end() {
        // The discriminating input the white-noise tests miss: a harmonic,
        // bursty far end (speech through a speaker). Three separate
        // instabilities were measured here before their fixes: burst-onset
        // step overshoot (decaying Px), empty-bin amplification (Px ~ 0 in
        // bins the tone never touches), and a step size above the
        // practical narrowband bound.
        let sr = 16_000u32;
        let n = 20 * sr as usize;
        let mut far = vec![0.0f32; n];
        let (on, off) = (sr as usize * 16 / 10, sr as usize * 6 / 10);
        let mut i = 0usize;
        let mut f0 = 120.0f32;
        while i < n {
            let end = (i + on).min(n);
            for (k, slot) in far[i..end].iter_mut().enumerate() {
                let t = k as f32 / sr as f32;
                let am = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 4.0 * t).sin();
                let mut v = 0.0;
                for h in 1..=4 {
                    v += (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin() / h as f32;
                }
                *slot = 0.12 * am * v;
            }
            f0 = if f0 > 190.0 { 120.0 } else { f0 + 23.0 };
            i = end + off;
        }
        // Echo path: sparse decaying taps, 40 ms bulk delay.
        let taps: [f32; 6] = [0.12, -0.05, 0.03, -0.015, 0.008, -0.004];
        let delay = 40 * sr as usize / 1000;
        let mut mic = vec![0.0f32; n];
        for (j, m) in mic.iter_mut().enumerate() {
            for (ti, &tap) in taps.iter().enumerate() {
                let src = j as isize - delay as isize - (ti as isize * 37);
                if src >= 0 {
                    *m += tap * far[src as usize];
                }
            }
        }

        let (mut aec, far_end) = Aec::new(
            AecConfig {
                delay_ms: 40,
                ..AecConfig::default()
            },
            sr,
        );
        let mut out = Vec::with_capacity(n);
        for (idx, block) in mic.chunks(320).enumerate() {
            let a = idx * 320;
            far_end.push_f32(&far[a..(a + block.len()).min(n)]);
            let mut buf = block.to_vec();
            let mut bus = AudioBus {
                samples: &mut buf,
                sample_rate: sr,
            };
            aec.process(&mut bus);
            out.extend_from_slice(&buf);
        }

        assert!(out.iter().all(|s| s.is_finite()), "output diverged");
        let tail = &out[n - 2 * sr as usize..];
        let mic_tail = &mic[n - 2 * sr as usize..];
        let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
        assert!(
            rms(tail) < 0.5 * rms(mic_tail),
            "no cancellation: out {} vs mic {}",
            rms(tail),
            rms(mic_tail)
        );
        assert!(aec.erle_db() > 6.0, "erle {} dB", aec.erle_db());
    }

    use super::*;

    /// Fixed-seed xorshift32 — deterministic, no `rand` dependency.
    struct Xorshift32(u32);

    impl Xorshift32 {
        fn new(seed: u32) -> Self {
            Self(seed.max(1))
        }

        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            x
        }

        /// Uniform in `[-1.0, 1.0)`.
        fn next_signed(&mut self) -> f32 {
            (self.next_u32() as f32 / u32::MAX as f32).mul_add(2.0, -1.0)
        }
    }

    const SR: u32 = 16_000;
    const BLOCK: usize = 160;

    /// 48-tap synthetic room response: decaying, sign-alternating every 4
    /// taps (`0.05 * (-0.7)^(i/4)`). Scaled to `0.05` rather than the naive
    /// `0.5` first-tap gain: repeating each decay step over 4 taps before
    /// attenuating means the *sum* of the naive-amplitude taps exceeds unity
    /// gain (a "louder than the far end" echo), which is not physically
    /// realistic (echo return loss attenuates) and — more importantly for
    /// the test — would spuriously trip the Geigel double-talk detector on
    /// pure echo with no near-end speech at all.
    fn rir_taps() -> Vec<f32> {
        (0..48).map(|i: i32| 0.05 * (-0.7f32).powi(i / 4)).collect()
    }

    fn convolve_causal(far: &[f32], h: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; far.len()];
        for (n, out_n) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (i, &hi) in h.iter().enumerate() {
                if i > n {
                    break;
                }
                acc += hi * far[n - i];
            }
            *out_n = acc;
        }
        out
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|&s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Feed one block through the canceller: push the far-end reference,
    /// process the mic block, return the output.
    fn step(aec: &mut Aec, far_end: &AecFarEnd, mic_chunk: &[f32], far_chunk: &[f32]) -> Vec<f32> {
        far_end.push_f32(far_chunk);
        let mut samples = mic_chunk.to_vec();
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: SR,
        };
        aec.process(&mut bus);
        for &s in &samples {
            debug_assert!(!s.is_nan(), "aec produced NaN output");
        }
        samples
    }

    #[test]
    fn converges_on_synthetic_echo() {
        let h = rir_taps();
        let mut far_rng = Xorshift32::new(0x1234_5678);
        // mu = 0.1 (the measured-safe default) converges ~5x slower than
        // the old 0.5 — same ERLE floors, longer runway.
        let total = 24 * SR as usize;
        let far: Vec<f32> = (0..total).map(|_| 0.3 * far_rng.next_signed()).collect();
        let clean = convolve_causal(&far, &h);
        let mut noise_rng = Xorshift32::new(0x9e37_79b9);
        let mic: Vec<f32> = clean
            .iter()
            .map(|&c| c + 1e-4 * noise_rng.next_signed())
            .collect();

        let (mut aec, far_end) = Aec::new(
            AecConfig {
                tail_ms: 128,
                delay_ms: 0,
                mu: 0.05,
            },
            SR,
        );

        let mut output = Vec::with_capacity(total);
        for start in (0..total).step_by(BLOCK) {
            let range = start..start + BLOCK;
            let out = step(&mut aec, &far_end, &mic[range.clone()], &far[range]);
            output.extend(out);
            if start + BLOCK == 16 * SR as usize {
                assert!(aec.erle_db() > 12.0, "erle at 16s = {}", aec.erle_db());
            }
        }

        let last_1s = SR as usize;
        let out_rms = rms(&output[output.len() - last_1s..]);
        let mic_rms = rms(&mic[mic.len() - last_1s..]);
        assert!(
            out_rms < 0.25 * mic_rms,
            "out_rms={out_rms} mic_rms={mic_rms} (want out_rms < 25% of mic_rms)"
        );
    }

    #[test]
    fn no_far_end_is_passthrough() {
        let total = 2 * SR as usize;
        let mut rng = Xorshift32::new(0xdead_beef);
        let mic: Vec<f32> = (0..total)
            .map(|i| {
                let t = i as f32 / SR as f32;
                0.2 * (2.0 * std::f32::consts::PI * 220.0 * t).sin() + 0.05 * rng.next_signed()
            })
            .collect();

        let (mut aec, _far_end) = Aec::new(AecConfig::default(), SR);
        let mut output = Vec::with_capacity(total);
        for start in (0..total).step_by(BLOCK) {
            let mut samples = mic[start..start + BLOCK].to_vec();
            let mut bus = AudioBus {
                samples: &mut samples,
                sample_rate: SR,
            };
            aec.process(&mut bus);
            output.extend(samples);
        }

        let latency = aec.latency_samples();
        assert_eq!(latency, BLOCK);
        assert!(output[..latency].iter().all(|&s| s == 0.0));
        for i in latency..output.len() {
            assert_eq!(output[i], mic[i - latency], "mismatch at sample {i}");
        }
        assert_eq!(aec.erle_db(), 0.0);
    }

    #[test]
    fn double_talk_freezes_adaptation() {
        let h = rir_taps();
        let mut far_rng = Xorshift32::new(0x7777_8888);
        // 14 s to converge (mu = 0.1) + 0.5 s double-talk burst + recovery.
        let total = 20 * SR as usize;
        let far: Vec<f32> = (0..total).map(|_| 0.3 * far_rng.next_signed()).collect();
        let clean = convolve_causal(&far, &h);
        let mut noise_rng = Xorshift32::new(0xaaaa_bbbb);
        let mut mic: Vec<f32> = clean
            .iter()
            .map(|&c| c + 1e-4 * noise_rng.next_signed())
            .collect();

        let burst_start = 14 * SR as usize;
        let burst_len = SR as usize / 2;
        for (i, sample) in mic[burst_start..burst_start + burst_len]
            .iter_mut()
            .enumerate()
        {
            let t = i as f32 / SR as f32;
            *sample += 0.8 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        }

        let (mut aec, far_end) = Aec::new(
            AecConfig {
                tail_ms: 128,
                delay_ms: 0,
                mu: 0.05,
            },
            SR,
        );

        let mut burst_out = Vec::with_capacity(burst_len);
        for start in (0..total).step_by(BLOCK) {
            let range = start..start + BLOCK;
            let out = step(&mut aec, &far_end, &mic[range.clone()], &far[range]);
            if start == burst_start - BLOCK {
                assert!(aec.erle_db() > 12.0, "pre-burst erle = {}", aec.erle_db());
            }
            if start >= burst_start && start < burst_start + burst_len {
                burst_out.extend(out);
            }
        }

        // (a) the near-end burst survives adaptation-frozen cancellation.
        let expected_burst_rms = 0.8 / std::f32::consts::SQRT_2;
        let burst_rms = rms(&burst_out);
        assert!(
            burst_rms > 0.7 * expected_burst_rms,
            "burst_rms={burst_rms} expected>={}",
            0.7 * expected_burst_rms
        );

        // (b) weights survived the burst untouched: ERLE recovers.
        assert!(
            aec.erle_db() > 10.0,
            "post-recovery erle = {}",
            aec.erle_db()
        );
    }

    #[test]
    fn delay_compensation_works() {
        let h = rir_taps();
        let delay_samples = 640; // 40 ms @ 16 kHz
        let mut far_rng = Xorshift32::new(0x2222_3333);
        // mu = 0.1 (the measured-safe default) converges ~5x slower than
        // the old 0.5 — same ERLE floors, longer runway.
        let total = 24 * SR as usize;
        let far: Vec<f32> = (0..total).map(|_| 0.3 * far_rng.next_signed()).collect();

        let mut far_delayed = vec![0.0f32; total];
        far_delayed[delay_samples..].copy_from_slice(&far[..total - delay_samples]);
        let clean = convolve_causal(&far_delayed, &h);
        let mut noise_rng = Xorshift32::new(0x4444_5555);
        let mic: Vec<f32> = clean
            .iter()
            .map(|&c| c + 1e-4 * noise_rng.next_signed())
            .collect();

        let (mut aec, far_end) = Aec::new(
            AecConfig {
                tail_ms: 128,
                delay_ms: 40,
                mu: 0.05,
            },
            SR,
        );

        for start in (0..total).step_by(BLOCK) {
            let range = start..start + BLOCK;
            step(&mut aec, &far_end, &mic[range.clone()], &far[range]);
        }

        assert!(aec.erle_db() > 12.0, "erle = {}", aec.erle_db());
    }

    #[test]
    fn erle_reports_zero_without_activity() {
        let (mut aec, _far_end) = Aec::new(AecConfig::default(), SR);
        assert_eq!(aec.erle_db(), 0.0);

        let mut samples = vec![0.01f32; BLOCK];
        let mut bus = AudioBus {
            samples: &mut samples,
            sample_rate: SR,
        };
        aec.process(&mut bus);
        assert_eq!(aec.erle_db(), 0.0);
    }
}
