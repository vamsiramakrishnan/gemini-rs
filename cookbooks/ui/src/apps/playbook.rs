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

use crate::app::{AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};
use crate::cookbook_meta;

use super::extractors::RegexExtractor;
use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

// ---------------------------------------------------------------------------
// Phase definitions for the customer support state machine
// ---------------------------------------------------------------------------

struct Phase {
    #[cfg_attr(not(test), allow(dead_code))]
    name: &'static str,
    instruction: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
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

static NAME_PATTERNS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i)my name is (\w+)").unwrap(),
        Regex::new(r"(?i)i'?m (\w+)").unwrap(),
        Regex::new(r"(?i)this is (\w+)").unwrap(),
        Regex::new(r"(?i)call me (\w+)").unwrap(),
    ]
});
static ORDER_HASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#(\d{4,})").unwrap());
static ORDER_WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)order\s+(?:number\s+)?(\d{4,})").unwrap());

// ---------------------------------------------------------------------------
// State extraction via keyword/pattern matching
// ---------------------------------------------------------------------------

/// Extract structured state from conversation text using simple pattern matching.
fn extract_state(
    text: &str,
    existing: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut extracted = HashMap::new();
    let lower = text.to_lowercase();

    // Detect customer name: "my name is ___" / "I'm ___" / "this is ___"
    if !existing.contains_key("customer_name") {
        for pat in &*NAME_PATTERNS {
            if let Some(caps) = pat.captures(text) {
                if let Some(name) = caps.get(1) {
                    let name_str = name.as_str();
                    // Skip common false positives.
                    let skip = [
                        "a", "the", "not", "so", "very", "really", "just", "here", "having",
                    ];
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
            "broken",
            "not working",
            "defective",
            "issue",
            "problem",
            "damaged",
            "wrong",
            "missing",
            "late",
            "delayed",
            "refund",
            "return",
            "exchange",
            "complaint",
            "error",
            "failed",
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
        let resolution_keywords = [
            "refund",
            "replacement",
            "fix",
            "credit",
            "exchange",
            "repair",
        ];
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
            "yes",
            "okay",
            "ok",
            "sure",
            "sounds good",
            "i understand",
            "that works",
            "perfect",
            "agreed",
            "confirmed",
            "alright",
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
            "thank you",
            "thanks",
            "satisfied",
            "happy",
            "great",
            "no that's all",
            "nothing else",
            "that's it",
            "all good",
        ];
        for kw in &satisfied_keywords {
            if lower.contains(kw) {
                extracted.insert("satisfied".into(), json!(true));
                break;
            }
        }
    }

    // Detect sentiment.
    let negative = [
        "angry",
        "frustrated",
        "terrible",
        "awful",
        "ridiculous",
        "unacceptable",
        "furious",
    ];
    let positive = [
        "happy",
        "satisfied",
        "great",
        "wonderful",
        "pleased",
        "excellent",
    ];

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
#[cfg(test)]
fn evaluate_phase(
    phase_name: &str,
    state: &HashMap<String, serde_json::Value>,
    turn_count: usize,
) -> (f64, String) {
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
        let present = phase
            .required_keys
            .iter()
            .filter(|k| state.contains_key(**k))
            .count();
        let progress = present as f64 / total_required as f64;
        score = 0.3 + (0.7 * progress); // Base 0.3, up to 1.0 when all keys present.
        notes.push(format!(
            "{present}/{total_required} required keys extracted"
        ));
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

// ConversationBuffer is imported from super (apps/mod.rs)

// ---------------------------------------------------------------------------
// Per-phase context formatter
// ---------------------------------------------------------------------------

fn playbook_context(s: &State) -> String {
    let extracted: serde_json::Value = s.get("playbook_state").unwrap_or(serde_json::json!({}));
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
// Playbook app
// ---------------------------------------------------------------------------

/// State machine + text agent evaluation for customer support playbook.
pub struct Playbook;

#[async_trait]
impl CookbookApp for Playbook {
    cookbook_meta! {
        name: "playbook",
        description: "State machine + text agent evaluation for customer support playbook",
        category: Advanced,
        features: ["voice", "transcription", "state-machine", "evaluation"],
        tips: [
            "The agent follows a 6-phase support flow: greet, identify, investigate, explain, resolve, close",
            "Watch the devtools panel for phase transitions and evaluation scores",
            "Try giving your name and describing a product issue to trigger state transitions",
        ],
        try_saying: [
            "Hi, my name is Alex and I need help with my order.",
            "My order #12345 arrived damaged.",
            "I'd like a refund please.",
        ],
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
            .system_instruction(PHASES[0].instruction);

        // Create a RegexExtractor wrapping the existing extract_state function.
        let extractor = Arc::new(RegexExtractor::new("playbook_state", 10, extract_state));

        // Build Live session with callbacks, extraction, and phase machine.
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

        // Phase on_enter callbacks: send PhaseChange + StateUpdate to the browser.
        // greet -> identify
        let tx_enter_identify = tx.clone();
        // identify -> investigate
        let tx_enter_investigate = tx.clone();
        // investigate -> explain
        let tx_enter_explain = tx.clone();
        // explain -> resolve
        let tx_enter_resolve = tx.clone();
        // resolve -> close
        let tx_enter_close = tx.clone();

        let handle = Live::builder()
            // Model greets the customer immediately on connect
            .greeting("Begin the conversation. Welcome the customer warmly.")
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
            // Phase machine: 6 phases with transition guards based on extracted state.
            .phase_defaults(|d| d.navigation())
            .phase("greet")
            .instruction(PHASES[0].instruction)
            .transition_with(
                "identify",
                |s| {
                    s.get::<serde_json::Value>("playbook_state")
                        .and_then(|v| v.get("customer_name").cloned())
                        .is_some()
                },
                "when customer name is provided",
            )
            .on_enter(move |_state, _writer| {
                async move {
                    // Initial phase — entered at session start, no "from" phase.
                }
            })
            .with_context(playbook_context)
            .done()
            .phase("identify")
            .instruction(PHASES[1].instruction)
            .transition_with(
                "investigate",
                |s| {
                    s.get::<serde_json::Value>("playbook_state")
                        .and_then(|v| v.get("issue_description").cloned())
                        .is_some()
                },
                "when issue description is provided",
            )
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_identify.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greet".into(),
                        to: "identify".into(),
                        reason: "All required keys present".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("identify"),
                    });
                }
            })
            .with_context(playbook_context)
            .done()
            .phase("investigate")
            .instruction(PHASES[2].instruction)
            .transition_with(
                "explain",
                |s| {
                    s.get::<serde_json::Value>("playbook_state")
                        .and_then(|v| v.get("resolution_type").cloned())
                        .is_some()
                },
                "when resolution type is determined",
            )
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_investigate.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "identify".into(),
                        to: "investigate".into(),
                        reason: "All required keys present".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("investigate"),
                    });
                }
            })
            .with_context(playbook_context)
            .done()
            .phase("explain")
            .instruction(PHASES[3].instruction)
            .transition_with(
                "resolve",
                |s| {
                    s.get::<serde_json::Value>("playbook_state")
                        .and_then(|v| v.get("customer_confirmed").cloned())
                        .is_some()
                },
                "when customer confirms understanding",
            )
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_explain.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "investigate".into(),
                        to: "explain".into(),
                        reason: "All required keys present".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("explain"),
                    });
                }
            })
            .with_context(playbook_context)
            .done()
            .phase("resolve")
            .instruction(PHASES[4].instruction)
            .transition_with(
                "close",
                |s| {
                    s.get::<serde_json::Value>("playbook_state")
                        .and_then(|v| v.get("satisfied").cloned())
                        .is_some()
                },
                "when customer is satisfied",
            )
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_resolve.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "explain".into(),
                        to: "resolve".into(),
                        reason: "All required keys present".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("resolve"),
                    });
                }
            })
            .with_context(playbook_context)
            .done()
            .phase("close")
            .instruction(PHASES[5].instruction)
            .terminal()
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_close.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "resolve".into(),
                        to: "close".into(),
                        reason: "All required keys present".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("close"),
                    });
                }
            })
            .with_context(playbook_context)
            .done()
            .initial_phase("greet")
            // Turn boundary: increment turn counter.
            .on_turn_boundary({
                move |state, _writer| async move {
                    let _turn_count: u32 = state.modify("session:turn_count", 0u32, |n| n + 1);
                }
            })
            // Standard voice callbacks.
            .on_audio(move |data| {
                let _ = tx_audio.send(ServerMessage::Audio {
                    data: data.to_vec(),
                });
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
                    info!("Playbook session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("Playbook session connected");

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

        // Send initial phase notification.
        let _ = tx.send(ServerMessage::PhaseChange {
            from: "none".into(),
            to: "greet".into(),
            reason: "Session started".into(),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "phase".into(),
            value: json!("greet"),
        });

        // Browser -> Gemini loop.
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } => match b64.decode(&data) {
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
                },
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error {
                            message: e.to_string(),
                        });
                    }
                }
                ClientMessage::Stop => {
                    info!("Playbook session stopping");
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
        let result = extract_state(
            "The product I received is broken and I want a refund.",
            &state,
        );
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
        assert!(
            score < 1.0,
            "Score should be below 1.0 without required keys"
        );
        assert!(score >= 0.3, "Score should have base of 0.3");
    }

    #[test]
    fn evaluate_greet_phase_with_name() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Alice"));
        let (score, _notes) = evaluate_phase("greet", &state, 2);
        assert!(
            (score - 1.0).abs() < f64::EPSILON,
            "Score should be 1.0 with all keys"
        );
    }

    #[test]
    fn evaluate_penalizes_long_phases() {
        let mut state = HashMap::new();
        state.insert("customer_name".into(), json!("Alice"));
        let (score, notes) = evaluate_phase("greet", &state, 10);
        assert!(score < 1.0, "Score should be penalized for long phase");
        assert!(
            notes.contains("Extended"),
            "Notes should mention extended duration"
        );
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
