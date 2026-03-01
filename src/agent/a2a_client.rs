//! A2A (Agent-to-Agent) protocol client for inter-agent collaboration.
//!
//! Implements A2A v0.3: agent card discovery, JSON-RPC 2.0 task management,
//! and SSE streaming for long-running tasks.

use serde::Deserialize;

#[cfg(feature = "agent-a2a")]
use super::AgentError;

/// Configuration for the A2A client.
#[derive(Debug, Clone)]
pub struct A2AConfig {
    /// Base URL of the remote agent.
    pub base_url: String,
    /// Timeout for HTTP requests in seconds.
    pub timeout_secs: u64,
    /// Optional authentication token.
    pub auth_token: Option<String>,
}

impl Default for A2AConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:9000".to_string(),
            timeout_secs: 30,
            auth_token: None,
        }
    }
}

/// Agent Card as defined by the A2A specification.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub url: String,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
    #[serde(default)]
    pub supported_protocols: Vec<String>,
}

/// A skill advertised by a remote agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_modes: Vec<String>,
    #[serde(default)]
    pub output_modes: Vec<String>,
}

/// Result of an A2A task.
#[derive(Debug, Clone)]
pub struct A2ATask {
    /// Task ID.
    pub id: String,
    /// Task status.
    pub status: String,
    /// Task result data.
    pub result: serde_json::Value,
}

/// A2A protocol client for discovering and communicating with remote agents.
#[cfg(feature = "agent-a2a")]
pub struct A2AClient {
    config: A2AConfig,
    client: reqwest::Client,
    agent_card: Option<AgentCard>,
}

#[cfg(feature = "agent-a2a")]
impl A2AClient {
    /// Create a new A2A client.
    pub fn new(config: A2AConfig) -> Self {
        let builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs));

        // Auth header will be added per-request if configured
        let client = builder.build().expect("HTTP client creation should not fail");

        Self {
            config,
            client,
            agent_card: None,
        }
    }

    /// Discover a remote agent's capabilities via its agent card.
    pub async fn discover(&mut self) -> Result<&AgentCard, AgentError> {
        let card_url = format!(
            "{}/.well-known/agent.json",
            self.config.base_url.trim_end_matches('/')
        );

        let mut req = self.client.get(&card_url);
        if let Some(ref token) = self.config.auth_token {
            req = req.bearer_auth(token);
        }

        let response = req
            .send()
            .await
            .map_err(|e| AgentError::DiscoveryFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(AgentError::DiscoveryFailed(format!(
                "Agent card request failed with status {}",
                response.status()
            )));
        }

        let card: AgentCard = response
            .json()
            .await
            .map_err(|e| AgentError::ResponseParse(e.to_string()))?;

        self.agent_card = Some(card);
        Ok(self.agent_card.as_ref().unwrap())
    }

    /// Get the discovered agent card, if available.
    pub fn agent_card(&self) -> Option<&AgentCard> {
        self.agent_card.as_ref()
    }

    /// Send a task to the remote agent using JSON-RPC 2.0.
    pub async fn send_task(&self, message: &str) -> Result<A2ATask, AgentError> {
        let card = self
            .agent_card
            .as_ref()
            .ok_or(AgentError::NotDiscovered)?;

        let message_id = uuid::Uuid::new_v4().to_string();
        let request_id = uuid::Uuid::new_v4().to_string();

        let rpc_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "message/send",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{ "text": message }],
                    "messageId": message_id
                }
            }
        });

        let mut req = self
            .client
            .post(&card.url)
            .header("Content-Type", "application/json");

        if let Some(ref token) = self.config.auth_token {
            req = req.bearer_auth(token);
        }

        let response = req
            .json(&rpc_request)
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
                "A2A task request failed: {status} - {body}"
            )));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AgentError::ResponseParse(e.to_string()))?;

        // Parse JSON-RPC response
        if let Some(error) = result.get("error") {
            return Err(AgentError::DispatchFailed(format!(
                "JSON-RPC error: {}",
                error
            )));
        }

        let task_result = result
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let task_id = task_result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(&request_id)
            .to_string();

        let status = task_result
            .get("status")
            .and_then(|v| v.get("state"))
            .and_then(|v| v.as_str())
            .unwrap_or("completed")
            .to_string();

        Ok(A2ATask {
            id: task_id,
            status,
            result: task_result,
        })
    }
}

/// Stub for when the `agent-a2a` feature is not enabled.
#[cfg(not(feature = "agent-a2a"))]
pub struct A2AClient;

#[cfg(not(feature = "agent-a2a"))]
impl A2AClient {
    pub fn new(_config: A2AConfig) -> Self {
        panic!("Enable the `agent-a2a` feature to use A2AClient")
    }
}
