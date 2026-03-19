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

static NAME_PATTERNS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i)my name is (\w+)").unwrap(),
        Regex::new(r"(?i)i'?m (\w+)").unwrap(),
        Regex::new(r"(?i)this is (\w+)").unwrap(),
        Regex::new(r"(?i)call me (\w+)").unwrap(),
    ]
});

// ---------------------------------------------------------------------------
// State extraction
// ---------------------------------------------------------------------------

/// Extract structured state from conversation text using pattern matching.
fn extract_state(
    text: &str,
    existing: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut extracted = HashMap::new();
    let lower = text.to_lowercase();

    // Detect customer name.
    if !existing.contains_key("customer_name") {
        let skip = [
            "a", "the", "not", "so", "very", "really", "just", "here", "having",
        ];
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
            "not working",
            "broken",
            "crash",
            "error",
            "bug",
            "freeze",
            "slow",
            "won't load",
            "can't connect",
            "wifi",
            "bluetooth",
            "screen",
            "password reset",
            "login",
            "install",
            "update",
            "device",
            "connectivity",
            "troubleshoot",
        ];
        let billing_keywords = [
            "charge",
            "refund",
            "bill",
            "payment",
            "invoice",
            "overcharged",
            "subscription",
            "cancel",
            "plan",
            "pricing",
            "credit",
            "charged twice",
            "double charge",
            "unexpected charge",
        ];

        let tech_count = technical_keywords
            .iter()
            .filter(|kw| lower.contains(*kw))
            .count();
        let bill_count = billing_keywords
            .iter()
            .filter(|kw| lower.contains(*kw))
            .count();

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
            "refund",
            "charge",
            "amount",
            "dollars",
            "payment",
            "credit",
            "invoice",
            "receipt",
            "transaction",
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
            "yes",
            "okay",
            "ok",
            "sure",
            "sounds good",
            "i agree",
            "that works",
            "perfect",
            "go ahead",
            "confirmed",
            "alright",
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
            "not working",
            "broken",
            "crash",
            "error",
            "bug",
            "freeze",
            "slow",
            "won't load",
            "can't connect",
            "problem",
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
        } else if lower.contains("screen") || lower.contains("device") || lower.contains("hardware")
        {
            extracted.insert("tech_category".into(), json!("device"));
        } else if lower.contains("install") || lower.contains("update") || lower.contains("app") {
            extracted.insert("tech_category".into(), json!("software"));
        } else if lower.contains("login") || lower.contains("password") || lower.contains("account")
        {
            extracted.insert("tech_category".into(), json!("account-access"));
        }
    }

    // Detect troubleshoot result.
    if !existing.contains_key("troubleshoot_result") {
        let resolved_keywords = [
            "fixed",
            "works now",
            "resolved",
            "that did it",
            "working now",
        ];
        let unresolved_keywords = [
            "still broken",
            "didn't work",
            "same issue",
            "no luck",
            "still not",
        ];
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
        } else if lower.contains("fixed") || lower.contains("resolved") || lower.contains("working")
        {
            extracted.insert("final_outcome".into(), json!("resolved"));
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
        "thank",
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
        let present = phase
            .required_keys
            .iter()
            .filter(|k| state.contains_key(**k))
            .count();
        let progress = present as f64 / total_required as f64;
        score = 0.3 + (0.7 * progress);
        notes.push(format!(
            "{present}/{total_required} required keys extracted"
        ));
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
    state
        .get("issue_type")
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
impl DemoApp for SupportAssistant {
    demo_meta! {
        name: "support-assistant",
        description: "Multi-agent handoff with billing + technical support flows",
        category: Showcase,
        features: ["voice", "transcription", "state-machine", "evaluation", "guardrails", "multi-agent"],
        tips: [
            "Starts with a billing agent — describe a technical issue to trigger handoff to technical support",
            "Watch the devtools for agent handoff events and phase tracking across both agents",
            "The system tracks evaluation scores for each phase and handoff quality",
        ],
        try_saying: [
            "I'm having trouble with my internet connection.",
            "I was overcharged $50 on my last bill.",
            "My device keeps crashing and won't restart.",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("SupportAssistant session starting");

        let bridge = SessionBridge::new(tx.clone());
        let tx_boundary = tx.clone();
        let tx_watcher = tx;

        bridge
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());
                let extractor = Arc::new(RegexExtractor::new("support_state", 10, extract_state));

                // =================================================================
                // DESIGN DISSECTION: Why this app is built the way it is
                // =================================================================
                //
                // Steering Mode: ContextInjection
                //   Both the billing and technical support agents share the same
                //   base persona ("helpful support agent"). Despite the multi-agent
                //   handoff (billing -> technical), the persona doesn't radically
                //   shift — it's still the same support company. ContextInjection
                //   keeps the base persona stable and delivers phase-specific
                //   behavior as context turns.
                //
                // greeting("..."):
                //   The support agent should answer warmly. This fires once at
                //   session start, before the phase machine activates.
                //
                // NO prompt_on_enter on any phase:
                //   This app relies entirely on the greeting for initial speech.
                //   All phase transitions are customer-driven — the customer
                //   describes their issue, provides details, confirms resolutions.
                //   The model responds naturally to each piece of information.
                //   This is the simplest correct pattern for most support apps.
                //
                // NO enter_prompt on any phase:
                //   The app doesn't use enter_prompt because phase transitions are
                //   smooth and customer-initiated. The model has enough context
                //   from with_context(support_context) to continue naturally.
                //
                // Multi-agent handoff (billing -> technical):
                //   Issue type detection ("technical" vs "billing") drives a
                //   computed state variable (active_agent) and routes the
                //   conversation to the appropriate phase chain. This is agent
                //   routing via the phase machine, not separate session creation.
                //
                // with_context(support_context):
                //   Every phase gets a context formatter with customer name and
                //   current extracted state. This keeps both agent personas
                //   informed without duplicating state in instructions.
                //
                // .navigation() in phase_defaults:
                //   Gives the model awareness of where it is in the 10-phase
                //   graph (5 billing + 5 technical). Critical for a large phase
                //   graph where the model needs to understand its position.
                // =================================================================
                live.model(GeminiModel::Gemini2_0FlashLive)
                    .voice(voice)
                    .instruction(
                        start
                            .system_instruction
                            .as_deref()
                            .unwrap_or(BILLING_PHASES[0].instruction),
                    )
                    .transcription(true, true)
                    .steering_mode(SteeringMode::ContextInjection)
                    .context_delivery(ContextDelivery::Deferred)
                    // Model greets the caller immediately on connect
                    .greeting("Greet the caller warmly and ask how you can help them today.")
                    .extractor(extractor)
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
                    .phase_defaults(|d| d.navigation())
                    .phase("billing:greet")
                        .instruction(BILLING_PHASES[0].instruction)
                        .transition_with("billing:identify", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("customer_name").cloned())
                                .is_some()
                        }, "when customer name is provided")
                        .with_context(support_context)
                        .done()
                    .phase("billing:identify")
                        .instruction(BILLING_PHASES[1].instruction)
                        // Tech handoff transition FIRST (priority over billing:investigate).
                        .transition_with("tech:greet", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("issue_type").cloned())
                                .and_then(|v| v.as_str().map(|s| s.to_string()))
                                .map(|t| t == "technical")
                                .unwrap_or(false)
                        }, "when issue type is technical — handoff to tech support")
                        // Then billing:investigate for any other issue_type.
                        .transition_with("billing:investigate", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("issue_type").cloned())
                                .is_some()
                        }, "when billing issue type is identified")
                        .with_context(support_context)
                        .done()
                    .phase("billing:investigate")
                        .instruction(BILLING_PHASES[2].instruction)
                        .transition_with("billing:resolve", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("billing_detail").cloned())
                                .is_some()
                        }, "when billing details are gathered")
                        .with_context(support_context)
                        .done()
                    .phase("billing:resolve")
                        .instruction(BILLING_PHASES[3].instruction)
                        .transition_with("billing:close", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("resolution_confirmed").cloned())
                                .is_some()
                        }, "when resolution is confirmed by customer")
                        .with_context(support_context)
                        .done()
                    .phase("billing:close")
                        .instruction(BILLING_PHASES[4].instruction)
                        .terminal()
                        .with_context(support_context)
                        .done()
                    // --- Technical Phases ---
                    .phase("tech:greet")
                        .instruction(TECHNICAL_PHASES[0].instruction)
                        .transition_with("tech:identify", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("tech_issue_desc").cloned())
                                .is_some()
                        }, "when technical issue is described")
                        .with_context(support_context)
                        .done()
                    .phase("tech:identify")
                        .instruction(TECHNICAL_PHASES[1].instruction)
                        .transition_with("tech:troubleshoot", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("tech_category").cloned())
                                .is_some()
                        }, "when tech category is identified")
                        .with_context(support_context)
                        .done()
                    .phase("tech:troubleshoot")
                        .instruction(TECHNICAL_PHASES[2].instruction)
                        .transition_with("tech:resolve", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("troubleshoot_result").cloned())
                                .is_some()
                        }, "when troubleshooting result is determined")
                        .with_context(support_context)
                        .done()
                    .phase("tech:resolve")
                        .instruction(TECHNICAL_PHASES[3].instruction)
                        .transition_with("tech:close", |s| {
                            s.get::<serde_json::Value>("support_state")
                                .and_then(|v| v.get("final_outcome").cloned())
                                .is_some()
                        }, "when final outcome is reached")
                        .with_context(support_context)
                        .done()
                    .phase("tech:close")
                        .instruction(TECHNICAL_PHASES[4].instruction)
                        .terminal()
                        .with_context(support_context)
                        .done()
                    .initial_phase("billing:greet")
                    // Watch for escalation.
                    .watch("support_state")
                        .changed()
                        .then({
                            let tx = tx_watcher.clone();
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
                        let tx = tx_boundary;
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
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppCategory;

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
        assert_eq!(
            result.get("troubleshoot_result"),
            Some(&json!("unresolved"))
        );
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
