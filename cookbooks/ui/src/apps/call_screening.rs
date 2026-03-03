use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;
use rs_adk::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};
use rs_adk::state::StateKey;

use rs_genai::session::SessionEvent;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

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

const GREETING_INSTRUCTION: &str = "\
You are a professional call screener for Alex Rivera. \
Greet the caller warmly and ask who is calling and the purpose of their call. \
Use the check_contact_list tool if the caller provides their name. \
Be professional and polite, but do not reveal Alex's availability until you know \
who is calling and why.";

const IDENTIFY_CALLER_INSTRUCTION: &str = "\
Get the caller's full name and organization. \
Use the check_contact_list tool to verify if they are a known contact. \
If the caller refuses to identify themselves after being asked multiple times, \
offer to take a message instead. \
Be patient but firm — Alex requires knowing who is calling before taking calls.";

const DETERMINE_PURPOSE_INSTRUCTION: &str = "\
Ask the caller why they are calling and assess the urgency of the matter. \
Use the check_calendar tool to see if Alex has availability. \
Determine whether this call should be transferred, a message taken, or declined. \
Ask clarifying questions if the purpose is vague. \
Rate the urgency based on time-sensitivity and importance.";

const SCREEN_DECISION_INSTRUCTION: &str = "\
Based on the caller's identity and purpose, decide the appropriate action:\n\
1. If the caller is a known contact or the matter is urgent (urgency > 0.8), transfer the call.\n\
2. If the caller is unknown but the matter is legitimate, take a message.\n\
3. If the caller is hostile or the call appears to be spam, politely decline.\n\n\
Use the transfer_call, take_message, or block_caller tool as appropriate. \
Be decisive but professional.";

const TAKE_MESSAGE_INSTRUCTION: &str = "\
Take a detailed message for Alex. Collect:\n\
1. The caller's name (confirm spelling)\n\
2. A callback phone number\n\
3. The message they want to leave\n\n\
Use the take_message tool to record the message. \
Confirm the details back to the caller before saving. \
Let them know Alex will receive the message and an estimated callback time.";

const TRANSFER_INSTRUCTION: &str = "\
Inform the caller that you are transferring them to Alex now. \
Use the transfer_call tool to initiate the transfer. \
Let the caller know the transfer is in progress and to hold briefly. \
Be warm and reassuring.";

const FAREWELL_INSTRUCTION: &str = "\
Thank the caller for calling. \
If a message was taken, confirm it will be delivered. \
If the call was transferred, wish them a good conversation. \
If the call was declined, be polite but firm. \
Say goodbye professionally.";

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
    if known {
        "transfer"
    } else if urgency > 0.8 {
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
fn call_screening_tools() -> rs_genai::prelude::Tool {
    use rs_genai::prelude::{FunctionDeclaration, Tool};
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
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &Value) -> Value {
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
impl CookbookApp for CallScreening {
    fn name(&self) -> &str {
        "call-screening"
    }

    fn description(&self) -> &str {
        "AI call screening with caller identification, urgency detection, and smart routing"
    }

    fn category(&self) -> AppCategory {
        AppCategory::Showcase
    }

    fn features(&self) -> Vec<String> {
        vec![
            "phase-machine".into(),
            "llm-extraction".into(),
            "tool-calling".into(),
            "watchers".into(),
            "computed-state".into(),
            "temporal-patterns".into(),
            "state-keys".into(),
        ]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "Try calling as a known contact like 'Jane Smith from Marketing'".into(),
            "Call with an urgent matter to trigger auto-transfer".into(),
            "Be hostile to test the decline flow".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "Hi, this is Jane Smith from the marketing team. Is Alex available?".into(),
            "Hello, I'm calling about an urgent delivery issue.".into(),
            "I'd like to speak to whoever is in charge.".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        handle_session(tx, rx).await
    }
}

// ---------------------------------------------------------------------------
// handle_session — standalone async fn (same pattern as debt_collection)
// ---------------------------------------------------------------------------

async fn handle_session(
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
        .add_tool(call_screening_tools())
        .system_instruction(SYSTEM_INSTRUCTION);

    // 2. Create GeminiLlm for LLM extraction
    let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
        model: Some("gemini-2.5-flash".to_string()),
        ..Default::default()
    }));

    // 3. Clone tx for all callbacks
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

    // Phase on_enter clones
    let tx_enter_greeting = tx.clone();
    let tx_enter_identify = tx.clone();
    let tx_enter_purpose = tx.clone();
    let tx_enter_decision = tx.clone();
    let tx_enter_take_message = tx.clone();
    let tx_enter_transfer = tx.clone();
    let tx_enter_farewell = tx.clone();

    // Watcher clones
    let tx_watcher_urgency = tx.clone();
    let tx_watcher_known = tx.clone();
    let tx_watcher_hostile = tx.clone();

    // Temporal pattern clones
    let tx_sustained_impatient = tx.clone();
    let tx_turns_stalled = tx.clone();

    // 4. Build Live::builder() with full pipeline
    let handle = Live::builder()
        // --- Model-initiated greeting ---
        .greeting("A new call is coming in. Greet the caller professionally and ask who is calling.")
        // --- LLM extraction ---
        .extract_turns_windowed::<CallerState>(
            llm,
            "Extract from the call screening conversation: caller_name, \
             caller_organization, call_purpose, urgency_level (0.0-1.0), \
             caller_sentiment (friendly/neutral/impatient/hostile).",
            5,
        )
        // --- on_extracted: broadcast state to browser ---
        .on_extracted({
            let tx = tx.clone();
            move |name, value| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: name.clone(),
                        value: value.clone(),
                    });
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
        .on_extraction_error(|name, err| async move {
            warn!("Extraction error for {name}: {err}");
        })
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
        .before_tool_response(move |responses, state| {
            async move {
                responses
                    .into_iter()
                    .map(|r| {
                        match r.name.as_str() {
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
                        }
                        r
                    })
                    .collect()
            }
        })
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
        // --- Phase defaults (inherited by all phases) ---
        .phase_defaults(|d| {
            d.with_state(SCREEN_STATE_KEYS)
                .when(caller_is_hostile, CAUTION_WARNING)
                .prompt_on_enter(true)
        })
        // --- 7 Phases ---
        // Phase 1: Greeting
        .phase("greeting")
            .instruction(GREETING_INSTRUCTION)
            .tools(vec!["check_contact_list".into()])
            .transition("identify_caller", |_s: &State| {
                // Transition after the initial exchange (model has greeted and caller responds)
                true
            })
            .on_enter(move |_state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_greeting.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "none".into(),
                        to: "greeting".into(),
                        reason: "Incoming call — greeting caller".into(),
                    });
                }
            })
            .done()
        // Phase 2: Identify Caller
        .phase("identify_caller")
            .instruction(IDENTIFY_CALLER_INSTRUCTION)
            .tools(vec!["check_contact_list".into()])
            .transition("determine_purpose", |s: &State| {
                let name: Option<String> = s.get("caller_name");
                name.is_some()
            })
            .transition("take_message", |s: &State| {
                // If caller refuses to identify after several turns, offer to take a message
                let tc: u32 = s.session().get("turn_count").unwrap_or(0);
                let name: Option<String> = s.get("caller_name");
                tc >= 3 && name.is_none()
            })
            .on_enter(move |_state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_identify.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greeting".into(),
                        to: "identify_caller".into(),
                        reason: "Initial greeting done — identifying caller".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("identify_caller"),
                    });
                }
            })
            .enter_prompt("The caller has responded to the greeting. I'll now identify who they are.")
            .done()
        // Phase 3: Determine Purpose
        .phase("determine_purpose")
            .instruction(DETERMINE_PURPOSE_INSTRUCTION)
            .tools(vec!["check_calendar".into()])
            .transition("screen_decision", |s: &State| {
                let purpose: Option<String> = s.get("call_purpose");
                purpose.is_some()
            })
            .on_enter(move |_state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_purpose.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "identify_caller".into(),
                        to: "determine_purpose".into(),
                        reason: "Caller identified — determining call purpose".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("determine_purpose"),
                    });
                }
            })
            .enter_prompt("I've identified the caller. Now I'll determine why they are calling.")
            .done()
        // Phase 4: Screen Decision
        .phase("screen_decision")
            .instruction(SCREEN_DECISION_INSTRUCTION)
            .tools(vec![
                "transfer_call".into(),
                "take_message".into(),
                "block_caller".into(),
            ])
            .transition("transfer", |s: &State| {
                let known: bool = s.get("is_known_contact").unwrap_or(false);
                let urgency: f64 = s.get("urgency_level").unwrap_or(0.0);
                known || urgency > 0.8
            })
            .transition("farewell", S::is_true("caller_blocked"))
            .transition("take_message", |s: &State| {
                let known: bool = s.get("is_known_contact").unwrap_or(false);
                let urgency: f64 = s.get("urgency_level").unwrap_or(0.0);
                !known && urgency <= 0.8
            })
            .on_enter(move |_state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_decision.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "determine_purpose".into(),
                        to: "screen_decision".into(),
                        reason: "Purpose determined — making screening decision".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("screen_decision"),
                    });
                }
            })
            .enter_prompt("I know who is calling and why. I'll now decide the best course of action.")
            .done()
        // Phase 5: Take Message
        .phase("take_message")
            .instruction(TAKE_MESSAGE_INSTRUCTION)
            .tools(vec!["take_message".into()])
            .transition("farewell", S::is_true("message_taken"))
            .on_enter(move |_state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_take_message.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "previous".into(),
                        to: "take_message".into(),
                        reason: "Taking a message for Alex".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("take_message"),
                    });
                }
            })
            .enter_prompt("I'll take a message for Alex. Let me collect the details.")
            .done()
        // Phase 6: Transfer
        .phase("transfer")
            .instruction(TRANSFER_INSTRUCTION)
            .tools(vec!["transfer_call".into()])
            .transition("farewell", S::is_true("call_transferred"))
            .on_enter(move |_state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_transfer.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "previous".into(),
                        to: "transfer".into(),
                        reason: "Transferring call to Alex".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("transfer"),
                    });
                }
            })
            .enter_prompt("I'll transfer the caller to Alex now.")
            .done()
        // Phase 7: Farewell (terminal)
        .phase("farewell")
            .instruction(FAREWELL_INSTRUCTION)
            .terminal()
            .on_enter(move |state: State, _writer: Arc<dyn SessionWriter>| {
                let tx = tx_enter_farewell.clone();
                async move {
                    let transferred: bool = state.get("call_transferred").unwrap_or(false);
                    let message_taken: bool = state.get("message_taken").unwrap_or(false);
                    let blocked: bool = state.get("caller_blocked").unwrap_or(false);

                    let reason = if transferred {
                        "Call transferred — saying goodbye"
                    } else if message_taken {
                        "Message recorded — saying goodbye"
                    } else if blocked {
                        "Caller blocked — ending call"
                    } else {
                        "Call concluding"
                    };
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "previous".into(),
                        to: "farewell".into(),
                        reason: reason.into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("farewell"),
                    });
                }
            })
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
            .then({
                let tx = tx_watcher_urgency.clone();
                move |_old, new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "watcher:urgency_high".into(),
                            value: json!({
                                "triggered": true,
                                "value": new,
                                "action": "High urgency detected — consider immediate transfer"
                            }),
                        });
                    }
                }
            })
        // Boolean: is_known_contact became true
        .watch("is_known_contact")
            .became_true()
            .then({
                let tx = tx_watcher_known.clone();
                move |_old, _new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "watcher:known_contact".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Known contact identified — prioritize call"
                            }),
                        });
                    }
                }
            })
        // Value: caller_sentiment changed to "hostile"
        .watch("caller_sentiment")
            .changed_to(json!("hostile"))
            .then({
                let tx = tx_watcher_hostile.clone();
                move |_old, _new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::Violation {
                            rule: "hostile_caller".into(),
                            severity: "warning".into(),
                            detail: "Caller sentiment is hostile — exercise caution".into(),
                        });
                    }
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
            {
                let tx = tx_sustained_impatient.clone();
                move |_state: State, writer: Arc<dyn SessionWriter>| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::Violation {
                            rule: "sustained_impatience".into(),
                            severity: "warning".into(),
                            detail: "Caller has been impatient for over 20 seconds".into(),
                        });
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
                }
            },
        )
        // Turns: screening stalled for 4 turns
        .when_turns(
            "screening_stalled",
            |s: &State| {
                let phase: String = s.get("session:phase").unwrap_or_default();
                phase == "identify_caller" || phase == "determine_purpose"
            },
            4,
            {
                let tx = tx_turns_stalled.clone();
                move |_state: State, writer: Arc<dyn SessionWriter>| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "temporal:screening_stalled".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Screening has stalled for 4 turns"
                            }),
                        });
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
                }
            },
        )
        // --- Fast lane callbacks ---
        .on_audio(move |data| {
            let encoded = b64.encode(data);
            let _ = tx_audio.send(ServerMessage::Audio { data: encoded });
        })
        .on_input_transcript(move |text: &str, _is_final: bool| {
            let _ = tx_input.send(ServerMessage::InputTranscription {
                text: text.to_string(),
            });
        })
        .on_output_transcript(move |text: &str, _is_final: bool| {
            let _ = tx_output.send(ServerMessage::OutputTranscription {
                text: text.to_string(),
            });
        })
        .on_text(move |t: &str| {
            let _ = tx_text.send(ServerMessage::TextDelta {
                text: t.to_string(),
            });
        })
        .on_text_complete(move |t: &str| {
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
        .on_error(move |msg: String| {
            let tx = tx_error.clone();
            async move {
                let _ = tx.send(ServerMessage::Error { message: msg });
            }
        })
        .on_go_away(move |duration: Duration| {
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
        .on_disconnected(move |reason: Option<String>| {
            let _tx = tx_disconnected.clone();
            async move {
                info!("CallScreening session disconnected: {reason:?}");
            }
        })
        .connect(config)
        .await
        .map_err(|e| AppError::Connection(e.to_string()))?;

    // 5. Send Connected + AppMeta + initial state
    let _ = tx.send(ServerMessage::Connected);
    let app = CallScreening;
    send_app_meta(&tx, &app);
    info!("CallScreening session connected");

    // Periodic telemetry sender (auto-collected by SDK telemetry lane)
    let telem = handle.telemetry().clone();
    let telem_state = handle.state().clone();
    let tx_telem = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let mut stats = telem.snapshot();
            if let Some(obj) = stats.as_object_mut() {
                let phase: String = telem_state.get("session:phase").unwrap_or_default();
                let known: bool = telem_state.get("is_known_contact").unwrap_or(false);
                let urgency: f64 = telem_state.get("urgency_level").unwrap_or(0.0);
                let tc: u32 = telem_state.session().get("turn_count").unwrap_or(0);
                obj.insert("current_phase".into(), json!(phase));
                obj.insert("is_known_contact".into(), json!(known));
                obj.insert("urgency_level".into(), json!(urgency));
                obj.insert("turn_count".into(), json!(tc));
            }
            if tx_telem.send(ServerMessage::Telemetry { stats }).is_err() {
                break;
            }
        }
    });

    let _ = tx.send(ServerMessage::PhaseChange {
        from: "none".into(),
        to: "greeting".into(),
        reason: "Session started".into(),
    });
    let _ = tx.send(ServerMessage::StateUpdate {
        key: "phase".into(),
        value: json!("greeting"),
    });
    let _ = tx.send(ServerMessage::StateUpdate {
        key: "screen_recommendation".into(),
        value: json!("pending"),
    });

    // 6. Browser -> Gemini recv loop
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
                info!("CallScreening session stopping");
                let _ = handle.disconnect().await;
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(compute_screen_decision(false, 0.5, "neutral"), "take_message");
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
