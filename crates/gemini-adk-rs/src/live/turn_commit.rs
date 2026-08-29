//! Pure turn-commit state machine.
//!
//! Raw VAD edges make two measured mistakes when used directly as turn signals.
//! (1) Firing end-of-turn on every speech offset commits during mid-turn pauses:
//! false-positive rate 0.206 at recall 0.895. Holding the commit through extra
//! silence walks a clean frontier: hold 600ms → fp 0.135; 800ms → recall 0.798
//! fp 0.087; 1200ms → fp 0.032. (2) Treating every speech onset during the
//! other side's turn as an interruption fires on backchannels ("mm-hm"):
//! fp 0.702. Requiring the speech to SUSTAIN before committing suppresses them:
//! sustain 600ms → fp 0.319 recall 0.939; 1000ms → fp 0.126; 1400ms → fp 0.062
//! recall 0.899. This module is that mechanism: a policy layer between VAD
//! edges and turn signals, with the operating point as configuration.
//!
//! Measurements are from Sesame's TurnBench dev set (38 conversations of real
//! dyadic speech).

use gemini_genai_rs::prelude::VadEvent;
use std::time::Duration;

/// Operating point for turn commitment. All timings are measured from the
/// VAD edge (which already includes the detector's hangover).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnCommitConfig {
    /// Extra silence after SpeechEnd before committing end-of-turn; speech
    /// resuming within the hold cancels the commit (the pause is bridged).
    pub eot_hold: Duration,
    /// Speech that begins while the model holds the floor must sustain this
    /// long before committing as an interruption; shorter overlapped speech
    /// is treated as a backchannel and never surfaces.
    pub min_interruption: Duration,
}

impl TurnCommitConfig {
    /// Zeros — bit-compatible with raw edge forwarding.
    pub fn immediate() -> Self {
        Self {
            eot_hold: Duration::ZERO,
            min_interruption: Duration::ZERO,
        }
    }

    /// Mid-frontier: eot_hold 400ms, min_interruption 600ms.
    pub fn responsive() -> Self {
        Self {
            eot_hold: Duration::from_millis(400),
            min_interruption: Duration::from_millis(600),
        }
    }

    /// TurnBench 0.1-fp-budget qualifying point: eot_hold 800ms, min_interruption 1400ms.
    pub fn conversational() -> Self {
        Self {
            eot_hold: Duration::from_millis(800),
            min_interruption: Duration::from_millis(1400),
        }
    }
}

impl Default for TurnCommitConfig {
    fn default() -> Self {
        Self::responsive()
    }
}

/// A committed turn signal, produced at its commit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnSignal {
    /// User took a free floor — forward as activityStart immediately.
    ActivityStart,
    /// Sustained user speech while the model held the floor — the
    /// activityStart of a deliberate barge-in.
    InterruptionStart,
    /// User turn ended (silence outlasted the hold) — forward as activityEnd.
    ActivityEnd,
}

/// The policy state machine. Driven by a monotonic millisecond audio clock
/// (advance it by the duration of each audio chunk, not wall time).
///
/// Commit time IS the call's now_ms — signals carry no timestamps.
pub struct TurnCommitPolicy {
    config: TurnCommitConfig,
    /// Whether an activityStart or InterruptionStart has been committed without
    /// a matching ActivityEnd yet.
    turn_active: bool,
    /// If Some, a SpeechEnd was recorded at this time; ActivityEnd will emit
    /// when age >= eot_hold.
    pending_end_at: Option<u64>,
    /// If Some, a SpeechStart during model_speaking was recorded at this time;
    /// InterruptionStart will emit when age >= min_interruption, provided the
    /// SpeechEnd never arrived (sustain check).
    pending_interruption_at: Option<u64>,
}

impl TurnCommitPolicy {
    /// Create a new policy state machine with the given config.
    pub fn new(config: TurnCommitConfig) -> Self {
        Self {
            config,
            turn_active: false,
            pending_end_at: None,
            pending_interruption_at: None,
        }
    }

    /// Apply the VAD edges observed in the chunk ending at `now_ms`, then
    /// advance pending holds/sustains to `now_ms`. `model_speaking` is the
    /// caller's knowledge of whether the model currently holds the floor.
    /// Returns the signals that COMMIT at this call, in order.
    pub fn advance(
        &mut self,
        now_ms: u64,
        edges: &[VadEvent],
        model_speaking: bool,
    ) -> Vec<TurnSignal> {
        let mut signals = Vec::new();

        // Apply edges in slice order.
        for &edge in edges {
            match edge {
                VadEvent::SpeechStart => {
                    // Cancel any pending end-hold (pause bridged — no ActivityEnd will fire).
                    self.pending_end_at = None;

                    if self.turn_active {
                        // If a user turn is already active, nothing else.
                    } else if model_speaking && self.config.min_interruption > Duration::ZERO {
                        // Record a pending interruption start at now_ms (nothing emitted yet).
                        self.pending_interruption_at = Some(now_ms);
                    } else {
                        // Emit ActivityStart (or InterruptionStart if model_speaking with min_interruption == 0).
                        let signal = if model_speaking {
                            TurnSignal::InterruptionStart
                        } else {
                            TurnSignal::ActivityStart
                        };
                        signals.push(signal);
                        self.turn_active = true;
                    }
                }
                VadEvent::SpeechEnd => {
                    if self.pending_interruption_at.is_some() {
                        // Pending interruption start exists (speech never sustained).
                        // Discard it silently — that was a backchannel.
                        self.pending_interruption_at = None;
                    } else if self.turn_active {
                        // Turn is active: record pending end at now_ms (emit nothing yet).
                        self.pending_end_at = Some(now_ms);
                    }
                    // If eot_hold == 0 this commits ActivityEnd on the same advance() call
                    // (handled below in expiry).
                }
            }
        }

        // After applying edges, expire pendings against now_ms.
        if let Some(pending_at) = self.pending_interruption_at {
            let age_ms = now_ms.saturating_sub(pending_at);
            if age_ms >= self.config.min_interruption.as_millis() as u64 {
                signals.push(TurnSignal::InterruptionStart);
                self.turn_active = true;
                self.pending_interruption_at = None;
            }
        }

        if let Some(pending_at) = self.pending_end_at {
            let age_ms = now_ms.saturating_sub(pending_at);
            if age_ms >= self.config.eot_hold.as_millis() as u64 {
                signals.push(TurnSignal::ActivityEnd);
                self.turn_active = false;
                self.pending_end_at = None;
            }
        }

        signals
    }

    /// Whether an activityStart/InterruptionStart has been committed without
    /// a matching ActivityEnd yet.
    pub fn user_turn_active(&self) -> bool {
        self.turn_active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_passthrough_start() {
        let mut policy = TurnCommitPolicy::new(TurnCommitConfig::immediate());
        let edges = vec![VadEvent::SpeechStart];
        let signals = policy.advance(0, &edges, false);
        assert_eq!(signals, vec![TurnSignal::ActivityStart]);
        assert!(policy.user_turn_active());
    }

    #[test]
    fn immediate_passthrough_end() {
        let mut policy = TurnCommitPolicy::new(TurnCommitConfig::immediate());
        // Start first
        let edges = vec![VadEvent::SpeechStart];
        let signals = policy.advance(0, &edges, false);
        assert_eq!(signals, vec![TurnSignal::ActivityStart]);
        // Then end
        let edges = vec![VadEvent::SpeechEnd];
        let signals = policy.advance(100, &edges, false);
        assert_eq!(signals, vec![TurnSignal::ActivityEnd]);
        assert!(!policy.user_turn_active());
    }

    #[test]
    fn immediate_passthrough_interruption() {
        let mut policy = TurnCommitPolicy::new(TurnCommitConfig::immediate());
        let edges = vec![VadEvent::SpeechStart];
        let signals = policy.advance(0, &edges, true);
        assert_eq!(signals, vec![TurnSignal::InterruptionStart]);
        assert!(policy.user_turn_active());
    }

    #[test]
    fn pause_bridging() {
        let config = TurnCommitConfig {
            eot_hold: Duration::from_millis(600),
            min_interruption: Duration::from_millis(600),
        };
        let mut policy = TurnCommitPolicy::new(config);

        // Start speech
        let signals = policy.advance(0, &[VadEvent::SpeechStart], false);
        assert_eq!(signals, vec![TurnSignal::ActivityStart]);

        // End speech at t=100
        let signals = policy.advance(100, &[VadEvent::SpeechEnd], false);
        assert!(signals.is_empty(), "Should not emit yet, pending");

        // Resume within hold (at 100 + 300ms < 600ms hold)
        let signals = policy.advance(400, &[VadEvent::SpeechStart], false);
        assert!(signals.is_empty(), "Speech resumed, cancels pending end");
        assert!(policy.user_turn_active());

        // Eventually end and wait past hold
        let signals = policy.advance(500, &[VadEvent::SpeechEnd], false);
        assert!(signals.is_empty(), "Pending end at 500");

        let signals = policy.advance(1100, &[], false);
        assert_eq!(signals, vec![TurnSignal::ActivityEnd], "Hold expired");
        assert!(!policy.user_turn_active());
    }

    #[test]
    fn backchannel_suppression() {
        let config = TurnCommitConfig {
            eot_hold: Duration::from_millis(600),
            min_interruption: Duration::from_millis(600),
        };
        let mut policy = TurnCommitPolicy::new(config);

        // Model speaking
        let signals = policy.advance(0, &[VadEvent::SpeechStart], true);
        // Pending interruption, not emitted yet
        assert!(signals.is_empty());
        assert!(!policy.user_turn_active());

        // End shortly after (200ms < 600ms sustain requirement)
        let signals = policy.advance(200, &[VadEvent::SpeechEnd], true);
        // Backchannel discarded, nothing emitted
        assert!(signals.is_empty());
        assert!(!policy.user_turn_active());
    }

    #[test]
    fn sustained_interruption() {
        let config = TurnCommitConfig {
            eot_hold: Duration::from_millis(600),
            min_interruption: Duration::from_millis(600),
        };
        let mut policy = TurnCommitPolicy::new(config);

        // Model speaking, user starts speech
        let signals = policy.advance(0, &[VadEvent::SpeechStart], true);
        assert!(signals.is_empty(), "Pending interruption");

        // Advance past sustain threshold without ending
        let signals = policy.advance(700, &[], true);
        assert_eq!(
            signals,
            vec![TurnSignal::InterruptionStart],
            "Sustain expired, interrupt emitted"
        );
        assert!(policy.user_turn_active());

        // Now end and wait for hold
        let signals = policy.advance(800, &[VadEvent::SpeechEnd], true);
        assert!(signals.is_empty(), "Pending end");

        let signals = policy.advance(1500, &[], true);
        assert_eq!(signals, vec![TurnSignal::ActivityEnd]);
        assert!(!policy.user_turn_active());
    }

    #[test]
    fn free_floor_start_never_delayed() {
        let config = TurnCommitConfig {
            eot_hold: Duration::from_millis(800),
            min_interruption: Duration::from_millis(10000), // huge
        };
        let mut policy = TurnCommitPolicy::new(config);

        // Free floor (model not speaking)
        let signals = policy.advance(0, &[VadEvent::SpeechStart], false);
        assert_eq!(signals, vec![TurnSignal::ActivityStart]);
        assert!(policy.user_turn_active());
    }

    #[test]
    fn eot_hold_expiry_timing() {
        let config = TurnCommitConfig {
            eot_hold: Duration::from_millis(400),
            min_interruption: Duration::from_millis(600),
        };
        let mut policy = TurnCommitPolicy::new(config);

        // Start and end
        policy.advance(0, &[VadEvent::SpeechStart], false);
        policy.advance(100, &[VadEvent::SpeechEnd], false);

        // Advance to hold - 1ms, should not emit
        let signals = policy.advance(499, &[], false);
        assert!(signals.is_empty());
        assert!(policy.user_turn_active());

        // Advance to hold, should emit
        let signals = policy.advance(500, &[], false);
        assert_eq!(signals, vec![TurnSignal::ActivityEnd]);
        assert!(!policy.user_turn_active());
    }

    #[test]
    fn config_presets() {
        let immediate = TurnCommitConfig::immediate();
        assert_eq!(immediate.eot_hold, Duration::ZERO);
        assert_eq!(immediate.min_interruption, Duration::ZERO);

        let responsive = TurnCommitConfig::responsive();
        assert_eq!(responsive.eot_hold, Duration::from_millis(400));
        assert_eq!(responsive.min_interruption, Duration::from_millis(600));

        let conversational = TurnCommitConfig::conversational();
        assert_eq!(conversational.eot_hold, Duration::from_millis(800));
        assert_eq!(conversational.min_interruption, Duration::from_millis(1400));

        // Default == responsive
        let default = TurnCommitConfig::default();
        assert_eq!(default, responsive);
    }

    #[test]
    fn multiple_edges_in_one_call() {
        let config = TurnCommitConfig {
            eot_hold: Duration::from_millis(600),
            min_interruption: Duration::from_millis(600),
        };
        let mut policy = TurnCommitPolicy::new(config);

        // Start and end in same call
        let signals = policy.advance(0, &[VadEvent::SpeechStart, VadEvent::SpeechEnd], false);
        assert_eq!(signals, vec![TurnSignal::ActivityStart]);
        // SpeechEnd recorded as pending, not expired yet
        assert!(policy.user_turn_active());

        // Expire the hold
        let signals = policy.advance(700, &[], false);
        assert_eq!(signals, vec![TurnSignal::ActivityEnd]);
        assert!(!policy.user_turn_active());
    }
}
