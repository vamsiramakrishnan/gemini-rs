use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Agent manifest loaded from `agent.toml`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AgentManifest {
    /// Agent name (used as identifier).
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Model identifier, e.g. "gemini-2.0-flash".
    #[serde(default = "default_model")]
    pub model: String,
    /// System instruction for the agent.
    #[serde(default)]
    pub instruction: String,
    /// List of tool names this agent can use.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Sub-agent references (directory names or paths).
    #[serde(default)]
    pub sub_agents: Vec<String>,
}

fn default_model() -> String {
    "gemini-2.0-flash".to_string()
}

/// Discover all `agent.toml` manifests under the given directory (non-recursive top-level scan).
///
/// Looks for:
///   - `<dir>/agent.toml`               (single agent)
///   - `<dir>/<subdir>/agent.toml`       (multi-agent project)
#[allow(dead_code)]
pub fn discover_agents(dir: &Path) -> Vec<(PathBuf, AgentManifest)> {
    let mut agents = Vec::new();

    // Check root
    let root_manifest = dir.join("agent.toml");
    if root_manifest.is_file() {
        if let Ok(m) = load_manifest(&root_manifest) {
            agents.push((dir.to_path_buf(), m));
        }
    }

    // Check immediate subdirectories
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let sub_manifest = path.join("agent.toml");
                if sub_manifest.is_file() {
                    if let Ok(m) = load_manifest(&sub_manifest) {
                        agents.push((path, m));
                    }
                }
            }
        }
    }

    agents
}

/// Load and parse a single `agent.toml` file.
pub fn load_manifest(path: &Path) -> Result<AgentManifest, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let manifest: AgentManifest = toml::from_str(&content)?;
    Ok(manifest)
}
