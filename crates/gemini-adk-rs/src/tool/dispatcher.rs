//! Tool dispatcher — routes function calls to the right tool implementation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use gemini_genai_rs::prelude::{FunctionCall, FunctionDeclaration, FunctionResponse, Tool};

use crate::error::ToolError;

use super::{ActiveStreamingTool, ToolClass, ToolFunction, ToolKind, DEFAULT_TOOL_TIMEOUT};

/// Routes function calls to the right tool implementation.
pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<tokio::sync::Mutex<HashMap<String, ActiveStreamingTool>>>,
    default_timeout: Duration,
    /// Cached tool declarations — computed once on first access.
    cached_declarations: std::sync::OnceLock<Vec<Tool>>,
}

impl ToolDispatcher {
    /// Create a new empty tool dispatcher with the default 30-second timeout.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use gemini_adk_rs::tool::{ToolDispatcher, SimpleTool};
    /// use serde_json::json;
    ///
    /// let mut dispatcher = ToolDispatcher::new();
    /// dispatcher.register(SimpleTool::new(
    ///     "echo", "Echo input", None,
    ///     |args| async move { Ok(args) },
    /// ));
    /// ```
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            active: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            default_timeout: DEFAULT_TOOL_TIMEOUT,
            cached_declarations: std::sync::OnceLock::new(),
        }
    }

    /// Set the default timeout for tool calls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Returns the configured default timeout.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// Register a tool that implements [`ToolFunction`].
    pub fn register(&mut self, tool: impl ToolFunction) {
        let tool = Arc::new(tool);
        self.tools
            .insert(tool.name().to_string(), ToolKind::Function(tool));
    }

    /// Register a regular function tool (pre-wrapped in Arc).
    pub fn register_function(&mut self, tool: Arc<dyn ToolFunction>) {
        self.tools
            .insert(tool.name().to_string(), ToolKind::Function(tool));
    }

    /// Register a streaming tool.
    pub fn register_streaming(&mut self, tool: Arc<dyn super::StreamingTool>) {
        self.tools
            .insert(tool.name().to_string(), ToolKind::Streaming(tool));
    }

    /// Register an input-streaming tool.
    pub fn register_input_streaming(&mut self, tool: Arc<dyn super::InputStreamingTool>) {
        self.tools
            .insert(tool.name().to_string(), ToolKind::InputStream(tool));
    }

    /// Get a tool by name (for introspection/streaming tool spawning).
    pub fn get_tool(&self, name: &str) -> Option<&ToolKind> {
        self.tools.get(name)
    }

    /// Classify a tool by name.
    pub fn classify(&self, name: &str) -> Option<ToolClass> {
        self.tools.get(name).map(|t| match t {
            ToolKind::Function(_) => ToolClass::Regular,
            ToolKind::Streaming(_) => ToolClass::Streaming,
            ToolKind::InputStream(_) => ToolClass::InputStream,
        })
    }

    /// Call a regular function tool by name, using the default timeout.
    pub async fn call_function(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.call_function_with_timeout(name, args, self.default_timeout)
            .await
    }

    /// Call a regular function tool by name with an explicit timeout.
    ///
    /// If the tool does not complete within the given duration, its future is
    /// dropped (cancelling it) and `ToolError::Timeout` is returned.
    pub async fn call_function_with_timeout(
        &self,
        name: &str,
        args: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, ToolError> {
        let func = match self.tools.get(name) {
            Some(ToolKind::Function(f)) => f.clone(),
            Some(_) => {
                return Err(ToolError::Other(format!(
                    "{name} is not a regular function tool"
                )))
            }
            None => return Err(ToolError::NotFound(name.to_string())),
        };

        match tokio::time::timeout(timeout, func.call(args)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ToolError::Timeout(timeout)),
        }
    }

    /// Call a regular function tool by name, racing against a cancellation token.
    ///
    /// If the token is cancelled before the tool completes, its future is
    /// dropped and `ToolError::Cancelled` is returned.
    pub async fn call_function_with_cancel(
        &self,
        name: &str,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<serde_json::Value, ToolError> {
        let func = match self.tools.get(name) {
            Some(ToolKind::Function(f)) => f.clone(),
            Some(_) => {
                return Err(ToolError::Other(format!(
                    "{name} is not a regular function tool"
                )))
            }
            None => return Err(ToolError::NotFound(name.to_string())),
        };

        tokio::select! {
            result = func.call(args) => result,
            _ = cancel.cancelled() => Err(ToolError::Cancelled),
        }
    }

    /// Build a FunctionResponse from a FunctionCall result.
    pub fn build_response(
        call: &FunctionCall,
        result: Result<serde_json::Value, ToolError>,
    ) -> FunctionResponse {
        match result {
            Ok(value) => FunctionResponse {
                name: call.name.clone(),
                response: value,
                id: call.id.clone(),
                scheduling: None,
            },
            Err(e) => FunctionResponse {
                name: call.name.clone(),
                response: serde_json::json!({"error": e.to_string()}),
                id: call.id.clone(),
                scheduling: None,
            },
        }
    }

    /// Cancel a streaming tool by name.
    pub async fn cancel_streaming(&self, name: &str) {
        let mut active = self.active.lock().await;
        if let Some(tool) = active.remove(name) {
            tool.cancel.cancel();
            tool.task.abort();
        }
    }

    /// Store an active streaming tool (for cancellation tracking).
    pub(crate) async fn store_active(&self, id: String, tool: ActiveStreamingTool) {
        self.active.lock().await.insert(id, tool);
    }

    /// Cancel streaming tools by IDs.
    pub async fn cancel_by_ids(&self, ids: &[String]) {
        let mut active = self.active.lock().await;
        for id in ids {
            if let Some(tool) = active.remove(id.as_str()) {
                tool.cancel.cancel();
                tool.task.abort();
            }
        }
    }

    /// Generate Tool declarations for the setup message.
    ///
    /// Results are cached after first computation. The cache is invalidated
    /// when tools are registered via `register*()` methods.
    pub fn to_tool_declarations(&self) -> Vec<Tool> {
        self.cached_declarations
            .get_or_init(|| {
                let declarations: Vec<FunctionDeclaration> = self
                    .tools
                    .values()
                    .map(|t| {
                        let (name, desc, params) = match t {
                            ToolKind::Function(f) => (f.name(), f.description(), f.parameters()),
                            ToolKind::Streaming(s) => (s.name(), s.description(), s.parameters()),
                            ToolKind::InputStream(i) => (i.name(), i.description(), i.parameters()),
                        };
                        FunctionDeclaration {
                            name: name.to_string(),
                            description: desc.to_string(),
                            parameters: params,
                            behavior: None,
                        }
                    })
                    .collect();

                if declarations.is_empty() {
                    vec![]
                } else {
                    vec![Tool::functions(declarations)]
                }
            })
            .clone()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl gemini_genai_rs::prelude::ToolProvider for ToolDispatcher {
    fn declarations(&self) -> Vec<gemini_genai_rs::prelude::Tool> {
        self.to_tool_declarations()
    }
}
