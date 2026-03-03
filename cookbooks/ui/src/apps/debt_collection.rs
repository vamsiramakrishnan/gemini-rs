use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;
use rs_adk::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::extractors::RegexExtractor;
use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

// ---------------------------------------------------------------------------
// Phase instructions
// ---------------------------------------------------------------------------

const DISCLOSURE_INSTRUCTION: &str = "\
You are a professional debt collection agent. You MUST begin by delivering the Mini-Miranda disclosure exactly as follows:\n\n\
\"This is an attempt to collect a debt. Any information obtained will be used for that purpose. \
This call may be monitored or recorded for quality assurance.\"\n\n\
After delivering this disclosure, ask the customer to confirm they understand. \
Be professional and courteous at all times. Do NOT discuss any debt details until the disclosure is acknowledged.";

const VERIFY_IDENTITY_INSTRUCTION: &str = "\
You need to verify the customer's identity before discussing any account details. \
Ask for their full name, date of birth, and the last four digits of their Social Security Number. \
Use the verify_identity tool to confirm their identity. \
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
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
        todo!("Implemented in Task 6")
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
