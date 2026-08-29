//! Backend input VAD for browser microphone PCM.

use std::time::Instant;

/// A per-frame processor applied to outgoing microphone audio inside
/// [`LiveHandle::send_audio`](super::LiveHandle::send_audio) — the L1 seam
/// the L2 `voice` chain (denoiser, noise gate) plugs into so hosted
/// surfaces (web bridge, API server) get the same hardened path as native
/// pumps. Runs on the send path: keep it fast and allocation-light.
pub trait InputAudioProcessor: Send {
    /// Process one PCM16 frame in place (the frame may change length).
    fn process_frame(&mut self, frame: &mut Vec<i16>);
}

/// Who decides when user speech interrupts the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityAuthority {
    /// The Live API's automatic activity detection decides (default).
    #[default]
    Server,
    /// This client's input VAD decides: `send_audio` emits
    /// `activityStart`/`activityEnd` marks on speech edges. Only meaningful
    /// when the session was configured with automatic activity detection
    /// disabled — otherwise the server ignores the marks.
    Client,
}

use gemini_genai_rs::prelude::{VadConfig, VadEvent, VoiceActivityDetector};
use gemini_genai_rs::vad::VadState;
use serde::Serialize;

/// Snapshot of backend VAD state for devtools and diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct BackendVadSnapshot {
    /// Active detector backend name.
    pub backend: &'static str,
    /// Input sample rate in Hz.
    pub sample_rate: u32,
    /// Detector frame duration in milliseconds.
    pub frame_duration_ms: u32,
    /// Detector frame size in samples.
    pub frame_size: usize,
    /// Current detector state.
    pub state: &'static str,
    /// Whether the detector is currently in a speech state.
    pub speaking: bool,
    /// Last normalized speech probability or binary decision.
    pub last_probability: Option<f32>,
    /// Number of complete frames processed by the backend detector.
    pub frames_processed: u64,
    /// Milliseconds since the last speech start/end transition.
    pub last_transition_ms_ago: Option<u64>,
}

/// Incremental VAD over arbitrary PCM16 byte chunks.
pub struct BackendInputVad {
    detector: VoiceActivityDetector,
    config: VadConfig,
    pending_samples: Vec<i16>,
    frames_processed: u64,
    last_transition_at: Option<Instant>,
}

impl BackendInputVad {
    /// Create a backend input VAD with explicit detector configuration.
    pub fn new(config: VadConfig) -> Self {
        Self {
            detector: VoiceActivityDetector::new(config.clone()),
            config,
            pending_samples: Vec::new(),
            frames_processed: 0,
            last_transition_at: None,
        }
    }

    /// Process arbitrary little-endian PCM16 bytes and return speech edge events.
    pub fn process_pcm_bytes(&mut self, bytes: &[u8]) -> Vec<VadEvent> {
        self.pending_samples
            .extend(bytes.chunks_exact(2).map(|pair| {
                let raw = [pair[0], pair[1]];
                i16::from_le_bytes(raw)
            }));

        let frame_size = self.config.frame_size();
        if frame_size == 0 {
            return Vec::new();
        }

        let mut events = Vec::new();
        while self.pending_samples.len() >= frame_size {
            let frame: Vec<i16> = self.pending_samples.drain(..frame_size).collect();
            self.frames_processed += 1;
            if let Some(event) = self.detector.process_frame(&frame) {
                self.last_transition_at = Some(Instant::now());
                events.push(event);
            }
        }
        events
    }

    /// The detector's configured input sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Return a diagnostics snapshot suitable for UI/devtools display.
    pub fn snapshot(&self) -> BackendVadSnapshot {
        BackendVadSnapshot {
            backend: self.detector.backend_name(),
            sample_rate: self.config.sample_rate,
            frame_duration_ms: self.config.frame_duration_ms,
            frame_size: self.config.frame_size(),
            state: state_name(self.detector.state()),
            speaking: self.detector.is_speaking(),
            last_probability: self.detector.last_probability(),
            frames_processed: self.frames_processed,
            last_transition_ms_ago: self
                .last_transition_at
                .map(|instant| instant.elapsed().as_millis() as u64),
        }
    }

    #[cfg(test)]
    /// Whether the detector is currently speaking.
    pub fn is_speaking(&self) -> bool {
        self.detector.is_speaking()
    }
}

impl Default for BackendInputVad {
    fn default() -> Self {
        Self::new(VadConfig {
            sample_rate: 16000,
            frame_duration_ms: 30,
            min_speech_frames: 2,
            hangover_frames: 8,
            ..VadConfig::default()
        })
    }
}

fn state_name(state: VadState) -> &'static str {
    match state {
        VadState::Silence => "silence",
        VadState::PendingSpeech => "pending_speech",
        VadState::Speech => "speech",
        VadState::Hangover => "hangover",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech_frame(len: usize, amplitude: i16) -> Vec<i16> {
        (0..len)
            .map(|i| if i % 4 < 2 { amplitude } else { -amplitude })
            .collect()
    }

    fn bytes(samples: &[i16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn buffers_arbitrary_chunks_into_vad_frames() {
        let mut vad = BackendInputVad::new(VadConfig {
            sample_rate: 16000,
            frame_duration_ms: 20,
            min_speech_frames: 2,
            hangover_frames: 2,
            speech_zcr_range: (0.01, 0.9),
            ..VadConfig::default()
        });
        let speech = speech_frame(640, 10000);
        let half = bytes(&speech[..100]);
        assert!(vad.process_pcm_bytes(&half).is_empty());

        let rest = bytes(&speech[100..]);
        let events = vad.process_pcm_bytes(&rest);
        assert!(events.contains(&VadEvent::SpeechStart));
        assert!(vad.is_speaking());
    }
}
