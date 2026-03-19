//! Soft turn detection for proactive silence awareness.
//!
//! When `proactiveAudio` is enabled, the model may choose not to respond.
//! No `TurnComplete` fires, so the 17-step pipeline never runs. This module
//! detects when the user finished speaking (VAD end) but the model stayed
//! silent, and triggers a lightweight "soft turn" to keep state updated.

use std::time::{Duration, Instant};

/// Default timeout before a soft turn fires after VAD end.
pub const DEFAULT_SOFT_TURN_TIMEOUT: Duration = Duration::from_secs(2);

/// Detects proactive silence — user stopped speaking but model didn't respond.
pub struct SoftTurnDetector {
    /// When VAD end was last observed (reset when model responds).
    vad_ended_at: Option<Instant>,
    /// How long to wait after VAD end before declaring a soft turn.
    timeout: Duration,
}

impl SoftTurnDetector {
    /// Create with a custom timeout.
    pub fn new(timeout: Duration) -> Self {
        Self {
            vad_ended_at: None,
            timeout,
        }
    }

    /// Called when VAD end is observed.
    pub fn on_vad_end(&mut self) {
        self.vad_ended_at = Some(Instant::now());
    }

    /// Called when the model produces any response (text, audio, tool call).
    /// Resets the detector — no soft turn needed.
    pub fn on_model_response(&mut self) {
        self.vad_ended_at = None;
    }

    /// Check if a soft turn should fire.
    pub fn check(&self, now: Instant) -> bool {
        self.vad_ended_at
            .map(|t| now.duration_since(t) >= self.timeout)
            .unwrap_or(false)
    }

    /// Reset after a soft turn fires.
    pub fn reset(&mut self) {
        self.vad_ended_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_soft_turn_without_vad_end() {
        let d = SoftTurnDetector::new(Duration::from_millis(100));
        assert!(!d.check(Instant::now()));
    }

    #[test]
    fn soft_turn_after_timeout() {
        let mut d = SoftTurnDetector::new(Duration::from_millis(50));
        d.on_vad_end();
        // Immediately: no
        assert!(!d.check(Instant::now()));
        // After timeout
        std::thread::sleep(Duration::from_millis(60));
        assert!(d.check(Instant::now()));
    }

    #[test]
    fn model_response_cancels_soft_turn() {
        let mut d = SoftTurnDetector::new(Duration::from_millis(50));
        d.on_vad_end();
        d.on_model_response();
        std::thread::sleep(Duration::from_millis(60));
        assert!(!d.check(Instant::now()));
    }

    #[test]
    fn reset_clears_state() {
        let mut d = SoftTurnDetector::new(Duration::from_millis(50));
        d.on_vad_end();
        std::thread::sleep(Duration::from_millis(60));
        assert!(d.check(Instant::now()));
        d.reset();
        assert!(!d.check(Instant::now()));
    }
}
