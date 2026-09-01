//! Tool dispatcher — routes function calls to the right tool implementation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use gemini_genai_rs::prelude::{FunctionCall, FunctionDeclaration, FunctionResponse, Tool};

use crate::error::ToolError;

use super::{ActiveStreamingTool, DEFAULT_TOOL_TIMEOUT, ToolClass, ToolFunction, ToolKind};

/// Routes function calls to the right tool implementation.
pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<tokio::sync::Mutex<HashMap<String, ActiveStreamingTool>>>,
    default_timeout: Duration,
    /// Cached tool declarations — computed once on first access.
    cached_declarations: std::sync::OnceLock<Vec<Tool>>,
    /// Optional provider consulted before running confirmation-gated tools.
    confirmation_provider: Option<Arc<dyn crate::confirmation::ConfirmationProvider>>,
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
            confirmation_provider: None,
        }
    }

    /// Set the default timeout for tool calls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Attach a confirmation provider (builder form).
    ///
    /// Once set, any tool reporting
    /// [`requires_confirmation`](crate::tool::ToolFunction::requires_confirmation)
    /// — e.g. one built with `T::confirm(..)` — is checked against the provider
    /// before it executes; a denied decision returns a `ToolError` instead of
    /// running the tool. With no provider configured, confirmation-gated tools
    /// run normally (enforcement is opt-in).
    pub fn with_confirmation_provider(
        mut self,
        provider: Arc<dyn crate::confirmation::ConfirmationProvider>,
    ) -> Self {
        self.confirmation_provider = Some(provider);
        self
    }

    /// Attach a confirmation provider in place. See
    /// [`with_confirmation_provider`](Self::with_confirmation_provider).
    pub fn set_confirmation_provider(
        &mut self,
        provider: Arc<dyn crate::confirmation::ConfirmationProvider>,
    ) {
        self.confirmation_provider = Some(provider);
    }

    /// Whether a confirmation provider is configured.
    pub fn has_confirmation_provider(&self) -> bool {
        self.confirmation_provider.is_some()
    }

    /// Consult the confirmation provider for a gated tool. Returns `Ok(())`
    /// when the tool is not gated, no provider is set, or the call is approved;
    /// returns a `ToolError` when the provider denies it.
    async fn ensure_confirmed(
        &self,
        func: &Arc<dyn ToolFunction>,
        args: &serde_json::Value,
    ) -> Result<(), ToolError> {
        if !func.requires_confirmation() {
            return Ok(());
        }
        let Some(provider) = &self.confirmation_provider else {
            return Ok(());
        };
        let request = crate::confirmation::ConfirmationRequest {
            tool_name: func.name().to_string(),
            args: args.clone(),
            message: func.confirmation_message().map(str::to_string),
        };
        let decision = provider.confirm(request).await;
        if decision.confirmed {
            Ok(())
        } else {
            Err(ToolError::Cancelled)
        }
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
                )));
            }
            None => return Err(ToolError::NotFound(name.to_string())),
        };

        self.ensure_confirmed(&func, &args).await?;

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
                )));
            }
            None => return Err(ToolError::NotFound(name.to_string())),
        };

        self.ensure_confirmed(&func, &args).await?;

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

#[cfg(test)]
mod confirmation_tests {
    use super::*;
    use crate::confirmation::StaticConfirmation;
    use crate::tool::{PolicyTool, SimpleTool, policy::ToolPolicy};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A counting tool wrapped in a confirm policy.
    fn confirm_tool(runs: Arc<AtomicUsize>) -> Arc<dyn ToolFunction> {
        let inner: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "danger",
            "does something sensitive",
            None,
            move |_| {
                let runs = runs.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({ "ok": true }))
                }
            },
        ));
        Arc::new(PolicyTool::new(
            inner,
            ToolPolicy::new().with_confirm(Some("delete production data?".into())),
        ))
    }

    #[tokio::test]
    async fn denied_confirmation_blocks_execution() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));
        d.set_confirmation_provider(StaticConfirmation::deny_all("blocked by policy"));

        let result = d.call_function("danger", json!({})).await;
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "tool must not run when denied"
        );
    }

    #[tokio::test]
    async fn approved_confirmation_runs() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));
        d.set_confirmation_provider(StaticConfirmation::allow_all());

        let out = d.call_function("danger", json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_provider_runs_optin() {
        // Enforcement is opt-in: a confirm-gated tool runs when no provider is set.
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));

        let out = d.call_function("danger", json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_sees_request_and_ignores_non_gated_tools() {
        // A non-confirm tool is never sent to the (deny-all) provider.
        let mut d = ToolDispatcher::new();
        d.register(SimpleTool::new(
            "plain",
            "no confirmation",
            None,
            |_| async move { Ok(json!({ "ran": true })) },
        ));
        d.set_confirmation_provider(StaticConfirmation::deny_all("should not be consulted"));

        let out = d.call_function("plain", json!({})).await.unwrap();
        assert_eq!(out["ran"], true);
    }

    #[tokio::test]
    async fn nested_policy_wrapper_does_not_bypass_confirmation() {
        // T::cached(T::confirm(tool)): an outer cache PolicyTool (confirm=false)
        // wraps an inner confirm PolicyTool. The gate must still fire.
        let runs = Arc::new(AtomicUsize::new(0));
        let inner_confirm = confirm_tool(runs.clone()); // Arc<PolicyTool{confirm}>
        let outer_cached: Arc<dyn ToolFunction> = Arc::new(PolicyTool::new(
            inner_confirm,
            ToolPolicy::new().with_cache(),
        ));
        assert!(
            outer_cached.requires_confirmation(),
            "must propagate through nesting"
        );

        let mut d = ToolDispatcher::new();
        d.register_function(outer_cached);
        d.set_confirmation_provider(StaticConfirmation::deny_all("blocked"));

        let result = d.call_function("danger", json!({})).await;
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "nested confirm must not run when denied"
        );
    }

    #[tokio::test]
    async fn closure_provider_can_gate_by_name() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));
        d.set_confirmation_provider(Arc::new(
            |req: crate::confirmation::ConfirmationRequest| async move {
                if req.tool_name == "danger" {
                    crate::confirmation::ToolConfirmation::denied("name-gated")
                } else {
                    crate::confirmation::ToolConfirmation::confirmed()
                }
            },
        ));

        assert!(d.call_function("danger", json!({})).await.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
