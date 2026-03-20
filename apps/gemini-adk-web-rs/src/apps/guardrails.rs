use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::info;

use gemini_adk_fluent_rs::prelude::*;

use crate::app::{AppError, ClientMessage, DemoApp, ServerMessage, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

use super::extractors::RegexExtractor;
use super::resolve_voice;

// ---------------------------------------------------------------------------
// Violation detection
// ---------------------------------------------------------------------------

/// Detected violation with matched detail.
struct DetectedViolation {
    rule_name: &'static str,
    severity: &'static str,
    detail: String,
}

// Pre-compiled regex patterns for violation detection.
static SSN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{3}[-.]?\d{2}[-.]?\d{4}\b").unwrap());
static CC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{4}[\s-]?\d{4}[\s-]?\d{4}[\s-]?\d{4}\b").unwrap());

/// Check text for policy violations. Returns all detected violations.
fn check_violations(text: &str) -> Vec<DetectedViolation> {
    let mut violations = Vec::new();

    // PII: SSN pattern (XXX-XX-XXXX or XXX.XX.XXXX or XXXXXXXXX).
    if SSN_RE.is_match(text) {
        if let Some(m) = SSN_RE.find(text) {
            let digits_only: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
            if digits_only.len() == 9 {
                violations.push(DetectedViolation {
                    rule_name: "pii_ssn",
                    severity: "critical",
                    detail: format!("Possible SSN detected: {}***", &m.as_str()[..3]),
                });
            }
        }
    }

    // PII: Credit card pattern (XXXX-XXXX-XXXX-XXXX or XXXX XXXX XXXX XXXX).
    if CC_RE.is_match(text) {
        if let Some(m) = CC_RE.find(text) {
            violations.push(DetectedViolation {
                rule_name: "pii_credit_card",
                severity: "critical",
                detail: format!("Possible credit card detected: {}****", &m.as_str()[..4]),
            });
        }
    }

    // Off-topic detection: keywords outside normal support context.
    let lower = text.to_lowercase();
    let off_topic_keywords = [
        "sports",
        "football",
        "basketball",
        "baseball",
        "soccer",
        "weather forecast",
        "movie",
        "film",
        "tv show",
        "netflix",
        "politics",
        "election",
        "president",
        "congress",
        "recipe",
        "cooking tip",
    ];
    for kw in &off_topic_keywords {
        if lower.contains(kw) {
            violations.push(DetectedViolation {
                rule_name: "off_topic",
                severity: "warning",
                detail: format!("Off-topic content detected: \"{kw}\""),
            });
            break; // One off-topic violation per check is enough.
        }
    }

    // Sentiment monitoring: frustrated/angry customer.
    let negative_keywords = [
        "angry",
        "frustrated",
        "terrible",
        "awful",
        "ridiculous",
        "unacceptable",
        "furious",
        "worst",
        "horrible",
        "disgusting",
        "incompetent",
        "useless",
    ];
    for kw in &negative_keywords {
        if lower.contains(kw) {
            violations.push(DetectedViolation {
                rule_name: "negative_sentiment",
                severity: "info",
                detail: format!("Negative sentiment keyword: \"{kw}\""),
            });
            break;
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// Violation tracker
// ---------------------------------------------------------------------------

/// Tracks violation counts and types across the session.
#[cfg(test)]
struct ViolationTracker {
    total_count: usize,
    by_rule: std::collections::HashMap<String, usize>,
    /// Cooldown: don't re-fire the same rule within N turns.
    last_fired_turn: std::collections::HashMap<String, usize>,
    cooldown_turns: usize,
}

#[cfg(test)]
impl ViolationTracker {
    fn new(cooldown_turns: usize) -> Self {
        Self {
            total_count: 0,
            by_rule: std::collections::HashMap::new(),
            last_fired_turn: std::collections::HashMap::new(),
            cooldown_turns,
        }
    }

    /// Returns true if the rule should fire (not in cooldown).
    fn should_fire(&self, rule_name: &str, current_turn: usize) -> bool {
        match self.last_fired_turn.get(rule_name) {
            Some(&last_turn) => current_turn.saturating_sub(last_turn) >= self.cooldown_turns,
            None => true,
        }
    }

    /// Record that a violation fired.
    fn record(&mut self, rule_name: &str, current_turn: usize) {
        self.total_count += 1;
        *self.by_rule.entry(rule_name.to_string()).or_insert(0) += 1;
        self.last_fired_turn
            .insert(rule_name.to_string(), current_turn);
    }

    /// Generate a JSON summary of violation stats.
    fn summary(&self) -> serde_json::Value {
        json!({
            "total_violations": self.total_count,
            "by_rule": self.by_rule,
        })
    }
}

// ---------------------------------------------------------------------------
// Conversation buffer
// ---------------------------------------------------------------------------

// ConversationBuffer is imported from super (apps/mod.rs)

// ---------------------------------------------------------------------------
// Guardrails app
// ---------------------------------------------------------------------------

const BASE_INSTRUCTION: &str = "\
You are a professional customer support agent. Be helpful, empathetic, and focused on resolving the customer's issue. \
Follow company policies at all times. Never ask for or repeat sensitive personal information like \
Social Security Numbers or credit card numbers. Stay focused on the support topic.";

/// Policy monitoring + corrective injection for Gemini Live conversations.
pub struct Guardrails;

#[async_trait]
impl DemoApp for Guardrails {
    demo_meta! {
        name: "guardrails",
        description: "Policy monitoring + corrective injection for live conversations",
        category: Advanced,
        features: ["voice", "transcription", "guardrails"],
        tips: [
            "Four active policies: PII detection (SSN/credit card), off-topic detection, sentiment monitoring",
            "Try triggering a violation — the system will inject corrective instructions in real time",
            "Watch the devtools Evaluator tab for violation alerts and policy stats",
        ],
        try_saying: [
            "My SSN is 123-45-6789 (triggers PII detection)",
            "Did you see the football game last night? (triggers off-topic)",
            "This is absolutely terrible service! (triggers sentiment alert)",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("Guardrails session starting");

        // Create a ViolationExtractor wrapping check_violations.
        // Guardrails violations are stateless — re-detect each turn, don't accumulate.
        let extractor = Arc::new(RegexExtractor::new(
            "guardrails_state",
            10,
            |text, _existing| {
                let violations = check_violations(text);
                let mut result = HashMap::new();
                for v in &violations {
                    result.insert(
                        format!("violation:{}", v.rule_name),
                        json!({"severity": v.severity, "detail": v.detail}),
                    );
                }
                // Track active violation count
                result.insert("violation_count".into(), json!(violations.len()));
                result
            },
        ));

        let tx_violation = tx.clone();

        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());

                live.model(super::live_model())
                    .voice(voice)
                    .instruction(BASE_INSTRUCTION)
                    .transcription(true, true)
                    .extractor(extractor)
                    // Watcher: detect violations and send to browser.
                    .watch("guardrails_state")
                        .changed()
                        .blocking()
                        .then(move |_old, new, _state| {
                            let tx = tx_violation.clone();
                            async move {
                                if let Some(obj) = new.as_object() {
                                    for (key, val) in obj {
                                        if key.starts_with("violation:") && key != "violation_count" {
                                            let rule = key.strip_prefix("violation:").unwrap_or(key);
                                            let severity = val.get("severity").and_then(|v| v.as_str()).unwrap_or("medium");
                                            let detail = val.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                                            let _ = tx.send(ServerMessage::Violation {
                                                rule: rule.to_string(),
                                                severity: severity.to_string(),
                                                detail: detail.to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        })
                    // Instruction amendment: additively appends corrective instructions based on violations.
                    .instruction_amendment(|state| {
                        let extracted: serde_json::Value = state.get("guardrails_state").unwrap_or(json!({}));
                        let obj = match extracted.as_object() {
                            Some(o) => o,
                            None => return None,
                        };

                        let has_violations = obj.keys().any(|k| k.starts_with("violation:"));
                        if !has_violations {
                            return None;
                        }

                        let mut amendment = String::new();

                        if obj.contains_key("violation:pii_ssn") || obj.contains_key("violation:pii_credit_card") {
                            amendment.push_str("CRITICAL: The user just shared sensitive PII. Do NOT repeat, acknowledge, or reference any SSNs, credit card numbers, or other sensitive data. Respond helpfully without echoing the sensitive information.\n\n");
                        }
                        if obj.contains_key("violation:off_topic") {
                            amendment.push_str("NOTE: The conversation has gone off-topic. Gently redirect back to the main topic. Stay focused and professional.\n\n");
                        }
                        if obj.contains_key("violation:negative_sentiment") {
                            amendment.push_str("NOTE: The user is expressing frustration. Show extra empathy and understanding. Acknowledge their feelings before addressing their concern.\n\n");
                        }

                        if amendment.is_empty() { None } else { Some(amendment.trim().to_string()) }
                    })
                    .on_turn_boundary(move |state, _writer| {
                        async move {
                            let _turn_count: u32 = state.modify("session:turn_count", 0u32, |n| n + 1);
                        }
                    })
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::ConversationBuffer;

    #[test]
    fn detect_ssn() {
        let violations = check_violations("My SSN is 123-45-6789");
        assert!(violations.iter().any(|v| v.rule_name == "pii_ssn"));
    }

    #[test]
    fn detect_ssn_no_dashes() {
        let violations = check_violations("Here is my number 123456789");
        assert!(violations.iter().any(|v| v.rule_name == "pii_ssn"));
    }

    #[test]
    fn detect_ssn_dots() {
        let violations = check_violations("My ID is 123.45.6789");
        assert!(violations.iter().any(|v| v.rule_name == "pii_ssn"));
    }

    #[test]
    fn detect_credit_card() {
        let violations = check_violations("My card is 4111-1111-1111-1111");
        assert!(violations.iter().any(|v| v.rule_name == "pii_credit_card"));
    }

    #[test]
    fn detect_credit_card_spaces() {
        let violations = check_violations("Card number: 4111 1111 1111 1111");
        assert!(violations.iter().any(|v| v.rule_name == "pii_credit_card"));
    }

    #[test]
    fn detect_off_topic() {
        let violations = check_violations("Did you see the football game last night?");
        assert!(violations.iter().any(|v| v.rule_name == "off_topic"));
    }

    #[test]
    fn detect_negative_sentiment() {
        let violations = check_violations("This is absolutely terrible service!");
        assert!(violations
            .iter()
            .any(|v| v.rule_name == "negative_sentiment"));
    }

    #[test]
    fn no_violations_normal_text() {
        let violations = check_violations("I would like to check on my order status please.");
        assert!(violations.is_empty());
    }

    #[test]
    fn tracker_cooldown() {
        let mut tracker = ViolationTracker::new(3);

        // First fire should work.
        assert!(tracker.should_fire("pii_ssn", 1));
        tracker.record("pii_ssn", 1);

        // Within cooldown should not fire.
        assert!(!tracker.should_fire("pii_ssn", 2));
        assert!(!tracker.should_fire("pii_ssn", 3));

        // After cooldown should fire again.
        assert!(tracker.should_fire("pii_ssn", 4));
    }

    #[test]
    fn tracker_different_rules_independent() {
        let mut tracker = ViolationTracker::new(3);

        tracker.record("pii_ssn", 1);
        // Different rule should still fire.
        assert!(tracker.should_fire("off_topic", 1));
    }

    #[test]
    fn tracker_summary() {
        let mut tracker = ViolationTracker::new(3);
        tracker.record("pii_ssn", 1);
        tracker.record("off_topic", 2);
        tracker.record("pii_ssn", 5);

        let summary = tracker.summary();
        assert_eq!(summary["total_violations"], 3);
        assert_eq!(summary["by_rule"]["pii_ssn"], 2);
        assert_eq!(summary["by_rule"]["off_topic"], 1);
    }

    #[test]
    fn conversation_buffer_limits() {
        let mut buf = ConversationBuffer::new(3);
        buf.push("a".into());
        buf.push("b".into());
        buf.push("c".into());
        buf.push("d".into());
        assert_eq!(buf.turns.len(), 3);
        assert_eq!(buf.turns[0], "b");
        assert!(buf.recent_text().contains("b c d"));
    }

    #[test]
    fn no_false_positive_phone_as_ssn() {
        // Phone numbers are 10 digits, SSNs are 9 — the regex checks digit count.
        let violations = check_violations("Call me at 123-456-7890");
        // Should NOT detect as SSN since it's 10 digits.
        assert!(!violations.iter().any(|v| v.rule_name == "pii_ssn"));
    }
}
