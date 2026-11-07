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
    /// Trace id for this run, fetchable via `GET /debug/trace/:id`.
    pub trace_id: String,
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

/// Pagination query parameters shared by list endpoints
/// (session listing, eval results).
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    /// Maximum number of items to return (default 50).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of items to skip from the start of the collection.
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
#[derive(Debug, Clone, Serialize)]
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

/// The current time as an RFC 3339 UTC timestamp, to the second
/// (`2024-02-29T12:34:56Z`). This used to return `"{unix_seconds}Z"`, which
/// was not a date; it is now the shared formatter in `gemini-adk-rs`.
pub fn now_iso8601() -> String {
    gemini_adk_rs::utils::time::now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::now_iso8601;

    #[test]
    fn now_iso8601_is_an_rfc_3339_utc_timestamp() {
        let now = now_iso8601();
        let b = now.as_bytes();
        assert_eq!(now.len(), "2024-02-29T12:34:56Z".len(), "{now}");
        assert!(
            b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':',
            "{now}"
        );
        assert!(now.ends_with('Z'), "{now}");
    }
}
