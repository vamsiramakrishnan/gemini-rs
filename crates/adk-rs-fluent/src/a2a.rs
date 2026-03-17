//! A2A — Agent-to-Agent protocol builders.
//!
//! Fluent builders for remote agent discovery, delegation, and server publishing.

use std::time::Duration;

/// Builder for a remote agent reference (client-side).
///
/// ```ignore
/// let remote = RemoteAgent::new("verifier")
///     .endpoint("https://agent.example.com")
///     .timeout(Duration::from_secs(30))
///     .describe("Verifies caller identity");
/// ```
#[derive(Clone, Debug)]
pub struct RemoteAgent {
    name: String,
    endpoint: Option<String>,
    timeout: Option<Duration>,
    description: Option<String>,
    streaming: bool,
}

impl RemoteAgent {
    /// Create a new remote agent reference with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            endpoint: None,
            timeout: None,
            description: None,
            streaming: false,
        }
    }

    /// Set the remote endpoint URL.
    pub fn endpoint(mut self, url: impl Into<String>) -> Self {
        self.endpoint = Some(url.into());
        self
    }

    /// Set the request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Set a description for this remote agent.
    pub fn describe(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Enable streaming responses from the remote agent.
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.streaming = enabled;
        self
    }

    /// The agent name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configured endpoint.
    pub fn get_endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The configured timeout.
    pub fn get_timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

/// Builder for an A2A server that exposes a local agent.
///
/// ```ignore
/// let server = A2AServer::new(my_agent)
///     .host("0.0.0.0")
///     .port(8080)
///     .health_check("/health");
/// ```
#[derive(Clone, Debug)]
pub struct A2AServer {
    agent_name: String,
    host: String,
    port: u16,
    health_check: String,
    streaming: bool,
}

impl A2AServer {
    /// Create a new A2A server for the given agent name.
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            host: "0.0.0.0".to_string(),
            port: 8080,
            health_check: "/health".to_string(),
            streaming: false,
        }
    }

    /// Set the host to bind to.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Set the port to listen on.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the health check endpoint path.
    pub fn health_check(mut self, path: impl Into<String>) -> Self {
        self.health_check = path.into();
        self
    }

    /// Enable streaming support.
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.streaming = enabled;
        self
    }

    /// The agent name this server exposes.
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// The configured host.
    pub fn get_host(&self) -> &str {
        &self.host
    }

    /// The configured port.
    pub fn get_port(&self) -> u16 {
        self.port
    }
}

/// Registry for discovering remote agents.
#[derive(Clone, Debug)]
pub struct AgentRegistry {
    base_url: String,
}

impl AgentRegistry {
    /// Create a registry pointing at the given base URL.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    /// The base URL of this registry.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// A2A skill declaration metadata.
#[derive(Clone, Debug)]
pub struct SkillDeclaration {
    /// Skill identifier.
    pub id: String,
    /// Human-readable skill name.
    pub name: String,
    /// Description of what the skill does.
    pub description: Option<String>,
}

impl SkillDeclaration {
    /// Create a new skill declaration.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: None,
        }
    }

    /// Set a description.
    pub fn describe(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_agent_builder() {
        let agent = RemoteAgent::new("verifier")
            .endpoint("https://agent.example.com")
            .timeout(Duration::from_secs(30))
            .describe("Verifies identity")
            .streaming(true);

        assert_eq!(agent.name(), "verifier");
        assert_eq!(agent.get_endpoint(), Some("https://agent.example.com"));
        assert_eq!(agent.get_timeout(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn a2a_server_builder() {
        let server = A2AServer::new("my-agent")
            .host("127.0.0.1")
            .port(9090)
            .health_check("/ping")
            .streaming(true);

        assert_eq!(server.agent_name(), "my-agent");
        assert_eq!(server.get_host(), "127.0.0.1");
        assert_eq!(server.get_port(), 9090);
    }

    #[test]
    fn agent_registry() {
        let registry = AgentRegistry::new("https://registry.example.com");
        assert_eq!(registry.base_url(), "https://registry.example.com");
    }

    #[test]
    fn skill_declaration() {
        let skill = SkillDeclaration::new("verify", "Identity Verification")
            .describe("Verifies caller identity");
        assert_eq!(skill.id, "verify");
        assert_eq!(skill.name, "Identity Verification");
        assert!(skill.description.is_some());
    }
}
