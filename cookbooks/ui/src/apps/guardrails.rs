use std::sync::LazyLock;

use async_trait::async_trait;
use base64::Engine;
use regex::Regex;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rs_genai::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, send_app_meta, wait_for_start, ConversationBuffer};

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
struct ViolationTracker {
    total_count: usize,
    by_rule: std::collections::HashMap<String, usize>,
    /// Cooldown: don't re-fire the same rule within N turns.
    last_fired_turn: std::collections::HashMap<String, usize>,
    cooldown_turns: usize,
}

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

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        // Resolve voice selection (default to Puck).
        let selected_voice = match start.voice.as_deref() {
            Some("Aoede") => Voice::Aoede,
            Some("Charon") => Voice::Charon,
            Some("Fenrir") => Voice::Fenrir,
            Some("Kore") => Voice::Kore,
            Some("Puck") | None => Voice::Puck,
            Some(other) => Voice::Custom(other.to_string()),
        };

        // Build session config for voice mode with base instruction.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(BASE_INSTRUCTION);

        // Connect to Gemini Live.
        let handle = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        handle.wait_for_phase(SessionPhase::Active).await;
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

        // Subscribe to server events.
        let mut events = handle.subscribe();
        let b64 = base64::engine::general_purpose::STANDARD;

        // Tracking state.
        let mut tracker = ViolationTracker::new(3); // 3-turn cooldown per rule
        let mut conversation = ConversationBuffer::new(20);
        let mut turn_count: usize = 0;
        let mut active_corrections: Vec<String> = Vec::new();

        loop {
            tokio::select! {
                // Client -> Gemini
                client_msg = rx.recv() => {
                    match client_msg {
                        Some(ClientMessage::Audio { data }) => {
                            match b64.decode(&data) {
                                Ok(pcm_bytes) => {
                                    if let Err(e) = handle.send_audio(pcm_bytes).await {
                                        warn!("Failed to send audio: {e}");
                                        let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to decode base64 audio: {e}");
                                }
                            }
                        }
                        Some(ClientMessage::Text { text }) => {
                            if let Err(e) = handle.send_text(&text).await {
                                warn!("Failed to send text: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Some(ClientMessage::Stop) | None => {
                            info!("Guardrails session stopping");
                            let _ = handle.disconnect().await;
                            break;
                        }
                        _ => {}
                    }
                }

                // Gemini -> Client
                event = recv_event(&mut events) => {
                    match event {
                        Some(SessionEvent::AudioData(bytes)) => {
                            let encoded = b64.encode(&bytes);
                            let _ = tx.send(ServerMessage::Audio { data: encoded });
                        }
                        Some(SessionEvent::InputTranscription(text)) => {
                            conversation.push(format!("[User] {text}"));
                            let _ = tx.send(ServerMessage::InputTranscription { text });
                        }
                        Some(SessionEvent::OutputTranscription(text)) => {
                            conversation.push(format!("[Agent] {text}"));
                            let _ = tx.send(ServerMessage::OutputTranscription { text });
                        }
                        Some(SessionEvent::TextDelta(text)) => {
                            let _ = tx.send(ServerMessage::TextDelta { text });
                        }
                        Some(SessionEvent::TextComplete(text)) => {
                            conversation.push(format!("[Agent] {text}"));
                            let _ = tx.send(ServerMessage::TextComplete { text });
                        }
                        Some(SessionEvent::TurnComplete) => {
                            turn_count += 1;
                            let _ = tx.send(ServerMessage::TurnComplete);

                            // --- Check for policy violations ---
                            let recent = conversation.recent_text();
                            let violations = check_violations(&recent);

                            let mut instruction_updated = false;
                            let mut new_corrections: Vec<String> = Vec::new();

                            for v in &violations {
                                if tracker.should_fire(v.rule_name, turn_count) {
                                    tracker.record(v.rule_name, turn_count);

                                    // Send violation to browser.
                                    let _ = tx.send(ServerMessage::Violation {
                                        rule: v.rule_name.to_string(),
                                        severity: v.severity.to_string(),
                                        detail: v.detail.clone(),
                                    });

                                    info!(
                                        "Policy violation: {} ({}): {}",
                                        v.rule_name, v.severity, v.detail
                                    );

                                    // Collect corrective instruction if not already active.
                                    let correction = v.corrective_instruction.to_string();
                                    if !active_corrections.contains(&correction) {
                                        new_corrections.push(correction);
                                        instruction_updated = true;
                                    }
                                }
                            }

                            // Update instruction if new corrections were added.
                            if instruction_updated {
                                active_corrections.extend(new_corrections);

                                let full_instruction = format!(
                                    "{}\n\n--- Active Policy Corrections ---\n{}",
                                    BASE_INSTRUCTION,
                                    active_corrections.join("\n\n")
                                );

                                if let Err(e) = handle.update_instruction(full_instruction).await {
                                    warn!("Failed to update instruction: {e}");
                                }
                            }

                            // Send updated violation stats.
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "violations".into(),
                                value: tracker.summary(),
                            });
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "turn_count".into(),
                                value: json!(turn_count),
                            });
                        }
                        Some(SessionEvent::Interrupted) => {
                            let _ = tx.send(ServerMessage::Interrupted);
                        }
                        Some(SessionEvent::VoiceActivityStart) => {
                            let _ = tx.send(ServerMessage::VoiceActivityStart);
                        }
                        Some(SessionEvent::VoiceActivityEnd) => {
                            let _ = tx.send(ServerMessage::VoiceActivityEnd);
                        }
                        Some(SessionEvent::Error(msg)) => {
                            let _ = tx.send(ServerMessage::Error { message: msg });
                        }
                        Some(SessionEvent::Disconnected(_)) => {
                            info!("Guardrails session disconnected by server");
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
