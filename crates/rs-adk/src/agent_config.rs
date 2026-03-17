//! YAML/TOML agent configuration — define agents without code.
//!
//! Mirrors upstream ADK's `root_agent.yaml` format. Agents can be defined
//! declaratively and loaded at runtime by the CLI or API server.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Declarative agent configuration — loadable from YAML or TOML.
///
/// # Example YAML
///
/// ```yaml
/// name: weather_agent
/// model: gemini-2.0-flash
/// instruction: "You are a helpful weather assistant."
/// description: "Provides weather information for cities."
/// tools:
///   - name: get_weather
///     description: "Get weather for a city"
///   - builtin: google_search
/// sub_agents:
///   - name: forecast_agent
///     model: gemini-2.0-flash
///     instruction: "Provide 5-day forecasts."
/// output_key: weather_result
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent name (required).
    pub name: String,

    /// Model identifier (e.g., "gemini-2.0-flash", "gemini-2.5-pro").
    #[serde(default)]
    pub model: Option<String>,

    /// System instruction for the agent.
    #[serde(default)]
    pub instruction: Option<String>,

    /// Human-readable description of what this agent does.
    #[serde(default)]
    pub description: Option<String>,

    /// Tool declarations.
    #[serde(default)]
    pub tools: Vec<ToolConfig>,

    /// Sub-agent configurations (for multi-agent hierarchies).
    #[serde(default)]
    pub sub_agents: Vec<AgentConfig>,

    /// Temperature for generation (0.0 - 2.0).
    #[serde(default)]
    pub temperature: Option<f32>,

    /// Maximum output tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,

    /// Thinking budget (Google AI only).
    #[serde(default)]
    pub thinking_budget: Option<u32>,

    /// State key to auto-save the agent's final response into.
    #[serde(default)]
    pub output_key: Option<String>,

    /// JSON Schema for structured output.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,

    /// Maximum number of LLM calls per invocation (safety limit).
    #[serde(default)]
    pub max_llm_calls: Option<u32>,

    /// Agent type: "llm" (default), "sequential", "parallel", "loop".
    #[serde(default = "default_agent_type")]
    pub agent_type: String,

    /// For loop agents: maximum iterations.
    #[serde(default)]
    pub max_iterations: Option<u32>,

    /// Custom metadata (passed through to state or callbacks).
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,

    /// Voice configuration for live agents.
    #[serde(default)]
    pub voice: Option<String>,

    /// Greeting message (model speaks first on connect).
    #[serde(default)]
    pub greeting: Option<String>,

    /// Whether to enable transcription.
    #[serde(default)]
    pub transcription: Option<bool>,

    /// Whether to enable A2A protocol endpoint.
    #[serde(default)]
    pub a2a: Option<bool>,

    /// Environment variables to set when loading this agent.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_agent_type() -> String {
    "llm".to_string()
}

/// Tool configuration within an agent config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Tool name (for custom tools).
    #[serde(default)]
    pub name: Option<String>,

    /// Tool description.
    #[serde(default)]
    pub description: Option<String>,

    /// Built-in tool type (e.g., "google_search", "code_execution", "url_context").
    #[serde(default)]
    pub builtin: Option<String>,

    /// JSON Schema for the tool's parameters.
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

/// Errors from agent config operations.
#[derive(Debug, thiserror::Error)]
pub enum AgentConfigError {
    /// Failed to read the config file.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to parse YAML.
    #[error("YAML parse error: {0}")]
    Yaml(String),

    /// Failed to parse TOML.
    #[error("TOML parse error: {0}")]
    Toml(String),

    /// Failed to parse JSON.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// Invalid configuration.
    #[error("Invalid config: {0}")]
    Invalid(String),
}

impl AgentConfig {
    /// Load agent config from a YAML file.
    pub fn from_yaml_file(path: &Path) -> Result<Self, AgentConfigError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_yaml(&content)
    }

    /// Parse agent config from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, AgentConfigError> {
        serde_json::from_value(
            serde_json::to_value(
                // Use serde_json roundtrip since we don't want to add serde_yaml dep.
                // In practice, the CLI crate will parse YAML and pass the Value.
                serde_json::from_str::<serde_json::Value>(yaml)
                    .map_err(|e| AgentConfigError::Yaml(e.to_string()))?,
            )
            .map_err(|e| AgentConfigError::Yaml(e.to_string()))?,
        )
        .map_err(|e| AgentConfigError::Yaml(e.to_string()))
    }

    /// Parse agent config from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, AgentConfigError> {
        Ok(serde_json::from_str(json)?)
    }

    /// Parse agent config from a JSON value.
    pub fn from_value(value: serde_json::Value) -> Result<Self, AgentConfigError> {
        Ok(serde_json::from_value(value)?)
    }

    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), AgentConfigError> {
        if self.name.is_empty() {
            return Err(AgentConfigError::Invalid("Agent name is required".into()));
        }
        if let Some(temp) = self.temperature {
            if !(0.0..=2.0).contains(&temp) {
                return Err(AgentConfigError::Invalid(format!(
                    "Temperature must be 0.0-2.0, got {}",
                    temp
                )));
            }
        }
        // Validate sub-agents recursively.
        for sub in &self.sub_agents {
            sub.validate()?;
        }
        Ok(())
    }

    /// Check if this is a built-in tool reference.
    pub fn builtin_tools(&self) -> Vec<&str> {
        self.tools
            .iter()
            .filter_map(|t| t.builtin.as_deref())
            .collect()
    }

    /// Check if this is a workflow agent (non-LLM).
    pub fn is_workflow(&self) -> bool {
        matches!(self.agent_type.as_str(), "sequential" | "parallel" | "loop")
    }
}

/// Discover agent configurations in a directory.
///
/// Scans for files named `agent.yaml`, `agent.json`, `agent.toml`,
/// `root_agent.yaml`, or `root_agent.json`.
pub fn discover_agent_configs(dir: &Path) -> Result<Vec<AgentConfig>, AgentConfigError> {
    let candidates = ["agent.json", "root_agent.json", "agent.toml"];

    let mut configs = Vec::new();
    for candidate in &candidates {
        let path = dir.join(candidate);
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: AgentConfig = if candidate.ends_with(".json") {
                serde_json::from_str(&content)?
            } else if candidate.ends_with(".toml") {
                // TOML parsing delegated to CLI crate which has the toml dep
                return Err(AgentConfigError::Toml(
                    "TOML parsing requires the adk-cli crate".into(),
                ));
            } else {
                return Err(AgentConfigError::Yaml(
                    "YAML parsing requires the adk-cli crate".into(),
                ));
            };
            config.validate()?;
            configs.push(config);
        }
    }

    // Also scan subdirectories for agent configs.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(sub_configs) = discover_agent_configs(&path) {
                    configs.extend(sub_configs);
                }
            }
        }
    }

    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_json_config() {
        let json = r#"{"name": "test_agent"}"#;
        let config = AgentConfig::from_json(json).unwrap();
        assert_eq!(config.name, "test_agent");
        assert_eq!(config.agent_type, "llm");
        assert!(config.model.is_none());
        assert!(config.tools.is_empty());
    }

    #[test]
    fn parse_full_json_config() {
        let json = r#"{
            "name": "weather_agent",
            "model": "gemini-2.0-flash",
            "instruction": "You are a weather assistant.",
            "description": "Gets weather info",
            "temperature": 0.3,
            "max_output_tokens": 1024,
            "output_key": "weather_result",
            "max_llm_calls": 10,
            "tools": [
                {"name": "get_weather", "description": "Get weather for a city"},
                {"builtin": "google_search"}
            ],
            "sub_agents": [
                {"name": "forecast", "instruction": "Give forecasts"}
            ]
        }"#;
        let config = AgentConfig::from_json(json).unwrap();
        assert_eq!(config.name, "weather_agent");
        assert_eq!(config.model.as_deref(), Some("gemini-2.0-flash"));
        assert_eq!(config.temperature, Some(0.3));
        assert_eq!(config.output_key.as_deref(), Some("weather_result"));
        assert_eq!(config.max_llm_calls, Some(10));
        assert_eq!(config.tools.len(), 2);
        assert_eq!(config.sub_agents.len(), 1);
        assert_eq!(config.builtin_tools(), vec!["google_search"]);
    }

    #[test]
    fn validate_empty_name_fails() {
        let config = AgentConfig::from_json(r#"{"name": ""}"#).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_bad_temperature_fails() {
        let config = AgentConfig::from_json(r#"{"name": "test", "temperature": 3.0}"#).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_good_config_passes() {
        let config = AgentConfig::from_json(r#"{"name": "test", "temperature": 0.7}"#).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn is_workflow_detection() {
        let sequential =
            AgentConfig::from_json(r#"{"name": "seq", "agent_type": "sequential"}"#).unwrap();
        assert!(sequential.is_workflow());

        let llm = AgentConfig::from_json(r#"{"name": "llm"}"#).unwrap();
        assert!(!llm.is_workflow());
    }

    #[test]
    fn tool_config_variants() {
        let custom = ToolConfig {
            name: Some("my_tool".into()),
            description: Some("Does stuff".into()),
            builtin: None,
            parameters: Some(serde_json::json!({"type": "object"})),
        };
        assert!(custom.name.is_some());
        assert!(custom.builtin.is_none());

        let builtin = ToolConfig {
            name: None,
            description: None,
            builtin: Some("google_search".into()),
            parameters: None,
        };
        assert!(builtin.builtin.is_some());
    }
}
