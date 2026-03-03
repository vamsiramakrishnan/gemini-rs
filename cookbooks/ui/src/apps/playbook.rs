use std::collections::HashMap;
use std::sync::LazyLock;

use async_trait::async_trait;
use base64::Engine;
use regex::Regex;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rs_genai::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::ConversationBuffer;

use super::{build_session_config, send_app_meta, wait_for_start};

// ---------------------------------------------------------------------------
// Phase definitions for the customer support state machine
// ---------------------------------------------------------------------------

struct Phase {
    name: &'static str,
    instruction: &'static str,
    required_keys: &'static [&'static str],
}

const PHASES: &[Phase] = &[
    Phase {
        name: "greet",
        instruction: "You are a friendly customer support agent. Greet the customer warmly and ask for their name.",
        required_keys: &["customer_name"],
    },
    Phase {
        name: "identify",
        instruction: "You know the customer's name. Ask them to describe their issue and provide an order number if applicable.",
        required_keys: &["issue_description"],
    },
    Phase {
        name: "investigate",
        instruction: "You understand the issue. Investigate by asking clarifying questions. Show empathy.",
        required_keys: &["resolution_type"],
    },
    Phase {
        name: "explain",
        instruction: "Explain the resolution clearly. Make sure the customer understands.",
        required_keys: &["customer_confirmed"],
    },
    Phase {
        name: "resolve",
        instruction: "Confirm the resolution has been applied. Ask if there's anything else.",
        required_keys: &["satisfied"],
    },
    Phase {
        name: "close",
        instruction: "Thank the customer and end the conversation professionally.",
        required_keys: &[],
    },
];

// ---------------------------------------------------------------------------
// Pre-compiled regex patterns
// ---------------------------------------------------------------------------

static NAME_PATTERNS: LazyLock<[Regex; 4]> = LazyLock::new(|| [
    Regex::new(r"(?i)my name is (\w+)").unwrap(),
    Regex::new(r"(?i)i'?m (\w+)").unwrap(),
    Regex::new(r"(?i)this is (\w+)").unwrap(),
    Regex::new(r"(?i)call me (\w+)").unwrap(),
]);
static ORDER_HASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#(\d{4,})").unwrap());
static ORDER_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)order\s+(?:number\s+)?(\d{4,})").unwrap());

// ---------------------------------------------------------------------------
// State extraction via keyword/pattern matching
// ---------------------------------------------------------------------------

/// Extract structured state from conversation text using simple pattern matching.
fn extract_state(text: &str, existing: &HashMap<String, serde_json::Value>) -> HashMap<String, serde_json::Value> {
    let mut extracted = HashMap::new();
    let lower = text.to_lowercase();

    // Detect customer name: "my name is ___" / "I'm ___" / "this is ___"
    if !existing.contains_key("customer_name") {
        for pat in &*NAME_PATTERNS {
            if let Some(caps) = pat.captures(text) {
                if let Some(name) = caps.get(1) {
                    let name_str = name.as_str();
                    // Skip common false positives.
                    let skip = ["a", "the", "not", "so", "very", "really", "just", "here", "having"];
                    if !skip.contains(&name_str.to_lowercase().as_str()) {
                        extracted.insert("customer_name".into(), json!(name_str));
                        break;
                    }
                }
            }
        }
    }

    // Detect order number: #1234 or order 1234 etc.
    if !existing.contains_key("order_number") {
        if let Some(caps) = ORDER_HASH_RE.captures(text) {
            if let Some(num) = caps.get(1) {
                extracted.insert("order_number".into(), json!(num.as_str()));
            }
        } else if let Some(caps) = ORDER_WORD_RE.captures(text) {
            if let Some(num) = caps.get(1) {
                extracted.insert("order_number".into(), json!(num.as_str()));
            }
        }
    }

    // Detect issue description: look for problem-indicating keywords.
    if !existing.contains_key("issue_description") {
        let issue_keywords = [
            "broken", "not working", "defective", "issue", "problem",
            "damaged", "wrong", "missing", "late", "delayed", "refund",
            "return", "exchange", "complaint", "error", "failed",
        ];
        for kw in &issue_keywords {
            if lower.contains(kw) {
                // Capture a short description around the keyword.
                extracted.insert("issue_description".into(), json!(kw));
                break;
            }
        }
    }

    // Detect resolution type: refund, replacement, fix, credit.
    if !existing.contains_key("resolution_type") {
        let resolution_keywords = ["refund", "replacement", "fix", "credit", "exchange", "repair"];
        for kw in &resolution_keywords {
            if lower.contains(kw) {
                extracted.insert("resolution_type".into(), json!(kw));
                break;
            }
        }
    }

    // Detect customer confirmation: yes, okay, sure, confirmed, understand.
    if !existing.contains_key("customer_confirmed") {
        let confirm_keywords = [
            "yes", "okay", "ok", "sure", "sounds good", "i understand",
            "that works", "perfect", "agreed", "confirmed", "alright",
        ];
        for kw in &confirm_keywords {
            if lower.contains(kw) {
                extracted.insert("customer_confirmed".into(), json!(true));
                break;
            }
        }
    }

    // Detect satisfaction.
    if !existing.contains_key("satisfied") {
        let satisfied_keywords = [
            "thank you", "thanks", "satisfied", "happy", "great",
            "no that's all", "nothing else", "that's it", "all good",
        ];
        for kw in &satisfied_keywords {
            if lower.contains(kw) {
                extracted.insert("satisfied".into(), json!(true));
                break;
            }
        }
    }

    // Detect sentiment.
    let negative = ["angry", "frustrated", "terrible", "awful", "ridiculous", "unacceptable", "furious"];
    let positive = ["happy", "satisfied", "great", "wonderful", "pleased", "excellent"];

    for kw in &negative {
        if lower.contains(kw) {
            extracted.insert("sentiment".into(), json!("negative"));
            break;
        }
    }
    if !extracted.contains_key("sentiment") {
        for kw in &positive {
            if lower.contains(kw) {
                extracted.insert("sentiment".into(), json!("positive"));
                break;
            }
        }
    }

    extracted
}

/// Evaluate phase adherence: simple heuristic scoring.
fn evaluate_phase(phase_name: &str, state: &HashMap<String, serde_json::Value>, turn_count: usize) -> (f64, String) {
    let phase = PHASES.iter().find(|p| p.name == phase_name);
    let phase = match phase {
        Some(p) => p,
        None => return (0.5, "Unknown phase".into()),
    };

    let mut score = 1.0;
    let mut notes = Vec::new();

    // Check how many required keys are present.
    let total_required = phase.required_keys.len();
    if total_required > 0 {
        let present = phase.required_keys.iter().filter(|k| state.contains_key(**k)).count();
        let progress = present as f64 / total_required as f64;
        score = 0.3 + (0.7 * progress); // Base 0.3, up to 1.0 when all keys present.
        notes.push(format!("{present}/{total_required} required keys extracted"));
    }

    // Penalize if too many turns in one phase (more than 6 turns suggests stalling).
    if turn_count > 6 {
        score *= 0.8;
        notes.push("Extended phase duration".into());
    }

    // Bonus for detecting sentiment.
    if let Some(sentiment) = state.get("sentiment") {
        if sentiment == "positive" {
            score = (score + 0.1).min(1.0);
            notes.push("Positive sentiment detected".into());
        } else if sentiment == "negative" {
            notes.push("Negative sentiment — extra empathy needed".into());
        }
    }

    (score, notes.join("; "))
}

// ---------------------------------------------------------------------------
// Conversation buffer
// ---------------------------------------------------------------------------

/// Rolling conversation buffer that keeps the last N turns.
// ConversationBuffer is imported from super (apps/mod.rs)

// ---------------------------------------------------------------------------
// Playbook app
// ---------------------------------------------------------------------------

/// State machine + text agent evaluation for customer support playbook.
pub struct Playbook;

#[async_trait]
impl CookbookApp for Playbook {
    fn name(&self) -> &str {
        "playbook"
    }

    fn description(&self) -> &str {
        "State machine + text agent evaluation for customer support playbook"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Advanced
    }

    fn features(&self) -> Vec<String> {
        vec![
            "voice".into(),
            "transcription".into(),
            "state-machine".into(),
            "evaluation".into(),
        ]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "The agent follows a 6-phase support flow: greet, identify, investigate, explain, resolve, close".into(),
            "Watch the devtools panel for phase transitions and evaluation scores".into(),
            "Try giving your name and describing a product issue to trigger state transitions".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "Hi, my name is Alex and I need help with my order.".into(),
            "My order #12345 arrived damaged.".into(),
            "I'd like a refund please.".into(),
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

        // Start in the "greet" phase.
        let initial_phase = &PHASES[0];

        // Build session config for voice mode with the initial phase instruction.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(initial_phase.instruction);

        // Connect to Gemini Live.
        let handle = ConnectBuilder::new(config)
            .build()
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        handle.wait_for_phase(SessionPhase::Active).await;
        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("Playbook session connected");

        // Send initial phase state.
        let _ = tx.send(ServerMessage::PhaseChange {
            from: "none".into(),
            to: initial_phase.name.into(),
            reason: "Session started".into(),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "phase".into(),
            value: json!(initial_phase.name),
        });

        // Subscribe to server events.
        let mut events = handle.subscribe();
        let b64 = base64::engine::general_purpose::STANDARD;

        // State tracking.
        let mut current_phase_idx: usize = 0;
        let mut state: HashMap<String, serde_json::Value> = HashMap::new();
        let mut conversation = ConversationBuffer::new(20);
        let mut phase_turn_count: usize = 0;

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
                            info!("Playbook session stopping");
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
                            phase_turn_count += 1;
                            let _ = tx.send(ServerMessage::TurnComplete);

                            // --- Analyze conversation for state extraction ---
                            let recent = conversation.recent_text();
                            let new_state = extract_state(&recent, &state);

                            // Merge new state and send updates.
                            for (key, value) in &new_state {
                                if !state.contains_key(key) || state.get(key) != Some(value) {
                                    let _ = tx.send(ServerMessage::StateUpdate {
                                        key: key.clone(),
                                        value: value.clone(),
                                    });
                                }
                            }
                            state.extend(new_state);

                            // --- Check phase transition ---
                            if current_phase_idx < PHASES.len() - 1 {
                                let current = &PHASES[current_phase_idx];
                                let all_keys_present = current
                                    .required_keys
                                    .iter()
                                    .all(|k| state.contains_key(*k));

                                if all_keys_present {
                                    // Evaluate the outgoing phase.
                                    let (score, notes) = evaluate_phase(
                                        current.name,
                                        &state,
                                        phase_turn_count,
                                    );
                                    let _ = tx.send(ServerMessage::Evaluation {
                                        phase: current.name.into(),
                                        score,
                                        notes,
                                    });

                                    // Transition to next phase.
                                    let old_name = current.name;
                                    current_phase_idx += 1;
                                    let new_phase = &PHASES[current_phase_idx];
                                    phase_turn_count = 0;

                                    let _ = tx.send(ServerMessage::PhaseChange {
                                        from: old_name.into(),
                                        to: new_phase.name.into(),
                                        reason: format!(
                                            "All required keys present: {:?}",
                                            PHASES[current_phase_idx - 1].required_keys
                                        ),
                                    });
                                    let _ = tx.send(ServerMessage::StateUpdate {
                                        key: "phase".into(),
                                        value: json!(new_phase.name),
                                    });

                                    // Build the new instruction with context.
                                    let customer_name = state
                                        .get("customer_name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("the customer");
                                    let context_instruction = format!(
                                        "{}\n\nCustomer name: {}. Current state: {}",
                                        new_phase.instruction,
                                        customer_name,
                                        serde_json::to_string(&state).unwrap_or_default()
                                    );

                                    info!(
                                        "Phase transition: {} -> {}",
                                        old_name, new_phase.name
                                    );

                                    if let Err(e) = handle
                                        .update_instruction(context_instruction)
                                        .await
                                    {
                                        warn!("Failed to update instruction: {e}");
                                    }
                                }
                            }
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
                            info!("Playbook session disconnected by server");
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
    fn extract_customer_name() {
        let state = HashMap::new();
        let result = extract_state("Hello, my name is Alice and I need help.", &state);
        assert_eq!(result.get("customer_name"), Some(&json!("Alice")));
    }

    #[test]
    fn extract_order_number() {
        let state = HashMap::new();
        let result = extract_state("My order #12345 has not arrived.", &state);
        assert_eq!(result.get("order_number"), Some(&json!("12345")));
    }

    #[test]
    fn extract_order_number_word_form() {
        let state = HashMap::new();
        let result = extract_state("I have order number 98765 that is delayed.", &state);
        assert_eq!(result.get("order_number"), Some(&json!("98765")));
    }

    #[test]
    fn extract_issue_description() {
        let state = HashMap::new();
        let result = extract_state("The product I received is broken and I want a refund.", &state);
        assert_eq!(result.get("issue_description"), Some(&json!("broken")));
    }

    #[test]
    fn extract_resolution_type() {
        let state = HashMap::new();
        let result = extract_state("I would like a refund please.", &state);
        assert_eq!(result.get("resolution_type"), Some(&json!("refund")));
    }

    #[test]
    fn extract_confirmation() {
        let state = HashMap::new();
        let result = extract_state("Yes, that sounds good to me.", &state);
        assert_eq!(result.get("customer_confirmed"), Some(&json!(true)));
    }

    #[test]
    fn extract_satisfaction() {
        let state = HashMap::new();
        let result = extract_state("Thank you so much for your help!", &state);
        assert_eq!(result.get("satisfied"), Some(&json!(true)));
    }

    #[test]
    fn extract_negative_sentiment() {
        let state = HashMap::new();
        let result = extract_state("I am so frustrated with this service!", &state);
        assert_eq!(result.get("sentiment"), Some(&json!("negative")));
    }

    #[test]
    fn extract_positive_sentiment() {
        let state = HashMap::new();
        let result = extract_state("I am very happy with the resolution.", &state);
        assert_eq!(result.get("sentiment"), Some(&json!("positive")));
    }

    #[test]
    fn no_duplicate_extraction() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Bob"));
        let result = extract_state("My name is Alice", &state);
        // Should not overwrite existing key.
        assert!(!result.contains_key("customer_name"));
    }

    #[test]
    fn skips_false_positive_names() {
        let state = HashMap::new();
        let result = extract_state("I'm just looking for help.", &state);
        assert!(!result.contains_key("customer_name"));
    }

    #[test]
    fn evaluate_greet_phase_no_keys() {
        let state = HashMap::new();
        let (score, _notes) = evaluate_phase("greet", &state, 2);
        assert!(score < 1.0, "Score should be below 1.0 without required keys");
        assert!(score >= 0.3, "Score should have base of 0.3");
    }

    #[test]
    fn evaluate_greet_phase_with_name() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Alice"));
        let (score, _notes) = evaluate_phase("greet", &state, 2);
        assert!((score - 1.0).abs() < f64::EPSILON, "Score should be 1.0 with all keys");
    }

    #[test]
    fn evaluate_penalizes_long_phases() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Alice"));
        let (score, notes) = evaluate_phase("greet", &state, 10);
        assert!(score < 1.0, "Score should be penalized for long phase");
        assert!(notes.contains("Extended"), "Notes should mention extended duration");
    }

    #[test]
    fn conversation_buffer_limits_turns() {
        let mut buf = ConversationBuffer::new(3);
        buf.push("one".into());
        buf.push("two".into());
        buf.push("three".into());
        buf.push("four".into());
        assert_eq!(buf.turns.len(), 3);
        assert_eq!(buf.turns[0], "two");
    }
}
