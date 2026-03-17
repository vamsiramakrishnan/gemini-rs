//! Unified agent loading — supports TOML, JSON, and programmatic registration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A registered agent with its metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub sub_agents: Vec<String>,
}

fn default_agent_type() -> String {
    "llm".to_string()
}

/// Agent registry — the single source of truth for available agents.
///
/// Load from files, register programmatically, or both.
pub struct AgentRegistry {
    agents: HashMap<String, AgentEntry>,
}

impl AgentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// Register a single agent.
    pub fn register(&mut self, agent: AgentEntry) {
        self.agents.insert(agent.name.clone(), agent);
    }

    /// Get an agent by name.
    pub fn get(&self, name: &str) -> Option<&AgentEntry> {
        self.agents.get(name)
    }

    /// List all registered agents.
    pub fn list(&self) -> Vec<&AgentEntry> {
        self.agents.values().collect()
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Discover and load agents from a directory.
    ///
    /// Scans for both TOML (`agent.toml`) and JSON (`agent.json`, `root_agent.json`)
    /// config files. Supports single-agent and multi-agent (subdirectory) layouts.
    ///
    /// Returns the number of agents loaded.
    pub fn discover(&mut self, dir: &Path) -> usize {
        let mut count = 0;

        // JSON discovery via rs-adk (agent.json, root_agent.json)
        if let Ok(configs) = rs_adk::discover_agent_configs(dir) {
            for c in configs {
                let entry = AgentEntry {
                    name: c.name,
                    description: c.description,
                    model: c.model,
                    agent_type: c.agent_type,
                    instruction: c.instruction,
                    tools: c
                        .tools
                        .iter()
                        .filter_map(|t| t.name.clone().or(t.builtin.clone()))
                        .collect(),
                    sub_agents: c.sub_agents.iter().map(|s| s.name.clone()).collect(),
                };
                tracing::info!("Discovered agent (JSON): {}", entry.name);
                self.register(entry);
                count += 1;
            }
        }

        // TOML discovery (agent.toml)
        count += self.discover_toml(dir);

        // Scan subdirectories for TOML
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    count += self.discover_toml(&path);
                }
            }
        }

        count
    }

    /// Try to load an `agent.toml` from a specific directory.
    fn discover_toml(&mut self, dir: &Path) -> usize {
        let toml_path = dir.join("agent.toml");
        if !toml_path.is_file() {
            return 0;
        }

        let content = match std::fs::read_to_string(&toml_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read {}: {e}", toml_path.display());
                return 0;
            }
        };

        match toml::from_str::<TomlManifest>(&content) {
            Ok(manifest) => {
                // Skip if already registered (JSON takes priority)
                if self.agents.contains_key(&manifest.name) {
                    tracing::debug!(
                        "Skipping TOML agent '{}' — already registered from JSON",
                        manifest.name
                    );
                    return 0;
                }

                let entry = AgentEntry {
                    name: manifest.name,
                    description: if manifest.description.is_empty() {
                        None
                    } else {
                        Some(manifest.description)
                    },
                    model: Some(manifest.model),
                    agent_type: "llm".to_string(),
                    instruction: if manifest.instruction.is_empty() {
                        None
                    } else {
                        Some(manifest.instruction)
                    },
                    tools: manifest.tools,
                    sub_agents: manifest.sub_agents,
                };
                tracing::info!("Discovered agent (TOML): {}", entry.name);
                self.register(entry);
                1
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {e}", toml_path.display());
                0
            }
        }
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// TOML agent manifest (internal — matches `adk create` output).
#[derive(Debug, Deserialize)]
struct TomlManifest {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    instruction: String,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    sub_agents: Vec<String>,
}

fn default_model() -> String {
    "gemini-2.0-flash".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut reg = AgentRegistry::new();
        reg.register(AgentEntry {
            name: "test".into(),
            description: Some("A test agent".into()),
            model: Some("gemini-2.0-flash".into()),
            agent_type: "llm".into(),
            instruction: None,
            tools: vec!["google_search".into()],
            sub_agents: vec![],
        });

        assert_eq!(reg.len(), 1);
        assert!(reg.get("test").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn list_agents() {
        let mut reg = AgentRegistry::new();
        reg.register(AgentEntry {
            name: "a".into(),
            description: None,
            model: None,
            agent_type: "llm".into(),
            instruction: None,
            tools: vec![],
            sub_agents: vec![],
        });
        reg.register(AgentEntry {
            name: "b".into(),
            description: None,
            model: None,
            agent_type: "sequential".into(),
            instruction: None,
            tools: vec![],
            sub_agents: vec![],
        });
        assert_eq!(reg.list().len(), 2);
    }
}
