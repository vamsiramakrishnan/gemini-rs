//! Container-based code executor — runs code in Docker containers.
//!
//! Mirrors ADK-Python's `container_code_executor`. Provides sandboxed
//! code execution by running code inside Docker containers.

use async_trait::async_trait;

use super::base::{CodeExecutor, CodeExecutorError};
use super::types::{CodeExecutionInput, CodeExecutionResult};

/// Configuration for container-based code execution.
#[derive(Debug, Clone)]
pub struct ContainerCodeExecutorConfig {
    /// Docker image to use for code execution.
    pub image: String,
    /// Container memory limit (e.g., "256m").
    pub memory_limit: Option<String>,
    /// Container CPU limit (e.g., "0.5").
    pub cpu_limit: Option<String>,
    /// Network mode (e.g., "none" for no network access).
    pub network_mode: Option<String>,
    /// Execution timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ContainerCodeExecutorConfig {
    fn default() -> Self {
        Self {
            image: "python:3.12-slim".into(),
            memory_limit: Some("256m".into()),
            cpu_limit: Some("0.5".into()),
            network_mode: Some("none".into()),
            timeout_secs: 30,
        }
    }
}

/// Code executor that runs code in Docker containers.
///
/// Provides strong isolation by executing code in disposable
/// Docker containers with configurable resource limits.
#[derive(Debug, Clone)]
pub struct ContainerCodeExecutor {
    config: ContainerCodeExecutorConfig,
}

impl ContainerCodeExecutor {
    /// Create a new container code executor with default configuration.
    pub fn new() -> Self {
        Self {
            config: ContainerCodeExecutorConfig::default(),
        }
    }

    /// Create with a custom configuration.
    pub fn with_config(config: ContainerCodeExecutorConfig) -> Self {
        Self { config }
    }

    /// Returns the configured Docker image.
    pub fn image(&self) -> &str {
        &self.config.image
    }
}

impl Default for ContainerCodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CodeExecutor for ContainerCodeExecutor {
    async fn execute_code(
        &self,
        input: CodeExecutionInput,
    ) -> Result<CodeExecutionResult, CodeExecutorError> {
        let mut cmd = tokio::process::Command::new("docker");
        cmd.arg("run").arg("--rm").arg("--read-only");

        if let Some(ref mem) = self.config.memory_limit {
            cmd.arg("--memory").arg(mem);
        }
        if let Some(ref cpu) = self.config.cpu_limit {
            cmd.arg("--cpus").arg(cpu);
        }
        if let Some(ref net) = self.config.network_mode {
            cmd.arg("--network").arg(net);
        }

        cmd.arg(&self.config.image)
            .arg("python3")
            .arg("-c")
            .arg(&input.code);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.timeout_secs),
            cmd.output(),
        )
        .await
        .map_err(|_| CodeExecutorError::Timeout(self.config.timeout_secs))?
        .map_err(|e| CodeExecutorError::ExecutionFailed(format!("Docker execution failed: {e}")))?;

        Ok(CodeExecutionResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            output_files: vec![],
        })
    }

    fn stateful(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = ContainerCodeExecutorConfig::default();
        assert_eq!(config.image, "python:3.12-slim");
        assert_eq!(config.timeout_secs, 30);
    }

    #[test]
    fn executor_metadata() {
        let exec = ContainerCodeExecutor::new();
        assert_eq!(exec.image(), "python:3.12-slim");
        assert!(!exec.stateful());
    }

    #[test]
    fn custom_config() {
        let exec = ContainerCodeExecutor::with_config(ContainerCodeExecutorConfig {
            image: "node:20-slim".into(),
            memory_limit: Some("512m".into()),
            cpu_limit: None,
            network_mode: None,
            timeout_secs: 60,
        });
        assert_eq!(exec.image(), "node:20-slim");
    }
}
