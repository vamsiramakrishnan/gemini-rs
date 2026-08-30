//! TurnBench predictor over the gemini-rs mic chain — pluggable ablation build.
//!
//! Reads two mono PCM16 16 kHz WAVs (one per speaker channel), streams both
//! in lockstep through a configurable mic chain and VAD backend, and emits
//! end-of-turn and interruption event timestamps as JSON on stdout.
//!
//! Causality: everything is strictly causal. Each decision commits at the
//! end of the frame that produced it (plus any hysteresis or configured EOT
//! hold), and the reported timestamp is that commit time — audio after the
//! timestamp is never consulted.
//!
//! Usage: turnbench-predictor <speaker1.wav> <speaker2.wav>
//!
//! Env (the ablation axes):
//!   CHAIN — comma list of stages applied in order, from {hpf, denoise}.
//!     "raw" or empty = none. Legacy alias: "denoise" alone still works.
//!   VAD   — decision backend:
//!     energy   — L0 `VoiceActivityDetector` (30 ms frames; alias:
//!                noisy_street; "default" selects `VadConfig::default()`)
//!     earshot  — pykeio/earshot neural VAD (16 ms frames) with causal
//!                hysteresis: EARSHOT_THRESHOLD (0.5), EARSHOT_START_MS
//!                (48), EARSHOT_END_MS (240)
//!     fusion   — earshot AND energy for onset (both must agree: fewer
//!                false starts), either-silent for offset (30 ms cadence)
//!   EOT_HOLD_MS — extra silence after speech end before committing an EOT
//!     (default 400; resets if speech resumes; commit time includes it).

use gemini_adk_fluent_rs::voice::dsp::{stages::HighPass, AudioBus, DspStage};
use gemini_adk_fluent_rs::voice::{Denoiser, MicProcessor};
use gemini_genai_rs::vad::{VadConfig, VadEvent, VoiceActivityDetector};

const SR: usize = 16_000;
const EARSHOT_FRAME: usize = 256; // 16 ms, fixed by the model

fn read_wav_pcm16(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).expect("wav file");
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            let start = offset + 8;
            let end = (start + size).min(bytes.len());
            return bytes[start..end]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
        }
        offset += 8 + size + (size & 1);
    }
    panic!("no data chunk in {path}");
}

// ── VAD backends ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Start,
    End,
}

/// A pluggable VAD: consumes fixed-length frames, emits speech edges.
/// Implementations must be strictly causal.
trait VadBackend {
    /// Frame length in samples this backend consumes per `process` call.
    fn frame_len(&self) -> usize;
    /// Feed one frame; return the edge this frame committed, if any.
    fn process(&mut self, frame: &[i16]) -> Option<Edge>;
}

/// L0 energy VAD (the SDK's shipped client VAD).
struct EnergyVad(VoiceActivityDetector, usize);

impl EnergyVad {
    fn new(config: VadConfig) -> Self {
        let len = config.frame_size();
        Self(VoiceActivityDetector::new(config), len)
    }
}

impl VadBackend for EnergyVad {
    fn frame_len(&self) -> usize {
        self.1
    }
    fn process(&mut self, frame: &[i16]) -> Option<Edge> {
        match self.0.process_frame(frame) {
            Some(VadEvent::SpeechStart) => Some(Edge::Start),
            Some(VadEvent::SpeechEnd) => Some(Edge::End),
            _ => None,
        }
    }
}

/// pykeio/earshot neural VAD with causal run-length hysteresis: `start_frames`
/// consecutive voiced frames commit a Start, `end_frames` consecutive
/// unvoiced frames commit an End.
struct EarshotVad {
    det: earshot::Detector,
    threshold: f32,
    start_frames: u32,
    end_frames: u32,
    speaking: bool,
    voiced_run: u32,
    unvoiced_run: u32,
}

impl EarshotVad {
    fn from_env() -> Self {
        let ms = |var: &str, def: f32| {
            std::env::var(var)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(def)
        };
        let frame_ms = EARSHOT_FRAME as f32 * 1000.0 / SR as f32;
        Self {
            det: earshot::Detector::const_default(),
            threshold: ms("EARSHOT_THRESHOLD", 0.5),
            start_frames: (ms("EARSHOT_START_MS", 48.0) / frame_ms).round().max(1.0) as u32,
            end_frames: (ms("EARSHOT_END_MS", 240.0) / frame_ms).round().max(1.0) as u32,
            speaking: false,
            voiced_run: 0,
            unvoiced_run: 0,
        }
    }

    fn step(&mut self, voiced: bool) -> Option<Edge> {
        if self.speaking {
            if voiced {
                self.unvoiced_run = 0;
            } else {
                self.unvoiced_run += 1;
                if self.unvoiced_run >= self.end_frames {
                    self.speaking = false;
                    self.voiced_run = 0;
                    return Some(Edge::End);
                }
            }
        } else if voiced {
            self.voiced_run += 1;
            if self.voiced_run >= self.start_frames {
                self.speaking = true;
                self.unvoiced_run = 0;
                return Some(Edge::Start);
            }
        } else {
            self.voiced_run = 0;
        }
        None
    }
}

impl VadBackend for EarshotVad {
    fn frame_len(&self) -> usize {
        EARSHOT_FRAME
    }
    fn process(&mut self, frame: &[i16]) -> Option<Edge> {
        let p = self.det.predict_i16(frame);
        self.step(p >= self.threshold)
    }
}

/// Conservative fusion: onset requires BOTH detectors speaking (fewer false
/// starts), offset fires when EITHER goes silent. Runs at the energy VAD's
/// 30 ms cadence, feeding earshot its 16 ms frames from an internal buffer.
struct FusionVad {
    energy: EnergyVad,
    earshot: EarshotVad,
    energy_speaking: bool,
    combined: bool,
    buf: Vec<i16>,
}

impl FusionVad {
    fn new(config: VadConfig) -> Self {
        Self {
            energy: EnergyVad::new(config),
            earshot: EarshotVad::from_env(),
            energy_speaking: false,
            combined: false,
            buf: Vec::with_capacity(EARSHOT_FRAME * 4),
        }
    }
}

impl VadBackend for FusionVad {
    fn frame_len(&self) -> usize {
        self.energy.frame_len()
    }
    fn process(&mut self, frame: &[i16]) -> Option<Edge> {
        match self.energy.process(frame) {
            Some(Edge::Start) => self.energy_speaking = true,
            Some(Edge::End) => self.energy_speaking = false,
            None => {}
        }
        self.buf.extend_from_slice(frame);
        let mut n = 0;
        while self.buf.len() - n >= EARSHOT_FRAME {
            self.earshot.process(&self.buf[n..n + EARSHOT_FRAME]);
            n += EARSHOT_FRAME;
        }
        self.buf.drain(..n);

        let now = self.energy_speaking && self.earshot.speaking;
        let was = self.combined;
        // Offset when either side went silent; onset only on agreement.
        self.combined = if was {
            self.energy_speaking && self.earshot.speaking
        } else {
            now
        };
        match (was, self.combined) {
            (false, true) => Some(Edge::Start),
            (true, false) => Some(Edge::End),
            _ => None,
        }
    }
}

// ── Mic chain ────────────────────────────────────────────────────────────

/// Ordered, ablatable chain stages applied per VAD frame.
struct Chain {
    hpf: Option<HighPass>,
    denoiser: Option<Denoiser>,
    /// Denoiser output FIFO. RNNoise emits in its own 160-sample (10 ms)
    /// block multiples, so for frame sizes that don't divide its block
    /// (earshot's 256) a call can return more or fewer samples than one
    /// frame. The FIFO is primed once with one block of leading silence —
    /// at least the denoiser's maximum holdback — so it can never
    /// underrun afterwards: a constant causal delay, every sample
    /// preserved in order. Truncating (the old behavior) dropped real
    /// samples whenever a call returned more than one frame.
    dn_fifo: Vec<i16>,
}

/// The denoiser's internal block at 16 kHz: it never holds back more than
/// one block minus one sample, so priming the FIFO with this many zeros
/// guarantees it always covers a full frame.
const DENOISE_BLOCK: usize = 160;

impl Chain {
    fn from_env() -> Self {
        let spec = std::env::var("CHAIN").unwrap_or_default();
        let mut hpf = false;
        let mut denoise = false;
        for stage in spec.split(',').map(|s| s.trim()) {
            match stage {
                "hpf" => hpf = true,
                "denoise" => denoise = true,
                "raw" | "" => {}
                other => panic!("unknown CHAIN stage {other}"),
            }
        }
        Self {
            hpf: hpf.then(|| HighPass::speech(SR as u32)),
            denoiser: denoise.then(|| Denoiser::new(SR as u32)),
            dn_fifo: vec![0i16; DENOISE_BLOCK],
        }
    }

    fn process(&mut self, buf: &mut Vec<i16>) {
        let target = buf.len();
        if let Some(h) = self.hpf.as_mut() {
            let mut f: Vec<f32> = buf.iter().map(|&s| f32::from(s) / 32768.0).collect();
            let mut bus = AudioBus {
                samples: &mut f,
                sample_rate: SR as u32,
            };
            h.process(&mut bus);
            for (dst, s) in buf.iter_mut().zip(&f) {
                *dst = (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            }
        }
        if let Some(d) = self.denoiser.as_mut() {
            d.process(buf);
            self.dn_fifo.append(buf);
            // Re-frame from the FIFO at the caller's cadence. The one-time
            // priming silence means the FIFO always holds a full frame:
            // fifo = prime + emitted - consumed = prime + target - holdback,
            // and holdback < DENOISE_BLOCK <= prime.
            debug_assert!(self.dn_fifo.len() >= target, "denoiser FIFO underrun");
            buf.extend(self.dn_fifo.drain(..target.min(self.dn_fifo.len())));
            buf.resize(target, 0);
        }
    }
}

// ── Per-speaker channel ──────────────────────────────────────────────────

struct Channel {
    chain: Chain,
    vad: Box<dyn VadBackend>,
    speaking: bool,
    eot_hold_left: Option<u32>,
    eot: Vec<f64>,
    interruption: Vec<f64>,
    segments: Vec<(f64, f64)>,
    open_start: Option<f64>,
}

impl Channel {
    fn new() -> Self {
        let config = match std::env::var("VAD").as_deref() {
            Ok("default") => VadConfig::default(),
            _ => VadConfig::noisy_street(),
        };
        let vad: Box<dyn VadBackend> = match std::env::var("VAD").as_deref() {
            Ok("earshot") => Box::new(EarshotVad::from_env()),
            Ok("fusion") => Box::new(FusionVad::new(config)),
            _ => Box::new(EnergyVad::new(config)),
        };
        Self {
            chain: Chain::from_env(),
            vad,
            speaking: false,
            eot_hold_left: None,
            eot: Vec::new(),
            interruption: Vec::new(),
            segments: Vec::new(),
            open_start: None,
        }
    }

    fn feed(&mut self, frame: &[i16]) -> Option<Edge> {
        let mut buf = frame.to_vec();
        self.chain.process(&mut buf);
        let edge = self.vad.process(&buf);
        match edge {
            Some(Edge::Start) => self.speaking = true,
            Some(Edge::End) => self.speaking = false,
            None => {}
        }
        edge
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The denoised stream must stay time-aligned regardless of the
    /// caller's frame size: 256-sample (earshot) framing and 480-sample
    /// (energy) framing must produce nearly the same stream. (Bit
    /// equality is not attainable — the denoiser's internal per-call
    /// resampler has a tiny call-boundary interpolation artifact — but
    /// the old truncate-to-frame behavior DROPPED 64 real samples per
    /// long call under 256-framing, so the two streams drifted apart in
    /// time and diverged completely.)
    #[test]
    fn denoiser_reframing_is_frame_size_invariant() {
        let n = SR; // 1 s
        let input: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                ((t * 220.0 * std::f32::consts::TAU).sin() * 8000.0) as i16
            })
            .collect();

        let run = |frame_len: usize| -> Vec<f64> {
            std::env::set_var("CHAIN", "denoise");
            let mut chain = Chain::from_env();
            let mut out = Vec::with_capacity(n);
            for block in input.chunks(frame_len) {
                let mut buf = block.to_vec();
                chain.process(&mut buf);
                assert_eq!(buf.len(), block.len(), "cadence must be preserved");
                out.extend(buf.iter().map(|&s| f64::from(s)));
            }
            out
        };

        let a = run(256);
        let b = run(480);
        let common = a.len().min(b.len());
        let (mut err, mut energy) = (0.0f64, 0.0f64);
        for i in 0..common {
            err += (a[i] - b[i]).powi(2);
            energy += b[i].powi(2);
        }
        let rel = (err / energy.max(1.0)).sqrt();
        assert!(
            rel < 0.05,
            "streams diverged (relative RMS diff {rel:.3}) — framing dropped or shifted samples"
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (path1, path2) = (&args[1], &args[2]);
    // Legacy alias: CHAIN=denoise meant "denoise on" before stages existed.
    if std::env::var("CHAIN").is_err() {
        std::env::set_var("CHAIN", "denoise");
    }
    let eot_hold_ms: u64 = std::env::var("EOT_HOLD_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);

    let audio = [read_wav_pcm16(path1), read_wav_pcm16(path2)];
    let mut channels = [Channel::new(), Channel::new()];

    let frame_len = channels[0].vad.frame_len();
    let frame_s = frame_len as f64 / SR as f64;
    let eot_hold_frames = (eot_hold_ms as f64 / 1000.0 / frame_s).ceil() as u32;

    let frames = audio[0].len().min(audio[1].len()) / frame_len;
    for i in 0..frames {
        // Commit time: the end of the frame that revealed the decision.
        let t = (i + 1) as f64 * frame_s;
        let span = i * frame_len..(i + 1) * frame_len;
        let events = [
            channels[0].feed(&audio[0][span.clone()]),
            channels[1].feed(&audio[1][span]),
        ];
        for me in 0..2 {
            let other_speaking = channels[1 - me].speaking;
            match events[me] {
                Some(Edge::Start) => {
                    if other_speaking {
                        channels[me].interruption.push(t);
                    }
                    channels[me].eot_hold_left = None;
                    channels[me].open_start = Some(t);
                }
                Some(Edge::End) => {
                    channels[me].eot_hold_left = Some(eot_hold_frames);
                    if let Some(start) = channels[me].open_start.take() {
                        channels[me].segments.push((start, t));
                    }
                }
                None => {}
            }
            if !channels[me].speaking {
                if let Some(left) = channels[me].eot_hold_left {
                    if left == 0 {
                        channels[me].eot.push(t);
                        channels[me].eot_hold_left = None;
                    } else {
                        channels[me].eot_hold_left = Some(left - 1);
                    }
                }
            }
        }
    }

    let end_t = frames as f64 * frame_s;
    for ch in channels.iter_mut() {
        if let Some(start) = ch.open_start.take() {
            ch.segments.push((start, end_t));
        }
    }

    let out = serde_json::json!({
        "speaker_1": {"eot": channels[0].eot, "interruption": channels[0].interruption,
                       "segments": channels[0].segments},
        "speaker_2": {"eot": channels[1].eot, "interruption": channels[1].interruption,
                       "segments": channels[1].segments},
    });
    println!("{out}");
}
