mod all_config;
mod call_screening;
mod clinic;
mod debt_collection;
pub mod extractors;
mod flow_studio;
mod guardrails;
mod playbook;
mod restaurant;
mod support;
mod text_chat;
mod tool_calling;
mod voice_chat;

use gemini_genai_rs::prelude::*;
use tokio::sync::mpsc;

// One copy of each, shared with the other surfaces: the extraction LLM from
// `GEMINI_EXTRACTION_*` (server crate) and the voice-name lookup (fluent).
pub use gemini_adk_fluent_rs::spec::resolve_voice;
pub use gemini_adk_server_rs::execution::build_extraction_llm;
use gemini_genai_rs::protocol::types::{
    api_key_from_env, cloud_location_from_env, cloud_project_from_env, non_empty_env,
    vertex_requested,
};

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
    registry.register(flow_studio::FlowStudio);
}

/// Parameters extracted from the browser's Start message.
pub struct StartParams {
    pub system_instruction: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
    /// App-specific configuration payload (e.g. a Flow Studio app spec).
    pub config: Option<serde_json::Value>,
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
            config,
        }) => Ok(StartParams {
            system_instruction,
            model,
            voice,
            config,
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
/// Reads `GOOGLE_GENAI_USE_VERTEXAI` (`true` or `1`), `GOOGLE_CLOUD_PROJECT`,
/// and the API key variables (`GEMINI_API_KEY`, `GOOGLE_GENAI_API_KEY`,
/// `GOOGLE_API_KEY`) to determine the correct endpoint, by the same rules as
/// `ApiEndpoint::from_env`.
///
/// For the Live session location, prefers `GEMINI_LIVE_MODEL_LOCATION` over
/// `GOOGLE_CLOUD_LOCATION` (default: `us-central1`). This allows setting
/// `GOOGLE_CLOUD_LOCATION=global` for HTTP APIs while using a regional
/// endpoint for the Live WebSocket API.
///
/// For the model, prefers `model` argument > `GEMINI_LIVE_MODEL` > `GEMINI_MODEL`.
pub fn build_session_config(model: Option<&str>) -> Result<SessionConfig, String> {
    let mut config = if vertex_requested() {
        let project = cloud_project_from_env()
            .ok_or_else(|| "GOOGLE_CLOUD_PROJECT env var not set".to_string())?;
        let location =
            non_empty_env("GEMINI_LIVE_MODEL_LOCATION").unwrap_or_else(cloud_location_from_env);

        let access_token = resolve_vertex_token()?;

        SessionConfig::from_vertex(project, location, access_token)
    } else {
        // Google AI with API key.
        let api_key = api_key_from_env().ok_or_else(|| {
            "No API key found. Set GEMINI_API_KEY (or GOOGLE_GENAI_API_KEY / GOOGLE_API_KEY), \
             or set GOOGLE_GENAI_USE_VERTEXAI=true for Vertex AI."
                .to_string()
        })?;

        SessionConfig::new(api_key)
    };

    let effective_model = model
        .map(std::string::ToString::to_string)
        .or_else(|| std::env::var("GEMINI_LIVE_MODEL").ok())
        .or_else(|| std::env::var("GEMINI_MODEL").ok());

    if let Some(m) = effective_model {
        config = config.model(ModelId::new(m));
    }

    Ok(config)
}

/// Resolve the Live session model from env vars, falling back to `Gemini2_0FlashLive`.
///
/// Precedence: `GEMINI_LIVE_MODEL` env var > `ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO`.
pub fn live_model() -> ModelId {
    std::env::var("GEMINI_LIVE_MODEL")
        .ok()
        .map(ModelId::new)
        .unwrap_or(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
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
