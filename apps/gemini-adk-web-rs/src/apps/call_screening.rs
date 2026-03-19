use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::info;

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};
use gemini_adk_rs::state::StateKey;

use crate::app::{AppError, ClientMessage, DemoApp, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

use super::resolve_voice;

// ---------------------------------------------------------------------------
// StateKey constants (documentation / future typed access)
// ---------------------------------------------------------------------------

const CALLER_NAME: StateKey<String> = StateKey::new("caller_name");
const CALLER_ORG: StateKey<String> = StateKey::new("caller_org");
const CALL_PURPOSE: StateKey<String> = StateKey::new("call_purpose");
const URGENCY: StateKey<f64> = StateKey::new("urgency");
const IS_KNOWN_CONTACT: StateKey<bool> = StateKey::new("is_known_contact");
const CALLER_SENTIMENT: StateKey<String> = StateKey::new("caller_sentiment");
const MESSAGE_TAKEN: StateKey<bool> = StateKey::new("message_taken");
const CALL_TRANSFERRED: StateKey<bool> = StateKey::new("call_transferred");

// Silence unused-constant warnings — these exist as documentation and for
// future typed-state access via `state.get_key(&KEY)`.
const _: () = {
    _ = CALLER_NAME;
    _ = CALLER_ORG;
    _ = CALL_PURPOSE;
    _ = URGENCY;
    _ = IS_KNOWN_CONTACT;
    _ = CALLER_SENTIMENT;
    _ = MESSAGE_TAKEN;
    _ = CALL_TRANSFERRED;
};

// ---------------------------------------------------------------------------
// System instruction
// ---------------------------------------------------------------------------

const SYSTEM_INSTRUCTION: &str = "\
You are a professional call screening assistant for Alex Rivera. \
You screen incoming calls, identify callers, determine purpose, and either \
transfer important calls, take messages, or politely decline. \
Be professional, warm, but appropriately cautious with unknown callers.";

// ---------------------------------------------------------------------------
// Phase instructions
// ---------------------------------------------------------------------------

// Phase instructions — lean directives for what to do in each phase.
// Contextual awareness ("where we are, what we know") is provided by
// the screening_context() closure via with_context, so the model always
// has situational bearings without repeating state in the instructions.

const GREETING_INSTRUCTION: &str = "\
Greet the caller warmly. Ask who is calling and the purpose of their call. \
If a name is provided, use check_contact_list. \
Do not reveal Alex's availability yet.";

const IDENTIFY_CALLER_INSTRUCTION: &str = "\
Get the caller's full name and organization. \
Use check_contact_list to verify if they are a known contact. \
Be patient but firm — Alex requires knowing who is calling.";

const DETERMINE_PURPOSE_INSTRUCTION: &str = "\
Ask why they are calling and assess urgency. \
Use check_calendar to see if Alex has availability. \
Ask clarifying questions if the purpose is vague.";

const SCREEN_DECISION_INSTRUCTION: &str = "\
Decide the appropriate action based on what you know:\n\
- Known contact or urgent matter → transfer the call\n\
- Unknown but legitimate → take a message\n\
- Hostile or spam → politely decline\n\n\
Use the appropriate tool. Be decisive but professional.";

const TAKE_MESSAGE_INSTRUCTION: &str = "\
Collect the caller's name, callback number, and message. \
Confirm details back before saving. \
Let them know Alex will receive the message.";

const TRANSFER_INSTRUCTION: &str = "\
Inform the caller you are transferring them to Alex. \
Use transfer_call. Let them know to hold briefly.";

const FAREWELL_INSTRUCTION: &str = "\
Thank the caller. Confirm any actions taken \
(message delivered, call transferred, or call declined). \
Say goodbye professionally.";

/// Builds a conversational context summary from accumulated state.
/// This is the "geolocation" — the model always knows where it is,
/// what it's gathered so far, and what's still needed.
fn screening_context(s: &State) -> String {
    let mut ctx = Vec::new();

    // Who is calling?
    let name: Option<String> = s.get("caller_name");
    let org: Option<String> = s.get("caller_organization");
    let known: bool = s.get("is_known_contact").unwrap_or(false);

    match (&name, &org) {
        (Some(n), Some(o)) => {
            let tag = if known {
                "known contact"
            } else {
                "not in contacts"
            };
            ctx.push(format!("Caller: {n} from {o} ({tag})."));
        }
        (Some(n), None) => {
            let tag = if known {
                "known contact"
            } else {
                "not in contacts"
            };
            ctx.push(format!("Caller: {n} ({tag}). Organization unknown."));
        }
        _ => {}
    }

    // Sentiment
    let sentiment: String = s.get("caller_sentiment").unwrap_or_default();
    if !sentiment.is_empty() && sentiment != "neutral" {
        ctx.push(format!("Caller seems {sentiment}."));
    }

    // Purpose & urgency
    if let Some(purpose) = s.get::<String>("call_purpose") {
        ctx.push(format!("Purpose: {purpose}."));
    }
    let urgency: f64 = s.get("urgency_level").unwrap_or(0.0);
    if urgency > 0.0 {
        let label = if urgency > 0.7 {
            "high"
        } else if urgency > 0.4 {
            "moderate"
        } else {
            "low"
        };
        ctx.push(format!("Urgency: {label} ({urgency:.1})."));
    }

    // Actions taken
    if s.get::<bool>("message_taken").unwrap_or(false) {
        ctx.push("A message has been recorded.".into());
    }
    if s.get::<bool>("call_transferred").unwrap_or(false) {
        ctx.push("Call transferred to Alex.".into());
    }
    if s.get::<bool>("caller_blocked").unwrap_or(false) {
        ctx.push("Caller has been blocked.".into());
    }

    if ctx.is_empty() {
        String::new()
    } else {
        ctx.join(" ")
    }
}

// ---------------------------------------------------------------------------
// Per-phase state keys for instruction modifiers
// ---------------------------------------------------------------------------

const SCREEN_STATE_KEYS: &[&str] = &[
    "caller_name",
    "caller_organization",
    "call_purpose",
    "urgency_level",
    "caller_sentiment",
    "derived:screen_recommendation",
    "is_known_contact",
];

const CAUTION_WARNING: &str = "\
IMPORTANT: The caller is showing hostile behavior. Be extra cautious. \
Do not reveal any personal information about Alex. If the caller becomes \
threatening, politely end the call.";

fn caller_is_hostile(s: &State) -> bool {
    let sentiment: String = s
        .get("caller_sentiment")
        .unwrap_or_else(|| "neutral".to_string());
    sentiment == "hostile"
}

// ---------------------------------------------------------------------------
// LLM-powered extraction struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct CallerState {
    /// The caller's name, if provided
    caller_name: Option<String>,
    /// The caller's organization, if provided
    caller_organization: Option<String>,
    /// The purpose of the call
    call_purpose: Option<String>,
    /// Urgency level from 0.0 (not urgent) to 1.0 (extremely urgent)
    urgency_level: Option<f64>,
    /// Caller's sentiment: friendly, neutral, impatient, or hostile
    caller_sentiment: Option<String>,
}

// ---------------------------------------------------------------------------
// Computed state helpers
// ---------------------------------------------------------------------------

fn compute_screen_decision(known: bool, urgency: f64, sentiment: &str) -> &'static str {
    if known || urgency > 0.8 {
        "transfer"
    } else if sentiment == "hostile" {
        "decline"
    } else {
        "take_message"
    }
}

// ---------------------------------------------------------------------------
// Tool declarations
// ---------------------------------------------------------------------------

/// Build the tool declarations so Gemini knows what functions it can call.
fn call_screening_tools() -> gemini_genai_rs::prelude::Tool {
    use gemini_genai_rs::prelude::{FunctionCallingBehavior, FunctionDeclaration, Tool};
    Tool::functions(vec![
        FunctionDeclaration {
            name: "check_contact_list".into(),
            description: "Check if a caller is in Alex's contact list".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The caller's name to look up"
                    },
                    "organization": {
                        "type": "string",
                        "description": "The caller's organization (optional)"
                    }
                },
                "required": ["name"]
            })),
            behavior: Some(FunctionCallingBehavior::NonBlocking),
        },
        FunctionDeclaration {
            name: "check_calendar".into(),
            description: "Check Alex's calendar for today's schedule and upcoming meetings".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "date": {
                        "type": "string",
                        "description": "The date to check (optional, defaults to today)"
                    }
                },
                "required": []
            })),
            behavior: Some(FunctionCallingBehavior::NonBlocking),
        },
        FunctionDeclaration {
            name: "take_message".into(),
            description: "Record a message for Alex".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "from_name": {
                        "type": "string",
                        "description": "Name of the person leaving the message"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message content"
                    },
                    "callback_number": {
                        "type": "string",
                        "description": "Callback phone number (optional)"
                    },
                    "urgency": {
                        "type": "string",
                        "description": "Urgency level: low, normal, high (optional)"
                    }
                },
                "required": ["from_name", "message"]
            })),
            behavior: Some(FunctionCallingBehavior::NonBlocking),
        },
        FunctionDeclaration {
            name: "transfer_call".into(),
            description: "Transfer the call to Alex".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Reason for transferring the call"
                    }
                },
                "required": ["reason"]
            })),
            behavior: None,
        },
        FunctionDeclaration {
            name: "block_caller".into(),
            description: "Block a caller from future calls".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Reason for blocking the caller"
                    }
                },
                "required": ["reason"]
            })),
            behavior: None,
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &serde_json::Value) -> serde_json::Value {
    match name {
        "check_contact_list" => {
            let caller = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let (found, relationship, priority) = match caller.to_lowercase().as_str() {
                s if s.contains("jane smith") => (true, "colleague — Marketing team", "normal"),
                s if s.contains("bob johnson") => (true, "manager — direct report to", "high"),
                s if s.contains("dr. patel") || s.contains("patel") => {
                    (true, "dentist — personal", "low")
                }
                _ => (false, "unknown", "unknown"),
            };
            json!({ "found": found, "relationship": relationship, "priority": priority })
        }
        "check_calendar" => {
            json!({
                "meetings_today": [
                    { "time": "10:00 AM", "title": "Team Standup", "duration": "30min" },
                    { "time": "2:00 PM", "title": "Project Review with Bob", "duration": "1hr" },
                ],
                "next_available": "11:00 AM",
                "busy_until": "10:30 AM"
            })
        }
        "take_message" => {
            json!({
                "message_id": format!("MSG-{:04}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() % 10000),
                "status": "recorded",
                "will_notify": true,
                "estimated_callback": "within 2 hours"
            })
        }
        "transfer_call" => {
            json!({ "status": "transferring", "message": "Connecting you now..." })
        }
        "block_caller" => {
            json!({ "status": "blocked", "effective": "immediately" })
        }
        _ => json!({ "error": format!("Unknown tool: {name}") }),
    }
}

// ---------------------------------------------------------------------------
// CallScreening app
// ---------------------------------------------------------------------------

/// AI call screening with caller identification, urgency detection, and smart routing.
pub struct CallScreening;

#[async_trait]
impl DemoApp for CallScreening {
    demo_meta! {
        name: "call-screening",
        description: "AI call screening with caller identification, urgency detection, and smart routing",
        category: Showcase,
        features: [
            "phase-machine",
            "llm-extraction",
            "tool-calling",
            "watchers",
            "computed-state",
            "temporal-patterns",
            "state-keys",
        ],
        tips: [
            "Try calling as a known contact like 'Jane Smith from Marketing'",
            "Call with an urgent matter to trigger auto-transfer",
            "Be hostile to test the decline flow",
        ],
        try_saying: [
            "Hi, this is Jane Smith from the marketing team. Is Alex available?",
            "Hello, I'm calling about an urgent delivery issue.",
            "I'd like to speak to whoever is in charge.",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("CallScreening session starting");

        // Create GeminiLlm for LLM extraction (background agent uses flash-lite at global)
        let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
            model: Some("gemini-3.1-flash-lite-preview".to_string()),
            location: Some("global".to_string()),
            ..Default::default()
        }));

        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());

                // =================================================================
                // DESIGN DISSECTION: Why this app is built the way it is
                // =================================================================
                //
                // Steering Mode: ContextInjection
                //   The personal assistant persona ("Alex Chen's assistant") is
                //   consistent across all phases. Phase instructions simply change
                //   what the assistant focuses on (screening, purpose, routing) —
                //   the identity never shifts. ContextInjection avoids unnecessary
                //   system instruction churn.
                //
                // greeting("..."):
                //   A personal assistant should answer the phone with a professional
                //   greeting. This fires once at session start — the model
                //   introduces itself before any screening logic runs.
                //
                // prompt_on_enter(true) on greeting ONLY:
                //   Only the greeting phase prompts the model to speak first. All
                //   other transitions are caller-driven — the caller provides their
                //   name, states their purpose, etc. The model responds naturally
                //   to this input without needing prompt_on_enter.
                //
                // enter_prompt (static) on identify_caller:
                //   "Ask the caller for their full name and organization." — a
                //   simple static bridge because the greeting-to-identify transition
                //   always needs the same prompt regardless of state.
                //
                // enter_prompt_fn on remaining phases:
                //   State-dependent bridges that summarize what's known (e.g., "The
                //   caller is John from Acme Corp. Let me determine their purpose.")
                //   This gives the model continuity without prompt_on_enter.
                //
                // Turn-count fallback (12 turns) on identify_caller:
                //   A generous safety net for callers who refuse to identify
                //   themselves. 12 turns is intentionally high — it's a last-resort
                //   escape hatch, not flow control. The LLM will naturally keep
                //   asking; this just prevents infinite loops.
                //
                // with_context(screening_context):
                //   Every phase gets caller info (name, org, purpose, urgency)
                //   injected as context. This lets the model make informed routing
                //   decisions without the state being in the system instruction.
                // =================================================================

                live.model(GeminiModel::Gemini2_0FlashLive)
                    .voice(voice)
                    .instruction(SYSTEM_INSTRUCTION)
                    .transcription(true, true)
                    .add_tool(call_screening_tools())
                    .steering_mode(SteeringMode::ContextInjection)
                    .context_delivery(ContextDelivery::Deferred)
                    // --- Model-initiated greeting ---
                    .greeting(
                        "A new call is coming in. Greet the caller professionally and ask who is calling.",
                    )
                    // --- LLM extraction ---
                    .extract_turns_triggered::<CallerState>(
                        llm,
                        "Extract from the call screening conversation: caller_name, \
                         caller_organization, call_purpose, urgency_level (0.0-1.0), \
                         caller_sentiment (friendly/neutral/impatient/hostile).",
                        5,
                        ExtractionTrigger::Interval(2),
                    )
                    // --- Computed state ---
                    .computed(
                        "screen_recommendation",
                        &["is_known_contact", "urgency_level", "caller_sentiment"],
                        |state| {
                            let known: bool = state.get("is_known_contact").unwrap_or(false);
                            let urgency: f64 = state.get("urgency_level").unwrap_or(0.0);
                            let sentiment: String = state
                                .get("caller_sentiment")
                                .unwrap_or_else(|| "neutral".to_string());
                            Some(json!(compute_screen_decision(known, urgency, &sentiment)))
                        },
                    )
                    // --- before_tool_response: state promotion from tool results ---
                    .before_tool_response(move |responses, state| async move {
                        responses
                            .into_iter()
                            .inspect(|r| match r.name.as_str() {
                                "check_contact_list" => {
                                    if r.response
                                        .get("found")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false)
                                    {
                                        state.set("is_known_contact", true);
                                    }
                                }
                                "take_message" => {
                                    if r.response.get("status").and_then(|v| v.as_str())
                                        == Some("recorded")
                                    {
                                        state.set("message_taken", true);
                                    }
                                }
                                "transfer_call" => {
                                    if r.response.get("status").and_then(|v| v.as_str())
                                        == Some("transferring")
                                    {
                                        state.set("call_transferred", true);
                                    }
                                }
                                "block_caller" => {
                                    if r.response.get("status").and_then(|v| v.as_str())
                                        == Some("blocked")
                                    {
                                        state.set("caller_blocked", true);
                                    }
                                }
                                _ => {}
                            })
                            .collect()
                    })
                    // --- on_tool_call: mock tool dispatch ---
                    .on_tool_call(move |calls, _state| async move {
                        let mut responses = Vec::new();
                        for call in &calls {
                            let result = execute_tool(&call.name, &call.args);
                            responses.push(FunctionResponse {
                                name: call.name.clone(),
                                response: result,
                                id: call.id.clone(),
                                scheduling: Some(FunctionResponseScheduling::WhenIdle),
                            });
                        }
                        Some(responses)
                    })
                    // --- Phase defaults (inherited by all phases) ---
                    .phase_defaults(|d| {
                        d.navigation()
                            .with_context(screening_context)
                            .when(caller_is_hostile, CAUTION_WARNING)
                    })
                    // --- 7 Phases ---
                    // Phase 1: Greeting
                    .phase("greeting")
                    .instruction(GREETING_INSTRUCTION)
                    .tools(vec!["check_contact_list".into()])
                    .prompt_on_enter(true)
                    .transition_with(
                        "identify_caller",
                        |s: &State| {
                            let tc: u32 = s.session().get("turn_count").unwrap_or(0);
                            tc >= 2
                        },
                        "after initial greeting exchange (2+ turns)",
                    )
                    .done()
                    // Phase 2: Identify Caller
                    .phase("identify_caller")
                    .instruction(IDENTIFY_CALLER_INSTRUCTION)
                    .tools(vec!["check_contact_list".into()])
                    .needs(&["caller_name", "caller_organization"])
                    .transition_with(
                        "determine_purpose",
                        |s: &State| {
                            let name: Option<String> = s.get("caller_name");
                            name.is_some()
                        },
                        "when caller name is provided",
                    )
                    .transition_with(
                        "take_message",
                        |s: &State| {
                            let tc: u32 = s.session().get("turn_count").unwrap_or(0);
                            let name: Option<String> = s.get("caller_name");
                            tc >= 12 && name.is_none()
                        },
                        "after 12 turns if caller refuses to identify",
                    )
                    .enter_prompt("Ask the caller for their full name and organization.")
                    .done()
                    // Phase 3: Determine Purpose
                    .phase("determine_purpose")
                    .instruction(DETERMINE_PURPOSE_INSTRUCTION)
                    .tools(vec!["check_calendar".into()])
                    .needs(&["call_purpose", "urgency_level"])
                    .transition_with(
                        "screen_decision",
                        |s: &State| s.get::<String>("call_purpose").is_some(),
                        "when call purpose is established",
                    )
                    .enter_prompt_fn(|s, _| {
                        let name: String = s.get("caller_name").unwrap_or_default();
                        format!(
                            "I've confirmed the caller is {name}. Now ask them why they're calling."
                        )
                    })
                    .done()
                    // Phase 4: Screen Decision
                    .phase("screen_decision")
                    .instruction(SCREEN_DECISION_INSTRUCTION)
                    .tools(vec![
                        "transfer_call".into(),
                        "take_message".into(),
                        "block_caller".into(),
                    ])
                    .transition_with(
                        "transfer",
                        |s: &State| {
                            s.get::<bool>("is_known_contact").unwrap_or(false)
                                || s.get::<f64>("urgency_level").unwrap_or(0.0) > 0.8
                        },
                        "known contact or high urgency → transfer",
                    )
                    .transition_with(
                        "farewell",
                        S::is_true("caller_blocked"),
                        "caller blocked → end call",
                    )
                    .transition_with(
                        "take_message",
                        |s: &State| {
                            !s.get::<bool>("is_known_contact").unwrap_or(false)
                                && s.get::<f64>("urgency_level").unwrap_or(0.0) <= 0.8
                        },
                        "unknown caller with low urgency → take message",
                    )
                    .enter_prompt_fn(|s, _| {
                        let name: String = s.get("caller_name").unwrap_or_default();
                        let purpose: String = s.get("call_purpose").unwrap_or_default();
                        format!(
                            "{name} is calling about: {purpose}. Decide the best course of action."
                        )
                    })
                    .done()
                    // Phase 5: Take Message
                    .phase("take_message")
                    .instruction(TAKE_MESSAGE_INSTRUCTION)
                    .tools(vec!["take_message".into()])
                    .transition_with(
                        "farewell",
                        S::is_true("message_taken"),
                        "message has been recorded",
                    )
                    .enter_prompt_fn(|s, _| {
                        let name: String =
                            s.get("caller_name").unwrap_or_else(|| "the caller".into());
                        format!(
                            "Let {name} know Alex is unavailable. Ask if they'd like to leave a message."
                        )
                    })
                    .done()
                    // Phase 6: Transfer
                    .phase("transfer")
                    .instruction(TRANSFER_INSTRUCTION)
                    .tools(vec!["transfer_call".into()])
                    .transition_with(
                        "farewell",
                        S::is_true("call_transferred"),
                        "call has been transferred",
                    )
                    .enter_prompt("I'll transfer the caller to Alex now.")
                    .done()
                    // Phase 7: Farewell (terminal)
                    .phase("farewell")
                    .instruction(FAREWELL_INSTRUCTION)
                    .terminal()
                    .enter_prompt_fn(|state, _tw| {
                        if S::is_true("call_transferred")(state) {
                            "The call has been transferred. I'll say goodbye now.".into()
                        } else if S::is_true("message_taken")(state) {
                            "The message has been recorded. I'll wrap up the call.".into()
                        } else if S::is_true("caller_blocked")(state) {
                            "The caller has been blocked. I'll end the call politely.".into()
                        } else {
                            "I'll wrap up the call now.".into()
                        }
                    })
                    .done()
                    .initial_phase("greeting")
                    // --- Watchers ---
                    // Numeric: urgency crossed above 0.8
                    .watch("urgency_level")
                    .crossed_above(0.8)
                    .then(move |_old, _new, state| {
                        async move {
                            state.set("urgency_high", true);
                        }
                    })
                    // Boolean: is_known_contact became true
                    .watch("is_known_contact")
                    .became_true()
                    .then(move |_old, _new, state| {
                        async move {
                            state.set("known_contact_verified", true);
                        }
                    })
                    // Value: caller_sentiment changed to "hostile"
                    .watch("caller_sentiment")
                    .changed_to(json!("hostile"))
                    .then(move |_old, _new, state| {
                        async move {
                            state.set("hostile_detected", true);
                        }
                    })
                    // --- Temporal patterns ---
                    // Sustained: caller impatient for 20 seconds
                    .when_sustained(
                        "caller_impatient",
                        |s: &State| {
                            let sentiment: String = s
                                .get("caller_sentiment")
                                .unwrap_or_else(|| "neutral".to_string());
                            sentiment == "impatient" || sentiment == "hostile"
                        },
                        Duration::from_secs(20),
                        move |_state: State, writer: Arc<dyn SessionWriter>| {
                            async move {
                                let _ = writer
                                    .send_client_content(
                                        vec![Content::user(
                                            "[System: The caller seems impatient. Speed up the screening \
                                             process. Consider offering to take a quick message or \
                                             transferring directly if appropriate.]",
                                        )],
                                        false,
                                    )
                                    .await;
                            }
                        },
                    )
                    // Turns: screening stalled for 8 turns
                    .when_turns(
                        "screening_stalled",
                        |s: &State| {
                            let phase: String = s.get("session:phase").unwrap_or_default();
                            phase == "identify_caller" || phase == "determine_purpose"
                        },
                        8,
                        move |_state: State, writer: Arc<dyn SessionWriter>| {
                            async move {
                                let _ = writer
                                    .send_client_content(
                                        vec![Content::user(
                                            "[System: Screening seems to be going slowly. Consider \
                                             offering to take a message if the caller is reluctant to \
                                             identify themselves or state their purpose.]",
                                        )],
                                        false,
                                    )
                                    .await;
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
    fn check_contact_list_known() {
        let result = execute_tool("check_contact_list", &json!({"name": "Jane Smith"}));
        assert_eq!(result["found"], true);
        assert_eq!(result["relationship"], "colleague — Marketing team");
        assert_eq!(result["priority"], "normal");
    }

    #[test]
    fn check_contact_list_manager() {
        let result = execute_tool("check_contact_list", &json!({"name": "Bob Johnson"}));
        assert_eq!(result["found"], true);
        assert_eq!(result["priority"], "high");
    }

    #[test]
    fn check_contact_list_unknown() {
        let result = execute_tool("check_contact_list", &json!({"name": "Random Person"}));
        assert_eq!(result["found"], false);
        assert_eq!(result["relationship"], "unknown");
    }

    #[test]
    fn check_contact_list_doctor() {
        let result = execute_tool("check_contact_list", &json!({"name": "Dr. Patel"}));
        assert_eq!(result["found"], true);
        assert_eq!(result["relationship"], "dentist — personal");
    }

    #[test]
    fn check_calendar_returns_meetings() {
        let result = execute_tool("check_calendar", &json!({}));
        let meetings = result["meetings_today"].as_array().unwrap();
        assert_eq!(meetings.len(), 2);
        assert_eq!(result["next_available"], "11:00 AM");
    }

    #[test]
    fn take_message_success() {
        let result = execute_tool(
            "take_message",
            &json!({"from_name": "John", "message": "Please call back"}),
        );
        assert_eq!(result["status"], "recorded");
        assert_eq!(result["will_notify"], true);
        assert!(result["message_id"].as_str().unwrap().starts_with("MSG-"));
    }

    #[test]
    fn transfer_call_success() {
        let result = execute_tool("transfer_call", &json!({"reason": "Known contact"}));
        assert_eq!(result["status"], "transferring");
    }

    #[test]
    fn block_caller_success() {
        let result = execute_tool("block_caller", &json!({"reason": "Spam caller"}));
        assert_eq!(result["status"], "blocked");
        assert_eq!(result["effective"], "immediately");
    }

    #[test]
    fn unknown_tool_returns_error() {
        let result = execute_tool("nonexistent", &json!({}));
        assert!(result["error"].as_str().unwrap().contains("Unknown"));
    }

    // -- Computed state --

    #[test]
    fn screen_decision_known_contact() {
        assert_eq!(compute_screen_decision(true, 0.3, "friendly"), "transfer");
    }

    #[test]
    fn screen_decision_high_urgency() {
        assert_eq!(compute_screen_decision(false, 0.9, "neutral"), "transfer");
    }

    #[test]
    fn screen_decision_hostile() {
        assert_eq!(compute_screen_decision(false, 0.3, "hostile"), "decline");
    }

    #[test]
    fn screen_decision_normal() {
        assert_eq!(
            compute_screen_decision(false, 0.5, "neutral"),
            "take_message"
        );
    }

    #[test]
    fn screen_decision_known_overrides_hostile() {
        assert_eq!(compute_screen_decision(true, 0.3, "hostile"), "transfer");
    }

    #[test]
    fn screen_decision_urgency_overrides_hostile() {
        assert_eq!(compute_screen_decision(false, 0.9, "hostile"), "transfer");
    }

    // -- App metadata --

    #[test]
    fn app_metadata() {
        let app = CallScreening;
        assert_eq!(app.name(), "call-screening");
        assert_eq!(app.category(), AppCategory::Showcase);
        assert!(app.features().contains(&"phase-machine".to_string()));
        assert!(app.features().contains(&"llm-extraction".to_string()));
        assert!(app.features().contains(&"watchers".to_string()));
        assert!(app.features().contains(&"computed-state".to_string()));
        assert!(app.features().contains(&"temporal-patterns".to_string()));
    }

    #[test]
    fn app_tips_not_empty() {
        let app = CallScreening;
        assert!(!app.tips().is_empty());
    }

    #[test]
    fn app_try_saying_not_empty() {
        let app = CallScreening;
        assert!(!app.try_saying().is_empty());
    }
}
