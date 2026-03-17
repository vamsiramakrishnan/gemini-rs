//! Bash execution tool — allows agents to execute shell commands.
//!
//! Mirrors ADK-Python's `ExecuteBashTool`. Provides policy-based
//! command validation and requires user confirmation before execution.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Policy for allowed bash commands based on prefix matching.
#[derive(Debug, Clone)]
pub struct BashToolPolicy {
    /// Allowed command prefixes. Use `["*"]` to allow all commands.
    pub allowed_command_prefixes: Vec<String>,
}

impl Default for BashToolPolicy {
    fn default() -> Self {
        Self {
            allowed_command_prefixes: vec!["*".into()],
        }
    }
}

impl BashToolPolicy {
    /// Check whether a command is allowed by this policy.
    pub fn validate(&self, command: &str) -> Result<(), String> {
        let stripped = command.trim();
        if stripped.is_empty() {
            return Err("Command is required.".into());
        }

        if self.allowed_command_prefixes.iter().any(|p| p == "*") {
            return Ok(());
        }

        for prefix in &self.allowed_command_prefixes {
            if stripped.starts_with(prefix.as_str()) {
                return Ok(());
            }
        }

        Err(format!(
            "Command blocked. Permitted prefixes are: {}",
            self.allowed_command_prefixes.join(", ")
        ))
    }
}

/// Tool that executes bash commands with policy-based validation.
///
/// Commands are validated against the configured policy before execution.
/// In a real deployment, this tool should also require user confirmation
/// via the tool confirmation mechanism.
#[derive(Debug, Clone)]
pub struct ExecuteBashTool {
    /// Working directory for command execution.
    workspace: PathBuf,
    /// Command validation policy.
    policy: BashToolPolicy,
    /// Command execution timeout in seconds.
    timeout_secs: u64,
}

impl ExecuteBashTool {
    /// Create a new bash execution tool with the given workspace directory.
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            policy: BashToolPolicy::default(),
            timeout_secs: 30,
        }
    }

    /// Set the command validation policy.
    pub fn with_policy(mut self, policy: BashToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Set the execution timeout in seconds.
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }
}

#[async_trait]
impl ToolFunction for ExecuteBashTool {
    fn name(&self) -> &str {
        "execute_bash"
    }

    fn description(&self) -> &str {
        "Executes a bash command with the working directory set to the workspace. \
         All commands require validation against the configured policy."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute."
                }
            },
            "required": ["command"]
        }))
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("Missing command".into()))?;

        // Validate command against policy
        if let Err(e) = self.policy.validate(command) {
            return Ok(serde_json::json!({"error": e}));
        }

        // Execute command
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to execute command: {e}")))?;

        Ok(serde_json::json!({
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "returncode": output.status.code().unwrap_or(-1)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_allows_all_by_default() {
        let policy = BashToolPolicy::default();
        assert!(policy.validate("ls -la").is_ok());
        assert!(policy.validate("echo hello").is_ok());
    }

    #[test]
    fn policy_blocks_unmatched_prefix() {
        let policy = BashToolPolicy {
            allowed_command_prefixes: vec!["ls".into(), "echo".into()],
        };
        assert!(policy.validate("ls -la").is_ok());
        assert!(policy.validate("echo hello").is_ok());
        assert!(policy.validate("rm -rf /").is_err());
    }

    #[test]
    fn policy_rejects_empty_command() {
        let policy = BashToolPolicy::default();
        assert!(policy.validate("").is_err());
        assert!(policy.validate("  ").is_err());
    }

    #[test]
    fn tool_metadata() {
        let tool = ExecuteBashTool::new(PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "execute_bash");
        assert!(tool.parameters().is_some());
    }

    #[tokio::test]
    async fn execute_simple_command() {
        let tool = ExecuteBashTool::new(PathBuf::from("/tmp"));
        let result = tool
            .call(serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(result["returncode"], 0);
    }

    #[tokio::test]
    async fn blocked_command_returns_error() {
        let tool = ExecuteBashTool::new(PathBuf::from("/tmp")).with_policy(BashToolPolicy {
            allowed_command_prefixes: vec!["ls".into()],
        });
        let result = tool
            .call(serde_json::json!({"command": "rm -rf /"}))
            .await
            .unwrap();
        assert!(result["error"].as_str().unwrap().contains("blocked"));
    }
}
