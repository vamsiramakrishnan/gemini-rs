//! Transcript redaction — sensitive data never reaches the lanes.
//!
//! A voice caller will read card numbers, one-time passcodes, and account
//! identifiers out loud, and speech recognition will faithfully transcribe
//! them. Anything downstream of the transcript — callbacks, the transcript
//! buffer, extraction, persistence snapshots, application logs — then holds
//! that data unless it is removed first.
//!
//! A [`TranscriptRedactor`] installed on the session
//! ([`LiveSessionBuilder::redaction`](super::builder::LiveSessionBuilder::redaction))
//! is applied at the event router, *before* either lane sees the text: what
//! the fast-lane callbacks receive, what the transcript buffer accumulates,
//! what extractors read, and what a persistence backend stores are all the
//! redacted form. There is deliberately no unredacted side channel.
//!
//! Two limits worth knowing:
//!
//! - **Streaming text deltas are not redacted.** A card number can straddle
//!   delta boundaries where no single chunk matches anything. Deltas are a
//!   text-mode display stream; voice deployments should treat transcripts
//!   and [`TextComplete`](super::events::LiveEvent::TextComplete) (both
//!   redacted) as the record.
//! - **Redaction is pattern-based**, applied to each partial and final
//!   transcript independently. It removes well-formed sensitive strings; it
//!   is a complement to, not a replacement for, infrastructure-level data
//!   loss prevention on stored audio.

use regex::Regex;

/// A card-number match is replaced by this, keeping the last four digits —
/// enough for the conversation to stay coherent ("the card ending 1234")
/// without retaining the number.
fn card_replacement(last4: &str) -> String {
    format!("[card ending {last4}]")
}

const NUMBER_REPLACEMENT: &str = "[redacted number]";

/// Pattern-based transcript scrubber. Build one with the methods below and
/// install it with
/// [`LiveSessionBuilder::redaction`](super::builder::LiveSessionBuilder::redaction).
///
/// ```
/// use gemini_adk_rs::live::redaction::TranscriptRedactor;
///
/// let redactor = TranscriptRedactor::new().card_numbers().long_digits(6);
/// assert_eq!(
///     redactor.redact("my card is 4111 1111 1111 1111 ok".into()),
///     "my card is [card ending 1111] ok"
/// );
/// ```
#[derive(Debug, Default)]
pub struct TranscriptRedactor {
    card_numbers: bool,
    long_digits: Option<usize>,
    custom: Vec<(Regex, String)>,
}

impl TranscriptRedactor {
    /// A redactor with nothing enabled. Chain the methods below.
    pub fn new() -> Self {
        Self::default()
    }

    /// Redact payment-card numbers: runs of 13–19 digits (spaces and dashes
    /// between groups allowed) that pass a Luhn check, replaced with
    /// `[card ending NNNN]`. The Luhn check keeps ordinary long numbers —
    /// tracking codes, reference IDs — from being eaten as cards.
    pub fn card_numbers(mut self) -> Self {
        self.card_numbers = true;
        self
    }

    /// Redact any remaining run of `min` or more consecutive digits
    /// (one-time passcodes, account numbers), replaced with
    /// `[redacted number]`. Runs after the card pass, so a card keeps its
    /// `[card ending NNNN]` form.
    pub fn long_digits(mut self, min: usize) -> Self {
        self.long_digits = Some(min);
        self
    }

    /// Redact a custom pattern with a fixed replacement — national
    /// identifiers, internal reference formats, whatever the deployment's
    /// compliance regime names.
    pub fn pattern(mut self, regex: Regex, replacement: impl Into<String>) -> Self {
        self.custom.push((regex, replacement.into()));
        self
    }

    /// Whether any rule is enabled. A no-op redactor is skipped entirely.
    pub fn is_active(&self) -> bool {
        self.card_numbers || self.long_digits.is_some() || !self.custom.is_empty()
    }

    /// Apply every enabled rule. Returns the input unchanged (no
    /// reallocation) when nothing matches.
    pub fn redact(&self, text: String) -> String {
        let mut text = text;
        if self.card_numbers {
            text = redact_cards(&text);
        }
        if let Some(min) = self.long_digits {
            text = redact_digit_runs(&text, min);
        }
        for (regex, replacement) in &self.custom {
            if regex.is_match(&text) {
                text = regex.replace_all(&text, replacement.as_str()).into_owned();
            }
        }
        text
    }
}

/// Candidate card spans: digit groups joined by optional single spaces or
/// dashes. Verified by length and Luhn before replacement.
fn card_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d(?:[ -]?\d){11,18}").expect("valid regex"))
}

fn digit_run_regex(min: usize) -> Regex {
    Regex::new(&format!(r"\d{{{min},}}")).expect("valid regex")
}

fn redact_cards(text: &str) -> String {
    if !card_regex().is_match(text) {
        return text.to_string();
    }
    card_regex()
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let raw = &caps[0];
            let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
            if (13..=19).contains(&digits.len()) && luhn_valid(&digits) {
                card_replacement(&digits[digits.len() - 4..])
            } else {
                raw.to_string()
            }
        })
        .into_owned()
}

fn redact_digit_runs(text: &str, min: usize) -> String {
    let regex = digit_run_regex(min);
    if !regex.is_match(text) {
        return text.to_string();
    }
    regex.replace_all(text, NUMBER_REPLACEMENT).into_owned()
}

/// The Luhn checksum every payment-card number satisfies (ISO/IEC 7812).
fn luhn_valid(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, ch) in digits.chars().rev().enumerate() {
        let Some(d) = ch.to_digit(10) else {
            return false;
        };
        let d = if i % 2 == 1 {
            let doubled = d * 2;
            if doubled > 9 {
                doubled - 9
            } else {
                doubled
            }
        } else {
            d
        };
        sum += d;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> TranscriptRedactor {
        TranscriptRedactor::new().card_numbers().long_digits(6)
    }

    #[test]
    fn redacts_a_spoken_card_number_keeping_last_four() {
        assert_eq!(
            full().redact("it's 4111 1111 1111 1111 thanks".into()),
            "it's [card ending 1111] thanks"
        );
        // Dashed and unseparated forms too.
        assert_eq!(
            full().redact("4012-8888-8888-1881".into()),
            "[card ending 1881]"
        );
        assert_eq!(
            full().redact("378282246310005".into()),
            "[card ending 0005]",
            "15-digit card numbers are in range"
        );
    }

    #[test]
    fn luhn_failures_are_not_cards_but_still_hit_the_digit_rule() {
        // 16 digits, wrong checksum: not a card, but still a long number.
        assert_eq!(
            full().redact("ref 4111111111111112".into()),
            format!("ref {NUMBER_REPLACEMENT}")
        );
        // Without the digit rule it passes through untouched.
        assert_eq!(
            TranscriptRedactor::new()
                .card_numbers()
                .redact("ref 4111111111111112".into()),
            "ref 4111111111111112"
        );
    }

    #[test]
    fn short_numbers_survive_otp_length_does_not() {
        assert_eq!(
            full().redact("table for 4 at 19:00".into()),
            "table for 4 at 19:00"
        );
        assert_eq!(
            full().redact("the code is 493028".into()),
            format!("the code is {NUMBER_REPLACEMENT}")
        );
    }

    #[test]
    fn custom_patterns_apply() {
        let redactor = TranscriptRedactor::new()
            .pattern(Regex::new(r"[STFG]\d{7}[A-Z]").unwrap(), "[redacted id]");
        assert_eq!(
            redactor.redact("my id is S1234567D ok".into()),
            "my id is [redacted id] ok"
        );
    }

    #[test]
    fn inactive_redactor_reports_itself() {
        assert!(!TranscriptRedactor::new().is_active());
        assert!(TranscriptRedactor::new().card_numbers().is_active());
    }
}
