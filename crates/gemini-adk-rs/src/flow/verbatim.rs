//! Verbatim stages: text the model must say word for word, verified.
//!
//! A regulated call has lines that must be read exactly: a disclosure, a
//! consent statement, the terms of a payment. An instruction to "read this
//! verbatim" is a request, not a guarantee. A verbatim stage turns it into a
//! guarantee at the governance level.
//!
//! - While the stage is active, the stack publishes the required text under
//!   [`VERBATIM_KEY`].
//! - At the end of each model turn, the control lane looks for the text in
//!   the model's output transcript ([`contained_similarity`]) and writes the
//!   verdict to [`verbatim_flag`]`(step)`. The text may be surrounded by
//!   other words in the same turn, such as a greeting before a disclosure.
//!   It also emits `LiveEvent::VerbatimChecked`.
//! - The stage completes only once the flag is true. A paraphrase keeps the
//!   conversation in the stage, where the posture asks for the exact text
//!   again.
//!
//! The check needs output transcription, since it reads what was actually
//! said. Without a transcript nothing is verified, and the stage does not
//! complete.

use serde::{Deserialize, Serialize};

use crate::state::State;

/// The state key under which the active verbatim requirement is published.
pub const VERBATIM_KEY: &str = "session:verbatim";

/// How close (0–1, word level) the spoken text must be to the required text.
/// Tolerates a transcription slip or two in a long passage, not a paraphrase.
pub const VERBATIM_MIN_SIMILARITY: f64 = 0.9;

/// The state key holding whether `step`'s text was said verbatim.
pub fn verbatim_flag(step: &str) -> String {
    format!("verbatim:{step}")
}

/// The published requirement of the active verbatim stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerbatimRequirement {
    /// The stage that requires it.
    pub step: String,
    /// The text to be said.
    pub text: String,
}

/// The outcome of one verbatim check.
#[derive(Debug, Clone, PartialEq)]
pub struct VerbatimVerdict {
    /// The stage checked.
    pub step: String,
    /// Word-level similarity of what was said to what was required.
    pub similarity: f64,
    /// Whether it meets [`VERBATIM_MIN_SIMILARITY`].
    pub passed: bool,
}

/// Word-level similarity of `heard` to `expected`, from 0 to 1: one minus
/// the word edit distance over the length of the longer text, after
/// lowercasing and dropping punctuation.
pub fn similarity(expected: &str, heard: &str) -> f64 {
    let words = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (a, b) = (words(expected), words(heard));
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    // Levenshtein over words, one row at a time.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, wa) in a.iter().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        for (j, wb) in b.iter().enumerate() {
            let cost = usize::from(wa != wb);
            row[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1);
        }
        prev = row;
    }
    1.0 - prev[b.len()] as f64 / longest as f64
}

/// How closely `expected` appears somewhere in `heard`: the best
/// [`similarity`] between `expected` and a run of consecutive words of
/// `heard` about as long as it (within a tenth either way). A model turn
/// that says the required text and something else, such as "Welcome to
/// Bright Smile. This call may be recorded. How can I help?", scores as the
/// text alone.
pub fn contained_similarity(expected: &str, heard: &str) -> f64 {
    let words = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (want, got) = (words(expected), words(heard));
    let n = want.len();
    if n == 0 || got.len() <= n {
        return similarity(expected, heard);
    }
    let slack = (n / 10).max(1);
    let want = want.join(" ");
    let mut best = 0.0_f64;
    for len in n.saturating_sub(slack).max(1)..=(n + slack).min(got.len()) {
        for start in 0..=got.len() - len {
            best = best.max(similarity(&want, &got[start..start + len].join(" ")));
            if best >= 1.0 {
                return best;
            }
        }
    }
    best
}

/// Check a finished model turn against the active verbatim requirement, if
/// any, and record the verdict in state. `heard` is the turn's output
/// transcript; an empty transcript checks nothing.
pub fn check_turn(state: &State, heard: &str) -> Option<VerbatimVerdict> {
    let requirement = state.get::<VerbatimRequirement>(VERBATIM_KEY)?;
    if heard.trim().is_empty() {
        return None;
    }
    let similarity = contained_similarity(&requirement.text, heard);
    let passed = similarity >= VERBATIM_MIN_SIMILARITY;
    // Once said verbatim, a later turn in the same stage cannot undo it.
    let flag = verbatim_flag(&requirement.step);
    if passed || state.get::<bool>(&flag) != Some(true) {
        let _ = state.set(&flag, passed);
    }
    Some(VerbatimVerdict {
        step: requirement.step,
        similarity,
        passed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TERMS: &str = "Calls may be recorded for quality and training purposes.";

    #[test]
    fn the_text_may_share_the_turn_with_other_words() {
        let disclosure = "This call may be recorded.";
        let turn = "Thanks for calling Trattoria Rustica. This call may be recorded. \
                    How many guests, and for when?";
        assert_eq!(contained_similarity(disclosure, turn), 1.0);
        assert!(similarity(disclosure, turn) < VERBATIM_MIN_SIMILARITY);
        // One recogniser slip in a long passage inside a longer turn passes.
        let long = "You are speaking with an automated assistant and this call may be recorded for quality.";
        let heard = "Hello! You are speaking with an automated assistant and this call may be recorded for quality. How can I help?";
        assert!(contained_similarity(long, heard) >= VERBATIM_MIN_SIMILARITY);
        let slipped = heard.replace("recorded", "recorder");
        assert!(contained_similarity(long, &slipped) >= VERBATIM_MIN_SIMILARITY);
        // A paraphrase still fails.
        assert!(
            contained_similarity(
                disclosure,
                "Hi! We might record this, just so you know. How can I help?"
            ) < VERBATIM_MIN_SIMILARITY
        );
        // A turn shorter than the text compares as a whole.
        assert_eq!(
            contained_similarity(disclosure, "This call"),
            similarity(disclosure, "This call")
        );
    }

    #[test]
    fn exact_and_near_exact_pass_a_paraphrase_does_not() {
        assert_eq!(similarity(TERMS, TERMS), 1.0);
        assert_eq!(
            similarity(
                TERMS,
                "calls may be recorded, for quality and training purposes"
            ),
            1.0,
            "case and punctuation do not count"
        );
        let slip = similarity(
            TERMS,
            "Calls may be recorded for quality and trading purposes.",
        );
        assert!((0.85..1.0).contains(&slip), "{slip}");
        assert!(similarity(TERMS, "We might record this call.") < 0.5);
    }

    #[test]
    fn a_turn_is_checked_against_the_published_requirement() {
        let state = State::new();
        assert_eq!(check_turn(&state, TERMS), None, "no verbatim stage active");

        let _ = state.set(
            VERBATIM_KEY,
            VerbatimRequirement {
                step: "terms".into(),
                text: TERMS.into(),
            },
        );
        let miss = check_turn(&state, "We might record this call.").unwrap();
        assert!(!miss.passed);
        assert_eq!(state.get::<bool>(&verbatim_flag("terms")), Some(false));

        let hit = check_turn(&state, TERMS).unwrap();
        assert!(hit.passed);
        assert_eq!(state.get::<bool>(&verbatim_flag("terms")), Some(true));

        // A later chatty turn does not undo a verbatim reading.
        check_turn(&state, "Anything else?");
        assert_eq!(state.get::<bool>(&verbatim_flag("terms")), Some(true));
    }
}
