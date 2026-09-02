//! Skill registry — centralized publish/discover for agent capabilities,
//! the ADK skills-registry pattern.
//!
//! A *skill* is a named, versioned capability: locally, an [`AgentConfig`]
//! this process can build and run; remotely, an A2A endpoint another
//! service exposes. Registries let a deployment resolve capabilities at
//! runtime instead of hardcoding them:
//!
//! ```ignore
//! let registry = LocalSkillRegistry::new();
//! registry.publish(SkillInfo::local("triage", "1.2.0", "Route a support ticket", config)).await?;
//!
//! // Later — possibly a different subsystem:
//! let skill = registry.resolve("triage", None).await?.expect("registered");
//! let config = skill.agent.unwrap();
//! ```
//!
//! [`LocalSkillRegistry`] is the in-process reference backend, and
//! [`LocalSkillRegistry::load_dir`] hydrates one from a directory of agent
//! config files (see [`discover_agent_configs`](crate::agent_config::discover_agent_configs)).
//! Cloud registries (a database, a service mesh, Google Cloud's skill
//! registry) implement [`SkillRegistryBackend`].

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::agent_config::AgentConfig;

/// One published skill: identity, humans-facing description, and either a
/// locally buildable agent, a remote A2A endpoint, or both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    /// Skill name — the lookup key.
    pub name: String,
    /// Version string; ordering is lexicographic on `(len, str)` so plain
    /// numeric schemes ("2" < "10") and dotted schemes sort usefully.
    pub version: String,
    /// What the skill does — surfaced to humans and to routing LLMs.
    pub description: String,
    /// Free-form discovery tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Locally buildable definition, when this process can run the skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfig>,
    /// Remote A2A endpoint URL, when the skill runs elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl SkillInfo {
    /// A locally runnable skill backed by an [`AgentConfig`].
    pub fn local(
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
        agent: AgentConfig,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: description.into(),
            tags: Vec::new(),
            agent: Some(agent),
            endpoint: None,
        }
    }

    /// A remote skill reachable over A2A.
    pub fn remote(
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: description.into(),
            tags: Vec::new(),
            agent: None,
            endpoint: Some(endpoint.into()),
        }
    }

    /// Add discovery tags.
    pub fn with_tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(std::string::ToString::to_string).collect();
        self
    }
}

/// Filter for [`SkillRegistryBackend::list`]. Empty filter matches all.
#[derive(Debug, Clone, Default)]
pub struct SkillFilter {
    /// Substring match against the skill name (case-insensitive).
    pub name_contains: Option<String>,
    /// Require this tag.
    pub tag: Option<String>,
}

impl SkillFilter {
    fn matches(&self, skill: &SkillInfo) -> bool {
        if let Some(needle) = &self.name_contains
            && !skill.name.to_lowercase().contains(&needle.to_lowercase())
        {
            return false;
        }
        if let Some(tag) = &self.tag
            && !skill.tags.iter().any(|t| t == tag)
        {
            return false;
        }
        true
    }
}

/// Storage backend for a skill registry. Implement this for a database or
/// cloud registry; [`LocalSkillRegistry`] is the in-process reference.
#[async_trait]
pub trait SkillRegistryBackend: Send + Sync {
    /// Publish (or re-publish) a skill version. Same name+version replaces.
    async fn publish(&self, skill: SkillInfo) -> Result<(), SkillRegistryError>;
    /// Resolve a skill: the exact version when given, else the latest.
    async fn resolve(
        &self,
        name: &str,
        version: Option<&str>,
    ) -> Result<Option<SkillInfo>, SkillRegistryError>;
    /// List skills matching the filter — the latest version of each name.
    async fn list(&self, filter: &SkillFilter) -> Result<Vec<SkillInfo>, SkillRegistryError>;
    /// Remove every version of a skill. Missing names are a no-op.
    async fn remove(&self, name: &str) -> Result<(), SkillRegistryError>;
}

/// Registry backend failure.
#[derive(Debug, thiserror::Error)]
pub enum SkillRegistryError {
    /// The backend rejected or failed the operation.
    #[error("skill registry: {0}")]
    Backend(String),
}

/// Version ordering: length-then-lexicographic, so "2" < "10" and
/// "1.9.0" < "1.10.0" without pulling in a semver dependency.
fn version_key(v: &str) -> (Vec<(usize, String)>, String) {
    (
        v.split('.')
            .map(|part| (part.len(), part.to_string()))
            .collect(),
        v.to_string(),
    )
}

/// In-process skill registry: a versioned map behind an async lock.
#[derive(Default)]
pub struct LocalSkillRegistry {
    skills: RwLock<HashMap<String, Vec<SkillInfo>>>,
}

impl LocalSkillRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hydrate a registry from a directory of agent config files: every
    /// discovered [`AgentConfig`] is published as version `"0"` under its
    /// own name with its description.
    pub async fn load_dir(dir: &std::path::Path) -> Result<Self, SkillRegistryError> {
        let configs = crate::agent_config::discover_agent_configs(dir)
            .map_err(|e| SkillRegistryError::Backend(e.to_string()))?;
        let registry = Self::new();
        for config in configs {
            let skill = SkillInfo::local(
                config.name.clone(),
                "0",
                config.description.clone().unwrap_or_default(),
                config,
            );
            registry.publish(skill).await?;
        }
        Ok(registry)
    }

    /// Wrap in an [`Arc`] for sharing across tasks.
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[async_trait]
impl SkillRegistryBackend for LocalSkillRegistry {
    async fn publish(&self, skill: SkillInfo) -> Result<(), SkillRegistryError> {
        if skill.name.is_empty() {
            return Err(SkillRegistryError::Backend("skill name is empty".into()));
        }
        let mut skills = self.skills.write().await;
        let versions = skills.entry(skill.name.clone()).or_default();
        versions.retain(|existing| existing.version != skill.version);
        versions.push(skill);
        versions.sort_by_key(|s| version_key(&s.version));
        Ok(())
    }

    async fn resolve(
        &self,
        name: &str,
        version: Option<&str>,
    ) -> Result<Option<SkillInfo>, SkillRegistryError> {
        let skills = self.skills.read().await;
        let Some(versions) = skills.get(name) else {
            return Ok(None);
        };
        Ok(match version {
            Some(v) => versions.iter().find(|s| s.version == v).cloned(),
            None => versions.last().cloned(),
        })
    }

    async fn list(&self, filter: &SkillFilter) -> Result<Vec<SkillInfo>, SkillRegistryError> {
        let skills = self.skills.read().await;
        let mut out: Vec<SkillInfo> = skills
            .values()
            .filter_map(|versions| versions.last())
            .filter(|s| filter.matches(s))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn remove(&self, name: &str) -> Result<(), SkillRegistryError> {
        self.skills.write().await.remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str) -> AgentConfig {
        AgentConfig::from_json(&format!(
            r#"{{"name": "{name}", "instruction": "do the thing"}}"#
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn publish_resolve_latest_and_exact() {
        let registry = LocalSkillRegistry::new();
        for v in ["1.9.0", "1.10.0", "1.2.0"] {
            registry
                .publish(SkillInfo::local(
                    "triage",
                    v,
                    "route tickets",
                    config("triage"),
                ))
                .await
                .unwrap();
        }
        // Latest is numeric-aware: 1.10.0 beats 1.9.0.
        let latest = registry.resolve("triage", None).await.unwrap().unwrap();
        assert_eq!(latest.version, "1.10.0");
        let exact = registry
            .resolve("triage", Some("1.2.0"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exact.version, "1.2.0");
        assert!(registry.resolve("missing", None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn republish_replaces_and_list_filters() {
        let registry = LocalSkillRegistry::new();
        registry
            .publish(
                SkillInfo::local("triage", "1", "old", config("triage")).with_tags(&["support"]),
            )
            .await
            .unwrap();
        registry
            .publish(
                SkillInfo::local("triage", "1", "new", config("triage")).with_tags(&["support"]),
            )
            .await
            .unwrap();
        registry
            .publish(SkillInfo::remote(
                "billing",
                "1",
                "invoices",
                "https://a2a.example",
            ))
            .await
            .unwrap();

        let all = registry.list(&SkillFilter::default()).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            registry
                .resolve("triage", None)
                .await
                .unwrap()
                .unwrap()
                .description,
            "new"
        );
        let tagged = registry
            .list(&SkillFilter {
                tag: Some("support".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0].name, "triage");

        registry.remove("triage").await.unwrap();
        assert!(registry.resolve("triage", None).await.unwrap().is_none());
    }
}
