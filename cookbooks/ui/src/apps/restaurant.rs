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
// StateKey constants
// ---------------------------------------------------------------------------

const GUEST_NAME: StateKey<String> = StateKey::new("guest_name");
const PARTY_SIZE: StateKey<u32> = StateKey::new("party_size");
const PREFERRED_DATE: StateKey<String> = StateKey::new("preferred_date");
const PREFERRED_TIME: StateKey<String> = StateKey::new("preferred_time");
const PHONE: StateKey<String> = StateKey::new("phone");
const DIETARY_NEEDS: StateKey<String> = StateKey::new("dietary_needs");
const SPECIAL_OCCASION: StateKey<String> = StateKey::new("special_occasion");
const RESERVATION_ID: StateKey<String> = StateKey::new("reservation_id");
const INTENT: StateKey<String> = StateKey::new("intent");

// Silence unused-constant warnings — these exist as documentation and for
// future typed-state access via `state.get_key(&KEY)`.
const _: () = {
    _ = GUEST_NAME;
    _ = PARTY_SIZE;
    _ = PREFERRED_DATE;
    _ = PREFERRED_TIME;
    _ = PHONE;
    _ = DIETARY_NEEDS;
    _ = SPECIAL_OCCASION;
    _ = RESERVATION_ID;
    _ = INTENT;
};

// ---------------------------------------------------------------------------
// Phase instructions
// ---------------------------------------------------------------------------

// Phase instructions -- lean directives for what to do in each phase.
// Contextual awareness ("where we are, what we know") is provided by
// the reservation_context() closure via with_context, so the model always
// has situational bearings without repeating state in the instructions.

const GREETING_INSTRUCTION: &str = "\
Warmly greet the caller and ask how you can help today. \
Determine their intent: new reservation, modify, cancel, or inquiry. \
If they want a new reservation, ask for party size and preferred date.";

const CHECK_AVAILABILITY_INSTRUCTION: &str = "\
Use check_availability to look up open time slots. \
Present available options clearly. If they ask about the menu, use check_menu. \
Once the guest picks a time, proceed to booking.";

const BOOKING_INSTRUCTION: &str = "\
Collect the guest's name, phone number, and any dietary needs or special occasions. \
Once you have name and phone, use make_reservation to finalize. \
Note any dietary restrictions or special occasions mentioned.";

const MODIFICATION_INSTRUCTION: &str = "\
Ask for their reservation ID or the name it is under. \
Use modify_reservation to apply changes. If changing date or time, \
use check_availability first. Confirm all changes before applying.";

const CANCELLATION_INSTRUCTION: &str = "\
Ask for the reservation ID or name. Confirm the reservation details, \
then use cancel_reservation. Express understanding and invite them back.";

const SPECIAL_REQUESTS_INSTRUCTION: &str = "\
Use add_special_request to record dietary restrictions, allergies, or occasion details. \
If they ask about menu options, use check_menu to show matching items. \
Common accommodations: vegetarian, vegan, gluten-free, nut allergy, \
birthday cake, anniversary flowers, high chair, wheelchair accessible.";

const CONFIRMATION_INSTRUCTION: &str = "\
Summarize the reservation details and ask if everything looks correct. \
If they confirm, thank them and prepare to say goodbye.";

const FAREWELL_INSTRUCTION: &str = "\
Thank the guest warmly. Mention their reservation ID if applicable. \
Let them know they can call back anytime. Wish them a wonderful day.";

const SYSTEM_INSTRUCTION: &str = "\
You are the AI receptionist for Bella Vista Italian Restaurant. \
You handle reservations with warmth and professionalism. \
The restaurant is open Tuesday through Sunday, 5:30 PM to 10:30 PM. \
Maximum capacity per time slot is 40 guests. \
Large parties of 9 or more require manager confirmation. \
Always be friendly, helpful, and knowledgeable about the restaurant.";

// ---------------------------------------------------------------------------
// LLM-powered extraction struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct ReservationState {
    /// The guest's name for the reservation
    guest_name: Option<String>,
    /// Number of guests in the party
    party_size: Option<u32>,
    /// Preferred date for the reservation (e.g., "2026-03-15", "this Saturday")
    preferred_date: Option<String>,
    /// Preferred time for the reservation (e.g., "7:00 PM", "seven o'clock")
    preferred_time: Option<String>,
    /// List of dietary restrictions (e.g., vegetarian, gluten-free, nut allergy)
    dietary_restrictions: Option<Vec<String>>,
    /// Special occasion type (birthday, anniversary, business dinner)
    special_occasion: Option<String>,
    /// The guest's intent (new_booking, modify, cancel, inquiry)
    intent: Option<String>,
    /// Existing reservation ID for modifications or cancellations
    reservation_id: Option<String>,
}

const EXTRACTION_PROMPT: &str = "\
Extract from the restaurant reservation conversation: \
guest_name, party_size (number), preferred_date, preferred_time, \
dietary_restrictions (list), special_occasion (birthday/anniversary/business dinner), \
intent (new_booking/modify/cancel/inquiry), reservation_id.";

// ---------------------------------------------------------------------------
// Geolocation context — natural-language summary of accumulated state
// ---------------------------------------------------------------------------

/// Builds a conversational-context summary so the model knows where it is,
/// what it knows, and what's still needed — without raw key-value dumps.
fn reservation_context(s: &State) -> String {
    let mut ctx = Vec::new();

    // Guest identity
    let name: Option<String> = s.get("guest_name");
    let phone: Option<String> = s.get("phone");
    match (&name, &phone) {
        (Some(n), Some(p)) => ctx.push(format!("Guest: {n} (phone: {p}).")),
        (Some(n), None) => ctx.push(format!("Guest: {n}. Phone not yet collected.")),
        _ => {}
    }

    // Party size
    if let Some(size) = s.get::<u32>("party_size") {
        let large = if size >= 9 {
            " (large party — requires manager confirmation)"
        } else {
            ""
        };
        ctx.push(format!("Party size: {size}{large}."));
    }

    // Date and time
    let date: Option<String> = s.get("preferred_date");
    let time: Option<String> = s.get("preferred_time");
    match (&date, &time) {
        (Some(d), Some(t)) => ctx.push(format!("Requested: {d} at {t}.")),
        (Some(d), None) => ctx.push(format!("Date: {d}. Time not yet chosen.")),
        _ => {}
    }

    // Dietary needs
    if let Some(dietary) = s.get::<String>("dietary_needs") {
        ctx.push(format!("Dietary needs: {dietary}."));
    }

    // Special occasion
    if let Some(occasion) = s.get::<String>("special_occasion") {
        ctx.push(format!("Special occasion: {occasion}."));
    }

    // Reservation ID (booking confirmed, modified, or pending cancellation)
    if let Some(res_id) = s.get::<String>("reservation_id") {
        ctx.push(format!("Reservation ID: {res_id}."));
    }

    // Intent
    let intent: String = s.get("intent").unwrap_or_default();
    if !intent.is_empty() {
        let label = match intent.as_str() {
            "new_booking" => "new reservation",
            "modify" => "modify existing reservation",
            "cancel" => "cancel reservation",
            "inquiry" => "general inquiry",
            other => other,
        };
        ctx.push(format!("Intent: {label}."));
    }

    if ctx.is_empty() {
        String::new()
    } else {
        ctx.join(" ")
    }
}

// ---------------------------------------------------------------------------
// Tool declarations
// ---------------------------------------------------------------------------

fn restaurant_tools() -> rs_genai::prelude::Tool {
    use rs_genai::prelude::{FunctionDeclaration, Tool};
    Tool::functions(vec![
        FunctionDeclaration {
            name: "check_availability".into(),
            description: "Check available reservation time slots for a given date and party size."
                .into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "date": {
                        "type": "string",
                        "description": "The date to check availability for (e.g., 2026-03-15)"
                    },
                    "party_size": {
                        "type": "integer",
                        "description": "Number of guests in the party"
                    },
                    "preferred_time": {
                        "type": "string",
                        "description": "Optional preferred time (e.g., 7:00 PM)"
                    }
                },
                "required": ["date", "party_size"]
            })),
            behavior: None,
        },
        FunctionDeclaration {
            name: "make_reservation".into(),
            description: "Create a new reservation with the provided details.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "guest_name": {
                        "type": "string",
                        "description": "Name for the reservation"
                    },
                    "phone": {
                        "type": "string",
                        "description": "Contact phone number"
                    },
                    "date": {
                        "type": "string",
                        "description": "Reservation date (e.g., 2026-03-15)"
                    },
                    "time": {
                        "type": "string",
                        "description": "Reservation time (e.g., 7:00 PM)"
                    },
                    "party_size": {
                        "type": "integer",
                        "description": "Number of guests"
                    }
                },
                "required": ["guest_name", "phone", "date", "time", "party_size"]
            })),
            behavior: None,
        },
        FunctionDeclaration {
            name: "modify_reservation".into(),
            description: "Modify an existing reservation.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "reservation_id": {
                        "type": "string",
                        "description": "The reservation ID to modify"
                    },
                    "changes": {
                        "type": "object",
                        "description": "Object containing the fields to change (e.g., date, time, party_size)"
                    }
                },
                "required": ["reservation_id"]
            })),
            behavior: None,
        },
        FunctionDeclaration {
            name: "cancel_reservation".into(),
            description: "Cancel an existing reservation.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "reservation_id": {
                        "type": "string",
                        "description": "The reservation ID to cancel"
                    },
                    "guest_name": {
                        "type": "string",
                        "description": "Name on the reservation for verification"
                    }
                },
                "required": ["reservation_id"]
            })),
            behavior: None,
        },
        FunctionDeclaration {
            name: "check_menu".into(),
            description: "Check menu items, optionally filtered by dietary restriction.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "dietary_restriction": {
                        "type": "string",
                        "description": "Filter menu by dietary restriction (e.g., vegetarian, vegan, gluten-free)"
                    }
                }
            })),
            behavior: None,
        },
        FunctionDeclaration {
            name: "add_special_request".into(),
            description: "Add a special request to an existing reservation.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "reservation_id": {
                        "type": "string",
                        "description": "The reservation ID to add the request to"
                    },
                    "request_type": {
                        "type": "string",
                        "description": "Type of special request (e.g., dietary, seating, occasion)"
                    },
                    "details": {
                        "type": "string",
                        "description": "Details of the special request"
                    }
                },
                "required": ["reservation_id", "request_type", "details"]
            })),
            behavior: None,
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &Value) -> Value {
    match name {
        "check_availability" => {
            let date = args
                .get("date")
                .and_then(|v| v.as_str())
                .unwrap_or("2026-03-15");
            let party_size = args.get("party_size").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
            let is_large = party_size >= 9;

            json!({
                "date": date,
                "party_size": party_size,
                "available_slots": [
                    {
                        "time": "5:30 PM",
                        "seats_remaining": 12,
                        "section": "main dining"
                    },
                    {
                        "time": "7:00 PM",
                        "seats_remaining": 8,
                        "section": "main dining"
                    },
                    {
                        "time": "8:30 PM",
                        "seats_remaining": 16,
                        "section": "patio"
                    }
                ],
                "large_party_notice": if is_large {
                    "Parties of 9 or more require manager confirmation. A manager will confirm within 24 hours."
                } else {
                    ""
                },
                "status": "available"
            })
        }
        "make_reservation" => {
            let guest_name = args
                .get("guest_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Guest");
            let date = args
                .get("date")
                .and_then(|v| v.as_str())
                .unwrap_or("2026-03-15");
            let time = args
                .get("time")
                .and_then(|v| v.as_str())
                .unwrap_or("7:00 PM");
            let party_size = args.get("party_size").and_then(|v| v.as_u64()).unwrap_or(2);
            let phone = args
                .get("phone")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let date_compact = date.replace('-', "");
            let id = format!("RES-{}-{:03}", date_compact, (party_size * 100 + 1) % 999);

            json!({
                "reservation_id": id,
                "status": "confirmed",
                "guest_name": guest_name,
                "date": date,
                "time": time,
                "party_size": party_size,
                "phone": phone,
                "confirmation_message": format!(
                    "Reservation confirmed for {} on {} at {} for {} guests.",
                    guest_name, date, time, party_size
                )
            })
        }
        "modify_reservation" => {
            let reservation_id = args
                .get("reservation_id")
                .and_then(|v| v.as_str())
                .unwrap_or("RES-UNKNOWN");
            let changes = args.get("changes").cloned().unwrap_or_else(|| json!({}));

            json!({
                "reservation_id": reservation_id,
                "status": "modified",
                "changes_applied": changes,
                "confirmation_message": format!(
                    "Reservation {} has been updated successfully.",
                    reservation_id
                )
            })
        }
        "cancel_reservation" => {
            let reservation_id = args
                .get("reservation_id")
                .and_then(|v| v.as_str())
                .unwrap_or("RES-UNKNOWN");
            let guest_name = args
                .get("guest_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Guest");

            json!({
                "reservation_id": reservation_id,
                "status": "cancelled",
                "guest_name": guest_name,
                "confirmation_message": format!(
                    "Reservation {} for {} has been cancelled. We hope to see you another time!",
                    reservation_id, guest_name
                )
            })
        }
        "check_menu" => {
            let restriction = args
                .get("dietary_restriction")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let items = match restriction.to_lowercase().as_str() {
                "vegetarian" => json!([
                    {"name": "Eggplant Parmigiana", "price": 18.00, "description": "Breaded eggplant with marinara and mozzarella"},
                    {"name": "Mushroom Risotto", "price": 22.00, "description": "Arborio rice with porcini and truffle oil"},
                    {"name": "Margherita Pizza", "price": 16.00, "description": "Fresh mozzarella, basil, and San Marzano tomatoes"},
                    {"name": "Caprese Salad", "price": 14.00, "description": "Heirloom tomatoes, burrata, and basil"}
                ]),
                "vegan" => json!([
                    {"name": "Pasta Primavera", "price": 19.00, "description": "Seasonal vegetables with garlic olive oil sauce"},
                    {"name": "Bruschetta al Pomodoro", "price": 12.00, "description": "Grilled bread with fresh tomatoes and basil"},
                    {"name": "Minestrone Soup", "price": 10.00, "description": "Hearty vegetable soup with cannellini beans"},
                    {"name": "Grilled Vegetable Platter", "price": 17.00, "description": "Zucchini, peppers, eggplant with balsamic glaze"}
                ]),
                "gluten-free" | "gluten free" => json!([
                    {"name": "Grilled Salmon", "price": 28.00, "description": "Atlantic salmon with lemon caper sauce"},
                    {"name": "Chicken Piccata (GF)", "price": 24.00, "description": "Pan-seared chicken with capers and white wine, served with risotto"},
                    {"name": "Gluten-Free Penne Bolognese", "price": 20.00, "description": "Rice pasta with traditional meat sauce"},
                    {"name": "Tiramisu (GF)", "price": 12.00, "description": "Gluten-free ladyfingers with mascarpone and espresso"}
                ]),
                _ => json!([
                    {"name": "Osso Buco", "price": 32.00, "description": "Braised veal shank with gremolata"},
                    {"name": "Lobster Ravioli", "price": 30.00, "description": "Handmade ravioli with lobster cream sauce"},
                    {"name": "Chicken Parmigiana", "price": 24.00, "description": "Breaded chicken with marinara and mozzarella"},
                    {"name": "Spaghetti Carbonara", "price": 20.00, "description": "Classic Roman pasta with pancetta and egg"},
                    {"name": "Tiramisu", "price": 12.00, "description": "Traditional Italian dessert with mascarpone and espresso"}
                ]),
            };

            json!({
                "dietary_filter": if restriction.is_empty() { "none" } else { restriction },
                "items": items,
                "note": "Please inform your server of any allergies. Our kitchen handles nuts, dairy, and gluten."
            })
        }
        "add_special_request" => {
            let reservation_id = args
                .get("reservation_id")
                .and_then(|v| v.as_str())
                .unwrap_or("RES-UNKNOWN");
            let request_type = args
                .get("request_type")
                .and_then(|v| v.as_str())
                .unwrap_or("general");
            let details = args.get("details").and_then(|v| v.as_str()).unwrap_or("");

            json!({
                "reservation_id": reservation_id,
                "request_type": request_type,
                "details": details,
                "status": "added",
                "confirmation_message": format!(
                    "Special request ({}) has been added to reservation {}: {}",
                    request_type, reservation_id, details
                )
            })
        }
        _ => json!({"error": format!("Unknown tool: {name}")}),
    }
}

// ---------------------------------------------------------------------------
// Restaurant app
// ---------------------------------------------------------------------------

/// Restaurant receptionist voice AI with reservation management, dietary accommodations,
/// and multi-phase conversation flow.
pub struct Restaurant;

#[async_trait]
impl CookbookApp for Restaurant {
    fn name(&self) -> &str {
        "restaurant"
    }

    fn description(&self) -> &str {
        "Restaurant reservation assistant with availability checking, booking management, and dietary accommodations"
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
            "Try making a reservation for a large group (9+) to see manager confirmation flow"
                .into(),
            "Ask about vegetarian or gluten-free menu options".into(),
            "Mention it's a birthday or anniversary for special handling".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "I'd like to make a reservation for 4 people this Saturday".into(),
            "I need to modify reservation RES-20260315-001".into(),
            "Do you have anything available for a large group of 12?".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        handle_session(tx, rx).await
    }
}

// ---------------------------------------------------------------------------
// handle_session
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
        .add_tool(restaurant_tools())
        .system_instruction(SYSTEM_INSTRUCTION);

    // 2. Create GeminiLlm for LLM extraction
    let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
        model: Some("gemini-2.5-flash".to_string()),
        ..Default::default()
    }));

    // 3. Clone tx for ALL callbacks
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
    let tx_enter_check = tx.clone();
    let tx_enter_booking = tx.clone();
    let tx_enter_modification = tx.clone();
    let tx_enter_cancellation = tx.clone();
    let tx_enter_special = tx.clone();
    let tx_enter_confirmation = tx.clone();
    let tx_enter_farewell = tx.clone();

    // Watcher clones
    let tx_watcher_large_party = tx.clone();
    let tx_watcher_readiness = tx.clone();

    // Temporal pattern clones
    let tx_sustained_undecided = tx.clone();
    let tx_turns_stalled = tx.clone();

    // 4. Build Live::builder() with full pipeline
    let handle = Live::builder()
        // --- Model-initiated greeting ---
        .greeting("Welcome the caller to Bella Vista Italian Restaurant. Ask how you can help them today — whether they'd like to make a new reservation, modify an existing one, cancel, or have a question.")
        // --- LLM extraction ---
        .extract_turns_triggered::<ReservationState>(llm, EXTRACTION_PROMPT, 5, ExtractionTrigger::Interval(2))
        // --- on_extracted: broadcast state to browser (concurrent — fire-and-forget) ---
        .on_extracted_concurrent({
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
        // --- on_extraction_error (concurrent — fire-and-forget) ---
        .on_extraction_error_concurrent({
            let tx = tx.clone();
            move |name, error| {
                let tx = tx.clone();
                async move {
                    warn!("Extraction error for {name}: {error}");
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: format!("extraction_error.{name}"),
                        value: json!({"error": error}),
                    });
                }
            }
        })
        // --- Computed state ---
        // is_large_party: party_size >= 9
        .computed("is_large_party", &["party_size"], |state| {
            let size: u32 = state.get("party_size").unwrap_or(0);
            Some(json!(size >= 9))
        })
        // booking_readiness: fraction of required fields present
        .computed(
            "booking_readiness",
            &["party_size", "preferred_date", "guest_name"],
            |state| {
                let has = [
                    state.get::<u32>("party_size").is_some(),
                    state.get::<String>("preferred_date").is_some(),
                    state.get::<String>("guest_name").is_some(),
                ];
                let score = has.iter().filter(|&&b| b).count() as f64 / 3.0;
                Some(json!(score))
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
                        scheduling: None,
                    });
                }
                Some(responses)
            }
        })
        // --- Phase defaults (inherited by all phases) ---
        .phase_defaults(|d| d.navigation().with_context(reservation_context))
        // --- 8 Phases ---
        // Phase 1: Greeting
        .phase("greeting")
            .instruction(GREETING_INSTRUCTION)
            .prompt_on_enter(true)
            .needs(&["guest_name", "party_size", "intent"])
            .transition_with("check_availability", |s| {
                S::eq("intent", "new_booking")(s)
                    && s.get::<u32>("party_size").is_some()
            }, "when guest name and party size are provided")
            .transition_with("modification", S::eq("intent", "modify"), "when intent is modify")
            .transition_with("cancellation", S::eq("intent", "cancel"), "when intent is cancel")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_greeting.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "none".into(),
                        to: "greeting".into(),
                        reason: "Session started — welcoming guest".into(),
                    });
                }
            })
            .done()
        // Phase 2: Check Availability
        .phase("check_availability")
            .instruction(CHECK_AVAILABILITY_INSTRUCTION)
            .needs(&["preferred_date", "preferred_time"])
            .transition_with("booking", |s| {
                s.get::<String>("preferred_time").is_some()
            }, "when availability is confirmed")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_check.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greeting".into(),
                        to: "check_availability".into(),
                        reason: "Guest wants a reservation — checking availability".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("check_availability"),
                    });
                }
            })
            .enter_prompt_fn(|s, _| {
                let size: u32 = s.get("party_size").unwrap_or(0);
                let date: String = s.get("preferred_date").unwrap_or_else(|| "their requested date".into());
                if size > 0 {
                    format!("A party of {size} wants to dine on {date}. Check available time slots.")
                } else {
                    format!("The guest wants a reservation on {date}. Check availability.")
                }
            })
            .done()
        // Phase 3: Booking
        .phase("booking")
            .instruction(BOOKING_INSTRUCTION)
            .needs(&["phone"])
            .transition_with("special_requests", |s| {
                s.get::<String>("reservation_id").is_some()
                    && (s.get::<String>("dietary_needs").is_some()
                        || s.get::<String>("special_occasion").is_some())
            }, "when booking details are confirmed")
            .transition_with("confirmation", |s| {
                s.get::<String>("reservation_id").is_some()
            }, "when booking is complete")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_booking.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "check_availability".into(),
                        to: "booking".into(),
                        reason: "Time slot selected — collecting booking details".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("booking"),
                    });
                }
            })
            .enter_prompt_fn(|s, _| {
                let time: String = s.get("preferred_time").unwrap_or_else(|| "their chosen time".into());
                let date: String = s.get("preferred_date").unwrap_or_else(|| "the requested date".into());
                let size: u32 = s.get("party_size").unwrap_or(0);
                if size > 0 {
                    format!("Party of {size} selected {time} on {date}. Collect their name and phone to finalize.")
                } else {
                    format!("Guest selected {time} on {date}. Collect their name and phone to finalize.")
                }
            })
            .done()
        // Phase 4: Modification
        .phase("modification")
            .instruction(MODIFICATION_INSTRUCTION)
            .needs(&["reservation_id"])
            .transition_with("confirmation", |s| {
                s.get::<String>("reservation_id").is_some()
            }, "when modification is complete")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_modification.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greeting".into(),
                        to: "modification".into(),
                        reason: "Guest wants to modify a reservation".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("modification"),
                    });
                }
            })
            .enter_prompt_fn(|s, _| {
                if let Some(res_id) = s.get::<String>("reservation_id") {
                    format!("The guest wants to modify reservation {res_id}. Ask what they'd like to change.")
                } else {
                    "The guest wants to modify a reservation. Ask for the reservation ID or name.".into()
                }
            })
            .done()
        // Phase 5: Cancellation
        .phase("cancellation")
            .instruction(CANCELLATION_INSTRUCTION)
            .needs(&["reservation_id"])
            .transition_with("farewell", |s| {
                // Move to farewell after cancellation is processed
                s.get::<String>("reservation_id").is_some()
            }, "when cancellation is processed")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_cancellation.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greeting".into(),
                        to: "cancellation".into(),
                        reason: "Guest wants to cancel a reservation".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("cancellation"),
                    });
                }
            })
            .enter_prompt_fn(|s, _| {
                if let Some(res_id) = s.get::<String>("reservation_id") {
                    format!("The guest wants to cancel reservation {res_id}. Confirm the details before proceeding.")
                } else {
                    "The guest wants to cancel a reservation. Ask for the reservation ID or name.".into()
                }
            })
            .done()
        // Phase 6: Special Requests
        .phase("special_requests")
            .instruction(SPECIAL_REQUESTS_INSTRUCTION)
            .needs(&["dietary_needs", "special_occasion"])
            .transition_with("confirmation", |s| {
                // Proceed once special request has been noted (reservation_id exists)
                // plus a safety-net turn-count fallback
                let has_res = s.get::<String>("reservation_id").is_some();
                let tc: u32 = s.session().get("turn_count").unwrap_or(0);
                has_res || tc >= 12
            }, "when special requests are noted")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_special.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "booking".into(),
                        to: "special_requests".into(),
                        reason: "Guest has dietary needs or special occasion — handling requests".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("special_requests"),
                    });
                }
            })
            .enter_prompt_fn(|s, _| {
                let mut parts = Vec::new();
                if let Some(dietary) = s.get::<String>("dietary_needs") {
                    parts.push(format!("dietary needs ({dietary})"));
                }
                if let Some(occasion) = s.get::<String>("special_occasion") {
                    parts.push(format!("a {occasion}"));
                }
                let res_id: String = s.get("reservation_id").unwrap_or_else(|| "their reservation".into());
                if parts.is_empty() {
                    format!("Record any special requests for reservation {res_id}.")
                } else {
                    format!("The guest mentioned {}. Record these for reservation {res_id}.", parts.join(" and "))
                }
            })
            .done()
        // Phase 7: Confirmation
        .phase("confirmation")
            .instruction(CONFIRMATION_INSTRUCTION)
            .transition_with("farewell", |s| {
                // Primary: reservation_id exists (the booking is confirmed).
                // Safety-net: cumulative turn_count >= 12 to avoid stuck sessions.
                let has_res = s.get::<String>("reservation_id").is_some();
                let tc: u32 = s.session().get("turn_count").unwrap_or(0);
                has_res || tc >= 12
            }, "when reservation is confirmed")
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_confirmation.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "previous".into(),
                        to: "confirmation".into(),
                        reason: "Summarizing reservation details".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("confirmation"),
                    });
                }
            })
            .enter_prompt_fn(|s, _| {
                let name: String = s.get("guest_name").unwrap_or_else(|| "the guest".into());
                let res_id: String = s.get("reservation_id").unwrap_or_else(|| "pending".into());
                format!("Summarize the reservation for {name} (ID: {res_id}) and ask them to confirm.")
            })
            .done()
        // Phase 8: Farewell
        .phase("farewell")
            .instruction(FAREWELL_INSTRUCTION)
            .terminal()
            .on_enter(move |state, _writer| {
                let tx = tx_enter_farewell.clone();
                async move {
                    let res_id: String = state
                        .get("reservation_id")
                        .unwrap_or_else(|| "none".to_string());
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "previous".into(),
                        to: "farewell".into(),
                        reason: "Call concluding — thanking guest".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("farewell"),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "final_reservation_id".into(),
                        value: json!(res_id),
                    });
                }
            })
            .enter_prompt_fn(|state, _tw| {
                let res_id: String = state
                    .get("reservation_id")
                    .unwrap_or_else(|| "none".to_string());
                if res_id != "none" {
                    format!(
                        "Everything is set. Reservation ID is {}. I'll thank the guest and wrap up.",
                        res_id
                    )
                } else {
                    "I'll thank the guest for calling and wish them a wonderful day.".into()
                }
            })
            .done()
        .initial_phase("greeting")
        // --- Watchers ---
        // Large party: party_size crossed above 8
        .watch("party_size")
            .crossed_above(8.0)
            .then({
                let tx = tx_watcher_large_party.clone();
                move |_old, new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "watcher:large_party".into(),
                            value: json!({
                                "triggered": true,
                                "party_size": new,
                                "action": "Large party notice: 9+ guests require manager confirmation"
                            }),
                        });
                    }
                }
            })
        // Booking readiness crossed above 0.9 — all info collected
        .watch("derived:booking_readiness")
            .crossed_above(0.9)
            .then({
                let tx = tx_watcher_readiness.clone();
                move |_old, new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "watcher:booking_ready".into(),
                            value: json!({
                                "triggered": true,
                                "readiness": new,
                                "action": "All required booking information collected"
                            }),
                        });
                    }
                }
            })
        // --- Temporal patterns ---
        // Sustained: guest undecided for 25 seconds — suggest popular time
        .when_sustained(
            "guest_undecided",
            |s| {
                let phase: String = s.get("session:phase").unwrap_or_default();
                let has_time = s.get::<String>("preferred_time").is_some();
                phase == "check_availability" && !has_time
            },
            Duration::from_secs(25),
            {
                let tx = tx_sustained_undecided.clone();
                move |_state, writer| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "temporal:guest_undecided".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Suggesting popular 7:00 PM time slot"
                            }),
                        });
                        let _ = writer
                            .send_client_content(
                                vec![Content::user(
                                    "[System: The guest seems undecided about a time. \
                                     Our most popular seating is 7:00 PM — gently suggest it \
                                     as a great option if they haven't decided yet.]",
                                )],
                                false,
                            )
                            .await;
                    }
                }
            },
        )
        // Turns: booking stalled for 4 turns — re-ask for name
        .when_turns(
            "booking_stalled",
            |s| {
                let phase: String = s.get("session:phase").unwrap_or_default();
                let has_name = s.get::<String>("guest_name").is_some();
                phase == "booking" && !has_name
            },
            4,
            {
                let tx = tx_turns_stalled.clone();
                move |_state, writer| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "temporal:booking_stalled".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Gently re-asking for guest name"
                            }),
                        });
                        let _ = writer
                            .send_client_content(
                                vec![Content::user(
                                    "[System: We still need the guest's name to complete \
                                     the reservation. Please gently ask for their name again.]",
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
                info!("Restaurant session disconnected: {reason:?}");
            }
        })
        .connect(config)
        .await
        .map_err(|e| AppError::Connection(e.to_string()))?;

    // 5. Send Connected + AppMeta
    let _ = tx.send(ServerMessage::Connected);
    let app = Restaurant;
    send_app_meta(&tx, &app);
    info!("Restaurant session connected");

    // Periodic telemetry sender
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
                let party_size: u32 = telem_state.get("party_size").unwrap_or(0);
                let readiness: f64 = telem_state.get("derived:booking_readiness").unwrap_or(0.0);
                let tc: u32 = telem_state.session().get("turn_count").unwrap_or(0);
                obj.insert("current_phase".into(), json!(phase));
                obj.insert("party_size".into(), json!(party_size));
                obj.insert("booking_readiness".into(), json!(readiness));
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
                info!("Restaurant session stopping");
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
    fn check_availability_returns_slots() {
        let result = execute_tool(
            "check_availability",
            &json!({"date": "2026-03-15", "party_size": 4}),
        );
        let slots = result["available_slots"].as_array().unwrap();
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0]["time"], "5:30 PM");
        assert_eq!(slots[1]["time"], "7:00 PM");
        assert_eq!(slots[2]["time"], "8:30 PM");
        assert_eq!(result["status"], "available");
    }

    #[test]
    fn check_availability_large_party_notice() {
        let result = execute_tool(
            "check_availability",
            &json!({"date": "2026-03-15", "party_size": 12}),
        );
        let notice = result["large_party_notice"].as_str().unwrap();
        assert!(notice.contains("manager confirmation"));
    }

    #[test]
    fn make_reservation_returns_id() {
        let result = execute_tool(
            "make_reservation",
            &json!({
                "guest_name": "Maria Rossi",
                "phone": "555-123-4567",
                "date": "2026-03-15",
                "time": "7:00 PM",
                "party_size": 4
            }),
        );
        assert_eq!(result["status"], "confirmed");
        let res_id = result["reservation_id"].as_str().unwrap();
        assert!(res_id.starts_with("RES-"));
        assert_eq!(result["guest_name"], "Maria Rossi");
    }

    #[test]
    fn modify_reservation_success() {
        let result = execute_tool(
            "modify_reservation",
            &json!({
                "reservation_id": "RES-20260315-001",
                "changes": {"time": "8:00 PM"}
            }),
        );
        assert_eq!(result["status"], "modified");
        assert_eq!(result["reservation_id"], "RES-20260315-001");
    }

    #[test]
    fn cancel_reservation_success() {
        let result = execute_tool(
            "cancel_reservation",
            &json!({
                "reservation_id": "RES-20260315-001",
                "guest_name": "Maria Rossi"
            }),
        );
        assert_eq!(result["status"], "cancelled");
        assert!(result["confirmation_message"]
            .as_str()
            .unwrap()
            .contains("cancelled"));
    }

    #[test]
    fn check_menu_vegetarian() {
        let result = execute_tool("check_menu", &json!({"dietary_restriction": "vegetarian"}));
        let items = result["items"].as_array().unwrap();
        assert!(!items.is_empty());
        let names: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).collect();
        assert!(names.contains(&"Mushroom Risotto"));
        assert!(names.contains(&"Eggplant Parmigiana"));
    }

    #[test]
    fn check_menu_vegan() {
        let result = execute_tool("check_menu", &json!({"dietary_restriction": "vegan"}));
        let items = result["items"].as_array().unwrap();
        assert!(!items.is_empty());
        let names: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).collect();
        assert!(names.contains(&"Pasta Primavera"));
    }

    #[test]
    fn check_menu_gluten_free() {
        let result = execute_tool("check_menu", &json!({"dietary_restriction": "gluten-free"}));
        let items = result["items"].as_array().unwrap();
        assert!(!items.is_empty());
        let names: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).collect();
        assert!(names.contains(&"Grilled Salmon"));
    }

    #[test]
    fn check_menu_full() {
        let result = execute_tool("check_menu", &json!({}));
        let items = result["items"].as_array().unwrap();
        assert!(items.len() >= 4);
    }

    #[test]
    fn add_special_request_success() {
        let result = execute_tool(
            "add_special_request",
            &json!({
                "reservation_id": "RES-20260315-001",
                "request_type": "occasion",
                "details": "Birthday celebration — please prepare a cake"
            }),
        );
        assert_eq!(result["status"], "added");
        assert!(result["confirmation_message"]
            .as_str()
            .unwrap()
            .contains("Birthday"));
    }

    #[test]
    fn unknown_tool_returns_error() {
        let result = execute_tool("nonexistent", &json!({}));
        assert!(result["error"].as_str().unwrap().contains("Unknown"));
    }

    // -- App metadata --

    #[test]
    fn app_metadata() {
        let app = Restaurant;
        assert_eq!(app.name(), "restaurant");
        assert_eq!(app.category(), AppCategory::Showcase);
        assert!(app.features().contains(&"phase-machine".to_string()));
        assert!(app.features().contains(&"llm-extraction".to_string()));
        assert!(app.features().contains(&"tool-calling".to_string()));
        assert!(app.features().contains(&"watchers".to_string()));
        assert!(app.features().contains(&"computed-state".to_string()));
        assert!(app.features().contains(&"temporal-patterns".to_string()));
        assert!(app.features().contains(&"state-keys".to_string()));
    }

    #[test]
    fn app_tips_not_empty() {
        let app = Restaurant;
        assert!(!app.tips().is_empty());
        assert!(!app.try_saying().is_empty());
    }

    #[test]
    fn app_description() {
        let app = Restaurant;
        assert!(app.description().contains("reservation"));
    }
}
