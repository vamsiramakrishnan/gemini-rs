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

use super::{build_session_config, send_app_meta, wait_for_start, ConversationBuffer};

// ---------------------------------------------------------------------------
// Agent definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentKind {
    Billing,
    Technical,
}

impl AgentKind {
    fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Billing => "billing-support",
            AgentKind::Technical => "technical-support",
        }
    }
}

struct AgentPhase {
    name: &'static str,
    instruction: &'static str,
    required_keys: &'static [&'static str],
}

const BILLING_PHASES: &[AgentPhase] = &[
    AgentPhase {
        name: "greet",
        instruction: "You are a friendly billing support agent. Greet the customer warmly and ask for their name.",
        required_keys: &["customer_name"],
    },
    AgentPhase {
        name: "identify",
        instruction: "You know the customer's name. Ask them to describe their billing issue. Are they asking about a charge, requesting a refund, or need a payment plan?",
        required_keys: &["issue_type"],
    },
    AgentPhase {
        name: "billing-investigate",
        instruction: "You are investigating the customer's billing issue. Ask for their order number or account details. Clarify the amount in question and timeframe.",
        required_keys: &["billing_detail"],
    },
    AgentPhase {
        name: "billing-resolve",
        instruction: "You have gathered enough information. Propose a resolution: refund, credit, payment plan, or explanation of charges. Confirm the customer agrees.",
        required_keys: &["resolution_confirmed"],
    },
    AgentPhase {
        name: "close",
        instruction: "The issue is resolved. Thank the customer, provide a reference number, and ask if there's anything else.",
        required_keys: &[],
    },
];

const TECHNICAL_PHASES: &[AgentPhase] = &[
    AgentPhase {
        name: "greet",
        instruction: "You are a friendly technical support agent. The customer has been transferred to you. Introduce yourself as tech support and ask them to describe their technical issue.",
        required_keys: &["tech_issue_desc"],
    },
    AgentPhase {
        name: "tech-identify",
        instruction: "Identify the type of technical issue: device problem, connectivity issue, software bug, or account access. Ask for device/platform details.",
        required_keys: &["tech_category"],
    },
    AgentPhase {
        name: "troubleshoot",
        instruction: "Walk the customer through troubleshooting steps. Start with the basics: restart, clear cache, check connections. Ask if each step resolves the issue.",
        required_keys: &["troubleshoot_result"],
    },
    AgentPhase {
        name: "escalate-or-resolve",
        instruction: "Based on troubleshooting results, either resolve the issue with a fix or escalate to a specialist. If escalating, explain why and set expectations.",
        required_keys: &["final_outcome"],
    },
    AgentPhase {
        name: "close",
        instruction: "The technical issue has been addressed. Summarize what was done, provide a ticket number, and close the conversation.",
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

// ---------------------------------------------------------------------------
// State extraction
// ---------------------------------------------------------------------------

/// Extract structured state from conversation text using pattern matching.
fn extract_state(text: &str, existing: &HashMap<String, serde_json::Value>) -> HashMap<String, serde_json::Value> {
    let mut extracted = HashMap::new();
    let lower = text.to_lowercase();

    // Detect customer name.
    if !existing.contains_key("customer_name") {
        let skip = ["a", "the", "not", "so", "very", "really", "just", "here", "having"];
        for pat in &*NAME_PATTERNS {
            if let Some(caps) = pat.captures(text) {
                if let Some(name) = caps.get(1) {
                    let name_str = name.as_str();
                    if !skip.contains(&name_str.to_lowercase().as_str()) {
                        extracted.insert("customer_name".into(), json!(name_str));
                        break;
                    }
                }
            }
        }
    }

    // Detect issue type: billing vs technical.
    if !existing.contains_key("issue_type") {
        let technical_keywords = [
            "not working", "broken", "crash", "error", "bug", "freeze",
            "slow", "won't load", "can't connect", "wifi", "bluetooth",
            "screen", "password reset", "login", "install", "update",
            "device", "connectivity", "troubleshoot",
        ];
        let billing_keywords = [
            "charge", "refund", "bill", "payment", "invoice", "overcharged",
            "subscription", "cancel", "plan", "pricing", "credit",
            "charged twice", "double charge", "unexpected charge",
        ];

        let tech_count = technical_keywords.iter().filter(|kw| lower.contains(*kw)).count();
        let bill_count = billing_keywords.iter().filter(|kw| lower.contains(*kw)).count();

        if tech_count > 0 || bill_count > 0 {
            if tech_count > bill_count {
                extracted.insert("issue_type".into(), json!("technical"));
            } else {
                extracted.insert("issue_type".into(), json!("billing"));
            }
        }
    }

    // Detect billing detail.
    if !existing.contains_key("billing_detail") {
        let billing_detail_keywords = [
            "refund", "charge", "amount", "dollars", "payment", "credit",
            "invoice", "receipt", "transaction",
        ];
        for kw in &billing_detail_keywords {
            if lower.contains(kw) {
                extracted.insert("billing_detail".into(), json!(kw));
                break;
            }
        }
    }

    // Detect resolution confirmed.
    if !existing.contains_key("resolution_confirmed") {
        let confirm_keywords = [
            "yes", "okay", "ok", "sure", "sounds good", "i agree",
            "that works", "perfect", "go ahead", "confirmed", "alright",
        ];
        for kw in &confirm_keywords {
            if lower.contains(kw) {
                extracted.insert("resolution_confirmed".into(), json!(true));
                break;
            }
        }
    }

    // Detect technical issue description.
    if !existing.contains_key("tech_issue_desc") {
        let tech_issue_keywords = [
            "not working", "broken", "crash", "error", "bug", "freeze",
            "slow", "won't load", "can't connect", "problem",
        ];
        for kw in &tech_issue_keywords {
            if lower.contains(kw) {
                extracted.insert("tech_issue_desc".into(), json!(kw));
                break;
            }
        }
    }

    // Detect technical category.
    if !existing.contains_key("tech_category") {
        if lower.contains("wifi") || lower.contains("internet") || lower.contains("connect") {
            extracted.insert("tech_category".into(), json!("connectivity"));
        } else if lower.contains("screen") || lower.contains("device") || lower.contains("hardware") {
            extracted.insert("tech_category".into(), json!("device"));
        } else if lower.contains("install") || lower.contains("update") || lower.contains("app") {
            extracted.insert("tech_category".into(), json!("software"));
        } else if lower.contains("login") || lower.contains("password") || lower.contains("account") {
            extracted.insert("tech_category".into(), json!("account-access"));
        }
    }

    // Detect troubleshoot result.
    if !existing.contains_key("troubleshoot_result") {
        let resolved_keywords = ["fixed", "works now", "resolved", "that did it", "working now"];
        let unresolved_keywords = ["still broken", "didn't work", "same issue", "no luck", "still not"];
        for kw in &resolved_keywords {
            if lower.contains(kw) {
                extracted.insert("troubleshoot_result".into(), json!("resolved"));
                break;
            }
        }
        if !extracted.contains_key("troubleshoot_result") {
            for kw in &unresolved_keywords {
                if lower.contains(kw) {
                    extracted.insert("troubleshoot_result".into(), json!("unresolved"));
                    break;
                }
            }
        }
    }

    // Detect final outcome.
    if !existing.contains_key("final_outcome") {
        if lower.contains("escalat") {
            extracted.insert("final_outcome".into(), json!("escalated"));
        } else if lower.contains("fixed") || lower.contains("resolved") || lower.contains("working") {
            extracted.insert("final_outcome".into(), json!("resolved"));
        }
    }

    // Detect sentiment.
    let negative = ["angry", "frustrated", "terrible", "awful", "ridiculous", "unacceptable", "furious"];
    let positive = ["happy", "satisfied", "great", "wonderful", "pleased", "excellent", "thank"];
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

/// Evaluate phase adherence with heuristic scoring.
fn evaluate_phase(
    phase_name: &str,
    phases: &[AgentPhase],
    state: &HashMap<String, serde_json::Value>,
    turn_count: usize,
) -> (f64, String) {
    let phase = phases.iter().find(|p| p.name == phase_name);
    let phase = match phase {
        Some(p) => p,
        None => return (0.5, "Unknown phase".into()),
    };

    let mut score = 1.0;
    let mut notes = Vec::new();

    let total_required = phase.required_keys.len();
    if total_required > 0 {
        let present = phase.required_keys.iter().filter(|k| state.contains_key(**k)).count();
        let progress = present as f64 / total_required as f64;
        score = 0.3 + (0.7 * progress);
        notes.push(format!("{present}/{total_required} required keys extracted"));
    }

    if turn_count > 6 {
        score *= 0.8;
        notes.push("Extended phase duration".into());
    }

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

/// Determine if the conversation indicates a handoff from billing to technical.
fn should_handoff_to_technical(state: &HashMap<String, serde_json::Value>) -> bool {
    state.get("issue_type")
        .and_then(|v| v.as_str())
        .map(|t| t == "technical")
        .unwrap_or(false)
}

/// Build a system instruction incorporating current state context.
fn build_instruction(phase: &AgentPhase, state: &HashMap<String, serde_json::Value>) -> String {
    let customer_name = state
        .get("customer_name")
        .and_then(|v| v.as_str())
        .unwrap_or("the customer");

    format!(
        "{}\n\nCustomer name: {}. Current state: {}",
        phase.instruction,
        customer_name,
        serde_json::to_string(state).unwrap_or_default()
    )
}

// ---------------------------------------------------------------------------
// SupportAssistant app
// ---------------------------------------------------------------------------

/// Showcase: Multi-agent handoff + dynamic instructions for customer support.
pub struct SupportAssistant;

#[async_trait]
impl CookbookApp for SupportAssistant {
    fn name(&self) -> &str {
        "support-assistant"
    }

    fn description(&self) -> &str {
        "Multi-agent handoff with billing + technical support flows"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Showcase
    }

    fn features(&self) -> Vec<String> {
        vec![
            "voice".into(),
            "transcription".into(),
            "state-machine".into(),
            "evaluation".into(),
            "guardrails".into(),
            "multi-agent".into(),
        ]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "Starts with a billing agent — describe a technical issue to trigger handoff to technical support".into(),
            "Watch the devtools for agent handoff events and phase tracking across both agents".into(),
            "The system tracks evaluation scores for each phase and handoff quality".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "I'm having trouble with my internet connection.".into(),
            "I was overcharged $50 on my last bill.".into(),
            "My device keeps crashing and won't restart.".into(),
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

        // Start in the billing agent's "greet" phase.
        let mut active_agent = AgentKind::Billing;
        let mut current_phase_idx: usize = 0;
        let initial_phase = &BILLING_PHASES[0];

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
        info!("SupportAssistant session connected");

        // Send initial state.
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "active_agent".into(),
            value: json!(active_agent.as_str()),
        });
        let _ = tx.send(ServerMessage::PhaseChange {
            from: "none".into(),
            to: initial_phase.name.into(),
            reason: "Session started with billing agent".into(),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "phase".into(),
            value: json!(initial_phase.name),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "handoff_count".into(),
            value: json!(0),
        });

        // Subscribe to server events.
        let mut events = handle.subscribe();
        let b64 = base64::engine::general_purpose::STANDARD;

        // State tracking.
        let mut state: HashMap<String, serde_json::Value> = HashMap::new();
        let mut conversation = ConversationBuffer::new(20);
        let mut phase_turn_count: usize = 0;
        let mut handoff_count: usize = 0;
        let mut handoff_done = false;

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
                            info!("SupportAssistant session stopping");
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

                            // --- Extract state from conversation ---
                            let recent = conversation.recent_text();
                            let new_state = extract_state(&recent, &state);

                            for (key, value) in &new_state {
                                if !state.contains_key(key) || state.get(key) != Some(value) {
                                    let _ = tx.send(ServerMessage::StateUpdate {
                                        key: key.clone(),
                                        value: value.clone(),
                                    });
                                }
                            }
                            state.extend(new_state);

                            // --- Check for agent handoff ---
                            // During billing "identify" phase, if issue is technical, handoff.
                            let active_phases = match active_agent {
                                AgentKind::Billing => BILLING_PHASES,
                                AgentKind::Technical => TECHNICAL_PHASES,
                            };

                            if active_agent == AgentKind::Billing
                                && !handoff_done
                                && current_phase_idx == 1 // "identify" phase
                                && should_handoff_to_technical(&state)
                            {
                                // Evaluate the outgoing phase before handoff.
                                let (score, notes) = evaluate_phase(
                                    active_phases[current_phase_idx].name,
                                    active_phases,
                                    &state,
                                    phase_turn_count,
                                );
                                let _ = tx.send(ServerMessage::Evaluation {
                                    phase: "billing:identify".into(),
                                    score,
                                    notes: format!("{notes}; triggering handoff to technical"),
                                });

                                // Perform handoff.
                                handoff_count += 1;
                                handoff_done = true;
                                active_agent = AgentKind::Technical;
                                current_phase_idx = 0;
                                phase_turn_count = 0;

                                let _ = tx.send(ServerMessage::PhaseChange {
                                    from: "billing".into(),
                                    to: "technical".into(),
                                    reason: "Technical issue detected".into(),
                                });
                                let _ = tx.send(ServerMessage::StateUpdate {
                                    key: "active_agent".into(),
                                    value: json!("technical-support"),
                                });
                                let _ = tx.send(ServerMessage::StateUpdate {
                                    key: "handoff_count".into(),
                                    value: json!(handoff_count),
                                });
                                let _ = tx.send(ServerMessage::StateUpdate {
                                    key: "phase".into(),
                                    value: json!(TECHNICAL_PHASES[0].name),
                                });

                                // Evaluate handoff quality.
                                let context_preserved = state.contains_key("customer_name");
                                let _ = tx.send(ServerMessage::Evaluation {
                                    phase: "handoff".into(),
                                    score: if context_preserved { 0.9 } else { 0.6 },
                                    notes: format!(
                                        "Handoff #{handoff_count}: billing -> technical. Context preserved: {context_preserved}"
                                    ),
                                });

                                // Update system instruction to technical support context.
                                let tech_instruction = build_instruction(
                                    &TECHNICAL_PHASES[0],
                                    &state,
                                );
                                info!("Agent handoff: billing -> technical");
                                if let Err(e) = handle.update_instruction(tech_instruction).await {
                                    warn!("Failed to update instruction for handoff: {e}");
                                }

                                continue;
                            }

                            // --- Check for escalation in technical agent ---
                            if active_agent == AgentKind::Technical
                                && current_phase_idx == 3 // "escalate-or-resolve" phase
                            {
                                if let Some(outcome) = state.get("final_outcome") {
                                    if outcome == "escalated" {
                                        let _ = tx.send(ServerMessage::StateUpdate {
                                            key: "escalation".into(),
                                            value: json!({
                                                "reason": "Troubleshooting steps exhausted",
                                                "priority": if state.get("sentiment") == Some(&json!("negative")) {
                                                    "high"
                                                } else {
                                                    "normal"
                                                }
                                            }),
                                        });
                                    }
                                }
                            }

                            // --- Check phase transition within current agent ---
                            let active_phases = match active_agent {
                                AgentKind::Billing => BILLING_PHASES,
                                AgentKind::Technical => TECHNICAL_PHASES,
                            };

                            if current_phase_idx < active_phases.len() - 1 {
                                let current = &active_phases[current_phase_idx];
                                let all_keys_present = current
                                    .required_keys
                                    .iter()
                                    .all(|k| state.contains_key(*k));

                                if all_keys_present {
                                    let (score, notes) = evaluate_phase(
                                        current.name,
                                        active_phases,
                                        &state,
                                        phase_turn_count,
                                    );
                                    let phase_label = format!(
                                        "{}:{}",
                                        active_agent.as_str(),
                                        current.name
                                    );
                                    let _ = tx.send(ServerMessage::Evaluation {
                                        phase: phase_label,
                                        score,
                                        notes,
                                    });

                                    let old_name = current.name;
                                    current_phase_idx += 1;
                                    let new_phase = &active_phases[current_phase_idx];
                                    phase_turn_count = 0;

                                    let _ = tx.send(ServerMessage::PhaseChange {
                                        from: old_name.into(),
                                        to: new_phase.name.into(),
                                        reason: format!(
                                            "All required keys present: {:?}",
                                            active_phases[current_phase_idx - 1].required_keys
                                        ),
                                    });
                                    let _ = tx.send(ServerMessage::StateUpdate {
                                        key: "phase".into(),
                                        value: json!(new_phase.name),
                                    });

                                    let context_instruction = build_instruction(new_phase, &state);
                                    info!(
                                        "[{}] Phase transition: {} -> {}",
                                        active_agent.as_str(),
                                        old_name,
                                        new_phase.name
                                    );

                                    if let Err(e) = handle.update_instruction(context_instruction).await {
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
                            info!("SupportAssistant session disconnected by server");
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
    fn extract_billing_issue_type() {
        let state = HashMap::new();
        let result = extract_state("I was overcharged on my last bill.", &state);
        assert_eq!(result.get("issue_type"), Some(&json!("billing")));
    }

    #[test]
    fn extract_technical_issue_type() {
        let state = HashMap::new();
        let result = extract_state("My device is not working and keeps crashing.", &state);
        assert_eq!(result.get("issue_type"), Some(&json!("technical")));
    }

    #[test]
    fn handoff_triggers_for_technical() {
        let mut state = HashMap::new();
        state.insert("issue_type".into(), json!("technical"));
        assert!(should_handoff_to_technical(&state));
    }

    #[test]
    fn no_handoff_for_billing() {
        let mut state = HashMap::new();
        state.insert("issue_type".into(), json!("billing"));
        assert!(!should_handoff_to_technical(&state));
    }

    #[test]
    fn extract_customer_name() {
        let state = HashMap::new();
        let result = extract_state("Hello, my name is Sarah and I need help.", &state);
        assert_eq!(result.get("customer_name"), Some(&json!("Sarah")));
    }

    #[test]
    fn extract_tech_category_connectivity() {
        let state = HashMap::new();
        let result = extract_state("I can't connect to the wifi.", &state);
        assert_eq!(result.get("tech_category"), Some(&json!("connectivity")));
    }

    #[test]
    fn extract_tech_category_device() {
        let state = HashMap::new();
        let result = extract_state("My screen is broken on the device.", &state);
        assert_eq!(result.get("tech_category"), Some(&json!("device")));
    }

    #[test]
    fn extract_tech_category_software() {
        let state = HashMap::new();
        let result = extract_state("The app won't install properly.", &state);
        assert_eq!(result.get("tech_category"), Some(&json!("software")));
    }

    #[test]
    fn extract_tech_category_account() {
        let state = HashMap::new();
        let result = extract_state("I forgot my password and can't login.", &state);
        assert_eq!(result.get("tech_category"), Some(&json!("account-access")));
    }

    #[test]
    fn extract_troubleshoot_resolved() {
        let state = HashMap::new();
        let result = extract_state("That fixed it! It works now.", &state);
        assert_eq!(result.get("troubleshoot_result"), Some(&json!("resolved")));
    }

    #[test]
    fn extract_troubleshoot_unresolved() {
        let state = HashMap::new();
        let result = extract_state("It still broken, same issue persists.", &state);
        assert_eq!(result.get("troubleshoot_result"), Some(&json!("unresolved")));
    }

    #[test]
    fn extract_final_outcome_escalated() {
        let state = HashMap::new();
        let result = extract_state("This needs to be escalated to a specialist.", &state);
        assert_eq!(result.get("final_outcome"), Some(&json!("escalated")));
    }

    #[test]
    fn extract_billing_detail() {
        let state = HashMap::new();
        let result = extract_state("I want a refund for the charge.", &state);
        assert!(result.contains_key("billing_detail"));
    }

    #[test]
    fn extract_resolution_confirmed() {
        let state = HashMap::new();
        let result = extract_state("Sure, that sounds good to me.", &state);
        assert_eq!(result.get("resolution_confirmed"), Some(&json!(true)));
    }

    #[test]
    fn evaluate_billing_greet_no_keys() {
        let state = HashMap::new();
        let (score, _notes) = evaluate_phase("greet", BILLING_PHASES, &state, 2);
        assert!(score < 1.0);
        assert!(score >= 0.3);
    }

    #[test]
    fn evaluate_billing_greet_with_name() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Alice"));
        let (score, _notes) = evaluate_phase("greet", BILLING_PHASES, &state, 2);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evaluate_penalizes_long_phase() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Alice"));
        let (score, notes) = evaluate_phase("greet", BILLING_PHASES, &state, 10);
        assert!(score < 1.0);
        assert!(notes.contains("Extended"));
    }

    #[test]
    fn no_duplicate_name_extraction() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Bob"));
        let result = extract_state("My name is Alice", &state);
        assert!(!result.contains_key("customer_name"));
    }

    #[test]
    fn skips_false_positive_names() {
        let state = HashMap::new();
        let result = extract_state("I'm just looking for help.", &state);
        assert!(!result.contains_key("customer_name"));
    }

    #[test]
    fn app_metadata() {
        let app = SupportAssistant;
        assert_eq!(app.name(), "support-assistant");
        assert_eq!(app.category(), AppCategory::Showcase);
        assert!(app.features().contains(&"multi-agent".to_string()));
        assert!(app.features().contains(&"state-machine".to_string()));
    }
}
