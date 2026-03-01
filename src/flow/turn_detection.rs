//! Client-side turn detection — complements Gemini's server-side VAD.
//!
//! Provides configurable end-of-speech detection to signal `activityEnd`,
//! allowing the server to start model generation faster.

use std::time::{Duration, Instant};

/// Configuration for client-side turn detection.
#[derive(Debug, Clone)]
pub struct TurnDetectionConfig {
    /// Delay after speech ends before signaling end-of-turn (ms).
    pub end_of_speech_delay_ms: u64,
    /// Whether client-side turn detection is enabled.
    /// When disabled, we rely entirely on server-side VAD.
    pub enabled: bool,
}

impl Default for TurnDetectionConfig {
    fn default() -> Self {
        Self {
            end_of_speech_delay_ms: 300,
            enabled: true,
        }
    }
}

/// Events from the turn detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDetectionEvent {
    /// User started speaking.
    SpeechStarted,
    /// User finished speaking (end-of-speech delay elapsed).
    TurnEnded,
}

/// Client-side turn detector.
pub struct TurnDetector {
    config: TurnDetectionConfig,
    /// Whether the user is currently speaking.
    is_speaking: bool,
    /// When speech last ended (VAD transitioned to silence).
    speech_ended_at: Option<Instant>,
    /// Whether we've already emitted TurnEnded for this speech segment.
    turn_ended_emitted: bool,
}

impl TurnDetector {
    /// Create a new turn detector.
    pub fn new(config: TurnDetectionConfig) -> Self {
        Self {
            config,
            is_speaking: false,
            speech_ended_at: None,
            turn_ended_emitted: false,
        }
    }

    /// Update with the current VAD state.
    ///
    /// Returns a `TurnDetectionEvent` if a transition occurred.
    pub fn update(&mut self, vad_is_speaking: bool) -> Option<TurnDetectionEvent> {
        if !self.config.enabled {
            return None;
        }

        if vad_is_speaking && !self.is_speaking {
            // Speech started
            self.is_speaking = true;
            self.speech_ended_at = None;
            self.turn_ended_emitted = false;
            return Some(TurnDetectionEvent::SpeechStarted);
        }

        if !vad_is_speaking && self.is_speaking {
            // Speech just ended — start the delay timer
            self.is_speaking = false;
            self.speech_ended_at = Some(Instant::now());
        }

        // Check if end-of-speech delay has elapsed
        if let Some(ended_at) = self.speech_ended_at {
            if !self.turn_ended_emitted
                && ended_at.elapsed()
                    >= Duration::from_millis(self.config.end_of_speech_delay_ms)
            {
                self.turn_ended_emitted = true;
                self.speech_ended_at = None;
                return Some(TurnDetectionEvent::TurnEnded);
            }
        }

        None
    }

    /// Whether speech is currently in progress.
    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    /// Whether we're waiting for the end-of-speech delay.
    pub fn is_pending_turn_end(&self) -> bool {
        self.speech_ended_at.is_some() && !self.turn_ended_emitted
    }

    /// Reset the detector state.
    pub fn reset(&mut self) {
        self.is_speaking = false;
        self.speech_ended_at = None;
        self.turn_ended_emitted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn speech_start_detected() {
        let mut detector = TurnDetector::new(TurnDetectionConfig::default());

        let event = detector.update(true);
        assert_eq!(event, Some(TurnDetectionEvent::SpeechStarted));
        assert!(detector.is_speaking());
    }

    #[test]
    fn turn_end_after_delay() {
        let mut detector = TurnDetector::new(TurnDetectionConfig {
            end_of_speech_delay_ms: 50,
            enabled: true,
        });

        // Start speaking
        detector.update(true);

        // Stop speaking
        detector.update(false);
        assert!(detector.is_pending_turn_end());

        // Not enough time yet
        let event = detector.update(false);
        assert!(event.is_none() || matches!(event, Some(TurnDetectionEvent::TurnEnded)));

        // Wait for delay
        thread::sleep(Duration::from_millis(60));
        let event = detector.update(false);
        assert_eq!(event, Some(TurnDetectionEvent::TurnEnded));
    }

    #[test]
    fn speech_resume_cancels_turn_end() {
        let mut detector = TurnDetector::new(TurnDetectionConfig {
            end_of_speech_delay_ms: 200,
            enabled: true,
        });

        // Start and stop speaking
        detector.update(true);
        detector.update(false);
        assert!(detector.is_pending_turn_end());

        // Resume speaking before delay elapses
        let event = detector.update(true);
        assert_eq!(event, Some(TurnDetectionEvent::SpeechStarted));
        assert!(!detector.is_pending_turn_end());
    }

    #[test]
    fn disabled_detector_emits_nothing() {
        let mut detector = TurnDetector::new(TurnDetectionConfig {
            enabled: false,
            ..Default::default()
        });

        assert!(detector.update(true).is_none());
        assert!(detector.update(false).is_none());
    }
}
