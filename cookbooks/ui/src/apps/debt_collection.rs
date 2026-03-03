use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::let_clone;
use adk_rs_fluent::prelude::*;
use rs_adk::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};
use rs_adk::state::StateKey;

use rs_genai::session::SessionEvent;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::extractors::RegexExtractor;
use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

// ---------------------------------------------------------------------------
// Typed state keys
// ---------------------------------------------------------------------------

const IDENTITY_VERIFIED: StateKey<bool> = StateKey::new("identity_verified");
const DISCLOSURE_GIVEN: StateKey<bool> = StateKey::new("disclosure_given");
const CEASE_DESIST: StateKey<bool> = StateKey::new("cease_desist_requested");
const PAYMENT_PROCESSED: StateKey<bool> = StateKey::new("payment_processed");
const WILLINGNESS: StateKey<f64> = StateKey::new("willingness_to_pay");

// Suppress unused-constant warnings — these serve as documentation for
// the typed state contract and will be used when the codebase migrates
// to `state.get_key()`/`state.set_key()`.
const _: () = {
    _ = IDENTITY_VERIFIED;
    _ = DISCLOSURE_GIVEN;
    _ = CEASE_DESIST;
    _ = PAYMENT_PROCESSED;
    _ = WILLINGNESS;
};

// ---------------------------------------------------------------------------
// Phase instructions
// ---------------------------------------------------------------------------

const DISCLOSURE_INSTRUCTION: &str = "\
You are a professional debt collection agent. You MUST begin by delivering the Mini-Miranda disclosure exactly as follows:\n\n\
\"This is an attempt to collect a debt. Any information obtained will be used for that purpose. \
This call may be monitored or recorded for quality assurance.\"\n\n\
After delivering this disclosure, ask the customer to confirm they understand. \
Once they confirm, let them know you will need to verify their identity before discussing any account details. \
Be professional and courteous at all times. Do NOT discuss any debt details until the disclosure is acknowledged.";

const VERIFY_IDENTITY_INSTRUCTION: &str = "\
You need to verify the customer's identity before discussing any account details. \
Ask for their full name, date of birth, and the last four digits of their Social Security Number. \
Use the verify_identity tool to confirm their identity. \
Once you verify inform them that you will get their details to share the exact details of their debt. \
Be patient and professional. If they are reluctant, explain that this is required for their protection. \
Do NOT reveal any account information until identity is verified.";

const INFORM_DEBT_INSTRUCTION: &str = "\
The customer's identity is verified. Now inform them about the debt:\n\
1. Use the lookup_account tool to retrieve account details.\n\
2. State the creditor name, total balance owed, and days past due.\n\
3. Inform them of their right to dispute the debt within 30 days.\n\
4. If they acknowledge the debt, explore resolution options.\n\
5. If they dispute the debt, inform them that a validation notice will be sent and you cannot continue collection.\n\n\
Be empathetic but clear about the obligation. Never threaten or use abusive language.";

const NEGOTIATE_INSTRUCTION: &str = "\
The customer has acknowledged the debt. Now work toward a resolution:\n\
1. Ask about their current financial situation.\n\
2. Use the calculate_payment_plan tool to generate options.\n\
3. Present 2-3 payment plan options (e.g., full payment with discount, 3-month plan, 6-month plan).\n\
4. Be flexible and empathetic. The goal is a mutually agreeable arrangement.\n\
5. Never pressure, threaten, or use deceptive tactics.\n\n\
If the customer agrees to a plan, confirm the details before proceeding.";

const ARRANGE_PAYMENT_INSTRUCTION: &str = "\
The customer has agreed to a payment plan. Now collect payment details:\n\
1. Ask for their preferred payment method (bank transfer, credit card, check).\n\
2. Use the process_payment tool to process the first payment or set up the plan.\n\
3. Confirm the payment was processed successfully.\n\
4. Provide the confirmation number.\n\n\
Handle payment information securely. Never read back full card numbers.";

const CONFIRM_INSTRUCTION: &str = "\
Payment has been processed. Now summarize the agreement:\n\
1. Confirm the total amount, payment schedule, and first payment.\n\
2. Inform them a written confirmation will be mailed.\n\
3. Confirm or ask for their mailing address for the written agreement.\n\
4. Provide a reference number for future inquiries.\n\n\
Be warm and reassuring. Thank them for working with you to resolve this.";

const CLOSE_INSTRUCTION: &str = "\
The call is concluding. Wrap up professionally:\n\
1. Thank the customer for their time.\n\
2. Remind them of next steps (first payment date, written confirmation in mail).\n\
3. Provide a contact number for any future questions.\n\
4. Wish them well.\n\n\
If the call ended due to cease-and-desist, acknowledge their request, confirm that \
all collection activity will stop, and inform them of any remaining legal obligations. \
If the debt is disputed, confirm that a validation notice will be sent within 5 business days.";

// ---------------------------------------------------------------------------
// Per-phase instruction modifiers
// ---------------------------------------------------------------------------

const DEBT_STATE_KEYS: &[&str] = &[
    "emotional_state",
    "willingness_to_pay",
    "derived:call_risk_level",
    "identity_verified",
    "disclosure_given",
];

const RISK_WARNING: &str = "\
IMPORTANT: The caller is showing signs of distress. Use extra empathy. \
Never threaten, harass, or use deceptive language. If they request to stop \
being contacted, immediately comply with cease-and-desist requirements.";

fn risk_is_elevated(s: &State) -> bool {
    let risk: String = s
        .get("derived:call_risk_level")
        .unwrap_or_else(|| "low".to_string());
    risk == "high" || risk == "critical"
}

// ---------------------------------------------------------------------------
// Mock account data
// ---------------------------------------------------------------------------

const MOCK_ACCOUNT: &str = r#"{
    "account_id": "78234561",
    "debtor_name": "Jane Smith",
    "ssn": "123-45-6789",
    "date_of_birth": "1985-03-15",
    "address": "123 Main St, Anytown, USA 12345",
    "creditor": "Acme Medical Group",
    "original_amount": 5200.00,
    "balance": 4250.00,
    "days_past_due": 127,
    "last_payment_date": "2025-10-15",
    "last_payment_amount": 150.00,
    "payment_history": [
        {"date": "2025-08-15", "amount": 200.00},
        {"date": "2025-09-15", "amount": 200.00},
        {"date": "2025-10-15", "amount": 150.00}
    ],
    "phone": "555-123-4567"
}"#;

// ---------------------------------------------------------------------------
// LLM-powered extraction struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct DebtorState {
    /// "calm", "cooperative", "frustrated", "angry"
    emotional_state: Option<String>,
    /// 0.0 (refusing) to 1.0 (eager)
    willingness_to_pay: Option<f32>,
    /// "full_pay", "partial_pay", "dispute", "refuse", "delay"
    negotiation_intent: Option<String>,
    /// Whether debtor explicitly requested cease-and-desist
    cease_desist_requested: Option<bool>,
    /// Whether debtor acknowledged owing the debt
    debt_acknowledged: Option<bool>,
}

// ---------------------------------------------------------------------------
// Computed state helpers
// ---------------------------------------------------------------------------

fn sentiment_from_emotion(emotion: &str) -> f64 {
    match emotion {
        "cooperative" => 0.9,
        "calm" => 0.7,
        "frustrated" => 0.4,
        "angry" => 0.2,
        _ => 0.5,
    }
}

fn compute_risk_level(sentiment: f64, cease_desist: bool) -> &'static str {
    if cease_desist { "critical" }
    else if sentiment < 0.3 { "high" }
    else if sentiment < 0.5 { "medium" }
    else { "low" }
}

// ---------------------------------------------------------------------------
// Regex-based structured field extraction
// ---------------------------------------------------------------------------

static DOLLAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$[\d,]+\.?\d*").unwrap());
static PHONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{3}[-.]?\d{3}[-.]?\d{4}\b").unwrap());
static DATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:january|february|march|april|may|june|july|august|september|october|november|december)\s+\d{1,2},?\s+\d{4}\b|\b\d{1,2}/\d{1,2}/\d{2,4}\b|\b\d{4}-\d{2}-\d{2}\b").unwrap()
});
static DISCLOSURE_ACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:yes|yeah|I understand|I acknowledge|got it|okay|ok|sure)\b").unwrap()
});

fn extract_structured(text: &str, existing: &HashMap<String, Value>) -> HashMap<String, Value> {
    let mut extracted = HashMap::new();

    if !existing.contains_key("dollar_amount") {
        if let Some(m) = DOLLAR_RE.find(text) {
            extracted.insert("dollar_amount".into(), json!(m.as_str()));
        }
    }

    if !existing.contains_key("phone_number") {
        if let Some(m) = PHONE_RE.find(text) {
            extracted.insert("phone_number".into(), json!(m.as_str()));
        }
    }

    if !existing.contains_key("date_mentioned") {
        if let Some(m) = DATE_RE.find(text) {
            extracted.insert("date_mentioned".into(), json!(m.as_str()));
        }
    }

    if !existing.contains_key("disclosure_given") {
        let lower = text.to_lowercase();
        if (lower.contains("understand") || lower.contains("acknowledge"))
            && DISCLOSURE_ACK_RE.is_match(text)
        {
            extracted.insert("disclosure_given".into(), json!(true));
        } else if DISCLOSURE_ACK_RE.is_match(text)
            && existing.is_empty()
        {
            extracted.insert("disclosure_given".into(), json!(true));
        }
    }

    extracted
}

// ---------------------------------------------------------------------------
// Tool declarations
// ---------------------------------------------------------------------------

/// Build the tool declarations so Gemini knows what functions it can call.
fn debt_collection_tools() -> rs_genai::prelude::Tool {
    use rs_genai::prelude::{FunctionDeclaration, Tool};
    Tool::functions(vec![
        FunctionDeclaration {
            name: "lookup_account".into(),
            description: "Look up a debtor's account details including balance, creditor, and payment history.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "account_id": {
                        "type": "string",
                        "description": "The account ID to look up"
                    }
                },
                "required": ["account_id"]
            })),
        },
        FunctionDeclaration {
            name: "verify_identity".into(),
            description: "Verify the debtor's identity by checking name, date of birth, and last 4 digits of SSN.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The debtor's full name"
                    },
                    "dob": {
                        "type": "string",
                        "description": "Date of birth in YYYY-MM-DD format"
                    },
                    "last4ssn": {
                        "type": "string",
                        "description": "Last 4 digits of the Social Security Number"
                    }
                },
                "required": ["name", "dob", "last4ssn"]
            })),
        },
        FunctionDeclaration {
            name: "calculate_payment_plan".into(),
            description: "Calculate payment plan options given a total balance and number of months.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "total": {
                        "type": "number",
                        "description": "Total debt balance in dollars"
                    },
                    "months": {
                        "type": "integer",
                        "description": "Number of months for the payment plan"
                    }
                },
                "required": ["total", "months"]
            })),
        },
        FunctionDeclaration {
            name: "process_payment".into(),
            description: "Process a payment or set up a recurring payment plan.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "account_id": {
                        "type": "string",
                        "description": "The account ID"
                    },
                    "amount": {
                        "type": "number",
                        "description": "Payment amount in dollars"
                    },
                    "method": {
                        "type": "string",
                        "description": "Payment method: bank_transfer, credit_card, or check"
                    }
                },
                "required": ["account_id", "amount", "method"]
            })),
        },
        FunctionDeclaration {
            name: "log_compliance_event".into(),
            description: "Log a compliance event for the audit trail.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "event_type": {
                        "type": "string",
                        "description": "Type of compliance event (e.g., disclosure_given, identity_verified, cease_desist)"
                    },
                    "details": {
                        "type": "string",
                        "description": "Details about the compliance event"
                    }
                },
                "required": ["event_type", "details"]
            })),
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &Value) -> Value {
    match name {
        "lookup_account" => {
            serde_json::from_str(MOCK_ACCOUNT).unwrap()
        }
        "verify_identity" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let dob = args.get("dob").and_then(|v| v.as_str()).unwrap_or("");
            let last4 = args.get("last4ssn").and_then(|v| v.as_str()).unwrap_or("");

            let account: Value = serde_json::from_str(MOCK_ACCOUNT).unwrap();
            let expected_name = account["debtor_name"].as_str().unwrap_or("");
            let expected_dob = account["date_of_birth"].as_str().unwrap_or("");
            let expected_ssn = account["ssn"].as_str().unwrap_or("");

            let name_match = name.eq_ignore_ascii_case(expected_name);
            let dob_match = dob == expected_dob;
            let ssn_match = expected_ssn.ends_with(last4);

            let verified = name_match && dob_match && ssn_match;
            let reason = if verified {
                "Identity confirmed".to_string()
            } else {
                let mut mismatches = Vec::new();
                if !name_match { mismatches.push("name"); }
                if !dob_match { mismatches.push("date of birth"); }
                if !ssn_match { mismatches.push("SSN last 4"); }
                format!("Mismatch: {}", mismatches.join(", "))
            };

            json!({"verified": verified, "reason": reason})
        }
        "calculate_payment_plan" => {
            let total = args.get("total").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let months = args.get("months").and_then(|v| v.as_u64()).unwrap_or(1) as f64;
            let interest_rate = 0.05;
            let total_with_interest = total * (1.0 + interest_rate);
            let monthly = (total_with_interest / months * 100.0).round() / 100.0;

            json!({
                "monthly_payment": monthly,
                "interest_rate": interest_rate,
                "total_cost": (total_with_interest * 100.0).round() / 100.0,
                "first_due_date": "2026-04-01",
                "months": months as u32,
            })
        }
        "process_payment" => {
            let amount = args.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let method = args.get("method").and_then(|v| v.as_str()).unwrap_or("unknown");

            json!({
                "confirmation_id": format!("PAY-{:06}", (amount * 1000.0) as u64 % 999999),
                "status": "processed",
                "amount": amount,
                "method": method,
                "processed_at": "2026-03-03T10:30:00Z",
            })
        }
        "log_compliance_event" => {
            let event_type = args.get("event_type").and_then(|v| v.as_str()).unwrap_or("unknown");
            let details = args.get("details").and_then(|v| v.as_str()).unwrap_or("");
            info!("Compliance event: {event_type} — {details}");
            json!({"logged": true, "event_id": format!("EVT-{event_type}")})
        }
        _ => json!({"error": format!("Unknown tool: {name}")}),
    }
}

// ---------------------------------------------------------------------------
// PII redaction
// ---------------------------------------------------------------------------

fn redact_pii(value: &Value) -> Value {
    let mut redacted = value.clone();
    if let Some(obj) = redacted.as_object_mut() {
        if let Some(ssn) = obj.get("ssn").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            if ssn.len() >= 4 {
                let last4 = &ssn[ssn.len() - 4..];
                obj.insert("ssn".into(), json!(format!("***-**-{last4}")));
            }
        }
        if let Some(acct) = obj.get("account_id").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            if acct.len() >= 4 {
                let last4 = &acct[acct.len() - 4..];
                obj.insert("account_id".into(), json!(format!("****{last4}")));
            }
        }
        obj.remove("address");
    }
    redacted
}

// ---------------------------------------------------------------------------
// DebtCollection app
// ---------------------------------------------------------------------------

/// FDCPA-compliant debt collection agent with compliance gates and emotional monitoring.
pub struct DebtCollection;

#[async_trait]
impl CookbookApp for DebtCollection {
    fn name(&self) -> &str {
        "debt-collection"
    }

    fn description(&self) -> &str {
        "FDCPA-compliant debt collection with compliance gates, emotional monitoring, and payment negotiation"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Showcase
    }

    fn features(&self) -> Vec<String> {
        vec![
            "phase-machine".into(),
            "compliance-gates".into(),
            "temporal-patterns".into(),
            "llm-extraction".into(),
            "tool-response-redaction".into(),
            "numeric-watchers".into(),
            "computed-state".into(),
            "turn-boundary-injection".into(),
        ]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "The agent must deliver a Mini-Miranda disclosure before discussing the debt".into(),
            "Try saying you refuse to pay or want to dispute the debt to see guardrails".into(),
            "Express frustration to trigger emotional monitoring and de-escalation".into(),
            "Ask to stop being contacted to trigger cease-and-desist compliance".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "Hello, who is this?".into(),
            "I don't think I owe that much.".into(),
            "I'd like to set up a payment plan.".into(),
            "Stop calling me! I don't want to be contacted anymore.".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        // 1. Wait for Start, resolve voice, build SessionConfig
        let start = wait_for_start(&mut rx).await?;
        let selected_voice = resolve_voice(start.voice.as_deref());

        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .add_tool(debt_collection_tools())
            .system_instruction(DISCLOSURE_INSTRUCTION);

        // 2. Create GeminiLlm for LLM extraction
        let llm: Arc<dyn rs_adk::llm::BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
            model: Some("gemini-2.5-flash".to_string()),
            ..Default::default()
        }));

        // 3. Create RegexExtractor for debt_fields
        let extractor = Arc::new(RegexExtractor::new("debt_fields", 10, |text, existing| {
            extract_structured(text, existing)
        }));

        // 4. Clone tx for all callbacks
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
        let tx_go_away = tx.clone();
        let tx_tool_call = tx.clone();

        // Phase on_enter / on_exit clones
        let tx_enter_disclosure = tx.clone();
        let tx_enter_verify = tx.clone();
        let tx_enter_inform = tx.clone();
        let tx_enter_negotiate = tx.clone();
        let tx_enter_arrange = tx.clone();
        let tx_enter_confirm = tx.clone();
        let tx_enter_close = tx.clone();
        let tx_exit_disclosure = tx.clone();
        let tx_exit_verify = tx.clone();

        // Watcher clones
        let tx_watcher_willingness = tx.clone();
        let tx_watcher_sentiment = tx.clone();
        let tx_watcher_cease = tx.clone();
        let tx_watcher_identity = tx.clone();
        let tx_watcher_dispute = tx.clone();

        // Temporal pattern clones
        let tx_sustained_frustration = tx.clone();
        let tx_rate_interruptions = tx.clone();
        let tx_turns_stalled = tx.clone();

        // Interceptor clones
        let tx_turn_boundary = tx.clone();

        // 5. Build Live::builder() with full pipeline
        let handle = Live::builder()
            // --- Model-initiated greeting ---
            // The agent delivers the Mini-Miranda disclosure immediately on connect
            .greeting("Begin the call. Deliver the required Mini-Miranda disclosure now.")
            // --- Regex extractor ---
            .extractor(extractor)
            // --- LLM extraction (windowed, min 5 words to skip "uh huh" / "ok") ---
            .extract_turns_windowed::<DebtorState>(
                llm,
                "Extract from the debt collection conversation: the debtor's emotional state \
                 (calm/cooperative/frustrated/angry), willingness to pay (0.0-1.0), \
                 negotiation intent (full_pay/partial_pay/dispute/refuse/delay), \
                 whether they requested cease-and-desist, and whether they acknowledged the debt.",
                5,
            )
            // --- on_extraction_error: log failures (concurrent — fire-and-forget) ---
            .on_extraction_error_concurrent(|name, err| async move {
                warn!("Extraction '{name}' failed: {err}");
            })
            // --- on_extracted: broadcast state to browser (concurrent — fire-and-forget) ---
            .on_extracted_concurrent({
                let tx = tx.clone();
                move |name, value| {
                    let tx = tx.clone();
                    async move {
                        // Send the extracted value as a state update
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: name.clone(),
                            value: value.clone(),
                        });
                        // If it's an object, also broadcast individual keys
                        if let Some(obj) = value.as_object() {
                            for (key, val) in obj {
                                let _ = tx.send(ServerMessage::StateUpdate {
                                    key: format!("{name}.{key}"),
                                    value: val.clone(),
                                });
                            }
                        }
                    }
                }
            })
            // --- Computed state ---
            // With auto-flatten, extractor fields are individual state keys.
            .computed("sentiment_score", &["emotional_state"], |state| {
                let emotion: String = state.get("emotional_state")?;
                Some(json!(sentiment_from_emotion(&emotion)))
            })
            .computed("call_risk_level", &["derived:sentiment_score", "cease_desist_requested"], |state| {
                let sentiment: f64 = state.get("derived:sentiment_score").unwrap_or(0.5);
                let cease_desist: bool = state.get("cease_desist_requested").unwrap_or(false);
                Some(json!(compute_risk_level(sentiment, cease_desist)))
            })
            // --- before_tool_response: PII redaction + state promotion ---
            // Tool results drive state that guards/watchers need.
            .before_tool_response(move |responses, state| {
                async move {
                    responses.into_iter().map(|mut r| {
                        // Promote tool-response booleans to state keys
                        match r.name.as_str() {
                            "verify_identity" => {
                                if r.response.get("verified").and_then(|v| v.as_bool()).unwrap_or(false) {
                                    state.set("identity_verified", true);
                                }
                            }
                            "process_payment" => {
                                if r.response.get("status").and_then(|v| v.as_str()) == Some("processed") {
                                    state.set("payment_processed", true);
                                }
                            }
                            _ => {}
                        }
                        // Redact PII
                        r.response = redact_pii(&r.response);
                        r
                    }).collect()
                }
            })
            // --- on_turn_boundary: compliance reminders ---
            .on_turn_boundary(move |state, writer| {
                let tx = tx_turn_boundary.clone();
                async move {
                    let risk: String = state.get("derived:call_risk_level").unwrap_or_default();
                    let phase: String = state.get("session:phase").unwrap_or_default();

                    // If risk is high/critical and we're past disclosure, inject a reminder
                    if (risk == "high" || risk == "critical")
                        && phase != "disclosure"
                        && phase != "close"
                    {
                        let reminder = "[Compliance reminder: The debtor appears distressed. \
                            Use empathetic language. Do not threaten, harass, or use deceptive tactics. \
                            If they request cease-and-desist, you must comply immediately.]";
                        let _ = writer.send_client_content(
                            vec![Content::user(reminder)],
                            false,
                        ).await;
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "compliance_reminder".into(),
                            value: json!({"injected": true, "risk": risk, "phase": phase}),
                        });
                    }
                }
            })
            // --- Phase defaults (inherited by all phases) ---
            .phase_defaults(|d| d
                .with_state(DEBT_STATE_KEYS)
                .when(risk_is_elevated, RISK_WARNING)
                .prompt_on_enter(true)
            )
            // --- 7 Phases ---
            // Phase 1: Disclosure (Mini-Miranda)
            .phase("disclosure")
                .instruction(DISCLOSURE_INSTRUCTION)
                .transition("verify_identity", S::is_true("disclosure_given"))
                .transition("close", S::is_true("cease_desist_requested"))
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_disclosure.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "none".into(),
                            to: "disclosure".into(),
                            reason: "Session started — delivering Mini-Miranda disclosure".into(),
                        });
                    }
                })
                .on_exit(move |_state, _writer| {
                    let tx = tx_exit_disclosure.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "compliance_event".into(),
                            value: json!({"event": "disclosure_acknowledged"}),
                        });
                    }
                })
                .done()
            // Phase 2: Verify Identity
            .phase("verify_identity")
                .instruction(VERIFY_IDENTITY_INSTRUCTION)
                .guard(S::is_true("disclosure_given"))
                .transition("inform_debt", S::is_true("identity_verified"))
                .transition("close", S::is_true("cease_desist_requested"))
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_verify.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "disclosure".into(),
                            to: "verify_identity".into(),
                            reason: "Disclosure acknowledged — verifying identity".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("verify_identity"),
                        });
                    }
                })
                .on_exit(move |_state, _writer| {
                    let tx = tx_exit_verify.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "compliance_event".into(),
                            value: json!({"event": "identity_verified"}),
                        });
                    }
                })
                .enter_prompt("The caller confirmed the disclosure. I'll now verify their identity.")
                .done()
            // Phase 3: Inform Debt
            .phase("inform_debt")
                .instruction(INFORM_DEBT_INSTRUCTION)
                .guard(S::is_true("identity_verified"))
                .transition("negotiate", S::is_true("debt_acknowledged"))
                .transition("close", |s| {
                    S::is_true("cease_desist_requested")(s) || S::eq("negotiation_intent", "dispute")(s)
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_inform.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "verify_identity".into(),
                            to: "inform_debt".into(),
                            reason: "Identity verified — informing about debt".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("inform_debt"),
                        });
                    }
                })
                .enter_prompt("The caller's identity is verified. I'll now inform them about the debt.")
                .done()
            // Phase 4: Negotiate
            .phase("negotiate")
                .instruction(NEGOTIATE_INSTRUCTION)
                .guard(S::is_true("debt_acknowledged"))
                .transition("arrange_payment", S::one_of("negotiation_intent", &["full_pay", "partial_pay"]))
                .transition("close", |s| {
                    S::is_true("cease_desist_requested")(s) || S::eq("negotiation_intent", "refuse")(s)
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_negotiate.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "inform_debt".into(),
                            to: "negotiate".into(),
                            reason: "Debt acknowledged — negotiating payment".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("negotiate"),
                        });
                    }
                })
                .enter_prompt("The caller acknowledges the debt. I'll now discuss resolution options.")
                .done()
            // Phase 5: Arrange Payment
            .phase("arrange_payment")
                .instruction(ARRANGE_PAYMENT_INSTRUCTION)
                .guard(S::one_of("negotiation_intent", &["full_pay", "partial_pay"]))
                .transition("confirm", S::is_true("payment_processed"))
                .transition("close", S::is_true("cease_desist_requested"))
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_arrange.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "negotiate".into(),
                            to: "arrange_payment".into(),
                            reason: "Payment plan agreed — arranging payment".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("arrange_payment"),
                        });
                    }
                })
                .enter_prompt("We've agreed on a payment arrangement. I'll now collect the payment details.")
                .done()
            // Phase 6: Confirm
            .phase("confirm")
                .instruction(CONFIRM_INSTRUCTION)
                .guard(S::is_true("payment_processed"))
                .transition("close", |_s| {
                    // Close after confirmation summary is delivered
                    // (model will naturally complete the summary turn)
                    true
                })
                .on_enter(move |_state, _writer| {
                    let tx = tx_enter_confirm.clone();
                    async move {
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "arrange_payment".into(),
                            to: "confirm".into(),
                            reason: "Payment processed — confirming agreement".into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("confirm"),
                        });
                    }
                })
                .enter_prompt("Payment is processed. I'll now confirm the agreement details.")
                .done()
            // Phase 7: Close
            .phase("close")
                .instruction(CLOSE_INSTRUCTION)
                .terminal()
                .on_enter(move |state, _writer| {
                    let tx = tx_enter_close.clone();
                    async move {
                        let cease: bool = state.get("cease_desist_requested").unwrap_or(false);
                        let dispute = state.get::<String>("negotiation_intent")
                            .map(|i| i == "dispute")
                            .unwrap_or(false);

                        let reason = if cease {
                            "Cease-and-desist requested — closing call"
                        } else if dispute {
                            "Debt disputed — closing call"
                        } else {
                            "Call concluding"
                        };
                        let _ = tx.send(ServerMessage::PhaseChange {
                            from: "previous".into(),
                            to: "close".into(),
                            reason: reason.into(),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "phase".into(),
                            value: json!("close"),
                        });
                    }
                })
                .enter_prompt_fn(|state, _tw| {
                    if S::is_true("cease_desist_requested")(state) {
                        "The caller has requested cease-and-desist. I'll close the call respectfully.".into()
                    } else if S::eq("negotiation_intent", "dispute")(state) {
                        "The caller disputes the debt. I'll close the call and arrange validation.".into()
                    } else {
                        "I'll now wrap up the call.".into()
                    }
                })
                .done()
            .initial_phase("disclosure")
            // --- Watchers ---
            // With auto-flatten, each extracted field is its own state key.
            // Numeric: willingness crossed above 0.7
            .watch("willingness_to_pay")
                .crossed_above(0.7)
                .then({
                    let tx = tx_watcher_willingness.clone();
                    move |_old, new, _state| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "watcher:willingness_high".into(),
                                value: json!({
                                    "triggered": true,
                                    "value": new,
                                    "action": "Debtor showing strong willingness to pay"
                                }),
                            });
                        }
                    }
                })
            // Numeric: sentiment crossed below 0.3
            .watch("derived:sentiment_score")
                .crossed_below(0.3)
                .then({
                    let tx = tx_watcher_sentiment.clone();
                    move |_old, new, _state| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::Violation {
                                rule: "low_sentiment".into(),
                                severity: "warning".into(),
                                detail: format!("Sentiment dropped below 0.3: {new}"),
                            });
                        }
                    }
                })
            // Boolean: cease_desist became true
            .watch("cease_desist_requested")
                .became_true()
                .blocking()
                .then({
                    let tx = tx_watcher_cease.clone();
                    move |_old, _new, state| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::Violation {
                                rule: "cease_and_desist".into(),
                                severity: "critical".into(),
                                detail: "Debtor requested cease-and-desist — must stop collection".into(),
                            });
                            state.set("cease_desist_active", true);
                        }
                    }
                })
            // Boolean: identity_verified became false
            .watch("identity_verified")
                .became_false()
                .then({
                    let tx = tx_watcher_identity.clone();
                    move |_old, _new, _state| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::Violation {
                                rule: "identity_unverified".into(),
                                severity: "warning".into(),
                                detail: "Identity verification revoked".into(),
                            });
                        }
                    }
                })
            // Value: negotiation_intent changed to "dispute"
            .watch("negotiation_intent")
                .changed_to(json!("dispute"))
                .then({
                    let tx = tx_watcher_dispute.clone();
                    move |_old, _new, _state| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::Violation {
                                rule: "debt_disputed".into(),
                                severity: "info".into(),
                                detail: "Debtor is disputing the debt — must send validation notice".into(),
                            });
                        }
                    }
                })
            // --- Temporal patterns ---
            // Sustained frustration for 30 seconds
            .when_sustained(
                "sustained_frustration",
                |s| {
                    let sentiment: f64 = s.get("derived:sentiment_score").unwrap_or(0.5);
                    sentiment < 0.4
                },
                Duration::from_secs(30),
                {
                    let tx = tx_sustained_frustration.clone();
                    move |_state, writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::Violation {
                                rule: "sustained_frustration".into(),
                                severity: "warning".into(),
                                detail: "Debtor has been frustrated for over 30 seconds".into(),
                            });
                            // Inject de-escalation prompt
                            let _ = writer.send_client_content(
                                vec![Content::user(
                                    "[System: The debtor has been frustrated for an extended period. \
                                     Please pause, acknowledge their feelings, and offer to help \
                                     find a manageable solution. Consider offering to call back at \
                                     a better time.]"
                                )],
                                false,
                            ).await;
                        }
                    }
                },
            )
            // Rate: 3 turn completes in 60 seconds (rapid-fire exchanges)
            .when_rate(
                "rapid_exchanges",
                |evt| matches!(evt, SessionEvent::TurnComplete),
                3,
                Duration::from_secs(60),
                {
                    let tx = tx_rate_interruptions.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "temporal:rapid_exchanges".into(),
                                value: json!({
                                    "triggered": true,
                                    "action": "Conversation pace is very fast — consider slowing down"
                                }),
                            });
                        }
                    }
                },
            )
            // Turns: stalled negotiation for 5 turns
            .when_turns(
                "stalled_negotiation",
                |s| {
                    let phase: String = s.get("session:phase").unwrap_or_default();
                    phase == "negotiate"
                },
                5,
                {
                    let tx = tx_turns_stalled.clone();
                    move |_state, writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "temporal:stalled_negotiation".into(),
                                value: json!({
                                    "triggered": true,
                                    "action": "Negotiation has stalled for 5 turns"
                                }),
                            });
                            // Inject suggestion to offer alternatives
                            let _ = writer.send_client_content(
                                vec![Content::user(
                                    "[System: Negotiation seems stalled. Consider presenting \
                                     different payment options, offering a temporary hardship \
                                     plan, or asking if there are specific concerns preventing \
                                     agreement.]"
                                )],
                                false,
                            ).await;
                        }
                    }
                },
            )
            // --- on_tool_call: mock tool dispatch ---
            .on_tool_call(move |calls, _state| {
                let tx = tx_tool_call.clone();
                async move {
                    let mut responses = Vec::new();
                    for call in &calls {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "tool_call".into(),
                            value: json!({
                                "name": call.name,
                                "args": call.args,
                            }),
                        });

                        let result = execute_tool(&call.name, &call.args);

                        let _ = tx.send(ServerMessage::ToolCallEvent {
                            name: call.name.clone(),
                            args: serde_json::to_string(&call.args).unwrap_or_default(),
                            result: serde_json::to_string(&result).unwrap_or_default(),
                        });

                        responses.push(FunctionResponse {
                            name: call.name.clone(),
                            response: result,
                            id: call.id.clone(),
                        });
                    }
                    Some(responses)
                }
            })
            // NOTE: on_turn_boundary for compliance reminders is set above.
            // Telemetry is auto-collected by the SDK's telemetry lane and
            // sent to the browser via the periodic telemetry sender post-connect.
            // --- Fast lane callbacks ---
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
            // --- Control lane callbacks ---
            .on_turn_complete({
                let tx = tx_turn.clone();
                move || {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::TurnComplete);
                    }
                }
            })
            .on_interrupted({
                let tx = tx_interrupted.clone();
                move || {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::Interrupted);
                    }
                }
            })
            .on_vad_start(move || {
                let _ = tx_vad_start.send(ServerMessage::VoiceActivityStart);
            })
            .on_vad_end(move || {
                let _ = tx_vad_end.send(ServerMessage::VoiceActivityEnd);
            })
            .on_error_concurrent(move |msg| {
                let tx = tx_error.clone();
                async move {
                    let _ = tx.send(ServerMessage::Error { message: msg });
                }
            })
            .on_go_away_concurrent(move |duration| {
                let tx = tx_go_away.clone();
                async move {
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "go_away".into(),
                        value: json!({
                            "time_remaining_secs": duration.as_secs(),
                        }),
                    });
                }
            })
            .on_disconnected_concurrent(move |reason| {
                let _tx = tx_disconnected.clone();
                async move {
                    info!("DebtCollection session disconnected: {reason:?}");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        // 6. Send Connected + AppMeta + initial state
        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("DebtCollection session connected");

        // Periodic telemetry sender (auto-collected by SDK telemetry lane)
        let telem = handle.telemetry().clone();
        let telem_state = handle.state().clone();
        let tx_telem = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                let mut stats = telem.snapshot();
                // Merge app-specific stats
                if let Some(obj) = stats.as_object_mut() {
                    let phase: String = telem_state.get("session:phase").unwrap_or_default();
                    let risk: String = telem_state.get("derived:call_risk_level").unwrap_or_else(|| "low".to_string());
                    let tc: u32 = telem_state.session().get("turn_count").unwrap_or(0);
                    obj.insert("current_phase".into(), json!(phase));
                    obj.insert("risk_level".into(), json!(risk));
                    obj.insert("turn_count".into(), json!(tc));
                }
                if tx_telem.send(ServerMessage::Telemetry { stats }).is_err() {
                    break;
                }
            }
        });

        let _ = tx.send(ServerMessage::PhaseChange {
            from: "none".into(),
            to: "disclosure".into(),
            reason: "Session started".into(),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "phase".into(),
            value: json!("disclosure"),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "call_risk_level".into(),
            value: json!("low"),
        });

        // 7. Browser -> Gemini recv loop
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
                    info!("DebtCollection session stopping");
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

    // -- Mock tools --

    #[test]
    fn lookup_account_returns_balance() {
        let result = execute_tool("lookup_account", &json!({"account_id": "78234561"}));
        assert_eq!(result["balance"], 4250.00);
        assert_eq!(result["creditor"], "Acme Medical Group");
    }

    #[test]
    fn verify_identity_success() {
        let result = execute_tool(
            "verify_identity",
            &json!({"name": "Jane Smith", "dob": "1985-03-15", "last4ssn": "6789"}),
        );
        assert_eq!(result["verified"], true);
    }

    #[test]
    fn verify_identity_wrong_name() {
        let result = execute_tool(
            "verify_identity",
            &json!({"name": "John Doe", "dob": "1985-03-15", "last4ssn": "6789"}),
        );
        assert_eq!(result["verified"], false);
    }

    #[test]
    fn calculate_payment_plan_monthly() {
        let result = execute_tool(
            "calculate_payment_plan",
            &json!({"total": 4250.0, "months": 6}),
        );
        let monthly = result["monthly_payment"].as_f64().unwrap();
        assert!(monthly > 700.0 && monthly < 800.0);
    }

    #[test]
    fn process_payment_success() {
        let result = execute_tool(
            "process_payment",
            &json!({"account_id": "78234561", "amount": 708.33, "method": "bank_transfer"}),
        );
        assert_eq!(result["status"], "processed");
        assert!(result["confirmation_id"].as_str().unwrap().starts_with("PAY-"));
    }

    #[test]
    fn log_compliance_event_success() {
        let result = execute_tool(
            "log_compliance_event",
            &json!({"event_type": "disclosure_given", "details": "Mini-Miranda delivered"}),
        );
        assert_eq!(result["logged"], true);
    }

    #[test]
    fn unknown_tool_returns_error() {
        let result = execute_tool("nonexistent", &json!({}));
        assert!(result["error"].as_str().unwrap().contains("Unknown"));
    }

    // -- PII redaction --

    #[test]
    fn redact_ssn() {
        let input = json!({"ssn": "123-45-6789", "name": "Jane"});
        let redacted = redact_pii(&input);
        assert_eq!(redacted["ssn"], "***-**-6789");
        assert_eq!(redacted["name"], "Jane");
    }

    #[test]
    fn redact_account_id() {
        let input = json!({"account_id": "78234561"});
        let redacted = redact_pii(&input);
        assert_eq!(redacted["account_id"], "****4561");
    }

    #[test]
    fn redact_strips_address() {
        let input = json!({"address": "123 Main St", "balance": 100.0});
        let redacted = redact_pii(&input);
        assert!(redacted.get("address").is_none());
        assert_eq!(redacted["balance"], 100.0);
    }

    // -- Regex extraction --

    #[test]
    fn extract_dollar_amount() {
        let state = HashMap::new();
        let result = extract_structured("I can pay $500.00 right now", &state);
        assert_eq!(result.get("dollar_amount"), Some(&json!("$500.00")));
    }

    #[test]
    fn extract_dollar_amount_with_comma() {
        let state = HashMap::new();
        let result = extract_structured("The balance is $4,250.00", &state);
        assert_eq!(result.get("dollar_amount"), Some(&json!("$4,250.00")));
    }

    #[test]
    fn extract_date_mentioned() {
        let state = HashMap::new();
        let result = extract_structured("I can pay by March 15, 2026", &state);
        assert!(result.contains_key("date_mentioned"));
    }

    #[test]
    fn extract_phone_number() {
        let state = HashMap::new();
        let result = extract_structured("Call me back at 555-867-5309", &state);
        assert_eq!(result.get("phone_number"), Some(&json!("555-867-5309")));
    }

    #[test]
    fn extract_skips_existing_keys() {
        let mut state = HashMap::new();
        state.insert("dollar_amount".into(), json!("$100.00"));
        let result = extract_structured("I can pay $500.00", &state);
        assert!(!result.contains_key("dollar_amount"));
    }

    #[test]
    fn extract_disclosure_acknowledgment() {
        let state = HashMap::new();
        let result = extract_structured("Yes, I understand the disclosure", &state);
        assert_eq!(result.get("disclosure_given"), Some(&json!(true)));
    }

    // -- Computed state --

    #[test]
    fn sentiment_score_cooperative() {
        assert_eq!(sentiment_from_emotion("cooperative"), 0.9);
    }

    #[test]
    fn sentiment_score_angry() {
        assert_eq!(sentiment_from_emotion("angry"), 0.2);
    }

    #[test]
    fn sentiment_score_unknown() {
        assert_eq!(sentiment_from_emotion("confused"), 0.5);
    }

    #[test]
    fn risk_level_critical_on_cease_desist() {
        assert_eq!(compute_risk_level(0.9, true), "critical");
    }

    #[test]
    fn risk_level_high_on_low_sentiment() {
        assert_eq!(compute_risk_level(0.2, false), "high");
    }

    #[test]
    fn risk_level_low_on_good_sentiment() {
        assert_eq!(compute_risk_level(0.8, false), "low");
    }

    // -- App metadata --

    #[test]
    fn app_metadata() {
        let app = DebtCollection;
        assert_eq!(app.name(), "debt-collection");
        assert_eq!(app.category(), AppCategory::Showcase);
        assert!(app.features().contains(&"phase-machine".to_string()));
        assert!(app.features().contains(&"temporal-patterns".to_string()));
        assert!(app.features().contains(&"llm-extraction".to_string()));
    }
}
