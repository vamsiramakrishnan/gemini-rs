//! Transcript accumulation, stable-prefix tracking, and speculation gating.
//!
//! Gemini Live emits input transcription independently of turn boundaries and
//! revises partial results as recognition improves. Two consequences shape this
//! module:
//!
//! 1. **Partial transcripts are hypotheses.** They may prefetch context; they
//!    may never become evidence. Only `is_final` output is admissible.
//! 2. **Ordering cannot be assumed.** Turn identity is assigned locally from
//!    VAD and turn-complete events, and every asynchronous task carries the
//!    generation it was started for, so a late result cannot overwrite a newer
//!    one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bm25::tokenize;
use crate::core::{TranscriptConfig, TurnId};

/// The current reading of an in-progress utterance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscriptHypothesis {
    /// The portion that has stopped changing between revisions.
    pub stable_prefix: String,
    /// The portion still being revised.
    pub unstable_suffix: String,
    /// How many revisions have been seen for this turn.
    pub revision: u64,
    /// Whether the transcript has been finalized.
    pub finalised: bool,
}

impl TranscriptHypothesis {
    /// The full text as currently understood.
    pub fn text(&self) -> String {
        if self.unstable_suffix.is_empty() {
            self.stable_prefix.clone()
        } else if self.stable_prefix.is_empty() {
            self.unstable_suffix.clone()
        } else {
            format!("{} {}", self.stable_prefix, self.unstable_suffix)
        }
    }
}

/// Accumulates transcript revisions for one turn.
///
/// The stable prefix is the longest word-aligned common prefix across
/// revisions. Speculating on the stable prefix rather than the whole partial
/// avoids re-running retrieval every time the recognizer changes its mind about
/// the last word.
#[derive(Debug, Default)]
pub struct TranscriptAccumulator {
    turn_id: TurnId,
    previous_words: Vec<String>,
    stable_words: Vec<String>,
    revision: u64,
    finalised: bool,
}

impl TranscriptAccumulator {
    /// Start accumulating for a turn.
    pub fn new(turn_id: TurnId) -> Self {
        Self {
            turn_id,
            ..Default::default()
        }
    }

    /// Discard state and begin a new turn.
    pub fn begin_turn(&mut self, turn_id: TurnId) {
        *self = Self::new(turn_id);
    }

    /// The turn being accumulated.
    pub fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    /// Fold in a partial transcript, returning the updated hypothesis.
    pub fn push_partial(&mut self, text: &str) -> TranscriptHypothesis {
        let words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        let common = common_prefix_len(&self.previous_words, &words);
        // The final word of a partial is the one most likely to be revised, so
        // it is never treated as stable until a later revision confirms it.
        let stable_len = common.min(words.len().saturating_sub(1));
        if stable_len > self.stable_words.len() {
            self.stable_words = words[..stable_len].to_vec();
        }
        self.previous_words = words.clone();
        self.revision += 1;

        TranscriptHypothesis {
            stable_prefix: self.stable_words.join(" "),
            unstable_suffix: words[self.stable_words.len().min(words.len())..].join(" "),
            revision: self.revision,
            finalised: false,
        }
    }

    /// Fold in the finalized transcript.
    pub fn finalize(&mut self, text: &str) -> TranscriptHypothesis {
        self.stable_words = text.split_whitespace().map(str::to_string).collect();
        self.previous_words = self.stable_words.clone();
        self.revision += 1;
        self.finalised = true;
        TranscriptHypothesis {
            stable_prefix: self.stable_words.join(" "),
            unstable_suffix: String::new(),
            revision: self.revision,
            finalised: true,
        }
    }

    /// Whether the turn's transcript has been finalized.
    pub fn is_finalised(&self) -> bool {
        self.finalised
    }
}

fn common_prefix_len(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// The generation counter that makes stale asynchronous work harmless.
///
/// Every speculative task records the generation it was started at; before
/// publishing a result it re-reads the counter, and abandons the result if the
/// world has moved on. Without this, a slow retrieval for turn 4 could overwrite
/// the prepared context for turn 5.
#[derive(Debug, Default)]
pub struct GenerationGuard {
    current: AtomicU64,
}

impl GenerationGuard {
    /// A guard starting at generation zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// The generation in force.
    pub fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    /// Invalidate outstanding work and return the new generation.
    pub fn advance(&self) -> u64 {
        self.current.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Whether a result computed at `generation` is still wanted.
    pub fn is_current(&self, generation: u64) -> bool {
        self.current() == generation
    }
}

/// Decides whether a partial transcript revision is worth speculating on.
///
/// Two gates, both cheap: a debounce window, and a minimum amount of genuinely
/// new content. Recognizers emit revisions far faster than retrieval can
/// usefully run, and most revisions change nothing that would change the query.
#[derive(Debug)]
pub struct SpeculationGate {
    debounce: Duration,
    minimum_new_tokens: usize,
    last_fired_at: Option<Instant>,
    last_query_tokens: Vec<String>,
}

/// Why the gate did or did not fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeculationDecision {
    /// Speculate now.
    Fire,
    /// Too soon since the last speculation.
    Debounced,
    /// Not enough new content to change the query.
    InsufficientNewContent,
    /// Nothing stable to speculate on yet.
    NothingStable,
}

impl SpeculationGate {
    /// Build a gate from transcript configuration.
    pub fn new(config: &TranscriptConfig) -> Self {
        Self {
            debounce: Duration::from_millis(config.partial_debounce_ms),
            minimum_new_tokens: config.minimum_new_content_tokens,
            last_fired_at: None,
            last_query_tokens: Vec::new(),
        }
    }

    /// Reset between turns.
    pub fn reset(&mut self) {
        self.last_fired_at = None;
        self.last_query_tokens.clear();
    }

    /// Decide whether to speculate on `hypothesis` at `now`.
    ///
    /// A signal the deterministic extractor already recognised (a known entity,
    /// an explicit recall phrase) overrides both gates: those are exactly the
    /// cases where prefetching pays for itself.
    pub fn consider(
        &mut self,
        hypothesis: &TranscriptHypothesis,
        has_strong_signal: bool,
        now: Instant,
    ) -> SpeculationDecision {
        let tokens = tokenize(&hypothesis.stable_prefix);
        if tokens.is_empty() && !hypothesis.finalised {
            return SpeculationDecision::NothingStable;
        }

        let new_tokens = tokens
            .len()
            .saturating_sub(common_prefix_len_str(&self.last_query_tokens, &tokens));
        let urgent = has_strong_signal || hypothesis.finalised;

        if !urgent {
            if let Some(last) = self.last_fired_at
                && now.duration_since(last) < self.debounce
            {
                return SpeculationDecision::Debounced;
            }
            if new_tokens < self.minimum_new_tokens {
                return SpeculationDecision::InsufficientNewContent;
            }
        }

        self.last_fired_at = Some(now);
        self.last_query_tokens = tokens;
        SpeculationDecision::Fire
    }
}

fn common_prefix_len_str(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stable_prefix_grows_as_revisions_agree() {
        let mut acc = TranscriptAccumulator::new(TurnId(1));
        acc.push_partial("what should");
        let h = acc.push_partial("what should we eat");
        assert_eq!(h.stable_prefix, "what should");
        assert_eq!(h.unstable_suffix, "we eat");

        let h = acc.push_partial("what should we eat tonight");
        assert_eq!(h.stable_prefix, "what should we eat");
    }

    #[test]
    fn a_revised_tail_does_not_corrupt_the_stable_prefix() {
        let mut acc = TranscriptAccumulator::new(TurnId(1));
        acc.push_partial("book a table for read");
        let h = acc.push_partial("book a table for Rhea");
        assert_eq!(h.stable_prefix, "book a table for");
        assert!(h.text().contains("Rhea"));
    }

    #[test]
    fn finalizing_replaces_the_hypothesis_wholesale() {
        let mut acc = TranscriptAccumulator::new(TurnId(1));
        acc.push_partial("I am vegetarian");
        let h = acc.finalize("I am pescatarian");
        assert!(h.finalised);
        assert_eq!(h.stable_prefix, "I am pescatarian");
        assert!(h.unstable_suffix.is_empty());
        assert!(acc.is_finalised());
    }

    #[test]
    fn beginning_a_turn_clears_previous_state() {
        let mut acc = TranscriptAccumulator::new(TurnId(1));
        acc.finalize("first turn");
        acc.begin_turn(TurnId(2));
        assert_eq!(acc.turn_id(), TurnId(2));
        assert!(!acc.is_finalised());
        let h = acc.push_partial("second");
        assert!(h.stable_prefix.is_empty());
    }

    #[test]
    fn stale_generations_are_rejected() {
        let guard = GenerationGuard::new();
        let started_at = guard.current();
        assert!(guard.is_current(started_at));
        let newer = guard.advance();
        assert!(!guard.is_current(started_at));
        assert!(guard.is_current(newer));
    }

    #[test]
    fn the_gate_debounces_rapid_revisions() {
        let config = TranscriptConfig::default();
        let mut gate = SpeculationGate::new(&config);
        let t0 = Instant::now();

        let first = TranscriptHypothesis {
            stable_prefix: "what should we eat for dinner".into(),
            ..Default::default()
        };
        assert_eq!(gate.consider(&first, false, t0), SpeculationDecision::Fire);

        let second = TranscriptHypothesis {
            stable_prefix: "what should we eat for dinner tonight with Rhea and Kushal nearby"
                .into(),
            ..Default::default()
        };
        assert_eq!(
            gate.consider(&second, false, t0 + Duration::from_millis(50)),
            SpeculationDecision::Debounced
        );
        assert_eq!(
            gate.consider(&second, false, t0 + Duration::from_millis(400)),
            SpeculationDecision::Fire
        );
    }

    #[test]
    fn the_gate_ignores_revisions_that_add_nothing() {
        let mut gate = SpeculationGate::new(&TranscriptConfig::default());
        let t0 = Instant::now();
        let h = TranscriptHypothesis {
            stable_prefix: "what should we eat for dinner".into(),
            ..Default::default()
        };
        assert_eq!(gate.consider(&h, false, t0), SpeculationDecision::Fire);

        let barely_more = TranscriptHypothesis {
            stable_prefix: "what should we eat for dinner now".into(),
            ..Default::default()
        };
        assert_eq!(
            gate.consider(&barely_more, false, t0 + Duration::from_secs(5)),
            SpeculationDecision::InsufficientNewContent
        );
    }

    #[test]
    fn a_strong_signal_or_a_final_transcript_bypasses_both_gates() {
        let mut gate = SpeculationGate::new(&TranscriptConfig::default());
        let t0 = Instant::now();
        let h = TranscriptHypothesis {
            stable_prefix: "tell me about Rhea".into(),
            ..Default::default()
        };
        assert_eq!(gate.consider(&h, false, t0), SpeculationDecision::Fire);
        assert_eq!(
            gate.consider(&h, true, t0 + Duration::from_millis(1)),
            SpeculationDecision::Fire
        );

        let finalised = TranscriptHypothesis {
            stable_prefix: "tell me about Rhea".into(),
            finalised: true,
            ..Default::default()
        };
        assert_eq!(
            gate.consider(&finalised, false, t0 + Duration::from_millis(2)),
            SpeculationDecision::Fire
        );
    }

    #[test]
    fn nothing_stable_yet_means_nothing_to_speculate_on() {
        let mut gate = SpeculationGate::new(&TranscriptConfig::default());
        let empty = TranscriptHypothesis {
            unstable_suffix: "wha".into(),
            ..Default::default()
        };
        assert_eq!(
            gate.consider(&empty, false, Instant::now()),
            SpeculationDecision::NothingStable
        );
    }
}
