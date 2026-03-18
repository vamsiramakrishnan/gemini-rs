use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::info;

use adk_rs_fluent::prelude::*;

use crate::app::{AppError, ClientMessage, DemoApp, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

use super::extractors::RegexExtractor;
use super::resolve_voice;

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
impl DemoApp for Playbook {
    demo_meta! {
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
        info!("Playbook session starting");

        let extractor = Arc::new(RegexExtractor::new("playbook_state", 10, extract_state));

        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());

                // =================================================================
                // DESIGN DISSECTION: Why this app is built the way it is
                // =================================================================
                //
                // Steering Mode: ContextInjection
                //   The support agent persona is consistent across all 6 phases.
                //   Each phase changes focus (greet, identify, investigate,
                //   explain, resolve, close) but the personality stays the same.
                //   ContextInjection avoids the overhead of replacing the system
                //   instruction 6 times.
                //
                // greeting("..."):
                //   The model starts the conversation with a warm welcome. This
                //   fires once at connect — before any phase evaluation runs.
                //
                // NO prompt_on_enter on any phase:
                //   All transitions are customer-driven. The customer gives their
                //   name (→ identify), describes the issue (→ investigate),
                //   mentions a resolution type (→ explain), confirms (→ resolve),
                //   expresses satisfaction (→ close). The model responds naturally
                //   at each step without needing an explicit prompt.
                //
                // NO enter_prompt on any phase:
                //   This is the simplest correct pattern — the model has enough
                //   context from with_context(playbook_context) to maintain
                //   continuity. enter_prompt would be useful if transitions felt
                //   abrupt, but with ContextInjection the phase instruction
                //   itself provides sufficient direction.
                //
                // RegexExtractor for state extraction:
                //   Uses pattern matching (not LLM extraction) to detect customer
                //   name, issue type, resolution preferences, etc. This is faster
                //   and cheaper than LLM extraction, appropriate for structured
                //   data that follows predictable patterns.
                //
                // State-based transitions only:
                //   Pure state guards with no turn-count fallbacks. The 6-phase
                //   flow relies entirely on the LLM's natural conversational
                //   ability to gather the required info.
                // =================================================================
                live.model(GeminiModel::Gemini2_0FlashLive)
                    .voice(voice)
                    .transcription(true, true)
                    .steering_mode(SteeringMode::ContextInjection)
                    // Model greets the customer immediately on connect
                    .greeting("Begin the conversation. Welcome the customer warmly.")
                    .extractor(extractor)
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
                    .with_context(playbook_context)
                    .done()
                    .phase("close")
                    .instruction(PHASES[5].instruction)
                    .terminal()
                    .with_context(playbook_context)
                    .done()
                    .initial_phase("greet")
                    // Turn boundary: increment turn counter.
                    .on_turn_boundary(move |state, _writer| async move {
                        let _turn_count: u32 = state.modify("session:turn_count", 0u32, |n| n + 1);
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
