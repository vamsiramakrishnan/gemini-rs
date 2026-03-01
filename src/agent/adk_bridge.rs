//! ADK Bridge — dispatches Gemini tool calls to a Google ADK agent backend.
//!
//! Translates between Gemini's `FunctionCall`/`FunctionResponse` protocol and
//! ADK's `LiveRequestQueue`/event model.

#[cfg(feature = "agent-adk")]
use crate::protocol::{FunctionCall, FunctionResponse};
#[cfg(feature = "agent-adk")]
use crate::session::{SessionCommand, SessionEvent, SessionHandle};

#[cfg(feature = "agent-adk")]
use super::AgentError;

/// Configuration for the ADK bridge.
#[derive(Debug, Clone)]
pub struct AdkConfig {
    /// ADK server endpoint (FastAPI backend).
    pub endpoint: String,
    /// Agent name to invoke.
    pub agent_name: String,
    /// Session ID for ADK state persistence.
    pub session_id: Option<String>,
    /// Timeout for ADK requests in seconds.
    pub timeout_secs: u64,
}

impl Default for AdkConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8080".to_string(),
            agent_name: "default".to_string(),
            session_id: None,
            timeout_secs: 30,
        }
    }
}

/// Bridge between `gemini-live-rs` sessions and Google ADK agents.
#[cfg(feature = "agent-adk")]
pub struct AdkBridge {
    config: AdkConfig,
    client: reqwest::Client,
}

#[cfg(feature = "agent-adk")]
impl AdkBridge {
    /// Create a new ADK bridge.
    pub fn new(config: AdkConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .expect("HTTP client creation should not fail");
        Self { config, client }
    }

    /// Dispatch a function call to the ADK agent and return the response.
    pub async fn dispatch(&self, call: &FunctionCall) -> Result<FunctionResponse, AgentError> {
        let request = serde_json::json!({
            "agent_name": self.config.agent_name,
            "session_id": self.config.session_id,
            "function_call": {
                "name": call.name,
                "args": call.args,
            }
        });

        let endpoint = format!("{}/dispatch", self.config.endpoint.trim_end_matches('/'));

        let response = self
            .client
            .post(&endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentError::DispatchFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            return Err(AgentError::DispatchFailed(format!(
                "ADK returned {status}: {body}"
            )));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AgentError::ResponseParse(e.to_string()))?;

        Ok(FunctionResponse {
            name: call.name.clone(),
            response: result,
            id: call.id.clone(),
        })
    }

    /// Attach to a session — automatically handles all tool calls via ADK.
    ///
    /// Returns a `JoinHandle` for the background task.
    pub fn attach(self, handle: &SessionHandle) -> tokio::task::JoinHandle<()> {
        let mut events = handle.subscribe();
        let command_tx = handle.command_tx.clone();

        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                if let SessionEvent::ToolCall(calls) = event {
                    let mut responses = Vec::new();
                    for call in &calls {
                        match self.dispatch(call).await {
                            Ok(resp) => responses.push(resp),
                            Err(e) => {
                                responses.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: serde_json::json!({ "error": e.to_string() }),
                                    id: call.id.clone(),
                                });
                            }
                        }
                    }
                    let _ = command_tx
                        .send(SessionCommand::SendToolResponse(responses))
                        .await;
                }
            }
        })
    }
}

/// Stub for when the `agent-adk` feature is not enabled.
#[cfg(not(feature = "agent-adk"))]
pub struct AdkBridge;

#[cfg(not(feature = "agent-adk"))]
impl AdkBridge {
    pub fn new(_config: AdkConfig) -> Self {
        panic!("Enable the `agent-adk` feature to use AdkBridge")
    }
}
