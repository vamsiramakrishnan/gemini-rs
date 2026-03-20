# Debt Collection Demo Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build an FDCPA-compliant debt collection voice agent demo that exercises 15 previously-unused L2 fluent API features.

**Architecture:** Single-agent, 7-phase conversation flow (`disclosure → verify_identity → inform_debt → negotiate → arrange_payment → confirm → close`) with compliance-gated transitions, emotional monitoring via LLM + regex hybrid extraction, temporal escalation patterns, tool response redaction, and turn boundary compliance injection.

**Tech Stack:** `gemini-adk-fluent-rs` L2 API (`Live::builder()`), `gemini-adk-rs` (State, TurnExtractor, BaseLlm, GeminiLlm), `gemini-genai-rs` (FunctionCall, FunctionResponse, SessionConfig, SessionEvent), `schemars` (LLM extraction schema), `serde/serde_json`, `regex`, `base64`, `tokio`, `async-trait`, `tracing`.

**Design doc:** `docs/plans/2026-03-03-debt-collection-cookbook-design.md`

---

### Task 1: Register the module + add schemars dependency

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Modify: `apps/gemini-adk-web-rs/Cargo.toml`
- Create: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Add `schemars` dependency to Cargo.toml**

In `apps/gemini-adk-web-rs/Cargo.toml`, add to `[dependencies]`:

```toml
schemars = "0.8"
```

This is needed for `#[derive(JsonSchema)]` on the LLM extraction struct (`DebtorState`).

**Step 2: Create the skeleton file**

Create `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`:

```rust
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

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::extractors::RegexExtractor;
use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

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
        todo!("Implemented in subsequent tasks")
    }
}
```

**Step 3: Register in mod.rs**

In `apps/gemini-adk-web-rs/src/apps/mod.rs`, add the module declaration:

```rust
mod debt_collection;
```

And in `register_all()`, add:

```rust
registry.register(debt_collection::DebtCollection);
```

**Step 4: Verify it compiles**

Run: `cargo check -p gemini-genai-ui 2>&1`
Expected: Compiles (with warnings about unused imports — that's fine).

**Step 5: Commit**

```bash
git add apps/gemini-adk-web-rs/Cargo.toml apps/gemini-adk-web-rs/src/apps/mod.rs apps/gemini-adk-web-rs/src/apps/debt_collection.rs
git commit -m "feat(examples): scaffold debt-collection app with CookbookApp impl"
```

---

### Task 2: Phase instructions and constants

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Add all phase instruction constants**

Add these constants above the `DebtCollection` struct:

```rust
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
```

**Step 2: Verify it compiles**

Run: `cargo check -p gemini-genai-ui 2>&1`
Expected: Compiles (constants are unused for now — that's fine).

**Step 3: Commit**

```bash
git add apps/gemini-adk-web-rs/src/apps/debt_collection.rs
git commit -m "feat(examples): add debt-collection phase instructions and mock data"
```

---

### Task 3: Mock tools and PII redaction

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Write tests for the mock tools and redaction**

Add at the bottom of `debt_collection.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-ui -- debt_collection 2>&1`
Expected: FAIL — `execute_tool` and `redact_pii` not found.

**Step 3: Implement the mock tools and redaction**

Add above the `#[cfg(test)]` block:

```rust
// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

/// Execute a mock tool call and return the result as JSON.
fn execute_tool(name: &str, args: &Value) -> Value {
    match name {
        "lookup_account" => {
            // Return mock account data regardless of input account_id.
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
            let interest_rate = 0.05; // 5% flat fee for payment plans
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

/// Redact PII fields from a JSON value before sending to the model or browser.
///
/// - SSN: `123-45-6789` → `***-**-6789`
/// - Account ID: `78234561` → `****4561`
/// - Address: removed entirely
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
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p gemini-genai-ui -- debt_collection 2>&1`
Expected: All 10 tests PASS.

**Step 5: Commit**

```bash
git add apps/gemini-adk-web-rs/src/apps/debt_collection.rs
git commit -m "feat(examples): add debt-collection mock tools and PII redaction"
```

---

### Task 4: Regex extractor for structured fields

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Write tests for regex extraction**

Add to the `tests` module:

```rust
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
        // Should NOT overwrite existing key.
        assert!(!result.contains_key("dollar_amount"));
    }

    #[test]
    fn extract_confirmation_yes() {
        let state = HashMap::new();
        let result = extract_structured("Yes, I understand the disclosure", &state);
        assert_eq!(result.get("disclosure_given"), Some(&json!(true)));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-ui -- debt_collection 2>&1`
Expected: FAIL — `extract_structured` not found.

**Step 3: Implement the regex extraction function**

Add above the mock tool execution section:

```rust
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

/// Extract structured fields from conversation text using regex.
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

    // Disclosure acknowledgment (only extracted once).
    if !existing.contains_key("disclosure_given") {
        // Only match if the text appears to be a response to the disclosure.
        let lower = text.to_lowercase();
        if (lower.contains("understand") || lower.contains("acknowledge"))
            && DISCLOSURE_ACK_RE.is_match(text)
        {
            extracted.insert("disclosure_given".into(), json!(true));
        } else if DISCLOSURE_ACK_RE.is_match(text) && lower.contains("disclosure") {
            extracted.insert("disclosure_given".into(), json!(true));
        } else if DISCLOSURE_ACK_RE.is_match(text)
            && existing.values().count() == 0
        {
            // Early in conversation, a "yes" likely acknowledges the disclosure.
            extracted.insert("disclosure_given".into(), json!(true));
        }
    }

    extracted
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p gemini-genai-ui -- debt_collection 2>&1`
Expected: All 16 tests PASS.

**Step 5: Commit**

```bash
git add apps/gemini-adk-web-rs/src/apps/debt_collection.rs
git commit -m "feat(examples): add debt-collection regex extraction for structured fields"
```

---

### Task 5: LLM extraction struct and computed state derivation

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Write tests for computed state**

Add to the `tests` module:

```rust
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-ui -- debt_collection 2>&1`
Expected: FAIL — functions not found.

**Step 3: Implement LLM extraction struct and computed helpers**

Add above the regex extraction section:

```rust
// ---------------------------------------------------------------------------
// LLM-powered extraction struct
// ---------------------------------------------------------------------------

/// State extracted by the LLM from conversation turns.
///
/// Used with `Live::builder().extract_turns::<DebtorState>(...)`.
/// The LLM analyzes the last 3 turns and fills in whichever fields it can detect.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct DebtorState {
    /// The debtor's current emotional state.
    /// One of: "calm", "cooperative", "frustrated", "angry".
    emotional_state: Option<String>,

    /// How willing the debtor is to pay, from 0.0 (flat refusal) to 1.0 (eager).
    willingness_to_pay: Option<f32>,

    /// The debtor's negotiation intent.
    /// One of: "full_pay", "partial_pay", "dispute", "refuse", "delay".
    negotiation_intent: Option<String>,

    /// Whether the debtor explicitly requested to stop all contact (cease and desist).
    cease_desist_requested: Option<bool>,

    /// Whether the debtor has acknowledged owing the debt.
    debt_acknowledged: Option<bool>,
}

// ---------------------------------------------------------------------------
// Computed state helpers
// ---------------------------------------------------------------------------

/// Map an emotional state string to a numeric sentiment score (0.0–1.0).
fn sentiment_from_emotion(emotion: &str) -> f64 {
    match emotion {
        "cooperative" => 0.9,
        "calm" => 0.7,
        "frustrated" => 0.4,
        "angry" => 0.2,
        _ => 0.5,
    }
}

/// Compute the call risk level from sentiment and cease-desist status.
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
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p gemini-genai-ui -- debt_collection 2>&1`
Expected: All 22 tests PASS.

**Step 5: Commit**

```bash
git add apps/gemini-adk-web-rs/src/apps/debt_collection.rs
git commit -m "feat(examples): add DebtorState LLM extraction struct and computed helpers"
```

---

### Task 6: Implement handle_session — the full L2 pipeline

This is the core task. It wires up everything: callbacks, phases, watchers, temporal patterns, extractors, interceptors, and the browser recv loop.

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs`

**Step 1: Replace the `todo!()` in `handle_session` with the full implementation**

Replace the `handle_session` method body with:

```rust
    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;
        let selected_voice = resolve_voice(start.voice.as_deref());

        // Build session config for voice mode.
        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(DISCLOSURE_INSTRUCTION);

        // Build LLM client for extraction (uses same credentials).
        let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));

        let b64 = base64::engine::general_purpose::STANDARD;

        // Clone tx for all callbacks.
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
        let tx_tool = tx.clone();
        let tx_extracted = tx.clone();
        let tx_phase_enter = tx.clone();
        let tx_phase_exit = tx.clone();
        let tx_cease = tx.clone();
        let tx_sentiment_drop = tx.clone();
        let tx_willingness = tx.clone();
        let tx_dispute = tx.clone();
        let tx_id_revoked = tx.clone();
        let tx_sustained = tx.clone();
        let tx_rate = tx.clone();
        let tx_stalled = tx.clone();
        let tx_goaway = tx.clone();

        let handle = Live::builder()
            // -- Regex extractor for structured fields --
            .extractor(Arc::new(RegexExtractor::new(
                "debt_fields",
                5,
                extract_structured,
            )))
            // -- LLM extractor for nuanced state --
            .extract_turns::<DebtorState>(
                llm,
                "Analyze the debtor's emotional state, willingness to pay (0.0-1.0), \
                 negotiation intent (full_pay/partial_pay/dispute/refuse/delay), \
                 whether they requested cease-and-desist, and whether they acknowledged the debt. \
                 Only fill in fields you are confident about from the conversation.",
            )
            // -- Extraction callback: broadcast all extracted state to browser --
            .on_extracted(move |name, value| {
                let tx = tx_extracted.clone();
                async move {
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: format!("extracted:{name}"),
                        value,
                    });
                }
            })
            // -- Computed state --
            .computed("sentiment_score", &["emotional_state"], |state| {
                let emotion: String = state.get("emotional_state")?;
                Some(json!(sentiment_from_emotion(&emotion)))
            })
            .computed("call_risk_level", &["sentiment_score", "cease_desist_requested"], |state| {
                let sentiment: f64 = state.get("sentiment_score").unwrap_or(0.5);
                let cease: bool = state.get("cease_desist_requested").unwrap_or(false);
                Some(json!(compute_risk_level(sentiment, cease)))
            })
            // -- PII redaction interceptor --
            .before_tool_response(move |responses, _state| {
                async move {
                    responses
                        .into_iter()
                        .map(|mut resp| {
                            resp.response = redact_pii(&resp.response);
                            resp
                        })
                        .collect()
                }
            })
            // -- Turn boundary: compliance reminder --
            .on_turn_boundary(move |state, _writer| {
                async move {
                    let disclosed: bool = state.get("disclosure_given").unwrap_or(false);
                    if !disclosed {
                        info!("Turn boundary: disclosure not yet given, injecting reminder");
                    }
                }
            })
            // -- Phase machine --
            .phase("disclosure")
                .instruction(DISCLOSURE_INSTRUCTION)
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "none".into(),
                                to: "disclosure".into(),
                                reason: "Session started".into(),
                            });
                        }
                    }
                })
                .on_exit({
                    let tx = tx_phase_exit.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "compliance_log".into(),
                                value: json!({"event": "disclosure_completed"}),
                            });
                        }
                    }
                })
                .transition("verify_identity", |s| {
                    s.get::<bool>("disclosure_given").unwrap_or(false)
                })
                .done()
            .phase("verify_identity")
                .instruction(VERIFY_IDENTITY_INSTRUCTION)
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "disclosure".into(),
                                to: "verify_identity".into(),
                                reason: "Disclosure acknowledged".into(),
                            });
                        }
                    }
                })
                .on_exit({
                    let tx = tx_phase_exit.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "compliance_log".into(),
                                value: json!({"event": "identity_verified"}),
                            });
                        }
                    }
                })
                .transition("inform_debt", |s| {
                    s.get::<bool>("identity_verified").unwrap_or(false)
                })
                .done()
            .phase("inform_debt")
                .instruction(INFORM_DEBT_INSTRUCTION)
                .guard(|s| s.get::<bool>("identity_verified").unwrap_or(false))
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "verify_identity".into(),
                                to: "inform_debt".into(),
                                reason: "Identity confirmed".into(),
                            });
                        }
                    }
                })
                .transition("close", |s| {
                    s.get::<String>("negotiation_intent")
                        .map_or(false, |i| i == "dispute")
                })
                .transition("negotiate", |s| {
                    s.get::<bool>("debt_acknowledged").unwrap_or(false)
                })
                .done()
            .phase("negotiate")
                .instruction(NEGOTIATE_INSTRUCTION)
                .guard(|s| {
                    let verified = s.get::<bool>("identity_verified").unwrap_or(false);
                    let disclosed = s.get::<bool>("disclosure_given").unwrap_or(false);
                    let disputed = s.get::<String>("negotiation_intent")
                        .map_or(false, |i| i == "dispute");
                    verified && disclosed && !disputed
                })
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "inform_debt".into(),
                                to: "negotiate".into(),
                                reason: "Debt acknowledged".into(),
                            });
                        }
                    }
                })
                .transition("arrange_payment", |s| {
                    let willing = s.get::<f64>("willingness_to_pay").unwrap_or(0.0);
                    willing > 0.7
                })
                .done()
            .phase("arrange_payment")
                .instruction(ARRANGE_PAYMENT_INSTRUCTION)
                .guard(|s| {
                    s.get::<f64>("willingness_to_pay").unwrap_or(0.0) > 0.7
                })
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "negotiate".into(),
                                to: "arrange_payment".into(),
                                reason: "Customer willing to pay".into(),
                            });
                        }
                    }
                })
                .transition("confirm", |s| {
                    s.get::<Value>("debt_fields")
                        .and_then(|v| v.get("payment_confirmed"))
                        .is_some()
                })
                .done()
            .phase("confirm")
                .instruction(CONFIRM_INSTRUCTION)
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "arrange_payment".into(),
                                to: "confirm".into(),
                                reason: "Payment processed".into(),
                            });
                        }
                    }
                })
                .transition("close", |_s| {
                    // Manual transition — the instruction tells the model to wrap up.
                    false
                })
                .done()
            .phase("close")
                .instruction(CLOSE_INSTRUCTION)
                .terminal()
                .on_enter({
                    let tx = tx_phase_enter.clone();
                    move |state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let reason = if state.get::<bool>("cease_desist_requested").unwrap_or(false) {
                                "Cease and desist requested"
                            } else if state.get::<String>("negotiation_intent").map_or(false, |i| i == "dispute") {
                                "Debt disputed — validation notice required"
                            } else {
                                "Call concluding"
                            };
                            let _ = tx.send(ServerMessage::PhaseChange {
                                from: "previous".into(),
                                to: "close".into(),
                                reason: reason.into(),
                            });
                        }
                    }
                })
                .done()
            .initial_phase("disclosure")
            // -- State-reactive instruction template --
            .instruction_template(|state| {
                let phase: String = state.get("session:phase").unwrap_or_default();
                let base = match phase.as_str() {
                    "disclosure" => DISCLOSURE_INSTRUCTION,
                    "verify_identity" => VERIFY_IDENTITY_INSTRUCTION,
                    "inform_debt" => INFORM_DEBT_INSTRUCTION,
                    "negotiate" => NEGOTIATE_INSTRUCTION,
                    "arrange_payment" => ARRANGE_PAYMENT_INSTRUCTION,
                    "confirm" => CONFIRM_INSTRUCTION,
                    "close" => CLOSE_INSTRUCTION,
                    _ => DISCLOSURE_INSTRUCTION,
                };

                let mut instruction = base.to_string();

                // Inject context from extracted state.
                let sentiment: f64 = state.get("sentiment_score").unwrap_or(0.5);
                if sentiment < 0.4 {
                    instruction.push_str("\n\nIMPORTANT: The customer is frustrated. Show extra empathy, \
                        acknowledge their feelings, and be patient. Do not pressure them.");
                }

                let cease: bool = state.get("cease_desist_requested").unwrap_or(false);
                if cease {
                    instruction.push_str("\n\nCRITICAL: The customer has requested to stop all contact. \
                        You MUST immediately stop discussing the debt. Acknowledge their request, \
                        confirm that all collection activity will cease, and end the call professionally.");
                }

                Some(instruction)
            })
            // -- Numeric watchers --
            .watch("willingness_to_pay")
                .crossed_above(0.7)
                .then(move |_old, _new, _state| {
                    let tx = tx_willingness.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "negotiation_signal".into(),
                            value: json!("ready_to_pay"),
                        });
                    }
                })
            .watch("sentiment_score")
                .crossed_below(0.3)
                .blocking()
                .then(move |_old, _new, _state| {
                    let tx = tx_sentiment_drop.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "risk_alert".into(),
                            value: json!("de-escalation activated"),
                        });
                    }
                })
            // -- Boolean watchers --
            .watch("cease_desist_requested")
                .became_true()
                .blocking()
                .then(move |_old, _new, _state| {
                    let tx = tx_cease.clone();
                    async move {
                        let _ = tx.send(ServerMessage::Violation {
                            rule: "cease_desist".into(),
                            severity: "critical".into(),
                            detail: "Customer requested cease and desist — halting collection".into(),
                        });
                    }
                })
            .watch("identity_verified")
                .became_false()
                .then(move |_old, _new, _state| {
                    let tx = tx_id_revoked.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "verification_revoked".into(),
                            value: json!(true),
                        });
                    }
                })
            // -- Value watcher --
            .watch("negotiation_intent")
                .changed_to(json!("dispute"))
                .blocking()
                .then(move |_old, _new, _state| {
                    let tx = tx_dispute.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "debt_status".into(),
                            value: json!("disputed"),
                        });
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "action_required".into(),
                            value: json!("send_validation_notice"),
                        });
                    }
                })
            // -- Temporal patterns --
            .when_sustained(
                "sustained_frustration",
                |state| state.get::<f64>("sentiment_score").map_or(false, |s| s < 0.4),
                Duration::from_secs(30),
                {
                    let tx = tx_sustained.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "temporal_alert".into(),
                                value: json!("sustained_frustration"),
                            });
                        }
                    }
                },
            )
            .when_rate(
                "rapid_objections",
                |evt| matches!(evt, SessionEvent::TurnComplete),
                3,
                Duration::from_secs(60),
                {
                    let tx = tx_rate.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "escalation".into(),
                                value: json!("supervisor_recommended"),
                            });
                        }
                    }
                },
            )
            .when_turns(
                "stalled_conversation",
                |state| {
                    // No new extraction keys in last check.
                    state.get::<Value>("debt_fields")
                        .and_then(|v| v.as_object())
                        .map_or(true, |obj| obj.is_empty())
                },
                5,
                {
                    let tx = tx_stalled.clone();
                    move |_state, _writer| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: "temporal_alert".into(),
                                value: json!("stalled_suggest_callback"),
                            });
                        }
                    }
                },
            )
            // -- Tool call handler --
            .on_tool_call(move |calls| {
                let tx = tx_tool.clone();
                async move {
                    let responses: Vec<gemini_genai_rs::prelude::FunctionResponse> = calls
                        .iter()
                        .map(|call| {
                            let result = execute_tool(&call.name, &call.args);
                            info!("Tool '{}' -> {}", call.name, result);
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: format!("tool:{}", call.name),
                                value: json!({
                                    "name": call.name,
                                    "args": call.args,
                                    "result": redact_pii(&result),
                                }),
                            });
                            gemini_genai_rs::prelude::FunctionResponse {
                                name: call.name.clone(),
                                response: result,
                                id: call.id.clone(),
                            }
                        })
                        .collect();
                    Some(responses)
                }
            })
            // -- Fast lane callbacks --
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
            .on_go_away(move |duration| {
                let tx = tx_goaway.clone();
                async move {
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "session_ending".into(),
                        value: json!({"reason": "server_goaway", "time_remaining_secs": duration.as_secs()}),
                    });
                }
            })
            .on_disconnected(move |_reason| {
                let _tx = tx_disconnected.clone();
                async move {
                    info!("Debt collection session disconnected by server");
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        // Connected — send initial state to browser.
        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("Debt collection session connected");

        let _ = tx.send(ServerMessage::StateUpdate {
            key: "initial_phase".into(),
            value: json!("disclosure"),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "compliance_status".into(),
            value: json!({"disclosure": false, "identity_verified": false}),
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
                    info!("Debt collection session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
```

**Step 2: Verify it compiles**

Run: `cargo check -p gemini-genai-ui 2>&1`
Expected: Compiles (possibly with warnings about unused variables which is fine).

**Important**: This step may require adjusting import paths or type signatures based on the exact API. If `SessionEvent` is not in scope, add `use gemini_genai_rs::session::SessionEvent;`. If `FunctionResponse` isn't in `gemini_genai_rs::prelude`, use `gemini_genai_rs::protocol::types::FunctionResponse`. Adapt as needed — the exact types are documented in the design doc.

**Step 3: Run all tests**

Run: `cargo test -p gemini-genai-ui 2>&1`
Expected: All existing 67 tests + all new debt_collection tests PASS.

**Step 4: Commit**

```bash
git add apps/gemini-adk-web-rs/src/apps/debt_collection.rs
git commit -m "feat(examples): implement debt-collection handle_session with full L2 pipeline

Exercises 15 previously-unused L2 features: phase guards, on_exit,
temporal patterns (when_sustained, when_rate, when_turns), numeric
watchers (crossed_above, crossed_below), boolean watchers (became_true,
became_false), value watcher (changed_to), before_tool_response,
on_turn_boundary, extract_turns::<T>(), on_go_away, and computed state."
```

---

### Task 7: Final verification and cleanup

**Files:**
- Possibly modify: `apps/gemini-adk-web-rs/src/apps/debt_collection.rs` (fix any warnings)

**Step 1: Full workspace check**

Run: `cargo check --workspace 2>&1`
Expected: Compiles. Only pre-existing warnings (Evaluation variant, Other variant in app.rs).

**Step 2: Full workspace tests**

Run: `cargo test --workspace 2>&1`
Expected: All tests pass across all crates.

**Step 3: Check for new warnings in debt_collection**

Run: `cargo check -p gemini-genai-ui 2>&1 | grep debt_collection`
Expected: No warnings from debt_collection.rs.

If there are unused import warnings, fix them. If there are dead_code warnings on helper functions only used at runtime, that's expected.

**Step 4: Commit any cleanups**

```bash
git add -u
git commit -m "refactor(examples): clean up debt-collection warnings"
```

**Step 5: Push to remote**

```bash
git push origin main
```
