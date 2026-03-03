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
