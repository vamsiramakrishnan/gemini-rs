use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

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
// Agent definitions
// ---------------------------------------------------------------------------


struct AgentPhase {
    #[cfg_attr(not(test), allow(dead_code))]
    name: &'static str,
    instruction: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
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
#[cfg(test)]
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
#[cfg(test)]
fn should_handoff_to_technical(state: &HashMap<String, serde_json::Value>) -> bool {
    state.get("issue_type")
        .and_then(|v| v.as_str())
        .map(|t| t == "technical")
        .unwrap_or(false)
}


// ---------------------------------------------------------------------------
// Per-phase context formatter
// ---------------------------------------------------------------------------

fn support_context(s: &State) -> String {
    let extracted: serde_json::Value = s.get("support_state").unwrap_or(serde_json::json!({}));
    let customer_name = extracted
        .get("customer_name")
        .and_then(|v| v.as_str())
        .unwrap_or("the customer");
    format!(
        "Customer name: {}. Current state: {}",
        customer_name, extracted
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
        let selected_voice = resolve_voice(start.voice.as_deref());

        // Build session config for voice mode with the initial phase instruction.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(BILLING_PHASES[0].instruction);

        // Create a RegexExtractor wrapping the existing extract_state function.
        let extractor = Arc::new(RegexExtractor::new("support_state", 10, |text, existing| {
            extract_state(text, existing)
        }));

        // Build Live session with callbacks, extraction, and phase machine.
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

        // Phase on_enter callbacks.
        let tx_enter_billing_identify = tx.clone();
        let tx_enter_tech_greet = tx.clone();
        let tx_enter_billing_investigate = tx.clone();
        let tx_enter_billing_resolve = tx.clone();
        let tx_enter_billing_close = tx.clone();
        let tx_enter_tech_identify = tx.clone();
        let tx_enter_tech_troubleshoot = tx.clone();
        let tx_enter_tech_resolve = tx.clone();
        let tx_enter_tech_close = tx.clone();

        let handle = Live::builder()
            // Model greets the caller immediately on connect
            .greeting("Greet the caller warmly and ask how you can help them today.")
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
            // Computed state: active_agent derived from support_state.
            .computed("active_agent", &["support_state"], |state| {
                let extracted: serde_json::Value = state.get("support_state").unwrap_or(json!({}));
                let issue_type = extracted.get("issue_type").and_then(|v| v.as_str());
                match issue_type {
                    Some("technical") => Some(json!("technical-support")),
                    _ => Some(json!("billing-support")),
                }
            })
            // --- Billing Phases ---
            .phase("billing:greet")
                .instruction(BILLING_PHASES[0].instruction)
                .transition("billing:identify", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("customer_name").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    async move {
                        // Initial phase — entered at session start, no "from" phase.
                    }
                })
                .with_context(support_context)
                .done()
            .phase("billing:identify")
                .instruction(BILLING_PHASES[1].instruction)
                // Tech handoff transition FIRST (priority over billing:investigate).
                .transition("tech:greet", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("issue_type").cloned())
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .map(|t| t == "technical")
                        .unwrap_or(false)
                })
                // Then billing:investigate for any other issue_type.
                .transition("billing:investigate", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("issue_type").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_billing_identify.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "billing:greet".into(),
                            to: "billing:identify".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("billing:identify"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .phase("billing:investigate")
                .instruction(BILLING_PHASES[2].instruction)
                .transition("billing:resolve", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("billing_detail").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_billing_investigate.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "billing:identify".into(),
                            to: "billing:investigate".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("billing:investigate"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .phase("billing:resolve")
                .instruction(BILLING_PHASES[3].instruction)
                .transition("billing:close", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("resolution_confirmed").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_billing_resolve.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "billing:investigate".into(),
                            to: "billing:resolve".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("billing:resolve"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .phase("billing:close")
                .instruction(BILLING_PHASES[4].instruction)
                .terminal()
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_billing_close.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "billing:resolve".into(),
                            to: "billing:close".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("billing:close"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            // --- Technical Phases ---
            .phase("tech:greet")
                .instruction(TECHNICAL_PHASES[0].instruction)
                .transition("tech:identify", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("tech_issue_desc").cloned())
                        .is_some()
                })
                .on_enter({
                    let tx = tx_enter_tech_greet.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "billing:identify".into(),
                                to: "tech:greet".into(),
                                reason: "Technical issue detected — transferring to technical support".into(),
                            });
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "active_agent".into(),
                                value: json!("technical-support"),
                            });
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "phase".into(),
                                value: json!("tech:greet"),
                            });
                        }
                    }
                })
                .with_context(support_context)
                .done()
            .phase("tech:identify")
                .instruction(TECHNICAL_PHASES[1].instruction)
                .transition("tech:troubleshoot", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("tech_category").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_tech_identify.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "tech:greet".into(),
                            to: "tech:identify".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("tech:identify"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .phase("tech:troubleshoot")
                .instruction(TECHNICAL_PHASES[2].instruction)
                .transition("tech:resolve", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("troubleshoot_result").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_tech_troubleshoot.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "tech:identify".into(),
                            to: "tech:troubleshoot".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("tech:troubleshoot"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .phase("tech:resolve")
                .instruction(TECHNICAL_PHASES[3].instruction)
                .transition("tech:close", |s| {
                    s.get::<serde_json::Value>("support_state")
                        .and_then(|v| v.get("final_outcome").cloned())
                        .is_some()
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_tech_resolve.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "tech:troubleshoot".into(),
                            to: "tech:resolve".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("tech:resolve"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .phase("tech:close")
                .instruction(TECHNICAL_PHASES[4].instruction)
                .terminal()
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_tech_close.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "tech:resolve".into(),
                            to: "tech:close".into(),
                            reason: "All required keys present".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("tech:close"),
                        });
                    }
                })
                .with_context(support_context)
                .done()
            .initial_phase("billing:greet")
            // Watch for escalation.
            .watch("support_state")
                .changed()
                .then({
                    let tx = tx.clone();
                    move |_old, new, _state| {
                        let tx = tx.clone();
                        async move {
                            if let Some(outcome) = new.as_object()
                                .and_then(|obj| obj.get("final_outcome"))
                                .and_then(|v| v.as_str())
                            {
                                if outcome == "escalated" {
                                    let _ = tx.send(ServerMessage::StateUpdate {
                                        key: "escalation".into(),
                                        value: json!({"priority": "high", "reason": "Customer issue escalated"}),
                                    });
                                }
                            }
                        }
                    }
                })
            .on_turn_boundary({
                let tx = tx.clone();
                move |state, _writer| {
                    let tx = tx.clone();
                    async move {
                        let turn_count: u32 = state.modify("session:turn_count", 0u32, |n| n + 1);
                        let current_phase: String = state.get("session:phase").unwrap_or_default();
                        let active_agent: String = state.get("active_agent").unwrap_or_else(|| "billing-support".to_string());

                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "turn_count".into(),
                            value: json!(turn_count),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "current_phase".into(),
                            value: json!(current_phase),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "active_agent".into(),
                            value: json!(active_agent),
                        });
                    }
                }
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
                    info!("SupportAssistant session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("SupportAssistant session connected");

        // Send initial state.
        let _ = tx.send(ServerMessage::PhaseChange {
            from: "none".into(),
            to: "billing:greet".into(),
            reason: "Session started".into(),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "active_agent".into(),
            value: json!("billing-support"),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "phase".into(),
            value: json!("billing:greet"),
        });

        // Periodic telemetry sender (auto-collected by SDK telemetry lane)
        let telem = handle.telemetry().clone();
        let tx_telem = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                let stats = telem.snapshot();
                if tx_telem.send(ServerMessage::Telemetry { stats }).is_err() {
                    break;
                }
            }
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
                    info!("SupportAssistant session stopping");
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
