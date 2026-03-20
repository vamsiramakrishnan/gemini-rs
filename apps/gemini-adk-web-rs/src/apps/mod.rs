mod all_config;
#[allow(unused)]
mod call_screening;
#[allow(unused)]
mod clinic;
#[allow(unused)]
mod debt_collection;
pub mod extractors;
mod guardrails;
mod playbook;
#[allow(unused)]
mod restaurant;
mod support;
mod text_chat;
mod tool_calling;
mod voice_chat;

use std::sync::Arc;

use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams};
use gemini_genai_rs::prelude::*;
use tokio::sync::mpsc;

use crate::app::{AppError, AppInfo, AppRegistry, ClientMessage, DemoApp, ServerMessage, WsSender};

/// Register all demo apps.
pub fn register_all(registry: &mut AppRegistry) {
    registry.register(text_chat::TextChat);
    registry.register(voice_chat::VoiceChat);
    registry.register(tool_calling::ToolCalling);
    registry.register(playbook::Playbook);
    registry.register(guardrails::Guardrails);
    registry.register(support::SupportAssistant);
    registry.register(all_config::AllConfig);
    registry.register(debt_collection::DebtCollection);
    registry.register(call_screening::CallScreening);
    registry.register(restaurant::Restaurant);
    registry.register(clinic::Clinic);
}

/// Parameters extracted from the browser's Start message.
pub struct StartParams {
    pub system_instruction: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
}

/// Wait for the Start message from the browser.
/// Returns the extracted parameters, or an error if the client disconnects or sends
/// an unexpected message first.
pub async fn wait_for_start(
    rx: &mut mpsc::UnboundedReceiver<ClientMessage>,
) -> Result<StartParams, AppError> {
    match rx.recv().await {
        Some(ClientMessage::Start {
            system_instruction,
            model,
            voice,
        }) => Ok(StartParams {
            system_instruction,
            model,
            voice,
        }),
        Some(_) => Err(AppError::Session("Expected Start message".into())),
        None => Err(AppError::Connection(
            "Client disconnected before start".into(),
        )),
    }
}

/// Resolve the Vertex AI access token from env vars or gcloud CLI.
fn resolve_vertex_token() -> Result<String, String> {
    std::env::var("GOOGLE_ACCESS_TOKEN")
        .or_else(|_| std::env::var("GCLOUD_ACCESS_TOKEN"))
        .or_else(|_| {
            std::process::Command::new("gcloud")
                .args(["auth", "print-access-token"])
                .output()
                .map_err(|e| format!("Failed to run gcloud: {e}"))
                .and_then(|output| {
                    if output.status.success() {
                        String::from_utf8(output.stdout)
                            .map(|s| s.trim().to_string())
                            .map_err(|e| format!("Invalid gcloud output: {e}"))
                    } else {
                        Err(format!(
                            "gcloud failed: {}",
                            String::from_utf8_lossy(&output.stderr)
                        ))
                    }
                })
        })
        .map_err(|e| format!("Cannot obtain Vertex AI access token: {e}"))
}

/// Build a base `SessionConfig` from environment variables.
///
/// Reads `GOOGLE_GENAI_USE_VERTEXAI`, `GOOGLE_CLOUD_PROJECT`, and API key env vars
/// to determine the correct endpoint.
///
/// For the Live session location, prefers `GEMINI_LIVE_MODEL_LOCATION` over
/// `GOOGLE_CLOUD_LOCATION` (default: `us-central1`). This allows setting
/// `GOOGLE_CLOUD_LOCATION=global` for HTTP APIs while using a regional
/// endpoint for the Live WebSocket API.
///
/// For the model, prefers `model` argument > `GEMINI_LIVE_MODEL` > `GEMINI_MODEL`.
pub fn build_session_config(model: Option<&str>) -> Result<SessionConfig, String> {
    let use_vertex = std::env::var("GOOGLE_GENAI_USE_VERTEXAI")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mut config = if use_vertex {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT")
            .map_err(|_| "GOOGLE_CLOUD_PROJECT env var not set".to_string())?;
        let location = std::env::var("GEMINI_LIVE_MODEL_LOCATION")
            .or_else(|_| std::env::var("GOOGLE_CLOUD_LOCATION"))
            .unwrap_or_else(|_| "us-central1".to_string());

        let access_token = resolve_vertex_token()?;

        SessionConfig::from_vertex(project, location, access_token)
    } else {
        // Google AI with API key.
        let api_key = std::env::var("GOOGLE_GENAI_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .map_err(|_| {
                "No API key found. Set GOOGLE_GENAI_API_KEY or GEMINI_API_KEY, \
                 or set GOOGLE_GENAI_USE_VERTEXAI=TRUE for Vertex AI."
                    .to_string()
            })?;

        SessionConfig::new(api_key)
    };

    let effective_model = model
        .map(|s| s.to_string())
        .or_else(|| std::env::var("GEMINI_LIVE_MODEL").ok())
        .or_else(|| std::env::var("GEMINI_MODEL").ok());

    if let Some(m) = effective_model {
        config = config.model(GeminiModel::Custom(m));
    }

    Ok(config)
}

/// Resolve the Live session model from env vars, falling back to `Gemini2_0FlashLive`.
///
/// Precedence: `GEMINI_LIVE_MODEL` env var > `GeminiModel::Gemini2_0FlashLive`.
pub fn live_model() -> GeminiModel {
    std::env::var("GEMINI_LIVE_MODEL")
        .ok()
        .map(GeminiModel::Custom)
        .unwrap_or(GeminiModel::Gemini2_0FlashLive)
}

/// Build a `GeminiLlm` for OOB extraction from environment variables.
///
/// Reads `GEMINI_EXTRACTION_MODEL` (default: `gemini-2.0-flash`) and
/// `GEMINI_EXTRACTION_LOCATION` (default: `GOOGLE_CLOUD_LOCATION`, then `us-central1`).
pub fn build_extraction_llm() -> Arc<dyn BaseLlm> {
    let model = std::env::var("GEMINI_EXTRACTION_MODEL").ok();
    let location = std::env::var("GEMINI_EXTRACTION_LOCATION")
        .or_else(|_| std::env::var("GOOGLE_CLOUD_LOCATION"))
        .ok();

    Arc::new(GeminiLlm::new(GeminiLlmParams {
        model,
        location,
        ..Default::default()
    }))
}

/// Send appMeta message to the browser so devtools can configure tabs.
#[allow(dead_code)]
pub fn send_app_meta(tx: &WsSender, app: &dyn DemoApp) {
    let _ = tx.send(ServerMessage::AppMeta {
        info: AppInfo {
            name: app.name().to_string(),
            description: app.description().to_string(),
            category: app.category(),
            features: app.features(),
            tips: app.tips(),
            try_saying: app.try_saying(),
        },
    });
}

/// Resolve a voice name string to the Voice enum.
pub fn resolve_voice(name: Option<&str>) -> Voice {
    match name {
        Some("Aoede") => Voice::Aoede,
        Some("Charon") => Voice::Charon,
        Some("Fenrir") => Voice::Fenrir,
        Some("Kore") => Voice::Kore,
        Some("Puck") | None => Voice::Puck,
        Some(other) => Voice::Custom(other.to_string()),
    }
}

/// Rolling buffer of recent conversation turns for analysis.
#[cfg(test)]
pub struct ConversationBuffer {
    turns: Vec<String>,
    max_turns: usize,
}

#[cfg(test)]
impl ConversationBuffer {
    pub fn new(max_turns: usize) -> Self {
        Self {
            turns: Vec::new(),
            max_turns,
        }
    }

    pub fn push(&mut self, text: String) {
        self.turns.push(text);
        if self.turns.len() > self.max_turns {
            self.turns.remove(0);
        }
    }

    pub fn recent_text(&self) -> String {
        self.turns.join(" ")
    }
}
