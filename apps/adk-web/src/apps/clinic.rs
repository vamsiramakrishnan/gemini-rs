use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::info;

use adk_rs_fluent::prelude::*;
use rs_adk::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};
use rs_adk::state::StateKey;

use crate::app::{AppError, ClientMessage, CookbookApp, WsSender};
use crate::bridge::SessionBridge;
use crate::cookbook_meta;

use super::resolve_voice;

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
// Phase instructions — lean directives for what to do in each phase.
// Contextual awareness ("where we are, what we know") is provided by
// the clinic_context() closure via with_context, so the model always
// has situational bearings without repeating state in the instructions.
// ---------------------------------------------------------------------------

const SYSTEM_INSTRUCTION: &str = "\
You are the AI receptionist for Clearview Medical Center, a multi-specialty clinic. \
You help patients book, reschedule, and cancel appointments across departments. \
Be empathetic and professional. NEVER provide medical diagnoses. \
If symptoms suggest an emergency (chest pain, difficulty breathing, severe bleeding), \
advise calling 911 immediately.";

const GREETING_INSTRUCTION: &str = "\
Welcome the patient to Clearview Medical Center. Ask how you can help today. \
Determine their intent: new appointment, reschedule, or other inquiry. \
If they mention a name, look them up.";

const SYMPTOM_TRIAGE_INSTRUCTION: &str = "\
Ask about symptoms empathetically. Understand nature, duration, and severity. \
Do NOT diagnose. If they mention chest pain, difficulty breathing, or severe bleeding, \
advise calling 911 immediately.";

const DEPARTMENT_SELECTION_INSTRUCTION: &str = "\
Suggest the most appropriate department using list_departments. \
Explain your recommendation and confirm with the patient before proceeding.";

const DOCTOR_SELECTION_INSTRUCTION: &str = "\
Show available doctors using list_doctors. Share specialties and ratings. \
Let the patient choose or recommend based on their needs. \
Use check_doctor_availability for time slots.";

const APPOINTMENT_BOOKING_INSTRUCTION: &str = "\
Book the appointment using book_appointment. Confirm all details before booking. \
Use lookup_patient to check if they are an existing patient. \
If not in our system, let them know registration is needed first.";

const RESCHEDULING_INSTRUCTION: &str = "\
Help reschedule or cancel their appointment. Look up current details, \
offer new slots via check_doctor_availability. \
Use reschedule_appointment or cancel_appointment as needed.";

const PATIENT_REGISTRATION_INSTRUCTION: &str = "\
Collect: full name, date of birth, phone number, and insurance provider (optional). \
Use register_patient to create their profile. Be welcoming.";

const CONFIRMATION_INSTRUCTION: &str = "\
Confirm the appointment details: doctor, date, time, department, and appointment ID. \
Make sure the patient has everything they need.";

const FAREWELL_INSTRUCTION: &str = "\
Thank the patient. Remind them to bring insurance card and photo ID. \
New patients should arrive 15 minutes early. Wish them well.";

// ---------------------------------------------------------------------------
// Geolocation context — natural-language summary of accumulated state.
// Attached via with_context in phase_defaults so every phase instruction
// gets situational bearings: where we are, what we know, what's still needed.
// ---------------------------------------------------------------------------

fn clinic_context(s: &State) -> String {
    let mut ctx = Vec::new();

    // Patient info
    let name: Option<String> = s.get("patient_name");
    let pid: Option<String> = s.get("patient_id");
    let is_new: Option<bool> = s.get("is_new_patient");
    match (&name, &pid, is_new) {
        (Some(n), Some(id), Some(true)) => {
            ctx.push(format!("Patient: {n} ({id}, new patient)."));
        }
        (Some(n), Some(id), _) => {
            ctx.push(format!("Patient: {n} ({id})."));
        }
        (Some(n), None, Some(true)) => {
            ctx.push(format!("Patient: {n} (new, not yet registered)."));
        }
        (Some(n), None, _) => {
            ctx.push(format!("Patient: {n}."));
        }
        (None, _, Some(true)) => {
            ctx.push("Patient is new (not in our system).".into());
        }
        _ => {}
    }

    // Insurance
    if let Some(ins) = s.get::<String>("insurance_provider") {
        if !ins.is_empty() {
            ctx.push(format!("Insurance: {ins}."));
        }
    }

    // Symptoms
    if let Some(symptoms) = s.get::<String>("symptoms") {
        if !symptoms.is_empty() {
            ctx.push(format!("Symptoms: {symptoms}."));
        }
    }

    // Urgency
    let urgency: f64 = s.get("clinical_urgency").unwrap_or(0.0);
    if urgency > 0.0 {
        let label = if urgency > 0.9 {
            "critical"
        } else if urgency > 0.6 {
            "elevated"
        } else {
            "low"
        };
        ctx.push(format!("Urgency: {label} ({urgency:.1})."));
    }

    // Department
    let dept: Option<String> = s.get("department");
    let suggested: Option<String> = s.get("derived:suggested_department");
    match (&dept, &suggested) {
        (Some(d), _) => ctx.push(format!("Department: {d} (confirmed).")),
        (None, Some(d)) => ctx.push(format!("Suggested department: {d} (not yet confirmed).")),
        _ => {}
    }

    // Doctor
    if let Some(doc) = s.get::<String>("doctor_name") {
        if !doc.is_empty() {
            ctx.push(format!("Doctor: {doc}."));
        }
    }

    // Appointment
    let date: Option<String> = s.get("appointment_date");
    let time: Option<String> = s.get("appointment_time");
    let booked: bool = s.get("appointment_booked").unwrap_or(false);
    let apt_id: Option<String> = s.get("appointment_id");
    if booked {
        if let Some(id) = &apt_id {
            ctx.push(format!("Appointment {id} booked."));
        } else {
            ctx.push("Appointment confirmed.".into());
        }
    }
    match (&date, &time) {
        (Some(d), Some(t)) => ctx.push(format!("Scheduled: {d} at {t}.")),
        (Some(d), None) => ctx.push(format!("Date selected: {d}.")),
        _ => {}
    }

    // Intent
    if let Some(intent) = s.get::<String>("intent") {
        if !intent.is_empty() {
            let label = match intent.as_str() {
                "new_appointment" => "booking a new appointment",
                "reschedule" => "rescheduling an existing appointment",
                "cancel" => "cancelling an appointment",
                "inquiry" => "general inquiry",
                other => other,
            };
            ctx.push(format!("Intent: {label}."));
        }
    }

    if ctx.is_empty() {
        String::new()
    } else {
        ctx.join(" ")
    }
}

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
    use rs_genai::prelude::{FunctionCallingBehavior, FunctionDeclaration, Tool};
    Tool::functions(vec![
        FunctionDeclaration {
            name: "list_departments".into(),
            description: "List all available departments at Clearview Medical Center with doctor counts and wait times.".into(),
            parameters: Some(json!({
                "type": "object",
                "properties": {},
                "required": []
            })),
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
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
            behavior: Some(FunctionCallingBehavior::NonBlocking),
        },
    ])
}

// ---------------------------------------------------------------------------
// Mock tool execution
// ---------------------------------------------------------------------------

fn execute_tool(name: &str, args: &serde_json::Value) -> serde_json::Value {
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
            let dept = args
                .get("department")
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
            let doctor = args
                .get("doctor_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
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
            let pid = args
                .get("patient_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
    cookbook_meta! {
        name: "clinic",
        description: "Multi-specialty clinic with symptom triage, doctor matching, and patient registration",
        category: Showcase,
        features: [
            "phase-machine",
            "llm-extraction",
            "tool-calling",
            "watchers",
            "computed-state",
            "temporal-patterns",
            "state-keys",
            "symptom-triage",
            "department-routing",
        ],
        tips: [
            "Describe symptoms like chest pain to see emergency handling",
            "Try as a new patient to see registration flow",
            "Ask for a specific department to skip triage",
        ],
        try_saying: [
            "I need to see a doctor about persistent headaches",
            "I'd like to reschedule my appointment with Dr. Chen",
            "My child has had a fever for three days",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        // DESIGN DISSECTION: Why this app is built the way it is
        //
        // Steering Mode: ContextInjection
        //   The clinic receptionist persona is stable across all phases — intake,
        //   triage, scheduling, confirmation. Only the *focus* shifts, not the
        //   identity. ContextInjection avoids replacing the system instruction on
        //   every transition, reducing latency.
        //
        // Context Delivery: Deferred
        //   Queues context turns until the next user audio chunk. Prevents
        //   isolated WebSocket frames during silence that can cause audio glitches.
        //
        // greeting (not prompt_on_enter):
        //   `.greeting()` fires once at session start via send_text. The model
        //   initiates "Welcome to Clearview Medical Center..." without needing
        //   prompt_on_enter on the initial phase.
        //
        // No prompt_on_enter on any phase:
        //   After the initial greeting, the patient drives the conversation.
        //   Phase transitions happen in response to patient statements, and the
        //   model naturally continues without needing a turn prompt.
        //
        // with_context for clinical state:
        //   Symptoms, urgency, and department are injected as model-role context
        //   turns. This lets the model reference clinical details without baking
        //   volatile state into the system instruction.
        //
        // State-based transitions:
        //   Transitions fire on extracted state (urgency > 0.8, symptoms present,
        //   patient_id set). The LLM naturally gathers this information through
        //   conversation — no turn-count fallbacks needed.

        info!("Clinic session starting");
        SessionBridge::new(tx)
            .run(self, &mut rx, |live, start| {
                let voice = resolve_voice(start.voice.as_deref());

                // Create GeminiLlm for LLM extraction
                let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
                    model: Some("gemini-3.1-flash-lite-preview".to_string()),
                    location: Some("global".to_string()),
                    ..Default::default()
                }));

                live.model(GeminiModel::Gemini2_0FlashLive)
                    .voice(voice)
                    .instruction(
                        start
                            .system_instruction
                            .as_deref()
                            .unwrap_or(SYSTEM_INSTRUCTION),
                    )
                    .transcription(true, true)
                    .add_tool(clinic_tools())
                    .steering_mode(SteeringMode::ContextInjection)
                    .context_delivery(ContextDelivery::Deferred)
                    // --- Model-initiated greeting ---
                    .greeting("Welcome the patient to Clearview Medical Center and ask how you can help them today.")
                    // --- LLM extraction ---
                    .extract_turns_triggered::<ClinicState>(
                        llm,
                        "Extract from the medical clinic conversation: symptoms (list of strings), \
                         preferred_department, preferred_doctor, patient_name, patient_id, \
                         urgency (0.0=routine to 1.0=emergency, >0.9 if chest pain/breathing difficulty/severe bleeding), \
                         intent (new_appointment/reschedule/cancel/inquiry), \
                         insurance_mentioned (bool).",
                        5,
                        ExtractionTrigger::Interval(2),
                    )
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
                                .inspect(|r| {
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
                                })
                                .collect()
                        }
                    })
                    // --- Phase defaults (inherited by all phases) ---
                    .phase_defaults(|d| {
                        d.navigation()
                            .with_context(clinic_context)
                            .when(urgency_is_critical, EMERGENCY_WARNING)
                    })
                    // --- 9 Phases ---
                    // Phase 1: Greeting
                    .phase("greeting")
                        .instruction(GREETING_INSTRUCTION)
                        .prompt_on_enter(true)
                        .needs(&["intent"])
                        .transition_with("symptom_triage", S::eq("intent", "new_appointment"), "when intent is new_appointment")
                        .transition_with("rescheduling", S::one_of("intent", &["reschedule", "cancel"]), "when intent is reschedule or cancel")
                        .done()
                    // Phase 2: Symptom Triage
                    .phase("symptom_triage")
                        .instruction(SYMPTOM_TRIAGE_INSTRUCTION)
                        .needs(&["symptoms", "symptom_severity"])
                        .transition_with("department_selection", |s| {
                            let symptoms: String = s.get("symptoms").unwrap_or_default();
                            !symptoms.is_empty()
                        }, "when symptoms have been described")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("patient_name").unwrap_or_else(|| "the patient".into());
                            format!("{name} wants to book an appointment. I'll ask about their symptoms.")
                        })
                        .done()
                    // Phase 3: Department Selection
                    .phase("department_selection")
                        .instruction(DEPARTMENT_SELECTION_INSTRUCTION)
                        .needs(&["department"])
                        .transition_with("doctor_selection", |s| {
                            let dept: String = s.get("department").unwrap_or_default();
                            !dept.is_empty()
                        }, "when department is selected")
                        .enter_prompt_fn(|s, _| {
                            let suggested: String = s.get("derived:suggested_department").unwrap_or_default();
                            if suggested.is_empty() {
                                "I have a good understanding of the symptoms. Let me suggest the right department.".into()
                            } else {
                                format!("Based on the symptoms, {suggested} looks like the right fit. Let me confirm with the patient.")
                            }
                        })
                        .done()
                    // Phase 4: Doctor Selection
                    .phase("doctor_selection")
                        .instruction(DOCTOR_SELECTION_INSTRUCTION)
                        .needs(&["doctor_name"])
                        .transition_with("appointment_booking", |s| {
                            let doctor: String = s.get("doctor_name").unwrap_or_default();
                            !doctor.is_empty()
                        }, "when doctor is chosen")
                        .enter_prompt_fn(|s, _| {
                            let dept: String = s.get("department").unwrap_or_else(|| "the selected department".into());
                            format!("{dept} is confirmed. Let me show the available doctors.")
                        })
                        .done()
                    // Phase 5: Appointment Booking
                    .phase("appointment_booking")
                        .instruction(APPOINTMENT_BOOKING_INSTRUCTION)
                        .needs(&["appointment_date", "appointment_time"])
                        .transition_with("patient_registration", S::is_true("is_new_patient"), "when patient is new (is_new_patient)")
                        .transition_with("confirmation", S::is_true("appointment_booked"), "when appointment is booked")
                        .enter_prompt_fn(|s, _| {
                            let doctor: String = s.get("doctor_name").unwrap_or_else(|| "the selected doctor".into());
                            format!("{doctor} has been selected. I'll now book the appointment.")
                        })
                        .done()
                    // Phase 6: Rescheduling
                    .phase("rescheduling")
                        .instruction(RESCHEDULING_INSTRUCTION)
                        .transition_with("confirmation", S::is_true("appointment_booked"), "when appointment is rescheduled")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("patient_name").unwrap_or_else(|| "The patient".into());
                            let intent: String = s.get("intent").unwrap_or_else(|| "reschedule".into());
                            let action = if intent == "cancel" { "cancel" } else { "reschedule" };
                            format!("{name} wants to {action} their appointment. I'll look up the details.")
                        })
                        .done()
                    // Phase 7: Patient Registration
                    .phase("patient_registration")
                        .instruction(PATIENT_REGISTRATION_INSTRUCTION)
                        .needs(&["patient_name", "insurance_provider"])
                        .transition_with("appointment_booking", |s| {
                            // Loop back once registered (is_new_patient becomes false)
                            let is_new: bool = s.get("is_new_patient").unwrap_or(true);
                            !is_new
                        }, "when registration is complete")
                        .enter_prompt_fn(|s, _| {
                            let name: String = s.get("patient_name").unwrap_or_default();
                            if name.is_empty() {
                                "This is a new patient. I'll collect their registration information.".into()
                            } else {
                                format!("{name} is new to Clearview Medical Center. I'll get them registered.")
                            }
                        })
                        .done()
                    // Phase 8: Confirmation
                    .phase("confirmation")
                        .instruction(CONFIRMATION_INSTRUCTION)
                        .transition_with("farewell", |_s| true, "when confirmation is acknowledged")
                        .enter_prompt_fn(|s, _| {
                            let doctor: String = s.get("doctor_name").unwrap_or_default();
                            let apt_id: String = s.get("appointment_id").unwrap_or_default();
                            if !doctor.is_empty() && !apt_id.is_empty() {
                                format!("Appointment {apt_id} with {doctor} is booked. I'll confirm all the details.")
                            } else {
                                "The appointment is processed. I'll confirm the details with the patient.".into()
                            }
                        })
                        .done()
                    // Phase 9: Farewell
                    .phase("farewell")
                        .instruction(FAREWELL_INSTRUCTION)
                        .terminal()
                        .enter_prompt_fn(|state, _tw| {
                            let name: String = state.get("patient_name").unwrap_or_else(|| "the patient".into());
                            let is_new: bool = state.get("is_new_patient").unwrap_or(false);
                            let doctor: String = state.get("doctor_name").unwrap_or_default();
                            if is_new && !doctor.is_empty() {
                                format!("Thank {name} for registering. Their appointment with {doctor} is confirmed. Remind them to arrive 15 minutes early.")
                            } else if is_new {
                                format!("Thank {name} for registering. Remind them to arrive 15 minutes early with insurance card.")
                            } else if !doctor.is_empty() {
                                format!("{name}'s appointment with {doctor} is all set. I'll wrap up and wish them well.")
                            } else {
                                format!("I'll wrap up the call with {name} and wish them well.")
                            }
                        })
                        .done()
                    .initial_phase("greeting")
                    // --- Watchers ---
                    // Emergency: clinical urgency crossed above 0.9
                    .watch("clinical_urgency")
                        .crossed_above(0.9)
                        .blocking()
                        .then(move |_old, _new, state| {
                            async move {
                                state.set("emergency_detected", true);
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
                        move |_state, writer| {
                            async move {
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
                        move |_state, writer| {
                            async move {
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
                        },
                    )
                    // --- on_tool_call: mock tool dispatch ---
                    .on_tool_call(|calls, _state| {
                        async move {
                            let responses: Vec<FunctionResponse> = calls
                                .iter()
                                .map(|call| {
                                    let result = execute_tool(&call.name, &call.args);
                                    FunctionResponse {
                                        name: call.name.clone(),
                                        response: result,
                                        id: call.id.clone(),
                                        scheduling: Some(FunctionResponseScheduling::WhenIdle),
                                    }
                                })
                                .collect();
                            Some(responses)
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
        assert!(!slots[0]["times"].as_array().unwrap().is_empty());
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
        assert!(app.features().contains(&"department-routing".to_string()));
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
