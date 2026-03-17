//! Shared request/response types for all ADK server endpoints.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Agent Execution ─────────────────────────────────────────────

/// Request body for `POST /run`.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    /// Agent name to execute.
    pub agent: String,
    /// User message.
    pub message: String,
    /// Session ID (creates new if absent).
    #[serde(default)]
    pub session_id: Option<String>,
    /// User ID.
    #[serde(default = "default_user_id")]
    pub user_id: String,
    /// Run config overrides.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

fn default_user_id() -> String {
    "default_user".to_string()
}

/// Response from `POST /run`.
#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub session_id: String,
    pub response: String,
    pub events: Vec<AgentEvent>,
    pub state: HashMap<String, serde_json::Value>,
}

/// A single event from agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}

// ── Sessions ────────────────────────────────────────────────────

/// Stored session data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub id: String,
    pub app_name: String,
    pub user_id: String,
    pub state: HashMap<String, serde_json::Value>,
    pub events: Vec<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// Query parameters for session listing.
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    50
}

/// Request body for session rewind.
#[derive(Debug, Deserialize)]
pub struct RewindRequest {
    pub invocation_id: String,
}

// ── Artifacts ───────────────────────────────────────────────────

/// A stored artifact version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactEntry {
    pub name: String,
    pub version: usize,
    pub mime_type: String,
    pub content: String,
    pub size: usize,
    pub timestamp: String,
}

/// Artifact summary for listing.
#[derive(Debug, Serialize)]
pub struct ArtifactSummary {
    pub name: String,
    pub versions: usize,
    pub latest_mime_type: String,
    pub latest_size: usize,
}

// ── Eval ────────────────────────────────────────────────────────

/// Eval run request.
#[derive(Debug, Deserialize)]
pub struct EvalRunRequest {
    pub agent: String,
    #[serde(default)]
    pub eval_set: Option<String>,
    #[serde(default)]
    pub criteria: Vec<String>,
}

/// Eval result summary.
#[derive(Debug, Serialize)]
pub struct EvalResultSummary {
    pub agent: String,
    pub timestamp: String,
    pub total_cases: usize,
    pub passed: usize,
    pub failed: usize,
    pub pass_rate: f64,
    pub criteria_scores: HashMap<String, f64>,
}

// ── Debug ───────────────────────────────────────────────────────

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub agents_loaded: usize,
    pub sessions_active: usize,
}

// ── Helpers ─────────────────────────────────────────────────────

/// ISO 8601 timestamp from system clock.
pub fn now_iso8601() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", dur.as_secs())
}
