//! Barge-in (interruption) detection and handling.
//!
//! Coordinates client-side VAD with jitter buffer flush and server signaling
//! to achieve atomic barge-in with minimal latency.
//!
//! When `tentative` mode is enabled (the default), the detector follows a
//! three-step duck-confirm-flush sequence:
//!
//! 1. **Duck** — On the first speech frame during `ModelSpeaking`, reduce
//!    playback volume instead of immediately silencing. This avoids jarring
//!    silence from false-positive VAD triggers (e.g., background noise).
//! 2. **Interrupt** — Once speech has been sustained for `min_speech_frames`,
//!    flush the jitter buffer and signal the server.
//! 3. **Restore** — If speech stops before reaching the confirmation threshold,
//!    restore the original playback volume (false positive resolved).

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
    /// Enable tentative barge-in (duck before flush).
    pub tentative: bool,
    /// Volume multiplier during duck phase (0.0-1.0).
    pub duck_volume: f32,
}

impl Default for BargeInConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            energy_threshold_db: 15.0,
            min_speech_frames: 2,
            tentative: true,
            duck_volume: 0.3,
        }
    }
}

/// Result of a barge-in check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BargeInAction {
    /// No barge-in — continue normal operation.
    None,
    /// Duck audio volume — tentative barge-in detected.
    /// The `f32` is the volume multiplier (0.0 = silent, 1.0 = full).
    Duck(f32),
    /// Barge-in detected — flush buffer and signal server.
    Interrupt,
    /// Restore audio volume — false positive resolved.
    Restore,
}

/// Internal state of the tentative barge-in detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectorState {
    /// No tentative barge-in in progress.
    Idle,
    /// Audio has been ducked; waiting for confirmation or silence.
    Ducked { frames: u32 },
}

/// Barge-in detector — checks whether user speech should interrupt model output.
pub struct BargeInDetector {
    config: BargeInConfig,
    /// Count of consecutive speech frames during model output.
    speech_frame_count: u32,
    /// Internal state for tentative barge-in.
    state: DetectorState,
}

impl BargeInDetector {
    /// Create a new barge-in detector.
    pub fn new(config: BargeInConfig) -> Self {
        Self {
            config,
            speech_frame_count: 0,
            state: DetectorState::Idle,
        }
    }

    /// Check whether a VAD speech detection during model output should trigger barge-in.
    ///
    /// Call this when the VAD detects speech while the session is in `ModelSpeaking` phase.
    ///
    /// When tentative mode is enabled, the sequence is:
    /// - First speech frame → `Duck(volume)` (reduce playback volume)
    /// - Sustained speech reaching `min_speech_frames` → `Interrupt` (flush and signal)
    /// - Silence before confirmation → `Restore` (false positive)
    ///
    /// When tentative mode is disabled, the legacy behavior applies:
    /// - `None` until `min_speech_frames` consecutive frames → `Interrupt`
    pub fn check(&mut self, current_phase: SessionPhase, vad_is_speaking: bool) -> BargeInAction {
        if !self.config.enabled {
            return BargeInAction::None;
        }

        // Only trigger barge-in during model output
        if current_phase != SessionPhase::ModelSpeaking {
            let action = self.restore_if_ducked();
            self.speech_frame_count = 0;
            self.state = DetectorState::Idle;
            return action;
        }

        if self.config.tentative {
            self.check_tentative(vad_is_speaking)
        } else {
            self.check_legacy(vad_is_speaking)
        }
    }

    /// Legacy barge-in check: None until min_speech_frames consecutive frames → Interrupt.
    fn check_legacy(&mut self, vad_is_speaking: bool) -> BargeInAction {
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

    /// Tentative barge-in check: Duck → Interrupt or Restore.
    fn check_tentative(&mut self, vad_is_speaking: bool) -> BargeInAction {
        match self.state {
            DetectorState::Idle => {
                if vad_is_speaking {
                    // First speech frame — duck audio and start counting.
                    // We count this frame as frame 1.
                    self.state = DetectorState::Ducked { frames: 1 };
                    // Check if min_speech_frames == 1 (immediate interrupt).
                    if self.config.min_speech_frames <= 1 {
                        self.state = DetectorState::Idle;
                        return BargeInAction::Interrupt;
                    }
                    BargeInAction::Duck(self.config.duck_volume)
                } else {
                    BargeInAction::None
                }
            }
            DetectorState::Ducked { frames } => {
                if vad_is_speaking {
                    let new_frames = frames + 1;
                    if new_frames >= self.config.min_speech_frames {
                        // Confirmed speech — full interrupt.
                        self.state = DetectorState::Idle;
                        BargeInAction::Interrupt
                    } else {
                        self.state = DetectorState::Ducked { frames: new_frames };
                        // Already ducked, no new action needed.
                        BargeInAction::None
                    }
                } else {
                    // Silence while ducked — false positive, restore volume.
                    self.state = DetectorState::Idle;
                    BargeInAction::Restore
                }
            }
        }
    }

    /// If currently ducked, return Restore; otherwise None.
    fn restore_if_ducked(&self) -> BargeInAction {
        match self.state {
            DetectorState::Ducked { .. } => BargeInAction::Restore,
            DetectorState::Idle => BargeInAction::None,
        }
    }

    /// Reset the detector state.
    pub fn reset(&mut self) {
        self.speech_frame_count = 0;
        self.state = DetectorState::Idle;
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
            tentative: false,
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
            tentative: false,
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

    #[test]
    fn tentative_barge_in_duck_then_interrupt() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 3,
            tentative: true,
            duck_volume: 0.3,
            ..Default::default()
        });

        // First speech frame → Duck
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Duck(0.3)
        );
        // Second speech frame → still ducked, no new action
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::None
        );
        // Third speech frame → confirmed, Interrupt
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Interrupt
        );
    }

    #[test]
    fn tentative_barge_in_duck_then_restore() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 3,
            tentative: true,
            duck_volume: 0.3,
            ..Default::default()
        });

        // First speech frame → Duck
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Duck(0.3)
        );
        // Silence before confirmation → Restore
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, false),
            BargeInAction::Restore
        );
        // Back to idle — silence does nothing
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, false),
            BargeInAction::None
        );
    }

    #[test]
    fn tentative_disabled_skips_duck() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 3,
            tentative: false,
            duck_volume: 0.3,
            ..Default::default()
        });

        // Without tentative, first frames return None (no Duck).
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::None
        );
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::None
        );
        // Reaching min_speech_frames → Interrupt directly (no Duck).
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Interrupt
        );
    }

    #[test]
    fn duck_volume_in_action() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 5,
            tentative: true,
            duck_volume: 0.5,
            ..Default::default()
        });

        let action = detector.check(SessionPhase::ModelSpeaking, true);
        assert_eq!(action, BargeInAction::Duck(0.5));
    }

    #[test]
    fn tentative_restores_on_phase_change() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 5,
            tentative: true,
            duck_volume: 0.3,
            ..Default::default()
        });

        // Start ducking
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Duck(0.3)
        );
        // Phase changes away from ModelSpeaking while ducked → Restore
        assert_eq!(
            detector.check(SessionPhase::Active, true),
            BargeInAction::Restore
        );
    }

    #[test]
    fn tentative_immediate_interrupt_when_min_frames_one() {
        let mut detector = BargeInDetector::new(BargeInConfig {
            min_speech_frames: 1,
            tentative: true,
            duck_volume: 0.3,
            ..Default::default()
        });

        // With min_speech_frames=1, even in tentative mode we go straight to Interrupt
        assert_eq!(
            detector.check(SessionPhase::ModelSpeaking, true),
            BargeInAction::Interrupt
        );
    }
}
