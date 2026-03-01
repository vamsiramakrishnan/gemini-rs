//! Barge-in (interruption) detection and handling.
//!
//! Coordinates client-side VAD with jitter buffer flush and server signaling
//! to achieve atomic barge-in with minimal latency.

use crate::buffer::AudioJitterBuffer;
use crate::session::{SessionCommand, SessionPhase};

/// Configuration for barge-in behavior.
#[derive(Debug, Clone)]
pub struct BargeInConfig {
    /// Whether barge-in is enabled.
    pub enabled: bool,
    /// Minimum energy (dBFS above noise floor) to trigger barge-in.
    pub energy_threshold_db: f64,
    /// Minimum duration of speech (in frames) before triggering barge-in.
    pub min_speech_frames: u32,
}

impl Default for BargeInConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            energy_threshold_db: 15.0,
            min_speech_frames: 2,
        }
    }
}

/// Result of a barge-in check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BargeInAction {
    /// No barge-in — continue normal operation.
    None,
    /// Barge-in detected — flush buffer and signal server.
    Interrupt,
}

/// Barge-in detector — checks whether user speech should interrupt model output.
pub struct BargeInDetector {
    config: BargeInConfig,
    /// Count of consecutive speech frames during model output.
    speech_frame_count: u32,
}

impl BargeInDetector {
    /// Create a new barge-in detector.
    pub fn new(config: BargeInConfig) -> Self {
        Self {
            config,
            speech_frame_count: 0,
        }
    }

    /// Check whether a VAD speech detection during model output should trigger barge-in.
    ///
    /// Call this when the VAD detects speech while the session is in `ModelSpeaking` phase.
    ///
    /// Returns `BargeInAction::Interrupt` when speech has been sustained for long enough
    /// to confirm it's real user speech (not a false VAD trigger).
    pub fn check(&mut self, current_phase: SessionPhase, vad_is_speaking: bool) -> BargeInAction {
        if !self.config.enabled {
            return BargeInAction::None;
        }

        // Only trigger barge-in during model output
        if current_phase != SessionPhase::ModelSpeaking {
            self.speech_frame_count = 0;
            return BargeInAction::None;
        }

        if vad_is_speaking {
            self.speech_frame_count += 1;
            if self.speech_frame_count >= self.config.min_speech_frames {
                self.speech_frame_count = 0;
                return BargeInAction::Interrupt;
            }
        } else {
            self.speech_frame_count = 0;
        }

        BargeInAction::None
    }

    /// Reset the detector state.
    pub fn reset(&mut self) {
        self.speech_frame_count = 0;
    }

    /// Execute the barge-in sequence:
    /// 1. Flush the jitter buffer (instant silence)
    /// 2. Return the command to signal activity start
    ///
    /// The caller is responsible for sending the command and transitioning the FSM.
    pub fn execute_barge_in(jitter_buffer: &mut AudioJitterBuffer) -> SessionCommand {
        // Step 1: Instant silence — flush the playback buffer
        jitter_buffer.flush();

        // Step 2: Signal activity start to the server
        SessionCommand::ActivityStart
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_barge_in_when_disabled() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            enabled: false,
            ..Default::default()
        });

        let action = detector.check(SessionPhase::ModelSpeaking, true);
        assert_eq!(action, BargeInAction::None);
    }

    #[test]
    fn no_barge_in_when_not_model_speaking() {
        let mut detector = BargeInDetector::new(BargeInConfig::default());

        let action = detector.check(SessionPhase::Active, true);
        assert_eq!(action, BargeInAction::None);
    }

    #[test]
    fn barge_in_after_min_frames() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 3,
            ..Default::default()
        });

        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::None
        );
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::None
        );
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Interrupt
        );
    }

    #[test]
    fn barge_in_resets_on_silence() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 3,
            ..Default::default()
        });

        detector.check(SessionPhase::ModelSpeaking, true);
        detector.check(SessionPhase::ModelSpeaking, true);
        // Silence interrupts the count
        detector.check(SessionPhase::ModelSpeaking, false);
        // Must start over
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::None
        );
    }
}
