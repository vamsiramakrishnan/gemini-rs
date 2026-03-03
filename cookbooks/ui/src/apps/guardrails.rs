use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use base64::Engine;
use regex::Regex;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::extractors::RegexExtractor;
use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

// ---------------------------------------------------------------------------
// Policy rules
// ---------------------------------------------------------------------------

/// A policy rule that can detect violations and produce corrective instructions.
#[allow(dead_code)]
struct PolicyRule {
    name: &'static str,
    severity: &'static str,
    /// Human-readable description of what this rule detects.
    description: &'static str,
    /// Instruction injected when a violation is detected.
    corrective_instruction: &'static str,
}

const POLICIES: &[PolicyRule] = &[
    PolicyRule {
        name: "pii_ssn",
        severity: "critical",
        description: "Social Security Number detected in conversation",
        corrective_instruction: "IMPORTANT: Do NOT ask for, repeat, or acknowledge any Social Security Numbers or government ID numbers. If the customer has shared one, advise them to never share such information in a chat. Redirect the conversation to resolving their issue through safe verification methods.",
    },
    PolicyRule {
        name: "pii_credit_card",
        severity: "critical",
        description: "Credit card number detected in conversation",
        corrective_instruction: "IMPORTANT: Do NOT ask for, repeat, or acknowledge any credit card numbers or payment card details. If the customer has shared one, advise them to never share such information in a chat. Use only secure payment processing channels.",
    },
    PolicyRule {
        name: "off_topic",
        severity: "warning",
        description: "Conversation drifted to off-topic subjects",
        corrective_instruction: "Please stay focused on the customer's support issue. Politely redirect the conversation back to resolving their problem if it drifts to unrelated topics.",
    },
    PolicyRule {
        name: "negative_sentiment",
        severity: "info",
        description: "Customer appears frustrated or upset",
        corrective_instruction: "The customer seems upset. Show extra empathy and patience. Acknowledge their frustration, apologize for the inconvenience, and focus on finding a resolution quickly.",
    },
];

// ---------------------------------------------------------------------------
// Violation detection
// ---------------------------------------------------------------------------

/// Detected violation with matched detail.
#[allow(dead_code)]
struct DetectedViolation {
    rule_name: &'static str,
    severity: &'static str,
    detail: String,
    corrective_instruction: &'static str,
}

// Pre-compiled regex patterns for violation detection.
static SSN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{3}[-.]?\d{2}[-.]?\d{4}\b").unwrap());
static CC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{4}[\s-]?\d{4}[\s-]?\d{4}[\s-]?\d{4}\b").unwrap());

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
                    corrective_instruction: POLICIES[0].corrective_instruction,
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
                corrective_instruction: POLICIES[1].corrective_instruction,
            });
        }
    }

    // Off-topic detection: keywords outside normal support context.
    let lower = text.to_lowercase();
    let off_topic_keywords = [
        "sports", "football", "basketball", "baseball", "soccer",
        "weather forecast", "movie", "film", "tv show", "netflix",
        "politics", "election", "president", "congress",
        "recipe", "cooking tip",
    ];
    for kw in &off_topic_keywords {
        if lower.contains(kw) {
            violations.push(DetectedViolation {
                rule_name: "off_topic",
                severity: "warning",
                detail: format!("Off-topic content detected: \"{kw}\""),
                corrective_instruction: POLICIES[2].corrective_instruction,
            });
            break; // One off-topic violation per check is enough.
        }
    }

    // Sentiment monitoring: frustrated/angry customer.
    let negative_keywords = [
        "angry", "frustrated", "terrible", "awful", "ridiculous",
        "unacceptable", "furious", "worst", "horrible", "disgusting",
        "incompetent", "useless",
    ];
    for kw in &negative_keywords {
        if lower.contains(kw) {
            violations.push(DetectedViolation {
                rule_name: "negative_sentiment",
                severity: "info",
                detail: format!("Negative sentiment keyword: \"{kw}\""),
                corrective_instruction: POLICIES[3].corrective_instruction,
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
#[allow(dead_code)]
struct ViolationTracker {
    total_count: usize,
    by_rule: std::collections::HashMap<String, usize>,
    /// Cooldown: don't re-fire the same rule within N turns.
    last_fired_turn: std::collections::HashMap<String, usize>,
    cooldown_turns: usize,
}

#[allow(dead_code)]
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
        self.last_fired_turn.insert(rule_name.to_string(), current_turn);
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

/// Rolling conversation buffer that keeps the last N turns.
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
impl CookbookApp for Guardrails {
    fn name(&self) -> &str {
        "guardrails"
    }

    fn description(&self) -> &str {
        "Policy monitoring + corrective injection for live conversations"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Advanced
    }

    fn features(&self) -> Vec<String> {
        vec![
            "voice".into(),
            "transcription".into(),
            "guardrails".into(),
        ]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "Four active policies: PII detection (SSN/credit card), off-topic detection, sentiment monitoring".into(),
            "Try triggering a violation — the system will inject corrective instructions in real time".into(),
            "Watch the devtools Evaluator tab for violation alerts and policy stats".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "My SSN is 123-45-6789 (triggers PII detection)".into(),
            "Did you see the football game last night? (triggers off-topic)".into(),
            "This is absolutely terrible service! (triggers sentiment alert)".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        // Resolve voice selection (default to Puck).
        let selected_voice = resolve_voice(start.voice.as_deref());

        // Build session config for voice mode with base instruction.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(BASE_INSTRUCTION);

        // Create a ViolationExtractor wrapping check_violations.
        // Guardrails violations are stateless — re-detect each turn, don't accumulate.
        let extractor = Arc::new(RegexExtractor::new("guardrails_state", 10, |text, _existing| {
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
        }));

        // Build Live session with callbacks, extraction, watchers, and instruction template.
        let b64 = base64::engine::general_purpose::STANDARD;

        let tx_audio = tx.clone();
        let tx_input = tx.clone();
        let tx_output = tx.clone();
        let tx_text = tx.clone();
        let tx_text_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupted = tx.clone();
        let tx_vad_start = tx.clone();
        let tx_vad_end = tx.clone();
        let tx_error = tx.clone();
        let tx_disconnected = tx.clone();

        let handle = Live::builder()
            .extractor(extractor)
            .on_extracted({
                let tx = tx.clone();
                move |_name, value| {
                    let tx = tx.clone();
                    async move {
                        if let Some(obj) = value.as_object() {
                            for (key, val) in obj {
                                let _ = tx.send(ServerMessage::StateUpdate {
                                    key: key.clone(),
                                    value: val.clone(),
                                });
                            }
                        }
                    }
                }
            })
            // Watcher: detect violations and send to browser.
            .watch("guardrails_state")
                .changed()
                .blocking()
                .then({
                    let tx = tx.clone();
                    move |_old, new, _state| {
                        let tx = tx.clone();
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
                    }
                })
            // State-reactive instruction template for corrective instruction injection.
            .instruction_template(|state| {
                let extracted: serde_json::Value = state.get("guardrails_state").unwrap_or(json!({}));
                let has_violations = extracted.as_object()
                    .map(|obj| obj.keys().any(|k| k.starts_with("violation:")))
                    .unwrap_or(false);

                if !has_violations {
                    return Some(BASE_INSTRUCTION.to_string());
                }

                let mut instruction = BASE_INSTRUCTION.to_string();
                let obj = extracted.as_object().unwrap();

                if obj.contains_key("violation:pii_ssn") || obj.contains_key("violation:pii_credit_card") {
                    instruction.push_str("\n\nCRITICAL: The user just shared sensitive PII. Do NOT repeat, acknowledge, or reference any SSNs, credit card numbers, or other sensitive data. Respond helpfully without echoing the sensitive information.");
                }
                if obj.contains_key("violation:off_topic") {
                    instruction.push_str("\n\nNOTE: The conversation has gone off-topic. Gently redirect back to the main topic. Stay focused and professional.");
                }
                if obj.contains_key("violation:negative_sentiment") {
                    instruction.push_str("\n\nNOTE: The user is expressing frustration. Show extra empathy and understanding. Acknowledge their feelings before addressing their concern.");
                }

                Some(instruction)
            })
            // Standard voice callbacks.
            .on_audio(move |data| {
                let encoded = b64.encode(data);
                let _ = tx_audio.send(ServerMessage::Audio { data: encoded });
            })
            .on_input_transcript(move |text, _is_final| {
                let _ = tx_input.send(ServerMessage::InputTranscription {
                    text: text.to_string(),
                });
            })
            .on_output_transcript(move |text, _is_final| {
                let _ = tx_output.send(ServerMessage::OutputTranscription {
                    text: text.to_string(),
                });
            })
            .on_text(move |t| {
                let _ = tx_text.send(ServerMessage::TextDelta {
                    text: t.to_string(),
                });
            })
            .on_text_complete(move |t| {
                let _ = tx_text_complete.send(ServerMessage::TextComplete {
                    text: t.to_string(),
                });
            })
            .on_turn_complete(move || {
                let tx = tx_turn.clone();
                async move {
                    let _ = tx.send(ServerMessage::TurnComplete);
                }
            })
            .on_interrupted(move || {
                let tx = tx_interrupted.clone();
                async move {
                    let _ = tx.send(ServerMessage::Interrupted);
                }
            })
            .on_vad_start(move || {
                let _ = tx_vad_start.send(ServerMessage::VoiceActivityStart);
            })
            .on_vad_end(move || {
                let _ = tx_vad_end.send(ServerMessage::VoiceActivityEnd);
            })
            .on_error(move |msg| {
                let tx = tx_error.clone();
                async move {
                    let _ = tx.send(ServerMessage::Error { message: msg });
                }
            })
            .on_disconnected(move |_reason| {
                let _tx = tx_disconnected.clone();
                async move {
                    info!("Guardrails session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("Guardrails session connected");

        // Send initial state.
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "active_policies".into(),
            value: json!(POLICIES.iter().map(|p| p.name).collect::<Vec<_>>()),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "violations".into(),
            value: json!({"total_violations": 0, "by_rule": {}}),
        });

        // Browser -> Gemini loop.
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } => {
                    match b64.decode(&data) {
                        Ok(pcm_bytes) => {
                            if let Err(e) = handle.send_audio(pcm_bytes).await {
                                warn!("Failed to send audio: {e}");
                                let _ = tx.send(ServerMessage::Error {
                                    message: e.to_string(),
                                });
                            }
                        }
                        Err(e) => {
                            warn!("Failed to decode base64 audio: {e}");
                        }
                    }
                }
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error {
                            message: e.to_string(),
                        });
                    }
                }
                ClientMessage::Stop => {
                    info!("Guardrails session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        Ok(())
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
        assert!(violations.iter().any(|v| v.rule_name == "negative_sentiment"));
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
