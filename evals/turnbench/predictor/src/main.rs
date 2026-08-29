//! TurnBench predictor over the gemini-rs mic chain.
//!
//! Reads two mono PCM16 16 kHz WAVs (one per speaker channel), streams both
//! in lockstep through the shipped chain — optional `voice::Denoiser`, then
//! the L0 `VoiceActivityDetector` — and emits end-of-turn and interruption
//! event timestamps as JSON on stdout.
//!
//! Causality: the chain is strictly causal. Each decision commits at the
//! end of the 30 ms VAD frame that produced it (plus the denoiser's one
//! 10 ms block of buffering, plus any configured EOT hold), and the
//! reported timestamp is that commit time — audio after the timestamp is
//! never consulted.
//!
//! Usage: turnbench-predictor <speaker1.wav> <speaker2.wav>
//! Env:   CHAIN=raw|denoise (default denoise)
//!        VAD=default|noisy_street (default noisy_street)
//!        EOT_HOLD_MS — extra silence after SpeechEnd before committing an
//!        EOT (default 400; resets if speech resumes; the commit time
//!        includes the hold, as deployed).

use gemini_adk_fluent_rs::voice::MicProcessor;
use gemini_genai_rs::vad::{VadConfig, VadEvent, VoiceActivityDetector};

const SR: usize = 16_000;

fn read_wav_pcm16(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).expect("wav file");
    // Minimal RIFF walk: find the `data` chunk; assume PCM16 mono 16 kHz
    // (the driver guarantees this).
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

struct Channel {
    denoiser: Option<gemini_adk_fluent_rs::voice::Denoiser>,
    vad: VoiceActivityDetector,
    speaking: bool,
    /// Pending EOT hold: frames of continued silence still required.
    eot_hold_left: Option<u32>,
    eot: Vec<f64>,
    interruption: Vec<f64>,
    /// Committed speech segments [(start_commit, end_commit)] for offline
    /// operating-point sweeps; the open segment's start while speaking.
    segments: Vec<(f64, f64)>,
    open_start: Option<f64>,
}

impl Channel {
    fn new(denoise: bool, config: VadConfig) -> Self {
        Self {
            denoiser: denoise.then(|| gemini_adk_fluent_rs::voice::Denoiser::new(SR as u32)),
            vad: VoiceActivityDetector::new(config),
            speaking: false,
            eot_hold_left: None,
            eot: Vec::new(),
            interruption: Vec::new(),
            segments: Vec::new(),
            open_start: None,
        }
    }

    /// Feed one VAD frame; returns the event edge this frame produced.
    fn feed(&mut self, frame: &[i16]) -> Option<VadEvent> {
        let mut buf = frame.to_vec();
        if let Some(d) = self.denoiser.as_mut() {
            d.process(&mut buf);
            // The denoiser may return fewer samples than a full VAD frame
            // while its block buffer fills; pad with silence to keep the
            // frame cadence (a fixed, causal alignment cost).
            buf.resize(frame.len(), 0);
        }
        let event = self.vad.process_frame(&buf);
        if let Some(VadEvent::SpeechStart) = event {
            self.speaking = true;
        }
        if let Some(VadEvent::SpeechEnd) = event {
            self.speaking = false;
        }
        event
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (path1, path2) = (&args[1], &args[2]);
    let denoise = std::env::var("CHAIN").as_deref() != Ok("raw");
    let config = match std::env::var("VAD").as_deref() {
        Ok("default") => VadConfig::default(),
        _ => VadConfig::noisy_street(),
    };
    let eot_hold_ms: u64 = std::env::var("EOT_HOLD_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);

    let frame_len = config.frame_size();
    let frame_s = frame_len as f64 / SR as f64;
    let eot_hold_frames = (eot_hold_ms as f64 / 1000.0 / frame_s).ceil() as u32;

    let audio = [read_wav_pcm16(path1), read_wav_pcm16(path2)];
    let mut channels = [
        Channel::new(denoise, config.clone()),
        Channel::new(denoise, config),
    ];

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
                Some(VadEvent::SpeechStart) => {
                    // Taking the floor while the other side holds it.
                    if other_speaking {
                        channels[me].interruption.push(t);
                    }
                    // Speech resumed: cancel any pending EOT.
                    channels[me].eot_hold_left = None;
                    channels[me].open_start = Some(t);
                }
                Some(VadEvent::SpeechEnd) => {
                    channels[me].eot_hold_left = Some(eot_hold_frames);
                    if let Some(start) = channels[me].open_start.take() {
                        channels[me].segments.push((start, t));
                    }
                }
                None => {}
            }
            // Advance a pending EOT hold through continued silence.
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

    // Close any still-open segment at end of audio.
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
