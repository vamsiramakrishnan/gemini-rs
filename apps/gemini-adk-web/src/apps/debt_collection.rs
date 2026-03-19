use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tracing::{info, warn};

use gemini_adk::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};
use gemini_adk::state::StateKey;
use gemini_adk_fluent::prelude::*;

use gemini_live::session::SessionEvent;

use crate::app::{AppError, ClientMessage, DemoApp, ServerMessage, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

use super::extractors::RegexExtractor;
use super::resolve_voice;

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
// Phase instructions -- lean directives for what to do in each phase.
// Contextual awareness ("where we are, what we know") is provided by
// the collection_context() closure via with_context, so the model always
// has situational bearings without repeating state in the instructions.
// ---------------------------------------------------------------------------

const DISCLOSURE_INSTRUCTION: &str = "\
You are a professional debt collection agent. Deliver the Mini-Miranda disclosure exactly:\n\n\
\"This is an attempt to collect a debt. Any information obtained will be used for that purpose. \
This call may be monitored or recorded for quality assurance.\"\n\n\
Ask the customer to confirm they understand. \
Do NOT discuss any debt details until the disclosure is acknowledged.";

const VERIFY_IDENTITY_INSTRUCTION: &str = "\
Verify the customer's identity before discussing account details. \
Ask for full name, date of birth, and last four digits of SSN. \
Use verify_identity to confirm. Be patient; explain it's for their protection.";

const INFORM_DEBT_INSTRUCTION: &str = "\
Use lookup_account to retrieve account details. \
State creditor, balance, and days past due. \
Inform them of their right to dispute within 30 days. \
If they dispute, a validation notice will be sent and collection stops.";

const NEGOTIATE_INSTRUCTION: &str = "\
Work toward a mutually agreeable resolution. \
Ask about their financial situation. Use calculate_payment_plan to generate options. \
Present 2-3 plans (full with discount, 3-month, 6-month). \
Never pressure or threaten. Confirm details before proceeding.";

const ARRANGE_PAYMENT_INSTRUCTION: &str = "\
Collect payment details: preferred method (bank transfer, credit card, check). \
Use process_payment to process the first payment or set up the plan. \
Confirm success and provide the confirmation number. \
Never read back full card numbers.";

const CONFIRM_INSTRUCTION: &str = "\
Summarize the agreement: total amount, payment schedule, first payment. \
Inform them a written confirmation will be mailed. \
Confirm or ask for mailing address. Provide a reference number.";

const CLOSE_INSTRUCTION: &str = "\
Wrap up professionally. Thank the customer. \
Remind them of next steps and provide a contact number. \
If cease-and-desist: confirm all collection stops, note remaining legal obligations. \
If disputed: confirm validation notice within 5 business days.";

// ---------------------------------------------------------------------------
// Per-phase instruction modifiers
// ---------------------------------------------------------------------------

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

/// Builds a conversational context summary from accumulated state.
/// This is the "geolocation" -- the model always knows where it is,
/// what it has gathered so far, and what is still needed.
fn collection_context(s: &State) -> String {
    let mut ctx = Vec::new();

    // Debtor info
    let name: Option<String> = s.get("debtor_name");
    let verified: bool = s.get("identity_verified").unwrap_or(false);
    if let Some(n) = &name {
        let tag = if verified {
            "identity verified"
        } else {
            "identity NOT verified"
        };
        ctx.push(format!("Debtor: {n} ({tag})."));
    } else if verified {
        ctx.push("Identity verified but debtor name not yet recorded.".into());
    }

    // Debt details
    let creditor: Option<String> = s.get("creditor");
    let balance: Option<f64> = s.get("balance");
    let account: Option<String> = s.get("account_id");
    let days_past_due: Option<u32> = s.get("days_past_due");
    if let Some(bal) = balance {
        let mut debt_line = format!("Balance: ${bal:.2}");
        if let Some(c) = &creditor {
            debt_line.push_str(&format!(", creditor: {c}"));
        }
        if let Some(dpd) = days_past_due {
            debt_line.push_str(&format!(", {dpd} days past due"));
        }
        if let Some(a) = &account {
            debt_line.push_str(&format!(", account: {a}"));
        }
        debt_line.push('.');
        ctx.push(debt_line);
    }

    // Compliance state
    let disclosure: bool = s.get("disclosure_given").unwrap_or(false);
    let cease: bool = s.get("cease_desist_requested").unwrap_or(false);
    if !disclosure {
        ctx.push("Mini-Miranda disclosure NOT yet acknowledged.".into());
    } else {
        ctx.push("Disclosure acknowledged.".into());
    }
    if cease {
        ctx.push("CEASE-AND-DESIST requested -- must stop collection.".into());
    }

    // Negotiation progress
    let acknowledged: bool = s.get("debt_acknowledged").unwrap_or(false);
    let intent: Option<String> = s.get("negotiation_intent");
    let payment_processed: bool = s.get("payment_processed").unwrap_or(false);
    if acknowledged {
        ctx.push("Debt acknowledged.".into());
    }
    if let Some(i) = &intent {
        let label = match i.as_str() {
            "full_pay" => "willing to pay in full",
            "partial_pay" => "willing to pay partially",
            "dispute" => "disputing the debt",
            "refuse" => "refusing to pay",
            "delay" => "requesting delay",
            _ => i.as_str(),
        };
        ctx.push(format!("Intent: {label}."));
    }
    if let Some(amt) = s.get::<String>("dollar_amount") {
        ctx.push(format!("Amount mentioned: {amt}."));
    }
    if payment_processed {
        ctx.push("Payment processed.".into());
    }

    // Emotional state & risk
    let emotion: String = s.get("emotional_state").unwrap_or_default();
    if !emotion.is_empty() {
        ctx.push(format!("Debtor seems {emotion}."));
    }
    let risk: String = s.get("derived:call_risk_level").unwrap_or_default();
    if !risk.is_empty() && risk != "low" {
        ctx.push(format!("Risk level: {risk}."));
    }
    let willingness: Option<f64> = s.get("willingness_to_pay");
    if let Some(w) = willingness {
        let label = if w >= 0.7 {
            "high"
        } else if w >= 0.4 {
            "moderate"
        } else {
            "low"
        };
        ctx.push(format!("Willingness to pay: {label} ({w:.1})."));
    }

    if ctx.is_empty() {
        String::new()
    } else {
        ctx.join(" ")
    }
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
    if cease_desist {
        "critical"
    } else if sentiment < 0.3 {
        "high"
    } else if sentiment < 0.5 {
        "medium"
    } else {
        "low"
    }
}

// ---------------------------------------------------------------------------
// Regex-based structured field extraction
// ---------------------------------------------------------------------------

static DOLLAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$[\d,]+\.?\d*").unwrap());
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
            || (DISCLOSURE_ACK_RE.is_match(text) && existing.is_empty())
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
fn debt_collection_tools() -> gemini_live::prelude::Tool {
    use gemini_live::prelude::{FunctionCallingBehavior, FunctionDeclaration, Tool};
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &Value) -> Value {
    match name {
        "lookup_account" => serde_json::from_str(MOCK_ACCOUNT).unwrap(),
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
                if !name_match {
                    mismatches.push("name");
                }
                if !dob_match {
                    mismatches.push("date of birth");
                }
                if !ssn_match {
                    mismatches.push("SSN last 4");
                }
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
            let method = args
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            json!({
                "confirmation_id": format!("PAY-{:06}", (amount * 1000.0) as u64 % 999999),
                "status": "processed",
                "amount": amount,
                "method": method,
                "processed_at": "2026-03-03T10:30:00Z",
            })
        }
        "log_compliance_event" => {
            let event_type = args
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
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
        if let Some(ssn) = obj
            .get("ssn")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
            if ssn.len() >= 4 {
                let last4 = &ssn[ssn.len() - 4..];
                obj.insert("ssn".into(), json!(format!("***-**-{last4}")));
            }
        }
        if let Some(acct) = obj
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
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
impl DemoApp for DebtCollection {
    demo_meta! {
        name: "debt-collection",
        description: "FDCPA-compliant debt collection with compliance gates, emotional monitoring, and payment negotiation",
        category: Showcase,
        features: [
            "phase-machine",
            "compliance-gates",
            "temporal-patterns",
            "llm-extraction",
            "tool-response-redaction",
            "numeric-watchers",
            "computed-state",
            "turn-boundary-injection",
        ],
        tips: [
            "The agent must deliver a Mini-Miranda disclosure before discussing the debt",
            "Try saying you refuse to pay or want to dispute the debt to see guardrails",
            "Express frustration to trigger emotional monitoring and de-escalation",
            "Ask to stop being contacted to trigger cease-and-desist compliance",
        ],
        try_saying: [
            "Hello, who is this?",
            "I don't think I owe that much.",
            "I'd like to set up a payment plan.",
            "Stop calling me! I don't want to be contacted anymore.",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("DebtCollection session starting");

        // Create GeminiLlm for LLM extraction
        let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
            model: Some("gemini-3.1-flash-lite-preview".to_string()),
            ..Default::default()
        }));

        // Create RegexExtractor for debt_fields
        let extractor = Arc::new(RegexExtractor::new("debt_fields", 10, |text, existing| {
            extract_structured(text, existing)
        }));

        // Watcher tx clones
        let tx_watcher_willingness = tx.clone();
        let tx_watcher_sentiment = tx.clone();
        let tx_watcher_cease = tx.clone();
        let tx_watcher_identity = tx.clone();
        let tx_watcher_dispute = tx.clone();

        // Temporal pattern tx clones
        let tx_sustained_frustration = tx.clone();
        let tx_rate_interruptions = tx.clone();
        let tx_turns_stalled = tx.clone();

        // Turn boundary tx clone
        let tx_turn_boundary = tx.clone();

        // DESIGN DISSECTION: Why this app is built the way it is
        //
        // Steering Mode: ContextInjection
        //   The collector persona is constant — FDCPA-compliant, empathetic,
        //   professional. Phases (disclosure → verify → negotiate → payment)
        //   represent stages in the same conversation, not persona shifts.
        //   ContextInjection keeps the base persona stable.
        //
        // Context Delivery: Deferred
        //   Queues context turns until the next user audio chunk, preventing
        //   isolated frames during debtor silence.
        //
        // greeting + prompt_on_enter on disclosure:
        //   `.greeting()` fires the Mini-Miranda disclosure at session start.
        //   The disclosure phase also uses `prompt_on_enter(true)` because the
        //   agent must speak first — the debtor didn't initiate this call.
        //
        // enter_prompt on verify_identity:
        //   "The caller confirmed the disclosure. I'll now verify their identity."
        //   This Content::model() injection gives the model continuity across the
        //   disclosure→verify transition. Without it, the model loses context
        //   about what just happened and says "how can I help?"
        //
        // Compliance watchers:
        //   cease_desist, identity_verified, and willingness watchers react to
        //   extracted state. cease_desist triggers an immediate halt. These are
        //   legal requirements, not optional UX polish.
        //
        // .repair() for identity verification:
        //   If the debtor avoids giving their name/DOB for 3+ turns, the repair
        //   system nudges the model. After 6 turns, it escalates. This prevents
        //   infinite loops without over-constraining the LLM.

        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());

                live.model(GeminiModel::Gemini2_0FlashLive)
                    .voice(voice)
                    .instruction(
                        start
                            .system_instruction
                            .as_deref()
                            .unwrap_or(DISCLOSURE_INSTRUCTION),
                    )
                    .transcription(true, true)
                    .add_tool(debt_collection_tools())
                    .steering_mode(SteeringMode::ContextInjection)
                    .context_delivery(ContextDelivery::Deferred)
                    // --- Model-initiated greeting ---
                    .greeting("Begin the call. Deliver the required Mini-Miranda disclosure now.")
                    // --- Regex extractor ---
                    .extractor(extractor)
                    // --- LLM extraction (windowed, min 5 words to skip "uh huh" / "ok") ---
                    .extract_turns_triggered::<DebtorState>(
                        llm,
                        "Extract from the debt collection conversation: the debtor's emotional state \
                         (calm/cooperative/frustrated/angry), willingness to pay (0.0-1.0), \
                         negotiation intent (full_pay/partial_pay/dispute/refuse/delay), \
                         whether they requested cease-and-desist, and whether they acknowledged the debt.",
                        5,
                        ExtractionTrigger::Interval(2),
                    )
                    // --- on_extraction_error: log failures (concurrent — fire-and-forget) ---
                    .on_extraction_error_concurrent(|name, err| async move {
                        warn!("Extraction '{name}' failed: {err}");
                    })
                    // --- Computed state ---
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
                    .before_tool_response(move |responses, state| {
                        async move {
                            responses.into_iter().map(|mut r| {
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
                    // --- on_tool_call: mock tool dispatch ---
                    .on_tool_call(move |calls, _state| {
                        async move {
                            let mut responses = Vec::new();
                            for call in &calls {
                                let result = execute_tool(&call.name, &call.args);

                                let scheduling = if call.name == "log_compliance_event" {
                                    FunctionResponseScheduling::Silent
                                } else {
                                    FunctionResponseScheduling::WhenIdle
                                };
                                responses.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: result,
                                    id: call.id.clone(),
                                    scheduling: Some(scheduling),
                                });
                            }
                            Some(responses)
                        }
                    })
                    // --- Phase defaults (inherited by all phases) ---
                    .phase_defaults(|d| d
                        .navigation()
                        .with_context(collection_context)
                        .when(risk_is_elevated, RISK_WARNING)
                    )
                    // --- 7 Phases ---
                    // Phase 1: Disclosure (Mini-Miranda)
                    .phase("disclosure")
                        .instruction(DISCLOSURE_INSTRUCTION)
                        .needs(&["disclosure_given"])
                        .prompt_on_enter(true)
                        .transition_with("verify_identity", S::is_true("disclosure_given"), "when disclosure has been given")
                        .transition_with("close", S::is_true("cease_desist_requested"), "when debtor requests cease and desist")
                        .done()
                    // Phase 2: Verify Identity
                    .phase("verify_identity")
                        .instruction(VERIFY_IDENTITY_INSTRUCTION)
                        .needs(&["identity_verified"])
                        .guard(S::is_true("disclosure_given"))
                        .transition_with("inform_debt", S::is_true("identity_verified"), "when identity is verified")
                        .transition_with("close", S::is_true("cease_desist_requested"), "when debtor requests cease and desist")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("debtor_name").unwrap_or_else(|| "the caller".into());
                            format!("{name} confirmed the disclosure. I'll now verify their identity.")
                        })
                        .done()
                    // Phase 3: Inform Debt
                    .phase("inform_debt")
                        .instruction(INFORM_DEBT_INSTRUCTION)
                        .needs(&["debt_acknowledged"])
                        .guard(S::is_true("identity_verified"))
                        .transition_with("negotiate", S::is_true("debt_acknowledged"), "when debt is acknowledged")
                        .transition_with("close", |s| {
                            S::is_true("cease_desist_requested")(s) || S::eq("negotiation_intent", "dispute")(s)
                        }, "when debtor disputes or requests cease and desist")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("debtor_name").unwrap_or_else(|| "the caller".into());
                            format!("{name}'s identity is verified. I'll now inform them about the debt.")
                        })
                        .done()
                    // Phase 4: Negotiate
                    .phase("negotiate")
                        .instruction(NEGOTIATE_INSTRUCTION)
                        .needs(&["negotiation_intent", "willingness_to_pay"])
                        .guard(S::is_true("debt_acknowledged"))
                        .transition_with("arrange_payment", S::one_of("negotiation_intent", &["full_pay", "partial_pay"]), "when debtor agrees to full or partial payment")
                        .transition_with("close", |s| {
                            S::is_true("cease_desist_requested")(s) || S::eq("negotiation_intent", "refuse")(s)
                        }, "when debtor refuses to pay or requests cease and desist")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("debtor_name").unwrap_or_else(|| "the caller".into());
                            let balance: String = s.get::<f64>("balance")
                                .map(|b| format!("${b:.2}"))
                                .unwrap_or_else(|| "the outstanding balance".into());
                            format!("{name} acknowledges the {balance} debt. I'll discuss resolution options.")
                        })
                        .done()
                    // Phase 5: Arrange Payment
                    .phase("arrange_payment")
                        .instruction(ARRANGE_PAYMENT_INSTRUCTION)
                        .needs(&["payment_processed"])
                        .guard(S::one_of("negotiation_intent", &["full_pay", "partial_pay"]))
                        .transition_with("confirm", S::is_true("payment_processed"), "when payment is processed")
                        .transition_with("close", S::is_true("cease_desist_requested"), "when debtor requests cease and desist")
                        .enter_prompt_fn(|s, _| {
                            let intent: String = s.get("negotiation_intent").unwrap_or_default();
                            let label = if intent == "full_pay" { "full payment" } else { "a payment plan" };
                            format!("We've agreed on {label}. I'll now collect the payment details.")
                        })
                        .done()
                    // Phase 6: Confirm
                    .phase("confirm")
                        .instruction(CONFIRM_INSTRUCTION)
                        .guard(S::is_true("payment_processed"))
                        .transition_with("close", |_s| {
                            true
                        }, "after confirmation is complete")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("debtor_name").unwrap_or_else(|| "the caller".into());
                            format!("Payment is processed for {name}. I'll now confirm the agreement details.")
                        })
                        .done()
                    // Phase 7: Close
                    .phase("close")
                        .instruction(CLOSE_INSTRUCTION)
                        .terminal()
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
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppCategory;

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
        assert!(result["confirmation_id"]
            .as_str()
            .unwrap()
            .starts_with("PAY-"));
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
