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
// Typed state keys
// ---------------------------------------------------------------------------

#[allow(dead_code)]
const PATIENT_NAME: StateKey<String> = StateKey::new("patient_name");
#[allow(dead_code)]
const PATIENT_ID: StateKey<String> = StateKey::new("patient_id");
#[allow(dead_code)]
const SYMPTOMS: StateKey<String> = StateKey::new("symptoms");
#[allow(dead_code)]
const DEPARTMENT: StateKey<String> = StateKey::new("department");
#[allow(dead_code)]
const DOCTOR_NAME: StateKey<String> = StateKey::new("doctor_name");
#[allow(dead_code)]
const APPOINTMENT_DATE: StateKey<String> = StateKey::new("appointment_date");
#[allow(dead_code)]
const APPOINTMENT_TIME: StateKey<String> = StateKey::new("appointment_time");
#[allow(dead_code)]
const IS_NEW_PATIENT: StateKey<bool> = StateKey::new("is_new_patient");
#[allow(dead_code)]
const INSURANCE_PROVIDER: StateKey<String> = StateKey::new("insurance_provider");
#[allow(dead_code)]
const CLINICAL_URGENCY: StateKey<f64> = StateKey::new("clinical_urgency");
#[allow(dead_code)]
const APPOINTMENT_BOOKED: StateKey<bool> = StateKey::new("appointment_booked");

// ---------------------------------------------------------------------------
// Phase instructions
// ---------------------------------------------------------------------------

const SYSTEM_INSTRUCTION: &str = "\
You are the AI receptionist for Clearview Medical Center, a multi-specialty clinic. \
You help patients book, reschedule, and cancel appointments across departments. \
Be empathetic and professional. NEVER provide medical diagnoses. \
If symptoms suggest an emergency (chest pain, difficulty breathing, severe bleeding), \
advise calling 911 immediately.";

const GREETING_INSTRUCTION: &str = "\
Welcome the patient to Clearview Medical Center. Ask how you can help them today. \
Determine their intent: are they booking a new appointment, rescheduling an existing one, \
or have another inquiry? If they mention a name, try looking them up. \
Be warm and professional.";

const SYMPTOM_TRIAGE_INSTRUCTION: &str = "\
Ask the patient about their symptoms empathetically. Listen carefully and ask \
follow-up questions to understand the nature, duration, and severity of their symptoms. \
Do NOT diagnose — you are not a doctor. Assess urgency based on what they describe. \
If they mention chest pain, difficulty breathing, or severe bleeding, \
immediately advise calling 911. \
Once you have a clear picture of the symptoms, suggest an appropriate department.";

const DEPARTMENT_SELECTION_INSTRUCTION: &str = "\
Based on the patient's symptoms, suggest the most appropriate department. \
Use the list_departments tool to show available departments. \
Explain why you're recommending a particular department. \
Confirm the department selection with the patient before proceeding.";

const DOCTOR_SELECTION_INSTRUCTION: &str = "\
Show the available doctors in the selected department using the list_doctors tool. \
Share each doctor's specialty and rating. Let the patient choose, or recommend \
the best fit based on their symptoms. Use check_doctor_availability to show \
available time slots once a doctor is selected.";

const APPOINTMENT_BOOKING_INSTRUCTION: &str = "\
Book the appointment using the book_appointment tool. \
If the patient is not in our system, let them know you'll need to register them first. \
Confirm all details before booking: doctor name, date, and time. \
Use the lookup_patient tool to check if they are an existing patient.";

const RESCHEDULING_INSTRUCTION: &str = "\
Help the patient reschedule or cancel their existing appointment. \
Look up their current appointment details. Offer new available slots using \
check_doctor_availability. Use reschedule_appointment or cancel_appointment as needed. \
Be understanding — patients reschedule for many reasons.";

const PATIENT_REGISTRATION_INSTRUCTION: &str = "\
The patient is new to Clearview Medical Center. Collect the following information:\n\
1. Full name\n\
2. Date of birth\n\
3. Phone number\n\
4. Insurance provider (optional)\n\n\
Use the register_patient tool to create their profile. \
Be welcoming — this is their first interaction with us.";

const CONFIRMATION_INSTRUCTION: &str = "\
Confirm the appointment details with the patient:\n\
1. Doctor name and specialty\n\
2. Appointment date and time\n\
3. Department\n\
4. Appointment ID\n\n\
Make sure they have all the information they need.";

const FAREWELL_INSTRUCTION: &str = "\
Thank the patient for choosing Clearview Medical Center. \
Remind them to bring their insurance card and photo ID. \
If they are a new patient, emphasize the importance of arriving 15 minutes early \
to complete any remaining paperwork. Wish them well.";

// ---------------------------------------------------------------------------
// State keys for phase instruction modifiers
// ---------------------------------------------------------------------------

const CLINIC_STATE_KEYS: &[&str] = &[
    "symptoms",
    "department",
    "doctor_name",
    "clinical_urgency",
    "is_new_patient",
    "derived:suggested_department",
];

const EMERGENCY_WARNING: &str = "\
URGENT: The patient's symptoms suggest a potential medical emergency. \
Immediately advise them to call 911 or go to the nearest emergency room. \
Do NOT attempt to schedule an appointment for emergency symptoms.";

fn urgency_is_critical(s: &State) -> bool {
    let urgency: f64 = s.get("clinical_urgency").unwrap_or(0.0);
    urgency > 0.9
}

// ---------------------------------------------------------------------------
// LLM-powered extraction struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct ClinicState {
    /// List of symptoms the patient describes
    symptoms: Option<Vec<String>>,
    /// Which department they prefer or need
    preferred_department: Option<String>,
    /// Which doctor they prefer
    preferred_doctor: Option<String>,
    /// The patient's name
    patient_name: Option<String>,
    /// The patient's ID if known
    patient_id: Option<String>,
    /// 0.0 = routine, 1.0 = emergency. >0.9 if chest pain, breathing difficulty, severe bleeding
    urgency: Option<f64>,
    /// new_appointment, reschedule, cancel, inquiry
    intent: Option<String>,
    /// Whether insurance was mentioned
    insurance_mentioned: Option<bool>,
}

// ---------------------------------------------------------------------------
// Computed state: symptom-to-department mapping
// ---------------------------------------------------------------------------

fn suggest_department(symptoms: &str) -> &'static str {
    let lower = symptoms.to_lowercase();
    if lower.contains("chest")
        || lower.contains("heart")
        || lower.contains("blood pressure")
        || lower.contains("palpitation")
    {
        "Cardiology"
    } else if lower.contains("skin")
        || lower.contains("rash")
        || lower.contains("acne")
        || lower.contains("mole")
    {
        "Dermatology"
    } else if lower.contains("bone")
        || lower.contains("joint")
        || lower.contains("back pain")
        || lower.contains("fracture")
        || lower.contains("sprain")
    {
        "Orthopedics"
    } else if lower.contains("child")
        || lower.contains("kid")
        || lower.contains("infant")
        || lower.contains("toddler")
    {
        "Pediatrics"
    } else if lower.contains("ear")
        || lower.contains("nose")
        || lower.contains("throat")
        || lower.contains("sinus")
        || lower.contains("hearing")
    {
        "ENT"
    } else {
        "General Medicine"
    }
}

// ---------------------------------------------------------------------------
// Tool declarations
// ---------------------------------------------------------------------------

fn clinic_tools() -> rs_genai::prelude::Tool {
    use rs_genai::prelude::{FunctionDeclaration, Tool};
    Tool::functions(vec![
        FunctionDeclaration {
            name: "list_departments".into(),
            description: "List all available departments at Clearview Medical Center with doctor counts and wait times.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {},
                "required": []
            })),
        },
        FunctionDeclaration {
            name: "list_doctors".into(),
            description: "List available doctors in a specific department.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "department": {
                        "type": "string",
                        "description": "The department name to list doctors for"
                    }
                },
                "required": ["department"]
            })),
        },
        FunctionDeclaration {
            name: "check_doctor_availability".into(),
            description: "Check a doctor's available appointment slots.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "doctor_name": {
                        "type": "string",
                        "description": "The doctor's name"
                    },
                    "date": {
                        "type": "string",
                        "description": "Optional preferred date in YYYY-MM-DD format"
                    }
                },
                "required": ["doctor_name"]
            })),
        },
        FunctionDeclaration {
            name: "book_appointment".into(),
            description: "Book an appointment for a patient with a specific doctor.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "patient_id": {
                        "type": "string",
                        "description": "The patient's ID"
                    },
                    "doctor_name": {
                        "type": "string",
                        "description": "The doctor's name"
                    },
                    "date": {
                        "type": "string",
                        "description": "Appointment date in YYYY-MM-DD format"
                    },
                    "time": {
                        "type": "string",
                        "description": "Appointment time (e.g., 9:00 AM)"
                    }
                },
                "required": ["patient_id", "doctor_name", "date", "time"]
            })),
        },
        FunctionDeclaration {
            name: "reschedule_appointment".into(),
            description: "Reschedule an existing appointment to a new date and time.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "appointment_id": {
                        "type": "string",
                        "description": "The appointment ID to reschedule"
                    },
                    "new_date": {
                        "type": "string",
                        "description": "New date in YYYY-MM-DD format"
                    },
                    "new_time": {
                        "type": "string",
                        "description": "New time (e.g., 2:00 PM)"
                    }
                },
                "required": ["appointment_id", "new_date", "new_time"]
            })),
        },
        FunctionDeclaration {
            name: "cancel_appointment".into(),
            description: "Cancel an existing appointment.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "appointment_id": {
                        "type": "string",
                        "description": "The appointment ID to cancel"
                    }
                },
                "required": ["appointment_id"]
            })),
        },
        FunctionDeclaration {
            name: "register_patient".into(),
            description: "Register a new patient at Clearview Medical Center.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Patient's full name"
                    },
                    "date_of_birth": {
                        "type": "string",
                        "description": "Date of birth in YYYY-MM-DD format"
                    },
                    "phone": {
                        "type": "string",
                        "description": "Phone number"
                    },
                    "insurance_provider": {
                        "type": "string",
                        "description": "Insurance provider name (optional)"
                    }
                },
                "required": ["name", "date_of_birth", "phone"]
            })),
        },
        FunctionDeclaration {
            name: "lookup_patient".into(),
            description: "Look up a patient by name or patient ID.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Patient's name to search for"
                    },
                    "patient_id": {
                        "type": "string",
                        "description": "Patient's ID to look up"
                    }
                },
                "required": []
            })),
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &Value) -> Value {
    match name {
        "list_departments" => json!({
            "departments": [
                { "name": "General Medicine", "doctors": 3, "wait_days": 2 },
                { "name": "Cardiology", "doctors": 2, "wait_days": 5 },
                { "name": "Dermatology", "doctors": 2, "wait_days": 7 },
                { "name": "Orthopedics", "doctors": 2, "wait_days": 4 },
                { "name": "Pediatrics", "doctors": 2, "wait_days": 3 },
                { "name": "ENT", "doctors": 1, "wait_days": 6 },
            ]
        }),
        "list_doctors" => {
            let dept = args.get("department").and_then(|v| v.as_str()).unwrap_or("");
            let doctors = match dept.to_lowercase().as_str() {
                s if s.contains("general") => json!([
                    { "name": "Dr. Sarah Chen", "specialty": "Internal Medicine", "rating": 4.8 },
                    { "name": "Dr. James Wilson", "specialty": "Family Medicine", "rating": 4.6 },
                ]),
                s if s.contains("cardio") => json!([
                    { "name": "Dr. Priya Sharma", "specialty": "Interventional Cardiology", "rating": 4.9 },
                    { "name": "Dr. Michael Torres", "specialty": "Electrophysiology", "rating": 4.7 },
                ]),
                s if s.contains("derma") => json!([
                    { "name": "Dr. Emily Park", "specialty": "Medical Dermatology", "rating": 4.8 },
                    { "name": "Dr. David Kim", "specialty": "Cosmetic Dermatology", "rating": 4.5 },
                ]),
                s if s.contains("ortho") => json!([
                    { "name": "Dr. Robert Martinez", "specialty": "Sports Medicine", "rating": 4.7 },
                    { "name": "Dr. Lisa Anderson", "specialty": "Joint Replacement", "rating": 4.9 },
                ]),
                s if s.contains("pediatr") => json!([
                    { "name": "Dr. Nina Gupta", "specialty": "General Pediatrics", "rating": 4.9 },
                    { "name": "Dr. Thomas Lee", "specialty": "Pediatric Allergies", "rating": 4.6 },
                ]),
                s if s.contains("ent") => json!([
                    { "name": "Dr. Ahmed Hassan", "specialty": "Otolaryngology", "rating": 4.7 },
                ]),
                _ => json!([]),
            };
            json!({ "department": dept, "doctors": doctors })
        }
        "check_doctor_availability" => {
            json!({
                "available_slots": [
                    { "date": "2026-03-10", "times": ["9:00 AM", "11:30 AM", "2:00 PM"] },
                    { "date": "2026-03-11", "times": ["10:00 AM", "3:30 PM"] },
                    { "date": "2026-03-12", "times": ["9:30 AM", "1:00 PM", "4:00 PM"] },
                ]
            })
        }
        "book_appointment" => {
            let doctor = args.get("doctor_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let date = args.get("date").and_then(|v| v.as_str()).unwrap_or("");
            let time = args.get("time").and_then(|v| v.as_str()).unwrap_or("");
            json!({
                "appointment_id": format!("APT-{}-{:04}", date.replace('-', ""),
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default().subsec_nanos() % 10000),
                "status": "confirmed",
                "doctor": doctor,
                "date": date,
                "time": time,
                "instructions": "Please arrive 15 minutes early. Bring your insurance card and photo ID."
            })
        }
        "reschedule_appointment" => {
            json!({
                "status": "rescheduled",
                "new_date": args.get("new_date").unwrap_or(&json!("")),
                "new_time": args.get("new_time").unwrap_or(&json!("")),
                "message": "Your appointment has been rescheduled successfully."
            })
        }
        "cancel_appointment" => {
            json!({
                "status": "cancelled",
                "cancellation_id": "CAN-20260303-001",
                "refund_eligible": true,
                "message": "Your appointment has been cancelled. You may rebook at any time."
            })
        }
        "register_patient" => {
            json!({
                "patient_id": format!("PAT-{:05}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() % 100000),
                "status": "registered",
                "message": "Welcome to Clearview Medical Center! Your patient profile has been created."
            })
        }
        "lookup_patient" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let pid = args.get("patient_id").and_then(|v| v.as_str()).unwrap_or("");
            if name.to_lowercase().contains("maria garcia") || pid == "PAT-10042" {
                json!({
                    "found": true,
                    "patient_id": "PAT-10042",
                    "name": "Maria Garcia",
                    "insurance": "BlueCross",
                    "upcoming_appointments": [
                        { "date": "2026-03-15", "doctor": "Dr. Sarah Chen", "department": "General Medicine" }
                    ]
                })
            } else {
                json!({ "found": false })
            }
        }
        _ => json!({ "error": format!("Unknown tool: {name}") }),
    }
}

// ---------------------------------------------------------------------------
// Clinic app
// ---------------------------------------------------------------------------

/// Multi-specialty clinic voice AI receptionist with symptom triage,
/// department routing, doctor matching, and patient registration.
pub struct Clinic;

#[async_trait]
impl CookbookApp for Clinic {
    fn name(&self) -> &str {
        "clinic"
    }

    fn description(&self) -> &str {
        "Multi-specialty clinic with symptom triage, doctor matching, and patient registration"
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
            "symptom-triage".into(),
            "department-routing".into(),
        ]
    }

    fn tips(&self) -> Vec<String> {
        vec![
            "Describe symptoms like chest pain to see emergency handling".into(),
            "Try as a new patient to see registration flow".into(),
            "Ask for a specific department to skip triage".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "I need to see a doctor about persistent headaches".into(),
            "I'd like to reschedule my appointment with Dr. Chen".into(),
            "My child has had a fever for three days".into(),
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
// Session handler
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
        .add_tool(clinic_tools())
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
    let tx_enter_triage = tx.clone();
    let tx_enter_dept = tx.clone();
    let tx_enter_doctor = tx.clone();
    let tx_enter_booking = tx.clone();
    let tx_enter_reschedule = tx.clone();
    let tx_enter_registration = tx.clone();
    let tx_enter_confirmation = tx.clone();
    let tx_enter_farewell = tx.clone();

    // Watcher clones
    let tx_watcher_urgency = tx.clone();
    let tx_watcher_new_patient = tx.clone();
    let tx_watcher_dept_changed = tx.clone();

    // Temporal pattern clones
    let tx_triage_stalled = tx.clone();
    let tx_patient_anxious = tx.clone();

    // 4. Build Live::builder() with full pipeline
    let handle = Live::builder()
        // --- Model-initiated greeting ---
        .greeting("Welcome the patient to Clearview Medical Center and ask how you can help them today.")
        // --- LLM extraction ---
        .extract_turns_windowed::<ClinicState>(
            llm,
            "Extract from the medical clinic conversation: symptoms (list of strings), \
             preferred_department, preferred_doctor, patient_name, patient_id, \
             urgency (0.0=routine to 1.0=emergency, >0.9 if chest pain/breathing difficulty/severe bleeding), \
             intent (new_appointment/reschedule/cancel/inquiry), \
             insurance_mentioned (bool).",
            5,
        )
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
                        key: "extraction_error".into(),
                        value: json!({ "extractor": name, "error": error }),
                    });
                }
            }
        })
        // --- Computed state: symptom-to-department mapping ---
        .computed("suggested_department", &["symptoms"], |state| {
            let symptoms: String = state.get("symptoms").unwrap_or_default();
            if symptoms.is_empty() {
                return None;
            }
            Some(json!(suggest_department(&symptoms)))
        })
        // --- before_tool_response: state promotion from tool results ---
        .before_tool_response(move |responses, state| {
            async move {
                responses
                    .into_iter()
                    .map(|r| {
                        match r.name.as_str() {
                            "book_appointment" => {
                                if r.response.get("status").and_then(|v| v.as_str())
                                    == Some("confirmed")
                                {
                                    state.set("appointment_booked", true);
                                    if let Some(apt_id) =
                                        r.response.get("appointment_id").and_then(|v| v.as_str())
                                    {
                                        state.set(
                                            "appointment_id",
                                            apt_id.to_string(),
                                        );
                                    }
                                }
                            }
                            "register_patient" => {
                                if r.response.get("status").and_then(|v| v.as_str())
                                    == Some("registered")
                                {
                                    if let Some(pid) =
                                        r.response.get("patient_id").and_then(|v| v.as_str())
                                    {
                                        state.set("patient_id", pid.to_string());
                                        state.set("is_new_patient", false);
                                    }
                                }
                            }
                            "lookup_patient" => {
                                let found = r
                                    .response
                                    .get("found")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                if found {
                                    if let Some(pid) =
                                        r.response.get("patient_id").and_then(|v| v.as_str())
                                    {
                                        state.set("patient_id", pid.to_string());
                                    }
                                    state.set("is_new_patient", false);
                                } else {
                                    state.set("is_new_patient", true);
                                }
                            }
                            "reschedule_appointment" | "cancel_appointment" => {
                                if r.response.get("status").is_some() {
                                    state.set("appointment_booked", true);
                                }
                            }
                            _ => {}
                        }
                        r
                    })
                    .collect()
            }
        })
        // --- Phase defaults (inherited by all phases) ---
        .phase_defaults(|d| {
            d.with_state(CLINIC_STATE_KEYS)
                .when(urgency_is_critical, EMERGENCY_WARNING)
                .prompt_on_enter(true)
        })
        // --- 9 Phases ---
        // Phase 1: Greeting
        .phase("greeting")
            .instruction(GREETING_INSTRUCTION)
            .transition("symptom_triage", S::eq("intent", "new_appointment"))
            .transition("rescheduling", S::one_of("intent", &["reschedule", "cancel"]))
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_greeting.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "none".into(),
                        to: "greeting".into(),
                        reason: "Session started — greeting patient".into(),
                    });
                }
            })
            .done()
        // Phase 2: Symptom Triage
        .phase("symptom_triage")
            .instruction(SYMPTOM_TRIAGE_INSTRUCTION)
            .transition("department_selection", |s| {
                let symptoms: String = s.get("symptoms").unwrap_or_default();
                !symptoms.is_empty()
            })
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_triage.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greeting".into(),
                        to: "symptom_triage".into(),
                        reason: "Patient wants an appointment — triaging symptoms".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("symptom_triage"),
                    });
                }
            })
            .enter_prompt("The patient wants to book an appointment. I'll ask about their symptoms.")
            .done()
        // Phase 3: Department Selection
        .phase("department_selection")
            .instruction(DEPARTMENT_SELECTION_INSTRUCTION)
            .transition("doctor_selection", |s| {
                let dept: String = s.get("department").unwrap_or_default();
                !dept.is_empty()
            })
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_dept.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "symptom_triage".into(),
                        to: "department_selection".into(),
                        reason: "Symptoms collected — selecting department".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("department_selection"),
                    });
                }
            })
            .enter_prompt("I have a good understanding of the symptoms. Let me suggest the right department.")
            .done()
        // Phase 4: Doctor Selection
        .phase("doctor_selection")
            .instruction(DOCTOR_SELECTION_INSTRUCTION)
            .transition("appointment_booking", |s| {
                let doctor: String = s.get("doctor_name").unwrap_or_default();
                !doctor.is_empty()
            })
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_doctor.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "department_selection".into(),
                        to: "doctor_selection".into(),
                        reason: "Department confirmed — selecting doctor".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("doctor_selection"),
                    });
                }
            })
            .enter_prompt("The department is selected. Let me show the available doctors.")
            .done()
        // Phase 5: Appointment Booking
        .phase("appointment_booking")
            .instruction(APPOINTMENT_BOOKING_INSTRUCTION)
            .transition("patient_registration", S::is_true("is_new_patient"))
            .transition("confirmation", S::is_true("appointment_booked"))
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_booking.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "doctor_selection".into(),
                        to: "appointment_booking".into(),
                        reason: "Doctor selected — booking appointment".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("appointment_booking"),
                    });
                }
            })
            .enter_prompt("A doctor has been selected. I'll now book the appointment.")
            .done()
        // Phase 6: Rescheduling
        .phase("rescheduling")
            .instruction(RESCHEDULING_INSTRUCTION)
            .transition("confirmation", S::is_true("appointment_booked"))
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_reschedule.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "greeting".into(),
                        to: "rescheduling".into(),
                        reason: "Patient wants to reschedule or cancel".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("rescheduling"),
                    });
                }
            })
            .enter_prompt("The patient wants to reschedule or cancel. I'll look up their appointment.")
            .done()
        // Phase 7: Patient Registration
        .phase("patient_registration")
            .instruction(PATIENT_REGISTRATION_INSTRUCTION)
            .transition("appointment_booking", |s| {
                // Loop back once registered (is_new_patient becomes false)
                let is_new: bool = s.get("is_new_patient").unwrap_or(true);
                !is_new
            })
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_registration.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "appointment_booking".into(),
                        to: "patient_registration".into(),
                        reason: "New patient — collecting registration info".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("patient_registration"),
                    });
                }
            })
            .enter_prompt("This is a new patient. I'll collect their registration information.")
            .done()
        // Phase 8: Confirmation
        .phase("confirmation")
            .instruction(CONFIRMATION_INSTRUCTION)
            .transition("farewell", |_s| true)
            .on_enter(move |_state, _writer| {
                let tx = tx_enter_confirmation.clone();
                async move {
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "previous".into(),
                        to: "confirmation".into(),
                        reason: "Appointment processed — confirming details".into(),
                    });
                    let _ = tx.send(ServerMessage::StateUpdate {
                        key: "phase".into(),
                        value: json!("confirmation"),
                    });
                }
            })
            .enter_prompt("The appointment is booked. I'll now confirm all the details with the patient.")
            .done()
        // Phase 9: Farewell
        .phase("farewell")
            .instruction(FAREWELL_INSTRUCTION)
            .terminal()
            .on_enter(move |state, _writer| {
                let tx = tx_enter_farewell.clone();
                async move {
                    let is_new: bool = state.get("is_new_patient").unwrap_or(false);
                    let reason = if is_new {
                        "New patient registered — wrapping up with extra reminders"
                    } else {
                        "Appointment confirmed — saying goodbye"
                    };
                    let _ = tx.send(ServerMessage::PhaseChange {
                        from: "confirmation".into(),
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
                let is_new: bool = state.get("is_new_patient").unwrap_or(false);
                if is_new {
                    "Thank the patient for registering. Remind them to arrive 15 minutes early with insurance card.".into()
                } else {
                    "I'll wrap up the call and wish the patient well.".into()
                }
            })
            .done()
        .initial_phase("greeting")
        // --- Watchers ---
        // Emergency: clinical urgency crossed above 0.9
        .watch("clinical_urgency")
            .crossed_above(0.9)
            .blocking()
            .then({
                let tx = tx_watcher_urgency.clone();
                move |_old, _new, state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::Violation {
                            rule: "emergency_urgency".into(),
                            severity: "critical".into(),
                            detail: "Patient symptoms suggest a medical emergency — advise calling 911".into(),
                        });
                        state.set("emergency_detected", true);
                    }
                }
            })
        // Boolean: is_new_patient became true
        .watch("is_new_patient")
            .became_true()
            .then({
                let tx = tx_watcher_new_patient.clone();
                move |_old, _new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "watcher:new_patient".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Patient not found in system — registration required"
                            }),
                        });
                    }
                }
            })
        // Value: suggested_department changed
        .watch("derived:suggested_department")
            .changed()
            .then({
                let tx = tx_watcher_dept_changed.clone();
                move |_old, new, _state| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "watcher:suggested_department".into(),
                            value: json!({
                                "triggered": true,
                                "new_department": new,
                                "action": "Symptom analysis suggests a department"
                            }),
                        });
                    }
                }
            })
        // --- Temporal patterns ---
        // Turns: triage stalled for 3 turns with no symptoms extracted
        .when_turns(
            "triage_stalled",
            |s| {
                let phase: String = s.get("session:phase").unwrap_or_default();
                let symptoms: String = s.get("symptoms").unwrap_or_default();
                phase == "symptom_triage" && symptoms.is_empty()
            },
            3,
            {
                let tx = tx_triage_stalled.clone();
                move |_state, writer| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "temporal:triage_stalled".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Triage stalled for 3 turns — asking more directed questions"
                            }),
                        });
                        let _ = writer
                            .send_client_content(
                                vec![Content::user(
                                    "[System: The patient hasn't described specific symptoms yet. \
                                     Ask more directed questions: Where does it hurt? How long has \
                                     this been going on? Is it getting worse? Any other symptoms?]",
                                )],
                                false,
                            )
                            .await;
                    }
                }
            },
        )
        // Sustained: patient showing anxiety for 20 seconds
        .when_sustained(
            "patient_anxious",
            |s| {
                let urgency: f64 = s.get("clinical_urgency").unwrap_or(0.0);
                urgency > 0.6
            },
            Duration::from_secs(20),
            {
                let tx = tx_patient_anxious.clone();
                move |_state, writer| {
                    let tx = tx.clone();
                    async move {
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: "temporal:patient_anxious".into(),
                            value: json!({
                                "triggered": true,
                                "action": "Patient has been anxious for over 20 seconds — injecting reassurance"
                            }),
                        });
                        let _ = writer
                            .send_client_content(
                                vec![Content::user(
                                    "[System: The patient seems anxious about their health. \
                                     Please reassure them that they are in good hands. \
                                     Clearview Medical Center has experienced specialists \
                                     who will take excellent care of them. Help them feel \
                                     comfortable and heard.]",
                                )],
                                false,
                            )
                            .await;
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
                info!("Clinic session disconnected: {reason:?}");
            }
        })
        .connect(config)
        .await
        .map_err(|e| AppError::Connection(e.to_string()))?;

    // 5. Send Connected + AppMeta + initial state
    let _ = tx.send(ServerMessage::Connected);
    let clinic = Clinic;
    send_app_meta(&tx, &clinic);
    info!("Clinic session connected");

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
                let urgency: f64 = telem_state.get("clinical_urgency").unwrap_or(0.0);
                let dept: String = telem_state
                    .get("derived:suggested_department")
                    .unwrap_or_default();
                let tc: u32 = telem_state.session().get("turn_count").unwrap_or(0);
                obj.insert("current_phase".into(), json!(phase));
                obj.insert("clinical_urgency".into(), json!(urgency));
                obj.insert("suggested_department".into(), json!(dept));
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
        key: "clinical_urgency".into(),
        value: json!(0.0),
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
                info!("Clinic session stopping");
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
    fn list_departments_returns_all() {
        let result = execute_tool("list_departments", &json!({}));
        let depts = result["departments"].as_array().unwrap();
        assert_eq!(depts.len(), 6);
        assert_eq!(depts[0]["name"], "General Medicine");
        assert_eq!(depts[1]["name"], "Cardiology");
    }

    #[test]
    fn list_doctors_cardiology() {
        let result = execute_tool("list_doctors", &json!({"department": "Cardiology"}));
        let doctors = result["doctors"].as_array().unwrap();
        assert_eq!(doctors.len(), 2);
        assert_eq!(doctors[0]["name"], "Dr. Priya Sharma");
    }

    #[test]
    fn list_doctors_unknown_department() {
        let result = execute_tool("list_doctors", &json!({"department": "Neurology"}));
        let doctors = result["doctors"].as_array().unwrap();
        assert!(doctors.is_empty());
    }

    #[test]
    fn check_doctor_availability_returns_slots() {
        let result = execute_tool(
            "check_doctor_availability",
            &json!({"doctor_name": "Dr. Sarah Chen"}),
        );
        let slots = result["available_slots"].as_array().unwrap();
        assert_eq!(slots.len(), 3);
        assert!(slots[0]["times"].as_array().unwrap().len() > 0);
    }

    #[test]
    fn book_appointment_confirmed() {
        let result = execute_tool(
            "book_appointment",
            &json!({
                "patient_id": "PAT-10042",
                "doctor_name": "Dr. Sarah Chen",
                "date": "2026-03-10",
                "time": "9:00 AM"
            }),
        );
        assert_eq!(result["status"], "confirmed");
        assert!(result["appointment_id"]
            .as_str()
            .unwrap()
            .starts_with("APT-"));
        assert_eq!(result["doctor"], "Dr. Sarah Chen");
    }

    #[test]
    fn reschedule_appointment_success() {
        let result = execute_tool(
            "reschedule_appointment",
            &json!({
                "appointment_id": "APT-20260310-0001",
                "new_date": "2026-03-12",
                "new_time": "1:00 PM"
            }),
        );
        assert_eq!(result["status"], "rescheduled");
        assert_eq!(result["new_date"], "2026-03-12");
    }

    #[test]
    fn cancel_appointment_success() {
        let result = execute_tool(
            "cancel_appointment",
            &json!({"appointment_id": "APT-20260310-0001"}),
        );
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["refund_eligible"], true);
    }

    #[test]
    fn register_patient_returns_id() {
        let result = execute_tool(
            "register_patient",
            &json!({
                "name": "John Doe",
                "date_of_birth": "1990-05-20",
                "phone": "555-999-1234",
                "insurance_provider": "Aetna"
            }),
        );
        assert_eq!(result["status"], "registered");
        assert!(result["patient_id"].as_str().unwrap().starts_with("PAT-"));
    }

    #[test]
    fn lookup_patient_found() {
        let result = execute_tool("lookup_patient", &json!({"name": "Maria Garcia"}));
        assert_eq!(result["found"], true);
        assert_eq!(result["patient_id"], "PAT-10042");
        assert_eq!(result["insurance"], "BlueCross");
    }

    #[test]
    fn lookup_patient_by_id() {
        let result = execute_tool("lookup_patient", &json!({"patient_id": "PAT-10042"}));
        assert_eq!(result["found"], true);
        assert_eq!(result["name"], "Maria Garcia");
    }

    #[test]
    fn lookup_patient_not_found() {
        let result = execute_tool("lookup_patient", &json!({"name": "Unknown Person"}));
        assert_eq!(result["found"], false);
    }

    #[test]
    fn unknown_tool_returns_error() {
        let result = execute_tool("nonexistent", &json!({}));
        assert!(result["error"].as_str().unwrap().contains("Unknown"));
    }

    // -- Suggest department logic --

    #[test]
    fn suggest_chest_pain_cardiology() {
        assert_eq!(suggest_department("chest pain and tightness"), "Cardiology");
    }

    #[test]
    fn suggest_heart_cardiology() {
        assert_eq!(suggest_department("heart palpitations"), "Cardiology");
    }

    #[test]
    fn suggest_blood_pressure_cardiology() {
        assert_eq!(suggest_department("high blood pressure"), "Cardiology");
    }

    #[test]
    fn suggest_skin_dermatology() {
        assert_eq!(suggest_department("skin rash on arms"), "Dermatology");
    }

    #[test]
    fn suggest_acne_dermatology() {
        assert_eq!(suggest_department("persistent acne"), "Dermatology");
    }

    #[test]
    fn suggest_joint_orthopedics() {
        assert_eq!(suggest_department("joint pain in knee"), "Orthopedics");
    }

    #[test]
    fn suggest_back_pain_orthopedics() {
        assert_eq!(suggest_department("lower back pain"), "Orthopedics");
    }

    #[test]
    fn suggest_child_pediatrics() {
        assert_eq!(suggest_department("my child has a fever"), "Pediatrics");
    }

    #[test]
    fn suggest_ear_ent() {
        assert_eq!(suggest_department("ear infection and hearing loss"), "ENT");
    }

    #[test]
    fn suggest_sinus_ent() {
        assert_eq!(suggest_department("chronic sinus problems"), "ENT");
    }

    #[test]
    fn suggest_headache_general() {
        assert_eq!(
            suggest_department("persistent headaches"),
            "General Medicine"
        );
    }

    #[test]
    fn suggest_empty_general() {
        assert_eq!(suggest_department(""), "General Medicine");
    }

    // -- App metadata --

    #[test]
    fn app_metadata() {
        let app = Clinic;
        assert_eq!(app.name(), "clinic");
        assert_eq!(app.category(), AppCategory::Showcase);
        assert!(app.features().contains(&"phase-machine".to_string()));
        assert!(app.features().contains(&"llm-extraction".to_string()));
        assert!(app.features().contains(&"symptom-triage".to_string()));
        assert!(app
            .features()
            .contains(&"department-routing".to_string()));
    }

    #[test]
    fn app_has_tips() {
        let app = Clinic;
        assert!(!app.tips().is_empty());
        assert!(app.tips().len() >= 3);
    }

    #[test]
    fn app_has_try_saying() {
        let app = Clinic;
        assert!(!app.try_saying().is_empty());
        assert!(app.try_saying().len() >= 3);
    }
}
