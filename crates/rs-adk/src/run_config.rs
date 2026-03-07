//! RunConfig — configuration for agent execution runs.

use serde::{Deserialize, Serialize};

/// How the agent communicates with clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamingMode {
    /// No streaming — single request/response.
    None,
    /// Server-sent events (unidirectional streaming).
    SSE,
    /// Bidirectional streaming (e.g., WebSocket).
    #[default]
    Bidi,
}


/// Configuration for an agent execution run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Maximum number of LLM calls per run (safety limit).
    pub max_llm_calls: u32,
    /// How the agent streams responses.
    pub streaming_mode: StreamingMode,
    /// Whether to save input blobs (images, audio) as artifacts.
    pub save_input_blobs_as_artifacts: bool,
    /// Whether to support compositional function calling.
    pub support_cfc: bool,
    /// Whether to pause execution on tool calls (for confirmation UX).
    pub pause_on_tool_calls: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_llm_calls: 500,
            streaming_mode: StreamingMode::default(),
            save_input_blobs_as_artifacts: false,
            support_cfc: false,
            pause_on_tool_calls: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let config = RunConfig::default();
        assert_eq!(config.max_llm_calls, 500);
        assert_eq!(config.streaming_mode, StreamingMode::Bidi);
        assert!(!config.save_input_blobs_as_artifacts);
        assert!(!config.support_cfc);
        assert!(!config.pause_on_tool_calls);
    }

    #[test]
    fn streaming_mode_default_is_bidi() {
        assert_eq!(StreamingMode::default(), StreamingMode::Bidi);
    }

    #[test]
    fn clone_run_config() {
        let config = RunConfig {
            max_llm_calls: 100,
            streaming_mode: StreamingMode::SSE,
            save_input_blobs_as_artifacts: true,
            support_cfc: true,
            pause_on_tool_calls: true,
        };
        let cloned = config.clone();
        assert_eq!(cloned.max_llm_calls, 100);
        assert_eq!(cloned.streaming_mode, StreamingMode::SSE);
        assert!(cloned.save_input_blobs_as_artifacts);
    }

    #[test]
    fn streaming_mode_serde_roundtrip() {
        let mode = StreamingMode::SSE;
        let json = serde_json::to_string(&mode).unwrap();
        let parsed: StreamingMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, StreamingMode::SSE);
    }
}
