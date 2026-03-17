//! Remote A2A agent — communicates with remote agents via A2A protocol.
//!
//! Mirrors ADK-Python's `RemoteA2aAgent`. Represents a remote agent
//! that can be used as a sub-agent via the Agent-to-Agent protocol.

use serde::{Deserialize, Serialize};

/// Agent card describing a remote A2A agent's capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    /// The agent's name.
    pub name: String,
    /// The agent's description.
    pub description: String,
    /// URL of the remote agent's A2A endpoint.
    pub url: String,
    /// Supported input content types.
    #[serde(default)]
    pub input_content_types: Vec<String>,
    /// Supported output content types.
    #[serde(default)]
    pub output_content_types: Vec<String>,
    /// Skills/capabilities advertised by the agent.
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
}

/// A skill advertised by a remote agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    /// Name of the skill.
    pub name: String,
    /// Description of what the skill does.
    pub description: String,
    /// Example inputs that trigger this skill.
    #[serde(default)]
    pub examples: Vec<String>,
}

/// Configuration for the remote A2A agent.
#[derive(Debug, Clone)]
pub struct RemoteA2aAgentConfig {
    /// Connection timeout in seconds.
    pub timeout_secs: u64,
    /// Whether to send full conversation history for stateless remote agents.
    pub full_history_when_stateless: bool,
}

impl Default for RemoteA2aAgentConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            full_history_when_stateless: false,
        }
    }
}

/// A remote agent accessible via the A2A protocol.
///
/// This agent delegates execution to a remote service via HTTP,
/// converting between the local agent framework's events and the
/// A2A protocol wire format.
#[derive(Debug, Clone)]
pub struct RemoteA2aAgent {
    /// The agent's local name.
    name: String,
    /// The remote agent's card (or URL to fetch it from).
    agent_card: AgentCard,
    /// Configuration.
    config: RemoteA2aAgentConfig,
}

impl RemoteA2aAgent {
    /// Create a new remote A2A agent from an agent card.
    pub fn new(name: impl Into<String>, agent_card: AgentCard) -> Self {
        Self {
            name: name.into(),
            agent_card,
            config: RemoteA2aAgentConfig::default(),
        }
    }

    /// Set the configuration.
    pub fn with_config(mut self, config: RemoteA2aAgentConfig) -> Self {
        self.config = config;
        self
    }

    /// Returns the agent's local name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the remote agent's card.
    pub fn agent_card(&self) -> &AgentCard {
        &self.agent_card
    }

    /// Returns the remote agent's A2A endpoint URL.
    pub fn url(&self) -> &str {
        &self.agent_card.url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_card() -> AgentCard {
        AgentCard {
            name: "remote-helper".into(),
            description: "A helpful remote agent".into(),
            url: "https://example.com/a2a".into(),
            input_content_types: vec!["text/plain".into()],
            output_content_types: vec!["text/plain".into()],
            skills: vec![AgentSkill {
                name: "search".into(),
                description: "Search the web".into(),
                examples: vec!["Find information about...".into()],
            }],
        }
    }

    #[test]
    fn create_remote_agent() {
        let agent = RemoteA2aAgent::new("helper", test_card());
        assert_eq!(agent.name(), "helper");
        assert_eq!(agent.url(), "https://example.com/a2a");
    }

    #[test]
    fn agent_card_serde() {
        let card = test_card();
        let json = serde_json::to_string(&card).unwrap();
        let deserialized: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "remote-helper");
        assert_eq!(deserialized.skills.len(), 1);
    }

    #[test]
    fn default_config() {
        let config = RemoteA2aAgentConfig::default();
        assert_eq!(config.timeout_secs, 30);
        assert!(!config.full_history_when_stateless);
    }
}
